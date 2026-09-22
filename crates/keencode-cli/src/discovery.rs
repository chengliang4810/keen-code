//! 同数据根 Host 的发现记录与路径规则。
//!
//! 发现文件只是一条提示信息。它不能代表 Host 所有权，不能据此删除锁、抢占
//! Runtime 或绕过 ACP `initialize` 握手。Host 必须先持有根级 `host.lock`，再用
//! 原子写入发布该文件；客户端连接后仍需校验握手中的数据根指纹与协议版本。

use keencode_acp::HostDiscoveryValidationError;
pub use keencode_acp::{
    HostDiscoveryRecord as EndpointRecord, HostOwnerKind, HostTransportKind as IpcTransport,
};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Host 发现文件名。
pub const ENDPOINT_FILE_NAME: &str = "host.endpoint.json";
/// 发现文件允许的最大字节数，避免把任意大文件当作 JSON 解析。
const MAX_ENDPOINT_FILE_BYTES: u64 = 16 * 1024;

/// 发现记录校验错误。
#[derive(Debug)]
pub enum EndpointRecordError {
    /// 文件系统读取失败。
    Io(std::io::Error),
    /// JSON 形状或字段不符合当前协议。
    InvalidJson,
    /// 记录字段违反安全边界。
    InvalidField(&'static str),
    /// 发现记录与当前数据根不匹配。
    RootMismatch,
}

impl fmt::Display for EndpointRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "读取 Host 发现记录失败: {error}"),
            Self::InvalidJson => formatter.write_str("Host 发现记录 JSON 无效"),
            Self::InvalidField(field) => write!(formatter, "Host 发现记录字段无效: {field}"),
            Self::RootMismatch => formatter.write_str("Host 发现记录不属于当前数据根"),
        }
    }
}

impl std::error::Error for EndpointRecordError {}

impl From<std::io::Error> for EndpointRecordError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<HostDiscoveryValidationError> for EndpointRecordError {
    fn from(error: HostDiscoveryValidationError) -> Self {
        match error {
            HostDiscoveryValidationError::InvalidField(field) => Self::InvalidField(field),
            HostDiscoveryValidationError::RootMismatch => Self::RootMismatch,
            HostDiscoveryValidationError::Fingerprint(error) => Self::Io(error),
        }
    }
}

/// 从数据根读取并严格校验共享 ACP discovery 记录。
pub fn read_endpoint_record(data_root: &Path) -> Result<EndpointRecord, EndpointRecordError> {
    let path = endpoint_path(data_root);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(EndpointRecordError::InvalidField("endpoint file type"));
    }
    if metadata.len() > MAX_ENDPOINT_FILE_BYTES {
        return Err(EndpointRecordError::InvalidField("endpoint file size"));
    }
    let bytes = fs::read(path)?;
    let record = serde_json::from_slice::<EndpointRecord>(&bytes)
        .map_err(|_| EndpointRecordError::InvalidJson)?;
    record
        .validate_for_root(data_root)
        .map_err(EndpointRecordError::from)?;
    Ok(record)
}

/// 返回发现记录路径。
pub fn endpoint_path(data_root: &Path) -> PathBuf {
    data_root.join(ENDPOINT_FILE_NAME)
}

/// 计算数据根的稳定指纹；实现由共享 ACP 协议提供。
pub use keencode_acp::data_root_fingerprint;

/// 返回与 Desktop `storage.rs` 一致的默认数据根。
pub fn default_data_root() -> Result<PathBuf, EndpointRecordError> {
    if std::env::var("KEENCODE_BENCHMARK").ok().as_deref() == Some("1") {
        if let Some(path) =
            std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR").filter(|path| !path.is_empty())
        {
            return Ok(PathBuf::from(path));
        }
    }
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    }
    .filter(|path| !path.is_empty())
    .ok_or(EndpointRecordError::InvalidField("home directory"))?;
    let directory = if cfg!(debug_assertions) {
        ".keencode-dev"
    } else {
        ".keencode"
    };
    Ok(PathBuf::from(home).join(directory))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 发现记录保持字段和根指纹() {
        let root = tempfile::tempdir().expect("临时根目录应创建");
        let record = EndpointRecord::new(
            if cfg!(windows) {
                IpcTransport::NamedPipe
            } else {
                IpcTransport::UnixSocket
            },
            if cfg!(windows) {
                r"\\.\pipe\keencode-test"
            } else {
                "/tmp/keencode-test.sock"
            },
            HostOwnerKind::Headless,
            42,
            root.path(),
            "2026-09-21T12:00:00Z",
        )
        .expect("记录应有效");
        let json = serde_json::to_vec(&record).expect("记录应可序列化");
        let decoded: EndpointRecord = serde_json::from_slice(&json).expect("记录应可恢复");
        assert_eq!(record, decoded);
        assert!(decoded.validate_for_root(root.path()).is_ok());
        assert!(
            decoded
                .validate_for_root(root.path().join("other"))
                .is_err()
        );
        assert_eq!(
            record.data_root_fingerprint,
            keencode_acp::data_root_fingerprint(root.path()).expect("ACP 指纹应可计算")
        );
        assert!(
            serde_json::to_value(&record)
                .expect("记录应可编码")
                .get("hostId")
                .is_some()
        );
    }

    #[test]
    fn 发现路径固定在数据根内() {
        let root = Path::new("/tmp/keencode");
        assert_eq!(endpoint_path(root), root.join(ENDPOINT_FILE_NAME));
    }
}
