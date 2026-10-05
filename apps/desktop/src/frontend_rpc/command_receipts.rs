//! Persistent command receipt contract used by the desktop RPC gateway.
//!
//! This module deliberately contains no transport route and no second production
//! store.  The Runtime/Journal adapter owns atomic persistence; this file only
//! defines the bounded record shape and the decisions that an adapter must make.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;

/// 收据 schema 独立于 RPC 方法名版本化，冷恢复遇到不兼容记录时必须拒绝重放副作用。
pub(crate) const COMMAND_RECEIPT_SCHEMA: &str = "keencode.command-receipt.v1";
/// 命令 ACK 只保留小型协议结果；大型正文必须留在权威 Session/Artifact 投影中。
pub(crate) const MAX_COMMAND_RECEIPT_BYTES: usize = 64 * 1024;
const MAX_SCOPE_BYTES: usize = 4096;
const MAX_ID_BYTES: usize = 128;
const MAX_COMMAND_TYPE_BYTES: usize = 128;

/// 跨重连和进程重启保持稳定的作用域与命令身份。
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CommandReceiptKey {
    pub(crate) scope: String,
    pub(crate) command_id: String,
}

impl CommandReceiptKey {
    pub(crate) fn new(
        scope: impl Into<String>,
        command_id: impl Into<String>,
    ) -> Result<Self, ReceiptError> {
        let key = Self {
            scope: scope.into(),
            command_id: command_id.into(),
        };
        key.validate()?;
        Ok(key)
    }

    fn validate(&self) -> Result<(), ReceiptError> {
        validate_identifier(&self.scope, MAX_SCOPE_BYTES, "scope")?;
        validate_identifier(&self.command_id, MAX_ID_BYTES, "commandId")
    }
}

/// 原样保存返回 renderer 的终态 ACK，使 ACK 丢失后可以重放而无需再次执行命令。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum CommandReceiptState {
    /// Journal admission 已持久化但副作用尚无确认终态；重试只能等待或查询。
    Admitted,
    /// 副作用和 ACK 结果均已持久确认。
    Completed { ack: Value },
    /// 校验或确定性的业务拒绝已持久确认。
    Rejected { ack: Value },
    /// 副作用可能已经发生但没有可证明终态；该状态明确禁止自动重放。
    Unknown { reason_code: String },
}

/// 一条有界且由 Journal 支持的命令收据。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CommandReceiptRecord {
    pub(crate) schema: String,
    pub(crate) key: CommandReceiptKey,
    pub(crate) command_type: String,
    pub(crate) payload_sha256: String,
    pub(crate) state: CommandReceiptState,
}

impl CommandReceiptRecord {
    /// 构造持久 admission 记录；调用方必须在执行任何命令前原子写入。
    pub(crate) fn admitted(
        key: CommandReceiptKey,
        command_type: impl Into<String>,
        payload_sha256: impl Into<String>,
    ) -> Result<Self, ReceiptError> {
        let record = Self {
            schema: COMMAND_RECEIPT_SCHEMA.to_owned(),
            key,
            command_type: command_type.into(),
            payload_sha256: payload_sha256.into(),
            state: CommandReceiptState::Admitted,
        };
        record.validate()?;
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn completed(&self, ack: Value) -> Result<Self, ReceiptError> {
        self.terminal(CommandReceiptState::Completed { ack })
    }

    #[cfg(test)]
    pub(crate) fn rejected(&self, ack: Value) -> Result<Self, ReceiptError> {
        self.terminal(CommandReceiptState::Rejected { ack })
    }

    #[cfg(test)]
    pub(crate) fn unknown(&self, reason_code: impl Into<String>) -> Result<Self, ReceiptError> {
        self.terminal(CommandReceiptState::Unknown {
            reason_code: reason_code.into(),
        })
    }

    #[cfg(test)]
    fn terminal(&self, state: CommandReceiptState) -> Result<Self, ReceiptError> {
        let record = Self {
            schema: self.schema.clone(),
            key: self.key.clone(),
            command_type: self.command_type.clone(),
            payload_sha256: self.payload_sha256.clone(),
            state,
        };
        record.validate()?;
        Ok(record)
    }

    pub(crate) fn validate(&self) -> Result<(), ReceiptError> {
        if self.schema != COMMAND_RECEIPT_SCHEMA {
            return Err(ReceiptError::InvalidField("schema"));
        }
        self.key.validate()?;
        validate_identifier(&self.command_type, MAX_COMMAND_TYPE_BYTES, "commandType")?;
        if !is_sha256_hex(&self.payload_sha256) {
            return Err(ReceiptError::InvalidField("payloadSha256"));
        }
        match &self.state {
            CommandReceiptState::Admitted => {}
            CommandReceiptState::Completed { ack } | CommandReceiptState::Rejected { ack } => {
                validate_ack(ack)?;
            }
            CommandReceiptState::Unknown { reason_code } => {
                validate_identifier(reason_code, MAX_COMMAND_TYPE_BYTES, "reasonCode")?;
            }
        }
        Ok(())
    }

    /// Compare a persisted record with the current request before any replay.
    pub(crate) fn matches_request(&self, candidate: &Self) -> Result<bool, ReceiptError> {
        self.validate()?;
        candidate.validate()?;
        Ok(self.schema == candidate.schema
            && self.key == candidate.key
            && self.command_type == candidate.command_type
            && self.payload_sha256 == candidate.payload_sha256)
    }
}

/// 网关原子检查或写入收据后可以采取的动作。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ReceiptDecision {
    /// Adapter 已持久化 `Admitted`，本次请求拥有执行权。
    Execute,
    /// 已有终态 ACK，可以直接返回且不调用业务代码。
    Replay { ack: Value },
    /// 另一请求拥有当前 admission 的唯一执行权。
    InFlight,
    /// Journal 知道副作用已越过执行起点但没有终态；调用方必须向用户暴露该状态。
    Unknown { reason_code: String },
    /// 同一 commandId 被不同类型或 payload 复用。
    PayloadConflict,
}

/// 分类既有记录；Journal adapter 必须在 admission/append 原子边界内调用，不能先检查再单独插入。
pub(crate) fn classify_receipt(
    existing: Option<&CommandReceiptRecord>,
    candidate: &CommandReceiptRecord,
) -> Result<ReceiptDecision, ReceiptError> {
    candidate.validate()?;
    let Some(existing) = existing else {
        return Ok(ReceiptDecision::Execute);
    };
    if !existing.matches_request(candidate)? {
        return Ok(ReceiptDecision::PayloadConflict);
    }
    match &existing.state {
        CommandReceiptState::Admitted => Ok(ReceiptDecision::InFlight),
        CommandReceiptState::Completed { ack } | CommandReceiptState::Rejected { ack } => {
            Ok(ReceiptDecision::Replay { ack: ack.clone() })
        }
        CommandReceiptState::Unknown { reason_code } => Ok(ReceiptDecision::Unknown {
            reason_code: reason_code.clone(),
        }),
    }
}

/// 构造并校验一次 RPC admission 的本地协议候选；Runtime 随后把同一摘要写入
/// Journal。这里保持桌面边界与测试夹具共用对象身份和 payload canonicalization。
pub(crate) fn command_candidate(
    scope: &str,
    command_id: &str,
    command_type: &str,
    payload: &Value,
) -> Result<CommandReceiptRecord, ReceiptError> {
    let record = CommandReceiptRecord::admitted(
        CommandReceiptKey::new(scope, command_id)?,
        command_type,
        payload_sha256(payload)?,
    )?;
    debug_assert!(matches!(
        classify_receipt(None, &record),
        Ok(ReceiptDecision::Execute)
    ));
    Ok(record)
}

/// 计算用于 payload 冲突检测的稳定摘要。
///
/// 递归排序对象键，避免 renderer 仅因插入顺序不同就生成不同命令；数组顺序仍有语义。
pub(crate) fn payload_sha256(value: &Value) -> Result<String, ReceiptError> {
    let canonical = canonicalize_json(value);
    let bytes =
        serde_json::to_vec(&canonical).map_err(|_| ReceiptError::InvalidField("payload"))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key.clone(), canonicalize_json(value));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        _ => value.clone(),
    }
}

fn validate_ack(ack: &Value) -> Result<(), ReceiptError> {
    let bytes = serde_json::to_vec(ack).map_err(|_| ReceiptError::InvalidField("ack"))?;
    if bytes.len() > MAX_COMMAND_RECEIPT_BYTES {
        return Err(ReceiptError::AckTooLarge);
    }
    Ok(())
}

fn validate_identifier(
    value: &str,
    max_bytes: usize,
    field: &'static str,
) -> Result<(), ReceiptError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ReceiptError::InvalidField(field));
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptError {
    InvalidField(&'static str),
    AckTooLarge,
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "命令收据字段无效: {field}"),
            Self::AckTooLarge => formatter.write_str("命令收据 ACK 超出大小上限"),
        }
    }
}

impl std::error::Error for ReceiptError {}

#[cfg(test)]
mod tests {
    use super::{
        CommandReceiptKey, CommandReceiptRecord, CommandReceiptState, ReceiptDecision, Value,
        classify_receipt, payload_sha256,
    };
    use std::collections::HashMap;

    fn candidate(payload: Value) -> CommandReceiptRecord {
        let key = CommandReceiptKey::new("session:session-1", "command-1").unwrap();
        CommandReceiptRecord::admitted(key, "sendText", payload_sha256(&payload).unwrap()).unwrap()
    }

    #[test]
    fn concurrent_admission_has_one_executor_and_one_inflight_replay() {
        let mut journal = HashMap::new();
        let candidate = candidate(serde_json::json!({"text": "hello"}));

        assert_eq!(
            classify_receipt(journal.get(&candidate.key), &candidate).unwrap(),
            ReceiptDecision::Execute
        );
        journal.insert(candidate.key.clone(), candidate.clone());
        assert_eq!(
            classify_receipt(journal.get(&candidate.key), &candidate).unwrap(),
            ReceiptDecision::InFlight
        );
    }

    #[test]
    fn same_command_id_with_different_payload_is_rejected() {
        let first = candidate(serde_json::json!({"text": "first"}));
        let second = candidate(serde_json::json!({"text": "second"}));
        assert_eq!(
            classify_receipt(Some(&first), &second).unwrap(),
            ReceiptDecision::PayloadConflict
        );
    }

    #[test]
    fn completed_ack_is_replayed_after_transport_loss() {
        let admitted = candidate(serde_json::json!({"text": "hello"}));
        let completed = admitted
            .completed(serde_json::json!({
                "commandId": "command-1",
                "status": "accepted"
            }))
            .unwrap();
        assert_eq!(
            classify_receipt(Some(&completed), &admitted).unwrap(),
            ReceiptDecision::Replay {
                ack: serde_json::json!({
                    "commandId": "command-1",
                    "status": "accepted"
                })
            }
        );
    }

    #[test]
    fn deterministic_rejection_is_replayed_without_reexecution() {
        let admitted = candidate(serde_json::json!({"text": "hello"}));
        let rejected = admitted
            .rejected(serde_json::json!({
                "commandId": "command-1",
                "status": "rejected",
                "reasonCode": "fault.command.invalid"
            }))
            .unwrap();
        assert_eq!(
            classify_receipt(Some(&rejected), &admitted).unwrap(),
            ReceiptDecision::Replay {
                ack: serde_json::json!({
                    "commandId": "command-1",
                    "status": "rejected",
                    "reasonCode": "fault.command.invalid"
                })
            }
        );
    }

    #[test]
    fn unknown_side_effect_is_never_classified_as_replayable() {
        let admitted = candidate(serde_json::json!({"text": "hello"}));
        let unknown = admitted.unknown("fault.command.resultUnknown").unwrap();
        assert_eq!(
            classify_receipt(Some(&unknown), &admitted).unwrap(),
            ReceiptDecision::Unknown {
                reason_code: "fault.command.resultUnknown".to_owned()
            }
        );
        assert!(matches!(unknown.state, CommandReceiptState::Unknown { .. }));
    }

    #[test]
    fn payload_digest_is_object_order_independent_but_array_ordered() {
        let first = payload_sha256(&serde_json::json!({"a": 1, "b": [1, 2]})).unwrap();
        let reordered = payload_sha256(&serde_json::json!({"b": [1, 2], "a": 1})).unwrap();
        let array_changed = payload_sha256(&serde_json::json!({"a": 1, "b": [2, 1]})).unwrap();
        assert_eq!(first, reordered);
        assert_ne!(first, array_changed);
    }

    #[test]
    fn contract_record_is_bounded_and_serializable() {
        let record = candidate(serde_json::json!({"text": "hello"}));
        let encoded = serde_json::to_vec(&record).unwrap();
        let decoded: CommandReceiptRecord = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(record, decoded);
    }
}
