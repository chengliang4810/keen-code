use std::{
    collections::HashMap,
    future::Future,
    io,
    pin::Pin,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
#[cfg(windows)]
use process_wrap::tokio::{CommandWrapper, JobObject, KillOnDrop};
use rmcp::{
    model::{Resource, Tool},
    service::{Peer, QuitReason, RoleClient, RunningService, ServiceError},
    transport::{async_rw::AsyncRwTransport, Transport},
};
use thiserror::Error;
use tokio::process::{ChildStdin, ChildStdout};

use super::{
    channel_handler::ChannelHandler,
    config::{ConfigSource, McpServerConfig},
    oauth_flow::{OAuthCallbackResult, OAuthFlowEvent},
};

/// Wrapper for RunningService that can hold either handler type
pub(crate) enum McpServiceWrapper {
    Default(RunningService<RoleClient, ()>),
    Channel(RunningService<RoleClient, Arc<ChannelHandler>>),
}

impl McpServiceWrapper {
    pub async fn close_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.close_with_timeout(timeout).await,
            McpServiceWrapper::Channel(svc) => svc.close_with_timeout(timeout).await,
        }
    }

    pub async fn list_all_tools(&self) -> Result<Vec<Tool>, ServiceError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.list_all_tools().await,
            McpServiceWrapper::Channel(svc) => svc.list_all_tools().await,
        }
    }

    pub async fn list_all_resources(&self) -> Result<Vec<Resource>, ServiceError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.list_all_resources().await,
            McpServiceWrapper::Channel(svc) => svc.list_all_resources().await,
        }
    }

    pub fn peer(&self) -> &Peer<RoleClient> {
        match self {
            McpServiceWrapper::Default(svc) => svc.peer(),
            McpServiceWrapper::Channel(svc) => svc.peer(),
        }
    }
}

/// MCP 客户端连接状态
#[derive(Debug, Clone, PartialEq)]
pub enum ClientStatus {
    Connected,
    Failed(String),
    Disconnected,
    Disabled,
    /// 配置存在但从未尝试连接（不在 clients 表中，仅在 configs 表中）
    Uninitialized,
}

/// MCP 连接池初始化状态
#[derive(Debug, Clone, PartialEq)]
pub enum McpInitStatus {
    Pending,
    Initializing { connected: usize, total: usize },
    Ready { total: usize },
    Failed(String),
}

/// MCP 服务器 OAuth 授权状态
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OAuthStatus {
    /// 不使用 OAuth（stdio 传输或未配置 OAuth）
    #[default]
    None,
    /// 已授权（token 有效）
    Authorized,
    /// 需要授权（HTTP 传输且配置了 OAuth，但 token 缺失或过期）
    NeedsAuthorization,
}

/// 单个 MCP 服务器的详细信息（用于 TUI 面板展示）
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub name: String,
    pub transport_type: String,
    pub status: ClientStatus,
    pub tool_count: usize,
    pub resource_count: usize,
    /// OAuth 授权状态
    pub oauth_status: OAuthStatus,
    /// 配置来源
    pub source: Option<ConfigSource>,
    /// 服务器 URL（HTTP 传输）
    pub url: Option<String>,
    /// 插件来源标识（`"name@marketplace"`），非插件 server 为 None
    pub plugin_source: Option<String>,
}

/// 连接池级别错误
#[derive(Debug, Error)]
pub enum McpPoolError {
    #[error("MCP 服务器 \"{server}\" 连接失败: {reason}")]
    ConnectionFailed { server: String, reason: String },
    #[error("MCP 服务器 \"{server}\" 工具发现失败: {reason}")]
    ToolDiscoveryFailed { server: String, reason: String },
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected {
        server: String,
        status: ClientStatus,
    },
}

/// 单个 MCP 服务器的客户端句柄
#[derive(Clone)]
pub struct McpClientHandle {
    pub name: String,
    pub peer: Option<Peer<RoleClient>>,
    pub tools: Vec<Tool>,
    pub resources: Vec<Resource>,
    pub status: ClientStatus,
    pub oauth_status: OAuthStatus,
    /// 配置来源
    pub source: Option<ConfigSource>,
    /// 服务器 URL（HTTP 传输）
    pub url: Option<String>,
    /// Whether the MCP server declared experimental.claude/channel capability
    pub channel_capable: bool,
}

/// MCP 客户端连接池
pub struct McpClientPool {
    pub(crate) clients: parking_lot::RwLock<HashMap<String, Arc<McpClientHandle>>>,
    pub(crate) services: tokio::sync::Mutex<HashMap<String, McpServiceWrapper>>,
    pub(crate) configs: parking_lot::RwLock<HashMap<String, McpServerConfig>>,
    /// 插件来源旁路表：key 为 server name（如 `"plugin:p1:srv1"`），value 为 `"name@marketplace"`
    pub(crate) plugin_sources: parking_lot::RwLock<HashMap<String, String>>,
    /// 初始化阶段内部存储（M-TUI 收口：TUI 不再持有 watch channel，`mcp/list`
    /// 命令面经 `McpPoolPort::snapshot` 读取；`run_initialize` 与外部
    /// `status_tx` 同步更新）。
    pub(crate) init_status: parking_lot::RwLock<McpInitStatus>,
    /// 初始化是否已完成。完成前发生的状态写入**不**产生上下线通知——
    /// 会话首 turn 的 `first_turn_reminder` 概览已覆盖初始连接结果，避免与
    /// 逐台上线事件重复（初始化未完成时，迟到的连接成功自然成为运行中变化）。
    pub(crate) initialized: std::sync::atomic::AtomicBool,
    /// 运行中状态变化的待注入文本缓冲（McpMiddleware::before_model drain 后
    /// 以 Info 消息推送进模型上下文；全局缓冲，任一会话消费一次即清空）。
    pub(crate) pending_changes: parking_lot::Mutex<Vec<String>>,
    /// 状态变化通知回调（装配时注入；发布 system-notification 给 TUI 通知面）。
    notifier: parking_lot::RwLock<Option<Box<dyn Fn(&str) + Send + Sync>>>,
    /// OAuth 流程事件回调（装配时注入；`AuthorizationNeeded` 需把
    /// `callback_tx` 注册进 `pending_oauth_callbacks` 供授权码回传 RPC 投递，
    /// 其余事件转发为 ACP `oauth-needed` / `oauth-completed` / `oauth-failed`）。
    oauth_event_callback: parking_lot::RwLock<Option<Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>>>,
    /// 待完成 OAuth 授权的回调通道（key: server_name）。TUI 经
    /// `mcp/oauth_callback` RPC 回传授权码时由 host 装配面查表投递。
    pending_oauth_callbacks:
        parking_lot::Mutex<HashMap<String, tokio::sync::oneshot::Sender<OAuthCallbackResult>>>,
}

/// MCP 状态变化 → 通知文本（每台一行：上线带工具数，失败报名字 + 错误）。
pub(crate) fn status_change_text(name: &str, status: &ClientStatus, tool_count: usize) -> String {
    match status {
        ClientStatus::Connected => {
            format!("MCP: {name} connected ({tool_count} tools)")
        }
        ClientStatus::Failed(reason) => {
            format!("MCP: {name} failed: {reason}")
        }
        ClientStatus::Disconnected => {
            format!("MCP: {name} disconnected")
        }
        ClientStatus::Disabled => {
            format!("MCP: {name} disabled")
        }
        ClientStatus::Uninitialized => {
            format!("MCP: {name} uninitialized")
        }
    }
}

pub(crate) const STDIO_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// MCP stdio graceful shutdown 的最长等待时间，与 rmcp 默认实现保持一致。
const MCP_CHILD_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Windows 桌面应用启动 MCP 子进程时不创建控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Job Object 绑定前让 Windows 子进程保持挂起，消除 spawn 后再绑定的竞态窗口。
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// 在 Job Object 的挂起创建标志基础上保留桌面应用的无控制台行为。
#[cfg(windows)]
#[derive(Debug)]
struct WindowsNoWindow;

#[cfg(windows)]
impl CommandWrapper for WindowsNoWindow {
    /// 恢复 CREATE_NO_WINDOW，并保留 JobObject 要求的 CREATE_SUSPENDED。
    fn pre_spawn(
        &mut self,
        command: &mut tokio::process::Command,
        _core: &CommandWrap,
    ) -> std::io::Result<()> {
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        Ok(())
    }
}

/// MCP child wrapper：同步 Drop 时终止完整进程组或 Job Object。
///
/// rmcp 的 `TokioChildProcess` 在 Drop 中通过 `tokio::spawn` 异步调用 child
/// `kill()`；当 transport 在运行时关闭或同步清理路径释放时，这个任务可能
/// 无法创建。此 wrapper 将 process-wrap 的 `start_kill()` 提升到同步 Drop，
/// 保证 Unix 后代和 Windows Job Object 成员都不会依赖运行时继续存活。
#[derive(Debug)]
struct McpChild {
    /// 被 process-wrap 平台 wrapper 包装的实际子进程。
    inner: Option<Box<dyn ChildWrapper>>,
    /// 尚未成功 wait 时，Drop 是否需要终止整个进程树。
    terminate_on_drop: bool,
}

impl McpChild {
    /// 从 process-wrap child 创建带同步 Drop 清理语义的 MCP child。
    fn new(inner: Box<dyn ChildWrapper>) -> Self {
        Self {
            inner: Some(inner),
            terminate_on_drop: true,
        }
    }
}

impl ChildWrapper for McpChild {
    /// 返回底层 process-wrap child wrapper。
    fn inner(&self) -> &dyn ChildWrapper {
        self.inner
            .as_ref()
            .expect("MCP child wrapper is already consumed")
            .inner()
    }

    /// 返回可变的底层 process-wrap child wrapper。
    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.inner
            .as_mut()
            .expect("MCP child wrapper is already consumed")
            .inner_mut()
    }

    /// 消费 wrapper 并转移 child 所有权，同时关闭本 wrapper 的 Drop 清理。
    fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
        self.terminate_on_drop = false;
        self.inner
            .take()
            .expect("MCP child wrapper is already consumed")
    }

    /// 等待完整进程树退出；成功后取消 Drop 的重复终止。
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
        Box::pin(async move {
            let result = self
                .inner
                .as_mut()
                .expect("MCP child wrapper is already consumed")
                .wait()
                .await;
            if result.is_ok() {
                self.terminate_on_drop = false;
            }
            result
        })
    }

    /// 同步向完整进程树发送强制终止请求。
    fn start_kill(&mut self) -> io::Result<()> {
        self.inner
            .as_mut()
            .expect("MCP child wrapper is already consumed")
            .start_kill()
    }
}

impl Drop for McpChild {
    /// 在任意同步 Drop 路径终止整棵 MCP 进程树，不能只依赖 Tokio 根进程清理。
    fn drop(&mut self) {
        if self.terminate_on_drop {
            if let Some(inner) = self.inner.as_mut() {
                // 进程已退出时的错误属于正常竞态；Drop 不能传播错误或阻塞等待。
                let _ = inner.start_kill();
            }
        }
    }
}

/// MCP stdio transport：复用 rmcp 的 JSON-RPC 读写实现，并持有同步清理 child。
pub(crate) struct McpStdioTransport {
    /// 仍由当前 transport 管理的 MCP 子进程；graceful close 成功后置空。
    child: Option<McpChild>,
    /// rmcp 标准异步读写 transport，保持原有协议和消息编解码行为。
    transport: AsyncRwTransport<RoleClient, ChildStdout, ChildStdin>,
}

impl McpStdioTransport {
    /// 关闭写端并等待 child；超时则同步终止整棵进程树后等待回收。
    async fn graceful_shutdown(&mut self) -> io::Result<()> {
        self.transport.close().await?;
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };

        let wait_fut = child.wait();
        tokio::select! {
            _ = tokio::time::sleep(MCP_CHILD_SHUTDOWN_TIMEOUT) => {
                if let Err(error) = Box::into_pin(child.kill()).await {
                    tracing::warn!(error = %error, "关闭 MCP stdio 子进程失败");
                    return Err(error);
                }
            }
            result = wait_fut => {
                result?;
            }
        }
        Ok(())
    }
}

impl Transport<RoleClient> for McpStdioTransport {
    type Error = io::Error;

    /// 发送 JSON-RPC 消息，委托 rmcp 的标准异步读写 transport。
    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.transport.send(item)
    }

    /// 接收 JSON-RPC 消息，委托 rmcp 的标准异步读写 transport。
    fn receive(
        &mut self,
    ) -> impl Future<Output = Option<rmcp::service::RxJsonRpcMessage<RoleClient>>> + Send {
        self.transport.receive()
    }

    /// 先关闭写端，再优雅等待或强制终止 MCP child。
    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        self.graceful_shutdown()
    }
}

impl McpClientPool {
    pub fn new_pending() -> Self {
        Self {
            clients: parking_lot::RwLock::new(HashMap::new()),
            services: tokio::sync::Mutex::new(HashMap::new()),
            configs: parking_lot::RwLock::new(HashMap::new()),
            plugin_sources: parking_lot::RwLock::new(HashMap::new()),
            init_status: parking_lot::RwLock::new(McpInitStatus::Pending),
            initialized: std::sync::atomic::AtomicBool::new(false),
            pending_changes: parking_lot::Mutex::new(Vec::new()),
            notifier: parking_lot::RwLock::new(None),
            oauth_event_callback: parking_lot::RwLock::new(None),
            pending_oauth_callbacks: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn new_empty() -> Self {
        Self::new_pending()
    }

    /// 注入 OAuth 流程事件回调（host 装配面在 `run_initialize` 前调用；
    /// 回调负责把 `AuthorizationNeeded` 的 `callback_tx` 注册进
    /// `pending_oauth_callbacks`，并将事件转发为 ACP 通知）。
    pub fn set_oauth_event_callback<F>(&self, callback: F)
    where
        F: Fn(OAuthFlowEvent) + Send + Sync + 'static,
    {
        *self.oauth_event_callback.write() = Some(Arc::new(callback));
    }

    /// 读取 OAuth 流程事件回调（spawn 授权任务时克隆给 `OAuthFlowManager`）。
    pub(crate) fn oauth_event_callback(&self) -> Option<Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>> {
        self.oauth_event_callback.read().clone()
    }

    /// 注册待完成 OAuth 授权的回调通道（`AuthorizationNeeded` 事件处理时调用）。
    pub fn register_oauth_callback(
        &self,
        server_name: &str,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) {
        self.pending_oauth_callbacks
            .lock()
            .insert(server_name.to_string(), callback_tx);
    }

    /// 投递授权码回传（`mcp/oauth_callback` RPC 调用）：查表取 `callback_tx`
    /// 投递 `{code, state}`；无 pending 通道返回错误。
    pub fn deliver_oauth_callback(
        &self,
        server_name: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        let tx = self
            .pending_oauth_callbacks
            .lock()
            .remove(server_name)
            .ok_or_else(|| format!("{server_name} 无进行中的 OAuth 授权"))?;
        tx.send(OAuthCallbackResult { code, state })
            .map_err(|_| format!("{server_name} OAuth 授权流程已结束"))
    }

    /// 取消进行中的 OAuth 授权（`mcp/oauth_cancel` RPC 调用）：移除 pending
    /// 通道并 drop sender，后台 `run_oauth_flow` 收到 Cancelled 终止。
    pub fn cancel_oauth_callback(&self, server_name: &str) -> bool {
        self.pending_oauth_callbacks
            .lock()
            .remove(server_name)
            .is_some()
    }

    /// 查询指定 server 的插件来源标识，非插件 server 返回 None
    /// key 格式为 `"plugin_name__server_name"`，返回 `"name@marketplace"`
    pub fn plugin_source_of(&self, name: &str) -> Option<String> {
        self.plugin_sources.read().get(name).cloned()
    }

    pub(crate) fn insert_failed(pool: &Arc<Self>, name: &str, reason: String) {
        let (source, url) = pool
            .configs
            .read()
            .get(name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
        pool.clients.write().insert(
            name.to_string(),
            Arc::new(McpClientHandle {
                name: name.to_string(),
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason.clone()),
                oauth_status: OAuthStatus::default(),
                source,
                url,
                channel_capable: false,
            }),
        );
        pool.record_status_change(name, old_status.as_ref());
        peri_agent::metrics::emit(
            "mcp.error",
            serde_json::json!({
                "server": name,
                "tool": "connect",
                "error": reason,
            }),
            None,
            None,
        );
    }

    /// 插入需要 OAuth 授权的服务器（HTTP 传输收到 401/AuthRequired 时使用）
    pub(crate) fn insert_needs_auth(pool: &Arc<Self>, name: &str, reason: String) {
        tracing::info!(server = %name, "HTTP 服务器需要 OAuth 授权，可在 MCP 面板按 r 键触发");
        let (source, url) = pool
            .configs
            .read()
            .get(name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
        pool.clients.write().insert(
            name.to_string(),
            Arc::new(McpClientHandle {
                name: name.to_string(),
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason),
                oauth_status: OAuthStatus::NeedsAuthorization,
                source,
                url,
                channel_capable: false,
            }),
        );
        pool.record_status_change(name, old_status.as_ref());
    }

    /// 检测错误是否为 HTTP 401 认证错误
    pub(crate) fn is_auth_required_error(error: &str, transport_is_http: bool) -> bool {
        transport_is_http && (error.contains("Auth required") || error.contains("AuthRequired"))
    }

    pub async fn remove_server(self: &Arc<Self>, server_name: &str) {
        self.clients.write().remove(server_name);
        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        self.configs.write().remove(server_name);
    }

    /// 将服务器标记为 Disabled：关闭连接但保留 config 和 handle（用于面板展示）
    pub async fn set_disabled(self: &Arc<Self>, server_name: &str) {
        // 关闭实际连接
        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        // 更新 handle 为 Disabled 状态（保留 config 引用）
        let (source, url) = self
            .configs
            .read()
            .get(server_name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        self.clients.write().insert(
            server_name.to_string(),
            Arc::new(McpClientHandle {
                name: server_name.to_string(),
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Disabled,
                oauth_status: OAuthStatus::default(),
                source,
                url,
                channel_capable: false,
            }),
        );
    }

    pub fn server_infos(&self) -> Vec<ServerInfo> {
        self.clients
            .read()
            .values()
            .map(|h| ServerInfo {
                name: h.name.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                tool_count: h.tools.len(),
                resource_count: h.resources.len(),
                oauth_status: h.oauth_status.clone(),
                source: h.source.clone(),
                url: h.url.clone(),
                plugin_source: self.plugin_source_of(&h.name),
            })
            .collect()
    }

    /// 返回所有 MCP 服务器信息（合并 configs + clients）
    ///
    /// config 中有但 clients 中没有的 server 会被标记为 Uninitialized。
    /// 这覆盖了连接失败后被移除、运行时新增配置、以及 disabled 后被清理等场景。
    pub fn all_server_infos(&self) -> Vec<ServerInfo> {
        let clients = self.clients.read();
        let configs = self.configs.read();

        let mut result: Vec<ServerInfo> = Vec::new();

        // 先遍历 clients 表中的所有条目
        for h in clients.values() {
            result.push(ServerInfo {
                name: h.name.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                tool_count: h.tools.len(),
                resource_count: h.resources.len(),
                oauth_status: h.oauth_status.clone(),
                source: h.source.clone(),
                url: h.url.clone(),
                plugin_source: self.plugin_source_of(&h.name),
            });
        }

        // 遍历 configs，补充 clients 中不存在的条目（标记为 Uninitialized）
        for (name, sc) in configs.iter() {
            if !clients.contains_key(name) {
                result.push(ServerInfo {
                    name: name.clone(),
                    transport_type: if sc.url.is_some() { "http" } else { "stdio" }.to_string(),
                    status: ClientStatus::Uninitialized,
                    tool_count: 0,
                    resource_count: 0,
                    oauth_status: OAuthStatus::default(),
                    source: sc.source.clone(),
                    url: sc.url.clone(),
                    plugin_source: self.plugin_source_of(name),
                });
            }
        }

        result
    }

    pub fn get_tools(&self, name: &str) -> Vec<Tool> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.tools.clone())
            .unwrap_or_default()
    }
    pub fn get_resources(&self, name: &str) -> Vec<Resource> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.resources.clone())
            .unwrap_or_default()
    }
    pub fn get_client(&self, name: &str) -> Option<Arc<McpClientHandle>> {
        self.clients.read().get(name).cloned()
    }
    pub fn get_all_clients(&self) -> Vec<Arc<McpClientHandle>> {
        self.clients
            .read()
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected))
            .cloned()
            .collect()
    }
    pub fn has_resources(&self) -> bool {
        self.clients
            .read()
            .values()
            .any(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
    }
    pub fn resource_summary(&self) -> String {
        self.clients
            .read()
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
            .map(|c| {
                format!(
                    "- server \"{}\": {} ({} resources)",
                    c.name,
                    c.resources
                        .iter()
                        .map(|r| r.uri.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    c.resources.len()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub async fn shutdown(&self) {
        let names: Vec<String> = self.clients.read().keys().cloned().collect();
        for name in &names {
            if let Some(c) = self.clients.write().get_mut(name) {
                if matches!(c.status, ClientStatus::Connected) {
                    tracing::info!(server = %name, "关闭连接");
                }
                let h = Arc::make_mut(c);
                h.status = ClientStatus::Disconnected;
                h.peer = None;
            }
        }
        for (_name, mut svc) in self.services.lock().await.drain() {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
    }

    /// 关闭现有连接并把连接池恢复成可重新初始化的 Pending 状态。
    ///
    /// OAuth 事件回调与状态通知回调属于宿主生命周期配置，重载时保留；服务器
    /// 配置、连接句柄、插件来源和尚未完成的授权回传则随旧配置一起清空。
    pub async fn reset_for_reinitialize(&self) {
        self.shutdown().await;
        self.clients.write().clear();
        self.configs.write().clear();
        self.plugin_sources.write().clear();
        self.pending_oauth_callbacks.lock().clear();
        self.pending_changes.lock().clear();
        *self.init_status.write() = McpInitStatus::Pending;
        self.initialized
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    // ── 状态变化统一出口（上下线通知） ──────────────────────────────────────

    /// 标记初始化完成。此后发生的状态变化才产生上下线通知（初始化期间的
    /// 连接结果由首 turn 概览覆盖，不逐条通知）。
    pub fn mark_initialized(&self) {
        self.initialized
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 记录一次状态变化（统一出口）。
    ///
    /// `old` 为调用方在修改客户端表**之前**捕获的旧状态；`None` 表示表内
    /// 此前不存在（首次插入/重建，无"变化"语义，不通知）。调用方完成
    /// 状态写入后调用本方法。
    ///
    /// 仅当：初始化已完成 + 表内存在且状态确实变化时，生成一行通知文本
    /// （`status_change_text`）写入 `pending_changes` 缓冲（McpMiddleware
    /// 经 before_model drain 后以 Info 消息推送进模型上下文），并调用
    /// notifier 回调（发布 system-notification 给 TUI 通知面）。
    pub(crate) fn record_status_change(&self, name: &str, old: Option<&ClientStatus>) {
        if !self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(old) = old else { return };
        let clients = self.clients.read();
        let Some(handle) = clients.get(name) else {
            return;
        };
        if &handle.status == old {
            return;
        }
        let text = status_change_text(name, &handle.status, handle.tools.len());
        self.pending_changes.lock().push(text.clone());
        if let Some(notifier) = self.notifier.read().as_ref() {
            notifier(&text);
        }
    }

    /// 注入状态变化通知回调（发布 system-notification 事件；装配时调用）。
    pub fn set_notifier(&self, notifier: Box<dyn Fn(&str) + Send + Sync>) {
        *self.notifier.write() = Some(notifier);
    }

    /// 取出待注入的状态变化文本（McpMiddleware::before_model 调用；恰好一次）。
    pub(crate) fn drain_pending_changes(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending_changes.lock())
    }
}

pub(crate) fn spawn_stdio_transport(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> std::io::Result<McpStdioTransport> {
    let arg_strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let shell_command = peri_agent::agent::async_tasks::shell_command(command, &arg_strs);
    let mut cmd = CommandWrap::from(shell_command);
    cmd.command_mut()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        // bash 作为新进程组组长；process-wrap 的 kill 会向组内所有进程发送信号，
        // 避免 MCP server 自行派生的子进程在连接取消后成为孤儿。
        cmd.wrap(ProcessGroup::leader());
    }
    #[cfg(windows)]
    {
        // Job Object 从创建时即接管进程，KillOnDrop 在 rmcp transport 释放时
        // 触发整树清理。JobObject 先设置 CREATE_SUSPENDED，再由最后的 wrapper
        // 恢复 CREATE_NO_WINDOW，避免子进程先运行后才绑定 Job Object 的竞态。
        cmd.wrap(KillOnDrop);
        cmd.wrap(JobObject);
        cmd.wrap(WindowsNoWindow);
    }

    // 先将 child 包装成本地 RAII owner，再提取 stdio；任一提取失败都会
    // 触发同步 Drop，覆盖 spawn 成功后的异常窗口。
    let mut child = McpChild::new(cmd.spawn()?);
    let stdin = child
        .stdin()
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "MCP 子进程 stdin 管道不可用"))?;
    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "MCP 子进程 stdout 管道不可用"))?;
    let stderr_opt = child.stderr().take();

    // 启动后台任务消费 stderr 并记录到 tracing
    if let Some(stderr) = stderr_opt {
        let cmd_name = command.to_string();
        let stderr_task = async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(
                    command = %cmd_name,
                    stderr = %line,
                    "MCP 子进程 stderr"
                );
            }
        };
        // 正常调用来自 Tokio runtime；无 runtime 的同步构造路径不能调用
        // `tokio::spawn`，此时直接关闭 stderr，transport Drop 仍负责杀树。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(stderr_task);
        }
    }

    Ok(McpStdioTransport {
        child: Some(child),
        transport: AsyncRwTransport::new(stdout, stdin),
    })
}

pub(crate) fn build_http_transport(
    url: &str,
    headers: &HashMap<String, String>,
) -> rmcp::transport::StreamableHttpClientTransport<reqwest::Client> {
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    let mut config = StreamableHttpClientTransportConfig::with_uri(url);
    let mut custom_headers = std::collections::HashMap::new();
    for (key, value) in headers {
        match reqwest::header::HeaderName::try_from(key.as_str()) {
            Ok(name) => match reqwest::header::HeaderValue::from_str(value) {
                Ok(val) => {
                    custom_headers.insert(name, val);
                }
                Err(e) => {
                    tracing::warn!(header = %key, error = %e, "header 值无效");
                }
            },
            Err(e) => {
                tracing::warn!(header = %key, error = %e, "header 名称无效");
            }
        }
    }
    if !custom_headers.is_empty() {
        config = config.custom_headers(custom_headers);
    }
    rmcp::transport::StreamableHttpClientTransport::with_client(reqwest::Client::new(), config)
}

pub(crate) fn build_authed_transport(
    url: &str,
    headers: &HashMap<String, String>,
    auth_manager: rmcp::transport::auth::AuthorizationManager,
) -> rmcp::transport::StreamableHttpClientTransport<
    rmcp::transport::auth::AuthClient<reqwest::Client>,
> {
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    let mut config = StreamableHttpClientTransportConfig::with_uri(url);
    let mut custom_headers = std::collections::HashMap::new();
    for (key, value) in headers {
        match reqwest::header::HeaderName::try_from(key.as_str()) {
            Ok(name) => match reqwest::header::HeaderValue::from_str(value) {
                Ok(val) => {
                    custom_headers.insert(name, val);
                }
                Err(e) => {
                    tracing::warn!(header = %key, error = %e, "header 值无效");
                }
            },
            Err(e) => {
                tracing::warn!(header = %key, error = %e, "header 名称无效");
            }
        }
    }
    if !custom_headers.is_empty() {
        config = config.custom_headers(custom_headers);
    }
    let auth_client = rmcp::transport::auth::AuthClient::new(reqwest::Client::new(), auth_manager);
    rmcp::transport::StreamableHttpClientTransport::with_client(auth_client, config)
}

// 3.0 批 2 波 2：装配注入端口实现（ACP 侧只持 `Arc<dyn McpPoolPort>`）。
// M-TUI 收口：`shutdown`（host/shutdown 命令面）与 `snapshot`（mcp/list
// 命令面）为新增数据端口；TUI 不再直持池句柄与 watch channel。
#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for McpClientPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) {
        McpClientPool::shutdown(self).await;
    }

    fn snapshot(&self) -> serde_json::Value {
        let init_phase = match &*self.init_status.read() {
            McpInitStatus::Pending => "pending",
            McpInitStatus::Initializing { .. } => "initializing",
            McpInitStatus::Ready { .. } => "ready",
            McpInitStatus::Failed(_) => "failed",
        };
        let infos = self.all_server_infos();
        serde_json::json!({
            "initPhase": init_phase,
            "servers": infos.iter().map(|info| serde_json::json!({
                "name": info.name.clone(),
                "status": match &info.status {
                    ClientStatus::Connected => "connected",
                    ClientStatus::Failed(_) => "failed",
                    ClientStatus::Disconnected => "disconnected",
                    ClientStatus::Disabled => "disabled",
                    ClientStatus::Uninitialized => "uninitialized",
                },
                "oauthStatus": match &info.oauth_status {
                    OAuthStatus::None => "none",
                    OAuthStatus::Authorized => "authorized",
                    OAuthStatus::NeedsAuthorization => "needs_authorization",
                },
                "error": match &info.status {
                    ClientStatus::Failed(error) => Some(error.clone()),
                    _ => None,
                },
                "transport": info.transport_type.clone(),
                "toolsCount": info.tool_count,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
