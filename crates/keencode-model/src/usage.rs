use serde::{Deserialize, Serialize};

/// 一次模型调用归一化后的 Token 用量。
///
/// 每个字段使用 `Option<u64>`：`None` 表示远端没有报告，`Some(0)` 表示远端明确报告为零。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// 输入 Token；未报告时为 `None`。
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
