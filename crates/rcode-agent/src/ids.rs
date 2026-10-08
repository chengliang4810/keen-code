//! 运行时稳定标识的领域类型。

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// 持久稳定标识允许的最大 UTF-8 字节数，避免恢复数据被重复克隆时放大无界文本。
const MAX_STABLE_IDENTIFIER_BYTES: usize = 1_024;

/// 创建领域标识时返回的稳定校验错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentifierError {
    /// 标识为空或只包含空白。
    Empty,
    /// 标识超过该领域允许的 UTF-8 字节上限。
    TooLong {
        /// 允许的最大 UTF-8 字节数。
        maximum_bytes: usize,
    },
}

impl fmt::Display for IdentifierError {
    /// 输出不包含原始标识内容的安全错误信息。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("标识不能为空或只包含空白"),
            Self::TooLong { maximum_bytes } => {
                write!(formatter, "标识超过最大 UTF-8 字节数 {maximum_bytes}")
            }
        }
    }
}

impl Error for IdentifierError {}

/// 为不同领域标识生成互不混用的字符串新类型。
macro_rules! define_identifier {
    ($(#[$metadata:meta])* $name:ident) => {
        $(#[$metadata])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// 从非空且有界的字符串创建标识，并保留调用方提供的原始内容。
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(IdentifierError::Empty);
                }
                if value.len() > MAX_STABLE_IDENTIFIER_BYTES {
                    return Err(IdentifierError::TooLong {
                        maximum_bytes: MAX_STABLE_IDENTIFIER_BYTES,
                    });
                }
                Ok(Self(value))
            }

            /// 返回标识的字符串视图。
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// 消费标识并返回内部字符串。
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            /// 将标识按原值写入格式化器。
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            /// 返回标识的字符串引用。
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            /// 从字符串切片解析非空且有界的标识。
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            /// 将标识序列化为单个字符串。
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            /// 反序列化时重新执行领域标识的非空和字节上限校验。
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

define_identifier!(
    /// 唯一标识一个持久会话。
    SessionId
);

define_identifier!(
    /// 唯一标识会话中的一次用户 Turn。
    TurnId
);

define_identifier!(
    /// 唯一标识根 Agent 或单层子 Agent。
    AgentId
);

define_identifier!(
    /// 唯一标识一个 Runtime 权威事件的投递身份。
    AgentEventId
);

/// 为首次投递的 Runtime 权威事件分配跨 Runner 唯一的 UUID v7 身份。
pub(crate) fn allocate_agent_event_id() -> AgentEventId {
    AgentEventId::new(format!("agent-event-{}", Uuid::now_v7()))
        .expect("UUID v7 Agent 事件标识始终非空")
}

/// 工具调用标识允许的最大 UTF-8 字节数，防止 Provider 返回无界身份文本。
pub const MAX_TOOL_CALL_ID_BYTES: usize = MAX_STABLE_IDENTIFIER_BYTES;

/// 唯一标识一次模型工具调用的可信有界身份。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// 从非空且有界的 Provider 工具调用标识创建可信身份。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(IdentifierError::Empty);
        }
        if value.len() > MAX_TOOL_CALL_ID_BYTES {
            return Err(IdentifierError::TooLong {
                maximum_bytes: MAX_TOOL_CALL_ID_BYTES,
            });
        }
        Ok(Self(value))
    }

    /// 返回工具调用标识的字符串视图。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费工具调用标识并返回内部字符串。
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ToolCallId {
    /// 将工具调用标识按原值写入格式化器。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ToolCallId {
    /// 返回工具调用标识的字符串引用。
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ToolCallId {
    type Err = IdentifierError;

    /// 从字符串切片解析非空且有界的工具调用标识。
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for ToolCallId {
    /// 将工具调用标识序列化为单个字符串。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ToolCallId {
    /// 反序列化时重新执行非空和字节上限校验。
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
    use super::{
        AgentEventId, AgentId, IdentifierError, MAX_STABLE_IDENTIFIER_BYTES, SessionId, TurnId,
    };

    /// 所有可持久标识都必须在构造和反序列化入口拒绝超长文本。
    #[test]
    fn stable_identifiers_reject_oversized_values() {
        let maximum = "x".repeat(MAX_STABLE_IDENTIFIER_BYTES);
        let oversized = "x".repeat(MAX_STABLE_IDENTIFIER_BYTES + 1);

        assert!(SessionId::new(maximum.clone()).is_ok());
        assert!(TurnId::new(maximum.clone()).is_ok());
        assert!(AgentId::new(maximum.clone()).is_ok());
        assert!(AgentEventId::new(maximum).is_ok());
        assert_eq!(
            SessionId::new(oversized.clone()),
            Err(IdentifierError::TooLong {
                maximum_bytes: MAX_STABLE_IDENTIFIER_BYTES,
            })
        );
        assert_eq!(
            TurnId::new(oversized.clone()),
            Err(IdentifierError::TooLong {
                maximum_bytes: MAX_STABLE_IDENTIFIER_BYTES,
            })
        );
        assert_eq!(
            AgentId::new(oversized.clone()),
            Err(IdentifierError::TooLong {
                maximum_bytes: MAX_STABLE_IDENTIFIER_BYTES,
            })
        );
        assert_eq!(
            AgentEventId::new(oversized.clone()),
            Err(IdentifierError::TooLong {
                maximum_bytes: MAX_STABLE_IDENTIFIER_BYTES,
            })
        );

        let encoded = serde_json::to_string(&oversized).expect("测试标识应可编码");
        assert!(serde_json::from_str::<TurnId>(&encoded).is_err());
    }
}
