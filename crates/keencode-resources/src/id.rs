use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::ResourceError;

/// 校验一个外部标识可以安全地作为单一路径段，或作为受控的 Turn 层级标识。
fn validate_id(kind: &str, value: &str, allow_hierarchy: bool) -> Result<(), ResourceError> {
    if value.is_empty() || value.len() > 128 {
        return Err(ResourceError::InvalidId(format!(
            "{kind} 长度必须在 1 到 128 字节之间"
        )));
    }
    let segments = value.split('/').collect::<Vec<_>>();
    let hierarchy_shape = if allow_hierarchy {
        !value.starts_with('/') && !value.ends_with('/')
    } else {
        segments.len() == 1
    };
    let valid = hierarchy_shape
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && !matches!(*segment, "." | "..")
                && !segment.ends_with('.')
                && segment
                    .bytes()
                    .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
        });
    if !valid {
        return Err(ResourceError::InvalidId(format!(
            "{kind} 只能包含小写 ASCII 字母、数字、点、横线、下划线，并仅允许受控层级使用斜线"
        )));
    }
    if segments.iter().any(|segment| reserved_device_name(segment)) {
        return Err(ResourceError::InvalidId(format!(
            "{kind} 不能使用系统保留设备名"
        )));
    }
    Ok(())
}

/// 判断单个标识段是否会命中 Windows 保留设备名。
fn reserved_device_name(value: &str) -> bool {
    let portable_stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .to_ascii_uppercase();
    matches!(portable_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || portable_stem
            .strip_prefix("COM")
            .or_else(|| portable_stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $label:literal, $allow_hierarchy:expr) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// 创建经过领域安全规则校验的标识。
            pub fn new(value: impl Into<String>) -> Result<Self, ResourceError> {
                let value = value.into();
                validate_id($label, &value, $allow_hierarchy)?;
                Ok(Self(value))
            }

            /// 返回经过校验的稳定字符串。
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            /// 输出稳定标识正文。
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            /// 序列化为单个字符串。
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            /// 反序列化时重新执行领域标识校验。
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_id!(
    /// 一个 Session 的稳定且可安全映射到磁盘的标识。
    SessionId,
    "Session 标识",
    false
);
define_id!(
    /// 一个 Turn 的稳定标识；内部协作 Turn 可以使用受控层级分隔符。
    TurnId,
    "Turn 标识",
    true
);
define_id!(
    /// 一个终端执行的稳定标识。
    TerminalId,
    "终端标识",
    false
);
define_id!(
    /// 一个主 Agent 或子 Agent 的稳定标识。
    AgentId,
    "Agent 标识",
    false
);
define_id!(
    /// 一条子 Agent 邮箱消息的稳定标识。
    MailboxMessageId,
    "邮箱消息标识",
    false
);
define_id!(
    /// 一次 Session 事件提交的稳定幂等标识。
    SessionEventId,
    "Session 事件标识",
    false
);
define_id!(
    /// 一个本地 Memory、Goal 或 Plan 文档作用域的稳定标识。
    ScopeId,
    "文档作用域标识",
    false
);

/// 从应用授权的现有绝对项目根目录派生跨 Session 共享的安全作用域。
pub fn project_scope_id(project_root: impl AsRef<Path>) -> Result<ScopeId, ResourceError> {
    let project_root = project_root.as_ref();
    if project_root.as_os_str().is_empty() || !project_root.is_absolute() {
        return Err(ResourceError::UnsafePath(
            "项目根目录必须是现有绝对路径".to_owned(),
        ));
    }
    let canonical = fs::canonicalize(project_root)
        .map_err(|error| ResourceError::io("canonicalize_project_scope", error))?;
    if !fs::metadata(&canonical)
        .map_err(|error| ResourceError::io("inspect_project_scope", error))?
        .is_dir()
    {
        return Err(ResourceError::UnsafePath(
            "项目根目录必须指向目录".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(b"keencode/project-scope/v2\0");
    update_project_scope_hash(&mut hasher, &canonical);
    ScopeId::new(format!("project-{}", digest_hex(hasher.finalize())))
}

/// 在 Windows 上按不区分 ASCII 大小写和分隔符的规范路径语义写入摘要。
#[cfg(windows)]
fn update_project_scope_hash(hasher: &mut Sha256, path: &Path) {
    use std::os::windows::ffi::OsStrExt as _;

    for unit in path.as_os_str().encode_wide() {
        let unit = match unit {
            value if value == u16::from(b'/') => u16::from(b'\\'),
            value if value <= u16::from(u8::MAX) => u16::from((value as u8).to_ascii_lowercase()),
            value => value,
        };
        hasher.update(unit.to_le_bytes());
    }
}

/// 在 Unix 上按规范路径的原始字节写入摘要，保留大小写与非 UTF-8 身份。
#[cfg(unix)]
fn update_project_scope_hash(hasher: &mut Sha256, path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;

    hasher.update(path.as_os_str().as_bytes());
}

/// 在其他平台使用规范路径文本写入摘要。
#[cfg(not(any(unix, windows)))]
fn update_project_scope_hash(hasher: &mut Sha256, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
}

/// 一个由完整模型工具调用作用域派生的稳定工具请求标识。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(String);

impl RequestId {
    /// 从已持久化的 64 位小写 SHA-256 字符串恢复工具请求标识。
    pub fn new(value: impl Into<String>) -> Result<Self, ResourceError> {
        let value = value.into();
        validate_sha256_id("工具请求标识", &value)?;
        Ok(Self(value))
    }

    /// 使用 Session、Turn、Agent、模型 Round 与 Provider 原始调用标识派生唯一内部标识。
    pub fn derive_model_tool_call(
        session_id: &SessionId,
        turn_id: &TurnId,
        agent_id: &AgentId,
        model_round: u32,
        model_tool_call_id: &str,
    ) -> Result<Self, ResourceError> {
        if model_round == 0 || model_tool_call_id.is_empty() {
            return Err(ResourceError::InvalidId(
                "模型 Round 必须大于零且 Provider 工具调用标识不能为空".to_owned(),
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(b"keencode/request-id/model-tool-call/v1\0");
        update_length_prefixed(&mut hasher, session_id.as_str().as_bytes());
        update_length_prefixed(&mut hasher, turn_id.as_str().as_bytes());
        update_length_prefixed(&mut hasher, agent_id.as_str().as_bytes());
        hasher.update(model_round.to_be_bytes());
        update_length_prefixed(&mut hasher, model_tool_call_id.as_bytes());
        Self::new(digest_hex(hasher.finalize()))
    }

    /// 返回固定 64 位小写 SHA-256 字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    /// 输出稳定工具请求标识。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for RequestId {
    /// 序列化为固定小写 SHA-256 字符串。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RequestId {
    /// 反序列化时重新校验派生标识格式。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// 一个与 SHA-256 小写十六进制摘要完全相同的内容寻址 Artifact 标识。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// 创建长度固定为 64 的小写十六进制 Artifact 标识。
    pub fn new(value: impl Into<String>) -> Result<Self, ResourceError> {
        let value = value.into();
        validate_sha256_id("Artifact 标识", &value)?;
        Ok(Self(value))
    }

    /// 返回经过校验的 SHA-256 小写十六进制字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 把带长度前缀的字段写入派生摘要，避免不同字段拼接产生歧义。
fn update_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
}

/// 校验内容寻址或派生标识使用固定 64 位小写 SHA-256。
fn validate_sha256_id(kind: &str, value: &str) -> Result<(), ResourceError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ResourceError::InvalidId(format!(
            "{kind} 必须是 64 位 SHA-256 小写十六进制"
        )));
    }
    Ok(())
}

/// 把摘要字节编码成固定小写十六进制。
fn digest_hex(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
    }
    output
}

impl fmt::Display for ArtifactId {
    /// 输出稳定的 SHA-256 小写十六进制正文。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ArtifactId {
    /// 序列化为单个字符串。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ArtifactId {
    /// 反序列化时重新执行 SHA-256 格式校验。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentId, SessionId, TurnId};

    /// Turn 标识允许受控层级，但其他可落盘标识仍只能使用单一路径段。
    #[test]
    fn turn_ids_allow_hierarchy_without_widening_other_ids() {
        assert!(TurnId::new("turn/root/1").is_ok());
        assert!(TurnId::new("root/namespace/42").is_ok());
        assert!(SessionId::new("session/root/1").is_err());
        assert!(AgentId::new("agent/root/1").is_err());
    }

    /// 层级 Turn 标识必须拒绝路径穿越、空段、绝对路径和非便携字符。
    #[test]
    fn hierarchical_turn_ids_reject_unsafe_shapes() {
        for value in [
            "/turn/root",
            "turn/root/",
            "turn//root",
            "turn/./root",
            "turn/../root",
            "turn\\root",
            "Turn/root",
            "turn/root:name",
            "turn/con/root",
            "turn/root/COM1.txt",
        ] {
            assert!(
                TurnId::new(value).is_err(),
                "应拒绝非法 Turn 标识 {value:?}"
            );
        }
    }
}
