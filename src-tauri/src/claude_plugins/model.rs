//! Claude 插件公开数据模型、来源计划与存储布局。

use super::*;
use std::fmt;

/// Claude Code 插件根清单的相对路径。
pub const CLAUDE_PLUGIN_MANIFEST: &str = ".claude-plugin/plugin.json";
/// Claude Code 市场清单的相对路径。
pub const CLAUDE_MARKETPLACE_MANIFEST: &str = ".claude-plugin/marketplace.json";
/// 单个 JSON 清单允许读取的最大字节数，避免恶意市场耗尽内存。
pub const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
/// 本模块所有可展示的错误；错误文本不包含用户配置中的敏感值。
#[derive(Debug)]
pub enum ClaudePluginError {
    /// 文件系统读写错误。
    Io(io::Error),
    /// JSON 结构或字段值错误。
    Json(serde_json::Error),
    /// 调用者输入或清单违反当前 Claude Code 约束。
    Invalid(String),
    /// 依赖图中出现了闭环。
    DependencyCycle(Vec<PluginId>),
    /// 变量插值需要的值没有提供。
    MissingVariable(String),
}

impl fmt::Display for ClaudePluginError {
    /// 将错误转换为面向调用方的中文文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Claude 插件文件操作失败：{error}"),
            Self::Json(error) => write!(formatter, "Claude 插件 JSON 格式无效：{error}"),
            Self::Invalid(message) => write!(formatter, "Claude 插件配置无效：{message}"),
            Self::DependencyCycle(cycle) => write!(
                formatter,
                "Claude 插件依赖存在循环：{}",
                cycle
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::MissingVariable(name) => write!(formatter, "Claude 插件缺少变量值：{name}"),
        }
    }
}

impl std::error::Error for ClaudePluginError {}

impl From<io::Error> for ClaudePluginError {
    /// 将底层 I/O 错误转换为模块错误。
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ClaudePluginError {
    /// 将 serde JSON 错误转换为模块错误。
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// 模块内统一 Result 别名。
pub type Result<T> = std::result::Result<T, ClaudePluginError>;

/// `plugin@marketplace` 形式的插件稳定标识。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PluginId {
    /// 市场内的插件名称。
    pub plugin: String,
    /// 可选市场命名空间；省略时由解析上下文唯一确定。
    pub marketplace: Option<String>,
}

impl PluginId {
    /// 从 `plugin` 或 `plugin@marketplace` 解析插件 ID。
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ClaudePluginError::Invalid("插件 ID 不能为空".to_owned()));
        }
        let (plugin, marketplace) = match raw.rsplit_once('@') {
            Some((plugin, marketplace)) if !plugin.is_empty() && !marketplace.is_empty() => {
                (plugin, Some(marketplace))
            }
            Some(_) => {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件 ID 必须为 plugin 或 plugin@marketplace：{raw}"
                )));
            }
            None => (raw, None),
        };
        Ok(Self {
            plugin: normalized_identifier(plugin, "插件名称")?,
            marketplace: marketplace
                .map(|value| normalized_identifier(value, "市场名称"))
                .transpose()?,
        })
    }

    /// 使用给定市场补全无命名空间的 ID。
    pub fn in_marketplace(&self, marketplace: &str) -> Result<Self> {
        Ok(Self {
            plugin: self.plugin.clone(),
            marketplace: Some(
                self.marketplace
                    .clone()
                    .unwrap_or(normalized_identifier(marketplace, "市场名称")?),
            ),
        })
    }
}

impl fmt::Display for PluginId {
    /// 输出唯一 ID；未命名空间的 ID 保留其原始简写。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.marketplace {
            Some(marketplace) => write!(formatter, "{}@{marketplace}", self.plugin),
            None => formatter.write_str(&self.plugin),
        }
    }
}

impl<'de> Deserialize<'de> for PluginId {
    /// 读取当前状态文件使用的对象形式，同时接受插件引用常用的字符串简写。
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(raw) => Self::parse(&raw).map_err(serde::de::Error::custom),
            Value::Object(mut object) => {
                let plugin = object
                    .remove("plugin")
                    .and_then(|value| value.as_str().map(ToOwned::to_owned))
                    .ok_or_else(|| serde::de::Error::custom("插件 ID 对象必须有 string plugin"))?;
                let marketplace = match object.remove("marketplace") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) => Some(value),
                    Some(_) => {
                        return Err(serde::de::Error::custom(
                            "插件 ID 对象的 marketplace 必须是 string 或 null",
                        ));
                    }
                };
                Self::parse(&match marketplace {
                    Some(marketplace) => format!("{plugin}@{marketplace}"),
                    None => plugin,
                })
                .map_err(serde::de::Error::custom)
            }
            _ => Err(serde::de::Error::custom(
                "插件 ID 必须是 plugin@marketplace 字符串或对象",
            )),
        }
    }
}

/// Claude 市场顶层清单。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceManifest {
    /// 市场稳定命名空间；用于组成 `plugin@marketplace`。
    pub name: String,
    /// 市场的人类可读所有者信息。
    #[serde(default)]
    pub owner: Option<MarketplaceOwner>,
    /// 市场元数据（Claude Code 允许任意附加字段）。
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    /// 市场提供的插件条目。
    pub plugins: Vec<MarketplacePlugin>,
    /// 前向兼容的未知顶层字段，完整保留而不是丢弃。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Claude 市场的所有者描述。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceOwner {
    /// 显示名称。
    #[serde(default)]
    pub name: Option<String>,
    /// 联系邮箱或其他公开标识。
    #[serde(default)]
    pub email: Option<String>,
    /// 所有者主页。
    #[serde(default)]
    pub url: Option<String>,
    /// 未识别的公开字段。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 市场中的单个可安装插件。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplacePlugin {
    /// 市场内唯一的插件名称。
    pub name: String,
    /// 用于取得插件目录的来源。
    pub source: PluginSource,
    /// 市场为插件提供的简短说明。
    #[serde(default)]
    pub description: Option<String>,
    /// 市场锁定或展示的版本。
    #[serde(default)]
    pub version: Option<String>,
    /// 市场标签。
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Claude Code 的可选分类。
    #[serde(default)]
    pub category: Option<String>,
    /// 市场层声明的依赖（补充插件清单 dependencies）。
    #[serde(default, deserialize_with = "deserialize_dependencies")]
    pub dependencies: BTreeMap<String, VersionRequirement>,
    /// 未识别字段原样保存，以兼容新版 Claude Code。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Claude `.claude-plugin/plugin.json` 清单。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    /// 插件稳定名称；安装时必须匹配市场条目名称。
    pub name: String,
    /// 插件版本；Claude Code 允许省略，缓存会使用受控的 `unversioned` 段。
    #[serde(default)]
    pub version: Option<String>,
    /// 插件用途说明。
    #[serde(default)]
    pub description: Option<String>,
    /// 插件作者。
    #[serde(default)]
    pub author: Option<PluginAuthor>,
    /// 插件主页。
    #[serde(default)]
    pub homepage: Option<String>,
    /// 源代码仓库。
    #[serde(default)]
    pub repository: Option<Repository>,
    /// 许可证表达式或许可证名称。
    #[serde(default)]
    pub license: Option<String>,
    /// 搜索关键词。
    #[serde(default)]
    pub keywords: Vec<String>,
    /// 命令目录或命令文件声明。
    #[serde(default)]
    pub commands: ComponentDeclaration,
    /// Skill 目录或 Skill 文件声明。
    #[serde(default)]
    pub skills: ComponentDeclaration,
    /// Agent 目录或 Agent 文件声明。
    #[serde(default)]
    pub agents: ComponentDeclaration,
    /// Hook 配置；完整 JSON 留给运行时适配器解释。
    #[serde(default)]
    pub hooks: Option<Value>,
    /// 清单内声明的 MCP Server，接受内联对象、数组和相对配置文件。
    #[serde(default)]
    pub mcp_servers: McpServersDeclaration,
    /// 清单内声明的 LSP Server；格式对齐 Peri `agent-v3.6.5` 的数组契约。
    #[serde(default)]
    pub lsp_servers: Vec<PluginLspServer>,
    /// 用户可配置字段定义。
    #[serde(default)]
    pub user_config: BTreeMap<String, UserConfigDefinition>,
    /// 插件级依赖声明。
    #[serde(default, deserialize_with = "deserialize_dependencies")]
    pub dependencies: BTreeMap<String, VersionRequirement>,
    /// 未识别字段原样保存，以兼容新版 Claude Code。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 插件作者信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAuthor {
    /// 作者显示名称。
    #[serde(default)]
    pub name: Option<String>,
    /// 作者公开邮箱。
    #[serde(default)]
    pub email: Option<String>,
    /// 作者主页。
    #[serde(default)]
    pub url: Option<String>,
    /// 未识别的公开字段。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 插件仓库字段，接受 Claude 清单使用的字符串或对象形式。
#[derive(Clone, Debug, Serialize)]
pub enum Repository {
    /// 简单 URL 或 `owner/repository` 字符串。
    String(String),
    /// 带类型、URL 和目录的完整仓库对象。
    Object(RepositoryObject),
}

/// 完整仓库对象。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryObject {
    /// 仓库类型，例如 git。
    #[serde(default)]
    pub kind: Option<String>,
    /// 仓库 URL。
    #[serde(default)]
    pub url: Option<String>,
    /// 可选的仓库内目录。
    #[serde(default)]
    pub directory: Option<String>,
    /// 未识别字段。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl<'de> Deserialize<'de> for Repository {
    /// 兼容字符串和对象两种仓库表示。
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(value) => Ok(Self::String(value)),
            Value::Object(object) => serde_json::from_value(Value::Object(object))
                .map(Self::Object)
                .map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom("repository 必须是字符串或对象")),
        }
    }
}

/// 命令、Skills、Agents 的清单声明；兼容单个路径、路径数组和 `{ path }` 对象。
#[derive(Clone, Debug, Default, Serialize)]
pub struct ComponentDeclaration {
    /// 相对于插件根目录的路径。
    pub paths: Vec<String>,
}

impl<'de> Deserialize<'de> for ComponentDeclaration {
    /// 将所有 Claude 允许的紧凑表示归一为路径数组。
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let paths = match value {
            Value::Null => Vec::new(),
            Value::String(path) => vec![path],
            Value::Array(values) => values
                .into_iter()
                .map(|value| match value {
                    Value::String(path) => Ok(path),
                    Value::Object(mut object) => object
                        .remove("path")
                        .and_then(|value| value.as_str().map(ToOwned::to_owned))
                        .ok_or_else(|| "组件数组对象必须有 string path".to_owned()),
                    _ => Err("组件路径必须是字符串或带 path 的对象".to_owned()),
                })
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(serde::de::Error::custom)?,
            Value::Object(mut object) => object
                .remove("path")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .map(|path| vec![path])
                .ok_or_else(|| serde::de::Error::custom("组件对象必须有 string path"))?,
            _ => {
                return Err(serde::de::Error::custom(
                    "组件声明必须是路径、路径数组或对象",
                ));
            }
        };
        Ok(Self { paths })
    }
}

/// `mcpServers` 的归一化声明；Claude 清单可内联 Server、提供数组，或引用相对 JSON 文件。
#[derive(Clone, Debug, Default, Serialize)]
pub struct McpServersDeclaration {
    /// 清单中直接声明的 Server，后者覆盖同名文件配置。
    pub inline: BTreeMap<String, Value>,
    /// 相对于插件根目录的 MCP JSON 文件。
    pub files: Vec<String>,
}

impl<'de> Deserialize<'de> for McpServersDeclaration {
    /// 兼容对象、Server 数组、单个文件路径及它们的组合对象。
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_mcp_servers_declaration(Value::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

/// Peri `agent-v3.6.5` 支持的 Claude 插件 LSP Server 声明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLspServer {
    /// 插件内唯一的 Server 名称；运行时会加上 `plugin:<plugin>:` 命名空间。
    pub name: String,
    /// 直接交给 LSP 子进程的可执行命令，支持 Claude 插件变量插值。
    pub command: String,
    /// 直接交给 LSP 子进程的参数，支持 Claude 插件变量插值。
    #[serde(default)]
    pub args: Vec<String>,
    /// 传递给 LSP 子进程的环境变量，支持 Claude 插件变量插值。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 文件扩展名到 LSP language ID 的映射。
    #[serde(default)]
    pub extension_to_language: BTreeMap<String, String>,
    /// 直接传给 LSP initialize 请求的初始化选项。
    #[serde(default)]
    pub initialization_options: Option<Value>,
    /// 是否禁用该 LSP Server。
    #[serde(default)]
    pub disabled: Option<bool>,
    /// LSP Server 允许的最大自动重启次数。
    #[serde(default)]
    pub max_restarts: Option<u32>,
    /// LSP initialize 请求的启动超时，单位为毫秒。
    #[serde(default)]
    pub startup_timeout: Option<u64>,
    /// 未识别字段原样保存，避免新版 Claude 插件字段导致整个市场不可用。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 用户配置值类型。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserConfigType {
    /// UTF-8 文本。
    String,
    /// JSON 数字。
    Number,
    /// JSON 布尔值。
    Boolean,
    /// 在 `enum_values` 中选择一个值。
    Select,
    /// 用户选择的本地目录路径。
    Directory,
    /// 用户选择的本地文件路径。
    File,
}

/// 插件用户配置字段定义。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserConfigDefinition {
    /// 字段值类型。
    #[serde(rename = "type")]
    pub value_type: UserConfigType,
    /// 设置界面展示的说明。
    #[serde(default)]
    pub description: Option<String>,
    /// 设置界面中的短标题；未提供时调用方展示字段名。
    #[serde(default)]
    pub title: Option<String>,
    /// 未提供时采用的默认值；敏感字段禁止持久化该值。
    #[serde(default)]
    pub default: Option<Value>,
    /// 是否必须由用户提供。
    #[serde(default)]
    pub required: bool,
    /// 是否把值写入独立的敏感存储。
    #[serde(default)]
    pub sensitive: bool,
    /// 是否允许多个同类型值；启用后值必须是数组。
    #[serde(default)]
    pub multiple: bool,
    /// 数字最小值，或字符串/路径最短长度。
    #[serde(default)]
    pub min: Option<f64>,
    /// 数字最大值，或字符串/路径最大长度。
    #[serde(default)]
    pub max: Option<f64>,
    /// select 类型允许的候选值；兼容 `enum` 字段。
    #[serde(default, rename = "enum")]
    pub enum_values: Vec<Value>,
    /// 未识别字段。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 依赖的可选版本要求；不自行实现 semver 求解，只精确保存市场声明。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct VersionRequirement(pub String);

/// Claude 市场来源，包括 URL/GitHub/Git/npm/file/directory/settings。
#[derive(Clone, Debug, Serialize)]
pub enum MarketplaceSource {
    /// HTTP(S) 市场清单 URL。
    Url {
        /// 市场清单地址。
        url: String,
        /// 请求市场清单时附加的 HTTP 头；敏感值只在当前调用中使用。
        headers: BTreeMap<String, String>,
    },
    /// GitHub 仓库和可选 ref。
    Github {
        /// GitHub `owner/repo`。
        repo: String,
        /// 可选 branch、tag 或 commit。
        reference: Option<String>,
        /// 仓库内 marketplace.json 的路径；默认 `.claude-plugin/marketplace.json`。
        path: Option<String>,
        /// Git sparse-checkout 的目录列表。
        sparse_paths: Vec<String>,
    },
    /// 通用 Git URL 和可选 ref。
    Git {
        /// Git 仓库地址。
        url: String,
        /// 可选 branch、tag 或 commit。
        reference: Option<String>,
        /// 仓库内 marketplace.json 的路径；默认 `.claude-plugin/marketplace.json`。
        path: Option<String>,
        /// Git sparse-checkout 的目录列表。
        sparse_paths: Vec<String>,
    },
    /// npm 包名与可选版本。
    Npm {
        /// npm 包名。
        package: String,
        /// 可选版本或版本范围。
        version: Option<String>,
        /// 可选的私有 npm registry URL。
        registry: Option<String>,
    },
    /// 本地市场清单文件。
    File { path: String },
    /// 本地市场根目录。
    Directory { path: String },
    /// 从应用 settings 中取具名市场来源。
    Settings { key: String },
}

/// 插件来源，包括相对路径/npm/url/github/git-subdir/pip。
#[derive(Clone, Debug, Serialize)]
pub enum PluginSource {
    /// 相对于市场清单所在目录的插件目录。
    Relative { path: String },
    /// npm 包名与可选版本。
    Npm {
        /// npm 包名。
        package: String,
        /// 可选版本或版本范围。
        version: Option<String>,
        /// 可选的私有 npm registry URL。
        registry: Option<String>,
    },
    /// HTTP(S) Git 仓库 URL；Claude Code 将 `url` source 解释为 Git 仓库。
    Url {
        /// 可克隆的 Git 仓库地址。
        url: String,
        /// 可选 branch、tag 或 commit。
        reference: Option<String>,
        /// 可选的固定 40 位提交 SHA。
        sha: Option<String>,
    },
    /// GitHub 仓库和可选 ref。
    Github {
        /// GitHub 仓库和可选 ref。
        repo: String,
        reference: Option<String>,
        /// 可选的固定 40 位提交 SHA。
        sha: Option<String>,
    },
    /// Git 仓库子目录和可选 ref。
    GitSubdir {
        /// Git 仓库 URL。
        url: String,
        /// 仓库中的相对插件目录。
        path: String,
        /// 可选 branch、tag 或 commit。
        reference: Option<String>,
        /// 可选的固定 40 位提交 SHA。
        sha: Option<String>,
    },
    /// pip 包；由系统能力层以受审计的参数化调用取得，模块本身不执行 pip。
    Pip {
        /// PyPI 包名。
        package: String,
        /// 可选版本约束。
        version: Option<String>,
        /// 可选的私有 PyPI registry URL。
        registry: Option<String>,
    },
}

impl<'de> Deserialize<'de> for MarketplaceSource {
    /// 解析字符串简写和 `source` 判别对象。
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_marketplace_source(Value::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for PluginSource {
    /// 解析相对路径字符串和 `source` 判别对象。
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_plugin_source(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// 系统能力层应执行的来源取得计划；本模块只生成计划，不绕过审查自行执行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceFetchPlan {
    /// 使用 HTTP GET 下载 URL。
    Http { url: String },
    /// 克隆 Git 仓库；`subdir` 为克隆后要取出的目录。
    Git {
        /// Git 可访问的仓库 URL。
        url: String,
        /// 可选 Git ref。
        reference: Option<String>,
        /// 可选的固定 40 位提交 SHA。
        sha: Option<String>,
        /// 仓库内相对目标目录。
        subdir: Option<PathBuf>,
    },
    /// 使用 npm pack 获得包归档。
    Npm {
        /// npm 包规范。
        package_spec: String,
        /// 可选的私有 registry URL。
        registry: Option<String>,
    },
    /// 使用 pip 安装或下载获得 Python 包；不得拼接为 shell 文本执行。
    Pip {
        /// pip 包规范。
        package_spec: String,
        /// 可选的私有 PyPI registry URL。
        registry: Option<String>,
    },
    /// 已受访问范围授权的本地文件。
    File { path: PathBuf },
    /// 已受访问范围授权的本地目录。
    Directory { path: PathBuf },
}

/// 市场来源和 settings 键之间的安全解析器。
pub trait MarketplaceSettings {
    /// 返回 settings 中的市场来源；调用方负责决定哪些设置键可用。
    fn marketplace_source(&self, key: &str) -> Option<MarketplaceSource>;
}

impl MarketplaceSource {
    /// 将市场来源转换为需审查的取得计划。
    pub fn fetch_plan(&self, settings: &dyn MarketplaceSettings) -> Result<SourceFetchPlan> {
        validate_marketplace_source(self)?;
        match self {
            Self::Url { url, .. } => Ok(SourceFetchPlan::Http {
                url: validated_http_url(url, "市场 URL")?,
            }),
            Self::Github {
                repo, reference, ..
            } => Ok(SourceFetchPlan::Git {
                url: github_git_url(repo)?,
                reference: reference.clone(),
                sha: None,
                subdir: None,
            }),
            Self::Git { url, reference, .. } => Ok(SourceFetchPlan::Git {
                url: non_empty(url, "Git URL")?.to_owned(),
                reference: reference.clone(),
                sha: None,
                subdir: None,
            }),
            Self::Npm {
                package,
                version,
                registry,
            } => Ok(SourceFetchPlan::Npm {
                package_spec: package_spec(package, version.as_deref())?,
                registry: validate_optional_registry(registry.as_deref())?,
            }),
            Self::File { path } => Ok(SourceFetchPlan::File {
                path: PathBuf::from(non_empty(path, "市场文件路径")?),
            }),
            Self::Directory { path } => Ok(SourceFetchPlan::Directory {
                path: PathBuf::from(non_empty(path, "市场目录路径")?),
            }),
            Self::Settings { key } => settings
                .marketplace_source(non_empty(key, "settings 市场键")?)
                .ok_or_else(|| {
                    ClaudePluginError::Invalid(format!("settings 中不存在市场来源：{key}"))
                })?
                .fetch_plan(settings),
        }
    }
}

/// 校验 marketplace source 的扩展字段，避免 Git 路径或 HTTP 头逃逸安全边界。
fn validate_marketplace_source(source: &MarketplaceSource) -> Result<()> {
    match source {
        MarketplaceSource::Url { url, headers } => {
            validated_http_url(url, "市场 URL")?;
            for (name, value) in headers {
                if name.trim().is_empty()
                    || value.trim().is_empty()
                    || name.bytes().any(|byte| byte == b'\r' || byte == b'\n')
                    || value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
                {
                    return Err(ClaudePluginError::Invalid(
                        "市场 headers 不能包含空值或换行符".to_owned(),
                    ));
                }
            }
        }
        MarketplaceSource::Github {
            path, sparse_paths, ..
        }
        | MarketplaceSource::Git {
            path, sparse_paths, ..
        } => {
            if let Some(path) = path {
                safe_relative_path(path, "市场 marketplace.json 路径")?;
                if !path.ends_with(".json") {
                    return Err(ClaudePluginError::Invalid(
                        "市场 marketplace.json 路径必须以 .json 结尾".to_owned(),
                    ));
                }
            }
            for sparse_path in sparse_paths {
                safe_relative_path(sparse_path, "市场 sparsePaths")?;
            }
        }
        MarketplaceSource::Npm { .. }
        | MarketplaceSource::File { .. }
        | MarketplaceSource::Directory { .. }
        | MarketplaceSource::Settings { .. } => {}
    }
    Ok(())
}

impl PluginSource {
    /// 将插件来源转换为需审查的取得计划；pip 仍由外层系统能力执行。
    pub fn fetch_plan(&self, marketplace_root: &Path) -> Result<SourceFetchPlan> {
        match self {
            Self::Relative { path } => Ok(SourceFetchPlan::Directory {
                path: safe_relative_join(marketplace_root, path, "插件相对路径")?,
            }),
            Self::Npm {
                package,
                version,
                registry,
            } => Ok(SourceFetchPlan::Npm {
                package_spec: package_spec(package, version.as_deref())?,
                registry: validate_optional_registry(registry.as_deref())?,
            }),
            Self::Url {
                url,
                reference,
                sha,
            } => Ok(SourceFetchPlan::Git {
                url: non_empty(url, "插件 Git URL")?.to_owned(),
                reference: reference.clone(),
                sha: sha.clone(),
                subdir: None,
            }),
            Self::Github {
                repo,
                reference,
                sha,
            } => Ok(SourceFetchPlan::Git {
                url: github_git_url(repo)?,
                reference: reference.clone(),
                sha: sha.clone(),
                subdir: None,
            }),
            Self::GitSubdir {
                url,
                path,
                reference,
                sha,
            } => Ok(SourceFetchPlan::Git {
                url: non_empty(url, "Git URL")?.to_owned(),
                reference: reference.clone(),
                sha: sha.clone(),
                subdir: Some(safe_relative_path(path, "Git 子目录")?),
            }),
            Self::Pip {
                package,
                version,
                registry,
            } => Ok(SourceFetchPlan::Pip {
                package_spec: pip_package_spec(package, version.as_deref())?,
                registry: validate_optional_registry(registry.as_deref())?,
            }),
        }
    }
}

/// 版本化缓存的目录布局和公开状态目录。
#[derive(Clone, Debug)]
pub struct PluginStorage {
    /// 插件受控数据根目录（`<data-root>/claude-plugins`）。
    root: PathBuf,
    /// 插件版本副本的根目录。
    pub cache_root: PathBuf,
    /// 不含敏感值的安装和用户配置状态文件。
    pub state_path: PathBuf,
    /// 仅由平台密钥库/安全存储实现使用的敏感值命名空间前缀。
    pub secret_namespace: String,
}

impl PluginStorage {
    /// 依据应用数据根目录构造当前唯一的 Claude 插件布局。
    pub fn under(data_root: impl Into<PathBuf>) -> Self {
        let root = data_root.into().join("claude-plugins");
        Self {
            root: root.clone(),
            cache_root: root.join("cache"),
            state_path: root.join("state.json"),
            secret_namespace: "keencode.claude-plugin".to_owned(),
        }
    }

    /// 返回 `<cache>/<marketplace>/<plugin>/<content-fingerprint>`，拒绝目录穿越。
    pub fn versioned_path(&self, id: &PluginId, version: &str) -> Result<PathBuf> {
        let marketplace = id.marketplace.as_deref().ok_or_else(|| {
            ClaudePluginError::Invalid("版本化缓存必须使用 plugin@marketplace".to_owned())
        })?;
        Ok(self
            .cache_root
            .join(stable_cache_component(marketplace, "市场名称")?)
            .join(stable_cache_component(&id.plugin, "插件名称")?)
            .join(safe_cache_component(version, "插件版本")?))
    }

    /// 创建缓存和公开状态所在父目录；不写入任何密钥。
    pub fn ensure_directories(&self) -> Result<()> {
        ensure_controlled_root(&self.root, "插件受控根目录")?;
        ensure_controlled_descendant_chain(&self.root, &self.cache_root, "插件缓存根目录")?;
        self.validate_layout()?;
        Ok(())
    }

    /// 校验受控目录层级，不跟随 `.keencode/claude-plugins`、`cache` 或状态路径中的
    /// 任意符号链接。缺失的末端目录允许由 `ensure_directories` 创建，已有路径必须
    /// 是当前布局中的普通目录/文件。
    pub(super) fn validate_layout(&self) -> Result<()> {
        validate_controlled_root(&self.root, "插件受控根目录")?;
        if let Ok(metadata) = fs::symlink_metadata(&self.root) {
            if metadata.file_type().is_symlink() {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件受控根目录不允许是符号链接：{}",
                    self.root.display()
                )));
            }
            if !metadata.is_dir() {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件受控根目录不是目录：{}",
                    self.root.display()
                )));
            }
        }

        validate_controlled_path(&self.root, &self.cache_root, "插件缓存根目录")?;
        if let Ok(metadata) = fs::symlink_metadata(&self.cache_root) {
            if metadata.file_type().is_symlink() {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件缓存根目录不允许是符号链接：{}",
                    self.cache_root.display()
                )));
            }
            if !metadata.is_dir() {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件缓存根目录不是目录：{}",
                    self.cache_root.display()
                )));
            }
        }

        validate_controlled_path(&self.root, &self.state_path, "符号链接插件状态文件")?;
        if let Ok(metadata) = fs::symlink_metadata(&self.state_path) {
            if metadata.file_type().is_symlink() {
                return Err(ClaudePluginError::Invalid(format!(
                    "不允许读取符号链接插件状态文件：{}",
                    self.state_path.display()
                )));
            }
            if !metadata.is_file() {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件状态文件不是普通文件：{}",
                    self.state_path.display()
                )));
            }
        }
        Ok(())
    }

    /// 返回平台安全存储使用的、不会泄漏实际值的键名。
    pub fn secret_key(&self, id: &PluginId, field: &str) -> Result<String> {
        self.secret_key_at(id, field, 0)
    }

    /// 返回指定代际的系统安全存储键。代际只作为公开状态中的指针，不包含敏感值。
    pub fn secret_key_at(&self, id: &PluginId, field: &str, generation: u64) -> Result<String> {
        let id = id.in_marketplace(id.marketplace.as_deref().ok_or_else(|| {
            ClaudePluginError::Invalid("敏感配置必须使用带市场的插件 ID".to_owned())
        })?)?;
        let marketplace =
            stable_cache_component(id.marketplace.as_deref().unwrap_or_default(), "市场名称")?;
        let plugin = stable_cache_component(&id.plugin, "插件名称")?;
        let field = safe_cache_component(field, "配置字段")?;

        // 公开标识符允许包含点号，直接使用点号拼接会让例如
        // (market, plugin.part, field) 与 (market.plugin, part, field) 生成
        // 同一个密钥。固定域、长度前缀和字段顺序组成无歧义的输入，再用完整
        // SHA-256 压缩成固定长度的 account，避免把最长合法标识直接交给 Keychain。
        let mut digest = Sha256::new();
        digest.update(b"keencode.claude-plugin/secret-key/v2\0");
        for component in [&marketplace, &plugin, &field] {
            digest.update((component.len() as u16).to_be_bytes());
            digest.update(component.as_bytes());
        }
        let digest = format!("{:x}", digest.finalize());
        Ok(format!(
            "{}.v2.{}.g{}",
            self.secret_namespace, digest, generation
        ))
    }
}
