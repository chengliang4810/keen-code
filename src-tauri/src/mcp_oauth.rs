//! MCP OAuth 的桌面宿主服务。
//!
//! 本模块把 `keencode-mcp` 中与 Provider 无关的 OAuth 状态机接到桌面进程：
//! 令牌只进入系统密钥库，PKCE pending 只留在当前进程，浏览器回调只绑定本次
//! 授权生成的本机回环端口。模块不打开浏览器，也不把 OAuth 私密字段放入 ACP。

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use keencode_mcp::{
    AuthToken, McpAuthProvider, McpError, OAuthAuthorizationRequest, OAuthCallback, OAuthConfig,
    OAuthError, OAuthMachine, OAuthSnapshot, OAuthStatus, OAuthTokenRequest, OAuthTokenSet,
    ReqwestOAuthTokenExchanger,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use url::Url;

/// 所有本模块的可注入异步边界都使用这一种对象安全 future 形状。
type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 不额外引入运行时依赖的可克隆异步取消信号。
#[derive(Clone, Default)]
struct CancellationSignal {
    /// 取消状态的原子快照。
    cancelled: Arc<AtomicBool>,
    /// 取消通知，避免监听器持续轮询。
    notify: Arc<Notify>,
}

impl CancellationSignal {
    /// 创建一个尚未取消的信号。
    fn new() -> Self {
        Self::default()
    }

    /// 发布取消通知；重复调用保持幂等。
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// 等待信号被取消。
    async fn cancelled(&self) {
        if self.cancelled.load(Ordering::Acquire) {
            return;
        }
        loop {
            let notified = self.notify.notified();
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
        }
    }
}

/// 系统密钥库使用的独立服务名，不能与插件密钥共用命名空间。
const KEYRING_SERVICE: &str = "com.keencode.desktop.mcp-oauth";
/// 本机 OAuth 回调使用的固定精确路径；每次授权仍使用独立随机端口。
const CALLBACK_PATH: &str = "/oauth/callback";
/// OAuth 授权 URL 和回调请求行的最大字节数。
const MAX_CALLBACK_REQUEST_LINE_BYTES: usize = 4 * 1024;
/// OAuth 回调请求头（包含请求行）的最大字节数。
const MAX_CALLBACK_HEADER_BYTES: usize = 16 * 1024;
/// OAuth 回调查询字符串的最大字节数。
const MAX_CALLBACK_QUERY_BYTES: usize = 8 * 1024;
/// OAuth 客户端标识的最大字节数。
const MAX_CLIENT_ID_BYTES: usize = 4 * 1024;
/// MCP Server 名称与项目作用域的最大字节数。
const MAX_ID_BYTES: usize = 4 * 1024;
/// OAuth resource URI 的最大字节数。
const MAX_RESOURCE_BYTES: usize = 8 * 1024;
/// 所有 OAuth scope 拼接后的最大字节数。
const MAX_SCOPES_BYTES: usize = 4 * 1024;
/// 授权回调单个请求的最长读取时间。
const CALLBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 默认令牌交换请求的总超时时间。
const DEFAULT_TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 默认令牌交换响应正文大小上限。
const DEFAULT_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;

/// MCP OAuth 注册所需的公开配置。
///
/// `scopes` 缺失时由反序列化默认为空数组。这里绝不接受 token、secret 或
/// endpoint 字段；发现结果与令牌交换参数只保留在本次运行的内部状态中。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct McpOAuthSettings {
    /// OAuth 客户端标识。
    pub(crate) client_id: String,
    /// RFC 8707 资源标识，必须是 HTTPS URI。
    pub(crate) resource: String,
    /// 要请求的 OAuth scope 列表；缺失时为空。
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
}

impl McpOAuthSettings {
    /// 校验配置边界，避免不受控文本进入密钥键、授权 URL 或网络请求。
    pub(crate) fn validate(&self) -> Result<(), McpOAuthServiceError> {
        validate_text("client_id", &self.client_id, MAX_CLIENT_ID_BYTES, true)?;
        validate_text("resource", &self.resource, MAX_RESOURCE_BYTES, true)?;
        let resource = Url::parse(&self.resource).map_err(|_| {
            McpOAuthServiceError::InvalidConfiguration("resource 必须是有效 HTTPS URI".to_owned())
        })?;
        if resource.scheme() != "https"
            || resource.host_str().is_none()
            || !resource.username().is_empty()
            || resource.password().is_some()
            || resource.fragment().is_some()
        {
            return Err(McpOAuthServiceError::InvalidConfiguration(
                "resource 必须使用 HTTPS，且不得包含凭据或片段".to_owned(),
            ));
        }

        let mut scopes_bytes = 0_usize;
        for scope in &self.scopes {
            validate_scope(scope)?;
            scopes_bytes = scopes_bytes
                .checked_add(scope.len())
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    McpOAuthServiceError::InvalidConfiguration(
                        "OAuth scopes 超过大小上限".to_owned(),
                    )
                })?;
        }
        if scopes_bytes > MAX_SCOPES_BYTES {
            return Err(McpOAuthServiceError::InvalidConfiguration(
                "OAuth scopes 超过大小上限".to_owned(),
            ));
        }
        Ok(())
    }

    /// 使用没有 scope 的最小设置创建注册项。
    #[cfg(test)]
    pub(crate) fn new(client_id: impl Into<String>, resource: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            resource: resource.into(),
            scopes: Vec::new(),
        }
    }
}

/// MCP OAuth 宿主服务的脱敏错误。
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum McpOAuthServiceError {
    /// 注册配置或外部输入无效。
    InvalidConfiguration(String),
    /// 找不到对应作用域下的 OAuth 注册项。
    NotRegistered,
    /// 当前注册项已有授权或刷新操作。
    OperationInProgress,
    /// 当前没有待决授权或待决操作。
    NoPendingOperation,
    /// 本机系统密钥库访问失败。
    SecretStore,
    /// 事件通知边界失败。
    EventDelivery,
    /// 底层 OAuth 状态机失败。
    OAuth(OAuthError),
    /// 异步任务无法继续执行。
    Cancelled,
}

impl fmt::Debug for McpOAuthServiceError {
    /// 调试输出只显示错误类别，避免不可信文本意外进入日志。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for McpOAuthServiceError {
    /// 将错误转换为固定且不含 URL、token、code 或 state 的用户可见摘要。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => write!(formatter, "OAuth 配置无效：{message}"),
            Self::NotRegistered => formatter.write_str("OAuth Server 未注册"),
            Self::OperationInProgress => formatter.write_str("OAuth 操作正在进行中"),
            Self::NoPendingOperation => formatter.write_str("没有待决 OAuth 操作"),
            Self::SecretStore => formatter.write_str("系统密钥库操作失败"),
            Self::EventDelivery => formatter.write_str("OAuth 状态事件投递失败"),
            Self::OAuth(error) => write!(formatter, "{error}"),
            Self::Cancelled => formatter.write_str("OAuth 操作已取消"),
        }
    }
}

impl std::error::Error for McpOAuthServiceError {}

impl From<OAuthError> for McpOAuthServiceError {
    /// 把状态机错误纳入桌面服务错误边界。
    fn from(error: OAuthError) -> Self {
        Self::OAuth(error)
    }
}

/// 需要由桌面层消费的 OAuth 生命周期事件；事件对象不包含 token、code、state
/// 或 PKCE verifier。
#[derive(Clone, Eq, PartialEq)]
pub(crate) enum McpOAuthEvent {
    /// 用户需要在浏览器中完成授权。
    AuthorizationRequired {
        /// 规范化项目根，用于让宿主把事件路由到正确的项目会话。
        project_scope: String,
        /// MCP Server 稳定名称。
        server_name: String,
        /// 已通过安全 URL 校验的授权地址；该地址仍可包含 OAuth state 与 PKCE。
        authorization_url: String,
    },
    /// MCP Server 已获得可用授权。
    Authorized {
        /// 规范化项目根，用于让宿主把事件路由到正确的项目会话。
        project_scope: String,
        /// MCP Server 稳定名称。
        server_name: String,
    },
    /// OAuth 授权或刷新失败。
    Failed {
        /// 规范化项目根，用于让宿主把事件路由到正确的项目会话。
        project_scope: String,
        /// MCP Server 稳定名称。
        server_name: String,
        /// 固定脱敏失败摘要。
        message: String,
    },
}

impl fmt::Debug for McpOAuthEvent {
    /// 调试输出不回显授权地址中的 state、code challenge 或其他查询参数。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorizationRequired {
                project_scope,
                server_name,
                ..
            } => formatter
                .debug_struct("McpOAuthEvent::AuthorizationRequired")
                .field("project_scope", project_scope)
                .field("server_name", server_name)
                .field("authorization_url", &"<redacted>")
                .finish(),
            Self::Authorized {
                project_scope,
                server_name,
            } => formatter
                .debug_struct("McpOAuthEvent::Authorized")
                .field("project_scope", project_scope)
                .field("server_name", server_name)
                .finish(),
            Self::Failed {
                project_scope,
                server_name,
                message,
            } => formatter
                .debug_struct("McpOAuthEvent::Failed")
                .field("project_scope", project_scope)
                .field("server_name", server_name)
                .field("message", message)
                .finish(),
        }
    }
}

/// 可注入的 OAuth 事件投递边界。
pub(crate) trait McpOAuthEventSink: Send + Sync {
    /// 投递一条已经脱敏且通过 URL 校验的生命周期事件。
    fn emit<'a>(
        &'a self,
        event: McpOAuthEvent,
    ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>>;
}

/// 默认事件接收器；仅用于不需要桌面接线的单元测试。
#[cfg(test)]
struct NoopEventSink;

#[cfg(test)]
impl McpOAuthEventSink for NoopEventSink {
    /// 忽略事件，供单元测试和默认构造使用。
    fn emit<'a>(
        &'a self,
        _event: McpOAuthEvent,
    ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
        Box::pin(async { Ok(()) })
    }
}

/// OAuth 令牌安全存储边界；实现只保存令牌集合，不保存 pending PKCE 数据。
pub(crate) trait OAuthSecretStore: Send + Sync {
    /// 读取指定哈希键对应的令牌集合。
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> ServiceFuture<'a, Result<Option<OAuthTokenSet>, McpOAuthServiceError>>;
    /// 覆盖写入指定哈希键对应的令牌集合。
    fn set<'a>(
        &'a self,
        key: &'a str,
        token_set: &'a OAuthTokenSet,
    ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>>;
    /// 幂等删除指定哈希键对应的令牌集合。
    fn delete<'a>(&'a self, key: &'a str) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>>;
}

/// OAuth 令牌交换边界，允许测试注入不访问网络的确定性实现。
pub(crate) trait OAuthTokenExchanger: Send + Sync {
    /// 执行一次授权码或刷新令牌交换。
    fn exchange<'a>(
        &'a self,
        request: &'a OAuthTokenRequest,
        now_unix_seconds: u64,
    ) -> ServiceFuture<'a, Result<OAuthTokenSet, OAuthError>>;
}

impl OAuthTokenExchanger for ReqwestOAuthTokenExchanger {
    /// 调用 MCP 核心中有界、禁止重定向的 Reqwest 交换器。
    fn exchange<'a>(
        &'a self,
        request: &'a OAuthTokenRequest,
        now_unix_seconds: u64,
    ) -> ServiceFuture<'a, Result<OAuthTokenSet, OAuthError>> {
        Box::pin(ReqwestOAuthTokenExchanger::exchange(
            self,
            request,
            now_unix_seconds,
        ))
    }
}

/// 使用 macOS Keychain / Windows Credential Manager 的 OAuth 安全存储。
#[derive(Debug, Default)]
pub(crate) struct SystemOAuthSecretStore;

impl OAuthSecretStore for SystemOAuthSecretStore {
    /// 在阻塞线程访问系统密钥库，避免阻塞 Tokio 运行线程。
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> ServiceFuture<'a, Result<Option<OAuthTokenSet>, McpOAuthServiceError>> {
        let key = key.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || system_secret_get(&key))
                .await
                .map_err(|_| McpOAuthServiceError::SecretStore)?
        })
    }

    /// 在阻塞线程写入系统密钥库，令牌正文不会进入错误文本。
    fn set<'a>(
        &'a self,
        key: &'a str,
        token_set: &'a OAuthTokenSet,
    ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
        let key = key.to_owned();
        let token_set = token_set.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || system_secret_set(&key, &token_set))
                .await
                .map_err(|_| McpOAuthServiceError::SecretStore)?
        })
    }

    /// 在阻塞线程幂等删除系统密钥库条目。
    fn delete<'a>(&'a self, key: &'a str) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
        let key = key.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || system_secret_delete(&key))
                .await
                .map_err(|_| McpOAuthServiceError::SecretStore)?
        })
    }
}

/// 供模块测试使用的内存安全存储；生产注册表默认不使用此实现。
#[derive(Clone, Default)]
#[cfg(test)]
pub(crate) struct InMemoryOAuthSecretStore {
    /// 以哈希密钥索引的测试令牌集合。
    values: Arc<StdMutex<BTreeMap<String, OAuthTokenSet>>>,
}

#[cfg(test)]
impl InMemoryOAuthSecretStore {
    /// 创建空的内存令牌存储。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 返回测试存储当前是否存在指定哈希键。
    pub(crate) fn contains(&self, key: &str) -> bool {
        self.values
            .lock()
            .map(|values| values.contains_key(key))
            .unwrap_or(false)
    }
}

#[cfg(test)]
impl OAuthSecretStore for InMemoryOAuthSecretStore {
    /// 在短暂同步锁范围内复制令牌，绝不跨越 await。
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> ServiceFuture<'a, Result<Option<OAuthTokenSet>, McpOAuthServiceError>> {
        let value = self
            .values
            .lock()
            .map(|values| values.get(key).cloned())
            .map_err(|_| McpOAuthServiceError::SecretStore);
        Box::pin(async move { value })
    }

    /// 在短暂同步锁范围内写入测试令牌。
    fn set<'a>(
        &'a self,
        key: &'a str,
        token_set: &'a OAuthTokenSet,
    ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
        let value = self
            .values
            .lock()
            .map(|mut values| {
                values.insert(key.to_owned(), token_set.clone());
            })
            .map_err(|_| McpOAuthServiceError::SecretStore);
        Box::pin(async move { value })
    }

    /// 在短暂同步锁范围内幂等删除测试令牌。
    fn delete<'a>(&'a self, key: &'a str) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
        let value = self
            .values
            .lock()
            .map(|mut values| {
                values.remove(key);
            })
            .map_err(|_| McpOAuthServiceError::SecretStore);
        Box::pin(async move { value })
    }
}

/// `start` 返回给宿主的授权请求与当前操作代次；不会单独暴露 state。
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct McpOAuthStartResult {
    /// 已包含动态回调地址、state 和 PKCE challenge 的授权请求。
    pub(crate) authorization_request: OAuthAuthorizationRequest,
    /// 本次授权操作的 CAS 代次。
    pub(crate) generation: u64,
    /// 本次实际监听的回调地址。
    pub(crate) redirect_uri: String,
}

impl fmt::Debug for McpOAuthStartResult {
    /// 调试输出不回显授权 URL 或 state。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthStartResult")
            .field("authorization_request", &"<redacted>")
            .field("generation", &self.generation)
            .field("redirect_uri", &"<redacted>")
            .finish()
    }
}

impl McpOAuthStartResult {
    /// 返回授权地址，供宿主打开浏览器；调用方不得记录该地址。
    pub(crate) fn authorization_url(&self) -> &str {
        &self.authorization_request.authorization_url
    }

    /// 返回本次授权 CAS 代次。
    #[cfg(test)]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// 返回本次本机回调地址。
    #[cfg(test)]
    pub(crate) fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }
}

/// `callback` 接管成功后的脱敏确认。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct McpOAuthCallbackResult {
    /// 本次已经接管回调的操作代次。
    pub(crate) generation: u64,
}

/// 不向 ACP 或界面返回 token 的当前状态快照。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct McpOAuthStatusSnapshot {
    /// OAuth 状态机阶段。
    pub(crate) status: OAuthStatus,
    /// 当前操作 CAS 代次。
    pub(crate) generation: u64,
    /// 当前访问令牌代次；无令牌时为零。
    pub(crate) token_generation: u64,
    /// 是否仍有进程内待决操作。
    pub(crate) operation_pending: bool,
}

/// 内存中的项目与 Server 身份绑定；不会作为密钥库键直接使用。
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct EntryIdentity {
    /// 规范化项目作用域文本。
    project_scope: String,
    /// 当前作用域内的 MCP Server 名称。
    server_id: String,
    /// OAuth 资源标识。
    resource: String,
    /// OAuth 客户端标识。
    client_id: String,
    /// 完整 OAuth scope 配置；scope 变化必须撤销旧的 pending/Provider。
    scopes: Vec<String>,
}

/// 一个项目内 Server 的唯一当前绑定键；完整身份变化也只能保留一个条目。
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct EntryBinding {
    /// 规范化项目作用域文本。
    project_scope: String,
    /// 当前作用域内的 MCP Server 名称。
    server_id: String,
}

impl From<&EntryIdentity> for EntryBinding {
    /// 从完整身份提取用于当前绑定查找的稳定键。
    fn from(identity: &EntryIdentity) -> Self {
        Self {
            project_scope: identity.project_scope.clone(),
            server_id: identity.server_id.clone(),
        }
    }
}

/// 单个注册项的短暂状态；所有 await 前都复制所需数据并释放该锁。
struct EntryState {
    /// 注册配置（不含 token）。
    settings: McpOAuthSettings,
    /// 当前已发现并校验的 OAuth 端点配置。
    oauth_config: Option<OAuthConfig>,
    /// 当前授权服务器签发方对应的令牌是否已经从密钥库加载。
    ///
    /// 该标记必须与 `oauth_config.authorization_server_issuer` 一起切换，避免
    /// issuer 变化后把旧签发方的“无令牌”结果误当成新签发方已加载。
    loaded: bool,
    /// 该条目是否已经被新的 resource/client 绑定撤销。
    revoked: bool,
    /// 使用上述配置构造的本地 OAuth 状态机。
    machine: Option<OAuthMachine>,
    /// 便于无配置时加载和 CAS 的令牌副本；只存在于进程内。
    token_set: Option<OAuthTokenSet>,
    /// 当前访问令牌的代次。
    token_generation: u64,
    /// 当前操作 CAS 代次。
    generation: u64,
    /// 当前进程中的授权或刷新操作。
    active: Option<ActiveOperation>,
    /// 最近一次终态结果，供 refresh single-flight 等待者读取。
    last_result: Option<CompletedOperation>,
}

/// 一个异步授权或刷新操作的内部记录。
struct ActiveOperation {
    /// 此操作占用的 CAS 代次。
    generation: u64,
    /// 操作类型。
    kind: OperationKind,
    /// 授权开始结果；刷新操作没有此值。
    start_result: Option<McpOAuthStartResult>,
    /// 终止本机回调监听任务。
    listener_cancel: CancellationSignal,
    /// 终止当前令牌交换任务。
    exchange_cancel: CancellationSignal,
    /// 操作开始前的令牌，用于失败后安全恢复。
    previous_token_set: Option<OAuthTokenSet>,
}

/// OAuth 操作类型。
#[derive(Clone, Copy, Eq, PartialEq)]
enum OperationKind {
    /// 授权码交换。
    AuthorizationCode,
    /// 刷新令牌交换。
    Refresh,
}

/// 已完成操作的安全结果。
struct CompletedOperation {
    /// 终态操作代次。
    generation: u64,
    /// 仅保存成功或脱敏错误类别。
    result: Result<(), McpOAuthServiceError>,
}

/// 单个注册项及其异步 gate。
struct OAuthEntry {
    /// 内存身份。
    identity: EntryIdentity,
    /// 无锁快速撤销闸门，阻止迟到交换在停用后继续提交。
    revoked: AtomicBool,
    /// 不跨 await 持有的状态锁。
    state: Mutex<EntryState>,
    /// 确保令牌首次读取只执行一次。
    load_gate: Mutex<()>,
    /// 序列化安全存储写入与取消的 async gate。
    persistence_gate: Mutex<()>,
    /// 序列化绑定本机监听器和创建授权操作。
    start_gate: Mutex<()>,
    /// 通知 refresh single-flight 等待者。
    notify: Notify,
}

/// OAuth 注册表共享实现。
struct OAuthRegistryInner {
    /// 以项目和 Server 的当前绑定索引进程内注册项。
    entries: StdMutex<BTreeMap<EntryBinding, Arc<OAuthEntry>>>,
    /// 生产令牌安全存储。
    secret_store: Arc<dyn OAuthSecretStore>,
    /// 生产或测试令牌交换器。
    exchanger: Arc<dyn OAuthTokenExchanger>,
    /// 可注入的事件投递边界。
    event_sink: Arc<dyn McpOAuthEventSink>,
}

/// 当前桌面进程唯一的 MCP OAuth 注册表。
#[derive(Clone)]
pub(crate) struct McpOAuthRegistry {
    /// 可克隆的共享注册表实现。
    inner: Arc<OAuthRegistryInner>,
}

#[cfg(test)]
impl Default for McpOAuthRegistry {
    /// 使用系统密钥库、Reqwest 交换器和广播事件创建生产注册表。
    fn default() -> Self {
        Self::new()
    }
}

impl McpOAuthRegistry {
    /// 创建生产 OAuth 注册表；构造阶段不访问密钥库、不访问网络。
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::new_with_event_sink(Arc::new(NoopEventSink))
    }

    /// 创建使用系统密钥库、Reqwest 交换器和指定事件边界的生产注册表。
    ///
    /// 事件 sink 是宿主接收 OAuth 终态的权威边界；默认构造仅为不需要接线的
    /// 测试和早期初始化阶段使用 `NoopEventSink`。
    pub(crate) fn new_with_event_sink(event_sink: Arc<dyn McpOAuthEventSink>) -> Self {
        let exchanger = ReqwestOAuthTokenExchanger::new(
            DEFAULT_TOKEN_REQUEST_TIMEOUT,
            DEFAULT_TOKEN_RESPONSE_BYTES,
        )
        .expect("固定 OAuth 交换器配置必须有效");
        Self::with_dependencies(
            Arc::new(SystemOAuthSecretStore),
            Arc::new(exchanger),
            event_sink,
        )
    }

    /// 使用注入的安全存储、令牌交换器和事件投递器创建注册表。
    pub(crate) fn with_dependencies(
        secret_store: Arc<dyn OAuthSecretStore>,
        exchanger: Arc<dyn OAuthTokenExchanger>,
        event_sink: Arc<dyn McpOAuthEventSink>,
    ) -> Self {
        Self {
            inner: Arc::new(OAuthRegistryInner {
                entries: StdMutex::new(BTreeMap::new()),
                secret_store,
                exchanger,
                event_sink,
            }),
        }
    }

    /// 注册一个项目作用域下的 MCP Server OAuth 设置。
    pub(crate) async fn register<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: impl Into<String>,
        settings: McpOAuthSettings,
    ) -> Result<(), McpOAuthServiceError> {
        let server_id = server_id.into();
        let project_scope = scope_text(project_scope.as_ref())?;
        settings.validate()?;
        let identity = EntryIdentity {
            project_scope,
            server_id,
            resource: settings.resource.clone(),
            client_id: settings.client_id.clone(),
            scopes: settings.scopes.clone(),
        };
        validate_identity(&identity)?;
        let (entry, replaced) = self.entry_for_identity(identity, settings.clone())?;
        if let Some(replaced) = replaced {
            self.revoke_entry(replaced).await?;
        }
        let mut state = entry.state.lock().await;
        // 相同身份的重复注册只更新公开设置；不得打断已经在运行的授权/刷新操作。
        state.settings = settings;
        Ok(())
    }

    /// 为已注册项保存一次发现得到的 OAuth 端点配置。
    pub(crate) async fn configure<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: &str,
        oauth_config: OAuthConfig,
    ) -> Result<(), McpOAuthServiceError> {
        let project_scope = scope_text(project_scope.as_ref())?;
        let identity = self.find_identity(&project_scope, server_id)?;
        oauth_config.validate()?;
        if oauth_config.resource != identity.resource
            || oauth_config.client_id != identity.client_id
        {
            return Err(McpOAuthServiceError::InvalidConfiguration(
                "OAuth 配置与注册项身份不一致".to_owned(),
            ));
        }
        let entry = self.entry_for_existing(&identity)?;
        self.configure_entry(&entry, oauth_config).await
    }

    /// 为注册项创建动态认证提供方；提供方只在请求时读取当前令牌。
    pub(crate) fn auth_provider<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: &str,
    ) -> Result<Arc<dyn McpAuthProvider>, McpOAuthServiceError> {
        let project_scope = scope_text(project_scope.as_ref())?;
        let identity = self.find_identity(&project_scope, server_id)?;
        let entry = self.entry_for_existing(&identity)?;
        Ok(Arc::new(RegistryAuthProvider {
            registry: self.clone(),
            entry,
        }))
    }

    /// 对已经配置端点的注册项开始一次浏览器授权。
    pub(crate) async fn start<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: &str,
        now_unix_seconds: u64,
    ) -> Result<McpOAuthStartResult, McpOAuthServiceError> {
        let project_scope = scope_text(project_scope.as_ref())?;
        let identity = self.find_identity(&project_scope, server_id)?;
        let entry = self.entry_for_existing(&identity)?;
        let oauth_config = {
            let state = entry.state.lock().await;
            state.oauth_config.clone().ok_or_else(|| {
                McpOAuthServiceError::InvalidConfiguration(
                    "OAuth Server 尚未完成端点发现".to_owned(),
                )
            })?
        };
        self.start_with_entry(entry, oauth_config, now_unix_seconds)
            .await
    }

    /// 提交 ACP 或本机回调携带的授权 code/state，并异步开始令牌交换。
    pub(crate) async fn callback<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: &str,
        code: String,
        state_value: String,
        now_unix_seconds: u64,
    ) -> Result<McpOAuthCallbackResult, McpOAuthServiceError> {
        let project_scope = scope_text(project_scope.as_ref())?;
        let identity = self.find_identity(&project_scope, server_id)?;
        let entry = self.entry_for_existing(&identity)?;
        self.callback_entry(
            entry,
            OAuthCallback {
                state: state_value,
                code: Some(code),
                error: None,
                error_description: None,
            },
            now_unix_seconds,
        )
        .await
    }

    /// 取消当前作用域和 Server 的首次待决 OAuth 操作。
    pub(crate) async fn cancel<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: &str,
    ) -> Result<bool, McpOAuthServiceError> {
        let project_scope = scope_text(project_scope.as_ref())?;
        let identity = self.find_identity(&project_scope, server_id)?;
        let entry = self.entry_for_existing(&identity)?;
        self.cancel_entry(entry).await
    }

    /// 停用当前项目/Server 绑定并撤销已有 Provider；停用不删除密钥库令牌。
    ///
    /// 下一次重新注册并完成相同 issuer 的发现后，新的条目会按同一密钥键恢复令牌。
    pub(crate) async fn deactivate<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: &str,
    ) -> Result<bool, McpOAuthServiceError> {
        let project_scope = scope_text(project_scope.as_ref())?;
        validate_identifier("server_id", server_id)?;
        let binding = EntryBinding {
            project_scope,
            server_id: server_id.to_owned(),
        };
        let entry = self
            .inner
            .entries
            .lock()
            .map_err(|_| McpOAuthServiceError::SecretStore)?
            .remove(&binding);
        let Some(entry) = entry else {
            return Ok(false);
        };
        self.revoke_entry(entry).await?;
        Ok(true)
    }

    /// 返回不含令牌正文的当前 OAuth 状态。
    pub(crate) async fn status<P: AsRef<Path>>(
        &self,
        project_scope: P,
        server_id: &str,
    ) -> Result<McpOAuthStatusSnapshot, McpOAuthServiceError> {
        let project_scope = scope_text(project_scope.as_ref())?;
        let identity = self.find_identity(&project_scope, server_id)?;
        let entry = self.entry_for_existing(&identity)?;
        self.ensure_loaded(&entry).await?;
        let state = entry.state.lock().await;
        let status = state
            .machine
            .as_ref()
            .map(|machine| machine.snapshot().status())
            .unwrap_or_else(|| {
                if state.token_set.is_some() {
                    OAuthStatus::Authorized
                } else {
                    OAuthStatus::Idle
                }
            });
        // 状态查询只读取内存，不启动刷新；但不能把已经过期的持久令牌
        // 报告为仍可调用的 Authorized。Refreshing/Awaiting 等进行中的状态
        // 必须保持原样，交由对应操作完成后再发布终态。
        let status = if status == OAuthStatus::Authorized
            && state.token_set.as_ref().is_some_and(|token_set| {
                token_set
                    .expires_at
                    .is_some_and(|expires_at| now_unix_seconds() >= expires_at)
            }) {
            OAuthStatus::Expired
        } else {
            status
        };
        Ok(McpOAuthStatusSnapshot {
            status,
            generation: state.generation,
            token_generation: state.token_generation,
            operation_pending: state.active.is_some(),
        })
    }

    /// 根据完整身份生成只含 SHA-256 的系统密钥库键。
    #[cfg(test)]
    pub(crate) fn storage_key<P: AsRef<Path>>(
        project_scope: P,
        server_id: &str,
        settings: &McpOAuthSettings,
        authorization_server_issuer: &str,
    ) -> Result<String, McpOAuthServiceError> {
        settings.validate()?;
        let project_scope = scope_text(project_scope.as_ref())?;
        storage_key_for_parts(
            &project_scope,
            server_id,
            authorization_server_issuer,
            &settings.resource,
            &settings.client_id,
        )
    }

    /// 创建或获取一个已经注册的内部条目。
    fn entry_for_identity(
        &self,
        identity: EntryIdentity,
        settings: McpOAuthSettings,
    ) -> Result<(Arc<OAuthEntry>, Option<Arc<OAuthEntry>>), McpOAuthServiceError> {
        let binding = EntryBinding::from(&identity);
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| McpOAuthServiceError::SecretStore)?;
        if let Some(entry) = entries.get(&binding)
            && entry.identity == identity
        {
            return Ok((entry.clone(), None));
        }
        let replaced = entries.remove(&binding);
        let entry = Arc::new(OAuthEntry {
            identity: identity.clone(),
            revoked: AtomicBool::new(false),
            state: Mutex::new(EntryState {
                settings,
                oauth_config: None,
                loaded: false,
                revoked: false,
                machine: None,
                token_set: None,
                token_generation: 0,
                generation: 0,
                active: None,
                last_result: None,
            }),
            load_gate: Mutex::new(()),
            persistence_gate: Mutex::new(()),
            start_gate: Mutex::new(()),
            notify: Notify::new(),
        });
        entries.insert(binding, entry.clone());
        Ok((entry, replaced))
    }

    /// 按已注册身份查找条目。
    fn entry_for_existing(
        &self,
        identity: &EntryIdentity,
    ) -> Result<Arc<OAuthEntry>, McpOAuthServiceError> {
        let binding = EntryBinding::from(identity);
        self.inner
            .entries
            .lock()
            .map_err(|_| McpOAuthServiceError::SecretStore)?
            .get(&binding)
            .cloned()
            .filter(|entry| entry.identity == *identity)
            .ok_or(McpOAuthServiceError::NotRegistered)
    }

    /// 撤销被新 OAuth 配置绑定替换的旧条目及其迟到异步操作。
    async fn revoke_entry(&self, entry: Arc<OAuthEntry>) -> Result<(), McpOAuthServiceError> {
        entry.revoked.store(true, Ordering::Release);
        let _persist = entry.persistence_gate.lock().await;
        let (listener_cancel, exchange_cancel) = {
            let mut state = entry.state.lock().await;
            state.revoked = true;
            let Some(active) = state.active.take() else {
                return Ok(());
            };
            state.generation = next_generation(state.generation)?;
            state.last_result = Some(CompletedOperation {
                generation: active.generation,
                result: Err(McpOAuthServiceError::Cancelled),
            });
            restore_failed_machine(&mut state, active.previous_token_set)?;
            (active.listener_cancel, active.exchange_cancel)
        };
        listener_cancel.cancel();
        exchange_cancel.cancel();
        entry.notify.notify_waiters();
        Ok(())
    }

    /// 用资源和客户端标识定位同一项目下的 Server 注册项。
    fn find_identity(
        &self,
        project_scope: &str,
        server_id: &str,
    ) -> Result<EntryIdentity, McpOAuthServiceError> {
        validate_identifier("server_id", server_id)?;
        let binding = EntryBinding {
            project_scope: project_scope.to_owned(),
            server_id: server_id.to_owned(),
        };
        let entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| McpOAuthServiceError::SecretStore)?;
        entries
            .get(&binding)
            .map(|entry| entry.identity.clone())
            .ok_or(McpOAuthServiceError::NotRegistered)
    }

    /// 确保单个条目的令牌从安全存储只读取一次。
    async fn ensure_loaded(&self, entry: &Arc<OAuthEntry>) -> Result<(), McpOAuthServiceError> {
        let _load = entry.load_gate.lock().await;
        loop {
            let (storage_key, config) = {
                let mut state = entry.state.lock().await;
                if entry.revoked.load(Ordering::Acquire) || state.revoked {
                    return Err(McpOAuthServiceError::NotRegistered);
                }
                if state.loaded {
                    return Ok(());
                }
                let Some(config) = state.oauth_config.clone() else {
                    // 未完成发现时没有可安全推导的密钥库键；保持空令牌状态，
                    // configure_entry 会在收到可信 issuer 后重新打开加载流程。
                    state.loaded = true;
                    return Ok(());
                };
                let storage_key = storage_key_for_parts(
                    &entry.identity.project_scope,
                    &entry.identity.server_id,
                    &config.authorization_server_issuer,
                    &config.resource,
                    &config.client_id,
                )?;
                (storage_key, config)
            };

            let token_set = self.inner.secret_store.get(&storage_key).await?;
            let mut state = entry.state.lock().await;
            if entry.revoked.load(Ordering::Acquire) || state.revoked {
                return Err(McpOAuthServiceError::NotRegistered);
            }
            let current_key = state
                .oauth_config
                .as_ref()
                .map(|current| {
                    storage_key_for_parts(
                        &entry.identity.project_scope,
                        &entry.identity.server_id,
                        &current.authorization_server_issuer,
                        &current.resource,
                        &current.client_id,
                    )
                })
                .transpose()?;
            if current_key.as_deref() != Some(storage_key.as_str()) {
                // configure_entry 在本次读取期间切换了 issuer；丢弃旧键读取结果，
                // 下一轮只提交当前 issuer 对应的令牌。
                continue;
            }
            if state.loaded {
                return Ok(());
            }
            state.token_set = token_set;
            state.token_generation = u64::from(state.token_set.is_some());
            // 冷启动恢复的令牌也占用一个已提交代次，确保第一次刷新分配更大的
            // generation，等待者不会把刷新成功误判成旧令牌。
            state.generation = state.generation.max(state.token_generation);
            state.machine = Some(machine_for_token(&config, state.token_set.clone())?);
            state.loaded = true;
            return Ok(());
        }
    }

    /// 保存端点配置并按当前令牌恢复状态机。
    async fn configure_entry(
        &self,
        entry: &Arc<OAuthEntry>,
        oauth_config: OAuthConfig,
    ) -> Result<(), McpOAuthServiceError> {
        oauth_config.validate()?;
        if oauth_config.resource != entry.identity.resource
            || oauth_config.client_id != entry.identity.client_id
        {
            return Err(McpOAuthServiceError::InvalidConfiguration(
                "OAuth 配置与注册项身份不一致".to_owned(),
            ));
        }

        // 先校验并计算 issuer 绑定的密钥键；不能在 issuer 未纳入键的情况下
        // 读取或覆盖任何令牌。
        storage_key_for_parts(
            &entry.identity.project_scope,
            &entry.identity.server_id,
            &oauth_config.authorization_server_issuer,
            &oauth_config.resource,
            &oauth_config.client_id,
        )?;

        let should_load = {
            let mut state = entry.state.lock().await;
            if entry.revoked.load(Ordering::Acquire) || state.revoked {
                return Err(McpOAuthServiceError::NotRegistered);
            }
            let same_binding = state.oauth_config.as_ref().is_some_and(|current| {
                current.authorization_server_issuer == oauth_config.authorization_server_issuer
                    && current.resource == oauth_config.resource
                    && current.client_id == oauth_config.client_id
            });
            if state.active.is_some() {
                if same_binding {
                    // 发现重试可能与一次进行中的授权并发；相同可信 issuer/
                    // resource/client 绑定不能重置 pending 或 authorized machine。
                    return Ok(());
                }
                return Err(McpOAuthServiceError::OperationInProgress);
            }

            if !same_binding {
                // issuer 变化即是新的安全主体：旧 issuer 的内存令牌不可继续使用，
                // 也不得把它当成新 issuer 的加载结果。
                state.token_set = None;
                state.token_generation = 0;
                state.machine = None;
                state.loaded = false;
            }
            state.oauth_config = Some(oauth_config.clone());
            if state.loaded {
                state.machine = Some(machine_for_token(&oauth_config, state.token_set.clone())?);
            }
            !state.loaded
        };
        if should_load {
            self.ensure_loaded(entry).await?;
        }
        Ok(())
    }

    /// 串行创建本次授权监听器和状态机 pending。
    async fn start_with_entry(
        &self,
        entry: Arc<OAuthEntry>,
        oauth_config: OAuthConfig,
        now_unix_seconds: u64,
    ) -> Result<McpOAuthStartResult, McpOAuthServiceError> {
        let _start = entry.start_gate.lock().await;
        self.ensure_loaded(&entry).await?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|_| {
            McpOAuthServiceError::InvalidConfiguration("无法绑定 OAuth 回调端口".to_owned())
        })?;
        let address = listener.local_addr().map_err(|_| {
            McpOAuthServiceError::InvalidConfiguration("无法读取 OAuth 回调端口".to_owned())
        })?;
        let redirect_uri = format!(
            "http://{}:{}{}",
            address.ip(),
            address.port(),
            CALLBACK_PATH
        );
        let mut config = oauth_config;
        config.redirect_uri = redirect_uri.clone();
        config.validate()?;
        validate_authorization_redirect(&redirect_uri)?;

        let cancel_listener = CancellationSignal::new();
        let cancel_exchange = CancellationSignal::new();
        let (start_result, generation, expires_at) = {
            let mut state = entry.state.lock().await;
            if let Some(active) = &state.active {
                if let Some(result) = &active.start_result {
                    return Ok(result.clone());
                }
                return Err(McpOAuthServiceError::OperationInProgress);
            }
            state.oauth_config = Some(config.clone());
            state.machine = Some(machine_for_token(&config, state.token_set.clone())?);
            let machine = state.machine.as_mut().ok_or_else(|| {
                McpOAuthServiceError::InvalidConfiguration("OAuth 状态机不可用".to_owned())
            })?;
            let authorization = machine.begin_authorization(now_unix_seconds)?;
            let generation = next_generation(state.generation)?;
            state.generation = generation;
            state.last_result = None;
            let result = McpOAuthStartResult {
                redirect_uri: redirect_uri.clone(),
                authorization_request: authorization,
                generation,
            };
            let expires_at = result.authorization_request.expires_at;
            state.active = Some(ActiveOperation {
                generation,
                kind: OperationKind::AuthorizationCode,
                start_result: Some(result.clone()),
                listener_cancel: cancel_listener.clone(),
                exchange_cancel: cancel_exchange.clone(),
                previous_token_set: state.token_set.clone(),
            });
            (result, generation, expires_at)
        };

        let event = McpOAuthEvent::AuthorizationRequired {
            project_scope: entry.identity.project_scope.clone(),
            server_name: entry.identity.server_id.clone(),
            authorization_url: start_result.authorization_url().to_owned(),
        };
        if let Err(error) = self.emit_event(event).await {
            let _ = self.cancel_generation(&entry, generation).await;
            return Err(error);
        }
        // 绑定与状态提交已经完成，释放创建 gate 后再启动独立监听任务。
        drop(_start);
        let registry = self.clone();
        tokio::spawn(async move {
            run_callback_listener(
                registry,
                entry,
                listener,
                generation,
                expires_at,
                cancel_listener,
            )
            .await;
        });
        Ok(start_result)
    }

    /// 处理一个来自 ACP 或回环 HTTP 的授权回调。
    async fn callback_entry(
        &self,
        entry: Arc<OAuthEntry>,
        callback: OAuthCallback,
        now_unix_seconds: u64,
    ) -> Result<McpOAuthCallbackResult, McpOAuthServiceError> {
        self.ensure_loaded(&entry).await?;
        let (request, generation, cancel_listener, cancel_exchange) = {
            let mut state = entry.state.lock().await;
            let mut active = state
                .active
                .take()
                .ok_or(McpOAuthServiceError::NoPendingOperation)?;
            if active.kind != OperationKind::AuthorizationCode {
                state.active = Some(active);
                return Err(McpOAuthServiceError::OperationInProgress);
            }
            let machine = state.machine.as_mut().ok_or_else(|| {
                McpOAuthServiceError::InvalidConfiguration("OAuth 状态机不可用".to_owned())
            })?;
            match machine.handle_callback(callback, now_unix_seconds) {
                Ok(request) => {
                    active.start_result = None;
                    let values = (
                        request,
                        active.generation,
                        active.listener_cancel.clone(),
                        active.exchange_cancel.clone(),
                    );
                    state.active = Some(active);
                    values
                }
                Err(error @ OAuthError::InvalidState) => {
                    state.active = Some(active);
                    return Err(error.into());
                }
                Err(error) => {
                    let generation = active.generation;
                    let previous_token_set = active.previous_token_set.clone();
                    let listener_cancel = active.listener_cancel.clone();
                    let exchange_cancel = active.exchange_cancel.clone();
                    state.last_result = Some(CompletedOperation {
                        generation,
                        result: Err(error.clone().into()),
                    });
                    restore_failed_machine(&mut state, previous_token_set)?;
                    listener_cancel.cancel();
                    exchange_cancel.cancel();
                    drop(state);
                    self.emit_failed(&entry, &error).await?;
                    entry.notify.notify_waiters();
                    return Err(error.into());
                }
            }
        };
        cancel_listener.cancel();
        let registry = self.clone();
        let task_entry = entry.clone();
        tokio::spawn(async move {
            run_exchange(
                registry,
                task_entry,
                generation,
                request,
                OperationKind::AuthorizationCode,
                cancel_exchange,
            )
            .await;
        });
        Ok(McpOAuthCallbackResult { generation })
    }

    /// 取消一个指定代次的操作，避免迟到交换结果获得授权。
    async fn cancel_generation(
        &self,
        entry: &Arc<OAuthEntry>,
        generation: u64,
    ) -> Result<bool, McpOAuthServiceError> {
        let _persist = entry.persistence_gate.lock().await;
        let (listener_cancel, exchange_cancel, previous_token_set) = {
            let mut state = entry.state.lock().await;
            let Some(active) = state.active.as_ref() else {
                return Ok(false);
            };
            if active.generation != generation {
                return Ok(false);
            }
            let active = state
                .active
                .take()
                .ok_or(McpOAuthServiceError::NoPendingOperation)?;
            state.generation = next_generation(state.generation)?;
            state.last_result = Some(CompletedOperation {
                generation,
                result: Err(McpOAuthServiceError::Cancelled),
            });
            restore_failed_machine(&mut state, active.previous_token_set.clone())?;
            (
                active.listener_cancel,
                active.exchange_cancel,
                active.previous_token_set,
            )
        };
        let _ = previous_token_set;
        listener_cancel.cancel();
        exchange_cancel.cancel();
        entry.notify.notify_waiters();
        Ok(true)
    }

    /// 取消当前操作；返回值严格表示第一次 CAS 是否成功。
    async fn cancel_entry(&self, entry: Arc<OAuthEntry>) -> Result<bool, McpOAuthServiceError> {
        let generation = entry
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| active.generation);
        let Some(generation) = generation else {
            return Ok(false);
        };
        let cancelled = self.cancel_generation(&entry, generation).await?;
        if cancelled {
            let _ = self
                .emit_event(McpOAuthEvent::Failed {
                    project_scope: entry.identity.project_scope.clone(),
                    server_name: entry.identity.server_id.clone(),
                    message: "OAuth 授权已取消".to_owned(),
                })
                .await;
        }
        Ok(cancelled)
    }

    /// 将到期操作以 CAS 方式终止。
    async fn expire_entry(&self, entry: Arc<OAuthEntry>) -> Result<bool, McpOAuthServiceError> {
        let generation = entry
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| active.generation);
        let Some(generation) = generation else {
            return Ok(false);
        };
        let expired = self.cancel_generation(&entry, generation).await?;
        if expired {
            let _ = self
                .emit_event(McpOAuthEvent::Failed {
                    project_scope: entry.identity.project_scope.clone(),
                    server_name: entry.identity.server_id.clone(),
                    message: "OAuth 授权请求已过期".to_owned(),
                })
                .await;
        }
        Ok(expired)
    }

    /// 启动一次令牌刷新或等待已经存在的刷新 single-flight。
    async fn refresh_if_needed(
        &self,
        entry: &Arc<OAuthEntry>,
        sent_generation: u64,
    ) -> Result<(), McpOAuthServiceError> {
        loop {
            // 必须先注册并 enable waiter，再检查状态和启动刷新任务，否则任务
            // 可能在 `notified()` 创建前完成并永久丢失唤醒。
            let notified = entry.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = entry.state.lock().await;
                if entry.revoked.load(Ordering::Acquire) || state.revoked {
                    return Err(McpOAuthServiceError::NotRegistered);
                }
                if state.token_generation > sent_generation {
                    return Ok(());
                }
                if let Some(completed) = &state.last_result
                    && state.active.is_none()
                    && completed.generation >= sent_generation
                {
                    if let Err(error) = &completed.result {
                        return Err(error.clone());
                    }
                    if state.token_generation > sent_generation {
                        return Ok(());
                    }
                }
                if let Some(active) = &state.active {
                    if active.kind != OperationKind::Refresh {
                        return Err(McpOAuthServiceError::OperationInProgress);
                    }
                } else {
                    let machine = state.machine.as_mut().ok_or_else(|| {
                        McpOAuthServiceError::InvalidConfiguration(
                            "OAuth Server 尚未完成端点发现".to_owned(),
                        )
                    })?;
                    let request = machine.begin_refresh()?;
                    let generation = next_generation(state.generation)?;
                    state.generation = generation;
                    let listener_cancel = CancellationSignal::new();
                    let exchange_cancel = CancellationSignal::new();
                    let previous_token_set = state.token_set.clone();
                    state.active = Some(ActiveOperation {
                        generation,
                        kind: OperationKind::Refresh,
                        start_result: None,
                        listener_cancel,
                        exchange_cancel: exchange_cancel.clone(),
                        previous_token_set,
                    });
                    let registry = self.clone();
                    let task_entry = entry.clone();
                    tokio::spawn(async move {
                        run_exchange(
                            registry,
                            task_entry,
                            generation,
                            request,
                            OperationKind::Refresh,
                            exchange_cancel,
                        )
                        .await;
                    });
                }
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(DEFAULT_TOKEN_REQUEST_TIMEOUT) => {
                    return Err(McpOAuthServiceError::OAuth(
                        OAuthError::InvalidTransition("OAuth 刷新等待超时".to_owned()),
                    ));
                }
            }
        }
    }

    /// 对一次成功或失败的交换执行状态与密钥库 CAS 提交。
    async fn finish_exchange(
        &self,
        entry: Arc<OAuthEntry>,
        generation: u64,
        kind: OperationKind,
        result: Result<OAuthTokenSet, OAuthError>,
    ) {
        let (event, event_error) = {
            // 持久化 gate 只覆盖状态快照和提交；事件通知必须在离开此块后执行，
            // 以允许宿主 sink 重入 registry。
            let _persist = entry.persistence_gate.lock().await;
            let storage_key = {
                let state = entry.state.lock().await;
                let Some(config) = state.oauth_config.as_ref() else {
                    return;
                };
                match storage_key_for_parts(
                    &entry.identity.project_scope,
                    &entry.identity.server_id,
                    &config.authorization_server_issuer,
                    &config.resource,
                    &config.client_id,
                ) {
                    Ok(key) => key,
                    Err(_) => return,
                }
            };
            let mut candidate = {
                let state = entry.state.lock().await;
                if entry.revoked.load(Ordering::Acquire) || state.revoked {
                    return;
                }
                let Some(active) = state.active.as_ref() else {
                    return;
                };
                if active.generation != generation || active.kind != kind {
                    return;
                }
                match &result {
                    Ok(token_set) => {
                        let mut machine = match state.machine.clone() {
                            Some(machine) => machine,
                            None => return,
                        };
                        if machine.accept_token(token_set.clone()).is_err() {
                            return;
                        }
                        Some(machine)
                    }
                    Err(_) => None,
                }
            };

            // active 保留在状态中，其他路径会继续看到操作进行中；只在这里释放
            // state 锁后执行可能阻塞的密钥库调用。
            let persistence = match &result {
                Ok(token_set) => self.inner.secret_store.set(&storage_key, token_set).await,
                Err(error) if is_invalid_grant(error) => {
                    self.inner.secret_store.delete(&storage_key).await
                }
                Err(_) => Ok(()),
            };

            let mut state = entry.state.lock().await;
            if entry.revoked.load(Ordering::Acquire) || state.revoked {
                return;
            }
            let Some(active) = state.active.as_ref() else {
                return;
            };
            if active.generation != generation || active.kind != kind {
                return;
            }
            match result {
                Ok(token_set) => {
                    if let Err(error) = persistence {
                        let previous_token_set = active.previous_token_set.clone();
                        let _ = state.active.take();
                        state.last_result = Some(CompletedOperation {
                            generation,
                            result: Err(error.clone()),
                        });
                        let _ = restore_failed_machine(&mut state, previous_token_set);
                        (None, Some(error))
                    } else {
                        let _ = state.active.take();
                        let Some(candidate) = candidate.take() else {
                            return;
                        };
                        state.machine = Some(candidate);
                        state.token_set = Some(token_set);
                        state.token_generation = generation;
                        state.last_result = Some(CompletedOperation {
                            generation,
                            result: Ok(()),
                        });
                        (
                            Some(McpOAuthEvent::Authorized {
                                project_scope: entry.identity.project_scope.clone(),
                                server_name: entry.identity.server_id.clone(),
                            }),
                            None,
                        )
                    }
                }
                Err(error) => {
                    let service_error: McpOAuthServiceError = error.clone().into();
                    let invalid_grant = is_invalid_grant(&error);
                    let storage_error = persistence.err();
                    let previous_token_set = active.previous_token_set.clone();
                    let _ = state.active.take();
                    if invalid_grant {
                        state.token_set = None;
                        state.token_generation = 0;
                        state.machine = state
                            .oauth_config
                            .as_ref()
                            .and_then(|config| OAuthMachine::new(config.clone()).ok());
                    } else if kind == OperationKind::Refresh {
                        if let Some(machine) = state.machine.as_mut() {
                            machine.reject_refresh(now_unix_seconds());
                        }
                        state.token_set = previous_token_set;
                    } else {
                        let _ = restore_failed_machine(&mut state, previous_token_set);
                    }
                    let final_error = storage_error.unwrap_or(service_error);
                    state.last_result = Some(CompletedOperation {
                        generation,
                        result: Err(final_error.clone()),
                    });
                    (
                        Some(McpOAuthEvent::Failed {
                            project_scope: entry.identity.project_scope.clone(),
                            server_name: entry.identity.server_id.clone(),
                            message: safe_failure_message(&error),
                        }),
                        Some(final_error),
                    )
                }
            }
        };
        entry.notify.notify_waiters();
        if let Some(event) = event {
            let _ = self.emit_event(event).await;
        }
        let _ = event_error;
    }

    /// 发送一条事件到注入边界。
    async fn emit_event(&self, event: McpOAuthEvent) -> Result<(), McpOAuthServiceError> {
        self.inner.event_sink.emit(event).await
    }

    /// 对回调状态机失败发布固定失败摘要。
    async fn emit_failed(
        &self,
        entry: &Arc<OAuthEntry>,
        error: &OAuthError,
    ) -> Result<(), McpOAuthServiceError> {
        self.emit_event(McpOAuthEvent::Failed {
            project_scope: entry.identity.project_scope.clone(),
            server_name: entry.identity.server_id.clone(),
            message: safe_failure_message(error),
        })
        .await
    }
}

/// 给 MCP HTTP Transport 使用的动态认证适配器。
struct RegistryAuthProvider {
    /// 共享 OAuth 注册表。
    registry: McpOAuthRegistry,
    /// 绑定的完整注册项。
    entry: Arc<OAuthEntry>,
}

impl McpAuthProvider for RegistryAuthProvider {
    /// 按请求动态加载并在令牌到期时触发刷新。
    fn access_token<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AuthToken>, McpError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.registry
                .ensure_loaded(&self.entry)
                .await
                .map_err(to_mcp_error)?;
            loop {
                let (token, generation, expired) = {
                    let state = self.entry.state.lock().await;
                    let token = state.token_set.clone();
                    let generation = state.token_generation;
                    let expired = token.as_ref().is_some_and(|token_set| {
                        token_set
                            .expires_at
                            .is_some_and(|expires_at| now_unix_seconds() >= expires_at)
                    });
                    (token, generation, expired)
                };
                let Some(token) = token else {
                    return Ok(None);
                };
                if expired {
                    self.registry
                        .refresh_if_needed(&self.entry, generation)
                        .await
                        .map_err(to_mcp_error)?;
                    continue;
                }
                return Ok(Some(AuthToken {
                    token: token.access_token,
                    generation,
                }));
            }
        })
    }

    /// 处理一次 MCP 401，旧代次已经被替换时直接返回。
    fn on_unauthorized<'life0, 'life1, 'async_trait>(
        &'life0 self,
        sent_generation: u64,
        _www_authenticate: Option<&'life1 str>,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.registry
                .ensure_loaded(&self.entry)
                .await
                .map_err(to_mcp_error)?;
            self.registry
                .refresh_if_needed(&self.entry, sent_generation)
                .await
                .map_err(to_mcp_error)
        })
    }
}

/// 启动单个回环 HTTP 回调监听器；监听器结束后会以 CAS 方式通知 registry。
async fn run_callback_listener(
    registry: McpOAuthRegistry,
    entry: Arc<OAuthEntry>,
    listener: TcpListener,
    generation: u64,
    expires_at: u64,
    cancellation: CancellationSignal,
) {
    let remaining = expires_at.saturating_sub(now_unix_seconds()).max(1);
    let accept_result = tokio::time::timeout(Duration::from_secs(remaining), async {
        loop {
            let (stream, _) = tokio::select! {
                _ = cancellation.cancelled() => return ListenerOutcome::Cancelled,
                accepted = listener.accept() => match accepted {
                    Ok(value) => value,
                    Err(_) => return ListenerOutcome::Failed,
                },
            };
            let parsed =
                tokio::time::timeout(CALLBACK_REQUEST_TIMEOUT, read_callback_request(stream)).await;
            let Ok(Ok(request)) = parsed else {
                continue;
            };
            let Some(request) = request else {
                continue;
            };
            let outcome = registry
                .callback_entry(entry.clone(), request, now_unix_seconds())
                .await;
            match outcome {
                Ok(_) => return ListenerOutcome::Accepted,
                Err(McpOAuthServiceError::OAuth(OAuthError::InvalidState)) => continue,
                Err(_) => return ListenerOutcome::Failed,
            }
        }
    })
    .await;
    if accept_result.is_err() {
        let _ = registry.expire_entry(entry).await;
    }
    let _ = generation;
}

/// 回环监听器的内部结束原因。
enum ListenerOutcome {
    /// 已接管回调。
    Accepted,
    /// 由取消命令结束。
    Cancelled,
    /// 发生不可恢复的监听错误。
    Failed,
}

/// 读取并严格解析一个 HTTP GET 回调请求；响应正文固定且不包含 OAuth 字段。
async fn read_callback_request(mut stream: TcpStream) -> Result<Option<OAuthCallback>, ()> {
    let mut bytes = Vec::with_capacity(MAX_CALLBACK_HEADER_BYTES);
    let mut buffer = [0_u8; 512];
    let header_end = loop {
        let read = stream.read(&mut buffer).await.map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        if bytes.len().saturating_add(read) > MAX_CALLBACK_HEADER_BYTES {
            let _ = write_http_response(&mut stream, 431, "请求头过大").await;
            return Err(());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let header = &bytes[..header_end];
    let Some(line_end) = header.windows(2).position(|window| window == b"\r\n") else {
        let _ = write_http_response(&mut stream, 400, "请求无效").await;
        return Err(());
    };
    if line_end > MAX_CALLBACK_REQUEST_LINE_BYTES {
        let _ = write_http_response(&mut stream, 414, "请求行过大").await;
        return Err(());
    }
    let line = std::str::from_utf8(&header[..line_end]).map_err(|_| ())?;
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if method != "GET" || version != "HTTP/1.1" || parts.next().is_some() {
        let _ = write_http_response(&mut stream, 405, "请求方法不受支持").await;
        return Err(());
    }
    if target.len() > MAX_CALLBACK_QUERY_BYTES + MAX_CALLBACK_REQUEST_LINE_BYTES {
        let _ = write_http_response(&mut stream, 414, "请求地址过大").await;
        return Err(());
    }
    let parsed = Url::parse(&format!("http://localhost{target}")).map_err(|_| ())?;
    if parsed.path() != CALLBACK_PATH
        || parsed.fragment().is_some()
        || parsed.host_str() != Some("localhost")
    {
        let _ = write_http_response(&mut stream, 404, "请求地址不存在").await;
        return Err(());
    }
    let query = parsed.query().unwrap_or_default();
    if query.len() > MAX_CALLBACK_QUERY_BYTES {
        let _ = write_http_response(&mut stream, 414, "请求地址过大").await;
        return Err(());
    }
    let mut state_value = None;
    let mut code = None;
    let mut error = None;
    let mut error_description = None;
    for (name, value) in parsed.query_pairs() {
        match name.as_ref() {
            "state" if state_value.is_none() => state_value = Some(value.into_owned()),
            "code" if code.is_none() => code = Some(value.into_owned()),
            "error" if error.is_none() => error = Some(value.into_owned()),
            "error_description" if error_description.is_none() => {
                error_description = Some(value.into_owned())
            }
            _ => {}
        }
    }
    let Some(state_value) = state_value else {
        let _ = write_http_response(&mut stream, 400, "回调参数无效").await;
        return Err(());
    };
    if code.is_none() && error.is_none() {
        let _ = write_http_response(&mut stream, 400, "回调参数无效").await;
        return Err(());
    }
    let callback = OAuthCallback {
        state: state_value,
        code,
        error,
        error_description,
    };
    write_http_response(&mut stream, 200, "授权处理完成，可以关闭此页面")
        .await
        .map_err(|_| ())?;
    Ok(Some(callback))
}

/// 向本机浏览器写入固定的最小 HTTP 响应。
async fn write_http_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), ()> {
    let body = body.as_bytes();
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|_| ())?;
    stream.write_all(body).await.map_err(|_| ())
}

/// 根据统一长度前缀计算只含哈希的密钥库 account。
fn storage_key_for_parts(
    project_scope: &str,
    server_id: &str,
    authorization_server_issuer: &str,
    resource: &str,
    client_id: &str,
) -> Result<String, McpOAuthServiceError> {
    validate_identifier("server_id", server_id)?;
    validate_text("project_scope", project_scope, MAX_ID_BYTES, true)?;
    validate_issuer(authorization_server_issuer)?;
    validate_text("resource", resource, MAX_RESOURCE_BYTES, true)?;
    validate_text("client_id", client_id, MAX_CLIENT_ID_BYTES, true)?;
    let mut digest = Sha256::new();
    digest.update(b"keencode-mcp-oauth-storage-v2\0");
    for value in [
        project_scope,
        server_id,
        authorization_server_issuer,
        resource,
        client_id,
    ] {
        let length = u64::try_from(value.len())
            .map_err(|_| McpOAuthServiceError::InvalidConfiguration("OAuth 身份过大".to_owned()))?;
        digest.update(length.to_be_bytes());
        digest.update(value.as_bytes());
    }
    let mut result = String::from("oauth-v2-");
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        let _ = write!(&mut result, "{byte:02x}");
    }
    Ok(result)
}

/// 校验发现得到的授权服务器 issuer；它必须是无凭据、无查询和无片段的 HTTPS
/// 规范地址，并且在进入密钥库键之前通过长度与控制字符检查。
fn validate_issuer(value: &str) -> Result<(), McpOAuthServiceError> {
    validate_text(
        "authorization_server_issuer",
        value,
        MAX_RESOURCE_BYTES,
        true,
    )?;
    let url = Url::parse(value).map_err(|_| {
        McpOAuthServiceError::InvalidConfiguration(
            "authorization_server_issuer 必须是有效 HTTPS URI".to_owned(),
        )
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(McpOAuthServiceError::InvalidConfiguration(
            "authorization_server_issuer 必须使用无凭据、无查询和无片段的 HTTPS URI".to_owned(),
        ));
    }
    Ok(())
}

/// 校验项目和 Server 的稳定内存身份。
fn validate_identity(identity: &EntryIdentity) -> Result<(), McpOAuthServiceError> {
    validate_text("project_scope", &identity.project_scope, MAX_ID_BYTES, true)?;
    validate_identifier("server_id", &identity.server_id)
}

/// 校验作用域内 Server 标识的非空、有界和单行约束。
fn validate_identifier(name: &str, value: &str) -> Result<(), McpOAuthServiceError> {
    validate_text(name, value, MAX_ID_BYTES, true)
}

/// 把路径作用域转换为无控制字符、非空文本；调用方应传入规范项目根。
fn scope_text(scope: &Path) -> Result<String, McpOAuthServiceError> {
    let value = scope.to_str().ok_or_else(|| {
        McpOAuthServiceError::InvalidConfiguration("项目作用域不是有效 UTF-8".to_owned())
    })?;
    validate_text("project_scope", value, MAX_ID_BYTES, true)?;
    Ok(value.to_owned())
}

/// 校验普通 OAuth 文本字段。
fn validate_text(
    name: &str,
    value: &str,
    limit: usize,
    require_nonempty: bool,
) -> Result<(), McpOAuthServiceError> {
    if (require_nonempty && value.trim().is_empty())
        || value.len() > limit
        || value.chars().any(char::is_control)
    {
        return Err(McpOAuthServiceError::InvalidConfiguration(format!(
            "{name} 字段无效"
        )));
    }
    Ok(())
}

/// 校验 OAuth scope 字符。
fn validate_scope(scope: &str) -> Result<(), McpOAuthServiceError> {
    if scope.is_empty()
        || !scope.bytes().all(|byte| {
            byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte)
        })
    {
        return Err(McpOAuthServiceError::InvalidConfiguration(
            "OAuth scope 字段无效".to_owned(),
        ));
    }
    Ok(())
}

/// 校验授权地址只能使用 HTTPS 或明确的回环 HTTP，且拒绝凭据查询参数。
fn validate_authorization_redirect(value: &str) -> Result<(), McpOAuthServiceError> {
    let url = Url::parse(value)
        .map_err(|_| McpOAuthServiceError::InvalidConfiguration("OAuth 回调地址无效".to_owned()))?;
    if url.scheme() != "http"
        || !url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(McpOAuthServiceError::InvalidConfiguration(
            "OAuth 回调地址必须是回环 HTTP 地址".to_owned(),
        ));
    }
    Ok(())
}

/// 从当前密钥库令牌构造已授权或空闲状态机。
fn machine_for_token(
    config: &OAuthConfig,
    token_set: Option<OAuthTokenSet>,
) -> Result<OAuthMachine, McpOAuthServiceError> {
    let Some(token_set) = token_set else {
        return OAuthMachine::new(config.clone()).map_err(Into::into);
    };
    let snapshot: OAuthSnapshot = serde_json::from_value(serde_json::json!({
        "status": "authorized",
        "pending": null,
        "tokenSet": token_set,
        "lastError": null
    }))
    .map_err(|_| McpOAuthServiceError::SecretStore)?;
    OAuthMachine::restore(config.clone(), snapshot).map_err(Into::into)
}

/// 在操作失败或取消后恢复旧令牌，pending 永远不会进入密钥库。
fn restore_failed_machine(
    state: &mut EntryState,
    previous_token_set: Option<OAuthTokenSet>,
) -> Result<(), McpOAuthServiceError> {
    state.token_set = previous_token_set;
    if let Some(config) = state.oauth_config.clone() {
        state.machine = Some(machine_for_token(&config, state.token_set.clone())?);
    } else {
        state.machine = None;
    }
    Ok(())
}

/// 对一次交换任务执行取消选择，并把结果提交到 CAS 状态机。
async fn run_exchange(
    registry: McpOAuthRegistry,
    entry: Arc<OAuthEntry>,
    generation: u64,
    request: OAuthTokenRequest,
    kind: OperationKind,
    cancellation: CancellationSignal,
) {
    let result = tokio::select! {
        _ = cancellation.cancelled() => Err(OAuthError::InvalidTransition("OAuth 操作已取消".to_owned())),
        result = registry.inner.exchanger.exchange(&request, now_unix_seconds()) => result,
    };
    registry
        .finish_exchange(entry, generation, kind, result)
        .await;
}

/// 返回安全的 Unix 秒时间；系统时钟异常时固定回退到零。
fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 为代次分配下一个非零值。
fn next_generation(current: u64) -> Result<u64, McpOAuthServiceError> {
    current
        .checked_add(1)
        .ok_or_else(|| McpOAuthServiceError::InvalidConfiguration("OAuth 操作代次耗尽".to_owned()))
}

/// 把服务错误转换为 MCP 认证边界错误。
fn to_mcp_error(error: McpOAuthServiceError) -> McpError {
    match error {
        McpOAuthServiceError::OAuth(error) => McpError::OAuth(error),
        other => McpError::Transport(other.to_string()),
    }
}

/// 把状态机错误转换为固定展示摘要。
fn safe_failure_message(error: &OAuthError) -> String {
    match error {
        OAuthError::AuthorizationDenied { .. } => "OAuth 授权被拒绝".to_owned(),
        OAuthError::AuthorizationExpired => "OAuth 授权请求已过期".to_owned(),
        OAuthError::InvalidState => "OAuth 回调校验失败".to_owned(),
        OAuthError::MissingRefreshToken => "OAuth 没有可用刷新令牌".to_owned(),
        OAuthError::TokenExpired => "OAuth 访问令牌已过期".to_owned(),
        OAuthError::InvalidConfiguration(_) => "OAuth 配置无效".to_owned(),
        OAuthError::InvalidDiscovery(_) => "OAuth 发现结果无效".to_owned(),
        OAuthError::DiscoveryTransport(_) => "OAuth 网络请求失败".to_owned(),
        OAuthError::Randomness(_) => "OAuth 随机数生成失败".to_owned(),
        OAuthError::InvalidCallback(_) => "OAuth 回调或令牌响应无效".to_owned(),
        OAuthError::InvalidTransition(_) => "OAuth 状态转换失败".to_owned(),
    }
}

/// 判断令牌交换是否因授权服务明确拒绝旧令牌而失败。
fn is_invalid_grant(error: &OAuthError) -> bool {
    match error {
        OAuthError::AuthorizationDenied { code, .. } => code == "invalid_grant",
        _ => false,
    }
}

/// 读取系统密钥库中的 OAuth JSON；仅在支持的桌面平台调用。
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn system_secret_get(key: &str) -> Result<Option<OAuthTokenSet>, McpOAuthServiceError> {
    use keyring::{Entry, Error as KeyringError};
    let entry = Entry::new(KEYRING_SERVICE, key).map_err(|_| McpOAuthServiceError::SecretStore)?;
    let value = match entry.get_password() {
        Ok(value) => value,
        Err(KeyringError::NoEntry) => return Ok(None),
        Err(_) => return Err(McpOAuthServiceError::SecretStore),
    };
    serde_json::from_str(&value)
        .map(Some)
        .map_err(|_| McpOAuthServiceError::SecretStore)
}

/// 在不支持系统密钥库的平台明确失败，避免静默落盘明文 token。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn system_secret_get(_key: &str) -> Result<Option<OAuthTokenSet>, McpOAuthServiceError> {
    Err(McpOAuthServiceError::SecretStore)
}

/// 写入系统密钥库中的 OAuth JSON。
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn system_secret_set(key: &str, token_set: &OAuthTokenSet) -> Result<(), McpOAuthServiceError> {
    use keyring::Entry;
    let entry = Entry::new(KEYRING_SERVICE, key).map_err(|_| McpOAuthServiceError::SecretStore)?;
    let value = serde_json::to_string(token_set).map_err(|_| McpOAuthServiceError::SecretStore)?;
    entry
        .set_password(&value)
        .map_err(|_| McpOAuthServiceError::SecretStore)
}

/// 在不支持系统密钥库的平台拒绝写入明文 token。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn system_secret_set(_key: &str, _token_set: &OAuthTokenSet) -> Result<(), McpOAuthServiceError> {
    Err(McpOAuthServiceError::SecretStore)
}

/// 幂等删除系统密钥库中的 OAuth JSON。
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn system_secret_delete(key: &str) -> Result<(), McpOAuthServiceError> {
    use keyring::{Entry, Error as KeyringError};
    let entry = Entry::new(KEYRING_SERVICE, key).map_err(|_| McpOAuthServiceError::SecretStore)?;
    match entry.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(_) => Err(McpOAuthServiceError::SecretStore),
    }
}

/// 在不支持系统密钥库的平台拒绝删除操作，避免误报清理成功。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn system_secret_delete(_key: &str) -> Result<(), McpOAuthServiceError> {
    Err(McpOAuthServiceError::SecretStore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Notify;

    /// 构造只包含公开绑定字段的测试设置。
    fn settings() -> McpOAuthSettings {
        McpOAuthSettings::new("keencode-test", "https://mcp.example.test/mcp")
    }

    /// 构造指定 scope 的测试设置，用于验证完整配置变化会撤销旧绑定。
    fn settings_with_scopes(scopes: &[&str]) -> McpOAuthSettings {
        let mut settings = settings();
        settings.scopes = scopes.iter().map(|scope| (*scope).to_owned()).collect();
        settings
    }

    /// 授权服务器签发方必须成为密钥库键的一部分，避免不同 issuer 复用令牌。
    #[test]
    fn storage_key_isolated_by_authorization_server_issuer() {
        let settings = settings();
        let first = McpOAuthRegistry::storage_key(
            "/project",
            "server",
            &settings,
            "https://auth-one.example.test",
        )
        .expect("第一个 issuer 应生成密钥键");
        let second = McpOAuthRegistry::storage_key(
            "/project",
            "server",
            &settings,
            "https://auth-two.example.test",
        )
        .expect("第二个 issuer 应生成密钥键");
        assert_ne!(first, second);
    }

    /// 密钥库键拒绝不安全或带查询/片段的发现 issuer。
    #[test]
    fn storage_key_rejects_unsafe_authorization_server_issuer() {
        let settings = settings();
        for issuer in [
            "http://auth.example.test",
            "https://user:secret@auth.example.test",
            "https://auth.example.test?secret=1",
            "https://auth.example.test#fragment",
        ] {
            assert!(
                McpOAuthRegistry::storage_key("/project", "server", &settings, issuer).is_err(),
                "issuer {issuer} 必须被拒绝"
            );
        }
    }

    /// 状态查询应把没有 refresh token 的过期令牌报告为 Expired，但不能启动刷新请求。
    #[tokio::test]
    async fn status_reports_expired_token_without_starting_refresh() {
        let store = InMemoryOAuthSecretStore::new();
        let issuer = "https://auth.example.test";
        seed_token(
            &store,
            issuer,
            &token_set(
                "expired-access",
                None,
                Some(now_unix_seconds().saturating_sub(1)),
            ),
        )
        .await;
        let exchanger = FakeExchanger::immediate(token_set(
            "unused-access",
            Some("unused-refresh"),
            Some(now_unix_seconds() + 3600),
        ));
        let registry = McpOAuthRegistry::with_dependencies(
            Arc::new(store),
            Arc::new(exchanger.clone()),
            Arc::new(RecordingEventSink::default()),
        );
        install(&registry, oauth_config(issuer, 60))
            .await
            .expect("带过期令牌的配置应成功");

        let snapshot = registry
            .status("/project", "server")
            .await
            .expect("状态应可读取");
        assert_eq!(snapshot.status, OAuthStatus::Expired);
        assert_eq!(exchanger.calls(), 0, "状态查询不得启动刷新");
    }

    /// 交换器行为，允许测试立即返回、延迟返回或等待显式放行。
    #[derive(Clone)]
    enum ExchangeBehavior {
        /// 立即返回固定结果。
        Immediate(Result<OAuthTokenSet, OAuthError>),
        /// 设置 started 后等待 release，再返回固定结果。
        Gate {
            /// 等待开始观察的标志。
            release: Arc<AtomicBool>,
            /// 放行通知。
            release_notify: Arc<Notify>,
            /// 放行后的固定结果。
            result: Result<OAuthTokenSet, OAuthError>,
        },
        /// 等待固定时间后返回固定结果。
        Delay {
            /// 模拟网络延迟。
            delay: Duration,
            /// 延迟后的固定结果。
            result: Result<OAuthTokenSet, OAuthError>,
        },
    }

    /// 不访问网络的确定性 OAuth 令牌交换器。
    #[derive(Clone)]
    struct FakeExchanger {
        /// 已进入交换边界的调用数。
        calls: Arc<AtomicUsize>,
        /// 首次进入交换边界的标志。
        started: Arc<AtomicBool>,
        /// 首次进入交换边界的通知。
        started_notify: Arc<Notify>,
        /// 当前注入的行为。
        behavior: Arc<StdMutex<ExchangeBehavior>>,
    }

    impl FakeExchanger {
        /// 创建立即返回令牌的交换器。
        fn immediate(token_set: OAuthTokenSet) -> Self {
            Self::with_behavior(ExchangeBehavior::Immediate(Ok(token_set)))
        }

        /// 创建等待放行的交换器。
        fn gated(token_set: OAuthTokenSet) -> Self {
            Self::with_behavior(ExchangeBehavior::Gate {
                release: Arc::new(AtomicBool::new(false)),
                release_notify: Arc::new(Notify::new()),
                result: Ok(token_set),
            })
        }

        /// 创建延迟返回固定结果的交换器。
        fn delayed(result: Result<OAuthTokenSet, OAuthError>, delay: Duration) -> Self {
            Self::with_behavior(ExchangeBehavior::Delay { delay, result })
        }

        /// 使用指定行为创建交换器。
        fn with_behavior(behavior: ExchangeBehavior) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(AtomicBool::new(false)),
                started_notify: Arc::new(Notify::new()),
                behavior: Arc::new(StdMutex::new(behavior)),
            }
        }

        /// 返回交换调用次数。
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }

        /// 等待至少一次交换调用进入边界。
        async fn wait_started(&self) {
            timeout_wait_for_flag(&self.started).await;
        }

        /// 放行等待中的交换。
        fn release(&self) {
            let behavior = self.behavior.lock().expect("交换器行为锁未中毒");
            if let ExchangeBehavior::Gate {
                release,
                release_notify,
                ..
            } = &*behavior
            {
                release.store(true, Ordering::Release);
                release_notify.notify_waiters();
            }
        }
    }

    impl OAuthTokenExchanger for FakeExchanger {
        /// 执行注入的确定性交换行为。
        fn exchange<'a>(
            &'a self,
            _request: &'a OAuthTokenRequest,
            _now_unix_seconds: u64,
        ) -> ServiceFuture<'a, Result<OAuthTokenSet, OAuthError>> {
            let calls = self.calls.clone();
            let started = self.started.clone();
            let started_notify = self.started_notify.clone();
            let behavior = self.behavior.lock().expect("交换器行为锁未中毒").clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::AcqRel);
                started.store(true, Ordering::Release);
                started_notify.notify_waiters();
                match behavior {
                    ExchangeBehavior::Immediate(result) => result,
                    ExchangeBehavior::Gate {
                        release,
                        release_notify,
                        result,
                    } => {
                        let notified = release_notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        if !release.load(Ordering::Acquire) {
                            notified.await;
                        }
                        result
                    }
                    ExchangeBehavior::Delay { delay, result } => {
                        tokio::time::sleep(delay).await;
                        result
                    }
                }
            })
        }
    }

    /// 收集已脱敏生命周期事件的测试 sink。
    #[derive(Clone, Default)]
    struct RecordingEventSink {
        /// 事件序列。
        events: Arc<StdMutex<Vec<McpOAuthEvent>>>,
    }

    impl RecordingEventSink {
        /// 返回当前事件快照。
        fn events(&self) -> Vec<McpOAuthEvent> {
            self.events.lock().expect("事件锁未中毒").clone()
        }
    }

    impl McpOAuthEventSink for RecordingEventSink {
        /// 将事件写入内存序列，不产生额外异步等待。
        fn emit<'a>(
            &'a self,
            event: McpOAuthEvent,
        ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
            let events = self.events.clone();
            Box::pin(async move {
                events
                    .lock()
                    .map_err(|_| McpOAuthServiceError::EventDelivery)?
                    .push(event);
                Ok(())
            })
        }
    }

    /// 在一次 get 期间阻塞的内存密钥库，用于覆盖加载与停用的 TOCTOU。
    #[derive(Clone)]
    struct BlockingGetSecretStore {
        /// 实际保存令牌的内存存储。
        inner: InMemoryOAuthSecretStore,
        /// 是否阻塞 get。
        block_get: Arc<AtomicBool>,
        /// get 是否已经进入阻塞点。
        get_started: Arc<AtomicBool>,
        /// get 开始通知。
        get_started_notify: Arc<Notify>,
        /// 是否放行 get。
        release_get: Arc<AtomicBool>,
        /// get 放行通知。
        release_get_notify: Arc<Notify>,
    }

    impl BlockingGetSecretStore {
        /// 创建默认不阻塞的密钥库。
        fn new() -> Self {
            Self {
                inner: InMemoryOAuthSecretStore::new(),
                block_get: Arc::new(AtomicBool::new(false)),
                get_started: Arc::new(AtomicBool::new(false)),
                get_started_notify: Arc::new(Notify::new()),
                release_get: Arc::new(AtomicBool::new(false)),
                release_get_notify: Arc::new(Notify::new()),
            }
        }

        /// 开启后续 get 的阻塞。
        fn block(&self) {
            self.get_started.store(false, Ordering::Release);
            self.release_get.store(false, Ordering::Release);
            self.block_get.store(true, Ordering::Release);
        }

        /// 等待 get 已经进入阻塞点。
        async fn wait_started(&self) {
            timeout_wait_for_flag(&self.get_started).await;
        }

        /// 放行当前阻塞的 get。
        fn release(&self) {
            self.release_get.store(true, Ordering::Release);
            self.release_get_notify.notify_waiters();
        }
    }

    impl OAuthSecretStore for BlockingGetSecretStore {
        /// 在需要时阻塞读取，再委托给内存存储。
        fn get<'a>(
            &'a self,
            key: &'a str,
        ) -> ServiceFuture<'a, Result<Option<OAuthTokenSet>, McpOAuthServiceError>> {
            let this = self.clone();
            let key = key.to_owned();
            Box::pin(async move {
                if this.block_get.load(Ordering::Acquire) {
                    this.get_started.store(true, Ordering::Release);
                    this.get_started_notify.notify_waiters();
                    let notified = this.release_get_notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if !this.release_get.load(Ordering::Acquire) {
                        notified.await;
                    }
                }
                this.inner.get(&key).await
            })
        }

        /// 写入内存存储。
        fn set<'a>(
            &'a self,
            key: &'a str,
            token_set: &'a OAuthTokenSet,
        ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
            self.inner.set(key, token_set)
        }

        /// 删除内存存储中的令牌。
        fn delete<'a>(
            &'a self,
            key: &'a str,
        ) -> ServiceFuture<'a, Result<(), McpOAuthServiceError>> {
            self.inner.delete(key)
        }
    }

    /// 构造完整且通过校验的 OAuth 端点配置。
    fn oauth_config(issuer: &str, timeout_seconds: u64) -> OAuthConfig {
        OAuthConfig {
            authorization_server_issuer: issuer.to_owned(),
            authorization_endpoint: format!("{issuer}/authorize"),
            token_endpoint: format!("{issuer}/token"),
            resource: "https://mcp.example.test/mcp".to_owned(),
            client_id: "keencode-test".to_owned(),
            redirect_uri: "http://127.0.0.1/oauth/callback".to_owned(),
            scopes: vec!["mcp".to_owned()],
            code_challenge_methods_supported: vec!["S256".to_owned()],
            authorization_timeout_seconds: timeout_seconds,
        }
    }

    /// 注册并完成一次受控 OAuth 发现配置。
    async fn install(
        registry: &McpOAuthRegistry,
        config: OAuthConfig,
    ) -> Result<(), McpOAuthServiceError> {
        registry.register("/project", "server", settings()).await?;
        registry.configure("/project", "server", config).await
    }

    /// 将令牌预置到当前 issuer 对应的测试密钥键。
    async fn seed_token(store: &InMemoryOAuthSecretStore, issuer: &str, token: &OAuthTokenSet) {
        let key = McpOAuthRegistry::storage_key("/project", "server", &settings(), issuer)
            .expect("测试 issuer 应能生成密钥键");
        store.set(&key, token).await.expect("测试令牌应能预置");
    }

    /// 构造供测试交换器返回的访问令牌集合。
    fn token_set(
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at: Option<u64>,
    ) -> OAuthTokenSet {
        OAuthTokenSet {
            access_token: access_token.to_owned(),
            token_type: "Bearer".to_owned(),
            expires_at,
            refresh_token: refresh_token.map(str::to_owned),
            scope: Some("mcp".to_owned()),
        }
    }

    /// 等待布尔原子标志变为真，外层调用方再套总超时。
    async fn timeout_wait_for_flag(flag: &AtomicBool) {
        for _ in 0..500 {
            if flag.load(Ordering::Acquire) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("测试等待的异步边界未进入");
    }

    /// 等待当前授权或刷新操作结束，并把轮询限制在测试调用方预算内。
    async fn wait_for_operation(registry: &McpOAuthRegistry) -> McpOAuthStatusSnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = registry
                    .status("/project", "server")
                    .await
                    .expect("状态应可读取");
                if !status.operation_pending {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("OAuth 操作应在测试预算内结束")
    }

    /// 使用真实回环 HTTP 请求提交授权回调。
    async fn send_http_callback(start: &McpOAuthStartResult, code: &str) {
        let redirect = Url::parse(start.redirect_uri()).expect("回调地址应有效");
        let host = redirect.host_str().expect("回调地址应有主机");
        let port = redirect.port().expect("回调地址应有随机端口");
        let mut stream = None;
        for _ in 0..100 {
            match tokio::time::timeout(Duration::from_millis(50), TcpStream::connect((host, port)))
                .await
            {
                Ok(Ok(connection)) => {
                    stream = Some(connection);
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(2)).await,
            }
        }
        let mut stream = stream.expect("回环 OAuth listener 应接受连接");
        let request = format!(
            "GET /oauth/callback?state={}&code={} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            start.authorization_request.state, code
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("回调请求应写入");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
            .await
            .expect("回调响应应及时返回")
            .expect("回调响应应可读取");
        assert!(String::from_utf8_lossy(&response).contains("200"));
    }

    /// 等待指定 Provider 返回访问令牌，并将总等待限定在测试调用方。
    async fn wait_for_token(provider: Arc<dyn McpAuthProvider>) -> AuthToken {
        loop {
            if let Some(token) = provider
                .access_token()
                .await
                .expect("Provider 读取不应失败")
            {
                return token;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// 等待被取消的回环 listener 不再接受连接。
    async fn wait_listener_rejected(redirect_uri: &str) {
        let redirect = Url::parse(redirect_uri).expect("回调地址应有效");
        let host = redirect.host_str().expect("回调地址应有主机");
        let port = redirect.port().expect("回调地址应有端口");
        for _ in 0..100 {
            let connected =
                tokio::time::timeout(Duration::from_millis(50), TcpStream::connect((host, port)))
                    .await
                    .is_ok_and(|result| result.is_ok());
            if !connected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("已取消的 OAuth listener 仍接受连接");
    }

    /// 真实回环 HTTP 回调应完成授权码交换、持久化令牌并通知宿主。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loopback_authorization_completes_end_to_end() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let exchanger = FakeExchanger::immediate(token_set(
                "access-from-callback",
                Some("refresh-from-callback"),
                Some(now_unix_seconds() + 3600),
            ));
            let sink = RecordingEventSink::default();
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store.clone()),
                Arc::new(exchanger.clone()),
                Arc::new(sink.clone()),
            );
            let issuer = "https://auth.example.test";
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("测试 OAuth 配置应成功");
            let start = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("授权应能开始");
            send_http_callback(&start, "authorization-code").await;

            let provider = registry
                .auth_provider("/project", "server")
                .expect("认证提供方应能创建");
            let token = wait_for_token(provider).await;
            assert_eq!(token.token, "access-from-callback");
            assert_eq!(exchanger.calls(), 1);
            let key = McpOAuthRegistry::storage_key("/project", "server", &settings(), issuer)
                .expect("测试 issuer 应生成密钥键");
            assert!(store.contains(&key));
            let events = sink.events();
            assert!(
                events
                    .iter()
                    .any(|event| { matches!(event, McpOAuthEvent::AuthorizationRequired { .. }) })
            );
            assert!(
                events
                    .iter()
                    .any(|event| { matches!(event, McpOAuthEvent::Authorized { .. }) })
            );
        })
        .await
        .expect("回环授权测试不得超时");
    }

    /// 错误 state 只能被拒绝，不能消费 pending；随后正确回调仍应成功。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_state_does_not_consume_pending_authorization() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let exchanger = FakeExchanger::immediate(token_set(
                "access-after-state-check",
                Some("refresh-after-state-check"),
                Some(now_unix_seconds() + 3600),
            ));
            let sink = RecordingEventSink::default();
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store),
                Arc::new(exchanger),
                Arc::new(sink),
            );
            install(&registry, oauth_config("https://auth.example.test", 60))
                .await
                .expect("测试 OAuth 配置应成功");
            let start = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("授权应能开始");
            let invalid = registry
                .callback(
                    "/project",
                    "server",
                    "wrong-code".to_owned(),
                    "wrong-state".to_owned(),
                    now_unix_seconds(),
                )
                .await;
            assert!(matches!(
                invalid,
                Err(McpOAuthServiceError::OAuth(OAuthError::InvalidState))
            ));
            let pending_status = registry
                .status("/project", "server")
                .await
                .expect("状态应可读取");
            assert_eq!(pending_status.status, OAuthStatus::AwaitingAuthorization);
            assert!(pending_status.operation_pending);
            registry
                .callback(
                    "/project",
                    "server",
                    "correct-code".to_owned(),
                    start.authorization_request.state,
                    now_unix_seconds(),
                )
                .await
                .expect("正确 state 应继续交换");
            let provider = registry
                .auth_provider("/project", "server")
                .expect("认证提供方应能创建");
            assert_eq!(
                wait_for_token(provider).await.token,
                "access-after-state-check"
            );
        })
        .await
        .expect("state 校验测试不得超时");
    }

    /// 取消操作后迟到的交换结果不得写入密钥库，旧 listener 也必须退出且不影响新代次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_stops_listener_and_rejects_late_exchange() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let exchanger = FakeExchanger::gated(token_set(
                "late-access",
                Some("late-refresh"),
                Some(now_unix_seconds() + 3600),
            ));
            let sink = RecordingEventSink::default();
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store.clone()),
                Arc::new(exchanger.clone()),
                Arc::new(sink),
            );
            install(&registry, oauth_config("https://auth.example.test", 60))
                .await
                .expect("测试 OAuth 配置应成功");
            let first = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("第一次授权应能开始");
            registry
                .callback(
                    "/project",
                    "server",
                    "delayed-code".to_owned(),
                    first.authorization_request.state.clone(),
                    now_unix_seconds(),
                )
                .await
                .expect("回调应启动延迟交换");
            exchanger.wait_started().await;
            assert!(
                registry
                    .cancel("/project", "server")
                    .await
                    .expect("取消应执行")
            );
            exchanger.release();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let key = McpOAuthRegistry::storage_key(
                "/project",
                "server",
                &settings(),
                "https://auth.example.test",
            )
            .expect("测试 issuer 应生成密钥键");
            assert!(!store.contains(&key));
            assert!(
                registry
                    .callback(
                        "/project",
                        "server",
                        "late-code".to_owned(),
                        first.authorization_request.state.clone(),
                        now_unix_seconds(),
                    )
                    .await
                    .is_err()
            );
            wait_listener_rejected(&first.redirect_uri).await;

            // 取消后立即开启新代次；旧 listener 的退出不能取消或接管新操作。
            let second = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("取消后新授权应能开始");
            assert!(second.generation() > first.generation());
            assert_ne!(second.redirect_uri(), first.redirect_uri());
            registry
                .callback(
                    "/project",
                    "server",
                    "new-code".to_owned(),
                    second.authorization_request.state,
                    now_unix_seconds(),
                )
                .await
                .expect("新代次回调应能提交");
            // 当前 exchanger 已放行，新的交换结果也应最终可见。
            let provider = registry
                .auth_provider("/project", "server")
                .expect("认证提供方应能创建");
            assert_eq!(wait_for_token(provider).await.token, "late-access");
        })
        .await
        .expect("取消和迟到结果测试不得超时");
    }

    /// scope 变化属于完整 OAuth 配置变化，旧 pending 与旧 Provider 都必须失效。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn changing_scopes_revokes_old_pending_and_provider() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(InMemoryOAuthSecretStore::new()),
                Arc::new(FakeExchanger::immediate(token_set(
                    "unused-access",
                    Some("unused-refresh"),
                    Some(now_unix_seconds() + 3600),
                ))),
                Arc::new(RecordingEventSink::default()),
            );
            let issuer = "https://auth.example.test";
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("测试 OAuth 配置应成功");
            let old_provider = registry
                .auth_provider("/project", "server")
                .expect("旧 Provider 应能创建");
            let pending = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("旧 scope 配置应能开始授权");

            registry
                .register(
                    "/project",
                    "server",
                    settings_with_scopes(&["changed-scope"]),
                )
                .await
                .expect("scope 变化应创建新绑定");

            assert!(old_provider.access_token().await.is_err());
            let status = registry
                .status("/project", "server")
                .await
                .expect("新绑定状态应可读取");
            assert_eq!(status.status, OAuthStatus::Idle);
            assert!(!status.operation_pending);
            wait_listener_rejected(&pending.redirect_uri).await;
        })
        .await
        .expect("scope 变化撤销测试不得超时");
    }

    /// 授权等待到期后应清除 pending 并关闭本机 listener。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authorization_timeout_closes_listener() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let exchanger = FakeExchanger::delayed(
                Ok(token_set("unused", Some("unused-refresh"), None)),
                Duration::from_millis(1),
            );
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(InMemoryOAuthSecretStore::new()),
                Arc::new(exchanger),
                Arc::new(RecordingEventSink::default()),
            );
            install(&registry, oauth_config("https://auth.example.test", 1))
                .await
                .expect("测试 OAuth 配置应成功");
            let start = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("授权应能开始");
            tokio::time::sleep(Duration::from_millis(1300)).await;
            let status = registry
                .status("/project", "server")
                .await
                .expect("状态应可读取");
            assert!(!status.operation_pending);
            wait_listener_rejected(&start.redirect_uri).await;
        })
        .await
        .expect("授权超时测试不得超时");
    }

    /// 授权码交换失败后，已过期的旧令牌必须恢复为 Expired 而不是 Authorized。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authorization_failure_restores_expired_previous_token_as_expired() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let issuer = "https://auth.example.test";
            seed_token(
                &store,
                issuer,
                &token_set(
                    "expired-old-access",
                    Some("old-refresh"),
                    Some(now_unix_seconds().saturating_sub(1)),
                ),
            )
            .await;
            let exchanger = FakeExchanger::with_behavior(ExchangeBehavior::Immediate(Err(
                OAuthError::DiscoveryTransport("temporary exchange failure".to_owned()),
            )));
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store),
                Arc::new(exchanger),
                Arc::new(RecordingEventSink::default()),
            );
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("带过期旧令牌的配置应成功");
            let start = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("重新授权应能开始");
            registry
                .callback(
                    "/project",
                    "server",
                    "authorization-code".to_owned(),
                    start.authorization_request.state,
                    now_unix_seconds(),
                )
                .await
                .expect("回调应能提交交换");

            let status = wait_for_operation(&registry).await;
            assert_eq!(status.status, OAuthStatus::Expired);
        })
        .await
        .expect("授权失败恢复测试不得超时");
    }

    /// 取消重新授权后，已过期的旧令牌也必须保持 Expired 状态。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_restores_expired_previous_token_as_expired() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let issuer = "https://auth.example.test";
            seed_token(
                &store,
                issuer,
                &token_set(
                    "expired-old-access",
                    Some("old-refresh"),
                    Some(now_unix_seconds().saturating_sub(1)),
                ),
            )
            .await;
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store),
                Arc::new(FakeExchanger::immediate(token_set(
                    "unused-access",
                    Some("unused-refresh"),
                    Some(now_unix_seconds() + 3600),
                ))),
                Arc::new(RecordingEventSink::default()),
            );
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("带过期旧令牌的配置应成功");
            let start = registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("重新授权应能开始");
            assert!(
                registry
                    .cancel("/project", "server")
                    .await
                    .expect("取消应成功")
            );

            let status = registry
                .status("/project", "server")
                .await
                .expect("取消后状态应可读取");
            assert_eq!(status.status, OAuthStatus::Expired);
            wait_listener_rejected(&start.redirect_uri).await;
        })
        .await
        .expect("取消恢复测试不得超时");
    }

    /// 并发读取过期令牌只能启动一次刷新，所有调用方都应在有限时间内获得新令牌。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_expired_access_tokens_use_one_refresh_exchange() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let now = now_unix_seconds();
            let issuer = "https://auth.example.test";
            seed_token(
                &store,
                issuer,
                &token_set(
                    "expired-access",
                    Some("refresh-for-single-flight"),
                    Some(now.saturating_sub(1)),
                ),
            )
            .await;
            let exchanger = FakeExchanger::immediate(token_set(
                "refreshed-access",
                Some("refresh-for-single-flight"),
                Some(now + 3600),
            ));
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store),
                Arc::new(exchanger.clone()),
                Arc::new(RecordingEventSink::default()),
            );
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("带过期令牌的配置应成功");
            let provider = registry
                .auth_provider("/project", "server")
                .expect("认证提供方应能创建");
            let mut handles = Vec::new();
            for _ in 0..20 {
                let provider = provider.clone();
                handles.push(tokio::spawn(async move {
                    provider
                        .access_token()
                        .await
                        .expect("并发读取不应失败")
                        .expect("刷新后应有令牌")
                }));
            }
            let tokens = tokio::time::timeout(Duration::from_secs(5), async {
                let mut values = Vec::new();
                for handle in handles {
                    values.push(handle.await.expect("并发读取任务应完成"));
                }
                values
            })
            .await
            .expect("single-flight 刷新应在超时内完成");
            assert_eq!(exchanger.calls(), 1);
            assert!(tokens.iter().all(|token| token.token == "refreshed-access"));
        })
        .await
        .expect("并发刷新测试不得超时");
    }

    /// 状态查询遇到进行中的刷新时必须保留 Refreshing，不提前改写为 Expired。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_preserves_refreshing_state_while_exchange_is_in_flight() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let issuer = "https://auth.example.test";
            seed_token(
                &store,
                issuer,
                &token_set(
                    "expired-access",
                    Some("refresh-token"),
                    Some(now_unix_seconds().saturating_sub(1)),
                ),
            )
            .await;
            let exchanger = FakeExchanger::gated(token_set(
                "refreshed-access",
                Some("refresh-token"),
                Some(now_unix_seconds() + 3600),
            ));
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store),
                Arc::new(exchanger.clone()),
                Arc::new(RecordingEventSink::default()),
            );
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("带过期令牌的配置应成功");
            let provider = registry
                .auth_provider("/project", "server")
                .expect("认证提供方应能创建");
            let task = tokio::spawn(async move { provider.access_token().await });
            exchanger.wait_started().await;

            let status = registry
                .status("/project", "server")
                .await
                .expect("刷新中的状态应可读取");
            assert_eq!(status.status, OAuthStatus::Refreshing);
            assert!(status.operation_pending);

            exchanger.release();
            let result = task
                .await
                .expect("刷新任务应结束")
                .expect("刷新任务不应失败")
                .expect("刷新后应有访问令牌");
            assert_eq!(result.token, "refreshed-access");
        })
        .await
        .expect("刷新状态保留测试不得超时");
    }

    /// 有效旧令牌的临时刷新失败必须保留令牌和 Authorized 能力，供后续请求继续使用。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn temporary_refresh_failure_preserves_valid_authorization() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let issuer = "https://auth.example.test";
            seed_token(
                &store,
                issuer,
                &token_set(
                    "valid-old-access",
                    Some("old-refresh"),
                    Some(now_unix_seconds() + 3600),
                ),
            )
            .await;
            let exchanger = FakeExchanger::with_behavior(ExchangeBehavior::Immediate(Err(
                OAuthError::DiscoveryTransport("temporary refresh failure".to_owned()),
            )));
            let sink = RecordingEventSink::default();
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store),
                Arc::new(exchanger.clone()),
                Arc::new(sink.clone()),
            );
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("带有效旧令牌的配置应成功");
            let provider = registry
                .auth_provider("/project", "server")
                .expect("认证提供方应能创建");
            let old_token = provider
                .access_token()
                .await
                .expect("旧令牌读取不应失败")
                .expect("应有有效旧令牌");
            assert_eq!(old_token.token, "valid-old-access");

            assert!(
                provider
                    .on_unauthorized(old_token.generation, None)
                    .await
                    .is_err()
            );
            let status = registry
                .status("/project", "server")
                .await
                .expect("刷新失败后状态应可读取");
            assert_eq!(status.status, OAuthStatus::Authorized);
            assert_eq!(exchanger.calls(), 1);
            assert_eq!(
                provider
                    .access_token()
                    .await
                    .expect("临时失败后旧令牌仍应可读取")
                    .expect("临时失败后仍应保留授权")
                    .token,
                "valid-old-access"
            );
            assert!(
                sink.events()
                    .iter()
                    .any(|event| matches!(event, McpOAuthEvent::Failed { .. }))
            );
        })
        .await
        .expect("临时刷新失败保留测试不得超时");
    }

    /// 停用必须在阻塞的密钥库读取解除后再次检查 revoked，旧 Provider 不得复活。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deactivate_during_blocked_load_fails_closed_after_get() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = BlockingGetSecretStore::new();
            let issuer = "https://auth.example.test";
            let stored = token_set(
                "stored-before-deactivate",
                Some("refresh-before-deactivate"),
                Some(now_unix_seconds() + 3600),
            );
            seed_token(&store.inner, issuer, &stored).await;
            let exchanger = FakeExchanger::immediate(token_set("unused", None, None));
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store.clone()),
                Arc::new(exchanger),
                Arc::new(RecordingEventSink::default()),
            );
            registry
                .register("/project", "server", settings())
                .await
                .expect("注册应成功");
            let old_provider = registry
                .auth_provider("/project", "server")
                .expect("旧 Provider 应能创建");
            store.block();
            let configure_registry = registry.clone();
            let configure_task = tokio::spawn(async move {
                configure_registry
                    .configure("/project", "server", oauth_config(issuer, 60))
                    .await
            });
            store.wait_started().await;
            assert!(
                registry
                    .deactivate("/project", "server")
                    .await
                    .expect("停用应成功")
            );
            store.release();
            let configured = configure_task.await.expect("配置任务应结束");
            assert!(matches!(
                configured,
                Err(McpOAuthServiceError::NotRegistered)
            ));
            // 旧条目仍被 configure 的 Provider 引用时，也必须 fail-closed。
            assert!(
                registry.auth_provider("/project", "server").is_err(),
                "停用后当前绑定应已从查找表移除"
            );
            assert!(old_provider.access_token().await.is_err());
        })
        .await
        .expect("停用 TOCTOU 测试不得超时");
    }

    /// 停用保留密钥库令牌，重新注册同一 issuer 后可恢复；旧 Provider 永久失效。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deactivate_preserves_token_for_same_issuer_reactivation() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let issuer = "https://auth.example.test";
            let stored = token_set(
                "persisted-for-reactivation",
                Some("refresh-for-reactivation"),
                Some(now_unix_seconds() + 3600),
            );
            seed_token(&store, issuer, &stored).await;
            let exchanger = FakeExchanger::immediate(token_set("unused", None, None));
            let registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store.clone()),
                Arc::new(exchanger),
                Arc::new(RecordingEventSink::default()),
            );
            install(&registry, oauth_config(issuer, 60))
                .await
                .expect("首次配置应成功");
            let old_provider = registry
                .auth_provider("/project", "server")
                .expect("旧 Provider 应能创建");
            assert_eq!(
                old_provider
                    .access_token()
                    .await
                    .expect("旧令牌读取不应失败")
                    .expect("应恢复预置令牌")
                    .token,
                "persisted-for-reactivation"
            );
            let key = McpOAuthRegistry::storage_key("/project", "server", &settings(), issuer)
                .expect("测试 issuer 应生成密钥键");
            assert!(
                registry
                    .deactivate("/project", "server")
                    .await
                    .expect("停用应成功")
            );
            assert!(store.contains(&key), "停用不得删除密钥库令牌");
            assert!(old_provider.access_token().await.is_err());

            registry
                .register("/project", "server", settings())
                .await
                .expect("重新注册应成功");
            registry
                .configure("/project", "server", oauth_config(issuer, 60))
                .await
                .expect("相同 issuer 重新发现应成功");
            let new_provider = registry
                .auth_provider("/project", "server")
                .expect("新 Provider 应能创建");
            assert_eq!(
                new_provider
                    .access_token()
                    .await
                    .expect("新令牌读取不应失败")
                    .expect("应从密钥库恢复令牌")
                    .token,
                "persisted-for-reactivation"
            );
        })
        .await
        .expect("停用恢复测试不得超时");
    }

    /// 冷 Registry 只恢复密钥库令牌，不恢复旧进程内 pending 授权状态。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cold_registry_restores_token_but_not_pending_authorization() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let store = InMemoryOAuthSecretStore::new();
            let issuer = "https://auth.example.test";
            let stored = token_set(
                "cold-restored-access",
                Some("cold-restored-refresh"),
                Some(now_unix_seconds() + 3600),
            );
            seed_token(&store, issuer, &stored).await;
            let exchanger = FakeExchanger::immediate(token_set("unused", None, None));
            let sink = RecordingEventSink::default();
            let first_registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store.clone()),
                Arc::new(exchanger.clone()),
                Arc::new(sink.clone()),
            );
            install(&first_registry, oauth_config(issuer, 60))
                .await
                .expect("首个 Registry 配置应成功");
            let pending = first_registry
                .start("/project", "server", now_unix_seconds())
                .await
                .expect("首个 Registry 应能创建 pending");
            assert!(
                first_registry
                    .status("/project", "server")
                    .await
                    .expect("首个状态应可读取")
                    .operation_pending
            );

            let cold_registry = McpOAuthRegistry::with_dependencies(
                Arc::new(store),
                Arc::new(exchanger),
                Arc::new(RecordingEventSink::default()),
            );
            install(&cold_registry, oauth_config(issuer, 60))
                .await
                .expect("冷 Registry 配置应成功");
            let cold_status = cold_registry
                .status("/project", "server")
                .await
                .expect("冷 Registry 状态应可读取");
            assert_eq!(cold_status.status, OAuthStatus::Authorized);
            assert!(!cold_status.operation_pending);
            assert_eq!(
                cold_registry
                    .auth_provider("/project", "server")
                    .expect("冷 Provider 应能创建")
                    .access_token()
                    .await
                    .expect("冷令牌读取不应失败")
                    .expect("冷 Registry 应恢复令牌")
                    .token,
                "cold-restored-access"
            );
            // 清理首个进程内 listener，确认 pending 没有被写入共享密钥库。
            assert!(
                first_registry
                    .cancel("/project", "server")
                    .await
                    .expect("首个 pending 应能取消")
            );
            assert!(!pending.redirect_uri().is_empty());
        })
        .await
        .expect("冷 Registry 恢复测试不得超时");
    }
}
