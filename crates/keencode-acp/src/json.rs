//! ACP 边界共享的有界 JSON 校验与严格原始解析。

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;
use std::io::{self, Write};

use serde::Deserializer;
use serde::de::{self, DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use crate::AcpBoundaryError;

/// 原始 JSON 解析遇到重复对象键时使用的固定内部标记。
const DUPLICATE_KEY_MARKER: &str = "__keencode_duplicate_json_key";
/// 原始 JSON 解析超过深度上限时使用的固定内部标记。
const TOO_DEEP_MARKER: &str = "__keencode_json_too_deep";
/// 原始 JSON 解析超过节点上限时使用的固定内部标记。
const TOO_MANY_NODES_MARKER: &str = "__keencode_json_too_many_nodes";

/// 一次 JSON 值校验允许使用的资源边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JsonValueLimits {
    /// 编码后的 JSON 最大字节数。
    pub(crate) max_bytes: usize,
    /// 对象或数组容器的最大嵌套层数。
    pub(crate) max_depth: usize,
    /// JSON 值节点的最大总数。
    pub(crate) max_nodes: usize,
}

/// 先以非递归遍历限制节点和容器深度，再以有界 Writer 计算编码字节数。
pub(crate) fn validate_value(
    value: &Value,
    limits: JsonValueLimits,
) -> Result<(), AcpBoundaryError> {
    let mut pending = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((current, parent_depth)) = pending.pop() {
        nodes = nodes
            .checked_add(1)
            .ok_or(AcpBoundaryError::PayloadTooManyNodes {
                limit: limits.max_nodes,
            })?;
        if nodes > limits.max_nodes {
            return Err(AcpBoundaryError::PayloadTooManyNodes {
                limit: limits.max_nodes,
            });
        }

        match current {
            Value::Array(values) => {
                let depth = enter_container(parent_depth, limits.max_depth)?;
                pending.extend(values.iter().map(|child| (child, depth)));
            }
            Value::Object(values) => {
                let depth = enter_container(parent_depth, limits.max_depth)?;
                pending.extend(values.values().map(|child| (child, depth)));
            }
            _ => {}
        }
    }

    let mut writer = BoundedWriter::new(limits.max_bytes);
    let result = serde_json::to_writer(&mut writer, value);
    if writer.exceeded {
        return Err(AcpBoundaryError::PayloadTooLarge {
            limit: limits.max_bytes,
        });
    }
    result.map_err(|_| AcpBoundaryError::InvalidParams)
}

/// 在分配 JSON DOM 前限制原始字节，并拒绝重复对象键、过深容器和过多节点。
pub(crate) fn parse_raw_value(
    raw: &[u8],
    limits: JsonValueLimits,
) -> Result<Value, AcpBoundaryError> {
    if raw.len() > limits.max_bytes {
        return Err(AcpBoundaryError::PayloadTooLarge {
            limit: limits.max_bytes,
        });
    }

    let budget = ParseBudget {
        max_depth: limits.max_depth,
        max_nodes: limits.max_nodes,
        nodes: Cell::new(0),
    };
    let seed = StrictValueSeed {
        budget: &budget,
        parent_depth: 0,
    };
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    let value = seed
        .deserialize(&mut deserializer)
        .and_then(|value| {
            deserializer.end()?;
            Ok(value)
        })
        .map_err(|error| classify_raw_error(&error.to_string(), limits))?;
    validate_value(&value, limits)?;
    Ok(value)
}

/// 判断输入中的每个字段和值是否都被当前类型完整保留。
///
/// 当 Serde 为可选字段补默认值时，规范化结果可以比输入多字段；但输入不能
/// 包含被忽略的未知字段、别名或被改写的值。
pub(crate) fn input_preserved(input: &Value, normalized: &Value) -> bool {
    match (input, normalized) {
        (Value::Object(input), Value::Object(normalized)) => input.iter().all(|(key, value)| {
            normalized
                .get(key)
                .is_some_and(|normalized| input_preserved(value, normalized))
        }),
        (Value::Array(input), Value::Array(normalized)) => {
            input.len() == normalized.len()
                && input
                    .iter()
                    .zip(normalized)
                    .all(|(input, normalized)| input_preserved(input, normalized))
        }
        _ => input == normalized,
    }
}

/// 校验标识为非空、有界且不含控制字符的字符串。
pub(crate) fn validate_identifier(value: &str, max_bytes: usize) -> Result<(), AcpBoundaryError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().any(|character| character.is_control())
    {
        return Err(AcpBoundaryError::InvalidIdentifier);
    }
    Ok(())
}

/// 校验用户文本为非空、有界且只包含允许的换行控制字符。
pub(crate) fn validate_text(value: &str, max_bytes: usize) -> Result<(), AcpBoundaryError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    Ok(())
}

/// 进入一个对象或数组容器并校验新的容器深度。
fn enter_container(parent_depth: usize, max_depth: usize) -> Result<usize, AcpBoundaryError> {
    let depth = parent_depth
        .checked_add(1)
        .ok_or(AcpBoundaryError::PayloadTooDeep { limit: max_depth })?;
    if depth > max_depth {
        return Err(AcpBoundaryError::PayloadTooDeep { limit: max_depth });
    }
    Ok(depth)
}

/// 把严格原始解析器的内部固定标记转换为稳定边界错误。
fn classify_raw_error(message: &str, limits: JsonValueLimits) -> AcpBoundaryError {
    if message.contains(DUPLICATE_KEY_MARKER) {
        AcpBoundaryError::DuplicateJsonKey
    } else if message.contains(TOO_DEEP_MARKER) {
        AcpBoundaryError::PayloadTooDeep {
            limit: limits.max_depth,
        }
    } else if message.contains(TOO_MANY_NODES_MARKER) {
        AcpBoundaryError::PayloadTooManyNodes {
            limit: limits.max_nodes,
        }
    } else {
        AcpBoundaryError::InvalidParams
    }
}

/// 一个只计数且在达到上限后立即停止序列化的 Writer。
struct BoundedWriter {
    /// 当前已接受的字节数。
    written: usize,
    /// 当前允许的最大字节数。
    limit: usize,
    /// Writer 是否因为超过上限而停止。
    exceeded: bool,
}

impl BoundedWriter {
    /// 创建尚未接收任何字节的计数 Writer。
    const fn new(limit: usize) -> Self {
        Self {
            written: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedWriter {
    /// 接收一段序列化输出，并在累计值超过上限前停止。
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.written) {
            self.exceeded = true;
            return Err(io::Error::other("JSON byte limit exceeded"));
        }
        self.written += bytes.len();
        Ok(bytes.len())
    }

    /// 计数 Writer 不持有缓冲区，因此刷新始终立即完成。
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 原始 JSON 解析过程中共享的节点和深度预算。
struct ParseBudget {
    /// 对象或数组容器的最大嵌套层数。
    max_depth: usize,
    /// JSON 值节点的最大总数。
    max_nodes: usize,
    /// 当前已经看到的 JSON 值节点数。
    nodes: Cell<usize>,
}

impl ParseBudget {
    /// 为一个新值节点消耗配额。
    fn record_node<E>(&self) -> Result<(), E>
    where
        E: de::Error,
    {
        let nodes = self.nodes.get().saturating_add(1);
        if nodes > self.max_nodes {
            return Err(E::custom(TOO_MANY_NODES_MARKER));
        }
        self.nodes.set(nodes);
        Ok(())
    }

    /// 进入一个新容器并返回其深度。
    fn enter_container<E>(&self, parent_depth: usize) -> Result<usize, E>
    where
        E: de::Error,
    {
        let depth = parent_depth.saturating_add(1);
        if depth > self.max_depth {
            return Err(E::custom(TOO_DEEP_MARKER));
        }
        Ok(depth)
    }
}

/// 在构造每个 JSON 值前消耗共享预算的 Deserialize Seed。
struct StrictValueSeed<'a> {
    /// 当前原始解析的共享预算。
    budget: &'a ParseBudget,
    /// 当前值所在父容器的深度。
    parent_depth: usize,
}

impl<'de> DeserializeSeed<'de> for StrictValueSeed<'_> {
    type Value = Value;

    /// 严格解析一个 JSON 值，并在进入该值前消耗节点配额。
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.budget.record_node::<D::Error>()?;
        deserializer.deserialize_any(StrictValueVisitor {
            budget: self.budget,
            parent_depth: self.parent_depth,
        })
    }
}

/// 严格构造 serde_json::Value 且拒绝重复对象键的 Visitor。
struct StrictValueVisitor<'a> {
    /// 当前原始解析的共享预算。
    budget: &'a ParseBudget,
    /// 当前值所在父容器的深度。
    parent_depth: usize,
}

impl<'de> Visitor<'de> for StrictValueVisitor<'_> {
    type Value = Value;

    /// 描述当前 Visitor 接受的输入类型。
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value")
    }

    /// 构造 JSON null。
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    /// 构造 JSON null。
    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    /// 构造 JSON 布尔值。
    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    /// 构造 JSON 有符号整数。
    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    /// 构造 JSON 无符号整数。
    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    /// 构造 JSON 浮点数，并拒绝非有限值。
    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    /// 构造借用 JSON 字符串。
    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    /// 构造拥有所有权的 JSON 字符串。
    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    /// 构造 JSON 数组，并为每个元素继续使用相同共享预算。
    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let depth = self.budget.enter_container::<A::Error>(self.parent_depth)?;
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(1024));
        while let Some(value) = sequence.next_element_seed(StrictValueSeed {
            budget: self.budget,
            parent_depth: depth,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    /// 构造 JSON 对象，并在读取值前拒绝同一对象中的重复键。
    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let depth = self.budget.enter_container::<A::Error>(self.parent_depth)?;
        let mut keys = HashSet::with_capacity(object.size_hint().unwrap_or(0).min(1024));
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(A::Error::custom(DUPLICATE_KEY_MARKER));
            }
            let value = object.next_value_seed(StrictValueSeed {
                budget: self.budget,
                parent_depth: depth,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
