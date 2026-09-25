//! 内置工具共享的工作目录、资源上限与路径解析。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use keencode_agent::{ToolContext, ToolError, ToolOutputArtifactSink};

/// 只读搜索与文件读取工具声明的外层墙钟上限。
///
/// 挂起的文件系统遍历或读取不能把整个 Turn 挂死到用户取消；这些工具
/// 只有大小限制没有内部时间上限，因此声明一个显著大于正常开销的墙钟。
pub(crate) const READ_ONLY_WALL_CLOCK_TIMEOUT: Duration = Duration::from_secs(15);

/// 在文件原子替换前为运行时准备一条可提交的文件变更记录。
pub trait FileMutationRecorder: std::fmt::Debug + Send + Sync {
    /// 记录完整的调用上下文、目标路径以及替换前后的原始字节。
    fn prepare(
        &self,
        context: &ToolContext,
        path: &Path,
        before: Option<&[u8]>,
        after: &[u8],
    ) -> Result<Box<dyn PreparedFileMutation>, ToolError>;
}

/// 已通过准备阶段、等待文件原子替换完成后提交的变更记录。
pub trait PreparedFileMutation: Send {
    /// 标记对应文件变更已经成功落盘。
    fn mark_applied(&self) -> Result<(), ToolError>;
}

/// 防止单次文件或搜索工具无界占用内存的确定性上限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolLimits {
    /// `Read` 单次最多返回的文本行数。
    pub max_read_lines: usize,
    /// `Read` 单次文本结果包含文件头、行号和续读提示在内的最大字节数。
    pub max_read_output_bytes: usize,
    /// `Glob` 或 `Grep` 单次最多返回的匹配项数量。
    pub max_search_results: usize,
    /// `Grep` 会加载到内存搜索的单文件最大字节数。
    pub max_search_file_bytes: u64,
    /// `Edit` 或 `Write` 会加载并原子替换的单文件最大字节数。
    pub max_mutation_file_bytes: u64,
    /// `Read` 可内联返回的单张图片最大字节数。
    pub max_image_bytes: u64,
    /// Shell 未指定超时时采用的默认毫秒数。
    pub default_command_timeout_ms: u64,
    /// Shell 允许请求的最大超时毫秒数。
    pub max_command_timeout_ms: u64,
    /// 每个标准输出流的原始内容预览预算，不含截断标记和完整输出路径。
    pub max_command_preview_bytes: usize,
}

impl Default for ToolLimits {
    /// 返回适合桌面编码会话的保守默认值。
    fn default() -> Self {
        Self {
            max_read_lines: 20_000,
            max_read_output_bytes: 32 * 1024,
            max_search_results: 10_000,
            max_search_file_bytes: 16 * 1024 * 1024,
            max_mutation_file_bytes: 64 * 1024 * 1024,
            max_image_bytes: 8 * 1024 * 1024,
            default_command_timeout_ms: 120_000,
            max_command_timeout_ms: 3_600_000,
            max_command_preview_bytes: 16 * 1024,
        }
    }
}

impl ToolLimits {
    /// 校验全部上限都大于零。
    pub fn validate(self) -> Result<Self, ToolError> {
        if self.max_read_lines == 0
            || self.max_read_output_bytes == 0
            || self.max_search_results == 0
            || self.max_search_file_bytes == 0
            || self.max_mutation_file_bytes == 0
            || self.max_image_bytes == 0
            || self.default_command_timeout_ms == 0
            || self.max_command_timeout_ms == 0
            || self.max_command_preview_bytes == 0
        {
            return Err(ToolError::permanent(
                "invalid_tool_limits",
                "文件与搜索工具的资源上限必须全部大于零",
            ));
        }
        if self.default_command_timeout_ms > self.max_command_timeout_ms {
            return Err(ToolError::permanent(
                "invalid_tool_limits",
                "默认命令超时不能大于最大命令超时",
            ));
        }
        Ok(self)
    }
}

/// 每个 Session 创建一次并由全部本地工具共享的不可变环境。
#[derive(Clone, Debug)]
pub struct ToolEnvironment {
    /// 已规范化且确认存在的 Session 工作目录。
    working_directory: PathBuf,
    /// 文件和搜索工具采用的确定性资源上限。
    limits: ToolLimits,
    /// 保存超大命令完整输出的应用数据目录。
    artifact_directory: PathBuf,
    /// 可选的文件变更记录器；未配置时文件工具保持独立运行。
    file_mutation_recorder: Option<Arc<dyn FileMutationRecorder>>,
    /// 本 Session 内模型读取过的文件指纹，用于拒绝基于陈旧内容的整文件覆写。
    read_state: Arc<Mutex<HashMap<PathBuf, ReadFingerprint>>>,
    /// 本 Session 已生成的 Shell 环境快照路径。
    ///
    /// 快照把用户登录 Shell 的环境变量与别名固化成一份可复放的脚本，后续每条
    /// 命令先 source 它，使 `pnpm`、自定义别名等只在交互式 rc 中定义的命令在
    /// 非交互执行下同样可用。`None` 表示尚未生成（惰性，首个命令时创建）。
    shell_snapshot: Arc<Mutex<Option<PathBuf>>>,
}

/// 模型最近一次读取某文件时观察到的身份信息。
///
/// 只保存读取时刻的 `size` 与 `mtime`，不保存文件正文：正文可能远超上下文
/// 预算，而这两项足以判断"磁盘上的内容是否还是模型读到的那份"。分页读取
/// 同样登记指纹——若要求完整覆盖才允许覆写，受输出字节上限约束的大文件将
/// 永远无法重写；此处只阻断"完全没读过"与"读后已被外部改动"两种情况。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadFingerprint {
    /// 读取时刻的文件字节数。
    pub(crate) size: u64,
    /// 读取时刻的文件修改时间。
    pub(crate) modified: Option<SystemTime>,
}

impl ReadFingerprint {
    /// 从文件元数据构造指纹。
    pub(crate) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

impl ToolEnvironment {
    /// 使用默认资源上限创建本地工具环境。
    pub fn new(working_directory: impl AsRef<Path>) -> Result<Self, ToolError> {
        Self::with_limits(working_directory, ToolLimits::default())
    }

    /// 使用显式资源上限创建本地工具环境。
    pub fn with_limits(
        working_directory: impl AsRef<Path>,
        limits: ToolLimits,
    ) -> Result<Self, ToolError> {
        let limits = limits.validate()?;
        let working_directory =
            std::fs::canonicalize(working_directory.as_ref()).map_err(|error| {
                ToolError::permanent(
                    "invalid_working_directory",
                    format!("无法解析 Session 工作目录：{error}"),
                )
            })?;
        if !working_directory.is_dir() {
            return Err(ToolError::permanent(
                "invalid_working_directory",
                "Session 工作目录不是目录",
            ));
        }
        Ok(Self {
            working_directory,
            limits,
            artifact_directory: std::env::temp_dir().join("keencode").join("tool-output"),
            file_mutation_recorder: None,
            read_state: Arc::new(Mutex::new(HashMap::new())),
            shell_snapshot: Arc::new(Mutex::new(None)),
        })
    }

    /// 覆盖保存超大命令完整输出的目录；目录只在确有输出时创建。
    pub fn with_artifact_directory(
        mut self,
        artifact_directory: impl AsRef<Path>,
    ) -> Result<Self, ToolError> {
        let artifact_directory =
            std::path::absolute(artifact_directory.as_ref()).map_err(|error| {
                ToolError::permanent(
                    "invalid_artifact_directory",
                    format!("无法解析工具输出目录：{error}"),
                )
            })?;
        if artifact_directory.exists() && !artifact_directory.is_dir() {
            return Err(ToolError::permanent(
                "invalid_artifact_directory",
                "工具输出路径存在但不是目录",
            ));
        }
        self.artifact_directory = artifact_directory;
        Ok(self)
    }

    /// 返回 Session 的规范化绝对工作目录。
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    /// 返回当前文件与搜索资源上限。
    pub const fn limits(&self) -> ToolLimits {
        self.limits
    }

    /// 返回保存超大命令完整输出的绝对目录。
    pub fn artifact_directory(&self) -> &Path {
        &self.artifact_directory
    }

    /// 返回本 Session 已生成的 Shell 环境快照路径；尚未生成时返回 `None`。
    pub(crate) fn shell_snapshot_path(&self) -> Option<PathBuf> {
        self.shell_snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 登记本 Session 的 Shell 环境快照路径。
    pub(crate) fn set_shell_snapshot_path(&self, path: PathBuf) {
        *self
            .shell_snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    }

    /// 为后续文件编辑和写入安装可选的运行时变更记录器。
    pub fn with_file_mutation_recorder(mut self, recorder: Arc<dyn FileMutationRecorder>) -> Self {
        self.file_mutation_recorder = Some(recorder);
        self
    }

    /// 返回当前配置的文件变更记录器；未配置时返回 `None`。
    pub fn file_mutation_recorder(&self) -> Option<&dyn FileMutationRecorder> {
        self.file_mutation_recorder.as_deref()
    }

    /// 记录模型刚刚读取过某个文件，供后续整文件覆写判断内容是否仍然新鲜。
    pub(crate) fn record_read(&self, path: &Path, fingerprint: ReadFingerprint) {
        let mut state = self.read_state.lock().unwrap_or_else(|poisoned| {
            // 读取指纹只是保护性提示，中毒锁恢复后继续使用既有映射。
            poisoned.into_inner()
        });
        state.insert(path.to_path_buf(), fingerprint);
    }

    /// 返回本 Session 内模型读取该文件时观察到的指纹。
    pub(crate) fn read_fingerprint(&self, path: &Path) -> Option<ReadFingerprint> {
        let state = self.read_state.lock().unwrap_or_else(|poisoned| {
            // 读取指纹只是保护性提示，中毒锁恢复后继续使用既有映射。
            poisoned.into_inner()
        });
        state.get(path).copied()
    }

    /// 判断精确编辑是否建立在仍然有效的内容认知之上。
    ///
    /// 比整文件覆写宽松：编辑只替换匹配到的片段，不丢弃文件其余内容，因此
    /// 只要求"本 Session 读过且读后未被外部改动"。未读过的文件不在此处拦截，
    /// 交给 `old_string` 精确匹配本身去证明模型确实知道要替换的文本。
    pub(crate) fn ensure_edit_is_fresh(&self, path: &Path) -> Result<(), ToolError> {
        let Some(known) = self.read_fingerprint(path) else {
            return Ok(());
        };
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(());
        };
        if ReadFingerprint::from_metadata(&metadata) != known {
            return Err(ToolError::permanent(
                "file_changed_since_read",
                format!(
                    "文件在读取后被外部修改，已拒绝编辑：{}；请重新读取该文件后再编辑",
                    display_path(path)
                ),
            ));
        }
        Ok(())
    }

    /// 判断整文件覆写是否建立在仍然有效的内容认知之上。
    ///
    /// 返回 `Err` 表示模型没有读过该文件，或读取后文件已被外部改动：此时
    /// 整文件覆写会静默丢弃外部改动，必须要求先重新读取。新建文件（路径
    /// 当前不存在且没有读取记录）不属于陈旧写，由写入路径单独处理。
    pub(crate) fn ensure_write_is_fresh(&self, path: &Path) -> Result<(), ToolError> {
        let Some(known) = self.read_fingerprint(path) else {
            return Err(ToolError::permanent(
                "write_requires_read",
                format!(
                    "整文件覆写前必须先读取目标文件：{}；请先 Read 该文件（或改用 Edit 做局部修改）后再写入",
                    display_path(path)
                ),
            ));
        };
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ToolError::permanent(
                    "file_removed_since_read",
                    format!(
                        "文件在读取后已被删除，已拒绝写入：{}；请确认路径后重试",
                        display_path(path)
                    ),
                ));
            }
            Err(error) => {
                return Err(ToolError::permanent(
                    "write_metadata_failed",
                    format!("{}：{error}", display_path(path)),
                ));
            }
        };
        if ReadFingerprint::from_metadata(&metadata) != known {
            return Err(ToolError::permanent(
                "file_changed_since_read",
                format!(
                    "文件在读取后被外部修改，已拒绝整文件覆写：{}；请重新读取该文件后再写入",
                    display_path(path)
                ),
            ));
        }
        Ok(())
    }

    /// 把非空绝对路径或相对 Session 工作目录的路径转为绝对路径。
    pub(crate) fn resolve_path(&self, raw_path: &str) -> Result<PathBuf, ToolError> {
        if raw_path.trim().is_empty() {
            return Err(ToolError::permanent("invalid_path", "路径不能为空"));
        }
        let path = Path::new(raw_path);
        if !path.is_absolute() && matches!(path.components().next(), Some(Component::Prefix(_))) {
            return Err(ToolError::permanent(
                "invalid_path",
                "不支持缺少根目录的驱动器相对路径",
            ));
        }
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_directory.join(path)
        };
        std::path::absolute(candidate).map_err(|error| {
            ToolError::permanent("invalid_path", format!("无法解析绝对路径：{error}"))
        })
    }
}

/// 把平台路径转为模型输出中稳定的斜杠形式。
pub(crate) fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// 把严格 JSON 输入解析错误归一为工具错误。
pub(crate) fn invalid_input(error: impl std::fmt::Display) -> ToolError {
    ToolError::permanent("invalid_input", format!("工具输入无效：{error}"))
}

/// 把超出模型预算的完整工具输出保存到 Session 工件目录的落盘通道。
///
/// 复用命令输出工件的目录与命名约定（`keencode-{label}-` 前缀随机文件、
/// `.log` 后缀，参见 `command.rs` 的 `create_artifact`），返回的路径文本
/// 会嵌入截断说明，模型可据此用 Read 取回完整输出。
pub(crate) struct EnvironmentArtifactSink {
    /// Session 共享环境中的输出工件目录。
    artifact_directory: PathBuf,
}

impl EnvironmentArtifactSink {
    /// 绑定指定 Session 环境的工件目录。
    pub(crate) fn new(environment: &ToolEnvironment) -> Self {
        Self {
            artifact_directory: environment.artifact_directory().to_path_buf(),
        }
    }
}

impl ToolOutputArtifactSink for EnvironmentArtifactSink {
    /// 同步保存完整 UTF-8 正文并返回模型可读取的稳定路径文本。
    fn save_output(&self, label: &str, content: &str) -> std::io::Result<String> {
        std::fs::create_dir_all(&self.artifact_directory)?;
        // 工具名只保留文件名安全字符并限制长度，避免拼接出异常路径。
        let prefix: String = label
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .take(32)
            .collect();
        let named = tempfile::Builder::new()
            .prefix(&format!("keencode-{prefix}-"))
            .suffix(".log")
            .tempfile_in(&self.artifact_directory)?;
        let (mut file, path) = named.keep().map_err(|error| error.error)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        Ok(display_path(&path))
    }
}
