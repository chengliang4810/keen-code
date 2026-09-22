//! MCP OAuth 2.1 PKCE 的本地可持久化状态机。
//!
//! 本模块只负责生成授权请求、校验回调并产出令牌交换参数，不主动访问真实 OAuth
//! 服务。网络交换由上层密钥与账户组件负责。

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::{sanitize_untrusted, write_untrusted};

const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 16 * 1024;
const MAX_SCOPE_BYTES: usize = 4 * 1024;
const MAX_CALLBACK_FIELD_BYTES: usize = 8 * 1024;
const MAX_STATE_BYTES: usize = 1024;
const AUTHORIZATION_DENIED_SUMMARY: &str = "授权被拒绝";
const REFRESH_FAILED_SUMMARY: &str = "刷新令牌请求失败";

/// OAuth 授权服务器与客户端的静态配置。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthConfig {
    /// 发现并验证的授权服务器 issuer；用于把持久令牌绑定到实际签发方。
    pub authorization_server_issuer: String,
    /// 用户代理应打开的授权端点。
    pub authorization_endpoint: String,
    /// 上层组件交换授权码或刷新令牌时使用的令牌端点。
    pub token_endpoint: String,
    /// RFC 8707 资源标识，必须是目标 MCP 服务的规范 HTTPS URI。
    pub resource: String,
    /// OAuth 服务为 KeenCode 分配的客户端标识。
    pub client_id: String,
    /// 完成授权后返回本机的回调地址。
    pub redirect_uri: String,
    /// 请求的 OAuth scope 列表。
    #[serde(default)]
    pub scopes: Vec<String>,
    /// 授权服务器元数据公布的 PKCE code challenge 方法。
    pub code_challenge_methods_supported: Vec<String>,
    /// 授权回调允许等待的秒数。
    #[serde(default = "default_authorization_timeout_seconds")]
    pub authorization_timeout_seconds: u64,
}

impl fmt::Debug for OAuthConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthConfig")
            .field(
                "authorization_server_issuer",
                &redacted_url(&self.authorization_server_issuer),
            )
            .field(
                "authorization_endpoint",
                &redacted_url(&self.authorization_endpoint),
            )
            .field("token_endpoint", &redacted_url(&self.token_endpoint))
            .field("resource", &redacted_url(&self.resource))
            .field("client_id", &self.client_id)
            .field("redirect_uri", &redacted_url(&self.redirect_uri))
            .field("scopes", &self.scopes)
            .field(
                "code_challenge_methods_supported",
                &self.code_challenge_methods_supported,
            )
            .field(
                "authorization_timeout_seconds",
                &self.authorization_timeout_seconds,
            )
            .finish()
    }
}

impl OAuthConfig {
    /// 校验端点、客户端标识、回调地址和等待时间。
    pub fn validate(&self) -> Result<(), OAuthError> {
        validate_issuer(&self.authorization_server_issuer)?;
        validate_oauth_endpoint("authorization_endpoint", &self.authorization_endpoint)?;
        validate_oauth_endpoint("token_endpoint", &self.token_endpoint)?;
        validate_https_endpoint("resource", &self.resource)?;
        let resource = Url::parse(&self.resource).map_err(|error| {
            OAuthError::InvalidConfiguration(format!("resource 不是有效 URL：{error}"))
        })?;
        if resource.fragment().is_some()
            || resource.username() != ""
            || resource.password().is_some()
        {
            return Err(OAuthError::InvalidConfiguration(
                "resource 不得包含片段或用户凭据".to_owned(),
            ));
        }
        let redirect = Url::parse(&self.redirect_uri).map_err(|error| {
            OAuthError::InvalidConfiguration(format!("redirect_uri 不是有效 URL：{error}"))
        })?;
        if redirect.scheme() != "https" && !is_loopback_url(&redirect) {
            return Err(OAuthError::InvalidConfiguration(
                "redirect_uri 必须使用 HTTPS 或回环地址".to_owned(),
            ));
        }
        if redirect.fragment().is_some()
            || redirect.username() != ""
            || redirect.password().is_some()
        {
            return Err(OAuthError::InvalidConfiguration(
                "redirect_uri 不得包含片段或用户凭据".to_owned(),
            ));
        }
        if self.client_id.trim().is_empty() {
            return Err(OAuthError::InvalidConfiguration(
                "client_id 不得为空".to_owned(),
            ));
        }
        if !self
            .code_challenge_methods_supported
            .iter()
            .any(|method| method == "S256")
        {
            return Err(OAuthError::InvalidConfiguration(
                "授权服务器元数据没有声明 PKCE S256 支持".to_owned(),
            ));
        }
        if self.authorization_timeout_seconds == 0 {
            return Err(OAuthError::InvalidConfiguration(
                "authorization_timeout_seconds 必须大于零".to_owned(),
            ));
        }
        Ok(())
    }
}

/// RFC 9728 受保护资源元数据中本客户端使用的字段。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OAuthProtectedResourceMetadata {
    /// 受保护 MCP 服务的规范资源标识。
    pub resource: String,
    /// 可以为该资源签发令牌的授权服务器 issuer 列表。
    pub authorization_servers: Vec<String>,
    /// 资源服务公布的可选基础 scope。
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

impl fmt::Debug for OAuthProtectedResourceMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthProtectedResourceMetadata")
            .field("resource", &redacted_url(&self.resource))
            .field(
                "authorization_servers",
                &self
                    .authorization_servers
                    .iter()
                    .map(|url| redacted_url(url))
                    .collect::<Vec<_>>(),
            )
            .field("scopes_supported", &self.scopes_supported)
            .finish()
    }
}

/// RFC 8414 或 OpenID Connect discovery 返回的授权服务器元数据子集。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OAuthAuthorizationServerMetadata {
    /// 授权服务器 issuer。
    pub issuer: String,
    /// 用户授权端点。
    pub authorization_endpoint: String,
    /// 授权码和刷新令牌交换端点。
    pub token_endpoint: String,
    /// 授权服务器支持的 PKCE challenge 方法。
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

impl fmt::Debug for OAuthAuthorizationServerMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthAuthorizationServerMetadata")
            .field("issuer", &redacted_url(&self.issuer))
            .field(
                "authorization_endpoint",
                &redacted_url(&self.authorization_endpoint),
            )
            .field("token_endpoint", &redacted_url(&self.token_endpoint))
            .field(
                "code_challenge_methods_supported",
                &self.code_challenge_methods_supported,
            )
            .finish()
    }
}

/// 从 `WWW-Authenticate: Bearer` 中提取的发现提示。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct OAuthChallenge {
    /// 服务端明确给出的 RFC 9728 元数据 URL。
    pub resource_metadata: Option<String>,
    /// 当前请求所需的 scope，优先于元数据的 scopes_supported。
    pub scopes: Vec<String>,
}

impl fmt::Debug for OAuthChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthChallenge")
            .field(
                "resource_metadata",
                &self.resource_metadata.as_deref().map(redacted_url),
            )
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// OAuth 元数据读取边界，测试可注入确定性的内存实现。
#[async_trait]
pub trait OAuthMetadataFetcher: Send + Sync {
    /// 读取并解析指定 HTTPS 元数据 URL 的 JSON 对象。
    async fn fetch_json(&self, url: &str) -> Result<serde_json::Value, OAuthError>;
}

/// 使用 Rustls、超时和响应上限读取 OAuth 发现文档。
pub struct ReqwestOAuthMetadataFetcher {
    request_timeout: Duration,
    max_response_bytes: usize,
}

impl ReqwestOAuthMetadataFetcher {
    /// 创建禁止重定向的有界 OAuth 元数据读取器。
    pub fn new(request_timeout: Duration, max_response_bytes: usize) -> Result<Self, OAuthError> {
        if request_timeout.is_zero() || max_response_bytes == 0 {
            return Err(OAuthError::InvalidConfiguration(
                "OAuth 元数据超时与响应上限必须大于零".to_owned(),
            ));
        }
        Ok(Self {
            request_timeout,
            max_response_bytes,
        })
    }
}

#[async_trait]
impl OAuthMetadataFetcher for ReqwestOAuthMetadataFetcher {
    async fn fetch_json(&self, url: &str) -> Result<serde_json::Value, OAuthError> {
        tokio::time::timeout(self.request_timeout, async {
            let (url, host, addresses) =
                resolve_public_metadata_target(url, self.request_timeout).await?;
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .resolve_to_addrs(&host, &addresses)
                .build()
                .map_err(|error| {
                    OAuthError::InvalidConfiguration(format!(
                        "创建 OAuth HTTP 客户端失败：{}",
                        error.without_url()
                    ))
                })?;
            let response = client
                .get(url)
                .header("MCP-Protocol-Version", crate::DEFAULT_PROTOCOL_VERSION)
                .send()
                .await
                .map_err(|error| {
                    OAuthError::DiscoveryTransport(format!(
                        "OAuth 元数据请求失败：{}",
                        error.without_url()
                    ))
                })?;
            if !response.status().is_success() {
                return Err(OAuthError::DiscoveryTransport(format!(
                    "OAuth 元数据端点返回 {}",
                    response.status()
                )));
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !content_type.starts_with("application/json") {
                return Err(OAuthError::InvalidDiscovery(
                    "OAuth 元数据响应必须使用 application/json".to_owned(),
                ));
            }
            let mut stream = response.bytes_stream();
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    OAuthError::DiscoveryTransport(format!(
                        "读取 OAuth 元数据失败：{}",
                        error.without_url()
                    ))
                })?;
                if body.len().saturating_add(chunk.len()) > self.max_response_bytes {
                    return Err(OAuthError::InvalidDiscovery(format!(
                        "OAuth 元数据超过 {} 字节上限",
                        self.max_response_bytes
                    )));
                }
                body.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&body).map_err(|error| {
                OAuthError::InvalidDiscovery(format!("OAuth 元数据 JSON 无效：{error}"))
            })
        })
        .await
        .map_err(|_| OAuthError::DiscoveryTransport("OAuth 元数据整体请求超时".to_owned()))?
    }
}

/// 完成 PRM 与授权服务器元数据发现，并生成资源绑定的 OAuth 配置。
pub async fn discover_oauth_config(
    fetcher: &dyn OAuthMetadataFetcher,
    resource: &str,
    client_id: impl Into<String>,
    redirect_uri: impl Into<String>,
    challenge: Option<&OAuthChallenge>,
) -> Result<OAuthConfig, OAuthError> {
    let protected_value = fetch_first(
        fetcher,
        &protected_resource_metadata_urls(resource, challenge)?,
        "受保护资源元数据",
    )
    .await?;
    let protected: OAuthProtectedResourceMetadata = serde_json::from_value(protected_value)
        .map_err(|error| OAuthError::InvalidDiscovery(format!("PRM 结构无效：{error}")))?;
    let issuer = protected
        .authorization_servers
        .first()
        .ok_or_else(|| OAuthError::InvalidDiscovery("PRM 没有 authorization_servers".to_owned()))?;
    let authorization_value = fetch_first(
        fetcher,
        &authorization_server_metadata_urls(issuer)?,
        "授权服务器元数据",
    )
    .await?;
    let authorization: OAuthAuthorizationServerMetadata =
        serde_json::from_value(authorization_value).map_err(|error| {
            OAuthError::InvalidDiscovery(format!("授权服务器元数据结构无效：{error}"))
        })?;
    if authorization.issuer != *issuer {
        return Err(OAuthError::InvalidDiscovery(
            "授权服务器元数据 issuer 与实际发现来源不一致".to_owned(),
        ));
    }
    OAuthConfig::from_discovery(
        resource,
        client_id,
        redirect_uri,
        &protected,
        &authorization,
        challenge,
    )
}

async fn fetch_first(
    fetcher: &dyn OAuthMetadataFetcher,
    urls: &[String],
    label: &str,
) -> Result<serde_json::Value, OAuthError> {
    let mut last_error = None;
    for url in urls {
        match fetcher.fetch_json(url).await {
            Ok(value) => return Ok(value),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| OAuthError::InvalidDiscovery(format!("{label}没有可尝试的 URL"))))
}

impl OAuthConfig {
    /// 使用完成验证的 PRM 与授权服务器元数据创建 PKCE 客户端配置。
    pub fn from_discovery(
        resource: impl Into<String>,
        client_id: impl Into<String>,
        redirect_uri: impl Into<String>,
        protected: &OAuthProtectedResourceMetadata,
        authorization: &OAuthAuthorizationServerMetadata,
        challenge: Option<&OAuthChallenge>,
    ) -> Result<Self, OAuthError> {
        let resource = resource.into();
        if protected.resource != resource {
            return Err(OAuthError::InvalidDiscovery(
                "受保护资源元数据的 resource 与目标 MCP 服务不一致".to_owned(),
            ));
        }
        let selected_issuer = protected.authorization_servers.first().ok_or_else(|| {
            OAuthError::InvalidDiscovery("PRM 没有 authorization_servers".to_owned())
        })?;
        if selected_issuer != &authorization.issuer {
            return Err(OAuthError::InvalidDiscovery(
                "授权服务器 issuer 与选定的发现来源不一致".to_owned(),
            ));
        }
        validate_issuer(selected_issuer)?;
        let scopes = challenge
            .filter(|challenge| !challenge.scopes.is_empty())
            .map_or_else(
                || protected.scopes_supported.clone(),
                |challenge| challenge.scopes.clone(),
            );
        let config = Self {
            authorization_server_issuer: authorization.issuer.clone(),
            authorization_endpoint: authorization.authorization_endpoint.clone(),
            token_endpoint: authorization.token_endpoint.clone(),
            resource,
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
            scopes,
            code_challenge_methods_supported: authorization
                .code_challenge_methods_supported
                .clone(),
            authorization_timeout_seconds: default_authorization_timeout_seconds(),
        };
        config.validate()?;
        Ok(config)
    }
}

/// OAuth 状态机当前所处的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthStatus {
    /// 尚未发起授权。
    Idle,
    /// 已生成授权 URL，正在等待浏览器回调。
    AwaitingAuthorization,
    /// 回调已通过校验，等待上层交换授权码。
    ExchangingCode,
    /// 已获得仍可使用的访问令牌。
    Authorized,
    /// 已生成刷新请求，等待上层交换刷新令牌。
    Refreshing,
    /// 用户或授权服务拒绝了授权。
    Denied,
    /// 授权请求或访问令牌已经过期。
    Expired,
}

/// 浏览器授权步骤需要交给上层打开和跟踪的数据。
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthAuthorizationRequest {
    /// 已包含 PKCE、state、scope 和回调地址的完整授权 URL。
    pub authorization_url: String,
    /// 本次授权使用的 CSRF state；回调必须完全一致。
    pub state: String,
    /// 本次授权的 PKCE S256 code challenge。
    pub code_challenge: String,
    /// 本次授权等待截止的 Unix 秒时间戳。
    pub expires_at: u64,
}

impl fmt::Debug for OAuthAuthorizationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthAuthorizationRequest")
            .field("authorization_url", &"<redacted>")
            .field("state", &"<redacted>")
            .field("code_challenge", &self.code_challenge)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// 从本机回调端点解析出的 OAuth 参数。
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthCallback {
    /// 授权服务原样返回的 CSRF state。
    pub state: String,
    /// 授权成功时返回的授权码。
    pub code: Option<String>,
    /// 授权失败时返回的标准 OAuth 错误码。
    pub error: Option<String>,
    /// 授权失败时返回的可选说明。
    pub error_description: Option<String>,
}

impl fmt::Debug for OAuthCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthCallback")
            .field("state", &"<redacted>")
            .field("code", &self.code.as_ref().map(|_| "<redacted>"))
            .field("error_present", &self.error.is_some())
            .field(
                "error_description_present",
                &self.error_description.is_some(),
            )
            .finish()
    }
}

/// 上层网络组件执行授权码或刷新令牌交换所需的数据。
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthTokenRequest {
    /// OAuth 令牌端点。
    pub token_endpoint: String,
    /// OAuth grant_type 值。
    pub grant_type: String,
    /// 授权码交换时使用的授权码。
    pub code: Option<String>,
    /// 刷新交换时使用的刷新令牌。
    pub refresh_token: Option<String>,
    /// 授权码交换时必须回传的回调地址。
    pub redirect_uri: Option<String>,
    /// OAuth 客户端标识。
    pub client_id: String,
    /// RFC 8707 目标 MCP 资源，授权码与刷新请求都必须携带。
    pub resource: String,
    /// 授权码交换时必须回传的 PKCE verifier。
    pub code_verifier: Option<String>,
}

impl fmt::Debug for OAuthTokenRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthTokenRequest")
            .field("token_endpoint", &redacted_url(&self.token_endpoint))
            .field("grant_type", &self.grant_type)
            .field("code", &self.code.as_ref().map(|_| "<redacted>"))
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "redirect_uri",
                &self.redirect_uri.as_deref().map(redacted_url),
            )
            .field("client_id", &self.client_id)
            .field("resource", &redacted_url(&self.resource))
            .field(
                "code_verifier",
                &self.code_verifier.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// OAuth 服务返回并由本机密钥组件持久化的令牌集合。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthTokenSet {
    /// 调用 MCP HTTP 服务时使用的访问令牌。
    pub access_token: String,
    /// HTTP Authorization 方案，通常为 `Bearer`。
    pub token_type: String,
    /// 访问令牌失效的 Unix 秒时间戳；未知时为空。
    pub expires_at: Option<u64>,
    /// 可用于获取新访问令牌的刷新令牌。
    pub refresh_token: Option<String>,
    /// OAuth 服务实际授予的 scope 字符串。
    pub scope: Option<String>,
}

impl fmt::Debug for OAuthTokenSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthTokenSet")
            .field("access_token", &"<redacted>")
            .field("token_type", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("scope_present", &self.scope.is_some())
            .finish()
    }
}

/// 可序列化到本机安全存储的 OAuth 状态快照。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSnapshot {
    status: OAuthStatus,
    pending: Option<PendingAuthorization>,
    token_set: Option<OAuthTokenSet>,
    #[serde(default)]
    last_error: Option<String>,
}

impl fmt::Debug for OAuthSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthSnapshot")
            .field("status", &self.status)
            .field("pending", &self.pending.as_ref().map(|_| "<redacted>"))
            .field("token_set", &self.token_set)
            .field("last_error_present", &self.last_error.is_some())
            .finish()
    }
}

impl Default for OAuthSnapshot {
    fn default() -> Self {
        Self {
            status: OAuthStatus::Idle,
            pending: None,
            token_set: None,
            last_error: None,
        }
    }
}

impl OAuthSnapshot {
    /// 返回当前 OAuth 阶段。
    pub fn status(&self) -> OAuthStatus {
        self.status
    }

    /// 返回当前令牌；调用者不得把令牌写入日志。
    pub fn token_set(&self) -> Option<&OAuthTokenSet> {
        self.token_set.as_ref()
    }

    /// 返回最后一次授权或刷新失败的说明。
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

/// 驱动 OAuth PKCE 授权、回调校验与刷新步骤的本地状态机。
#[derive(Debug, Clone)]
pub struct OAuthMachine {
    config: OAuthConfig,
    snapshot: OAuthSnapshot,
}

impl OAuthMachine {
    /// 使用空闲状态创建 OAuth 状态机。
    pub fn new(config: OAuthConfig) -> Result<Self, OAuthError> {
        config.validate()?;
        Ok(Self {
            config,
            snapshot: OAuthSnapshot::default(),
        })
    }

    /// 从已持久化快照恢复 OAuth 状态机。
    pub fn restore(config: OAuthConfig, mut snapshot: OAuthSnapshot) -> Result<Self, OAuthError> {
        config.validate()?;
        validate_snapshot(&mut snapshot)?;
        Ok(Self { config, snapshot })
    }

    /// 返回可持久化快照的只读引用。
    pub fn snapshot(&self) -> &OAuthSnapshot {
        &self.snapshot
    }

    /// 获取可序列化并写入安全存储的快照副本。
    pub fn snapshot_owned(&self) -> OAuthSnapshot {
        self.snapshot.clone()
    }

    /// 生成新的 PKCE 授权 URL，并进入等待回调状态。
    pub fn begin_authorization(
        &mut self,
        now_unix_seconds: u64,
    ) -> Result<OAuthAuthorizationRequest, OAuthError> {
        self.config.validate()?;
        let state = random_urlsafe(32)?;
        let code_verifier = random_urlsafe(64)?;
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let expires_at = now_unix_seconds
            .checked_add(self.config.authorization_timeout_seconds)
            .ok_or_else(|| {
                OAuthError::InvalidConfiguration("授权截止时间发生整数溢出".to_owned())
            })?;

        let mut authorization_url =
            Url::parse(&self.config.authorization_endpoint).map_err(|e| {
                OAuthError::InvalidConfiguration(format!("authorization_endpoint 无效：{e}"))
            })?;
        {
            let mut query = authorization_url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", &self.config.client_id);
            query.append_pair("redirect_uri", &self.config.redirect_uri);
            query.append_pair("resource", &self.config.resource);
            if !self.config.scopes.is_empty() {
                query.append_pair("scope", &self.config.scopes.join(" "));
            }
            query.append_pair("state", &state);
            query.append_pair("code_challenge", &code_challenge);
            query.append_pair("code_challenge_method", "S256");
        }

        self.snapshot = OAuthSnapshot {
            status: OAuthStatus::AwaitingAuthorization,
            pending: Some(PendingAuthorization {
                state: state.clone(),
                code_verifier,
                expires_at,
            }),
            token_set: self.snapshot.token_set.take(),
            last_error: None,
        };

        Ok(OAuthAuthorizationRequest {
            authorization_url: authorization_url.into(),
            state,
            code_challenge,
            expires_at,
        })
    }

    /// 校验 OAuth 回调并产出授权码交换请求；此方法不会访问令牌端点。
    pub fn handle_callback(
        &mut self,
        callback: OAuthCallback,
        now_unix_seconds: u64,
    ) -> Result<OAuthTokenRequest, OAuthError> {
        if self.snapshot.status != OAuthStatus::AwaitingAuthorization {
            return Err(OAuthError::InvalidTransition(format!(
                "状态 {:?} 不能处理授权回调",
                self.snapshot.status
            )));
        }
        let pending = self.snapshot.pending.as_ref().ok_or_else(|| {
            OAuthError::InvalidTransition("等待授权状态缺少 PKCE 数据".to_owned())
        })?;
        validate_callback_value("state", &callback.state, MAX_STATE_BYTES)?;
        if now_unix_seconds > pending.expires_at {
            self.snapshot.status = OAuthStatus::Expired;
            self.snapshot.pending = None;
            return Err(OAuthError::AuthorizationExpired);
        }
        if !constant_time_equal(callback.state.as_bytes(), pending.state.as_bytes()) {
            return Err(OAuthError::InvalidState);
        }
        if let Some(error) = callback.error {
            let error = sanitize_callback_value("error", error)?;
            let description = callback
                .error_description
                .map(|description| sanitize_callback_value("error_description", description))
                .transpose()?;
            self.snapshot.status = OAuthStatus::Denied;
            self.snapshot.pending = None;
            self.snapshot.last_error = Some(AUTHORIZATION_DENIED_SUMMARY.to_owned());
            return Err(OAuthError::AuthorizationDenied {
                code: error,
                description,
            });
        }
        let code = callback
            .code
            .map(|code| {
                validate_callback_value("code", &code, MAX_CALLBACK_FIELD_BYTES).map(|_| code)
            })
            .transpose()?
            .ok_or_else(|| OAuthError::InvalidCallback("回调缺少授权码".to_owned()))?;
        let code_verifier = pending.code_verifier.clone();
        self.snapshot.status = OAuthStatus::ExchangingCode;
        self.snapshot.last_error = None;

        Ok(OAuthTokenRequest {
            token_endpoint: self.config.token_endpoint.clone(),
            grant_type: "authorization_code".to_owned(),
            code: Some(code),
            refresh_token: None,
            redirect_uri: Some(self.config.redirect_uri.clone()),
            client_id: self.config.client_id.clone(),
            resource: self.config.resource.clone(),
            code_verifier: Some(code_verifier),
        })
    }

    /// 接收上层交换得到的令牌并进入已授权状态。
    pub fn accept_token(&mut self, mut token_set: OAuthTokenSet) -> Result<(), OAuthError> {
        if !matches!(
            self.snapshot.status,
            OAuthStatus::ExchangingCode | OAuthStatus::Refreshing
        ) {
            return Err(OAuthError::InvalidTransition(format!(
                "状态 {:?} 不能接收令牌",
                self.snapshot.status
            )));
        }
        if self.snapshot.status == OAuthStatus::Refreshing {
            if token_set.refresh_token.is_none() {
                token_set.refresh_token = self
                    .snapshot
                    .token_set
                    .as_ref()
                    .and_then(|tokens| tokens.refresh_token.clone());
            }
            if token_set.scope.is_none() {
                token_set.scope = self
                    .snapshot
                    .token_set
                    .as_ref()
                    .and_then(|tokens| tokens.scope.clone());
            }
        }
        validate_token_set(
            &mut token_set,
            self.snapshot.status == OAuthStatus::Refreshing,
        )?;
        self.snapshot.status = OAuthStatus::Authorized;
        self.snapshot.pending = None;
        self.snapshot.token_set = Some(token_set);
        self.snapshot.last_error = None;
        Ok(())
    }

    /// 在令牌过期时更新状态，并返回仍然有效的访问令牌。
    pub fn access_token(&mut self, now_unix_seconds: u64) -> Result<&str, OAuthError> {
        let token = self
            .snapshot
            .token_set
            .as_ref()
            .ok_or(OAuthError::TokenExpired)?;
        if token
            .expires_at
            .is_some_and(|expires_at| now_unix_seconds >= expires_at)
        {
            self.snapshot.status = OAuthStatus::Expired;
            return Err(OAuthError::TokenExpired);
        }
        if self.snapshot.status != OAuthStatus::Authorized {
            return Err(OAuthError::InvalidTransition(format!(
                "状态 {:?} 没有可用访问令牌",
                self.snapshot.status
            )));
        }
        Ok(&token.access_token)
    }

    /// 生成刷新令牌交换请求，并进入刷新中状态。
    pub fn begin_refresh(&mut self) -> Result<OAuthTokenRequest, OAuthError> {
        if !matches!(
            self.snapshot.status,
            OAuthStatus::Authorized | OAuthStatus::Expired
        ) {
            return Err(OAuthError::InvalidTransition(format!(
                "状态 {:?} 不能刷新令牌",
                self.snapshot.status
            )));
        }
        let refresh_token = self
            .snapshot
            .token_set
            .as_ref()
            .and_then(|tokens| tokens.refresh_token.clone())
            .ok_or(OAuthError::MissingRefreshToken)?;
        self.snapshot.status = OAuthStatus::Refreshing;
        self.snapshot.last_error = None;
        Ok(OAuthTokenRequest {
            token_endpoint: self.config.token_endpoint.clone(),
            grant_type: "refresh_token".to_owned(),
            code: None,
            refresh_token: Some(refresh_token),
            redirect_uri: None,
            client_id: self.config.client_id.clone(),
            resource: self.config.resource.clone(),
            code_verifier: None,
        })
    }

    /// 记录固定的本地刷新失败摘要，并根据旧访问令牌是否过期回到可用或过期状态。
    ///
    /// 调用方不得把服务端正文传入状态机；该正文既不参与状态转换，也不进入快照。
    pub fn reject_refresh(&mut self, now_unix_seconds: u64) {
        let expired = self.snapshot.token_set.as_ref().is_none_or(|tokens| {
            tokens
                .expires_at
                .is_some_and(|expires_at| now_unix_seconds >= expires_at)
        });
        self.snapshot.status = if expired {
            OAuthStatus::Expired
        } else {
            OAuthStatus::Authorized
        };
        self.snapshot.last_error = Some(REFRESH_FAILED_SUMMARY.to_owned());
    }
}

/// OAuth 本地状态机产生的错误。
#[derive(Clone, PartialEq, Eq)]
pub enum OAuthError {
    /// OAuth 静态配置无效。
    InvalidConfiguration(String),
    /// OAuth 发现文档、issuer 或受保护资源绑定无效。
    InvalidDiscovery(String),
    /// OAuth 元数据网络读取失败或超时。
    DiscoveryTransport(String),
    /// 操作系统安全随机源不可用。
    Randomness(String),
    /// 回调的 CSRF state 与当前授权请求不一致。
    InvalidState,
    /// OAuth 回调或令牌内容不完整。
    InvalidCallback(String),
    /// 浏览器授权等待时间已结束。
    AuthorizationExpired,
    /// 用户或授权服务拒绝授权。
    AuthorizationDenied {
        /// 标准 OAuth 错误码。
        code: String,
        /// 授权服务返回的可选说明。
        description: Option<String>,
    },
    /// 当前令牌集合不包含刷新令牌。
    MissingRefreshToken,
    /// 当前状态不允许执行所请求的转换。
    InvalidTransition(String),
    /// 没有访问令牌或访问令牌已经过期。
    TokenExpired,
}

impl fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => {
                write!(formatter, "配置无效：")?;
                write_untrusted(formatter, message)
            }
            Self::InvalidDiscovery(message) => {
                write!(formatter, "发现结果无效：")?;
                write_untrusted(formatter, message)
            }
            Self::DiscoveryTransport(message) => {
                write!(formatter, "发现传输失败：")?;
                write_untrusted(formatter, message)
            }
            Self::Randomness(message) => {
                write!(formatter, "安全随机数生成失败：")?;
                write_untrusted(formatter, message)
            }
            Self::InvalidState => write!(formatter, "回调 state 校验失败"),
            Self::InvalidCallback(message) => {
                write!(formatter, "回调无效：")?;
                write_untrusted(formatter, message)
            }
            Self::AuthorizationExpired => write!(formatter, "授权请求已经过期"),
            Self::AuthorizationDenied {
                code: _,
                description: _,
            } => write!(formatter, "{AUTHORIZATION_DENIED_SUMMARY}"),
            Self::MissingRefreshToken => write!(formatter, "没有可用的刷新令牌"),
            Self::InvalidTransition(message) => {
                write!(formatter, "状态转换无效：")?;
                write_untrusted(formatter, message)
            }
            Self::TokenExpired => write!(formatter, "访问令牌不存在或已过期"),
        }
    }
}

impl fmt::Debug for OAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for OAuthError {}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingAuthorization {
    state: String,
    code_verifier: String,
    expires_at: u64,
}

fn validate_snapshot(snapshot: &mut OAuthSnapshot) -> Result<(), OAuthError> {
    let pending_required = matches!(
        snapshot.status,
        OAuthStatus::AwaitingAuthorization | OAuthStatus::ExchangingCode
    );
    if pending_required != snapshot.pending.is_some() {
        return Err(OAuthError::InvalidTransition(
            "OAuth 快照状态与 PKCE pending 数据不一致".to_owned(),
        ));
    }
    if let Some(pending) = &snapshot.pending {
        if pending.state.is_empty()
            || pending.state.len() > MAX_STATE_BYTES
            || !pending
                .state
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(OAuthError::InvalidCallback(
                "OAuth 快照 state 不是有界 Base64URL 文本".to_owned(),
            ));
        }
        if !(43..=128).contains(&pending.code_verifier.len())
            || !pending.code_verifier.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
            })
        {
            return Err(OAuthError::InvalidCallback(
                "OAuth 快照 code_verifier 不符合 PKCE 边界".to_owned(),
            ));
        }
        if pending.expires_at == 0 {
            return Err(OAuthError::InvalidCallback(
                "OAuth 快照授权截止时间无效".to_owned(),
            ));
        }
    }

    if let Some(token_set) = &mut snapshot.token_set {
        validate_token_set(token_set, snapshot.status == OAuthStatus::Refreshing)?;
    }
    if matches!(
        snapshot.status,
        OAuthStatus::Authorized | OAuthStatus::Refreshing
    ) && snapshot.token_set.is_none()
    {
        return Err(OAuthError::InvalidTransition(
            "OAuth 快照授权状态缺少令牌集合".to_owned(),
        ));
    }
    if snapshot.status == OAuthStatus::Idle && snapshot.token_set.is_some() {
        return Err(OAuthError::InvalidTransition(
            "OAuth 空闲快照不得包含令牌".to_owned(),
        ));
    }
    match snapshot.status {
        OAuthStatus::Denied
            if snapshot.last_error.as_deref() != Some(AUTHORIZATION_DENIED_SUMMARY) =>
        {
            return Err(OAuthError::InvalidCallback(
                "OAuth 拒绝快照缺少固定本地摘要".to_owned(),
            ));
        }
        OAuthStatus::Authorized | OAuthStatus::Expired
            if snapshot
                .last_error
                .as_deref()
                .is_some_and(|summary| summary != REFRESH_FAILED_SUMMARY) =>
        {
            return Err(OAuthError::InvalidCallback(
                "OAuth 快照包含未知刷新失败摘要".to_owned(),
            ));
        }
        OAuthStatus::Idle
        | OAuthStatus::AwaitingAuthorization
        | OAuthStatus::ExchangingCode
        | OAuthStatus::Refreshing
            if snapshot.last_error.is_some() =>
        {
            return Err(OAuthError::InvalidCallback(
                "OAuth 当前状态不得包含失败摘要".to_owned(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_token_set(
    token_set: &mut OAuthTokenSet,
    require_refresh_token: bool,
) -> Result<(), OAuthError> {
    validate_opaque_token(
        "access_token",
        &token_set.access_token,
        MAX_ACCESS_TOKEN_BYTES,
    )?;
    if !token_set.token_type.eq_ignore_ascii_case("Bearer") {
        return Err(OAuthError::InvalidCallback(
            "token_type 必须是 Bearer".to_owned(),
        ));
    }
    token_set.token_type = "Bearer".to_owned();
    if let Some(refresh_token) = &token_set.refresh_token {
        validate_opaque_token("refresh_token", refresh_token, MAX_REFRESH_TOKEN_BYTES)?;
    } else if require_refresh_token {
        return Err(OAuthError::MissingRefreshToken);
    }
    if let Some(scope) = &token_set.scope {
        if scope.is_empty()
            || scope.len() > MAX_SCOPE_BYTES
            || scope.split(' ').any(str::is_empty)
            || !scope.bytes().all(|byte| {
                byte == b' '
                    || byte == 0x21
                    || (0x23..=0x5b).contains(&byte)
                    || (0x5d..=0x7e).contains(&byte)
            })
        {
            return Err(OAuthError::InvalidCallback(
                "scope 不是有界的 OAuth scope 字符串".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_opaque_token(name: &str, value: &str, limit: usize) -> Result<(), OAuthError> {
    if value.is_empty() || value.len() > limit || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(OAuthError::InvalidCallback(format!(
            "{name} 必须是非空、有界的可见 ASCII"
        )));
    }
    Ok(())
}

fn validate_callback_value(name: &str, value: &str, limit: usize) -> Result<(), OAuthError> {
    if value.trim().is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(OAuthError::InvalidCallback(format!(
            "OAuth 回调 {name} 必须是非空、有界的单行文本"
        )));
    }
    Ok(())
}

fn sanitize_callback_value(name: &str, value: String) -> Result<String, OAuthError> {
    validate_callback_value(name, &value, MAX_CALLBACK_FIELD_BYTES)?;
    Ok(sanitize_untrusted(&value))
}

fn default_authorization_timeout_seconds() -> u64 {
    600
}

fn random_urlsafe(byte_count: usize) -> Result<String, OAuthError> {
    let mut bytes = vec![0_u8; byte_count];
    getrandom::fill(&mut bytes).map_err(|error| OAuthError::Randomness(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn validate_https_endpoint(name: &str, value: &str) -> Result<(), OAuthError> {
    let url = Url::parse(value).map_err(|error| {
        OAuthError::InvalidConfiguration(format!("{name} 不是有效 URL：{error}"))
    })?;
    if url.scheme() != "https" {
        return Err(OAuthError::InvalidConfiguration(format!(
            "{name} 必须使用 HTTPS"
        )));
    }
    Ok(())
}

fn redacted_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return "<invalid-url>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn validate_oauth_endpoint(name: &str, value: &str) -> Result<(), OAuthError> {
    validate_https_endpoint(name, value)?;
    let url = Url::parse(value).map_err(|error| {
        OAuthError::InvalidConfiguration(format!("{name} 不是有效 URL：{error}"))
    })?;
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        return Err(OAuthError::InvalidConfiguration(format!(
            "{name} 不得包含用户凭据或片段"
        )));
    }
    Ok(())
}

fn validate_metadata_endpoint(name: &str, value: &str) -> Result<Url, OAuthError> {
    validate_https_endpoint(name, value)?;
    let url = Url::parse(value).map_err(|error| {
        OAuthError::InvalidConfiguration(format!("{name} 不是有效 URL：{error}"))
    })?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(OAuthError::InvalidConfiguration(format!(
            "{name} 不得包含用户凭据、查询参数或片段"
        )));
    }
    Ok(url)
}

fn validate_issuer(value: &str) -> Result<(), OAuthError> {
    validate_metadata_endpoint("issuer", value).map(|_| ())
}

async fn resolve_public_metadata_target(
    value: &str,
    timeout: Duration,
) -> Result<(Url, String, Vec<std::net::SocketAddr>), OAuthError> {
    let url = validate_metadata_endpoint("OAuth metadata URL", value)?;
    let host = url
        .host_str()
        .ok_or_else(|| OAuthError::InvalidConfiguration("OAuth metadata URL 缺少主机".to_owned()))?
        .to_owned();
    let port = url.port_or_known_default().ok_or_else(|| {
        OAuthError::InvalidConfiguration("OAuth metadata URL 缺少有效端口".to_owned())
    })?;
    let addresses = if let Ok(address) = host.parse::<std::net::IpAddr>() {
        vec![std::net::SocketAddr::new(address, port)]
    } else {
        tokio::time::timeout(timeout, tokio::net::lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| OAuthError::DiscoveryTransport("OAuth 元数据 DNS 解析超时".to_owned()))?
            .map_err(|_| OAuthError::DiscoveryTransport("OAuth 元数据 DNS 解析失败".to_owned()))?
            .collect::<Vec<_>>()
    };
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| !is_public_network_address(address.ip()))
    {
        return Err(OAuthError::InvalidDiscovery(
            "OAuth 元数据地址解析到非公网目标，已拒绝请求".to_owned(),
        ));
    }
    Ok((url, host, addresses))
}

fn is_public_network_address(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224)
        }
        std::net::IpAddr::V6(address) => {
            let segments = address.segments();
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && segments[1] == 0x0000)
                && !(segments[0] == 0x2001 && segments[1] == 0x0002)
                && !(segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0010)
                && !(segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0020)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
        }
    }
}

/// 解析 Bearer challenge 中的 `resource_metadata` 与 `scope` 参数。
pub fn parse_www_authenticate(value: &str) -> Result<OAuthChallenge, OAuthError> {
    let mut found_bearer = false;
    let mut collecting_bearer = false;
    let mut bearer_parameters = Vec::new();
    for item in split_challenge_parameters(value.trim())? {
        if item.is_empty() {
            return Err(OAuthError::InvalidDiscovery(
                "WWW-Authenticate 包含空 challenge 项".to_owned(),
            ));
        }
        if let Some((scheme, parameters)) = split_challenge_start(item) {
            if collecting_bearer {
                break;
            }
            collecting_bearer = scheme.eq_ignore_ascii_case("Bearer");
            if collecting_bearer {
                found_bearer = true;
                if !parameters.is_empty() {
                    bearer_parameters.push(parameters);
                }
            }
        } else if collecting_bearer {
            bearer_parameters.push(item);
        }
    }
    if !found_bearer {
        return Err(OAuthError::InvalidDiscovery(
            "WWW-Authenticate 不是 Bearer challenge".to_owned(),
        ));
    }

    let mut challenge = OAuthChallenge::default();
    let mut resource_metadata_seen = false;
    let mut scope_seen = false;
    for parameter in bearer_parameters {
        let (name, raw_value) = parameter.split_once('=').ok_or_else(|| {
            OAuthError::InvalidDiscovery("Bearer challenge 参数缺少等号".to_owned())
        })?;
        let decoded = raw_value
            .trim()
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(|| {
                OAuthError::InvalidDiscovery("Bearer challenge 参数必须使用引号".to_owned())
            })?
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
        match name.trim().to_ascii_lowercase().as_str() {
            "resource_metadata" => {
                if resource_metadata_seen {
                    return Err(OAuthError::InvalidDiscovery(
                        "Bearer challenge 重复 resource_metadata".to_owned(),
                    ));
                }
                resource_metadata_seen = true;
                challenge.resource_metadata = Some(decoded);
            }
            "scope" => {
                if scope_seen {
                    return Err(OAuthError::InvalidDiscovery(
                        "Bearer challenge 重复 scope".to_owned(),
                    ));
                }
                scope_seen = true;
                challenge.scopes = decoded.split_whitespace().map(str::to_owned).collect();
            }
            _ => {}
        }
    }
    if let Some(resource_metadata) = &challenge.resource_metadata {
        validate_metadata_endpoint("resource_metadata", resource_metadata)?;
    }
    Ok(challenge)
}

fn split_challenge_start(value: &str) -> Option<(&str, &str)> {
    let value = value.trim();
    let whitespace = value.find(char::is_whitespace);
    match whitespace {
        Some(index) => {
            let scheme = &value[..index];
            let remainder = value[index..].trim_start();
            if scheme.contains('=') || remainder.starts_with('=') {
                None
            } else {
                Some((scheme, remainder))
            }
        }
        None if value.contains('=') => None,
        None => Some((value, "")),
    }
}

/// 按 MCP 2025-11-25 顺序生成 RFC 9728 元数据候选 URL。
pub fn protected_resource_metadata_urls(
    resource: &str,
    challenge: Option<&OAuthChallenge>,
) -> Result<Vec<String>, OAuthError> {
    validate_https_endpoint("resource", resource)?;
    if let Some(explicit) = challenge.and_then(|challenge| challenge.resource_metadata.clone()) {
        validate_metadata_endpoint("resource_metadata", &explicit)?;
        return Ok(vec![explicit]);
    }
    let resource = Url::parse(resource)
        .map_err(|error| OAuthError::InvalidDiscovery(format!("resource 无效：{error}")))?;
    if resource.username() != "" || resource.password().is_some() || resource.fragment().is_some() {
        return Err(OAuthError::InvalidDiscovery(
            "resource 不得包含用户凭据或片段".to_owned(),
        ));
    }
    let path = resource.path().trim_matches('/');
    let mut urls = Vec::new();
    if !path.is_empty() {
        let mut path_metadata = resource.clone();
        path_metadata.set_query(None);
        path_metadata.set_fragment(None);
        path_metadata.set_path(&format!("/.well-known/oauth-protected-resource/{path}"));
        urls.push(path_metadata.into());
    }
    let mut root_metadata = resource;
    root_metadata.set_query(None);
    root_metadata.set_fragment(None);
    root_metadata.set_path("/.well-known/oauth-protected-resource");
    urls.push(root_metadata.into());
    Ok(urls)
}

/// 按 RFC 8414 与 OIDC 兼容顺序生成授权服务器元数据候选 URL。
pub fn authorization_server_metadata_urls(issuer: &str) -> Result<Vec<String>, OAuthError> {
    validate_issuer(issuer)?;
    let issuer = Url::parse(issuer)
        .map_err(|error| OAuthError::InvalidDiscovery(format!("issuer 无效：{error}")))?;
    let path = issuer.path().trim_matches('/').to_owned();
    let mut urls = Vec::new();
    let mut candidate = issuer.clone();
    candidate.set_query(None);
    candidate.set_fragment(None);
    let oauth_path = if path.is_empty() {
        "/.well-known/oauth-authorization-server".to_owned()
    } else {
        format!("/.well-known/oauth-authorization-server/{path}")
    };
    candidate.set_path(&oauth_path);
    urls.push(candidate.into());
    let mut oidc_inserted = issuer.clone();
    oidc_inserted.set_query(None);
    oidc_inserted.set_fragment(None);
    let oidc_path = if path.is_empty() {
        "/.well-known/openid-configuration".to_owned()
    } else {
        format!("/.well-known/openid-configuration/{path}")
    };
    oidc_inserted.set_path(&oidc_path);
    urls.push(oidc_inserted.into());
    if !path.is_empty() {
        let mut oidc_appended = issuer;
        oidc_appended.set_query(None);
        oidc_appended.set_fragment(None);
        oidc_appended.set_path(&format!("/{path}/.well-known/openid-configuration"));
        urls.push(oidc_appended.into());
    }
    Ok(urls)
}

fn split_challenge_parameters(value: &str) -> Result<Vec<&str>, OAuthError> {
    let mut parameters = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ',' && !quoted {
            parameters.push(value[start..index].trim());
            start = index + 1;
        }
    }
    if quoted || escaped {
        return Err(OAuthError::InvalidDiscovery(
            "Bearer challenge 引号没有闭合".to_owned(),
        ));
    }
    parameters.push(value[start..].trim());
    Ok(parameters)
}

fn is_loopback_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
