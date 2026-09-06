//! ACP Elicitation 能力协商与 Agent 到 Client 的类型安全出站路由。

use std::collections::HashSet;

use agent_client_protocol_schema::{
    AgentNotification, AgentRequest, ClientCapabilities, ClientResponse,
    CompleteElicitationNotification, CreateElicitationRequest, CreateElicitationResponse,
    ElicitationFormMode, ElicitationMode, ElicitationScope, ElicitationUrlMode, RequestId,
};
use url::{Host, Url};

use crate::AcpBoundaryError;
use crate::json::{JsonValueLimits, validate_identifier, validate_text, validate_value};

/// Elicitation 标识允许的最大 UTF-8 字节数。
const MAX_ELICITATION_ID_BYTES: usize = 256;
/// Elicitation 用户说明允许的最大 UTF-8 字节数。
const MAX_ELICITATION_MESSAGE_BYTES: usize = 16 * 1024;
/// Elicitation URL 允许的最大 UTF-8 字节数。
const MAX_ELICITATION_URL_BYTES: usize = 8 * 1024;
/// 表单 Schema 允许的最大规范 JSON 字节数。
const MAX_ELICITATION_SCHEMA_BYTES: usize = 64 * 1024;
/// 表单 Schema 允许的最大 JSON 容器嵌套层数。
const MAX_ELICITATION_SCHEMA_DEPTH: usize = 16;
/// 表单 Schema 允许的最大 JSON 节点数。
const MAX_ELICITATION_SCHEMA_NODES: usize = 4096;
/// 表单 Schema 允许的最大属性数。
const MAX_ELICITATION_PROPERTIES: usize = 128;

/// 从 InitializeRequest 的 ClientCapabilities 固化一次连接的 Elicitation 路由能力。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ElicitationRouter {
    /// Client 是否声明表单式 Elicitation。
    supports_form: bool,
    /// Client 是否声明 URL 式 Elicitation。
    supports_url: bool,
}

impl ElicitationRouter {
    /// 从标准 ClientCapabilities 创建一次连接的不可变 Elicitation 能力快照。
    pub fn from_client_capabilities(capabilities: &ClientCapabilities) -> Self {
        let elicitation = capabilities.elicitation.as_ref();
        Self {
            supports_form: elicitation.is_some_and(|value| value.form.is_some()),
            supports_url: elicitation.is_some_and(|value| value.url.is_some()),
        }
    }

    /// 返回 Client 是否声明表单式 Elicitation。
    pub const fn supports_form(&self) -> bool {
        self.supports_form
    }

    /// 返回 Client 是否声明 URL 式 Elicitation。
    pub const fn supports_url(&self) -> bool {
        self.supports_url
    }

    /// 校验能力和有界字段后，通过标准 AgentRequest 路由 Elicitation 创建请求。
    pub fn route_create_request(
        &self,
        request: CreateElicitationRequest,
    ) -> Result<AgentRequest, AcpBoundaryError> {
        validate_text(&request.message, MAX_ELICITATION_MESSAGE_BYTES)?;
        match &request.mode {
            ElicitationMode::Form(mode) if self.supports_form => validate_form_mode(mode)?,
            ElicitationMode::Url(mode) if self.supports_url => {
                validate_url_mode(mode)?;
            }
            _ => return Err(AcpBoundaryError::CapabilityNotAdvertised),
        }
        Ok(AgentRequest::CreateElicitationRequest(request))
    }

    /// 通过标准 ClientResponse 路由 Client 对 Elicitation 的响应。
    pub fn route_create_response(response: CreateElicitationResponse) -> ClientResponse {
        ClientResponse::CreateElicitationResponse(response)
    }

    /// 校验 URL 能力和标识后，通过标准 AgentNotification 路由完成通知。
    pub fn route_complete_notification(
        &self,
        notification: CompleteElicitationNotification,
    ) -> Result<AgentNotification, AcpBoundaryError> {
        if !self.supports_url {
            return Err(AcpBoundaryError::CapabilityNotAdvertised);
        }
        validate_identifier(&notification.elicitation_id.0, MAX_ELICITATION_ID_BYTES)?;
        Ok(AgentNotification::CompleteElicitationNotification(
            notification,
        ))
    }
}

/// 校验表单作用域、Schema 总资源上限和 required 引用一致性。
fn validate_form_mode(mode: &ElicitationFormMode) -> Result<(), AcpBoundaryError> {
    validate_scope(&mode.scope)?;
    let schema = &mode.requested_schema;
    if schema.properties.len() > MAX_ELICITATION_PROPERTIES {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    if let Some(title) = &schema.title {
        validate_text(title, MAX_ELICITATION_MESSAGE_BYTES)?;
    }
    if let Some(description) = &schema.description {
        validate_text(description, MAX_ELICITATION_MESSAGE_BYTES)?;
    }
    for name in schema.properties.keys() {
        validate_identifier(name, MAX_ELICITATION_ID_BYTES)?;
    }
    if let Some(required) = &schema.required {
        if required.len() > schema.properties.len() {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        let mut unique = HashSet::with_capacity(required.len());
        for name in required {
            validate_identifier(name, MAX_ELICITATION_ID_BYTES)?;
            if !schema.properties.contains_key(name) || !unique.insert(name) {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
        }
    }

    let value = serde_json::to_value(schema).map_err(|_| AcpBoundaryError::InvalidParams)?;
    validate_value(
        &value,
        JsonValueLimits {
            max_bytes: MAX_ELICITATION_SCHEMA_BYTES,
            max_depth: MAX_ELICITATION_SCHEMA_DEPTH,
            max_nodes: MAX_ELICITATION_SCHEMA_NODES,
        },
    )
}

/// 校验 URL 式问答的作用域、标识与可安全打开的 URL。
fn validate_url_mode(mode: &ElicitationUrlMode) -> Result<(), AcpBoundaryError> {
    validate_scope(&mode.scope)?;
    validate_identifier(&mode.elicitation_id.0, MAX_ELICITATION_ID_BYTES)?;
    validate_identifier(&mode.url, MAX_ELICITATION_URL_BYTES)?;
    let parsed = Url::parse(&mode.url).map_err(|_| AcpBoundaryError::InvalidSemanticValue)?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    let Some(host) = parsed.host() else {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    };
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(host) => Ok(()),
        _ => Err(AcpBoundaryError::InvalidSemanticValue),
    }
}

/// 校验 Elicitation 只绑定有界 Session、Tool Call 或 JSON-RPC 请求标识。
fn validate_scope(scope: &ElicitationScope) -> Result<(), AcpBoundaryError> {
    match scope {
        ElicitationScope::Session(scope) => {
            validate_identifier(&scope.session_id.0, MAX_ELICITATION_ID_BYTES)?;
            if let Some(tool_call_id) = &scope.tool_call_id {
                validate_identifier(&tool_call_id.0, MAX_ELICITATION_ID_BYTES)?;
            }
            Ok(())
        }
        ElicitationScope::Request(scope) => match &scope.request_id {
            RequestId::Str(value) => validate_identifier(value, MAX_ELICITATION_ID_BYTES),
            RequestId::Null | RequestId::Number(_) => Ok(()),
        },
        _ => Err(AcpBoundaryError::InvalidSemanticValue),
    }
}

/// 判断 HTTP host 是否确定指向本机回环地址。
fn is_loopback_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain == "localhost" || domain.ends_with(".localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}
