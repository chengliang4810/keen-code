use std::cmp::Ordering;
use std::collections::HashSet;

use serde_json::{Map, Number, Value};

use crate::{
    ContentBlock, ModelError, ModelResponse, StopReason, StructuredOutputEnforcement,
    StructuredOutputFailureKind,
};

/// 单个结构化输出 Schema 允许的最大递归层数。
const MAX_SCHEMA_DEPTH: usize = 64;

/// 单个结构化输出 Schema 允许的最大子 Schema 数量。
const MAX_SCHEMA_NODES: usize = 10_000;

/// 单个结构化输出 Schema 及校验中间值允许的最大累计字节数。
const MAX_SCHEMA_BYTES: usize = 4 * 1024 * 1024;

/// 一次结构化输出实例校验允许的最大规则执行次数。
const MAX_VALIDATION_OPERATIONS: usize = 100_000;

/// 一次结构化输出实例允许访问的最大 JSON 节点数。
const MAX_VALIDATION_NODES: usize = 100_000;

/// 一次结构化输出实例及校验中间值允许的最大累计字节数。
const MAX_VALIDATION_BYTES: usize = 16 * 1024 * 1024;

/// 原生结构化输出文本允许的最大 UTF-8 字节数。
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// Schema 注解值、枚举值和实例允许的最大 JSON 嵌套深度。
const MAX_VALUE_DEPTH: usize = 128;

/// 校验 Provider 中立层明确支持的 JSON Schema 子集。
pub(crate) fn validate_schema(schema: &Value) -> Result<(), ModelError> {
    let mut budget = ComplexityBudget::schema();
    budget
        .measure_value(schema)
        .map_err(|error| ModelError::InvalidRequest {
            message: format!("结构化输出 Schema 无效：{}", error.message),
        })?;
    inspect_schema(schema, "$", 0, &mut budget).map_err(|message| ModelError::InvalidRequest {
        message: format!("结构化输出 Schema 无效：{message}"),
    })
}

/// 校验一个 JSON 值满足已经由公开配置入口预检的结构化输出 Schema。
pub(crate) fn validate_value_prechecked(
    schema: &Value,
    value: &Value,
    enforcement: StructuredOutputEnforcement,
) -> Result<(), ModelError> {
    let mut budget = ComplexityBudget::validation();
    budget.measure_value(value).map_err(|error| {
        structured_error(
            enforcement,
            StructuredOutputFailureKind::SchemaViolation,
            error.message,
        )
    })?;
    validate_instance(schema, value, "$", &mut budget).map_err(|issue| {
        structured_error(
            enforcement,
            StructuredOutputFailureKind::SchemaViolation,
            issue.into_message(),
        )
    })
}

/// 从最终模型响应提取唯一 JSON 值，并使用已经预检的 Schema 校验。
pub(crate) fn parse_response_prechecked(
    schema: &Value,
    response: &ModelResponse,
    enforcement: StructuredOutputEnforcement,
) -> Result<Value, ModelError> {
    if response.stop_reason != StopReason::Completed {
        return Err(structured_error(
            enforcement,
            StructuredOutputFailureKind::Incomplete,
            format!(
                "模型未正常完成结构化输出，结束原因为 {:?}",
                response.stop_reason
            ),
        ));
    }

    let mut text = String::new();
    let mut text_blocks = 0usize;
    for block in &response.content {
        match block {
            ContentBlock::Text { text: part } => {
                text_blocks += 1;
                if text.len().saturating_add(part.len()) > MAX_OUTPUT_BYTES {
                    return Err(structured_error(
                        enforcement,
                        StructuredOutputFailureKind::SchemaViolation,
                        format!("最终结构化响应超过字节上限 {MAX_OUTPUT_BYTES}"),
                    ));
                }
                text.push_str(part);
            }
            ContentBlock::Reasoning { .. } => {}
            ContentBlock::Image { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::ToolResult { .. } => {
                return Err(structured_error(
                    enforcement,
                    StructuredOutputFailureKind::UnexpectedContent,
                    "最终结构化响应包含文本与推理之外的内容块",
                ));
            }
        }
    }
    if text_blocks == 0 || text.trim().is_empty() {
        return Err(structured_error(
            enforcement,
            StructuredOutputFailureKind::MissingOutput,
            "最终结构化响应没有 JSON 文本",
        ));
    }

    let value = serde_json::from_str::<Value>(&text).map_err(|error| {
        structured_error(
            enforcement,
            StructuredOutputFailureKind::InvalidJson,
            format!("最终结构化响应不是唯一完整 JSON 值：{error}"),
        )
    })?;
    validate_value_prechecked(schema, &value, enforcement)?;
    Ok(value)
}

/// 创建带稳定阶段和执行方式的结构化输出错误。
pub(crate) fn structured_error(
    enforcement: StructuredOutputEnforcement,
    failure: StructuredOutputFailureKind,
    message: impl Into<String>,
) -> ModelError {
    ModelError::StructuredOutput {
        enforcement,
        failure,
        message: message.into(),
    }
}

/// Schema 与实例校验共用的节点、字节和操作预算。
struct ComplexityBudget {
    /// 已经访问的 JSON 节点数量。
    nodes: usize,
    /// 已经检查或生成的 UTF-8 字节数量。
    bytes: usize,
    /// 已经执行的语义验证操作数量。
    operations: usize,
    /// 当前阶段允许的最大 JSON 节点数量。
    max_nodes: usize,
    /// 当前阶段允许的最大累计字节数量。
    max_bytes: usize,
    /// 当前阶段允许的最大语义操作数量。
    max_operations: usize,
}

/// 预算耗尽时携带不会包含完整输出的稳定说明。
struct BudgetExceeded {
    /// 指出耗尽维度及上限的说明。
    message: String,
}

/// 区分普通 Schema 不匹配、预算耗尽和不可能的内部状态。
enum ValidationIssue {
    /// 实例值不满足一个已预检的 Schema 规则。
    Mismatch(String),
    /// 校验工作量超过本地安全预算。
    Budget(String),
    /// 已预检 Schema 出现理论上不可达的形状或数值。
    Internal(String),
}

impl ValidationIssue {
    /// 创建普通 Schema 不匹配错误。
    fn mismatch(message: impl Into<String>) -> Self {
        Self::Mismatch(message.into())
    }

    /// 创建未经预检或不可能状态错误。
    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// 只为普通不匹配增加组合分支上下文，保留预算和内部错误类型。
    fn with_mismatch_context(self, context: impl FnOnce(String) -> String) -> Self {
        match self {
            Self::Mismatch(message) => Self::Mismatch(context(message)),
            Self::Budget(message) => Self::Budget(message),
            Self::Internal(message) => Self::Internal(message),
        }
    }

    /// 转换为不包含完整模型输出的稳定错误说明。
    fn into_message(self) -> String {
        match self {
            Self::Mismatch(message) => message,
            Self::Budget(message) => format!("结构化输出校验预算耗尽：{message}"),
            Self::Internal(message) => format!("结构化输出校验内部错误：{message}"),
        }
    }
}

impl From<BudgetExceeded> for ValidationIssue {
    /// 保留预算耗尽类型，确保组合规则不能把它当成普通分支失败。
    fn from(error: BudgetExceeded) -> Self {
        Self::Budget(error.message)
    }
}

impl ComplexityBudget {
    /// 创建 Schema 预检预算。
    const fn schema() -> Self {
        Self {
            nodes: 0,
            bytes: 0,
            operations: 0,
            max_nodes: MAX_SCHEMA_NODES,
            max_bytes: MAX_SCHEMA_BYTES,
            max_operations: MAX_VALIDATION_OPERATIONS,
        }
    }

    /// 创建实例校验预算。
    const fn validation() -> Self {
        Self {
            nodes: 0,
            bytes: 0,
            operations: 0,
            max_nodes: MAX_VALIDATION_NODES,
            max_bytes: MAX_VALIDATION_BYTES,
            max_operations: MAX_VALIDATION_OPERATIONS,
        }
    }

    /// 以显式栈统计完整 JSON 值，覆盖注解、枚举、常量、字符串和对象键。
    fn measure_value(&mut self, root: &Value) -> Result<(), BudgetExceeded> {
        let mut pending = vec![(root, 0usize)];
        while let Some((value, depth)) = pending.pop() {
            if depth > MAX_VALUE_DEPTH {
                return Err(BudgetExceeded {
                    message: format!("JSON 值超过最大嵌套层数 {MAX_VALUE_DEPTH}"),
                });
            }
            self.charge_node()?;
            match value {
                Value::Null => self.charge_bytes(4)?,
                Value::Bool(_) => self.charge_bytes(5)?,
                Value::Number(number) => self.charge_bytes(number.to_string().len())?,
                Value::String(text) => self.charge_bytes(text.len())?,
                Value::Array(values) => {
                    for child in values.iter().rev() {
                        pending.push((child, depth + 1));
                    }
                }
                Value::Object(values) => {
                    for (key, child) in values.iter().rev() {
                        self.charge_bytes(key.len())?;
                        pending.push((child, depth + 1));
                    }
                }
            }
        }
        Ok(())
    }

    /// 记录一个 JSON 节点。
    fn charge_node(&mut self) -> Result<(), BudgetExceeded> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > self.max_nodes {
            return Err(BudgetExceeded {
                message: format!("JSON 节点超过上限 {}", self.max_nodes),
            });
        }
        Ok(())
    }

    /// 记录输入或校验中间值占用的字节。
    fn charge_bytes(&mut self, count: usize) -> Result<(), BudgetExceeded> {
        self.bytes = self.bytes.saturating_add(count);
        if self.bytes > self.max_bytes {
            return Err(BudgetExceeded {
                message: format!("JSON 数据和校验中间值超过字节上限 {}", self.max_bytes),
            });
        }
        Ok(())
    }

    /// 记录一次语义规则或值比较操作。
    fn charge_operation(&mut self) -> Result<(), BudgetExceeded> {
        self.operations = self.operations.saturating_add(1);
        if self.operations > self.max_operations {
            return Err(BudgetExceeded {
                message: format!("结构化输出校验操作超过上限 {}", self.max_operations),
            });
        }
        Ok(())
    }
}

/// JSON 数值的精确十进制规范形式，不经过 `f64` 舍入。
#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedNumber {
    /// 非零数值是否为负数；零始终规范为非负。
    negative: bool,
    /// 去除前导零和尾随零后的十进制有效数字。
    digits: String,
    /// 有效数字整数需要乘以的十进制指数。
    exponent: i64,
}

impl NormalizedNumber {
    /// 从 `serde_json` 保留的 JSON 数值文本建立精确规范形式。
    fn from_number(number: &Number) -> Result<Self, String> {
        let raw = number.to_string();
        let (negative, unsigned) = if let Some(unsigned) = raw.strip_prefix('-') {
            (true, unsigned)
        } else {
            (false, raw.as_str())
        };

        let exponent_index = unsigned.find('e').or_else(|| unsigned.find('E'));
        let (mantissa, explicit_exponent) = if let Some(index) = exponent_index {
            let exponent = unsigned
                .get(index + 1..)
                .ok_or_else(|| format!("无法解析 JSON 数值 {raw}"))?;
            if exponent.is_empty() {
                return Err(format!("无法解析 JSON 数值 {raw}"));
            }
            let exponent = exponent
                .parse::<i64>()
                .map_err(|_| format!("JSON 数值指数超出范围 {raw}"))?;
            (&unsigned[..index], exponent)
        } else {
            (unsigned, 0)
        };

        let (integer, fraction) = if let Some((integer, fraction)) = mantissa.split_once('.') {
            (integer, fraction)
        } else {
            (mantissa, "")
        };
        if integer.is_empty()
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(format!("无法解析 JSON 数值 {raw}"));
        }

        let fraction_length = i64::try_from(fraction.len())
            .map_err(|_| format!("JSON 数值小数位数超出范围 {raw}"))?;
        let mut exponent = explicit_exponent
            .checked_sub(fraction_length)
            .ok_or_else(|| format!("JSON 数值指数超出范围 {raw}"))?;
        let mut digits = String::with_capacity(integer.len().saturating_add(fraction.len()));
        digits.push_str(integer);
        digits.push_str(fraction);

        let Some(first_non_zero) = digits.bytes().position(|byte| byte != b'0') else {
            return Ok(Self {
                negative: false,
                digits: "0".to_owned(),
                exponent: 0,
            });
        };
        let mut digits = digits[first_non_zero..].to_owned();
        while digits.as_bytes().last() == Some(&b'0') {
            digits.pop();
            exponent = exponent
                .checked_add(1)
                .ok_or_else(|| format!("JSON 数值指数超出范围 {raw}"))?;
        }

        Ok(Self {
            negative,
            digits,
            exponent,
        })
    }

    /// 返回该数值在数学意义上是否为整数。
    fn is_integer(&self) -> bool {
        self.digits == "0" || self.exponent >= 0
    }

    /// 精确比较两个规范十进制数值。
    fn compare(&self, other: &Self) -> Ordering {
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let absolute = self.compare_absolute(other);
        if self.negative {
            absolute.reverse()
        } else {
            absolute
        }
    }

    /// 比较两个规范十进制数值的绝对值。
    fn compare_absolute(&self, other: &Self) -> Ordering {
        let left_is_zero = self.digits == "0";
        let right_is_zero = other.digits == "0";
        match (left_is_zero, right_is_zero) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (false, false) => {}
        }
        let left_length = i128::try_from(self.digits.len()).unwrap_or(i128::MAX);
        let right_length = i128::try_from(other.digits.len()).unwrap_or(i128::MAX);
        let left_magnitude = i128::from(self.exponent).saturating_add(left_length);
        let right_magnitude = i128::from(other.exponent).saturating_add(right_length);
        let magnitude = left_magnitude.cmp(&right_magnitude);
        if magnitude != Ordering::Equal {
            return magnitude;
        }

        let left = self.digits.as_bytes();
        let right = other.digits.as_bytes();
        for index in 0..left.len().max(right.len()) {
            let left_digit = left.get(index).copied().unwrap_or(b'0');
            let right_digit = right.get(index).copied().unwrap_or(b'0');
            let ordering = left_digit.cmp(&right_digit);
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    }
}

/// 递归检查一个子 Schema 的关键字和参数形状。
fn inspect_schema(
    schema: &Value,
    path: &str,
    depth: usize,
    budget: &mut ComplexityBudget,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("{path} 超过最大递归层数 {MAX_SCHEMA_DEPTH}"));
    }
    budget.charge_operation().map_err(|error| error.message)?;
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path} 必须是 JSON 对象"))?;

    for keyword in object.keys() {
        if !is_supported_keyword(keyword) {
            return Err(format!("{path} 包含不支持的关键字 {keyword}"));
        }
    }
    validate_annotations(object, path)?;
    validate_type_keyword(object, path)?;
    validate_numeric_range(object, "minimum", "maximum", path)?;
    validate_integer_range(object, "minLength", "maxLength", path)?;
    validate_integer_range(object, "minItems", "maxItems", path)?;
    validate_enum_keyword(object, path, budget)?;

    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| format!("{path}/properties 必须是对象"))?;
        for (name, child) in properties {
            inspect_schema(
                child,
                &format!("{path}/properties/{}", escape_pointer(name)),
                depth + 1,
                budget,
            )?;
        }
    }
    if let Some(required) = object.get("required") {
        validate_unique_string_array(required, &format!("{path}/required"), false)?;
    }
    if let Some(additional) = object.get("additionalProperties") {
        match additional {
            Value::Bool(_) => {}
            Value::Object(_) => inspect_schema(
                additional,
                &format!("{path}/additionalProperties"),
                depth + 1,
                budget,
            )?,
            _ => {
                return Err(format!(
                    "{path}/additionalProperties 必须是布尔值或子 Schema"
                ));
            }
        }
    }
    if let Some(items) = object.get("items") {
        inspect_schema(items, &format!("{path}/items"), depth + 1, budget)?;
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = object.get(keyword) {
            let branches = branches
                .as_array()
                .ok_or_else(|| format!("{path}/{keyword} 必须是数组"))?;
            if branches.is_empty() {
                return Err(format!("{path}/{keyword} 不能为空数组"));
            }
            for (index, child) in branches.iter().enumerate() {
                inspect_schema(
                    child,
                    &format!("{path}/{keyword}/{index}"),
                    depth + 1,
                    budget,
                )?;
            }
        }
    }
    Ok(())
}

/// 返回关键字是否属于 Runtime 明确实现的 Schema 子集或无语义注解。
fn is_supported_keyword(keyword: &str) -> bool {
    matches!(
        keyword,
        "$schema"
            | "$id"
            | "$comment"
            | "title"
            | "description"
            | "default"
            | "examples"
            | "readOnly"
            | "writeOnly"
            | "deprecated"
            | "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "enum"
            | "const"
            | "minimum"
            | "maximum"
            | "minLength"
            | "maxLength"
            | "minItems"
            | "maxItems"
            | "allOf"
            | "anyOf"
            | "oneOf"
    )
}

/// 校验常见 Schema 注解自身不会携带错误类型。
fn validate_annotations(object: &Map<String, Value>, path: &str) -> Result<(), String> {
    for keyword in ["$schema", "$id", "$comment", "title", "description"] {
        if object.get(keyword).is_some_and(|value| !value.is_string()) {
            return Err(format!("{path}/{keyword} 必须是字符串"));
        }
    }
    if object
        .get("examples")
        .is_some_and(|value| !value.is_array())
    {
        return Err(format!("{path}/examples 必须是数组"));
    }
    for keyword in ["readOnly", "writeOnly", "deprecated"] {
        if object.get(keyword).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("{path}/{keyword} 必须是布尔值"));
        }
    }
    Ok(())
}

/// 校验 `type` 是受支持类型名称或不重复的类型名称数组。
fn validate_type_keyword(object: &Map<String, Value>, path: &str) -> Result<(), String> {
    let Some(value) = object.get("type") else {
        return Ok(());
    };
    match value {
        Value::String(name) => validate_type_name(name, &format!("{path}/type")),
        Value::Array(names) => {
            if names.is_empty() {
                return Err(format!("{path}/type 不能为空数组"));
            }
            let mut seen = HashSet::new();
            for name in names {
                let name = name
                    .as_str()
                    .ok_or_else(|| format!("{path}/type 数组元素必须是字符串"))?;
                validate_type_name(name, &format!("{path}/type"))?;
                if !seen.insert(name) {
                    return Err(format!("{path}/type 包含重复类型 {name}"));
                }
            }
            Ok(())
        }
        _ => Err(format!("{path}/type 必须是字符串或字符串数组")),
    }
}

/// 校验一个 JSON Schema 基础类型名称。
fn validate_type_name(name: &str, path: &str) -> Result<(), String> {
    if matches!(
        name,
        "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
    ) {
        Ok(())
    } else {
        Err(format!("{path} 包含不支持的类型 {name}"))
    }
}

/// 校验数值上下界都是数值且最小值不大于最大值。
fn validate_numeric_range(
    object: &Map<String, Value>,
    minimum: &str,
    maximum: &str,
    path: &str,
) -> Result<(), String> {
    let lower = object.get(minimum);
    let upper = object.get(maximum);
    if lower.is_some_and(|value| !value.is_number()) {
        return Err(format!("{path}/{minimum} 必须是数值"));
    }
    if upper.is_some_and(|value| !value.is_number()) {
        return Err(format!("{path}/{maximum} 必须是数值"));
    }
    if let (Some(lower), Some(upper)) = (lower, upper) {
        let ordering = compare_numbers(lower, upper)
            .map_err(|message| format!("{path} 的数值边界无效：{message}"))?;
        if ordering.is_gt() {
            return Err(format!("{path}/{minimum} 不能大于 {maximum}"));
        }
    }
    Ok(())
}

/// 校验非负整数上下界都是整数且最小值不大于最大值。
fn validate_integer_range(
    object: &Map<String, Value>,
    minimum: &str,
    maximum: &str,
    path: &str,
) -> Result<(), String> {
    let lower = object.get(minimum).map(|value| {
        value
            .as_u64()
            .ok_or_else(|| format!("{path}/{minimum} 必须是非负整数"))
    });
    let upper = object.get(maximum).map(|value| {
        value
            .as_u64()
            .ok_or_else(|| format!("{path}/{maximum} 必须是非负整数"))
    });
    let lower = lower.transpose()?;
    let upper = upper.transpose()?;
    if let (Some(lower), Some(upper)) = (lower, upper) {
        if lower > upper {
            return Err(format!("{path}/{minimum} 不能大于 {maximum}"));
        }
    }
    Ok(())
}

/// 校验 `enum` 非空且不包含重复 JSON 值。
fn validate_enum_keyword(
    object: &Map<String, Value>,
    path: &str,
    budget: &mut ComplexityBudget,
) -> Result<(), String> {
    let Some(values) = object.get("enum") else {
        return Ok(());
    };
    let values = values
        .as_array()
        .ok_or_else(|| format!("{path}/enum 必须是数组"))?;
    if values.is_empty() {
        return Err(format!("{path}/enum 不能为空数组"));
    }
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        budget.charge_operation().map_err(|error| error.message)?;
        let key = semantic_key(value, budget).map_err(ValidationIssue::into_message)?;
        if !seen.insert(key) {
            return Err(format!("{path}/enum 包含重复值"));
        }
    }
    Ok(())
}

/// 为 JSON 值生成带类型和长度边界的语义规范键。
fn semantic_key(value: &Value, budget: &mut ComplexityBudget) -> Result<Vec<u8>, ValidationIssue> {
    let mut output = Vec::new();
    write_semantic_value(value, 0, &mut output, budget)?;
    Ok(output)
}

/// 递归写入一个 JSON 值的语义规范表示。
fn write_semantic_value(
    value: &Value,
    depth: usize,
    output: &mut Vec<u8>,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    if depth > MAX_VALUE_DEPTH {
        return Err(ValidationIssue::Budget(format!(
            "JSON 值超过最大嵌套层数 {MAX_VALUE_DEPTH}"
        )));
    }
    budget.charge_operation().map_err(ValidationIssue::from)?;
    match value {
        Value::Null => write_key_bytes(output, b"n", budget),
        Value::Bool(false) => write_key_bytes(output, b"f", budget),
        Value::Bool(true) => write_key_bytes(output, b"t", budget),
        Value::Number(number) => {
            let normalized = NormalizedNumber::from_number(number).map_err(|message| {
                ValidationIssue::internal(format!("无法规范 JSON 数值：{message}"))
            })?;
            write_key_bytes(output, b"d", budget)?;
            if normalized.negative {
                write_key_bytes(output, b"-", budget)?;
            } else {
                write_key_bytes(output, b"+", budget)?;
            }
            let exponent = normalized.exponent.to_string();
            write_key_segment(output, exponent.as_bytes(), budget)?;
            write_key_segment(output, normalized.digits.as_bytes(), budget)
        }
        Value::String(text) => {
            write_key_bytes(output, b"s", budget)?;
            write_key_segment(output, text.as_bytes(), budget)
        }
        Value::Array(values) => {
            write_key_bytes(output, b"a", budget)?;
            write_key_length(output, values.len(), budget)?;
            for child in values {
                budget.charge_operation().map_err(ValidationIssue::from)?;
                write_semantic_value(child, depth + 1, output, budget)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            write_key_bytes(output, b"o", budget)?;
            write_key_length(output, values.len(), budget)?;
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (key, child) in entries {
                budget.charge_operation().map_err(ValidationIssue::from)?;
                write_key_bytes(output, b"k", budget)?;
                write_key_segment(output, key.as_bytes(), budget)?;
                write_semantic_value(child, depth + 1, output, budget)?;
            }
            Ok(())
        }
    }
}

/// 写入一个以十进制字节长度为边界的语义键片段。
fn write_key_segment(
    output: &mut Vec<u8>,
    value: &[u8],
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    write_key_length(output, value.len(), budget)?;
    write_key_bytes(output, value, budget)
}

/// 写入语义键片段的十进制长度和分隔符。
fn write_key_length(
    output: &mut Vec<u8>,
    length: usize,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    let length = length.to_string();
    write_key_bytes(output, length.as_bytes(), budget)?;
    write_key_bytes(output, b":", budget)
}

/// 在扩展语义键之前计入生成字节预算。
fn write_key_bytes(
    output: &mut Vec<u8>,
    value: &[u8],
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    budget
        .charge_bytes(value.len())
        .map_err(ValidationIssue::from)?;
    output.extend_from_slice(value);
    Ok(())
}

/// 校验一个数组只含不重复字符串，并按需允许空数组。
fn validate_unique_string_array(
    value: &Value,
    path: &str,
    allow_empty: bool,
) -> Result<(), String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} 必须是数组"))?;
    if !allow_empty && values.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for value in values {
        let value = value
            .as_str()
            .ok_or_else(|| format!("{path} 的元素必须是字符串"))?;
        if !seen.insert(value) {
            return Err(format!("{path} 包含重复字符串 {value}"));
        }
    }
    Ok(())
}

/// 递归校验实例值并返回第一条稳定路径错误。
fn validate_instance(
    schema: &Value,
    instance: &Value,
    path: &str,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    budget.charge_operation().map_err(ValidationIssue::from)?;
    let object = schema
        .as_object()
        .ok_or_else(|| ValidationIssue::internal("未经预检的子 Schema 不是对象"))?;

    if let Some(expected) = object.get("type") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        if !matches_type(expected, instance)? {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 的类型为 {}，不满足 type {}",
                instance_type(instance),
                compact_json(expected)
            )));
        }
    }
    if let Some(expected) = object.get("const") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        if !semantic_values_equal(instance, expected, budget)? {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 不等于 const {}",
                compact_json(expected)
            )));
        }
    }
    if let Some(values) = object.get("enum") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let values = values
            .as_array()
            .ok_or_else(|| ValidationIssue::internal("enum 未通过 Schema 预检"))?;
        if values.is_empty() {
            return Err(ValidationIssue::internal("enum 为空但未被 Schema 预检拒绝"));
        }
        let instance_key = semantic_key(instance, budget)?;
        let mut allowed = HashSet::with_capacity(values.len());
        for value in values {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            let key = semantic_key(value, budget)?;
            if !allowed.insert(key) {
                return Err(ValidationIssue::internal("enum 重复值未被 Schema 预检拒绝"));
            }
        }
        if !allowed.contains(&instance_key) {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 不属于 enum 允许值"
            )));
        }
    }

    validate_compositions(object, instance, path, budget)?;
    validate_object_keywords(object, instance, path, budget)?;
    validate_array_keywords(object, instance, path, budget)?;
    validate_string_keywords(object, instance, path, budget)?;
    validate_number_keywords(object, instance, path, budget)?;
    Ok(())
}

/// 使用规范键判断两个 JSON 值是否按 JSON Schema 数值语义相等。
fn semantic_values_equal(
    left: &Value,
    right: &Value,
    budget: &mut ComplexityBudget,
) -> Result<bool, ValidationIssue> {
    let left = semantic_key(left, budget)?;
    let right = semantic_key(right, budget)?;
    Ok(left == right)
}

/// 校验 `allOf`、`anyOf` 和 `oneOf` 组合规则。
fn validate_compositions(
    schema: &Map<String, Value>,
    instance: &Value,
    path: &str,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    if let Some(branches) = schema.get("allOf") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let branches = branches
            .as_array()
            .ok_or_else(|| ValidationIssue::internal("allOf 未通过 Schema 预检"))?;
        if branches.is_empty() {
            return Err(ValidationIssue::internal(
                "allOf 为空但未被 Schema 预检拒绝",
            ));
        }
        for (index, branch) in branches.iter().enumerate() {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            validate_instance(branch, instance, path, budget).map_err(|issue| {
                issue.with_mismatch_context(|message| {
                    format!("{path} 不满足 allOf 分支 {index}：{message}")
                })
            })?;
        }
    }
    if let Some(branches) = schema.get("anyOf") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let branches = branches
            .as_array()
            .ok_or_else(|| ValidationIssue::internal("anyOf 未通过 Schema 预检"))?;
        if branches.is_empty() {
            return Err(ValidationIssue::internal(
                "anyOf 为空但未被 Schema 预检拒绝",
            ));
        }
        let mut first_error = None;
        let mut matched = false;
        for branch in branches {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            match validate_instance(branch, instance, path, budget) {
                Ok(()) => {
                    matched = true;
                    break;
                }
                Err(ValidationIssue::Mismatch(message)) => {
                    first_error.get_or_insert(message);
                }
                Err(ValidationIssue::Budget(message)) => {
                    return Err(ValidationIssue::Budget(message));
                }
                Err(ValidationIssue::Internal(message)) => {
                    return Err(ValidationIssue::Internal(message));
                }
            }
        }
        if !matched {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 不满足任何 anyOf 分支：{}",
                first_error.unwrap_or_else(|| "未提供具体原因".to_owned())
            )));
        }
    }
    if let Some(branches) = schema.get("oneOf") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let branches = branches
            .as_array()
            .ok_or_else(|| ValidationIssue::internal("oneOf 未通过 Schema 预检"))?;
        if branches.is_empty() {
            return Err(ValidationIssue::internal(
                "oneOf 为空但未被 Schema 预检拒绝",
            ));
        }
        let mut matches = 0usize;
        for branch in branches {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            match validate_instance(branch, instance, path, budget) {
                Ok(()) => matches = matches.saturating_add(1),
                Err(ValidationIssue::Mismatch(_)) => {}
                Err(ValidationIssue::Budget(message)) => {
                    return Err(ValidationIssue::Budget(message));
                }
                Err(ValidationIssue::Internal(message)) => {
                    return Err(ValidationIssue::Internal(message));
                }
            }
        }
        if matches != 1 {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 必须恰好满足一个 oneOf 分支，实际匹配 {matches} 个"
            )));
        }
    }
    Ok(())
}

/// 校验对象属性、必需字段和额外字段规则。
fn validate_object_keywords(
    schema: &Map<String, Value>,
    instance: &Value,
    path: &str,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    let Some(instance) = instance.as_object() else {
        return Ok(());
    };
    let properties = match schema.get("properties") {
        Some(Value::Object(properties)) => Some(properties),
        Some(_) => return Err(ValidationIssue::internal("properties 未通过 Schema 预检")),
        None => None,
    };
    if let Some(required) = schema.get("required") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let required = required
            .as_array()
            .ok_or_else(|| ValidationIssue::internal("required 未通过 Schema 预检"))?;
        for name in required {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            let name = name
                .as_str()
                .ok_or_else(|| ValidationIssue::internal("required 元素未通过 Schema 预检"))?;
            if !instance.contains_key(name) {
                return Err(ValidationIssue::mismatch(format!(
                    "{path} 缺少必需属性 {}",
                    escape_pointer(name)
                )));
            }
        }
    }
    if let Some(properties) = properties {
        for (name, child_schema) in properties {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            if let Some(child) = instance.get(name) {
                validate_instance(child_schema, child, &child_path(path, name), budget)?;
            }
        }
    }
    if let Some(additional) = schema.get("additionalProperties") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        for (name, child) in instance {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            if properties.is_some_and(|properties| properties.contains_key(name)) {
                continue;
            }
            match additional {
                Value::Bool(true) => {}
                Value::Bool(false) => {
                    return Err(ValidationIssue::mismatch(format!(
                        "{} 是不允许的额外属性",
                        child_path(path, name)
                    )));
                }
                Value::Object(_) => {
                    validate_instance(additional, child, &child_path(path, name), budget)?;
                }
                _ => {
                    return Err(ValidationIssue::internal(
                        "additionalProperties 未通过 Schema 预检",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 校验数组长度及每个元素的 `items` 子 Schema。
fn validate_array_keywords(
    schema: &Map<String, Value>,
    instance: &Value,
    path: &str,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    let Some(instance) = instance.as_array() else {
        return Ok(());
    };
    if let Some(minimum) = schema.get("minItems") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let minimum = minimum
            .as_u64()
            .ok_or_else(|| ValidationIssue::internal("minItems 未通过 Schema 预检"))?;
        if usize_to_u64(instance.len()) < minimum {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 的元素数量小于 minItems {minimum}"
            )));
        }
    }
    if let Some(maximum) = schema.get("maxItems") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let maximum = maximum
            .as_u64()
            .ok_or_else(|| ValidationIssue::internal("maxItems 未通过 Schema 预检"))?;
        if usize_to_u64(instance.len()) > maximum {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 的元素数量大于 maxItems {maximum}"
            )));
        }
    }
    if let Some(items) = schema.get("items") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        for (index, child) in instance.iter().enumerate() {
            budget.charge_operation().map_err(ValidationIssue::from)?;
            validate_instance(items, child, &format!("{path}/{index}"), budget)?;
        }
    }
    Ok(())
}

/// 校验字符串的 Unicode 标量长度边界。
fn validate_string_keywords(
    schema: &Map<String, Value>,
    instance: &Value,
    path: &str,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    let Some(instance) = instance.as_str() else {
        return Ok(());
    };
    let length = usize_to_u64(instance.chars().count());
    if let Some(minimum) = schema.get("minLength") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let minimum = minimum
            .as_u64()
            .ok_or_else(|| ValidationIssue::internal("minLength 未通过 Schema 预检"))?;
        if length < minimum {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 的字符数小于 minLength {minimum}"
            )));
        }
    }
    if let Some(maximum) = schema.get("maxLength") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let maximum = maximum
            .as_u64()
            .ok_or_else(|| ValidationIssue::internal("maxLength 未通过 Schema 预检"))?;
        if length > maximum {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 的字符数大于 maxLength {maximum}"
            )));
        }
    }
    Ok(())
}

/// 校验数值的最小值和最大值边界。
fn validate_number_keywords(
    schema: &Map<String, Value>,
    instance: &Value,
    path: &str,
    budget: &mut ComplexityBudget,
) -> Result<(), ValidationIssue> {
    if !instance.is_number() {
        return Ok(());
    }
    if let Some(minimum) = schema.get("minimum") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let ordering = compare_numbers(instance, minimum)
            .map_err(|message| ValidationIssue::internal(format!("minimum 无效：{message}")))?;
        if ordering.is_lt() {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 小于 minimum {}",
                compact_json(minimum)
            )));
        }
    }
    if let Some(maximum) = schema.get("maximum") {
        budget.charge_operation().map_err(ValidationIssue::from)?;
        let ordering = compare_numbers(instance, maximum)
            .map_err(|message| ValidationIssue::internal(format!("maximum 无效：{message}")))?;
        if ordering.is_gt() {
            return Err(ValidationIssue::mismatch(format!(
                "{path} 大于 maximum {}",
                compact_json(maximum)
            )));
        }
    }
    Ok(())
}

/// 判断实例是否满足单个或联合 JSON 类型约束。
fn matches_type(expected: &Value, instance: &Value) -> Result<bool, ValidationIssue> {
    match expected {
        Value::String(name) => matches_single_type(name, instance),
        Value::Array(names) => {
            if names.is_empty() {
                return Err(ValidationIssue::internal("type 为空但未被 Schema 预检拒绝"));
            }
            for name in names {
                let name = name
                    .as_str()
                    .ok_or_else(|| ValidationIssue::internal("type 数组未通过 Schema 预检"))?;
                if matches_single_type(name, instance)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Err(ValidationIssue::internal("type 未通过 Schema 预检")),
    }
}

/// 判断实例是否满足一个基础 JSON 类型名称。
fn matches_single_type(expected: &str, instance: &Value) -> Result<bool, ValidationIssue> {
    let matches = match expected {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "number" => instance.is_number(),
        "integer" => match instance {
            Value::Number(number) => NormalizedNumber::from_number(number)
                .map_err(|message| ValidationIssue::internal(format!("整数判断失败：{message}")))?
                .is_integer(),
            _ => false,
        },
        "string" => instance.is_string(),
        _ => return Err(ValidationIssue::internal("type 名称未通过 Schema 预检")),
    };
    Ok(matches)
}

/// 返回错误消息使用的稳定 JSON 类型名称。
fn instance_type(instance: &Value) -> &'static str {
    match instance {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 比较两个已经预检为 JSON 数值的值，不经过二进制浮点舍入。
fn compare_numbers(left: &Value, right: &Value) -> Result<Ordering, String> {
    let Value::Number(left) = left else {
        return Err("左值不是 JSON 数值".to_owned());
    };
    let Value::Number(right) = right else {
        return Err("右值不是 JSON 数值".to_owned());
    };
    let left = NormalizedNumber::from_number(left)?;
    let right = NormalizedNumber::from_number(right)?;
    Ok(left.compare(&right))
}

/// 把 usize 长度安全转换为 u64，极端平台溢出时使用最大值。
fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// 构造子属性的 JSON Pointer 路径。
fn child_path(path: &str, property: &str) -> String {
    format!("{path}/{}", escape_pointer(property))
}

/// 转义 JSON Pointer 中的属性名称。
fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

/// 生成不会换行的紧凑 JSON 错误片段。
fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid-json>".to_owned())
}
