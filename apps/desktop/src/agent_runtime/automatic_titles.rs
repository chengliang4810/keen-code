//! 基于首条真实用户消息的后台命名；标题结果与变更均复用 Runtime Journal。
use super::*;
use keencode_resources::TitleSource;
use keencode_runtime::RuntimeEventReceiveError;

struct AutomaticTitleLease(Arc<TitleGeneration>);

impl Drop for AutomaticTitleLease {
    fn drop(&mut self) {
        self.0.automatic_inflight.store(false, Ordering::Release);
    }
}

const AUTOMATIC_TITLE_INPUT_LIMIT: usize = 4000;

struct AutomaticTitleProjection {
    input: Option<(String, String)>,
    expected_title: String,
    expected_source: TitleSource,
    has_running_turn: bool,
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
            if let Some(text) = bounded_trimmed_message_text(message) {
                return Some((
                    format!("auto-title-{}", title_input_sha256(&message.message_id)),
                    text,
                ));
            }
        }
    }
    None
}

/// 在 Journal 读锁内提取自动标题所需的小投影，避免克隆完整 SessionState。
fn automatic_title_projection(state: &SessionState) -> Option<AutomaticTitleProjection> {
    if !automatic_title_eligible(state) {
        return None;
    }
    Some(AutomaticTitleProjection {
        input: automatic_title_input(state),
        expected_title: state.title.clone(),
        expected_source: state.title_source,
        has_running_turn: state
            .turns
            .values()
            .any(|turn| turn.status == TurnStatus::Running),
    })
}

/// 流式截取全局 trim 后的前 4000 个字符，保留 Text 段之间的换行但不拼接完整正文。
fn bounded_trimmed_message_text(message: &SessionMessage) -> Option<String> {
    let mut output = String::with_capacity(AUTOMATIC_TITLE_INPUT_LIMIT);
    let mut pending_whitespace = String::new();
    let mut output_chars = 0;
    let mut pending_whitespace_chars = 0;
    let mut started = false;
    let mut has_text_part = false;

    for part in &message.content {
        let ResourceMessagePart::Text { text } = part else {
            continue;
        };
        if has_text_part
            && append_bounded_title_char(
                '\n',
                &mut output,
                &mut pending_whitespace,
                &mut output_chars,
                &mut pending_whitespace_chars,
                &mut started,
            )
        {
            break;
        }
        has_text_part = true;
        for character in text.chars() {
            if append_bounded_title_char(
                character,
                &mut output,
                &mut pending_whitespace,
                &mut output_chars,
                &mut pending_whitespace_chars,
                &mut started,
            ) {
                return Some(output);
            }
        }
    }

    (!output.is_empty()).then_some(output)
}

/// 返回 true 表示已经形成不可再被末尾 trim 改变的 4000 字符结果。
fn append_bounded_title_char(
    character: char,
    output: &mut String,
    pending_whitespace: &mut String,
    output_chars: &mut usize,
    pending_whitespace_chars: &mut usize,
    started: &mut bool,
) -> bool {
    if !*started {
        if character.is_whitespace() {
            return false;
        }
        *started = true;
        output.push(character);
        *output_chars += 1;
        return *output_chars == AUTOMATIC_TITLE_INPUT_LIMIT;
    }

    if character.is_whitespace() {
        if *output_chars + *pending_whitespace_chars < AUTOMATIC_TITLE_INPUT_LIMIT {
            pending_whitespace.push(character);
            *pending_whitespace_chars += 1;
        }
        return false;
    }

    if *pending_whitespace_chars > 0 {
        output.push_str(pending_whitespace);
        *output_chars += *pending_whitespace_chars;
        pending_whitespace.clear();
        *pending_whitespace_chars = 0;
        if *output_chars == AUTOMATIC_TITLE_INPUT_LIMIT {
            return true;
        }
    }

    output.push(character);
    *output_chars += 1;
    *output_chars == AUTOMATIC_TITLE_INPUT_LIMIT
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
        // 先订阅再读权威状态；start_root_turn 的 ACK 可能早于首条用户消息提交。
        // 此等待不依赖窗口订阅的寿命，切换会话不会漏掉随后确认的输入。
        let Ok(mut events) = session.subscribe() else {
            return;
        };
        let Ok(Some(initial)) = session.read_state(automatic_title_projection) else {
            return;
        };
        if initial.input.is_none() && !initial.has_running_turn {
            return;
        }
        let mut input = initial.input;
        let mut has_running_turn = initial.has_running_turn;
        let expected_title = initial.expected_title;
        let expected_source = initial.expected_source;
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
        self.executor_handle.spawn(async move {
            let _lease = lease;
            let result = async {
                // 根 Turn 完成前不发起隔离标题请求，避免与主请求争用 Provider
                // 连接和本地测试/离线服务的顺序；已提交正文仍由 Journal 提供。
                while input.is_none() || has_running_turn {
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
                    let Some(projection) = session
                        .read_state(automatic_title_projection)
                        .map_err(runtime_operation_failed)?
                    else {
                        return Ok(());
                    };
                    input = projection.input;
                    has_running_turn = projection.has_running_turn;
                    if input.is_none() && !has_running_turn {
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
