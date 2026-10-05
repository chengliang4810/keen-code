//! 本地编辑器发现与启动。
//!
//! 这里是 Tauri 平台能力边界，不是工作流或会话状态的第二个事实源。编辑器的
//! 可执行文件只从受控的系统位置/PATH 中发现，打开目标仍由 workspace 的已登记
//! 项目根授权；因此 renderer 不能通过传入任意 editorId 或可执行文件绕过路径门禁。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, Webview};

const ICON_DATA_MIME: &str = "data:image/svg+xml;base64,";
const MAIN_WINDOW_LABEL: &str = "main";

#[derive(Clone, Debug)]
struct DiscoveredEditor {
    id: &'static str,
    name: &'static str,
    executable: PathBuf,
    kind: EditorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorKind {
    /// Windows Explorer/macOS Finder 使用系统文件管理器的显式参数。
    FileManager,
    /// 普通本地编辑器，目标路径作为一个 argv 元素传入。
    Application,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorInfo {
    pub id: String,
    pub name: String,
    pub icon_data_url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationIconInfo {
    pub icon_data_url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenInEditorResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenInEditorOptions {
    /// 远程编辑器已经从本地产品面裁剪；协议字段仍能被解码，但本地命令明确
    /// 拒绝 SSH/WSL/Docker 目标，不把远程路径误当成本地路径。
    remote_target: Option<Value>,
    workspace_identity: Option<String>,
    path_kind: Option<RequestedPathKind>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum RequestedPathKind {
    File,
    Directory,
}

#[derive(Clone, Debug)]
struct LaunchInvocation {
    executable: PathBuf,
    args: Vec<OsString>,
}

/// 返回来源 `EditorInfo` 的同形状本地编辑器清单。
#[tauri::command]
pub fn ui_editors_get_installed(caller: Webview) -> Result<Vec<EditorInfo>, String> {
    require_main_webview(&caller)?;
    Ok(discover_editors()
        .into_iter()
        .map(|editor| EditorInfo {
            id: editor.id.to_owned(),
            name: editor.name.to_owned(),
            icon_data_url: editor_icon_data_url(&editor),
        })
        .collect())
}

/// 实现 shared `getApplicationIcon` 合同；只接受已发现编辑器的身份，不接受
/// renderer 任意指定的可执行文件路径。
#[tauri::command]
pub fn ui_editors_get_icon(
    caller: Webview,
    request: Value,
) -> Result<Option<ApplicationIconInfo>, String> {
    require_main_webview(&caller)?;
    let editors = discover_editors();
    let editor = icon_request_editor(&request, &editors);
    Ok(editor.map(|editor| ApplicationIconInfo {
        icon_data_url: editor_icon_data_url(editor),
    }))
}

/// 使用已登记 workspace 路径启动本地编辑器。
#[tauri::command]
pub fn ui_editors_open(
    app: AppHandle,
    caller: Webview,
    editor_id: String,
    path: String,
    options: Option<OpenInEditorOptions>,
) -> Result<OpenInEditorResult, String> {
    require_main_webview(&caller)?;
    let options = options.unwrap_or_default();
    if options.remote_target.is_some() {
        return Err("本地编辑器不支持远程工作区".to_owned());
    }
    // workspaceIdentity 是 UI 的上下文标识，不参与授权；真实权限只由 Rust
    // 登记的项目根决定，避免 caller 伪造身份扩大可访问范围。
    let _workspace_identity = options.workspace_identity.as_deref();
    validate_editor_path_argument(Path::new(&path))?;
    let canonical_path = crate::workspace::authorize_existing_absolute(&app, Path::new(&path))?;
    validate_requested_path_kind(&canonical_path, options.path_kind)?;

    let editors = discover_editors();
    let editor = select_editor(&editors, &editor_id)?;
    let invocation = build_launch_invocation(editor, &canonical_path);
    let mut command = Command::new(&invocation.executable);
    command.args(&invocation.args);
    command.spawn().map_err(|error| {
        format!(
            "无法启动编辑器 {}（{}）：{error}",
            editor.name,
            invocation.executable.display()
        )
    })?;

    if std::env::var_os("KEENCODE_BENCHMARK").as_deref() == Some(std::ffi::OsStr::new("1")) {
        // 原生验收只需要确认受控命令已成功 spawn；禁止把路径、argv 或配置写入诊断。
        tracing::info!(
            target: "keencode_diagnostics",
            component = "ui_editors",
            editor_id = editor.id,
            success = true,
            "ui_editors_open spawn succeeded"
        );
        // 原生验收要在点击后的短窗口内读取这条脱敏事件；仅入队会让成功结果
        // 等到进程退出或下一次 flush 才可见，掩盖真实的 spawn 结果。
        if let Some(diagnostics) =
            app.try_state::<std::sync::Arc<crate::diagnostics::Diagnostics>>()
        {
            let _ = diagnostics.flush();
        }
    }

    Ok(OpenInEditorResult {
        success: true,
        error: None,
    })
}

fn is_main_webview_identity(webview_label: &str, window_label: &str) -> bool {
    webview_label == MAIN_WINDOW_LABEL && window_label == MAIN_WINDOW_LABEL
}

fn require_main_webview(caller: &Webview) -> Result<(), String> {
    if is_main_webview_identity(caller.label(), caller.window().label()) {
        Ok(())
    } else {
        Err("编辑器能力只允许主窗口主 WebView 调用".to_owned())
    }
}

fn select_editor<'a>(
    editors: &'a [DiscoveredEditor],
    editor_id: &str,
) -> Result<&'a DiscoveredEditor, String> {
    editors
        .iter()
        .find(|candidate| candidate.id == editor_id)
        .ok_or_else(|| format!("未知或未安装的编辑器：{editor_id}"))
}

fn validate_requested_path_kind(
    path: &Path,
    requested: Option<RequestedPathKind>,
) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("无法读取目标路径 {}：{error}", path.display()))?;
    match requested {
        Some(RequestedPathKind::File) if !metadata.is_file() => {
            Err(format!("目标路径不是文件：{}", path.display()))
        }
        Some(RequestedPathKind::Directory) if !metadata.is_dir() => {
            Err(format!("目标路径不是目录：{}", path.display()))
        }
        _ => Ok(()),
    }
}

fn validate_editor_path_argument(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("路径必须是绝对路径".to_owned());
    }
    if path.as_os_str().to_string_lossy().contains('\0') {
        return Err("路径不能包含 NUL 字符".to_owned());
    }
    Ok(())
}

fn build_launch_invocation(editor: &DiscoveredEditor, path: &Path) -> LaunchInvocation {
    #[cfg(target_os = "macos")]
    if editor.kind == EditorKind::Application {
        // macOS 的发现结果是 .app bundle，必须由系统 `open -a` 解析，不能把
        // bundle 目录误当作可执行文件；参数仍保持逐个 argv 元素。
        return LaunchInvocation {
            executable: PathBuf::from("/usr/bin/open"),
            args: vec![
                OsString::from("-a"),
                editor.executable.as_os_str().to_owned(),
                path.as_os_str().to_owned(),
            ],
        };
    }

    let args = match editor.kind {
        EditorKind::Application => vec![path.as_os_str().to_owned()],
        EditorKind::FileManager => file_manager_args(path),
    };
    LaunchInvocation {
        executable: editor.executable.clone(),
        args,
    }
}

#[cfg(target_os = "windows")]
fn file_manager_args(path: &Path) -> Vec<OsString> {
    if path.is_dir() {
        vec![path.as_os_str().to_owned()]
    } else {
        // Explorer 的 /select, 参数必须与路径组成一个 argv 元素；不能通过 cmd
        // /C 或字符串拼接把文件名重新交给 shell 解析。
        vec![OsString::from(format!(
            "/select,{}",
            path.to_string_lossy()
        ))]
    }
}

#[cfg(target_os = "macos")]
fn file_manager_args(path: &Path) -> Vec<OsString> {
    if path.is_dir() {
        vec![path.as_os_str().to_owned()]
    } else {
        vec![OsString::from("-R"), path.as_os_str().to_owned()]
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn file_manager_args(path: &Path) -> Vec<OsString> {
    vec![path.as_os_str().to_owned()]
}

fn icon_request_editor<'a>(
    request: &Value,
    editors: &'a [DiscoveredEditor],
) -> Option<&'a DiscoveredEditor> {
    match request {
        Value::String(id) => editors
            .iter()
            .find(|editor| editor.id == id || editor_id_for_bundle_id(id) == Some(editor.id)),
        Value::Object(object) => {
            let locators = object.get("locators")?.as_array()?;
            for locator in locators {
                let kind = locator.get("kind")?.as_str()?;
                let value = locator.get("value")?.as_str()?;
                if kind == "darwin-bundle-id" {
                    let editor_id = editor_id_for_bundle_id(value)?;
                    return editors.iter().find(|editor| editor.id == editor_id);
                }
                if kind == "windows-executable-path" {
                    let canonical = fs::canonicalize(value).ok()?;
                    return editors.iter().find(|editor| {
                        fs::canonicalize(&editor.executable)
                            .map(|candidate| candidate == canonical)
                            .unwrap_or(false)
                    });
                }
            }
            None
        }
        _ => None,
    }
}

fn editor_id_for_bundle_id(bundle_id: &str) -> Option<&'static str> {
    match bundle_id {
        "com.microsoft.VSCode" => Some("vscode"),
        "com.microsoft.VSCodeInsiders" => Some("vscode-insiders"),
        "com.todesktop.230313mzl4w4u92" => Some("cursor"),
        "dev.zed.Zed" => Some("zed"),
        _ => None,
    }
}

fn editor_icon_data_url(editor: &DiscoveredEditor) -> String {
    #[cfg(target_os = "windows")]
    if let Some(data_url) = windows_icon_data_url(&editor.executable) {
        return data_url;
    }

    // macOS/Linux 的图标 API 不属于当前桌面依赖图。此稳定 SVG 仅在系统图标
    // 不可读取时作为同一 editorId 的可识别回退，不影响发现或启动的真实结果。
    fallback_icon_data_url(editor.id)
}

fn fallback_icon_data_url(editor_id: &str) -> String {
    let glyph = match editor_id {
        "explorer" => "E",
        "finder" => "F",
        "vscode" | "vscode-insiders" => "V",
        "cursor" => "C",
        "zed" => "Z",
        _ => "•",
    };
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 32 32\"><rect width=\"32\" height=\"32\" rx=\"6\" fill=\"#52606d\"/><text x=\"16\" y=\"22\" fill=\"white\" font-family=\"sans-serif\" font-size=\"16\" text-anchor=\"middle\">{glyph}</text></svg>"
    );
    format!(
        "{ICON_DATA_MIME}{}",
        base64::engine::general_purpose::STANDARD.encode(svg.as_bytes())
    )
}

fn discover_editors() -> Vec<DiscoveredEditor> {
    let mut editors = Vec::new();

    #[cfg(target_os = "windows")]
    discover_windows_editors(&mut editors);
    #[cfg(target_os = "macos")]
    discover_macos_editors(&mut editors);
    #[cfg(all(unix, not(target_os = "macos")))]
    discover_unix_editors(&mut editors);

    editors
}

fn push_editor(
    editors: &mut Vec<DiscoveredEditor>,
    id: &'static str,
    name: &'static str,
    executable: PathBuf,
    kind: EditorKind,
) {
    let exists = match kind {
        EditorKind::Application => {
            executable.is_file() || (cfg!(target_os = "macos") && executable.is_dir())
        }
        // Finder uses `/usr/bin/open`; Windows Explorer is an executable too. The
        // directory check also supports macOS app bundles used by `open -a`.
        EditorKind::FileManager => executable.is_file() || executable.is_dir(),
    };
    if !exists || editors.iter().any(|editor| editor.id == id) {
        return;
    }
    editors.push(DiscoveredEditor {
        id,
        name,
        executable,
        kind,
    });
}

fn push_first_existing(
    editors: &mut Vec<DiscoveredEditor>,
    id: &'static str,
    name: &'static str,
    candidates: impl IntoIterator<Item = PathBuf>,
    kind: EditorKind,
) {
    for candidate in candidates {
        if candidate.exists() {
            push_editor(editors, id, name, candidate, kind);
            return;
        }
    }
}

fn path_command(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.is_absolute() && command_path.is_file() {
        return Some(command_path.to_owned());
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| {
            let candidate = directory.join(command);
            candidate.is_file().then_some(candidate)
        })
    })
}

#[cfg(target_os = "windows")]
fn windows_env_path(key: &str, suffix: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(|root| PathBuf::from(root).join(suffix))
}

#[cfg(target_os = "windows")]
fn discover_windows_editors(editors: &mut Vec<DiscoveredEditor>) {
    let explorer = windows_env_path("WINDIR", "explorer.exe")
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\explorer.exe"));
    push_editor(
        editors,
        "explorer",
        "资源管理器",
        explorer,
        EditorKind::FileManager,
    );

    push_first_existing(
        editors,
        "vscode",
        "VS Code",
        [
            windows_env_path("LOCALAPPDATA", r"Programs\Microsoft VS Code\Code.exe"),
            windows_env_path("ProgramFiles", r"Microsoft VS Code\Code.exe"),
            windows_env_path("ProgramFiles(x86)", r"Microsoft VS Code\Code.exe"),
            path_command("code.exe"),
        ]
        .into_iter()
        .flatten(),
        EditorKind::Application,
    );
    push_first_existing(
        editors,
        "vscode-insiders",
        "VS Code Insiders",
        [
            windows_env_path(
                "LOCALAPPDATA",
                r"Programs\Microsoft VS Code Insiders\Code - Insiders.exe",
            ),
            windows_env_path(
                "ProgramFiles",
                r"Microsoft VS Code Insiders\Code - Insiders.exe",
            ),
            path_command("code-insiders.exe"),
        ]
        .into_iter()
        .flatten(),
        EditorKind::Application,
    );
    push_first_existing(
        editors,
        "cursor",
        "Cursor",
        [
            windows_env_path("LOCALAPPDATA", r"Programs\cursor\Cursor.exe"),
            windows_env_path("LOCALAPPDATA", r"Programs\Cursor\Cursor.exe"),
            path_command("cursor.exe"),
        ]
        .into_iter()
        .flatten(),
        EditorKind::Application,
    );
    push_first_existing(
        editors,
        "zed",
        "Zed",
        [
            windows_env_path("LOCALAPPDATA", r"Programs\Zed\zed.exe"),
            path_command("zed.exe"),
        ]
        .into_iter()
        .flatten(),
        EditorKind::Application,
    );
}

#[cfg(target_os = "macos")]
fn discover_macos_editors(editors: &mut Vec<DiscoveredEditor>) {
    push_editor(
        editors,
        "finder",
        "Finder",
        PathBuf::from("/usr/bin/open"),
        EditorKind::FileManager,
    );
    for (id, name, app) in [
        ("vscode", "VS Code", "Visual Studio Code.app"),
        (
            "vscode-insiders",
            "VS Code Insiders",
            "Visual Studio Code - Insiders.app",
        ),
        ("cursor", "Cursor", "Cursor.app"),
        ("zed", "Zed", "Zed.app"),
    ] {
        push_first_existing(
            editors,
            id,
            name,
            [
                PathBuf::from("/Applications").join(app),
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join("Applications").join(app)),
            ]
            .into_iter()
            .flatten(),
            EditorKind::Application,
        );
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn discover_unix_editors(editors: &mut Vec<DiscoveredEditor>) {
    for (id, name, command) in [
        ("vscode", "VS Code", "code"),
        ("vscode-insiders", "VS Code Insiders", "code-insiders"),
        ("zed", "Zed", "zed"),
        ("cursor", "Cursor", "cursor"),
    ] {
        if let Some(path) = path_command(command) {
            push_editor(editors, id, name, path, EditorKind::Application);
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_icon_data_url(path: &Path) -> Option<String> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetDIBits, GetObjectW,
        ReleaseDC,
    };
    use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut file_info = SHFILEINFOW::default();
    let file_info_size = size_of::<SHFILEINFOW>() as u32;
    let result = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut file_info,
            file_info_size,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if result == 0 || file_info.hIcon.is_null() {
        return None;
    }

    let icon = file_info.hIcon;
    let mut icon_info = ICONINFO::default();
    let success = unsafe { GetIconInfo(icon, &mut icon_info) != 0 };
    if !success {
        unsafe { DestroyIcon(icon) };
        return None;
    }
    let cleanup = IconBitmapCleanup {
        icon,
        color: icon_info.hbmColor,
        mask: icon_info.hbmMask,
    };

    let bitmap = icon_info.hbmColor;
    if bitmap.is_null() {
        return None;
    }
    let mut dimensions = BITMAP::default();
    let object_size = unsafe {
        GetObjectW(
            bitmap,
            size_of::<BITMAP>() as i32,
            (&mut dimensions as *mut BITMAP).cast(),
        )
    };
    if object_size <= 0 || dimensions.bmWidth <= 0 || dimensions.bmHeight <= 0 {
        return None;
    }
    let width = dimensions.bmWidth as u32;
    let height = dimensions.bmHeight as u32;
    if width > 256 || height > 256 {
        return None;
    }

    let mut bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: dimensions.bmWidth,
            // Negative height requests top-down rows, avoiding a second image flip.
            biHeight: -dimensions.bmHeight,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..BITMAPINFOHEADER::default()
        },
        bmiColors: [unsafe { zeroed() }],
    };
    let mut bgra = vec![0_u8; width as usize * height as usize * 4];
    let dc = unsafe { GetDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return None;
    }
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height,
            bgra.as_mut_ptr().cast(),
            &mut bitmap_info,
            DIB_RGB_COLORS,
        )
    };
    unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
    if lines == 0 {
        return None;
    }

    let mut rgba = bgra;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let mut png_data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_data, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&rgba).ok()?;
    }
    drop(cleanup);
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png_data)
    ))
}

#[cfg(target_os = "windows")]
struct IconBitmapCleanup {
    icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
    color: windows_sys::Win32::Graphics::Gdi::HBITMAP,
    mask: windows_sys::Win32::Graphics::Gdi::HBITMAP,
}

#[cfg(target_os = "windows")]
impl Drop for IconBitmapCleanup {
    fn drop(&mut self) {
        use windows_sys::Win32::Graphics::Gdi::DeleteObject;
        use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
        unsafe {
            if !self.color.is_null() {
                DeleteObject(self.color);
            }
            if !self.mask.is_null() {
                DeleteObject(self.mask);
            }
            if !self.icon.is_null() {
                DestroyIcon(self.icon);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_editor(id: &'static str, kind: EditorKind) -> DiscoveredEditor {
        DiscoveredEditor {
            id,
            name: "测试编辑器",
            executable: PathBuf::from(if cfg!(target_os = "windows") {
                r"C:\Windows\System32\editor.exe"
            } else {
                "/usr/bin/editor"
            }),
            kind,
        }
    }

    #[test]
    fn unknown_icon_request_does_not_select_an_arbitrary_path() {
        let editors = vec![test_editor("vscode", EditorKind::Application)];
        let request = serde_json::json!({
            "locators": [{
                "kind": "windows-executable-path",
                "value": "C:\\not-allowlisted\\editor.exe"
            }]
        });
        assert!(icon_request_editor(&request, &editors).is_none());
    }

    #[test]
    fn editor_id_icon_request_matches_only_known_editor() {
        let editors = vec![test_editor("vscode", EditorKind::Application)];
        assert_eq!(
            icon_request_editor(&Value::String("vscode".into()), &editors).map(|e| e.id),
            Some("vscode")
        );
        assert!(icon_request_editor(&Value::String("arbitrary".into()), &editors).is_none());
    }

    #[test]
    fn unknown_editor_id_cannot_create_a_launch_target() {
        let editors = vec![test_editor("vscode", EditorKind::Application)];
        let error = select_editor(&editors, "arbitrary-executable").expect_err("must reject");
        assert!(error.contains("未知或未安装"));
    }

    #[test]
    fn child_webview_identity_is_rejected() {
        assert!(is_main_webview_identity("main", "main"));
        assert!(!is_main_webview_identity("browser-child", "main"));
        assert!(!is_main_webview_identity("main", "settings"));
    }

    #[test]
    fn file_manager_path_is_one_argument_and_never_shell_code() {
        let path = if cfg!(target_os = "windows") {
            PathBuf::from(r"C:\project\a & b.txt")
        } else {
            PathBuf::from("/tmp/a & b.txt")
        };
        let editor = test_editor(
            if cfg!(target_os = "windows") {
                "explorer"
            } else {
                "finder"
            },
            EditorKind::FileManager,
        );
        let invocation = build_launch_invocation(&editor, &path);
        assert!(
            !invocation
                .args
                .iter()
                .any(|arg| arg == std::ffi::OsStr::new("/C"))
        );
        if cfg!(target_os = "windows") {
            assert_eq!(invocation.args.len(), 1);
            assert!(invocation.args[0].to_string_lossy().contains("a & b.txt"));
        }
    }

    #[test]
    fn application_path_is_passed_as_one_argument() {
        let path = PathBuf::from(if cfg!(target_os = "windows") {
            r"C:\project\quoted & path\file.txt"
        } else {
            "/tmp/quoted & path/file.txt"
        });
        let invocation =
            build_launch_invocation(&test_editor("vscode", EditorKind::Application), &path);
        assert_eq!(invocation.args, vec![path.into_os_string()]);
    }

    #[test]
    fn fallback_icon_is_a_nonempty_data_url() {
        let icon = fallback_icon_data_url("vscode");
        assert!(icon.starts_with(ICON_DATA_MIME));
        assert!(icon.len() > ICON_DATA_MIME.len());
    }

    #[test]
    fn requested_path_kind_rejects_mismatch() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().canonicalize().expect("canonical path");
        assert!(validate_requested_path_kind(&path, Some(RequestedPathKind::File)).is_ok());
        assert!(validate_requested_path_kind(&path, Some(RequestedPathKind::Directory)).is_err());
    }

    #[test]
    fn relative_or_nul_path_is_rejected_before_authorization() {
        assert!(validate_editor_path_argument(Path::new("relative/file.txt")).is_err());
        assert!(validate_editor_path_argument(Path::new("/tmp/with\0nul")).is_err());
    }
}
