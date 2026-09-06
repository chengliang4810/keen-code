//! 标准 ACP `SessionUpdate` 从 Runtime 投递到桌面层的严格信封。

use serde::{Deserialize, Serialize};

use crate::json::{JsonValueLimits, input_preserved, parse_raw_value, validate_identifier};
use crate::{AcpBoundaryError, schema};

/// 当前标准 Session 更新投递信封的 Schema 版本。
pub const SESSION_UPDATE_DELIVERY_SCHEMA_VERSION: u16 = 1;
/// 投递信封中标识允许的最大 UTF-8 字节数。
const MAX_DELIVERY_IDENTIFIER_BYTES: usize = 256;

/// 从不可信原始字节恢复单个 Session 更新投递时使用的资源边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionUpdateDeliveryLimits {
    /// 单个投递信封允许的最大原始 JSON 字节数。
    max_bytes: usize,
    /// 投递 JSON 允许的最大容器嵌套层数。
    max_depth: usize,
    /// 投递 JSON 允许的最大值节点数。
    max_nodes: usize,
}

impl SessionUpdateDeliveryLimits {
    /// 创建所有上限均大于零的 Session 更新恢复边界。
    pub const fn new(
        max_bytes: usize,
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<Self, AcpBoundaryError> {
        if max_bytes == 0 || max_depth == 0 || max_nodes == 0 {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self {
            max_bytes,
            max_depth,
            max_nodes,
        })
    }

    /// 返回单个投递信封允许的最大原始 JSON 字节数。
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// 返回投递 JSON 允许的最大容器嵌套层数。
    pub const fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// 返回投递 JSON 允许的最大值节点数。
    pub const fn max_nodes(&self) -> usize {
        self.max_nodes
    }

    /// 转换为共享严格 JSON 解码边界。
    const fn json_limits(self) -> JsonValueLimits {
        JsonValueLimits {
            max_bytes: self.max_bytes,
            max_depth: self.max_depth,
            max_nodes: self.max_nodes,
        }
    }
}

impl Default for SessionUpdateDeliveryLimits {
    /// 返回适合单条桌面 Session 更新的保守边界。
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_depth: 32,
            max_nodes: 16_384,
        }
    }
}

/// 一条带可信 Session、Turn、Agent 身份和独立投递序号的标准 ACP 更新。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateDeliveryEnvelope {
    /// 投递信封 Schema 版本，首版固定为一。
    schema_version: u16,
    /// 根 Session 的稳定标识。
    session_id: String,
    /// 产生更新的 Turn；Session 级更新没有该值。
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    /// 产生更新的根 Agent 或单层子 Agent；Session 级更新没有该值。
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_id: Option<String>,
    /// Session 当前 Runtime 内严格单调的投递序号。
    delivery_sequence: u64,
    /// 更新发生时的 UTC Unix 毫秒时间。
    occurred_at_ms: u64,
    /// 原样保留的标准 ACP Session 更新。
    update: schema::SessionUpdate,
}

impl SessionUpdateDeliveryEnvelope {
    /// 创建首版 Session 更新投递信封，并执行全部字段和身份校验。
    pub fn new(
        session_id: impl Into<String>,
        turn_id: Option<String>,
        source_agent_id: Option<String>,
        delivery_sequence: u64,
        occurred_at_ms: u64,
        update: schema::SessionUpdate,
    ) -> Result<Self, AcpBoundaryError> {
        Self::restore(
            SESSION_UPDATE_DELIVERY_SCHEMA_VERSION,
            session_id,
            turn_id,
            source_agent_id,
            delivery_sequence,
            occurred_at_ms,
            update,
        )
    }

    /// 从持久或跨边界数据恢复信封，并拒绝未知版本和非法身份。
    pub fn restore(
        schema_version: u16,
        session_id: impl Into<String>,
        turn_id: Option<String>,
        source_agent_id: Option<String>,
        delivery_sequence: u64,
        occurred_at_ms: u64,
        update: schema::SessionUpdate,
    ) -> Result<Self, AcpBoundaryError> {
        let envelope = Self {
            schema_version,
            session_id: session_id.into(),
            turn_id,
            source_agent_id,
            delivery_sequence,
            occurred_at_ms,
            update,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// 从未解析原始 JSON 恢复投递，并在分配 DOM 前执行默认资源边界。
    pub fn decode_raw(raw: &[u8]) -> Result<Self, AcpBoundaryError> {
        Self::decode_raw_with_limits(raw, SessionUpdateDeliveryLimits::default())
    }

    /// 从未解析原始 JSON 恢复投递，并拒绝超限、重复键、未知字段和非法线格式。
    pub fn decode_raw_with_limits(
        raw: &[u8],
        limits: SessionUpdateDeliveryLimits,
    ) -> Result<Self, AcpBoundaryError> {
        let value = parse_raw_value(raw, limits.json_limits())?;
        let wire = serde_json::from_value::<SessionUpdateDeliveryEnvelopeWire>(value.clone())
            .map_err(|_| AcpBoundaryError::InvalidParams)?;
        let envelope = Self::restore(
            wire.schema_version,
            wire.session_id,
            wire.turn_id,
            wire.source_agent_id,
            wire.delivery_sequence,
            wire.occurred_at_ms,
            wire.update,
        )?;
        let normalized =
            serde_json::to_value(&envelope).map_err(|_| AcpBoundaryError::InvalidParams)?;
        if !input_preserved(&value, &normalized) {
            return Err(AcpBoundaryError::InvalidParams);
        }
        Ok(envelope)
    }

    /// 校验版本、非零投递序号和时间，以及 Turn/Agent 身份成对不变量。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        if self.schema_version != SESSION_UPDATE_DELIVERY_SCHEMA_VERSION
            || self.delivery_sequence == 0
            || self.occurred_at_ms == 0
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        validate_identifier(&self.session_id, MAX_DELIVERY_IDENTIFIER_BYTES)?;
        if self.turn_id.is_some() != self.source_agent_id.is_some() {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        let session_scoped = matches!(
            &self.update,
            schema::SessionUpdate::AvailableCommandsUpdate(_)
                | schema::SessionUpdate::CurrentModeUpdate(_)
                | schema::SessionUpdate::ConfigOptionUpdate(_)
                | schema::SessionUpdate::Plan(_)
                | schema::SessionUpdate::SessionInfoUpdate(_)
        );
        // 用户消息既可能是旧 Session 历史中的无 Turn 独立消息，也可能绑定当前 Turn；
        // 其余标准 Session 更新仍必须严格区分 Session 级与 Turn 级身份。
        let user_message = matches!(&self.update, schema::SessionUpdate::UserMessageChunk(_));
        if !user_message && session_scoped == self.turn_id.is_some() {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        if let Some(turn_id) = &self.turn_id {
            validate_identifier(turn_id, MAX_DELIVERY_IDENTIFIER_BYTES)?;
        }
        if let Some(source_agent_id) = &self.source_agent_id {
            validate_identifier(source_agent_id, MAX_DELIVERY_IDENTIFIER_BYTES)?;
        }
        Ok(())
    }

    /// 返回投递信封 Schema 版本。
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// 返回根 Session 稳定标识。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 返回产生更新的可选 Turn 标识。
    pub fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }

    /// 返回产生更新的可选 Agent 标识。
    pub fn source_agent_id(&self) -> Option<&str> {
        self.source_agent_id.as_deref()
    }

    /// 返回 Session 当前 Runtime 内的投递序号。
    pub const fn delivery_sequence(&self) -> u64 {
        self.delivery_sequence
    }

    /// 返回更新发生时的 UTC Unix 毫秒时间。
    pub const fn occurred_at_ms(&self) -> u64 {
        self.occurred_at_ms
    }

    /// 返回原样保留的标准 ACP Session 更新。
    pub const fn update(&self) -> &schema::SessionUpdate {
        &self.update
    }
}

/// 只用于严格反序列化后进入恢复校验入口的线格式。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionUpdateDeliveryEnvelopeWire {
    /// 投递信封 Schema 版本。
    schema_version: u16,
    /// 根 Session 稳定标识。
    session_id: String,
    /// 可选 Turn 标识。
    turn_id: Option<String>,
    /// 可选来源 Agent 标识。
    source_agent_id: Option<String>,
    /// Session 当前 Runtime 内严格单调的投递序号。
    delivery_sequence: u64,
    /// UTC Unix 毫秒时间。
    occurred_at_ms: u64,
    /// 标准 ACP Session 更新。
    update: schema::SessionUpdate,
}
