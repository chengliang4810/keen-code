use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::fs;
#[cfg(any(target_os = "windows", test))]
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

/// 串行化应用设置读写。
static SETTINGS_IO_LOCK: Mutex<()> = Mutex::new(());

/// KeenCode 当前唯一的应用设置结构。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AppSettings {
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
    /// 是否自动归档符合条件的旧任务。
    pub auto_archive_old_tasks: bool,
    /// 任务进入自动归档候选前需保持未更新的天数。
    pub archive_retention_days: u16,
    /// 是否根据本机历史对话生成并在后续对话中使用本地记忆。
    pub local_memories: bool,
}

impl AppSettings {
    /// 构造首次启动设置。
    fn initial() -> Self {
        Self {
            app_update_download_source: AppUpdateDownloadSource::Auto,
            chrome_hardware_acceleration: true,
            show_full_thinking: true,
            sidebar_collapsed_project_ids: Vec::new(),
            task_notifications: true,
            notification_sound: true,
            keep_computer_awake: false,
            auto_archive_old_tasks: true,
            archive_retention_days: 7,
            local_memories: true,
        }
    }

    /// 校验设置中不能仅靠类型系统表达的约束。
    fn validate(&self) -> Result<()> {
        if !(1..=365).contains(&self.archive_retention_days) {
            anyhow::bail!("归档保留时长必须在 1 到 365 天之间");
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
    /// 更新自动归档旧任务开关。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub auto_archive_old_tasks: Option<bool>,
    /// 更新自动归档保留天数。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub archive_retention_days: Option<u16>,
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
    load_unlocked(app)
}

/// 应用并保存一个严格类型的设置补丁。
pub fn set(app: &AppHandle, patch: AppSettingsPatch) -> Result<AppSettings> {
    let _guard = SETTINGS_IO_LOCK.lock().expect("应用设置读写锁已损坏");
    let mut settings = load_unlocked(app)?;
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
    if let Some(value) = patch.auto_archive_old_tasks {
        settings.auto_archive_old_tasks = value;
    }
    if let Some(value) = patch.archive_retention_days {
        settings.archive_retention_days = value;
    }
    if let Some(value) = patch.local_memories {
        settings.local_memories = value;
    }
    settings.validate()?;
    save_unlocked(app, &settings)?;
    Ok(settings)
}

/// 读取当前设置文件；文件不存在时构造首次启动设置。
fn load_unlocked(app: &AppHandle) -> Result<AppSettings> {
    let path = settings_path(app)?;
    let settings = match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<AppSettings>(&content)
            .with_context(|| format!("应用设置格式无效：{}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppSettings::initial(),
        Err(error) => {
            return Err(error).with_context(|| format!("读取应用设置失败：{}", path.display()));
        }
    };
    settings.validate()?;
    Ok(settings)
}

/// 原子保存当前完整应用设置。
fn save_unlocked(app: &AppHandle, settings: &AppSettings) -> Result<()> {
    settings.validate()?;
    let path = settings_path(app)?;
    let parent = path.parent().context("应用设置路径缺少父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建应用设置目录失败：{}", parent.display()))?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(settings).context("序列化应用设置失败")?;
    fs::write(&temporary, bytes)
        .with_context(|| format!("写入临时应用设置失败：{}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .with_context(|| format!("替换应用设置失败：{}", path.display()))?;
    Ok(())
}

/// 返回应用设置文件路径。
fn settings_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join("settings.json"))
}

/// 读取 WebView 创建前所需的当前设置；仅文件不存在表示首次启动。
#[cfg(any(target_os = "windows", test))]
fn load_before_start(path: &Path) -> Result<AppSettings> {
    let settings = match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str::<AppSettings>(&content)
            .with_context(|| format!("应用设置格式无效：{}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppSettings::initial(),
        Err(error) => {
            return Err(error).with_context(|| format!("读取应用设置失败：{}", path.display()));
        }
    };
    settings.validate()?;
    Ok(settings)
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
    use super::{AppSettings, AppSettingsPatch, AppUpdateDownloadSource, load_before_start};
    use std::fs;

    /// 当前设置文件必须包含完整字段并拒绝未知字段。
    #[test]
    fn settings_schema_is_complete_and_strict() {
        let valid = r#"{
            "chromeHardwareAcceleration": true,
            "showFullThinking": true,
            "sidebarCollapsedProjectIds": [],
            "taskNotifications": true,
            "notificationSound": true,
            "keepComputerAwake": false,
            "autoArchiveOldTasks": true,
            "archiveRetentionDays": 7
            ,"localMemories": true
        }"#;
        assert!(serde_json::from_str::<AppSettings>(valid).is_ok());
        assert_eq!(
            serde_json::from_str::<AppSettings>(valid)
                .unwrap()
                .app_update_download_source,
            AppUpdateDownloadSource::Auto
        );
        assert!(serde_json::from_str::<AppSettings>("{}").is_err());

        let unknown = valid.replace(
            "\"sidebarCollapsedProjectIds\": []",
            "\"sidebarCollapsedProjectIds\": [], \"oldSetting\": true",
        );
        assert!(serde_json::from_str::<AppSettings>(&unknown).is_err());

        let invalid = AppSettings {
            sidebar_collapsed_project_ids: vec![" project-1 ".to_owned()],
            ..serde_json::from_str(valid).expect("应解析当前设置")
        };
        assert!(invalid.validate().is_err());

        let invalid_retention = AppSettings {
            archive_retention_days: 0,
            ..serde_json::from_str(valid).expect("应解析当前设置")
        };
        assert!(invalid_retention.validate().is_err());
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

    /// 补丁允许字段缺失，但拒绝 null、未知字段和错误类型。
    #[test]
    fn settings_patch_rejects_ambiguous_values() {
        assert!(serde_json::from_str::<AppSettingsPatch>("{}").is_ok());
        assert!(
            serde_json::from_str::<AppSettingsPatch>(r#"{"chromeHardwareAcceleration": false}"#)
                .is_ok()
        );
        for invalid in [
            r#"{"chromeHardwareAcceleration": null}"#,
            r#"{"showFullThinking": "true"}"#,
            r#"{"sidebarCollapsedProjectIds": null}"#,
            r#"{"taskNotifications": null}"#,
            r#"{"notificationSound": "true"}"#,
            r#"{"keepComputerAwake": null}"#,
            r#"{"autoArchiveOldTasks": null}"#,
            r#"{"localMemories": null}"#,
            r#"{"oldSetting": true}"#,
        ] {
            assert!(serde_json::from_str::<AppSettingsPatch>(invalid).is_err());
        }
    }

    /// 启动预读只把不存在的文件视为首次启动，损坏内容必须返回错误。
    #[test]
    fn before_start_settings_are_strict() {
        let directory = std::env::temp_dir().join(format!(
            "keencode-app-settings-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        fs::create_dir_all(&directory).expect("创建测试目录");
        let path = directory.join("settings.json");

        let initial = load_before_start(&path).expect("缺失文件应使用首次启动设置");
        assert!(initial.chrome_hardware_acceleration);
        assert!(initial.local_memories, "本地记忆必须默认开启");

        fs::write(&path, "{}").expect("写入损坏设置");
        assert!(load_before_start(&path).is_err());

        fs::write(
            &path,
            r#"{
                "chromeHardwareAcceleration": false,
                "showFullThinking": true,
                "sidebarCollapsedProjectIds": [],
                "taskNotifications": true,
                "notificationSound": false,
                "keepComputerAwake": true,
                "autoArchiveOldTasks": true,
                "archiveRetentionDays": 7
                ,"localMemories": true
            }"#,
        )
        .expect("写入当前设置");
        let settings = load_before_start(&path).expect("读取当前设置");
        assert!(!settings.chrome_hardware_acceleration);

        fs::remove_file(&path).expect("删除测试设置");
        fs::remove_dir(&directory).expect("删除测试目录");
    }
}
