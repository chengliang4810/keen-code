//! 原页面附件的本地资产存储；消息正文仍只由 ACP Journal 持有。
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;
pub(crate) const V4_MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentAsset {
    pub id: String,
    pub thread_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: usize,
    pub path: String,
}

#[derive(Clone, Copy)]
struct StageLimits {
    max_file_bytes: usize,
    max_image_bytes: usize,
}

fn directory(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|e| e.to_string())?
        .join("ui-attachments"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn image_extension(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// 不采用客户端路径；文件位置完全由宿主签发的 ID 和 MIME 决定。
fn content_path(root: &Path, asset: &AttachmentAsset) -> Result<PathBuf, String> {
    if !valid_id(&asset.id) {
        return Err("附件标识无效".into());
    }
    let extension = if asset.kind == "image" {
        image_extension(&asset.mime_type).ok_or("附件图片格式不受支持")?
    } else if asset.kind == "file" {
        // 保留安全扩展名供既有 Read 工具识别文本/媒体；文件名永不作为目录使用。
        Path::new(&asset.name)
            .extension()
            .and_then(|v| v.to_str())
            .filter(|v| {
                !v.is_empty() && v.len() <= 16 && v.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .unwrap_or("bin")
    } else {
        return Err("附件类型无效".into());
    };
    Ok(root.join(&asset.id).join(format!("content.{extension}")))
}

fn read_asset(root: &Path, id: &str) -> Result<AttachmentAsset, String> {
    if !valid_id(id) {
        return Err("附件标识无效".into());
    }
    let canonical_root = fs::canonicalize(root).map_err(|_| "附件目录不存在")?;
    let canonical_folder = fs::canonicalize(root.join(id)).map_err(|_| "附件目录不存在")?;
    if canonical_folder != canonical_root.join(id) {
        return Err("附件目录不能是重定向链接".into());
    }
    let bytes = crate::storage::read_private_bytes_bounded(
        &root.join(id).join("asset.json"),
        4096,
        "附件元数据",
    )
    .map_err(|e| e.to_string())?
    .ok_or("附件不存在")?;
    let mut asset: AttachmentAsset =
        serde_json::from_slice(&bytes).map_err(|_| "附件元数据无效")?;
    if asset.id != id {
        return Err("附件标识不一致".into());
    }
    let expected = content_path(root, &asset)?;
    let canonical_file = fs::canonicalize(&expected).map_err(|_| "附件内容不存在")?;
    // 拒绝资产子目录中的链接把读写引向存储根目录之外。
    if !canonical_file.starts_with(&canonical_root) || !canonical_file.is_file() {
        return Err("附件路径越过存储边界".into());
    }
    let size = fs::metadata(&canonical_file)
        .map_err(|_| "附件内容不可读")?
        .len();
    if size != asset.size_bytes as u64 || size > MAX_FILE_BYTES as u64 {
        return Err("附件大小不一致".into());
    }
    asset.path = crate::path_utils::path_to_frontend(&canonical_file);
    Ok(asset)
}

fn stage_with_limits(
    root: &Path,
    thread_id: String,
    kind: String,
    name: String,
    mime_type: String,
    bytes: &[u8],
    limits: StageLimits,
) -> Result<AttachmentAsset, String> {
    if thread_id.is_empty()
        || thread_id.len() > 256
        || thread_id.chars().any(char::is_control)
        || name.trim().is_empty()
        || name.len() > 255
        || name.chars().any(char::is_control)
        || mime_type.is_empty()
        || mime_type.len() > 100
        || mime_type.chars().any(char::is_control)
    {
        return Err("附件参数无效".into());
    }
    let limit = if kind == "image" {
        limits.max_image_bytes
    } else {
        limits.max_file_bytes
    };
    if bytes.len() > limit {
        return Err("附件超过大小限制".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时间无效")?
        .as_nanos();
    let mut asset = AttachmentAsset {
        id: format!("{stamp:x}-{:x}", SEQUENCE.fetch_add(1, Ordering::Relaxed)),
        thread_id,
        kind,
        name,
        mime_type,
        size_bytes: bytes.len(),
        path: String::new(),
    };
    let path = content_path(root, &asset)?;
    let folder = path.parent().ok_or("附件目录无效")?;
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    fs::create_dir(folder).map_err(|e| e.to_string())?;
    asset.path = crate::path_utils::path_to_frontend(&path);
    let result = (|| {
        crate::storage::atomic_write_private(&path, bytes).map_err(|e| e.to_string())?;
        let metadata = serde_json::to_vec(&asset).map_err(|e| e.to_string())?;
        crate::storage::atomic_write_private(&folder.join("asset.json"), &metadata)
            .map_err(|e| e.to_string())?;
        read_asset(root, &asset.id)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(folder.join("asset.json"));
        let _ = fs::remove_dir(folder);
    }
    result
}

fn stage(
    root: &Path,
    thread_id: String,
    kind: String,
    name: String,
    mime_type: String,
    bytes: &[u8],
) -> Result<AttachmentAsset, String> {
    stage_with_limits(
        root,
        thread_id,
        kind,
        name,
        mime_type,
        bytes,
        StageLimits {
            max_file_bytes: MAX_FILE_BYTES,
            max_image_bytes: 10 * 1024 * 1024,
        },
    )
}

/// V4 上传使用协议统一的 20 MiB 限制；仍复用同一份原子资产写入与路径校验。
pub(crate) fn stage_v4(
    root: &Path,
    thread_id: String,
    kind: String,
    name: String,
    mime_type: String,
    bytes: &[u8],
) -> Result<AttachmentAsset, String> {
    stage_with_limits(
        root,
        thread_id,
        kind,
        name,
        mime_type,
        bytes,
        StageLimits {
            max_file_bytes: V4_MAX_FILE_BYTES,
            max_image_bytes: V4_MAX_FILE_BYTES,
        },
    )
}

/// V4 读取复用既有 Session 归属证明，避免新 RPC 复制一套附件授权逻辑。
pub(crate) fn resolve_v4(
    app: &AppHandle,
    id: &str,
    session_id: &str,
) -> Result<AttachmentAsset, String> {
    let root = directory(app)?;
    let asset = read_asset(&root, id)?;
    if asset.thread_id == session_id {
        return Ok(asset);
    }
    let runtime = app
        .try_state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>()
        .ok_or("AgentRuntime 未初始化")?;
    let session = crate::session_commands::open_authorized_session(&runtime, app, session_id)?;
    let snapshot = session.snapshot().map_err(|error| error.to_string())?;
    for message in snapshot.state.raw_transcript_messages() {
        if message.role != keencode_resources::MessageRole::User || message.is_meta {
            continue;
        }
        let materialized = session
            .materialize_message(message)
            .map_err(|error| error.to_string())?;
        if message
            .references
            .iter()
            .any(|reference| reference.path == asset.path)
            || materialized.content.iter().any(|part| {
                matches!(
                    part,
                    keencode_model::ContentBlock::Text { text }
                        if text_references_asset(text, &asset.path)
                )
            })
        {
            return Ok(asset);
        }
    }
    Err("附件不属于此会话且未被其历史引用".into())
}

#[tauri::command]
pub fn ui_attachment_stage(
    app: AppHandle,
    thread_id: String,
    kind: String,
    name: String,
    mime_type: String,
    data: String,
) -> Result<AttachmentAsset, String> {
    // 在解码前限制 IPC 的 base64 长度，避免先分配任意大的二进制缓冲。
    if data.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
        return Err("附件超过大小限制".into());
    }
    let bytes = STANDARD.decode(data).map_err(|_| "附件编码无效")?;
    let asset = stage(&directory(&app)?, thread_id, kind, name, mime_type, &bytes)?;
    app.asset_protocol_scope()
        .allow_file(&asset.path)
        .map_err(|e| e.to_string())?;
    Ok(asset)
}

#[tauri::command]
pub fn ui_attachment_list(app: AppHandle) -> Result<Vec<AttachmentAsset>, String> {
    let root = directory(&app)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut assets = Vec::new();
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())? {
        if assets.len() >= 4096 {
            return Err("附件索引超过大小限制".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if let Ok(asset) = read_asset(&root, &id) {
            app.asset_protocol_scope()
                .allow_file(&asset.path)
                .map_err(|e| e.to_string())?;
            assets.push(asset);
        }
    }
    Ok(assets)
}

#[tauri::command]
pub async fn ui_attachment_resolve(
    app: AppHandle,
    id: String,
    thread_id: String,
    session_id: Option<String>,
) -> Result<AttachmentAsset, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let asset = read_asset(&directory(&app)?, &id)?;
        if asset.thread_id != thread_id {
            // 分叉及编辑前归档保留资产原路径；继承权限仅由目标 Session 的权威用户消息证明。
            let session_id = session_id.ok_or("附件不属于此会话")?;
            let runtime = app.try_state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>()
                .ok_or("AgentRuntime 未初始化")?;
            let session = crate::session_commands::open_authorized_session(&runtime, &app, &session_id)?;
            let snapshot = session.snapshot().map_err(|error| error.to_string())?;
            let mut referenced = false;
            for message in snapshot.state.raw_transcript_messages() {
                if message.role != keencode_resources::MessageRole::User || message.is_meta {
                    continue;
                }
                let materialized = session.materialize_message(message).map_err(|error| error.to_string())?;
                if materialized.content.iter().any(|part| matches!(part,
                    keencode_model::ContentBlock::Text { text } if text_references_asset(text, &asset.path))) {
                    referenced = true;
                    break;
                }
            }
            if !referenced {
                return Err("附件不属于此会话且未被其历史引用".into());
            }
        }
        Ok(asset)
    }).await.map_err(|error| error.to_string())?
}

/// 只接受应用生成的独占路径行；正文提及、转义路径或前缀相似的路径均不能授权资产。
fn text_references_asset(text: &str, path: &str) -> bool {
    let expected = path.replace('\\', "/");
    text.lines().any(|line| {
        let line = line.trim();
        let reference = line
            .strip_prefix("@image ")
            .or_else(|| line.strip_prefix('@'));
        reference.is_some_and(|value| value.trim().replace('\\', "/") == expected)
    })
}

#[tauri::command]
pub fn ui_attachment_cancel(app: AppHandle, id: String) -> Result<(), String> {
    let root = directory(&app)?;
    if !valid_id(&id) {
        return Err("附件标识无效".into());
    }
    if !root.join(&id).exists() {
        return Ok(());
    }
    let asset = read_asset(&root, &id)?;
    let path = content_path(&root, &asset)?;
    fs::remove_file(path).map_err(|e| e.to_string())?;
    fs::remove_file(root.join(&id).join("asset.json")).map_err(|e| e.to_string())?;
    fs::remove_dir(root.join(&id)).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inherited_attachment_requires_exact_unescaped_history_reference() {
        let path = "D:/assets/file.txt";
        assert!(text_references_asset("prompt\n\n@D:/assets/file.txt", path));
        assert!(text_references_asset("@image D:\\assets\\file.txt", path));
        for text in [
            "正文 @D:/assets/file.txt",
            "\\@D:/assets/file.txt",
            "@D:/assets/file.txt.other",
            "@D:/other/file.txt",
        ] {
            assert!(!text_references_asset(text, path));
        }
    }
    #[test]
    fn assets_keep_original_metadata_but_never_use_client_filename_as_path() {
        let temp = tempfile::tempdir().unwrap();
        let asset = stage(
            temp.path(),
            "thread-a".into(),
            "file".into(),
            "../原文件.txt".into(),
            "text/plain".into(),
            b"fixture",
        )
        .unwrap();
        let restored = read_asset(temp.path(), &asset.id).unwrap();
        assert_eq!(restored.name, "../原文件.txt");
        assert_eq!(restored.size_bytes, 7);
        assert!(
            fs::canonicalize(&restored.path)
                .unwrap()
                .starts_with(fs::canonicalize(temp.path()).unwrap())
        );
        assert_eq!(fs::read(restored.path).unwrap(), b"fixture");
        assert!(read_asset(temp.path(), "../escape").is_err());
    }
    #[test]
    fn rejects_invalid_images_and_detects_modified_asset_content() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            stage(
                temp.path(),
                "t".into(),
                "image".into(),
                "x.svg".into(),
                "image/svg+xml".into(),
                b"<svg/>"
            )
            .is_err()
        );
        let asset = stage(
            temp.path(),
            "t".into(),
            "file".into(),
            "x".into(),
            "text/plain".into(),
            b"old",
        )
        .unwrap();
        fs::write(&asset.path, b"changed").unwrap();
        assert!(read_asset(temp.path(), &asset.id).is_err());
    }
}
