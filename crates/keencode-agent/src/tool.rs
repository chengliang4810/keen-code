//! Agent Runtime 的工具与注册表异步边界。

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io::{self, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use keencode_model::{ImageSource, ToolDefinition, ToolResult, ToolResultContent};
use serde_json::Value;

use crate::{AgentId, SessionId, ToolCallId, ToolEffect, TurnCancellation, TurnId};

/// 工具输出进入生命周期事件、Hook 与 Transcript 前统一采用的不可放宽硬上限。
pub const TOOL_OUTPUT_LIMITS: ToolOutputLimits = ToolOutputLimits {
    max_content_blocks: 64,
    max_text_bytes: 512 * 1_024,
    max_image_source_bytes: 12 * 1_024 * 1_024,
    max_image_decoded_bytes: 8 * 1_024 * 1_024,
    max_base64_characters: 11_184_812,
    max_remote_url_bytes: 16 * 1_024,
    max_data_url_bytes: 12 * 1_024 * 1_024,
    max_media_type_bytes: 128,
    max_result_json_bytes: 12 * 1_024 * 1_024,
    max_round_content_blocks: 64,
    max_round_model_visible_bytes: 16 * 1_024 * 1_024,
    max_round_json_bytes: 32 * 1_024 * 1_024,
    max_tool_error_code_bytes: 128,
    max_tool_error_message_bytes: 4 * 1_024,
    max_post_hook_additions: 64,
    max_post_hook_model_visible_bytes: 64 * 1_024,
    max_post_hook_json_bytes: 96 * 1_024,
};

/// 工具输出及其 PostHook 内容在进程内允许占用的固定容量边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolOutputLimits {
    /// 单个工具结果允许返回的最大文本或图片块数量。
    pub max_content_blocks: usize,
    /// 单个文本块允许包含的最大 UTF-8 字节数。
    pub max_text_bytes: usize,
    /// 单张图片的 Base64、data URL 或远端 URL 来源允许占用的最大字节数。
    pub max_image_source_bytes: usize,
    /// 单张内联图片解码后允许占用的最大原始字节数。
    pub max_image_decoded_bytes: usize,
    /// 单张内联图片允许包含的最大 Base64 ASCII 字符数。
    pub max_base64_characters: usize,
    /// 单张远端图片 URL 允许占用的最大 UTF-8 字节数。
    pub max_remote_url_bytes: usize,
    /// 单张 Base64 data URL 允许占用的最大 ASCII 字节数。
    pub max_data_url_bytes: usize,
    /// 单张内联图片媒体类型允许占用的最大 UTF-8 字节数。
    pub max_media_type_bytes: usize,
    /// 单个完整 ToolResult 序列化为 JSON 后允许占用的最大字节数。
    pub max_result_json_bytes: usize,
    /// 同一工具 Round 的全部 ToolResult 与 PostHook 消息最多允许包含的内容块数量。
    pub max_round_content_blocks: usize,
    /// 同一工具 Round 的全部 ToolResult 与 PostHook 消息允许占用的最大模型可见字节数。
    pub max_round_model_visible_bytes: usize,
    /// 同一工具 Round 的全部 ToolResult 与 PostHook 消息编码为 JSON 后允许占用的最大字节数。
    pub max_round_json_bytes: usize,
    /// 工具错误码允许占用的最大 UTF-8 字节数。
    pub max_tool_error_code_bytes: usize,
    /// 工具错误说明允许占用的最大 UTF-8 字节数。
    pub max_tool_error_message_bytes: usize,
    /// 同一工具 Round 的全部 PostHook 最多允许新增的上下文项数。
    pub max_post_hook_additions: usize,
    /// 同一工具 Round 的全部 PostHook 新增消息允许占用的最大模型可见字节数。
    pub max_post_hook_model_visible_bytes: usize,
    /// 同一工具 Round 的全部 PostHook 新增消息编码为 JSON 后允许占用的最大字节数。
    pub max_post_hook_json_bytes: usize,
}

/// 工具输出超过安全边界时供控制面和模型稳定识别的机器错误码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolOutputErrorCode {
    /// 只读工具输出超过单结果或 Round 聚合硬上限。
    LimitExceeded,
    /// 状态变更工具已经执行，但输出超过硬上限且无法证明副作用未发生。
    SideEffectLimitExceeded,
}

impl ToolOutputErrorCode {
    /// 返回跨日志、Hook 与控制面保持稳定的 ASCII 机器错误码。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LimitExceeded => "tool_output_limit_exceeded",
            Self::SideEffectLimitExceeded => "tool_output_limit_exceeded_side_effect",
        }
    }
}

impl fmt::Display for ToolOutputErrorCode {
    /// 输出稳定机器错误码。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 固定且不包含原工具输出的只读工具超限结果。
///
/// 仅在截断落盘通道不可用（未接入工件目录或保存失败，证据无法保全）时
/// 作为保守回退使用；正常路径下超限输出会被截断并落盘后回流。
pub(crate) const TOOL_OUTPUT_LIMIT_RESULT: &str =
    "tool_output_limit_exceeded：工具输出超过安全上限；不可自动重试";

/// 固定且不包含原工具输出的副作用工具超限结果。
///
/// 仅在截断落盘通道不可用（未接入工件目录或保存失败，证据无法保全）时
/// 作为保守回退使用；正常路径下超限输出会被截断并落盘后回流，Turn 继续。
pub(crate) const SIDE_EFFECT_TOOL_OUTPUT_LIMIT_RESULT: &str =
    "tool_output_limit_exceeded_side_effect：工具输出超过安全上限；副作用可能已发生；禁止自动重试";

/// 工具错误字段不可信或超过硬上限时使用的固定机器错误码。
pub(crate) const INVALID_TOOL_ERROR_CODE: &str = "invalid_tool_error";

/// 工具错误字段不可信或超过硬上限时使用的固定安全说明。
pub(crate) const INVALID_TOOL_ERROR_MESSAGE: &str = "工具错误字段为空或超过安全上限";

/// 每个尚未归一结果为固定错误文本预留的模型可见字节数。
const ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES: usize = 256;

/// 每个尚未归一结果为最坏转义调用 ID 和固定错误文本预留的 JSON 字节数。
const ROUND_PENDING_RESULT_JSON_RESERVE_BYTES: usize = 8 * 1_024;

/// 工具在同一模型 Round 内允许采用的执行方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolConcurrency {
    /// 只读且实现保证线程安全时可以与相邻同类调用并行。
    ParallelReadOnly,
    /// 必须作为顺序屏障独占执行。
    Exclusive,
}

/// 一次工具执行可访问的稳定运行上下文。
#[derive(Clone, Debug)]
pub struct ToolContext {
    /// 工具调用所属的根 Session。
    pub session_id: SessionId,
    /// 工具调用所属的用户 Turn。
    pub turn_id: TurnId,
    /// 发起调用的根 Agent 或单层子 Agent。
    pub source_agent_id: AgentId,
    /// Runner 从真实模型 ToolCall 冻结的可信调用标识，工具输入不能覆盖。
    pub tool_call_id: ToolCallId,
    /// 工具必须主动观察的取消令牌。
    pub cancellation: TurnCancellation,
}

/// 工具成功完成后返回给模型的有序内容。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolOutput {
    /// 文本或图片结果；空列表表示成功但没有可见输出。
    pub content: Vec<ToolResultContent>,
}

impl ToolOutput {
    /// 创建只包含 UTF-8 文本的成功结果。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::Text { text: text.into() }],
        }
    }
}

/// 一个已经验证、可安全复制到权威事件和 Transcript 的工具结果容量快照。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ToolResultFootprint {
    /// 当前完整 ToolResult 包含的文本或图片块数量。
    pub(crate) content_blocks: usize,
    /// 文本、远端 URL 或内联图片原始内容占用的模型可见字节数。
    pub(crate) model_visible_bytes: usize,
    /// 完整 ToolResult 使用 serde JSON 编码后的精确字节数。
    pub(crate) json_bytes: usize,
}

/// 工具成功输出未能通过统一归一边界的稳定分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolOutputRejection {
    /// 输出不满足 Provider 中立图片或 Base64 结构约束。
    Invalid,
    /// 输出任一容量维度超过固定硬上限。
    LimitExceeded,
}

/// 一个工具 Round 已经占用的结果与 PostHook 聚合容量。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ToolRoundOutputBudget {
    /// 当前 Round 已经接纳的 ToolResult 内容块数量。
    content_blocks: usize,
    /// 当前 Round 已经接纳的模型可见字节数。
    model_visible_bytes: usize,
    /// 当前 Round 已经接纳的 JSON 编码字节数。
    json_bytes: usize,
    /// 尚未接纳且必须至少能写入固定失败结果的工具调用数量。
    pending_results: usize,
}

impl ToolRoundOutputBudget {
    /// 判断冻结调用数是否至少能为每项保留一个固定错误结果。
    pub(crate) fn can_reserve_results(result_count: usize) -> bool {
        result_count <= TOOL_OUTPUT_LIMITS.max_round_content_blocks
            && result_count
                .checked_mul(ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES)
                .is_some_and(|bytes| bytes <= TOOL_OUTPUT_LIMITS.max_round_model_visible_bytes)
            && result_count
                .checked_mul(ROUND_PENDING_RESULT_JSON_RESERVE_BYTES)
                .is_some_and(|bytes| bytes <= TOOL_OUTPUT_LIMITS.max_round_json_bytes)
    }

    /// 按本 Round 的冻结工具调用数创建带固定失败结果预留的聚合预算。
    pub(crate) const fn new(result_count: usize) -> Self {
        Self {
            content_blocks: 0,
            model_visible_bytes: 0,
            json_bytes: 0,
            pending_results: result_count,
        }
    }

    /// 原子尝试接纳一个 ToolResult，并为所有后续调用保留固定失败容量。
    pub(crate) fn try_charge_result(&mut self, footprint: ToolResultFootprint) -> bool {
        let Some(pending_results) = self.pending_results.checked_sub(1) else {
            return false;
        };
        if !self.fits_with_pending_reserve(
            footprint.content_blocks,
            footprint.model_visible_bytes,
            footprint.json_bytes,
            pending_results,
        ) {
            return false;
        }
        self.content_blocks += footprint.content_blocks;
        self.model_visible_bytes += footprint.model_visible_bytes;
        self.json_bytes += footprint.json_bytes;
        self.pending_results = pending_results;
        true
    }

    /// 原子尝试接纳 PostHook 内容块与编码容量，同时保留全部尚未生成的固定结果容量。
    pub(crate) fn try_charge_post_hook(
        &mut self,
        additional_content_blocks: usize,
        additional_model_visible_bytes: usize,
        additional_json_bytes: usize,
    ) -> bool {
        if !self.fits_with_pending_reserve(
            additional_content_blocks,
            additional_model_visible_bytes,
            additional_json_bytes,
            self.pending_results,
        ) {
            return false;
        }
        self.content_blocks += additional_content_blocks;
        self.model_visible_bytes += additional_model_visible_bytes;
        self.json_bytes += additional_json_bytes;
        true
    }

    /// 原子回滚已经接纳但因后续容量失败必须整批丢弃的 PostHook 占用。
    pub(crate) fn try_release_post_hook(
        &mut self,
        content_blocks: usize,
        model_visible_bytes: usize,
        json_bytes: usize,
    ) -> bool {
        let Some(remaining_content_blocks) = self.content_blocks.checked_sub(content_blocks) else {
            return false;
        };
        let Some(remaining_model_visible_bytes) =
            self.model_visible_bytes.checked_sub(model_visible_bytes)
        else {
            return false;
        };
        let Some(remaining_json_bytes) = self.json_bytes.checked_sub(json_bytes) else {
            return false;
        };
        self.content_blocks = remaining_content_blocks;
        self.model_visible_bytes = remaining_model_visible_bytes;
        self.json_bytes = remaining_json_bytes;
        true
    }

    /// 返回下一个结果在为剩余固定失败结果预留容量后可占用的上限三元组
    /// （内容块数、模型可见字节数、JSON 编码字节数）。
    ///
    /// 该结果自身仍占用一个待生成结果的固定失败预留，因此预留按
    /// `pending_results - 1` 计算；三个维度与
    /// [`ToolRoundOutputBudget::try_charge_result`] 的判定完全对齐，按此
    /// 上限截断后的结果必定能被接纳。
    pub(crate) fn next_result_capacity(&self) -> (usize, usize, usize) {
        let remaining_pending = self.pending_results.saturating_sub(1);
        let remaining_blocks = self.content_blocks.saturating_add(remaining_pending);
        let model_reserve =
            remaining_pending.saturating_mul(ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES);
        let json_reserve =
            remaining_pending.saturating_mul(ROUND_PENDING_RESULT_JSON_RESERVE_BYTES);
        (
            TOOL_OUTPUT_LIMITS
                .max_round_content_blocks
                .saturating_sub(remaining_blocks),
            TOOL_OUTPUT_LIMITS
                .max_round_model_visible_bytes
                .saturating_sub(self.model_visible_bytes.saturating_add(model_reserve)),
            TOOL_OUTPUT_LIMITS
                .max_round_json_bytes
                .saturating_sub(self.json_bytes.saturating_add(json_reserve)),
        )
    }

    /// 检查新增容量和所有待生成固定结果能否同时留在 Round 硬上限内。
    fn fits_with_pending_reserve(
        &self,
        additional_content_blocks: usize,
        additional_model_visible_bytes: usize,
        additional_json_bytes: usize,
        pending_results: usize,
    ) -> bool {
        let Some(content_blocks) = self.content_blocks.checked_add(additional_content_blocks)
        else {
            return false;
        };
        let Some(model_visible_bytes) = self
            .model_visible_bytes
            .checked_add(additional_model_visible_bytes)
        else {
            return false;
        };
        let Some(json_bytes) = self.json_bytes.checked_add(additional_json_bytes) else {
            return false;
        };
        let Some(model_reserve) =
            pending_results.checked_mul(ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES)
        else {
            return false;
        };
        let Some(json_reserve) =
            pending_results.checked_mul(ROUND_PENDING_RESULT_JSON_RESERVE_BYTES)
        else {
            return false;
        };
        content_blocks
            .checked_add(pending_results)
            .is_some_and(|total| total <= TOOL_OUTPUT_LIMITS.max_round_content_blocks)
            && model_visible_bytes
                .checked_add(model_reserve)
                .is_some_and(|total| total <= TOOL_OUTPUT_LIMITS.max_round_model_visible_bytes)
            && json_bytes
                .checked_add(json_reserve)
                .is_some_and(|total| total <= TOOL_OUTPUT_LIMITS.max_round_json_bytes)
    }
}

/// 工具成功输出未能通过统一归一边界的稳定分类；超限时交还原完整输出。
#[derive(Debug, PartialEq)]
pub(crate) enum ToolOutputValidation {
    /// 输出不满足 Provider 中立图片或 Base64 结构约束。
    Invalid,
    /// 输出任一容量维度超过固定硬上限；携带原始完整输出供截断落盘复用。
    LimitExceeded { output: ToolOutput },
}

/// 先验证完整成功输出，再计算后续所有消费者复用的唯一结果容量。
pub(crate) fn validate_tool_output(
    tool_call_id: String,
    output: ToolOutput,
) -> Result<(ToolResult, ToolResultFootprint), ToolOutputValidation> {
    if output.content.len() > TOOL_OUTPUT_LIMITS.max_content_blocks {
        return Err(ToolOutputValidation::LimitExceeded { output });
    }
    let result = ToolResult::new(tool_call_id, output.content, false);
    match measure_tool_result(&result) {
        Ok(footprint) => Ok((result, footprint)),
        Err(ToolOutputRejection::Invalid) => Err(ToolOutputValidation::Invalid),
        Err(ToolOutputRejection::LimitExceeded) => Err(ToolOutputValidation::LimitExceeded {
            output: ToolOutput {
                content: result.content,
            },
        }),
    }
}

/// 判断一个图片来源是否无法通过图片硬上限校验。
///
/// 图片或 data URL 自身超限不属于文本截断的修复范围，落盘图片不在
/// 输出截断机制的范围内，此时维持既有拒绝语义。
pub(crate) fn image_source_over_limit(source: &ImageSource) -> bool {
    measure_image_source(source).is_err()
}

/// 验证完整 ToolResult 并返回模型可见与 JSON 编码容量；不会保留任何前缀副本。
pub(crate) fn measure_tool_result(
    result: &ToolResult,
) -> Result<ToolResultFootprint, ToolOutputRejection> {
    result
        .validate()
        .map_err(|_| ToolOutputRejection::Invalid)?;
    if result.content.len() > TOOL_OUTPUT_LIMITS.max_content_blocks {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    let mut model_visible_bytes = 0_usize;
    for item in &result.content {
        let item_bytes = match item {
            ToolResultContent::Text { text } => {
                if text.len() > TOOL_OUTPUT_LIMITS.max_text_bytes {
                    return Err(ToolOutputRejection::LimitExceeded);
                }
                text.len()
            }
            ToolResultContent::Image { image } => measure_image_source(&image.source)?,
        };
        model_visible_bytes = model_visible_bytes
            .checked_add(item_bytes)
            .ok_or(ToolOutputRejection::LimitExceeded)?;
    }
    let json_bytes = serialized_json_bytes(result);
    if json_bytes > TOOL_OUTPUT_LIMITS.max_result_json_bytes {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    Ok(ToolResultFootprint {
        content_blocks: result.content.len(),
        model_visible_bytes,
        json_bytes,
    })
}

/// 返回图片来源进入模型时占用的原始或解码字节数。
fn measure_image_source(source: &ImageSource) -> Result<usize, ToolOutputRejection> {
    match source {
        ImageSource::Base64 { media_type, data } => {
            validate_media_type(media_type)?;
            let source_bytes = media_type
                .len()
                .checked_add(data.len())
                .ok_or(ToolOutputRejection::LimitExceeded)?;
            if source_bytes > TOOL_OUTPUT_LIMITS.max_image_source_bytes
                || data.len() > TOOL_OUTPUT_LIMITS.max_base64_characters
            {
                return Err(ToolOutputRejection::LimitExceeded);
            }
            strict_base64_decoded_bytes(data)
        }
        ImageSource::Url { url } if is_data_url(url) => measure_data_url(url),
        ImageSource::Url { url } => {
            validate_remote_image_url(url)?;
            Ok(url.len())
        }
    }
}

/// 校验远端图片地址具有明确 HTTP(S) authority，且不包含解析器可歧义解释的语法。
fn validate_remote_image_url(url: &str) -> Result<(), ToolOutputRejection> {
    if url.len() > TOOL_OUTPUT_LIMITS.max_remote_url_bytes
        || url.len() > TOOL_OUTPUT_LIMITS.max_image_source_bytes
    {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    if !url.is_ascii()
        || url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace() || byte == b'\\')
    {
        return Err(ToolOutputRejection::Invalid);
    }
    let authority_start = if has_ascii_prefix(url, "http://") {
        "http://".len()
    } else if has_ascii_prefix(url, "https://") {
        "https://".len()
    } else {
        return Err(ToolOutputRejection::Invalid);
    };
    let remainder = &url[authority_start..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return Err(ToolOutputRejection::Invalid);
    }
    validate_remote_authority(authority)?;
    validate_percent_encoding(&remainder[authority_end..])
}

/// 判断地址是否为有界、无 userinfo 且不存在解析歧义的规范 HTTP(S) 远端图片 URL。
pub fn is_canonical_remote_image_url(url: &str) -> bool {
    validate_remote_image_url(url).is_ok()
}

/// 校验不含 userinfo 的 HTTP(S) authority，并拒绝模糊 IPv4、端口和主机写法。
fn validate_remote_authority(authority: &str) -> Result<(), ToolOutputRejection> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, suffix)) = bracketed.split_once(']') else {
            return Err(ToolOutputRejection::Invalid);
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(ToolOutputRejection::Invalid);
        }
        return validate_optional_port(suffix);
    }
    if authority.contains(['[', ']']) || authority.matches(':').count() > 1 {
        return Err(ToolOutputRejection::Invalid);
    }
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    validate_remote_host(host)?;
    if let Some(port) = port {
        validate_port(port)?;
    }
    Ok(())
}

/// 校验括号 IPv6 地址后只能为空或携带一个十进制端口。
fn validate_optional_port(suffix: &str) -> Result<(), ToolOutputRejection> {
    if suffix.is_empty() {
        return Ok(());
    }
    let Some(port) = suffix.strip_prefix(':') else {
        return Err(ToolOutputRejection::Invalid);
    };
    validate_port(port)
}

/// 校验远端主机为规范 IPv4、localhost 或不含空标签的 ASCII DNS 名称。
fn validate_remote_host(host: &str) -> Result<(), ToolOutputRejection> {
    if host.is_empty() || host.len() > 253 || host.contains('%') {
        return Err(ToolOutputRejection::Invalid);
    }
    if host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        let address = host
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| ToolOutputRejection::Invalid)?;
        if address.to_string() != host {
            return Err(ToolOutputRejection::Invalid);
        }
        return Ok(());
    }
    if host.split('.').all(is_legacy_ipv4_number) {
        return Err(ToolOutputRejection::Invalid);
    }
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    if !host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err(ToolOutputRejection::Invalid);
    }
    Ok(())
}

/// 识别浏览器可能按十六进制、八进制或缩写 IPv4 解释的非规范数字标签。
fn is_legacy_ipv4_number(label: &str) -> bool {
    if let Some(hexadecimal) = label
        .strip_prefix("0x")
        .or_else(|| label.strip_prefix("0X"))
    {
        return !hexadecimal.is_empty() && hexadecimal.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if label.len() > 1 && label.starts_with('0') {
        return label.bytes().all(|byte| matches!(byte, b'0'..=b'7'));
    }
    !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_digit())
}

/// 校验端口是非空且落在 u16 范围内的十进制数字。
fn validate_port(port: &str) -> Result<(), ToolOutputRejection> {
    if port.is_empty()
        || !port.bytes().all(|byte| byte.is_ascii_digit())
        || port.parse::<u16>().is_err()
    {
        return Err(ToolOutputRejection::Invalid);
    }
    Ok(())
}

/// 校验 URL 路径、查询与片段中的每个百分号都具有完整十六进制转义。
fn validate_percent_encoding(value: &str) -> Result<(), ToolOutputRejection> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let valid = bytes
                .get(index + 1..index + 3)
                .is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit));
            if !valid {
                return Err(ToolOutputRejection::Invalid);
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(())
}

/// 校验 Base64 data URL 的媒体类型、编码长度和解码后大小。
fn measure_data_url(url: &str) -> Result<usize, ToolOutputRejection> {
    if url.len() > TOOL_OUTPUT_LIMITS.max_data_url_bytes
        || url.len() > TOOL_OUTPUT_LIMITS.max_image_source_bytes
    {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    let body = url.get(5..).ok_or(ToolOutputRejection::Invalid)?;
    let (metadata, data) = body.split_once(',').ok_or(ToolOutputRejection::Invalid)?;
    let mut metadata_parts = metadata.split(';');
    let media_type = metadata_parts.next().ok_or(ToolOutputRejection::Invalid)?;
    validate_media_type(media_type)?;
    let mut saw_base64 = false;
    for parameter in metadata_parts {
        if !parameter.eq_ignore_ascii_case("base64") || saw_base64 {
            return Err(ToolOutputRejection::Invalid);
        }
        saw_base64 = true;
    }
    if !saw_base64 {
        return Err(ToolOutputRejection::Invalid);
    }
    if data.len() > TOOL_OUTPUT_LIMITS.max_base64_characters {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    strict_base64_decoded_bytes(data)
}

/// 判断图片 URL 是否使用大小写不敏感的 data scheme。
fn is_data_url(url: &str) -> bool {
    url.get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
}

/// 判断字符串是否具有大小写不敏感的 ASCII 前缀。
fn has_ascii_prefix(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

/// 校验媒体类型为无参数 ASCII `image/<restricted-name>` 且不超过固定字节上限。
fn validate_media_type(media_type: &str) -> Result<(), ToolOutputRejection> {
    if media_type.len() > TOOL_OUTPUT_LIMITS.max_media_type_bytes {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    if !is_canonical_image_media_type(media_type) {
        return Err(ToolOutputRejection::Invalid);
    }
    Ok(())
}

/// 判断媒体类型是否为可跨 Agent 与 Runtime 直接复用的规范小写 `image/<restricted-name>`。
pub fn is_canonical_image_media_type(media_type: &str) -> bool {
    if media_type.len() > TOOL_OUTPUT_LIMITS.max_media_type_bytes {
        return false;
    }
    let Some(subtype) = media_type.strip_prefix("image/") else {
        return false;
    };
    !subtype.is_empty()
        && subtype.is_ascii()
        && subtype
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && subtype
            .bytes()
            .all(|byte| is_media_type_restricted_name_byte(byte) && !byte.is_ascii_uppercase())
}

/// 判断一个 ASCII 字节能否出现在 RFC 6838 type/subtype restricted-name 中。
const fn is_media_type_restricted_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
        )
}

/// 严格校验标准有填充 Base64，并返回不分配解码缓冲区的精确原始字节数。
fn strict_base64_decoded_bytes(data: &str) -> Result<usize, ToolOutputRejection> {
    if data.is_empty() || data.len() % 4 != 0 {
        return Err(ToolOutputRejection::Invalid);
    }
    if data.len() > TOOL_OUTPUT_LIMITS.max_base64_characters {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    let bytes = data.as_bytes();
    let padding = match bytes {
        [.., b'=', b'='] => 2,
        [.., b'='] => 1,
        _ => 0,
    };
    let data_end = bytes.len().saturating_sub(padding);
    if bytes[..data_end].iter().any(|byte| !is_base64_byte(*byte))
        || bytes[data_end..].iter().any(|byte| *byte != b'=')
    {
        return Err(ToolOutputRejection::Invalid);
    }
    if padding > 0 && bytes[..data_end].contains(&b'=') {
        return Err(ToolOutputRejection::Invalid);
    }
    let trailing_value = data_end
        .checked_sub(1)
        .and_then(|index| base64_value(bytes[index]))
        .ok_or(ToolOutputRejection::Invalid)?;
    if (padding == 1 && trailing_value & 0b11 != 0)
        || (padding == 2 && trailing_value & 0b1111 != 0)
    {
        return Err(ToolOutputRejection::Invalid);
    }
    let decoded_bytes = data
        .len()
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|bytes| bytes.checked_sub(padding))
        .ok_or(ToolOutputRejection::LimitExceeded)?;
    if decoded_bytes > TOOL_OUTPUT_LIMITS.max_image_decoded_bytes {
        return Err(ToolOutputRejection::LimitExceeded);
    }
    Ok(decoded_bytes)
}

/// 判断一个 ASCII 字节能否出现在标准 Base64 的非填充区域。
const fn is_base64_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
}

/// 把一个标准 Base64 字符映射为六位值，用于校验尾部未使用位为零。
const fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// 使用只计数 Writer 获得 serde JSON 编码的精确字节数，避免复制大结果。
pub(crate) fn serialized_json_bytes<T: serde::Serialize>(value: &T) -> usize {
    let mut writer = JsonByteCounter::default();
    if serde_json::to_writer(&mut writer, value).is_err() {
        return usize::MAX;
    }
    writer.bytes
}

/// 丢弃 JSON 正文且只累计编码字节数的 Writer。
#[derive(Default)]
struct JsonByteCounter {
    /// 已被 serde JSON 写入的饱和字节数。
    bytes: usize,
}

impl Write for JsonByteCounter {
    /// 累计当前片段长度并报告完整消费，不保存正文。
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    /// 计数 Writer 没有待刷新的底层缓冲区。
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 工具校验、执行或取消时返回的可展示错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolError {
    /// 适合自动化统计的稳定错误码。
    pub code: String,
    /// 不包含凭据或未截断用户数据的安全说明。
    pub message: String,
    /// 相同输入是否适合由模型或运行时稍后重试。
    pub retryable: bool,
}

impl ToolError {
    /// 创建一个不可重试的工具错误。
    pub fn permanent(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }

    /// 创建一个可以有限重试的工具错误。
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
        }
    }
}

/// 已通过错误字段硬上限且可以安全进入结果文本与循环观察的工具错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NormalizedToolError {
    /// 非空且不超过硬上限的稳定错误码。
    pub(crate) code: String,
    /// 非空且不超过硬上限的固定或工具安全说明。
    pub(crate) message: String,
    /// 仅在原始两个字段都合规时保留的有限重试声明。
    pub(crate) retryable: bool,
}

/// 原子校验 ToolError 的 code 和 message；任一字段违规时整体替换，避免前缀泄漏。
pub(crate) fn normalize_tool_error(error: &ToolError) -> NormalizedToolError {
    let valid_code = !error.code.trim().is_empty()
        && error.code.len() <= TOOL_OUTPUT_LIMITS.max_tool_error_code_bytes;
    let valid_message = !error.message.trim().is_empty()
        && error.message.len() <= TOOL_OUTPUT_LIMITS.max_tool_error_message_bytes;
    if valid_code && valid_message {
        return NormalizedToolError {
            code: error.code.clone(),
            message: error.message.clone(),
            retryable: error.retryable,
        };
    }
    NormalizedToolError {
        code: INVALID_TOOL_ERROR_CODE.to_owned(),
        message: INVALID_TOOL_ERROR_MESSAGE.to_owned(),
        retryable: false,
    }
}

impl fmt::Display for ToolError {
    /// 输出稳定错误码和安全说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}：{}", self.code, self.message)
    }
}

impl Error for ToolError {}

/// 对象安全的异步工具执行返回值。
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>;

/// Runtime 对未声明自管超时工具施加的默认外层墙钟上限。
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

/// 把超出模型预算的完整工具输出保存到本机的同步落盘通道。
///
/// 实现必须复用命令输出先例的工件目录与命名约定（`keencode-{label}-`
/// 前缀的随机临时文件），返回的路径文本会嵌入截断标记，模型可据此用
/// Read 类方式取回完整输出。实现必须是可在任意线程安全调用的纯阻塞
/// 文件操作，不得访问异步运行时资源；运行时在工具完成后的归一阶段
/// 同步调用。
pub trait ToolOutputArtifactSink: Send + Sync {
    /// 同步保存完整 UTF-8 正文，返回模型可读取的稳定路径文本。
    ///
    /// `label` 是来源工具名，实现方负责将其净化为安全的文件名前缀；
    /// 返回 `Err` 表示落盘失败，运行时会回退到既有固定拒绝语义。
    fn save_output(&self, label: &str, content: &str) -> io::Result<String>;
}

/// 收缩重选阈值与预览保留预算的基准字节数；与命令输出预览粒度保持一致。
pub(crate) const TRUNCATED_TEXT_KEEP_BYTES: usize = 8 * 1_024;

/// 截断标记两侧各保留的预览字节数。
///
/// 在 [`TRUNCATED_TEXT_KEEP_BYTES`] 基础上扣除标记自身占用，使收缩后的
/// 块总长不超过重选阈值 `2 × TRUNCATED_TEXT_KEEP_BYTES`；否则收缩结果
/// 会被再次选中且长度不再变化，截断循环永远无法终止。
pub(crate) const TRUNCATION_PREVIEW_KEEP_BYTES: usize =
    TRUNCATED_TEXT_KEEP_BYTES - TRUNCATION_MARKER.len();

/// 文本块中部省略时嵌入的稳定标记。
pub(crate) const TRUNCATION_MARKER: &str = "\n...[中间输出已截断]...\n";

/// 截断说明文本的稳定前缀，模型据此识别工具输出的截断指针。
pub(crate) const TRUNCATION_SENTINEL_PREFIX: &str = "工具输出超过模型上下文安全上限";

/// 完整输出工件在运行时内传递的最小事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TruncatedOutputArtifact {
    /// 工件的模型可读路径文本，已嵌入截断说明。
    pub(crate) path: String,
    /// 完整输出正文的 UTF-8 字节数。
    pub(crate) total_bytes: usize,
}

/// 把超限成功输出保存到工件并按给定容量截断。
///
/// 容量三元组依次为内容块数、模型可见字节数与 JSON 编码字节数。返回
/// 截断后的输出与工件事实；图片来源自身无法通过校验、完整正文序列化
/// 失败、落盘失败或容量小到连截断说明都无法容纳时返回 `None`，调用方
/// 回退既有固定拒绝语义。
pub(crate) fn truncate_tool_output_with_artifact(
    tool_call_id: &str,
    output: ToolOutput,
    label: &str,
    sink: &dyn ToolOutputArtifactSink,
    capacity: (usize, usize, usize),
) -> Option<(ToolOutput, TruncatedOutputArtifact)> {
    let body = tool_output_artifact_body(&output)?;
    let path = sink.save_output(label, &body).ok()?;
    let artifact = TruncatedOutputArtifact {
        path,
        total_bytes: body.len(),
    };
    let truncated = truncate_tool_output_to_capacity(tool_call_id, output, &artifact, capacity)?;
    Some((truncated, artifact))
}

/// 按给定容量截断一个完整输出已保全在工件中的超限结果。
pub(crate) fn truncate_tool_output_to_capacity(
    tool_call_id: &str,
    output: ToolOutput,
    artifact: &TruncatedOutputArtifact,
    capacity: (usize, usize, usize),
) -> Option<ToolOutput> {
    // 图片或 data URL 自身超限（或结构无效）不属于文本截断的修复范围，
    // 落盘图片也不在输出截断机制范围内，此时维持既有拒绝语义。
    if output
        .content
        .iter()
        .any(|block| matches!(block, ToolResultContent::Image { image } if image_source_over_limit(&image.source)))
    {
        return None;
    }
    let kept_blocks = capacity.0.checked_sub(1)?;
    let mut content = output.content;
    // 为截断说明块预留一个槽位；被丢弃的尾部块已在工件中完整保全。
    while content.len() > kept_blocks {
        content.pop();
    }
    content.push(ToolResultContent::Text {
        text: truncation_sentinel(artifact),
    });
    let mut result = ToolResult::new(tool_call_id.to_owned(), content, false);
    loop {
        if truncated_result_fits(&result, capacity) {
            break;
        }
        if !shrink_largest_text_block(&mut result.content) {
            // 文本收缩已到下限或不再产生长度进展仍放不下（例如大量合法
            // 图片占满 JSON 预算）：整体收缩为仅剩截断说明；连说明都容纳
            // 不下时回退固定拒绝。
            result.content = vec![ToolResultContent::Text {
                text: truncation_sentinel(artifact),
            }];
            if !truncated_result_fits(&result, capacity) {
                return None;
            }
            break;
        }
    }
    Some(ToolOutput {
        content: result.content,
    })
}

/// 判断截断候选结果是否同时满足单结果硬上限与给定容量三元组。
fn truncated_result_fits(result: &ToolResult, capacity: (usize, usize, usize)) -> bool {
    match measure_tool_result(result) {
        Ok(footprint) => {
            footprint.content_blocks <= capacity.0
                && footprint.model_visible_bytes <= capacity.1
                && footprint.json_bytes <= capacity.2
        }
        // 图片已预检；此处失败只可能是单块文本越界或 JSON 越界，
        // 均属于继续收缩可修复的状态。
        Err(_) => false,
    }
}

/// 把最大的文本块收缩为首尾各保留 [`TRUNCATION_PREVIEW_KEEP_BYTES`] 字节
/// 的有界预览；返回是否发生了使长度严格变小的收缩。
///
/// 收缩后的块总长低于重选阈值，不会在后续迭代中被再次选中；若保留参数
/// 与重选阈值的组合无法让长度严格变小（例如未来调整阈值后出现的不动
/// 点），返回 `false` 让调用方进入整体塌缩分支而不是空转。
fn shrink_largest_text_block(content: &mut [ToolResultContent]) -> bool {
    let shrink_threshold = TRUNCATED_TEXT_KEEP_BYTES.saturating_mul(2);
    let mut largest = None;
    let mut largest_bytes = shrink_threshold;
    for (index, block) in content.iter().enumerate() {
        if let ToolResultContent::Text { text } = block
            && text.len() > largest_bytes
        {
            largest = Some(index);
            largest_bytes = text.len();
        }
    }
    let Some(index) = largest else {
        return false;
    };
    let Some(ToolResultContent::Text { text }) = content.get_mut(index) else {
        return false;
    };
    let mut head_end = TRUNCATION_PREVIEW_KEEP_BYTES;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(TRUNCATION_PREVIEW_KEEP_BYTES);
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let tail = text[tail_start..].to_owned();
    // 护栏：收缩无法让块长度严格变小时报告无进展，避免同一不动点被
    // 反复选中导致循环永不终止。
    if head_end + TRUNCATION_MARKER.len() + tail.len() >= text.len() {
        return false;
    }
    text.truncate(head_end);
    text.push_str(TRUNCATION_MARKER);
    text.push_str(&tail);
    true
}

/// 构造指向完整输出工件的稳定截断说明；呈现方式与命令输出的
/// 工件路径字段保持同构，模型可据此用 Read 取回完整内容。
fn truncation_sentinel(artifact: &TruncatedOutputArtifact) -> String {
    format!(
        "{TRUNCATION_SENTINEL_PREFIX}，已省略部分内容；完整输出共 {} 字节，已保存到：{}；可用 Read 工具读取完整内容。",
        artifact.total_bytes, artifact.path
    )
}

/// 返回写入工件的完整输出正文；单文本块按原文保存，其余序列化为 JSON。
///
/// 序列化失败时返回 `None`，调用方回退既有固定拒绝语义；不得静默写入
/// 空工件，否则模型会据空文件误判已取回完整输出。
fn tool_output_artifact_body(output: &ToolOutput) -> Option<String> {
    if let [ToolResultContent::Text { text }] = &output.content[..] {
        return Some(text.clone());
    }
    let serialized = serde_json::to_string(&output.content);
    debug_assert!(
        serialized.is_ok(),
        "工具结果内容块序列化为 JSON 不应失败：{:?}",
        serialized.as_ref().err()
    );
    serialized.ok()
}

/// Agent Runtime 可注册的一个 Provider 中立工具。
pub trait AgentTool: Send + Sync {
    /// 返回提供给模型的名称、说明和 JSON Schema。
    fn definition(&self) -> ToolDefinition;

    /// 按规范化输入判断本次调用是否可能产生外部副作用。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError>;

    /// 返回本工具在只读调用时允许的并发方式。
    fn concurrency(&self) -> ToolConcurrency;

    /// 返回单次执行的外层墙钟上限；`None` 表示工具自管超时或有界用户交互。
    ///
    /// 默认 120 秒兜底防止挂起的本地操作挂死整个 Turn。内部已完整管理
    /// 超时（如命令执行）或等待外部确认（如用户问答）的工具应覆盖为
    /// `None`，避免外层上限截断合法的长执行。
    fn timeout(&self) -> Option<Duration> {
        Some(DEFAULT_TOOL_TIMEOUT)
    }

    /// 返回把超出模型预算的完整输出保存到本机工件的落盘通道。
    ///
    /// 默认 `None`：未接入 Session 工件目录的工具（MCP 桥接、LSP、
    /// Skill、状态与协作工具）无法提供完整输出副本，输出超限时维持
    /// 既有固定拒绝语义。持有共享环境的内置工具应覆盖为本环境提供的
    /// 通道，使超限输出被截断并落盘后回流。
    fn output_artifact_sink(&self) -> Option<Arc<dyn ToolOutputArtifactSink>> {
        None
    }

    /// 校验并执行一次工具调用。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_>;
}

/// 按精确工具名称保存实现的确定性注册表。
#[derive(Clone, Default)]
pub struct ToolRegistry {
    /// 按冻结名称排序保存的工具定义和执行实现。
    tools: BTreeMap<String, RegisteredTool>,
}

/// 注册时冻结的工具定义与执行实现。
#[derive(Clone)]
struct RegisteredTool {
    /// 提供给模型且在运行期间保持不变的工具定义。
    definition: ToolDefinition,
    /// 接收已校验输入的实际工具实现。
    implementation: Arc<dyn AgentTool>,
}

impl ToolRegistry {
    /// 创建空工具注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个名称唯一且定义有效的工具。
    pub fn register(&mut self, tool: Arc<dyn AgentTool>) -> Result<(), ToolRegistryError> {
        let definition = tool.definition();
        definition
            .validate()
            .map_err(|error| ToolRegistryError::InvalidDefinition {
                message: error.to_string(),
            })?;
        if self.tools.contains_key(&definition.name) {
            return Err(ToolRegistryError::DuplicateName {
                name: definition.name,
            });
        }
        self.tools.insert(
            definition.name.clone(),
            RegisteredTool {
                definition,
                implementation: tool,
            },
        );
        Ok(())
    }

    /// 返回按名称排序的模型工具定义快照。
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|tool| tool.definition.clone())
            .collect()
    }

    /// 按冻结名称精确复制一个子集；任何未知或重复名称都会整体拒绝。
    pub fn select_exact(&self, names: &[String]) -> Result<Self, ToolRegistryError> {
        let mut selected = BTreeMap::new();
        for name in names {
            let tool = self
                .tools
                .get(name)
                .ok_or_else(|| ToolRegistryError::UnknownName { name: name.clone() })?;
            if selected.insert(name.clone(), tool.clone()).is_some() {
                return Err(ToolRegistryError::DuplicateName { name: name.clone() });
            }
        }
        Ok(Self { tools: selected })
    }

    /// 返回已注册工具数量。
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// 返回注册表是否为空。
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 按精确名称查找工具实现。
    pub(crate) fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.get(name).map(|tool| tool.implementation.clone())
    }

    /// 按精确名称返回注册时冻结的工具定义。
    pub(crate) fn definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name).map(|tool| &tool.definition)
    }
}

/// 工具定义不能安全加入注册表时返回的错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolRegistryError {
    /// 工具名称已经被另一个实现占用。
    DuplicateName {
        /// 发生冲突的精确工具名称。
        name: String,
    },
    /// 冻结工具快照引用了当前注册表中不存在的名称。
    UnknownName {
        /// 无法解析的精确工具名称。
        name: String,
    },
    /// 工具名称、说明或输入 Schema 无效。
    InvalidDefinition {
        /// Provider 中立校验返回的说明。
        message: String,
    },
}

impl fmt::Display for ToolRegistryError {
    /// 输出不包含工具输入的注册失败说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName { name } => write!(formatter, "工具名称重复：{name}"),
            Self::UnknownName { name } => write!(formatter, "工具名称不存在：{name}"),
            Self::InvalidDefinition { message } => write!(formatter, "工具定义无效：{message}"),
        }
    }
}

impl Error for ToolRegistryError {}

#[cfg(test)]
mod output_guard_tests {
    use super::*;
    use keencode_model::ImageContent;

    /// 构造只包含一个文本块的成功输出。
    fn text_output(text: String) -> ToolOutput {
        ToolOutput {
            content: vec![ToolResultContent::Text { text }],
        }
    }

    /// 文本按 UTF-8 字节而非字符计数，边界内完整保留，越界整体拒绝。
    #[test]
    fn text_output_uses_exact_utf8_byte_limit() {
        let exact = "你".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes / "你".len());
        let (result, _) = validate_tool_output("call-exact".to_owned(), text_output(exact.clone()))
            .expect("边界内 UTF-8 文本应完整通过");
        assert_eq!(
            result.content,
            vec![ToolResultContent::Text { text: exact }]
        );

        let oversized = format!("{}界", "a".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes - 1));
        assert_eq!(
            validate_tool_output("call-over".to_owned(), text_output(oversized.clone())),
            Err(ToolOutputValidation::LimitExceeded {
                output: text_output(oversized)
            })
        );
    }

    /// 内容块数量允许精确边界，并在多一个块时拒绝整个结果。
    #[test]
    fn content_block_count_has_hard_boundary() {
        let exact = ToolOutput {
            content: (0..TOOL_OUTPUT_LIMITS.max_content_blocks)
                .map(|_| ToolResultContent::Text {
                    text: "x".to_owned(),
                })
                .collect(),
        };
        assert!(validate_tool_output("call-exact".to_owned(), exact).is_ok());
        let oversized = ToolOutput {
            content: (0..=TOOL_OUTPUT_LIMITS.max_content_blocks)
                .map(|_| ToolResultContent::Text {
                    text: "x".to_owned(),
                })
                .collect(),
        };
        assert_eq!(
            validate_tool_output("call-over".to_owned(), oversized.clone()),
            Err(ToolOutputValidation::LimitExceeded { output: oversized })
        );
    }

    /// 标准 Base64 校验填充位置、尾部未使用位、字符集和解码后字节数。
    #[test]
    fn base64_validation_is_canonical_and_decoded_size_is_exact() {
        assert_eq!(strict_base64_decoded_bytes("AA=="), Ok(1));
        assert_eq!(strict_base64_decoded_bytes("AAA="), Ok(2));
        assert_eq!(strict_base64_decoded_bytes("AAAA"), Ok(3));
        for invalid in ["A===", "AB==", "AAB=", "AA=A", "AA\r\n", "你==="] {
            assert_eq!(
                strict_base64_decoded_bytes(invalid),
                Err(ToolOutputRejection::Invalid),
                "{invalid:?} 不得被接受"
            );
        }

        let mut maximum = "A".repeat(TOOL_OUTPUT_LIMITS.max_base64_characters);
        maximum.replace_range(maximum.len() - 1.., "=");
        assert_eq!(
            strict_base64_decoded_bytes(&maximum),
            Ok(TOOL_OUTPUT_LIMITS.max_image_decoded_bytes)
        );
        maximum.push_str("AAAA");
        assert_eq!(
            strict_base64_decoded_bytes(&maximum),
            Err(ToolOutputRejection::LimitExceeded)
        );
    }

    /// Base64 图片同时执行媒体类型、来源表示和解码容量校验。
    #[test]
    fn base64_image_checks_media_type_and_source_limits() {
        let output = ToolOutput {
            content: vec![ToolResultContent::Image {
                image: ImageContent::from_base64("image/png", "AA=="),
            }],
        };
        assert!(validate_tool_output("call-image".to_owned(), output).is_ok());

        let media_type = format!(
            "image/{}",
            "x".repeat(TOOL_OUTPUT_LIMITS.max_media_type_bytes)
        );
        let output = ToolOutput {
            content: vec![ToolResultContent::Image {
                image: ImageContent::from_base64(media_type, "AA=="),
            }],
        };
        assert_eq!(
            validate_tool_output("call-media".to_owned(), output.clone()),
            Err(ToolOutputValidation::LimitExceeded { output })
        );

        for media_type in [
            "text/plain",
            "foo/bar",
            "image/",
            "image/png/extra",
            "image/PNG",
            "image/p ng",
            "image/png;charset=utf-8",
            "image/foo%bar",
            "image/foo'bar",
            "image/foo*bar",
            "image/foo`bar",
            "image/foo|bar",
            "image/foo~bar",
            "image/+suffix",
            "image/.hidden",
            "image/-private",
            "image/_private",
        ] {
            let output = ToolOutput {
                content: vec![ToolResultContent::Image {
                    image: ImageContent::from_base64(media_type, "AA=="),
                }],
            };
            assert_eq!(
                validate_tool_output("call-media-invalid".to_owned(), output),
                Err(ToolOutputValidation::Invalid),
                "{media_type:?} 不得作为图片媒体类型"
            );
        }
        for media_type in ["image/vnd.test+json", "image/x.foo-bar_2", "image/a!#$&^z"] {
            let output = ToolOutput {
                content: vec![ToolResultContent::Image {
                    image: ImageContent::from_base64(media_type, "AA=="),
                }],
            };
            assert!(
                validate_tool_output("call-media-valid".to_owned(), output).is_ok(),
                "{media_type:?} 应符合 ArtifactStore 的 restricted-name 语法"
            );
        }
    }

    /// data URL 只接受单一 Base64 参数，并拒绝未知参数、重复参数、CRLF 和非 ASCII。
    #[test]
    fn data_url_rejects_ambiguous_or_non_ascii_forms() {
        let valid = ToolOutput {
            content: vec![ToolResultContent::Image {
                image: ImageContent::from_url("DaTa:image/png;BaSe64,AA=="),
            }],
        };
        assert!(validate_tool_output("call-data".to_owned(), valid).is_ok());

        for url in [
            "data:image/png;base64;base64,AA==",
            "data:image/png;charset=utf-8;base64,AA==",
            "data:image/png,AA==",
            "data:image/png\r\n;base64,AA==",
            "data:image/图;base64,AA==",
        ] {
            let output = ToolOutput {
                content: vec![ToolResultContent::Image {
                    image: ImageContent::from_url(url),
                }],
            };
            assert_eq!(
                validate_tool_output("call-data-invalid".to_owned(), output),
                Err(ToolOutputValidation::Invalid),
                "{url:?} 不得被接受"
            );
        }
    }

    /// 远端 URL 只接受有界且语法明确的 ASCII HTTP(S)，并拒绝全部空白和歧义 authority。
    #[test]
    fn remote_url_is_ascii_http_and_bounded() {
        let exact = format!(
            "https://example.test/{}",
            "a".repeat(TOOL_OUTPUT_LIMITS.max_remote_url_bytes - "https://example.test/".len())
        );
        let output = ToolOutput {
            content: vec![ToolResultContent::Image {
                image: ImageContent::from_url(exact.clone()),
            }],
        };
        assert!(crate::is_canonical_remote_image_url(&exact));
        assert!(validate_tool_output("call-url".to_owned(), output).is_ok());

        for url in [
            format!(
                "https://example.test/{}",
                "a".repeat(
                    TOOL_OUTPUT_LIMITS.max_remote_url_bytes - "https://example.test/".len() + 1
                )
            ),
            "https://example.test/a b".to_owned(),
            "https://example.test/a\tb".to_owned(),
            "https://example.test/a\nb".to_owned(),
            "https://example.test/a\x0bb".to_owned(),
            "https://example.test/a\x0cb".to_owned(),
            "https://example.test/a\rb".to_owned(),
            "https://example.test/image\r\nheader:value".to_owned(),
            "https://example.test/image\0tail".to_owned(),
            "https://example.test/image\u{7f}tail".to_owned(),
            "https://example.test/图片".to_owned(),
            "file:///tmp/image.png".to_owned(),
            "https://".to_owned(),
            "https:///image.png".to_owned(),
            "https://user:secret@example.test/image.png".to_owned(),
            "https://example.test\\@attacker.test/image.png".to_owned(),
            "https://example.test:bad/image.png".to_owned(),
            "https://example.test:65536/image.png".to_owned(),
            "https://[not-ipv6]/image.png".to_owned(),
            "https://127.1/image.png".to_owned(),
            "https://127.000.000.001/image.png".to_owned(),
            "https://0x7f000001/image.png".to_owned(),
            "https://0x7f.1/image.png".to_owned(),
            "https://017700000001/image.png".to_owned(),
            "https://example..test/image.png".to_owned(),
            "https://-example.test/image.png".to_owned(),
            "https://example.test/%GG".to_owned(),
            "https://example.test/%0".to_owned(),
        ] {
            assert!(!crate::is_canonical_remote_image_url(&url));
            let output = ToolOutput {
                content: vec![ToolResultContent::Image {
                    image: ImageContent::from_url(url),
                }],
            };
            assert!(validate_tool_output("call-url-invalid".to_owned(), output).is_err());
        }

        for url in [
            "http://localhost/image.png",
            "https://127.0.0.1:8443/image.png?size=2%20x2",
            "https://[2001:db8::1]/image.png",
            "HTTPS://cdn.example.test/image.png#preview",
        ] {
            assert!(crate::is_canonical_remote_image_url(url));
            let output = ToolOutput {
                content: vec![ToolResultContent::Image {
                    image: ImageContent::from_url(url),
                }],
            };
            assert!(
                validate_tool_output("call-url-valid".to_owned(), output).is_ok(),
                "{url:?} 应作为规范远端图片地址通过"
            );
        }
    }

    /// JSON 编码预算按转义后的实际字节计数，而不是只看原始文本长度。
    #[test]
    fn result_json_limit_counts_escaped_bytes_without_copying_output() {
        let content: Vec<ToolResultContent> = (0..5)
            .map(|_| ToolResultContent::Text {
                text: "\0".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes),
            })
            .collect();
        assert_eq!(
            validate_tool_output(
                "call-json".to_owned(),
                ToolOutput {
                    content: content.clone()
                }
            ),
            Err(ToolOutputValidation::LimitExceeded {
                output: ToolOutput { content }
            })
        );
    }

    /// Round 聚合预算为后续固定结果保留容量，并分别约束模型可见与 JSON 字节。
    #[test]
    fn round_budget_is_atomic_and_reserves_future_results() {
        let mut budget = ToolRoundOutputBudget::new(2);
        assert!(!budget.try_charge_result(ToolResultFootprint {
            content_blocks: 1,
            model_visible_bytes: TOOL_OUTPUT_LIMITS.max_round_model_visible_bytes,
            json_bytes: 1,
        }));
        assert!(budget.try_charge_result(ToolResultFootprint {
            content_blocks: 1,
            model_visible_bytes: TOOL_OUTPUT_LIMITS.max_round_model_visible_bytes
                - ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES,
            json_bytes: 1,
        }));
        assert!(budget.try_charge_result(ToolResultFootprint {
            content_blocks: 1,
            model_visible_bytes: ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES,
            json_bytes: 1,
        }));

        let mut json_budget = ToolRoundOutputBudget::new(1);
        assert!(!json_budget.try_charge_result(ToolResultFootprint {
            content_blocks: 1,
            model_visible_bytes: 0,
            json_bytes: TOOL_OUTPUT_LIMITS.max_round_json_bytes + 1,
        }));
    }

    /// Round 内容块预算跨多个结果累计，并为每个尚未完成的固定失败块保留槽位。
    #[test]
    fn round_content_block_limit_is_global_and_reserves_pending_failures() {
        assert!(ToolRoundOutputBudget::can_reserve_results(
            TOOL_OUTPUT_LIMITS.max_round_content_blocks
        ));
        assert!(!ToolRoundOutputBudget::can_reserve_results(
            TOOL_OUTPUT_LIMITS.max_round_content_blocks + 1
        ));

        let mut exact = ToolRoundOutputBudget::new(4);
        for _ in 0..4 {
            assert!(exact.try_charge_result(ToolResultFootprint {
                content_blocks: 16,
                model_visible_bytes: 0,
                json_bytes: 0,
            }));
        }

        let mut overflowing = ToolRoundOutputBudget::new(2);
        assert!(overflowing.try_charge_result(ToolResultFootprint {
            content_blocks: 63,
            model_visible_bytes: 0,
            json_bytes: 0,
        }));
        assert!(!overflowing.try_charge_result(ToolResultFootprint {
            content_blocks: 2,
            model_visible_bytes: 0,
            json_bytes: 0,
        }));
        assert!(overflowing.try_charge_result(ToolResultFootprint {
            content_blocks: 1,
            model_visible_bytes: 0,
            json_bytes: 0,
        }));

        let mut with_post_hook = ToolRoundOutputBudget::new(2);
        for _ in 0..2 {
            assert!(with_post_hook.try_charge_result(ToolResultFootprint {
                content_blocks: 16,
                model_visible_bytes: 0,
                json_bytes: 0,
            }));
        }
        assert!(with_post_hook.try_charge_post_hook(32, 0, 0));
        assert!(!with_post_hook.try_charge_post_hook(1, 0, 0));
    }

    /// ToolError 两个字段只有同时满足 UTF-8 硬上限时才会被原样保留。
    #[test]
    fn tool_error_fields_are_atomically_bounded() {
        let valid = ToolError::retryable(
            "码".repeat(TOOL_OUTPUT_LIMITS.max_tool_error_code_bytes / "码".len()),
            "错".repeat(TOOL_OUTPUT_LIMITS.max_tool_error_message_bytes / "错".len()),
        );
        let normalized = normalize_tool_error(&valid);
        assert_eq!(normalized.code, valid.code);
        assert_eq!(normalized.message, valid.message);
        assert!(normalized.retryable);

        let oversized = ToolError::retryable(
            "safe_code",
            "错".repeat(TOOL_OUTPUT_LIMITS.max_tool_error_message_bytes / "错".len() + 1),
        );
        assert_eq!(
            normalize_tool_error(&oversized),
            NormalizedToolError {
                code: INVALID_TOOL_ERROR_CODE.to_owned(),
                message: INVALID_TOOL_ERROR_MESSAGE.to_owned(),
                retryable: false,
            }
        );
    }

    /// next_result_capacity 的三个维度与 try_charge_result 判定完全对齐，
    /// 按容量上限构造的结果必定能被 Round 预算接纳。
    #[test]
    fn next_result_capacity_matches_charge_success() {
        let mut budget = ToolRoundOutputBudget::new(3);
        assert!(budget.try_charge_result(ToolResultFootprint {
            content_blocks: 61,
            model_visible_bytes: 0,
            json_bytes: 0,
        }));
        let (blocks, model, json) = budget.next_result_capacity();
        // 已占用 61 块且仍有一个后续待生成结果需要预留一个固定失败块。
        assert_eq!(blocks, TOOL_OUTPUT_LIMITS.max_round_content_blocks - 61 - 1);
        assert!(budget.try_charge_result(ToolResultFootprint {
            content_blocks: blocks,
            model_visible_bytes: model,
            json_bytes: json,
        }));

        let mut budget = ToolRoundOutputBudget::new(2);
        assert!(budget.try_charge_result(ToolResultFootprint {
            content_blocks: 1,
            model_visible_bytes: TOOL_OUTPUT_LIMITS.max_round_model_visible_bytes
                - ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES,
            json_bytes: 0,
        }));
        let (_, model, _) = budget.next_result_capacity();
        assert_eq!(model, ROUND_PENDING_RESULT_MODEL_RESERVE_BYTES);
    }

    /// 图片来源自身无法通过校验时不能进入文本截断，维持既有拒绝语义。
    #[test]
    fn image_source_over_limit_output_cannot_be_truncated() {
        let oversized_url = format!(
            "https://example.test/{}",
            "a".repeat(TOOL_OUTPUT_LIMITS.max_remote_url_bytes + 1)
        );
        let output = ToolOutput {
            content: vec![ToolResultContent::Image {
                image: ImageContent::from_url(oversized_url),
            }],
        };
        let artifact = TruncatedOutputArtifact {
            path: "/tmp/keencode-full.log".to_owned(),
            total_bytes: 1,
        };
        assert_eq!(
            truncate_tool_output_to_capacity(
                "call-image",
                output,
                &artifact,
                (
                    TOOL_OUTPUT_LIMITS.max_content_blocks,
                    usize::MAX,
                    TOOL_OUTPUT_LIMITS.max_result_json_bytes
                )
            ),
            None
        );
    }

    /// 容量小到只够截断说明时整体收缩为仅剩说明块，保证结果可接纳。
    #[test]
    fn tiny_capacity_collapses_to_sentinel_only() {
        let output = ToolOutput::text("x".repeat(100_000));
        let artifact = TruncatedOutputArtifact {
            path: "/tmp/keencode-full.log".to_owned(),
            total_bytes: 100_000,
        };
        let truncated = truncate_tool_output_to_capacity(
            "call-tiny",
            output,
            &artifact,
            (1, usize::MAX, TOOL_OUTPUT_LIMITS.max_result_json_bytes),
        )
        .expect("单块截断说明容量必定可接纳");
        let [ToolResultContent::Text { text }] = &truncated.content[..] else {
            panic!("收缩结果应只包含一个截断说明块");
        };
        assert!(text.starts_with(TRUNCATION_SENTINEL_PREFIX));
        assert!(text.contains("完整输出共 100000 字节"));
    }

    /// 模型可见容量低于收缩下限时必须塌缩为仅剩说明块并快速终止。
    ///
    /// 这是收缩不动点回归：保留预算一旦使收缩结果仍高于重选阈值且长度
    /// 不再变化，修复前的实现会在收缩循环中永久空转挂死整个 Turn。
    #[test]
    fn capacity_below_shrink_floor_collapses_instead_of_looping() {
        let output = ToolOutput::text("x".repeat(300 * 1_024));
        let artifact = TruncatedOutputArtifact {
            path: "/tmp/keencode-full.log".to_owned(),
            total_bytes: 300 * 1_024,
        };
        let truncated = truncate_tool_output_to_capacity(
            "call-shrink-floor",
            output,
            &artifact,
            (10, 8_192, usize::MAX),
        )
        .expect("截断说明自身应能容纳在模型可见容量内");
        let [ToolResultContent::Text { text }] = &truncated.content[..] else {
            panic!("容量低于收缩下限时应整体塌缩为仅剩截断说明");
        };
        assert!(text.starts_with(TRUNCATION_SENTINEL_PREFIX));
    }

    /// 容量连截断说明都无法容纳时返回 `None` 交由调用方回退固定拒绝。
    #[test]
    fn capacity_below_sentinel_returns_none_quickly() {
        let output = ToolOutput::text("x".repeat(300 * 1_024));
        let artifact = TruncatedOutputArtifact {
            path: "/tmp/keencode-full.log".to_owned(),
            total_bytes: 300 * 1_024,
        };
        assert_eq!(
            truncate_tool_output_to_capacity(
                "call-below-sentinel",
                output,
                &artifact,
                (10, 64, usize::MAX),
            ),
            None
        );
    }

    /// 两张合法图片占满 JSON 预算时文本收缩无法修复，必须经塌缩快速终止。
    #[test]
    fn legal_images_exhausting_json_budget_terminate_via_collapse() {
        // 每张图片 Base64 数据 6 MiB：本身合法，但两图的 JSON 编码已超出
        // 单结果 JSON 硬上限，文本收缩无法触及该下界。
        let data = format!("{}==", "A".repeat(6_291_454));
        let output = ToolOutput {
            content: vec![
                ToolResultContent::Image {
                    image: ImageContent::from_base64("image/png", data.clone()),
                },
                ToolResultContent::Image {
                    image: ImageContent::from_base64("image/png", data),
                },
                ToolResultContent::Text {
                    text: "x".repeat(300 * 1_024),
                },
            ],
        };
        let artifact = TruncatedOutputArtifact {
            path: "/tmp/keencode-full.log".to_owned(),
            total_bytes: 2 * 6_291_456 + 300 * 1_024,
        };
        let truncated = truncate_tool_output_to_capacity(
            "call-images",
            output,
            &artifact,
            (
                TOOL_OUTPUT_LIMITS.max_content_blocks,
                usize::MAX,
                TOOL_OUTPUT_LIMITS.max_result_json_bytes,
            ),
        )
        .expect("塌缩为仅剩说明块后应能容纳在 JSON 容量内");
        let [ToolResultContent::Text { text }] = &truncated.content[..] else {
            panic!("图片占满 JSON 预算时应整体塌缩为仅剩截断说明");
        };
        assert!(text.starts_with(TRUNCATION_SENTINEL_PREFIX));
    }
}
