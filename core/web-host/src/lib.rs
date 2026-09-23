//! KeenCode 本机 Web Host 的传输边界。
//!
//! 这个 crate 只负责 HTTP/WS 的安全边界、认证会话、静态文件、授权资源和上传暂存。
//! Session、Turn、Journal、Provider 与 Runtime 事实仍由 Host Core 负责；业务路由通过
//! 宿主注入，不在这里复制一套会话状态。

use axum::body::Body;
use axum::extract::{
    Extension, OriginalUri, Path as AxumPath, State, WebSocketUpgrade,
    ws::rejection::WebSocketUpgradeRejection,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use keencode_acp::ConnectionId;
use percent_encoding::percent_decode_str;
use rand::RngCore;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tower::limit::ConcurrencyLimitLayer;
use url::Url;

mod host_business;
mod server;

pub use host_business::{
    BoundedOutboundQueue, DeliveryCursor, DeliveryOutcome, HostBusinessError, HostBusinessFuture,
    HostBusinessRouter, HostConnectionContext, HostWsAdapter, HostWsConnection, OutboundEnvelope,
    OutboundEvent, OutboundGap, SnapshotCursor, SnapshotEnvelope, SnapshotReason, SnapshotRequest,
};
pub use server::{DEFAULT_GRACEFUL_SHUTDOWN_TIMEOUT, PeerConnectInfo, WebServerOwner};

/// 浏览器登录后使用的 HttpOnly Cookie 名称。
pub const SESSION_COOKIE_NAME: &str = "keencode_session";
/// 双提交 CSRF Cookie 名称。
pub const CSRF_COOKIE_NAME: &str = "keencode_csrf";
/// 状态变更必须携带的 CSRF Header 名称。
pub const CSRF_HEADER_NAME: &str = "X-KeenCode-CSRF";
/// 服务器拒绝在 URL 查询字符串中接收认证 Token。
pub const TOKEN_QUERY_PARAMETER: &str = "token";
/// 默认认证会话有效期。
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// 默认认证失败窗口。
pub const DEFAULT_AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// 默认窗口内允许的失败次数。
pub const DEFAULT_AUTH_FAILURE_LIMIT: usize = 5;
/// 默认单次 HTTP 请求正文上限。
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// 默认单个上传文件上限。
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 128 * 1024 * 1024;
/// 默认并发连接/请求队列上限。
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;

/// Web Host 错误。
#[derive(Debug, Error)]
pub enum WebError {
    #[error("Web Host 配置无效：{0}")]
    InvalidConfig(String),
    #[error("Web Host 未启用")]
    Disabled,
    #[error("Web Host 监听失败：{0}")]
    Bind(#[from] io::Error),
    #[error("认证失败")]
    InvalidCredentials,
    #[error("认证会话无效或已过期")]
    InvalidSession,
    #[error("认证失败次数过多，请稍后重试")]
    RateLimited { retry_after: Duration },
    #[error("Host Header 不被允许")]
    InvalidHost,
    #[error("Origin 不被允许")]
    InvalidOrigin,
    #[error("缺少 CSRF 校验")]
    InvalidCsrf,
    #[error("请求资源不存在或未授权")]
    ResourceNotFound,
    #[error("资源路径不安全")]
    UnsafePath,
    #[error("Range 不可满足")]
    InvalidRange,
    #[error("上传失败：{0}")]
    Upload(String),
    #[error("能力未授权：{0}")]
    CapabilityDenied(&'static str),
    #[error("内部 Web Host 错误：{0}")]
    Internal(String),
    #[error("系统凭据 provider 失败：{0}")]
    Credential(String),
}

impl WebError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidConfig(_) | Self::UnsafePath | Self::InvalidRange => {
                StatusCode::BAD_REQUEST
            }
            Self::Disabled => StatusCode::SERVICE_UNAVAILABLE,
            Self::Bind(_) | Self::Internal(_) | Self::Credential(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::InvalidCredentials | Self::InvalidSession => StatusCode::UNAUTHORIZED,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::InvalidHost | Self::InvalidOrigin | Self::InvalidCsrf => StatusCode::FORBIDDEN,
            Self::ResourceNotFound => StatusCode::NOT_FOUND,
            Self::Upload(_) => StatusCode::BAD_REQUEST,
            Self::CapabilityDenied(_) => StatusCode::FORBIDDEN,
        }
    }
}

/// Web Token 包装，Debug/Serialize 永远不回显秘密正文。
#[derive(Clone, Eq, PartialEq)]
pub struct WebToken(String);

impl WebToken {
    /// 从用户配置或 headless 环境变量读取 Token；此函数不写入任何文件。
    pub fn from_env(name: &str) -> Result<Self, WebError> {
        let value = std::env::var(name)
            .map_err(|_| WebError::InvalidConfig(format!("环境变量 {name} 未设置")))?;
        Self::try_from(value)
    }

    /// 返回 Token 的长度；不返回内容，便于诊断配置是否存在。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Token 是否为空；有效 Token 始终非空，此方法用于配置表单校验。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 仅供系统凭据 provider 在写入密钥库时消费 Token 正文。
    ///
    /// 调用方不得记录、序列化或返回闭包中的字符串；普通业务代码应继续只
    /// 使用 [`Self::len`] 和 [`Self::is_empty`]。该受限回调避免 transport 层
    /// 为了持久化而暴露一个可被随意复制的公开字符串引用。
    pub fn with_secret<R>(&self, consumer: impl FnOnce(&str) -> R) -> R {
        consumer(&self.0)
    }

    fn constant_time_matches(&self, candidate: &[u8]) -> bool {
        self.0.as_bytes().ct_eq(candidate).into()
    }
}

impl TryFrom<String> for WebToken {
    type Error = WebError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !(8..=512).contains(&value.len())
            || value
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(WebError::InvalidConfig(
                "Web Token 必须是 8 到 512 字节且不能包含空白或控制字符".to_owned(),
            ));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for WebToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebToken")
            .field("value", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for WebToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str("[REDACTED]")
    }
}

impl<'de> Deserialize<'de> for WebToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

/// Desktop/headless 注入的系统凭据边界。
///
/// `keencode-web` 只调用这两个操作，不直接依赖 Windows Credential Manager、
/// macOS Keychain 或 Linux Secret Service；provider 的具体实现属于宿主入口。
pub trait SystemCredentialProvider: Send + Sync {
    /// 读取当前 Web Host 长期 Token。
    fn load_web_token(&self) -> Result<WebToken, WebError>;
    /// 原子持久化新的 Web Host 长期 Token；失败时不得触发版本轮换。
    fn store_web_token(&self, token: &WebToken) -> Result<(), WebError>;
}

/// 固定端口、绑定地址、静态资源根及背压预算。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebHostConfig {
    /// Web 默认关闭；关闭时不创建监听器。
    pub enabled: bool,
    /// 监听的本机地址。`0.0.0.0` 仅应在用户明确开启局域网访问时使用。
    pub bind: IpAddr,
    /// 用户固定端口；禁止使用 0，冲突必须报错而不能随机换端口。
    pub port: u16,
    /// 用户配置的长期 Token；序列化时只输出 `[REDACTED]`。
    pub token: WebToken,
    /// 生产构建显式传入的静态资源根，不从源码目录推断。
    pub static_root: PathBuf,
    /// 与静态资源根分离的显式可写上传根；静态文件服务不会写入此目录。
    pub upload_root: PathBuf,
    /// 单个请求正文上限。
    pub max_request_bytes: usize,
    /// 单个上传文件上限。
    pub max_upload_bytes: u64,
    /// 并发请求/连接队列上限。
    pub max_connections: usize,
    /// 登录会话有效期秒数。
    pub session_ttl_secs: u64,
}

impl WebHostConfig {
    /// 构造启用 Web 的严格配置。
    pub fn new(
        bind: IpAddr,
        port: u16,
        token: WebToken,
        static_root: PathBuf,
        upload_root: PathBuf,
    ) -> Result<Self, WebError> {
        Self::new_with_roots(bind, port, token, static_root, upload_root)
    }

    /// 使用显式静态根和上传根构造启用 Web 的严格配置。
    pub fn new_with_roots(
        bind: IpAddr,
        port: u16,
        token: WebToken,
        static_root: PathBuf,
        upload_root: PathBuf,
    ) -> Result<Self, WebError> {
        let config = Self {
            enabled: true,
            bind,
            port,
            token,
            static_root,
            upload_root,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            session_ttl_secs: DEFAULT_SESSION_TTL.as_secs(),
        };
        config.validate()?;
        Ok(config)
    }

    /// 构造默认关闭的配置；关闭状态不要求静态资源目录存在。
    pub fn disabled(token: WebToken) -> Self {
        Self {
            enabled: false,
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 32123,
            token,
            static_root: PathBuf::new(),
            upload_root: PathBuf::new(),
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            session_ttl_secs: DEFAULT_SESSION_TTL.as_secs(),
        }
    }

    /// 修改背压预算并重新校验。
    pub fn with_limits(
        mut self,
        max_request_bytes: usize,
        max_upload_bytes: u64,
        max_connections: usize,
    ) -> Result<Self, WebError> {
        self.max_request_bytes = max_request_bytes;
        self.max_upload_bytes = max_upload_bytes;
        self.max_connections = max_connections;
        self.validate()?;
        Ok(self)
    }

    /// 设置会话有效期。
    pub fn with_session_ttl(mut self, ttl: Duration) -> Result<Self, WebError> {
        self.session_ttl_secs = ttl.as_secs();
        self.validate()?;
        Ok(self)
    }

    /// 校验固定端口及全部资源预算。
    pub fn validate(&self) -> Result<(), WebError> {
        if self.port == 0 {
            return Err(WebError::InvalidConfig(
                "Web 端口必须固定且不能为 0".to_owned(),
            ));
        }
        if self.max_request_bytes == 0
            || self.max_upload_bytes == 0
            || self.max_upload_bytes > usize::MAX as u64
            || self.max_connections == 0
            || self.session_ttl_secs == 0
        {
            return Err(WebError::InvalidConfig("Web 资源预算必须大于 0".to_owned()));
        }
        if self.enabled && self.static_root.as_os_str().is_empty() {
            return Err(WebError::InvalidConfig(
                "生产 Web 必须显式提供静态资源根目录".to_owned(),
            ));
        }
        if self.enabled && self.upload_root.as_os_str().is_empty() {
            return Err(WebError::InvalidConfig(
                "生产 Web 必须显式提供可写上传根目录".to_owned(),
            ));
        }
        Ok(())
    }

    fn session_ttl(&self) -> Duration {
        Duration::from_secs(self.session_ttl_secs)
    }
}

/// Web Host 运行状态；不包含 Token 或本地文件正文。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WebHostState {
    Disabled,
    Stopped,
    Running,
    Stopping,
    Failed,
}

/// Web Host 状态面板使用的安全快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebHostStatus {
    pub state: WebHostState,
    pub bind: IpAddr,
    pub port: u16,
    pub active_connections: usize,
    pub max_connections: usize,
    pub session_count: usize,
    pub token_version: u64,
}

/// Host 的静态状态和可撤销认证会话。
pub struct WebHost {
    config: WebHostConfig,
    state: Arc<WebState>,
    status: Arc<RwLock<WebHostStatus>>,
    router: Router,
}

impl fmt::Debug for WebHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebHost")
            .field("config", &self.config)
            .field("status", &self.status())
            .finish()
    }
}

impl WebHost {
    /// 按生产资源布局创建 Web Host；不会随机选择端口，也不会读取源码目录。
    pub fn new(config: WebHostConfig) -> Result<Self, WebError> {
        let host_policy = HostHeaderPolicy::for_bind(config.bind, config.port);
        Self::new_with_host_policy(config, host_policy)
    }

    /// 使用 Host 适配器计算好的严格 Host/Origin 白名单创建 Web Host。
    ///
    /// `0.0.0.0` 监听时，默认策略只包含 loopback 和绑定地址；需要手机通过
    /// 局域网地址访问时，Host 适配器必须把实际允许的 LAN 地址显式加入此策略。
    pub fn new_with_host_policy(
        config: WebHostConfig,
        host_policy: HostHeaderPolicy,
    ) -> Result<Self, WebError> {
        config.validate()?;
        let static_assets = if config.enabled {
            StaticAssetResolver::production(&config.static_root)?
        } else {
            StaticAssetResolver::disabled()
        };
        let auth = Arc::new(AuthService::new(
            config.token.clone(),
            config.session_ttl(),
            AuthFailureLimiter::default(),
        ));
        let resources = Arc::new(ResourceRegistry::new());
        let uploads = Arc::new(if config.enabled {
            if static_assets.overlaps(&config.upload_root) {
                return Err(WebError::InvalidConfig(
                    "静态资源根与上传根必须分离，避免上传文件被静态服务暴露".to_owned(),
                ));
            }
            let uploads = UploadStore::new(config.upload_root.clone(), config.max_upload_bytes)?;
            if static_assets.overlaps(uploads.root()) {
                return Err(WebError::InvalidConfig(
                    "静态资源根与上传根必须分离，避免上传文件被静态服务暴露".to_owned(),
                ));
            }
            uploads
        } else {
            UploadStore::disabled(config.max_upload_bytes)?
        });
        let state = Arc::new(WebState {
            auth,
            host_policy,
            static_assets: Arc::new(static_assets),
            resources,
            uploads,
            capabilities: CapabilityPolicy::for_mobile(),
            max_request_bytes: config.max_request_bytes,
        });
        let status = Arc::new(RwLock::new(WebHostStatus {
            state: if config.enabled {
                WebHostState::Stopped
            } else {
                WebHostState::Disabled
            },
            bind: config.bind,
            port: config.port,
            active_connections: 0,
            max_connections: config.max_connections,
            session_count: 0,
            token_version: 1,
        }));
        let router = build_router(
            Arc::clone(&state),
            config.max_request_bytes,
            config.max_connections,
            true,
        );
        Ok(Self {
            config,
            state,
            status,
            router,
        })
    }

    /// 返回可克隆的 Axum Router；业务 Host 可在外层 merge 自己的 ACP 路由。
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// 返回不占用 `/api/ws` 的 transport Router，供 Host Core 注入业务 WebSocket。
    ///
    /// 该 Router 仍保留认证、静态资源、上传和能力边界；业务 WebSocket handler
    /// 必须自行调用 [`WebState::validate_headers`]、[`WebState::authenticate`] 与
    /// [`WebState::require_capability`]。
    pub fn router_without_websocket(&self) -> Router {
        build_router(
            Arc::clone(&self.state),
            self.config.max_request_bytes,
            self.config.max_connections,
            false,
        )
    }

    /// 返回接入 Host Core ACP 路由的完整 Web Router。
    ///
    /// adapter 只接收已经认证的连接并持有连接级投递状态；Session、Journal 和
    /// Runtime 仍由 [`HostBusinessRouter`] 实现者负责。
    pub fn router_with_business(&self, adapter: Arc<HostWsAdapter>) -> Router {
        self.router_without_websocket()
            .route(
                "/api/ws",
                get(business_websocket_handler).with_state(Arc::clone(&self.state)),
            )
            .layer(Extension(adapter))
    }

    /// 启动真正覆盖 TCP/HTTP/WebSocket 生命周期的 server owner。
    pub fn start_server(self: &Arc<Self>, router: Router) -> Result<WebServerOwner, WebError> {
        WebServerOwner::start(Arc::clone(self), router)
    }

    /// 使用注入的系统凭据 provider 加载长期 Token，再创建 Web Host。
    ///
    /// provider 只负责 Desktop/headless 的系统密钥库接入；本 crate 不实现任何
    /// Credential Manager、Keychain 或 Secret Service 访问。
    pub fn new_with_credential_provider(
        mut config: WebHostConfig,
        provider: &dyn SystemCredentialProvider,
    ) -> Result<Self, WebError> {
        config.token = provider.load_web_token()?;
        Self::new(config)
    }

    /// 使用注入的系统凭据 provider 和显式 Host/Origin 策略创建 Web Host。
    pub fn new_with_host_policy_and_credential_provider(
        mut config: WebHostConfig,
        host_policy: HostHeaderPolicy,
        provider: &dyn SystemCredentialProvider,
    ) -> Result<Self, WebError> {
        config.token = provider.load_web_token()?;
        Self::new_with_host_policy(config, host_policy)
    }

    /// 返回 transport/auth/resource 共享状态，供 Host 注入业务路由。
    pub fn state(&self) -> Arc<WebState> {
        Arc::clone(&self.state)
    }

    /// 固定绑定用户选择的地址和端口；冲突直接返回，不回退随机端口。
    pub fn bind_listener(&self) -> Result<tokio::net::TcpListener, WebError> {
        if !self.config.enabled {
            return Err(WebError::Disabled);
        }
        let listener = TcpListener::bind(SocketAddr::new(self.config.bind, self.config.port))?;
        listener.set_nonblocking(true).map_err(WebError::Bind)?;
        let listener = tokio::net::TcpListener::from_std(listener).map_err(WebError::Bind)?;
        self.status
            .write()
            .expect("Web Host status lock should not be poisoned")
            .state = WebHostState::Running;
        Ok(listener)
    }

    /// 将 Host 标记为已停止；真正的连接关闭由 axum server 的 shutdown future 控制。
    pub fn stop(&self) {
        if let Ok(mut status) = self.status.write() {
            if status.state != WebHostState::Disabled {
                status.state = WebHostState::Stopped;
                status.active_connections = 0;
            }
        }
        self.state.auth.revoke_all();
    }

    /// 替换用户配置 Token 并撤销所有已有浏览器会话。
    pub fn rotate_token(&self, token: WebToken) -> Result<u64, WebError> {
        let version = self.state.auth.rotate_token(token)?;
        if let Ok(mut status) = self.status.write() {
            status.token_version = version;
            status.session_count = 0;
        }
        Ok(version)
    }

    /// 先持久化新 Token，成功后再递增版本并撤销全部旧会话。
    pub fn rotate_token_with_provider(
        &self,
        provider: &dyn SystemCredentialProvider,
        token: WebToken,
    ) -> Result<u64, WebError> {
        provider.store_web_token(&token)?;
        self.rotate_token(token)
    }

    /// 返回不含秘密的运行状态。
    pub fn status(&self) -> WebHostStatus {
        let mut status = self
            .status
            .read()
            .expect("Web Host status lock should not be poisoned")
            .clone();
        status.session_count = self.state.auth.session_count();
        status.token_version = self.state.auth.token_version();
        status
    }

    /// 更新活动连接数；由实际 accept/close 适配器调用并保持在上限内。
    pub fn set_active_connections(&self, active: usize) {
        if let Ok(mut status) = self.status.write() {
            status.active_connections = active.min(status.max_connections);
        }
    }

    pub(crate) fn connection_opened(&self) {
        if let Ok(mut status) = self.status.write() {
            status.active_connections = status
                .active_connections
                .saturating_add(1)
                .min(status.max_connections);
        }
    }

    pub(crate) fn connection_closed(&self) {
        if let Ok(mut status) = self.status.write() {
            status.active_connections = status.active_connections.saturating_sub(1);
        }
    }

    pub(crate) fn server_stopping(&self) {
        if let Ok(mut status) = self.status.write() {
            if status.state != WebHostState::Disabled {
                status.state = WebHostState::Stopping;
            }
        }
    }

    pub(crate) fn server_stopped(&self) {
        if let Ok(mut status) = self.status.write() {
            if status.state != WebHostState::Disabled {
                status.state = WebHostState::Stopped;
                status.active_connections = 0;
            }
        }
        self.state.auth.revoke_all();
    }

    pub(crate) fn server_failed(&self) {
        if let Ok(mut status) = self.status.write() {
            if status.state != WebHostState::Disabled {
                status.state = WebHostState::Failed;
            }
        }
    }
}

/// 给业务路由注入的共享 Web 状态；不持有 Runtime/Journal/Session 事实源。
pub struct WebState {
    auth: Arc<AuthService>,
    host_policy: HostHeaderPolicy,
    static_assets: Arc<StaticAssetResolver>,
    resources: Arc<ResourceRegistry>,
    uploads: Arc<UploadStore>,
    capabilities: CapabilityPolicy,
    max_request_bytes: usize,
}

impl fmt::Debug for WebState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebState")
            .field("host_policy", &self.host_policy)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl WebState {
    /// 认证服务只暴露非秘密操作，Token 正文不会从这里取回。
    pub fn auth(&self) -> &AuthService {
        &self.auth
    }

    /// 资源授权登记入口；客户端只能拿到 opaque Resource ID。
    pub fn resources(&self) -> &ResourceRegistry {
        &self.resources
    }

    /// 克隆资源索引的共享句柄，供 Web ACP adapter 在连接级授权后解析资源。
    pub fn resource_registry(&self) -> Arc<ResourceRegistry> {
        Arc::clone(&self.resources)
    }

    /// 上传暂存入口；客户端不能指定最终磁盘路径。
    pub fn uploads(&self) -> &UploadStore {
        &self.uploads
    }

    /// 校验通用 Host/Origin 头。
    pub fn validate_headers(
        &self,
        headers: &HeaderMap,
        require_origin: bool,
    ) -> Result<(), WebError> {
        self.host_policy.validate_headers(headers, require_origin)
    }

    /// 校验当前请求的登录会话。
    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        now: Instant,
    ) -> Result<AuthenticatedSession, WebError> {
        let cookies = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .ok_or(WebError::InvalidSession)?;
        self.auth.authenticate_cookie(cookies, now)
    }

    /// 状态变更必须同时通过 Origin 和双提交 CSRF。
    pub fn require_mutation(
        &self,
        headers: &HeaderMap,
        session: &AuthenticatedSession,
    ) -> Result<(), WebError> {
        self.host_policy.validate_headers(headers, true)?;
        let cookies = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .ok_or(WebError::InvalidCsrf)?;
        CsrfPolicy::verify(cookies, headers, session)
    }

    /// 移动端服务端能力白名单；UI 隐藏不是授权边界。
    pub fn require_capability(&self, capability: MobileCapability) -> Result<(), WebError> {
        self.capabilities.require(capability)
    }

    /// 当前移动客户端可以看到的能力列表。
    pub fn capabilities(&self) -> Vec<MobileCapability> {
        self.capabilities.list()
    }
}

/// 认证会话的非秘密返回值。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResult {
    pub expires_in_secs: u64,
    pub token_version: u64,
}

struct IssuedSession {
    result: LoginResult,
    session_id: String,
    csrf_token: String,
}

/// 已通过认证的会话；Cookie/CSRF 原文只在请求生命周期内存在。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedSession {
    pub session_id: String,
    csrf_token: String,
    pub token_version: u64,
}

impl AuthenticatedSession {
    fn csrf_token(&self) -> &str {
        &self.csrf_token
    }
}

#[derive(Clone, Debug)]
struct SessionRecord {
    csrf_token: String,
    expires_at: Instant,
    token_version: u64,
}

struct AuthInner {
    token: WebToken,
    token_version: u64,
    sessions: HashMap<String, SessionRecord>,
}

/// 可撤销、按 Token 版本隔离的浏览器会话服务。
pub struct AuthService {
    inner: Mutex<AuthInner>,
    ttl: Duration,
    failures: AuthFailureLimiter,
}

impl fmt::Debug for AuthService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthService")
            .field("ttl", &self.ttl)
            .field("token_version", &self.token_version())
            .field("session_count", &self.session_count())
            .finish()
    }
}

impl AuthService {
    pub fn new(token: WebToken, ttl: Duration, failures: AuthFailureLimiter) -> Self {
        Self {
            inner: Mutex::new(AuthInner {
                token,
                token_version: 1,
                sessions: HashMap::new(),
            }),
            ttl,
            failures,
        }
    }

    /// 用 Token 登录并生成随机 HttpOnly session 与 CSRF Cookie。
    pub fn login(
        &self,
        candidate: &str,
        rate_key: &str,
        now: Instant,
    ) -> Result<LoginResult, WebError> {
        Ok(self.login_issued(candidate, rate_key, now)?.result)
    }

    fn login_issued(
        &self,
        candidate: &str,
        rate_key: &str,
        now: Instant,
    ) -> Result<IssuedSession, WebError> {
        self.failures.check(rate_key, now)?;
        let mut inner = self.inner.lock().expect("auth lock should not be poisoned");
        purge_sessions(&mut inner.sessions, now);
        if !inner.token.constant_time_matches(candidate.as_bytes()) {
            drop(inner);
            self.failures.record_failure(rate_key, now);
            return Err(WebError::InvalidCredentials);
        }
        self.failures.reset(rate_key);
        let session_id = random_opaque_id();
        let csrf_token = random_opaque_id();
        let token_version = inner.token_version;
        inner.sessions.insert(
            session_id.clone(),
            SessionRecord {
                csrf_token: csrf_token.clone(),
                expires_at: now + self.ttl,
                token_version,
            },
        );
        Ok(IssuedSession {
            result: LoginResult {
                expires_in_secs: self.ttl.as_secs(),
                token_version,
            },
            session_id,
            csrf_token,
        })
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// 用固定 Cookie 名读取已登录会话；过期/旧 Token 版本自动拒绝。
    pub fn authenticate_cookie(
        &self,
        cookies: &str,
        now: Instant,
    ) -> Result<AuthenticatedSession, WebError> {
        let session_id =
            cookie_value(cookies, SESSION_COOKIE_NAME).ok_or(WebError::InvalidSession)?;
        let mut inner = self.inner.lock().expect("auth lock should not be poisoned");
        purge_sessions(&mut inner.sessions, now);
        let record = inner
            .sessions
            .get(session_id)
            .ok_or(WebError::InvalidSession)?;
        if record.token_version != inner.token_version || record.expires_at <= now {
            return Err(WebError::InvalidSession);
        }
        Ok(AuthenticatedSession {
            session_id: session_id.to_owned(),
            csrf_token: record.csrf_token.clone(),
            token_version: record.token_version,
        })
    }

    /// 注销一个会话。
    pub fn logout_cookie(&self, cookies: &str) {
        if let Some(session_id) = cookie_value(cookies, SESSION_COOKIE_NAME) {
            if let Ok(mut inner) = self.inner.lock() {
                inner.sessions.remove(session_id);
            }
        }
    }

    /// Token 修改时递增版本并撤销全部会话。
    pub fn rotate_token(&self, token: WebToken) -> Result<u64, WebError> {
        let mut inner = self.inner.lock().expect("auth lock should not be poisoned");
        inner.token = token;
        inner.token_version = inner.token_version.saturating_add(1).max(1);
        inner.sessions.clear();
        Ok(inner.token_version)
    }

    pub fn revoke_all(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.sessions.clear();
        }
    }

    pub fn token_version(&self) -> u64 {
        self.inner
            .lock()
            .expect("auth lock should not be poisoned")
            .token_version
    }

    pub fn session_count(&self) -> usize {
        self.inner
            .lock()
            .expect("auth lock should not be poisoned")
            .sessions
            .len()
    }
}

fn purge_sessions(sessions: &mut HashMap<String, SessionRecord>, now: Instant) {
    sessions.retain(|_, session| session.expires_at > now);
}

fn random_opaque_id() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 固定窗口的认证失败限流器；成功登录会清除当前来源的失败记录。
#[derive(Clone)]
pub struct AuthFailureLimiter {
    inner: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    limit: usize,
    window: Duration,
}

impl fmt::Debug for AuthFailureLimiter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthFailureLimiter")
            .field("limit", &self.limit)
            .field("window", &self.window)
            .finish()
    }
}

impl Default for AuthFailureLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_AUTH_FAILURE_LIMIT, DEFAULT_AUTH_FAILURE_WINDOW)
    }
}

impl AuthFailureLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        assert!(limit > 0, "auth failure limit must be positive");
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            limit,
            window,
        }
    }

    pub fn check(&self, key: &str, now: Instant) -> Result<(), WebError> {
        let mut inner = self
            .inner
            .lock()
            .expect("rate limiter lock should not be poisoned");
        let queue = inner.entry(key.to_owned()).or_default();
        trim_failure_queue(queue, now, self.window);
        if queue.len() >= self.limit {
            let retry_after = queue
                .front()
                .and_then(|started| started.checked_add(self.window))
                .map(|expires| expires.saturating_duration_since(now))
                .unwrap_or(self.window);
            return Err(WebError::RateLimited { retry_after });
        }
        Ok(())
    }

    pub fn record_failure(&self, key: &str, now: Instant) {
        if let Ok(mut inner) = self.inner.lock() {
            let queue = inner.entry(key.to_owned()).or_default();
            trim_failure_queue(queue, now, self.window);
            queue.push_back(now);
        }
    }

    pub fn reset(&self, key: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.remove(key);
        }
    }
}

fn trim_failure_queue(queue: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while queue
        .front()
        .is_some_and(|started| now.saturating_duration_since(*started) >= window)
    {
        queue.pop_front();
    }
}

/// Host/Origin 白名单；不信任 X-Forwarded-*，防止局域网 DNS rebinding/CORS 绕过。
#[derive(Clone, Debug)]
pub struct HostHeaderPolicy {
    allowed_hosts: BTreeSet<String>,
    allowed_origins: BTreeSet<String>,
}

impl HostHeaderPolicy {
    pub fn new<I, J>(allowed_hosts: I, allowed_origins: J) -> Result<Self, WebError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
        J: IntoIterator,
        J::Item: AsRef<str>,
    {
        let allowed_hosts = allowed_hosts
            .into_iter()
            .map(|host| normalize_host(host.as_ref()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let allowed_origins = allowed_origins
            .into_iter()
            .map(|origin| normalize_origin(origin.as_ref()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if allowed_hosts.is_empty() || allowed_origins.is_empty() {
            return Err(WebError::InvalidConfig(
                "Host 和 Origin 白名单不能为空".to_owned(),
            ));
        }
        Ok(Self {
            allowed_hosts,
            allowed_origins,
        })
    }

    pub fn for_bind(bind: IpAddr, port: u16) -> Self {
        let mut hosts = BTreeSet::new();
        let mut origins = BTreeSet::new();
        let bind_text = format_ip(bind);
        let host_candidates = if bind.is_unspecified() {
            vec![
                bind_text,
                "localhost".to_owned(),
                "127.0.0.1".to_owned(),
                "[::1]".to_owned(),
            ]
        } else {
            vec![bind_text]
        };
        for host in host_candidates {
            let host_with_port = format_host_port(&host, port);
            hosts.insert(host_with_port.clone());
            origins.insert(format!("http://{host_with_port}"));
        }
        Self {
            allowed_hosts: hosts,
            allowed_origins: origins,
        }
    }

    pub fn validate_headers(
        &self,
        headers: &HeaderMap,
        require_origin: bool,
    ) -> Result<(), WebError> {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .ok_or(WebError::InvalidHost)?;
        let normalized_host = normalize_host(host).map_err(|_| WebError::InvalidHost)?;
        if !self.allowed_hosts.contains(&normalized_host) {
            return Err(WebError::InvalidHost);
        }
        match headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
        {
            Some(origin) => {
                let normalized_origin =
                    normalize_origin(origin).map_err(|_| WebError::InvalidOrigin)?;
                if !self.allowed_origins.contains(&normalized_origin) {
                    return Err(WebError::InvalidOrigin);
                }
            }
            None if require_origin => return Err(WebError::InvalidOrigin),
            None => {}
        }
        Ok(())
    }

    pub fn allowed_hosts(&self) -> &BTreeSet<String> {
        &self.allowed_hosts
    }

    pub fn allowed_origins(&self) -> &BTreeSet<String> {
        &self.allowed_origins
    }
}

fn format_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

fn format_host_port(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

fn normalize_host(value: &str) -> Result<String, WebError> {
    let value = value.trim();
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(WebError::InvalidHost);
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let close = rest.find(']').ok_or(WebError::InvalidHost)?;
        let host = &rest[..close];
        let remainder = &rest[close + 1..];
        let port = remainder
            .strip_prefix(':')
            .map(|port| port.parse::<u16>().map_err(|_| WebError::InvalidHost))
            .transpose()?;
        (format!("[{host}]"), port)
    } else if value.matches(':').count() == 1 {
        let (host, port) = value.rsplit_once(':').ok_or(WebError::InvalidHost)?;
        (
            host.to_owned(),
            Some(port.parse::<u16>().map_err(|_| WebError::InvalidHost)?),
        )
    } else if value.contains(':') {
        return Err(WebError::InvalidHost);
    } else {
        (value.to_owned(), None)
    };
    if host.is_empty() || host.contains('/') || host.contains('\\') || host.contains('@') {
        return Err(WebError::InvalidHost);
    }
    Ok(match port {
        Some(port) => format!("{}:{port}", host.to_ascii_lowercase()),
        None => host.to_ascii_lowercase(),
    })
}

fn normalize_origin(value: &str) -> Result<String, WebError> {
    let parsed = Url::parse(value).map_err(|_| WebError::InvalidOrigin)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err(WebError::InvalidOrigin);
    }
    let host = parsed.host().ok_or(WebError::InvalidOrigin)?;
    let host = match host {
        url::Host::Domain(host) => host.to_owned(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => format!("[{ip}]"),
    };
    let port = parsed.port().or_else(|| match parsed.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    });
    let default_port = (parsed.scheme() == "http" && port == Some(80))
        || (parsed.scheme() == "https" && port == Some(443));
    let authority = if default_port {
        host
    } else {
        format!("{host}:{}", port.ok_or(WebError::InvalidOrigin)?)
    };
    Ok(format!(
        "{}://{}",
        parsed.scheme().to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}

/// CSRF 双提交校验；Session Cookie 本身保持 HttpOnly，CSRF Cookie 可被浏览器脚本读取。
pub struct CsrfPolicy;

impl CsrfPolicy {
    pub fn verify(
        cookies: &str,
        headers: &HeaderMap,
        session: &AuthenticatedSession,
    ) -> Result<(), WebError> {
        let cookie = cookie_value(cookies, CSRF_COOKIE_NAME).ok_or(WebError::InvalidCsrf)?;
        let header_value = headers
            .get(CSRF_HEADER_NAME)
            .and_then(|value| value.to_str().ok())
            .ok_or(WebError::InvalidCsrf)?;
        if cookie.as_bytes().ct_eq(header_value.as_bytes()).into()
            && cookie
                .as_bytes()
                .ct_eq(session.csrf_token().as_bytes())
                .into()
        {
            Ok(())
        } else {
            Err(WebError::InvalidCsrf)
        }
    }
}

fn cookie_value<'a>(cookies: &'a str, name: &str) -> Option<&'a str> {
    cookies.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

fn cookie_header(name: &str, value: &str, http_only: bool, max_age: Duration) -> HeaderValue {
    let mut cookie = format!(
        "{name}={value}; Path=/; SameSite=Lax; Max-Age={}",
        max_age.as_secs()
    );
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    HeaderValue::from_str(&cookie).expect("opaque cookie values must be valid headers")
}

fn expired_cookie_header(name: &str, http_only: bool) -> HeaderValue {
    let mut cookie = format!("{name}=; Path=/; SameSite=Lax; Max-Age=0");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    HeaderValue::from_str(&cookie).expect("fixed cookie values must be valid headers")
}

/// 单个授权资源的 opaque ID；客户端不能从中推导本地路径。
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ResourceId {
    type Error = WebError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if !(16..=128).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(WebError::ResourceNotFound);
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone)]
struct ResourceRecord {
    owner_session: String,
    path: PathBuf,
    root: PathBuf,
    content_type: String,
    file_name: String,
}

impl fmt::Debug for ResourceRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceRecord")
            .field("owner_session", &self.owner_session)
            .field("path", &"[LOCAL_PATH]")
            .field("root", &"[LOCAL_PATH]")
            .field("content_type", &self.content_type)
            .field("file_name", &self.file_name)
            .finish()
    }
}

/// Host capability 登记的资源索引；没有从客户端路径注册的公共接口。
pub struct ResourceRegistry {
    entries: Mutex<HashMap<ResourceId, ResourceRecord>>,
}

impl fmt::Debug for ResourceRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceRegistry")
            .field(
                "entry_count",
                &self
                    .entries
                    .lock()
                    .map(|entries| entries.len())
                    .unwrap_or(0),
            )
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct AuthorizedResource {
    pub id: ResourceId,
    pub path: PathBuf,
    pub length: u64,
    pub content_type: String,
    pub file_name: String,
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 仅 Host/Runtime 调用：登记一个已经由能力边界授权的本地文件。
    pub fn register(
        &self,
        owner_session: &str,
        root: impl AsRef<Path>,
        path: impl AsRef<Path>,
        content_type: impl Into<String>,
        file_name: impl AsRef<str>,
    ) -> Result<ResourceId, WebError> {
        let root = canonical_directory(root.as_ref())?;
        let path = canonical_file(path.as_ref())?;
        if !path.starts_with(&root) {
            return Err(WebError::UnsafePath);
        }
        let file_name = safe_display_name(file_name.as_ref()).ok_or(WebError::UnsafePath)?;
        let id = ResourceId(random_opaque_id());
        let record = ResourceRecord {
            owner_session: owner_session.to_owned(),
            path,
            root,
            content_type: content_type.into(),
            file_name,
        };
        self.entries
            .lock()
            .map_err(|_| WebError::Internal("resource registry lock poisoned".to_owned()))?
            .insert(id.clone(), record);
        Ok(id)
    }

    pub fn authorize(&self, owner_session: &str, id: &str) -> Result<AuthorizedResource, WebError> {
        let id = ResourceId::try_from(id)?;
        let entries = self
            .entries
            .lock()
            .map_err(|_| WebError::Internal("resource registry lock poisoned".to_owned()))?;
        let record = entries.get(&id).ok_or(WebError::ResourceNotFound)?;
        if record.owner_session != owner_session {
            return Err(WebError::ResourceNotFound);
        }
        let path = canonical_file(&record.path)?;
        if !path.starts_with(&record.root) {
            return Err(WebError::ResourceNotFound);
        }
        let length = fs::metadata(&path)
            .map_err(|_| WebError::ResourceNotFound)?
            .len();
        Ok(AuthorizedResource {
            id,
            path,
            length,
            content_type: record.content_type.clone(),
            file_name: record.file_name.clone(),
        })
    }

    pub fn revoke_session(&self, owner_session: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|_, record| record.owner_session != owner_session);
        }
    }
}

impl Default for ResourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, WebError> {
    let path = fs::canonicalize(path).map_err(|_| WebError::UnsafePath)?;
    if fs::metadata(&path)
        .map_err(|_| WebError::UnsafePath)?
        .is_dir()
    {
        Ok(path)
    } else {
        Err(WebError::UnsafePath)
    }
}

fn canonical_file(path: &Path) -> Result<PathBuf, WebError> {
    let path = fs::canonicalize(path).map_err(|_| WebError::ResourceNotFound)?;
    if fs::metadata(&path)
        .map_err(|_| WebError::ResourceNotFound)?
        .is_file()
    {
        Ok(path)
    } else {
        Err(WebError::ResourceNotFound)
    }
}

fn safe_display_name(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 255
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        // Windows Alternate Data Streams use `:` as a path separator.
        || value.contains(':')
        || value.chars().any(|ch| ch.is_control())
    {
        None
    } else {
        Some(value.to_owned())
    }
}

/// 单一 Range 解析结果；多 Range 被明确拒绝，避免拼接响应复杂度和缓存歧义。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(self) -> bool {
        self.start > self.end
    }
}

pub fn parse_single_range(value: Option<&str>, size: u64) -> Result<Option<ByteRange>, WebError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.strip_prefix("bytes=").ok_or(WebError::InvalidRange)?;
    if value.contains(',') || size == 0 {
        return Err(WebError::InvalidRange);
    }
    let (start, end) = value.split_once('-').ok_or(WebError::InvalidRange)?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| WebError::InvalidRange)?;
        if suffix == 0 {
            return Err(WebError::InvalidRange);
        }
        let length = suffix.min(size);
        return Ok(Some(ByteRange {
            start: size - length,
            end: size - 1,
        }));
    }
    let start = start.parse::<u64>().map_err(|_| WebError::InvalidRange)?;
    if start >= size {
        return Err(WebError::InvalidRange);
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>()
            .map_err(|_| WebError::InvalidRange)?
            .min(size - 1)
    };
    if end < start {
        return Err(WebError::InvalidRange);
    }
    Ok(Some(ByteRange { start, end }))
}

/// 上传暂存状态；最终文件名和路径由服务器生成，客户端只能提交展示名称。
pub struct UploadStore {
    root: PathBuf,
    max_bytes: u64,
    enabled: bool,
    entries: Mutex<HashMap<String, UploadEntry>>,
}

struct UploadEntry {
    owner_session: String,
    display_name: String,
    content_type: String,
    temporary_path: PathBuf,
    file: File,
    length: u64,
    expected_length: Option<u64>,
    created_at: Instant,
}

/// 已完成上传的服务器元数据；路径仍只在服务端内部流转。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadedResource {
    pub path: PathBuf,
    pub file_name: String,
    pub content_type: String,
    pub length: u64,
}

impl fmt::Debug for UploadStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UploadStore")
            .field("root", &"[LOCAL_PATH]")
            .field("max_bytes", &self.max_bytes)
            .field("enabled", &self.enabled)
            .field(
                "entry_count",
                &self
                    .entries
                    .lock()
                    .map(|entries| entries.len())
                    .unwrap_or(0),
            )
            .finish()
    }
}

impl UploadStore {
    pub fn new(root: PathBuf, max_bytes: u64) -> Result<Self, WebError> {
        if max_bytes == 0 {
            return Err(WebError::InvalidConfig("上传上限必须大于 0".to_owned()));
        }
        fs::create_dir_all(&root).map_err(|error| WebError::Upload(error.to_string()))?;
        let root =
            canonical_directory(&root).map_err(|_| WebError::Upload("上传目录无效".to_owned()))?;
        Ok(Self {
            root,
            max_bytes,
            enabled: true,
            entries: Mutex::new(HashMap::new()),
        })
    }

    /// 构造不创建任何目录的关闭态上传存储；Web Host 未启用时使用此值。
    pub fn disabled(max_bytes: u64) -> Result<Self, WebError> {
        if max_bytes == 0 {
            return Err(WebError::InvalidConfig("上传上限必须大于 0".to_owned()));
        }
        Ok(Self {
            root: PathBuf::new(),
            max_bytes,
            enabled: false,
            entries: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn begin(
        &self,
        owner_session: &str,
        display_name: &str,
        expected_length: Option<u64>,
        now: Instant,
    ) -> Result<String, WebError> {
        if !self.enabled {
            return Err(WebError::Disabled);
        }
        let display_name = safe_display_name(display_name)
            .ok_or_else(|| WebError::Upload("文件名必须是单一安全名称".to_owned()))?;
        if expected_length.is_some_and(|length| length > self.max_bytes) {
            return Err(WebError::Upload("上传文件超过大小上限".to_owned()));
        }
        let id = random_opaque_id();
        let temporary_path = self.root.join(format!(".{id}.part"));
        let file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|error| WebError::Upload(error.to_string()))?;
        self.entries
            .lock()
            .map_err(|_| WebError::Internal("upload store lock poisoned".to_owned()))?
            .insert(
                id.clone(),
                UploadEntry {
                    owner_session: owner_session.to_owned(),
                    content_type: content_type_for_path(Path::new(&display_name)),
                    display_name,
                    temporary_path,
                    file,
                    length: 0,
                    expected_length,
                    created_at: now,
                },
            );
        Ok(id)
    }

    pub fn write_chunk(
        &self,
        owner_session: &str,
        upload_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<u64, WebError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| WebError::Internal("upload store lock poisoned".to_owned()))?;
        let entry = entries
            .get_mut(upload_id)
            .ok_or_else(|| WebError::Upload("上传任务不存在或已中断".to_owned()))?;
        if entry.owner_session != owner_session || entry.length != offset {
            return Err(WebError::Upload("上传偏移不连续或会话不匹配".to_owned()));
        }
        let next = entry.length.saturating_add(bytes.len() as u64);
        if next > self.max_bytes
            || entry
                .expected_length
                .is_some_and(|expected| next > expected)
        {
            return Err(WebError::Upload("上传文件超过大小上限".to_owned()));
        }
        entry
            .file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| entry.file.write_all(bytes))
            .map_err(|error| WebError::Upload(error.to_string()))?;
        entry.length = next;
        Ok(next)
    }

    pub fn finish(&self, owner_session: &str, upload_id: &str) -> Result<PathBuf, WebError> {
        Ok(self.finish_resource(owner_session, upload_id)?.path)
    }

    /// 完成上传并保留原始展示名称和服务器推断的 MIME 元数据。
    pub fn finish_resource(
        &self,
        owner_session: &str,
        upload_id: &str,
    ) -> Result<UploadedResource, WebError> {
        let entry = {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| WebError::Internal("upload store lock poisoned".to_owned()))?;
            let entry = entries
                .get(upload_id)
                .ok_or_else(|| WebError::Upload("上传任务不存在或已中断".to_owned()))?;
            if entry.owner_session != owner_session {
                return Err(WebError::Upload("上传任务不属于当前会话".to_owned()));
            }
            let entry = entries
                .remove(upload_id)
                .expect("upload entry exists while holding the store lock");
            if entry
                .expected_length
                .is_some_and(|expected| expected != entry.length)
            {
                let temporary_path = entry.temporary_path.clone();
                // Windows 不允许删除仍被打开的临时文件；先关闭句柄再清理。
                drop(entry.file);
                let _ = fs::remove_file(temporary_path);
                return Err(WebError::Upload("上传长度与声明不一致".to_owned()));
            }
            entry
        };
        if let Err(error) = entry.file.sync_all() {
            let temporary_path = entry.temporary_path.clone();
            drop(entry.file);
            let _ = fs::remove_file(temporary_path);
            return Err(WebError::Upload(error.to_string()));
        }
        drop(entry.file);
        let final_path = self
            .root
            .join(format!("{upload_id}-{}", entry.display_name));
        if let Err(error) = fs::rename(&entry.temporary_path, &final_path) {
            let _ = fs::remove_file(&entry.temporary_path);
            return Err(WebError::Upload(error.to_string()));
        }
        Ok(UploadedResource {
            path: final_path,
            file_name: entry.display_name,
            content_type: entry.content_type,
            length: entry.length,
        })
    }

    pub fn abort(&self, owner_session: &str, upload_id: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            if let Some(entry) = entries
                .get(upload_id)
                .filter(|entry| entry.owner_session == owner_session)
            {
                let temporary_path = entry.temporary_path.clone();
                entries.remove(upload_id);
                let _ = fs::remove_file(temporary_path);
            }
        }
    }

    pub fn cleanup_expired(&self, now: Instant, max_age: Duration) -> usize {
        let mut expired = Vec::new();
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|id, entry| {
                let keep = now.saturating_duration_since(entry.created_at) < max_age;
                if !keep {
                    expired.push((id.clone(), entry.temporary_path.clone()));
                }
                keep
            });
        }
        for (_, path) in &expired {
            let _ = fs::remove_file(path);
        }
        expired.len()
    }
}

impl Drop for UploadStore {
    fn drop(&mut self) {
        if let Ok(entries) = self.entries.get_mut() {
            for entry in entries.values() {
                let _ = fs::remove_file(&entry.temporary_path);
            }
        }
    }
}

/// 生产静态资源解析器；开发资源根必须由调用方显式选择。
#[derive(Clone, Debug)]
pub struct StaticAssetResolver {
    root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct StaticAsset {
    pub path: PathBuf,
    pub content_type: String,
    pub length: u64,
}

impl StaticAssetResolver {
    pub fn production(root: impl AsRef<Path>) -> Result<Self, WebError> {
        Ok(Self {
            root: Some(canonical_directory(root.as_ref())?),
        })
    }

    pub fn development(root: impl AsRef<Path>) -> Result<Self, WebError> {
        Self::production(root)
    }

    pub fn disabled() -> Self {
        Self { root: None }
    }

    fn overlaps(&self, other: &Path) -> bool {
        let Some(root) = &self.root else {
            return false;
        };
        paths_overlap(root, other)
    }

    pub fn resolve(&self, url_path: &str) -> Result<StaticAsset, WebError> {
        let root = self.root.as_ref().ok_or(WebError::Disabled)?;
        let relative = safe_relative_url_path(url_path)?;
        let candidate = root.join(&relative);
        let candidate = if candidate.is_dir() {
            candidate.join("index.html")
        } else {
            candidate
        };
        let candidate = canonical_file(&candidate).map_err(|_| WebError::ResourceNotFound)?;
        if !candidate.starts_with(root) {
            return Err(WebError::UnsafePath);
        }
        let length = fs::metadata(&candidate)
            .map_err(|_| WebError::ResourceNotFound)?
            .len();
        Ok(StaticAsset {
            content_type: content_type_for_path(&candidate),
            path: candidate,
            length,
        })
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = normalized_path_for_overlap(left);
    let right = normalized_path_for_overlap(right);
    left == right || left.starts_with(&right) || right.starts_with(&left)
}

fn normalized_path_for_overlap(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_owned())
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return canonical;
    }
    let mut existing = absolute.clone();
    let mut missing = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        missing.push(name.to_owned());
        if !existing.pop() {
            break;
        }
    }
    let mut normalized = fs::canonicalize(&existing).unwrap_or(existing);
    for component in missing.iter().rev() {
        normalized.push(component);
    }
    normalized
}

fn safe_relative_url_path(url_path: &str) -> Result<PathBuf, WebError> {
    let decoded = percent_decode_str(url_path)
        .decode_utf8()
        .map_err(|_| WebError::UnsafePath)?;
    let value = decoded.strip_prefix('/').unwrap_or(&decoded);
    if value.is_empty() {
        return Ok(PathBuf::from("index.html"));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_str().ok_or(WebError::UnsafePath)?;
                if text.is_empty()
                    || text.contains('\\')
                    // Reject drive/ADS syntax even when the host runs on Unix;
                    // the same static bundle can otherwise be unsafe on Windows.
                    || text.contains(':')
                    || text.chars().any(|ch| ch.is_control())
                {
                    return Err(WebError::UnsafePath);
                }
                relative.push(part);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return Err(WebError::UnsafePath),
        }
    }
    if relative.as_os_str().is_empty() {
        relative.push("index.html");
    }
    Ok(relative)
}

fn content_type_for_path(path: &Path) -> String {
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let mime = match extension.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    };
    mime.to_owned()
}

/// 移动端服务端能力白名单；Terminal 与 Host 管理能力默认永远拒绝。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MobileCapability {
    ProjectRead,
    SessionRead,
    SessionWrite,
    QueueWrite,
    Steer,
    Stop,
    Elicitation,
    ToolRead,
    ChildStatus,
    DiffRead,
    AttachmentUpload,
    AttachmentRead,
    Terminal,
    HostAdmin,
}

#[derive(Clone, Debug)]
pub struct CapabilityPolicy {
    allowed: BTreeSet<MobileCapability>,
}

impl CapabilityPolicy {
    pub fn for_mobile() -> Self {
        Self {
            allowed: [
                MobileCapability::ProjectRead,
                MobileCapability::SessionRead,
                MobileCapability::SessionWrite,
                MobileCapability::QueueWrite,
                MobileCapability::Steer,
                MobileCapability::Stop,
                MobileCapability::Elicitation,
                MobileCapability::ToolRead,
                MobileCapability::ChildStatus,
                MobileCapability::DiffRead,
                MobileCapability::AttachmentUpload,
                MobileCapability::AttachmentRead,
            ]
            .into_iter()
            .collect(),
        }
    }

    pub fn require(&self, capability: MobileCapability) -> Result<(), WebError> {
        if self.allowed.contains(&capability) {
            Ok(())
        } else {
            Err(WebError::CapabilityDenied(capability_name(capability)))
        }
    }

    pub fn list(&self) -> Vec<MobileCapability> {
        self.allowed.iter().copied().collect()
    }
}

fn capability_name(capability: MobileCapability) -> &'static str {
    match capability {
        MobileCapability::ProjectRead => "projectRead",
        MobileCapability::SessionRead => "sessionRead",
        MobileCapability::SessionWrite => "sessionWrite",
        MobileCapability::QueueWrite => "queueWrite",
        MobileCapability::Steer => "steer",
        MobileCapability::Stop => "stop",
        MobileCapability::Elicitation => "elicitation",
        MobileCapability::ToolRead => "toolRead",
        MobileCapability::ChildStatus => "childStatus",
        MobileCapability::DiffRead => "diffRead",
        MobileCapability::AttachmentUpload => "attachmentUpload",
        MobileCapability::AttachmentRead => "attachmentRead",
        MobileCapability::Terminal => "terminal",
        MobileCapability::HostAdmin => "hostAdmin",
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest {
    token: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    authenticated: bool,
    token_version: u64,
    expires_in_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadStartRequest {
    file_name: String,
    expected_length: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadStartResponse {
    upload_id: String,
    max_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadFinishResponse {
    resource_id: ResourceId,
    file_name: String,
    content_type: String,
    size: u64,
}

#[derive(Debug)]
struct ApiError(WebError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let error = self.0;
        let status = error.status_code();
        let mut headers = HeaderMap::new();
        if let WebError::RateLimited { retry_after } = &error {
            let seconds = retry_after.as_secs().max(1).to_string();
            if let Ok(value) = HeaderValue::from_str(&seconds) {
                headers.insert(header::RETRY_AFTER, value);
            }
        }
        let body = Json(serde_json::json!({
            "error": error_code(&error),
            "message": public_error_message(&error),
        }));
        (status, headers, body).into_response()
    }
}

fn public_error_message(error: &WebError) -> &'static str {
    match error {
        WebError::InvalidConfig(_) => "Web Host 配置无效",
        WebError::Bind(_) => "Web Host 监听失败",
        WebError::Internal(_) => "Web Host 内部错误",
        WebError::Upload(_) => "上传失败",
        WebError::Disabled => "Web Host 未启用",
        WebError::InvalidCredentials => "认证失败",
        WebError::InvalidSession => "认证会话无效或已过期",
        WebError::RateLimited { .. } => "认证失败次数过多，请稍后重试",
        WebError::InvalidHost => "Host Header 不被允许",
        WebError::InvalidOrigin => "Origin 不被允许",
        WebError::InvalidCsrf => "缺少 CSRF 校验",
        WebError::ResourceNotFound => "请求资源不存在或未授权",
        WebError::UnsafePath => "资源路径不安全",
        WebError::InvalidRange => "Range 不可满足",
        WebError::CapabilityDenied(_) => "能力未授权",
        WebError::Credential(_) => "系统凭据 provider 失败",
    }
}

fn error_code(error: &WebError) -> &'static str {
    match error {
        WebError::InvalidCredentials => "invalid_credentials",
        WebError::InvalidSession => "invalid_session",
        WebError::RateLimited { .. } => "rate_limited",
        WebError::InvalidHost => "invalid_host",
        WebError::InvalidOrigin => "invalid_origin",
        WebError::InvalidCsrf => "invalid_csrf",
        WebError::CapabilityDenied(_) => "capability_denied",
        WebError::ResourceNotFound => "resource_not_found",
        WebError::InvalidRange => "invalid_range",
        WebError::Upload(_) => "upload_error",
        WebError::Disabled => "web_disabled",
        _ => "web_error",
    }
}

fn build_router(
    state: Arc<WebState>,
    max_request_bytes: usize,
    max_connections: usize,
    include_websocket: bool,
) -> Router {
    let router = Router::new()
        .route("/api/auth/login", post(login_handler))
        .route("/api/auth/logout", post(logout_handler))
        .route("/api/auth/session", get(session_handler))
        .route("/api/capabilities", get(capabilities_handler))
        .route(
            "/api/resources/{id}",
            get(resource_handler).head(resource_handler),
        )
        .route("/api/uploads/start", post(upload_start_handler))
        .route(
            "/api/uploads/{id}",
            put(upload_chunk_handler).delete(upload_abort_handler),
        )
        .route("/api/uploads/{id}/complete", post(upload_complete_handler))
        .route("/api/terminal", any(denied_terminal_handler))
        .route("/api/host", any(denied_host_admin_handler));
    let router = if include_websocket {
        router.route("/api/ws", get(websocket_handler))
    } else {
        router
    };
    // 单次整包上传的声明上限是 max_upload_bytes；DefaultBodyLimit 只对该
    // 路由放宽到两者较大值（route 级 layer 后应用，覆盖全局默认），其余
    // API 仍受 max_request_bytes 约束。
    let upload_body_limit =
        max_request_bytes.max(usize::try_from(state.uploads.max_bytes).unwrap_or(usize::MAX));
    let router = router.route(
        "/api/uploads",
        post(single_upload_handler).layer(axum::extract::DefaultBodyLimit::max(upload_body_limit)),
    );
    router
        .fallback(static_handler)
        .layer(axum::extract::DefaultBodyLimit::max(max_request_bytes))
        .layer(ConcurrencyLimitLayer::new(max_connections))
        .with_state(state)
}

fn rate_key(peer: Option<Extension<crate::PeerConnectInfo>>) -> String {
    // 只读取服务器注入的对端地址 extension，不接受客户端可伪造的普通请求头。
    peer.map(|Extension(PeerConnectInfo(address))| address.to_string())
        .unwrap_or_else(|| "unknown-peer".to_owned())
}

fn require_host(
    state: &WebState,
    headers: &HeaderMap,
    require_origin: bool,
) -> Result<(), ApiError> {
    state
        .validate_headers(headers, require_origin)
        .map_err(ApiError)
}

fn require_session(
    state: &WebState,
    headers: &HeaderMap,
) -> Result<AuthenticatedSession, ApiError> {
    state
        .authenticate(headers, Instant::now())
        .map_err(ApiError)
}

async fn login_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    peer: Option<Extension<crate::PeerConnectInfo>>,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    require_host(&state, &headers, false)?;
    let rate_key = rate_key(peer);
    let issued = state
        .auth
        .login_issued(&body.token, &rate_key, Instant::now())
        .map_err(ApiError)?;
    // 直接使用本次登录生成的会话值，避免并发登录时从 HashMap 取到其他会话。
    let mut response = Json(issued.result).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie_header(
            SESSION_COOKIE_NAME,
            &issued.session_id,
            true,
            state.auth.ttl(),
        ),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie_header(
            CSRF_COOKIE_NAME,
            &issued.csrf_token,
            false,
            state.auth.ttl(),
        ),
    );
    Ok(response)
}

async fn logout_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_host(&state, &headers, true)?;
    let session = require_session(&state, &headers)?;
    state
        .require_mutation(&headers, &session)
        .map_err(ApiError)?;
    if let Some(cookies) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        state.auth.logout_cookie(cookies);
        state.resources.revoke_session(&session.session_id);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        expired_cookie_header(SESSION_COOKIE_NAME, true),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        expired_cookie_header(CSRF_COOKIE_NAME, false),
    );
    Ok(response)
}

async fn session_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, ApiError> {
    require_host(&state, &headers, false)?;
    let session = require_session(&state, &headers)?;
    Ok(Json(SessionResponse {
        authenticated: true,
        token_version: session.token_version,
        expires_in_secs: state.auth.ttl().as_secs(),
    }))
}

async fn capabilities_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<MobileCapability>>, ApiError> {
    require_host(&state, &headers, false)?;
    let _ = require_session(&state, &headers)?;
    Ok(Json(state.capabilities()))
}

async fn resource_handler(
    State(state): State<Arc<WebState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response, ApiError> {
    require_host(&state, &headers, false)?;
    let session = require_session(&state, &headers)?;
    state
        .require_capability(MobileCapability::AttachmentRead)
        .map_err(ApiError)?;
    let resource = state
        .resources
        .authorize(&session.session_id, &id)
        .map_err(ApiError)?;
    let range = parse_single_range(
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok()),
        resource.length,
    );
    let range = match range {
        Ok(range) => range,
        Err(WebError::InvalidRange) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{}", resource.length)) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            return Ok(response);
        }
        Err(error) => return Err(ApiError(error)),
    };
    let selected = range.unwrap_or(ByteRange {
        start: 0,
        end: resource.length.saturating_sub(1),
    });
    let content_length = if resource.length == 0 {
        0
    } else {
        selected.len()
    };
    let body = if method == Method::HEAD || content_length == 0 {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::open(&resource.path)
            .await
            .map_err(|_| ApiError(WebError::ResourceNotFound))?;
        file.seek(std::io::SeekFrom::Start(selected.start))
            .await
            .map_err(|error| ApiError(WebError::Internal(error.to_string())))?;
        Body::from_stream(ReaderStream::new(file.take(content_length)))
    };
    let mut response = Response::new(body);
    *response.status_mut() = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&resource.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string())
            .expect("numeric content length is valid"),
    );
    if range.is_some() {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!(
                "bytes {}-{}/{}",
                selected.start, selected.end, resource.length
            ))
            .expect("numeric content range is valid"),
        );
    }
    Ok(response)
}

async fn upload_start_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Json(body): Json<UploadStartRequest>,
) -> Result<Json<UploadStartResponse>, ApiError> {
    require_host(&state, &headers, true)?;
    let session = require_session(&state, &headers)?;
    state
        .require_mutation(&headers, &session)
        .map_err(ApiError)?;
    state
        .require_capability(MobileCapability::AttachmentUpload)
        .map_err(ApiError)?;
    let upload_id = state
        .uploads
        .begin(
            &session.session_id,
            &body.file_name,
            body.expected_length,
            Instant::now(),
        )
        .map_err(ApiError)?;
    Ok(Json(UploadStartResponse {
        upload_id,
        max_bytes: state.uploads.max_bytes,
    }))
}

async fn upload_chunk_handler(
    State(state): State<Arc<WebState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_host(&state, &headers, true)?;
    let session = require_session(&state, &headers)?;
    state
        .require_mutation(&headers, &session)
        .map_err(ApiError)?;
    state
        .require_capability(MobileCapability::AttachmentUpload)
        .map_err(ApiError)?;
    let offset = headers
        .get("x-keencode-upload-offset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| ApiError(WebError::Upload("缺少合法上传偏移".to_owned())))?;
    let length = state
        .uploads
        .write_chunk(&session.session_id, &id, offset, &body)
        .map_err(ApiError)?;
    Ok(Json(serde_json::json!({ "offset": length })))
}

async fn upload_complete_handler(
    State(state): State<Arc<WebState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Json<UploadFinishResponse>, ApiError> {
    require_host(&state, &headers, true)?;
    let session = require_session(&state, &headers)?;
    state
        .require_mutation(&headers, &session)
        .map_err(ApiError)?;
    state
        .require_capability(MobileCapability::AttachmentUpload)
        .map_err(ApiError)?;
    let uploaded = state
        .uploads
        .finish_resource(&session.session_id, &id)
        .map_err(ApiError)?;
    let resource_id = state
        .resources
        .register(
            &session.session_id,
            state.uploads.root(),
            &uploaded.path,
            uploaded.content_type.clone(),
            &uploaded.file_name,
        )
        .map_err(ApiError)?;
    Ok(Json(UploadFinishResponse {
        resource_id,
        file_name: uploaded.file_name,
        content_type: uploaded.content_type,
        size: uploaded.length,
    }))
}

async fn upload_abort_handler(
    State(state): State<Arc<WebState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_host(&state, &headers, true)?;
    let session = require_session(&state, &headers)?;
    state
        .require_mutation(&headers, &session)
        .map_err(ApiError)?;
    state
        .require_capability(MobileCapability::AttachmentUpload)
        .map_err(ApiError)?;
    state.uploads.abort(&session.session_id, &id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn single_upload_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<UploadFinishResponse>, ApiError> {
    require_host(&state, &headers, true)?;
    let session = require_session(&state, &headers)?;
    state
        .require_mutation(&headers, &session)
        .map_err(ApiError)?;
    state
        .require_capability(MobileCapability::AttachmentUpload)
        .map_err(ApiError)?;
    if body.len() as u64 > state.uploads.max_bytes {
        return Err(ApiError(WebError::Upload(
            "上传文件超过大小上限".to_owned(),
        )));
    }
    let display_name = headers
        .get("x-keencode-file-name")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError(WebError::Upload("缺少安全文件名".to_owned())))?;
    let id = state
        .uploads
        .begin(
            &session.session_id,
            display_name,
            Some(body.len() as u64),
            Instant::now(),
        )
        .map_err(ApiError)?;
    if let Err(error) = state
        .uploads
        .write_chunk(&session.session_id, &id, 0, &body)
    {
        state.uploads.abort(&session.session_id, &id);
        return Err(ApiError(error));
    }
    let uploaded = state
        .uploads
        .finish_resource(&session.session_id, &id)
        .map_err(ApiError)?;
    let resource_id = state
        .resources
        .register(
            &session.session_id,
            state.uploads.root(),
            &uploaded.path,
            uploaded.content_type.clone(),
            &uploaded.file_name,
        )
        .map_err(ApiError)?;
    Ok(Json(UploadFinishResponse {
        resource_id,
        file_name: uploaded.file_name,
        content_type: uploaded.content_type,
        size: uploaded.length,
    }))
}

async fn websocket_handler(
    State(state): State<Arc<WebState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    websocket: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if let Err(error) = require_host(&state, &headers, true) {
        return error.into_response();
    }
    if uri.query().is_some_and(|query| {
        query
            .split('&')
            .any(|part| part.split('=').next() == Some(TOKEN_QUERY_PARAMETER))
    }) {
        return ApiError(WebError::InvalidCredentials).into_response();
    }
    if let Err(error) = require_session(&state, &headers) {
        return error.into_response();
    }
    let websocket = match websocket {
        Ok(websocket) => websocket,
        Err(rejection) => return rejection.into_response(),
    };
    websocket
        .on_upgrade(|mut socket| async move {
            let _ = socket
                .send(axum::extract::ws::Message::Text(
                    r#"{"type":"ready","transport":"websocket"}"#.into(),
                ))
                .await;
            while let Some(Ok(message)) = socket.recv().await {
                match message {
                    axum::extract::ws::Message::Ping(payload) => {
                        let _ = socket.send(axum::extract::ws::Message::Pong(payload)).await;
                    }
                    axum::extract::ws::Message::Close(_) => break,
                    _ => {}
                }
            }
        })
        .into_response()
}

async fn business_websocket_handler(
    State(state): State<Arc<WebState>>,
    Extension(adapter): Extension<Arc<HostWsAdapter>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    websocket: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if let Err(error) = require_host(&state, &headers, true) {
        return error.into_response();
    }
    if uri.query().is_some_and(|query| {
        query
            .split('&')
            .any(|part| part.split('=').next() == Some(TOKEN_QUERY_PARAMETER))
    }) {
        return ApiError(WebError::InvalidCredentials).into_response();
    }
    let session = match require_session(&state, &headers) {
        Ok(session) => session,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = state.require_capability(MobileCapability::SessionRead) {
        return ApiError(error).into_response();
    }
    let websocket = match websocket {
        Ok(websocket) => websocket,
        Err(rejection) => return rejection.into_response(),
    };
    let connection_id = match ConnectionId::new(format!("web-{}", random_opaque_id())) {
        Ok(connection_id) => connection_id,
        Err(_) => {
            return ApiError(WebError::Internal("连接标识生成失败".to_owned())).into_response();
        }
    };
    let context = match HostConnectionContext::new(
        connection_id.clone(),
        session.session_id,
        session.token_version,
    ) {
        Ok(context) => context,
        Err(_) => return ApiError(WebError::Internal("连接上下文无效".to_owned())).into_response(),
    };
    let connection = match adapter.connect(context) {
        Ok(connection) => connection,
        Err(error) => return business_error_response(error),
    };
    websocket
        .on_upgrade(move |socket| business_websocket_loop(socket, adapter, connection))
        .into_response()
}

async fn business_websocket_loop(
    socket: axum::extract::ws::WebSocket,
    adapter: Arc<HostWsAdapter>,
    connection: HostWsConnection,
) {
    let connection_id = connection.context().connection_id.clone();
    // dispatcher 响应与出站事件共用同一条 socket：写端用互斥锁串行化，
    // 使入站 dispatch 可以放到后台任务而不会与出站排水交叉写坏帧。
    let (raw_sender, mut receiver) = socket.split();
    let sender = std::sync::Arc::new(tokio::sync::Mutex::new(raw_sender));
    if sender
        .lock()
        .await
        .send(axum::extract::ws::Message::Text(
            r#"{"type":"ready","transport":"websocket","protocol":"acp"}"#.into(),
        ))
        .await
        .is_err()
    {
        adapter.disconnect(&connection_id);
        return;
    }
    // 后台 dispatch 任务写 socket 失败时置位；主循环在下轮 select 后退出。
    let send_failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 并发 dispatch 上限对齐 IPC server 的 permit 模式，防止单连接洪泛。
    let dispatch_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        if send_failed.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        tokio::select! {
            inbound = receiver.next() => {
                let Some(Ok(message)) = inbound else { break };
                let raw = match message {
                    axum::extract::ws::Message::Text(value) => value.as_bytes().to_vec(),
                    axum::extract::ws::Message::Binary(value) => value.to_vec(),
                    axum::extract::ws::Message::Ping(value) => {
                        if sender.lock().await.send(axum::extract::ws::Message::Pong(value)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    axum::extract::ws::Message::Close(_) => break,
                    axum::extract::ws::Message::Pong(_) => continue,
                };
                // 关键：dispatch 不再内联 await（长 session/prompt 会让 select
                // 卡在入站分支、出站队列撑爆后清空已缓冲事件）。放到后台任务，
                // 主循环继续排水 outbound；响应经写锁串行回发。
                let dispatch_adapter = std::sync::Arc::clone(&adapter);
                let dispatch_connection = connection_id.clone();
                let dispatch_sender = std::sync::Arc::clone(&sender);
                let dispatch_failed = std::sync::Arc::clone(&send_failed);
                let dispatch_permit = std::sync::Arc::clone(&dispatch_permits);
                tokio::spawn(async move {
                    let _permit = dispatch_permit
                        .acquire()
                        .await
                        .expect("Web dispatch semaphore 必须保持开放");
                    match dispatch_adapter.dispatch_acp(&dispatch_connection, &raw).await {
                        Ok(Some(response)) => {
                            let mut writer = dispatch_sender.lock().await;
                            if send_business_bytes(&mut writer, response).await.is_err() {
                                dispatch_failed.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let mut writer = dispatch_sender.lock().await;
                            if writer.send(business_error_message(error)).await.is_err() {
                                dispatch_failed.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
            outbound = connection.recv() => {
                let Some(outbound) = outbound else { break };
                let message = match outbound {
                    OutboundEnvelope::Event(event) => match String::from_utf8(event.payload) {
                        Ok(value) => axum::extract::ws::Message::Text(value.into()),
                        Err(_) => break,
                    },
                    OutboundEnvelope::Snapshot(snapshot) => match String::from_utf8(snapshot.payload) {
                        Ok(value) => axum::extract::ws::Message::Text(value.into()),
                        Err(_) => break,
                    },
                    OutboundEnvelope::Gap(gap) => {
                        let Some(session_id) = connection.active_session_id() else { break };
                        match serde_json::to_string(&serde_json::json!({
                            "type": "gap",
                            "sessionId": session_id,
                            "expected": gap.expected,
                            "latest": gap.latest,
                            "reason": gap.reason,
                            "snapshotRequired": true,
                            "recoveryMethod": "session/load",
                        })) {
                            Ok(value) => axum::extract::ws::Message::Text(value.into()),
                            Err(_) => break,
                        }
                    },
                };
                if sender.lock().await.send(message).await.is_err() {
                    break;
                }
            }
        }
    }
    adapter.disconnect(&connection_id);
}

async fn send_business_bytes(
    sender: &mut futures_util::stream::SplitSink<
        axum::extract::ws::WebSocket,
        axum::extract::ws::Message,
    >,
    bytes: Vec<u8>,
) -> Result<(), ()> {
    let value = String::from_utf8(bytes).map_err(|_| ())?;
    sender
        .send(axum::extract::ws::Message::Text(value.into()))
        .await
        .map_err(|_| ())
}

fn business_error_response(error: HostBusinessError) -> Response {
    let code = business_error_code(&error);
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": code })),
    )
        .into_response()
}

fn business_error_message(error: HostBusinessError) -> axum::extract::ws::Message {
    axum::extract::ws::Message::Text(
        serde_json::json!({ "type": "error", "error": business_error_code(&error) })
            .to_string()
            .into(),
    )
}

fn business_error_code(error: &HostBusinessError) -> &'static str {
    match error {
        HostBusinessError::InvalidAcp => "invalid_acp",
        HostBusinessError::ConnectionNotFound => "connection_not_found",
        HostBusinessError::ConnectionClosed => "connection_closed",
        HostBusinessError::Router => "router_error",
        HostBusinessError::SnapshotUnavailable => "snapshot_unavailable",
        HostBusinessError::CapabilityDenied => "capability_denied",
        HostBusinessError::InvalidConfig(_) => "invalid_connection",
    }
}

async fn denied_terminal_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_host(&state, &headers, false)?;
    let _ = require_session(&state, &headers)?;
    Err(ApiError(WebError::CapabilityDenied("terminal")))
}

async fn denied_host_admin_handler(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_host(&state, &headers, false)?;
    let _ = require_session(&state, &headers)?;
    Err(ApiError(WebError::CapabilityDenied("hostAdmin")))
}

async fn static_handler(
    State(state): State<Arc<WebState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    method: Method,
) -> Result<Response, ApiError> {
    require_host(&state, &headers, false)?;
    if uri.path().starts_with("/api/") {
        return Err(ApiError(WebError::InvalidSession));
    }
    if uri.path() == "/" && uri.query().is_none() {
        return Ok(Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header(header::LOCATION, "/?hostMode=mobile-remote")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .expect("fixed Web Host redirect must be valid"));
    }
    let asset = state.static_assets.resolve(uri.path()).map_err(ApiError)?;
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let file = tokio::fs::File::open(&asset.path)
            .await
            .map_err(|error| ApiError(WebError::Internal(error.to_string())))?;
        Body::from_stream(ReaderStream::new(file))
    };
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&asset.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&asset.length.to_string()).expect("numeric length is valid"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

// Keep helper APIs in this file small and explicit; business routes should use WebState guards.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    fn token() -> WebToken {
        WebToken::try_from("test-token-123456789".to_owned()).unwrap()
    }

    fn config(root: &Path) -> WebHostConfig {
        WebHostConfig::new(
            IpAddr::from([127, 0, 0, 1]),
            32123,
            token(),
            root.to_owned(),
            root.with_extension("uploads"),
        )
        .unwrap()
    }

    fn headers(origin: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:32123"));
        if origin {
            headers.insert(
                header::ORIGIN,
                HeaderValue::from_static("http://127.0.0.1:32123"),
            );
        }
        headers
    }

    #[test]
    fn token_debug_serialize_never_leaks() {
        let token = token();
        assert!(!format!("{token:?}").contains("test-token"));
        assert_eq!(serde_json::to_string(&token).unwrap(), r#""[REDACTED]""#);
    }

    #[test]
    fn fixed_port_rejects_zero() {
        let error = WebHostConfig::new(
            IpAddr::from([127, 0, 0, 1]),
            0,
            token(),
            PathBuf::from("x"),
            PathBuf::from("uploads"),
        )
        .unwrap_err();
        assert!(matches!(error, WebError::InvalidConfig(_)));
    }

    #[test]
    fn auth_session_and_rotation_revoke_old_cookie() {
        let now = Instant::now();
        let auth = AuthService::new(
            token(),
            Duration::from_secs(60),
            AuthFailureLimiter::default(),
        );
        let result = auth.login("test-token-123456789", "peer", now).unwrap();
        assert_eq!(result.token_version, 1);
        let inner = auth.inner.lock().unwrap();
        let (session_id, record) = inner.sessions.iter().next().unwrap();
        let cookie = format!(
            "{}={}; {}={}",
            SESSION_COOKIE_NAME, session_id, CSRF_COOKIE_NAME, record.csrf_token
        );
        drop(inner);
        assert_eq!(
            auth.authenticate_cookie(&cookie, now)
                .unwrap()
                .token_version,
            1
        );
        auth.rotate_token(WebToken::try_from("new-test-token-123456".to_owned()).unwrap())
            .unwrap();
        assert!(matches!(
            auth.authenticate_cookie(&cookie, now),
            Err(WebError::InvalidSession)
        ));
    }

    #[test]
    fn rate_limit_after_five_failures() {
        let now = Instant::now();
        let auth = AuthService::new(
            token(),
            Duration::from_secs(60),
            AuthFailureLimiter::new(2, Duration::from_secs(30)),
        );
        assert!(matches!(
            auth.login("wrong-token", "peer", now),
            Err(WebError::InvalidCredentials)
        ));
        assert!(matches!(
            auth.login("wrong-token", "peer", now),
            Err(WebError::InvalidCredentials)
        ));
        assert!(matches!(
            auth.login("wrong-token", "peer", now),
            Err(WebError::RateLimited { .. })
        ));
    }

    #[test]
    fn host_origin_and_csrf_are_strict() {
        let policy = HostHeaderPolicy::for_bind(IpAddr::from([127, 0, 0, 1]), 32123);
        assert!(policy.validate_headers(&headers(true), true).is_ok());
        let mut wrong_host = headers(true);
        wrong_host.insert(header::HOST, HeaderValue::from_static("evil.example:32123"));
        assert!(matches!(
            policy.validate_headers(&wrong_host, true),
            Err(WebError::InvalidHost)
        ));
        let mut no_origin = headers(false);
        assert!(matches!(
            policy.validate_headers(&no_origin, true),
            Err(WebError::InvalidOrigin)
        ));
        no_origin.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example:32123"),
        );
        assert!(matches!(
            policy.validate_headers(&no_origin, true),
            Err(WebError::InvalidOrigin)
        ));
    }

    #[test]
    fn ranges_are_single_and_bounded() {
        assert_eq!(parse_single_range(None, 10).unwrap(), None);
        assert_eq!(
            parse_single_range(Some("bytes=2-5"), 10).unwrap(),
            Some(ByteRange { start: 2, end: 5 })
        );
        assert_eq!(
            parse_single_range(Some("bytes=-3"), 10).unwrap(),
            Some(ByteRange { start: 7, end: 9 })
        );
        assert!(matches!(
            parse_single_range(Some("bytes=1-2,4-5"), 10),
            Err(WebError::InvalidRange)
        ));
        assert!(matches!(
            parse_single_range(Some("bytes=99-"), 10),
            Err(WebError::InvalidRange)
        ));
    }

    #[test]
    fn resource_registry_never_accepts_outside_path() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("secret.txt");
        fs::write(&path, b"secret").unwrap();
        let registry = ResourceRegistry::new();
        assert!(matches!(
            registry.register("s", root.path(), &path, "text/plain", "secret.txt"),
            Err(WebError::UnsafePath)
        ));
    }

    #[test]
    fn static_resolver_rejects_traversal_and_percent_encoded_parent() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), b"ok").unwrap();
        let resolver = StaticAssetResolver::production(root.path()).unwrap();
        assert!(resolver.resolve("/").is_ok());
        assert!(matches!(
            resolver.resolve("/../secret"),
            Err(WebError::UnsafePath)
        ));
        assert!(matches!(
            resolver.resolve("/%2e%2e/secret"),
            Err(WebError::UnsafePath)
        ));
    }

    #[test]
    fn upload_is_contiguous_and_cleans_on_expiry() {
        let root = tempfile::tempdir().unwrap();
        let store = UploadStore::new(root.path().join("uploads"), 10).unwrap();
        let now = Instant::now();
        let id = store.begin("s", "photo.png", Some(4), now).unwrap();
        assert!(store.write_chunk("s", &id, 1, b"x").is_err());
        assert_eq!(store.write_chunk("s", &id, 0, b"test").unwrap(), 4);
        let uploaded = store.finish_resource("s", &id).unwrap();
        assert!(uploaded.path.is_file());
        assert_eq!(uploaded.file_name, "photo.png");
        assert_eq!(uploaded.content_type, "image/png");
        assert_eq!(uploaded.length, 4);
        let id = store.begin("s", "note.txt", Some(4), now).unwrap();
        store.write_chunk("s", &id, 0, b"x").unwrap();
        assert!(matches!(
            store.finish_resource("s", &id),
            Err(WebError::Upload(message)) if message == "上传长度与声明不一致"
        ));
        assert_eq!(
            fs::read_dir(store.root()).unwrap().count(),
            1,
            "长度不符的临时文件必须在关闭句柄后被清理"
        );
        let id = store.begin("s", "note.txt", None, now).unwrap();
        assert_eq!(
            store.cleanup_expired(now + Duration::from_secs(61), Duration::from_secs(60)),
            1
        );
        assert!(store.write_chunk("s", &id, 0, b"x").is_err());
    }

    #[test]
    fn upload_root_is_explicit_and_static_root_stays_read_only() {
        let static_root = tempfile::tempdir().unwrap();
        let upload_root = tempfile::tempdir().unwrap();
        fs::write(static_root.path().join("index.html"), b"static").unwrap();
        let config = WebHostConfig::new(
            IpAddr::from([127, 0, 0, 1]),
            32124,
            token(),
            static_root.path().to_owned(),
            upload_root.path().to_owned(),
        )
        .unwrap();
        let host = WebHost::new(config).unwrap();
        let id = host
            .state()
            .uploads()
            .begin("session", "note.txt", Some(4), Instant::now())
            .unwrap();
        host.state()
            .uploads()
            .write_chunk("session", &id, 0, b"data")
            .unwrap();
        let path = host.state().uploads().finish("session", &id).unwrap();
        assert!(path.starts_with(fs::canonicalize(upload_root.path()).unwrap()));
        assert!(static_root.path().join("index.html").is_file());
        assert!(!static_root.path().join(".uploads").exists());
        assert!(matches!(
            host.state().static_assets.resolve("/note.txt"),
            Err(WebError::ResourceNotFound)
        ));
    }

    #[test]
    fn overlapping_upload_root_is_rejected_before_directory_creation() {
        let static_root = tempfile::tempdir().unwrap();
        fs::write(static_root.path().join("index.html"), b"static").unwrap();
        let upload_root = static_root.path().join(".uploads");
        let config = WebHostConfig::new(
            IpAddr::from([127, 0, 0, 1]),
            32126,
            token(),
            static_root.path().to_owned(),
            upload_root.clone(),
        )
        .unwrap();
        assert!(matches!(
            WebHost::new(config),
            Err(WebError::InvalidConfig(_))
        ));
        assert!(!upload_root.exists());
    }

    struct TestCredentialProvider {
        token: WebToken,
        fail_store: bool,
    }

    impl SystemCredentialProvider for TestCredentialProvider {
        fn load_web_token(&self) -> Result<WebToken, WebError> {
            Ok(self.token.clone())
        }

        fn store_web_token(&self, _token: &WebToken) -> Result<(), WebError> {
            if self.fail_store {
                Err(WebError::Credential("测试 provider 拒绝写入".to_owned()))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn credential_provider_must_persist_before_rotation_and_revoke() {
        let static_root = tempfile::tempdir().unwrap();
        let upload_root = tempfile::tempdir().unwrap();
        fs::write(static_root.path().join("index.html"), b"static").unwrap();
        let config = WebHostConfig::new(
            IpAddr::from([127, 0, 0, 1]),
            32125,
            token(),
            static_root.path().to_owned(),
            upload_root.path().to_owned(),
        )
        .unwrap();
        let provider = TestCredentialProvider {
            token: token(),
            fail_store: false,
        };
        let host = WebHost::new_with_credential_provider(config, &provider).unwrap();
        let now = Instant::now();
        host.state()
            .auth
            .login("test-token-123456789", "test-peer", now)
            .unwrap();
        let (session_id, csrf_token) = {
            let state = host.state();
            let inner = state.auth.inner.lock().unwrap();
            let (session_id, record) = inner.sessions.iter().next().unwrap();
            (session_id.clone(), record.csrf_token.clone())
        };
        assert_eq!(host.status().token_version, 1);

        let failing = TestCredentialProvider {
            token: token(),
            fail_store: true,
        };
        assert!(
            host.rotate_token_with_provider(
                &failing,
                WebToken::try_from("new-test-token-123456".to_owned()).unwrap(),
            )
            .is_err()
        );
        assert_eq!(host.status().token_version, 1);
        assert_eq!(host.status().session_count, 1);

        host.rotate_token_with_provider(
            &provider,
            WebToken::try_from("new-test-token-123456".to_owned()).unwrap(),
        )
        .unwrap();
        assert_eq!(host.status().token_version, 2);
        let cookie = format!(
            "{}={}; {}={}",
            SESSION_COOKIE_NAME, session_id, CSRF_COOKIE_NAME, csrf_token
        );
        assert!(matches!(
            host.state().auth.authenticate_cookie(&cookie, now),
            Err(WebError::InvalidSession)
        ));
    }

    #[test]
    fn mobile_policy_denies_terminal_and_host_admin() {
        let policy = CapabilityPolicy::for_mobile();
        assert!(policy.require(MobileCapability::AttachmentUpload).is_ok());
        assert!(matches!(
            policy.require(MobileCapability::Terminal),
            Err(WebError::CapabilityDenied("terminal"))
        ));
        assert!(matches!(
            policy.require(MobileCapability::HostAdmin),
            Err(WebError::CapabilityDenied("hostAdmin"))
        ));
    }

    #[tokio::test]
    async fn login_sets_http_only_session_and_csrf_cookie() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), b"ok").unwrap();
        let host = WebHost::new(config(root.path())).unwrap();
        let request = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/auth/login")
            .header(header::HOST, "127.0.0.1:32123")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"token":"test-token-123456789"}"#))
            .unwrap();
        let response = host.router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.contains("keencode_session=") && cookie.contains("HttpOnly"))
        );
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.contains("keencode_csrf=") && !cookie.contains("HttpOnly"))
        );
    }

    #[tokio::test]
    async fn static_page_is_public_but_api_data_requires_session() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), b"ok").unwrap();
        let host = WebHost::new(config(root.path())).unwrap();
        let static_request = axum::http::Request::builder()
            .uri("/")
            .header(header::HOST, "127.0.0.1:32123")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            host.router()
                .clone()
                .oneshot(static_request)
                .await
                .unwrap()
                .status(),
            StatusCode::TEMPORARY_REDIRECT
        );
        let remote_request = axum::http::Request::builder()
            .uri("/?hostMode=mobile-remote")
            .header(header::HOST, "127.0.0.1:32123")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            host.router()
                .clone()
                .oneshot(remote_request)
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let api_request = axum::http::Request::builder()
            .uri("/api/capabilities")
            .header(header::HOST, "127.0.0.1:32123")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            host.router().oneshot(api_request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn resource_handler_streams_ranges_and_serves_empty_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), b"ok").unwrap();
        let host = WebHost::new(config(root.path())).unwrap();
        let issued = host
            .state
            .auth
            .login_issued("test-token-123456789", "test-peer", Instant::now())
            .unwrap();
        let session_cookie = format!("{}={}", SESSION_COOKIE_NAME, issued.session_id);

        let ranged_path = root.path().join("ranged.bin");
        fs::write(&ranged_path, b"abcdef").unwrap();
        let ranged_id = host
            .state
            .resources
            .register(
                &issued.session_id,
                root.path(),
                &ranged_path,
                "application/octet-stream",
                "ranged.bin",
            )
            .unwrap();
        let ranged_request = axum::http::Request::builder()
            .uri(format!("/api/resources/{}", ranged_id.as_str()))
            .header(header::HOST, "127.0.0.1:32123")
            .header(header::COOKIE, &session_cookie)
            .header(header::RANGE, "bytes=2-4")
            .body(Body::empty())
            .unwrap();
        let ranged_response = host.router().clone().oneshot(ranged_request).await.unwrap();
        assert_eq!(ranged_response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(read_body(ranged_response).await, b"cde");

        let empty_path = root.path().join("empty.bin");
        fs::write(&empty_path, []).unwrap();
        let empty_id = host
            .state
            .resources
            .register(
                &issued.session_id,
                root.path(),
                &empty_path,
                "application/octet-stream",
                "empty.bin",
            )
            .unwrap();
        let empty_request = axum::http::Request::builder()
            .uri(format!("/api/resources/{}", empty_id.as_str()))
            .header(header::HOST, "127.0.0.1:32123")
            .header(header::COOKIE, &session_cookie)
            .body(Body::empty())
            .unwrap();
        let empty_response = host.router().oneshot(empty_request).await.unwrap();
        assert_eq!(empty_response.status(), StatusCode::OK);
        assert_eq!(empty_response.headers()[header::CONTENT_LENGTH], "0");
        assert!(read_body(empty_response).await.is_empty());
    }

    #[tokio::test]
    async fn upload_completion_returns_preview_metadata_and_resource_can_be_read() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), b"ok").unwrap();
        let host = WebHost::new(config(root.path())).unwrap();
        let issued = host
            .state
            .auth
            .login_issued("test-token-123456789", "test-peer", Instant::now())
            .unwrap();
        let cookies = format!(
            "{}={}; {}={}",
            SESSION_COOKIE_NAME, issued.session_id, CSRF_COOKIE_NAME, issued.csrf_token
        );
        let upload = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/uploads")
            .header(header::HOST, "127.0.0.1:32123")
            .header(header::ORIGIN, "http://127.0.0.1:32123")
            .header(header::COOKIE, &cookies)
            .header(CSRF_HEADER_NAME, &issued.csrf_token)
            .header("x-keencode-file-name", "photo.png")
            .header(header::CONTENT_TYPE, "image/png")
            .body(Body::from("image"))
            .unwrap();
        let response = host.router().clone().oneshot(upload).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let payload: serde_json::Value =
            serde_json::from_slice(&read_body(response).await).unwrap();
        assert_eq!(payload["fileName"], "photo.png");
        assert_eq!(payload["contentType"], "image/png");
        assert_eq!(payload["size"], 5);
        let resource_id = payload["resourceId"].as_str().unwrap();

        let resource_request = axum::http::Request::builder()
            .uri(format!("/api/resources/{resource_id}"))
            .header(header::HOST, "127.0.0.1:32123")
            .header(header::COOKIE, &cookies)
            .body(Body::empty())
            .unwrap();
        let resource_response = host.router().oneshot(resource_request).await.unwrap();
        assert_eq!(resource_response.status(), StatusCode::OK);
        assert_eq!(
            resource_response.headers()[header::CONTENT_TYPE],
            "image/png"
        );
        assert_eq!(
            resource_response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(read_body(resource_response).await, b"image");
    }

    #[tokio::test]
    async fn ws_rejects_token_in_query() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), b"ok").unwrap();
        let host = WebHost::new(config(root.path())).unwrap();
        let request = axum::http::Request::builder()
            .uri("/api/ws?token=secret")
            .header(header::HOST, "127.0.0.1:32123")
            .header(header::ORIGIN, "http://127.0.0.1:32123")
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            host.router().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[allow(dead_code)]
    async fn read_body(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec()
    }
}
