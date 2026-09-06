//! Skills 对外暴露的 Provider 中立领域类型。

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// Skill 来自的本地配置层级。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SkillSource {
    /// 当前项目的 `.agents/skills` 目录，优先级最高。
    Project,
    /// KeenCode 配置数据目录下的 `skills` 目录。
    Data,
    /// 已启用 KeenCode 插件显式声明的 Skill 文件目录。
    Plugin,
}

impl SkillSource {
    /// 返回同名冲突时使用的稳定优先级，数值越小优先级越高。
    pub const fn priority(self) -> u8 {
        match self {
            Self::Project => 0,
            Self::Data => 1,
            Self::Plugin => 2,
        }
    }
}

/// 一个由上层可信插件清单显式声明的额外 Skills 根目录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillRoot {
    /// 必须是绝对路径且不能是符号链接的 Skills 根目录。
    pub path: PathBuf,
    /// 当前根对应的来源分类。
    pub source: SkillSource,
    /// 是否允许递归发现子目录；精确插件声明通常应设为 `false`。
    pub recursive: bool,
}

/// KeenCode 配置数据目录和当前项目目录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillDirectories {
    /// KeenCode 的配置数据目录；Skill 根目录固定为其下的 `skills`。
    pub data_directory: PathBuf,
    /// 当前项目目录；Skill 根目录固定为其下的 `.agents/skills`。
    pub project_directory: PathBuf,
}

/// Skill 发现和读取的资源上限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SkillLimits {
    /// 单个 `SKILL.md` 允许读取的最大字节数。
    pub max_skill_bytes: u64,
    /// YAML 前置元数据结束分隔符允许出现的最大字节偏移。
    pub max_front_matter_bytes: usize,
    /// 单个 Skill 名称允许包含的最大字节数。
    pub max_name_bytes: usize,
    /// 单个 Skill 说明允许包含的最大字节数。
    pub max_description_bytes: usize,
    /// 两个来源合计允许检查的最大 `SKILL.md` 数量。
    pub max_manifests: usize,
    /// 两个来源合计允许遍历的最大文件系统目录项数量。
    pub max_entries: usize,
    /// 相对 Skill 根目录允许递归进入的最大目录深度。
    pub max_depth: usize,
}

impl Default for SkillLimits {
    /// 返回兼顾常见 Skill 文档与桌面端资源预算的默认上限。
    fn default() -> Self {
        Self {
            max_skill_bytes: 256 * 1024,
            max_front_matter_bytes: 16 * 1024,
            max_name_bytes: 128,
            max_description_bytes: 4 * 1024,
            max_manifests: 512,
            max_entries: 16 * 1024,
            max_depth: 8,
        }
    }
}

impl SkillLimits {
    /// 校验所有限制均可安全用于有界读取和遍历。
    pub fn validate(self) -> Result<(), SkillConfigError> {
        if self.max_skill_bytes == 0 {
            return Err(SkillConfigError::ZeroLimit {
                field: "max_skill_bytes",
            });
        }
        let maximum_read_bytes = (usize::MAX as u64).saturating_sub(1);
        if self.max_skill_bytes > maximum_read_bytes {
            return Err(SkillConfigError::LimitTooLarge {
                field: "max_skill_bytes",
            });
        }
        if self.max_depth > 64 {
            return Err(SkillConfigError::LimitTooLarge { field: "max_depth" });
        }
        for (field, value) in [
            ("max_front_matter_bytes", self.max_front_matter_bytes),
            ("max_name_bytes", self.max_name_bytes),
            ("max_description_bytes", self.max_description_bytes),
            ("max_manifests", self.max_manifests),
            ("max_entries", self.max_entries),
        ] {
            if value == 0 {
                return Err(SkillConfigError::ZeroLimit { field });
            }
        }
        Ok(())
    }
}

/// 一次 Skills 发现使用的完整配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillDiscoveryConfig {
    /// 配置数据目录和当前项目目录。
    pub directories: SkillDirectories,
    /// 按名称禁用的 Skills；ASCII 大小写不影响匹配。
    pub disabled_names: BTreeSet<String>,
    /// 由上层可信插件清单提供的额外绝对根目录。
    pub additional_roots: Vec<SkillRoot>,
    /// 文件大小、数量和递归限制。
    pub limits: SkillLimits,
}

impl SkillDiscoveryConfig {
    /// 使用默认限制和空禁用集合创建发现配置。
    pub fn new(data_directory: impl Into<PathBuf>, project_directory: impl Into<PathBuf>) -> Self {
        Self {
            directories: SkillDirectories {
                data_directory: data_directory.into(),
                project_directory: project_directory.into(),
            },
            disabled_names: BTreeSet::new(),
            additional_roots: Vec::new(),
            limits: SkillLimits::default(),
        }
    }

    /// 替换按名称禁用的 Skills 集合。
    pub fn with_disabled_names(mut self, disabled_names: impl IntoIterator<Item = String>) -> Self {
        self.disabled_names = disabled_names.into_iter().collect();
        self
    }

    /// 替换由上层可信插件清单显式声明的额外根目录。
    pub fn with_additional_roots(mut self, roots: impl IntoIterator<Item = SkillRoot>) -> Self {
        self.additional_roots = roots.into_iter().collect();
        self
    }

    /// 替换发现与读取使用的资源上限。
    pub const fn with_limits(mut self, limits: SkillLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// 目录阶段可安全提供给任意 Provider 的 Skill 元数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCatalogEntry {
    /// Front matter 声明的稳定 Skill 名称。
    pub name: String,
    /// Front matter 声明的简短用途说明。
    pub description: String,
    /// 同名冲突处理后实际生效的来源。
    pub source: SkillSource,
    /// 当前配置是否允许按需加载该 Skill。
    pub enabled: bool,
}

/// 已解析但尚未关联本地来源的 Skill 文档。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSkillDocument {
    /// Front matter 声明的稳定 Skill 名称。
    pub name: String,
    /// Front matter 声明的简短用途说明。
    pub description: String,
    /// 移除 front matter 后的 Markdown 正文。
    pub markdown: String,
}

/// 调用方明确选择后可注入 Agent 上下文的 Skill 内容。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InjectableSkill {
    /// Front matter 声明的稳定 Skill 名称。
    pub name: String,
    /// Front matter 声明的简短用途说明。
    pub description: String,
    /// 移除 front matter 后的 Markdown 正文。
    pub markdown: String,
    /// 当前生效 Skill 的本地来源。
    pub source: SkillSource,
}

/// Skill 诊断的严重程度。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillDiagnosticSeverity {
    /// 不影响其他 Skills 的提示信息。
    Info,
    /// 当前条目被跳过，但扫描可以继续。
    Warning,
    /// 当前根目录或安全边界不可用。
    Error,
}

/// 可供 UI 和日志稳定分类的 Skill 诊断代码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillDiagnosticCode {
    /// 配置的数据目录或项目目录不存在。
    BaseDirectoryMissing,
    /// 固定的 Skills 根目录不存在。
    RootDirectoryMissing,
    /// 基础路径或 Skills 根路径不是目录。
    NotDirectory,
    /// Skills 根目录本身是符号链接。
    RootSymlinkRejected,
    /// Skills 根目录规范化后离开了声明的基础目录。
    RootOutsideBoundary,
    /// 目录读取或路径规范化失败。
    FileSystemFailure,
    /// 遍历遇到符号链接并安全跳过。
    SymlinkSkipped,
    /// 递归深度达到配置上限。
    DepthLimitReached,
    /// 文件系统目录项数量达到配置上限。
    EntryLimitReached,
    /// `SKILL.md` 候选数量达到配置上限。
    ManifestLimitReached,
    /// `SKILL.md` 超过单文件大小上限。
    ManifestTooLarge,
    /// `SKILL.md` 不是普通文件。
    ManifestNotFile,
    /// `SKILL.md` 内容或 front matter 无效。
    InvalidDocument,
    /// 同名 Skill 被更高优先级或稳定路径顺序遮蔽。
    NameConflict,
    /// Skill 被当前配置按名称禁用。
    Disabled,
}

/// 一条不含 Skill 正文的安全发现诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillDiagnostic {
    /// 诊断严重程度。
    pub severity: SkillDiagnosticSeverity,
    /// 稳定机器可读的诊断代码。
    pub code: SkillDiagnosticCode,
    /// 诊断对应的配置来源；基础目录诊断也会填写来源。
    pub source: SkillSource,
    /// 相对对应 Skills 根目录的路径；根级诊断为 `None`。
    pub relative_path: Option<PathBuf>,
    /// 不包含 Skill 正文和密钥的中文诊断信息。
    pub message: String,
}

/// Skills 配置在开始访问文件系统前校验失败。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillConfigError {
    /// 指定限制被配置为零。
    ZeroLimit {
        /// 配置字段的稳定名称。
        field: &'static str,
    },
    /// 指定限制无法安全执行加一哨兵读取。
    LimitTooLarge {
        /// 配置字段的稳定名称。
        field: &'static str,
    },
    /// 额外根目录为空、不是绝对路径或数量超过现有清单容量。
    InvalidAdditionalRoots,
}

impl fmt::Display for SkillConfigError {
    /// 输出不包含用户文件内容的配置错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit { field } => write!(formatter, "Skills 限制 {field} 不能为零"),
            Self::LimitTooLarge { field } => {
                write!(formatter, "Skills 限制 {field} 过大，无法安全读取")
            }
            Self::InvalidAdditionalRoots => {
                formatter.write_str("Skills 额外根目录无效或数量超过上限")
            }
        }
    }
}

impl Error for SkillConfigError {}

/// 按名称加载 Skill 正文时可能返回的安全错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkillLoadError {
    /// 当前目录中不存在指定名称。
    NotFound {
        /// 调用方请求的原始名称。
        name: String,
    },
    /// 指定 Skill 已被当前配置禁用。
    Disabled {
        /// 目录中记录的规范名称。
        name: String,
    },
    /// 发现后根目录被替换或移动。
    RootChanged {
        /// 目录中记录的规范名称。
        name: String,
    },
    /// 发现后路径出现符号链接或越过安全根目录。
    UnsafePath {
        /// 目录中记录的规范名称。
        name: String,
    },
    /// 发现后文件被删除或不再是普通文件。
    Unavailable {
        /// 目录中记录的规范名称。
        name: String,
    },
    /// 当前文件超过发现时配置的大小上限。
    TooLarge {
        /// 目录中记录的规范名称。
        name: String,
        /// 允许读取的最大字节数。
        limit: u64,
    },
    /// 当前文件无法读取。
    ReadFailed {
        /// 目录中记录的规范名称。
        name: String,
        /// 不包含本地绝对路径的错误说明。
        message: String,
    },
    /// 当前文件不是有效 UTF-8 或 Skill 文档。
    InvalidDocument {
        /// 目录中记录的规范名称。
        name: String,
        /// 不包含 Skill 正文的错误说明。
        message: String,
    },
    /// Front matter 元数据在发现后发生变化，调用方应重新发现。
    CatalogStale {
        /// 目录中记录的规范名称。
        name: String,
    },
}

impl fmt::Display for SkillLoadError {
    /// 输出不包含 Skill 正文和绝对路径的加载错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { name } => write!(formatter, "目录中不存在 Skill：{name}"),
            Self::Disabled { name } => write!(formatter, "Skill 已禁用：{name}"),
            Self::RootChanged { name } => write!(formatter, "Skill 根目录已变化：{name}"),
            Self::UnsafePath { name } => write!(formatter, "Skill 路径不安全：{name}"),
            Self::Unavailable { name } => write!(formatter, "Skill 文件不可用：{name}"),
            Self::TooLarge { name, limit } => {
                write!(formatter, "Skill 文件超过 {limit} 字节上限：{name}")
            }
            Self::ReadFailed { name, message } => {
                write!(formatter, "无法读取 Skill {name}：{message}")
            }
            Self::InvalidDocument { name, message } => {
                write!(formatter, "Skill 文档无效 {name}：{message}")
            }
            Self::CatalogStale { name } => {
                write!(formatter, "Skill 元数据已变化，需要重新发现：{name}")
            }
        }
    }
}

impl Error for SkillLoadError {}
