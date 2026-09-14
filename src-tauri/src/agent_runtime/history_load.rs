//! session/load 的轮次窗口：首页进入实时投递泵，旧页作为独立 ACP 投影返回。
use super::*;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct HistoryLoadRequest {
    /// -1 表示全部；正数按根轮次计数。
    pub limit: i32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl HistoryLoadRequest {
    pub fn validate(&self) -> bool {
        (self.limit == -1 || (1..=100).contains(&self.limit))
            && self
                .cursor
                .as_ref()
                .is_none_or(|cursor| !cursor.is_empty() && cursor.len() < 200)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryLoadPage {
    pub session_id: String,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    /// 首页沿用真实投递水位；旧页不进入实时投递世代。
    pub replay: Option<ReplaySessionResponse>,
    pub deliveries: Vec<serde_json::Value>,
}

pub(super) struct HistoryBackfill {
    state: Arc<SessionState>,
    index: keencode_resources::SessionHistoryIndex,
    before: u64,
    anchor: String,
}

impl HistoryBackfill {
    fn cursor(&self) -> String {
        format!(
            "{}:{}:{}",
            self.state.last_sequence, self.before, self.anchor
        )
    }
}

impl AgentRuntime {
    pub(crate) async fn load_history_page(
        self: &Arc<Self>,
        session_id: &str,
        request: HistoryLoadRequest,
    ) -> Result<HistoryLoadPage, AgentRuntimeError> {
        if !request.validate() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        let initial = request.cursor.is_none();
        let delivery = if initial {
            self.reset_session_delivery(session_id).await?
        } else {
            self.session_delivery(session_id)?
        };
        let mut history = delivery.history_backfill.lock().await;
        if initial {
            let session = session.clone();
            *history = Some(
                tokio::task::spawn_blocking(move || {
                    let state = session.snapshot().map_err(runtime_operation_failed)?.state;
                    let index = session.history_index().map_err(runtime_operation_failed)?;
                    let anchor = history_anchor(&session, state.last_sequence)?;
                    Ok::<_, AgentRuntimeError>(HistoryBackfill {
                        before: state.last_sequence + 1,
                        state: Arc::new(state),
                        index,
                        anchor,
                    })
                })
                .await
                .map_err(runtime_operation_failed)??,
            );
        }
        let cached = history
            .as_ref()
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        if !initial && request.cursor.as_deref() != Some(cached.cursor().as_str()) {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let before = cached.before;
        let start = history_window_start(&cached.index.root_starts, before, request.limit);
        let state = Arc::clone(&cached.state);
        let through = state.last_sequence;
        let provider = cached
            .index
            .providers
            .range(..start)
            .next_back()
            .map(|(_, value)| value.clone());
        let context_sequences = cached
            .index
            .context
            .values()
            .filter_map(|sequences| {
                let end = sequences.partition_point(|sequence| *sequence < start);
                end.checked_sub(1).map(|index| sequences[index])
            })
            .collect::<std::collections::BTreeSet<_>>();
        let child_turns = cached
            .index
            .context
            .keys()
            .filter_map(|key| key.strip_prefix("child-start:").map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>();
        let expected_anchor = cached.anchor.clone();
        let read_session = session.clone();
        let drafts = tokio::task::spawn_blocking(move || {
            if history_anchor(&read_session, through)? != expected_anchor {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
            let mut context = Vec::new();
            for sequence in context_sequences {
                let page = read_session
                    .replay((sequence > 1).then_some(sequence - 1), 1)
                    .map_err(runtime_operation_failed)?;
                for record in page.records {
                    let (mapped, _) = map_authoritative_record_with_projection(
                        &read_session,
                        &state,
                        &record,
                        AuthoritativeProjectionMode::Replay,
                        ProviderProjection::from_current(provider.clone()),
                    )?;
                    context.extend(mapped.into_iter().filter(|draft| match draft {
                        DeliveryDraft::KeenCodeEvent {
                            event:
                                KeenCodeEvent::AgentSpawned { .. }
                                | KeenCodeEvent::AgentStatusChanged { .. },
                            ..
                        } => true,
                        DeliveryDraft::KeenCodeEvent {
                            turn_id: Some(turn_id),
                            event:
                                KeenCodeEvent::TurnStarted { .. }
                                | KeenCodeEvent::TurnCompleted
                                | KeenCodeEvent::TurnCancelled
                                | KeenCodeEvent::TurnFailed { .. },
                            ..
                        } => child_turns.contains(turn_id),
                        DeliveryDraft::SessionUpdate { update, .. } => matches!(
                            update.as_ref(),
                            keencode_acp::schema::SessionUpdate::Plan(_)
                        ),
                        _ => false,
                    }));
                }
            }
            context.extend(history_window_drafts(
                &read_session,
                &state,
                start,
                before,
                provider,
            )?);
            Ok(context)
        })
        .await
        .map_err(runtime_operation_failed)??;
        let count = u32::try_from(drafts.len()).map_err(runtime_operation_failed)?;
        let has_more = start > 1;
        let (replay, deliveries) = if initial {
            // 最近窗口已含当前根轮次；释放 live 门，无需等待更早历史。
            let sequence = delivery.send_replay_batch(drafts, through, true).await?;
            (
                Some(ReplaySessionResponse {
                    session_id: session_id.to_owned(),
                    start_after: start - 1,
                    next_after: through,
                    through_journal_sequence: through,
                    through_delivery_sequence: sequence,
                    replayed_events: count,
                    has_more: false,
                }),
                Vec::new(),
            )
        } else {
            let values = drafts
                .into_iter()
                .enumerate()
                .map(|(index, draft)| {
                    let envelope = materialize_delivery(session_id, index as u64 + 1, draft)?;
                    serde_json::to_value(envelope).map_err(runtime_operation_failed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            (None, values)
        };
        let next_cursor = if has_more {
            let cached = history
                .as_mut()
                .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
            cached.before = start;
            Some(cached.cursor())
        } else {
            *history = None;
            None
        };
        Ok(HistoryLoadPage {
            session_id: session_id.to_owned(),
            next_cursor,
            has_more,
            replay,
            deliveries,
        })
    }
}

/// 页边界落在完整根轮次之前，最老页连同 Session 级前缀一起读取。
fn history_window_start(starts: &[u64], before: u64, limit: i32) -> u64 {
    let end = starts.partition_point(|sequence| *sequence < before);
    if limit == -1 || end <= limit as usize {
        1
    } else {
        starts[end - limit as usize]
    }
}

fn history_anchor(session: &RuntimeSession, through: u64) -> Result<String, AgentRuntimeError> {
    if through == 0 {
        return Ok(String::new());
    }
    let page = session
        .replay((through > 1).then_some(through - 1), 1)
        .map_err(runtime_operation_failed)?;
    let record = page
        .records
        .first()
        .filter(|record| record.sequence == through)
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
    Ok(record.event_id.to_string())
}

fn history_window_drafts(
    session: &RuntimeSession,
    state: &SessionState,
    start: u64,
    before: u64,
    provider: Option<ProviderSnapshot>,
) -> Result<Vec<DeliveryDraft>, AgentRuntimeError> {
    let mut provider = ProviderProjection::from_current(provider);
    let mut after = start - 1;
    let mut drafts = Vec::new();
    while after + 1 < before {
        let count = ((before - after - 1) as usize).min(MAX_REPLAY_EVENTS as usize);
        let page = session
            .replay((after > 0).then_some(after), count)
            .map_err(runtime_operation_failed)?;
        if page.records.is_empty() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        for record in page.records {
            if record.sequence >= before {
                break;
            }
            let (mapped, next) = map_authoritative_record_with_projection(
                session,
                state,
                &record,
                AuthoritativeProjectionMode::Replay,
                provider,
            )?;
            drafts.extend(mapped);
            provider = next;
            after = record.sequence;
        }
    }
    Ok(drafts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_cover_history_without_splitting_root_turns() {
        let starts = [4, 20, 35, 90];
        assert_eq!(history_window_start(&starts, 120, 1), 90);
        assert_eq!(history_window_start(&starts, 90, 2), 20);
        assert_eq!(history_window_start(&starts, 20, 2), 1);
        assert_eq!(history_window_start(&starts, 120, -1), 1);
        assert_eq!(history_window_start(&[], 1, 1), 1);
    }
    #[test]
    fn limits_reject_zero_and_out_of_range() {
        for limit in [-2, 0, 101] {
            assert!(
                !HistoryLoadRequest {
                    limit,
                    cursor: None
                }
                .validate()
            );
        }
        for limit in [-1, 1, 100] {
            assert!(
                HistoryLoadRequest {
                    limit,
                    cursor: None
                }
                .validate()
            );
        }
    }
}
