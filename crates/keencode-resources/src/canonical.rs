use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::ResourceError;

/// 计算任意可序列化值的递归对象键排序 JSON SHA-256。
pub(crate) fn canonical_json_sha256<T: Serialize + ?Sized>(
    value: &T,
) -> Result<String, ResourceError> {
    let value =
        serde_json::to_value(value).map_err(|error| ResourceError::Json(error.to_string()))?;
    let canonical = canonicalize_json(value);
    let bytes =
        serde_json::to_vec(&canonical).map_err(|error| ResourceError::Json(error.to_string()))?;
    Ok(digest_hex(Sha256::digest(bytes)))
}

/// 递归按对象键排序，确保 Cargo feature 不会改变 JSON 摘要顺序。
fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonicalize_json(value));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value,
    }
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
