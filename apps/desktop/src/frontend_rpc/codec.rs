//! ZCode Channel RPC 的二进制编解码。
//!
//! 载荷只包含 `@zcode/rpc` 的 VQL 值序列，不包含 SocketProtocol 的 13 字节
//! 物理帧头。Tauri `Channel<InvokeResponseBody>` 以 Raw body 提供消息边界，
//! 额外套一层物理帧会让前端的 ChannelClient 无法读取响应头。

use base64::Engine as _;
use serde_json::{Map, Number, Value};
use std::{fmt, str};

const DATA_TYPE_UNDEFINED: u8 = 0;
const DATA_TYPE_STRING: u8 = 1;
const DATA_TYPE_BUFFER: u8 = 2;
const DATA_TYPE_VSBUFFER: u8 = 3;
const DATA_TYPE_ARRAY: u8 = 4;
const DATA_TYPE_OBJECT: u8 = 5;
const DATA_TYPE_INT: u8 = 6;

pub const RPC_NESTED_UINT8_ARRAY_MARKER: &str = "__zcode_rpc_nested_uint8array_v1";
pub const RPC_NESTED_UINT8_ARRAY_BASE64_KEY: &str = "base64";

const MAX_VQL_BYTES: usize = 5;
const MAX_VALUE_BYTES: usize = 64 * 1024 * 1024;

/// 一个值在 ZCode RPC 线上可能使用的类型。
///
/// Object 使用 Vec 保留 JSON 字段顺序。TS 端的 Object fallback 会先运行
/// `JSON.stringify`，所以字段顺序属于可观察的互操作细节。
#[derive(Clone, Debug, PartialEq)]
pub enum RpcValue {
    Undefined,
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<RpcValue>),
    Int(i64),
    Null,
    Bool(bool),
    Number(Number),
    Object(Vec<(String, RpcValue)>),
}

impl RpcValue {
    pub fn from_json(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(*value),
            Value::Number(value) => {
                if let Some(integer) = value.as_i64()
                    && i32::try_from(integer).is_ok()
                {
                    return Self::Int(integer);
                }
                Self::Number(value.clone())
            }
            Value::String(value) => Self::String(value.clone()),
            Value::Array(values) => Self::Array(values.iter().map(Self::from_json).collect()),
            Value::Object(values) => {
                if let Some(bytes) = decode_nested_uint8_array(values) {
                    return Self::Bytes(bytes);
                }
                Self::Object(
                    values
                        .iter()
                        .map(|(key, value)| (key.clone(), Self::from_json(value)))
                        .collect(),
                )
            }
        }
    }

    /// 转换为服务层使用的 JSON 值。
    ///
    /// JSON 没有 Uint8Array 类型，因此嵌套 Bytes 按 ZCode marker 表示；顶层
    /// Bytes 仍由 `DATA_TYPE_BUFFER` 单独编码。Undefined 只会出现在无 body 的
    /// RPC 帧中，这里用 null 保持服务层参数为合法 JSON。
    pub fn to_json(&self) -> Value {
        match self {
            Self::Undefined => Value::Null,
            Self::String(value) => Value::String(value.clone()),
            Self::Bytes(value) => nested_uint8_array(value),
            Self::Array(values) => Value::Array(values.iter().map(Self::to_json).collect()),
            Self::Int(value) => Value::Number(Number::from(*value)),
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::Number(value) => Value::Number(value.clone()),
            Self::Object(values) => {
                let mut object = Map::new();
                for (key, value) in values {
                    // JSON.stringify 会省略对象属性中的 undefined；数组分支
                    // 则按上面的规则保留为 null，和 TypeScript 端一致。
                    if matches!(value, Self::Undefined) {
                        continue;
                    }
                    object.insert(key.clone(), value.to_json());
                }
                Value::Object(object)
            }
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Int(value) if *value >= 0 => u32::try_from(*value).ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    UnexpectedEof,
    InvalidUtf8,
    InvalidJson(String),
    InvalidVql,
    InvalidLength,
    ValueTooLarge,
    UnknownType(u8),
    TrailingBytes(usize),
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => {
                formatter.write_str("RPC payload ended before the value was complete")
            }
            Self::InvalidUtf8 => formatter.write_str("RPC string is not valid UTF-8"),
            Self::InvalidJson(error) => write!(formatter, "RPC object is not valid JSON: {error}"),
            Self::InvalidVql => formatter.write_str("RPC VQL integer is invalid or too large"),
            Self::InvalidLength => formatter.write_str("RPC value length is invalid"),
            Self::ValueTooLarge => formatter.write_str("RPC value exceeds the size limit"),
            Self::UnknownType(value) => write!(formatter, "RPC value has unknown type tag {value}"),
            Self::TrailingBytes(value) => {
                write!(formatter, "RPC payload has {value} trailing bytes")
            }
        }
    }
}

impl std::error::Error for CodecError {}

/// 编码一个 VQL 值。
#[allow(dead_code)]
pub fn encode(value: &RpcValue) -> Result<Vec<u8>, CodecError> {
    let mut output = Vec::new();
    encode_into(value, &mut output)?;
    Ok(output)
}

/// 编码由 header 和 body 组成的一个原始 Channel RPC payload。
pub fn encode_message(header: &RpcValue, body: &RpcValue) -> Result<Vec<u8>, CodecError> {
    let mut output = Vec::new();
    encode_into(header, &mut output)?;
    encode_into(body, &mut output)?;
    Ok(output)
}

/// 解码一个值，并要求输入恰好只包含这个值。
#[allow(dead_code)]
pub fn decode(bytes: &[u8]) -> Result<RpcValue, CodecError> {
    let (value, consumed) = decode_at(bytes, 0)?;
    if consumed != bytes.len() {
        return Err(CodecError::TrailingBytes(bytes.len() - consumed));
    }
    Ok(value)
}

/// 解码一个原始 Channel RPC payload。
pub fn decode_message(bytes: &[u8]) -> Result<(RpcValue, RpcValue), CodecError> {
    let (header, offset) = decode_at(bytes, 0)?;
    let (body, consumed) = decode_at(bytes, offset)?;
    if consumed != bytes.len() {
        return Err(CodecError::TrailingBytes(bytes.len() - consumed));
    }
    Ok((header, body))
}

fn encode_into(value: &RpcValue, output: &mut Vec<u8>) -> Result<(), CodecError> {
    match value {
        RpcValue::Undefined => output.push(DATA_TYPE_UNDEFINED),
        RpcValue::String(value) => {
            output.push(DATA_TYPE_STRING);
            write_length(output, value.len())?;
            output.extend_from_slice(value.as_bytes());
        }
        RpcValue::Bytes(value) => {
            output.push(DATA_TYPE_BUFFER);
            write_length(output, value.len())?;
            output.extend_from_slice(value);
        }
        RpcValue::Array(values) => {
            output.push(DATA_TYPE_ARRAY);
            write_length(output, values.len())?;
            for value in values {
                encode_into(value, output)?;
            }
        }
        RpcValue::Int(value) => {
            if i32::try_from(*value).is_err() {
                return Err(CodecError::InvalidVql);
            }
            output.push(DATA_TYPE_INT);
            write_vql(output, *value as u32);
        }
        RpcValue::Null | RpcValue::Bool(_) | RpcValue::Number(_) | RpcValue::Object(_) => {
            output.push(DATA_TYPE_OBJECT);
            let json = serde_json::to_vec(&value.to_json())
                .map_err(|error| CodecError::InvalidJson(error.to_string()))?;
            write_length(output, json.len())?;
            output.extend_from_slice(&json);
        }
    }
    Ok(())
}

fn decode_at(bytes: &[u8], offset: usize) -> Result<(RpcValue, usize), CodecError> {
    let type_tag = *bytes.get(offset).ok_or(CodecError::UnexpectedEof)?;
    let mut cursor = offset + 1;
    let value = match type_tag {
        DATA_TYPE_UNDEFINED => RpcValue::Undefined,
        DATA_TYPE_STRING => {
            let (length, next) = read_length(bytes, cursor)?;
            cursor = next;
            let data = read_slice(bytes, &mut cursor, length)?;
            RpcValue::String(
                str::from_utf8(data)
                    .map_err(|_| CodecError::InvalidUtf8)?
                    .to_string(),
            )
        }
        DATA_TYPE_BUFFER | DATA_TYPE_VSBUFFER => {
            let (length, next) = read_length(bytes, cursor)?;
            cursor = next;
            RpcValue::Bytes(read_slice(bytes, &mut cursor, length)?.to_vec())
        }
        DATA_TYPE_ARRAY => {
            let (length, next) = read_length(bytes, cursor)?;
            cursor = next;
            let mut values = Vec::with_capacity(length);
            for _ in 0..length {
                let (value, next) = decode_at(bytes, cursor)?;
                cursor = next;
                values.push(value);
            }
            RpcValue::Array(values)
        }
        DATA_TYPE_OBJECT => {
            let (length, next) = read_length(bytes, cursor)?;
            cursor = next;
            let data = read_slice(bytes, &mut cursor, length)?;
            let value: Value = serde_json::from_slice(data)
                .map_err(|error| CodecError::InvalidJson(error.to_string()))?;
            RpcValue::from_json(&value)
        }
        DATA_TYPE_INT => RpcValue::Int(read_vql(bytes, &mut cursor)? as i32 as i64),
        value => return Err(CodecError::UnknownType(value)),
    };
    Ok((value, cursor))
}

fn write_length(output: &mut Vec<u8>, length: usize) -> Result<(), CodecError> {
    if length > MAX_VALUE_BYTES || length > u32::MAX as usize {
        return Err(CodecError::ValueTooLarge);
    }
    write_vql(output, length as u32);
    Ok(())
}

fn write_vql(output: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            return;
        }
    }
}

fn read_length(bytes: &[u8], offset: usize) -> Result<(usize, usize), CodecError> {
    let mut cursor = offset;
    let value = read_vql(bytes, &mut cursor)? as usize;
    if value > MAX_VALUE_BYTES {
        return Err(CodecError::ValueTooLarge);
    }
    Ok((value, cursor))
}

fn read_vql(bytes: &[u8], cursor: &mut usize) -> Result<u32, CodecError> {
    let mut value = 0u32;
    for shift in (0..MAX_VQL_BYTES).map(|index| index * 7) {
        let byte = *bytes.get(*cursor).ok_or(CodecError::UnexpectedEof)?;
        *cursor += 1;
        let part = (byte & 0x7f) as u32;
        if shift == 28 && part > 0x0f {
            return Err(CodecError::InvalidVql);
        }
        value |= part << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(CodecError::InvalidVql)
}

fn read_slice<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    length: usize,
) -> Result<&'a [u8], CodecError> {
    let end = cursor
        .checked_add(length)
        .ok_or(CodecError::InvalidLength)?;
    let value = bytes.get(*cursor..end).ok_or(CodecError::UnexpectedEof)?;
    *cursor = end;
    Ok(value)
}

fn nested_uint8_array(value: &[u8]) -> Value {
    let mut object = Map::new();
    object.insert(RPC_NESTED_UINT8_ARRAY_MARKER.to_string(), Value::Bool(true));
    object.insert(
        RPC_NESTED_UINT8_ARRAY_BASE64_KEY.to_string(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(value)),
    );
    Value::Object(object)
}

fn decode_nested_uint8_array(value: &Map<String, Value>) -> Option<Vec<u8>> {
    if value.len() != 2 || value.get(RPC_NESTED_UINT8_ARRAY_MARKER) != Some(&Value::Bool(true)) {
        return None;
    }
    let encoded = value
        .get(RPC_NESTED_UINT8_ARRAY_BASE64_KEY)
        .and_then(Value::as_str)?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_zcode_scalar_golden_payloads() {
        assert_eq!(encode(&RpcValue::Undefined).unwrap(), [0]);
        assert_eq!(
            encode(&RpcValue::String("hello".into())).unwrap(),
            [1, 5, b'h', b'e', b'l', b'l', b'o']
        );
        assert_eq!(
            encode(&RpcValue::Bytes(vec![1, 2, 255])).unwrap(),
            [2, 3, 1, 2, 255]
        );
        // Node 端可能发送 VSBuffer；Rust 网关与 Uint8Array 共用 Bytes 表示。
        assert_eq!(
            decode(&[3, 3, 1, 2, 255]).unwrap(),
            RpcValue::Bytes(vec![1, 2, 255])
        );
        assert_eq!(encode(&RpcValue::Int(127)).unwrap(), [6, 127]);
        assert_eq!(encode(&RpcValue::Int(128)).unwrap(), [6, 128, 1]);
        assert_eq!(
            encode(&RpcValue::Int(-1)).unwrap(),
            [6, 255, 255, 255, 255, 15]
        );
        assert_eq!(
            encode(&RpcValue::Int(i64::from(i32::MAX) + 1)),
            Err(CodecError::InvalidVql)
        );
    }

    #[test]
    fn round_trips_nested_uint8_array_marker() {
        let value = RpcValue::Object(vec![("payload".into(), RpcValue::Bytes(vec![1, 2, 3]))]);
        let encoded = encode(&value).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn nested_uint8_array_matches_typescript_json_fixture() {
        let value = RpcValue::Object(vec![("payload".into(), RpcValue::Bytes(vec![1, 2, 3]))]);
        let json = br#"{"payload":{"__zcode_rpc_nested_uint8array_v1":true,"base64":"AQID"}}"#;
        let mut expected = vec![5, json.len() as u8];
        expected.extend_from_slice(json);
        assert_eq!(encode(&value).unwrap(), expected);
    }

    #[test]
    fn object_undefined_matches_json_stringify_omission() {
        let value = RpcValue::Object(vec![
            ("omitted".into(), RpcValue::Undefined),
            ("kept".into(), RpcValue::String("ok".into())),
        ]);
        assert_eq!(value.to_json(), serde_json::json!({"kept": "ok"}));
    }

    #[test]
    fn decodes_types_emitted_by_zcode() {
        let header = RpcValue::Array(vec![
            RpcValue::Int(100),
            RpcValue::Int(7),
            RpcValue::String("zcode-agent".into()),
            RpcValue::String("ping".into()),
        ]);
        let body = RpcValue::Array(vec![RpcValue::Int(128), RpcValue::String("ok".into())]);
        let bytes = encode_message(&header, &body).unwrap();
        let (decoded_header, decoded_body) = decode_message(&bytes).unwrap();
        assert_eq!(decoded_header, header);
        assert_eq!(decoded_body, body);
        assert_eq!(&bytes[..5], &[4, 4, 6, 100, 6]);
    }

    #[test]
    fn rejects_truncated_and_unknown_payloads() {
        assert_eq!(decode(&[1, 4, b'a']), Err(CodecError::UnexpectedEof));
        assert_eq!(decode(&[99]), Err(CodecError::UnknownType(99)));
        assert_eq!(decode(&[0, 0]), Err(CodecError::TrailingBytes(1)));
    }
}
