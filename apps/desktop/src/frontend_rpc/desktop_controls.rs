//! 原生窗口、页面缩放和帮助菜单的 Tauri 边界。
//!
//! 这些命令只接受当前 WebView 注入的 `Webview`，不接受前端传入的
//! 窗口标识或任意 URL。这样标题栏控制不会被用来操作其他窗口，也不会把
//! Node/Electron 宿主能力带进桌面运行时。

use crate::diagnostics::Diagnostics;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, EventTarget, Manager, Webview, WebviewWindow, Window};
use tauri_plugin_dialog::DialogExt;

const ZOOM_MIN_LEVEL: i32 = -3;
const ZOOM_MAX_LEVEL: i32 = 5;
const ZOOM_FACTOR_STEP: f64 = 1.1;
const WINDOW_CHROME_CHANGED_EVENT: &str = "keencode://desktop-window-chrome-state-changed";
const ZOOM_LEVEL_CHANGED_EVENT: &str = "keencode://desktop-zoom-level-changed";
const RESOURCE_MANAGER_OPEN_EVENT: &str = "keencode://resource-manager-open";
const RESOURCE_MANAGER_STORAGE_PROGRESS_EVENT: &str =
    "keencode://resource-manager-storage-progress";
const ABOUT_OPEN_EVENT: &str = "keencode://about-open";
pub(crate) const WINDOW_FULLSCREEN_CHANGED_EVENT: &str = "keencode://window-fullscreen-changed";
const MAIN_WINDOW_LABEL: &str = "main";

const STORAGE_CATEGORY_IDS: [&str; 11] = [
    "sessionStore",
    "subagentTranscripts",
    "toolOutputs",
    "modelTrajectory",
    "devTraces",
    "logs",
    "backups",
    "exports",
    "runtimes",
    "config",
    "other",
];
const MAX_STORAGE_ENTRIES: usize = 100_000;
const MAX_STORAGE_DEPTH: usize = 32;
const MAX_STORAGE_ERRORS: usize = 64;
const MAX_CATEGORY_ENTRIES: usize = 24;

/// 共享平台契约中的窗口状态；字段名必须与 `packages/shared/src/platform.ts` 一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopWindowChromeState {
    pub(crate) is_maximized: bool,
    pub(crate) mac_os_major_version: Option<u32>,
    pub(crate) supports_native_rounded_corners: bool,
}

/// 共享平台契约中的主 WebView 缩放状态。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopZoomState {
    pub(crate) zoom_level: i32,
}

/// ResourceManager 的跨进程 DTO。CPU 没有相邻采样基线时保留 `null`，
/// 不能把“未知”编码成 0 让界面误判为真实空闲。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceUsageProcessDto {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) category: String,
    pub(crate) group_key: String,
    pub(crate) group_label: String,
    pub(crate) cpu_percent: Option<f64>,
    pub(crate) memory_bytes: u64,
    pub(crate) sampled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceSystemUsageDto {
    pub(crate) cpu_percent: Option<f64>,
    pub(crate) memory_total_bytes: u64,
    pub(crate) memory_used_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceAppUsageDto {
    pub(crate) cpu_percent: Option<f64>,
    pub(crate) memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceUsageSnapshotDto {
    pub(crate) sampled_at: u64,
    pub(crate) logical_cpu_count: usize,
    pub(crate) system: ResourceSystemUsageDto,
    pub(crate) app: ResourceAppUsageDto,
    pub(crate) processes: Vec<ResourceUsageProcessDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageEntryUsageDto {
    pub(crate) relative_path: String,
    pub(crate) bytes: u64,
    pub(crate) file_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageCategoryUsageDto {
    pub(crate) id: String,
    pub(crate) bytes: u64,
    pub(crate) file_count: u64,
    pub(crate) cleanability: String,
    pub(crate) entries: Vec<StorageEntryUsageDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageVolumeDto {
    pub(crate) device_id: String,
    pub(crate) mount_point: String,
    pub(crate) total_bytes: u64,
    pub(crate) free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageRootUsageDto {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) volume: Option<StorageVolumeDto>,
    pub(crate) bytes: u64,
    pub(crate) file_count: u64,
    pub(crate) categories: Vec<StorageCategoryUsageDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoragePathErrorDto {
    pub(crate) path: String,
    pub(crate) code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageUsageSnapshotDto {
    pub(crate) job_id: String,
    pub(crate) status: String,
    pub(crate) started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at: Option<u64>,
    pub(crate) roots: Vec<StorageRootUsageDto>,
    pub(crate) errors: Vec<StoragePathErrorDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageScanStartDto {
    pub(crate) job_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageCleanRequestDto {
    pub(crate) root_id: String,
    pub(crate) category_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoomAction {
    In,
    Out,
    Reset,
}

/// 缩放状态按 WebView label 隔离；Tauri 当前没有跨平台的 zoom getter，
/// 因此所有本模块发出的缩放写入都在同一进程内保留其已提交档位。
static ZOOM_LEVELS: OnceLock<Mutex<HashMap<String, i32>>> = OnceLock::new();

/// 存储扫描只保留最近一份只读快照；数据仍由磁盘上的 KeenCode 根目录作为唯一事实源。
static STORAGE_SNAPSHOT: OnceLock<Mutex<Option<StorageUsageSnapshotDto>>> = OnceLock::new();
static STORAGE_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn zoom_levels() -> &'static Mutex<HashMap<String, i32>> {
    ZOOM_LEVELS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn storage_snapshot() -> &'static Mutex<Option<StorageUsageSnapshotDto>> {
    STORAGE_SNAPSHOT.get_or_init(|| Mutex::new(None))
}

fn is_main_webview_identity(webview_label: &str, window_label: &str) -> bool {
    webview_label == MAIN_WINDOW_LABEL && window_label == MAIN_WINDOW_LABEL
}

/// Tauri 命令注册后默认可被任意 WebView 调用；产品窗口控制只允许主 WebView。
/// 子 WebView 与主界面共享宿主 Window，因此必须同时核对两层身份。
fn require_main_caller(caller: &Webview) -> Result<(), String> {
    if is_main_webview_identity(caller.label(), caller.window().label()) {
        Ok(())
    } else {
        Err("桌面窗口控制只允许主 WebView 调用".to_owned())
    }
}

fn main_host_window(caller: &Webview) -> Result<Window, String> {
    require_main_caller(caller)?;
    Ok(caller.window())
}

/// 桌面事件只投递给主 WebView，避免 WebviewWindow::emit 的全局广播把窗口状态
/// 送给浏览器等子页面；调用者已经通过身份校验，目标 label 仍固定为当前 caller。
fn emit_to_caller<S: Serialize + Clone>(
    caller: &Webview,
    event: &str,
    payload: S,
) -> Result<(), String> {
    caller
        .emit_to(EventTarget::webview_window(caller.label()), event, payload)
        .map_err(|error| format!("发送桌面事件失败：{error}"))
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn clamp_zoom_level(level: i32) -> i32 {
    level.clamp(ZOOM_MIN_LEVEL, ZOOM_MAX_LEVEL)
}

fn zoom_factor_for_level(level: i32) -> f64 {
    ZOOM_FACTOR_STEP.powi(clamp_zoom_level(level))
}

fn next_zoom_level(current: i32, action: ZoomAction) -> i32 {
    match action {
        ZoomAction::In => clamp_zoom_level(current.saturating_add(1)),
        ZoomAction::Out => clamp_zoom_level(current.saturating_sub(1)),
        ZoomAction::Reset => 0,
    }
}

fn current_zoom_level(window: &Webview) -> Result<i32, String> {
    zoom_levels()
        .lock()
        .map_err(|_| "桌面缩放状态锁已损坏".to_owned())
        .map(|levels| levels.get(window.label()).copied().unwrap_or(0))
}

fn emit_zoom_level(window: &Webview, level: i32) -> Result<Value, String> {
    let state = DesktopZoomState {
        zoom_level: clamp_zoom_level(level),
    };
    emit_to_caller(window, ZOOM_LEVEL_CHANGED_EVENT, &state)?;
    serde_json::to_value(state).map_err(|error| format!("编码桌面缩放状态失败：{error}"))
}

fn set_zoom_level(window: &Webview, level: i32) -> Result<Value, String> {
    let level = clamp_zoom_level(level);
    window
        .set_zoom(zoom_factor_for_level(level))
        .map_err(|error| format!("设置桌面页面缩放失败：{error}"))?;
    zoom_levels()
        .lock()
        .map_err(|_| "桌面缩放状态锁已损坏".to_owned())?
        .insert(window.label().to_owned(), level);
    emit_zoom_level(window, level)
}

#[cfg(target_os = "windows")]
fn supports_native_windows_rounded_corners() -> bool {
    // sysinfo 返回的 Windows 字符串通常包含 10.0.<build>；取最大数字可
    // 同时兼容 "Windows 11 10.0.26100" 这类带产品名的版本格式。
    sysinfo::System::os_version()
        .into_iter()
        .flat_map(|version| {
            version
                .split(|character: char| !character.is_ascii_digit())
                .filter_map(|part| part.parse::<u32>().ok())
                .collect::<Vec<_>>()
        })
        .max()
        .is_some_and(|build| build >= 22_000)
}

#[cfg(not(target_os = "windows"))]
fn supports_native_windows_rounded_corners() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn mac_os_major_version() -> Option<u32> {
    sysinfo::System::os_version()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[cfg(not(target_os = "macos"))]
fn mac_os_major_version() -> Option<u32> {
    None
}

fn resolve_window_chrome_state(is_maximized: bool) -> DesktopWindowChromeState {
    DesktopWindowChromeState {
        is_maximized,
        mac_os_major_version: mac_os_major_version(),
        supports_native_rounded_corners: supports_native_windows_rounded_corners(),
    }
}

fn window_chrome_state(window: &Webview) -> Result<DesktopWindowChromeState, String> {
    let host_window = main_host_window(window)?;
    let is_maximized = host_window
        .is_maximized()
        .map_err(|error| format!("读取桌面窗口最大化状态失败：{error}"))?;
    Ok(resolve_window_chrome_state(is_maximized))
}

pub(crate) fn emit_window_chrome_state(window: &WebviewWindow) -> Result<Value, String> {
    emit_window_chrome_state_for_caller(window.as_ref())
}

fn emit_window_chrome_state_for_caller(window: &Webview) -> Result<Value, String> {
    let state = window_chrome_state(window)?;
    emit_to_caller(window, WINDOW_CHROME_CHANGED_EVENT, &state)?;
    emit_window_fullscreen_state_for_caller(window)?;
    serde_json::to_value(state).map_err(|error| format!("编码桌面窗口状态失败：{error}"))
}

/// 将 Tauri 的真实全屏状态转发给 renderer。调用方应在主窗口的窗口事件中
/// 触发（全屏切换通常表现为一次 resize），不能由前端自行猜测状态。
fn emit_window_fullscreen_state_for_caller(window: &Webview) -> Result<bool, String> {
    let host_window = main_host_window(window)?;
    let is_fullscreen = host_window
        .is_fullscreen()
        .map_err(|error| format!("读取桌面全屏状态失败：{error}"))?;
    emit_to_caller(window, WINDOW_FULLSCREEN_CHANGED_EVENT, is_fullscreen)?;
    Ok(is_fullscreen)
}

fn validate_command(command: &str) -> Result<&str, String> {
    if command.is_empty() {
        return Err("桌面命令不能为空".to_owned());
    }
    if command.len() > 64 || command.chars().any(char::is_control) {
        return Err("桌面命令标识无效".to_owned());
    }
    match command {
        "minimizeWindow"
        | "toggleMaximizeWindow"
        | "zoomIn"
        | "zoomOut"
        | "resetZoom"
        | "openResourceManager"
        | "showAbout" => Ok(command),
        _ => Err(format!("不支持的桌面命令：{command}")),
    }
}

/// 窗口拖拽只接受浏览器主键；坐标由 Tauri 从当前原生鼠标事件读取，
/// 前端不能借此注入任意窗口位置或操作其他窗口。
fn validate_window_drag_button(button: u8) -> Result<(), String> {
    if button == 0 {
        Ok(())
    } else {
        Err("窗口拖拽只接受鼠标主键".to_owned())
    }
}

fn resource_manager_payload(app: &AppHandle, window: &Webview) -> Result<Value, String> {
    let diagnostics = app
        .try_state::<std::sync::Arc<Diagnostics>>()
        .ok_or_else(|| "诊断状态尚未初始化，无法打开资源管理器".to_owned())?;
    let snapshot = diagnostics.observability().snapshot();
    let resource_snapshot = resource_usage_snapshot(app, window)?;
    Ok(json!({
        "productName": "KeenCode",
        "windowLabel": window.label(),
        "diagnostics": snapshot,
        "resourceSnapshot": resource_snapshot,
    }))
}

fn open_resource_manager(app: &AppHandle, window: &Webview) -> Result<Value, String> {
    require_main_caller(window)?;
    let payload = resource_manager_payload(app, window)?;
    // 由主 renderer 的 ResourceManagerApp 消费；payload 同时携带首屏只读快照，
    // 后续刷新仍调用独立命令，避免把事件当成资源事实源。
    emit_to_caller(window, RESOURCE_MANAGER_OPEN_EVENT, &payload)
        .map_err(|error| format!("打开资源管理器失败：{error}"))?;
    Ok(payload)
}

fn show_about(app: &AppHandle, window: &Webview) -> Result<Value, String> {
    let host_window = main_host_window(window)?;
    let version = env!("CARGO_PKG_VERSION");
    let payload = json!({
        "productName": "KeenCode",
        "version": version,
        "description": "轻量、本地优先的桌面 AI 编码工具",
    });
    emit_to_caller(window, ABOUT_OPEN_EVENT, &payload)?;
    // 使用 Tauri 已安装的 dialog 插件展示真实品牌和构建版本，不依赖外部
    // Electron/Node 宿主；show 为异步调用，不阻塞窗口事件线程。
    app.dialog()
        .message(format!(
            "KeenCode\n版本 {version}\n轻量、本地优先的桌面 AI 编码工具"
        ))
        .title("关于 KeenCode")
        .parent(&host_window)
        .show(|_| {});
    Ok(payload)
}

/// 将 Diagnostics 的平台采样与系统内存信息转换为共享 ResourceManager 契约。
///
/// 进程行只报告当前 KeenCode 宿主及其受控 WebView2 后代的聚合值；不枚举或暴露
/// 用户机器上的无关进程。首次 CPU 采样没有基线时明确标记 `sampled=false`。
fn resource_usage_snapshot(
    app: &AppHandle,
    window: &Webview,
) -> Result<ResourceUsageSnapshotDto, String> {
    require_main_caller(window)?;
    let diagnostics = app
        .try_state::<std::sync::Arc<Diagnostics>>()
        .ok_or_else(|| "诊断状态尚未初始化，无法读取资源快照".to_owned())?;
    let process = diagnostics.process_resource_sample();

    // 只刷新系统内存；new_all 已经刷新全部进程，不能再用 refresh_all 重做一遍。
    // 复用 System，避免每秒重新初始化系统采样器；此处不启动后台定时任务。
    static SYSTEM_MEMORY: OnceLock<Mutex<sysinfo::System>> = OnceLock::new();
    let mut system = SYSTEM_MEMORY
        .get_or_init(|| Mutex::new(sysinfo::System::new()))
        .lock()
        .map_err(|_| "系统内存采样锁已损坏".to_owned())?;
    system.refresh_memory();
    let logical_cpu_count = crate::diagnostics::process_resources::logical_processor_count();
    let app_memory = process.resident_bytes.unwrap_or_default();
    let app_cpu = process.cpu_percent.map(|value| value.max(0.0));
    let sampled = process.process_count.is_some() && app_cpu.is_some();
    let process_name = if process.process_count.is_some_and(|count| count > 1) {
        "KeenCode + WebView2"
    } else {
        "KeenCode"
    };

    // sysinfo 的一次性 CPU 刷新没有前一时刻基线；返回 null，避免把
    // 初始化值当成整机真实空闲率。进程 CPU 由 Diagnostics 自己维护基线。
    Ok(ResourceUsageSnapshotDto {
        sampled_at: unix_timestamp_millis(),
        logical_cpu_count,
        system: ResourceSystemUsageDto {
            cpu_percent: None,
            memory_total_bytes: system.total_memory(),
            memory_used_bytes: system.used_memory(),
        },
        app: ResourceAppUsageDto {
            cpu_percent: app_cpu,
            memory_bytes: app_memory,
        },
        processes: vec![ResourceUsageProcessDto {
            pid: std::process::id(),
            name: process_name.to_owned(),
            category: "base".to_owned(),
            group_key: "main".to_owned(),
            group_label: "KeenCode".to_owned(),
            cpu_percent: app_cpu,
            memory_bytes: app_memory,
            sampled,
        }],
    })
}

#[derive(Default)]
struct StorageCategoryStats {
    bytes: u64,
    file_count: u64,
    entries: BTreeMap<String, (u64, u64)>,
}

fn storage_category(relative_path: &Path) -> &'static str {
    let first = relative_path
        .components()
        .next()
        .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if first == "sessions" || first == "session-store" {
        "sessionStore"
    } else if first.contains("subagent") || first.contains("transcript") {
        "subagentTranscripts"
    } else if first.contains("tool") || first.contains("cache") || first == "attachments" {
        "toolOutputs"
    } else if first.contains("trajectory") || first.contains("analytics") {
        "modelTrajectory"
    } else if first.contains("trace") || first.contains("capture") {
        "devTraces"
    } else if first.contains("log") || first.contains("crash") {
        "logs"
    } else if first.contains("backup") || first.ends_with(".bak") {
        "backups"
    } else if first.contains("export") || first.contains("feedback") {
        "exports"
    } else if first.contains("runtime") || first.contains("plugin") || first == "mcp" {
        "runtimes"
    } else if first.ends_with(".json")
        || first.ends_with(".toml")
        || first.ends_with(".yaml")
        || first.ends_with(".yml")
        || first.contains("config")
        || first.contains("memory")
    {
        "config"
    } else {
        "other"
    }
}

fn storage_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn storage_error(root: &Path, path: &Path, error: &std::io::Error) -> StoragePathErrorDto {
    StoragePathErrorDto {
        path: storage_relative_path(root, path),
        code: error.kind().to_string(),
    }
}

fn scan_storage_root(
    root: &Path,
    job_id: &str,
    started_at: u64,
) -> Result<StorageUsageSnapshotDto, String> {
    let mut categories = STORAGE_CATEGORY_IDS
        .iter()
        .map(|id| (*id, StorageCategoryStats::default()))
        .collect::<BTreeMap<_, _>>();
    let mut errors = Vec::new();
    let mut scanned_entries = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((directory, depth)) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                if errors.len() < MAX_STORAGE_ERRORS {
                    errors.push(storage_error(root, &directory, &error));
                }
                continue;
            }
        };
        for entry in entries {
            if scanned_entries >= MAX_STORAGE_ENTRIES {
                break;
            }
            scanned_entries = scanned_entries.saturating_add(1);
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if errors.len() < MAX_STORAGE_ERRORS {
                        errors.push(StoragePathErrorDto {
                            path: storage_relative_path(root, &directory),
                            code: error.kind().to_string(),
                        });
                    }
                    continue;
                }
            };
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    if errors.len() < MAX_STORAGE_ERRORS {
                        errors.push(storage_error(root, &path, &error));
                    }
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if depth < MAX_STORAGE_DEPTH {
                    stack.push((path, depth.saturating_add(1)));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }

            let relative = path.strip_prefix(root).unwrap_or(path.as_path());
            let category = storage_category(relative);
            let Some(stats) = categories.get_mut(category) else {
                continue;
            };
            let bytes = metadata.len();
            stats.bytes = stats.bytes.saturating_add(bytes);
            stats.file_count = stats.file_count.saturating_add(1);
            let entry_key = relative
                .components()
                .next()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| "其他".to_owned());
            let entry_stats = stats.entries.entry(entry_key).or_default();
            entry_stats.0 = entry_stats.0.saturating_add(bytes);
            entry_stats.1 = entry_stats.1.saturating_add(1);
        }
    }

    let category_values = STORAGE_CATEGORY_IDS
        .iter()
        .map(|id| {
            let stats = categories.remove(id).unwrap_or_default();
            let entries = stats
                .entries
                .into_iter()
                .map(
                    |(relative_path, (bytes, file_count))| StorageEntryUsageDto {
                        relative_path,
                        bytes,
                        file_count,
                    },
                )
                .take(MAX_CATEGORY_ENTRIES)
                .collect::<Vec<_>>();
            StorageCategoryUsageDto {
                id: (*id).to_owned(),
                bytes: stats.bytes,
                file_count: stats.file_count,
                cleanability: "none".to_owned(),
                entries,
            }
        })
        .collect::<Vec<_>>();
    let total_bytes = category_values
        .iter()
        .map(|category| category.bytes)
        .sum::<u64>();
    let total_files = category_values
        .iter()
        .map(|category| category.file_count)
        .sum::<u64>();

    Ok(StorageUsageSnapshotDto {
        job_id: job_id.to_owned(),
        status: "complete".to_owned(),
        started_at,
        finished_at: Some(unix_timestamp_millis()),
        roots: vec![StorageRootUsageDto {
            id: "dataBaseDir".to_owned(),
            path: crate::path_utils::path_to_frontend(root),
            volume: None,
            bytes: total_bytes,
            file_count: total_files,
            categories: category_values,
        }],
        errors,
    })
}

fn storage_root(app: &AppHandle) -> Result<PathBuf, String> {
    let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    fs::create_dir_all(&root).map_err(|error| format!("创建应用数据目录失败：{error}"))?;
    Ok(root)
}

/// 执行当前主 WebView 允许的窗口、缩放和帮助命令。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_execute_command(
    app: AppHandle,
    caller: Webview,
    command: String,
) -> Result<Value, String> {
    let host_window = main_host_window(&caller)?;
    match validate_command(&command)? {
        "minimizeWindow" => host_window
            .minimize()
            .map_err(|error| format!("最小化桌面窗口失败：{error}"))
            .map(|_| Value::Null),
        "toggleMaximizeWindow" => {
            let maximized = host_window
                .is_maximized()
                .map_err(|error| format!("读取桌面窗口最大化状态失败：{error}"))?;
            if maximized {
                host_window
                    .unmaximize()
                    .map_err(|error| format!("还原桌面窗口失败：{error}"))?;
            } else {
                host_window
                    .maximize()
                    .map_err(|error| format!("最大化桌面窗口失败：{error}"))?;
            }
            emit_window_chrome_state_for_caller(&caller)
        }
        "zoomIn" => {
            let current = current_zoom_level(&caller)?;
            set_zoom_level(&caller, next_zoom_level(current, ZoomAction::In))
        }
        "zoomOut" => {
            let current = current_zoom_level(&caller)?;
            set_zoom_level(&caller, next_zoom_level(current, ZoomAction::Out))
        }
        "resetZoom" => set_zoom_level(&caller, next_zoom_level(0, ZoomAction::Reset)),
        "openResourceManager" => open_resource_manager(&app, &caller),
        "showAbout" => show_about(&app, &caller),
        _ => unreachable!("validate_command 已筛选桌面命令"),
    }
}

/// 开始当前主窗口的原生标题栏拖拽。
///
/// caller 所属宿主 Window 的 `start_dragging` 使用当前 OS 鼠标事件启动拖拽；命令不接受
/// 窗口标识或坐标，避免 renderer 把拖拽转发成任意窗口控制。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_start_window_drag(caller: Webview, button: u8) -> Result<(), String> {
    let host_window = main_host_window(&caller)?;
    validate_window_drag_button(button)?;
    host_window
        .start_dragging()
        .map_err(|error| format!("启动桌面窗口拖拽失败：{error}"))
}

/// 返回当前原生窗口的真实最大化状态和平台圆角能力。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_window_chrome_state(
    caller: Webview,
) -> Result<DesktopWindowChromeState, String> {
    // getter 订阅发生在首次查询之前，因此这里也补发一次真实全屏状态，
    // 使 macOS 启动时已处于全屏的窗口不会一直停留在默认 false。
    let state = window_chrome_state(&caller)?;
    emit_window_fullscreen_state_for_caller(&caller)?;
    Ok(state)
}

/// 返回本进程最近一次成功应用到当前 WebView 的缩放档位。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_zoom_level(caller: Webview) -> Result<DesktopZoomState, String> {
    require_main_caller(&caller)?;
    Ok(DesktopZoomState {
        zoom_level: current_zoom_level(&caller)?,
    })
}

/// 读取当前主进程与其受控 WebView2 后代的资源占用快照。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_resource_usage_snapshot(
    app: AppHandle,
    caller: Webview,
) -> Result<ResourceUsageSnapshotDto, String> {
    resource_usage_snapshot(&app, &caller)
}

/// 启动一次有界的应用数据目录扫描，并把完整快照保存到进程内最近值。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_resource_manager_storage_start_scan(
    app: AppHandle,
    caller: Webview,
) -> Result<StorageScanStartDto, String> {
    require_main_caller(&caller)?;
    let started_at = unix_timestamp_millis();
    let sequence = STORAGE_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let job_id = format!("resource-storage-{started_at}-{sequence}");
    let root = storage_root(&app)?;
    let snapshot = scan_storage_root(&root, &job_id, started_at)?;
    storage_snapshot()
        .lock()
        .map_err(|_| "资源存储快照锁已损坏".to_owned())?
        .replace(snapshot.clone());
    emit_to_caller(&caller, RESOURCE_MANAGER_STORAGE_PROGRESS_EVENT, &snapshot)?;
    Ok(StorageScanStartDto { job_id })
}

/// 取消扫描请求；扫描本身是有界同步事务，完成后取消保持幂等。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_resource_manager_storage_cancel_scan(
    caller: Webview,
    job_id: String,
) -> Result<(), String> {
    require_main_caller(&caller)?;
    if job_id.is_empty() {
        return Err("存储扫描任务标识不能为空".to_owned());
    }
    Ok(())
}

/// 返回最近一次真实磁盘扫描结果；尚未扫描时返回 null。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_resource_manager_storage_get_snapshot(
    caller: Webview,
) -> Result<Option<StorageUsageSnapshotDto>, String> {
    require_main_caller(&caller)?;
    storage_snapshot()
        .lock()
        .map_err(|_| "资源存储快照锁已损坏".to_owned())
        .map(|snapshot| snapshot.clone())
}

/// 存储清理保持关闭，直到每类数据具备独立的权威生命周期策略。
///
/// UI 当前把所有扫描类别标为 `none`，因此不会把用户清理动作伪装成成功；
/// 直接调用也返回明确失败，避免误删会话、凭据或运行时数据。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_resource_manager_storage_clean(
    caller: Webview,
    request: StorageCleanRequestDto,
) -> Result<Value, String> {
    require_main_caller(&caller)?;
    Err(format!(
        "KeenCode 当前不提供资源管理器数据清理操作（{} / {}）",
        request.root_id, request.category_id
    ))
}

/// 在应用数据根内调用既有受限文件管理器入口，避免暴露任意本机路径。
#[tauri::command(rename_all = "camelCase")]
pub(crate) fn desktop_resource_manager_storage_reveal_path(
    app: AppHandle,
    caller: Webview,
    path: String,
) -> Result<(), String> {
    require_main_caller(&caller)?;
    let root = storage_root(&app)?;
    let candidate = Path::new(&path);
    let canonical =
        fs::canonicalize(candidate).map_err(|error| format!("无法定位资源路径：{error}"))?;
    let root = fs::canonicalize(root).map_err(|error| format!("无法访问应用数据目录：{error}"))?;
    if !canonical.starts_with(&root) {
        return Err("资源路径不属于 KeenCode 应用数据目录".to_owned());
    }
    crate::workspace::path_reveal(app, canonical.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zoom_levels_are_clamped_and_reset_to_zero() {
        assert_eq!(clamp_zoom_level(-100), ZOOM_MIN_LEVEL);
        assert_eq!(clamp_zoom_level(100), ZOOM_MAX_LEVEL);
        assert_eq!(
            next_zoom_level(ZOOM_MAX_LEVEL, ZoomAction::In),
            ZOOM_MAX_LEVEL
        );
        assert_eq!(
            next_zoom_level(ZOOM_MIN_LEVEL, ZoomAction::Out),
            ZOOM_MIN_LEVEL
        );
        assert_eq!(next_zoom_level(4, ZoomAction::Reset), 0);
        assert!((zoom_factor_for_level(1) - 1.1).abs() < f64::EPSILON);
    }

    #[test]
    fn command_allowlist_rejects_unknown_or_control_input() {
        assert_eq!(validate_command("zoomIn"), Ok("zoomIn"));
        assert_eq!(validate_command("showAbout"), Ok("showAbout"));
        assert!(validate_command("toggleDevTools").is_err());
        assert!(validate_command("zoom\nIn").is_err());
        assert!(validate_command("").is_err());
    }

    #[test]
    fn main_webview_identity_rejects_child_sharing_the_main_window() {
        assert!(is_main_webview_identity("main", "main"));
        assert!(!is_main_webview_identity("browser-child", "main"));
        assert!(!is_main_webview_identity("main", "settings"));
    }

    #[test]
    fn window_drag_accepts_only_primary_button() {
        assert!(validate_window_drag_button(0).is_ok());
        assert!(validate_window_drag_button(1).is_err());
        assert!(validate_window_drag_button(2).is_err());
    }

    #[test]
    fn shared_payload_uses_camel_case_contract() {
        let chrome = serde_json::to_value(resolve_window_chrome_state(false)).unwrap();
        let zoom = serde_json::to_value(DesktopZoomState { zoom_level: -2 }).unwrap();
        assert_eq!(chrome["isMaximized"], json!(false));
        assert!(chrome.get("supportsNativeRoundedCorners").is_some());
        assert_eq!(zoom, json!({"zoomLevel": -2}));
    }

    #[test]
    fn storage_scan_returns_real_categories_without_following_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = directory.path().join("sessions").join("one");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("events.jsonl"), b"event").unwrap();
        std::fs::create_dir_all(directory.path().join("logs")).unwrap();
        std::fs::write(directory.path().join("logs/app.log"), b"log").unwrap();

        let snapshot = scan_storage_root(directory.path(), "scan-test", 1).unwrap();
        assert_eq!(snapshot.status, "complete");
        assert_eq!(snapshot.roots[0].file_count, 2);
        assert_eq!(snapshot.roots[0].categories[0].id, "sessionStore");
        assert_eq!(snapshot.roots[0].categories[5].id, "logs");
    }

    #[test]
    fn resource_manager_storage_clean_is_explicitly_disabled_by_contract() {
        assert_eq!(
            storage_category(Path::new("sessions/example/events.jsonl")),
            "sessionStore"
        );
        assert_eq!(storage_category(Path::new("providers.json")), "config");
        assert_eq!(storage_category(Path::new("unknown.bin")), "other");
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ResourceManagerContractFixture {
        resource: ResourceUsageSnapshotDto,
        storage: StorageUsageSnapshotDto,
    }

    fn contract_fixture() -> ResourceManagerContractFixture {
        let categories = STORAGE_CATEGORY_IDS
            .iter()
            .enumerate()
            .map(|(index, id)| StorageCategoryUsageDto {
                id: (*id).to_owned(),
                bytes: if index == 0 { 12 } else { 0 },
                file_count: if index == 0 { 1 } else { 0 },
                cleanability: "none".to_owned(),
                entries: if index == 0 {
                    vec![StorageEntryUsageDto {
                        relative_path: "sessions/contract/events.jsonl".to_owned(),
                        bytes: 12,
                        file_count: 1,
                    }]
                } else {
                    Vec::new()
                },
            })
            .collect();
        ResourceManagerContractFixture {
            resource: ResourceUsageSnapshotDto {
                sampled_at: 1_700_000_000_000,
                logical_cpu_count: 8,
                system: ResourceSystemUsageDto {
                    cpu_percent: None,
                    memory_total_bytes: 16_000,
                    memory_used_bytes: 8_000,
                },
                app: ResourceAppUsageDto {
                    cpu_percent: None,
                    memory_bytes: 1_024,
                },
                processes: vec![ResourceUsageProcessDto {
                    pid: 42,
                    name: "KeenCode".to_owned(),
                    category: "base".to_owned(),
                    group_key: "main".to_owned(),
                    group_label: "KeenCode".to_owned(),
                    cpu_percent: None,
                    memory_bytes: 1_024,
                    sampled: false,
                }],
            },
            storage: StorageUsageSnapshotDto {
                job_id: "resource-storage-fixture".to_owned(),
                status: "complete".to_owned(),
                started_at: 1_700_000_000_000,
                finished_at: Some(1_700_000_000_100),
                roots: vec![StorageRootUsageDto {
                    id: "dataBaseDir".to_owned(),
                    path: "fixture/data-root".to_owned(),
                    volume: None,
                    bytes: 12,
                    file_count: 1,
                    categories,
                }],
                errors: Vec::new(),
            },
        }
    }

    #[test]
    fn resource_manager_dto_preserves_unknown_cpu_and_strict_storage_shape() {
        let fixture = contract_fixture();
        let value = serde_json::to_value(&fixture).unwrap();
        assert_eq!(value["resource"]["system"]["cpuPercent"], Value::Null);
        assert_eq!(value["resource"]["app"]["cpuPercent"], Value::Null);
        assert_eq!(value["resource"]["processes"][0]["cpuPercent"], Value::Null);
        assert_eq!(value["resource"]["processes"][0]["sampled"], json!(false));
        assert_eq!(value["storage"]["roots"][0]["volume"], Value::Null);
        assert_eq!(
            value["storage"]["roots"][0]["categories"]
                .as_array()
                .unwrap()
                .len(),
            STORAGE_CATEGORY_IDS.len()
        );

        if std::env::var_os("KEENCODE_WRITE_RESOURCE_MANAGER_FIXTURE").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tooling/native-live/workflow-contract-fixtures/resource_manager.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut json = serde_json::to_string_pretty(&fixture).unwrap();
            json.push('\n');
            std::fs::write(path, json).unwrap();
        }
    }
}
