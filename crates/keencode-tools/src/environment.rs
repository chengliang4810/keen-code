//! 内置工具共享的工作目录、资源上限与路径解析。

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use keencode_agent::{ToolContext, ToolError};

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
    /// Shell 与 Git 未指定超时时采用的默认毫秒数。
    pub default_command_timeout_ms: u64,
    /// Shell 与 Git 允许请求的最大超时毫秒数。
    pub max_command_timeout_ms: u64,
    /// 每个标准输出流直接返回给模型的最大预览字节数。
    pub max_command_preview_bytes: usize,
}

impl Default for ToolLimits {
    /// 返回适合桌面编码会话的保守默认值。
    fn default() -> Self {
        Self {
            max_read_lines: 20_000,
            max_read_output_bytes: 512 * 1024,
            max_search_results: 10_000,
            max_search_file_bytes: 16 * 1024 * 1024,
            max_mutation_file_bytes: 64 * 1024 * 1024,
            max_image_bytes: 8 * 1024 * 1024,
            default_command_timeout_ms: 120_000,
            max_command_timeout_ms: 3_600_000,
            max_command_preview_bytes: 256 * 1024,
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

    /// 为后续文件编辑和写入安装可选的运行时变更记录器。
    pub fn with_file_mutation_recorder(mut self, recorder: Arc<dyn FileMutationRecorder>) -> Self {
        self.file_mutation_recorder = Some(recorder);
        self
    }

    /// 返回当前配置的文件变更记录器；未配置时返回 `None`。
    pub fn file_mutation_recorder(&self) -> Option<&dyn FileMutationRecorder> {
        self.file_mutation_recorder.as_deref()
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
