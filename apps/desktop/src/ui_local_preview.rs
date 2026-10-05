//! 原文件预览的数据接口：临时授权只绑定一个真实文件，不增加项目权限或写入权限。

use serde::Serialize;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{
    AppHandle, Manager, Webview,
    http::{Request, Response},
};

const GRANT_TTL: Duration = Duration::from_secs(120);
const MAX_GRANTS: usize = 2048;
// 二进制按请求读取且有硬上限，不启动本地 HTTP 服务或常驻文件监听器。
const MAX_MEDIA_BYTES: u64 = 32 * 1024 * 1024;

fn is_main_preview_identity(webview_label: &str, window_label: &str) -> bool {
    webview_label == "main" && window_label == "main"
}

fn require_main_preview_webview(webview: &Webview, message: &str) -> Result<(), String> {
    // child WebView 与主界面共享宿主 Window，必须同时校验 caller 与宿主身份，避免子页面冒充主界面。
    if is_main_preview_identity(webview.label(), webview.window().label()) {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

/// 原 Markdown 的 file:///D:/... 经 URL.pathname 会得到 /D:/...；仅在 Windows 还原盘符路径。
fn preview_path(input: &str) -> PathBuf {
    #[cfg(windows)]
    if input.starts_with('/')
        && input.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
        && input.as_bytes().get(2) == Some(&b':')
        && input.as_bytes().get(3) == Some(&b'/')
    {
        return PathBuf::from(&input[1..]);
    }
    PathBuf::from(input)
}

#[derive(Default)]
pub struct PreviewGrants(Mutex<HashMap<String, Grant>>);
struct Grant {
    path: PathBuf,
    expires: Instant,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantResult {
    grant: String,
    expires_at: String,
}

impl PreviewGrants {
    fn issue(&self, path: &Path) -> Result<GrantResult, String> {
        let path = preview_path(&path.to_string_lossy());
        if !path.is_absolute() {
            return Err("预览授权只接受绝对文件路径".into());
        }
        let path = path
            .canonicalize()
            .map_err(|error| format!("预览文件不存在：{error}"))?;
        if !path.is_file() {
            return Err("预览目标不是文件".into());
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| format!("无法生成预览授权：{error}"))?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut grants = self.0.lock().map_err(|_| "预览授权锁已损坏")?;
        let now = Instant::now();
        grants.retain(|_, grant| grant.expires > now);
        if grants.len() >= MAX_GRANTS {
            return Err("临时预览授权过多，请稍后重试".into());
        }
        grants.insert(
            token.clone(),
            Grant {
                path,
                expires: now + GRANT_TTL,
            },
        );
        Ok(GrantResult {
            grant: token,
            expires_at: (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339(),
        })
    }

    fn authorize(&self, path: &Path, token: &str) -> Result<PathBuf, String> {
        let path = preview_path(&path.to_string_lossy());
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("预览文件不存在：{error}"))?;
        let mut grants = self.0.lock().map_err(|_| "预览授权锁已损坏")?;
        grants.retain(|_, grant| grant.expires > Instant::now());
        match grants.get(token) {
            Some(grant) if grant.path == canonical && canonical.is_file() => Ok(canonical),
            _ => Err("文件预览授权无效或已过期".into()),
        }
    }
}

#[tauri::command]
pub async fn ui_local_preview_grant(
    app: AppHandle,
    window: Webview,
    path: String,
) -> Result<GrantResult, String> {
    require_main_preview_webview(&window, "仅主界面可申请文件预览")?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PreviewGrants>().issue(Path::new(&path))
    })
    .await
    .map_err(|error| format!("预览授权后台任务失败：{error}"))?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextResult {
    file: crate::workspace::FsReadResult,
    symlink: bool,
}

#[tauri::command]
pub async fn ui_local_preview_read(
    app: AppHandle,
    window: Webview,
    path: String,
    grant: String,
    max_bytes: Option<usize>,
) -> Result<TextResult, String> {
    require_main_preview_webview(&window, "仅主界面可读取文件预览")?;
    let limit = max_bytes.unwrap_or(1_000_000);
    if !(1..=1_000_000).contains(&limit) {
        return Err("文本预览读取上限无效".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let input = preview_path(&path);
        let resolved = app.state::<PreviewGrants>().authorize(&input, &grant)?;
        let symlink = fs::symlink_metadata(&input)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);
        let file = crate::workspace::read_file_preview_with_limit(
            &resolved,
            crate::path_utils::path_to_frontend(&resolved),
            limit,
        )?;
        Ok(TextResult { file, symlink })
    })
    .await
    .map_err(|error| format!("文件预览后台任务失败：{error}"))?
}

/// 定位失败的相对引用，仅在用户 home 内向上查找；不读取内容、不自动签发授权。
#[tauri::command]
pub async fn ui_resolve_out_of_root(
    app: AppHandle,
    cwd: String,
    relative_path: String,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &cwd)?;
        let home = app.path().home_dir().map_err(|error| error.to_string())?;
        Ok(resolve_ancestor(&root, &home, &relative_path)
            .map(|path| crate::path_utils::path_to_frontend(&path)))
    })
    .await
    .map_err(|error| format!("文件引用定位后台任务失败：{error}"))?
}

fn resolve_ancestor(root: &Path, home: &Path, relative: &str) -> Option<PathBuf> {
    let relative = relative.trim().replace('\\', "/");
    if relative.is_empty()
        || relative.contains('\0')
        || relative.split('/').any(|part| part == "." || part == "..")
    {
        return None;
    }
    let relative = Path::new(&relative);
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let home = home.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    if !root.starts_with(&home) {
        return None;
    }
    // 已存在或访问失败的根内文件不能被同名祖先文件替换。
    match fs::metadata(root.join(relative)) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) => {}
        _ => return None,
    }
    for ancestor in root.ancestors().skip(1).take(32) {
        if !ancestor.starts_with(&home) {
            break;
        }
        if let Ok(candidate) = ancestor.join(relative).canonicalize()
            && candidate.starts_with(&home)
            && candidate.is_file()
        {
            return Some(candidate);
        }
    }
    None
}

fn media_mime(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "avif" => "image/avif",
            "bmp" => "image/bmp",
            "ico" => "image/x-icon",
            "heic" => "image/heic",
            "heif" => "image/heif",
            "tif" | "tiff" => "image/tiff",
            "pdf" => "application/pdf",
            _ => return None,
        },
    )
}

/// 原 /api/local-image URL 通过专用 Tauri 协议读取；每次请求重新验证路径和授权期限。
pub fn protocol_response(
    app: &AppHandle,
    main_webview: bool,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let mut origins = vec![
        "http://tauri.localhost".to_owned(),
        "https://tauri.localhost".to_owned(),
        "tauri://localhost".to_owned(),
    ];
    if let Some(url) = &app.config().build.dev_url {
        origins.push(url.origin().ascii_serialization());
    }
    media_response(main_webview, request, &origins, |params| {
        let path = params.get("path").ok_or("缺少预览路径")?;
        let input = preview_path(path);
        if input.is_absolute() {
            if let Some(grant) = params.get("grant") {
                app.state::<PreviewGrants>().authorize(&input, grant)
            } else {
                crate::workspace::authorize_existing_absolute(app, &input)
            }
        } else {
            let root = crate::workspace::registered_project_root(
                app,
                params.get("cwd").ok_or("相对预览缺少项目目录")?,
            )?;
            crate::workspace::resolve_existing_under_root(&root, path, false)
        }
    })
}

/// 将 HTTP 校验与文件定位分开，单测覆盖原 URL 参数、跨源限制和过期授权响应。
fn media_response(
    main_webview: bool,
    request: Request<Vec<u8>>,
    origins: &[String],
    resolve: impl FnOnce(&HashMap<String, String>) -> Result<PathBuf, String>,
) -> Response<Vec<u8>> {
    let origin = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok());
    let allowed_origin = origin.filter(|origin| origins.iter().any(|allowed| allowed == *origin));
    let response = |status, body: Vec<u8>, mime: &str, download: bool| {
        let mut builder = Response::builder()
            .status(status)
            .header("Content-Type", mime)
            .header("Cache-Control", "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .header(
                "Content-Security-Policy",
                "sandbox; default-src 'none'; style-src 'unsafe-inline'",
            );
        if let Some(origin) = allowed_origin {
            builder = builder
                .header("Access-Control-Allow-Origin", origin)
                .header("Vary", "Origin");
        }
        if download {
            builder = builder.header("Content-Disposition", "attachment");
        }
        builder.body(body).expect("固定预览响应头有效")
    };
    if !main_webview || origin.is_some() && allowed_origin.is_none() {
        return response(403, vec![], "text/plain", false);
    }
    if request.method() != "GET" {
        return response(405, vec![], "text/plain", false);
    }
    let Ok(url) = url::Url::parse(&request.uri().to_string()) else {
        return response(400, vec![], "text/plain", false);
    };
    if url.path() != "/api/local-image" {
        return response(404, vec![], "text/plain", false);
    }
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let result = resolve(&params).and_then(|resolved| read_media(&resolved));
    match result {
        Ok((mime, body)) => response(
            200,
            body,
            mime,
            params.get("download").is_some_and(|value| value == "1"),
        ),
        Err(error) => response(
            if error == "预览文件超过32MiB上限" {
                413
            } else {
                403
            },
            error.into_bytes(),
            "text/plain; charset=utf-8",
            false,
        ),
    }
}

fn read_media(path: &Path) -> Result<(&'static str, Vec<u8>), String> {
    let mime = media_mime(path).ok_or("此文件不是受支持的图片或 PDF")?;
    let file = File::open(path).map_err(|error| format!("无法打开预览文件：{error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("预览目标不是文件".into());
    }
    if metadata.len() > MAX_MEDIA_BYTES {
        return Err("预览文件超过32MiB上限".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_MEDIA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_MEDIA_BYTES {
        return Err("预览文件超过32MiB上限".into());
    }
    Ok((mime, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_identity_rejects_child_webview_on_main_window() {
        assert!(is_main_preview_identity("main", "main"));
        assert!(!is_main_preview_identity("browser-746162", "main"));
        assert!(!is_main_preview_identity("main", "secondary"));
    }

    #[cfg(windows)]
    #[test]
    fn markdown_file_uri_drive_path_uses_same_granted_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("中文.png");
        fs::write(&file, b"png").unwrap();
        let regular = crate::path_utils::path_to_frontend(&file);
        let markdown_path = format!("/{regular}");
        let grants = PreviewGrants::default();
        let issued = grants.issue(Path::new(&markdown_path)).unwrap();
        assert_eq!(
            grants.authorize(&file, &issued.grant).unwrap(),
            file.canonicalize().unwrap()
        );
        assert!(
            grants
                .authorize(Path::new(&markdown_path), &issued.grant)
                .is_ok()
        );
    }

    #[test]
    fn protocol_reads_original_url_and_rejects_wrong_grants_origins_and_guest_webviews() {
        let temp = tempfile::tempdir().unwrap();
        let pdf = temp.path().join("中文 #1.pdf");
        fs::write(&pdf, b"%PDF-1.7\n").unwrap();
        let grants = PreviewGrants::default();
        let grant = grants.issue(&pdf).unwrap();
        let mut url = url::Url::parse("http://local-preview.localhost/api/local-image").unwrap();
        url.query_pairs_mut()
            .append_pair("path", &pdf.to_string_lossy())
            .append_pair("grant", &grant.grant)
            .append_pair("download", "1");
        let origins = ["http://tauri.localhost".to_owned()];
        let request = |origin: &str| {
            Request::builder()
                .uri(url.as_str())
                .header("Origin", origin)
                .body(vec![])
                .unwrap()
        };
        let response = media_response(true, request(&origins[0]), &origins, |params| {
            grants.authorize(Path::new(&params["path"]), &params["grant"])
        });
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"%PDF-1.7\n");
        assert_eq!(response.headers()["Content-Type"], "application/pdf");
        assert_eq!(response.headers()["Cache-Control"], "no-store");
        assert_eq!(
            response.headers()["Access-Control-Allow-Origin"],
            origins[0]
        );
        assert_eq!(response.headers()["Content-Disposition"], "attachment");
        assert_eq!(
            media_response(false, request(&origins[0]), &origins, |_| panic!(
                "guest不得定位文件"
            ))
            .status(),
            403
        );
        assert_eq!(
            media_response(
                true,
                request("https://external.test"),
                &origins,
                |_| panic!("外部来源不得定位文件")
            )
            .status(),
            403
        );
        grants
            .0
            .lock()
            .unwrap()
            .get_mut(&grant.grant)
            .unwrap()
            .expires = Instant::now();
        assert_eq!(
            media_response(true, request(&origins[0]), &origins, |params| grants
                .authorize(Path::new(&params["path"]), &params["grant"]))
            .status(),
            403
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacing_symlink_target_does_not_reuse_existing_file_grant() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let other = temp.path().join("other.txt");
        let link = temp.path().join("link.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&other, "other").unwrap();
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let grants = PreviewGrants::default();
        let grant = grants.issue(&link).unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(grants.authorize(&link, &grant.grant).is_err());
    }

    #[test]
    fn grants_are_file_bound_random_and_expire_without_changing_project_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let other = temp.path().join("other.txt");
        fs::write(&first, "中文").unwrap();
        fs::write(&other, "other").unwrap();
        let grants = PreviewGrants::default();
        let issued = grants.issue(&first).unwrap();
        let renewed = grants.issue(&first).unwrap();
        assert_ne!(issued.grant, renewed.grant);
        assert!(chrono::DateTime::parse_from_rfc3339(&issued.expires_at).is_ok());
        assert_eq!(
            grants.authorize(&first, &issued.grant).unwrap(),
            first.canonicalize().unwrap()
        );
        assert!(grants.authorize(&other, &issued.grant).is_err());
        assert!(grants.authorize(&first, "invented").is_err());
        grants
            .0
            .lock()
            .unwrap()
            .get_mut(&issued.grant)
            .unwrap()
            .expires = Instant::now();
        assert!(grants.authorize(&first, &issued.grant).is_err());
        assert!(grants.issue(temp.path()).is_err());
    }

    #[test]
    fn ancestor_resolution_stays_inside_home_and_never_replaces_existing_reference() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home.join("project/sub");
        fs::create_dir_all(&root).unwrap();
        fs::write(home.join("project/note.md"), "nearest").unwrap();
        fs::write(home.join("note.md"), "home").unwrap();
        assert_eq!(
            resolve_ancestor(&root, &home, "note.md"),
            Some(home.join("project/note.md").canonicalize().unwrap())
        );
        assert!(resolve_ancestor(&root, &home, "../note.md").is_none());
        assert!(resolve_ancestor(&root, &home, "./note.md").is_none());
        assert!(resolve_ancestor(temp.path(), &home, "note.md").is_none());
        fs::create_dir(root.join("note.md")).unwrap();
        assert!(resolve_ancestor(&root, &home, "note.md").is_none());
    }

    #[test]
    fn preview_reads_limit_utf8_without_splitting_characters_and_media_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let text = temp.path().join("中文.txt");
        fs::write(&text, "a中文").unwrap();
        let file =
            crate::workspace::read_file_preview_with_limit(&text, "中文.txt".into(), 3).unwrap();
        assert!(file.truncated);
        assert_eq!(file.text.as_deref(), Some("a"));
        let pdf = temp.path().join("test.pdf");
        fs::write(&pdf, b"%PDF-1.7\n").unwrap();
        assert_eq!(
            read_media(&pdf).unwrap(),
            ("application/pdf", b"%PDF-1.7\n".to_vec())
        );
        assert!(read_media(&text).is_err());
        File::create(&pdf)
            .unwrap()
            .set_len(MAX_MEDIA_BYTES + 1)
            .unwrap();
        assert!(read_media(&pdf).unwrap_err().contains("32MiB"));
    }
}
