//! 原生文件导出能力：用户另存为和当前主 WebView 的 PDF 打印。
//!
//! 该模块只负责宿主边界。文件来源、目标路径和 PDF 临时文件都在 Rust 侧
//! 校验，渲染版面仍由当前 WebView 中的 `@media print` 页面提供。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(any(windows, test))]
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::Webview;
use tauri_plugin_dialog::DialogExt;
use url::Url;

/// 文件跨 IPC 和 HTTP 下载共用的硬上限，避免导出接口无界缓冲输入。
const MAX_FILE_BYTES: usize = 128 * 1024 * 1024;
/// URL 只用于定位当前下载资源，不应成为无界的 IPC 字符串载荷。
const MAX_SOURCE_URL_BYTES: usize = 8 * 1024;
/// WebView2 打印回调必须在有限时间内完成，超时后临时输出由 RAII 清理。
#[cfg(windows)]
const PRINT_TIMEOUT: Duration = Duration::from_secs(120);

/// WebView2 没有暴露 Electron `preferCSSPageSize` 对应的打印选项；页面调用方
/// 只能通过严格校验的 CSS px 尺寸指定 page box，缺省时保持 WebView2 默认纸张。
/// 四边距由原生设置明确归零，不对非法尺寸回退或裁切输出。
#[cfg(any(windows, test))]
const PDF_PRINT_MARGIN_INCHES: f64 = 0.0;

/// Chromium CSS 打印尺寸使用 96 px/in；上限限制来自宿主边界，避免 renderer
/// 把任意大页面转成耗尽内存的原生纸张。正常 PPTX 页面（如 1280x720）远低于上限。
#[cfg(any(windows, test))]
const PDF_CSS_PX_PER_INCH: f64 = 96.0;
const PDF_PAGE_SIZE_MIN_PX: f64 = 96.0;
const PDF_PAGE_SIZE_MAX_PX: f64 = 16_384.0;

/// 只允许主 WebView 同时存在一个原生打印请求。该 guard 由命令和完成回调共同持有，
/// 确保成功、启动失败、回调错误和超时都不会遗留或提前释放占用。
#[cfg(any(windows, test))]
static PDF_PRINT_IN_FLIGHT: OnceLock<Mutex<bool>> = OnceLock::new();

#[cfg(any(windows, test))]
struct PdfPrintInFlightGuard;

#[cfg(any(windows, test))]
fn acquire_pdf_print_in_flight() -> Result<PdfPrintInFlightGuard, String> {
    let mut in_flight = PDF_PRINT_IN_FLIGHT
        .get_or_init(|| Mutex::new(false))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *in_flight {
        return Err("PDF 导出正在进行，请稍后重试".to_owned());
    }
    *in_flight = true;
    Ok(PdfPrintInFlightGuard)
}

#[cfg(any(windows, test))]
impl Drop for PdfPrintInFlightGuard {
    fn drop(&mut self) {
        if let Some(lock) = PDF_PRINT_IN_FLIGHT.get() {
            let mut in_flight = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *in_flight = false;
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SaveFileRequest {
    /// 二进制来源与 `source_url` 必须严格二选一。
    pub(crate) data: Option<Vec<u8>>,
    pub(crate) source_url: Option<String>,
    pub(crate) suggested_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SaveFileResult {
    pub(crate) success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) canceled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrintPageToPdfResult {
    pub(crate) success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

/// PPTX 打印 DOM 提供的页面尺寸；只接受 CSS px，Rust 内部再转换为 WebView2 英寸。
/// `deny_unknown_fields` 防止 renderer 误把其他布局参数带入原生打印设置。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct PrintPageSize {
    width_px: f64,
    height_px: f64,
}

impl PrintPageSize {
    fn validate(self) -> Result<Self, String> {
        for (name, value) in [("widthPx", self.width_px), ("heightPx", self.height_px)] {
            if !value.is_finite() {
                return Err(format!("{name} 必须是有限数值"));
            }
            if !(PDF_PAGE_SIZE_MIN_PX..=PDF_PAGE_SIZE_MAX_PX).contains(&value) {
                return Err(format!(
                    "{name} 必须在 {PDF_PAGE_SIZE_MIN_PX} 到 {PDF_PAGE_SIZE_MAX_PX} CSS px 之间"
                ));
            }
        }
        Ok(self)
    }

    #[cfg(any(windows, test))]
    fn inches(self) -> (f64, f64) {
        (
            self.width_px / PDF_CSS_PX_PER_INCH,
            self.height_px / PDF_CSS_PX_PER_INCH,
        )
    }

    #[cfg(any(windows, test))]
    fn is_landscape(self) -> bool {
        self.width_px > self.height_px
    }
}

fn save_failure(error: impl Into<String>) -> SaveFileResult {
    SaveFileResult {
        success: false,
        canceled: None,
        path: None,
        error: Some(error.into()),
    }
}

fn pdf_failure(error: impl Into<String>) -> PrintPageToPdfResult {
    PrintPageToPdfResult {
        success: false,
        data: None,
        error: Some(error.into()),
    }
}

fn validate_suggested_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("建议文件名不能为空".to_owned());
    }
    if trimmed == "." || trimmed == ".." {
        return Err("建议文件名无效".to_owned());
    }
    if trimmed.len() > 255 {
        return Err("建议文件名超过 255 字节".to_owned());
    }
    if trimmed.chars().any(|character| {
        character == '/' || character == '\\' || character == '\0' || character.is_control()
    }) {
        return Err("建议文件名不能包含路径分隔符或控制字符".to_owned());
    }
    Ok(())
}

fn allowed_source_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

fn validate_source_url(raw: &str) -> Result<Url, String> {
    if raw.len() > MAX_SOURCE_URL_BYTES {
        return Err(format!("sourceUrl 超过 {MAX_SOURCE_URL_BYTES} 字节限制"));
    }
    let url = Url::parse(raw).map_err(|_| "sourceUrl 不是有效的 HTTP(S) 地址".to_owned())?;
    if !allowed_source_url(&url) {
        return Err("sourceUrl 仅支持不含凭据的 HTTP(S) 地址".to_owned());
    }
    Ok(url)
}

fn request_bytes(request: SaveFileRequest) -> Result<Vec<u8>, String> {
    match (request.data, request.source_url) {
        (Some(bytes), None) => {
            if bytes.len() > MAX_FILE_BYTES {
                return Err(format!("文件超过 {MAX_FILE_BYTES} 字节限制"));
            }
            Ok(bytes)
        }
        (None, Some(raw_url)) => download_source(&raw_url),
        (Some(_), Some(_)) => Err("data 与 sourceUrl 不能同时提供".to_owned()),
        (None, None) => Err("必须提供 data 或 sourceUrl".to_owned()),
    }
}

fn download_source(raw_url: &str) -> Result<Vec<u8>, String> {
    let url = validate_source_url(raw_url)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        // 重定向不能把用户给出的 HTTP(S) 请求变成 file/data 等本地协议，
        // 也不允许把 URL 中的凭据带入后续请求。
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if allowed_source_url(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|_| "无法创建远程文件下载器".to_owned())?;
    let response = client
        .get(url)
        .send()
        .map_err(|_| "远程文件下载失败".to_owned())?;
    if !response.status().is_success() {
        return Err("远程文件响应失败".to_owned());
    }
    crate::http_response::read_http_response_limited(response, MAX_FILE_BYTES).map_err(|error| {
        match error {
            crate::http_response::HttpResponseReadError::TooLarge { max_bytes } => {
                format!("远程文件超过 {max_bytes} 字节限制")
            }
            crate::http_response::HttpResponseReadError::Read(_) => "读取远程文件失败".to_owned(),
        }
    })
}

fn selected_path_to_absolute(selected: tauri_plugin_dialog::FilePath) -> Result<PathBuf, String> {
    let path = selected
        .into_path()
        .map_err(|_| "保存对话框返回的路径不是本机文件路径".to_owned())?;
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|_| "无法确定保存文件的当前目录".to_owned())
    }
}

fn is_main_webview_identity(webview_label: &str, window_label: &str) -> bool {
    webview_label == "main" && window_label == "main"
}

fn require_main_webview(caller: &Webview, capability: &str) -> Result<(), String> {
    if is_main_webview_identity(caller.label(), caller.window().label()) {
        Ok(())
    } else {
        Err(format!("仅主 WebView 支持 {capability}"))
    }
}

#[cfg(feature = "native-desktop-tests")]
const BENCHMARK_EXPORT_DIRECTORY: &str = "exports";

/// 解析原生验收用的保存目标；正式运行永远走系统另存为对话框。
///
/// 验收 harness 不能稳定操作操作系统文件选择器，因此只在显式 benchmark
/// 开关、隔离 data 目录和测试构建同时满足时选择固定的 `exports` 子目录。
/// 目标路径不接受前端传入，文件名仍经过统一的路径穿越校验。
#[cfg(feature = "native-desktop-tests")]
fn benchmark_export_path_from_root(
    data_root: &Path,
    suggested_name: &str,
) -> Result<PathBuf, String> {
    if !data_root.is_absolute() {
        return Err("原生验收数据目录必须是绝对路径".to_owned());
    }
    validate_suggested_name(suggested_name)?;
    fs::create_dir_all(data_root).map_err(|_| "无法创建原生验收数据目录".to_owned())?;
    let canonical_data_root =
        fs::canonicalize(data_root).map_err(|_| "无法确定原生验收数据目录".to_owned())?;
    let export_directory = canonical_data_root.join(BENCHMARK_EXPORT_DIRECTORY);
    fs::create_dir_all(&export_directory).map_err(|_| "无法创建原生验收导出目录".to_owned())?;
    let canonical_export_directory =
        fs::canonicalize(&export_directory).map_err(|_| "无法确定原生验收导出目录".to_owned())?;
    if canonical_export_directory == canonical_data_root
        || !canonical_export_directory.starts_with(&canonical_data_root)
    {
        return Err("原生验收导出目录超出隔离数据目录".to_owned());
    }
    Ok(canonical_export_directory.join(suggested_name.trim()))
}

#[cfg(feature = "native-desktop-tests")]
fn benchmark_export_path(
    app: &tauri::AppHandle,
    suggested_name: &str,
) -> Result<Option<PathBuf>, String> {
    if std::env::var_os("KEENCODE_BENCHMARK").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Ok(None);
    }
    let configured_root = std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "原生验收保存必须提供隔离数据目录".to_owned())?;
    let storage_root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    if storage_root != configured_root {
        return Err("原生验收保存数据目录与宿主隔离目录不一致".to_owned());
    }
    benchmark_export_path_from_root(&configured_root, suggested_name).map(Some)
}

/// 在用户选择的同一目录中完成写入和原子替换；不复用私有设置写入器，
/// 因为用户文件应保留现有权限，而不是被强制改成应用私有权限。
fn atomic_write_user_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "保存路径缺少父目录".to_owned())?;
    if !parent.is_dir() {
        return Err("保存路径的父目录不存在".to_owned());
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".keencode-save-")
        .tempfile_in(parent)
        .map_err(|_| "无法创建保存临时文件".to_owned())?;
    temporary
        .write_all(bytes)
        .map_err(|_| "无法写入保存临时文件".to_owned())?;
    temporary
        .flush()
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| "无法同步保存临时文件".to_owned())?;
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(temporary.path(), metadata.permissions())
            .map_err(|_| "无法保留目标文件权限".to_owned())?;
    }
    temporary
        .persist(path)
        .map_err(|_| "无法原子替换目标文件".to_owned())?;
    Ok(())
}

async fn write_saved_file(path: PathBuf, bytes: Vec<u8>) -> SaveFileResult {
    let write_path = path.clone();
    match tokio::task::spawn_blocking(move || atomic_write_user_file(&write_path, &bytes)).await {
        Ok(Ok(())) => SaveFileResult {
            success: true,
            canceled: None,
            path: Some(crate::path_utils::path_to_frontend(&path)),
            error: None,
        },
        Ok(Err(error)) => save_failure(error),
        Err(_) => save_failure("保存文件任务失败"),
    }
}

#[cfg(windows)]
fn read_pdf_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let bytes =
        crate::storage::read_private_bytes_bounded(path, MAX_FILE_BYTES as u64, "PDF 临时输出")
            .map_err(|_| "读取 PDF 输出失败".to_owned())?
            .ok_or_else(|| "PDF 输出不存在".to_owned())?;
    if !is_pdf_bytes(&bytes) {
        return Err("WebView2 未返回有效 PDF".to_owned());
    }
    Ok(bytes)
}

#[cfg(any(windows, test))]
fn is_pdf_bytes(bytes: &[u8]) -> bool {
    let tail = &bytes[bytes.len().saturating_sub(1024)..];
    bytes.starts_with(b"%PDF-")
        && tail
            .windows(b"%%EOF".len())
            .any(|window| window == b"%%EOF")
}

/// PDF 输出文件的生命周期由执行任务和 WebView2 完成回调共同持有。
///
/// 超时只结束等待方，不能立即删除路径：WebView2 可能在超时后才调用完成
/// 回调并完成文件写入。只有回调 handler 和命令任务都释放最后一个 `Arc` 后，
/// `TempPath` 才清理文件，避免晚回调留下不可控的临时 PDF。
#[cfg(windows)]
struct PrintTempFile {
    path: PathBuf,
    _temp_path: tempfile::TempPath,
}

#[cfg(windows)]
impl PrintTempFile {
    fn new() -> Result<Self, String> {
        let file = tempfile::Builder::new()
            .prefix("keencode-print-")
            .suffix(".pdf")
            .tempfile()
            .map_err(|_| "无法创建 PDF 临时文件".to_owned())?;
        let temp_path = file.into_temp_path();
        let path = temp_path.to_path_buf();
        Ok(Self {
            path,
            _temp_path: temp_path,
        })
    }
}

/// 使用系统另存为对话框保存前端提供的字节，或下载后保存 HTTP(S) 来源。
#[tauri::command]
pub(crate) async fn platform_file_save_file(
    app: tauri::AppHandle,
    caller: Webview,
    suggested_name: String,
    data: Option<Vec<u8>>,
    source_url: Option<String>,
) -> SaveFileResult {
    if let Err(error) = require_main_webview(&caller, "文件保存") {
        return save_failure(error);
    }
    let request = SaveFileRequest {
        data,
        source_url,
        suggested_name,
    };
    if let Err(error) = validate_suggested_name(&request.suggested_name) {
        return save_failure(error);
    }
    let suggested_name = request.suggested_name.trim().to_owned();
    let bytes = match tokio::task::spawn_blocking(move || request_bytes(request)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return save_failure(error),
        Err(_) => return save_failure("准备保存文件的任务失败"),
    };

    #[cfg(feature = "native-desktop-tests")]
    if let Some(path) = match benchmark_export_path(&app, &suggested_name) {
        Ok(path) => path,
        Err(error) => return save_failure(error),
    } {
        return write_saved_file(path, bytes).await;
    }

    let selected = match tokio::task::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_file_name(suggested_name)
            .blocking_save_file()
    })
    .await
    {
        Ok(Some(selected)) => selected,
        Ok(None) => {
            return SaveFileResult {
                success: false,
                canceled: Some(true),
                path: None,
                error: None,
            };
        }
        Err(_) => return save_failure("保存对话框任务失败"),
    };
    let path = match selected_path_to_absolute(selected) {
        Ok(path) => path,
        Err(error) => return save_failure(error),
    };
    if path.is_dir() {
        return save_failure("保存目标不能是目录");
    }
    write_saved_file(path, bytes).await
}

/// 将当前主 WebView 的打印媒体版面输出为 PDF；非 Windows 平台没有该能力。
#[tauri::command]
pub(crate) async fn platform_file_print_page_to_pdf(
    caller: Webview,
    page_size: Option<PrintPageSize>,
) -> PrintPageToPdfResult {
    if let Err(error) = require_main_webview(&caller, "PDF 导出") {
        return pdf_failure(error);
    }
    let page_size = match page_size {
        Some(page_size) => match page_size.validate() {
            Ok(page_size) => Some(page_size),
            Err(error) => return pdf_failure(error),
        },
        None => None,
    };
    #[cfg(windows)]
    {
        print_page_to_pdf_windows(caller, page_size).await
    }
    #[cfg(not(windows))]
    {
        let _ = (caller, page_size);
        pdf_failure("当前平台不支持 PDF 导出")
    }
}

#[cfg(windows)]
async fn print_page_to_pdf_windows(
    caller: Webview,
    page_size: Option<PrintPageSize>,
) -> PrintPageToPdfResult {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::Arc;

    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE, COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT,
        ICoreWebView2_7, ICoreWebView2Environment6, ICoreWebView2PrintSettings,
    };
    use windows_core::{Interface, PCWSTR};

    let print_guard = match acquire_pdf_print_in_flight() {
        Ok(guard) => std::sync::Arc::new(guard),
        Err(error) => return pdf_failure(error),
    };
    let temporary = match PrintTempFile::new() {
        Ok(file) => std::sync::Arc::new(file),
        Err(error) => return pdf_failure(error),
    };
    let path = temporary.path.clone();
    let path_wide: Vec<u16> = OsStr::new(&path).encode_wide().chain([0]).collect();
    let (send, receive) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let sender = Arc::new(Mutex::new(Some(send)));
    let callback_sender = Arc::clone(&sender);
    let callback_path = path_wide.clone();
    let callback_temp = Arc::clone(&temporary);
    let callback_print_guard = Arc::clone(&print_guard);

    // COM 接口只在 Tauri 提供的 WebView2 STA 回调中访问；完成结果通过 oneshot
    // 回传异步命令，避免把 COM 指针或阻塞读写带出窗口线程。
    let dispatch = caller.with_webview(move |native| {
        let handler = webview2_com::PrintToPdfCompletedHandler::create(Box::new(
            move |status, successful| {
                // 超时只结束异步等待；COM 完成 handler 仍持有 guard，防止旧打印
                // 尚未结束时启动第二个请求。handler 释放后才允许下一次打印。
                let _ = &callback_print_guard;
                // 保留动态 UTF-16 路径直到回调结束，避免晚到的 COM 回调引用已释放的输入。
                let _ = callback_path.as_ptr();
                // 保留 TempPath 到回调结束；超时返回不会提前删除 WebView2 的输出路径。
                let _ = callback_temp._temp_path.as_os_str();
                let result = match status {
                    Err(_) => Err("WebView2 PDF 打印失败".to_owned()),
                    Ok(()) if successful => Ok(()),
                    Ok(()) => Err("WebView2 未完成 PDF 打印".to_owned()),
                };
                if let Some(send) = callback_sender
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                {
                    let _ = send.send(result);
                }
                Ok(())
            },
        ));
        let started = unsafe {
            native
                .environment()
                .cast::<ICoreWebView2Environment6>()
                .and_then(|environment| environment.CreatePrintSettings())
                .and_then(|settings| {
                    // Electron 的打印契约要求保留背景并关闭页眉页脚；WebView2
                    // 的四边距单位是英寸，设置为 0 后再交给文档 @page 定尺寸。
                    settings.SetShouldPrintBackgrounds(true)?;
                    settings.SetShouldPrintHeaderAndFooter(false)?;
                    settings.SetMarginTop(PDF_PRINT_MARGIN_INCHES)?;
                    settings.SetMarginBottom(PDF_PRINT_MARGIN_INCHES)?;
                    settings.SetMarginLeft(PDF_PRINT_MARGIN_INCHES)?;
                    settings.SetMarginRight(PDF_PRINT_MARGIN_INCHES)?;

                    if let Some(page_size) = page_size {
                        let (width_inches, height_inches) = page_size.inches();
                        settings.SetPageWidth(width_inches)?;
                        settings.SetPageHeight(height_inches)?;
                        settings.SetOrientation(if page_size.is_landscape() {
                            COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE
                        } else {
                            COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT
                        })?;
                    }

                    // 0.38.2 的 WebView2 PrintSettings 没有 preferCSSPageSize，也
                    // 没有可靠的 CSS 尺寸回读接口；页面尺寸只能由 Rust 校验后的
                    // pageSize 转成英寸后设置，缺省调用仍不写 PageWidth/PageHeight。
                    native
                        .controller()
                        .CoreWebView2()
                        .and_then(|core| core.cast::<ICoreWebView2_7>())
                        .and_then(|core| {
                            core.PrintToPdf(
                                PCWSTR(path_wide.as_ptr()),
                                Option::<&ICoreWebView2PrintSettings>::Some(&settings),
                                &handler,
                            )
                        })
                })
        };
        if let Err(error) = started
            && let Some(send) = sender
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        {
            let _ = send.send(Err(format!("启动 WebView2 PDF 打印失败：{error}")));
        }
    });
    if dispatch.is_err() {
        return pdf_failure("无法调度 WebView2 PDF 打印");
    }
    match tokio::time::timeout(PRINT_TIMEOUT, receive).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => return pdf_failure(error),
        Ok(Err(_)) => return pdf_failure("PDF 打印回调已关闭"),
        Err(_) => return pdf_failure("PDF 打印超时"),
    }
    let read_path = path.clone();
    let bytes = match tokio::task::spawn_blocking(move || read_pdf_bytes(&read_path)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return pdf_failure(error),
        Err(_) => return pdf_failure("读取 PDF 输出任务失败"),
    };
    PrintPageToPdfResult {
        success: true,
        data: Some(bytes),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_trusted_main_webview_identity() {
        assert!(is_main_webview_identity("main", "main"));
        assert!(!is_main_webview_identity("browser-child", "main"));
        assert!(!is_main_webview_identity("main", "secondary"));
    }

    #[test]
    fn validates_safe_suggested_names() {
        assert!(validate_suggested_name("image.png").is_ok());
        assert!(validate_suggested_name(" ").is_err());
        assert!(validate_suggested_name("../image.png").is_err());
        assert!(validate_suggested_name("nested\\image.png").is_err());
        assert!(validate_suggested_name("bad\0name.png").is_err());
        assert!(validate_suggested_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn validates_http_source_without_accepting_credentials_or_local_schemes() {
        assert!(validate_source_url("https://example.test/image.png").is_ok());
        assert!(validate_source_url("http://example.test/path").is_ok());
        assert!(validate_source_url("file:///tmp/image.png").is_err());
        assert!(validate_source_url("https://user:secret@example.test/image.png").is_err());
        assert!(validate_source_url("data:text/plain,hello").is_err());
        assert!(
            validate_source_url(&format!(
                "https://example.test/{}",
                "x".repeat(MAX_SOURCE_URL_BYTES)
            ))
            .is_err()
        );
    }

    #[test]
    fn validates_request_source_is_exactly_one() {
        let both = SaveFileRequest {
            data: Some(vec![1]),
            source_url: Some("https://example.test/file".to_owned()),
            suggested_name: "file.bin".to_owned(),
        };
        assert!(request_bytes(both).is_err());
        let neither = SaveFileRequest {
            data: None,
            source_url: None,
            suggested_name: "file.bin".to_owned(),
        };
        assert!(request_bytes(neither).is_err());
    }

    #[test]
    fn validates_pdf_signature_and_end_marker() {
        assert!(is_pdf_bytes(b"%PDF-1.7\nbody\n%%EOF\n"));
        assert!(!is_pdf_bytes(b"not-pdf"));
        assert!(!is_pdf_bytes(b"%PDF-1.7\nbody"));
    }

    #[test]
    fn validates_print_page_size_contract_and_converts_css_pixels() {
        let wide: PrintPageSize =
            serde_json::from_str(r#"{"widthPx":1280,"heightPx":720}"#).expect("解析页面尺寸");
        let wide = wide.validate().expect("合法页面尺寸应通过");
        let (width_inches, height_inches) = wide.inches();
        assert!((width_inches - (1280.0 / 96.0)).abs() < f64::EPSILON);
        assert!((height_inches - 7.5).abs() < f64::EPSILON);
        assert!(wide.is_landscape());

        let portrait = PrintPageSize {
            width_px: 720.0,
            height_px: 1280.0,
        }
        .validate()
        .expect("合法竖版页面尺寸应通过");
        assert!(!portrait.is_landscape());

        for value in [
            -1.0,
            0.0,
            PDF_PAGE_SIZE_MIN_PX - 0.01,
            PDF_PAGE_SIZE_MAX_PX + 0.01,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert!(
                PrintPageSize {
                    width_px: value,
                    height_px: 720.0,
                }
                .validate()
                .is_err(),
                "非法宽度 {value:?} 不应进入原生打印设置"
            );
        }
        assert!(
            serde_json::from_str::<PrintPageSize>(r#"{"widthPx":1280,"heightPx":720,"scale":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<PrintPageSize>(r#"{"widthPx":"1280","heightPx":720}"#).is_err()
        );
    }

    #[test]
    fn pdf_print_in_flight_guard_rejects_overlap_and_releases_after_drop() {
        let first =
            std::sync::Arc::new(acquire_pdf_print_in_flight().expect("第一次打印应获得占用"));
        let callback_owner = std::sync::Arc::clone(&first);
        drop(first);
        assert!(acquire_pdf_print_in_flight().is_err());
        drop(callback_owner);
        assert!(acquire_pdf_print_in_flight().is_ok());
    }

    #[test]
    fn atomic_user_write_replaces_target_and_keeps_no_partial_file() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("export.bin");
        fs::write(&target, b"before").expect("写入初始文件");
        atomic_write_user_file(&target, b"after").expect("原子替换应成功");
        assert_eq!(fs::read(&target).expect("读取目标文件"), b"after");
        assert!(
            !directory
                .path()
                .read_dir()
                .expect("读取临时目录")
                .any(|entry| entry
                    .expect("读取目录项")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".keencode-save-"))
        );
    }

    #[cfg(feature = "native-desktop-tests")]
    #[test]
    fn benchmark_export_path_is_fixed_inside_isolated_data_root() {
        let directory = tempfile::tempdir().expect("创建隔离数据目录");
        let target = benchmark_export_path_from_root(directory.path(), "deck.pdf")
            .expect("应选择固定 exports 子目录");
        assert_eq!(
            target.file_name().and_then(|name| name.to_str()),
            Some("deck.pdf")
        );
        assert_eq!(
            target
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some(BENCHMARK_EXPORT_DIRECTORY)
        );
        let canonical_root = fs::canonicalize(directory.path()).expect("规范化隔离数据目录");
        assert!(target.starts_with(canonical_root));
        assert!(benchmark_export_path_from_root(directory.path(), "..\\escape.pdf").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn print_temp_file_waits_for_the_callback_owner_before_cleanup() {
        let task_owner = std::sync::Arc::new(PrintTempFile::new().expect("创建 PDF 临时文件"));
        let callback_owner = std::sync::Arc::clone(&task_owner);
        let path = task_owner.path.clone();
        assert!(path.is_file());
        drop(task_owner);
        assert!(path.is_file(), "回调仍持有临时文件时不能清理");
        drop(callback_owner);
        assert!(!path.exists(), "最后一个回调所有者释放后应清理临时文件");
    }
}
