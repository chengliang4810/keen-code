//! 跨 Desktop、Web、TUI 和 Agent CLI 共享的 Host 协议契约。
//!
//! 本模块只定义稳定的 wire/value 类型和校验，不持有 Runtime、Tauri 或传输连接。
//! 运行时必须把权威 Journal sequence 与连接级 delivery sequence 分开维护：前者
//! 可用于断线恢复，后者只保证单连接内排序，不能作为跨连接的恢复游标。

use std::fmt;
use std::fs;
use std::path::Path;

use agent_client_protocol_schema::Meta;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::AcpBoundaryError;
use crate::json::validate_identifier;

/// Host 协议当前的 wire 版本。
pub const HOST_PROTOCOL_VERSION: u16 = 1;
/// Host discovery 记录的 wire schema 版本。
pub const HOST_DISCOVERY_SCHEMA_VERSION: u16 = 1;
/// Host discovery 使用的固定协议标识。
pub const HOST_DISCOVERY_PROTOCOL: &str = "keencode-acp-ndjson";
/// 初始化响应中 Host 数据根指纹的 `_meta` 键。
pub const HOST_DATA_ROOT_FINGERPRINT_META_KEY: &str = "keencode/host/dataRootFingerprint";
/// 初始化响应中 Host owner 类型的 `_meta` 键。
pub const HOST_OWNER_KIND_META_KEY: &str = "keencode/host/ownerKind";
/// ACP 初始化中已有的工作目录 `_meta` 键；Host 不得删除或覆盖它。
pub const DEFAULT_CWD_META_KEY: &str = "keencode/defaultCwd";
/// Host 运行时单条提示正文允许占用的最大 UTF-8 字节数。
pub const MAX_HOST_PROMPT_BYTES: usize = 64 * 1024;
/// Host discovery 端点文本允许占用的最大 UTF-8 字节数。
pub const MAX_HOST_ENDPOINT_BYTES: usize = 4 * 1024;
/// Host discovery 启动时间文本允许占用的最大 UTF-8 字节数。
pub const MAX_HOST_STARTED_AT_BYTES: usize = 128;
/// Host data-root fingerprint 固定为 SHA-256 十六进制文本长度。
pub const DATA_ROOT_FINGERPRINT_HEX_BYTES: usize = 64;

/// Host owner 的稳定 wire 值。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOwnerKind {
    /// Desktop 进程内嵌并拥有 Host Core。
    Desktop,
    /// 独立 headless Host 进程拥有 Host Core。
    Headless,
}

/// Host discovery 使用的本地 IPC 传输类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostTransportKind {
    /// Unix/macOS 本地 socket。
    UnixSocket,
    /// Windows 受 ACL 保护的命名管道。
    NamedPipe,
}

impl HostTransportKind {
    /// 返回固定的 wire 文本。
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::UnixSocket => "unix_socket",
            Self::NamedPipe => "named_pipe",
        }
    }
}

impl HostOwnerKind {
    /// 返回固定的 wire 文本 `desktop` 或 `headless`。
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Headless => "headless",
        }
    }

    /// 从固定 wire 文本解析 owner 类型，不接受别名或大小写变体。
    pub fn parse_wire(value: &str) -> Result<Self, AcpBoundaryError> {
        match value {
            "desktop" => Ok(Self::Desktop),
            "headless" => Ok(Self::Headless),
            _ => Err(AcpBoundaryError::InvalidSemanticValue),
        }
    }
}

/// 连接相对于 Host owner 的生命周期角色。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostConnectionRole {
    /// 进程内嵌 Host 并负责持有根级 OS lease 的 Desktop。
    DesktopOwner,
    /// 独立 headless Host 进程。
    HeadlessOwner,
    /// 连接已有 Host、但不拥有根级 lease 的 Desktop。
    DesktopClient,
    /// 普通 Web、TUI 或 Agent CLI 客户端；断开只释放连接。
    Client,
    /// 请求任务在客户端断开后继续运行的 CLI 客户端。
    DetachedClient,
}

impl HostConnectionRole {
    /// 判断该连接是否有权触发 Host shutdown。
    pub const fn owns_host(self) -> bool {
        matches!(self, Self::DesktopOwner | Self::HeadlessOwner)
    }

    /// 判断客户端断开后是否只执行 detach，而不影响 Host。
    pub const fn disconnect_action(self) -> HostDisconnectAction {
        // 传输断开不等于用户明确退出：Desktop 关闭到托盘、WebSocket 短断线和
        // CLI 网络重连都必须保留 Host/Runtime。owner 权限只用于显式 shutdown 命令。
        let _ = self;
        HostDisconnectAction::DetachClient
    }
}

/// 连接断开时 Host 应执行的生命周期动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostDisconnectAction {
    /// 任意传输连接断开；Runtime、队列和 Agent 任务继续由 Host 管理。
    DetachClient,
}

/// Host 连接的稳定标识；不能复用 JSON-RPC request id。
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ConnectionId(String);

impl ConnectionId {
    /// 创建一个有界、无控制字符的连接标识。
    pub fn new(value: impl Into<String>) -> Result<Self, AcpBoundaryError> {
        let value = value.into();
        validate_identifier(&value, 256)?;
        Ok(Self(value))
    }

    /// 返回连接标识文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费并返回连接标识文本。
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ConnectionId {
    /// 输出稳定连接标识。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 跨连接幂等的 Host 操作标识。
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    /// 创建一个有界操作标识。
    pub fn new(value: impl Into<String>) -> Result<Self, AcpBoundaryError> {
        let value = value.into();
        validate_identifier(&value, 256)?;
        Ok(Self(value))
    }

    /// 返回操作标识文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费并返回操作标识文本。
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for OperationId {
    /// 输出稳定操作标识。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Host discovery 文件或握手中携带的完整只读描述。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostDiscoveryRecord {
    /// Discovery 记录格式版本；不匹配时客户端必须停止连接。
    pub schema_version: u16,
    /// Discovery 使用的固定传输协议名称。
    pub protocol: String,
    /// Host ACP 协议版本；不匹配时客户端必须停止连接。
    pub protocol_version: u16,
    /// Host discovery 使用的本地 IPC 传输类型。
    pub transport: HostTransportKind,
    /// 本地端点文本；不能携带认证 Token。
    pub endpoint: String,
    /// 进程 owner 类型，固定为 `desktop` 或 `headless`。
    pub owner_kind: HostOwnerKind,
    /// 仅供诊断展示的进程 ID；不能代替 OS 锁。
    pub pid: u32,
    /// Host 稳定实例标识；不作为所有权判断依据。
    pub host_id: String,
    /// 根级数据路径的 SHA-256 指纹；不暴露原始用户路径。
    pub data_root_fingerprint: String,
    /// Host 启动时间，通常为 RFC 3339 文本；仅用于诊断展示。
    pub started_at: String,
}

impl HostDiscoveryRecord {
    /// 创建一条 discovery 记录并由共享协议计算数据根指纹。
    pub fn new(
        transport: HostTransportKind,
        endpoint: impl Into<String>,
        owner_kind: HostOwnerKind,
        pid: u32,
        data_root: impl AsRef<Path>,
        started_at: impl Into<String>,
    ) -> Result<Self, HostDiscoveryValidationError> {
        let data_root_fingerprint =
            data_root_fingerprint(data_root).map_err(HostDiscoveryValidationError::Fingerprint)?;
        Self::new_with_host_id(
            transport,
            endpoint,
            owner_kind,
            pid,
            format!("keencode-host-{pid}"),
            data_root_fingerprint,
            started_at,
        )
    }

    /// 创建一条使用调用方指定 Host ID 的 discovery 记录。
    pub fn new_with_host_id(
        transport: HostTransportKind,
        endpoint: impl Into<String>,
        owner_kind: HostOwnerKind,
        pid: u32,
        host_id: impl Into<String>,
        data_root_fingerprint: impl Into<String>,
        started_at: impl Into<String>,
    ) -> Result<Self, HostDiscoveryValidationError> {
        let record = Self {
            schema_version: HOST_DISCOVERY_SCHEMA_VERSION,
            protocol: HOST_DISCOVERY_PROTOCOL.to_owned(),
            protocol_version: HOST_PROTOCOL_VERSION,
            transport,
            endpoint: endpoint.into(),
            owner_kind,
            pid,
            host_id: host_id.into(),
            data_root_fingerprint: data_root_fingerprint.into(),
            started_at: started_at.into(),
        };
        record.validate()?;
        Ok(record)
    }

    /// 校验 discovery 记录的固定字段、有界文本和当前平台传输类型。
    pub fn validate(&self) -> Result<(), HostDiscoveryValidationError> {
        if self.schema_version != HOST_DISCOVERY_SCHEMA_VERSION {
            return Err(HostDiscoveryValidationError::InvalidField("schemaVersion"));
        }
        if self.protocol != HOST_DISCOVERY_PROTOCOL {
            return Err(HostDiscoveryValidationError::InvalidField("protocol"));
        }
        if self.protocol_version != HOST_PROTOCOL_VERSION || self.pid == 0 {
            return Err(HostDiscoveryValidationError::InvalidField(
                "protocolVersion/pid",
            ));
        }
        if self.endpoint.is_empty()
            || self.endpoint.len() > MAX_HOST_ENDPOINT_BYTES
            || self.endpoint.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(HostDiscoveryValidationError::InvalidField("endpoint"));
        }
        validate_identifier(&self.host_id, 256)
            .map_err(|_| HostDiscoveryValidationError::InvalidField("hostId"))?;
        if self.started_at.is_empty()
            || self.started_at.len() > MAX_HOST_STARTED_AT_BYTES
            || self.started_at.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(HostDiscoveryValidationError::InvalidField("startedAt"));
        }
        validate_root_fingerprint(&self.data_root_fingerprint)
            .map_err(|_| HostDiscoveryValidationError::InvalidField("dataRootFingerprint"))?;
        #[cfg(windows)]
        if self.transport != HostTransportKind::NamedPipe {
            return Err(HostDiscoveryValidationError::InvalidField("transport"));
        }
        #[cfg(unix)]
        if self.transport != HostTransportKind::UnixSocket {
            return Err(HostDiscoveryValidationError::InvalidField("transport"));
        }
        Ok(())
    }

    /// 校验 discovery 记录并确认它属于指定数据根。
    pub fn validate_for_root(
        &self,
        data_root: impl AsRef<Path>,
    ) -> Result<(), HostDiscoveryValidationError> {
        self.validate()?;
        let expected =
            data_root_fingerprint(data_root).map_err(HostDiscoveryValidationError::Fingerprint)?;
        if self.data_root_fingerprint != expected {
            return Err(HostDiscoveryValidationError::RootMismatch);
        }
        Ok(())
    }

    /// 将 discovery 中的 Host identity 转为 ACP `initialize` 响应元数据。
    pub fn initialize_meta(&self) -> Result<Meta, AcpBoundaryError> {
        host_initialize_meta(self.data_root_fingerprint.clone(), self.owner_kind)
    }
}

/// Host discovery 记录的统一校验错误。
#[derive(Debug)]
pub enum HostDiscoveryValidationError {
    /// 记录字段不符合当前 wire 版本或平台约束。
    InvalidField(&'static str),
    /// 记录中的数据根指纹与当前数据根不匹配。
    RootMismatch,
    /// 计算当前数据根指纹失败。
    Fingerprint(std::io::Error),
}

impl fmt::Display for HostDiscoveryValidationError {
    /// 输出不包含路径、端点或认证材料的稳定错误摘要。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "Host discovery 字段无效: {field}"),
            Self::RootMismatch => formatter.write_str("Host discovery 数据根不匹配"),
            Self::Fingerprint(error) => write!(formatter, "Host 数据根指纹计算失败: {error}"),
        }
    }
}

impl std::error::Error for HostDiscoveryValidationError {}

/// 生成给 ACP `initialize` 响应使用的 Host `_meta` 字段。
///
/// 该函数只修改 Host 自己的两个键，调用方已有的 `keencode/defaultCwd` 和其他
/// ACP 扩展字段必须由调用方继续保留。
pub fn host_initialize_meta(
    data_root_fingerprint: impl Into<String>,
    owner_kind: HostOwnerKind,
) -> Result<Meta, AcpBoundaryError> {
    let data_root_fingerprint = data_root_fingerprint.into();
    validate_root_fingerprint(&data_root_fingerprint)?;
    let mut meta = Map::new();
    meta.insert(
        HOST_DATA_ROOT_FINGERPRINT_META_KEY.to_owned(),
        Value::String(data_root_fingerprint),
    );
    meta.insert(
        HOST_OWNER_KIND_META_KEY.to_owned(),
        Value::String(owner_kind.as_wire().to_owned()),
    );
    Ok(meta)
}

/// 按实际用户数据根路径计算稳定指纹，不向 wire 暴露原始路径。
///
/// 首选 canonical path 以消除符号链接和 `.`/`..` 差异；路径不存在或 canonicalize
/// 失败时直接报错，不能以未经规范化的路径继续连接不同 Host。
pub fn data_root_fingerprint(path: impl AsRef<Path>) -> std::io::Result<String> {
    let canonical = fs::canonicalize(path)?;
    let normalized = normalize_data_root_for_fingerprint(&canonical);
    Ok(format!("{:x}", Sha256::digest(normalized.as_bytes())))
}

/// 返回 Host 运行阶段；owner 退出必须经历 draining，不能直接丢弃 Runtime。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLifecyclePhase {
    /// 已创建 owner lease，尚未对客户端提供服务。
    Starting,
    /// Runtime、Journal、Provider 和服务端点已就绪。
    Ready,
    /// 不再接受新工作，等待当前任务、响应和投递收敛。
    Draining,
    /// 所有资源已关闭，不能重新接受连接。
    Stopped,
    /// 启动或 shutdown 失败，必须由外层重新创建 Host。
    Failed,
}

/// Journal 与连接 delivery 两种不同序号的恢复游标。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionEventCursor {
    /// 持久 Journal 的权威序号，可用于跨连接或 Host 重启恢复。
    pub journal_sequence: u64,
    /// 当前连接的易失 delivery 序号；只用于检测单连接内丢序。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_sequence: Option<u64>,
}

impl SessionEventCursor {
    /// 创建只依赖持久 Journal 的恢复游标。
    pub const fn journal(journal_sequence: u64) -> Self {
        Self {
            journal_sequence,
            delivery_sequence: None,
        }
    }

    /// 绑定当前连接 delivery 序号；不改变 Journal 恢复水位。
    pub const fn with_delivery(self, delivery_sequence: u64) -> Self {
        Self {
            journal_sequence: self.journal_sequence,
            delivery_sequence: Some(delivery_sequence),
        }
    }
}

/// Host 断线后为客户端选择的恢复动作。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventRecoveryPlan {
    /// 先加载一致 Snapshot，再从其 Journal 水位之后读取权威事件。
    SnapshotThenJournal,
    /// Snapshot 已足够新，只需读取其后的权威 Journal 事件。
    JournalOnly,
    /// 仅建立新连接后的实时订阅；不能伪造旧 transient stream。
    LiveOnly,
    /// 游标损坏、Journal 截断或协议不匹配，必须重新获取完整状态。
    ResyncRequired,
}

/// Host 生命周期动作在协议层的结果。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLifecycleAction {
    /// 连接普通客户端，不改变 Host owner。
    Attach,
    /// 客户端断开但任务继续由 Host 托管。
    Detach,
    /// owner 进入 draining 并完成 Host shutdown。
    Shutdown,
    /// Host 因错误停止服务，需要外层重新创建。
    Fail,
}

fn validate_root_fingerprint(value: &str) -> Result<(), AcpBoundaryError> {
    if value.len() != DATA_ROOT_FINGERPRINT_HEX_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    Ok(())
}

fn normalize_data_root_for_fingerprint(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    #[cfg(windows)]
    {
        normalized.make_ascii_lowercase();
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_wire值固定且不接受别名() {
        assert_eq!(HostOwnerKind::Desktop.as_wire(), "desktop");
        assert_eq!(HostOwnerKind::Headless.as_wire(), "headless");
        assert_eq!(
            HostOwnerKind::parse_wire("desktop").unwrap(),
            HostOwnerKind::Desktop
        );
        assert!(HostOwnerKind::parse_wire("Desktop").is_err());
        assert!(HostOwnerKind::parse_wire("server").is_err());
    }

    #[test]
    fn 数据根指纹只接受固定小写或大写十六进制文本() {
        let root =
            std::env::temp_dir().join(format!("keencode-host-protocol-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        let fingerprint = data_root_fingerprint(&root).unwrap();
        assert_eq!(fingerprint.len(), DATA_ROOT_FINGERPRINT_HEX_BYTES);
        let record = HostDiscoveryRecord {
            schema_version: HOST_DISCOVERY_SCHEMA_VERSION,
            protocol: HOST_DISCOVERY_PROTOCOL.to_owned(),
            protocol_version: HOST_PROTOCOL_VERSION,
            transport: if cfg!(windows) {
                HostTransportKind::NamedPipe
            } else {
                HostTransportKind::UnixSocket
            },
            endpoint: "ipc://keencode".to_owned(),
            owner_kind: HostOwnerKind::Desktop,
            pid: 7,
            host_id: "host-1".to_owned(),
            data_root_fingerprint: fingerprint,
            started_at: "2026-09-21T12:00:00Z".to_owned(),
        };
        record.validate().expect("合法 discovery 记录应通过");

        let invalid = HostDiscoveryRecord {
            data_root_fingerprint: "z".repeat(DATA_ROOT_FINGERPRINT_HEX_BYTES),
            ..record
        };
        assert!(invalid.validate().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cursor明确区分journal和delivery序号() {
        let cursor = SessionEventCursor::journal(42).with_delivery(7);
        let encoded = serde_json::to_value(cursor).unwrap();
        assert_eq!(encoded["journalSequence"], 42);
        assert_eq!(encoded["deliverySequence"], 7);
        let decoded: SessionEventCursor = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, cursor);
        assert_eq!(EventRecoveryPlan::LiveOnly, EventRecoveryPlan::LiveOnly);
    }

    #[test]
    fn 任意连接断开都只detach显式owner命令才shutdown() {
        assert_eq!(
            HostConnectionRole::DesktopOwner.disconnect_action(),
            HostDisconnectAction::DetachClient
        );
        assert_eq!(
            HostConnectionRole::Client.disconnect_action(),
            HostDisconnectAction::DetachClient
        );
        assert_eq!(
            HostConnectionRole::DetachedClient.disconnect_action(),
            HostDisconnectAction::DetachClient
        );
    }
}
