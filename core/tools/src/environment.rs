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

/// 把路径解析到稳定的比较形态：存在则 canonicalize（消解 8.3 短名与符号
/// 链接差异）；目标尚不存在时用已存在父目录的 canonicalize 结果拼接文件名；
/// 两者都失败时退回原路径。
fn stable_compare_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    if let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) {
        if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
            return canonical_parent.join(file_name);
        }
    }
    path.to_path_buf()
}

/// 守卫比较用的路径规范化：去掉 verbatim 前缀、统一分隔符，Windows 下不区分大小写。
fn normalize_guard_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace("\\\\?\\", "");
    if cfg!(windows) {
        text.replace('/', "\\").to_ascii_lowercase()
    } else {
        text
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
    /// 工作区白名单守卫。开启后文件工具与命令的路径一律限定在工作目录
    /// （加临时目录与系统目录豁免）之内，工作区外的读写直接拒绝。
    ///
    /// 由宿主通过构建器开启（评测模式）；桌面交互默认关闭，行为与过去一致。
    workspace_guard: bool,
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
            artifact_directory: long_form_temp_dir().join("keencode").join("tool-output"),
            file_mutation_recorder: None,
            read_state: Arc::new(Mutex::new(HashMap::new())),
            shell_snapshot: Arc::new(Mutex::new(None)),
            workspace_guard: false,
        })
    }

    /// 开启工作区白名单守卫：文件工具与命令的路径一律限定在工作目录之内
    /// （临时目录与系统目录豁免），工作区外的读写与执行直接拒绝。
    pub fn with_workspace_guard(mut self) -> Self {
        self.workspace_guard = true;
        self
    }

    /// 守卫的允许根清单：工作目录、长名临时目录（离树草稿的合法落点）、
    /// 系统目录（Windows 取 SystemRoot/ProgramFiles/ProgramData 等环境变量）。
    fn guard_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.working_directory.clone(), long_form_temp_dir()];
        // 系统目录豁免：Windows 取环境变量；Linux/macOS 与 pi-go 的
        // systemRoots 对齐（/usr、/bin、/etc、/opt）。
        let system_roots: &[&str] = if cfg!(windows) {
            &[
                "SystemRoot",
                "ProgramFiles",
                "ProgramFiles(x86)",
                "ProgramData",
            ]
        } else {
            &["/usr", "/bin", "/etc", "/opt"]
        };
        for name in system_roots {
            if let Some(value) = std::env::var_os(name) {
                let path = PathBuf::from(value);
                if path.is_dir() {
                    roots.push(path);
                }
            }
        }
        roots
    }

    /// 判定路径是否落在守卫允许根之内：文件精确匹配或目录前缀匹配。
    ///
    /// Windows 下比较不区分大小写并统一分隔符；两侧都先经
    /// [`stable_compare_path`](self::stable_compare_path) 解析，消除 8.3 短名、
    /// verbatim 前缀与符号差异。
    fn path_within_roots(&self, path: &Path) -> bool {
        let candidate = normalize_guard_path(&stable_compare_path(path));
        self.guard_roots().iter().any(|root| {
            let root = normalize_guard_path(root);
            candidate == root
                || candidate.starts_with(&format!("{root}{}", std::path::MAIN_SEPARATOR))
        })
    }

    /// 白名单守卫的路径检查：工作区外直接拒绝。
    pub fn check_workspace_path(&self, path: &Path) -> Result<(), ToolError> {
        if !self.workspace_guard || self.path_within_roots(path) {
            return Ok(());
        }
        Err(ToolError::permanent(
            "path_outside_workspace",
            format!(
                "refused: {} is outside the workspace {}. The harness keeps tool access inside the workspace; use a path under it, or ask the user to widen the boundary.",
                display_path(path),
                display_path(&self.working_directory)
            ),
        ))
    }

    /// 白名单守卫的命令边界检查：提取命令里的路径型 token（绝对路径、~、
    /// `..` 逃逸、POSIX 挂载形态），逐个映射后做根前缀判定。
    ///
    /// 如实边界：这是边界检查而非 shell 解析器——命令以守卫看不见的写法
    /// （如 `python -c` 内嵌路径）越界时，仍是模型自己的责任。
    pub fn check_command_boundary(&self, command: &str) -> Result<(), ToolError> {
        if !self.workspace_guard {
            return Ok(());
        }
        for token in command.split_whitespace() {
            let trimmed = token.trim_matches(|c: char| matches!(c, '"' | '\''));
            let Some(mapped) = self.map_boundary_token(trimmed) else {
                continue;
            };
            if !self.path_within_roots(&mapped) {
                return Err(ToolError::permanent(
                    "path_outside_workspace",
                    format!(
                        "refused: command touches \"{}\" which is outside the workspace {}. The harness keeps command access inside the workspace; use paths under it, or ask the user to widen the boundary.",
                        trimmed,
                        display_path(&self.working_directory)
                    ),
                ));
            }
        }
        Ok(())
    }

    /// 把命令 token 中可验证的位置形态映射为绝对路径；无法验证的形态返回
    /// `None`（按不可验证放行，与 pi-go 的守卫语义一致）。
    ///
    /// 覆盖：Windows 盘符绝对路径与 UNC、`~` 展开、`/tmp`（映射到长名临时
    /// 目录）、git-bash 盘符挂载（`/c/...` → `C:\...`）、`..` 逃逸；`/dev`、
    /// `/proc`、`/sys`、`/nul` 等设备与内核路径按约定跳过；其余 `/...` 形态
    /// 无法在本平台验证，放行。
    fn map_boundary_token(&self, token: &str) -> Option<PathBuf> {
        if token.is_empty() {
            return None;
        }
        let bytes = token.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            return Some(PathBuf::from(token));
        }
        if token.starts_with("\\\\") {
            return Some(PathBuf::from(token));
        }
        if token == ".." || token.starts_with("../") || token.starts_with("..\\") {
            return Some(self.working_directory.join(token));
        }
        if token.starts_with('~') {
            let rest = token.strip_prefix('~').unwrap_or("");
            let rest = rest.trim_start_matches(['/', '\\']);
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)?;
            return Some(join_posix_rest(home, rest));
        }
        if !token.starts_with('/') {
            return None;
        }
        if token == "/dev"
            || token.starts_with("/dev/")
            || token == "/proc"
            || token.starts_with("/proc/")
            || token == "/sys"
            || token.starts_with("/sys/")
            || token == "/nul"
        {
            return None;
        }
        if token == "/tmp" || token.starts_with("/tmp/") {
            return Some(join_posix_rest(
                long_form_temp_dir(),
                token.strip_prefix("/tmp").unwrap_or(""),
            ));
        }
        if token == "/var/tmp" || token.starts_with("/var/tmp/") {
            return Some(join_posix_rest(
                long_form_temp_dir(),
                token.strip_prefix("/var/tmp").unwrap_or(""),
            ));
        }
        // POSIX 盘符挂载（`/c/...` → `C:\...`）是 git-bash 约定，仅 Windows 存在；
        // Linux 上 `/c/...`、`/usr/...` 等就是真实的绝对路径，直接按原样校验。
        if cfg!(windows) {
            let rest = token.strip_prefix('/')?;
            let mut chars = rest.chars();
            let drive = chars.next()?;
            if !drive.is_ascii_alphabetic() {
                return None;
            }
            let after = chars.as_str();
            if !after.is_empty() && !after.starts_with('/') {
                return None;
            }
            let mut windows = String::from(drive.to_ascii_uppercase());
            windows.push(':');
            windows.push_str(&after.replace('/', "\\"));
            return Some(PathBuf::from(windows));
        }
        Some(PathBuf::from(token))
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

/// 返回长名形态的临时目录。
///
/// 用户环境的 TEMP/TMP 可能携带 8.3 短名（如 `CHENGL~1`）：同一物理目录的
/// 多种路径形态会让子进程的字符串级路径比对互相矛盾。此处在 KeenCode 边界
/// 一次性展开为 canonicalize 的长名，内部临时文件与子进程一律拿到稳定形态。
pub fn long_form_temp_dir() -> PathBuf {
    let canonical = stable_compare_path(&std::env::temp_dir());
    // canonicalize 产生 verbatim 前缀（`\\?\`）：对内部比较无所谓，但下发给
    // 子进程会制造另一种路径形态（cmd 与相对路径解析对它有兼容坑）。Temp
    // 目录是固定短结构，剥掉前缀是安全的。
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(stripped) => PathBuf::from(stripped.to_string()),
        None => canonical,
    }
}

/// 子 shell 的 TEMP/TMP 归一覆盖。
///
/// 用户环境的 TEMP/TMP 可能携带 8.3 短名（如 `CHENGL~1`）：同一物理目录的
/// 多种路径形态会让子进程的字符串级路径比对互相矛盾（写入与读取"看到的不是
/// 同一个文件"）。spawn 时用本覆盖把 TEMP/TMP 统一为 canonicalize 长名。
pub fn shell_temp_env_overrides() -> Vec<(String, String)> {
    let temp = long_form_temp_dir().to_string_lossy().into_owned();
    vec![
        ("TEMP".to_string(), temp.clone()),
        ("TMP".to_string(), temp),
    ]
}

/// 把 POSIX 形态的剩余部分拼到 Windows 基路径上；`/tmp` 等映射用。
fn join_posix_rest(base: PathBuf, rest: &str) -> PathBuf {
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        return base;
    }
    if cfg!(windows) {
        base.join(rest.replace('/', "\\"))
    } else {
        base.join(rest)
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
