//! 把权威工具结果无损投影为标准 ACP 输出和精确的 KeenCode 终态元数据。

use keencode_acp::schema;
use keencode_resources::{PersistedToolResult, ToolCompletionStatus};

use super::AgentRuntimeError;

/// 标准 ACP 无取消枚举；保持其状态合法，并用自有元数据保留不可丢失的真实终态。
pub(super) fn completed_fields(
    outcome: ToolCompletionStatus,
    result: &PersistedToolResult,
) -> Result<schema::ToolCallUpdateFields, AgentRuntimeError> {
    let status = match outcome {
        ToolCompletionStatus::Succeeded => schema::ToolCallStatus::Completed,
        ToolCompletionStatus::Failed
        | ToolCompletionStatus::Cancelled
        | ToolCompletionStatus::SideEffectUnknown => schema::ToolCallStatus::Failed,
    };
    let raw =
        serde_json::to_value(result).map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    // 正文只保留在当前唯一结果结构中；重复复制到 content 会放大转义后的投递大小。
    // 图片和 Artifact 引用同样原样保留，不在展示投影中展开大结果。
    Ok(schema::ToolCallUpdateFields::new()
        .status(status)
        .raw_output(raw))
}

/// 元数据位于标准 ToolCallUpdate 顶层，不扩展 ACP 的状态枚举或 rawOutput 形状。
pub(super) fn outcome_meta(
    outcome: ToolCompletionStatus,
) -> Result<schema::Meta, AgentRuntimeError> {
    let precise =
        serde_json::to_value(outcome).map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    Ok(serde_json::Map::from_iter([(
        "keencode/toolOutcome".to_owned(),
        precise,
    )]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_resources::ToolResultPart;

    /// 用真实信封序列化验证单段内联上限，转义不能被重复正文放大到投递边界之外。
    #[test]
    fn escaped_inline_result_fits_delivery_budget() {
        let result = PersistedToolResult {
            tool_call_id: "call-budget".to_owned(),
            content: vec![ToolResultPart::Text {
                text: "\\".repeat(64 * 1024),
            }],
            is_error: false,
        };
        let update = schema::ToolCallUpdate::new(
            result.tool_call_id.clone(),
            completed_fields(ToolCompletionStatus::Succeeded, &result).unwrap(),
        )
        .meta(Some(outcome_meta(ToolCompletionStatus::Succeeded).unwrap()));
        let envelope = keencode_acp::SessionUpdateDeliveryEnvelope::new(
            "session-budget",
            Some("turn-budget".to_owned()),
            Some("root".to_owned()),
            1,
            1,
            schema::SessionUpdate::ToolCallUpdate(update),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert!(
            bytes.len() <= keencode_acp::SessionUpdateDeliveryLimits::default().max_bytes(),
            "工具投递实际为 {} 字节",
            bytes.len()
        );
        keencode_acp::SessionUpdateDeliveryEnvelope::decode_raw(&bytes).unwrap();
    }

    /// 多段文本加上实际 Diff 内容后仍只编码一份结果，且标准内容不会覆盖原始输出。
    #[test]
    fn multipart_text_and_diff_fit_delivery_budget_without_loss() {
        let result = PersistedToolResult {
            tool_call_id: "call-multipart".to_owned(),
            content: vec![
                ToolResultPart::Text {
                    text: "\\".repeat(32 * 1024),
                },
                ToolResultPart::Text {
                    text: "\"".repeat(32 * 1024),
                },
            ],
            is_error: false,
        };
        let fields = completed_fields(ToolCompletionStatus::Succeeded, &result)
            .unwrap()
            .content(vec![schema::ToolCallContent::Diff(
                schema::Diff::new("result.txt", "after".repeat(4 * 1024))
                    .old_text("before".repeat(4 * 1024)),
            )]);
        let envelope = keencode_acp::SessionUpdateDeliveryEnvelope::new(
            "session-budget",
            Some("turn-budget".to_owned()),
            Some("root".to_owned()),
            1,
            1,
            schema::SessionUpdate::ToolCallUpdate(schema::ToolCallUpdate::new(
                "call-multipart",
                fields,
            )),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let recovered = keencode_acp::SessionUpdateDeliveryEnvelope::decode_raw(&bytes).unwrap();
        let value = serde_json::to_value(recovered).unwrap();
        assert_eq!(
            value["update"]["rawOutput"],
            serde_json::to_value(result).unwrap()
        );
        assert_eq!(
            value["update"]["content"][0]["newText"],
            "after".repeat(4 * 1024)
        );
    }

    /// 四种真实终态均保留，模型可见错误标志与界面取消语义互不混淆。
    #[test]
    fn exact_outcomes_preserve_standard_status_and_unique_raw_result() {
        for (outcome, precise, status) in [
            (ToolCompletionStatus::Succeeded, "succeeded", "completed"),
            (ToolCompletionStatus::Failed, "failed", "failed"),
            (ToolCompletionStatus::Cancelled, "cancelled", "failed"),
            (
                ToolCompletionStatus::SideEffectUnknown,
                "side_effect_unknown",
                "failed",
            ),
        ] {
            let result = PersistedToolResult {
                tool_call_id: "call-native".to_owned(),
                content: vec![ToolResultPart::Text {
                    text: "第一行\n第二行".to_owned(),
                }],
                is_error: outcome != ToolCompletionStatus::Succeeded,
            };
            let update = schema::ToolCallUpdate::new(
                result.tool_call_id.clone(),
                completed_fields(outcome, &result).unwrap(),
            )
            .meta(Some(outcome_meta(outcome).unwrap()));
            let value = serde_json::to_value(update).unwrap();
            assert_eq!(value["status"], status);
            assert_eq!(value["_meta"]["keencode/toolOutcome"], precise);
            assert_eq!(value["rawOutput"], serde_json::to_value(&result).unwrap());
            assert!(
                value.get("content").is_none(),
                "文本不得在标准内容中再复制一份"
            );
        }
    }
}
