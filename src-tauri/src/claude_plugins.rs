//! Claude Code 插件兼容层。
//!
//! 本模块只处理 Claude Code 的插件市场/插件清单、可审计的来源解析以及运行时投影；
//! 不依赖 Tauri，也不直接执行网络、Git、npm 或 pip 命令。调用方可以把
//! [`SourceFetchPlan`] 交给受审计的系统能力层执行，再调用本模块安装和加载。
//! 这样 extensions 与 peri runtime 可以复用完全相同的解析、依赖和变量规则。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Cursor, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Claude Code 插件根清单的相对路径。
pub const CLAUDE_PLUGIN_MANIFEST: &str = ".claude-plugin/plugin.json";
/// Claude Code 市场清单的相对路径。
pub const CLAUDE_MARKETPLACE_MANIFEST: &str = ".claude-plugin/marketplace.json";
/// 单个 JSON 清单允许读取的最大字节数，避免恶意市场耗尽内存。
pub const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
/// 单个 MCPB/DXT 归档允许读取的最大字节数，避免把插件包当作无限制下载器。
const MAX_MCPB_BYTES: usize = 128 * 1024 * 1024;
/// MCPB/DXT 解包后的总文件大小上限，防止 ZIP 炸弹耗尽磁盘。
const MAX_MCPB_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;
/// 单个 MCPB/DXT 归档允许包含的最大文件数量。
const MAX_MCPB_ENTRIES: usize = 4096;

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
    #[cfg(test)]
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
            #[cfg(test)]
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
            cache_root: root.join("cache"),
            state_path: root.join("state.json"),
            secret_namespace: "keencode.claude-plugin".to_owned(),
        }
    }

    /// 返回 `<cache>/<marketplace>/<plugin>/<version>`，拒绝目录穿越。
    pub fn versioned_path(&self, id: &PluginId, version: &str) -> Result<PathBuf> {
        let marketplace = id.marketplace.as_deref().ok_or_else(|| {
            ClaudePluginError::Invalid("版本化缓存必须使用 plugin@marketplace".to_owned())
        })?;
        Ok(self
            .cache_root
            .join(safe_cache_component(marketplace, "市场名称")?)
            .join(safe_cache_component(&id.plugin, "插件名称")?)
            .join(safe_cache_component(version, "插件版本")?))
    }

    /// 创建缓存和公开状态所在父目录；不写入任何密钥。
    pub fn ensure_directories(&self) -> Result<()> {
        fs::create_dir_all(&self.cache_root)?;
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    /// 返回平台安全存储使用的、不会泄漏实际值的键名。
    pub fn secret_key(&self, id: &PluginId, field: &str) -> Result<String> {
        let id = id.in_marketplace(id.marketplace.as_deref().ok_or_else(|| {
            ClaudePluginError::Invalid("敏感配置必须使用带市场的插件 ID".to_owned())
        })?)?;
        Ok(format!(
            "{}.{}.{}.{}",
            self.secret_namespace,
            safe_cache_component(id.marketplace.as_deref().unwrap_or_default(), "市场名称")?,
            safe_cache_component(&id.plugin, "插件名称")?,
            safe_cache_component(field, "配置字段")?
        ))
    }
}

/// 已安装插件的公开记录；绝不包含敏感配置值。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    /// 唯一的 `plugin@marketplace` 标识。
    pub id: PluginId,
    /// 已缓存的插件版本。
    pub version: String,
    /// 版本化缓存根目录。
    pub install_path: PathBuf,
    /// 是否参与运行时快照。
    pub enabled: bool,
    /// 用户配置中非敏感字段的 JSON 值。
    #[serde(default)]
    pub public_user_config: BTreeMap<String, Value>,
    /// 已写入安全存储的敏感字段名，不包含值。
    #[serde(default)]
    pub sensitive_user_config_keys: BTreeSet<String>,
}

/// 非敏感状态文件内容。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginState {
    /// 安装插件的公开状态。
    #[serde(default)]
    pub plugins: Vec<InstalledPlugin>,
}

/// 密钥库抽象；extensions 可接 Tauri Stronghold/系统钥匙串，测试可用内存实现。
pub trait SecretStore {
    /// 写入敏感 JSON 值。
    fn set_json(&mut self, key: &str, value: &Value) -> Result<()>;
    /// 读取敏感 JSON 值。
    fn get_json(&self, key: &str) -> Result<Option<Value>>;
    /// 删除敏感 JSON 值。
    fn delete(&mut self, key: &str) -> Result<()>;
}

/// 只用于单元测试和调用方适配测试的内存安全存储，不应替代生产密钥库。
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    /// 进程内测试数据。
    values: BTreeMap<String, Value>,
}

impl SecretStore for InMemorySecretStore {
    /// 写入进程内测试值。
    fn set_json(&mut self, key: &str, value: &Value) -> Result<()> {
        self.values.insert(key.to_owned(), value.clone());
        Ok(())
    }

    /// 读取进程内测试值。
    fn get_json(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.values.get(key).cloned())
    }

    /// 删除进程内测试值。
    fn delete(&mut self, key: &str) -> Result<()> {
        self.values.remove(key);
        Ok(())
    }
}

/// 安装来源与已经验证的插件目录。
#[derive(Clone, Debug)]
pub struct MaterializedPlugin {
    /// 市场清单中的唯一插件 ID。
    pub id: PluginId,
    /// 来源下载、解包或检出后得到的插件根目录。
    pub source_root: PathBuf,
}

/// 对公开状态和安全存储执行的一次用户配置更新。
#[derive(Clone, Debug, Default)]
pub struct UserConfigUpdate {
    /// 传入的新值，敏感字段也在此处出现但不会写入公开状态。
    pub values: BTreeMap<String, Value>,
    /// 是否删除未在本次输入中出现的旧值。
    pub replace: bool,
}

/// 安全安装、状态读写和运行时快照的纯 Rust 服务。
#[derive(Clone, Debug)]
pub struct ClaudePluginManager {
    /// 当前唯一的缓存和状态布局。
    pub storage: PluginStorage,
}

impl ClaudePluginManager {
    /// 根据应用数据根创建服务。
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            storage: PluginStorage::under(data_root),
        }
    }

    /// 从公开 JSON 状态文件读取安装记录；文件不存在时返回空状态。
    pub fn load_state(&self) -> Result<PluginState> {
        match fs::read(&self.storage.state_path) {
            Ok(bytes) => {
                if bytes.len() as u64 > MAX_MANIFEST_BYTES {
                    return Err(ClaudePluginError::Invalid(format!(
                        "插件状态文件超过 {} 字节：{}",
                        MAX_MANIFEST_BYTES,
                        self.storage.state_path.display()
                    )));
                }
                let state: PluginState = serde_json::from_slice(&bytes)?;
                validate_state(&state)?;
                Ok(state)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(PluginState::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// 原子写入公开状态，确保敏感值不进入 state.json。
    pub fn save_state(&self, state: &PluginState) -> Result<()> {
        validate_state(state)?;
        self.storage.ensure_directories()?;
        let bytes = serde_json::to_vec_pretty(state)?;
        write_private_atomic(&self.storage.state_path, &bytes)?;
        Ok(())
    }

    /// 复制验证过的插件目录到版本化缓存，并保存经过类型校验的用户配置。
    pub fn install_from_directory(
        &self,
        materialized: MaterializedPlugin,
        config: UserConfigUpdate,
        secrets: &mut dyn SecretStore,
    ) -> Result<()> {
        let id = require_marketplace_id(&materialized.id)?;
        let source_root = canonical_plugin_root(&materialized.source_root)?;
        let manifest = load_plugin_manifest(&source_root)?;
        if manifest.name != id.plugin {
            return Err(ClaudePluginError::Invalid(format!(
                "市场插件 ID {} 与 plugin.json name {} 不一致",
                id, manifest.name
            )));
        }
        let cache_version = manifest.version.as_deref().unwrap_or("unversioned");
        let destination = self.storage.versioned_path(&id, cache_version)?;
        self.storage.ensure_directories()?;
        if !destination.exists() {
            copy_plugin_tree(&source_root, &destination)?;
        }
        let mut state = self.load_state()?;
        let index = state.plugins.iter().position(|item| item.id == id);
        let previous = index.map(|index| state.plugins.remove(index));
        let (public_user_config, sensitive_user_config_keys) =
            self.apply_user_config(&id, &manifest, previous.as_ref(), config, secrets, false)?;
        // 安装阶段允许先落盘再配置 required userConfig；未完成配置的插件保持禁用，
        // 避免安装命令因为运行时插值缺失而失败，同时让设置页可以补齐配置后启用。
        let enabled = previous.as_ref().is_none_or(|item| item.enabled)
            && has_complete_required_user_config(
                &manifest,
                &public_user_config,
                &sensitive_user_config_keys,
            );
        let installed = InstalledPlugin {
            id: id.clone(),
            version: cache_version.to_owned(),
            install_path: destination,
            enabled,
            public_user_config,
            sensitive_user_config_keys: sensitive_user_config_keys.clone(),
        };
        state.plugins.push(installed.clone());
        state.plugins.sort_by(|left, right| left.id.cmp(&right.id));
        self.save_state(&state)?;
        Ok(())
    }

    /// 写入用户配置时把 sensitive 值交给密钥库，公开状态只记录字段名。
    pub fn update_user_config(
        &self,
        id: &PluginId,
        config: UserConfigUpdate,
        secrets: &mut dyn SecretStore,
    ) -> Result<()> {
        let id = require_marketplace_id(id)?;
        let mut state = self.load_state()?;
        let index = state
            .plugins
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| ClaudePluginError::Invalid(format!("没有安装插件：{id}")))?;
        let previous = state.plugins[index].clone();
        let manifest = load_plugin_manifest(&previous.install_path)?;
        let (public_user_config, sensitive_user_config_keys) =
            self.apply_user_config(&id, &manifest, Some(&previous), config, secrets, true)?;
        let installed = InstalledPlugin {
            public_user_config,
            sensitive_user_config_keys: sensitive_user_config_keys.clone(),
            ..previous
        };
        state.plugins[index] = installed.clone();
        self.save_state(&state)?;
        Ok(())
    }

    /// 删除插件公开状态和其所有敏感字段；缓存保留给调用方按版本回收。
    pub fn uninstall(
        &self,
        id: &PluginId,
        secrets: &mut dyn SecretStore,
    ) -> Result<InstalledPlugin> {
        let id = require_marketplace_id(id)?;
        let mut state = self.load_state()?;
        let index = state
            .plugins
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| ClaudePluginError::Invalid(format!("没有安装插件：{id}")))?;
        let removed = state.plugins.remove(index);
        for key in &removed.sensitive_user_config_keys {
            secrets.delete(&self.storage.secret_key(&id, key)?)?;
        }
        self.save_state(&state)?;
        Ok(removed)
    }

    /// 原子切换插件启用状态；缓存仍保留，后续运行时快照立即可见。
    pub fn set_enabled(&self, id: &PluginId, enabled: bool) -> Result<InstalledPlugin> {
        let id = require_marketplace_id(id)?;
        let mut state = self.load_state()?;
        let item = state
            .plugins
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| ClaudePluginError::Invalid(format!("没有安装插件：{id}")))?;
        item.enabled = enabled;
        let updated = item.clone();
        self.save_state(&state)?;
        Ok(updated)
    }

    /// 为所有启用插件构造可交给 extensions/peri runtime 的只读投影。
    pub fn runtime_snapshot(
        &self,
        project_dir: &Path,
        environment: &BTreeMap<String, String>,
        secrets: &dyn SecretStore,
    ) -> Result<PluginRuntimeSnapshot> {
        let state = self.load_state()?;
        let mut plugins = Vec::new();
        for installed in state.plugins.iter().filter(|item| item.enabled) {
            let manifest = load_plugin_manifest(&installed.install_path)?;
            let config = resolved_user_config(&self.storage, installed, &manifest, secrets)?;
            plugins.push(extract_components(
                installed.id.clone(),
                &installed.install_path,
                &manifest,
                project_dir,
                environment,
                &config,
            )?);
        }
        plugins.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(PluginRuntimeSnapshot {
            plugins,
            plugin_hooks: Vec::new(),
        })
    }

    /// 校验并应用一个用户配置变更，不会将敏感值置入返回的公开 map。
    fn apply_user_config(
        &self,
        id: &PluginId,
        manifest: &PluginManifest,
        previous: Option<&InstalledPlugin>,
        update: UserConfigUpdate,
        secrets: &mut dyn SecretStore,
        require_complete: bool,
    ) -> Result<(BTreeMap<String, Value>, BTreeSet<String>)> {
        let mut public = if update.replace {
            BTreeMap::new()
        } else {
            previous
                .map(|item| item.public_user_config.clone())
                .unwrap_or_default()
        };
        let mut sensitive = if update.replace {
            BTreeSet::new()
        } else {
            previous
                .map(|item| item.sensitive_user_config_keys.clone())
                .unwrap_or_default()
        };
        if update.replace
            && let Some(previous) = previous
        {
            for name in &previous.sensitive_user_config_keys {
                if !update.values.contains_key(name) {
                    secrets.delete(&self.storage.secret_key(id, name)?)?;
                }
            }
        }
        for (name, value) in update.values {
            let definition = manifest.user_config.get(&name).ok_or_else(|| {
                ClaudePluginError::Invalid(format!("插件 {} 没有 userConfig 字段 {name}", id))
            })?;
            validate_user_config_value(&name, definition, &value)?;
            if definition.sensitive {
                secrets.set_json(&self.storage.secret_key(id, &name)?, &value)?;
                public.remove(&name);
                sensitive.insert(name);
            } else {
                public.insert(name.clone(), value);
                if sensitive.remove(&name) {
                    secrets.delete(&self.storage.secret_key(id, &name)?)?;
                }
            }
        }
        if require_complete {
            for (name, definition) in &manifest.user_config {
                if !definition.required {
                    continue;
                }
                let exists = if definition.sensitive {
                    sensitive.contains(name)
                } else {
                    public.contains_key(name)
                };
                if !exists && definition.default.is_none() {
                    return Err(ClaudePluginError::Invalid(format!(
                        "插件 {} 缺少必填 userConfig：{name}",
                        id
                    )));
                }
            }
        }
        Ok((public, sensitive))
    }
}

/// 判断安装记录是否已经具备所有 required userConfig；默认值也算已满足。
fn has_complete_required_user_config(
    manifest: &PluginManifest,
    public: &BTreeMap<String, Value>,
    sensitive: &BTreeSet<String>,
) -> bool {
    manifest.user_config.iter().all(|(name, definition)| {
        !definition.required
            || definition.default.is_some()
            || if definition.sensitive {
                sensitive.contains(name)
            } else {
                public.contains_key(name)
            }
    })
}

/// 供 extensions 和 peri runtime 使用的纯数据快照。
#[derive(Clone, Debug, Default)]
pub struct PluginRuntimeSnapshot {
    /// 按稳定 ID 排序的启用插件投影。
    pub plugins: Vec<RuntimePlugin>,
    /// 已转换为 peri 注册表的插件 Hook。
    pub plugin_hooks: Vec<peri_middlewares::hooks::RegisteredHook>,
}

/// 单个启用插件的运行时投影。
#[derive(Clone, Debug)]
pub struct RuntimePlugin {
    /// 唯一插件 ID。
    pub id: PluginId,
    /// 规范化插件根目录。
    pub root: PathBuf,
    /// 命令 Markdown 文件。
    pub commands: Vec<ComponentFile>,
    /// Skill Markdown 文件。
    pub skills: Vec<ComponentFile>,
    /// Agent Markdown 文件。
    pub agents: Vec<ComponentFile>,
    /// 插件清单 hooks（已插值）。
    pub hooks: Option<Value>,
    /// `hooks` 中声明了但 peri `HookEvent::parse` 无法识别的事件名（如拼写错误或
    /// peri 尚未实现的 Claude Code 事件）；这些事件在运行时会被静默跳过，此字段仅用于向 UI 暴露可见性。
    pub unsupported_hooks: Vec<String>,
    /// `.mcp.json` 与 manifest mcpServers 合并后的配置（已插值）。
    pub mcp_servers: BTreeMap<String, Value>,
    /// manifest lspServers 转换后的 Peri 模板；静态变量已插值，Session 变量延迟绑定。
    pub lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
}

/// 一个可加载的 Claude 命令、Skill 或 Agent 文件。
#[derive(Clone, Debug)]
pub struct ComponentFile {
    /// 文件绝对路径。
    pub path: PathBuf,
    /// 相对插件根目录的可展示路径。
    pub relative_path: PathBuf,
}

/// 将字符串中的 `${NAME}` 替换为变量值；`$$` 不具有特殊含义，未知变量是硬错误。
pub fn interpolate_variables(input: &str, variables: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.find("${") {
        output.push_str(&remainder[..start]);
        let after_start = &remainder[start + 2..];
        let end = after_start
            .find('}')
            .ok_or_else(|| ClaudePluginError::Invalid("变量插值缺少结束大括号".to_owned()))?;
        let name = &after_start[..end];
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(ClaudePluginError::Invalid(format!("变量名无效：{name}")));
        }
        let value = variables
            .get(name)
            .ok_or_else(|| ClaudePluginError::MissingVariable(name.to_owned()))?;
        output.push_str(value);
        remainder = &after_start[end + 1..];
    }
    output.push_str(remainder);
    Ok(output)
}

/// 递归插值 JSON 中的所有字符串键和值，保持其余 JSON 类型不变。
pub fn interpolate_json(value: &Value, variables: &BTreeMap<String, String>) -> Result<Value> {
    match value {
        Value::String(value) => Ok(Value::String(interpolate_variables(value, variables)?)),
        Value::Array(values) => values
            .iter()
            .map(|value| interpolate_json(value, variables))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => {
            let mut result = Map::new();
            for (key, value) in values {
                result.insert(
                    interpolate_variables(key, variables)?,
                    interpolate_json(value, variables)?,
                );
            }
            Ok(Value::Object(result))
        }
        other => Ok(other.clone()),
    }
}

/// 解析一个市场清单字节串，并执行所有本地结构约束。
pub fn parse_marketplace_manifest(bytes: &[u8]) -> Result<MarketplaceManifest> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ClaudePluginError::Invalid(format!(
            "市场清单超过 {} 字节",
            MAX_MANIFEST_BYTES
        )));
    }
    let manifest: MarketplaceManifest = serde_json::from_slice(bytes)?;
    validate_marketplace_manifest(&manifest)?;
    Ok(manifest)
}

/// 解析一个插件清单字节串，并执行所有本地结构约束。
pub fn parse_plugin_manifest(bytes: &[u8]) -> Result<PluginManifest> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ClaudePluginError::Invalid(format!(
            "插件清单超过 {} 字节",
            MAX_MANIFEST_BYTES
        )));
    }
    let manifest: PluginManifest = serde_json::from_slice(bytes)?;
    validate_plugin_manifest(&manifest)?;
    Ok(manifest)
}

/// 从市场根目录读取 `.claude-plugin/marketplace.json`。
pub fn load_marketplace_manifest(root: &Path) -> Result<MarketplaceManifest> {
    parse_marketplace_manifest(&read_limited(&root.join(CLAUDE_MARKETPLACE_MANIFEST))?)
}

/// 从插件根目录读取 `.claude-plugin/plugin.json`。
pub fn load_plugin_manifest(root: &Path) -> Result<PluginManifest> {
    let mut manifest = parse_plugin_manifest(&read_limited(&root.join(CLAUDE_PLUGIN_MANIFEST))?)?;
    merge_mcp_bundle_user_config(root, &mut manifest)?;
    // DXT/MCPB schema 是插件运行时 userConfig 的一部分，合并后重新校验
    // required/default/min/max，确保 UI 与 SecretStore 使用同一份定义。
    validate_plugin_manifest(&manifest)?;
    Ok(manifest)
}

/// 将 marketplace 条目的对象形式 lspServers 转换为 Peri 可加载的合成插件清单。
///
/// Peri 3.6.5 用该路径支持官方市场中只有 LSP 声明、没有原生 plugin.json 的插件。
pub fn synthetic_marketplace_plugin_manifest(
    plugin: &MarketplacePlugin,
) -> Result<Option<PluginManifest>> {
    let Some(value) = synthetic_marketplace_plugin_manifest_value(plugin, false)? else {
        return Ok(None);
    };
    Ok(Some(parse_plugin_manifest(&serde_json::to_vec(&value)?)?))
}

/// 为已在本机展开的无清单插件识别 Claude Code 默认组件目录。
///
/// 官方市场的 `receipts`、`session-report` 等条目没有在 marketplace.json
/// 重复声明 `skills`，也没有 plugin.json，但仍按 Claude Code 约定提供
/// `skills/`。只在真实目录至少包含一个可解析组件时生成清单，避免把空目录
/// 误报为可安装插件。
pub fn synthetic_marketplace_plugin_manifest_for_root(
    plugin: &MarketplacePlugin,
    source_root: &Path,
) -> Result<Option<PluginManifest>> {
    let use_default_components = has_default_component_layout(source_root)?;
    let Some(value) = synthetic_marketplace_plugin_manifest_value(plugin, use_default_components)?
    else {
        return Ok(None);
    };
    Ok(Some(parse_plugin_manifest(&serde_json::to_vec(&value)?)?))
}

/// 复制无原生清单的 marketplace 插件，并在受控缓存副本中写入合成 plugin.json。
pub fn materialize_synthetic_marketplace_plugin(
    source_root: &Path,
    destination: &Path,
    plugin: &MarketplacePlugin,
) -> Result<PathBuf> {
    let source_root = fs::canonicalize(source_root)?;
    if !source_root.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "市场插件来源不是目录：{}",
            source_root.display()
        )));
    }
    let use_default_components = has_default_component_layout(&source_root)?;
    let manifest = synthetic_marketplace_plugin_manifest_value(plugin, use_default_components)?
        .ok_or_else(|| {
            ClaudePluginError::Invalid(format!(
                "市场插件 {} 缺少 .claude-plugin/plugin.json、可识别的默认组件目录、skills 与 lspServers",
                plugin.name
            ))
        })?;
    copy_plugin_tree(&source_root, destination)?;
    let manifest_dir = destination.join(".claude-plugin");
    fs::create_dir_all(&manifest_dir)?;
    fs::write(
        manifest_dir.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    // 写入后再走唯一解析器，确保磁盘内容与内存校验没有分叉。
    load_plugin_manifest(destination)?;
    Ok(fs::canonicalize(destination)?)
}

/// 构造 marketplace 合成清单的原始 JSON。
///
/// Claude Code 官方市场允许 `strict:false` 条目直接声明 `skills`，这类 skill
/// bundle 通常没有 `.claude-plugin/plugin.json`。这里把它们转换为当前运行时能
/// 读取的标准清单；同时保留没有原生清单的 LSP-only 条目行为。
fn synthetic_marketplace_plugin_manifest_value(
    plugin: &MarketplacePlugin,
    use_default_components: bool,
) -> Result<Option<Value>> {
    let skills = plugin
        .extra
        .get("skills")
        .map(|value| -> Result<Option<Vec<String>>> {
            let declaration = serde_json::from_value::<ComponentDeclaration>(value.clone())
                .map_err(|error| {
                    ClaudePluginError::Invalid(format!(
                        "市场插件 {} 的 skills 必须是有效的组件路径声明：{error}",
                        plugin.name
                    ))
                })?;
            if declaration.paths.is_empty() {
                Ok(None)
            } else {
                Ok(Some(declaration.paths))
            }
        })
        .transpose()?
        .flatten();
    let lsp_value = plugin.extra.get("lspServers");
    if skills.is_none() && lsp_value.is_none() && !use_default_components {
        return Ok(None);
    }
    let lsp_servers = if let Some(lsp_value) = lsp_value {
        let lsp_map = lsp_value.as_object().ok_or_else(|| {
            ClaudePluginError::Invalid(format!("市场插件 {} 的 lspServers 必须是对象", plugin.name))
        })?;
        if lsp_map.is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "市场插件 {} 的 lspServers 不能为空",
                plugin.name
            )));
        }
        let mut lsp_servers = Vec::with_capacity(lsp_map.len());
        for (server_name, server_value) in lsp_map {
            let mut server = server_value.as_object().cloned().ok_or_else(|| {
                ClaudePluginError::Invalid(format!(
                    "市场插件 {} 的 LSP Server {server_name} 必须是对象",
                    plugin.name
                ))
            })?;
            if let Some(declared_name) = server.get("name") {
                let declared_name = declared_name.as_str().ok_or_else(|| {
                    ClaudePluginError::Invalid(format!(
                        "市场插件 {} 的 LSP Server {server_name} name 必须是字符串",
                        plugin.name
                    ))
                })?;
                if declared_name != server_name {
                    return Err(ClaudePluginError::Invalid(format!(
                        "市场插件 {} 的 LSP Server 名称与对象键不一致：{server_name}",
                        plugin.name
                    )));
                }
            }
            server.insert("name".to_owned(), Value::String(server_name.clone()));
            lsp_servers.push(Value::Object(server));
        }
        Some(lsp_servers)
    } else {
        None
    };

    let mut manifest = Map::new();
    manifest.insert("name".to_owned(), Value::String(plugin.name.clone()));
    if let Some(version) = &plugin.version {
        manifest.insert("version".to_owned(), Value::String(version.clone()));
    }
    if let Some(description) = &plugin.description {
        manifest.insert("description".to_owned(), Value::String(description.clone()));
    }
    if let Some(mcp_servers) = plugin.extra.get("mcpServers") {
        manifest.insert("mcpServers".to_owned(), mcp_servers.clone());
    }
    if let Some(skills) = skills {
        manifest.insert(
            "skills".to_owned(),
            Value::Array(skills.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(lsp_servers) = lsp_servers {
        manifest.insert("lspServers".to_owned(), Value::Array(lsp_servers));
    }
    let value = Value::Object(manifest);
    // 合成阶段立即使用同一严格解析器验证，禁止把坏清单写入缓存。
    parse_plugin_manifest(&serde_json::to_vec(&value)?)?;
    Ok(Some(value))
}

/// Claude Code 在 plugin.json 省略组件字段时会扫描这三个约定目录。
/// 复用运行时的严格扫描器确认至少存在一个真实组件，并同时拒绝越界路径和
/// 符号链接；只有通过检查的目录才允许生成 name-only 清单。
fn has_default_component_layout(source_root: &Path) -> Result<bool> {
    let root = fs::canonicalize(source_root)?;
    if !root.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "市场插件来源不是目录：{}",
            root.display()
        )));
    }
    for directory in ["commands", "skills", "agents"] {
        if !scan_declared_or_default_components(&root, &[], directory)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 根据市场内所有插件的清单与市场字段构建依赖闭包，返回依赖在前的拓扑顺序。
#[cfg(test)]
pub fn dependency_closure(
    requested: &PluginId,
    marketplace: &MarketplaceManifest,
    manifests: &BTreeMap<String, PluginManifest>,
) -> Result<Vec<PluginId>> {
    let marketplace_name = normalized_identifier(&marketplace.name, "市场名称")?;
    let requested = if let Some(namespace) = &requested.marketplace {
        if namespace != &marketplace_name {
            return Err(ClaudePluginError::Invalid(format!(
                "请求插件市场 {namespace} 与当前市场 {marketplace_name} 不一致"
            )));
        }
        requested.clone()
    } else {
        requested.in_marketplace(&marketplace_name)?
    };
    let market_plugins = marketplace
        .plugins
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect::<HashMap<_, _>>();
    if !market_plugins.contains_key(requested.plugin.as_str()) {
        return Err(ClaudePluginError::Invalid(format!(
            "市场 {marketplace_name} 中不存在插件 {}",
            requested.plugin
        )));
    }
    let mut result = Vec::new();
    let mut visiting = Vec::new();
    let mut complete = BTreeSet::new();
    visit_dependency(
        &requested,
        &marketplace_name,
        &market_plugins,
        manifests,
        &mut visiting,
        &mut complete,
        &mut result,
    )?;
    Ok(result)
}

/// 扫描插件目录并抽取 Claude commands/skills/agents/hooks/MCP 组件。
pub fn extract_components(
    id: PluginId,
    root: &Path,
    manifest: &PluginManifest,
    project_dir: &Path,
    environment: &BTreeMap<String, String>,
    config: &ResolvedUserConfig,
) -> Result<RuntimePlugin> {
    let root = canonical_plugin_root(root)?;
    let mut variables = environment.clone();
    variables.insert(
        "CLAUDE_PLUGIN_ROOT".to_owned(),
        root.to_string_lossy().into_owned(),
    );
    variables.insert(
        "CLAUDE_PLUGIN_DATA".to_owned(),
        root.join(".data").to_string_lossy().into_owned(),
    );
    variables.insert(
        "CLAUDE_SKILL_DIR".to_owned(),
        root.join("skills").to_string_lossy().into_owned(),
    );
    variables
        .entry("CLAUDE_SESSION_ID".to_owned())
        .or_insert_with(|| {
            environment
                .get("CLAUDE_SESSION_ID")
                .cloned()
                .unwrap_or_default()
        });
    variables.insert(
        "CLAUDE_PROJECT_DIR".to_owned(),
        project_dir.to_string_lossy().into_owned(),
    );
    variables.insert("CLAUDE_PLUGIN_ID".to_owned(), id.to_string());
    variables.insert("CLAUDE_PLUGIN_NAME".to_owned(), id.plugin.clone());
    if let Some(marketplace) = &id.marketplace {
        variables.insert("CLAUDE_PLUGIN_MARKETPLACE".to_owned(), marketplace.clone());
    }
    if let Some(version) = &manifest.version {
        variables.insert("CLAUDE_PLUGIN_VERSION".to_owned(), version.clone());
    }
    for (name, value) in &config.values {
        if let Some(value) = config_value_as_variable(value) {
            variables.insert(
                format!("CLAUDE_PLUGIN_{}", normalize_variable_name(name)),
                value.clone(),
            );
            variables.insert(format!("user_config.{name}"), value);
        }
    }
    let commands =
        scan_declared_or_default_components(&root, &manifest.commands.paths, "commands")?;
    let skills = scan_declared_or_default_components(&root, &manifest.skills.paths, "skills")?;
    let agents = scan_declared_or_default_components(&root, &manifest.agents.paths, "agents")?;
    let hooks = load_hooks(&root, manifest.hooks.as_ref(), &variables)?;
    let unsupported_hooks = unsupported_hook_events(hooks.as_ref());
    let mut mcp_servers = BTreeMap::new();
    if let Some(file_servers) = load_mcp_file(&root)? {
        mcp_servers.extend(file_servers);
    }
    for file in &manifest.mcp_servers.files {
        mcp_servers.extend(load_mcp_servers_file(&root, file)?);
    }
    mcp_servers.extend(manifest.mcp_servers.inline.clone());
    let mcp_servers = mcp_servers
        .into_iter()
        .map(|(name, value)| {
            let value = interpolate_json(&value, &variables)?;
            Ok((name, normalize_mcp_server_value(value)?))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut lsp_variables = variables.clone();
    // cwd 与 Session ID 只能在 Peri 创建具体 Session 时确定；加载期只展开
    // 插件根、用户配置等静态变量，避免所有 Session 被启动目录或旧环境值污染。
    lsp_variables.insert(
        "CLAUDE_PROJECT_DIR".to_owned(),
        "${CLAUDE_PROJECT_DIR}".to_owned(),
    );
    lsp_variables.insert(
        "CLAUDE_SESSION_ID".to_owned(),
        "${CLAUDE_SESSION_ID}".to_owned(),
    );
    let lsp_servers = manifest
        .lsp_servers
        .iter()
        .map(|server| {
            let command = interpolate_variables(&server.command, &lsp_variables)?;
            let args = server
                .args
                .iter()
                .map(|argument| interpolate_variables(argument, &lsp_variables))
                .collect::<Result<Vec<_>>>()?;
            let environment = server
                .env
                .iter()
                .map(|(name, value)| {
                    Ok((name.clone(), interpolate_variables(value, &lsp_variables)?))
                })
                .collect::<Result<Vec<_>>>()?;
            let initialization_options = server
                .initialization_options
                .as_ref()
                .map(|value| interpolate_json(value, &lsp_variables))
                .transpose()?;
            let mut config = peri_resources::lsp::config::lsp_config_from_plugin(
                &id.plugin,
                &server.name,
                &command,
                &args,
                &root,
                server.extension_to_language.clone().into_iter().collect(),
            );
            let config_environment = config.env.get_or_insert_default();
            config_environment.extend(environment);
            // 插件不能用清单字段伪造安装根；该保留变量始终由宿主注入真实路径。
            config_environment.insert(
                "CLAUDE_PLUGIN_ROOT".to_owned(),
                root.to_string_lossy().into_owned(),
            );
            config.initialization_options = initialization_options;
            config.disabled = server.disabled;
            config.max_restarts = server.max_restarts;
            config.startup_timeout = server.startup_timeout;
            Ok(config)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RuntimePlugin {
        id,
        root,
        commands,
        skills,
        agents,
        hooks,
        unsupported_hooks,
        mcp_servers,
        lsp_servers,
    })
}

/// 提取 `hooks` 声明中 peri `HookEvent::parse` 无法识别的事件名，按原始顺序去重。
fn unsupported_hook_events(hooks: Option<&Value>) -> Vec<String> {
    let Some(Value::Object(events)) = hooks else {
        return Vec::new();
    };
    events
        .keys()
        .filter(|event_name| peri_middlewares::hooks::HookEvent::parse(event_name).is_none())
        .cloned()
        .collect()
}

/// 已解析的用户配置；只在进程内向变量插值提供敏感值，调用方不得序列化或记录它。
#[derive(Clone, Debug, Default)]
pub struct ResolvedUserConfig {
    /// 类型校验后的配置值，可能包含来自安全存储的敏感值。
    pub values: BTreeMap<String, Value>,
    /// 密钥库没有返回的敏感必填字段。
    pub missing_sensitive: BTreeSet<String>,
}

/// 读取公开配置与安全存储，填充默认值并再次校验值类型。
pub fn resolved_user_config(
    storage: &PluginStorage,
    installed: &InstalledPlugin,
    manifest: &PluginManifest,
    secrets: &dyn SecretStore,
) -> Result<ResolvedUserConfig> {
    let mut result = ResolvedUserConfig::default();
    for (name, definition) in &manifest.user_config {
        let value = if definition.sensitive {
            secrets.get_json(&storage.secret_key(&installed.id, name)?)?
        } else {
            installed.public_user_config.get(name).cloned()
        }
        .or_else(|| definition.default.clone());
        match value {
            Some(value) => {
                validate_user_config_value(name, definition, &value)?;
                result.values.insert(name.clone(), value);
            }
            None if definition.sensitive && definition.required => {
                result.missing_sensitive.insert(name.clone());
            }
            None if definition.required => {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件 {} 缺少必填 userConfig：{name}",
                    installed.id
                )));
            }
            None => {}
        }
    }
    Ok(result)
}

/// 验证市场清单的唯一命名空间、条目名称和来源。
fn validate_marketplace_manifest(manifest: &MarketplaceManifest) -> Result<()> {
    normalized_identifier(&manifest.name, "市场名称")?;
    validate_marketplace_name(&manifest.name)?;
    let mut names = BTreeSet::new();
    for plugin in &manifest.plugins {
        let name = normalized_identifier(&plugin.name, "市场插件名称")?;
        if !names.insert(name.clone()) {
            return Err(ClaudePluginError::Invalid(format!(
                "市场插件名称重复：{name}"
            )));
        }
        validate_plugin_source(&plugin.source)?;
        validate_dependency_names(&plugin.dependencies)?;
    }
    Ok(())
}

/// 校验 Claude 保留市场名称，阻止第三方伪装成官方 Anthropic 市场。
pub fn validate_marketplace_name(name: &str) -> Result<()> {
    let normalized = normalized_identifier(name, "市场名称")?;
    let lower = normalized.to_ascii_lowercase();
    if matches!(lower.as_str(), "builtin" | "inline") {
        return Err(ClaudePluginError::Invalid(format!(
            "市场名称 {name} 是 Claude 保留命名空间"
        )));
    }
    let official = matches!(
        lower.as_str(),
        "claude-code-marketplace"
            | "claude-code-plugins"
            | "claude-plugins-official"
            | "anthropic-marketplace"
            | "anthropic-plugins"
            | "agent-skills"
            | "life-sciences"
            | "knowledge-work-plugins"
    );
    let impersonation = lower.contains("official")
        && (lower.contains("claude") || lower.contains("anthropic"))
        || ((lower.starts_with("claude") || lower.starts_with("anthropic"))
            && (lower.contains("marketplace") || lower.contains("plugins")));
    if impersonation && !official {
        return Err(ClaudePluginError::Invalid(format!(
            "市场名称 {name} 可能冒充 Claude/Anthropic 官方市场"
        )));
    }
    Ok(())
}

/// 校验保留官方名称只能与 Anthropic 官方 GitHub 来源绑定。
pub fn validate_marketplace_name_source(name: &str, source: &str) -> Result<()> {
    validate_marketplace_name(name)?;
    let lower = name.to_ascii_lowercase();
    let official = matches!(
        lower.as_str(),
        "claude-code-marketplace"
            | "claude-code-plugins"
            | "claude-plugins-official"
            | "anthropic-marketplace"
            | "anthropic-plugins"
            | "agent-skills"
            | "life-sciences"
            | "knowledge-work-plugins"
    );
    if official {
        if !is_anthropic_github_source(source) {
            return Err(ClaudePluginError::Invalid(format!(
                "官方保留市场 {name} 只能来自 github.com/anthropics"
            )));
        }
    }
    Ok(())
}

/// 严格解析官方市场来源，禁止仅凭字符串包含关系绕过 Anthropic owner 校验。
fn is_anthropic_github_source(source: &str) -> bool {
    let source = source.trim();
    if let Some(repository) = source.strip_prefix("github:") {
        return is_anthropic_repository(repository);
    }
    if let Some(repository) = source.strip_prefix("git@github.com:") {
        return is_anthropic_repository(repository);
    }
    let source = source.strip_prefix("git:").unwrap_or(source);
    let Ok(url) = url::Url::parse(source) else {
        return false;
    };
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("github.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(mut segments) = url.path_segments() else {
        return false;
    };
    let (Some(owner), Some(repository), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    owner.eq_ignore_ascii_case("anthropics") && is_repository_component(repository)
}

/// 校验 `github:owner/repo[@ref]` 与 SSH `owner/repo` 的 owner/repo 结构。
fn is_anthropic_repository(value: &str) -> bool {
    let value = match value.rsplit_once('@') {
        Some((repository, reference)) if !reference.is_empty() => repository,
        Some(_) => return false,
        None => value,
    };
    let mut segments = value.split('/');
    let (Some(owner), Some(repository), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    owner.eq_ignore_ascii_case("anthropics") && is_repository_component(repository)
}

/// GitHub repository 名称不能借助路径、控制字符或空白伪造 owner/repo。
fn is_repository_component(value: &str) -> bool {
    let value = value.strip_suffix(".git").unwrap_or(value);
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 验证插件清单的版本、路径、配置和依赖。
fn validate_plugin_manifest(manifest: &PluginManifest) -> Result<()> {
    normalized_identifier(&manifest.name, "插件名称")?;
    if let Some(version) = &manifest.version {
        non_empty(version, "插件版本")?;
    }
    validate_component_paths(&manifest.commands.paths, "commands")?;
    validate_component_paths(&manifest.skills.paths, "skills")?;
    validate_component_paths(&manifest.agents.paths, "agents")?;
    validate_dependency_names(&manifest.dependencies)?;
    let mut lsp_names = BTreeSet::new();
    for server in &manifest.lsp_servers {
        let name = normalized_identifier(&server.name, "LSP Server 名称")?;
        if name != server.name {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server 名称不能包含首尾空白：{}",
                server.name
            )));
        }
        if !lsp_names.insert(name.clone()) {
            return Err(ClaudePluginError::Invalid(format!(
                "lspServers 包含重复名称：{name}"
            )));
        }
        if server.command.trim().is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server {name} 的 command 不能为空"
            )));
        }
        if server
            .command
            .chars()
            .any(|character| matches!(character, '\0' | '\r' | '\n'))
        {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server {name} 的 command 包含控制字符"
            )));
        }
        if server.args.iter().any(|argument| argument.contains('\0')) {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server {name} 的 args 包含空字符"
            )));
        }
        for (extension, language) in &server.extension_to_language {
            if extension.trim().is_empty() || language.trim().is_empty() {
                return Err(ClaudePluginError::Invalid(format!(
                    "LSP Server {name} 的 extensionToLanguage 不能为空"
                )));
            }
        }
    }
    for (name, definition) in &manifest.user_config {
        normalized_identifier(name, "userConfig 字段")?;
        if definition.min.is_some_and(|value| !value.is_finite())
            || definition.max.is_some_and(|value| !value.is_finite())
            || matches!((definition.min, definition.max), (Some(min), Some(max)) if min > max)
        {
            return Err(ClaudePluginError::Invalid(format!(
                "userConfig {name} 的 min/max 无效"
            )));
        }
        if definition.value_type == UserConfigType::Select && definition.enum_values.is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "select userConfig 必须声明 enum：{name}"
            )));
        }
        if let Some(default) = &definition.default {
            validate_user_config_value(name, definition, default)?;
        }
    }
    Ok(())
}

/// 仅校验插件来源自身，不在解析清单时访问 settings 或网络。
fn validate_plugin_source(source: &PluginSource) -> Result<()> {
    match source {
        PluginSource::Relative { path } => {
            safe_relative_path(path, "插件相对路径")?;
        }
        PluginSource::Npm {
            package,
            version,
            registry,
        } => {
            package_spec(package, version.as_deref())?;
            validate_optional_registry(registry.as_deref())?;
        }
        PluginSource::Pip {
            package,
            version,
            registry,
        } => {
            pip_package_spec(package, version.as_deref())?;
            validate_optional_registry(registry.as_deref())?;
        }
        PluginSource::Url {
            url,
            reference,
            sha,
        } => {
            non_empty(url, "插件 Git URL")?;
            if let Some(reference) = reference {
                non_empty(reference, "插件 Git ref")?;
            }
            validate_git_sha(sha.as_deref())?;
        }
        PluginSource::Github { repo, sha, .. } => {
            github_git_url(repo)?;
            validate_git_sha(sha.as_deref())?;
        }
        PluginSource::GitSubdir { url, path, sha, .. } => {
            non_empty(url, "Git URL")?;
            safe_relative_path(path, "Git 子目录")?;
            validate_git_sha(sha.as_deref())?;
        }
    }
    Ok(())
}

/// 校验依赖键都可以解析为插件 ID。
fn validate_dependency_names(dependencies: &BTreeMap<String, VersionRequirement>) -> Result<()> {
    for (id, requirement) in dependencies {
        PluginId::parse(id)?;
        if requirement.0.trim().is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "依赖版本要求不能为空：{id}"
            )));
        }
    }
    Ok(())
}

/// 递归构建依赖顺序并检测当前位置栈中的循环。
#[cfg(test)]
fn visit_dependency(
    id: &PluginId,
    marketplace: &str,
    market_plugins: &HashMap<&str, &MarketplacePlugin>,
    manifests: &BTreeMap<String, PluginManifest>,
    visiting: &mut Vec<PluginId>,
    complete: &mut BTreeSet<PluginId>,
    result: &mut Vec<PluginId>,
) -> Result<()> {
    if complete.contains(id) {
        return Ok(());
    }
    if let Some(start) = visiting.iter().position(|current| current == id) {
        let mut cycle = visiting[start..].to_vec();
        cycle.push(id.clone());
        return Err(ClaudePluginError::DependencyCycle(cycle));
    }
    let entry = market_plugins.get(id.plugin.as_str()).ok_or_else(|| {
        ClaudePluginError::Invalid(format!("市场 {marketplace} 中找不到依赖 {id}"))
    })?;
    let manifest = manifests.get(&id.plugin).ok_or_else(|| {
        ClaudePluginError::Invalid(format!("没有已解析的插件清单，无法计算依赖：{id}"))
    })?;
    visiting.push(id.clone());
    let mut dependencies = entry.dependencies.clone();
    dependencies.extend(manifest.dependencies.clone());
    for dependency in dependencies.keys() {
        let parsed = PluginId::parse(dependency)?;
        let dependency = match parsed.marketplace.as_deref() {
            None => parsed.in_marketplace(marketplace)?,
            Some(namespace) if namespace == marketplace => parsed,
            Some(namespace) => {
                return Err(ClaudePluginError::Invalid(format!(
                    "跨市场依赖 {dependency}@{namespace} 需要由上层市场解析器提供"
                )));
            }
        };
        visit_dependency(
            &dependency,
            marketplace,
            market_plugins,
            manifests,
            visiting,
            complete,
            result,
        )?;
    }
    visiting.pop();
    complete.insert(id.clone());
    result.push(id.clone());
    Ok(())
}

/// 读取并限制清单文件大小。
fn read_limited(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(ClaudePluginError::Invalid(format!(
            "不允许读取符号链接清单：{}",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "不是普通文件：{}",
            path.display()
        )));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ClaudePluginError::Invalid(format!(
            "文件超过 {} 字节：{}",
            MAX_MANIFEST_BYTES,
            path.display()
        )));
    }
    Ok(fs::read(path)?)
}

/// 在同一目录创建唯一临时文件、限制为用户私有权限并原子替换公开状态文件。
fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        ClaudePluginError::Invalid(format!("状态文件缺少父目录：{}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ClaudePluginError::Invalid(format!("系统时间无效：{error}")))?
        .as_nanos();
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id(),
        nonce
    ));
    write_new_private_file(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    set_private_permissions(path)?;
    Ok(())
}

/// Unix 上以 0600 直接创建临时文件，避免写入和收紧权限之间的泄漏窗口。
#[cfg(unix)]
fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Windows 由应用数据目录 ACL 约束；使用 create_new 避免覆盖任何现有临时文件。
#[cfg(not(unix))]
fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;

    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Unix 平台把状态限制为当前用户可读写；Windows ACL 由应用数据目录和用户令牌约束。
#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Windows 没有 std 提供的跨版本 ACL 设置接口，保留 Tauri 应用数据目录的用户边界。
#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// 解析市场 source 的完整 JSON 表示。
fn parse_marketplace_source(value: Value) -> Result<MarketplaceSource> {
    if let Value::String(value) = value {
        return Ok(MarketplaceSource::Directory { path: value });
    }
    let object = value.as_object().ok_or_else(|| {
        ClaudePluginError::Invalid("市场 source 必须是路径字符串或对象".to_owned())
    })?;
    let source = object_string(object, "source")?;
    match source.as_str() {
        "url" => Ok(MarketplaceSource::Url {
            url: object_string(object, "url")?,
            headers: optional_string_map(object, "headers")?,
        }),
        "github" => Ok(MarketplaceSource::Github {
            repo: object_string(object, "repo")?,
            reference: optional_string(object, "ref")?,
            path: optional_string(object, "path")?,
            sparse_paths: optional_string_array(object, "sparsePaths")?,
        }),
        "git" => Ok(MarketplaceSource::Git {
            url: object_string(object, "url")?,
            reference: optional_string(object, "ref")?,
            path: optional_string(object, "path")?,
            sparse_paths: optional_string_array(object, "sparsePaths")?,
        }),
        "npm" => Ok(MarketplaceSource::Npm {
            package: object_string(object, "package")?,
            version: optional_string(object, "version")?,
            registry: optional_string(object, "registry")?,
        }),
        "file" => Ok(MarketplaceSource::File {
            path: object_string(object, "path")?,
        }),
        "directory" => Ok(MarketplaceSource::Directory {
            path: object_string(object, "path")?,
        }),
        "settings" => Ok(MarketplaceSource::Settings {
            key: object
                .get("key")
                .or_else(|| object.get("path"))
                .and_then(Value::as_str)
                .ok_or_else(|| ClaudePluginError::Invalid("settings 市场来源缺少 key".to_owned()))?
                .to_owned(),
        }),
        other => Err(ClaudePluginError::Invalid(format!(
            "不支持的市场 source：{other}"
        ))),
    }
}

/// 解析插件 source 的完整 JSON 表示。
fn parse_plugin_source(value: Value) -> Result<PluginSource> {
    if let Value::String(value) = value {
        return Ok(PluginSource::Relative { path: value });
    }
    let object = value.as_object().ok_or_else(|| {
        ClaudePluginError::Invalid("插件 source 必须是相对路径字符串或对象".to_owned())
    })?;
    let source = object_string(object, "source")?;
    match source.as_str() {
        "npm" => Ok(PluginSource::Npm {
            package: object_string(object, "package")?,
            version: optional_string(object, "version")?,
            registry: optional_string(object, "registry")?,
        }),
        "url" => Ok(PluginSource::Url {
            url: object_string(object, "url")?,
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
        }),
        "github" => Ok(PluginSource::Github {
            repo: object_string(object, "repo")?,
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
        }),
        "git-subdir" => Ok(PluginSource::GitSubdir {
            url: object_string(object, "url")?,
            path: object_string(object, "path")?,
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
        }),
        "pip" => Ok(PluginSource::Pip {
            package: object_string(object, "package")?,
            version: optional_string(object, "version")?,
            registry: optional_string(object, "registry")?,
        }),
        other => Err(ClaudePluginError::Invalid(format!(
            "不支持的插件 source：{other}"
        ))),
    }
}

/// 解析清单 `mcpServers` 的内联对象、数组和相对 JSON 文件形式。
fn parse_mcp_servers_declaration(value: Value) -> Result<McpServersDeclaration> {
    match value {
        Value::Null => Ok(McpServersDeclaration::default()),
        Value::String(file) => {
            validate_mcp_reference(&file)?;
            Ok(McpServersDeclaration {
                inline: BTreeMap::new(),
                files: vec![file],
            })
        }
        Value::Array(values) => {
            let mut declaration = McpServersDeclaration::default();
            let mut inline = Vec::new();
            for value in values {
                match value {
                    Value::String(file) => {
                        validate_mcp_reference(&file)?;
                        declaration.files.push(file);
                    }
                    value => inline.push(value),
                }
            }
            if !inline.is_empty() {
                declaration.inline = parse_mcp_servers_entries(Value::Array(inline))?;
            }
            Ok(declaration)
        }
        Value::Object(mut object) => {
            let mut declaration = McpServersDeclaration::default();
            if let Some(files) = object.remove("file").or_else(|| object.remove("files")) {
                let files = match files {
                    Value::String(file) => vec![file],
                    Value::Array(values) => values
                        .into_iter()
                        .map(|value| {
                            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                                ClaudePluginError::Invalid(
                                    "mcpServers files 必须是字符串数组".to_owned(),
                                )
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                    _ => {
                        return Err(ClaudePluginError::Invalid(
                            "mcpServers file 必须是字符串或字符串数组".to_owned(),
                        ));
                    }
                };
                for file in files {
                    validate_mcp_reference(&file)?;
                    declaration.files.push(file);
                }
            }
            if let Some(servers) = object
                .remove("servers")
                .or_else(|| object.remove("mcpServers"))
            {
                declaration.inline = parse_mcp_servers_entries(servers)?;
            }
            if !object.is_empty() {
                for (name, value) in parse_mcp_servers_entries(Value::Object(object))? {
                    if declaration.inline.insert(name.clone(), value).is_some() {
                        return Err(ClaudePluginError::Invalid(format!(
                            "mcpServers 混合声明包含重复 Server：{name}"
                        )));
                    }
                }
            }
            Ok(declaration)
        }
        _ => Err(ClaudePluginError::Invalid(
            "mcpServers 必须是对象、数组或相对文件路径".to_owned(),
        )),
    }
}

/// 校验 MCP JSON 或 MCPB/DXT 引用，并拒绝任意路径与不安全 URL。
fn validate_mcp_reference(file: &str) -> Result<()> {
    if file.starts_with("http://") || file.starts_with("https://") {
        let parsed = url::Url::parse(file)
            .map_err(|_| ClaudePluginError::Invalid("MCPB/DXT URL 格式无效".to_owned()))?;
        let extension = parsed
            .path()
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(parsed.scheme(), "http" | "https")
            || !matches!(extension.as_str(), "mcpb" | "dxt")
        {
            return Err(ClaudePluginError::Invalid(
                "远程 MCPB/DXT 必须是 http(s) URL 且以 .mcpb 或 .dxt 结尾".to_owned(),
            ));
        }
        return Ok(());
    }
    safe_relative_path(file, "mcpServers 文件")?;
    Ok(())
}

/// 从 JSON 对象取必填字符串字段。
fn object_string(object: &Map<String, Value>, key: &str) -> Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ClaudePluginError::Invalid(format!("source 缺少非空 string {key}")))
}

/// 从 JSON 对象取可选字符串字段。
fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(ClaudePluginError::Invalid(format!(
            "source {key} 必须是非空字符串"
        ))),
    }
}

/// 从 JSON 对象取可选的字符串数组字段，并拒绝空项、非字符串和路径穿越项。
fn optional_string_array(object: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let Value::Array(values) = value else {
        return Err(ClaudePluginError::Invalid(format!(
            "source {key} 必须是字符串数组"
        )));
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ClaudePluginError::Invalid(format!("source {key} 必须是非空字符串数组"))
                })
        })
        .collect()
}

/// 从 JSON 对象取可选的 HTTP 头映射，避免把非字符串值传给网络层。
fn optional_string_map(object: &Map<String, Value>, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = object.get(key) else {
        return Ok(BTreeMap::new());
    };
    let Value::Object(values) = value else {
        return Err(ClaudePluginError::Invalid(format!(
            "source {key} 必须是字符串对象"
        )));
    };
    let mut output = BTreeMap::new();
    for (name, value) in values {
        let value = value.as_str().ok_or_else(|| {
            ClaudePluginError::Invalid(format!("source {key}.{name} 必须是字符串"))
        })?;
        if name.trim().is_empty() || value.trim().is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "source {key} 的名称和值不能为空"
            )));
        }
        output.insert(name.clone(), value.to_owned());
    }
    Ok(output)
}

/// 将 dependencies 的对象、字符串数组或对象数组归一为 map。
fn deserialize_dependencies<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, VersionRequirement>, D::Error>
where
    D: Deserializer<'de>,
{
    let value =
        Option::<Value>::deserialize(deserializer)?.unwrap_or_else(|| Value::Object(Map::new()));
    let mut output = BTreeMap::new();
    match value {
        Value::Object(values) => {
            for (name, version) in values {
                let version = match version {
                    Value::String(version) => version,
                    Value::Object(object) => object
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("*")
                        .to_owned(),
                    Value::Null => "*".to_owned(),
                    _ => {
                        return Err(serde::de::Error::custom(
                            "dependencies 值必须为字符串或对象",
                        ));
                    }
                };
                output.insert(name, VersionRequirement(version));
            }
        }
        Value::Array(values) => {
            for value in values {
                match value {
                    Value::String(name) => {
                        output.insert(name, VersionRequirement("*".to_owned()));
                    }
                    Value::Object(object) => {
                        let name = object
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| serde::de::Error::custom("依赖对象缺少 name"))?;
                        let version = object.get("version").and_then(Value::as_str).unwrap_or("*");
                        output.insert(name.to_owned(), VersionRequirement(version.to_owned()));
                    }
                    _ => {
                        return Err(serde::de::Error::custom(
                            "dependencies 数组项必须为字符串或对象",
                        ));
                    }
                }
            }
        }
        Value::Null => {}
        _ => return Err(serde::de::Error::custom("dependencies 必须为对象或数组")),
    }
    Ok(output)
}

/// 标准化且校验一个公开标识符。
fn normalized_identifier(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ClaudePluginError::Invalid(format!("{label} 无效：{value}")));
    }
    Ok(value.to_owned())
}

/// 验证非空文本但不修改其展示内容。
fn non_empty<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    if value.trim().is_empty() {
        return Err(ClaudePluginError::Invalid(format!("{label} 不能为空")));
    }
    Ok(value)
}

/// 验证 HTTP(S) URL，防止把未知 scheme 交给网络层。
fn validated_http_url(value: &str, label: &str) -> Result<String> {
    let value = non_empty(value, label)?;
    if !(value.starts_with("https://") || value.starts_with("http://")) {
        return Err(ClaudePluginError::Invalid(format!(
            "{label} 只允许 http 或 https：{value}"
        )));
    }
    Ok(value.to_owned())
}

/// 校验可选 npm/PyPI registry，只允许明确的 HTTP(S) 端点。
fn validate_optional_registry(registry: Option<&str>) -> Result<Option<String>> {
    registry
        .map(|value| validated_http_url(value, "registry URL"))
        .transpose()
}

/// 将 GitHub owner/repo 转换为 HTTPS Git 地址。
fn github_git_url(repo: &str) -> Result<String> {
    let repo = non_empty(repo, "GitHub repo")?;
    let mut parts = repo.split('/');
    let owner = parts.next();
    let name = parts.next();
    if owner.is_none() || name.is_none() || parts.next().is_some() {
        return Err(ClaudePluginError::Invalid(format!(
            "GitHub repo 必须为 owner/repo：{repo}"
        )));
    }
    Ok(format!("https://github.com/{repo}.git"))
}

/// 校验 Claude marketplace/plugin source 使用的固定 40 位提交 SHA。
fn validate_git_sha(sha: Option<&str>) -> Result<()> {
    let Some(sha) = sha else {
        return Ok(());
    };
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ClaudePluginError::Invalid(
            "Git source sha 必须是 40 位十六进制提交标识".to_owned(),
        ));
    }
    Ok(())
}

/// 合成 npm 包规范，保留 scope 包名中的 @。
fn package_spec(package: &str, version: Option<&str>) -> Result<String> {
    let package = non_empty(package, "npm/pip 包名")?;
    if package.contains(char::is_whitespace) {
        return Err(ClaudePluginError::Invalid(format!(
            "包名不能包含空白：{package}"
        )));
    }
    match version {
        Some(version) => Ok(format!("{package}@{}", non_empty(version, "包版本")?)),
        None => Ok(package.to_owned()),
    }
}

/// 构造 pip 的参数化包规范；pip 使用 `==` 锁定版本，不能复用 npm 的 `@version` 语法。
fn pip_package_spec(package: &str, version: Option<&str>) -> Result<String> {
    let package = non_empty(package, "pip 包名")?;
    if package.contains(char::is_whitespace)
        || package.contains(';')
        || package.contains('\n')
        || package.contains('\r')
    {
        return Err(ClaudePluginError::Invalid(format!(
            "pip 包名不能包含空白、环境标记或换行：{package}"
        )));
    }
    match version {
        Some(version) => {
            let version = non_empty(version, "pip 包版本")?;
            if version.contains(char::is_whitespace)
                || version.contains(';')
                || version.contains('\n')
                || version.contains('\r')
            {
                return Err(ClaudePluginError::Invalid(
                    "pip 包版本不能包含空白、环境标记或换行".to_owned(),
                ));
            }
            Ok(format!("{package}=={version}"))
        }
        None => Ok(package.to_owned()),
    }
}

/// 返回安全的相对路径；允许 Claude 市场惯用的 `./plugin`，拒绝 `..` 和绝对路径。
fn safe_relative_path(value: &str, label: &str) -> Result<PathBuf> {
    let value = non_empty(value, label)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(ClaudePluginError::Invalid(format!(
            "{label} 必须是安全相对路径：{value}"
        )));
    }
    Ok(path.to_path_buf())
}

/// 将安全相对路径拼接到已授权根目录并确认不会跳出该根目录。
fn safe_relative_join(root: &Path, value: &str, label: &str) -> Result<PathBuf> {
    let relative = safe_relative_path(value, label)?;
    let root = fs::canonicalize(root)?;
    let joined = root.join(relative);
    if joined.exists() {
        let canonical = fs::canonicalize(&joined)?;
        if !canonical.starts_with(&root) {
            return Err(ClaudePluginError::Invalid(format!(
                "{label} 越出市场根目录：{value}"
            )));
        }
        Ok(canonical)
    } else {
        Ok(joined)
    }
}

/// 验证组件声明的每个路径都安全且不重复。
fn validate_component_paths(paths: &[String], label: &str) -> Result<()> {
    let mut unique = BTreeSet::new();
    for path in paths {
        let path = safe_relative_path(path, label)?;
        if !unique.insert(path) {
            return Err(ClaudePluginError::Invalid(format!("{label} 包含重复路径")));
        }
    }
    Ok(())
}

/// 检查 userConfig 值是否符合其声明类型和 enum 约束。
fn validate_user_config_value(
    name: &str,
    definition: &UserConfigDefinition,
    value: &Value,
) -> Result<()> {
    let values = if definition.multiple {
        value.as_array().ok_or_else(|| {
            ClaudePluginError::Invalid(format!("userConfig {name} 启用 multiple 时值必须为数组"))
        })?
    } else {
        std::slice::from_ref(value)
    };
    for value in values {
        let valid_type = match definition.value_type {
            UserConfigType::String | UserConfigType::Directory | UserConfigType::File => {
                value.is_string()
            }
            UserConfigType::Number => value.is_number(),
            UserConfigType::Boolean => value.is_boolean(),
            UserConfigType::Select => definition.enum_values.contains(value),
        };
        if !valid_type {
            return Err(ClaudePluginError::Invalid(format!(
                "userConfig {name} 值不符合 {:?} 类型或 enum 约束",
                definition.value_type
            )));
        }
        validate_user_config_bounds(name, definition, value)?;
    }
    Ok(())
}

/// 校验 number 数值范围和 string/file/directory 的长度范围。
fn validate_user_config_bounds(
    name: &str,
    definition: &UserConfigDefinition,
    value: &Value,
) -> Result<()> {
    let measured = match definition.value_type {
        UserConfigType::Number => value.as_f64(),
        UserConfigType::String | UserConfigType::Directory | UserConfigType::File => {
            value.as_str().map(|value| value.chars().count() as f64)
        }
        UserConfigType::Boolean | UserConfigType::Select => None,
    };
    if let Some(measured) = measured
        && (definition.min.is_some_and(|minimum| measured < minimum)
            || definition.max.is_some_and(|maximum| measured > maximum))
    {
        return Err(ClaudePluginError::Invalid(format!(
            "userConfig {name} 超出 min/max 约束"
        )));
    }
    Ok(())
}

/// 规范化并确认一个插件根目录确实存在插件清单。
fn canonical_plugin_root(root: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    if !root.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件根目录不是目录：{}",
            root.display()
        )));
    }
    let manifest = root.join(CLAUDE_PLUGIN_MANIFEST);
    if !manifest.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件根目录缺少 {}：{}",
            CLAUDE_PLUGIN_MANIFEST,
            root.display()
        )));
    }
    Ok(root)
}

/// 从插件根目录扫描声明的 Markdown 文件；目录递归、文件直接纳入。
fn scan_components(root: &Path, declarations: &[String]) -> Result<Vec<ComponentFile>> {
    let mut files = BTreeSet::new();
    for declaration in declarations {
        let path = safe_relative_join(root, declaration, "组件路径")?;
        if path.is_file() {
            insert_markdown_file(root, &path, &mut files)?;
        } else if path.is_dir() {
            scan_component_directory(root, &path, &mut files)?;
        } else {
            return Err(ClaudePluginError::Invalid(format!(
                "组件路径不存在：{}",
                path.display()
            )));
        }
    }
    Ok(files
        .into_iter()
        .map(|path| ComponentFile {
            relative_path: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
            path,
        })
        .collect())
}

/// 扫描清单显式声明的组件；未声明时遵循 Claude Code 的默认目录约定。
fn scan_declared_or_default_components(
    root: &Path,
    declarations: &[String],
    default_directory: &str,
) -> Result<Vec<ComponentFile>> {
    if !declarations.is_empty() {
        return scan_components(root, declarations);
    }
    let default_path = root.join(default_directory);
    if default_path.is_dir() {
        scan_components(root, &[default_directory.to_owned()])
    } else {
        Ok(Vec::new())
    }
}

/// 加载 inline hooks、清单引用的相对 hook 文件，或默认 `hooks/hooks.json`。
fn load_hooks(
    root: &Path,
    declaration: Option<&Value>,
    variables: &BTreeMap<String, String>,
) -> Result<Option<Value>> {
    let declarations = match declaration {
        Some(value) => vec![value.clone()],
        None => {
            let default_path = root.join("hooks/hooks.json");
            if !default_path.is_file() {
                return Ok(None);
            }
            vec![Value::String("hooks/hooks.json".to_owned())]
        }
    };
    let mut events = Map::new();
    for declaration in declarations {
        merge_hook_declaration(root, declaration, variables, &mut events)?;
    }
    if events.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(events)))
    }
}

/// 递归展开 Claude Hook 的包装对象、路径数组和 hooks/hooks.json 文件。
fn merge_hook_declaration(
    root: &Path,
    declaration: Value,
    variables: &BTreeMap<String, String>,
    events: &mut Map<String, Value>,
) -> Result<()> {
    match declaration {
        Value::String(path) => {
            let value = load_hook_file(root, &path)?;
            merge_hook_declaration(root, value, variables, events)
        }
        Value::Array(values) => {
            for value in values {
                merge_hook_declaration(root, value, variables, events)?;
            }
            Ok(())
        }
        Value::Object(object) => {
            // hooks/hooks.json 和 plugin.json 的标准格式都可能包一层
            // `{ "hooks": { "PreToolUse": [...] } }`；外层 description 等字段忽略。
            if let Some(inner) = object.get("hooks") {
                return merge_hook_declaration(root, inner.clone(), variables, events);
            }
            let value = interpolate_json(&Value::Object(object), variables)?;
            let Some(object) = value.as_object() else {
                return Err(ClaudePluginError::Invalid(
                    "Claude Hooks 声明必须是对象".to_owned(),
                ));
            };
            for (event, groups) in object {
                merge_hook_event(events, event, groups.clone());
            }
            Ok(())
        }
        _ => Err(ClaudePluginError::Invalid(
            "Claude Hooks 声明必须是对象、路径或数组".to_owned(),
        )),
    }
}

/// 合并多个 Hook 文件中同一事件的 matcher 数组，保持声明顺序。
fn merge_hook_event(events: &mut Map<String, Value>, event: &str, value: Value) {
    let Some(existing) = events.get_mut(event) else {
        events.insert(event.to_owned(), value);
        return;
    };
    let mut merged = match existing.take() {
        Value::Array(values) => values,
        value => vec![value],
    };
    match value {
        Value::Array(values) => merged.extend(values),
        value => merged.push(value),
    }
    *existing = Value::Array(merged);
}

/// 在插件根目录内读取清单声明的 hook JSON 文件。
fn load_hook_file(root: &Path, declaration: &str) -> Result<Value> {
    let path = safe_relative_join(root, declaration, "hooks 文件")?;
    if !path.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "hooks 文件不存在：{}",
            path.display()
        )));
    }
    Ok(serde_json::from_slice(&read_limited(&path)?)?)
}

/// 将配置名转换为可由 `${CLAUDE_PLUGIN_*}` 引用的稳定变量名。
fn normalize_variable_name(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                byte.to_ascii_uppercase() as char
            } else {
                '_'
            }
        })
        .collect()
}

/// 将标量或多选 userConfig 转换为环境变量值，复合 JSON 不会被隐式字符串化。
fn config_value_as_variable(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Array(values) => values
            .iter()
            .map(config_value_as_variable)
            .collect::<Option<Vec<_>>>()
            .map(|values| values.join(",")),
        Value::Null | Value::Object(_) => None,
    }
}

/// 深度优先扫描目录中的 Markdown 组件，拒绝符号链接跨出插件目录。
fn scan_component_directory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            let target = fs::canonicalize(&path)?;
            if !target.starts_with(root) {
                return Err(ClaudePluginError::Invalid(format!(
                    "组件符号链接越出插件根目录：{}",
                    path.display()
                )));
            }
            if target.is_dir() {
                scan_component_directory(root, &target, files)?;
            } else {
                insert_markdown_file(root, &target, files)?;
            }
        } else if metadata.is_dir() {
            scan_component_directory(root, &path, files)?;
        } else if metadata.is_file() {
            insert_markdown_file(root, &path, files)?;
        }
    }
    Ok(())
}

/// 仅纳入 `.md` 文件，并二次确认文件仍在插件根目录内。
fn insert_markdown_file(root: &Path, path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    let path = fs::canonicalize(path)?;
    if !path.starts_with(root) {
        return Err(ClaudePluginError::Invalid(format!(
            "组件文件越出插件根目录：{}",
            path.display()
        )));
    }
    if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
        files.insert(path);
    }
    Ok(())
}

/// 读取可选 `.mcp.json` 并返回 `mcpServers` 映射。
fn load_mcp_file(root: &Path) -> Result<Option<BTreeMap<String, Value>>> {
    let path = root.join(".mcp.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(parse_mcp_servers_value(
        serde_json::from_slice(&read_limited(&path)?)?,
        &path,
    )?))
}

/// 把插件声明的 DXT/MCPB `manifest.user_config` 合并为 Claude 插件 userConfig。
///
/// Claude Code 允许插件通过 MCP bundle 声明配置；这些字段必须进入同一个
/// SecretStore/热刷新管道，否则设置界面只能看到顶层 plugin.json 的字段。
fn merge_mcp_bundle_user_config(root: &Path, manifest: &mut PluginManifest) -> Result<()> {
    let declarations = manifest.mcp_servers.files.clone();
    for declaration in declarations {
        let lower = declaration.to_ascii_lowercase();
        if !lower.ends_with(".mcpb") && !lower.ends_with(".dxt") {
            continue;
        }
        let (_extracted, bundle_manifest) = materialize_mcp_bundle(root, &declaration)?;
        let object = bundle_manifest.as_object().ok_or_else(|| {
            ClaudePluginError::Invalid("MCPB/DXT manifest 顶层必须是对象".to_owned())
        })?;
        let Some(schema) = object
            .get("user_config")
            .or_else(|| object.get("userConfig"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (name, definition) in schema {
            normalized_identifier(name, "MCPB/DXT userConfig 字段")?;
            let parsed: UserConfigDefinition =
                serde_json::from_value(definition.clone()).map_err(|error| {
                    ClaudePluginError::Invalid(format!(
                        "MCPB/DXT userConfig {name} 定义无效：{error}"
                    ))
                })?;
            // plugin.json 显式声明优先，避免 bundle 与插件作者定义冲突时
            // 静默改变已有字段的敏感性或类型。
            manifest.user_config.entry(name.clone()).or_insert(parsed);
        }
    }
    Ok(())
}

/// 读取并缓存本地或远程 MCPB/DXT 归档，返回解包目录和 `manifest.json`。
fn materialize_mcp_bundle(root: &Path, declaration: &str) -> Result<(PathBuf, Value)> {
    let bytes = if declaration.starts_with("http://") || declaration.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|error| {
                ClaudePluginError::Invalid(format!("MCPB HTTP 客户端创建失败：{error}"))
            })?;
        let response = client
            .get(declaration)
            .header(reqwest::header::USER_AGENT, "KeenCode-Claude-Plugin/1")
            .send()
            .map_err(|error| ClaudePluginError::Invalid(format!("下载 MCPB/DXT 失败：{error}")))?
            .error_for_status()
            .map_err(|error| {
                ClaudePluginError::Invalid(format!("下载 MCPB/DXT 返回错误：{error}"))
            })?;
        let bytes = response.bytes().map_err(|error| {
            ClaudePluginError::Invalid(format!("读取 MCPB/DXT 响应失败：{error}"))
        })?;
        if bytes.len() > MAX_MCPB_BYTES {
            return Err(ClaudePluginError::Invalid(format!(
                "MCPB/DXT 超过 {} MB 限制",
                MAX_MCPB_BYTES / (1024 * 1024)
            )));
        }
        bytes.to_vec()
    } else {
        let path = safe_relative_join(root, declaration, "MCPB/DXT 文件")?;
        let metadata = fs::metadata(&path)?;
        if metadata.len() > MAX_MCPB_BYTES as u64 {
            return Err(ClaudePluginError::Invalid(format!(
                "MCPB/DXT 超过 {} MB 限制：{}",
                MAX_MCPB_BYTES / (1024 * 1024),
                path.display()
            )));
        }
        read_limited(&path)?
    };

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let cache_root = root.join(".mcpb-cache");
    let extracted = cache_root.join(format!("{:016x}", hasher.finish()));
    let manifest_path = extracted.join("manifest.json");
    if !manifest_path.is_file() {
        fs::create_dir_all(&extracted)?;
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| ClaudePluginError::Invalid(format!("MCPB/DXT ZIP 无效：{error}")))?;
        if archive.len() > MAX_MCPB_ENTRIES {
            return Err(ClaudePluginError::Invalid(
                "MCPB/DXT 文件数量超过限制".to_owned(),
            ));
        }
        let mut extracted_bytes = 0u64;
        let mut paths = BTreeSet::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| {
                ClaudePluginError::Invalid(format!("读取 MCPB/DXT 条目失败：{error}"))
            })?;
            let relative = entry
                .enclosed_name()
                .ok_or_else(|| {
                    ClaudePluginError::Invalid(format!("MCPB/DXT 条目路径越界：{}", entry.name()))
                })?
                .to_path_buf();
            if !paths.insert(relative.clone()) {
                return Err(ClaudePluginError::Invalid(format!(
                    "MCPB/DXT 包含重复条目：{}",
                    relative.display()
                )));
            }
            let destination = extracted.join(&relative);
            if entry.is_dir() {
                fs::create_dir_all(&destination)?;
                continue;
            }
            extracted_bytes = extracted_bytes
                .checked_add(entry.size())
                .ok_or_else(|| ClaudePluginError::Invalid("MCPB/DXT 解包大小溢出".to_owned()))?;
            if extracted_bytes > MAX_MCPB_EXTRACTED_BYTES {
                return Err(ClaudePluginError::Invalid(
                    "MCPB/DXT 解包后超过磁盘保护上限".to_owned(),
                ));
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = fs::File::create(&destination)?;
            io::copy(&mut entry, &mut output)?;
            output.sync_all()?;
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&destination, fs::Permissions::from_mode(mode & 0o777))?;
            }
        }
    }
    let manifest = serde_json::from_slice(&read_limited(&manifest_path)?)?;
    Ok((extracted, manifest))
}

/// 将 DXT/MCPB `manifest.json` 的 server 描述转换成 Peri MCP 配置。
fn mcp_bundle_servers(extracted: &Path, manifest: Value) -> Result<BTreeMap<String, Value>> {
    let object = manifest
        .as_object()
        .ok_or_else(|| ClaudePluginError::Invalid("MCPB/DXT manifest 顶层必须是对象".to_owned()))?;
    let server = object
        .get("server")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ClaudePluginError::Invalid("MCPB/DXT manifest 缺少 server 配置".to_owned())
        })?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("mcpb-server")
        .to_owned();
    let mut config = server
        .get("mcp_config")
        .or_else(|| server.get("mcpConfig"))
        .cloned()
        .unwrap_or(Value::Null);
    if config.is_null() {
        let entry_point = server
            .get("entry_point")
            .or_else(|| server.get("entryPoint"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| {
                ClaudePluginError::Invalid("MCPB/DXT server 缺少 entry_point".to_owned())
            })?;
        let server_type = server
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("binary")
            .to_ascii_lowercase();
        let entry = extracted.join(safe_relative_path(entry_point, "MCPB entry_point")?);
        let (command, default_args) = match server_type.as_str() {
            "node" | "javascript" => ("node".to_owned(), vec![entry.display().to_string()]),
            "python" | "python3" => ("python3".to_owned(), vec![entry.display().to_string()]),
            "binary" | "executable" => (entry.display().to_string(), Vec::new()),
            other => {
                return Err(ClaudePluginError::Invalid(format!(
                    "不支持的 MCPB server.type：{other}"
                )));
            }
        };
        let args = server
            .get("args")
            .cloned()
            .unwrap_or_else(|| Value::Array(default_args.into_iter().map(Value::String).collect()));
        let mut map = Map::new();
        map.insert("command".to_owned(), Value::String(command));
        map.insert("args".to_owned(), args);
        if let Some(env) = server.get("env") {
            map.insert("env".to_owned(), env.clone());
        }
        config = Value::Object(map);
    }
    let config = replace_mcp_bundle_dir(&config, extracted)?;
    let mut servers = BTreeMap::new();
    servers.insert(name, normalize_mcp_server_value(config)?);
    Ok(servers)
}

/// 替换 DXT 专用 `${__dirname}` 变量，其他变量留给统一插值阶段。
fn replace_mcp_bundle_dir(value: &Value, extracted: &Path) -> Result<Value> {
    match value {
        Value::String(text) => Ok(Value::String(
            text.replace("${__dirname}", &extracted.display().to_string()),
        )),
        Value::Array(values) => values
            .iter()
            .map(|value| replace_mcp_bundle_dir(value, extracted))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), replace_mcp_bundle_dir(value, extracted)?)))
            .collect::<Result<Map<_, _>>>()
            .map(Value::Object),
        value => Ok(value.clone()),
    }
}

/// 从清单引用的相对 MCP 文件读取 Server；路径受插件根目录边界保护。
fn load_mcp_servers_file(root: &Path, declaration: &str) -> Result<BTreeMap<String, Value>> {
    if declaration.starts_with("http://") || declaration.starts_with("https://") {
        let (extracted, manifest) = materialize_mcp_bundle(root, declaration)?;
        return mcp_bundle_servers(&extracted, manifest);
    }
    let path = safe_relative_join(root, declaration, "mcpServers 文件")?;
    if !path.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "mcpServers 文件不存在：{}",
            path.display()
        )));
    }
    if matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("mcpb" | "dxt")
    ) {
        let (extracted, manifest) = materialize_mcp_bundle(root, declaration)?;
        return mcp_bundle_servers(&extracted, manifest);
    }
    parse_mcp_servers_value(serde_json::from_slice(&read_limited(&path)?)?, &path)
}

/// 从 `.mcp.json`、mcpServers 文件或内联 JSON 提取对象/数组形式的 Server 映射。
fn parse_mcp_servers_value(value: Value, path: &Path) -> Result<BTreeMap<String, Value>> {
    let object = value.as_object().ok_or_else(|| {
        ClaudePluginError::Invalid(format!("MCP 配置顶层必须是对象：{}", path.display()))
    })?;
    let servers = object
        .get("mcpServers")
        .or_else(|| object.get("servers"))
        .cloned()
        .unwrap_or_else(|| Value::Object(object.clone()));
    parse_mcp_servers_entries(servers)
}

/// 解析 `mcpServers` 的对象和数组形式；数组项可为 `{ name, config }` 或 `{ name, command }`。
fn parse_mcp_servers_entries(value: Value) -> Result<BTreeMap<String, Value>> {
    let mut servers = BTreeMap::new();
    match value {
        Value::Object(values) => {
            for (name, value) in values {
                insert_mcp_server(&mut servers, name, value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                let mut entry = value.as_object().cloned().ok_or_else(|| {
                    ClaudePluginError::Invalid("mcpServers 数组项必须是对象".to_owned())
                })?;
                let name = entry
                    .remove("name")
                    .and_then(|value| value.as_str().map(ToOwned::to_owned))
                    .ok_or_else(|| {
                        ClaudePluginError::Invalid("mcpServers 数组项缺少 name".to_owned())
                    })?;
                let config = entry
                    .remove("config")
                    .unwrap_or_else(|| Value::Object(entry));
                insert_mcp_server(&mut servers, name, config)?;
            }
        }
        _ => {
            return Err(ClaudePluginError::Invalid(
                "mcpServers 必须是对象或数组".to_owned(),
            ));
        }
    }
    Ok(servers)
}

/// 插入唯一的 MCP Server；同一 JSON 形式内的重复名称是配置错误。
fn insert_mcp_server(
    servers: &mut BTreeMap<String, Value>,
    name: String,
    config: Value,
) -> Result<()> {
    let name = normalized_identifier(&name, "MCP Server 名称")?;
    if servers.insert(name.clone(), config).is_some() {
        return Err(ClaudePluginError::Invalid(format!(
            "MCP Server 名称重复：{name}"
        )));
    }
    Ok(())
}

/// 将 Claude MCP Server 的 `type` 判别字段归一为 Peri 当前支持的 command/url。
fn normalize_mcp_server_value(value: Value) -> Result<Value> {
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| ClaudePluginError::Invalid("MCP Server 配置必须是对象".to_owned()))?;
    if let Some(kind) = object
        .remove("type")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
    {
        match kind.as_str() {
            "stdio" | "streamable-http" | "http" => {}
            "sse" | "websocket" | "sdk" | "claudeai-proxy" => {
                return Err(ClaudePluginError::Invalid(format!(
                    "当前运行时不支持该 MCP transport type={kind}"
                )));
            }
            other => {
                return Err(ClaudePluginError::Invalid(format!(
                    "未知 Claude MCP transport type={other}"
                )));
            }
        }
    }
    if object.get("disabled") == Some(&Value::Bool(false)) {
        object.remove("disabled");
    }
    if object.get("serverUrl").is_some()
        && object.get("url").is_none()
        && let Some(url) = object.remove("serverUrl")
    {
        object.insert("url".to_owned(), url);
    }
    Ok(Value::Object(object))
}

/// 安全递归复制插件树；符号链接按目标校验后复制其文件内容，不保留外部链接。
fn copy_plugin_tree(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Ok(());
    }
    let temporary = destination.with_extension("installing");
    if temporary.exists() {
        fs::remove_dir_all(&temporary)?;
    }
    fs::create_dir_all(&temporary)?;
    copy_tree_entry(source, source, &temporary)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(temporary, destination)?;
    Ok(())
}

/// 复制目录树中的一项，并拒绝指向源根之外的符号链接。
fn copy_tree_entry(root: &Path, source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            let target = fs::canonicalize(&source_path)?;
            if !target.starts_with(root) {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件符号链接越出根目录：{}",
                    source_path.display()
                )));
            }
            if target.is_dir() {
                fs::create_dir_all(&destination_path)?;
                copy_tree_entry(root, &target, &destination_path)?;
            } else {
                fs::copy(target, destination_path)?;
            }
        } else if metadata.is_dir() {
            fs::create_dir_all(&destination_path)?;
            copy_tree_entry(root, &source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

/// 确保公开状态不存在重复 ID、敏感值或未规范化的安装路径。
fn validate_state(state: &PluginState) -> Result<()> {
    let mut ids = BTreeSet::new();
    for plugin in &state.plugins {
        let id = require_marketplace_id(&plugin.id)?;
        if !ids.insert(id.clone()) {
            return Err(ClaudePluginError::Invalid(format!(
                "插件状态包含重复 ID：{id}"
            )));
        }
        non_empty(&plugin.version, "已安装插件版本")?;
        if !plugin.install_path.is_absolute() {
            return Err(ClaudePluginError::Invalid(format!(
                "安装路径必须为绝对路径：{id}"
            )));
        }
        for secret in &plugin.sensitive_user_config_keys {
            if plugin.public_user_config.contains_key(secret) {
                return Err(ClaudePluginError::Invalid(format!(
                    "敏感 userConfig 不能存在公开状态：{id}.{secret}"
                )));
            }
        }
    }
    Ok(())
}

/// 强制 ID 含市场命名空间，避免状态、缓存和密钥键冲突。
fn require_marketplace_id(id: &PluginId) -> Result<PluginId> {
    Ok(PluginId {
        plugin: normalized_identifier(&id.plugin, "插件名称")?,
        marketplace: Some(normalized_identifier(
            id.marketplace.as_deref().ok_or_else(|| {
                ClaudePluginError::Invalid("插件 ID 必须为 plugin@marketplace".to_owned())
            })?,
            "市场名称",
        )?),
    })
}

/// 把公开标识转换为安全的单层缓存路径分段。
fn safe_cache_component(value: &str, label: &str) -> Result<String> {
    normalized_identifier(value, label)
}

/// 空 settings 实现，只用于本地清单验证；settings 来源必须由调用方显式解析。
#[cfg(test)]
struct EmptyMarketplaceSettings;

#[cfg(test)]
impl MarketplaceSettings for EmptyMarketplaceSettings {
    /// 验证期间不允许隐式从环境或文件读取 settings。
    fn marketplace_source(&self, _key: &str) -> Option<MarketplaceSource> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `plugin@marketplace` ID 与无命名空间 ID 均可解析。
    #[test]
    fn parses_plugin_id() {
        assert_eq!(
            PluginId::parse("demo@official").unwrap().to_string(),
            "demo@official"
        );
        assert_eq!(PluginId::parse("demo").unwrap().to_string(), "demo");
        let object: PluginId =
            serde_json::from_str(r#"{"plugin":"demo","marketplace":"official"}"#)
                .expect("应读取当前状态中的插件 ID 对象");
        assert_eq!(object.to_string(), "demo@official");
        let string: PluginId =
            serde_json::from_str(r#""demo@official""#).expect("应读取插件 ID 字符串简写");
        assert_eq!(string, object);
    }

    /// 验证市场 source 的 GitHub 形式被转换为可审查 Git 计划。
    #[test]
    fn parses_marketplace_source() {
        let source: MarketplaceSource =
            serde_json::from_str(r#"{"source":"github","repo":"acme/plugins","ref":"v1","path":"repo/.claude-plugin/marketplace.json","sparsePaths":["repo/.claude-plugin","repo/plugins"]}"#)
                .unwrap();
        assert!(matches!(
            source,
            MarketplaceSource::Github {
                path: Some(_),
                sparse_paths,
                ..
            } if sparse_paths.len() == 2
        ));
        let source: MarketplaceSource =
            serde_json::from_str(r#"{"source":"github","repo":"acme/plugins","ref":"v1"}"#)
                .unwrap();
        assert_eq!(
            source.fetch_plan(&EmptyMarketplaceSettings).unwrap(),
            SourceFetchPlan::Git {
                url: "https://github.com/acme/plugins.git".to_owned(),
                reference: Some("v1".to_owned()),
                sha: None,
                subdir: None,
            }
        );
    }

    /// 官方保留市场必须严格绑定 GitHub 的 anthropics owner，不能只做字符串包含检查。
    #[test]
    fn validates_official_marketplace_source_owner_exactly() {
        for source in [
            "github:anthropics/claude-plugins-official",
            "github:anthropics/claude-plugins-official@main",
            "git@github.com:anthropics/claude-plugins-official.git",
            "https://github.com/anthropics/claude-plugins-official.git",
            "git:https://github.com/anthropics/claude-plugins-official.git",
        ] {
            assert!(
                validate_marketplace_name_source("claude-plugins-official", source).is_ok(),
                "应接受官方来源：{source}"
            );
        }
        for source in [
            "https://evil.example/github.com/anthropics/claude-plugins-official.git",
            "https://github.com/anthropics.evil/claude-plugins-official.git",
            "github:anthropics.evil/claude-plugins-official",
            "github:other/claude-plugins-official",
            "https://github.com/anthropics",
        ] {
            assert!(
                validate_marketplace_name_source("claude-plugins-official", source).is_err(),
                "应拒绝伪造来源：{source}"
            );
        }
    }

    /// Claude marketplace schema 允许暂时没有插件条目的市场清单。
    #[test]
    fn accepts_empty_marketplace_plugin_list() {
        let manifest = parse_marketplace_manifest(br#"{"name":"empty-market","plugins":[]}"#)
            .expect("空插件市场应符合 Claude 清单结构");
        assert!(manifest.plugins.is_empty());
    }

    /// 验证 MCPB/DXT 归档能安全解包并转换为 stdio MCP 配置。
    #[test]
    fn loads_mcpb_bundle() {
        use std::io::Write as _;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("keencode-mcpb-test-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let bundle = root.join("server.mcpb");
        let file = fs::File::create(&bundle).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(br#"{"name":"demo","server":{"type":"node","entry_point":"server.js"}}"#)
            .unwrap();
        writer
            .start_file("server.js", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"console.log('ok')").unwrap();
        writer.finish().unwrap();

        let (extracted, manifest) = materialize_mcp_bundle(&root, "./server.mcpb").unwrap();
        let servers = mcp_bundle_servers(&extracted, manifest).unwrap();
        let config = servers.get("demo").unwrap();
        assert_eq!(config.get("command").and_then(Value::as_str), Some("node"));
        assert!(
            config.get("args").and_then(Value::as_array).unwrap()[0]
                .as_str()
                .unwrap()
                .ends_with("server.js")
        );
        let _ = fs::remove_dir_all(root);
    }

    /// MCPB/DXT 的 user_config 应并入插件配置模型，敏感字段仍由同一 SecretStore 管道处理。
    #[test]
    fn merges_mcpb_user_config_into_plugin_manifest() {
        use std::io::Write as _;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-mcpb-config-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            br#"{"name":"bundle-plugin","mcpServers":["server.mcpb"]}"#,
        )
        .unwrap();
        let bundle = root.join("server.mcpb");
        let file = fs::File::create(&bundle).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(
                br#"{"name":"demo","user_config":{"token":{"type":"string","sensitive":true,"required":true},"port":{"type":"number","min":1,"max":65535}},"server":{"type":"node","entry_point":"server.js"}}"#,
            )
            .unwrap();
        writer
            .start_file("server.js", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"console.log('ok')").unwrap();
        writer.finish().unwrap();

        let manifest = load_plugin_manifest(&root).unwrap();
        assert!(manifest.user_config.get("token").unwrap().sensitive);
        assert_eq!(manifest.user_config.get("port").unwrap().min, Some(1.0));
        let _ = fs::remove_dir_all(root);
    }

    /// pip 来源生成结构化计划，版本使用 pip 的 `==` 语法而非 npm 的 `@`。
    #[test]
    fn pip_source_has_a_parameterized_fetch_plan() {
        let source: PluginSource =
            serde_json::from_str(r#"{"source":"pip","package":"acme-plugin","version":"1.2.3"}"#)
                .unwrap();
        assert_eq!(
            source.fetch_plan(Path::new("/tmp")).unwrap(),
            SourceFetchPlan::Pip {
                package_spec: "acme-plugin==1.2.3".to_owned(),
                registry: None,
            }
        );
    }

    /// npm/pip source 的私有 registry 必须进入结构化取得计划，不能在解析时丢失。
    #[test]
    fn preserves_package_registry_in_fetch_plans() {
        let npm: PluginSource = serde_json::from_str(
            r#"{"source":"npm","package":"@acme/plugin","version":"1.0.0","registry":"https://npm.acme.test/"}"#,
        )
        .unwrap();
        assert_eq!(
            npm.fetch_plan(Path::new("/tmp")).unwrap(),
            SourceFetchPlan::Npm {
                package_spec: "@acme/plugin@1.0.0".to_owned(),
                registry: Some("https://npm.acme.test/".to_owned()),
            }
        );
        let pip: PluginSource = serde_json::from_str(
            r#"{"source":"pip","package":"acme-plugin","registry":"https://pypi.acme.test/simple"}"#,
        )
        .unwrap();
        assert_eq!(
            pip.fetch_plan(Path::new("/tmp")).unwrap(),
            SourceFetchPlan::Pip {
                package_spec: "acme-plugin".to_owned(),
                registry: Some("https://pypi.acme.test/simple".to_owned()),
            }
        );
    }

    /// 变量只能使用字母、数字和下划线，且缺失值为硬错误。
    #[test]
    fn interpolates_variables() {
        let variables = BTreeMap::from([("NAME".to_owned(), "KeenCode".to_owned())]);
        assert_eq!(
            interpolate_variables("hello ${NAME}", &variables).unwrap(),
            "hello KeenCode"
        );
        assert!(matches!(
            interpolate_variables("${MISSING}", &variables),
            Err(ClaudePluginError::MissingVariable(_))
        ));
    }

    /// 缺省 version、mcpServers 文件/数组形式和 directory 多选配置均可解析。
    #[test]
    fn parses_current_claude_manifest_variants() {
        let manifest = parse_plugin_manifest(
            br#"{
                "name":"demo",
                "mcpServers":["./mcp.json", {"name":"inline","command":"echo"}],
                "userConfig":{"paths":{"type":"directory","title":"Paths","multiple":true,"min":1,"max":20}}
            }"#,
        )
        .unwrap();
        assert_eq!(manifest.version, None);
        assert_eq!(manifest.mcp_servers.files, vec!["./mcp.json"]);
        assert!(manifest.mcp_servers.inline.contains_key("inline"));
        let definition = manifest.user_config.get("paths").unwrap();
        validate_user_config_value(
            "paths",
            definition,
            &Value::Array(vec![Value::String("/tmp/project".to_owned())]),
        )
        .unwrap();
    }

    /// lspServers 接受 Peri 运行时字段，并保留未来字段以维持前向兼容。
    #[test]
    fn parses_complete_peri_lsp_server_contract() {
        let manifest = parse_plugin_manifest(
            br#"{
                "name":"demo",
                "lspServers":[{
                    "name":"rust-analyzer",
                    "command":"rust-analyzer",
                    "args":["--stdio"],
                    "env":{"RUST_LOG":"info"},
                    "extensionToLanguage":{".rs":"rust"},
                    "initializationOptions":{"cargo":{"allFeatures":true}},
                    "disabled":false,
                    "maxRestarts":5,
                    "startupTimeout":120000,
                    "futureField":{"mode":"auto"}
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(manifest.lsp_servers.len(), 1);
        let server = &manifest.lsp_servers[0];
        assert_eq!(server.name, "rust-analyzer");
        assert_eq!(server.env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert_eq!(server.disabled, Some(false));
        assert_eq!(server.max_restarts, Some(5));
        assert_eq!(server.startup_timeout, Some(120_000));
        assert_eq!(
            server.extra.get("futureField"),
            Some(&serde_json::json!({"mode":"auto"}))
        );

        assert!(
            parse_plugin_manifest(
                br#"{"name":"demo","lspServers":{"rust":{"command":"rust-analyzer"}}}"#
            )
            .is_err()
        );
        assert!(parse_plugin_manifest(
            br#"{"name":"demo","lspServers":[{"name":"rust","command":"one"},{"name":"rust","command":"two"}]}"#
        )
        .is_err());
    }

    /// 官方 jdtls/kotlin 的 marketplace LSP 形态与 120 秒启动超时可生成清单。
    #[test]
    fn materializes_official_marketplace_lsp_only_plugins() {
        let marketplace = parse_marketplace_manifest(
            br#"{
                "name":"official",
                "plugins":[{
                    "name":"jdtls-lsp",
                    "source":"./jdtls-lsp",
                    "version":"1.0.0",
                    "description":"Java language server (Eclipse JDT.LS) for code intelligence",
                    "lspServers":{
                        "jdtls":{
                            "command":"jdtls",
                            "extensionToLanguage":{".java":"java"},
                            "startupTimeout":120000
                        }
                    }
                },{
                    "name":"kotlin-lsp",
                    "source":"./kotlin-lsp",
                    "version":"1.0.0",
                    "description":"Kotlin language server for code intelligence",
                    "lspServers":{
                        "kotlin-lsp":{
                            "command":"kotlin-lsp",
                            "args":["--stdio"],
                            "extensionToLanguage":{".kt":"kotlin",".kts":"kotlin"},
                            "startupTimeout":120000,
                            "futureOption":"preserved"
                        }
                    }
                }]
            }"#,
        )
        .unwrap();
        let jdtls = &marketplace.plugins[0];
        let jdtls_manifest = synthetic_marketplace_plugin_manifest(jdtls)
            .unwrap()
            .unwrap();
        assert_eq!(jdtls_manifest.name, "jdtls-lsp");
        assert_eq!(jdtls_manifest.lsp_servers.len(), 1);
        assert_eq!(jdtls_manifest.lsp_servers[0].startup_timeout, Some(120_000));
        let kotlin_manifest = synthetic_marketplace_plugin_manifest(&marketplace.plugins[1])
            .unwrap()
            .unwrap();
        assert_eq!(
            kotlin_manifest.lsp_servers[0].startup_timeout,
            Some(120_000)
        );
        assert_eq!(
            kotlin_manifest.lsp_servers[0].extra.get("futureOption"),
            Some(&Value::String("preserved".to_owned()))
        );

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-market-lsp-test-{}-{nonce}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("materialized");
        fs::create_dir_all(&source).unwrap();

        let materialized =
            materialize_synthetic_marketplace_plugin(&source, &destination, jdtls).unwrap();
        assert!(!source.join(".claude-plugin/plugin.json").exists());
        assert!(materialized.join(".claude-plugin/plugin.json").is_file());
        assert_eq!(
            load_plugin_manifest(&materialized)
                .unwrap()
                .lsp_servers
                .len(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// 官方市场的 `strict:false + skills` skill bundle 没有 plugin.json 时也应可安装。
    #[test]
    fn materializes_official_marketplace_skill_bundle_without_plugin_manifest() {
        let marketplace = parse_marketplace_manifest(
            br#"{
                "name":"official",
                "plugins":[{
                    "name":"amd-skills",
                    "strict":false,
                    "source":"./skills",
                    "skills":["./local-ai-use","./serving-llms"]
                }]
            }"#,
        )
        .unwrap();
        let plugin = &marketplace.plugins[0];
        let manifest = synthetic_marketplace_plugin_manifest(plugin)
            .unwrap()
            .unwrap();
        assert_eq!(
            manifest.skills.paths,
            vec!["./local-ai-use", "./serving-llms"]
        );
        assert!(manifest.lsp_servers.is_empty());

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-market-skills-test-{}-{nonce}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("materialized");
        fs::create_dir_all(source.join("local-ai-use")).unwrap();
        fs::create_dir_all(source.join("serving-llms")).unwrap();
        fs::write(source.join("local-ai-use/SKILL.md"), "# Local AI").unwrap();
        fs::write(source.join("serving-llms/SKILL.md"), "# Serving").unwrap();

        let materialized =
            materialize_synthetic_marketplace_plugin(&source, &destination, plugin).unwrap();
        assert!(!source.join(".claude-plugin/plugin.json").exists());
        assert!(materialized.join(".claude-plugin/plugin.json").is_file());
        let loaded = load_plugin_manifest(&materialized).unwrap();
        let snapshot = extract_components(
            PluginId::parse("amd-skills@official").unwrap(),
            &materialized,
            &loaded,
            Path::new("."),
            &BTreeMap::new(),
            &ResolvedUserConfig::default(),
        )
        .unwrap();
        assert_eq!(snapshot.skills.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    /// 官方市场允许仅使用默认 skills/ 目录，不在 marketplace 条目重复声明组件。
    #[test]
    fn materializes_manifestless_plugin_with_default_skill_directory() {
        let marketplace = parse_marketplace_manifest(
            br#"{
                "name":"claude-plugins-official",
                "plugins":[{
                    "name":"receipts",
                    "description":"Usage impact report",
                    "source":"./plugins/receipts"
                }]
            }"#,
        )
        .unwrap();
        let plugin = &marketplace.plugins[0];
        assert!(
            synthetic_marketplace_plugin_manifest(plugin)
                .unwrap()
                .is_none()
        );

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-market-default-skills-test-{}-{nonce}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("materialized");
        fs::create_dir_all(source.join("skills/receipts")).unwrap();
        fs::write(source.join("skills/receipts/SKILL.md"), "# Receipts").unwrap();

        let inferred = synthetic_marketplace_plugin_manifest_for_root(plugin, &source)
            .unwrap()
            .expect("默认 skills 目录应生成清单");
        assert!(inferred.skills.paths.is_empty());
        let materialized =
            materialize_synthetic_marketplace_plugin(&source, &destination, plugin).unwrap();
        let loaded = load_plugin_manifest(&materialized).unwrap();
        let snapshot = extract_components(
            PluginId::parse("receipts@claude-plugins-official").unwrap(),
            &materialized,
            &loaded,
            Path::new("."),
            &BTreeMap::new(),
            &ResolvedUserConfig::default(),
        )
        .unwrap();
        assert_eq!(snapshot.skills.len(), 1);

        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(
            synthetic_marketplace_plugin_manifest_for_root(plugin, &empty)
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// 插件 LSP 只在加载期展开静态变量，cwd 与 Session ID 保留给 Peri 工厂。
    #[test]
    fn preserves_session_scoped_plugin_lsp_variables_for_peri_runtime() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-claude-lsp-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            br#"{
                "name":"demo",
                "lspServers":[{
                    "name":"rust",
                    "command":"${CLAUDE_PLUGIN_ROOT}/bin/server",
                    "args":["--project","${CLAUDE_PROJECT_DIR}","${user_config.channel}","${CLAUDE_SESSION_ID}"],
                    "env":{
                        "PLUGIN_CACHE":"${CLAUDE_PLUGIN_ROOT}/cache",
                        "SESSION_CACHE":"${CLAUDE_PROJECT_DIR}/${CLAUDE_SESSION_ID}"
                    },
                    "extensionToLanguage":{"rs":"rust"},
                    "initializationOptions":{
                        "project":"${CLAUDE_PROJECT_DIR}",
                        "session":"${CLAUDE_SESSION_ID}"
                    },
                    "disabled":false,
                    "maxRestarts":7,
                    "startupTimeout":120000
                }]
            }"#,
        )
        .unwrap();
        let manifest = load_plugin_manifest(&root).unwrap();
        let project = root.join("project");
        let runtime = extract_components(
            PluginId::parse("demo@local").unwrap(),
            &root,
            &manifest,
            &project,
            &BTreeMap::from([
                ("CLAUDE_PROJECT_DIR".to_owned(), "/stale/project".to_owned()),
                ("CLAUDE_SESSION_ID".to_owned(), "stale-session".to_owned()),
            ]),
            &ResolvedUserConfig {
                values: BTreeMap::from([(
                    "channel".to_owned(),
                    Value::String("stable".to_owned()),
                )]),
                missing_sensitive: BTreeSet::new(),
            },
        )
        .unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let server = runtime.lsp_servers.first().unwrap();
        assert_eq!(server.name, "plugin:demo:rust");
        assert_eq!(
            server.command,
            format!("{}/bin/server", canonical_root.display())
        );
        assert_eq!(server.args[1], "${CLAUDE_PROJECT_DIR}");
        assert_eq!(server.args[2], "stable");
        assert_eq!(server.args[3], "${CLAUDE_SESSION_ID}");
        assert_eq!(
            server
                .env
                .as_ref()
                .and_then(|environment| environment.get("CLAUDE_PLUGIN_ROOT"))
                .map(String::as_str),
            Some(canonical_root.to_string_lossy().as_ref())
        );
        assert_eq!(
            server
                .env
                .as_ref()
                .and_then(|environment| environment.get("PLUGIN_CACHE"))
                .map(String::as_str),
            Some(format!("{}/cache", canonical_root.display()).as_str())
        );
        assert_eq!(
            server
                .env
                .as_ref()
                .and_then(|environment| environment.get("SESSION_CACHE"))
                .map(String::as_str),
            Some("${CLAUDE_PROJECT_DIR}/${CLAUDE_SESSION_ID}")
        );
        assert_eq!(
            server.extension_to_language.get("rs"),
            Some(&"rust".to_owned())
        );
        assert_eq!(
            server.initialization_options,
            Some(serde_json::json!({
                "project": "${CLAUDE_PROJECT_DIR}",
                "session": "${CLAUDE_SESSION_ID}"
            }))
        );
        assert_eq!(server.disabled, Some(false));
        assert_eq!(server.max_restarts, Some(7));
        assert_eq!(server.startup_timeout, Some(120_000));
        fs::remove_dir_all(root).unwrap();
    }

    /// 未声明 inline hooks 时，默认加载插件根目录内的 hooks/hooks.json。
    #[test]
    fn loads_default_hooks_file() {
        let root =
            std::env::temp_dir().join(format!("keencode-claude-hooks-{}", std::process::id()));
        fs::create_dir_all(root.join("hooks")).unwrap();
        fs::write(
            root.join("hooks/hooks.json"),
            br#"{"PostToolUse":[{"command":"echo ${CLAUDE_PLUGIN_ROOT}"}]}"#,
        )
        .unwrap();
        let variables =
            BTreeMap::from([("CLAUDE_PLUGIN_ROOT".to_owned(), "/safe/plugin".to_owned())]);
        let hooks = load_hooks(&root, None, &variables).unwrap().unwrap();
        assert!(hooks.to_string().contains("/safe/plugin"));
        fs::remove_dir_all(root).unwrap();
    }

    /// 配置名称将被归一为合法的 `CLAUDE_PLUGIN_*` 变量命名空间。
    #[test]
    fn normalizes_plugin_variable_namespace() {
        assert_eq!(normalize_variable_name("api.key-name"), "API_KEY_NAME");
        assert_eq!(
            config_value_as_variable(&Value::Array(vec![
                Value::String("one".to_owned()),
                Value::String("two".to_owned()),
            ])),
            Some("one,two".to_owned())
        );
    }

    /// 公开状态使用新建私有临时文件并原子替换；Unix 上权限必须为 0600。
    #[test]
    fn saves_state_with_private_permissions() {
        let root =
            std::env::temp_dir().join(format!("keencode-claude-state-{}", std::process::id()));
        let manager = ClaudePluginManager::new(&root);
        manager.save_state(&PluginState::default()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&manager.storage.state_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// 依赖闭包应该把依赖排在目标插件之前。
    #[test]
    fn resolves_dependency_closure() {
        let marketplace = parse_marketplace_manifest(
            br#"{"name":"official","plugins":[{"name":"a","source":"./a","dependencies":{"b":"^1"}},{"name":"b","source":"./b"}]}"#,
        )
        .unwrap();
        let manifests = BTreeMap::from([
            (
                "a".to_owned(),
                parse_plugin_manifest(br#"{"name":"a","version":"1"}"#).unwrap(),
            ),
            (
                "b".to_owned(),
                parse_plugin_manifest(br#"{"name":"b","version":"1"}"#).unwrap(),
            ),
        ]);
        assert_eq!(
            dependency_closure(
                &PluginId::parse("a@official").unwrap(),
                &marketplace,
                &manifests
            )
            .unwrap()
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
            vec!["b@official", "a@official"]
        );
    }

    /// 循环依赖不能导致栈溢出，必须返回完整循环路径。
    #[test]
    fn detects_dependency_cycle() {
        let marketplace = parse_marketplace_manifest(
            br#"{"name":"official","plugins":[{"name":"a","source":"./a","dependencies":{"b":"*"}},{"name":"b","source":"./b","dependencies":{"a":"*"}}]}"#,
        )
        .unwrap();
        let manifests = BTreeMap::from([
            (
                "a".to_owned(),
                parse_plugin_manifest(br#"{"name":"a","version":"1"}"#).unwrap(),
            ),
            (
                "b".to_owned(),
                parse_plugin_manifest(br#"{"name":"b","version":"1"}"#).unwrap(),
            ),
        ]);
        assert!(matches!(
            dependency_closure(
                &PluginId::parse("a@official").unwrap(),
                &marketplace,
                &manifests
            ),
            Err(ClaudePluginError::DependencyCycle(_))
        ));
    }

    /// 敏感 userConfig 只能出现在 SecretStore，不能被写入公开状态。
    #[test]
    fn splits_sensitive_user_config() {
        let definition: UserConfigDefinition =
            serde_json::from_str(r#"{"type":"string","sensitive":true,"required":true}"#).unwrap();
        let manifest = PluginManifest {
            name: "demo".to_owned(),
            version: Some("1".to_owned()),
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: Vec::new(),
            commands: ComponentDeclaration::default(),
            skills: ComponentDeclaration::default(),
            agents: ComponentDeclaration::default(),
            hooks: None,
            mcp_servers: McpServersDeclaration::default(),
            lsp_servers: Vec::new(),
            user_config: BTreeMap::from([("token".to_owned(), definition)]),
            dependencies: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        let manager = ClaudePluginManager::new("/tmp/keencode-claude-plugin-test");
        let id = PluginId::parse("demo@official").unwrap();
        let mut store = InMemorySecretStore::default();
        let (public, sensitive) = manager
            .apply_user_config(
                &id,
                &manifest,
                None,
                UserConfigUpdate {
                    values: BTreeMap::from([(
                        "token".to_owned(),
                        Value::String("secret".to_owned()),
                    )]),
                    replace: false,
                },
                &mut store,
                true,
            )
            .unwrap();
        assert!(public.is_empty());
        assert!(sensitive.contains("token"));
        assert_eq!(
            store
                .get_json(&manager.storage.secret_key(&id, "token").unwrap())
                .unwrap(),
            Some(Value::String("secret".to_owned()))
        );
    }
}
