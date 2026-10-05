//! 基于首条真实用户消息的后台命名；标题结果与变更均复用 Runtime Journal。
use super::*;
use keencode_resources::TitleSource;

struct AutomaticTitleLease(Arc<TitleGeneration>);

impl Drop for AutomaticTitleLease {
    fn drop(&mut self) {
        self.0.automatic_inflight.store(false, Ordering::Release);
    }
}

/// 固定首条根会话用户消息与有界正文，避免后续输入或子 Agent 内部任务改变标题主题。
fn automatic_title_input(state: &SessionState) -> Option<(String, String)> {
    if !automatic_title_eligible(state) {
        return None;
    }
    for record in &state.transcript {
        let messages = match record {
            TranscriptRecord::MessageAdded(message) => std::slice::from_ref(message),
            TranscriptRecord::SegmentCommitted(segment) => &segment.messages,
            TranscriptRecord::CompactionApplied(_) => continue,
        };
        for message in messages {
            if message.role != ResourceMessageRole::User
                || message.is_meta
                || message.agent_id.is_some()
                // 子 Agent 的任务输入也可能省略 agent_id，必须按所属 Turn 判定来源。
                || message.turn_id.as_ref().and_then(|id| state.turns.get(id))
                    .is_some_and(|turn| turn.source_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID)
            {
                continue;
            }
            let text = message
                .content
                .iter()
                .filter_map(|part| match part {
                    ResourceMessagePart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let text = text.trim().chars().take(4000).collect::<String>();
            if !text.is_empty() {
                return Some((
                    format!("auto-title-{}", title_input_sha256(&message.message_id)),
                    text,
                ));
            }
        }
    }
    None
}

fn automatic_title_eligible(state: &SessionState) -> bool {
    matches!(
        state.title_source,
        TitleSource::Unspecified | TitleSource::MessagePrefix
    ) && (state.title_source != TitleSource::Unspecified
        || matches!(state.title.as_str(), "新对话" | "New conversation"))
}

impl AgentRuntime {
    /// V4 首次发送和打开旧会话共享此入口；只安排后台工作，不阻塞回复或订阅 ACK。
    pub(crate) fn schedule_automatic_title(self: &Arc<Self>, session_id: &str) {
        let Ok(session) = self.runtime_manager.get(session_id.to_owned()) else {
            return;
        };
        // 先订阅再读快照；start_root_turn 的 ACK 可能早于首条用户消息提交。
        // 此等待不依赖窗口订阅的寿命，切换会话不会漏掉随后确认的输入。
        let Ok(mut events) = session.subscribe() else {
            return;
        };
        let Ok(snapshot) = session.snapshot() else {
            return;
        };
        let mut input = automatic_title_input(&snapshot.state);
        if !automatic_title_eligible(&snapshot.state)
            || (input.is_none()
                && !snapshot
                    .state
                    .turns
                    .values()
                    .any(|turn| turn.status == TurnStatus::Running))
        {
            return;
        }
        let expected_title = snapshot.state.title;
        let expected_source = snapshot.state.title_source;
        let gate = {
            let Ok(mut gates) = self.title_generation_gates.lock() else {
                return;
            };
            Arc::clone(
                gates
                    .entry(session_id.to_owned())
                    .or_insert_with(|| Arc::new(TitleGeneration::default())),
            )
        };
        if gate
            .automatic_inflight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let lease = AutomaticTitleLease(gate);
        let runtime = Arc::clone(self);
        let session_id = session_id.to_owned();
        tokio::spawn(async move {
            let _lease = lease;
            let result = async {
                while input.is_none() {
                    let delivery = tokio::select! {
                        biased;
                        _ = _lease.0.cancellation.cancelled() => return Ok(()),
                        delivery = events.recv() => delivery,
                    };
                    match delivery {
                        Ok(delivery)
                            if matches!(delivery.payload, RuntimeEventPayload::Control(_)) =>
                        {
                            return Ok(());
                        }
                        Ok(delivery)
                            if !matches!(
                                delivery.payload,
                                RuntimeEventPayload::Authoritative(_)
                            ) =>
                        {
                            continue;
                        }
                        Err(RuntimeEventReceiveError::Closed) => return Ok(()),
                        Ok(_) | Err(RuntimeEventReceiveError::Lagged(_)) => {}
                    }
                    let state = session.snapshot().map_err(runtime_operation_failed)?.state;
                    if !automatic_title_eligible(&state) {
                        return Ok(());
                    }
                    input = automatic_title_input(&state);
                    if input.is_none()
                        && !state
                            .turns
                            .values()
                            .any(|turn| turn.status == TurnStatus::Running)
                    {
                        return Ok(());
                    }
                }
                // 只从已提交消息取主题，不能把 sendText 草稿当成会话事实。
                let (operation_id, input) = input.expect("已等待权威用户输入");
                drop(events);
                let title = runtime
                    .generate_title(&session_id, &operation_id, &input)
                    .await?;
                let current = runtime
                    .runtime_manager
                    .get(session_id.clone())
                    .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
                current
                    .rename_generated_title(
                        // 缓存和改名是两条独立控制事件，不得共用 Journal 去重键。
                        &format!("rename-{operation_id}"),
                        &expected_title,
                        expected_source,
                        &title,
                    )
                    .map_err(runtime_operation_failed)?;
                Ok::<(), AgentRuntimeError>(())
            }
            .await;
            if let Err(error) = result {
                // 不记录用户正文、Provider 配置或网络响应；失败不影响主回复，后续打开允许重试。
                tracing::warn!(session_id = %session_id, error = %error, "automatic_session_title_failed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_message(text: &str) -> SessionState {
        let mut state = SessionState::empty(
            keencode_resources::SessionId::new("automatic-title-input").unwrap(),
        );
        state.title = "新对话".to_owned();
        state
            .transcript
            .push(TranscriptRecord::MessageAdded(SessionMessage {
                is_meta: false,
                references: Vec::new(),
                message_id: "first-user-message".to_owned(),
                turn_id: None,
                agent_id: None,
                role: ResourceMessageRole::User,
                content: vec![ResourceMessagePart::Text {
                    text: text.to_owned(),
                }],
            }));
        state
    }

    /// 元上下文、子任务和后续用户输入都不能抢占首条根用户消息的主题。
    #[test]
    fn automatic_title_selects_first_root_input_and_bounds_unicode() {
        let mut state = state_with_message(&"汉".repeat(5000));
        let TranscriptRecord::MessageAdded(first) = &state.transcript[0] else {
            unreachable!()
        };
        let mut meta = first.clone();
        meta.is_meta = true;
        let mut child = first.clone();
        child.agent_id = Some(ResourceAgentId::new("child-agent").unwrap());
        let mut later = first.clone();
        later.message_id = "later-message".to_owned();
        later.content = vec![ResourceMessagePart::Text {
            text: "后续主题".to_owned(),
        }];
        state
            .transcript
            .insert(0, TranscriptRecord::MessageAdded(meta));
        state
            .transcript
            .insert(1, TranscriptRecord::MessageAdded(child));
        state.transcript.push(TranscriptRecord::MessageAdded(later));
        let (operation_id, input) = automatic_title_input(&state).unwrap();
        assert_eq!(input.chars().count(), 4000);
        assert_eq!(
            operation_id,
            format!("auto-title-{}", title_input_sha256("first-user-message"))
        );
        assert_eq!(automatic_title_input(&state), Some((operation_id, input)));
    }

    /// 不修改自定义创建标题、手工标题或已完成的自动标题；空草稿不产生请求。
    #[test]
    fn automatic_title_skips_named_sessions_and_empty_drafts() {
        let mut state = state_with_message("真实用户主题");
        for source in [TitleSource::Manual, TitleSource::Automatic] {
            state.title_source = source;
            assert!(automatic_title_input(&state).is_none());
        }
        state.title_source = TitleSource::Unspecified;
        state.title = "自定义任务名称".to_owned();
        assert!(automatic_title_input(&state).is_none());
        assert!(automatic_title_input(&state_with_message(" \n ")).is_none());
        state.title_source = TitleSource::MessagePrefix;
        assert_eq!(automatic_title_input(&state).unwrap().1, "真实用户主题");
    }
}
