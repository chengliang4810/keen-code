use anyhow::{Context, Result};
use keencode_tools::WebServiceConfig;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

use crate::path_utils::{path_text_to_frontend, path_to_frontend};

/// 应用更新安装包的下载源偏好。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AppUpdateDownloadSource {
    #[default]
    Auto,
    Github,
    ChinaMirror,
}

/// Windows 内置终端使用的 Shell；其他平台固定使用登录 Shell。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TerminalShell {
    #[default]
    Auto,
    PowerShell,
    PowerShell7,
    GitBash,
    Cmd,
}

/// KeenCode 界面与后台自然语言产物使用的语言。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum InterfaceLanguage {
    #[default]
    #[serde(rename = "zh")]
    SimplifiedChinese,
    #[serde(rename = "zh-TW")]
    TraditionalChinese,
    #[serde(rename = "en")]
    English,
}

impl InterfaceLanguage {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::SimplifiedChinese => "zh",
            Self::TraditionalChinese => "zh-TW",
            Self::English => "en",
        }
    }

    /// 记忆模型请求使用的明确语言约束。
    pub fn memory_instruction(self) -> &'static str {
        match self {
            Self::SimplifiedChinese => {
                "Write all natural-language content in Simplified Chinese. Preserve code, paths, commands, identifiers, and proper nouns as written."
            }
            Self::TraditionalChinese => {
                "Write all natural-language content in Traditional Chinese. Preserve code, paths, commands, identifiers, and proper nouns as written."
            }
            Self::English => {
                "Write all natural-language content in English. Preserve code, paths, commands, identifiers, and proper nouns as written."
            }
        }
    }
}

/// 串行化应用设置读写。
static SETTINGS_IO_LOCK: Mutex<()> = Mutex::new(());

pub const DEFAULT_BACKGROUND_AGENT_LIMIT: u16 = 10;
pub const MAX_BACKGROUND_AGENT_LIMIT: u16 = 999;
pub const DEFAULT_TERMINAL_FONT_FAMILY: &str =
    "ui-monospace, \"SFMono-Regular\", Menlo, Monaco, Consolas, monospace";
/// 当前应用设置文件的固定 schema 名称。
const APP_SETTINGS_SCHEMA: &str = "keencode/app-settings";
/// 当前应用设置文件的固定格式版本。
const APP_SETTINGS_VERSION: u32 = 1;

/// KeenCode 当前唯一的应用设置结构。
///
/// 缺失字段（旧版本设置文件尚未写入的新增字段）回退 [`AppSettings::initial`]
/// 默认值，未知或已移除字段被忽略并作为警告上报；设置读取不允许阻断启动。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// 当前界面语言；首次启动默认简体中文。
    pub interface_language: InterfaceLanguage,
    /// 应用更新安装包的下载源偏好。
    pub app_update_download_source: AppUpdateDownloadSource,
    /// Windows WebView2 是否启用硬件加速。
    pub chrome_hardware_acceleration: bool,
    /// 侧栏中由用户折叠的项目标识。
    pub sidebar_collapsed_project_ids: Vec<String>,
    /// 未手动选择现有目录时，新项目的默认父目录。
    pub project_directory: String,
    /// 是否发送任务完成或失败的桌面通知。
    pub task_notifications: bool,
    /// 对话中是否保留已结束的思考过程内容块。
    pub show_thinking_process: bool,
    /// 任务桌面通知是否请求播放系统默认提示音。
    pub notification_sound: bool,
    /// 是否阻止系统因用户空闲自动进入睡眠。
    pub keep_computer_awake: bool,
    /// 每个会话允许同时运行的后台 Agent 数量。
    pub background_agent_limit: u16,
    /// 内置终端使用的 CSS 字体族列表。
    pub terminal_font_family: String,
    /// Windows 内置终端使用的 Shell。
    pub terminal_shell: TerminalShell,
    /// 是否根据本机历史对话生成并在后续对话中使用本地记忆。
    pub local_memories: bool,
    /// 是否自动归档超过保留期且未置顶的对话。
    pub auto_archive_conversations: bool,
    /// 自动归档保留天数。
    pub archive_retention_days: u16,
    /// WebFetch 与 WebSearch 使用的兼容服务基础 URL；为空时使用内置服务。
    pub web_service_url: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::initial()
    }
}

/// 已读取并校验的当前应用设置。
pub struct SettingsLoad {
    /// 解析成功时的完整设置；原文件损坏、schema 不支持或值非法时为首次启动默认值。
    pub settings: AppSettings,
    /// 非致命诊断：被忽略的未知或已移除字段说明。
    pub warnings: Vec<String>,
    /// 原文件无法按当前格式解析时的根因说明；此时 `settings` 为默认值且原文件未被改动。
    pub load_error: Option<String>,
}

/// 应用设置文件的持久化外壳。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSettingsFile {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// 当前完整设置字段。
    #[serde(flatten)]
    settings: AppSettings,
}

impl AppSettingsFile {
    /// 为当前设置构造完整持久化文件。
    fn from_settings(settings: &AppSettings) -> Self {
        Self {
            schema: APP_SETTINGS_SCHEMA.to_owned(),
            version: APP_SETTINGS_VERSION,
            settings: settings.clone(),
        }
    }

    /// 校验文件身份并返回当前设置。
    fn into_settings(self) -> Result<AppSettings> {
        if self.schema != APP_SETTINGS_SCHEMA || self.version != APP_SETTINGS_VERSION {
            anyhow::bail!("应用设置 schema 或版本不受支持");
        }
        self.settings.validate()?;
        Ok(self.settings)
    }
}

impl AppSettings {
    /// 构造首次启动设置。
    fn initial() -> Self {
        Self {
            interface_language: InterfaceLanguage::SimplifiedChinese,
            app_update_download_source: AppUpdateDownloadSource::Auto,
            chrome_hardware_acceleration: true,
            sidebar_collapsed_project_ids: Vec::new(),
            project_directory: String::new(),
            task_notifications: true,
            show_thinking_process: true,
            notification_sound: true,
            keep_computer_awake: true,
            background_agent_limit: DEFAULT_BACKGROUND_AGENT_LIMIT,
            terminal_font_family: DEFAULT_TERMINAL_FONT_FAMILY.to_owned(),
            terminal_shell: TerminalShell::Auto,
            local_memories: true,
            auto_archive_conversations: true,
            archive_retention_days: 7,
            web_service_url: String::new(),
        }
    }

    /// 校验设置中不能仅靠类型系统表达的约束。
    fn validate(&self) -> Result<()> {
        if !self.project_directory.is_empty() {
            let project_directory = Path::new(&self.project_directory);
            if self.project_directory.trim() != self.project_directory
                || self.project_directory.chars().any(char::is_control)
                || !project_directory.is_absolute()
                || project_directory
                    .components()
                    .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
            {
                anyhow::bail!("默认项目保存位置必须是规范的绝对路径");
            }
        }
        if !(1..=365).contains(&self.archive_retention_days) {
            anyhow::bail!("归档保留天数必须在 1 到 365 之间");
        }
        if !(1..=MAX_BACKGROUND_AGENT_LIMIT).contains(&self.background_agent_limit) {
            anyhow::bail!("后台 Agent 并发数量必须在 1 到 {MAX_BACKGROUND_AGENT_LIMIT} 之间");
        }
        if self.terminal_font_family.is_empty()
            || self.terminal_font_family.len() > 256
            || self.terminal_font_family.trim() != self.terminal_font_family
            || self.terminal_font_family.chars().any(char::is_control)
        {
            anyhow::bail!("终端字体必须是 1 到 256 个字符的有效字体族列表");
        }
        if !self.web_service_url.is_empty() {
            WebServiceConfig::new(&self.web_service_url)
                .map_err(|error| anyhow::anyhow!("兼容服务基础 URL 无效：{error}"))?;
        }
        let mut project_ids = HashSet::new();
        for project_id in &self.sidebar_collapsed_project_ids {
            let mut characters = project_id.chars();
            if project_id.len() > 128
                || !characters
                    .next()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
                || !project_id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
            {
                anyhow::bail!("折叠项目标识格式无效：{project_id}");
            }
            if !project_ids.insert(project_id) {
                anyhow::bail!("折叠项目标识不能重复：{project_id}");
            }
        }
        Ok(())
    }
}

/// 应用设置局部更新；只允许修改当前界面实际暴露的字段。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AppSettingsPatch {
    /// 更新界面语言。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub interface_language: Option<InterfaceLanguage>,
    /// 更新应用安装包下载源偏好。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub app_update_download_source: Option<AppUpdateDownloadSource>,
    /// 更新 Windows WebView2 硬件加速开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub chrome_hardware_acceleration: Option<bool>,
    /// 更新侧栏折叠项目标识。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub sidebar_collapsed_project_ids: Option<Vec<String>>,
    /// 更新新项目的默认父目录。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub project_directory: Option<String>,
    /// 更新任务桌面通知开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub task_notifications: Option<bool>,
    /// 更新思考过程内容块显示开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub show_thinking_process: Option<bool>,
    /// 更新任务通知声音开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub notification_sound: Option<bool>,
    /// 更新阻止空闲睡眠开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub keep_computer_awake: Option<bool>,
    /// 更新每个会话的后台 Agent 并发数量。
    #[serde(default, deserialize_with = "deserialize_background_agent_limit")]
    pub background_agent_limit: Option<u16>,
    /// 更新内置终端字体族列表。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub terminal_font_family: Option<String>,
    /// 更新 Windows 内置终端使用的 Shell。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub terminal_shell: Option<TerminalShell>,
    /// 更新本地记忆总开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub local_memories: Option<bool>,
    /// 更新自动归档开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub auto_archive_conversations: Option<bool>,
    /// 更新自动归档保留天数。
    #[serde(default, deserialize_with = "deserialize_archive_retention_days")]
    pub archive_retention_days: Option<u16>,
    /// 更新 WebFetch 与 WebSearch 的兼容服务基础 URL；空字符串表示禁用。
    #[serde(default, deserialize_with = "deserialize_web_service_url")]
    pub web_service_url: Option<String>,
}

/// 将缺失补丁字段解析为空，同时拒绝调用方显式传入 null。
fn deserialize_optional_value<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn deserialize_archive_retention_days<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u16::deserialize(deserializer)?;
    if !(1..=365).contains(&value) {
        return Err(serde::de::Error::custom("归档保留天数必须在 1 到 365 之间"));
    }
    Ok(Some(value))
}

fn deserialize_background_agent_limit<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u16::deserialize(deserializer)?;
    if !(1..=MAX_BACKGROUND_AGENT_LIMIT).contains(&value) {
        return Err(serde::de::Error::custom(format!(
            "后台 Agent 并发数量必须在 1 到 {MAX_BACKGROUND_AGENT_LIMIT} 之间"
        )));
    }
    Ok(Some(value))
}

/// 校验并规范化网络工具补丁；空字符串明确表示关闭网络工具。
fn deserialize_web_service_url<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    normalize_web_service_url(&value)
        .map(Some)
        .map_err(|error| serde::de::Error::custom(error.to_string()))
}

/// 按网络工具共享的严格配置校验基础 URL，并返回去除首尾空白的持久值。
fn normalize_web_service_url(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    WebServiceConfig::new(value)
        .map_err(|error| anyhow::anyhow!("兼容服务基础 URL 无效：{error}"))?;
    Ok(value.to_owned())
}

/// 未覆盖服务地址时使用应用内置的 Tavily 兼容服务。
const DEFAULT_WEB_SERVICE_URL: &str = "https://tavily.claude-code-best.win";

/// 按当前设置创建网络工具配置；空字符串使用内置服务。
pub(crate) fn web_service_config(value: &str) -> Result<Option<WebServiceConfig>> {
    let value = value.trim();
    WebServiceConfig::new(if value.is_empty() {
        DEFAULT_WEB_SERVICE_URL
    } else {
        value
    })
    .map(Some)
    .map_err(|error| anyhow::anyhow!("兼容服务基础 URL 无效：{error}"))
}

impl AppSettings {
    /// 返回当前设置对应的网络工具配置；未配置时使用内置服务。
    pub(crate) fn web_service_config(&self) -> Result<Option<WebServiceConfig>> {
        web_service_config(&self.web_service_url)
    }
}

impl AppSettingsPatch {
    /// 返回补丁中的网络工具热更新；未提供字段时保持现有运行时配置。
    pub(crate) fn web_service_config_update(&self) -> Result<Option<Option<WebServiceConfig>>> {
        self.web_service_url
            .as_deref()
            .map(web_service_config)
            .transpose()
    }
}

/// 返回当前完整应用设置。
pub fn get(app: &AppHandle) -> Result<AppSettings> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let mut settings = load_unlocked(app).settings;
    apply_runtime_defaults(app, &mut settings)?;
    Ok(settings)
}

/// 启动时读取当前设置；任何解析失败都回退首次启动默认值并继续启动，
/// 原文件保持原样。缺失字段回退默认值；未知或已移除字段忽略并记录警告。
pub fn load_for_startup(app: &AppHandle) -> Result<SettingsLoad> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let mut loaded = load_unlocked(app);
    apply_runtime_defaults(app, &mut loaded.settings)?;
    Ok(loaded)
}

/// 应用并保存一个严格类型的设置补丁。
pub fn set(app: &AppHandle, patch: AppSettingsPatch) -> Result<AppSettings> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let path = settings_path(app)?;
    let loaded = load_unlocked(app);
    if let Some(load_error) = &loaded.load_error {
        anyhow::bail!("应用设置文件无法解析，已保留原文件且未保存本次修改：{load_error}");
    }
    let mut settings = loaded.settings;
    apply_runtime_defaults(app, &mut settings)?;
    if let Some(value) = patch.interface_language {
        settings.interface_language = value;
    }
    if let Some(value) = patch.app_update_download_source {
        settings.app_update_download_source = value;
    }
    if let Some(value) = patch.chrome_hardware_acceleration {
        settings.chrome_hardware_acceleration = value;
    }
    if let Some(value) = patch.sidebar_collapsed_project_ids {
        settings.sidebar_collapsed_project_ids = value;
    }
    if let Some(value) = patch.project_directory {
        if value.is_empty() {
            anyhow::bail!("默认项目保存位置不能为空");
        }
        settings.project_directory = path_text_to_frontend(&value);
    }
    if let Some(value) = patch.task_notifications {
        settings.task_notifications = value;
    }
    if let Some(value) = patch.show_thinking_process {
        settings.show_thinking_process = value;
    }
    if let Some(value) = patch.notification_sound {
        settings.notification_sound = value;
    }
    if let Some(value) = patch.keep_computer_awake {
        settings.keep_computer_awake = value;
    }
    if let Some(value) = patch.background_agent_limit {
        settings.background_agent_limit = value;
    }
    if let Some(value) = patch.terminal_font_family {
        settings.terminal_font_family = value;
    }
    if let Some(value) = patch.terminal_shell {
        settings.terminal_shell = value;
    }
    if let Some(value) = patch.local_memories {
        settings.local_memories = value;
    }
    if let Some(value) = patch.auto_archive_conversations {
        settings.auto_archive_conversations = value;
    }
    if let Some(value) = patch.archive_retention_days {
        settings.archive_retention_days = value;
    }
    if let Some(value) = patch.web_service_url {
        settings.web_service_url = normalize_web_service_url(&value)?;
    }
    settings.validate()?;
    save_to_path(&path, &settings)?;
    Ok(settings)
}

/// 返回操作系统文档目录下的 KeenCode 默认项目父目录。
pub(crate) fn default_project_directory(app: &AppHandle) -> Result<PathBuf> {
    Ok(app
        .path()
        .document_dir()
        .context("无法确定当前用户的文档目录")?
        .join("KeenCode"))
}

/// 首次读取设置时把平台相关默认值解析成前端可展示的绝对路径。
fn apply_runtime_defaults(app: &AppHandle, settings: &mut AppSettings) -> Result<()> {
    if settings.project_directory.is_empty() {
        settings.project_directory = path_to_frontend(&default_project_directory(app)?);
    } else {
        settings.project_directory = path_text_to_frontend(&settings.project_directory);
    }
    settings.validate()
}

/// 读取当前设置文件；缺失文件或无法解析时回退首次启动默认值，
/// 根因记录在 [`SettingsLoad::load_error`]，原文件保持原样。
fn load_unlocked(app: &AppHandle) -> SettingsLoad {
    let path = match settings_path(app) {
        Ok(path) => path,
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: Vec::new(),
                load_error: Some(format!("无法确定应用设置路径：{error:#}")),
            };
        }
    };
    load_from_path(&path)
}

/// 从磁盘读取当前设置文件；任何失败都回退首次启动默认值，不改写原文件。
fn load_from_path(path: &Path) -> SettingsLoad {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: Vec::new(),
                load_error: None,
            };
        }
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: Vec::new(),
                load_error: Some(format!("无法检查应用设置 {}：{error}", path.display())),
            };
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return SettingsLoad {
            settings: AppSettings::initial(),
            warnings: Vec::new(),
            load_error: Some(format!("应用设置路径不是普通文件：{}", path.display())),
        };
    }
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: Vec::new(),
                load_error: Some(format!("读取应用设置失败 {}：{error}", path.display())),
            };
        }
    };
    load_from_content(&content)
}

/// 解析当前设置文件内容：缺失字段回退首次启动默认值，未知或已移除字段
/// 忽略并记录警告；损坏、schema 不支持或字段值非法时返回默认设置并携带根因。
fn load_from_content(content: &str) -> SettingsLoad {
    let value: serde_json::Value = match serde_json::from_str(content) {
        Ok(value) => value,
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: Vec::new(),
                load_error: Some(format!("设置不是有效 JSON：{error}")),
            };
        }
    };
    let warnings = unknown_field_warnings(&value);
    let file: AppSettingsFile = match serde_json::from_value(value) {
        Ok(file) => file,
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings,
                load_error: Some(format!("设置文件结构无效：{error}")),
            };
        }
    };
    let settings = match file.into_settings() {
        Ok(settings) => settings,
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings,
                load_error: Some(format!("{error:#}")),
            };
        }
    };
    SettingsLoad {
        settings,
        warnings,
        load_error: None,
    }
}

/// 对比当前 schema 找出设置文件中的未知或已移除字段。
fn unknown_field_warnings(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let Ok(current) = serde_json::to_value(AppSettingsFile::from_settings(&AppSettings::initial()))
    else {
        return Vec::new();
    };
    let Some(known) = current.as_object() else {
        return Vec::new();
    };
    let unknown: Vec<&str> = object
        .keys()
        .filter(|key| !known.contains_key(*key))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "设置文件包含未知或已移除字段，已忽略：{}",
            unknown.join(", ")
        )]
    }
}

/// 将完整设置保存到指定路径；独立入口用于验证重复原子覆盖。
fn save_to_path(path: &Path, settings: &AppSettings) -> Result<()> {
    settings.validate()?;
    let file = AppSettingsFile::from_settings(settings);
    let bytes = serde_json::to_vec_pretty(&file).context("序列化应用设置失败")?;
    crate::storage::atomic_write_private(path, &bytes)
        .with_context(|| format!("保存应用设置失败：{}", path.display()))
}

/// 返回应用设置文件路径。
fn settings_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join("settings.json"))
}

/// 读取 WebView 创建前所需的当前设置；缺失文件使用首次启动默认值。
#[cfg(any(target_os = "windows", test))]
fn load_before_start(path: &Path) -> Result<AppSettings> {
    Ok(load_from_path(path).settings)
}

/// 在 Windows WebView2 创建前应用硬件加速偏好。
#[cfg(target_os = "windows")]
pub fn configure_hardware_acceleration_before_start() {
    fn configure() -> Result<()> {
        let path = crate::storage::root_dir_before_start()?.join("settings.json");
        let settings = load_before_start(&path)?;
        if settings.chrome_hardware_acceleration {
            return Ok(());
        }
        let mut arguments = match std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => String::new(),
            Err(std::env::VarError::NotUnicode(_)) => {
                anyhow::bail!("WebView2 启动参数不是有效的 Unicode 文本")
            }
        };
        if !arguments
            .split_whitespace()
            .any(|item| item == "--disable-gpu")
        {
            if !arguments.is_empty() {
                arguments.push(' ');
            }
            arguments.push_str("--disable-gpu");
            // SAFETY: 仅在 Tauri 和其他线程启动前修改进程环境。
            unsafe {
                std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", arguments);
            }
        }
        Ok(())
    }

    configure().unwrap_or_else(|error| panic!("预读 Windows 应用设置失败：{error:#}"));
}

/// 非 Windows 平台不需要 WebView2 启动参数。
#[cfg(not(target_os = "windows"))]
pub fn configure_hardware_acceleration_before_start() {}

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, AppSettingsFile, AppSettingsPatch, AppUpdateDownloadSource,
        DEFAULT_BACKGROUND_AGENT_LIMIT, DEFAULT_TERMINAL_FONT_FAMILY, DEFAULT_WEB_SERVICE_URL,
        InterfaceLanguage, MAX_BACKGROUND_AGENT_LIMIT, TerminalShell, load_before_start,
        load_from_content, load_from_path, save_to_path, web_service_config,
    };
    use std::fs;

    /// 当前设置文件必须包含固定 schema/version；缺失字段回退默认值，
    /// 未知或已移除字段忽略并产生警告，不阻断启动。
    #[test]
    fn settings_schema_tolerates_drift_with_defaults_and_warnings() {
        let settings = AppSettings::initial();
        let valid = serde_json::to_string(&AppSettingsFile::from_settings(&settings))
            .expect("当前设置应可编码");
        let loaded = load_from_content(&valid);
        assert!(loaded.load_error.is_none());
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.settings, AppSettings::initial());
        assert_eq!(
            loaded.settings.app_update_download_source,
            AppUpdateDownloadSource::Auto
        );
        assert_eq!(
            loaded.settings.background_agent_limit,
            DEFAULT_BACKGROUND_AGENT_LIMIT
        );
        assert_eq!(
            loaded.settings.terminal_font_family,
            DEFAULT_TERMINAL_FONT_FAMILY
        );
        assert_eq!(loaded.settings.terminal_shell, TerminalShell::Auto);
        assert!(loaded.settings.web_service_url.is_empty());

        // 未知或已移除字段（例如未来版本新增的字段）只忽略并警告。
        let mut unknown: serde_json::Value = serde_json::from_str(&valid).unwrap();
        unknown["oldSetting"] = serde_json::Value::Bool(true);
        let with_unknown = load_from_content(&unknown.to_string());
        assert!(with_unknown.load_error.is_none());
        assert!(with_unknown.warnings[0].contains("oldSetting"));
        assert_eq!(with_unknown.settings, AppSettings::initial());

        // 升级新增字段后，旧版本设置文件缺失新字段时回退首次启动默认值。
        let mut missing_new_field: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str::<serde_json::Value>(&valid)
                .unwrap()
                .as_object()
                .unwrap()
                .clone();
        missing_new_field.remove("showThinkingProcess");
        let upgraded = load_from_content(&serde_json::Value::Object(missing_new_field).to_string());
        assert!(upgraded.load_error.is_none());
        assert!(upgraded.settings.show_thinking_process);
        let mut missing_memories: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str::<serde_json::Value>(&valid)
                .unwrap()
                .as_object()
                .unwrap()
                .clone();
        missing_memories.remove("localMemories");
        let with_default_memories =
            load_from_content(&serde_json::Value::Object(missing_memories).to_string());
        assert!(with_default_memories.load_error.is_none());
        assert!(with_default_memories.settings.local_memories);
        let empty = load_from_content("{}");
        assert!(empty.load_error.is_some());
        assert_eq!(empty.settings, AppSettings::initial());

        // schema 或版本不受支持仍然按读取失败处理并回退默认值。
        let mut drifted: serde_json::Value = serde_json::from_str(&valid).unwrap();
        drifted["version"] = serde_json::Value::from(u32::MAX);
        let mismatched = load_from_content(&drifted.to_string());
        assert!(mismatched.load_error.is_some());
        assert_eq!(mismatched.settings, AppSettings::initial());

        let invalid = AppSettings {
            sidebar_collapsed_project_ids: vec![" project-1 ".to_owned()],
            ..settings.clone()
        };
        assert!(invalid.validate().is_err());
        let invalid_directory = AppSettings {
            project_directory: "relative/projects".to_owned(),
            ..AppSettings::initial()
        };
        assert!(invalid_directory.validate().is_err());
        let invalid_terminal_font = AppSettings {
            terminal_font_family: " Maple Mono NF CN ".to_owned(),
            ..AppSettings::initial()
        };
        assert!(invalid_terminal_font.validate().is_err());
    }

    /// 已存在但损坏的设置必须回退默认设置继续可用，并且不得覆盖或修复原文件。
    #[test]
    fn invalid_settings_fall_back_to_defaults_without_replacement() {
        let directory = tempfile::tempdir().expect("创建测试目录");
        let path = directory.path().join("settings.json");
        let original = "{ invalid user settings";
        fs::write(&path, original).expect("写入损坏设置");

        let loaded = load_from_path(&path);
        assert_eq!(loaded.settings, AppSettings::initial());
        assert!(loaded.load_error.is_some());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// 非普通文件不能作为设置文件，读取回退默认值且原路径保持原样。
    #[test]
    fn non_regular_settings_path_falls_back_to_defaults() {
        let directory = tempfile::tempdir().expect("创建测试目录");
        let path = directory.path().join("settings.json");
        fs::create_dir(&path).expect("创建不可备份的设置目录");

        let loaded = load_from_path(&path);
        assert_eq!(loaded.settings, AppSettings::initial());
        assert!(loaded.load_error.is_some());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// 设置符号链接不是当前配置文件，读取不跟随目标且原路径保持原样。
    #[cfg(unix)]
    #[test]
    fn symlinked_settings_are_not_followed_or_replaced() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("创建测试目录");
        let target = directory.path().join("outside.json");
        let path = directory.path().join("settings.json");
        fs::write(&target, "{broken target").expect("写入链接目标");
        symlink(&target, &path).expect("创建设置符号链接");

        let loaded = load_from_path(&path);
        assert_eq!(loaded.settings, AppSettings::initial());
        assert!(loaded.load_error.is_some());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "{broken target");
    }

    /// 活跃保存路径必须能连续覆盖同一文件（Windows 回归）。
    #[test]
    fn settings_save_replaces_existing_file_repeatedly() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("settings.json");
        let mut settings = AppSettings::initial();

        save_to_path(&path, &settings).expect("首次保存设置");
        save_to_path(&path, &settings).expect("第二次应覆盖已有设置");
        settings.keep_computer_awake = true;
        save_to_path(&path, &settings).expect("第三次仍应覆盖已有设置");

        let saved: AppSettingsFile =
            serde_json::from_slice(&fs::read(&path).expect("读取保存结果")).unwrap();
        assert!(saved.settings.keep_computer_awake);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn update_download_sources_use_the_frontend_contract_values() {
        assert_eq!(
            serde_json::to_value(AppUpdateDownloadSource::Auto).unwrap(),
            "auto"
        );
        assert_eq!(
            serde_json::to_value(AppUpdateDownloadSource::Github).unwrap(),
            "github"
        );
        assert_eq!(
            serde_json::to_value(AppUpdateDownloadSource::ChinaMirror).unwrap(),
            "chinaMirror"
        );
    }

    #[test]
    fn interface_languages_use_the_frontend_contract_values() {
        assert_eq!(
            serde_json::to_value(InterfaceLanguage::SimplifiedChinese).unwrap(),
            "zh"
        );
        assert_eq!(
            serde_json::to_value(InterfaceLanguage::TraditionalChinese).unwrap(),
            "zh-TW"
        );
        assert_eq!(
            serde_json::to_value(InterfaceLanguage::English).unwrap(),
            "en"
        );
        assert!(serde_json::from_str::<AppSettings>(r#"{"interfaceLanguage":"fr"}"#).is_err());
    }

    /// 补丁允许字段缺失，但拒绝 null、未知字段和错误类型。
    #[test]
    fn settings_patch_rejects_ambiguous_values() {
        assert!(serde_json::from_str::<AppSettingsPatch>("{}").is_ok());
        assert!(
            serde_json::from_str::<AppSettingsPatch>(r#"{"chromeHardwareAcceleration": false}"#)
                .is_ok()
        );
        for invalid in [
            r#"{"interfaceLanguage": null}"#,
            r#"{"chromeHardwareAcceleration": null}"#,
            r#"{"sidebarCollapsedProjectIds": null}"#,
            r#"{"projectDirectory": null}"#,
            r#"{"taskNotifications": null}"#,
            r#"{"showThinkingProcess": null}"#,
            r#"{"notificationSound": "true"}"#,
            r#"{"keepComputerAwake": null}"#,
            r#"{"backgroundAgentLimit": 0}"#,
            r#"{"backgroundAgentLimit": 1000}"#,
            r#"{"terminalFontFamily": null}"#,
            r#"{"terminalShell": "wsl"}"#,
            r#"{"localMemories": null}"#,
            r#"{"autoArchiveConversations": null}"#,
            r#"{"archiveRetentionDays": 0}"#,
            r#"{"webServiceUrl": null}"#,
            r#"{"oldSetting": true}"#,
        ] {
            assert!(serde_json::from_str::<AppSettingsPatch>(invalid).is_err());
        }
        assert!(
            serde_json::from_str::<AppSettingsPatch>(&format!(
                r#"{{"backgroundAgentLimit": {MAX_BACKGROUND_AGENT_LIMIT}}}"#
            ))
            .is_ok()
        );
    }

    /// 兼容服务 URL 使用当前唯一设置结构往返，并复用工具层规范化配置。
    #[test]
    fn web_service_url_round_trips_and_builds_config() {
        let mut settings = AppSettings::initial();
        settings.web_service_url = "http://127.0.0.1:3456/compat".to_owned();
        let encoded = serde_json::to_string(&AppSettingsFile::from_settings(&settings))
            .expect("兼容服务设置应可编码");
        let loaded = load_from_content(&encoded);
        assert!(loaded.load_error.is_none());
        assert_eq!(
            loaded.settings.web_service_url,
            "http://127.0.0.1:3456/compat"
        );
        assert_eq!(
            loaded
                .settings
                .web_service_config()
                .expect("兼容服务配置应可创建")
                .expect("非空 URL 应启用网络工具")
                .base_url()
                .as_str(),
            "http://127.0.0.1:3456/compat/"
        );
    }

    /// 兼容服务 URL 只能通过 WebServiceConfig 的严格规则校验。
    #[test]
    fn web_service_url_rejects_invalid_values() {
        for value in [
            "not-a-url",
            "ftp://127.0.0.1/compat",
            "http://user:password@127.0.0.1/compat",
            "http://127.0.0.1/compat?token=hidden",
            "http://127.0.0.1/compat#fragment",
        ] {
            let invalid = AppSettings {
                web_service_url: value.to_owned(),
                ..AppSettings::initial()
            };
            assert!(invalid.validate().is_err(), "应拒绝 URL：{value}");
            let patch = format!(r#"{{"webServiceUrl":{value:?}}}"#);
            assert!(
                serde_json::from_str::<AppSettingsPatch>(&patch).is_err(),
                "补丁应拒绝 URL：{value}"
            );
        }
    }

    /// 默认设置和清空地址的热更新均使用同一个内置服务。
    #[test]
    fn empty_web_service_url_uses_builtin_service() {
        for value in ["", "  "] {
            let config = web_service_config(value).unwrap().unwrap();
            assert_eq!(
                config.base_url().as_str(),
                format!("{DEFAULT_WEB_SERVICE_URL}/")
            );
        }
        let patch = serde_json::from_str::<AppSettingsPatch>(r#"{"webServiceUrl":""}"#).unwrap();
        let config = patch.web_service_config_update().unwrap().unwrap().unwrap();
        assert_eq!(
            config.base_url(),
            AppSettings::initial()
                .web_service_config()
                .unwrap()
                .unwrap()
                .base_url()
        );
        assert!(
            serde_json::from_str::<AppSettingsPatch>("{}")
                .unwrap()
                .web_service_config_update()
                .unwrap()
                .is_none()
        );
    }

    /// WebView 创建前的读取不阻断启动：缺失文件使用首次启动默认值，
    /// 损坏文件回退默认值。
    #[test]
    fn before_start_settings_fall_back_to_defaults() {
        let directory = tempfile::tempdir().expect("创建测试目录");
        let path = directory.path().join("settings.json");

        let initial = load_before_start(&path).expect("缺失文件应使用首次启动设置");
        assert!(initial.chrome_hardware_acceleration);
        assert!(initial.local_memories, "本地记忆必须默认开启");

        fs::write(&path, "{broken").expect("写入损坏设置");
        let fallback = load_before_start(&path).expect("损坏文件应回退默认设置");
        assert!(fallback.chrome_hardware_acceleration);
        assert!(fallback.local_memories, "本地记忆必须默认开启");

        let mut settings = AppSettings::initial();
        settings.chrome_hardware_acceleration = false;
        let valid =
            serde_json::to_vec(&AppSettingsFile::from_settings(&settings)).expect("写入当前设置");
        fs::write(&path, valid).expect("写入当前设置");
        let settings = load_before_start(&path).expect("读取当前设置");
        assert!(!settings.chrome_hardware_acceleration);
    }
}
