//! 标准 MCP 工具与 Provider 中立 Agent 工具之间的安全桥接。

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
};
use keencode_mcp::{
    CancellationToken, McpClient, McpClientOptions, McpContent, McpError, McpServerConfig, McpTool,
    McpToolEffect, McpToolSet, ToolCallResult,
};
use keencode_model::{ImageContent, MAX_TOOL_NAME_BYTES, ToolDefinition, ToolResultContent};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// MCP Server 标识或远端工具名称允许接收的最大 UTF-8 字节数。
const MAX_MCP_IDENTITY_BYTES: usize = 256;
/// 进入模型工具说明的 MCP 来源文本最大 UTF-8 字节数。
const MAX_MCP_DESCRIPTION_BYTES: usize = 8 * 1024;
/// 稳定名称摘要使用的十六进制字符数量。
const MCP_NAME_DIGEST_CHARACTERS: usize = 12;
/// 人类可读名称与稳定摘要之间的分隔符。
const MCP_NAME_DIGEST_SEPARATOR: &str = "__";
/// 进入扩展诊断报告的单条安全说明最大 UTF-8 字节数。
const MAX_MCP_DIAGNOSTIC_MESSAGE_BYTES: usize = 1_024;
/// Turn 取消后等待 MCP 传输完成取消通知的最长时间。
const MCP_CANCELLATION_GRACE: Duration = Duration::from_secs(1);

#[path = "mcp_resources.rs"]
mod resources;

/// MCP 扩展初始化或工具桥接失败的稳定诊断分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpDiagnosticCode {
    /// MCP Server 无法连接、初始化或已经不可用。
    ServerUnavailable,
    /// MCP `tools/list` 发现失败。
    ToolDiscoveryFailed,
    /// Server 或工具身份不满足本地边界。
    InvalidIdentity,
    /// 远端工具 Schema 无法进入 Provider 中立工具定义。
    InvalidDefinition,
    /// 远端工具要求当前未启用的 MCP Tasks 协议。
    UnsupportedTaskTool,
    /// 远端工具生成的 Provider 中立名称发生冲突。
    PortableNameCollision,
}

impl McpDiagnosticCode {
    /// 返回供日志、控制面和测试稳定识别的 ASCII 分类码。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServerUnavailable => "mcp_server_unavailable",
            Self::ToolDiscoveryFailed => "mcp_tool_discovery_failed",
            Self::InvalidIdentity => "mcp_invalid_identity",
            Self::InvalidDefinition => "mcp_invalid_definition",
            Self::UnsupportedTaskTool => "mcp_unsupported_task_tool",
            Self::PortableNameCollision => "mcp_portable_name_collision",
        }
    }
}

impl fmt::Display for McpDiagnosticCode {
    /// 输出稳定的 MCP 诊断分类码。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一个可安全交给上层 Session 记录的 MCP 扩展诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpToolDiagnostic {
    /// 经过边界处理的 MCP Server 标识。
    pub server_id: String,
    /// 经过边界处理的远端工具名称；Server 级故障时为空。
    pub tool_name: Option<String>,
    /// 不依赖远端正文的稳定诊断分类。
    pub code: McpDiagnosticCode,
    /// 已清理控制字符并截断的安全说明。
    pub message: String,
}

impl McpToolDiagnostic {
    /// 创建一条不会把无界或控制字符文本带入日志的诊断。
    fn new(
        server_id: &str,
        tool_name: Option<&str>,
        code: McpDiagnosticCode,
        message: impl AsRef<str>,
    ) -> Self {
        Self {
            server_id: diagnostic_identity(server_id),
            tool_name: tool_name.map(diagnostic_identity),
            code,
            message: bounded_diagnostic_message(message.as_ref()),
        }
    }
}

/// MCP 延迟入口 best-effort 构建结果；可用工具和资源操作保留，失败项进入诊断。
#[derive(Default)]
pub struct McpToolBuildReport {
    /// 已成功转换为 Provider 中立 Agent 工具的条目。
    tools: Vec<Arc<dyn AgentTool>>,
    /// 不影响其他可用工具的 Server 或工具级失败。
    diagnostics: Vec<McpToolDiagnostic>,
}

impl McpToolBuildReport {
    /// 消费报告并取得全部可用工具实现。
    pub fn into_tools(self) -> Vec<Arc<dyn AgentTool>> {
        self.tools
    }

    /// 返回可用工具的只读快照。
    pub fn tools(&self) -> &[Arc<dyn AgentTool>] {
        &self.tools
    }

    /// 返回按处理顺序记录的安全诊断。
    pub fn diagnostics(&self) -> &[McpToolDiagnostic] {
        &self.diagnostics
    }

    /// 返回本次构建是否有条目被降级跳过。
    pub fn is_degraded(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    /// 返回实际可调用入口数量，包含本地包装的资源操作，并非远端 tools/list 的原始数量。
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// 追加另一个报告，供多 Server 并行准备阶段合并结果。
    pub fn append(&mut self, mut other: Self) {
        self.tools.append(&mut other.tools);
        self.diagnostics.append(&mut other.diagnostics);
    }

    /// 向当前报告追加一条内部构造的诊断。
    fn push_diagnostic(&mut self, diagnostic: McpToolDiagnostic) {
        self.diagnostics.push(diagnostic);
    }
}

/// 将一个 MCP Server 当前公布的工具集冻结为延迟 Agent 工具。
pub fn build_mcp_deferred_tools(
    server_id: &str,
    client: McpClient,
    tool_set: &McpToolSet,
) -> Result<Vec<Arc<dyn AgentTool>>, McpToolBridgeError> {
    validate_remote_identity(server_id)?;
    let mut names = BTreeSet::new();
    let mut tools: Vec<Arc<dyn AgentTool>> = Vec::with_capacity(tool_set.tools().len());
    for tool in tool_set.tools() {
        if tool.requires_task() {
            return Err(McpToolBridgeError::UnsupportedTaskTool);
        }
        let effect = match tool_set.effect_for(&tool.name) {
            McpToolEffect::ReadOnly => ToolEffect::ReadOnly,
            McpToolEffect::ChangesState => ToolEffect::ChangesState,
        };
        let bridge = McpToolBridge::new(server_id, client.clone(), tool, effect)?;
        if !names.insert(bridge.definition.name.clone()) {
            return Err(McpToolBridgeError::PortableNameCollision);
        }
        tools.push(Arc::new(bridge));
    }
    Ok(tools)
}

/// 尽可能构建 MCP 工具；单个坏工具不会阻断同一 Server 的其他工具。
pub fn build_mcp_deferred_tools_best_effort(
    server_id: &str,
    client: McpClient,
    tool_set: &McpToolSet,
) -> McpToolBuildReport {
    let mut report = McpToolBuildReport::default();
    if let Err(error) = validate_remote_identity(server_id) {
        report.push_diagnostic(McpToolDiagnostic::new(
            server_id,
            None,
            error.diagnostic_code(),
            error.to_string(),
        ));
        return report;
    }

    let mut names = BTreeSet::new();
    for tool in tool_set.tools() {
        if tool.requires_task() {
            report.push_diagnostic(McpToolDiagnostic::new(
                server_id,
                Some(&tool.name),
                McpDiagnosticCode::UnsupportedTaskTool,
                "远端工具要求当前未启用的 MCP Tasks 协议",
            ));
            continue;
        }
        let effect = match tool_set.effect_for(&tool.name) {
            McpToolEffect::ReadOnly => ToolEffect::ReadOnly,
            McpToolEffect::ChangesState => ToolEffect::ChangesState,
        };
        let bridge = match McpToolBridge::new(server_id, client.clone(), tool, effect) {
            Ok(bridge) => bridge,
            Err(error) => {
                report.push_diagnostic(McpToolDiagnostic::new(
                    server_id,
                    Some(&tool.name),
                    error.diagnostic_code(),
                    error.to_string(),
                ));
                continue;
            }
        };
        if !names.insert(bridge.definition.name.clone()) {
            report.push_diagnostic(McpToolDiagnostic::new(
                server_id,
                Some(&tool.name),
                McpDiagnosticCode::PortableNameCollision,
                McpToolBridgeError::PortableNameCollision.to_string(),
            ));
            continue;
        }
        report.tools.push(Arc::new(bridge));
    }
    report
}

/// 连接单个 MCP Server，发现工具并登记资源入口；局部失败不阻断其他入口或核心 Session。
pub async fn prepare_mcp_server_tools(
    server_id: impl Into<String>,
    config: McpServerConfig,
    options: McpClientOptions,
) -> McpToolBuildReport {
    let server_id = server_id.into();
    let mut report = McpToolBuildReport::default();
    if let Err(error) = validate_remote_identity(&server_id) {
        report.push_diagnostic(McpToolDiagnostic::new(
            &server_id,
            None,
            error.diagnostic_code(),
            error.to_string(),
        ));
        return report;
    }
    let client = match McpClient::connect(config, options).await {
        Ok(client) => client,
        Err(error) => {
            report.push_diagnostic(McpToolDiagnostic::new(
                &server_id,
                None,
                McpDiagnosticCode::ServerUnavailable,
                error.to_string(),
            ));
            return report;
        }
    };
    let capabilities = client.session().capabilities;
    // 资源操作也进入现有延迟目录；只根据握手声明登记，不在启动时读取资源正文。
    if capabilities.resources.is_some() {
        report
            .tools
            .extend(resources::build_resource_tools(&server_id, client.clone()));
    }
    if capabilities.tools.is_some() {
        match client.list_tools().await {
            Ok(tool_set) => report.append(build_mcp_deferred_tools_best_effort(
                &server_id,
                client.clone(),
                &tool_set,
            )),
            Err(error) => report.push_diagnostic(McpToolDiagnostic::new(
                &server_id,
                None,
                McpDiagnosticCode::ToolDiscoveryFailed,
                error.to_string(),
            )),
        }
    }
    if report.tool_count() == 0 {
        // 所有工具均被安全策略跳过时，报告不再持有 Client；此处也必须
        // 显式关闭，以覆盖 HTTP 传输无法通过 Drop 发送 DELETE 的情况。
        let _ = client.close().await;
    }
    report
}

/// 为任意受限 MCP Server 与工具名称生成跨三种 Provider 协议可用的稳定名称。
pub fn portable_mcp_tool_name(
    server_id: &str,
    remote_tool_name: &str,
) -> Result<String, McpToolBridgeError> {
    validate_remote_identity(server_id)?;
    validate_remote_identity(remote_tool_name)?;
    let readable = format!(
        "mcp__{}__{}",
        portable_slug(server_id),
        portable_slug(remote_tool_name)
    );
    let mut hasher = Sha256::new();
    hasher.update(server_id.as_bytes());
    hasher.update([0]);
    hasher.update(remote_tool_name.as_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..MCP_NAME_DIGEST_CHARACTERS / 2]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let prefix_limit = MAX_TOOL_NAME_BYTES
        .checked_sub(MCP_NAME_DIGEST_SEPARATOR.len() + suffix.len())
        .ok_or(McpToolBridgeError::InvalidIdentity)?;
    let prefix = readable
        .get(..readable.len().min(prefix_limit))
        .ok_or(McpToolBridgeError::InvalidIdentity)?;
    Ok(format!("{prefix}{MCP_NAME_DIGEST_SEPARATOR}{suffix}"))
}

/// 一个冻结远端名称、Schema、影响分类与客户端句柄的 MCP 工具实现。
struct McpToolBridge {
    /// 提供给模型的可移植名称、受限说明与输入 Schema。
    definition: ToolDefinition,
    /// `tools/call` 使用的原始远端工具名称。
    remote_name: String,
    /// 本地可信配置给出的保守副作用分类。
    effect: ToolEffect,
    /// 已完成初始化且可并发克隆的标准 MCP 客户端。
    client: McpClient,
}

impl fmt::Debug for McpToolBridge {
    /// 调试输出只展示可移植名称与影响分类，不泄露远端说明或参数。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpToolBridge")
            .field("name", &self.definition.name)
            .field("effect", &self.effect)
            .finish_non_exhaustive()
    }
}

impl McpToolBridge {
    /// 从一个已经分页校验的 MCP 工具创建冻结桥接实现。
    fn new(
        server_id: &str,
        client: McpClient,
        tool: &McpTool,
        effect: ToolEffect,
    ) -> Result<Self, McpToolBridgeError> {
        let definition = mcp_tool_definition(server_id, tool)?;
        Ok(Self {
            definition,
            remote_name: tool.name.clone(),
            effect,
            client,
        })
    }
}

impl AgentTool for McpToolBridge {
    /// 返回目录装配时冻结的可移植定义。
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    /// 使用冻结 Schema 校验输入并返回本地可信影响分类。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        self.definition.validate_input(input).map_err(|_| {
            ToolError::permanent(
                "mcp_tool_input_invalid",
                "MCP 工具输入不符合已发现的 Schema",
            )
        })?;
        Ok(self.effect)
    }

    /// 只有本地明确列入只读策略的 MCP 工具允许并发。
    fn concurrency(&self) -> ToolConcurrency {
        match self.effect {
            ToolEffect::ReadOnly => ToolConcurrency::ParallelReadOnly,
            ToolEffect::ChangesState => ToolConcurrency::Exclusive,
        }
    }

    /// 调用原始远端名称，并把 Turn 取消传播到 MCP `notifications/cancelled`。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            self.definition.validate_input(&input).map_err(|_| {
                ToolError::permanent(
                    "mcp_tool_input_invalid",
                    "MCP 工具输入不符合已发现的 Schema",
                )
            })?;
            if context.cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let cancellation = CancellationToken::new();
            let request = self.client.call_tool_with_cancellation(
                self.remote_name.clone(),
                input,
                &cancellation,
            );
            tokio::pin!(request);
            let result = tokio::select! {
                result = &mut request => result,
                _ = context.cancellation.cancelled() => {
                    cancellation.cancel();
                    // 等待取消通知完成是为了尽量让远端停止工作，但不能把
                    // 已取消的 Agent 永久绑定到失控的 MCP Server。
                    let _ = tokio::time::timeout(MCP_CANCELLATION_GRACE, &mut request).await;
                    return Err(cancelled_error());
                }
            }
            .map_err(map_mcp_error)?;
            normalize_mcp_result(result)
        })
    }
}

/// MCP 工具目录无法安全进入 Agent Runtime 的稳定原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpToolBridgeError {
    /// Server 标识或工具名称为空、过长或包含控制字符。
    InvalidIdentity,
    /// MCP 工具定义不满足 Provider 中立 Schema 与说明边界。
    InvalidDefinition,
    /// 当前 MCP 工具强制要求尚未实现的 Tasks 协议。
    UnsupportedTaskTool,
    /// 两个远端身份生成了相同的可移植名称，目录必须整体拒绝。
    PortableNameCollision,
}

impl McpToolBridgeError {
    /// 把严格桥接错误映射为 best-effort 报告使用的稳定分类。
    pub const fn diagnostic_code(self) -> McpDiagnosticCode {
        match self {
            Self::InvalidIdentity => McpDiagnosticCode::InvalidIdentity,
            Self::InvalidDefinition => McpDiagnosticCode::InvalidDefinition,
            Self::UnsupportedTaskTool => McpDiagnosticCode::UnsupportedTaskTool,
            Self::PortableNameCollision => McpDiagnosticCode::PortableNameCollision,
        }
    }
}

impl fmt::Display for McpToolBridgeError {
    /// 输出不包含远端工具名称、说明或 Schema 的固定错误文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdentity => "MCP 工具身份无效",
            Self::InvalidDefinition => "MCP 工具定义无效",
            Self::UnsupportedTaskTool => "MCP 工具要求未启用的 Tasks 协议",
            Self::PortableNameCollision => "MCP 工具可移植名称发生冲突",
        })
    }
}

impl Error for McpToolBridgeError {}

/// 构造一个受限且可被三种 Provider Adapter 共同编码的工具定义。
pub(super) fn mcp_tool_definition(
    server_id: &str,
    tool: &McpTool,
) -> Result<ToolDefinition, McpToolBridgeError> {
    validate_remote_identity(&tool.name)?;
    let name = portable_mcp_tool_name(server_id, &tool.name)?;
    let source_description = tool
        .description
        .as_deref()
        .or(tool.title.as_deref())
        .unwrap_or("远端 MCP 扩展工具");
    let description = bounded_description(&format!(
        "MCP Server {server_id} 的工具 {}：{source_description}",
        tool.name
    ));
    let definition = ToolDefinition::new(name, description, tool.input_schema.clone());
    definition
        .validate()
        .map_err(|_| McpToolBridgeError::InvalidDefinition)?;
    Ok(definition)
}

/// 校验不会进入日志错误正文的远端身份边界。
fn validate_remote_identity(value: &str) -> Result<(), McpToolBridgeError> {
    if value.trim().is_empty()
        || value.len() > MAX_MCP_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(McpToolBridgeError::InvalidIdentity);
    }
    Ok(())
}

/// 为扩展诊断保存安全的 Server 或工具身份；非法身份统一隐藏原文。
fn diagnostic_identity(value: &str) -> String {
    if validate_remote_identity(value).is_ok() {
        value.to_owned()
    } else {
        "<invalid>".to_owned()
    }
}

/// 清理控制字符并按 UTF-8 边界截断 MCP 诊断说明。
fn bounded_diagnostic_message(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() <= MAX_MCP_DIAGNOSTIC_MESSAGE_BYTES {
        return sanitized;
    }
    let mut end = MAX_MCP_DIAGNOSTIC_MESSAGE_BYTES;
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    sanitized[..end].to_owned()
}

/// 把远端身份转换为只包含可移植字符的人类可读片段。
fn portable_slug(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect()
}

/// 清理控制字符并按 UTF-8 边界截断远端模型说明。
pub(super) fn bounded_description(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() <= MAX_MCP_DESCRIPTION_BYTES {
        return sanitized;
    }
    let mut end = MAX_MCP_DESCRIPTION_BYTES;
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    sanitized[..end].to_owned()
}

/// 把 MCP 成功结果完整映射为模型可消费的文本与图片块。
pub(super) fn normalize_mcp_result(result: ToolCallResult) -> Result<ToolOutput, ToolError> {
    if result.is_error {
        return Err(ToolError::permanent(
            "mcp_tool_failed",
            "MCP 工具返回业务错误",
        ));
    }
    let mut content = result
        .content
        .into_iter()
        .map(normalize_mcp_content)
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(structured_content) = result.structured_content {
        let text = serde_json::to_string(&json!({ "structured_content": structured_content }))
            .map_err(|_| ToolError::permanent("mcp_result_invalid", "MCP 结构化结果无法编码"))?;
        content.push(ToolResultContent::Text { text });
    }
    if content.is_empty() {
        content.push(ToolResultContent::Text {
            text: "MCP 工具执行完成".to_owned(),
        });
    }
    Ok(ToolOutput { content })
}

/// 映射一个 MCP 内容块；非文本和非图片类型以规范 JSON 文本无损保留。
pub(super) fn normalize_mcp_content(content: McpContent) -> Result<ToolResultContent, ToolError> {
    if content.content_type == "text" {
        if let Some(text) = content.text {
            return Ok(ToolResultContent::Text { text });
        }
    } else if content.content_type == "image" {
        if let (Some(data), Some(media_type)) = (content.data.as_ref(), content.mime_type.as_ref())
        {
            return Ok(ToolResultContent::Image {
                image: ImageContent::from_base64(media_type.clone(), data.clone()),
            });
        }
    }
    let text = serde_json::to_string(&content)
        .map_err(|_| ToolError::permanent("mcp_result_invalid", "MCP 内容块无法编码"))?;
    Ok(ToolResultContent::Text { text })
}

/// 将 MCP 客户端错误归一为不会回显服务端正文的工具错误。
fn map_mcp_error(error: McpError) -> ToolError {
    match error {
        McpError::Cancelled { .. } => cancelled_error(),
        McpError::Transport(_)
        | McpError::Timeout { .. }
        | McpError::SessionExpired
        | McpError::NotReady(_) => {
            ToolError::retryable("mcp_unavailable", "MCP 工具服务暂时不可用")
        }
        McpError::Configuration(_)
        | McpError::Protocol(_)
        | McpError::Rpc { .. }
        | McpError::ResponseTooLarge { .. }
        | McpError::Pagination { .. }
        | McpError::OAuth(_) => ToolError::permanent("mcp_tool_failed", "MCP 工具调用失败"),
    }
}

/// 返回 Turn 已取消时的稳定工具错误。
fn cancelled_error() -> ToolError {
    ToolError::permanent("tool_cancelled", "当前 Turn 已取消")
}
