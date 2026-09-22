//! 文件变更权威快照的 ACP 引用、分页读取请求与响应类型。

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AcpBoundaryError;
use crate::json::validate_identifier;

/// 标准 ACP ResourceLink 中 KeenCode 文件变更引用的唯一命名空间键。
pub const FILE_CHANGE_META_KEY: &str = "keencode/fileChange";

/// 单份文件变更快照允许的最大原始字节数；与资源层快照上限保持一致。
pub const MAX_FILE_CHANGE_BYTES: u64 = 64 * 1024 * 1024;
/// 单次文件变更读取允许返回的最大原始字节数；保留默认 ACP 响应预算空间。
pub const MAX_FILE_CHANGE_READ_BYTES: u32 = 512 * 1024;
/// 文件变更引用在资源链接中允许的最大路径字节数。
const MAX_FILE_CHANGE_PATH_BYTES: usize = 32 * 1024;
/// ACP 标识允许的最大 UTF-8 字节数；与协议其他扩展标识保持一致。
const MAX_IDENTIFIER_BYTES: usize = 256;
/// 小写 SHA-256 文本的固定字节数。
const SHA256_HEX_BYTES: usize = 64;
/// 空文件 SHA-256 的固定值；与资源层快照结构保持一致。
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// 单次读取上限对应的标准 Base64 编码最大字符数。
const MAX_BASE64_BYTES: usize = (MAX_FILE_CHANGE_READ_BYTES as usize).div_ceil(3) * 4;

/// 文件变更快照的两侧。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeSide {
    /// 写入前的原始文件快照；原文件不存在时由 Runtime 拒绝读取。
    Before,
    /// 写入后的原始文件快照。
    After,
}

/// 不携带文件正文的单侧快照信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSnapshotInfo {
    /// 完整快照的原始字节数。
    pub size_bytes: u64,
    /// 完整快照的小写 SHA-256。
    pub sha256: String,
}

impl FileSnapshotInfo {
    /// 创建一份快照描述，不读取文件正文。
    pub fn new(size_bytes: u64, sha256: impl Into<String>) -> Self {
        Self {
            size_bytes,
            sha256: sha256.into(),
        }
    }

    /// 校验快照大小和摘要的固定形状。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        if self.size_bytes > MAX_FILE_CHANGE_BYTES
            || !valid_sha256(&self.sha256)
            || (self.size_bytes == 0 && self.sha256 != EMPTY_SHA256)
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// 可放入 ACP resource_link._meta 的持久文件变更引用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileChangeReference {
    /// 产生该文件变更的根 Session 标识。
    pub session_id: String,
    /// 产生该文件变更的工具请求标识。
    pub request_id: String,
    /// 文件的跨平台绝对路径；正文不通过该引用传输。
    pub path: String,
    /// 写入前快照；缺失表示原文件不存在。
    pub before: Option<FileSnapshotInfo>,
    /// 写入后快照。
    pub after: FileSnapshotInfo,
    /// 文件变更是否已由工具确认实际应用。
    pub applied: bool,
}

impl FileChangeReference {
    /// 校验引用身份、路径和两侧快照描述。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.request_id, MAX_IDENTIFIER_BYTES)?;
        if self.path.is_empty()
            || self.path.len() > MAX_FILE_CHANGE_PATH_BYTES
            || self.path.chars().any(char::is_control)
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        if let Some(before) = &self.before {
            before.validate()?;
        }
        self.after.validate()
    }
}

/// keencode/session/file-change/read 的分页读取请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileChangeRequest {
    /// 目标根 Session 标识。
    pub session_id: String,
    /// 已提交文件变更所属的工具请求标识。
    pub request_id: String,
    /// 要读取的快照侧。
    pub side: FileChangeSide,
    /// 按原始字节计数的读取起点。
    pub offset: u64,
    /// 要读取的最大原始字节数。
    pub length: u32,
}

impl ReadFileChangeRequest {
    /// 创建一份分页读取请求。
    pub fn new(
        session_id: impl Into<String>,
        request_id: impl Into<String>,
        side: FileChangeSide,
        offset: u64,
        length: u32,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            request_id: request_id.into(),
            side,
            offset,
            length,
        }
    }

    /// 校验请求身份、整数边界和单次读取区间上限。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.request_id, MAX_IDENTIFIER_BYTES)?;
        if self.offset > MAX_FILE_CHANGE_BYTES
            || !(1..=MAX_FILE_CHANGE_READ_BYTES).contains(&self.length)
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// keencode/session/file-change/read 的严格分页读取响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileChangeResponse {
    /// 与请求完全一致的根 Session 标识。
    pub session_id: String,
    /// 与请求完全一致的工具请求标识。
    pub request_id: String,
    /// 与请求完全一致的快照侧。
    pub side: FileChangeSide,
    /// 本页原始字节起点。
    pub offset: u64,
    /// 所选完整快照的原始字节数。
    pub total_bytes: u64,
    /// 所选完整快照的小写 SHA-256。
    pub sha256: String,
    /// 本页原始字节的标准 Base64 编码。
    pub data: String,
    /// 本页是否已经到达完整快照末尾。
    pub eof: bool,
}

impl ReadFileChangeResponse {
    /// 解码并严格确认响应中的标准 Base64 正规形式。
    pub fn decoded_data(&self) -> Result<Vec<u8>, AcpBoundaryError> {
        if self.data.len() > MAX_BASE64_BYTES {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        let bytes = BASE64_STANDARD
            .decode(&self.data)
            .map_err(|_| AcpBoundaryError::InvalidSemanticValue)?;
        if BASE64_STANDARD.encode(&bytes) != self.data
            || bytes.len() > MAX_FILE_CHANGE_READ_BYTES as usize
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(bytes)
    }

    /// 校验响应边界、分页关系、Base64 正规形式和完整快照摘要形状。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.request_id, MAX_IDENTIFIER_BYTES)?;
        if self.offset > MAX_FILE_CHANGE_BYTES || self.total_bytes > MAX_FILE_CHANGE_BYTES {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        if !valid_sha256(&self.sha256) || self.offset > self.total_bytes {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        let bytes = self.decoded_data()?;
        let end = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or(AcpBoundaryError::InvalidSemanticValue)?;
        if end > self.total_bytes || self.eof != (end == self.total_bytes) {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        if bytes.is_empty() && !self.eof {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        // 完整单页时可以独立验证摘要；分页响应仍至少验证其固定摘要形状。
        if self.offset == 0 && end == self.total_bytes {
            let digest = format!("{:x}", Sha256::digest(&bytes));
            if digest != self.sha256 {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
        }
        Ok(())
    }
}

/// 判断字符串是否为唯一的小写 SHA-256 十六进制编码。
fn valid_sha256(value: &str) -> bool {
    value.len() == SHA256_HEX_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
