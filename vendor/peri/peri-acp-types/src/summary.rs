//! Migrated DTOs — originally defined in `peri-acp/src/event/dto.rs`,
//! re-exported from `peri-acp/src/event/mod.rs` for backward compatibility.
//!
//! These types are the public ACP contract between TUI/IDEs and the agent
//! runtime. Do not depend on `peri_agent` types directly — use these DTOs.

use serde::{Deserialize, Serialize};

// ─── Migrated DTOs (verbatim from peri-acp/src/event/dto.rs) ─────────────────

/// Compact 完成后保留的文件信息（DTO）
///
/// 替代 `peri_agent::agent::events::CompactFileInfo`，TUI/IDE 消费方应使用本类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactFileInfoDto {
    /// 文件路径
    pub path: String,
    /// 文件行数
    pub lines: usize,
}

/// Token 使用量（DTO，对应 `peri_model::TokenUsage`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TokenUsageDto {
    /// 总输入 token（含缓存 token）
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 输出 Token 中由 Provider 报告的推理 Token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u32>,
    /// 写入缓存的 token 数（仅 Anthropic 有意义，OpenAI 始终 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// 从缓存读取的 token 数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    /// API 提供商返回的请求 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// LLM 响应停止原因（DTO，对应 `peri_model::StopReason`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReasonDto {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other { value: String },
}

/// Todo 项状态（DTO，替代 `peri_middlewares::tools::todo::TodoStatus`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatusDto {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Todo 项（DTO，替代 `peri_middlewares::tools::todo::TodoItem`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TodoItemDto {
    pub content: String,
    #[serde(
        default,
        rename = "activeForm",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_form: Option<String>,
    #[serde(default)]
    pub status: TodoStatusDto,
}
