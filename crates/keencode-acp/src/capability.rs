//! Rust 1.85 兼容 ACP Schema 缺失能力的类型安全初始化响应 DTO。

use std::collections::HashSet;

use agent_client_protocol_schema::{
    AuthMethod, AuthenticateRequest, Implementation, McpCapabilities, Meta, PromptCapabilities,
    ProtocolVersion, SessionForkCapabilities, SessionListCapabilities, SessionModeState,
    SetSessionModeRequest,
};
use serde::{Deserialize, Serialize};

use crate::AcpBoundaryError;
use crate::json::validate_identifier;

/// 标准 ACP 认证方式、Session 和模式标识的最大 UTF-8 字节数。
const MAX_CAPABILITY_IDENTIFIER_BYTES: usize = 256;

/// 标准 `session/delete` 能力在 Schema 0.11.7 中的精确补充类型。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionDeleteCapabilities {
    /// ACP 为调用双方保留且不由 KeenCode 解释的扩展元数据。
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl SessionDeleteCapabilities {
    /// 创建不带扩展元数据的 Session 删除能力声明。
    pub fn new() -> Self {
        Self::default()
    }

    /// 附加由 ACP 保留且 KeenCode 不解释的扩展元数据。
    #[must_use]
    pub fn meta(mut self, meta: impl Into<Option<Meta>>) -> Self {
        self.meta = meta.into();
        self
    }
}

/// 初始化响应中类型安全且始终声明 `sessionCapabilities.delete` 的 Session 能力。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeSessionCapabilitiesDto {
    /// 是否支持标准 `session/list`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list: Option<SessionListCapabilities>,
    /// 是否支持当前启用的标准 `session/fork`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<SessionForkCapabilities>,
    /// 始终类型化声明标准 `session/delete` 能力。
    pub delete: SessionDeleteCapabilities,
    /// ACP 为调用双方保留且不由 KeenCode 解释的扩展元数据。
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl InitializeSessionCapabilitiesDto {
    /// 创建只声明 `session/delete` 的最小 Session 能力。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置标准 `session/list` 能力。
    #[must_use]
    pub fn list(mut self, list: impl Into<Option<SessionListCapabilities>>) -> Self {
        self.list = list.into();
        self
    }

    /// 设置标准 `session/fork` 能力。
    #[must_use]
    pub fn fork(mut self, fork: impl Into<Option<SessionForkCapabilities>>) -> Self {
        self.fork = fork.into();
        self
    }

    /// 附加由 ACP 保留且 KeenCode 不解释的扩展元数据。
    #[must_use]
    pub fn meta(mut self, meta: impl Into<Option<Meta>>) -> Self {
        self.meta = meta.into();
        self
    }
}

impl Default for InitializeSessionCapabilitiesDto {
    /// 创建只声明标准删除能力的 Session 能力。
    fn default() -> Self {
        Self {
            list: None,
            fork: None,
            delete: SessionDeleteCapabilities::new(),
            meta: None,
        }
    }
}

/// 初始化响应中包含标准字段和补充 Session 能力的 Agent 能力 DTO。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeAgentCapabilitiesDto {
    /// 是否支持标准 `session/load`。
    #[serde(default)]
    pub load_session: bool,
    /// Agent 支持的 Prompt 内容能力。
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    /// Agent 支持的标准 MCP 能力。
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
    /// 标准 Session 能力和强类型删除能力。
    #[serde(default)]
    pub session_capabilities: InitializeSessionCapabilitiesDto,
    /// ACP 为调用双方保留且不由 KeenCode 解释的扩展元数据。
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl InitializeAgentCapabilitiesDto {
    /// 创建仅声明标准 Session 删除能力的 Agent 能力。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置是否支持标准 `session/load`。
    #[must_use]
    pub const fn load_session(mut self, load_session: bool) -> Self {
        self.load_session = load_session;
        self
    }

    /// 设置 Agent 支持的 Prompt 内容能力。
    #[must_use]
    pub fn prompt_capabilities(mut self, prompt_capabilities: PromptCapabilities) -> Self {
        self.prompt_capabilities = prompt_capabilities;
        self
    }

    /// 设置 Agent 支持的标准 MCP 能力。
    #[must_use]
    pub fn mcp_capabilities(mut self, mcp_capabilities: McpCapabilities) -> Self {
        self.mcp_capabilities = mcp_capabilities;
        self
    }

    /// 设置标准 Session 能力和强类型删除能力。
    #[must_use]
    pub fn session_capabilities(
        mut self,
        session_capabilities: InitializeSessionCapabilitiesDto,
    ) -> Self {
        self.session_capabilities = session_capabilities;
        self
    }

    /// 附加由 ACP 保留且 KeenCode 不解释的扩展元数据。
    #[must_use]
    pub fn meta(mut self, meta: impl Into<Option<Meta>>) -> Self {
        self.meta = meta.into();
        self
    }
}

impl Default for InitializeAgentCapabilitiesDto {
    /// 创建仅声明标准 Session 删除能力的 Agent 能力。
    fn default() -> Self {
        Self {
            load_session: false,
            prompt_capabilities: PromptCapabilities::default(),
            mcp_capabilities: McpCapabilities::default(),
            session_capabilities: InitializeSessionCapabilitiesDto::default(),
            meta: None,
        }
    }
}

/// 初始化响应 DTO，补齐 Schema 0.11.7 尚未提供的 `session/delete` 能力字段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeResponseDto {
    /// 双方协商使用的 ACP 协议版本。
    pub protocol_version: ProtocolVersion,
    /// Agent 支持的标准能力与类型化删除能力。
    #[serde(default)]
    pub agent_capabilities: InitializeAgentCapabilitiesDto,
    /// Agent 支持的认证方式。
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    /// Agent 实现名称和版本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Implementation>,
    /// ACP 为调用双方保留且不由 KeenCode 解释的扩展元数据。
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl InitializeResponseDto {
    /// 创建始终声明标准 `session/delete` 能力的初始化响应。
    pub fn new(protocol_version: ProtocolVersion) -> Self {
        Self {
            protocol_version,
            agent_capabilities: InitializeAgentCapabilitiesDto::default(),
            auth_methods: Vec::new(),
            agent_info: None,
            meta: None,
        }
    }

    /// 设置 Agent 支持的标准能力与类型化删除能力。
    #[must_use]
    pub fn agent_capabilities(
        mut self,
        agent_capabilities: InitializeAgentCapabilitiesDto,
    ) -> Self {
        self.agent_capabilities = agent_capabilities;
        self
    }

    /// 设置 Agent 支持的认证方式。
    #[must_use]
    pub fn auth_methods(mut self, auth_methods: Vec<AuthMethod>) -> Self {
        self.auth_methods = auth_methods;
        self
    }

    /// 设置 Agent 实现名称和版本。
    #[must_use]
    pub fn agent_info(mut self, agent_info: impl Into<Option<Implementation>>) -> Self {
        self.agent_info = agent_info.into();
        self
    }

    /// 附加由 ACP 保留且 KeenCode 不解释的扩展元数据。
    #[must_use]
    pub fn meta(mut self, meta: impl Into<Option<Meta>>) -> Self {
        self.meta = meta.into();
        self
    }

    /// 校验认证方式标识有界且在初始化响应中唯一。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        let mut identifiers = HashSet::with_capacity(self.auth_methods.len());
        for method in &self.auth_methods {
            let identifier = method.id().0.as_ref();
            validate_identifier(identifier, MAX_CAPABILITY_IDENTIFIER_BYTES)?;
            if !identifiers.insert(identifier) {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
        }
        Ok(())
    }

    /// 确认 `authenticate` 请求只选择本初始化响应已公布的认证方式。
    pub fn validate_authenticate_request(
        &self,
        request: &AuthenticateRequest,
    ) -> Result<(), AcpBoundaryError> {
        self.validate()?;
        validate_identifier(&request.method_id.0, MAX_CAPABILITY_IDENTIFIER_BYTES)?;
        if self
            .auth_methods
            .iter()
            .any(|method| method.id() == &request.method_id)
        {
            Ok(())
        } else {
            Err(AcpBoundaryError::CapabilityNotAdvertised)
        }
    }
}

/// 校验 Session 模式状态自一致，且 `session/set_mode` 只选择可用模式。
pub fn validate_set_session_mode_request(
    request: &SetSessionModeRequest,
    state: &SessionModeState,
) -> Result<(), AcpBoundaryError> {
    validate_identifier(&request.session_id.0, MAX_CAPABILITY_IDENTIFIER_BYTES)?;
    validate_identifier(&request.mode_id.0, MAX_CAPABILITY_IDENTIFIER_BYTES)?;
    validate_identifier(&state.current_mode_id.0, MAX_CAPABILITY_IDENTIFIER_BYTES)?;
    if state.available_modes.is_empty() {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }

    let mut available = HashSet::with_capacity(state.available_modes.len());
    for mode in &state.available_modes {
        let identifier = mode.id.0.as_ref();
        validate_identifier(identifier, MAX_CAPABILITY_IDENTIFIER_BYTES)?;
        if !available.insert(identifier) {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
    }
    if !available.contains(state.current_mode_id.0.as_ref()) {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    if available.contains(request.mode_id.0.as_ref()) {
        Ok(())
    } else {
        Err(AcpBoundaryError::CapabilityNotAdvertised)
    }
}
