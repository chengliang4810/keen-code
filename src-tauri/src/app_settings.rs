use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::AppHandle;

/// 应用更新安装包的下载源偏好。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AppUpdateDownloadSource {
    #[default]
    Auto,
    Github,
    ChinaMirror,
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
                "所有自然语言内容必须使用简体中文；代码、路径、命令、标识符和专有名词保持原样。"
            }
            Self::TraditionalChinese => {
                "所有自然語言內容必須使用繁體中文；程式碼、路徑、命令、識別字和專有名詞保持原樣。"
            }
            Self::English => {
                "Write all natural-language content in English. Preserve code, paths, commands, identifiers, and proper nouns as written."
            }
        }
    }
}

/// 串行化应用设置读写。
static SETTINGS_IO_LOCK: Mutex<()> = Mutex::new(());

/// KeenCode 当前唯一的应用设置结构。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// 当前界面语言；首次启动默认简体中文。
    pub interface_language: InterfaceLanguage,
    /// 应用更新安装包的下载源偏好。
    #[serde(default)]
    pub app_update_download_source: AppUpdateDownloadSource,
    /// Windows WebView2 是否启用硬件加速。
    pub chrome_hardware_acceleration: bool,
    /// 是否展示每轮全部思考片段。
    pub show_full_thinking: bool,
    /// 侧栏中由用户折叠的项目标识。
    pub sidebar_collapsed_project_ids: Vec<String>,
    /// 是否发送任务完成、失败和等待确认的桌面通知。
    pub task_notifications: bool,
    /// 任务桌面通知是否请求播放系统默认提示音。
    pub notification_sound: bool,
    /// 是否阻止系统因用户空闲自动进入睡眠。
    pub keep_computer_awake: bool,
    /// 是否根据本机历史对话生成并在后续对话中使用本地记忆。
    pub local_memories: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::initial()
    }
}

/// 外部设置文件的容错读取结果。
pub struct SettingsLoad {
    pub settings: AppSettings,
    pub warnings: Vec<String>,
    /// 原文件已严重损坏；覆盖前必须先完成非破坏性备份。
    backup_required: bool,
}

impl AppSettings {
    /// 构造首次启动设置。
    fn initial() -> Self {
        Self {
            interface_language: InterfaceLanguage::SimplifiedChinese,
            app_update_download_source: AppUpdateDownloadSource::Auto,
            chrome_hardware_acceleration: true,
            show_full_thinking: true,
            sidebar_collapsed_project_ids: Vec::new(),
            task_notifications: true,
            notification_sound: true,
            keep_computer_awake: false,
            local_memories: true,
        }
    }

    /// 校验设置中不能仅靠类型系统表达的约束。
    fn validate(&self) -> Result<()> {
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
    /// 更新完整思考展示开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub show_full_thinking: Option<bool>,
    /// 更新侧栏折叠项目标识。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub sidebar_collapsed_project_ids: Option<Vec<String>>,
    /// 更新任务桌面通知开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub task_notifications: Option<bool>,
    /// 更新任务通知声音开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub notification_sound: Option<bool>,
    /// 更新阻止空闲睡眠开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub keep_computer_awake: Option<bool>,
    /// 更新本地记忆总开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub local_memories: Option<bool>,
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

/// 返回当前完整应用设置。
pub fn get(app: &AppHandle) -> Result<AppSettings> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let loaded = load_unlocked(app)?;
    for warning in &loaded.warnings {
        tracing::warn!(%warning, "应用设置已容错读取");
    }
    Ok(loaded.settings)
}

/// 启动时容错读取设置，并在安全时写回当前完整结构。
pub fn load_for_startup(app: &AppHandle) -> SettingsLoad {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let path = match settings_path(app) {
        Ok(path) => path,
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: vec![format!("无法解析设置文件路径，已使用默认设置: {error:#}")],
                backup_required: false,
            };
        }
    };
    repair_loaded_path(&path, load_compatible_path(&path))
}

/// 尝试将容错结果落回当前结构；严重损坏时必须先备份成功。
fn repair_loaded_path(path: &Path, mut loaded: SettingsLoad) -> SettingsLoad {
    if loaded.warnings.is_empty() {
        return loaded;
    }

    let may_write = if loaded.backup_required {
        match backup_invalid_settings(path) {
            Ok(backup_path) => {
                loaded.warnings.push(format!(
                    "原设置文件已备份后再写入默认配置: {}",
                    backup_path.display()
                ));
                true
            }
            Err(error) => {
                loaded.warnings.push(format!(
                    "无法备份原设置文件，本次仅在内存中使用默认设置且不会覆盖原文件: {error:#}"
                ));
                false
            }
        }
    } else {
        true
    };
    if may_write && let Err(error) = save_to_path(path, &loaded.settings) {
        loaded
            .warnings
            .push(format!("无法将容错后的当前设置写回文件: {error:#}"));
    }
    loaded
}

/// 应用并保存一个严格类型的设置补丁。
pub fn set(app: &AppHandle, patch: AppSettingsPatch) -> Result<AppSettings> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let path = settings_path(app)?;
    let loaded = load_compatible_path(&path);
    for warning in &loaded.warnings {
        tracing::warn!(%warning, "保存前已容错读取应用设置");
    }
    if loaded.backup_required {
        let backup_path = backup_invalid_settings(&path).with_context(|| {
            format!(
                "原设置文件损坏且无法备份，为避免覆盖已拒绝保存：{}",
                path.display()
            )
        })?;
        tracing::warn!(backup = %backup_path.display(), "原设置文件损坏，已在保存前备份");
    }
    let mut settings = loaded.settings;
    if let Some(value) = patch.interface_language {
        settings.interface_language = value;
    }
    if let Some(value) = patch.app_update_download_source {
        settings.app_update_download_source = value;
    }
    if let Some(value) = patch.chrome_hardware_acceleration {
        settings.chrome_hardware_acceleration = value;
    }
    if let Some(value) = patch.show_full_thinking {
        settings.show_full_thinking = value;
    }
    if let Some(value) = patch.sidebar_collapsed_project_ids {
        settings.sidebar_collapsed_project_ids = value;
    }
    if let Some(value) = patch.task_notifications {
        settings.task_notifications = value;
    }
    if let Some(value) = patch.notification_sound {
        settings.notification_sound = value;
    }
    if let Some(value) = patch.keep_computer_awake {
        settings.keep_computer_awake = value;
    }
    if let Some(value) = patch.local_memories {
        settings.local_memories = value;
    }
    settings.validate()?;
    save_to_path(&path, &settings)?;
    Ok(settings)
}

/// 容错读取当前设置文件；任何外部文件错误都降级为可启动的默认设置。
fn load_unlocked(app: &AppHandle) -> Result<SettingsLoad> {
    let path = settings_path(app)?;
    Ok(load_compatible_path(&path))
}

fn load_compatible_path(path: &Path) -> SettingsLoad {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: Vec::new(),
                backup_required: false,
            };
        }
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: vec![format!(
                    "无法检查设置文件 {}，已使用默认设置: {error}",
                    path.display()
                )],
                backup_required: true,
            };
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return SettingsLoad {
            settings: AppSettings::initial(),
            warnings: vec![format!(
                "设置路径不是普通文件，已使用默认设置且不会覆盖: {}",
                path.display()
            )],
            backup_required: true,
        };
    }
    match fs::read_to_string(path) {
        Ok(content) => load_compatible_content(&content),
        Err(error) => SettingsLoad {
            settings: AppSettings::initial(),
            warnings: vec![format!(
                "无法读取设置文件 {}，已使用默认设置: {error}",
                path.display()
            )],
            backup_required: true,
        },
    }
}

fn load_compatible_content(content: &str) -> SettingsLoad {
    const CURRENT_KEYS: &[&str] = &[
        "interfaceLanguage",
        "appUpdateDownloadSource",
        "chromeHardwareAcceleration",
        "showFullThinking",
        "sidebarCollapsedProjectIds",
        "taskNotifications",
        "notificationSound",
        "keepComputerAwake",
        "localMemories",
    ];
    let value = match serde_json::from_str::<serde_json::Value>(content) {
        Ok(value) => value,
        Err(error) => {
            return SettingsLoad {
                settings: AppSettings::initial(),
                warnings: vec![format!("设置文件不是有效 JSON，已使用默认设置: {error}")],
                backup_required: true,
            };
        }
    };
    let Some(object) = value.as_object() else {
        return SettingsLoad {
            settings: AppSettings::initial(),
            warnings: vec!["设置文件根节点不是对象，已使用默认设置".to_owned()],
            backup_required: true,
        };
    };
    let mut warnings = Vec::new();
    for key in object
        .keys()
        .filter(|key| !CURRENT_KEYS.contains(&key.as_str()))
    {
        warnings.push(format!("忽略未知设置参数: {key}"));
    }
    for key in CURRENT_KEYS
        .iter()
        .filter(|key| !object.contains_key(**key))
    {
        warnings.push(format!("设置参数缺失，已自动填充默认值: {key}"));
    }
    let mut compatible = serde_json::Map::new();
    for key in CURRENT_KEYS {
        if let Some(value) = object.get(*key) {
            compatible.insert((*key).to_owned(), value.clone());
        }
    }
    let (settings, backup_required) = match serde_json::from_value::<AppSettings>(compatible.into())
    {
        Ok(settings) if settings.validate().is_ok() => (settings, false),
        Ok(_) => {
            warnings.push("设置参数值不符合约束，已使用默认设置".to_owned());
            (AppSettings::initial(), true)
        }
        Err(error) => {
            warnings.push(format!("设置参数类型无效，已使用默认设置: {error}"));
            (AppSettings::initial(), true)
        }
    };
    SettingsLoad {
        settings,
        warnings,
        backup_required,
    }
}

/// 在覆盖严重损坏的外部设置前创建带日期且不覆盖既有文件的备份。
fn backup_invalid_settings(path: &Path) -> Result<PathBuf> {
    crate::storage::backup_private_file(path)
        .with_context(|| format!("备份设置文件失败：{}", path.display()))
}

/// 将完整设置保存到指定路径；独立入口用于验证重复原子覆盖。
fn save_to_path(path: &Path, settings: &AppSettings) -> Result<()> {
    settings.validate()?;
    let bytes = serde_json::to_vec_pretty(settings).context("序列化应用设置失败")?;
    crate::storage::atomic_write_private(path, &bytes)
        .with_context(|| format!("保存应用设置失败：{}", path.display()))
}

/// 返回应用设置文件路径。
fn settings_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join("settings.json"))
}

/// 容错读取 WebView 创建前所需的当前设置；正常启动阶段再记录并修复文件。
#[cfg(any(target_os = "windows", test))]
fn load_before_start(path: &Path) -> Result<AppSettings> {
    Ok(load_compatible_path(path).settings)
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
        AppSettings, AppSettingsPatch, AppUpdateDownloadSource, InterfaceLanguage,
        backup_invalid_settings, load_before_start, load_compatible_content, load_compatible_path,
        repair_loaded_path, save_to_path,
    };
    use std::fs;

    /// 外部设置允许缺失和未知字段，并转换为当前完整结构。
    #[test]
    fn settings_schema_is_compatible() {
        let valid = r#"{
            "chromeHardwareAcceleration": true,
            "showFullThinking": true,
            "sidebarCollapsedProjectIds": [],
            "taskNotifications": true,
            "notificationSound": true,
            "keepComputerAwake": false,
            "localMemories": true
        }"#;
        assert!(serde_json::from_str::<AppSettings>(valid).is_ok());
        assert_eq!(
            serde_json::from_str::<AppSettings>(valid)
                .unwrap()
                .app_update_download_source,
            AppUpdateDownloadSource::Auto
        );
        assert_eq!(
            serde_json::from_str::<AppSettings>(valid)
                .unwrap()
                .interface_language,
            InterfaceLanguage::SimplifiedChinese
        );
        assert!(serde_json::from_str::<AppSettings>("{}").is_ok());

        let unknown = valid.replace(
            "\"sidebarCollapsedProjectIds\": []",
            "\"sidebarCollapsedProjectIds\": [], \"oldSetting\": true",
        );
        let loaded = load_compatible_content(&unknown);
        assert!(!loaded.backup_required);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.contains("oldSetting"))
        );
        assert!(loaded.settings.local_memories);

        let missing = load_compatible_content(r#"{"keepComputerAwake": true}"#);
        assert!(missing.settings.keep_computer_awake);
        assert!(missing.settings.local_memories);
        assert!(!missing.backup_required);
        assert!(
            missing
                .warnings
                .iter()
                .any(|warning| warning.contains("localMemories"))
        );

        let invalid = AppSettings {
            sidebar_collapsed_project_ids: vec![" project-1 ".to_owned()],
            ..serde_json::from_str(valid).expect("应解析当前设置")
        };
        assert!(invalid.validate().is_err());
    }

    /// 严重损坏的配置必须先完整备份，且备份不能覆盖已有文件。
    #[test]
    fn invalid_settings_are_backed_up_before_replacement() {
        let directory = tempfile::tempdir().expect("创建测试目录");
        let path = directory.path().join("settings.json");
        let original = "{ invalid user settings";
        fs::write(&path, original).expect("写入损坏设置");

        let loaded = load_compatible_path(&path);
        assert!(loaded.backup_required);
        let first = backup_invalid_settings(&path).expect("创建首个备份");
        let second = backup_invalid_settings(&path).expect("创建不冲突的第二个备份");

        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".bak")
        );
        assert_eq!(fs::read_to_string(first).unwrap(), original);
        assert_eq!(fs::read_to_string(second).unwrap(), original);
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    /// 备份失败时只能使用内存默认值，绝不能覆盖原路径。
    #[test]
    fn failed_backup_keeps_invalid_path_untouched() {
        let directory = tempfile::tempdir().expect("创建测试目录");
        let path = directory.path().join("settings.json");
        fs::create_dir(&path).expect("创建不可备份的设置目录");

        let loaded = repair_loaded_path(&path, load_compatible_path(&path));

        assert!(loaded.settings.chrome_hardware_acceleration);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.contains("不会覆盖"))
        );
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    /// 设置符号链接不是当前配置文件，容错启动不得跟随或替换其目标。
    #[cfg(unix)]
    #[test]
    fn symlinked_settings_are_not_followed_or_replaced() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("创建测试目录");
        let target = directory.path().join("outside.json");
        let path = directory.path().join("settings.json");
        fs::write(&target, "{broken target").expect("写入链接目标");
        symlink(&target, &path).expect("创建设置符号链接");

        let loaded = repair_loaded_path(&path, load_compatible_path(&path));

        assert!(loaded.settings.chrome_hardware_acceleration);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.contains("不会覆盖"))
        );
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
        settings.show_full_thinking = false;
        save_to_path(&path, &settings).expect("第二次应覆盖已有设置");
        settings.keep_computer_awake = true;
        save_to_path(&path, &settings).expect("第三次仍应覆盖已有设置");

        let saved: AppSettings =
            serde_json::from_slice(&fs::read(&path).expect("读取保存结果")).unwrap();
        assert!(!saved.show_full_thinking);
        assert!(saved.keep_computer_awake);
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
            r#"{"showFullThinking": "true"}"#,
            r#"{"sidebarCollapsedProjectIds": null}"#,
            r#"{"taskNotifications": null}"#,
            r#"{"notificationSound": "true"}"#,
            r#"{"keepComputerAwake": null}"#,
            r#"{"localMemories": null}"#,
            r#"{"oldSetting": true}"#,
        ] {
            assert!(serde_json::from_str::<AppSettingsPatch>(invalid).is_err());
        }
    }

    /// WebView 创建前的读取也必须容错，正常启动阶段再完成备份和修复。
    #[test]
    fn before_start_settings_are_tolerant() {
        let directory = tempfile::tempdir().expect("创建测试目录");
        let path = directory.path().join("settings.json");

        let initial = load_before_start(&path).expect("缺失文件应使用首次启动设置");
        assert!(initial.chrome_hardware_acceleration);
        assert!(initial.local_memories, "本地记忆必须默认开启");

        fs::write(&path, "{broken").expect("写入损坏设置");
        let fallback = load_before_start(&path).expect("损坏设置应使用内存默认值");
        assert!(fallback.chrome_hardware_acceleration);

        fs::write(
            &path,
            r#"{
                "chromeHardwareAcceleration": false,
                "showFullThinking": true,
                "sidebarCollapsedProjectIds": [],
                "taskNotifications": true,
                "notificationSound": false,
                "keepComputerAwake": true,
                "localMemories": true
            }"#,
        )
        .expect("写入当前设置");
        let settings = load_before_start(&path).expect("读取当前设置");
        assert!(!settings.chrome_hardware_acceleration);
    }
}
