//! Desktop/headless Host 的 ownership、发现记录和生命周期状态。
//!
//! 这里不启动任何传输（WebSocket、本地 IPC）或 Agent Runtime。它只固化“先抢根级
//! OS lease，失败后读取既有 Host discovery，Owned/Client 分支不能混用”的状态机，
//! 负责 OS lease、discovery 文件和 Host Core 生命周期；真正的 Runtime/Transport
//! 装配由上层完成。Desktop、headless 进程和测试可以共享同一套所有权事实。

use keencode_acp::schema::Meta;
use keencode_acp::{
    ConnectionId, HostConnectionRole, HostDisconnectAction, HostDiscoveryRecord,
    HostDiscoveryValidationError, HostLifecycleAction, HostLifecyclePhase, HostOwnerKind,
    HostTransportKind, data_root_fingerprint, host_initialize_meta,
};
use keencode_resources::{HostLease, HostLeaseAcquire, HostOwner};
use sha2::{Digest, Sha256};
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{HostCoreConfig, HostCoreError, HostLifecycleController, HostPromptQueue};

/// 与 Desktop、CLI 共享的 Host discovery 文件名。
pub const HOST_DISCOVERY_FILE_NAME: &str = "host.endpoint.json";
/// discovery 文件读取上限。
const MAX_DISCOVERY_BYTES: u64 = 16 * 1024;

/// Host 当前是根级 owner 还是连接既有 owner 的 client。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostRuntimeMode {
    /// 当前进程持有根级 OS lease。
    Owned,
    /// 当前进程没有 lease，仅通过 discovery 连接既有 Host。
    Client,
}

/// Host 获取结果；Busy 只有在 discovery 校验成功后才返回 Client。
#[derive(Clone)]
pub enum HostRuntimeAcquire {
    /// 当前进程取得 owner lease。
    Owned(Arc<HostRuntime>),
    /// 当前进程发现并校验了既有 Host，不能再创建第二套 Runtime。
    Client(Arc<HostRuntime>),
}

impl HostRuntimeAcquire {
    /// 返回内部 Host 状态引用。
    pub fn runtime(&self) -> &Arc<HostRuntime> {
        match self {
            Self::Owned(runtime) | Self::Client(runtime) => runtime,
        }
    }

    /// 返回当前进程是否拥有根级 Host lease。
    pub const fn mode(&self) -> HostRuntimeMode {
        match self {
            Self::Owned(_) => HostRuntimeMode::Owned,
            Self::Client(_) => HostRuntimeMode::Client,
        }
    }
}

impl std::fmt::Debug for HostRuntimeAcquire {
    /// 只展示 owner/client 模式，不回显根路径或 endpoint。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple(match self {
                Self::Owned(_) => "Owned",
                Self::Client(_) => "Client",
            })
            .finish()
    }
}

/// Host 根级生命周期和共享 Prompt admission 状态。
pub struct HostRuntime {
    data_root: PathBuf,
    data_root_fingerprint: String,
    mode: HostRuntimeMode,
    owner_kind: HostOwnerKind,
    host_id: String,
    lease: Mutex<Option<HostLease>>,
    discovery: Mutex<Option<HostDiscoveryRecord>>,
    lifecycle: HostLifecycleController,
    prompt_queue: Arc<HostPromptQueue>,
}

impl HostRuntime {
    /// 先尝试取得根级 OS lease；竞争时只读取并校验既有 discovery。
    pub fn acquire(
        data_root: impl AsRef<Path>,
        owner_kind: HostOwnerKind,
    ) -> Result<HostRuntimeAcquire, HostRuntimeError> {
        // 首次启动时数据根可能尚不存在；先创建目录，再规范化并计算指纹。
        // 不把未经 canonicalize 的路径交给 lease/discovery，避免同一根目录因
        // 相对路径或符号链接产生多个 Host 身份。
        fs::create_dir_all(data_root.as_ref()).map_err(HostRuntimeError::Io)?;
        let data_root = fs::canonicalize(data_root.as_ref()).map_err(HostRuntimeError::Io)?;
        let owner = match owner_kind {
            HostOwnerKind::Desktop => HostOwner::Desktop,
            HostOwnerKind::Headless => HostOwner::Headless,
        };
        match HostLease::try_acquire(&data_root, owner).map_err(HostRuntimeError::Resource)? {
            HostLeaseAcquire::Acquired(lease) => {
                let host_id = make_host_id(&data_root, owner_kind);
                let runtime = Arc::new(Self::new(
                    data_root,
                    HostRuntimeMode::Owned,
                    owner_kind,
                    host_id,
                    Some(lease),
                    None,
                )?);
                runtime.attach(
                    ConnectionId::new("embedded-owner").map_err(HostRuntimeError::Protocol)?,
                    match owner_kind {
                        HostOwnerKind::Desktop => HostConnectionRole::DesktopOwner,
                        HostOwnerKind::Headless => HostConnectionRole::HeadlessOwner,
                    },
                )?;
                Ok(HostRuntimeAcquire::Owned(runtime))
            }
            HostLeaseAcquire::Busy { .. } => {
                let record = Self::read_discovery(&data_root)?;
                record
                    .validate_for_root(&data_root)
                    .map_err(map_discovery_validation)?;
                let client_role = match owner_kind {
                    HostOwnerKind::Desktop => HostConnectionRole::DesktopClient,
                    HostOwnerKind::Headless => HostConnectionRole::Client,
                };
                let runtime = Arc::new(Self::new(
                    data_root,
                    HostRuntimeMode::Client,
                    record.owner_kind,
                    record.host_id.clone(),
                    None,
                    Some(record),
                )?);
                runtime
                    .lifecycle
                    .mark_ready()
                    .map_err(HostRuntimeError::Core)?;
                runtime.attach(
                    ConnectionId::new("host-client").map_err(HostRuntimeError::Protocol)?,
                    client_role,
                )?;
                Ok(HostRuntimeAcquire::Client(runtime))
            }
        }
    }

    /// 返回当前 Host 数据根。
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// 返回当前数据根指纹。
    pub fn data_root_fingerprint(&self) -> &str {
        &self.data_root_fingerprint
    }

    /// 返回当前进程的 owner/client 模式。
    pub const fn mode(&self) -> HostRuntimeMode {
        self.mode
    }

    /// 返回 discovery 中的 owner 类型。
    pub const fn owner_kind(&self) -> HostOwnerKind {
        self.owner_kind
    }

    /// 返回 Host 实例 ID；不把它当作 lease 或 PID 证明。
    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    /// 返回共享 Prompt admission queue。
    pub fn prompt_queue(&self) -> &Arc<HostPromptQueue> {
        &self.prompt_queue
    }

    /// 返回 Host 生命周期 phase。
    pub fn phase(&self) -> Result<HostLifecyclePhase, HostRuntimeError> {
        self.lifecycle.phase().map_err(HostRuntimeError::Core)
    }

    /// 取得当前 Host 初始化 identity meta；Client 使用 discovery 记录的值。
    pub fn initialize_meta(&self) -> Result<Meta, HostRuntimeError> {
        if let Some(record) = self.lock_discovery()?.as_ref() {
            return host_initialize_meta(record.data_root_fingerprint.clone(), record.owner_kind)
                .map_err(HostRuntimeError::Protocol);
        }
        host_initialize_meta(self.data_root_fingerprint.clone(), self.owner_kind)
            .map_err(HostRuntimeError::Protocol)
    }

    /// 注册一个传输连接；连接 ID 必须由 transport 层生成，不能使用 JSON-RPC ID。
    pub fn attach_client(&self, connection_id: ConnectionId) -> Result<(), HostRuntimeError> {
        self.attach(connection_id, HostConnectionRole::Client)
    }

    /// 注销一个已经断开的传输连接。
    pub fn detach(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<HostDisconnectAction, HostRuntimeError> {
        self.lifecycle
            .disconnect(connection_id)
            .map_err(HostRuntimeError::Core)
    }

    /// Owned Host 在 Runtime 和 transport 已绑定后原子发布 discovery 并进入 Ready。
    pub fn mark_ready_and_publish(
        &self,
        transport: HostTransportKind,
        endpoint: impl Into<String>,
    ) -> Result<HostDiscoveryRecord, HostRuntimeError> {
        self.require_owned()?;
        let record = HostDiscoveryRecord::new_with_host_id(
            transport,
            endpoint,
            self.owner_kind,
            std::process::id(),
            self.host_id.clone(),
            self.data_root_fingerprint.clone(),
            started_at(),
        )
        .map_err(map_discovery_validation)?;
        let mut discovery = self.lock_discovery()?;
        publish_discovery(&self.data_root, &record)?;
        if let Err(error) = self.lifecycle.mark_ready() {
            // 如果 discovery 已可见但 Host 尚未 Ready，客户端可能连接到尚未
            // 服务的端点；清理这个不应发生的中间状态，不能掩盖原始错误。
            let _ = remove_discovery_if_owned(&self.data_root, &record);
            return Err(HostRuntimeError::Core(error));
        }
        *discovery = Some(record.clone());
        Ok(record)
    }

    /// 读取当前 discovery 快照；Owner 与 Client 均可调用。
    pub fn discovery(&self) -> Result<Option<HostDiscoveryRecord>, HostRuntimeError> {
        Ok(self.lock_discovery()?.clone())
    }

    /// 只有明确的 owner shutdown 才允许进入 Draining。
    pub fn explicit_shutdown(&self) -> Result<HostLifecycleAction, HostRuntimeError> {
        self.require_owned()?;
        let owner_connection =
            ConnectionId::new("embedded-owner").map_err(HostRuntimeError::Protocol)?;
        let action = self
            .lifecycle
            .begin_shutdown(&owner_connection)
            .map_err(HostRuntimeError::Core)?;
        let discovery_result = self.remove_owned_discovery();
        let lease = self
            .lease
            .lock()
            .map_err(|_| HostRuntimeError::StateUnavailable)?
            .take();
        let lease_result = lease.map_or(Ok(()), |lease| {
            // release(self) 消费租约；显式 shutdown 后不能继续被当作 owner。
            lease.release().map_err(HostRuntimeError::Resource)
        });
        let finish_result = self
            .lifecycle
            .finish_shutdown()
            .map_err(HostRuntimeError::Core);
        // 即使 discovery 损坏或已被其他 owner 替换，也必须释放 OS lease 并
        // 完成本地生命周期收尾。
        discovery_result.and(lease_result).and(finish_result)?;
        Ok(action)
    }

    fn attach(
        &self,
        connection_id: ConnectionId,
        role: HostConnectionRole,
    ) -> Result<(), HostRuntimeError> {
        self.lifecycle
            .attach(connection_id, role)
            .map_err(HostRuntimeError::Core)
    }

    fn new(
        data_root: PathBuf,
        mode: HostRuntimeMode,
        owner_kind: HostOwnerKind,
        host_id: String,
        lease: Option<HostLease>,
        discovery: Option<HostDiscoveryRecord>,
    ) -> Result<Self, HostRuntimeError> {
        let data_root_fingerprint =
            data_root_fingerprint(&data_root).map_err(HostRuntimeError::Fingerprint)?;
        Ok(Self {
            data_root,
            data_root_fingerprint,
            mode,
            owner_kind,
            host_id,
            lease: Mutex::new(lease),
            discovery: Mutex::new(discovery),
            lifecycle: HostLifecycleController::new(owner_kind),
            prompt_queue: Arc::new(
                HostPromptQueue::new(HostCoreConfig::default()).map_err(HostRuntimeError::Core)?,
            ),
        })
    }

    fn read_discovery(data_root: &Path) -> Result<HostDiscoveryRecord, HostRuntimeError> {
        let path = discovery_path(data_root);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                HostRuntimeError::DiscoveryUnavailable
            } else {
                HostRuntimeError::Io(error)
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(HostRuntimeError::InvalidDiscovery("fileType"));
        }
        if metadata.len() > MAX_DISCOVERY_BYTES {
            return Err(HostRuntimeError::InvalidDiscovery("fileSize"));
        }
        let bytes = fs::read(path).map_err(HostRuntimeError::Io)?;
        serde_json::from_slice(&bytes).map_err(|_| HostRuntimeError::InvalidDiscovery("json"))
    }

    fn lock_discovery(
        &self,
    ) -> Result<MutexGuard<'_, Option<HostDiscoveryRecord>>, HostRuntimeError> {
        self.discovery
            .lock()
            .map_err(|_| HostRuntimeError::StateUnavailable)
    }

    fn require_owned(&self) -> Result<(), HostRuntimeError> {
        if self.mode == HostRuntimeMode::Owned {
            Ok(())
        } else {
            Err(HostRuntimeError::NotOwner)
        }
    }

    fn remove_owned_discovery(&self) -> Result<(), HostRuntimeError> {
        self.require_owned()?;
        let path = discovery_path(&self.data_root);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Ok(());
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(HostRuntimeError::InvalidDiscovery("fileType"));
        }
        let current = match Self::read_discovery(&self.data_root) {
            Ok(current) => current,
            Err(HostRuntimeError::DiscoveryUnavailable)
            | Err(HostRuntimeError::InvalidDiscovery(_))
            | Err(HostRuntimeError::RootMismatch) => return Ok(()),
            Err(error) => return Err(error),
        };
        if current.host_id != self.host_id
            || current.data_root_fingerprint != self.data_root_fingerprint
        {
            // 新 owner 已经发布了自己的记录，旧 owner 不能把它删除。
            return Ok(());
        }
        fs::remove_file(path).map_err(HostRuntimeError::Io)
    }
}

impl Drop for HostRuntime {
    /// 未经过显式 shutdown 时只清理仍属于当前实例的 discovery；HostLease 的 Drop
    /// 负责释放尚未显式消费的 OS 锁。
    fn drop(&mut self) {
        if self.mode != HostRuntimeMode::Owned {
            return;
        }
        let path = discovery_path(&self.data_root);
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.is_file()
            && !metadata.file_type().is_symlink()
            && let Ok(bytes) = fs::read(&path)
            && let Ok(record) = serde_json::from_slice::<HostDiscoveryRecord>(&bytes)
            && record.host_id == self.host_id
            && record.data_root_fingerprint == self.data_root_fingerprint
        {
            let _ = fs::remove_file(path);
        }
    }
}

/// Host 生命周期与 discovery 操作的错误分类。
#[derive(Debug)]
pub enum HostRuntimeError {
    /// 资源层 root/lease 错误。
    Resource(keencode_resources::ResourceError),
    /// 本地文件系统错误。
    Io(io::Error),
    /// 数据根指纹计算失败。
    Fingerprint(io::Error),
    /// discovery 记录字段不符合当前版本。
    InvalidDiscovery(&'static str),
    /// discovery 与当前数据根不匹配。
    RootMismatch,
    /// Host lease 竞争时没有可用的 discovery 记录。
    DiscoveryUnavailable,
    /// ACP 协议元数据校验失败。
    Protocol(keencode_acp::AcpBoundaryError),
    /// Host Core 生命周期/队列错误。
    Core(HostCoreError),
    /// 当前进程只是 Client，不能执行 owner-only 操作。
    NotOwner,
    /// 内部状态锁被 poison。
    StateUnavailable,
}

impl std::fmt::Display for HostRuntimeError {
    /// 输出不包含数据根路径、端点或认证材料的稳定错误摘要。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource(error) => write!(formatter, "Host resource error: {error}"),
            Self::Io(error) => write!(formatter, "Host IO error: {error}"),
            Self::Fingerprint(error) => {
                write!(formatter, "Host data-root fingerprint failed: {error}")
            }
            Self::InvalidDiscovery(field) => write!(formatter, "Host discovery invalid: {field}"),
            Self::RootMismatch => formatter.write_str("Host discovery root mismatch"),
            Self::DiscoveryUnavailable => formatter.write_str("Host discovery unavailable"),
            Self::Protocol(error) => write!(formatter, "Host protocol metadata invalid: {error}"),
            Self::Core(error) => write!(formatter, "Host Core error: {error}"),
            Self::NotOwner => formatter.write_str("当前进程不是 Host owner"),
            Self::StateUnavailable => formatter.write_str("Host state unavailable"),
        }
    }
}

impl std::error::Error for HostRuntimeError {}

impl From<io::Error> for HostRuntimeError {
    /// 将普通文件系统错误归入 Host IO 边界。
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// 返回 discovery 文件路径。
pub fn discovery_path(data_root: &Path) -> PathBuf {
    data_root.join(HOST_DISCOVERY_FILE_NAME)
}

/// 原子发布 discovery；临时文件与目标同目录，避免跨文件系统 rename。
pub fn publish_discovery(
    data_root: &Path,
    record: &HostDiscoveryRecord,
) -> Result<(), HostRuntimeError> {
    record
        .validate_for_root(data_root)
        .map_err(map_discovery_validation)?;
    let path = discovery_path(data_root);
    // 匹配分支本身在所有平台执行符号类型校验；布尔结果只有 Windows 的 persist
    // 前删除旧目标会读取，其他平台不绑定使用。
    #[cfg_attr(not(windows), allow(unused_variables))]
    let destination_exists = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(HostRuntimeError::InvalidDiscovery("fileType"));
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(HostRuntimeError::Io(error)),
    };
    // 先完成序列化，再触碰文件系统；序列化失败时不产生任何临时文件残留。
    let bytes =
        serde_json::to_vec(record).map_err(|_| HostRuntimeError::InvalidDiscovery("json"))?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".keencode-host-discovery-");
    #[cfg(unix)]
    {
        // 创建即 0600，不经过先建后改的中间权限状态。
        builder.mode(0o600);
    }
    let mut temporary = builder
        .tempfile_in(data_root)
        .map_err(HostRuntimeError::Io)?;
    temporary.write_all(&bytes).map_err(HostRuntimeError::Io)?;
    temporary.flush().map_err(HostRuntimeError::Io)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(HostRuntimeError::Io)?;
    #[cfg(windows)]
    if destination_exists {
        fs::remove_file(&path).map_err(HostRuntimeError::Io)?;
    }
    temporary
        .persist(&path)
        .map_err(|error| HostRuntimeError::Io(error.error))?;
    #[cfg(unix)]
    if let Ok(directory) = File::open(data_root) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn remove_discovery_if_owned(
    data_root: &Path,
    expected: &HostDiscoveryRecord,
) -> Result<(), HostRuntimeError> {
    let path = discovery_path(data_root);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(());
    }
    let Ok(current) = HostRuntime::read_discovery(data_root) else {
        // 无法证明文件归属当前 Host 时，不删除它。
        return Ok(());
    };
    if current.host_id == expected.host_id
        && current.data_root_fingerprint == expected.data_root_fingerprint
    {
        fs::remove_file(path).map_err(HostRuntimeError::Io)?;
    }
    Ok(())
}

fn map_discovery_validation(error: HostDiscoveryValidationError) -> HostRuntimeError {
    match error {
        HostDiscoveryValidationError::InvalidField(field) => {
            HostRuntimeError::InvalidDiscovery(field)
        }
        HostDiscoveryValidationError::RootMismatch => HostRuntimeError::RootMismatch,
        HostDiscoveryValidationError::Fingerprint(error) => HostRuntimeError::Fingerprint(error),
    }
}

fn make_host_id(data_root: &Path, owner_kind: HostOwnerKind) -> String {
    let mut digest = Sha256::new();
    digest.update(data_root.to_string_lossy().as_bytes());
    digest.update([0]);
    digest.update(std::process::id().to_le_bytes());
    digest.update([0]);
    digest.update(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |value| value.as_nanos())
            .to_le_bytes(),
    );
    digest.update([0]);
    digest.update(owner_kind.as_wire().as_bytes());
    format!("host-{:x}", digest.finalize())
}

fn started_at() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_millis());
    format!("unix-ms-{millis}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionIdentity, OperationState, OperationTerminal, PromptAdmissionRequest};
    use keencode_acp::OperationId;

    fn temporary_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("测试数据根应创建")
    }

    fn endpoint() -> (HostTransportKind, &'static str) {
        if cfg!(windows) {
            (
                HostTransportKind::NamedPipe,
                r"\\.\pipe\keencode-runtime-test",
            )
        } else {
            (
                HostTransportKind::UnixSocket,
                "/tmp/keencode-runtime-test.sock",
            )
        }
    }

    #[test]
    fn owner_publish_meta_and_shutdown_releases_discovery() {
        let root = temporary_root();
        let acquired = HostRuntime::acquire(root.path(), HostOwnerKind::Headless).unwrap();
        assert_eq!(acquired.mode(), HostRuntimeMode::Owned);
        let runtime = acquired.runtime();
        let (transport, endpoint) = endpoint();
        let record = runtime
            .mark_ready_and_publish(transport, endpoint)
            .expect("owner 应发布 discovery");
        assert_eq!(runtime.phase().unwrap(), HostLifecyclePhase::Ready);
        assert_eq!(record.owner_kind, HostOwnerKind::Headless);
        assert_eq!(
            runtime.initialize_meta().unwrap()["keencode/host/ownerKind"],
            "headless"
        );
        runtime.explicit_shutdown().expect("owner shutdown 应完成");
        assert_eq!(runtime.phase().unwrap(), HostLifecyclePhase::Stopped);
        assert!(!discovery_path(root.path()).exists());
    }

    #[test]
    fn busy_root_only_returns_validated_client() {
        let root = temporary_root();
        let owned = HostRuntime::acquire(root.path(), HostOwnerKind::Headless).unwrap();
        let (transport, endpoint) = endpoint();
        let owner = owned.runtime();
        owner.mark_ready_and_publish(transport, endpoint).unwrap();
        let client = HostRuntime::acquire(root.path(), HostOwnerKind::Desktop).unwrap();
        assert_eq!(client.mode(), HostRuntimeMode::Client);
        assert_eq!(client.runtime().host_id(), owner.host_id());
        assert_eq!(client.runtime().phase().unwrap(), HostLifecyclePhase::Ready);
        assert_eq!(client.runtime().owner_kind(), HostOwnerKind::Headless);
        assert_eq!(
            owner.explicit_shutdown().unwrap(),
            HostLifecycleAction::Shutdown
        );
    }

    #[test]
    fn stale_or_tampered_discovery_never_allows_client_mode() {
        let root = temporary_root();
        let _lease = match HostLease::try_acquire(root.path(), HostOwner::Desktop).unwrap() {
            HostLeaseAcquire::Acquired(lease) => lease,
            HostLeaseAcquire::Busy { .. } => panic!("首次应取得 lease"),
        };
        let (transport, _) = endpoint();
        let record = HostDiscoveryRecord::new(
            transport,
            "endpoint",
            HostOwnerKind::Desktop,
            std::process::id(),
            root.path(),
            "2026-09-21T12:00:00Z",
        )
        .unwrap();
        publish_discovery(root.path(), &record).unwrap();
        let mut tampered = record;
        tampered.data_root_fingerprint = "f".repeat(64);
        fs::write(
            discovery_path(root.path()),
            serde_json::to_vec(&tampered).unwrap(),
        )
        .unwrap();
        // 保持真实 OS lease 占用，使下一次 acquire 必须验证 Busy + discovery，
        // 而不是直接成为新的 owner。
        assert!(matches!(
            HostRuntime::acquire(root.path(), HostOwnerKind::Desktop),
            Err(HostRuntimeError::RootMismatch | HostRuntimeError::InvalidDiscovery(_))
        ));
    }

    #[test]
    fn publishing_discovery_replaces_existing_regular_file() {
        let root = temporary_root();
        let (transport, _) = endpoint();
        let record = HostDiscoveryRecord::new(
            transport,
            "endpoint",
            HostOwnerKind::Desktop,
            std::process::id(),
            root.path(),
            "2026-09-21T12:00:00Z",
        )
        .unwrap();
        publish_discovery(root.path(), &record).unwrap();
        publish_discovery(root.path(), &record).unwrap();
        let decoded = HostRuntime::read_discovery(root.path()).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn busy_without_discovery_returns_explicit_unavailable_error() {
        let root = temporary_root();
        let _lease = match HostLease::try_acquire(root.path(), HostOwner::Headless).unwrap() {
            HostLeaseAcquire::Acquired(lease) => lease,
            HostLeaseAcquire::Busy { .. } => panic!("首次应取得 lease"),
        };
        assert!(matches!(
            HostRuntime::acquire(root.path(), HostOwnerKind::Desktop),
            Err(HostRuntimeError::DiscoveryUnavailable)
        ));
    }

    #[test]
    fn explicit_shutdown_releases_lease_when_discovery_is_malformed() {
        let root = temporary_root();
        let acquired = HostRuntime::acquire(root.path(), HostOwnerKind::Desktop).unwrap();
        let runtime = acquired.runtime();
        let (transport, _) = endpoint();
        runtime
            .mark_ready_and_publish(transport, "endpoint")
            .unwrap();
        fs::write(discovery_path(root.path()), b"malformed").unwrap();
        assert_eq!(
            runtime.explicit_shutdown().unwrap(),
            HostLifecycleAction::Shutdown
        );
        assert_eq!(runtime.phase().unwrap(), HostLifecyclePhase::Stopped);
        let next = HostRuntime::acquire(root.path(), HostOwnerKind::Headless).unwrap();
        assert_eq!(next.mode(), HostRuntimeMode::Owned);
    }

    #[test]
    fn detached_operation_survives_transport_disconnect_on_shared_runtime() {
        let root = temporary_root();
        let acquired = HostRuntime::acquire(root.path(), HostOwnerKind::Desktop).unwrap();
        let runtime = acquired.runtime();
        let (transport, endpoint) = endpoint();
        runtime
            .mark_ready_and_publish(transport, endpoint)
            .expect("Desktop owner 应进入 Ready");
        let connection_id = ConnectionId::new("detached-cli").unwrap();
        runtime.attach_client(connection_id.clone()).unwrap();
        let operation_id = OperationId::new("detached-operation").unwrap();
        runtime
            .prompt_queue()
            .admit(PromptAdmissionRequest {
                connection_id: connection_id.clone(),
                session_id: "session-detached".to_owned(),
                operation_id: operation_id.clone(),
                prompt: "continue after disconnect".to_owned(),
                payload_digest: "a".repeat(64),
                detached: true,
            })
            .unwrap();
        runtime
            .prompt_queue()
            .claim_operation("session-detached", &operation_id)
            .unwrap()
            .expect("队首 operation 应可 claim");
        runtime
            .prompt_queue()
            .bind_execution(
                &operation_id,
                ExecutionIdentity::new("session-detached", "turn-detached", "task-detached")
                    .unwrap(),
            )
            .unwrap();

        assert_eq!(
            runtime.detach(&connection_id).unwrap(),
            HostDisconnectAction::DetachClient
        );
        assert_eq!(
            runtime.prompt_queue().status(&operation_id).unwrap().state,
            OperationState::Running
        );
        runtime
            .prompt_queue()
            .finish(
                &operation_id,
                OperationState::Completed,
                OperationTerminal::new("completed", None::<String>).unwrap(),
            )
            .unwrap();
        assert_eq!(
            runtime.prompt_queue().status(&operation_id).unwrap().state,
            OperationState::Completed
        );
        runtime.explicit_shutdown().unwrap();
    }
}
