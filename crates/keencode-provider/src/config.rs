use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use keencode_model::{ProviderCapabilities, ProviderProtocol};
use reqwest::Url;
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// 模型服务凭据允许的最大 UTF-8 字节数，避免异常配置长期占用大块敏感内存。
const MAX_API_KEY_BYTES: usize = 16 * 1024;
/// Provider 稳定标识允许的最大 UTF-8 字节数。
const MAX_PROVIDER_ID_BYTES: usize = 256;

/// 不会通过 `Debug` 或 `Display` 泄露明文的模型服务凭据。
#[derive(Clone, Eq, PartialEq)]
pub struct ApiKey(String);

impl ApiKey {
    /// 从非空字符串创建模型服务凭据。
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderConfigError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ProviderConfigError::EmptyApiKey);
        }
        if value.len() > MAX_API_KEY_BYTES || value.chars().any(char::is_control) {
            return Err(ProviderConfigError::InvalidApiKey);
        }
        Ok(Self(value))
    }

    /// 仅供认证 Header 构造使用地读取明文凭据。
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// 从错误文本中移除当前凭据的完整明文。
    pub(crate) fn redact(&self, text: &str) -> String {
        text.replace(&self.0, "[REDACTED]")
    }
}

impl fmt::Debug for ApiKey {
    /// 始终输出固定脱敏占位符。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKey([REDACTED])")
    }
}

impl Drop for ApiKey {
    /// 在最后一个凭据副本离开内存前覆盖其字符串缓冲区。
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Adapter 在 HTTP 线上请求增量事件还是完整 JSON。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireResponseMode {
    /// 请求 SSE 增量响应；Agent Runtime 的默认模式。
    #[default]
    Streaming,
    /// 请求一个完整 JSON 响应；主要用于协议验证和受限服务。
    Buffered,
}

impl WireResponseMode {
    /// 返回协议请求体中 `stream` 字段应使用的布尔值。
    pub const fn is_streaming(self) -> bool {
        matches!(self, Self::Streaming)
    }
}

/// 三种协议和模型目录的端点路径。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEndpoints {
    /// Anthropic Messages 资源路径。
    pub messages: String,
    /// OpenAI Chat Completions 资源路径。
    pub chat_completions: String,
    /// OpenAI Responses 资源路径。
    pub responses: String,
    /// 模型目录资源路径。
    pub models: String,
}

impl Default for ProviderEndpoints {
    /// 返回协议标准资源名称，路径前缀由 `base_url` 提供。
    fn default() -> Self {
        Self {
            messages: "messages".to_owned(),
            chat_completions: "chat/completions".to_owned(),
            responses: "responses".to_owned(),
            models: "models".to_owned(),
        }
    }
}

impl ProviderEndpoints {
    /// 返回指定协议使用的资源路径。
    pub fn for_protocol(&self, protocol: ProviderProtocol) -> &str {
        match protocol {
            ProviderProtocol::Messages => &self.messages,
            ProviderProtocol::ChatCompletions => &self.chat_completions,
            ProviderProtocol::Responses => &self.responses,
        }
    }

    /// 校验所有路径都是不含源站替换语义的相对资源路径。
    pub fn validate(&self) -> Result<(), ProviderConfigError> {
        for (name, value) in [
            ("messages", self.messages.as_str()),
            ("chat_completions", self.chat_completions.as_str()),
            ("responses", self.responses.as_str()),
            ("models", self.models.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ProviderConfigError::InvalidEndpoint {
                    name: name.to_owned(),
                    message: "路径不能为空".to_owned(),
                });
            }
            if value.starts_with('/') || value.contains("//") || value.contains(['?', '#']) {
                return Err(ProviderConfigError::InvalidEndpoint {
                    name: name.to_owned(),
                    message: "路径必须是不含查询或片段的相对资源路径".to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// 构造一个模型 Provider Adapter 所需的完整配置。
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    /// 用户配置中的稳定 Provider 标识。
    pub id: String,
    /// 当前实例发送和解析的唯一协议。
    pub protocol: ProviderProtocol,
    /// 保留反向代理路径前缀且以斜杠结尾的基础地址。
    base_url: Url,
    /// 不通过调试输出暴露的可选认证凭据；本机或显式无认证端点为 `None`。
    api_key: Option<ApiKey>,
    /// 可由用户覆盖的协议资源路径。
    pub endpoints: ProviderEndpoints,
    /// 建立 TCP/TLS 连接的最大等待时间。
    pub connect_timeout: Duration,
    /// 一次模型请求从发送到完成的最大时间。
    pub request_timeout: Duration,
    /// 单个 JSON 或 SSE 事件允许的最大字节数。
    pub max_event_bytes: usize,
    /// 一次流式或缓冲模型响应在 HTTP 线上允许读取的累计最大字节数。
    pub max_response_bytes: usize,
    /// 一次模型目录分页链在 HTTP 线上允许读取的累计最大字节数。
    pub max_catalog_bytes: usize,
    /// HTTP 线上采用增量 SSE 还是完整 JSON 响应。
    pub response_mode: WireResponseMode,
    /// 模型目录分页允许的最大页数，用于阻止错误游标形成循环。
    pub max_catalog_pages: usize,
    /// 未登记模型采用的保守能力快照。
    pub default_capabilities: ProviderCapabilities,
    /// 按精确模型标识覆盖的能力快照。
    pub model_capabilities: BTreeMap<String, ProviderCapabilities>,
}

impl ProviderConfig {
    /// 创建使用默认端点和超时的 Provider 配置。
    pub fn new(
        id: impl Into<String>,
        protocol: ProviderProtocol,
        base_url: impl AsRef<str>,
        api_key: ApiKey,
    ) -> Result<Self, ProviderConfigError> {
        Self::new_with_authentication(id, protocol, base_url, Some(api_key))
    }

    /// 创建不发送认证 Header 且使用默认端点和超时的 Provider 配置。
    pub fn new_unauthenticated(
        id: impl Into<String>,
        protocol: ProviderProtocol,
        base_url: impl AsRef<str>,
    ) -> Result<Self, ProviderConfigError> {
        Self::new_with_authentication(id, protocol, base_url, None)
    }

    /// 创建带显式认证策略且使用默认端点和超时的 Provider 配置。
    fn new_with_authentication(
        id: impl Into<String>,
        protocol: ProviderProtocol,
        base_url: impl AsRef<str>,
        api_key: Option<ApiKey>,
    ) -> Result<Self, ProviderConfigError> {
        let id = id.into();
        validate_provider_id(&id)?;
        let mut base_url =
            Url::parse(base_url.as_ref()).map_err(|error| ProviderConfigError::InvalidBaseUrl {
                message: error.to_string(),
            })?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(ProviderConfigError::InvalidBaseUrl {
                message: "只允许 http 或 https 地址".to_owned(),
            });
        }
        if base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ProviderConfigError::InvalidBaseUrl {
                message: "地址必须包含主机且不能包含用户信息、查询或片段".to_owned(),
            });
        }
        if base_url.scheme() == "http" && !is_loopback_host(&base_url) {
            return Err(ProviderConfigError::InvalidBaseUrl {
                message: "远程 Provider 必须使用 HTTPS；HTTP 仅允许本机回环地址".to_owned(),
            });
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }

        Ok(Self {
            id,
            protocol,
            base_url,
            api_key,
            endpoints: ProviderEndpoints::default(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(300),
            max_event_bytes: 16 * 1024 * 1024,
            max_response_bytes: 64 * 1024 * 1024,
            max_catalog_bytes: 64 * 1024 * 1024,
            response_mode: WireResponseMode::Streaming,
            max_catalog_pages: 1000,
            default_capabilities: ProviderCapabilities::default(),
            model_capabilities: BTreeMap::new(),
        })
    }

    /// 返回不包含凭据的规范化基础地址。
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// 返回认证凭据的受限内部引用；无认证 Provider 返回 `None`。
    pub(crate) fn api_key(&self) -> Option<&ApiKey> {
        self.api_key.as_ref()
    }

    /// 返回当前 Provider 是否会发送协议对应的认证 Header。
    pub fn has_authentication(&self) -> bool {
        self.api_key.is_some()
    }

    /// 生成覆盖全部非敏感传输字段和模型能力、但不表示凭据身份的稳定 SHA-256 摘要。
    pub fn transport_fingerprint(&self) -> Result<String, ProviderConfigError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct FingerprintInput<'a> {
            /// 摘要契约版本。
            version: u32,
            /// Provider 稳定标识。
            id: &'a str,
            /// 唯一厂商协议。
            protocol: ProviderProtocol,
            /// 规范化且不含凭据的基础地址。
            base_url: &'a str,
            /// 是否配置了认证凭据，不包含凭据正文。
            authenticated: bool,
            /// 三协议与模型目录端点。
            endpoints: &'a ProviderEndpoints,
            /// TCP/TLS 建连超时秒与纳秒部分。
            connect_timeout: (u64, u32),
            /// 完整请求超时秒与纳秒部分。
            request_timeout: (u64, u32),
            /// 单事件字节上限。
            max_event_bytes: usize,
            /// 单响应累计字节上限。
            max_response_bytes: usize,
            /// 模型目录累计字节上限。
            max_catalog_bytes: usize,
            /// 线上响应模式。
            response_mode: WireResponseMode,
            /// 模型目录最大页数。
            max_catalog_pages: usize,
            /// 未登记模型能力。
            default_capabilities: &'a ProviderCapabilities,
            /// 精确模型能力覆盖。
            model_capabilities: &'a BTreeMap<String, ProviderCapabilities>,
        }

        self.validate()?;
        let input = FingerprintInput {
            version: 1,
            id: &self.id,
            protocol: self.protocol,
            base_url: self.base_url.as_str(),
            authenticated: self.has_authentication(),
            endpoints: &self.endpoints,
            connect_timeout: (
                self.connect_timeout.as_secs(),
                self.connect_timeout.subsec_nanos(),
            ),
            request_timeout: (
                self.request_timeout.as_secs(),
                self.request_timeout.subsec_nanos(),
            ),
            max_event_bytes: self.max_event_bytes,
            max_response_bytes: self.max_response_bytes,
            max_catalog_bytes: self.max_catalog_bytes,
            response_mode: self.response_mode,
            max_catalog_pages: self.max_catalog_pages,
            default_capabilities: &self.default_capabilities,
            model_capabilities: &self.model_capabilities,
        };
        let encoded = serde_json::to_vec(&input).map_err(|error| {
            ProviderConfigError::TransportFingerprintEncoding {
                message: error.to_string(),
            }
        })?;
        let digest = Sha256::digest(encoded);
        let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
        output.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut output, "{byte:02x}");
        }
        Ok(output)
    }

    /// 构造当前协议的完整资源地址。
    pub(crate) fn protocol_url(&self) -> Result<Url, ProviderConfigError> {
        self.endpoints.validate()?;
        self.join_endpoint(
            &format!("{:?}", self.protocol),
            self.endpoints.for_protocol(self.protocol),
        )
    }

    /// 构造模型目录的完整资源地址。
    pub(crate) fn models_url(&self) -> Result<Url, ProviderConfigError> {
        self.endpoints.validate()?;
        self.join_endpoint("models", &self.endpoints.models)
    }

    /// 返回指定模型的能力覆盖，未登记时使用默认快照。
    pub fn capabilities_for(&self, model: &str) -> ProviderCapabilities {
        self.model_capabilities
            .get(model)
            .cloned()
            .unwrap_or_else(|| self.default_capabilities.clone())
    }

    /// 校验全部配置字段可以安全构造 HTTP 请求。
    pub fn validate(&self) -> Result<(), ProviderConfigError> {
        validate_provider_id(&self.id)?;
        if self.max_event_bytes == 0 {
            return Err(ProviderConfigError::ZeroEventLimit);
        }
        if self.max_response_bytes < self.max_event_bytes {
            return Err(ProviderConfigError::ResponseByteLimitTooSmall);
        }
        if self.connect_timeout.is_zero() {
            return Err(ProviderConfigError::ZeroConnectTimeout);
        }
        if self.request_timeout.is_zero() {
            return Err(ProviderConfigError::ZeroRequestTimeout);
        }
        if self.max_catalog_bytes < self.max_event_bytes {
            return Err(ProviderConfigError::CatalogByteLimitTooSmall);
        }
        if self.max_catalog_pages == 0 {
            return Err(ProviderConfigError::ZeroCatalogPageLimit);
        }
        self.endpoints.validate()?;
        self.protocol_url()?;
        Ok(())
    }

    /// 在保留基础路径的前提下拼接端点，并拒绝源站替换或路径逃逸。
    fn join_endpoint(&self, name: &str, endpoint: &str) -> Result<Url, ProviderConfigError> {
        let joined =
            self.base_url
                .join(endpoint)
                .map_err(|error| ProviderConfigError::InvalidEndpoint {
                    name: name.to_owned(),
                    message: error.to_string(),
                })?;
        if joined.origin() != self.base_url.origin()
            || !joined.path().starts_with(self.base_url.path())
            || !joined.username().is_empty()
            || joined.password().is_some()
            || joined.query().is_some()
            || joined.fragment().is_some()
        {
            return Err(ProviderConfigError::InvalidEndpoint {
                name: name.to_owned(),
                message: "端点不得替换 Provider 源站或逃逸基础路径".to_owned(),
            });
        }
        Ok(joined)
    }
}

/// 校验 Provider 标识可安全进入结构化查询、日志和注册表键。
pub(crate) fn validate_provider_id(id: &str) -> Result<(), ProviderConfigError> {
    if id.trim().is_empty() {
        return Err(ProviderConfigError::EmptyProviderId);
    }
    if id.trim() != id
        || id.len() > MAX_PROVIDER_ID_BYTES
        || id.chars().any(is_dangerous_identifier_character)
    {
        return Err(ProviderConfigError::InvalidProviderId);
    }
    Ok(())
}

/// 判断字符是否会让稳定标识跨日志、终端或界面显示时产生换行、隐藏或双向覆盖。
pub(crate) fn is_dangerous_identifier_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2069}'
                | '\u{feff}'
        )
}

/// 判断 URL 主机是否是无需 TLS 即可安全携带凭据的本机回环地址。
fn is_loopback_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

/// Provider 配置无法安全使用时返回的错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderConfigError {
    /// Provider 稳定标识为空。
    EmptyProviderId,
    /// Provider 标识包含危险显示字符、边界空白或超过字节上限。
    InvalidProviderId,
    /// API Key 为空。
    EmptyApiKey,
    /// API Key 包含控制字符或超过安全字节上限。
    InvalidApiKey,
    /// 基础地址不是可接受的 HTTP(S) 地址。
    InvalidBaseUrl {
        /// 不包含凭据的失败说明。
        message: String,
    },
    /// 端点资源路径无效。
    InvalidEndpoint {
        /// 配置字段名称。
        name: String,
        /// 不包含凭据的失败说明。
        message: String,
    },
    /// SSE 或 JSON 事件字节上限不能为零。
    ZeroEventLimit,
    /// 模型响应累计字节上限不能小于单事件上限。
    ResponseByteLimitTooSmall,
    /// HTTP 建连超时不能为零。
    ZeroConnectTimeout,
    /// 完整请求超时不能为零。
    ZeroRequestTimeout,
    /// 模型目录累计字节上限不能小于单页响应上限。
    CatalogByteLimitTooSmall,
    /// 模型目录分页上限不能为零。
    ZeroCatalogPageLimit,
    /// HTTP 客户端无法按配置创建。
    HttpClient {
        /// 不包含凭据的失败说明。
        message: String,
    },
    /// 非敏感传输配置无法编码为稳定摘要输入。
    TransportFingerprintEncoding {
        /// 不包含凭据的序列化失败说明。
        message: String,
    },
}

impl fmt::Display for ProviderConfigError {
    /// 输出适合配置界面展示且不包含凭据的说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProviderId => formatter.write_str("Provider 标识不能为空"),
            Self::InvalidProviderId => formatter
                .write_str("Provider 标识不能包含危险显示字符或边界空白，且不得超过安全上限"),
            Self::EmptyApiKey => formatter.write_str("API Key 不能为空"),
            Self::InvalidApiKey => formatter.write_str("API Key 包含控制字符或超过安全字节上限"),
            Self::InvalidBaseUrl { message } => write!(formatter, "Provider 地址无效：{message}"),
            Self::InvalidEndpoint { name, message } => {
                write!(formatter, "Provider 端点 {name} 无效：{message}")
            }
            Self::ZeroEventLimit => formatter.write_str("Provider 事件字节上限必须大于零"),
            Self::ResponseByteLimitTooSmall => {
                formatter.write_str("Provider 响应累计字节上限不得小于单事件上限")
            }
            Self::ZeroConnectTimeout => formatter.write_str("Provider 建连超时必须大于零"),
            Self::ZeroRequestTimeout => formatter.write_str("Provider 请求超时必须大于零"),
            Self::CatalogByteLimitTooSmall => {
                formatter.write_str("Provider 模型目录累计字节上限不得小于单页响应上限")
            }
            Self::ZeroCatalogPageLimit => {
                formatter.write_str("Provider 模型目录分页上限必须大于零")
            }
            Self::HttpClient { message } => write!(formatter, "HTTP 客户端创建失败：{message}"),
            Self::TransportFingerprintEncoding { message } => {
                write!(formatter, "Provider 非敏感传输摘要编码失败：{message}")
            }
        }
    }
}

impl Error for ProviderConfigError {}
