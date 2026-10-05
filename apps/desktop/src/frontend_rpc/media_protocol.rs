//! 短时本地媒体预览使用的 `zcode-media` Tauri 协议。
//!
//! URL 只代表一次能力租约。每个请求都必须携带服务层签发的完整路径和
//! preview id，服务层会在读取字节前再次校验租约与规范化路径。

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

use tauri::{
    AppHandle,
    http::{Request, Response},
};
use url::Url;

use super::services::authorize_media_preview_path;

/// Tauri 自定义协议响应体在此处以内存承载，因此限制单次返回大小。
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
const MEDIA_SCHEME: &str = "zcode-media";
const MEDIA_HOST: &str = "local";
const MEDIA_ALIAS_SCHEME: &str = "http";
const MEDIA_ALIAS_HOST: &str = "zcode-media.localhost";
const MEDIA_PATH: &str = "/preview";

/// 为主 WebView 的媒体请求建立响应。
///
/// Tauri 构建器负责注册协议并传入 WebView 标记；文件授权始终来自服务
/// 层租约，处理器不会因为协议注册而获得通用文件系统访问权。
pub(crate) fn protocol_response(
    app: &AppHandle,
    main_webview: bool,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let origin = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let allowed_origin = origin
        .as_deref()
        .filter(|origin| {
            allowed_origins(app)
                .iter()
                .any(|allowed| allowed == *origin)
        })
        .map(str::to_owned);

    if !main_webview || origin.is_some() && allowed_origin.is_none() {
        return error_response(403, allowed_origin.as_deref(), "媒体预览来源不被允许");
    }
    if request.method() != "GET" && request.method() != "HEAD" {
        return error_response(405, allowed_origin.as_deref(), "只支持 GET 或 HEAD");
    }

    let Ok(url) = Url::parse(&request.uri().to_string()) else {
        return error_response(400, allowed_origin.as_deref(), "媒体预览 URL 无效");
    };
    if !is_media_url(&url) {
        return error_response(404, allowed_origin.as_deref(), "媒体预览资源不存在");
    }
    let Ok(path) = single_query_value(&url, "path") else {
        return error_response(400, allowed_origin.as_deref(), "媒体预览缺少唯一 path");
    };
    let Ok(preview_id) = single_query_value(&url, "previewId") else {
        return error_response(400, allowed_origin.as_deref(), "媒体预览缺少唯一 previewId");
    };
    if preview_id.len() > 256 || preview_id.chars().any(char::is_control) {
        return error_response(400, allowed_origin.as_deref(), "媒体预览 previewId 无效");
    }

    let requested_path = PathBuf::from(path);
    let range = request
        .headers()
        .get("range")
        .and_then(|value| value.to_str().ok());
    match read_authorized_media_response(
        &preview_id,
        &requested_path,
        request.method() == "HEAD",
        range,
    ) {
        Ok(media) => media_response(
            media.status,
            media.body,
            media.content_type,
            media.content_length,
            media.content_range,
            allowed_origin.as_deref(),
        ),
        Err(MediaError::InvalidRange { size }) => {
            range_error_response(allowed_origin.as_deref(), size)
        }
        Err(MediaError::Unauthorized) => {
            error_response(403, allowed_origin.as_deref(), "媒体预览授权无效或已过期")
        }
        Err(MediaError::TooLarge) => {
            error_response(413, allowed_origin.as_deref(), "媒体 range 超过读取上限")
        }
        Err(MediaError::NotFound) => {
            error_response(404, allowed_origin.as_deref(), "媒体文件不存在")
        }
        Err(MediaError::Read) => error_response(500, allowed_origin.as_deref(), "媒体文件读取失败"),
    }
}

/// 接受原生 scheme 和 Windows WebView2/Wry 暴露的 localhost 别名。
///
/// 两种形态都必须使用固定 host 与 `/preview` 路径，不能借此扩大协议
/// 到其他 HTTP 主机或路径。
fn is_media_url(url: &Url) -> bool {
    ((url.scheme() == MEDIA_SCHEME && url.host_str() == Some(MEDIA_HOST))
        || (url.scheme() == MEDIA_ALIAS_SCHEME && url.host_str() == Some(MEDIA_ALIAS_HOST)))
        && url.path() == MEDIA_PATH
}

fn allowed_origins(app: &AppHandle) -> Vec<String> {
    let mut origins = vec![
        "http://tauri.localhost".to_owned(),
        "https://tauri.localhost".to_owned(),
        "tauri://localhost".to_owned(),
    ];
    if let Some(url) = &app.config().build.dev_url {
        origins.push(url.origin().ascii_serialization());
    }
    origins
}

fn single_query_value(url: &Url, key: &str) -> Result<String, ()> {
    let values = url
        .query_pairs()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
        .collect::<Vec<_>>();
    match values.as_slice() {
        [value] if !value.is_empty() => Ok(value.clone()),
        _ => Err(()),
    }
}

#[derive(Debug)]
struct MediaResponse {
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
    content_length: u64,
    content_range: Option<String>,
}

#[derive(Debug)]
enum MediaError {
    Unauthorized,
    InvalidRange { size: u64 },
    TooLarge,
    NotFound,
    Read,
}

/// 统一执行媒体租约校验和有界读取，避免协议入口出现绕过授权的分支。
fn read_authorized_media_response(
    preview_id: &str,
    requested_path: &std::path::Path,
    head_only: bool,
    range_header: Option<&str>,
) -> Result<MediaResponse, MediaError> {
    let (authorized_path, media_type) = authorize_media_preview_path(preview_id, requested_path)
        .map_err(|_| MediaError::Unauthorized)?;
    read_media_response(&authorized_path, media_type, head_only, range_header)
}

fn read_media_response(
    path: &std::path::Path,
    media_type: &'static str,
    head_only: bool,
    range_header: Option<&str>,
) -> Result<MediaResponse, MediaError> {
    let mut file = File::open(path).map_err(|_| MediaError::NotFound)?;
    let metadata = file.metadata().map_err(|_| MediaError::Read)?;
    if !metadata.is_file() {
        return Err(MediaError::NotFound);
    }
    let size = metadata.len();
    let range = match keencode_web::parse_single_range(range_header, size) {
        Ok(range) => range,
        Err(_) => return Err(MediaError::InvalidRange { size }),
    };
    let (start, end, status) = match range {
        Some(range) => (range.start, range.end, 206),
        None if size == 0 => (0, 0, 200),
        None => (0, size - 1, 200),
    };
    let content_length = if size == 0 { 0 } else { end - start + 1 };
    if !head_only && content_length > MAX_RESPONSE_BYTES {
        return Err(MediaError::TooLarge);
    }

    let body = if head_only || content_length == 0 {
        Vec::new()
    } else {
        file.seek(SeekFrom::Start(start))
            .map_err(|_| MediaError::Read)?;
        let length = usize::try_from(content_length).map_err(|_| MediaError::TooLarge)?;
        let mut body = vec![0u8; length];
        file.read_exact(&mut body).map_err(|_| MediaError::Read)?;
        body
    };
    Ok(MediaResponse {
        status,
        body,
        content_type: media_type,
        content_length,
        content_range: range.map(|_| format!("bytes {start}-{end}/{size}")),
    })
}

fn media_response(
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
    content_length: u64,
    content_range: Option<String>,
    origin: Option<&str>,
) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("Content-Length", content_length.to_string())
        .header("Accept-Ranges", "bytes")
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header(
            "Content-Security-Policy",
            "sandbox; default-src 'none'; media-src 'self' blob: data:",
        );
    if let Some(origin) = origin {
        builder = builder
            .header("Access-Control-Allow-Origin", origin)
            .header("Vary", "Origin");
    }
    if let Some(content_range) = content_range {
        builder = builder.header("Content-Range", content_range);
    }
    builder.body(body).expect("固定媒体响应头有效")
}

fn error_response(status: u16, origin: Option<&str>, message: &str) -> Response<Vec<u8>> {
    let body = message.as_bytes().to_vec();
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Length", body.len().to_string())
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff");
    if let Some(origin) = origin {
        builder = builder
            .header("Access-Control-Allow-Origin", origin)
            .header("Vary", "Origin");
    }
    builder.body(body).expect("固定媒体错误响应头有效")
}

fn range_error_response(origin: Option<&str>, size: u64) -> Response<Vec<u8>> {
    let body = "媒体 range 不可满足".as_bytes().to_vec();
    let mut builder = Response::builder()
        .status(416)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Length", body.len().to_string())
        .header("Content-Range", format!("bytes */{size}"))
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff");
    if let Some(origin) = origin {
        builder = builder
            .header("Access-Control-Allow-Origin", origin)
            .header("Vary", "Origin");
    }
    builder.body(body).expect("固定 range 错误响应头有效")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_must_be_unique_and_non_empty() {
        let url = Url::parse("zcode-media://local/preview?path=/tmp/a&previewId=abc").unwrap();
        assert_eq!(single_query_value(&url, "previewId").unwrap(), "abc");
        assert!(single_query_value(&url, "missing").is_err());
        let duplicate =
            Url::parse("zcode-media://local/preview?previewId=abc&previewId=def").unwrap();
        assert!(single_query_value(&duplicate, "previewId").is_err());
    }

    #[test]
    fn accepts_native_scheme_and_windows_localhost_alias_only() {
        assert!(is_media_url(
            &Url::parse("zcode-media://local/preview?path=a&previewId=b").unwrap()
        ));
        assert!(is_media_url(
            &Url::parse("http://zcode-media.localhost/preview?path=a&previewId=b").unwrap()
        ));
        assert!(!is_media_url(
            &Url::parse("https://zcode-media.localhost/preview?path=a&previewId=b").unwrap()
        ));
        assert!(!is_media_url(
            &Url::parse("http://other.localhost/preview?path=a&previewId=b").unwrap()
        ));
        assert!(!is_media_url(
            &Url::parse("zcode-media://local/other?path=a&previewId=b").unwrap()
        ));
    }

    #[test]
    fn head_response_has_length_without_body() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let response = read_media_response(&path, "video/mp4", true, None).unwrap();
        assert_eq!(response.status, 200);
        assert!(response.body.is_empty());
        assert_eq!(response.content_length, 10);
        assert!(response.content_range.is_none());
    }

    #[test]
    fn authorized_media_range_and_head_use_the_registered_lease() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        let preview_id = "media-protocol-test-range".to_owned();
        crate::frontend_rpc::services::insert_media_preview_for_test(
            preview_id.clone(),
            canonical.clone(),
            "video/mp4",
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );

        let range =
            read_authorized_media_response(&preview_id, &canonical, false, Some("bytes=2-5"))
                .expect("有效租约应允许读取 range");
        assert_eq!(range.status, 206);
        assert_eq!(range.body, b"2345");
        assert_eq!(range.content_length, 4);

        let head = read_authorized_media_response(&preview_id, &canonical, true, None)
            .expect("有效租约应允许 HEAD");
        assert_eq!(head.status, 200);
        assert!(head.body.is_empty());
        assert_eq!(head.content_length, 10);

        crate::frontend_rpc::services::remove_media_preview_for_test(&preview_id);
    }

    #[test]
    fn expired_media_lease_is_rejected_before_file_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        let preview_id = "media-protocol-test-expired".to_owned();
        crate::frontend_rpc::services::insert_media_preview_for_test(
            preview_id.clone(),
            canonical.clone(),
            "video/mp4",
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );

        let error = read_authorized_media_response(&preview_id, &canonical, false, None)
            .expect_err("过期租约必须在读取前拒绝");
        assert!(matches!(error, MediaError::Unauthorized));
        crate::frontend_rpc::services::remove_media_preview_for_test(&preview_id);
    }

    #[test]
    fn invalid_range_returns_file_size_for_416() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let error = read_media_response(&path, "video/mp4", false, Some("bytes=99-100"))
            .expect_err("越界 range 必须失败");
        assert!(matches!(error, MediaError::InvalidRange { size: 10 }));
    }

    #[test]
    fn range_response_reads_requested_window() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let response = read_media_response(&path, "video/mp4", false, Some("bytes=2-5")).unwrap();
        assert_eq!(response.status, 206);
        assert_eq!(response.body, b"2345");
        assert_eq!(response.content_length, 4);
        assert_eq!(response.content_range.as_deref(), Some("bytes 2-5/10"));
    }
}
