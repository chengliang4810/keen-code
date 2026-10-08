use anyhow::Result;
use keencode_tools::WebServiceConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use crate::native_paths::NativePaths;
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
/// GPUI 终端设置只保存一个实际字体族名称；中文 fallback 由字体构造层负责。
pub const DEFAULT_TERMINAL_FONT_FAMILY: &str = if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "JetBrains Mono"
};
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
    /// 关闭主窗口时是隐藏到系统托盘常驻，还是直接退出应用。
    pub close_to_tray: bool,
    /// 所有对话共享的设备级后台 Agent 并发上限。
    pub background_agent_limit: u16,
    /// 内置终端使用的单一真实 GPUI 字体族名称。
    pub terminal_font_family: String,
    /// Windows 内置终端使用的 Shell。
    pub terminal_shell: TerminalShell,
    /// 是否让新建 PTY 加载系统终端的登录 profile；关闭时仍保留进程基础环境。
    pub terminal_inherit_system_profile: bool,
    /// 用户明确配置的 HTTP/HTTPS 出口代理；None 表示沿用进程/系统代理发现。
    pub http_proxy: Option<String>,
    /// 用户明确配置的代理绕过规则；空字符串表示清除绕过规则。
    pub http_proxy_no_proxy: Option<String>,
    /// 是否根据本机历史对话生成并在后续对话中使用本地记忆。
    pub local_memories: bool,
    /// 是否自动归档超过保留期且未置顶的对话。
    pub auto_archive_conversations: bool,
    /// 自动归档保留天数。
    pub archive_retention_days: u16,
    /// WebFetch 与 WebSearch 使用的兼容服务基础 URL；为空时使用内置服务。
    pub web_service_url: String,
}

/// 设置页对应用设置文件的一次字段级更新。
///
/// `None` 表示保持现值；代理字段使用嵌套 `Option` 区分“保持不变”和“明确清空”。
/// 该补丁不包含界面语言、思考显示或归档字段：当前 Native UI 没有对应的完整
/// 消费链，不能把记忆提示词语言伪装成全局界面语言，也不能保存一个没有消费者的开关。
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesPatch {
    pub(crate) app_update_download_source: Option<AppUpdateDownloadSource>,
    pub(crate) project_directory: Option<String>,
    pub(crate) task_notifications: Option<bool>,
    pub(crate) notification_sound: Option<bool>,
    pub(crate) keep_computer_awake: Option<bool>,
    pub(crate) close_to_tray: Option<bool>,
    pub(crate) background_agent_limit: Option<u16>,
    pub(crate) http_proxy: Option<Option<String>>,
    pub(crate) http_proxy_no_proxy: Option<Option<String>>,
    pub(crate) local_memories: Option<bool>,
    /// 常规页已有的终端字段也通过同一个文件级补丁提交，避免一次 UI 操作
    /// 在读取和写入之间覆盖另一项刚刚保存的设置。
    pub(crate) terminal_shell: Option<TerminalShell>,
    pub(crate) terminal_font_family: Option<String>,
    pub(crate) terminal_inherit_system_profile: Option<bool>,
}

impl PreferencesPatch {
    pub(crate) fn is_empty(&self) -> bool {
        self.app_update_download_source.is_none()
            && self.project_directory.is_none()
            && self.task_notifications.is_none()
            && self.notification_sound.is_none()
            && self.keep_computer_awake.is_none()
            && self.close_to_tray.is_none()
            && self.background_agent_limit.is_none()
            && self.http_proxy.is_none()
            && self.http_proxy_no_proxy.is_none()
            && self.local_memories.is_none()
            && self.terminal_shell.is_none()
            && self.terminal_font_family.is_none()
            && self.terminal_inherit_system_profile.is_none()
    }

    fn apply_to(&self, settings: &mut AppSettings) {
        if let Some(value) = self.app_update_download_source {
            settings.app_update_download_source = value;
        }
        if let Some(value) = &self.project_directory {
            settings.project_directory = if value.trim().is_empty() {
                String::new()
            } else {
                path_text_to_frontend(value.trim())
            };
        }
        if let Some(value) = self.task_notifications {
            settings.task_notifications = value;
        }
        if let Some(value) = self.notification_sound {
            settings.notification_sound = value;
        }
        if let Some(value) = self.keep_computer_awake {
            settings.keep_computer_awake = value;
        }
        if let Some(value) = self.close_to_tray {
            settings.close_to_tray = value;
        }
        if let Some(value) = self.background_agent_limit {
            settings.background_agent_limit = value;
        }
        if let Some(value) = &self.http_proxy {
            settings.http_proxy = value
                .as_ref()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
        }
        if let Some(value) = &self.http_proxy_no_proxy {
            settings.http_proxy_no_proxy = value
                .as_ref()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
        }
        if let Some(value) = self.local_memories {
            settings.local_memories = value;
        }
        if let Some(value) = self.terminal_shell {
            settings.terminal_shell = value;
        }
        if let Some(value) = &self.terminal_font_family {
            settings.terminal_font_family = if value.trim().is_empty() {
                DEFAULT_TERMINAL_FONT_FAMILY.to_owned()
            } else {
                value.trim().to_owned()
            };
        }
        if let Some(value) = self.terminal_inherit_system_profile {
            settings.terminal_inherit_system_profile = value;
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::initial()
    }
}

/// 已读取并校验的当前应用设置。
struct SettingsLoad {
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
        let settings = self.settings;
        settings.validate()?;
        Ok(settings)
    }
}

fn is_generic_font_family(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "inherit"
            | "initial"
            | "unset"
            | "ui-monospace"
            | "monospace"
            | "system-ui"
            | "-apple-system"
            | "sans-serif"
            | "serif"
            | "cursive"
            | "fantasy"
    )
}

impl AppSettings {
    /// 构造首次启动设置。
    fn initial() -> Self {
        Self {
            interface_language: InterfaceLanguage::SimplifiedChinese,
            app_update_download_source: AppUpdateDownloadSource::Auto,
            sidebar_collapsed_project_ids: Vec::new(),
            project_directory: String::new(),
            task_notifications: true,
            show_thinking_process: true,
            notification_sound: true,
            keep_computer_awake: true,
            close_to_tray: true,
            background_agent_limit: DEFAULT_BACKGROUND_AGENT_LIMIT,
            terminal_font_family: DEFAULT_TERMINAL_FONT_FAMILY.to_owned(),
            terminal_shell: TerminalShell::Auto,
            terminal_inherit_system_profile: true,
            http_proxy: None,
            http_proxy_no_proxy: None,
            local_memories: false,
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
            || self.terminal_font_family.contains(',')
            || is_generic_font_family(&self.terminal_font_family)
            || self.terminal_font_family.chars().any(char::is_control)
        {
            anyhow::bail!("终端字体必须是 1 到 256 个字符的真实字体族名");
        }
        if let Some(proxy) = &self.http_proxy {
            crate::network_proxy::validate_configured_proxy(proxy)?;
        }
        if let Some(no_proxy) = &self.http_proxy_no_proxy
            && (no_proxy.len() > 8192 || no_proxy.chars().any(char::is_control))
        {
            anyhow::bail!("代理绕过规则长度或字符无效");
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

/// 返回当前完整应用设置。
pub fn get(paths: &NativePaths) -> Result<AppSettings> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let loaded = load_unlocked(paths);
    report_load_diagnostics(&loaded);
    let mut settings = loaded.settings;
    apply_runtime_defaults(paths, &mut settings)?;
    Ok(settings)
}

/// 在同一把设置锁内读取、应用、校验并原子写入字段级偏好补丁。
///
/// 返回值是已展开运行时默认目录的冷启动快照；文件中仍保留空目录表示“使用
/// 平台默认目录”，因此清空项目目录不会把平台路径永久复制进设置文件。
pub(crate) fn update_preferences(
    paths: &NativePaths,
    patch: PreferencesPatch,
) -> Result<AppSettings> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let loaded = load_unlocked(paths);
    if let Some(load_error) = loaded.load_error {
        anyhow::bail!("应用设置文件无法解析，已保留原文件且未保存本次修改：{load_error}");
    }
    let mut settings = loaded.settings;
    patch.apply_to(&mut settings);
    settings.validate()?;
    let path = settings_path(paths)?;
    save_to_path(&path, &settings)?;
    apply_runtime_defaults(paths, &mut settings)?;
    Ok(settings)
}

/// 返回操作系统文档目录下的 KeenCode 默认项目父目录。
pub(crate) fn default_project_directory(paths: &NativePaths) -> Result<PathBuf> {
    if paths.documents_dir.as_os_str().is_empty() {
        anyhow::bail!("无法确定当前用户的文档目录");
    }
    Ok(paths.documents_dir.join("KeenCode"))
}

/// 首次读取设置时把平台相关默认值解析成前端可展示的绝对路径。
fn apply_runtime_defaults(paths: &NativePaths, settings: &mut AppSettings) -> Result<()> {
    if settings.project_directory.is_empty() {
        settings.project_directory = path_to_frontend(&default_project_directory(paths)?);
    } else {
        settings.project_directory = path_text_to_frontend(&settings.project_directory);
    }
    settings.validate()
}

/// 读取当前设置文件；缺失文件或无法解析时回退首次启动默认值，
/// 根因记录在 [`SettingsLoad::load_error`]，原文件保持原样。
fn load_unlocked(paths: &NativePaths) -> SettingsLoad {
    let path = match settings_path(paths) {
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

/// 将已通过校验的完整设置写入唯一的应用设置文件。
fn save_to_path(path: &Path, settings: &AppSettings) -> Result<()> {
    settings.validate()?;
    let file = AppSettingsFile::from_settings(settings);
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| anyhow::anyhow!("序列化应用设置失败：{error}"))?;
    crate::storage::atomic_write_private(path, &bytes)
        .map_err(|error| anyhow::anyhow!("保存应用设置失败：{}：{error}", path.display()))
}

/// 返回应用设置文件路径。
fn settings_path(paths: &NativePaths) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(paths)?.join("settings.json"))
}

/// 读取原生宿主启动所需的当前设置；缺失文件使用首次启动默认值。
pub(crate) fn load_before_start(path: &Path) -> Result<AppSettings> {
    let loaded = load_from_path(path);
    report_load_diagnostics(&loaded);
    Ok(loaded.settings)
}

/// 读取阶段的诊断不阻断启动，但必须进入原生日志，避免损坏配置被静默吞掉。
fn report_load_diagnostics(loaded: &SettingsLoad) {
    for warning in &loaded.warnings {
        tracing::warn!(warning, "应用设置包含被忽略的字段");
    }
    if let Some(error) = &loaded.load_error {
        tracing::warn!(error, "应用设置读取失败，已回退默认值");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, AppSettingsFile, AppUpdateDownloadSource, DEFAULT_BACKGROUND_AGENT_LIMIT,
        DEFAULT_TERMINAL_FONT_FAMILY, DEFAULT_WEB_SERVICE_URL, InterfaceLanguage, PreferencesPatch,
        TerminalShell, load_before_start, load_from_content, load_from_path, update_preferences,
        web_service_config,
    };
    use crate::native_paths::NativePaths;
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
        assert!(!with_default_memories.settings.local_memories);
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

    #[test]
    fn preferences_patch_is_field_atomic_and_survives_cold_reload() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let paths = NativePaths::from_data_root(directory.path().to_owned());
        let project_directory = directory.path().to_string_lossy().into_owned();

        update_preferences(
            &paths,
            PreferencesPatch {
                app_update_download_source: Some(AppUpdateDownloadSource::ChinaMirror),
                project_directory: Some(project_directory),
                task_notifications: Some(false),
                notification_sound: Some(false),
                keep_computer_awake: Some(false),
                close_to_tray: Some(false),
                background_agent_limit: Some(3),
                http_proxy: Some(Some("http://127.0.0.1:7890".to_owned())),
                http_proxy_no_proxy: Some(Some("localhost,127.0.0.1".to_owned())),
                local_memories: Some(true),
                ..PreferencesPatch::default()
            },
        )
        .expect("偏好补丁应写入");

        let cold = super::get(&paths).expect("冷启动读取偏好");
        assert_eq!(
            cold.app_update_download_source,
            AppUpdateDownloadSource::ChinaMirror
        );
        assert!(!cold.task_notifications);
        assert!(!cold.notification_sound);
        assert!(!cold.keep_computer_awake);
        assert!(!cold.close_to_tray);
        assert_eq!(cold.background_agent_limit, 3);
        assert_eq!(cold.http_proxy.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(
            cold.http_proxy_no_proxy.as_deref(),
            Some("localhost,127.0.0.1")
        );
        assert!(cold.local_memories);

        update_preferences(
            &paths,
            PreferencesPatch {
                task_notifications: Some(true),
                ..PreferencesPatch::default()
            },
        )
        .expect("单字段补丁应写入");
        let merged = super::get(&paths).expect("读取合并后的偏好");
        assert!(merged.task_notifications);
        assert!(!merged.notification_sound);
        assert_eq!(merged.background_agent_limit, 3);
        assert!(merged.local_memories);
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
            web_service_config(&loaded.settings.web_service_url)
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
        let config = web_service_config("").unwrap().unwrap();
        assert_eq!(
            config.base_url().as_str(),
            format!("{DEFAULT_WEB_SERVICE_URL}/")
        );
    }

    /// 原生宿主的预读不阻断启动：缺失文件使用首次启动默认值，损坏文件回退默认值。
    #[test]
    fn before_start_settings_fall_back_to_defaults() {
        let directory = tempfile::tempdir().expect("创建测试目录");
        let path = directory.path().join("settings.json");

        let initial = load_before_start(&path).expect("缺失文件应使用首次启动设置");
        assert!(!initial.local_memories, "本地记忆必须默认关闭");

        fs::write(&path, "{broken").expect("写入损坏设置");
        let fallback = load_before_start(&path).expect("损坏文件应回退默认设置");
        assert!(!fallback.local_memories, "本地记忆必须默认关闭");

        let settings = AppSettings::initial();
        let valid =
            serde_json::to_vec(&AppSettingsFile::from_settings(&settings)).expect("写入当前设置");
        fs::write(&path, valid).expect("写入当前设置");
        let settings = load_before_start(&path).expect("读取当前设置");
        assert!(!settings.local_memories);
    }
}
