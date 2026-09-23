//! Desktop Web Host 的配置、凭据和 ACP adapter。
//!
//! `keencode-web` 只持有 HTTP/WS transport 状态；本模块负责把它接到现有桌面
//! `AcpHost`，以及把 Web server 生命周期交给 Desktop owner。这里不复制 Session、
//! Journal 或 Runtime 事实，也不把 Provider 凭据暴露给浏览器。

use crate::acp_host;
use keencode_acp::AcpIncomingFrame;
use keencode_web::{
    HostBusinessError, HostBusinessFuture, HostBusinessRouter, HostConnectionContext,
    HostHeaderPolicy, HostWsAdapter, SystemCredentialProvider, WebError, WebHost, WebHostConfig,
    WebHostState, WebServerOwner, WebToken,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tauri::Manager;
use tokio::sync::Mutex;

pub use keencode_web::WebHostStatus;

/// Desktop Web Host 使用的固定 Token 环境变量名。
pub const WEB_TOKEN_ENV: &str = "KEENCODE_WEB_TOKEN";
/// Desktop Web Host 系统密钥库服务名。
const WEB_KEYRING_SERVICE: &str = "com.keencode.desktop.web";
/// Desktop Web Host 系统密钥库账户名。
const WEB_KEYRING_ACCOUNT: &str = "host-token";
/// CLI 与 Desktop ACP facade 共享的 Web 控制方法。
pub const WEB_START_METHOD: &str = "keencode/web/start";
/// 停止 Web Host 控制方法。
pub const WEB_STOP_METHOD: &str = "keencode/web/stop";
/// 查询 Web Host 状态控制方法。
pub const WEB_STATUS_METHOD: &str = "keencode/web/status";

/// Web Host 的非秘密配置；Token 永远由 [`SystemCredentialProvider`] 读取。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebHostSettings {
    /// 是否允许创建监听器；默认关闭。开启后桌面启动时自动开放监听
    /// （见 `lib.rs` setup 的 autostart 分支），失败仅记录诊断。
    pub enabled: bool,
    /// TCP 绑定地址。仅允许回环、私有网络和链路本地地址，拒绝通配与公网地址。
    pub bind: IpAddr,
    /// 固定监听端口；不接受 0，也不会自动换端口。
    pub port: u16,
    /// 生产构建静态资源的显式根目录。
    pub static_root: PathBuf,
    /// 与静态资源隔离的可写上传根目录。
    pub upload_root: PathBuf,
    /// 显式 Host 白名单；为空时使用绑定地址的默认严格策略。
    pub allowed_hosts: Vec<String>,
    /// 显式 Origin 白名单；为空时使用绑定地址的默认严格策略。
    pub allowed_origins: Vec<String>,
    /// 每条 WebSocket 的出站事件容量。
    pub outbound_queue_capacity: usize,
}

impl Default for WebHostSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 32123,
            static_root: PathBuf::new(),
            upload_root: PathBuf::new(),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            outbound_queue_capacity: 256,
        }
    }
}

impl WebHostSettings {
    /// 校验不依赖 Token 的配置部分。
    pub fn validate(&self) -> Result<(), WebError> {
        if self.port == 0 || self.outbound_queue_capacity == 0 {
            return Err(WebError::InvalidConfig(
                "Web Host 端口和出站队列容量必须大于 0".to_owned(),
            ));
        }
        if self.allowed_hosts.is_empty() != self.allowed_origins.is_empty() {
            return Err(WebError::InvalidConfig(
                "Host 和 Origin 白名单必须同时配置".to_owned(),
            ));
        }
        let allowed_bind = match self.bind {
            IpAddr::V4(address) => {
                address.is_loopback() || address.is_private() || address.is_link_local()
            }
            IpAddr::V6(address) => {
                address.is_loopback()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
            }
        };
        if !allowed_bind {
            return Err(WebError::InvalidConfig(
                "Web Host 只允许绑定回环或局域网 IP，不能使用未指定或公网地址".to_owned(),
            ));
        }
        Ok(())
    }

    fn host_policy(&self) -> Result<HostHeaderPolicy, WebError> {
        if self.allowed_hosts.is_empty() {
            Ok(HostHeaderPolicy::for_bind(self.bind, self.port))
        } else {
            HostHeaderPolicy::new(&self.allowed_hosts, &self.allowed_origins)
        }
    }

    fn web_config(&self, port: Option<u16>, token: WebToken) -> Result<WebHostConfig, WebError> {
        if self.enabled
            && (self.static_root.as_os_str().is_empty() || self.upload_root.as_os_str().is_empty())
        {
            return Err(WebError::InvalidConfig(
                "启用 Web Host 时必须先解析静态资源根和上传根".to_owned(),
            ));
        }
        let port = port.unwrap_or(self.port);
        let config = WebHostConfig::new(
            self.bind,
            port,
            token,
            self.static_root.clone(),
            self.upload_root.clone(),
        )?;
        config.with_limits(
            keencode_web::DEFAULT_MAX_REQUEST_BYTES,
            keencode_web::DEFAULT_MAX_UPLOAD_BYTES,
            keencode_web::DEFAULT_MAX_CONNECTIONS,
        )
    }

    /// 用宿主运行时目录补全未持久化的资源根；不会把这些路径写回普通设置文件。
    pub(crate) fn with_runtime_defaults(&self, app: &tauri::AppHandle) -> Result<Self, WebError> {
        let mut settings = self.clone();
        if settings.static_root.as_os_str().is_empty() {
            settings.static_root = app
                .path()
                .resource_dir()
                .map_err(|_| WebError::InvalidConfig("无法确定 Web 静态资源目录".to_owned()))?
                .join("web");
        }
        if settings.upload_root.as_os_str().is_empty() {
            settings.upload_root = crate::storage::root_dir(app)
                .map_err(|_| WebError::InvalidConfig("无法确定 Web 上传目录".to_owned()))?
                .join("web-uploads");
        }
        settings.validate()?;
        Ok(settings)
    }
}

/// 只从显式环境变量读取 Web Token；Token 正文不进入日志、Debug 或响应。
#[derive(Clone, Debug)]
pub struct EnvironmentCredentialProvider {
    variable: String,
}

impl EnvironmentCredentialProvider {
    /// 创建一个固定环境变量 provider。
    pub fn new(variable: impl Into<String>) -> Result<Self, WebError> {
        let variable = variable.into();
        if variable.is_empty()
            || variable.len() > 128
            || !variable
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(WebError::InvalidConfig("Token 环境变量名无效".to_owned()));
        }
        Ok(Self { variable })
    }
}

impl SystemCredentialProvider for EnvironmentCredentialProvider {
    fn load_web_token(&self) -> Result<WebToken, WebError> {
        WebToken::from_env(&self.variable)
    }

    fn store_web_token(&self, _token: &WebToken) -> Result<(), WebError> {
        Err(WebError::Credential(
            "环境变量 provider 不支持持久化 Token".to_owned(),
        ))
    }
}

/// 使用系统 Credential Manager/Keychain/Secret Service 保存 Web Token。
#[derive(Clone, Debug, Default)]
pub struct KeyringCredentialProvider;

impl KeyringCredentialProvider {
    fn entry() -> Result<keyring::Entry, WebError> {
        keyring::Entry::new(WEB_KEYRING_SERVICE, WEB_KEYRING_ACCOUNT)
            .map_err(|_| WebError::Credential("无法访问系统凭据存储".to_owned()))
    }
}

impl SystemCredentialProvider for KeyringCredentialProvider {
    fn load_web_token(&self) -> Result<WebToken, WebError> {
        let password = Self::entry()?
            .get_password()
            .map_err(|_| WebError::Credential("读取 Web Token 失败".to_owned()))?;
        WebToken::try_from(password)
            .map_err(|_| WebError::Credential("系统凭据中的 Web Token 无效".to_owned()))
    }

    fn store_web_token(&self, token: &WebToken) -> Result<(), WebError> {
        token.with_secret(|value| {
            Self::entry()?
                .set_password(value)
                .map_err(|_| WebError::Credential("写入 Web Token 失败".to_owned()))
        })
    }
}

/// 复用同一 Host 的已认证 Web ACP 业务路由。
#[derive(Debug, Default)]
pub struct DesktopWebBusinessRouter;

impl HostBusinessRouter for DesktopWebBusinessRouter {
    fn dispatch<'a>(
        &'a self,
        context: HostConnectionContext,
        frame: AcpIncomingFrame,
    ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let value = frame
                .into_json_rpc_value()
                .map_err(|_| HostBusinessError::InvalidAcp)?;
            let response =
                acp_host::acp_dispatch_value_for_connection(&context.connection_id, value)
                    .await
                    .map_err(|_| HostBusinessError::Router)?;
            response
                .map(|value| serde_json::to_vec(&value).map_err(|_| HostBusinessError::Router))
                .transpose()
        })
    }

    fn dispatch_client_response<'a>(
        &'a self,
        context: HostConnectionContext,
        response: Value,
    ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            acp_host::acp_dispatch_value_for_connection(&context.connection_id, response)
                .await
                .map_err(|_| HostBusinessError::Router)?;
            Ok(None)
        })
    }

    fn disconnect(&self, context: HostConnectionContext) {
        acp_host::acp_disconnect(&context.connection_id);
    }
}

struct WebHostRuntime {
    host: Arc<WebHost>,
    owner: WebServerOwner,
}

/// Desktop Web Host 的生命周期 owner；不缓存 Runtime/Session/Journal 数据。
pub struct WebHostManager {
    settings: RwLock<WebHostSettings>,
    provider: Arc<dyn SystemCredentialProvider>,
    runtime: Mutex<Option<WebHostRuntime>>,
    /// 供 Runtime 同步投递线程读取的当前 Web adapter；生命周期由 `runtime` owner 管理。
    active_adapter: RwLock<Option<Arc<HostWsAdapter>>>,
    /// 最近一次由 Runtime 广播的 Session Journal 水位，供定向临时事件复用。
    journal_watermarks: std::sync::Mutex<HashMap<String, u64>>,
    /// 与 Desktop Diagnostics 共享的脱敏 Host/Web transport 观测出口。
    observability: RwLock<Option<Arc<crate::diagnostics::observability::ObservabilityStore>>>,
}

impl std::fmt::Debug for WebHostManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebHostManager")
            .field(
                "settings",
                &self
                    .settings
                    .read()
                    .map(|settings| settings.clone())
                    .unwrap_or_default(),
            )
            .field("credential_provider", &"configured")
            .finish()
    }
}

impl WebHostManager {
    /// 创建尚未启动的 Desktop Web Host owner。
    pub fn new(
        settings: WebHostSettings,
        provider: Arc<dyn SystemCredentialProvider>,
    ) -> Result<Self, WebError> {
        settings.validate()?;
        Ok(Self {
            settings: RwLock::new(settings),
            provider,
            runtime: Mutex::new(None),
            active_adapter: RwLock::new(None),
            journal_watermarks: std::sync::Mutex::new(HashMap::new()),
            observability: RwLock::new(None),
        })
    }

    /// 绑定进程级 Diagnostics；Web Host 业务本身不持有第二份观测事实。
    pub(crate) fn set_observability(
        &self,
        observability: Arc<crate::diagnostics::observability::ObservabilityStore>,
    ) -> Result<(), WebError> {
        *self
            .observability
            .write()
            .map_err(|_| WebError::Internal("Web Host 观测状态不可用".to_owned()))? =
            Some(observability);
        Ok(())
    }

    /// 启动固定端口 Web Host；若传入端口则只覆盖本次启动配置。
    pub async fn start(&self, port: Option<u16>) -> Result<WebHostStatus, WebError> {
        let mut runtime = self.runtime.lock().await;
        if let Some(current) = runtime.as_ref() {
            return Ok(current.owner.status());
        }
        let settings = self
            .settings
            .read()
            .map_err(|_| WebError::Internal("Web Host 配置状态不可用".to_owned()))?
            .clone();
        if !settings.enabled {
            return Err(WebError::Disabled);
        }
        let token = self.provider.load_web_token()?;
        let config = settings.web_config(port, token)?;
        let policy = if port.is_some() && settings.allowed_hosts.is_empty() {
            HostHeaderPolicy::for_bind(settings.bind, config.port)
        } else {
            settings.host_policy()?
        };
        let host = Arc::new(WebHost::new_with_host_policy(config, policy)?);
        let resource_registry = host.state().resource_registry();
        let business = Arc::new(DesktopWebBusinessRouter);
        let adapter = Arc::new(
            HostWsAdapter::new_with_resources(
                business,
                settings.outbound_queue_capacity,
                resource_registry,
            )
            .map_err(|error| WebError::Internal(error.to_string()))?,
        );
        let router = host.router_with_business(Arc::clone(&adapter));
        let owner = WebServerOwner::start(Arc::clone(&host), router)?;
        let status = owner.status();
        *self
            .active_adapter
            .write()
            .map_err(|_| WebError::Internal("Web Host adapter 状态不可用".to_owned()))? =
            Some(Arc::clone(&adapter));
        *runtime = Some(WebHostRuntime { host, owner });
        if let Ok(observability) = self.observability.read()
            && let Some(observability) = observability.as_ref()
        {
            observability.increment_counter("web.host.started", 1);
            observability.set_gauge(
                "web.host.active_connections",
                status.active_connections as f64,
            );
        }
        Ok(status)
    }

    /// 保存非秘密配置；已运行的 Server 保持当前监听器，下一次启动使用新配置。
    pub async fn set_settings(&self, settings: WebHostSettings) -> Result<(), WebError> {
        settings.validate()?;
        *self
            .settings
            .write()
            .map_err(|_| WebError::Internal("Web Host 配置状态不可用".to_owned()))? = settings;
        Ok(())
    }

    /// 将新 Token 先写入系统凭据库，再轮换运行中 Host 的认证版本。
    pub async fn set_token(&self, token: WebToken) -> Result<WebHostStatus, WebError> {
        self.provider.store_web_token(&token)?;
        let mut runtime = self.runtime.lock().await;
        if let Some(current) = runtime.as_mut() {
            current.host.rotate_token(token)?;
            if let Ok(observability) = self.observability.read()
                && let Some(observability) = observability.as_ref()
            {
                observability.increment_counter("web.host.token_rotated", 1);
            }
        }
        Ok(runtime
            .as_ref()
            .map(|current| current.owner.status())
            .unwrap_or_else(|| self.disabled_status()))
    }

    /// 停止 Web Host 并撤销旧浏览器会话。
    pub async fn stop(&self) -> Result<WebHostStatus, WebError> {
        let mut runtime = self.runtime.lock().await;
        let Some(current) = runtime.as_mut() else {
            return Ok(self.disabled_status());
        };
        let stop_result = current.owner.stop().await;
        // 即使 graceful shutdown 超时，也必须撤销旧浏览器会话和事件出口。
        current.host.stop();
        let status = current.host.status();
        *self
            .active_adapter
            .write()
            .map_err(|_| WebError::Internal("Web Host adapter 状态不可用".to_owned()))? = None;
        *runtime = None;
        if let Ok(observability) = self.observability.read()
            && let Some(observability) = observability.as_ref()
        {
            observability.increment_counter("web.host.stopped", 1);
            observability.set_gauge("web.host.active_connections", 0.0);
        }
        stop_result?;
        Ok(status)
    }

    /// 返回不包含 Token 的运行状态。
    pub async fn status(&self) -> WebHostStatus {
        let runtime = self.runtime.lock().await;
        runtime
            .as_ref()
            .map(|current| current.owner.status())
            .unwrap_or_else(|| self.disabled_status())
    }

    /// 向当前连接发布已经由 Host Core 编码的出站事件。
    pub(crate) fn publish_session_event(
        &self,
        session_id: &str,
        journal_sequence: Option<u64>,
        payload: Vec<u8>,
    ) -> Result<usize, HostBusinessError> {
        let adapter = self
            .active_adapter
            .read()
            .map_err(|_| HostBusinessError::ConnectionClosed)?
            .clone();
        let Some(adapter) = adapter else {
            return Ok(0);
        };
        if let Some(sequence) = journal_sequence {
            let mut watermarks = self
                .journal_watermarks
                .lock()
                .map_err(|_| HostBusinessError::ConnectionClosed)?;
            watermarks
                .entry(session_id.to_owned())
                .and_modify(|current| *current = (*current).max(sequence))
                .or_insert(sequence);
        }
        let published = adapter.publish_for_session(session_id, journal_sequence, payload)?;
        if let Ok(observability) = self.observability.read()
            && let Some(observability) = observability.as_ref()
        {
            observability.increment_counter("web.transport.session_events", published as u64);
            observability.record_metric(
                "web.transport.session_event_recipients",
                published as f64,
                "count",
                [("delivery_scope".to_owned(), "session".to_owned())],
            );
        }
        Ok(published)
    }

    /// Desktop Client 模式下把远程 Owned Host 的 delivery 接入本地 Web adapter。
    ///
    /// 远程 IPC 连接只有一条，不能把 Web connection id 伪装成 Host 连接 id；
    /// 这里按 delivery 中的 Agent Session 选择已绑定的 Web 连接，沿用本地
    /// adapter 的有界队列、游标和 gap 规则。
    pub(crate) fn publish_remote_delivery(&self, delivery: &Value) -> Result<usize, WebError> {
        let session_id = delivery
            .pointer("/envelope/sessionId")
            .and_then(Value::as_str)
            .or_else(|| {
                delivery
                    .pointer("/request/params/sessionId")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                delivery
                    .pointer("/request/params/session_id")
                    .and_then(Value::as_str)
            })
            .filter(|value| !value.is_empty())
            .ok_or_else(|| WebError::Internal("远程 delivery 缺少 Session 标识".to_owned()))?;
        let journal_sequence = delivery
            .pointer("/envelope/journalSequence")
            .and_then(Value::as_u64);
        let payload = serde_json::to_vec(delivery)
            .map_err(|_| WebError::Internal("远程 delivery 无法编码".to_owned()))?;
        self.publish_session_event(session_id, journal_sequence, payload)
            .map_err(|error| WebError::Internal(error.to_string()))
    }

    /// 把已经通过 ACP 资源授权的 Prompt 目标绑定到发起 Web 连接。
    ///
    /// Desktop 进程也复用同一个 ACP Host；当本地 Desktop 请求经过这里时，
    /// 它不属于 Web adapter，连接不存在应视为正常跳过，而不是阻止 Prompt 执行。
    pub(crate) fn bind_connection_session(
        &self,
        connection_id: &keencode_acp::ConnectionId,
        session_id: &str,
    ) -> Result<(), HostBusinessError> {
        let adapter = self
            .active_adapter
            .read()
            .map_err(|_| HostBusinessError::ConnectionClosed)?
            .clone();
        let Some(adapter) = adapter else {
            return Ok(());
        };
        match adapter.bind_session(connection_id, session_id) {
            Ok(())
            | Err(HostBusinessError::ConnectionNotFound)
            | Err(HostBusinessError::ConnectionClosed) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// 只向目标 WebSocket 连接发布临时事件，不扩大到同 Session 的其他连接。
    pub(crate) fn publish_connection_event(
        &self,
        connection_id: &keencode_acp::ConnectionId,
        session_id: &str,
        payload: Vec<u8>,
    ) -> Result<(), HostBusinessError> {
        let adapter = self
            .active_adapter
            .read()
            .map_err(|_| HostBusinessError::ConnectionClosed)?
            .clone()
            .ok_or(HostBusinessError::ConnectionClosed)?;
        let journal_sequence = self
            .journal_watermarks
            .lock()
            .map_err(|_| HostBusinessError::ConnectionClosed)?
            .get(session_id)
            .copied()
            .unwrap_or(0);
        adapter
            .publish(connection_id, journal_sequence, payload)
            .map(|_| ())
            .inspect(|_| {
                if let Ok(observability) = self.observability.read()
                    && let Some(observability) = observability.as_ref()
                {
                    observability.increment_counter("web.transport.connection_events", 1);
                }
            })
    }

    fn disabled_status(&self) -> WebHostStatus {
        let settings = self
            .settings
            .read()
            .map(|settings| settings.clone())
            .unwrap_or_default();
        WebHostStatus {
            state: if settings.enabled {
                WebHostState::Stopped
            } else {
                WebHostState::Disabled
            },
            bind: settings.bind,
            port: settings.port,
            active_connections: 0,
            max_connections: keencode_web::DEFAULT_MAX_CONNECTIONS,
            session_count: 0,
            token_version: 0,
        }
    }
}

/// 构造显式环境变量 provider，供 headless/测试装配使用。
pub fn environment_provider() -> Result<EnvironmentCredentialProvider, WebError> {
    EnvironmentCredentialProvider::new(WEB_TOKEN_ENV)
}

/// 选择明确注入的环境 Token，否则使用系统密钥库；两者都不把凭据放入普通设置。
pub fn credential_provider() -> Result<Arc<dyn SystemCredentialProvider>, WebError> {
    if std::env::var_os(WEB_TOKEN_ENV).is_some() {
        return Ok(Arc::new(environment_provider()?));
    }
    Ok(Arc::new(KeyringCredentialProvider))
}

/// 处理本地 ACP facade 的 Web 控制方法；WebSocket ACP 不开放此控制面。
pub(crate) async fn dispatch_control(
    app: &tauri::AppHandle,
    message: &Value,
) -> Result<Option<Value>, String> {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !matches!(
        method,
        WEB_START_METHOD | WEB_STOP_METHOD | WEB_STATUS_METHOD
    ) {
        return Ok(None);
    }
    if !valid_control_request(message) {
        return Ok(Some(control_error(message, -32600, "无效的 JSON-RPC 请求")));
    }
    let Some(manager) = app.try_state::<Arc<WebHostManager>>() else {
        return Ok(Some(control_error(message, -32603, "Web Host 尚未配置")));
    };
    let params = message
        .get("params")
        .cloned()
        .expect("valid Web control request has params");
    let result = match method {
        WEB_START_METHOD => {
            let params = match serde_json::from_value::<StartParams>(params) {
                Ok(params) => params,
                Err(_) => return Ok(Some(control_error(message, -32602, "Web start 参数无效"))),
            };
            manager.start(params.port).await
        }
        WEB_STOP_METHOD => {
            if parse_empty_params(params).is_err() {
                return Ok(Some(control_error(message, -32602, "Web stop 参数无效")));
            }
            manager.stop().await
        }
        WEB_STATUS_METHOD => {
            if parse_empty_params(params).is_err() {
                return Ok(Some(control_error(message, -32602, "Web status 参数无效")));
            }
            Ok(manager.status().await)
        }
        _ => unreachable!(),
    };
    match result {
        Ok(value) => serde_json::to_value(value)
            .map(|value| {
                Some(json!({
                    "jsonrpc":"2.0",
                    "id": message.get("id").cloned().unwrap_or(Value::Null),
                    "result":value
                }))
            })
            .map_err(|_| "Web Host 状态无法序列化".to_owned()),
        Err(_) => Ok(Some(control_error(message, -32000, "Web Host 控制失败"))),
    }
}

fn valid_control_request(message: &Value) -> bool {
    let Some(object) = message.as_object() else {
        return false;
    };
    object.len() == 4
        && object.get("jsonrpc") == Some(&Value::String("2.0".to_owned()))
        && object
            .get("id")
            .is_some_and(|id| id.is_string() || id.is_number())
        && object.get("method").is_some_and(Value::is_string)
        && object.get("params").is_some_and(Value::is_object)
        && object
            .keys()
            .all(|key| matches!(key.as_str(), "jsonrpc" | "id" | "method" | "params"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartParams {
    #[serde(default)]
    port: Option<u16>,
}

fn parse_empty_params(value: Value) -> Result<(), String> {
    serde_json::from_value::<EmptyParams>(value)
        .map(|_| ())
        .map_err(|_| "Web Host 控制参数无效".to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

fn control_error(message: &Value, code: i64, text: &'static str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": message
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .cloned()
            .unwrap_or(Value::Null),
        "error": {"code": code, "message": text},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::net::TcpListener;

    struct TestCredentialProvider;

    impl SystemCredentialProvider for TestCredentialProvider {
        fn load_web_token(&self) -> Result<WebToken, WebError> {
            WebToken::try_from("test-web-token-123456".to_owned())
                .map_err(|_| WebError::Credential("测试 Token 无效".to_owned()))
        }

        fn store_web_token(&self, _token: &WebToken) -> Result<(), WebError> {
            Ok(())
        }
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn settings_require_matching_host_and_origin_lists() {
        let settings = WebHostSettings {
            allowed_hosts: vec!["127.0.0.1:32123".to_owned()],
            ..WebHostSettings::default()
        };
        assert!(matches!(
            settings.validate(),
            Err(WebError::InvalidConfig(_))
        ));

        let settings = WebHostSettings {
            enabled: true,
            static_root: PathBuf::from("static"),
            upload_root: PathBuf::from("uploads"),
            ..WebHostSettings::default()
        };
        assert!(settings.validate().is_ok());

        for bind in ["0.0.0.0", "8.8.8.8", "::"] {
            let settings = WebHostSettings {
                bind: bind.parse().unwrap(),
                ..WebHostSettings::default()
            };
            assert!(matches!(
                settings.validate(),
                Err(WebError::InvalidConfig(_))
            ));
        }
        let settings = WebHostSettings {
            bind: "192.168.1.20".parse().unwrap(),
            ..WebHostSettings::default()
        };
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn control_request_requires_strict_json_rpc_envelope() {
        assert!(valid_control_request(&json!({
            "jsonrpc": "2.0",
            "id": "request-1",
            "method": WEB_STATUS_METHOD,
            "params": {},
        })));
        assert!(!valid_control_request(&json!({
            "jsonrpc": "2.0",
            "id": null,
            "method": WEB_STATUS_METHOD,
            "params": {},
        })));
        assert!(!valid_control_request(&json!({
            "jsonrpc": "2.0",
            "id": "request-1",
            "method": WEB_STATUS_METHOD,
            "params": {},
            "extra": true,
        })));
    }

    #[tokio::test]
    async fn manager_start_and_stop_keep_fixed_port_lifecycle() {
        let static_root = tempfile::tempdir().unwrap();
        let upload_root = tempfile::tempdir().unwrap();
        fs::write(static_root.path().join("index.html"), b"ok").unwrap();
        let settings = WebHostSettings {
            enabled: true,
            port: free_port(),
            static_root: static_root.path().to_owned(),
            upload_root: upload_root.path().to_owned(),
            ..WebHostSettings::default()
        };
        let manager = WebHostManager::new(settings, Arc::new(TestCredentialProvider)).unwrap();
        assert_eq!(manager.status().await.state, WebHostState::Stopped);
        assert_eq!(
            manager.start(None).await.unwrap().state,
            WebHostState::Running
        );
        assert_eq!(manager.status().await.state, WebHostState::Running);
        assert_eq!(manager.stop().await.unwrap().state, WebHostState::Stopped);
        assert_eq!(manager.status().await.state, WebHostState::Stopped);
    }
}
