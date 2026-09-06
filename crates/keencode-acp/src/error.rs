//! ACP 边界的安全错误分类。

use std::error::Error;
use std::fmt;

/// ACP 请求、扩展事件或 Client 请求违反边界约束时返回的错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcpBoundaryError {
    /// ACP 边界配置包含零值或不一致上限。
    InvalidLimits,
    /// 方法名为空、过长或包含控制字符。
    InvalidMethod,
    /// 当前 Runtime 不实现该精确方法名。
    UnknownMethod,
    /// 请求参数不是符合当前唯一结构的 JSON。
    InvalidParams,
    /// Runtime 生成的响应无法序列化成有效 JSON-RPC 结果。
    InvalidResponse,
    /// Runtime 生成的 Agent 到 Client 请求无法序列化成有效 JSON-RPC 信封。
    InvalidClientRequest,
    /// 请求参数超过配置的字节预算。
    PayloadTooLarge {
        /// 当前允许的最大 JSON 字节数。
        limit: usize,
    },
    /// 请求参数超过配置的嵌套深度。
    PayloadTooDeep {
        /// 当前允许的最大 JSON 嵌套深度。
        limit: usize,
    },
    /// 请求参数超过配置的 JSON 节点数量预算。
    PayloadTooManyNodes {
        /// 当前允许的最大 JSON 节点数。
        limit: usize,
    },
    /// 原始 JSON 对象包含重复键。
    DuplicateJsonKey,
    /// 类型形状正确，但字段值违反当前协议语义。
    InvalidSemanticValue,
    /// 标识为空、过长或包含控制字符。
    InvalidIdentifier,
    /// Session 事件序号已经耗尽。
    SequenceExhausted,
    /// 恢复的 Session 事件序号无效。
    InvalidSequence,
    /// 客户端没有声明当前 Elicitation 模式能力。
    CapabilityNotAdvertised,
}

impl fmt::Display for AcpBoundaryError {
    /// 仅输出固定分类，不回显方法、参数、标识或远端正文。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("ACP 边界上限配置无效"),
            Self::InvalidMethod => formatter.write_str("ACP 方法名无效"),
            Self::UnknownMethod => formatter.write_str("ACP 方法未实现"),
            Self::InvalidParams => formatter.write_str("ACP 请求参数无效"),
            Self::InvalidResponse => formatter.write_str("ACP 响应无法序列化"),
            Self::InvalidClientRequest => formatter.write_str("ACP Client 请求无法序列化"),
            Self::PayloadTooLarge { limit } => {
                write!(formatter, "ACP 请求参数超过 {limit} 字节上限")
            }
            Self::PayloadTooDeep { limit } => {
                write!(formatter, "ACP 请求参数超过 {limit} 层嵌套上限")
            }
            Self::PayloadTooManyNodes { limit } => {
                write!(formatter, "ACP 请求参数超过 {limit} 个 JSON 节点上限")
            }
            Self::DuplicateJsonKey => formatter.write_str("ACP 原始 JSON 包含重复对象键"),
            Self::InvalidSemanticValue => formatter.write_str("ACP 请求字段语义无效"),
            Self::InvalidIdentifier => formatter.write_str("ACP 标识无效"),
            Self::SequenceExhausted => formatter.write_str("ACP Session 事件序号已耗尽"),
            Self::InvalidSequence => formatter.write_str("ACP Session 事件序号无效"),
            Self::CapabilityNotAdvertised => formatter.write_str("ACP 客户端未声明所需能力"),
        }
    }
}

impl Error for AcpBoundaryError {}
