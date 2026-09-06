//! 由系统能力层独占管理的真实 Git Worktree lease 生命周期。

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use keencode_agent::{SessionId, WorktreeLease};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 当前 lease 记录格式版本。
const LEASE_RECORD_VERSION: u32 = 1;
/// 单个 lease 记录允许占用的最大字节数。
const MAX_LEASE_RECORD_BYTES: u64 = 64 * 1024;
/// 一个管理根下允许扫描的最大 lease 记录数量。
const MAX_LEASE_RECORDS: usize = 10_000;
/// Git 错误输出允许进入错误信息的最大 UTF-8 字节数。
const MAX_GIT_ERROR_BYTES: usize = 32 * 1024;
/// Git commit-ish 输入允许的最大 UTF-8 字节数。
const MAX_START_POINT_BYTES: usize = 1_024;
/// 原子记录临时文件使用的固定前缀。
const RECORD_TEMP_PREFIX: &str = ".keencode-worktree-record-";
/// 管理根的跨进程独占锁文件名。
const MANAGER_LOCK_FILE: &str = "manager.lock";
/// Windows 子进程不创建控制台窗口的进程标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 创建一个受管 Git Worktree 所需的不可变输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitWorktreeCreateRequest {
    /// 拥有该 Worktree 的 Session。
    pub session_id: SessionId,
    /// 必须精确指向 Git 工作树顶层的现有目录。
    pub repository_root: PathBuf,
    /// 可选 Git commit-ish；为空时冻结创建瞬间的 `HEAD`。
    pub start_point: Option<String>,
}

/// 已创建且由系统 lease 独占管理的 Git Worktree。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedGitWorktree {
    /// 交给协作领域层保存的不透明唯一 lease。
    pub lease: WorktreeLease,
    /// 拥有该 lease 的 Session。
    pub session_id: SessionId,
    /// 创建 Worktree 的规范 Git 工作树顶层。
    pub repository_root: PathBuf,
    /// 只能位于管理器 `trees` 根下的规范绝对路径。
    pub path: PathBuf,
    /// 创建前解析并冻结的完整 commit 标识。
    pub commit: String,
}

/// 单个 lease 的幂等释放结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitWorktreeReleaseOutcome {
    /// 本次调用真实移除了 Worktree 并提交释放墓碑。
    Released,
    /// 该 lease 先前已经完整释放。
    AlreadyReleased,
}

/// 一次批量清理已完成的精确结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GitWorktreeCleanupReport {
    /// 本次真实完成释放的 lease，按标识稳定排序。
    pub released: Vec<WorktreeLease>,
    /// 扫描时已经处于释放终态的 lease，按标识稳定排序。
    pub already_released: Vec<WorktreeLease>,
}

/// 批量清理中一个未能完成的 lease 及脱敏错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitWorktreeCleanupFailure {
    /// 未完成清理的受管 lease。
    pub lease: WorktreeLease,
    /// 不包含命令参数或用户文件正文的失败说明。
    pub message: String,
}

/// Git Worktree 创建、持久登记或清理失败。
#[derive(Debug)]
pub enum GitWorktreeLeaseError {
    /// 管理根不是安全、可访问的绝对目录。
    InvalidManagedRoot {
        /// 脱敏失败说明。
        message: String,
    },
    /// 请求仓库不存在、不是顶层或与管理根重叠。
    InvalidRepository {
        /// 脱敏失败说明。
        message: String,
    },
    /// Git 起点为空、包含危险控制字符或选项注入前缀。
    InvalidStartPoint,
    /// 另一个进程已经独占同一 Worktree 管理根。
    ManagedRootBusy,
    /// 生成的 lease 与已经持久登记的标识发生碰撞。
    LeaseCollision,
    /// 调用方提供的 lease 从未由该管理根签发。
    UnknownLease,
    /// 持久 lease 记录损坏或不满足当前唯一格式。
    CorruptLeaseRecord {
        /// 不包含记录正文的失败说明。
        message: String,
    },
    /// 记录中的路径未通过管理根边界校验。
    UnsafeManagedPath {
        /// 不回显不可信路径正文的失败说明。
        message: String,
    },
    /// Git 可执行文件不可用或命令返回失败。
    GitCommandFailed {
        /// 固定操作名称。
        operation: &'static str,
        /// 有界 Git 错误摘要。
        message: String,
    },
    /// lease 记录的创建、读取或原子替换失败。
    PersistenceFailed {
        /// 固定持久化操作名称。
        operation: &'static str,
        /// 不包含记录正文的 I/O 错误。
        message: String,
    },
    /// 批量清理继续处理其余 lease 后仍有显式失败。
    CleanupFailed {
        /// 已经成功完成的部分结果。
        report: GitWorktreeCleanupReport,
        /// 未完成清理的全部 lease。
        failures: Vec<GitWorktreeCleanupFailure>,
    },
}

impl fmt::Display for GitWorktreeLeaseError {
    /// 输出有界且不包含用户文件正文的稳定错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManagedRoot { message } => {
                write!(formatter, "Worktree 管理根无效：{message}")
            }
            Self::InvalidRepository { message } => write!(formatter, "Git 仓库无效：{message}"),
            Self::InvalidStartPoint => formatter.write_str("Git Worktree 起点无效"),
            Self::ManagedRootBusy => formatter.write_str("Worktree 管理根已被其他进程占用"),
            Self::LeaseCollision => formatter.write_str("Worktree lease 标识发生碰撞"),
            Self::UnknownLease => formatter.write_str("Worktree lease 不存在"),
            Self::CorruptLeaseRecord { message } => {
                write!(formatter, "Worktree lease 记录损坏：{message}")
            }
            Self::UnsafeManagedPath { message } => {
                write!(formatter, "Worktree 受管路径无效：{message}")
            }
            Self::GitCommandFailed { operation, message } => {
                write!(formatter, "Git Worktree {operation} 失败：{message}")
            }
            Self::PersistenceFailed { operation, message } => {
                write!(formatter, "Worktree lease {operation} 失败：{message}")
            }
            Self::CleanupFailed { failures, .. } => {
                write!(formatter, "{} 个 Worktree lease 未完成清理", failures.len())
            }
        }
    }
}

impl Error for GitWorktreeLeaseError {}

/// lease 从预留到不可复用墓碑的持久状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LeaseState {
    /// 已先行持久预留路径，但 Git add 尚未确认完成。
    Provisioning,
    /// Git 已确认 Worktree 存在且记录完整。
    Active,
    /// 清理意图已经持久化，进程中断后必须继续清理。
    Releasing,
    /// Worktree 已不存在且 Git 元数据已清理，lease 永不复用。
    Released,
}

/// 持久记录中的仓库与受管 Worktree 当前可验证的关系。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepositoryRegistration {
    /// 仓库可用且其 Git 元数据明确登记了该 Worktree。
    Managed,
    /// 仓库可用，但其 Git 元数据没有登记该 Worktree。
    NotManaged,
    /// 原仓库目录已经不存在，无法再清理其 Git 元数据。
    Unavailable,
}

/// 磁盘中唯一受支持的 lease 记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LeaseRecord {
    /// 固定记录版本。
    version: u32,
    /// 与文件名严格一致的不透明 lease 标识。
    lease_id: String,
    /// 拥有该 Worktree 的 Session 标识。
    session_id: String,
    /// 创建命令使用的规范 Git 工作树顶层。
    repository_root: PathBuf,
    /// 由管理器派生且位于 `trees` 根下的唯一目标。
    worktree_path: PathBuf,
    /// 创建前解析并冻结的完整 commit 标识。
    commit: String,
    /// 当前持久状态。
    state: LeaseState,
    /// 创建时的 Unix 毫秒，仅供审计与诊断排序。
    created_at_ms: u64,
}

/// 在单一管理根内签发、持久登记并清理真实 Git Worktree 的系统组件。
pub struct GitWorktreeLeaseManager {
    /// 已规范化的管理根。
    managed_root: PathBuf,
    /// 全部真实 Worktree 的唯一父目录。
    trees_root: PathBuf,
    /// 与 Worktree 分离保存的 lease 记录目录。
    records_root: PathBuf,
    /// 禁用仓库 Hook 的受控空目录。
    hooks_root: PathBuf,
    /// 串行化同一管理器内的创建、恢复和清理事务。
    operation_lock: Mutex<()>,
    /// 跨进程独占管理根并在 Drop 时由操作系统释放的锁文件。
    instance_lock: File,
    /// 与进程和时间共同参与 lease 派生的单调计数器。
    next_lease: AtomicU64,
}

impl GitWorktreeLeaseManager {
    /// 打开唯一管理根、建立安全子目录并取得跨进程独占所有权。
    pub fn open(managed_root: impl AsRef<Path>) -> Result<Self, GitWorktreeLeaseError> {
        let requested_root = managed_root.as_ref();
        if !is_plain_absolute_path(requested_root) {
            return Err(GitWorktreeLeaseError::InvalidManagedRoot {
                message: "必须是没有 `.` 或 `..` 的绝对路径".to_owned(),
            });
        }
        create_real_directory_chain(requested_root, "create_managed_root")?;
        let managed_root = canonicalize_for_git(requested_root, "canonicalize_managed_root")?;
        ensure_real_directory(&managed_root, "inspect_managed_root")?;

        let trees_root = managed_root.join("trees");
        let records_root = managed_root.join("records");
        let hooks_root = managed_root.join("empty-hooks");
        for directory in [&trees_root, &records_root, &hooks_root] {
            create_real_directory_chain(directory, "create_managed_subdirectory")?;
            ensure_real_directory(directory, "inspect_managed_subdirectory")?;
        }
        let trees_root = canonicalize_for_git(&trees_root, "canonicalize_trees_root")?;
        let records_root = canonicalize_for_git(&records_root, "canonicalize_records_root")?;
        let hooks_root = canonicalize_for_git(&hooks_root, "canonicalize_hooks_root")?;

        let lock_path = managed_root.join(MANAGER_LOCK_FILE);
        ensure_regular_file_or_absent(&lock_path, "inspect_manager_lock")?;
        let instance_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| persistence_error("open_manager_lock", error))?;
        FileExt::try_lock_exclusive(&instance_lock).map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied
            ) || matches!(error.raw_os_error(), Some(32 | 33))
            {
                GitWorktreeLeaseError::ManagedRootBusy
            } else {
                persistence_error("lock_manager_root", error)
            }
        })?;

        Ok(Self {
            managed_root,
            trees_root,
            records_root,
            hooks_root,
            operation_lock: Mutex::new(()),
            instance_lock,
            next_lease: AtomicU64::new(1),
        })
    }

    /// 返回已经规范化且由本实例独占的管理根。
    pub fn managed_root(&self) -> &Path {
        &self.managed_root
    }

    /// 创建真实 detached Git Worktree，并在调用 Git 前后同步提交 lease 状态。
    pub fn create(
        &self,
        request: GitWorktreeCreateRequest,
    ) -> Result<ManagedGitWorktree, GitWorktreeLeaseError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| persistence_message("lock_operations", "进程内操作锁已损坏"))?;
        let repository_root = self.validate_repository_root(&request.repository_root)?;
        let start_point = validate_start_point(request.start_point.as_deref())?;
        let commit = self.resolve_commit(&repository_root, start_point)?;
        let created_at_ms = unix_millis()?;
        let (lease, record_path, worktree_path) = self.allocate_lease(
            &request.session_id,
            &repository_root,
            &commit,
            created_at_ms,
        )?;
        let mut record = LeaseRecord {
            version: LEASE_RECORD_VERSION,
            lease_id: lease.as_str().to_owned(),
            session_id: request.session_id.as_str().to_owned(),
            repository_root: repository_root.clone(),
            worktree_path: worktree_path.clone(),
            commit: commit.clone(),
            state: LeaseState::Provisioning,
            created_at_ms,
        };
        self.write_record(&record_path, &record)?;

        let add_result = self.run_git(
            "add",
            &repository_root,
            [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("--detach"),
                worktree_path.as_os_str().to_owned(),
                OsString::from(&commit),
            ],
            &[&repository_root, &worktree_path],
        );
        if let Err(error) = add_result {
            record.state = LeaseState::Releasing;
            let _ = self.write_record(&record_path, &record);
            let _ = self.release_record(&record_path, record);
            return Err(error);
        }
        self.ensure_managed_worktree_path(&record)?;
        record.state = LeaseState::Active;
        self.write_record(&record_path, &record)?;
        managed_worktree_from_record(record, lease)
    }

    /// 读取一个已登记 lease；释放墓碑仍返回记录以支持审计和幂等确认。
    pub fn inspect(
        &self,
        lease: &WorktreeLease,
    ) -> Result<ManagedGitWorktree, GitWorktreeLeaseError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| persistence_message("lock_operations", "进程内操作锁已损坏"))?;
        let (_, record) = self.read_record(lease)?;
        managed_worktree_from_record(record, lease.clone())
    }

    /// 按不透明 lease 幂等移除 Worktree；绝不接受调用方提供的目录路径。
    pub fn release(
        &self,
        lease: &WorktreeLease,
    ) -> Result<GitWorktreeReleaseOutcome, GitWorktreeLeaseError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| persistence_message("lock_operations", "进程内操作锁已损坏"))?;
        let (record_path, record) = self.read_record(lease)?;
        self.release_record(&record_path, record)
    }

    /// 逐一释放指定 lease，继续处理其余项并返回完整部分成功信息。
    pub fn release_many(
        &self,
        leases: &[WorktreeLease],
    ) -> Result<GitWorktreeCleanupReport, GitWorktreeLeaseError> {
        let mut ordered = leases.to_vec();
        ordered.sort();
        ordered.dedup();
        let mut report = GitWorktreeCleanupReport::default();
        let mut failures = Vec::new();
        for lease in ordered {
            match self.release(&lease) {
                Ok(GitWorktreeReleaseOutcome::Released) => report.released.push(lease),
                Ok(GitWorktreeReleaseOutcome::AlreadyReleased) => {
                    report.already_released.push(lease)
                }
                Err(error) => failures.push(GitWorktreeCleanupFailure {
                    lease,
                    message: truncate_utf8(&error.to_string(), MAX_GIT_ERROR_BYTES),
                }),
            }
        }
        if failures.is_empty() {
            Ok(report)
        } else {
            Err(GitWorktreeLeaseError::CleanupFailed { report, failures })
        }
    }

    /// 冷启动时扫描所有非终态记录并幂等清理上次进程留下的 Worktree。
    pub fn recover_stale(&self) -> Result<GitWorktreeCleanupReport, GitWorktreeLeaseError> {
        let leases = self.scan_record_leases()?;
        self.release_many(&leases)
    }

    /// 校验仓库确实是传入的 Git 工作树顶层，且与受管根不存在包含关系。
    fn validate_repository_root(&self, requested: &Path) -> Result<PathBuf, GitWorktreeLeaseError> {
        if !is_plain_absolute_path(requested) {
            return Err(GitWorktreeLeaseError::InvalidRepository {
                message: "仓库必须是没有 `.` 或 `..` 的绝对路径".to_owned(),
            });
        }
        let repository_root =
            canonicalize_for_git(requested, "canonicalize_repository").map_err(|error| {
                GitWorktreeLeaseError::InvalidRepository {
                    message: error.to_string(),
                }
            })?;
        ensure_real_directory(&repository_root, "inspect_repository").map_err(|error| {
            GitWorktreeLeaseError::InvalidRepository {
                message: error.to_string(),
            }
        })?;
        if repository_root.starts_with(&self.managed_root)
            || self.managed_root.starts_with(&repository_root)
        {
            return Err(GitWorktreeLeaseError::InvalidRepository {
                message: "仓库与 Worktree 管理根不能互相包含".to_owned(),
            });
        }
        let output = self.run_git(
            "show_toplevel",
            &repository_root,
            [
                OsString::from("rev-parse"),
                OsString::from("--show-toplevel"),
            ],
            &[&repository_root],
        )?;
        let reported = output_path(&output.stdout, "show_toplevel")?;
        let reported =
            canonicalize_for_git(&reported, "canonicalize_git_toplevel").map_err(|error| {
                GitWorktreeLeaseError::InvalidRepository {
                    message: error.to_string(),
                }
            })?;
        if reported != repository_root {
            return Err(GitWorktreeLeaseError::InvalidRepository {
                message: "传入目录不是 Git 工作树顶层".to_owned(),
            });
        }
        Ok(repository_root)
    }

    /// 把已校验 commit-ish 冻结为完整十六进制 commit 标识。
    fn resolve_commit(
        &self,
        repository_root: &Path,
        start_point: &str,
    ) -> Result<String, GitWorktreeLeaseError> {
        let revision = format!("{start_point}^{{commit}}");
        let output = self.run_git(
            "resolve_commit",
            repository_root,
            [
                OsString::from("rev-parse"),
                OsString::from("--verify"),
                OsString::from(revision),
            ],
            &[repository_root],
        )?;
        let commit = std::str::from_utf8(&output.stdout)
            .map_err(|_| GitWorktreeLeaseError::GitCommandFailed {
                operation: "resolve_commit",
                message: "Git 返回了非 UTF-8 commit 标识".to_owned(),
            })?
            .trim();
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(GitWorktreeLeaseError::GitCommandFailed {
                operation: "resolve_commit",
                message: "Git 返回了无效的完整 commit 标识".to_owned(),
            });
        }
        Ok(commit.to_ascii_lowercase())
    }

    /// 派生不可预测碰撞且永不复用的 lease，并预检记录与目录均不存在。
    fn allocate_lease(
        &self,
        session_id: &SessionId,
        repository_root: &Path,
        commit: &str,
        created_at_ms: u64,
    ) -> Result<(WorktreeLease, PathBuf, PathBuf), GitWorktreeLeaseError> {
        for _ in 0..32 {
            let sequence = self.next_lease.fetch_add(1, Ordering::Relaxed);
            let mut hasher = Sha256::new();
            hasher.update(session_id.as_str().as_bytes());
            hasher.update([0]);
            hasher.update(repository_root.as_os_str().as_encoded_bytes());
            hasher.update([0]);
            hasher.update(commit.as_bytes());
            hasher.update(created_at_ms.to_le_bytes());
            hasher.update(sequence.to_le_bytes());
            hasher.update(std::process::id().to_le_bytes());
            let digest = format!("{:x}", hasher.finalize());
            let lease_id = format!(
                "wt-{created_at_ms:016x}-{:08x}-{sequence:016x}-{}",
                std::process::id(),
                &digest[..16]
            );
            let lease =
                WorktreeLease::new(lease_id).map_err(|_| GitWorktreeLeaseError::LeaseCollision)?;
            let record_path = self.record_path(&lease);
            let worktree_path = self.trees_root.join(lease.as_str());
            if !record_path.exists() && !worktree_path.exists() {
                return Ok((lease, record_path, worktree_path));
            }
        }
        Err(GitWorktreeLeaseError::LeaseCollision)
    }

    /// 从固定文件名读取、限长解析并完整校验一个 lease 记录。
    fn read_record(
        &self,
        lease: &WorktreeLease,
    ) -> Result<(PathBuf, LeaseRecord), GitWorktreeLeaseError> {
        let path = self.record_path(lease);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                GitWorktreeLeaseError::UnknownLease
            } else {
                persistence_error("inspect_record", error)
            }
        })?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "记录路径不是普通文件".to_owned(),
            });
        }
        if metadata.len() > MAX_LEASE_RECORD_BYTES {
            return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "记录超过字节上限".to_owned(),
            });
        }
        let file = File::open(&path).map_err(|error| persistence_error("open_record", error))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_LEASE_RECORD_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| persistence_error("read_record", error))?;
        if bytes.len() as u64 > MAX_LEASE_RECORD_BYTES {
            return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "读取时记录超过字节上限".to_owned(),
            });
        }
        let record: LeaseRecord = serde_json::from_slice(&bytes).map_err(|_| {
            GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "记录不是当前严格 JSON 格式".to_owned(),
            }
        })?;
        self.validate_record(lease, &record)?;
        Ok((path, record))
    }

    /// 验证记录身份、状态字段、仓库和唯一受管目标路径。
    fn validate_record(
        &self,
        lease: &WorktreeLease,
        record: &LeaseRecord,
    ) -> Result<(), GitWorktreeLeaseError> {
        if record.version != LEASE_RECORD_VERSION || record.lease_id != lease.as_str() {
            return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "记录版本或 lease 身份不匹配".to_owned(),
            });
        }
        SessionId::new(record.session_id.clone()).map_err(|_| {
            GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "Session 身份无效".to_owned(),
            }
        })?;
        if !is_plain_absolute_path(&record.repository_root)
            || !is_plain_absolute_path(&record.worktree_path)
        {
            return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "记录包含非规范绝对路径".to_owned(),
            });
        }
        let expected = self.trees_root.join(lease.as_str());
        if record.worktree_path != expected {
            return Err(GitWorktreeLeaseError::UnsafeManagedPath {
                message: "记录目标不是由 lease 唯一派生的受管路径".to_owned(),
            });
        }
        if !matches!(record.commit.len(), 40 | 64)
            || !record.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "记录 commit 标识无效".to_owned(),
            });
        }
        Ok(())
    }

    /// 原子同步唯一当前格式的 lease 记录。
    fn write_record(&self, path: &Path, record: &LeaseRecord) -> Result<(), GitWorktreeLeaseError> {
        let lease = WorktreeLease::new(record.lease_id.clone()).map_err(|_| {
            GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "写入前 lease 身份无效".to_owned(),
            }
        })?;
        self.validate_record(&lease, record)?;
        if path != self.record_path(&lease) {
            return Err(GitWorktreeLeaseError::UnsafeManagedPath {
                message: "记录文件不是由 lease 唯一派生".to_owned(),
            });
        }
        ensure_regular_file_or_absent(path, "inspect_record_destination")?;
        let bytes =
            serde_json::to_vec(record).map_err(|_| GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "记录无法编码为 JSON".to_owned(),
            })?;
        if bytes.len() as u64 > MAX_LEASE_RECORD_BYTES {
            return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "编码后记录超过字节上限".to_owned(),
            });
        }
        let mut temporary = tempfile::Builder::new()
            .prefix(RECORD_TEMP_PREFIX)
            .tempfile_in(&self.records_root)
            .map_err(|error| persistence_error("create_record_temporary", error))?;
        temporary
            .write_all(&bytes)
            .map_err(|error| persistence_error("write_record_temporary", error))?;
        temporary
            .as_file_mut()
            .flush()
            .map_err(|error| persistence_error("flush_record_temporary", error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| persistence_error("sync_record_temporary", error))?;
        temporary
            .persist(path)
            .map_err(|error| persistence_error("persist_record", error.error))?;
        sync_directory(&self.records_root, "sync_records_root")
    }

    /// 把记录推进到 Releasing，安全调用 Git 删除并提交 Released 墓碑。
    fn release_record(
        &self,
        record_path: &Path,
        mut record: LeaseRecord,
    ) -> Result<GitWorktreeReleaseOutcome, GitWorktreeLeaseError> {
        let lease = WorktreeLease::new(record.lease_id.clone()).map_err(|_| {
            GitWorktreeLeaseError::CorruptLeaseRecord {
                message: "释放记录的 lease 身份无效".to_owned(),
            }
        })?;
        self.validate_record(&lease, &record)?;
        if record.state == LeaseState::Released {
            return Ok(GitWorktreeReleaseOutcome::AlreadyReleased);
        }
        record.state = LeaseState::Releasing;
        self.write_record(record_path, &record)?;
        self.ensure_safe_release_target(&record)?;

        let registration = self.repository_registration(&record)?;
        if record.worktree_path.exists() {
            if registration == RepositoryRegistration::Managed {
                self.run_git(
                    "remove",
                    &record.repository_root,
                    [
                        OsString::from("worktree"),
                        OsString::from("remove"),
                        OsString::from("--force"),
                        OsString::from("--force"),
                        record.worktree_path.as_os_str().to_owned(),
                    ],
                    &[&record.repository_root, &record.worktree_path],
                )?;
            } else {
                remove_managed_directory(&record.worktree_path)?;
            }
        }
        if record.worktree_path.exists() {
            return Err(GitWorktreeLeaseError::GitCommandFailed {
                operation: "remove",
                message: "Git 返回成功后受管 Worktree 仍然存在".to_owned(),
            });
        }
        if registration == RepositoryRegistration::Managed {
            self.run_git(
                "prune",
                &record.repository_root,
                [
                    OsString::from("worktree"),
                    OsString::from("prune"),
                    OsString::from("--expire"),
                    OsString::from("now"),
                ],
                &[&record.repository_root],
            )?;
        }
        record.state = LeaseState::Released;
        self.write_record(record_path, &record)?;
        Ok(GitWorktreeReleaseOutcome::Released)
    }

    /// 确认刚创建的目录仍是受管根下没有重解析跳转的真实 Worktree。
    fn ensure_managed_worktree_path(
        &self,
        record: &LeaseRecord,
    ) -> Result<(), GitWorktreeLeaseError> {
        self.ensure_safe_release_target(record)?;
        if !record.worktree_path.is_dir() {
            return Err(GitWorktreeLeaseError::GitCommandFailed {
                operation: "add",
                message: "Git 返回成功后 Worktree 目录不存在".to_owned(),
            });
        }
        Ok(())
    }

    /// 在任何删除前复核记录目标、父目录和现有节点都没有逃逸管理根。
    fn ensure_safe_release_target(
        &self,
        record: &LeaseRecord,
    ) -> Result<(), GitWorktreeLeaseError> {
        let lease = WorktreeLease::new(record.lease_id.clone()).map_err(|_| {
            GitWorktreeLeaseError::UnsafeManagedPath {
                message: "lease 身份无效".to_owned(),
            }
        })?;
        let expected = self.trees_root.join(lease.as_str());
        if record.worktree_path != expected || expected.parent() != Some(self.trees_root.as_path())
        {
            return Err(GitWorktreeLeaseError::UnsafeManagedPath {
                message: "目标不属于唯一受管子目录".to_owned(),
            });
        }
        ensure_real_directory(&self.trees_root, "inspect_trees_root")?;
        match fs::symlink_metadata(&expected) {
            Ok(metadata) if is_link_or_reparse(&metadata) => {
                Err(GitWorktreeLeaseError::UnsafeManagedPath {
                    message: "受管目标是符号链接或重解析点".to_owned(),
                })
            }
            Ok(metadata) if !metadata.is_dir() => Err(GitWorktreeLeaseError::UnsafeManagedPath {
                message: "受管目标存在但不是目录".to_owned(),
            }),
            Ok(_) => {
                let canonical = canonicalize_for_git(&expected, "canonicalize_worktree")?;
                if canonical.parent() != Some(self.trees_root.as_path()) {
                    return Err(GitWorktreeLeaseError::UnsafeManagedPath {
                        message: "规范化目标已经逃逸管理根".to_owned(),
                    });
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(persistence_error("inspect_worktree", error)),
        }
    }

    /// 返回记录中的仓库是否仍是可调用 Git 的真实目录。
    fn repository_is_available(&self, repository_root: &Path) -> bool {
        fs::symlink_metadata(repository_root)
            .map(|metadata| metadata.is_dir() && !is_link_or_reparse(&metadata))
            .unwrap_or(false)
    }

    /// 只在 Git 明确列出相同受管路径时授权 remove/prune 修改仓库元数据。
    fn repository_registration(
        &self,
        record: &LeaseRecord,
    ) -> Result<RepositoryRegistration, GitWorktreeLeaseError> {
        if !self.repository_is_available(&record.repository_root) {
            return Ok(RepositoryRegistration::Unavailable);
        }
        let output = self.run_git(
            "list",
            &record.repository_root,
            [
                OsString::from("worktree"),
                OsString::from("list"),
                OsString::from("--porcelain"),
                OsString::from("-z"),
            ],
            &[&record.repository_root, &record.worktree_path],
        )?;
        let expected = normalized_git_path_key(&record.worktree_path)?;
        let managed = output.stdout.split(|byte| *byte == 0).any(|field| {
            let Some(path) = field.strip_prefix(b"worktree ") else {
                return false;
            };
            std::str::from_utf8(path)
                .ok()
                .and_then(|path| normalized_git_path_key(Path::new(path)).ok())
                .is_some_and(|candidate| candidate == expected)
        });
        Ok(if managed {
            RepositoryRegistration::Managed
        } else {
            RepositoryRegistration::NotManaged
        })
    }

    /// 枚举受限数量的严格 JSON 记录并恢复其中的 lease 身份。
    fn scan_record_leases(&self) -> Result<Vec<WorktreeLease>, GitWorktreeLeaseError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| persistence_message("lock_operations", "进程内操作锁已损坏"))?;
        let mut leases = Vec::new();
        for entry in fs::read_dir(&self.records_root)
            .map_err(|error| persistence_error("scan_records", error))?
        {
            let entry = entry.map_err(|error| persistence_error("read_record_entry", error))?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if !file_name.ends_with(".json") {
                continue;
            }
            if leases.len() >= MAX_LEASE_RECORDS {
                return Err(GitWorktreeLeaseError::CorruptLeaseRecord {
                    message: "lease 记录数量超过扫描上限".to_owned(),
                });
            }
            let lease_id = &file_name[..file_name.len() - ".json".len()];
            let lease = WorktreeLease::new(lease_id.to_owned()).map_err(|_| {
                GitWorktreeLeaseError::CorruptLeaseRecord {
                    message: "记录文件名不是有效 lease".to_owned(),
                }
            })?;
            let (_, record) = self.read_record(&lease)?;
            if record.state != LeaseState::Released {
                leases.push(lease);
            }
        }
        leases.sort();
        Ok(leases)
    }

    /// 根据已验证 lease 生成固定记录路径。
    fn record_path(&self, lease: &WorktreeLease) -> PathBuf {
        self.records_root.join(format!("{}.json", lease.as_str()))
    }

    /// 以无 shell、无交互、禁用仓库 Hook 的参数数组执行一次 Git 命令。
    fn run_git<I>(
        &self,
        operation: &'static str,
        repository_root: &Path,
        args: I,
        redacted_paths: &[&Path],
    ) -> Result<Output, GitWorktreeLeaseError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut command = Command::new("git");
        command
            .arg("-c")
            .arg(config_argument("core.hooksPath", &self.hooks_root))
            .arg("-C")
            .arg(repository_root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "Never")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut command);
        let output = command
            .output()
            .map_err(|error| GitWorktreeLeaseError::GitCommandFailed {
                operation,
                message: truncate_utf8(&format!("无法启动 git：{error}"), MAX_GIT_ERROR_BYTES),
            })?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(GitWorktreeLeaseError::GitCommandFailed {
                operation,
                message: bounded_git_error(&output, redacted_paths),
            })
        }
    }
}

impl Drop for GitWorktreeLeaseManager {
    /// 提前释放跨进程锁；句柄关闭仍是最终兜底。
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.instance_lock);
    }
}

/// 把持久记录恢复为调用方可消费的只读 Worktree 描述。
fn managed_worktree_from_record(
    record: LeaseRecord,
    lease: WorktreeLease,
) -> Result<ManagedGitWorktree, GitWorktreeLeaseError> {
    let session_id = SessionId::new(record.session_id).map_err(|_| {
        GitWorktreeLeaseError::CorruptLeaseRecord {
            message: "Session 身份无效".to_owned(),
        }
    })?;
    Ok(ManagedGitWorktree {
        lease,
        session_id,
        repository_root: record.repository_root,
        path: record.worktree_path,
        commit: record.commit,
    })
}

/// 校验绝对路径只包含平台前缀、根和普通组件。
fn is_plain_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

/// 规范化路径，并移除 Windows Git 无法接受的 Win32 verbatim 前缀。
fn canonicalize_for_git(
    path: &Path,
    operation: &'static str,
) -> Result<PathBuf, GitWorktreeLeaseError> {
    let canonical = fs::canonicalize(path).map_err(|error| persistence_error(operation, error))?;
    #[cfg(windows)]
    {
        let rendered = canonical
            .to_str()
            .ok_or_else(|| persistence_message(operation, "Windows 路径不是可持久化的 UTF-8"))?;
        if let Some(remainder) = rendered.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{remainder}")));
        }
        if let Some(remainder) = rendered.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(remainder));
        }
    }
    Ok(canonical)
}

/// 校验可选 commit-ish，避免空值、控制字符和选项注入。
fn validate_start_point(start_point: Option<&str>) -> Result<&str, GitWorktreeLeaseError> {
    let start_point = start_point.unwrap_or("HEAD");
    if start_point.trim() != start_point
        || start_point.is_empty()
        || start_point.len() > MAX_START_POINT_BYTES
        || start_point.starts_with('-')
        || start_point.chars().any(char::is_control)
    {
        return Err(GitWorktreeLeaseError::InvalidStartPoint);
    }
    Ok(start_point)
}

/// 创建目录链，并拒绝最终目录是链接、重解析点或普通文件。
fn create_real_directory_chain(
    path: &Path,
    operation: &'static str,
) -> Result<(), GitWorktreeLeaseError> {
    fs::create_dir_all(path).map_err(|error| persistence_error(operation, error))?;
    ensure_real_directory(path, operation)
}

/// 验证现有路径是没有重解析跳转的真实目录。
fn ensure_real_directory(
    path: &Path,
    operation: &'static str,
) -> Result<(), GitWorktreeLeaseError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| persistence_error(operation, error))?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(GitWorktreeLeaseError::InvalidManagedRoot {
            message: "目录是链接、重解析点或非目录节点".to_owned(),
        });
    }
    Ok(())
}

/// 验证目标不存在或是没有重解析跳转的普通文件。
fn ensure_regular_file_or_absent(
    path: &Path,
    operation: &'static str,
) -> Result<(), GitWorktreeLeaseError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
            Err(persistence_message(operation, "目标不是普通文件"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(persistence_error(operation, error)),
    }
}

/// 返回元数据是否代表符号链接或 Windows 重解析点。
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

/// 安全删除已经通过唯一受管路径校验的真实目录。
fn remove_managed_directory(path: &Path) -> Result<(), GitWorktreeLeaseError> {
    fs::remove_dir_all(path).map_err(|error| persistence_error("remove_managed_directory", error))
}

/// 把 Git 标准输出中的唯一绝对路径解析为平台路径。
fn output_path(bytes: &[u8], operation: &'static str) -> Result<PathBuf, GitWorktreeLeaseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| GitWorktreeLeaseError::GitCommandFailed {
        operation,
        message: "Git 返回了非 UTF-8 路径".to_owned(),
    })?;
    let text = text.trim_end_matches(['\r', '\n']);
    if text.is_empty() || text.contains('\0') {
        return Err(GitWorktreeLeaseError::GitCommandFailed {
            operation,
            message: "Git 返回了无效路径".to_owned(),
        });
    }
    Ok(PathBuf::from(text))
}

/// 把 Git porcelain 路径和 Rust 平台路径归一为可比较的绝对文本键。
fn normalized_git_path_key(path: &Path) -> Result<String, GitWorktreeLeaseError> {
    let rendered = path
        .to_str()
        .ok_or_else(|| GitWorktreeLeaseError::UnsafeManagedPath {
            message: "Worktree 路径不是 UTF-8，无法核验 Git 登记".to_owned(),
        })?;
    let normalized = rendered.replace('\\', "/");
    #[cfg(windows)]
    let normalized = normalized.to_lowercase();
    Ok(normalized.trim_end_matches('/').to_owned())
}

/// 构造不经过 shell 的 `key=path` Git 临时配置参数。
fn config_argument(key: &str, path: &Path) -> OsString {
    let mut value = OsString::from(key);
    value.push("=");
    value.push(path.as_os_str());
    value
}

/// 返回不包含传入路径正文且严格有界的 Git 错误摘要。
fn bounded_git_error(output: &Output, redacted_paths: &[&Path]) -> String {
    let source = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let mut message = String::from_utf8_lossy(source).into_owned();
    for path in redacted_paths {
        let rendered = path.to_string_lossy();
        if !rendered.is_empty() {
            message = message.replace(rendered.as_ref(), "<path>");
        }
    }
    let message = message.trim();
    if message.is_empty() {
        format!("git 以状态 {} 退出", output.status)
    } else {
        truncate_utf8(message, MAX_GIT_ERROR_BYTES)
    }
}

/// 在 UTF-8 字符边界截断外部错误文本。
fn truncate_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// 返回当前 Unix Epoch 毫秒，时钟异常时稳定拒绝创建。
fn unix_millis() -> Result<u64, GitWorktreeLeaseError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| persistence_message("read_clock", "系统时间早于 Unix Epoch"))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| persistence_message("read_clock", "系统时间毫秒数溢出"))
}

/// 创建保留底层 I/O 分类且不包含不可信文件正文的持久化错误。
fn persistence_error(operation: &'static str, error: io::Error) -> GitWorktreeLeaseError {
    GitWorktreeLeaseError::PersistenceFailed {
        operation,
        message: truncate_utf8(&error.to_string(), MAX_GIT_ERROR_BYTES),
    }
}

/// 创建不包含外部正文的固定持久化错误。
fn persistence_message(operation: &'static str, message: &str) -> GitWorktreeLeaseError {
    GitWorktreeLeaseError::PersistenceFailed {
        operation,
        message: message.to_owned(),
    }
}

/// 在支持目录 fsync 的平台同步原子替换后的目录项。
fn sync_directory(directory: &Path, operation: &'static str) -> Result<(), GitWorktreeLeaseError> {
    #[cfg(unix)]
    {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| persistence_error(operation, error))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, operation);
    }
    Ok(())
}

/// 在 Windows 隐藏 Git 子进程控制台，其余平台保持默认创建标志。
fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use tempfile::TempDir;

    /// 创建包含一个提交的独立测试仓库并返回规范顶层路径。
    fn create_repository(root: &Path) -> PathBuf {
        let repository = root.join("repository");
        fs::create_dir(&repository).expect("测试仓库目录应可创建");
        git(&repository, ["init", "--quiet"]);
        git(&repository, ["config", "user.name", "KeenCode Test"]);
        git(
            &repository,
            ["config", "user.email", "keencode-test@example.invalid"],
        );
        git(&repository, ["config", "commit.gpgsign", "false"]);
        git(&repository, ["config", "core.autocrlf", "false"]);
        fs::write(repository.join("README.md"), "# managed worktree\n").expect("测试文件应可写入");
        git(&repository, ["add", "--", "README.md"]);
        git(&repository, ["commit", "--quiet", "-m", "initial"]);
        fs::canonicalize(repository).expect("测试仓库应可规范化")
    }

    /// 运行无交互测试 Git 命令并要求成功。
    fn git<I, S>(repository: &Path, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(repository)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "Never")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut command);
        let output = command.output().expect("测试环境必须存在 git");
        assert!(
            output.status.success(),
            "测试 Git 命令失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// 创建固定测试 Session 身份。
    fn test_session() -> SessionId {
        SessionId::new("session-managed-worktree").expect("测试 Session 身份应有效")
    }

    /// 创建请求、验证真实 checkout，并证明释放调用严格幂等。
    #[test]
    fn creates_inspects_and_idempotently_releases_real_worktree() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let repository = create_repository(temporary.path());
        let manager = GitWorktreeLeaseManager::open(temporary.path().join("managed"))
            .expect("管理根应可打开");

        let worktree = manager
            .create(GitWorktreeCreateRequest {
                session_id: test_session(),
                repository_root: repository.clone(),
                start_point: None,
            })
            .expect("真实 Worktree 应可创建");
        assert!(worktree.path.is_dir());
        assert_eq!(
            fs::read_to_string(worktree.path.join("README.md")).unwrap(),
            "# managed worktree\n"
        );
        let expected_commit = String::from_utf8(git(&repository, ["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        assert_eq!(worktree.commit, expected_commit);
        assert_eq!(manager.inspect(&worktree.lease).unwrap(), worktree);

        assert_eq!(
            manager.release(&worktree.lease).unwrap(),
            GitWorktreeReleaseOutcome::Released
        );
        assert!(!worktree.path.exists());
        assert_eq!(
            manager.release(&worktree.lease).unwrap(),
            GitWorktreeReleaseOutcome::AlreadyReleased
        );
    }

    /// 同一持久管理根在一个时刻只能由一个进程实例拥有。
    #[test]
    fn rejects_second_live_manager_for_same_root() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let managed = temporary.path().join("managed");
        let first = GitWorktreeLeaseManager::open(&managed).expect("首个 Manager 应取得锁");
        assert!(matches!(
            GitWorktreeLeaseManager::open(&managed),
            Err(GitWorktreeLeaseError::ManagedRootBusy)
        ));
        drop(first);
        GitWorktreeLeaseManager::open(&managed).expect("首个 Manager 释放后应可重新打开");
    }

    /// 仓库子目录和与管理根重叠的目录都不能作为创建入口。
    #[test]
    fn rejects_non_top_level_and_overlapping_repository_roots() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let repository = create_repository(temporary.path());
        let nested = repository.join("nested");
        fs::create_dir(&nested).unwrap();
        let manager = GitWorktreeLeaseManager::open(temporary.path().join("managed"))
            .expect("管理根应可打开");
        assert!(matches!(
            manager.create(GitWorktreeCreateRequest {
                session_id: test_session(),
                repository_root: fs::canonicalize(nested).unwrap(),
                start_point: None,
            }),
            Err(GitWorktreeLeaseError::InvalidRepository { .. })
        ));
        assert!(matches!(
            manager.create(GitWorktreeCreateRequest {
                session_id: test_session(),
                repository_root: manager.managed_root().to_path_buf(),
                start_point: None,
            }),
            Err(GitWorktreeLeaseError::InvalidRepository { .. })
        ));
    }

    /// commit-ish 必须在调用 Git 和创建持久记录前通过严格边界校验。
    #[test]
    fn rejects_unsafe_start_point_before_creating_record() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let repository = create_repository(temporary.path());
        let manager = GitWorktreeLeaseManager::open(temporary.path().join("managed"))
            .expect("管理根应可打开");
        for invalid in ["", " HEAD", "HEAD\nmain", "--help"] {
            assert!(matches!(
                manager.create(GitWorktreeCreateRequest {
                    session_id: test_session(),
                    repository_root: repository.clone(),
                    start_point: Some(invalid.to_owned()),
                }),
                Err(GitWorktreeLeaseError::InvalidStartPoint)
            ));
        }
        assert_eq!(
            fs::read_dir(manager.records_root.as_path())
                .unwrap()
                .count(),
            0
        );
    }

    /// 进程在 Git 删除后、墓碑提交前崩溃时，冷恢复应完成 prune 和墓碑写入。
    #[test]
    fn recovery_converges_after_worktree_was_removed_before_record_commit() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let repository = create_repository(temporary.path());
        let managed = temporary.path().join("managed");
        let manager = GitWorktreeLeaseManager::open(&managed).expect("管理根应可打开");
        let worktree = manager
            .create(GitWorktreeCreateRequest {
                session_id: test_session(),
                repository_root: repository.clone(),
                start_point: None,
            })
            .unwrap();
        git(
            &repository,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                OsStr::new("--force"),
                worktree.path.as_os_str(),
            ],
        );
        drop(manager);

        let recovered = GitWorktreeLeaseManager::open(&managed).expect("恢复 Manager 应取得锁");
        let report = recovered.recover_stale().expect("冷恢复应完成清理");
        assert_eq!(report.released, vec![worktree.lease.clone()]);
        assert_eq!(
            recovered.release(&worktree.lease).unwrap(),
            GitWorktreeReleaseOutcome::AlreadyReleased
        );
        assert!(recovered.recover_stale().unwrap().released.is_empty());
    }

    /// 原仓库已经丢失时，只能删除精确受管目标，不能对其他仓库执行 Git 修改。
    #[test]
    fn release_safely_removes_managed_tree_when_repository_disappears() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let repository = create_repository(temporary.path());
        let manager = GitWorktreeLeaseManager::open(temporary.path().join("managed"))
            .expect("管理根应可打开");
        let worktree = manager
            .create(GitWorktreeCreateRequest {
                session_id: test_session(),
                repository_root: repository.clone(),
                start_point: None,
            })
            .unwrap();
        fs::rename(&repository, temporary.path().join("repository-moved"))
            .expect("测试仓库应可模拟消失");

        assert_eq!(
            manager.release(&worktree.lease).unwrap(),
            GitWorktreeReleaseOutcome::Released
        );
        assert!(!worktree.path.exists());
    }

    /// 篡改记录不能诱导清理器删除管理根外的目录。
    #[test]
    fn tampered_record_never_deletes_unmanaged_path() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let repository = create_repository(temporary.path());
        let manager = GitWorktreeLeaseManager::open(temporary.path().join("managed"))
            .expect("管理根应可打开");
        let worktree = manager
            .create(GitWorktreeCreateRequest {
                session_id: test_session(),
                repository_root: repository.clone(),
                start_point: None,
            })
            .unwrap();
        let outsider = temporary.path().join("must-survive");
        fs::create_dir(&outsider).unwrap();
        fs::write(outsider.join("proof.txt"), "keep").unwrap();
        let record_path = manager.record_path(&worktree.lease);
        let original = fs::read(&record_path).unwrap();
        let mut tampered: serde_json::Value = serde_json::from_slice(&original).unwrap();
        tampered["worktreePath"] = serde_json::Value::String(
            fs::canonicalize(&outsider)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        fs::write(&record_path, serde_json::to_vec(&tampered).unwrap()).unwrap();

        assert!(matches!(
            manager.release(&worktree.lease),
            Err(GitWorktreeLeaseError::UnsafeManagedPath { .. })
        ));
        assert_eq!(
            fs::read_to_string(outsider.join("proof.txt")).unwrap(),
            "keep"
        );

        fs::write(&record_path, original).unwrap();
        manager.release(&worktree.lease).unwrap();
    }

    /// 批量释放对输入排序去重，并在未知 lease 失败时保留部分成功结果。
    #[test]
    fn batch_release_reports_partial_success_without_repeating_lease() {
        let temporary = TempDir::new().expect("测试临时目录应可创建");
        let repository = create_repository(temporary.path());
        let manager = GitWorktreeLeaseManager::open(temporary.path().join("managed"))
            .expect("管理根应可打开");
        let worktree = manager
            .create(GitWorktreeCreateRequest {
                session_id: test_session(),
                repository_root: repository,
                start_point: None,
            })
            .unwrap();
        let unknown = WorktreeLease::new("wt-unknown").unwrap();
        let error = manager
            .release_many(&[
                worktree.lease.clone(),
                unknown.clone(),
                worktree.lease.clone(),
            ])
            .expect_err("未知 lease 应形成显式批量失败");
        let GitWorktreeLeaseError::CleanupFailed { report, failures } = error else {
            panic!("应返回包含部分结果的 CleanupFailed")
        };
        assert_eq!(report.released, vec![worktree.lease]);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].lease, unknown);
    }
}
