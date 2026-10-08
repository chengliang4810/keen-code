//! 原生审批和问答读取 Runtime 唯一 pending 账本，窗口不维护执行许可。

use super::{NativeHost, ui_error};
use crate::{agent_runtime::RootTurnOptions, native_ui::model::*, permissions};
use keencode_acp::ConnectionId;

pub(super) fn connection() -> ConnectionId {
    ConnectionId::new("gpui-owner").expect("固定原生连接标识合法")
}

impl NativeHost {
    pub(super) fn persist_permission(
        &self,
        session_id: &str,
        operation_id: &str,
        mode: PermissionMode,
    ) -> Result<(), NativeUiError> {
        self.inner
            .runtime
            .set_permission_mode_with_operation(
                session_id,
                operation_id,
                match mode {
                    PermissionMode::Build => permissions::PermissionMode::Build,
                    PermissionMode::Edit => permissions::PermissionMode::Edit,
                    PermissionMode::Plan => permissions::PermissionMode::Plan,
                    PermissionMode::Yolo => permissions::PermissionMode::Yolo,
                },
            )
            .map_err(ui_error)?;
        Ok(())
    }

    pub(super) fn bind_native_interactions(
        &self,
        session_id: &str,
        options: &mut RootTurnOptions,
    ) -> Result<(), NativeUiError> {
        let connection = connection();
        self.inner
            .runtime
            .elicitation_coordinator()
            .bind_native_session(session_id, &connection)
            .map_err(ui_error)?;
        self.inner
            .runtime
            .permissions()
            .bind_native_session(session_id, &connection)
            .map_err(ui_error)?;
        options.elicitation_connection_id = Some(connection);
        Ok(())
    }

    pub(super) fn respond_permission(
        &self,
        session_id: &str,
        request_id: &str,
        approved: bool,
    ) -> Result<(), NativeUiError> {
        self.inner
            .runtime
            .permissions()
            .respond_native(session_id, &connection(), request_id, approved)
            .map_err(ui_error)
    }

    pub(super) fn respond_question(
        &self,
        session_id: &str,
        request_id: &str,
        answer: &str,
    ) -> Result<(), NativeUiError> {
        let pending = self
            .inner
            .runtime
            .elicitation_coordinator()
            .pending_for_native_session(session_id, &connection());
        let (pending, question) = pending
            .iter()
            .find_map(|pending| {
                let question_id = request_id.strip_prefix(&format!("{}#", pending.request_id))?;
                pending
                    .questions
                    .iter()
                    .find(|question| question.id == question_id)
                    .map(|question| (pending, question))
            })
            .ok_or_else(|| ui_error("问题已经结束或不属于当前会话"))?;
        let value = serde_json::from_str::<serde_json::Value>(answer)
            .unwrap_or_else(|_| serde_json::Value::String(answer.to_owned()));
        let values: Vec<String> = match value {
            serde_json::Value::String(value) => vec![value],
            serde_json::Value::Array(values) => values
                .into_iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| ui_error("问题答案必须为字符串"))
                })
                .collect::<Result<_, _>>()?,
            _ => return Err(ui_error("问题答案格式无效")),
        };
        if !question.multi_select && values.len() > 1 {
            return Err(ui_error("此问题只允许选择一个答案"));
        }
        if values.iter().any(|value| {
            value.len() > 16 * 1024
                || (!question.allow_custom
                    && !question.options.iter().any(|option| option.label == *value))
        }) {
            return Err(ui_error("答案不符合当前问题选项"));
        }
        let mut all = self.inner.question_answers.lock().map_err(ui_error)?;
        // 未结束请求只保存本轮有限答案；结束或取消的账本由 Runtime 查询清除。
        let ids: std::collections::HashSet<_> = self
            .inner
            .runtime
            .elicitation_coordinator()
            .pending_views_for_connection(&connection())
            .into_iter()
            .map(|view| view.request_id)
            .collect();
        all.retain(|id, _| ids.contains(id));
        let answers = all.entry(pending.request_id.clone()).or_default();
        answers.insert(question.id.clone(), serde_json::json!(values));
        if pending
            .questions
            .iter()
            .all(|question| answers.contains_key(&question.id))
        {
            let body = serde_json::json!({"answers": pending.questions.iter().map(|question| serde_json::json!({
                "id": question.id, "values": answers[&question.id]
            })).collect::<Vec<_>>()});
            self.inner
                .runtime
                .elicitation_coordinator()
                .respond_native(&connection(), &pending.request_id, &body.to_string())
                .map_err(ui_error)?;
            all.remove(&pending.request_id);
        }
        let _ = self.inner.local_changes.send((session_id.to_owned(), None));
        Ok(())
    }

    pub(super) fn pending_messages(&self, session_id: &str) -> Vec<UiMessage> {
        let permissions = self
            .inner
            .runtime
            .permissions()
            .pending_for_native_session(session_id, &connection());
        let questions = self
            .inner
            .runtime
            .elicitation_coordinator()
            .pending_for_native_session(session_id, &connection());
        let mut messages = Vec::new();
        for permission in permissions {
            messages.push(UiMessage {
                message_id: format!("approval:{}", permission.interaction_id),
                turn_id: None,
                role: MessageRole::System,
                blocks: vec![MessageBlock::Approval(ToolApprovalFact {
                    request_id: permission.interaction_id,
                    name: permission.tool_name,
                    arguments_json: super::projection::bounded_text(
                        &permission.detail.to_string(),
                        16 * 1024,
                    ),
                    effect: ToolEffect::ChangesState,
                    can_approve: true,
                    can_deny: true,
                    expires_at_unix_ms: None,
                })],
                feedback: None,
                created_at_unix_ms: Some(permission.created_at_unix_ms),
            });
        }
        let answers = self
            .inner
            .question_answers
            .lock()
            .expect("原生问答草稿锁已损坏");
        for pending in questions {
            for question in pending.questions {
                let request_id = format!("{}#{}", pending.request_id, question.id);
                messages.push(UiMessage {
                    message_id: format!("question:{request_id}"),
                    turn_id: None,
                    role: MessageRole::System,
                    blocks: vec![MessageBlock::Question(QuestionFact {
                        request_id,
                        prompt_markdown: super::projection::bounded_text(
                            &question.prompt,
                            64 * 1024,
                        ),
                        choices: question
                            .options
                            .into_iter()
                            .map(|option| QuestionChoice {
                                id: option.label.clone(),
                                label: super::projection::bounded_text(&option.label, 16 * 1024),
                                description: option.description.map(|description| {
                                    super::projection::bounded_text(&description, 16 * 1024)
                                }),
                            })
                            .collect(),
                        multi_select: question.multi_select,
                        allow_freeform: question.allow_custom,
                        answered: answers
                            .get(&pending.request_id)
                            .is_some_and(|answers| answers.contains_key(&question.id)),
                        answer: answers
                            .get(&pending.request_id)
                            .and_then(|answers| answers.get(&question.id))
                            .map(|answer| {
                                super::projection::bounded_text(&answer.to_string(), 64 * 1024)
                            }),
                    })],
                    feedback: None,
                    created_at_unix_ms: Some(pending.created_at_unix_ms),
                });
            }
        }
        messages
    }
}
