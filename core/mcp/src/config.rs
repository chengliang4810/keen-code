//! MCP 服务端与客户端运行参数。

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use url::Url;

use crate::DEFAULT_PROTOCOL_VERSION;
use crate::auth::McpAuthProvider;
use crate::error::McpError;
use crate::types::{ClientCapabilities, ImplementationInfo};

/// MCP 服务的传输配置。
#[derive(Clone)]
pub enum McpServerConfig {
    /// 通过本地子进程标准输入输出通信。
    Stdio(StdioServerConfig),
    /// 通过 MCP Streamable HTTP 端点通信。
    StreamableHttp(StreamableHttpConfig),
}

impl fmt::Debug for McpServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio(config) => formatter.debug_tuple("Stdio").field(config).finish(),
            Self::StreamableHttp(config) => formatter
                .debug_tuple("StreamableHttp")
                .field(config)
                .finish(),
        }
    }
}

/// stdio MCP 子进程配置。
#[derive(Clone)]
pub struct StdioServerConfig {
    /// 要启动的可执行文件或可由系统解析的命令名。
    pub command: String,
    /// 原样传给子进程的参数。
    pub args: Vec<String>,
    /// 子进程工作目录；为空时继承当前进程目录。
    pub current_dir: Option<PathBuf>,
    /// 覆盖或补充给子进程的环境变量。
    pub environment: BTreeMap<String, String>,
    /// 是否继承当前进程的环境变量。
    pub inherit_environment: bool,
}

impl StdioServerConfig {
    /// 使用指定命令和默认继承环境创建配置。
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            current_dir: None,
            environment: BTreeMap::new(),
            inherit_environment: true,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), McpError> {
        if self.command.trim().is_empty() {
            return Err(McpError::Configuration("stdio command 不得为空".to_owned()));
        }
        if let Some(current_dir) = self
            .current_dir
            .as_ref()
            .filter(|current_dir| !current_dir.is_dir())
        {
            return Err(McpError::Configuration(format!(
                "stdio 工作目录不存在或不是目录：{}",
                current_dir.display()
            )));
        }
        if self.environment.keys().any(|name| name.is_empty()) {
            return Err(McpError::Configuration(
                "stdio 环境变量名不得为空".to_owned(),
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for StdioServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StdioServerConfig")
            .field("command", &self.command)
            .field("args_count", &self.args.len())
            .field("current_dir", &self.current_dir)
            .field(
                "environment",
                &self
                    .environment
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .field("inherit_environment", &self.inherit_environment)
            .finish()
    }
}

/// Streamable HTTP MCP 端点配置。
#[derive(Clone)]
pub struct StreamableHttpConfig {
    /// 接收 MCP POST 请求的完整 URL。
    pub endpoint: String,
    /// 每个请求携带的附加 HTTP 请求头；值可能包含密钥，不会出现在 `Debug` 输出中。
    pub headers: BTreeMap<String, String>,
    /// 可选的按请求动态认证提供方；具体 OAuth 刷新由上层 registry 负责。
    pub auth_provider: Option<Arc<dyn McpAuthProvider>>,
    /// 关闭客户端时是否尝试用 HTTP DELETE 终止服务端会话。
    pub terminate_session_on_close: bool,
}

impl StreamableHttpConfig {
    /// 使用指定 MCP 端点和空请求头创建配置。
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            headers: BTreeMap::new(),
            auth_provider: None,
            terminate_session_on_close: true,
        }
    }

    pub(crate) fn validate(&self) -> Result<Url, McpError> {
        let endpoint = Url::parse(&self.endpoint).map_err(|error| {
            McpError::Configuration(format!("Streamable HTTP endpoint 无效：{error}"))
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(McpError::Configuration(
                "Streamable HTTP endpoint 必须使用 HTTP 或 HTTPS".to_owned(),
            ));
        }
        if endpoint.scheme() == "http" && !is_loopback_endpoint(&endpoint) {
            return Err(McpError::Configuration(
                "远端 Streamable HTTP endpoint 必须使用 HTTPS；HTTP 只允许 localhost 或回环 IP"
                    .to_owned(),
            ));
        }
        if endpoint.username() != "" || endpoint.password().is_some() {
            return Err(McpError::Configuration(
                "Streamable HTTP endpoint 不得在 URL 中携带凭据".to_owned(),
            ));
        }
        if self.auth_provider.is_some()
            && self
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("authorization"))
        {
            return Err(McpError::Configuration(
                "动态 OAuth 认证不能与静态 Authorization 请求头同时配置".to_owned(),
            ));
        }
        Ok(endpoint)
    }
}

fn is_loopback_endpoint(endpoint: &Url) -> bool {
    match endpoint.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

impl fmt::Debug for StreamableHttpConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoint_without_query = Url::parse(&self.endpoint).ok().map(|mut endpoint| {
            endpoint.set_query(None);
            endpoint.set_fragment(None);
            endpoint.to_string()
        });
        formatter
            .debug_struct("StreamableHttpConfig")
            .field("endpoint", &endpoint_without_query)
            .field(
                "header_names",
                &self.headers.keys().map(String::as_str).collect::<Vec<_>>(),
            )
            .field(
                "auth_provider",
                &self.auth_provider.as_ref().map(|_| "<configured>"),
            )
            .field(
                "terminate_session_on_close",
                &self.terminate_session_on_close,
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::StreamableHttpConfig;
    use crate::auth::{AuthToken, McpAuthProvider};
    use crate::error::McpError;

    struct TestAuthProvider;

    #[async_trait]
    impl McpAuthProvider for TestAuthProvider {
        async fn access_token(&self) -> Result<Option<AuthToken>, McpError> {
            Ok(None)
        }

        async fn on_unauthorized(
            &self,
            _sent_generation: u64,
            _www_authenticate: Option<&str>,
        ) -> Result<(), McpError> {
            Ok(())
        }
    }

    /// 动态认证与静态 Authorization 必须互斥。
    #[test]
    fn dynamic_auth_rejects_static_authorization_header() {
        let mut config = StreamableHttpConfig::new("http://127.0.0.1:1/mcp");
        config
            .headers
            .insert("aUtHoRiZaTion".to_owned(), "Bearer static".to_owned());
        config.auth_provider = Some(Arc::new(TestAuthProvider));
        assert!(config.validate().is_err());
    }
}

/// MCP 客户端的协议、超时与资源保护参数。
#[derive(Debug, Clone)]
pub struct McpClientOptions {
    /// initialize 请求声明的 MCP 协议版本。
    pub protocol_version: String,
    /// initialize 请求中的客户端实现信息。
    pub client_info: ImplementationInfo,
    /// initialize 请求中的客户端能力声明。
    pub capabilities: ClientCapabilities,
    /// 每个 JSON-RPC 请求允许等待的最长时间。
    pub request_timeout: Duration,
    /// stdio 单行或 HTTP 单次响应允许读取的最大字节数。
    pub max_response_bytes: usize,
    /// 单次 list 操作允许读取的最大页数。
    pub max_pages: usize,
    /// 单次 list 操作允许累计的最大条目数。
    pub max_total_items: usize,
    /// 单次 list 操作允许累计的序列化结果字节数。
    pub max_total_result_bytes: usize,
    /// 单个分页游标允许占用的最大 UTF-8 字节数。
    pub max_cursor_bytes: usize,
    /// 单次 list 操作允许累计保留的分页游标 UTF-8 字节数。
    pub max_total_cursor_bytes: usize,
    /// 异步通知广播缓冲区容量。
    pub notification_capacity: usize,
    /// 关闭 stdio 子进程或 Streamable HTTP 会话时允许等待的最长时间。
    pub shutdown_timeout: Duration,
}

impl Default for McpClientOptions {
    fn default() -> Self {
        Self {
            protocol_version: DEFAULT_PROTOCOL_VERSION.to_owned(),
            client_info: ImplementationInfo {
                name: "keencode".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                title: Some("KeenCode".to_owned()),
            },
            capabilities: ClientCapabilities::default(),
            request_timeout: Duration::from_secs(60),
            max_response_bytes: 8 * 1024 * 1024,
            max_pages: 100,
            max_total_items: 10_000,
            max_total_result_bytes: 32 * 1024 * 1024,
            max_cursor_bytes: 64 * 1024,
            max_total_cursor_bytes: 1024 * 1024,
            notification_capacity: 256,
            shutdown_timeout: Duration::from_secs(3),
        }
    }
}

impl McpClientOptions {
    pub(crate) fn validate(&self) -> Result<(), McpError> {
        if self.protocol_version != DEFAULT_PROTOCOL_VERSION {
            return Err(McpError::Configuration(format!(
                "当前实现仅支持 MCP 协议版本 {DEFAULT_PROTOCOL_VERSION}，不能声明 {:?}",
                self.protocol_version
            )));
        }
        if self.client_info.name.trim().is_empty() || self.client_info.version.trim().is_empty() {
            return Err(McpError::Configuration(
                "client_info 的 name 和 version 不得为空".to_owned(),
            ));
        }
        if self.capabilities != ClientCapabilities::default() {
            return Err(McpError::Configuration(
                "当前客户端没有实现 roots、sampling、elicitation、tasks 或实验性服务端请求，不能声明对应 capability"
                    .to_owned(),
            ));
        }
        if self.request_timeout.is_zero() {
            return Err(McpError::Configuration(
                "request_timeout 必须大于零".to_owned(),
            ));
        }
        if self.max_response_bytes == 0 {
            return Err(McpError::Configuration(
                "max_response_bytes 必须大于零".to_owned(),
            ));
        }
        if self.max_pages == 0 {
            return Err(McpError::Configuration("max_pages 必须大于零".to_owned()));
        }
        if self.max_total_items == 0 {
            return Err(McpError::Configuration(
                "max_total_items 必须大于零".to_owned(),
            ));
        }
        if self.max_total_result_bytes == 0 {
            return Err(McpError::Configuration(
                "max_total_result_bytes 必须大于零".to_owned(),
            ));
        }
        if self.max_cursor_bytes == 0 {
            return Err(McpError::Configuration(
                "max_cursor_bytes 必须大于零".to_owned(),
            ));
        }
        if self.max_total_cursor_bytes == 0 {
            return Err(McpError::Configuration(
                "max_total_cursor_bytes 必须大于零".to_owned(),
            ));
        }
        if self.notification_capacity == 0 {
            return Err(McpError::Configuration(
                "notification_capacity 必须大于零".to_owned(),
            ));
        }
        if self.shutdown_timeout.is_zero() {
            return Err(McpError::Configuration(
                "shutdown_timeout 必须大于零".to_owned(),
            ));
        }
        Ok(())
    }
}
