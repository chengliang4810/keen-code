use serde::{Deserialize, Serialize};

use crate::error::ModelError;
use crate::redaction::redact_error_secrets_bounded;

/// 上游结束原因名进入错误文本前允许保留的最大 UTF-8 字节数。
const MAX_FAILURE_REASON_BYTES: usize = 256;

/// 一次模型调用归一化后的 Token 用量。
///
/// 每个字段使用 `Option<u64>`：`None` 表示远端没有报告，`Some(0)` 表示远端明确报告为零。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// 输入 Token 总量（含缓存读取和写入）；由 Adapter 归一化，未报告时为 `None`。
    pub input_tokens: Option<u64>,
    /// 输出 Token；未报告时为 `None`。
    pub output_tokens: Option<u64>,
    /// 输出中用于推理的 Token；未报告时为 `None`。
    pub reasoning_tokens: Option<u64>,
    /// 从远端提示缓存读取的 Token；未报告时为 `None`。
    pub cache_read_tokens: Option<u64>,
    /// 本次写入远端提示缓存的 Token；未报告时为 `None`。
    pub cache_write_tokens: Option<u64>,
    /// 远端明确报告的总 Token；未报告时为 `None`。
    pub total_tokens: Option<u64>,
}

impl TokenUsage {
    /// 创建所有字段均为“未报告”的用量。
    pub fn unknown() -> Self {
        Self::default()
    }

    /// 返回至少一个字段是否由远端明确报告。
    pub fn is_reported(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.total_tokens.is_some()
    }

    /// 使用新快照中已报告的字段更新当前值，并保留新快照缺失的旧字段。
    pub fn update_from(&mut self, newer: &Self) {
        update_if_some(&mut self.input_tokens, newer.input_tokens);
        update_if_some(&mut self.output_tokens, newer.output_tokens);
        update_if_some(&mut self.reasoning_tokens, newer.reasoning_tokens);
        update_if_some(&mut self.cache_read_tokens, newer.cache_read_tokens);
        update_if_some(&mut self.cache_write_tokens, newer.cache_write_tokens);
        update_if_some(&mut self.total_tokens, newer.total_tokens);
    }
}

fn update_if_some(target: &mut Option<u64>, newer: Option<u64>) {
    if newer.is_some() {
        *target = newer;
    }
}

/// 计算一次模型调用的提示词缓存命中率：`cache_read / input`。
///
/// 口径依据（以各 Adapter 的归一化行为为准）：三个协议归一化后的
/// `input_tokens` 都已经包含缓存部分——Anthropic Messages 在解码用量时把
/// `cache_read_input_tokens` 与 `cache_creation_input_tokens` 显式加进输入总量；
/// OpenAI Chat Completions 与 Responses 的 `prompt_tokens`/`input_tokens` 本身
/// 就是总输入，`cached_tokens` 只是其中的明细子集。因此分母直接取
/// `input_tokens`，不再叠加 `cache_write_tokens`。
///
/// 缓存读取或输入总量未报告（`None`）、或输入总量为零时返回 `None`，
/// 不把“未报告”臆造为零；远端显式报告 `cache_read = 0` 时得到 `Some(0.0)`。
/// 结果大于 1 表示 provider 报告自相矛盾（cache_read 超过总输入），
/// 消费方应视为异常数据而非钳制。
pub fn cache_hit_rate(usage: &TokenUsage) -> Option<f64> {
    let cache_read = usage.cache_read_tokens? as f64;
    let input = usage.input_tokens?;
    if input == 0 {
        return None;
    }
    Some(cache_read / input as f64)
}

/// 模型结束当前响应的统一原因。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StopReason {
    /// 模型正常完成响应。
    Completed,
    /// 模型请求运行一个或多个工具。
    ToolUse,
    /// 响应达到当前请求或端点的输出上限。
    MaxOutputTokens,
    /// 响应被内容安全策略截断。
    ContentFilter,
    /// 响应被调用方取消。
    Cancelled,
    /// 端点返回了统一层暂未定义的结束原因。
    Other {
        /// 经脱敏并规范化后的原始原因名称。
        reason: String,
    },
}

impl StopReason {
    /// 把端点自报失败的结束原因归一为带上游原因的错误。
    ///
    /// 部分兼容端点在流式响应的最后一个 chunk 里用 `finish_reason: "error"`
    /// 之类的原因名报告上游失败：此时 HTTP 状态仍是 200，响应也没有顶层
    /// `error` 对象。这类原因名是端点的明确失败事实，必须与「缺少终止原因」
    /// 区分开，后者只是协议信息缺失。按非字母数字边界切词匹配，因此
    /// `server_error`、`internal_error` 等带前后缀的写法同样命中，而
    /// `pause_turn` 这类正常信号不会命中。
    ///
    /// 中性原因返回 `None`，由调用方按各自语义处理。原因名来自上游，进入展示
    /// 文本前执行有界脱敏并清理控制字符，避免异常端点借结束原因回显凭据。
    pub fn provider_failure_error(&self) -> Option<ModelError> {
        let Self::Other { reason } = self else {
            return None;
        };
        let names_failure = reason
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| {
                matches!(
                    token.to_ascii_lowercase().as_str(),
                    "error" | "failed" | "failure"
                )
            });
        if !names_failure {
            return None;
        }
        let redacted = redact_error_secrets_bounded(reason.trim(), MAX_FAILURE_REASON_BYTES);
        let display = redacted
            .chars()
            .map(|character| if character.is_control() { ' ' } else { character })
            .collect::<String>();
        Some(ModelError::ProviderUnavailable {
            message: format!("上游提前终止响应（结束原因 {display}）"),
            status_code: None,
            retryable: true,
        })
    }
}
