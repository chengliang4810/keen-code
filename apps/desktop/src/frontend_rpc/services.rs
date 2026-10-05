//! ZCode service-channel 适配层。
//!
//! 这里保留 ZCode 的 channel/method 名称，但所有副作用都落到 KeenCode
//! 已有的路径授权、配置原子写入、Git 与 PTY 边界。没有对应 Rust 能力的
//! 操作必须返回显式错误，不能用空数组或成功值掩盖缺口。

use super::dispatch::{EventCallback, GatewayContext, Handler, RpcError, RpcFuture, Subscription};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    future::Future,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio_util::sync::CancellationToken;

pub(crate) mod automations;
pub(crate) mod onboarding;

const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SEARCH_ENTRIES: usize = 20_000;
const BROADCAST_EVENT: &str = "keencode://service-broadcast";
const MEDIA_PREVIEW_TTL: Duration = Duration::from_secs(120);
const MAX_MEDIA_PREVIEWS: usize = 512;

/// 媒体 URL 的生命周期记录。记录只保存规范化路径，不把二进制内容常驻在服务状态里；
/// 刷新时重新校验文件仍指向同一普通文件，原生 `zcode-media` 协议按 previewId 读取。
#[derive(Clone, Debug)]
struct MediaPreviewLease {
    path: PathBuf,
    media_type: &'static str,
    expires_at: Instant,
}

static MEDIA_PREVIEWS: OnceLock<Mutex<HashMap<String, MediaPreviewLease>>> = OnceLock::new();
static PLUGIN_OPERATIONS: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();

/// 供根装配层直接注册到 `RpcGateway` 的 handler。
#[derive(Default)]
pub struct ServiceHandler;

impl Handler for ServiceHandler {
    fn call(&self, ctx: GatewayContext, channel: String, method: String, args: Value) -> RpcFuture {
        Box::pin(async move {
            call_service_for_connection(
                ctx.app,
                Some(ctx.connection_id.as_str()),
                &channel,
                &method,
                args,
            )
            .await
            .map_err(|error| service_rpc_error(&channel, &method, error))
        })
    }

    fn listen(
        &self,
        ctx: GatewayContext,
        channel: String,
        event: String,
        args: Value,
        callback: EventCallback,
    ) -> Result<Subscription, RpcError> {
        listen_service(&ctx, &channel, &event, args, callback)
            .map_err(|error| service_rpc_error(&channel, &event, error))
    }

    fn connection_closed(&self, ctx: GatewayContext) {
        automations::connection_closed(&ctx.connection_id);
        super::workspace_search::connection_closed(&ctx.connection_id);
    }
}

async fn call_service_for_connection(
    app: AppHandle,
    connection_id: Option<&str>,
    channel: &str,
    method: &str,
    args: Value,
) -> Result<Value, String> {
    let args = normalize_service_args(channel, method, args)?;
    match channel {
        "file" if method == "searchWorkspaceFiles" => {
            let root = registered_root(&app, &required_string(&args, "rootPath")?)?;
            let owner = connection_id.unwrap_or("native-local").to_owned();
            // 目录扫描和排序不能占用 Runtime 的异步执行线程。
            tauri::async_runtime::spawn_blocking(move || {
                super::workspace_search::search(root, owner, args)
            })
            .await
            .map_err(|error| format!("搜索任务失败：{error}"))?
        }
        "file" => file_call(&app, method, args).await,
        "media-preview" => media_preview_call(&app, method, args).await,
        "system" => system_call(&app, method, args),
        "git" => git_call(&app, method, args).await,
        "git-checkpoint" => checkpoint_call(&app, method, args).await,
        "setting" => setting_call(&app, method, args),
        "credential" => credential_call(&app, method, args),
        "broadcast" => broadcast_call(&app, method, args),
        "usage-stats" => usage_call(&app, method, args),
        "skills" => skills_call(&app, method, args),
        "plugins" => plugins_call(&app, method, args).await,
        "plugin-management" => plugin_management_call(&app, method, args).await,
        "commands" => commands_call(&app, method, args),
        "hooks" => hooks_call(&app, method, args),
        "memory" => memory_call(&app, method, args),
        "onboarding-record" => onboarding::call(&app, method, args),
        "mcp-sync" => mcp_sync_call(&app, method, args).await,
        "file-watcher" => file_watcher_call(&app, method, args, connection_id),
        "subagents" => subagents_call(&app, method, args).await,
        "client-config" => client_config_call(&app, method, args),
        // 本地桌面没有 SSH/WSL/Docker 等跨主机暂存后端。Composer 的本地附件
        // 使用 localPath 零拷贝或既有 V4 attachmentPut；不能把本机路径当成远端
        // 可读的 staged 引用返回成功。
        "prompt-attachment-transfer" => prompt_attachment_transfer_unsupported(method),
        "skill-sync" | "plugin-sync" => sync_call(channel, method),
        "zcode-agent" if automations::is_automation_method(method) => {
            automations::call_automation(&app, method, args, connection_id).await
        }
        // 会话与 Agent 生命周期由独立适配层拥有；这里拒绝以免创建第二套事实源。
        "zcode-task" | "zcode-agent" | "zcode-session" => unsupported(
            channel,
            method,
            "session/agent service is owned by the session adapter",
        ),
        // 用户明确排除官方账号、支付、云机器人和远程执行能力。
        "oauth" | "coding-plan-subscription" | "bots" | "off-peak-task" => unsupported(
            channel,
            method,
            "official account, billing, cloud-bot, and off-peak services are disabled",
        ),
        "window-controller"
        | "conversation-share"
        | "client-scenes"
        | "settings-sync"
        | "provider-provisioning-target"
        | "feedback" => unsupported(
            channel,
            method,
            "service is not implemented in the local desktop host",
        ),
        _ => Err(format!("未知服务 channel：{channel}")),
    }
}

/// 真实事件订阅入口。Tauri 事件监听器在 dispose 时解除，不创建空订阅。
pub fn listen_service(
    ctx: &GatewayContext,
    channel: &str,
    event: &str,
    args: Value,
    callback: EventCallback,
) -> Result<Subscription, String> {
    let args = normalize_listen_args(channel, event, args)?;
    let (event_name, filter_id) = match (channel, event) {
        ("broadcast", "onMessage") => (BROADCAST_EVENT.to_owned(), None),
        ("file-watcher", "onDynamicChange") => {
            let id = required_string(&args, "id")?;
            (format!("keencode://file-watcher/{id}"), Some(id))
        }
        ("prompt-attachment-transfer", "onDynamicProgress") => {
            return Err(prompt_attachment_transfer_unsupported_error(event));
        }
        ("plugin-management", "onDynamicPluginOperationProgress") => {
            let id = required_string(&args, "operationId")?;
            (format!("keencode://plugin-management/{id}"), Some(id))
        }
        _ => return Err(format!("未知或未接通事件：{channel}.{event}")),
    };
    if channel == "file-watcher" {
        let id = filter_id
            .as_deref()
            .ok_or_else(|| "file-watcher 订阅缺少 id".to_owned())?;
        return file_watcher_listener_guard(
            ctx.app.clone(),
            id,
            Some(ctx.connection_id.as_str()),
            callback,
        );
    }
    Ok(service_event_subscription(
        ctx.app.clone(),
        &event_name,
        filter_id,
        callback,
    ))
}

fn service_event_subscription(
    app: AppHandle,
    event_name: &str,
    id: Option<String>,
    callback: EventCallback,
) -> Subscription {
    let listener_id = app.listen(event_name, move |event| {
        let payload = serde_json::from_str::<Value>(event.payload())
            .unwrap_or(Value::String(event.payload().to_owned()));
        if let Some(expected) = id.as_deref() {
            let event_id = payload
                .get("id")
                .or_else(|| payload.get("operationId"))
                .and_then(Value::as_str);
            if event_id.is_some() && event_id != Some(expected) {
                return;
            }
        }
        let _ = callback(payload);
    });
    Subscription::new(move || app.unlisten(listener_id))
}

fn service_rpc_error(channel: &str, method: &str, error: String) -> RpcError {
    let code = if error.starts_with("未知服务 channel") {
        "rpc.unknownChannel"
    } else if error.starts_with("未知服务方法") || error.starts_with("未知或未接通事件")
    {
        "rpc.unknownMethod"
    } else if error.starts_with("不支持") || error.starts_with("service is not implemented") {
        "rpc.unsupportedService"
    } else {
        "service.error"
    };
    RpcError::new(code, error).with_data(json!({"channel": channel, "method": method}))
}

fn unsupported(channel: &str, method: &str, reason: &str) -> Result<Value, String> {
    Err(format!("不支持 {channel}.{method}：{reason}"))
}

fn unsupported_method(channel: &str, method: &str) -> Result<Value, String> {
    Err(format!("未知服务方法：{channel}.{method}"))
}

fn object(args: &Value) -> Result<&Map<String, Value>, String> {
    args.as_object()
        .ok_or_else(|| "服务参数必须是对象".to_owned())
}

fn positional_object(values: &[Value], keys: &[&str]) -> Result<Value, String> {
    if values.len() > keys.len() {
        return Err(format!(
            "服务参数数量无效：收到 {} 个，最多 {} 个",
            values.len(),
            keys.len()
        ));
    }
    let mut result = Map::new();
    for (index, value) in values.iter().enumerate() {
        if !value.is_null() {
            result.insert(keys[index].to_owned(), value.clone());
        }
    }
    Ok(Value::Object(result))
}

/// `ProxyChannel.toService` 的对象参数也会经过参数数组编码。这里保留 source
/// interface 的位置签名，再把数组转换为现有 Rust handler 使用的命名对象。
fn normalize_service_args(channel: &str, method: &str, args: Value) -> Result<Value, String> {
    let Some(values) = args.as_array() else {
        return Ok(args);
    };
    if values.len() == 1 && values[0].is_object() {
        return Ok(values[0].clone());
    }
    if values.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    match (channel, method) {
        ("setting", "update") => Ok(values.first().cloned().unwrap_or(Value::Null)),
        ("setting", "ensureDefaultProject") => positional_object(values, &["homedir"]),
        ("broadcast", "send") => positional_object(values, &["message"]),
        ("broadcast", "acquireClaim" | "tryClaim") => positional_object(values, &["key"]),
        ("broadcast", "commitClaim" | "releaseClaim") => Ok(values[0].clone()),
        ("broadcast", "onMessage") => Ok(Value::Object(Map::new())),
        ("prompt-attachment-transfer", "adopt" | "cancel" | "cleanup") => {
            positional_object(values, &["operationId"])
        }
        ("onboarding-record", "appendRecord") => positional_object(values, &["deviceMid", "entry"]),
        ("onboarding-record", "shouldOnboard" | "dismissOnboarding") => {
            positional_object(values, &["deviceMid"])
        }
        ("onboarding-record", "updateRecordPreferences") => positional_object(values, &["patch"]),
        _ if values.len() == 1 => Ok(values[0].clone()),
        _ => Err(format!("未定义的服务位置参数：{channel}.{method}")),
    }
}

fn normalize_listen_args(channel: &str, event: &str, args: Value) -> Result<Value, String> {
    if args.is_object() {
        return Ok(args);
    }
    if let Some(values) = args.as_array() {
        if values.len() == 1 && values[0].is_object() {
            return Ok(values[0].clone());
        }
        if values.len() == 1 {
            return normalize_listen_args(channel, event, values[0].clone());
        }
    }
    match (channel, event) {
        ("file-watcher", "onDynamicChange") => positional_object(&[args], &["id"]),
        ("prompt-attachment-transfer", "onDynamicProgress") => {
            positional_object(&[args], &["operationId"])
        }
        ("plugin-management", "onDynamicPluginOperationProgress") => {
            positional_object(&[args], &["operationId"])
        }
        _ => Ok(Value::Object(Map::new())),
    }
}

fn object_string(args: &Value, name: &str) -> Result<Option<String>, String> {
    let Some(value) = object(args)?.get(name) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| format!("参数 {name} 必须是字符串"))?;
    if value.is_empty() {
        return Err(format!("参数 {name} 不能为空"));
    }
    Ok(Some(value.to_owned()))
}

fn required_string(args: &Value, name: &str) -> Result<String, String> {
    object_string(args, name)?.ok_or_else(|| format!("缺少参数 {name}"))
}

fn optional_string(args: &Value, name: &str) -> Result<Option<String>, String> {
    object_string(args, name)
}

fn optional_bool(args: &Value, name: &str) -> Result<Option<bool>, String> {
    object(args)?
        .get(name)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| format!("参数 {name} 必须是布尔值"))
        })
        .transpose()
}

fn optional_u64(args: &Value, name: &str) -> Result<Option<u64>, String> {
    object(args)?
        .get(name)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("参数 {name} 必须是非负整数"))
        })
        .transpose()
}

fn required_array_strings(args: &Value, name: &str) -> Result<Vec<String>, String> {
    let values = object(args)?
        .get(name)
        .ok_or_else(|| format!("缺少参数 {name}"))?
        .as_array()
        .ok_or_else(|| format!("参数 {name} 必须是数组"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("参数 {name} 只能包含非空字符串"))
        })
        .collect()
}

fn optional_array_strings(args: &Value, name: &str) -> Result<Option<Vec<String>>, String> {
    let Some(value) = object(args)?.get(name) else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("参数 {name} 必须是数组"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("参数 {name} 只能包含非空字符串"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn to_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| format!("服务结果序列化失败：{error}"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default()
}

fn registered_root(app: &AppHandle, root_path: &str) -> Result<PathBuf, String> {
    crate::workspace::registered_project_root(app, root_path)
}

/// 将文件或目录路径归属到最近的已登记项目根，并返回项目根与规范化目标。
/// `registered_project_root` 要求参数必须是项目根；前端文件浏览器却会把
/// 子目录和文件的绝对路径直接传入，因此这里逐级向上查找后再做一次越界校验。
fn registered_path(
    app: &AppHandle,
    raw: &str,
    allow_missing: bool,
) -> Result<(PathBuf, PathBuf), String> {
    if raw.trim().is_empty() || raw.chars().any(char::is_control) {
        return Err("文件路径不能为空或包含控制字符".to_owned());
    }
    let input = PathBuf::from(raw);
    let mut current = if input.exists() {
        if input.is_dir() {
            input.clone()
        } else {
            input
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("文件路径没有父目录：{raw}"))?
        }
    } else {
        let mut parent = input
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("文件路径没有父目录：{raw}"))?;
        while !parent.exists() {
            let Some(next) = parent.parent() else {
                break;
            };
            if next == parent {
                break;
            }
            parent = next.to_path_buf();
        }
        parent
    };
    loop {
        if current.is_dir()
            && let Ok(root) = registered_root(app, &current.to_string_lossy())
        {
            let path = resolve_project_path(&root, raw, allow_missing)?;
            return Ok((root, path));
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    Err(format!("项目尚未添加：{raw}"))
}

fn path_within(root: &Path, path: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// 所有文件服务路径都必须先归属到已登记项目；不存在的写入路径只允许
/// 在已登记根下解析父目录，禁止通过 `..` 或符号链接越界。
fn resolve_project_path(root: &Path, raw: &str, allow_missing: bool) -> Result<PathBuf, String> {
    if raw.trim().is_empty() || raw.chars().any(char::is_control) {
        return Err("文件路径不能为空或包含控制字符".to_owned());
    }
    let input = Path::new(raw);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    if candidate.exists() {
        let canonical = fs::canonicalize(&candidate)
            .map_err(|error| format!("无法解析文件路径 {}：{error}", candidate.display()))?;
        if !path_within(root, &canonical) {
            return Err(format!("文件路径超出项目目录：{}", candidate.display()));
        }
        return Ok(canonical);
    }
    if !allow_missing {
        return Err(format!("文件不存在：{}", candidate.display()));
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| format!("文件路径没有父目录：{}", candidate.display()))?;
    let parent = fs::canonicalize(parent)
        .map_err(|error| format!("无法解析文件父目录 {}：{error}", parent.display()))?;
    if !path_within(root, &parent) {
        return Err(format!("文件路径超出项目目录：{}", candidate.display()));
    }
    let name = candidate
        .file_name()
        .ok_or_else(|| "文件路径缺少文件名".to_owned())?;
    Ok(parent.join(name))
}

fn modified_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default()
}

fn media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" | "ts" | "tsx" => "text/javascript",
        "md" | "txt" | "rs" | "toml" | "yaml" | "yml" => "text/plain",
        _ => "application/octet-stream",
    }
}

async fn file_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "readdir" => {
            let path = required_string(&args, "path")?;
            let (_, directory) = registered_path(app, &path, false)?;
            if !directory.is_dir() {
                return Err("readdir 目标不是目录".to_owned());
            }
            let include_hidden = optional_bool(&args, "includeHidden")?.unwrap_or(false);
            let mut entries = Vec::new();
            for entry in
                fs::read_dir(&directory).map_err(|error| format!("无法读取目录：{error}"))?
            {
                let entry = entry.map_err(|error| format!("无法读取目录项：{error}"))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !include_hidden && name.starts_with('.') {
                    continue;
                }
                let child = entry.path();
                let metadata = fs::symlink_metadata(&child)
                    .map_err(|error| format!("无法读取目录项元数据：{error}"))?;
                if metadata.file_type().is_symlink() {
                    continue;
                }
                entries.push(json!({
                    "name": name,
                    "path": crate::path_utils::path_to_frontend(&child),
                    "type": if metadata.is_dir() { "directory" } else { "file" },
                    "isSymbolicLink": false,
                }));
            }
            entries.sort_by(|left, right| {
                left["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .cmp(
                        &right["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_ascii_lowercase(),
                    )
            });
            Ok(Value::Array(entries))
        }
        "stat" => {
            let path = required_string(&args, "path")?;
            let (_, path) = registered_path(app, &path, false)?;
            let metadata =
                fs::metadata(&path).map_err(|error| format!("无法读取文件状态：{error}"))?;
            Ok(json!({
                "path": crate::path_utils::path_to_frontend(&path),
                "type": if metadata.is_dir() { "directory" } else { "file" },
                "size": metadata.is_file().then_some(metadata.len()),
                "mtimeMs": modified_ms(&metadata),
            }))
        }
        "checkFilesExist" => {
            let paths = required_array_strings(&args, "paths")?;
            if paths.len() > 4096 {
                return Err("文件存在性查询超过 4096 项".to_owned());
            }
            let mut result = Vec::with_capacity(paths.len());
            for path in paths {
                let exists = registered_path(app, &path, false).is_ok();
                result.push(json!({"path": path, "exists": exists}));
            }
            Ok(Value::Array(result))
        }
        "resolvePath" => {
            let path = required_string(&args, "path")?;
            let (_, path) = registered_path(app, &path, false)?;
            Ok(Value::String(crate::path_utils::path_to_frontend(&path)))
        }
        "ensureConversationWorkspace" | "createDefaultWorkspace" | "createScratchWorkspace" => {
            let data_root = crate::storage::root_dir(app)
                .map_err(|error| format!("无法确定应用数据目录：{error}"))?;
            let workspaces = data_root.join("chat-workspaces");
            fs::create_dir_all(&workspaces)
                .map_err(|error| format!("无法创建会话工作区：{error}"))?;
            let (name, purpose) = match method {
                "ensureConversationWorkspace" => ("conversation".to_owned(), "conversation"),
                "createDefaultWorkspace" => ("default".to_owned(), "default"),
                _ => {
                    let requested = required_string(&args, "name")?;
                    validate_component(&requested, "工作区名称")?;
                    (format!("scratch-{requested}"), "scratch")
                }
            };
            let path = workspaces.join(name);
            let created = !path.exists();
            fs::create_dir_all(&path).map_err(|error| format!("无法创建工作区：{error}"))?;
            let path = crate::path_utils::path_to_frontend(&path);
            if method == "ensureConversationWorkspace" {
                Ok(json!({"path": path, "created": created, "workspacePurpose": purpose}))
            } else {
                Ok(json!({"path": path}))
            }
        }
        "readTextFile" => {
            let path = required_string(&args, "path")?;
            let (_, path) = registered_path(app, &path, false)?;
            let offset = optional_u64(&args, "offset")?.unwrap_or(0);
            let requested = optional_u64(&args, "length")?.unwrap_or(MAX_FILE_BYTES);
            let (content, bytes_read, total, truncated, is_binary) =
                read_text_slice(&path, offset, requested)?;
            Ok(json!({
                "path": crate::path_utils::path_to_frontend(&path),
                "content": content,
                "offset": offset,
                "bytesRead": bytes_read,
                "totalBytes": total,
                "truncated": truncated,
                "isBinary": is_binary,
            }))
        }
        "readMediaPreview" | "readBinaryPreview" => {
            let path = required_string(&args, "path")?;
            let (_, path) = registered_path(app, &path, false)?;
            let metadata = fs::metadata(&path).map_err(|error| format!("无法读取文件：{error}"))?;
            if !metadata.is_file() {
                return Err("媒体预览目标不是文件".to_owned());
            }
            let limit = optional_u64(&args, "maxBytes")?
                .unwrap_or(MAX_PREVIEW_BYTES)
                .min(MAX_PREVIEW_BYTES);
            let bytes = read_prefix(&path, limit as usize)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            let path = crate::path_utils::path_to_frontend(&path);
            if method == "readMediaPreview" {
                Ok(
                    json!({"path": path, "mediaType": media_type(&PathBuf::from(&path)), "dataBase64": encoded, "totalBytes": metadata.len()}),
                )
            } else {
                Ok(json!({"path": path, "dataBase64": encoded, "totalBytes": metadata.len()}))
            }
        }
        "readFileRange" => {
            let path = required_string(&args, "path")?;
            let (_, path) = registered_path(app, &path, false)?;
            let offset =
                optional_u64(&args, "offset")?.ok_or_else(|| "缺少参数 offset".to_owned())?;
            let length = optional_u64(&args, "length")?
                .ok_or_else(|| "缺少参数 length".to_owned())?
                .min(MAX_PREVIEW_BYTES);
            let bytes = read_range(&path, offset, length as usize)?;
            // codec 将此 marker 还原为顶层 Uint8Array/Buffer，而非 JSON 数组。
            Ok(json!({
                crate::frontend_rpc::codec::RPC_NESTED_UINT8_ARRAY_MARKER: true,
                crate::frontend_rpc::codec::RPC_NESTED_UINT8_ARRAY_BASE64_KEY: base64::engine::general_purpose::STANDARD.encode(bytes),
            }))
        }
        "readWorkspaceFileSearchIgnore"
        | "applyWorkspaceFileSearchIgnoreTransform"
        | "writeWorkspaceFileSearchIgnore" => workspace_ignore_call(app, method, args),
        _ => unsupported_method("file", method),
    }
}

fn validate_component(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || value
            .chars()
            .any(|character| character.is_control() || "/\\:".contains(character))
    {
        return Err(format!("{label}无效"));
    }
    Ok(())
}

fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| format!("无法打开文件：{error}"))?;
    let mut bytes = Vec::new();
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取文件失败：{error}"))?;
    Ok(bytes)
}

fn read_range(path: &Path, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("无法打开文件：{error}"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("文件定位失败：{error}"))?;
    let mut bytes = vec![0; length];
    let count = file
        .read(&mut bytes)
        .map_err(|error| format!("读取文件范围失败：{error}"))?;
    bytes.truncate(count);
    Ok(bytes)
}

fn read_text_slice(
    path: &Path,
    offset: u64,
    requested: u64,
) -> Result<(String, usize, u64, bool, bool), String> {
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取文件状态：{error}"))?;
    if !metadata.is_file() {
        return Err("文本读取目标不是文件".to_owned());
    }
    if metadata.len() > MAX_FILE_BYTES && requested > MAX_FILE_BYTES {
        return Err("文本文件超过 16 MB 读取上限，请使用 readFileRange".to_owned());
    }
    let length = requested.min(MAX_FILE_BYTES) as usize;
    let bytes = read_range(path, offset, length)?;
    let is_binary = bytes.contains(&0);
    // UTF-8 BOM 属于文件头而不是可见正文；只在绝对起点且完整读取到三字节
    // 文件头时剥离展示内容。bytes_read、total 与 truncated 仍按物理字节计算，
    // 非零 offset 保持原始切片语义，避免改变分页边界或 CRLF 内容。
    let visible_bytes = if offset == 0 {
        bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)
    } else {
        &bytes
    };
    let content = String::from_utf8_lossy(visible_bytes).into_owned();
    Ok((
        content,
        bytes.len(),
        metadata.len(),
        offset.saturating_add(bytes.len() as u64) < metadata.len(),
        is_binary,
    ))
}

fn workspace_ignore_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    let root_path = required_string(&args, "rootPath")?;
    let root = registered_root(app, &root_path)?;
    let ignore_path = root.join(".zcodeignore");
    match method {
        "readWorkspaceFileSearchIgnore" => {
            if ignore_path.exists() {
                let content = fs::read_to_string(&ignore_path)
                    .map_err(|error| format!("读取 .zcodeignore 失败：{error}"))?;
                Ok(json!({"content": content, "source": "file"}))
            } else {
                Ok(json!({"content": default_ignore_content(), "source": "template"}))
            }
        }
        "applyWorkspaceFileSearchIgnoreTransform" => {
            let transform = required_string(&args, "transform")?;
            let current = if ignore_path.exists() {
                fs::read_to_string(&ignore_path)
                    .map_err(|error| format!("读取 .zcodeignore 失败：{error}"))?
            } else {
                default_ignore_content()
            };
            let content = match transform.as_str() {
                "reset-defaults" => default_ignore_content(),
                "sync-gitignore" => {
                    let gitignore = root.join(".gitignore");
                    let suffix = fs::read_to_string(gitignore).unwrap_or_default();
                    if suffix.trim().is_empty() {
                        current
                    } else {
                        format!(
                            "{}\n# Synced from .gitignore\n{}",
                            default_ignore_content().trim_end(),
                            suffix
                        )
                    }
                }
                _ => return Err(format!("未知 .zcodeignore 转换：{transform}")),
            };
            Ok(json!({"content": content}))
        }
        "writeWorkspaceFileSearchIgnore" => {
            let content = required_string(&args, "content")?;
            if content.len() > 512 * 1024 || content.contains('\0') {
                return Err(".zcodeignore 内容超过限制或包含 NUL".to_owned());
            }
            crate::storage::atomic_write_private(&ignore_path, content.as_bytes())
                .map_err(|error| format!("写入 .zcodeignore 失败：{error}"))?;
            Ok(Value::Null)
        }
        _ => unsupported_method("file", method),
    }
}

fn default_ignore_content() -> String {
    "# KeenCode defaults\n.git\nnode_modules\ntarget\ndist\n".to_owned()
}

async fn media_preview_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "prepare" => {
            let path = required_string(&args, "path")?;
            let (_, path) = registered_path(app, &path, false)?;
            let metadata =
                fs::metadata(&path).map_err(|error| format!("无法读取媒体文件：{error}"))?;
            if !metadata.is_file() {
                return Err("媒体预览目标不是文件".to_owned());
            }
            let expected = optional_string(&args, "expectedKind")?;
            let kind = match media_type(&path) {
                value if value.starts_with("image/") => "image",
                value if value.starts_with("audio/") => "audio",
                value if value.starts_with("video/") => "video",
                "application/pdf" => "document",
                _ => "binary",
            };
            if let Some(expected) = expected.as_deref()
                && expected != kind
                && expected != "binary"
            {
                return Err(format!("媒体类型不匹配：期望 {expected}，实际 {kind}"));
            }
            let preview_id = new_media_preview_id(&path, metadata.len());
            let lease = MediaPreviewLease {
                path: path.clone(),
                media_type: media_type(&path),
                expires_at: Instant::now() + MEDIA_PREVIEW_TTL,
            };
            let url = store_media_preview(preview_id.clone(), lease)?;
            Ok(json!({
                "kind": "host-range-url",
                "previewId": preview_id,
                "path": crate::path_utils::path_to_frontend(&path),
                "mediaType": media_type(&path),
                "size": metadata.len(),
                "totalBytes": metadata.len(),
                "url": url,
                "urlExpiresAt": now_ms().saturating_add(MEDIA_PREVIEW_TTL.as_millis() as u64),
            }))
        }
        "refreshPlaybackUrl" => {
            let preview_id = required_string(&args, "previewId")?;
            let (path, _media_type) = media_preview_lease(&preview_id)?;
            let (path, _media_type) = authorize_media_preview_path(&preview_id, &path)?;
            let metadata =
                fs::metadata(&path).map_err(|error| format!("无法读取媒体文件：{error}"))?;
            if !metadata.is_file() {
                return Err("媒体预览文件已被删除或替换".to_owned());
            }
            let expires_at = Instant::now() + MEDIA_PREVIEW_TTL;
            update_media_preview_expiry(&preview_id, expires_at)?;
            Ok(json!({
                "url": media_preview_url(&preview_id, &path)?,
                "expiresAt": now_ms().saturating_add(MEDIA_PREVIEW_TTL.as_millis() as u64),
            }))
        }
        "release" => {
            let preview_id = required_string(&args, "previewId")?;
            release_media_preview(&preview_id)?;
            Ok(Value::Null)
        }
        _ => unsupported_method("media-preview", method),
    }
}

fn media_preview_store() -> &'static Mutex<HashMap<String, MediaPreviewLease>> {
    MEDIA_PREVIEWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn new_media_preview_id(path: &Path, size: u64) -> String {
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    digest.update(size.to_le_bytes());
    digest.update(now_ms().to_le_bytes());
    let suffix = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("media-{suffix}")
}

fn store_media_preview(id: String, lease: MediaPreviewLease) -> Result<String, String> {
    let mut previews = media_preview_store()
        .lock()
        .map_err(|_| "媒体预览生命周期锁已损坏".to_owned())?;
    let now = Instant::now();
    previews.retain(|_, current| current.expires_at > now);
    if previews.len() >= MAX_MEDIA_PREVIEWS {
        return Err("媒体预览授权过多，请先关闭已有预览".to_owned());
    }
    let url = media_preview_url(&id, &lease.path)?;
    previews.insert(id, lease);
    Ok(url)
}

#[cfg(test)]
pub(crate) fn insert_media_preview_for_test(
    id: String,
    path: PathBuf,
    media_type: &'static str,
    expires_at: Instant,
) {
    media_preview_store().lock().unwrap().insert(
        id,
        MediaPreviewLease {
            path,
            media_type,
            expires_at,
        },
    );
}

#[cfg(test)]
pub(crate) fn remove_media_preview_for_test(id: &str) {
    media_preview_store().lock().unwrap().remove(id);
}

fn media_preview_url(preview_id: &str, path: &Path) -> Result<String, String> {
    // Windows WebView2 对媒体元素使用 localhost 协议别名更稳定；其他平台保留
    // 注册的原生 scheme。两种 URL 都由 media_protocol 严格校验同一份 lease。
    let base = if cfg!(windows) {
        "http://zcode-media.localhost/preview"
    } else {
        "zcode-media://local/preview"
    };
    let mut url =
        url::Url::parse(base).map_err(|error| format!("媒体预览 URL 构造失败：{error}"))?;
    url.query_pairs_mut()
        .append_pair("path", &crate::path_utils::path_to_frontend(path))
        .append_pair("previewId", preview_id);
    Ok(url.to_string())
}

fn media_preview_lease(id: &str) -> Result<(PathBuf, &'static str), String> {
    let mut previews = media_preview_store()
        .lock()
        .map_err(|_| "媒体预览生命周期锁已损坏".to_owned())?;
    let now = Instant::now();
    previews.retain(|_, current| current.expires_at > now);
    let lease = previews
        .get(id)
        .ok_or_else(|| "媒体预览授权无效或已过期".to_owned())?;
    let canonical =
        fs::canonicalize(&lease.path).map_err(|error| format!("媒体预览文件不可用：{error}"))?;
    if canonical != lease.path {
        return Err("媒体预览目标已被替换，已撤销授权".to_owned());
    }
    Ok((lease.path.clone(), lease.media_type))
}

/// 供 root 的 `zcode-media` URI protocol 在读取请求前复用同一份短期授权。
/// 原生协议必须把 URL 中的 path 一并传入，不能只凭 previewId 读取任意文件。
pub(crate) fn authorize_media_preview_path(
    preview_id: &str,
    requested_path: &Path,
) -> Result<(PathBuf, &'static str), String> {
    let (leased_path, media_type) = media_preview_lease(preview_id)?;
    let requested_path = fs::canonicalize(requested_path)
        .map_err(|error| format!("媒体预览请求路径不可用：{error}"))?;
    if requested_path != leased_path {
        return Err("媒体预览请求路径与授权不一致".to_owned());
    }
    Ok((leased_path, media_type))
}

fn update_media_preview_expiry(id: &str, expires_at: Instant) -> Result<(), String> {
    let mut previews = media_preview_store()
        .lock()
        .map_err(|_| "媒体预览生命周期锁已损坏".to_owned())?;
    let lease = previews
        .get_mut(id)
        .ok_or_else(|| "媒体预览授权无效或已过期".to_owned())?;
    lease.expires_at = expires_at;
    Ok(())
}

fn release_media_preview(id: &str) -> Result<(), String> {
    let mut previews = media_preview_store()
        .lock()
        .map_err(|_| "媒体预览生命周期锁已损坏".to_owned())?;
    if previews.remove(id).is_none() {
        return Err("媒体预览授权无效或已释放".to_owned());
    }
    Ok(())
}

fn system_call(_app: &AppHandle, method: &str, _args: Value) -> Result<Value, String> {
    match method {
        "info" => {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
                .ok_or_else(|| "无法确定用户主目录".to_owned())?;
            // Source 的 SystemInfo 复用 Node 的 process.platform 命名（win32/darwin）；
            // Rust 的 consts::OS 使用 windows/macos，必须在边界统一，否则 Windows
            // 集成 Shell 选择器会被 UI 错误隐藏。
            let platform = match std::env::consts::OS {
                "windows" => "win32",
                "macos" => "darwin",
                platform => platform,
            };
            Ok(json!({
                "homedir": crate::path_utils::path_to_frontend(&home),
                "platform": platform,
            }))
        }
        "listIntegratedTerminalShells" => {
            to_value(crate::terminal::integrated_terminal_shells_list())
        }
        "probeIntranet" => unsupported(
            "system",
            method,
            "intranet probing is not part of the local service contract",
        ),
        _ => unsupported_method("system", method),
    }
}

/// 将 Rust 应用设置中的真实 shell 选择投影回 Source settings DTO。
fn frontend_integrated_terminal_shell(shell: crate::app_settings::TerminalShell) -> Value {
    if shell == crate::app_settings::TerminalShell::Auto {
        return json!({"mode": "auto"});
    }
    crate::terminal::integrated_terminal_shells_list()
        .into_iter()
        .find(|option| match shell {
            crate::app_settings::TerminalShell::GitBash => option.dialect == "git-bash",
            crate::app_settings::TerminalShell::Cmd => option.dialect == "cmd",
            crate::app_settings::TerminalShell::Auto
            | crate::app_settings::TerminalShell::PowerShell
            | crate::app_settings::TerminalShell::PowerShell7 => false,
        })
        .map(|option| {
            json!({
                "mode": "shell",
                "dialect": option.dialect,
                "id": option.id,
                "label": option.label,
                "path": option.path,
            })
        })
        .unwrap_or_else(|| json!({"mode": "auto"}))
}

/// 将 Source settings 的 shell 选择转换为真实 PTY 使用的内部枚举。
fn app_terminal_shell_from_frontend(
    value: &Value,
) -> Result<crate::app_settings::TerminalShell, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "integratedTerminalShell 必须是对象".to_owned())?;
    match object.get("mode").and_then(Value::as_str) {
        Some("auto") => Ok(crate::app_settings::TerminalShell::Auto),
        Some("shell") => {
            let dialect = object
                .get("dialect")
                .and_then(Value::as_str)
                .ok_or_else(|| "integratedTerminalShell 缺少 dialect".to_owned())?;
            let expected_id = match dialect {
                "cmd" => "cmd",
                "git-bash" => "gitBash",
                _ => return Err("integratedTerminalShell dialect 无效".to_owned()),
            };
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "integratedTerminalShell 缺少 id".to_owned())?;
            if id != expected_id {
                return Err("integratedTerminalShell id 与 dialect 不匹配".to_owned());
            }
            let shell = if dialect == "cmd" {
                crate::app_settings::TerminalShell::Cmd
            } else {
                crate::app_settings::TerminalShell::GitBash
            };
            if !crate::terminal::terminal_shells_list()
                .iter()
                .any(|option| option.id == shell)
            {
                return Err("选择的集成终端 Shell 当前不可用".to_owned());
            }
            Ok(shell)
        }
        _ => Err("integratedTerminalShell.mode 无效".to_owned()),
    }
}

fn setting_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "get" => frontend_settings_get(app),
        "update" => frontend_settings_update(app, args),
        "ensureDefaultProject" => {
            // ZCode 传来的 homedir 只用于兼容请求形状；默认项目必须留在
            // KeenCode 数据根，避免 UI 初始化把文件写入用户真实 home。
            let _ = required_string(&args, "homedir")?;
            let path = crate::storage::root_dir(app)
                .map_err(|error| error.to_string())?
                .join("projects")
                .join("ZCodeProject");
            let created = !path.exists();
            fs::create_dir_all(&path).map_err(|error| format!("无法创建默认项目目录：{error}"))?;
            Ok(json!({"path": crate::path_utils::path_to_frontend(&path), "created": created}))
        }
        "updateDataBaseDir" => unsupported(
            "setting",
            method,
            "数据目录迁移需要 root 统一的事务存储桥；当前版本没有完整迁移实现",
        ),
        _ => unsupported_method("setting", method),
    }
}

const FRONTEND_SETTINGS_KEYS: &[&str] = &[
    "recentProjects",
    "locale",
    "shortcutBindings",
    "localePreference",
    "terminalInheritSystemProfile",
    "terminalFontFamily",
    "integratedTerminalShell",
    "httpProxy",
    "httpProxyNoProxy",
    "embeddedBrowserViewportPreference",
    "computerUseComposerEntryHidden",
    "taskAutoArchiveEnabled",
    "taskAutoArchiveOlderThanDays",
    "closeToTrayOnWindows",
    "closeToTrayOnWindowsMigrationInitialized",
    "keepAwakeWhileRunning",
    "desktopZoomLevel",
    "desktopWindowSize",
    "desktopChromiumHardwareAccelerationEnabled",
    "messageStreamShowReasoning",
    "messageStreamShowReasoningMigrationInitialized",
    "messageStreamShowTodos",
    "toolGroupingExploreEnabled",
    "toolGroupingTerminalEnabled",
    "toolGroupingChangesEnabled",
    "zcodeInteractionBehavior",
    "askUserQuestionAutoResolutionEnabled",
    "startPlanRecommendationDismissed",
    "nativeSearchEnhancementsEnabled",
    "onboardingOccupation",
    "proactiveSuggestionsEnabled",
    "memoryEnabled",
    "lastWorkspaceSession",
    "lastActiveTabIndex",
    "lastActiveTaskByWorkspace",
    "skippedElectronUpdateVersions",
    "settingsSyncFirstRunPromptHandled",
];

fn frontend_settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("frontend-settings.json"))
}

fn frontend_locale_from_settings(settings: &crate::app_settings::AppSettings) -> &'static str {
    match settings.interface_language.as_code() {
        "en" => "en-US",
        "zh-TW" => "zh-TW",
        _ => "zh-CN",
    }
}

fn frontend_settings_defaults(app: &AppHandle) -> Result<Map<String, Value>, String> {
    let settings = crate::app_settings::get(app).map_err(|error| error.to_string())?;
    let locale = frontend_locale_from_settings(&settings);
    serde_json::from_value(json!({
        "recentProjects": [],
        "locale": locale,
        // localePreference 的 source 默认是 system；locale 则反映 KeenCode
        // 当前生效语言，两者不能用同一个值冒充。
        "localePreference": "system",
        "shortcutBindings": {},
        "terminalInheritSystemProfile": settings.terminal_inherit_system_profile,
        "terminalFontFamily": settings.terminal_font_family,
        "integratedTerminalShell": frontend_integrated_terminal_shell(settings.terminal_shell),
        "httpProxy": settings.http_proxy.clone().unwrap_or_default(),
        "httpProxyNoProxy": settings.http_proxy_no_proxy.clone().unwrap_or_default(),
        "embeddedBrowserViewportPreference": {
            "mode": "normal",
            "viewport": {"width": 393, "height": 852},
            "zoom": "fit",
        },
        "computerUseComposerEntryHidden": true,
        "taskAutoArchiveEnabled": settings.auto_archive_conversations,
        "taskAutoArchiveOlderThanDays": settings.archive_retention_days,
        "closeToTrayOnWindows": settings.close_to_tray,
        "closeToTrayOnWindowsMigrationInitialized": true,
        "keepAwakeWhileRunning": settings.keep_computer_awake,
        "desktopChromiumHardwareAccelerationEnabled": settings.chrome_hardware_acceleration,
        "messageStreamShowReasoning": settings.show_thinking_process,
        "messageStreamShowReasoningMigrationInitialized": true,
        "messageStreamShowTodos": false,
        "toolGroupingExploreEnabled": true,
        "toolGroupingTerminalEnabled": true,
        "toolGroupingChangesEnabled": false,
        "zcodeInteractionBehavior": "queue",
        "askUserQuestionAutoResolutionEnabled": true,
        "startPlanRecommendationDismissed": false,
        "nativeSearchEnhancementsEnabled": true,
        "memoryEnabled": settings.local_memories,
        "lastWorkspaceSession": [],
        "lastActiveTabIndex": 0,
        "skippedElectronUpdateVersions": {},
    }))
    .map_err(|error| format!("前端设置默认值编码失败：{error}"))
}

/// 校验 ZCode UI 设置的形状，并过滤 KeenCode 不支持的会话事实。
///
/// UI 配置与 `AppSettings` 不是同一 schema；这里不能把任意 JSON 原样写入，
/// 否则旧版/云端字段会重新进入本地设置。`lastWorkspaceSession` 只保留本地
/// workspace 的布局恢复信息，远端连接与 task 标识由会话所有者管理。
fn normalize_frontend_setting(key: &str, value: &Value) -> Result<Value, String> {
    fn non_empty_string(value: &Value, key: &str) -> Result<String, String> {
        let text = value
            .as_str()
            .ok_or_else(|| format!("前端设置字段 {key} 必须是字符串"))?;
        if text.trim().is_empty() || text.chars().any(char::is_control) {
            return Err(format!("前端设置字段 {key} 不能为空或包含控制字符"));
        }
        Ok(text.to_owned())
    }

    fn editable_string(value: &Value, key: &str) -> Result<String, String> {
        let text = value
            .as_str()
            .ok_or_else(|| format!("前端设置字段 {key} 必须是字符串"))?;
        if text.chars().any(char::is_control) {
            return Err(format!("前端设置字段 {key} 不能包含控制字符"));
        }
        Ok(text.trim().to_owned())
    }

    fn bool_value(value: &Value, key: &str) -> Result<Value, String> {
        value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| format!("前端设置字段 {key} 必须是布尔值"))
    }

    fn integer(value: &Value, key: &str) -> Result<i64, String> {
        value
            .as_i64()
            .ok_or_else(|| format!("前端设置字段 {key} 必须是整数"))
    }

    match key {
        "recentProjects" => {
            let values = value
                .as_array()
                .ok_or_else(|| "前端设置字段 recentProjects 必须是数组".to_owned())?;
            if values.len() > 10 {
                return Err("前端设置字段 recentProjects 最多保存 10 个项目".to_owned());
            }
            let mut result = Vec::with_capacity(values.len());
            for value in values {
                result.push(Value::String(non_empty_string(value, key)?));
            }
            Ok(Value::Array(result))
        }
        "locale" => {
            let value = non_empty_string(value, key)?;
            if !matches!(value.as_str(), "zh-CN" | "zh-TW" | "en-US") {
                return Err(format!("不支持的前端 locale：{value}"));
            }
            Ok(Value::String(value))
        }
        "localePreference" => {
            let value = non_empty_string(value, key)?;
            if !matches!(value.as_str(), "system" | "zh-CN" | "zh-TW" | "en-US") {
                return Err(format!("不支持的 localePreference：{value}"));
            }
            Ok(Value::String(value))
        }
        "shortcutBindings" => {
            let bindings = value
                .as_object()
                .ok_or_else(|| "前端设置字段 shortcutBindings 必须是对象".to_owned())?;
            let mut result = Map::new();
            for (command, shortcuts) in bindings {
                if command.trim().is_empty() || command.chars().any(char::is_control) {
                    return Err("shortcutBindings 命令标识无效".to_owned());
                }
                let shortcuts = shortcuts
                    .as_array()
                    .ok_or_else(|| format!("shortcutBindings.{command} 必须是数组"))?;
                let mut values = Vec::with_capacity(shortcuts.len());
                for shortcut in shortcuts {
                    values.push(Value::String(non_empty_string(shortcut, key)?));
                }
                result.insert(command.clone(), Value::Array(values));
            }
            Ok(Value::Object(result))
        }
        "terminalInheritSystemProfile"
        | "computerUseComposerEntryHidden"
        | "taskAutoArchiveEnabled"
        | "closeToTrayOnWindows"
        | "closeToTrayOnWindowsMigrationInitialized"
        | "keepAwakeWhileRunning"
        | "desktopChromiumHardwareAccelerationEnabled"
        | "messageStreamShowReasoning"
        | "messageStreamShowReasoningMigrationInitialized"
        | "messageStreamShowTodos"
        | "toolGroupingExploreEnabled"
        | "toolGroupingTerminalEnabled"
        | "toolGroupingChangesEnabled"
        | "askUserQuestionAutoResolutionEnabled"
        | "startPlanRecommendationDismissed"
        | "nativeSearchEnhancementsEnabled"
        | "proactiveSuggestionsEnabled"
        | "memoryEnabled"
        | "settingsSyncFirstRunPromptHandled" => bool_value(value, key),
        "terminalFontFamily" => Ok(Value::String(non_empty_string(value, key)?)),
        "httpProxy" | "httpProxyNoProxy" => Ok(Value::String(editable_string(value, key)?)),
        "integratedTerminalShell" => {
            let shell = value
                .as_object()
                .ok_or_else(|| "integratedTerminalShell 必须是对象".to_owned())?;
            match shell.get("mode").and_then(Value::as_str) {
                Some("auto") if shell.len() == 1 => Ok(value.clone()),
                Some("shell") => {
                    let dialect = shell
                        .get("dialect")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "integratedTerminalShell 缺少 dialect".to_owned())?;
                    if !matches!(dialect, "cmd" | "git-bash") {
                        return Err("integratedTerminalShell dialect 无效".to_owned());
                    }
                    for field in ["id", "label", "path"] {
                        non_empty_string(
                            shell
                                .get(field)
                                .ok_or_else(|| format!("integratedTerminalShell 缺少 {field}"))?,
                            field,
                        )?;
                    }
                    Ok(value.clone())
                }
                _ => Err("integratedTerminalShell.mode 无效".to_owned()),
            }
        }
        "embeddedBrowserViewportPreference" => {
            let preference = value
                .as_object()
                .ok_or_else(|| "embeddedBrowserViewportPreference 必须是对象".to_owned())?;
            let mode = preference
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| "embeddedBrowserViewportPreference 缺少 mode".to_owned())?;
            if !matches!(mode, "normal" | "responsive") {
                return Err("embeddedBrowserViewportPreference.mode 无效".to_owned());
            }
            let viewport = preference
                .get("viewport")
                .and_then(Value::as_object)
                .ok_or_else(|| "embeddedBrowserViewportPreference 缺少 viewport".to_owned())?;
            let width = integer(
                viewport
                    .get("width")
                    .ok_or_else(|| "embeddedBrowserViewportPreference 缺少 width".to_owned())?,
                "viewport.width",
            )?;
            let height = integer(
                viewport
                    .get("height")
                    .ok_or_else(|| "embeddedBrowserViewportPreference 缺少 height".to_owned())?,
                "viewport.height",
            )?;
            if !(320..=3840).contains(&width) || !(320..=2160).contains(&height) {
                return Err("embeddedBrowserViewportPreference viewport 超出范围".to_owned());
            }
            let zoom = preference
                .get("zoom")
                .and_then(Value::as_str)
                .ok_or_else(|| "embeddedBrowserViewportPreference 缺少 zoom".to_owned())?;
            if !matches!(zoom, "fit" | "50" | "75" | "100" | "125" | "150" | "200") {
                return Err("embeddedBrowserViewportPreference.zoom 无效".to_owned());
            }
            Ok(value.clone())
        }
        "taskAutoArchiveOlderThanDays" => {
            let days = integer(value, key)?;
            if !(1..=365).contains(&days) {
                return Err("taskAutoArchiveOlderThanDays 必须在 1 到 365 之间".to_owned());
            }
            Ok(Value::Number(days.into()))
        }
        "desktopZoomLevel" => {
            let level = integer(value, key)?;
            if !(-3..=5).contains(&level) {
                return Err("desktopZoomLevel 必须在 -3 到 5 之间".to_owned());
            }
            Ok(Value::Number(level.into()))
        }
        "desktopWindowSize" => {
            let window = value
                .as_object()
                .ok_or_else(|| "desktopWindowSize 必须是对象".to_owned())?;
            let width = integer(
                window
                    .get("width")
                    .ok_or_else(|| "desktopWindowSize 缺少 width".to_owned())?,
                "desktopWindowSize.width",
            )?;
            let height = integer(
                window
                    .get("height")
                    .ok_or_else(|| "desktopWindowSize 缺少 height".to_owned())?,
                "desktopWindowSize.height",
            )?;
            if width < 480 || height < 640 {
                return Err("desktopWindowSize 尺寸过小".to_owned());
            }
            if !window.get("maximized").is_some_and(Value::is_boolean) {
                return Err("desktopWindowSize.maximized 必须是布尔值".to_owned());
            }
            Ok(value.clone())
        }
        "zcodeInteractionBehavior" => {
            let value = non_empty_string(value, key)?;
            if !matches!(value.as_str(), "queue" | "guide") {
                return Err("zcodeInteractionBehavior 无效".to_owned());
            }
            Ok(Value::String(value))
        }
        "onboardingOccupation" => {
            if value.is_null() {
                return Ok(Value::Null);
            }
            let value = non_empty_string(value, key)?;
            if !matches!(
                value.as_str(),
                "office"
                    | "developer"
                    | "independent"
                    | "infrastructure"
                    | "product"
                    | "design"
                    | "student"
                    | "creator"
                    | "operations"
                    | "marketing"
                    | "finance"
                    | "accounting"
                    | "legal"
                    | "other"
            ) {
                return Err("onboardingOccupation 无效".to_owned());
            }
            Ok(Value::String(value))
        }
        "lastWorkspaceSession" => {
            let entries = value
                .as_array()
                .ok_or_else(|| "lastWorkspaceSession 必须是数组".to_owned())?;
            let mut local_entries = Vec::new();
            for entry in entries {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                if entry.get("kind").and_then(Value::as_str) != Some("local") {
                    continue;
                }
                let workspace_path = entry
                    .get("workspacePath")
                    .ok_or_else(|| "本地 workspace session 缺少 workspacePath".to_owned())?;
                let workspace_path = non_empty_string(workspace_path, "workspacePath")?;
                let purpose = entry
                    .get("workspacePurpose")
                    .and_then(Value::as_str)
                    .unwrap_or("project");
                if !matches!(purpose, "project" | "conversation") {
                    return Err("本地 workspace session 的 workspacePurpose 无效".to_owned());
                }
                local_entries.push(json!({
                    "kind": "local",
                    "workspacePath": workspace_path,
                    "workspacePurpose": purpose,
                }));
            }
            Ok(Value::Array(local_entries))
        }
        "lastActiveTabIndex" => {
            let index = integer(value, key)?;
            if index < 0 {
                return Err("lastActiveTabIndex 不能为负数".to_owned());
            }
            Ok(Value::Number(index.into()))
        }
        "lastActiveTaskByWorkspace" => unsupported(
            "setting",
            key,
            "task/session facts are owned by the session adapter",
        ),
        "skippedElectronUpdateVersions" => {
            let versions = value
                .as_object()
                .ok_or_else(|| "skippedElectronUpdateVersions 必须是对象".to_owned())?;
            for (channel, version) in versions {
                if !matches!(channel.as_str(), "stable" | "preview") {
                    return Err(format!("未知更新 channel：{channel}"));
                }
                non_empty_string(version, key)?;
            }
            Ok(value.clone())
        }
        _ => Ok(value.clone()),
    }
}

fn frontend_settings_get(app: &AppHandle) -> Result<Value, String> {
    let path = frontend_settings_path(app)?;
    let mut defaults = frontend_settings_defaults(app)?;
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(&path, 8 * 1024 * 1024, "前端布局设置")
            .map_err(|error| error.to_string())?
    else {
        return Ok(Value::Object(defaults));
    };
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("前端布局设置格式无效：{error}"))?;
    let stored = value
        .as_object()
        .ok_or_else(|| "前端布局设置必须是对象".to_owned())?;
    for key in stored.keys() {
        if !FRONTEND_SETTINGS_KEYS.contains(&key.as_str()) {
            return Err(format!("前端布局设置包含不支持的字段：{key}"));
        }
    }
    for (key, value) in stored {
        // 会话事实字段来自旧版 ZCode 设置；读取时丢弃，避免把旧远端状态重新
        // 暴露给当前 UI。其余字段必须通过同一份 schema 校验后再合并默认值。
        if key == "lastActiveTaskByWorkspace" {
            continue;
        }
        defaults.insert(key.clone(), normalize_frontend_setting(key, value)?);
    }
    // Native PTY 与进程网络出口的事实源分别是 settings.json；不能让旧的
    // frontend-settings.json 值覆盖已生效的 Rust 设置，避免界面显示假配置。
    let settings = crate::app_settings::get(app).map_err(|error| error.to_string())?;
    defaults.insert(
        "integratedTerminalShell".to_owned(),
        frontend_integrated_terminal_shell(settings.terminal_shell),
    );
    defaults.insert(
        "httpProxy".to_owned(),
        Value::String(settings.http_proxy.unwrap_or_default()),
    );
    defaults.insert(
        "httpProxyNoProxy".to_owned(),
        Value::String(settings.http_proxy_no_proxy.unwrap_or_default()),
    );
    Ok(Value::Object(defaults))
}

fn frontend_settings_update(app: &AppHandle, args: Value) -> Result<Value, String> {
    let patch = args
        .as_object()
        .ok_or_else(|| "前端设置补丁必须是对象".to_owned())?;
    for key in patch.keys() {
        if !FRONTEND_SETTINGS_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "前端设置字段不受支持或属于官方账号/cloud 能力：{key}"
            ));
        }
    }
    let current = frontend_settings_get(app)?;
    let mut merged = current
        .as_object()
        .cloned()
        .ok_or_else(|| "前端布局设置必须是对象".to_owned())?;
    for (key, value) in patch {
        if value.is_null() && key != "onboardingOccupation" {
            return Err(format!("前端设置字段不能为 null：{key}"));
        }
        if key == "lastActiveTaskByWorkspace" {
            return unsupported(
                "setting",
                "update",
                "task/session facts are owned by the session adapter",
            );
        }
        merged.insert(key.clone(), normalize_frontend_setting(key, value)?);
    }
    let shared_patch = json!({
        "interfaceLanguage": match merged.get("locale").and_then(Value::as_str) {
            Some("en-US") => "en",
            Some("zh-TW") => "zh-TW",
            _ => "zh",
        },
        "terminalFontFamily": merged.get("terminalFontFamily"),
        "terminalInheritSystemProfile": merged.get("terminalInheritSystemProfile"),
        "autoArchiveConversations": merged.get("taskAutoArchiveEnabled"),
        "archiveRetentionDays": merged.get("taskAutoArchiveOlderThanDays"),
        "closeToTray": merged.get("closeToTrayOnWindows"),
        "keepComputerAwake": merged.get("keepAwakeWhileRunning"),
        "chromeHardwareAcceleration": merged
            .get("desktopChromiumHardwareAccelerationEnabled"),
        "showThinkingProcess": merged.get("messageStreamShowReasoning"),
        "localMemories": merged.get("memoryEnabled"),
    });
    let mut shared_patch = shared_patch
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, value)| !value.is_null())
        .collect::<Map<_, _>>();
    if patch
        .keys()
        .any(|key| matches!(key.as_str(), "httpProxy" | "httpProxyNoProxy"))
    {
        for key in ["httpProxy", "httpProxyNoProxy"] {
            if let Some(value) = merged.get(key) {
                shared_patch.insert(key.to_owned(), value.clone());
            }
        }
    }
    if patch.contains_key("integratedTerminalShell") {
        let shell = app_terminal_shell_from_frontend(
            merged
                .get("integratedTerminalShell")
                .ok_or_else(|| "integratedTerminalShell 设置缺失".to_owned())?,
        )?;
        shared_patch.insert(
            "terminalShell".to_owned(),
            serde_json::to_value(shell).map_err(|error| format!("终端 Shell 编码失败：{error}"))?,
        );
    }
    if !shared_patch.is_empty() {
        let patch: crate::app_settings::AppSettingsPatch =
            serde_json::from_value(Value::Object(shared_patch))
                .map_err(|error| format!("共享应用设置补丁无效：{error}"))?;
        crate::app_settings::set(app, patch).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&Value::Object(merged))
        .map_err(|error| format!("前端布局设置编码失败：{error}"))?;
    crate::storage::atomic_write_private(&frontend_settings_path(app)?, &bytes)
        .map_err(|error| format!("保存前端布局设置失败：{error}"))?;
    Ok(Value::Null)
}

fn credential_call(_app: &AppHandle, method: &str, _args: Value) -> Result<Value, String> {
    unsupported(
        "credential",
        method,
        "credential reads/writes require the existing keyring owner and are not safe through a generic RPC fallback",
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BroadcastLease {
    key: String,
    owner: String,
    token: String,
    expires_at: u64,
}

static BROADCAST_CLAIMS: OnceLock<Mutex<HashMap<String, BroadcastLease>>> = OnceLock::new();

fn claims() -> &'static Mutex<HashMap<String, BroadcastLease>> {
    BROADCAST_CLAIMS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn broadcast_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "send" => {
            let message = object(&args)?.get("message").cloned().unwrap_or(args);
            let bytes = serde_json::to_vec(&message)
                .map_err(|error| format!("广播消息序列化失败：{error}"))?;
            if bytes.len() > 512 * 1024 {
                return Err("广播消息超过 512 KB".to_owned());
            }
            app.emit(BROADCAST_EVENT, message.clone())
                .map_err(|error| format!("广播消息发送失败：{error}"))?;
            Ok(Value::Null)
        }
        "acquireClaim" | "tryClaim" => {
            let key = required_string(&args, "key")?;
            let owner = optional_string(&args, "owner")?.unwrap_or_else(|| "frontend".to_owned());
            let now = now_ms();
            let mut state = claims()
                .lock()
                .map_err(|_| "广播 claim 锁已损坏".to_owned())?;
            state.retain(|_, value| value.expires_at > now);
            if let Some(current) = state.get(&key) {
                if method == "tryClaim" {
                    return Ok(json!({"claimed": false, "lease": current}));
                }
                return Err(format!("广播 claim 已被 {} 持有", current.owner));
            }
            let lease = BroadcastLease {
                key: key.clone(),
                owner,
                token: format!("claim-{}-{}", now, state.len()),
                expires_at: now.saturating_add(30_000),
            };
            state.insert(key, lease.clone());
            if method == "tryClaim" {
                Ok(json!({"claimed": true, "lease": lease}))
            } else {
                Ok(to_value(lease)?)
            }
        }
        "commitClaim" | "releaseClaim" => {
            let lease: BroadcastLease = serde_json::from_value(args)
                .map_err(|error| format!("广播 claim 无效：{error}"))?;
            let mut state = claims()
                .lock()
                .map_err(|_| "广播 claim 锁已损坏".to_owned())?;
            let current = state
                .get(&lease.key)
                .ok_or_else(|| "广播 claim 不存在或已过期".to_owned())?;
            if current.token != lease.token || current.owner != lease.owner {
                return Err("广播 claim 所有者不匹配".to_owned());
            }
            state.remove(&lease.key);
            Ok(Value::Null)
        }
        _ => unsupported_method("broadcast", method),
    }
}

fn project_argument(args: &Value) -> Result<String, String> {
    for key in ["cwd", "projectPath", "workspacePath", "path"] {
        if let Some(value) = object(args)?.get(key).and_then(Value::as_str)
            && !value.is_empty()
        {
            return Ok(value.to_owned());
        }
    }
    Err("缺少项目路径（cwd/projectPath/workspacePath）".to_owned())
}

async fn git_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    let project = project_argument(&args)?;
    match method {
        "getRepositorySummary" => {
            let refresh = git_refresh_call(app, project, args).await?;
            Ok(refresh["summary"].clone())
        }
        "getWorkspaceRepositoryInfo" => git_workspace_repository_info_call(app, project).await,
        "getChanges" => git_changes_call(app, project, args).await,
        "refresh" => git_refresh_call(app, project, args).await,
        "getLocalBranches" => git_local_branches_call(app, project).await,
        "getCommitGraph" => git_commit_graph_call(app, project, args).await,
        "switchBranch" | "createBranchAndSwitch" => {
            let create = method == "createBranchAndSwitch";
            let branch = if create {
                required_string(&args, "branchName")?
            } else {
                required_string(&args, "targetBranchName")?
            };
            let start_point = if create {
                optional_string(&args, "startPoint")?
            } else {
                None
            };
            git_branch_mutation_call(app, project, branch, create, start_point).await
        }
        "getDiff" => git_diff_call(app, project, args).await,
        "getBranchComparison" => {
            let root = registered_root(app, &project)?;
            git_branch_comparison_value(&root)
        }
        "getIgnoredPaths" => {
            let root = registered_root(app, &project)?;
            let output = crate::workspace::run_git(
                &root,
                &[
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                ],
            )?;
            if !output.status.success() {
                return Err(redacted_git_error(&output));
            }
            let paths = String::from_utf8(output.stdout)
                .map_err(|_| "Git 返回非 UTF-8 路径".to_owned())?
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            to_value(paths)
        }
        "stagePaths" | "unstagePaths" => {
            let paths = required_array_strings(&args, "paths")?;
            crate::ui_git::ui_git_mutate(
                app.clone(),
                project,
                if method == "stagePaths" {
                    "stage"
                } else {
                    "unstage"
                }
                .to_owned(),
                paths,
            )
            .await
        }
        "discardPaths" => {
            let paths = required_array_strings(&args, "paths")?;
            if paths.is_empty() || paths.len() > 4096 {
                return Err("Git 丢弃路径数量无效".to_owned());
            }
            let root = registered_root(app, &project)?;
            let mut command = crate::workspace::git_command();
            command
                .arg("-C")
                .arg(&root)
                .arg("restore")
                .arg("--worktree")
                .arg("--");
            for path in &paths {
                validate_git_path(path)?;
                command.arg(path);
            }
            let output = command
                .output()
                .map_err(|error| format!("Git 丢弃失败：{error}"))?;
            if !output.status.success() {
                return Err(redacted_git_error(&output));
            }
            Ok(json!({"ok": true}))
        }
        "commit" => {
            let message = required_string(&args, "message")?;
            let include_unstaged = optional_bool(&args, "includeUnstaged")?
                .or(optional_bool(&args, "stagedOnly")?.map(|staged_only| !staged_only))
                .unwrap_or(false);
            let paths = optional_array_strings(&args, "paths")?;
            let result = if let Some(paths) = paths {
                crate::workspace::git_commit_selected(
                    app.clone(),
                    project.clone(),
                    message,
                    include_unstaged,
                    paths,
                )
                .await?
            } else {
                crate::workspace::git_commit(
                    app.clone(),
                    project.clone(),
                    message,
                    include_unstaged,
                )
                .await?
            };
            let root = registered_root(app, &project)?;
            let status = crate::workspace::git_status(app.clone(), project.clone()).await?;
            let summary =
                git_refresh_value(&project, &status, Some(&root), None)["summary"].clone();
            Ok(json!({
                "commitHash": result.commit,
                "summary": summary,
            }))
        }
        "push" => {
            let root = registered_root(app, &project)?;
            let tracking_before = git_output_trimmed(
                &root,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
            );
            let result = crate::workspace::git_push(app.clone(), project.clone()).await?;
            let tracking_after = git_output_trimmed(
                &root,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
            );
            let status = crate::workspace::git_status(app.clone(), project.clone()).await?;
            let summary =
                git_refresh_value(&project, &status, Some(&root), None)["summary"].clone();
            let remote_name = tracking_after
                .as_deref()
                .and_then(|tracking| tracking.split('/').next())
                .filter(|remote| !remote.is_empty());
            Ok(json!({
                "branchName": status.branch.or(result.branch),
                "trackingBranchName": tracking_after,
                "remoteName": remote_name,
                "setUpstream": tracking_before.is_none() && tracking_after.is_some(),
                "summary": summary,
            }))
        }
        "getIdentity" => {
            let root = registered_root(app, &project)?;
            Ok(git_identity_value(&root).unwrap_or(Value::Null))
        }
        "listWorktrees" => {
            to_value(crate::workspace::git_worktrees_list(app.clone(), project).await?)
        }
        "createWorktree" => {
            let name = required_string(&args, "name")?;
            let start_point = optional_string(&args, "startPoint")?;
            to_value(
                crate::workspace::git_worktree_add(app.clone(), project, name, start_point).await?,
            )
        }
        "gcWorktrees" => {
            let expire = optional_string(&args, "expire")?;
            let dry_run = optional_bool(&args, "dryRun")?.unwrap_or(false);
            let force = optional_bool(&args, "force")?.unwrap_or(false);
            to_value(
                crate::workspace::git_worktree_gc(app.clone(), project, dry_run, force, expire)
                    .await?,
            )
        }
        _ => unsupported_method("git", method),
    }
}

/// 将本地 workspace Git 状态转换为 `IGitService.refresh` 的共享协议。
/// 旧的 `ui_git_query(status)` 返回页面专用摘要，不能直接交给 `useGitRepository`；
/// 这里按 porcelain 的 index/worktree 两列分别生成 staged/unstaged 数组，避免
/// 前端把缺少数组的旧响应当作可迭代数据而崩溃。
async fn git_workspace_repository_info_call(
    app: &AppHandle,
    project: String,
) -> Result<Value, String> {
    let root = registered_root(app, &project)?;
    let status = crate::workspace::git_status(app.clone(), project.clone()).await?;
    let kind = if !status.available {
        "not-repository"
    } else if root.join(".git").is_file() {
        "linked-worktree"
    } else {
        "main-tree"
    };
    Ok(json!({
        "workspacePath": project,
        "kind": kind,
        "isGitAvailable": status.available,
    }))
}

async fn git_branch_mutation_call(
    app: &AppHandle,
    project: String,
    branch: String,
    create: bool,
    start_point: Option<String>,
) -> Result<Value, String> {
    let root = registered_root(app, &project)?;
    let before = crate::workspace::git_status(app.clone(), project.clone()).await?;
    let action = if create {
        "create-and-switch"
    } else {
        "switch"
    };
    match crate::workspace::git_checkout_branch_from(
        app.clone(),
        project.clone(),
        branch.clone(),
        create,
        start_point,
    )
    .await
    {
        Ok(result) => {
            let after = crate::workspace::git_status(app.clone(), project.clone()).await?;
            let summary = git_refresh_value(&project, &after, Some(&root), None)["summary"].clone();
            Ok(json!({
                "ok": true,
                "action": action,
                "branchName": result.branch,
                "didChange": before.branch != after.branch,
                "created": create,
                "summary": summary,
                "issues": [],
            }))
        }
        Err(error) => {
            let summary =
                git_refresh_value(&project, &before, Some(&root), None)["summary"].clone();
            Ok(json!({
                "ok": false,
                "action": action,
                "branchName": branch,
                "didChange": false,
                "created": false,
                "summary": summary,
                "issues": [git_branch_mutation_issue(&error, &branch)],
            }))
        }
    }
}

fn git_branch_mutation_issue(error: &str, branch: &str) -> Value {
    let lower = error.to_ascii_lowercase();
    let code = if error.contains("分支名称无效")
        || lower.contains("invalid branch name")
        || lower.contains("not a valid branch name")
    {
        "invalid-branch-name"
    } else if lower.contains("already exists") || lower.contains("a branch named") {
        "branch-already-exists"
    } else if lower.contains("already checked out at")
        || lower.contains("is already used by worktree")
        || lower.contains("already used by worktree")
    {
        "branch-in-other-worktree"
    } else if lower.contains("untracked working tree files would be overwritten")
        || lower.contains("untracked files would be overwritten")
    {
        "untracked-changes-would-be-overwritten"
    } else if lower.contains("local changes to the following files would be overwritten")
        || lower.contains("would be overwritten by checkout")
        || lower.contains("would be overwritten by switch")
    {
        "tracked-changes-would-be-overwritten"
    } else if lower.contains("cherry-pick")
        || lower.contains("rebase")
        || lower.contains("merge in progress")
        || lower.contains("bisect")
    {
        "operation-in-progress"
    } else if lower.contains("unmerged")
        || lower.contains("conflict")
        || lower.contains("needs merge")
    {
        "conflicts-present"
    } else if lower.contains("did not match any file")
        || lower.contains("invalid reference")
        || lower.contains("unknown revision")
        || lower.contains("pathspec")
    {
        "target-branch-not-found"
    } else {
        "unknown"
    };
    let message = match code {
        "invalid-branch-name" => "分支名称无效",
        "branch-already-exists" => "分支已存在",
        "target-branch-not-found" => "目标分支不存在",
        "tracked-changes-would-be-overwritten" => "已跟踪改动会被覆盖",
        "untracked-changes-would-be-overwritten" => "未跟踪改动会被覆盖",
        "conflicts-present" => "工作区存在冲突",
        "operation-in-progress" => "Git 操作尚未结束",
        "branch-in-other-worktree" => "分支正在其他工作树中使用",
        _ => "Git 分支操作失败",
    };
    let paths = if matches!(
        code,
        "tracked-changes-would-be-overwritten" | "untracked-changes-would-be-overwritten"
    ) {
        git_branch_issue_paths(error)
    } else {
        Vec::new()
    };
    let detail = error
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(512).collect::<String>());
    let mut issue = json!({
        "code": code,
        "message": message,
        "detail": detail.unwrap_or_else(|| branch.to_owned()),
    });
    if !paths.is_empty() {
        issue["paths"] = json!(paths);
    }
    issue
}

fn git_branch_issue_paths(error: &str) -> Vec<String> {
    let mut collecting = false;
    let mut paths = Vec::new();
    for line in error.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if (lower.contains("following") && lower.contains("would be overwritten"))
            || lower.contains("untracked files would be overwritten")
        {
            collecting = true;
            continue;
        }
        if !collecting {
            continue;
        }
        if trimmed.is_empty()
            || lower.starts_with("please ")
            || lower.starts_with("aborting")
            || lower.starts_with("error:")
            || lower.starts_with("fatal:")
        {
            collecting = false;
            continue;
        }
        if !paths.iter().any(|path| path == trimmed) {
            paths.push(trimmed.to_owned());
        }
    }
    paths
}

async fn git_changes_call(app: &AppHandle, project: String, args: Value) -> Result<Value, String> {
    let source_id = required_string(&args, "sourceId")?;
    let refresh = git_refresh_call(app, project, args).await?;
    match source_id.as_str() {
        "unstaged" => Ok(refresh["unstagedChanges"].clone()),
        "staged" => Ok(refresh["stagedChanges"].clone()),
        _ => Err("Git 变更来源必须是 unstaged 或 staged".to_owned()),
    }
}

async fn git_local_branches_call(app: &AppHandle, project: String) -> Result<Value, String> {
    let status = crate::workspace::git_status(app.clone(), project.clone()).await?;
    let root = registered_root(app, &project)?;
    Ok(git_local_branches_value(&root, &status))
}

fn git_local_branches_value(root: &Path, status: &crate::workspace::GitStatusResult) -> Value {
    if !status.available {
        return json!({
            "headRefType": "detached",
            "currentBranchName": Value::Null,
            "branches": [],
        });
    }
    let current_branch = status.branch.clone();
    let branches = status
        .branches
        .iter()
        .map(|name| {
            let ref_name = format!("refs/heads/{name}");
            let commit_hash = git_output_trimmed(root, &["rev-parse", "--verify", &ref_name]);
            let commit_timestamp_ms =
                git_output_trimmed(root, &["show", "-s", "--format=%ct", &ref_name])
                    .and_then(|value| value.parse::<i64>().ok())
                    .and_then(|seconds| seconds.checked_mul(1_000));
            let upstream_ref = format!("{name}@{{upstream}}");
            let upstream_name = git_output_trimmed(
                root,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    &upstream_ref,
                ],
            );
            json!({
                "name": name,
                "isCurrent": current_branch.as_deref() == Some(name.as_str()),
                "upstreamName": upstream_name,
                "commitHash": commit_hash,
                "commitTimestampMs": commit_timestamp_ms,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "headRefType": if current_branch.is_some() { "branch" } else { "detached" },
        "currentBranchName": current_branch,
        "branches": branches,
    })
}

async fn git_commit_graph_call(
    app: &AppHandle,
    project: String,
    args: Value,
) -> Result<Value, String> {
    let max_count = optional_u64(&args, "maxCount")?.unwrap_or(50);
    if max_count == 0 || max_count > 50 {
        return Err("Git 提交图查询数量必须为 1 到 50".to_owned());
    }
    let skip = optional_u64(&args, "skip")?.unwrap_or(0);
    if skip > 1_000_000 {
        return Err("Git 提交图偏移量过大".to_owned());
    }
    let root = registered_root(app, &project)?;
    git_commit_graph_value(&root, max_count as usize, skip as usize)
}

fn git_commit_graph_value(root: &Path, max_count: usize, skip: usize) -> Result<Value, String> {
    let count_arg = format!("-n{}", max_count.saturating_add(1));
    let skip_arg = format!("--skip={skip}");
    let output = crate::workspace::run_git(
        root,
        &[
            "log",
            "--no-show-signature",
            &skip_arg,
            &count_arg,
            "--format=%H%x00%P%x00%an%x00%ct%x00%s%x00%D",
            "-z",
        ],
    )?;
    if !output.status.success() {
        return Err(redacted_git_error(&output));
    }
    let raw =
        String::from_utf8(output.stdout).map_err(|_| "Git 提交图返回非 UTF-8 数据".to_owned())?;
    let fields = raw.split('\0').collect::<Vec<_>>();
    let remote_names = crate::workspace::run_git(root, &["remote"])
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut commits = fields
        .chunks(6)
        .filter_map(|record| {
            if record.len() != 6 || record[0].is_empty() {
                return None;
            }
            let authored_at_ms = record[3]
                .parse::<i64>()
                .ok()
                .and_then(|seconds| seconds.checked_mul(1_000));
            Some(json!({
                "hash": record[0],
                "parents": record[1]
                    .split_whitespace()
                    .filter(|parent| !parent.is_empty())
                    .collect::<Vec<_>>(),
                "refs": git_commit_graph_refs(record[5], &remote_names),
                "subject": record[4],
                "authorName": if record[2].is_empty() { Value::Null } else { json!(record[2]) },
                "authoredAtMs": authored_at_ms,
            }))
        })
        .collect::<Vec<_>>();
    let has_more = commits.len() > max_count;
    commits.truncate(max_count);
    Ok(json!({
        "commits": commits,
        "hasMore": has_more,
    }))
}

fn git_commit_graph_refs(decorations: &str, remote_names: &BTreeSet<String>) -> Vec<Value> {
    decorations
        .split(", ")
        .filter_map(|raw| {
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            if let Some(name) = raw.strip_prefix("HEAD -> ") {
                return Some(json!({"name": name, "kind": "head"}));
            }
            if raw == "HEAD" {
                return Some(json!({"name": "HEAD", "kind": "head"}));
            }
            if let Some(name) = raw.strip_prefix("tag: ") {
                return Some(json!({"name": name, "kind": "tag"}));
            }
            let kind = raw
                .split_once('/')
                .filter(|(remote, _)| remote_names.contains(*remote))
                .map(|_| "remote")
                .unwrap_or("branch");
            Some(json!({"name": raw, "kind": kind}))
        })
        .collect()
}

async fn git_diff_call(app: &AppHandle, project: String, args: Value) -> Result<Value, String> {
    let requested_path = required_string(&args, "path")?;
    let root = registered_root(app, &project)?;
    let source_id = optional_string(&args, "sourceId")?.unwrap_or_else(|| {
        if optional_bool(&args, "staged")
            .ok()
            .flatten()
            .unwrap_or(false)
        {
            "staged".to_owned()
        } else {
            "unstaged".to_owned()
        }
    });
    let scope = match source_id.as_str() {
        "unstaged" => "unstaged",
        "staged" => "staged",
        "branch" => "branch",
        "last-turn" => return Err("Git last-turn diff 由会话变更快照提供".to_owned()),
        _ => return Err("Git diff 来源无效".to_owned()),
    };
    let max_bytes = optional_u64(&args, "maxBytes")?.unwrap_or(1_000_000);
    if max_bytes == 0 || max_bytes > 1_000_000 {
        return Err("Git diff 大小上限必须为 1 到 1000000".to_owned());
    }
    let revision = optional_string(&args, "revision")?;
    git_diff_value(
        &root,
        &requested_path,
        scope,
        revision.as_deref(),
        max_bytes as usize,
    )
}

fn git_diff_value(
    root: &Path,
    requested_path: &str,
    scope: &str,
    revision: Option<&str>,
    max_bytes: usize,
) -> Result<Value, String> {
    let relative_path = git_relative_path(root, requested_path)?;
    let result = crate::ui_git::query(
        root,
        "diff",
        scope,
        revision,
        Some(&relative_path),
        max_bytes,
    )?;
    let patch = result
        .get("patch")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let truncated = result
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let availability = if truncated {
        "truncated"
    } else if patch.as_deref().is_some_and(|value| !value.is_empty()) {
        "patch"
    } else {
        "unavailable"
    };
    Ok(json!({
        "path": requested_path,
        "availability": availability,
        "patch": patch.filter(|value| !value.is_empty()),
        "beforeContent": Value::Null,
        "afterContent": Value::Null,
        "summary": Value::Null,
    }))
}

fn git_output_trimmed(root: &Path, args: &[&str]) -> Option<String> {
    let output = crate::workspace::run_git(root, args).ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then_some(value.to_owned())
}

fn git_relative_path(root: &Path, path: &str) -> Result<String, String> {
    validate_git_path(path)?;
    let comparison_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let input = Path::new(path);
    let relative = if input.is_absolute() {
        let canonical = if input.exists() {
            fs::canonicalize(input).map_err(|error| format!("无法访问 Git 文件路径：{error}"))?
        } else {
            input.to_path_buf()
        };
        canonical
            .strip_prefix(&comparison_root)
            .map(Path::to_path_buf)
            .map_err(|_| "Git 文件路径超出项目目录".to_owned())?
    } else {
        input.to_path_buf()
    };
    let relative = crate::path_utils::path_to_frontend(&relative);
    let relative_path = Path::new(&relative);
    if relative_path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        )
    }) {
        return Err("Git 文件路径必须位于项目内".to_owned());
    }
    Ok(relative)
}

async fn git_refresh_call(app: &AppHandle, project: String, args: Value) -> Result<Value, String> {
    let include_identity = optional_bool(&args, "includeIdentity")?.unwrap_or(false);
    let include_branch_comparison =
        optional_bool(&args, "includeBranchComparison")?.unwrap_or(false);
    // 对话草稿目录是应用数据下的受限 workspace，不是 Git 项目。首屏 Git
    // 状态查询仍会经过这里，但不能把“未登记项目”当成 RPC 失败；返回明确的
    // 非仓库空快照，让文件树和 Git 面板保持可用空态，同时继续拒绝其他 Git 写入。
    let conversation_root = existing_conversation_workspace_root(app)?;
    if conversation_root
        .as_deref()
        .is_some_and(|root| crate::workspace::conversation_read_root(&project, root).is_ok())
    {
        let status = crate::workspace::GitStatusResult {
            available: false,
            files: Vec::new(),
            branch: None,
            branches: Vec::new(),
            additions: 0,
            deletions: 0,
            has_unstaged_changes: false,
            reason: Some("对话 workspace 不属于项目 Git 仓库".to_owned()),
        };
        return Ok(git_refresh_value(&project, &status, None, None));
    }
    let status = crate::workspace::git_status(app.clone(), project.clone()).await?;
    let root = crate::workspace::registered_project_root(app, &project).ok();
    let identity = if include_identity {
        root.as_deref().and_then(git_identity_value)
    } else {
        None
    };
    let mut result = git_refresh_value(&project, &status, root.as_deref(), identity);
    result["branchComparison"] = if include_branch_comparison {
        root.as_deref()
            .and_then(|root| git_branch_comparison_value(root).ok())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(result)
}

/// 将已读取的 Git 状态编码为 `IGitService.refresh` 的稳定 JSON DTO。
/// 独立于 AppHandle 便于用真实临时仓库的 porcelain 结果做契约测试。
fn git_branch_comparison_value(root: &Path) -> Result<Value, String> {
    let base = git_branch_base(root).ok_or_else(|| "无法确定 Git 分支比较基线".to_owned())?;
    let head = git_output_trimmed(root, &["rev-parse", "--verify", "HEAD"])
        .ok_or_else(|| "无法读取 Git 当前提交".to_owned())?;
    let output = crate::workspace::run_git(
        root,
        &["diff", "--name-status", "-z", "--no-renames", &base, "HEAD"],
    )?;
    if !output.status.success() {
        return Err(redacted_git_error(&output));
    }
    let raw =
        String::from_utf8(output.stdout).map_err(|_| "Git 分支比较返回非 UTF-8 数据".to_owned())?;
    let stats = git_numstat_range(root, &base)?;
    let fields = raw
        .split('\0')
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index + 1 < fields.len() {
        let status = fields[index];
        let path = fields[index + 1].replace('\\', "/");
        index += 2;
        let kind = match status.chars().next().unwrap_or('M') {
            'A' => "added",
            'D' => "deleted",
            'R' => "renamed",
            _ => "modified",
        };
        let absolute_path = crate::path_utils::path_to_frontend(&root.join(&path));
        let (added, removed) = stats.get(&path).copied().unwrap_or((0, 0));
        changes.push(json!({
            "path": absolute_path,
            "repoRelativePath": path,
            "workspaceRelativePath": path,
            "kind": kind,
            "section": "branch",
            "added": added,
            "removed": removed,
            "isStaged": false,
            "isUntracked": false,
            "isConflicted": false,
        }));
    }
    Ok(json!({
        "baseRef": base,
        "headRef": head,
        "comparisonLabel": format!("{base}..{head}"),
        "changes": changes,
    }))
}

fn git_branch_base(root: &Path) -> Option<String> {
    let branch = git_output_trimmed(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let configured_key = format!("branch.{branch}.gh-merge-base");
    let configured = git_output_trimmed(root, &["config", "--get", &configured_key]);
    let upstream = git_output_trimmed(root, &["rev-parse", "--verify", "@{upstream}"]);
    let default = git_output_trimmed(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    );
    let mut candidates = configured
        .into_iter()
        .chain(upstream)
        .chain(default)
        .chain(["refs/heads/main".to_owned(), "refs/heads/master".to_owned()]);
    candidates.find_map(|candidate| git_output_trimmed(root, &["merge-base", &candidate, "HEAD"]))
}

fn git_numstat_range(root: &Path, base: &str) -> Result<BTreeMap<String, (u64, u64)>, String> {
    let output = crate::workspace::run_git(
        root,
        &["diff", "--numstat", "-z", "--no-renames", base, "HEAD"],
    )?;
    if !output.status.success() {
        return Err(redacted_git_error(&output));
    }
    let raw =
        String::from_utf8(output.stdout).map_err(|_| "Git 分支统计返回非 UTF-8 数据".to_owned())?;
    Ok(raw
        .split('\0')
        .filter_map(|entry| {
            let mut fields = entry.split('\t');
            let added = fields.next()?.parse::<u64>().ok()?;
            let removed = fields.next()?.parse::<u64>().ok()?;
            let path = fields.next()?.replace('\\', "/");
            Some((path, (added, removed)))
        })
        .collect())
}

fn git_refresh_value(
    project: &str,
    status: &crate::workspace::GitStatusResult,
    root: Option<&Path>,
    identity: Option<Value>,
) -> Value {
    let (tracking_branch, ahead, behind) = root.map(git_tracking_summary).unwrap_or((None, 0, 0));
    let staged_stats = root.map(|root| git_numstat(root, true)).unwrap_or_default();
    let unstaged_stats = root
        .map(|root| git_numstat(root, false))
        .unwrap_or_default();
    let mut staged_changes = Vec::new();
    let mut unstaged_changes = Vec::new();
    for entry in &status.files {
        let index_changed = entry.index_status != " " && entry.index_status != "?";
        let worktree_changed = entry.worktree_status != " " && entry.worktree_status != "!";
        let untracked = entry.kind == "untracked";
        if index_changed {
            staged_changes.push(git_change_value(entry, true, staged_stats.get(&entry.path)));
        }
        if worktree_changed
            || untracked
            || (!index_changed && !worktree_changed && entry.kind == "conflict")
        {
            unstaged_changes.push(git_change_value(
                entry,
                false,
                unstaged_stats.get(&entry.path),
            ));
        }
    }
    let branch_name = status.branch.clone();
    let summary = json!({
        "workspacePath": project,
        "repoRoot": root.map(crate::path_utils::path_to_frontend).unwrap_or_default(),
        "workspaceInRepoPath": ".",
        "autoRefreshWatchPaths": if status.available {
            root.map(|path| vec![json!({"path": crate::path_utils::path_to_frontend(path), "recursive": true})]).unwrap_or_default()
        } else {
            Vec::<Value>::new()
        },
        "branchName": branch_name,
        "trackingBranchName": tracking_branch,
        "headRefType": if status.branch.is_some() { "branch" } else { "detached" },
        "ahead": ahead,
        "behind": behind,
        "isDirty": !status.files.is_empty(),
        "isGitAvailable": status.available,
        "isRepository": status.available,
    });
    json!({
        "summary": summary,
        "identity": identity,
        "unstagedChanges": unstaged_changes,
        "stagedChanges": staged_changes,
        "branchComparison": Value::Null,
    })
}

fn git_tracking_summary(root: &Path) -> (Option<String>, u64, u64) {
    let tracking = git_output_trimmed(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    );
    let Some(tracking) = tracking else {
        return (None, 0, 0);
    };
    let counts = git_output_trimmed(
        root,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    );
    let mut values = counts
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|value| value.parse::<u64>().ok());
    (
        Some(tracking),
        values.next().unwrap_or(0),
        values.next().unwrap_or(0),
    )
}

fn git_change_value(
    entry: &crate::workspace::GitStatusEntry,
    staged: bool,
    stats: Option<&(u64, u64)>,
) -> Value {
    let kind = match entry.kind.as_str() {
        "added" | "untracked" => "added",
        "deleted" => "deleted",
        "renamed" => "renamed",
        _ => "modified",
    };
    let section = if entry.kind == "conflict" {
        "conflicted"
    } else if staged {
        "staged"
    } else if entry.kind == "untracked" {
        "untracked"
    } else {
        "unstaged"
    };
    json!({
        "path": entry.absolute_path,
        "repoRelativePath": entry.path,
        "workspaceRelativePath": entry.path,
        "kind": kind,
        "section": section,
        "added": stats.map(|value| value.0).unwrap_or(0),
        "removed": stats.map(|value| value.1).unwrap_or(0),
        "isStaged": staged,
        "isUntracked": entry.kind == "untracked",
        "isConflicted": entry.kind == "conflict",
    })
}

fn git_numstat(root: &Path, staged: bool) -> BTreeMap<String, (u64, u64)> {
    let args: &[&str] = if staged {
        &["diff", "--cached", "--numstat", "--no-renames"]
    } else {
        &["diff", "--numstat", "--no-renames"]
    };
    let Ok(output) = crate::workspace::run_git(root, args) else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let added = fields.next()?.parse::<u64>().ok()?;
            let removed = fields.next()?.parse::<u64>().ok()?;
            let path = fields.next()?.replace('\\', "/");
            Some((path, (added, removed)))
        })
        .collect()
}

fn git_identity_value(root: &Path) -> Option<Value> {
    let read = |key: &str| {
        let output = crate::workspace::run_git(root, &["config", "--get", key]).ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!value.is_empty()).then_some(value)
    };
    let name = read("user.name");
    let email = read("user.email");
    if name.is_none() && email.is_none() {
        return None;
    }
    Some(json!({
        "userName": name,
        "userEmail": email,
        "nameSource": name.as_ref().map(|_| "git-config"),
        "emailSource": email.as_ref().map(|_| "git-config"),
        "scopeLabel": "local",
    }))
}

fn validate_git_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 4096
        || path.starts_with('-')
        || path.contains('\0')
        || path.chars().any(char::is_control)
    {
        return Err("Git 路径无效".to_owned());
    }
    Ok(())
}

fn redacted_git_error(output: &std::process::Output) -> String {
    let error = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    keencode_model::redact_error_secrets(&String::from_utf8_lossy(error))
}

async fn checkpoint_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    let project = project_argument(&args)?;
    let root = registered_root(app, &project)?;
    let store = checkpoint_root(app)?;
    match method {
        "createCheckpoint" => {
            let label = optional_string(&args, "label")?.unwrap_or_else(|| "checkpoint".to_owned());
            validate_component(&label, "checkpoint 名称")?;
            let output = crate::workspace::run_git(&root, &["stash", "create", &label])?;
            if !output.status.success() {
                return Err(redacted_git_error(&output));
            }
            let reference = String::from_utf8(output.stdout)
                .map_err(|_| "Git checkpoint 引用不是 UTF-8".to_owned())?
                .trim()
                .to_owned();
            if reference.is_empty() {
                return Err("当前工作区没有可保存的 Git checkpoint".to_owned());
            }
            let id = format!("cp-{}-{}", now_ms(), &reference[..reference.len().min(12)]);
            let record = json!({"id": id, "root": crate::path_utils::path_to_frontend(&root), "reference": reference, "createdAt": now_ms(), "label": label});
            fs::create_dir_all(&store)
                .map_err(|error| format!("无法创建 checkpoint 目录：{error}"))?;
            crate::storage::atomic_write_private(
                &store.join(format!("{id}.json")),
                &serde_json::to_vec(&record).map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("保存 checkpoint 失败：{error}"))?;
            Ok(record)
        }
        "diffCheckpoints" => {
            let left = checkpoint_reference(app, &store, &required_string(&args, "from")?)?;
            let right = checkpoint_reference(app, &store, &required_string(&args, "to")?)?;
            let output =
                crate::workspace::run_git(&root, &["diff", "--no-ext-diff", &left, &right])?;
            if !output.status.success() {
                return Err(redacted_git_error(&output));
            }
            Ok(
                json!({"patch": String::from_utf8(output.stdout).map_err(|_| "checkpoint diff 不是 UTF-8".to_owned())?}),
            )
        }
        "restoreBetweenCheckpoints" => {
            let reference = checkpoint_reference(app, &store, &required_string(&args, "from")?)?;
            let output = crate::workspace::run_git(&root, &["stash", "apply", &reference])?;
            if !output.status.success() {
                return Err(redacted_git_error(&output));
            }
            Ok(json!({"restored": true, "reference": reference}))
        }
        "deleteCheckpoint" => {
            let id = required_string(&args, "checkpointId")?;
            validate_component(&id, "checkpoint 标识")?;
            let path = store.join(format!("{id}.json"));
            if !path.is_file() {
                return Err("checkpoint 不存在".to_owned());
            }
            fs::remove_file(path).map_err(|error| format!("删除 checkpoint 失败：{error}"))?;
            Ok(Value::Null)
        }
        _ => unsupported_method("git-checkpoint", method),
    }
}

fn checkpoint_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("git-checkpoints"))
}

fn checkpoint_reference(app: &AppHandle, store: &Path, id: &str) -> Result<String, String> {
    validate_component(id, "checkpoint 标识")?;
    let path = store.join(format!("{id}.json"));
    let bytes = crate::storage::read_private_bytes_bounded(&path, 64 * 1024, "Git checkpoint")
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "checkpoint 不存在".to_owned())?;
    let record: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("checkpoint 格式无效：{error}"))?;
    let reference = record
        .get("reference")
        .and_then(Value::as_str)
        .ok_or_else(|| "checkpoint 缺少 Git 引用".to_owned())?;
    if reference.len() > 128 || !reference.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("checkpoint Git 引用无效".to_owned());
    }
    let _ = app;
    Ok(reference.to_owned())
}

fn usage_call(app: &AppHandle, method: &str, _args: Value) -> Result<Value, String> {
    let records = crate::analytics::read_records(app)?;
    let mut total_requests = 0_u64;
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut models: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for record in records {
        if !matches!(record.status.as_str(), "success" | "completed") {
            continue;
        }
        total_requests = total_requests.saturating_add(1);
        input_tokens = input_tokens.saturating_add(record.input_tokens);
        output_tokens = output_tokens.saturating_add(record.output_tokens);
        let entry = models.entry(record.model).or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(record.input_tokens);
        entry.2 = entry.2.saturating_add(record.output_tokens);
    }
    match method {
        "getAppUsageSnapshot" | "getSnapshot" => {
            let model_stats = models
                .into_iter()
                .map(|(model, (requests, input, output))| {
                    json!({"model": model, "requests": requests, "inputTokens": input, "outputTokens": output, "totalTokens": input.saturating_add(output)})
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "totalRequests": total_requests,
                "inputTokens": input_tokens,
                "outputTokens": output_tokens,
                "totalTokens": input_tokens.saturating_add(output_tokens),
                "models": model_stats,
                "source": "local-provider-records",
            }))
        }
        "getCodingPlanUsageSnapshot"
        | "getCodingPlanResetStatus"
        | "requestCodingPlanResetOpportunity"
        | "useCodingPlanReset"
        | "markCodingPlanResetHistoryRead"
        | "getEntitlementSnapshot" => unsupported(
            "usage-stats",
            method,
            "official coding-plan entitlement and reset APIs are disabled",
        ),
        _ => unsupported_method("usage-stats", method),
    }
}

#[derive(Clone, Debug)]
struct SkillEntry {
    name: String,
    description: String,
    body: String,
    path: PathBuf,
    scope: &'static str,
}

fn skill_roots(app: &AppHandle, workspace: &Path) -> Result<Vec<(PathBuf, &'static str)>, String> {
    let data = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    Ok(vec![
        (workspace.join(".agents").join("skills"), "workspace"),
        (workspace.join(".zcode").join("skills"), "workspace"),
        (data.join("skills"), "user"),
    ])
}

fn scan_skills(roots: &[(PathBuf, &'static str)]) -> Result<Vec<SkillEntry>, String> {
    let mut result = Vec::new();
    let mut names = BTreeSet::new();
    for (root, scope) in roots {
        if !root.exists() {
            continue;
        }
        for entry in fs::read_dir(root).map_err(|error| format!("无法读取 Skill 目录：{error}"))?
        {
            let entry = entry.map_err(|error| format!("无法读取 Skill 目录项：{error}"))?;
            let directory = entry.path();
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                continue;
            }
            let file = directory.join("SKILL.md");
            if !file.is_file() {
                continue;
            }
            let body =
                fs::read_to_string(&file).map_err(|error| format!("读取 Skill 失败：{error}"))?;
            if body.len() > 2 * 1024 * 1024 {
                return Err(format!("Skill 超过 2 MB：{}", file.display()));
            }
            let (name, description) = parse_skill_frontmatter(
                &body,
                directory
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("skill"),
            );
            if !names.insert(name.to_ascii_lowercase()) {
                continue;
            }
            result.push(SkillEntry {
                name,
                description,
                body,
                path: file,
                scope,
            });
        }
    }
    result.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok(result)
}

fn parse_skill_frontmatter(content: &str, fallback: &str) -> (String, String) {
    let mut name = fallback.to_owned();
    let mut description = String::new();
    let mut in_frontmatter = false;
    for line in content.lines().take(80) {
        let trimmed = line.trim();
        if trimmed == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            if !value.trim().is_empty() {
                name = value
                    .trim()
                    .trim_matches(|character| character == '"' || character == '\'')
                    .to_owned();
            }
        } else if let Some(value) = trimmed.strip_prefix("description:") {
            description = value
                .trim()
                .trim_matches(|character| character == '"' || character == '\'')
                .to_owned();
        }
    }
    (name, description)
}

/// 将已扫描的 Skill 目录转换成前端服务的稳定列表结构。
///
/// 目录扫描仍由 `scan_skills` 负责，因此这个函数只抽出 `skills_call("list")`
/// 的实际投影逻辑，允许离线测试使用真实文件夹验证 user/workspace scope。
fn skills_list_value(skills: &[SkillEntry], disabled: &BTreeSet<String>) -> Value {
    json!({
        "skills": skills.iter().map(|skill| json!({
            "id": skill.name,
            "name": skill.name,
            "description": skill.description,
            "body": skill.body,
            "path": crate::path_utils::path_to_frontend(&skill.path),
            "scope": skill.scope,
            "enabled": !disabled.contains(&skill.name.to_ascii_lowercase()),
        })).collect::<Vec<_>>(),
        "capability": {"userScopeAvailable": true},
        "diagnostics": [],
    })
}

fn skills_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    let workspace = project_argument(&args)?;
    // Skills 的 user 作用域属于应用数据；无项目草稿仍会把内部 conversation
    // 根作为当前工作区传入。复用 Session 的严格授权入口，允许该唯一内部根，
    // 同时继续要求真实项目必须已登记，不能把普通未登记路径当作项目。
    let workspace = crate::session_commands::authorize_stored_root(app, &workspace)?;
    let roots = skill_roots(app, &workspace)?;
    let skills = scan_skills(&roots)?;
    match method {
        "list" => {
            let disabled = current_disabled_skills(app)?;
            Ok(skills_list_value(&skills, &disabled))
        }
        "setEnabled" => {
            let skill_id = required_string(&args, "skillId")?;
            let enabled = object(&args)?
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| "缺少布尔参数 enabled".to_owned())?;
            let mut disabled = current_disabled_skills(app)?;
            if enabled {
                disabled.remove(&skill_id.to_ascii_lowercase());
            } else {
                disabled.insert(skill_id.to_ascii_lowercase());
            }
            let presentation = crate::ui_presentation::ui_presentation_patch(
                app.clone(),
                "settings".to_owned(),
                None,
                json!({"skills": {"disabled": disabled.into_iter().collect::<Vec<_>>()}}),
                None,
            )
            .map_err(|error| error.to_string())?;
            let _ = presentation;
            Ok(Value::Null)
        }
        "buildPromptContext" => {
            let prompt = required_string(&args, "prompt")?;
            let activated = skills
                .iter()
                .filter(|skill| {
                    prompt
                        .to_ascii_lowercase()
                        .contains(&skill.name.to_ascii_lowercase())
                })
                .map(|skill| skill.name.clone())
                .collect::<Vec<_>>();
            Ok(json!({"prompt": prompt, "activatedSkillNames": activated}))
        }
        "copyToCommon" => {
            let skill_id = required_string(&args, "skillId")?;
            let source = skills
                .iter()
                .find(|skill| skill.name.eq_ignore_ascii_case(&skill_id))
                .ok_or_else(|| "找不到 Skill".to_owned())?;
            let target_root = workspace.join(".agents").join("skills");
            validate_component(&source.name, "Skill 名称")?;
            let target = target_root.join(&source.name);
            copy_skill_directory(source.path.parent().ok_or("Skill 目录缺失")?, &target)?;
            Ok(json!({"newPath": crate::path_utils::path_to_frontend(&target)}))
        }
        "removeFromCommon" | "deleteSkill" => {
            let skill_id = required_string(&args, "skillId")?;
            let source = skills
                .iter()
                .find(|skill| skill.name.eq_ignore_ascii_case(&skill_id))
                .ok_or_else(|| "找不到 Skill".to_owned())?;
            let common = workspace.join(".agents").join("skills");
            let user = crate::storage::root_dir(app)
                .map_err(|error| error.to_string())?
                .join("skills");
            let path = source.path.parent().ok_or("Skill 目录缺失")?;
            let allowed = path.starts_with(&common) || path.starts_with(&user);
            if !allowed {
                return Err("只允许删除本地 user/workspace Skill".to_owned());
            }
            fs::remove_dir_all(path).map_err(|error| format!("删除 Skill 失败：{error}"))?;
            Ok(Value::Null)
        }
        _ => unsupported_method("skills", method),
    }
}

fn current_disabled_skills(app: &AppHandle) -> Result<BTreeSet<String>, String> {
    let presentation = crate::ui_presentation::ui_presentation_get(app.clone())
        .map_err(|error| error.to_string())?;
    let value = serde_json::to_value(presentation).map_err(|error| error.to_string())?;
    Ok(value
        .get("settings")
        .and_then(|value| value.get("skills"))
        .and_then(|value| value.get("disabled"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
        .collect())
}

fn copy_skill_directory(source: &Path, target: &Path) -> Result<(), String> {
    if target.exists() {
        return Err(format!("Skill 目标已存在：{}", target.display()));
    }
    fs::create_dir_all(target).map_err(|error| format!("创建 Skill 目标失败：{error}"))?;
    for entry in fs::read_dir(source).map_err(|error| format!("读取 Skill 目录失败：{error}"))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            copy_skill_directory(&path, &target.join(name))?;
        } else if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            fs::copy(&path, target.join(name))
                .map_err(|error| format!("复制 Skill 文件失败：{error}"))?;
        }
    }
    Ok(())
}

fn plugin_manager(app: &AppHandle) -> Result<crate::plugins::PluginManager, String> {
    let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    Ok(crate::plugins::PluginManager::new(root))
}

fn plugin_overview(app: &AppHandle) -> Result<Value, String> {
    let installed = installed_plugin_summaries(app)?;
    let available_plugins = marketplace_available_plugins(app)?;
    let mut marketplaces = Vec::new();
    for record in load_marketplace_records(app)? {
        let (root, manifest) = load_marketplace_record(&record)?;
        let source = record
            .get("path")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| crate::path_utils::path_to_frontend(&root));
        marketplaces.push(json!({
            "id": manifest.name.clone(),
            "name": manifest.name,
            "source": source,
            "pluginCount": manifest.plugins.len(),
            "isOfficial": false,
        }));
    }
    Ok(json!({
        "marketplaces": marketplaces,
        "availablePlugins": available_plugins,
        "installedPlugins": installed,
        "capability": {"supported": true},
    }))
}

fn load_marketplace_record(
    record: &Value,
) -> Result<(PathBuf, crate::plugins::MarketplaceManifest), String> {
    let source = record
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "插件市场记录缺少 path".to_owned())?;
    let source = PathBuf::from(source);
    let manifest_path = record
        .get("manifestPath")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if source.is_file() {
                source.clone()
            } else if source
                .join(".claude-plugin")
                .join("marketplace.json")
                .is_file()
            {
                source.join(".claude-plugin").join("marketplace.json")
            } else {
                source.join("marketplace.json")
            }
        });
    let bytes = fs::read(&manifest_path)
        .map_err(|error| format!("读取插件市场清单失败 {}：{error}", manifest_path.display()))?;
    let manifest =
        crate::plugins::parse_marketplace_manifest(&bytes).map_err(|error| error.to_string())?;
    let root = if manifest_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some(".claude-plugin")
    {
        manifest_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| "插件市场清单缺少根目录".to_owned())?
            .to_path_buf()
    } else {
        manifest_path
            .parent()
            .ok_or_else(|| "插件市场清单缺少根目录".to_owned())?
            .to_path_buf()
    };
    Ok((fs::canonicalize(root).unwrap_or(source), manifest))
}

fn load_marketplace_records(app: &AppHandle) -> Result<Vec<Value>, String> {
    let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    let path = root.join("marketplaces.json");
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(&path, 8 * 1024 * 1024, "插件市场清单")
            .map_err(|error| error.to_string())?
    else {
        return Ok(Vec::new());
    };
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("插件市场清单格式无效：{error}"))?;
    let sources = value
        .get("sources")
        .ok_or_else(|| "插件市场清单缺少 sources 字段".to_owned())?
        .as_array()
        .ok_or_else(|| "插件市场清单 sources 必须是数组".to_owned())?;
    Ok(sources.clone())
}

async fn plugins_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    validate_plugin_workspace(app, &args)?;
    match method {
        "getOverview" => plugin_overview(app),
        "setPluginEnabled" => set_plugin_enabled(app, args).await,
        "addMarketplace" => add_plugin_marketplace(app, args).await,
        "removeMarketplace" => remove_plugin_marketplace(app, args),
        "updateMarketplace" => update_plugin_marketplace(app, args),
        "installPlugin" => install_plugin(app, args).await.map(|_| Value::Null),
        "uninstallPlugin" => uninstall_plugin(app, args).await.map(|_| Value::Null),
        _ => unsupported_method("plugins", method),
    }
}

async fn plugin_management_call(
    app: &AppHandle,
    method: &str,
    args: Value,
) -> Result<Value, String> {
    validate_plugin_workspace(app, &args)?;
    match method {
        "listPlugins" => plugin_management_list(app),
        "getPluginsOverview" => plugin_management_overview(app),
        "setPluginEnabled" => set_plugin_enabled(app, args).await,
        "addPluginMarketplace" => add_plugin_marketplace(app, args).await,
        "removePluginMarketplace" => remove_plugin_marketplace(app, args),
        "updateMarketplace" | "updatePluginMarketplace" => update_plugin_marketplace(app, args),
        "installPlugin" => install_plugin(app, args).await,
        "uninstallPlugin" => uninstall_plugin(app, args).await,
        "updatePlugin" => update_plugin(app, args).await,
        "configurePlugin" => configure_plugin(app, args).await,
        "resetPluginConfig" => reset_plugin_config(app, args).await,
        "restoreBuiltinPlugin" => restore_builtin_plugin(app, args).await,
        "cancelPluginOperation" => cancel_plugin_operation(args),
        "validatePlugin" => validate_plugin(app, args),
        "describePlugin" => describe_plugin_result(app, args),
        "getPluginReferenceCatalog" => plugin_reference_catalog(app, args).await,
        "resolveSuggestedPluginReference" => resolve_suggested_plugin_reference(app, args),
        _ => unsupported_method("plugin-management", method),
    }
}

fn validate_plugin_workspace(app: &AppHandle, args: &Value) -> Result<(), String> {
    if let Some(workspace_path) = object_string(args, "workspacePath")? {
        resolve_resource_workspace_root(app, &workspace_path)?;
    }
    Ok(())
}

fn plugin_source_arg(args: &Value) -> Result<String, String> {
    if let Some(source) = object_string(args, "source")? {
        return Ok(source);
    }
    let plugin_name = required_string(args, "pluginName")?;
    let marketplace = required_string(args, "marketplace")?;
    if Path::new(&plugin_name).exists() {
        return Ok(plugin_name);
    }
    crate::plugins::PluginId::from_components(&plugin_name, Some(&marketplace))
        .map(|id| id.to_string())
        .map_err(|error| error.to_string())
}

fn plugin_target_arg(args: &Value) -> Result<String, String> {
    if let Some(raw) = object_string(args, "pluginId")? {
        return Ok(raw);
    }
    if let Some(raw) = object_string(args, "name")? {
        return Ok(raw);
    }
    let plugin_name = required_string(args, "pluginName")?;
    let Some(marketplace) = object_string(args, "marketplace")? else {
        return Ok(plugin_name);
    };
    crate::plugins::PluginId::from_components(&plugin_name, Some(&marketplace))
        .map(|id| id.to_string())
        .map_err(|error| error.to_string())
}

// 当前 PluginManager 只暴露用户级原子状态；拒绝 workspace scope，避免把项目配置
// 静默写进 user 数据根后让 UI 误以为项目级配置已生效。
fn require_user_plugin_scope(args: &Value) -> Result<(), String> {
    if let Some(scope) = object_string(args, "scope")?
        && scope != "user"
    {
        return Err(
            "当前本地插件管理器只支持 user 作用域；workspace 插件配置需要独立的项目配置事务"
                .to_owned(),
        );
    }
    Ok(())
}

fn emit_plugin_operation_progress(app: &AppHandle, args: &Value) {
    let Ok(Some(operation_id)) = object_string(args, "operationId") else {
        return;
    };
    let _ = app.emit(
        &format!("keencode://plugin-management/{operation_id}"),
        json!({"operationId": operation_id, "state": "refreshing"}),
    );
}

fn plugin_operation_store() -> &'static Mutex<HashMap<String, CancellationToken>> {
    PLUGIN_OPERATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn begin_plugin_operation(args: &Value) -> Result<Option<(String, CancellationToken)>, String> {
    let Some(operation_id) = object_string(args, "operationId")? else {
        return Ok(None);
    };
    if operation_id.len() > 256 || operation_id.chars().any(char::is_control) {
        return Err("operationId 无效".to_owned());
    }
    let token = CancellationToken::new();
    let mut operations = plugin_operation_store()
        .lock()
        .map_err(|_| "插件 operationId 锁已损坏".to_owned())?;
    if operations.contains_key(&operation_id) {
        return Err(format!("插件 operationId 已在执行：{operation_id}"));
    }
    operations.insert(operation_id.clone(), token.clone());
    Ok(Some((operation_id, token)))
}

fn finish_plugin_operation(operation: Option<&(String, CancellationToken)>) {
    let Some((operation_id, _)) = operation else {
        return;
    };
    if let Ok(mut operations) = plugin_operation_store().lock() {
        operations.remove(operation_id);
    }
}

/// 让可取消服务事务共享 operationId 生命周期；无 operationId 时保持同步完成语义。
async fn run_plugin_operation<T, F, Fut>(args: &Value, run: F) -> Result<T, String>
where
    F: FnOnce(Option<CancellationToken>) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let operation = begin_plugin_operation(args)?;
    let token = operation.as_ref().map(|(_, token)| token.clone());
    let result = if let Some(token) = token {
        let cancellation = token.clone();
        tokio::select! {
            _ = cancellation.cancelled() => Err("插件操作已取消".to_owned()),
            result = run(Some(token)) => result,
        }
    } else {
        run(None).await
    };
    finish_plugin_operation(operation.as_ref());
    result
}

async fn add_plugin_marketplace(app: &AppHandle, args: Value) -> Result<Value, String> {
    if object(&args)?.get("dryRun").and_then(Value::as_bool) == Some(true) {
        return Err("本地插件市场 dryRun 需要来源校验事务，当前不执行模拟成功".to_owned());
    }
    emit_plugin_operation_progress(app, &args);
    let source = required_string(&args, "source")?;
    let app_for_operation = app.clone();
    run_plugin_operation(&args, move |cancellation| async move {
        crate::extensions::marketplace_add_with_cancellation(
            source,
            app_for_operation,
            cancellation,
        )
        .await
    })
    .await?;
    Ok(json!({"marketplaces": marketplace_summaries(app)?}))
}

fn remove_plugin_marketplace(app: &AppHandle, args: Value) -> Result<Value, String> {
    let marketplace = required_string(&args, "marketplace")?;
    let state = app.state::<crate::extensions::ExtensionsState>();
    crate::extensions::marketplace_remove(marketplace, app.clone(), state)?;
    Ok(json!({"marketplaces": marketplace_summaries(app)?}))
}

fn update_plugin_marketplace(app: &AppHandle, args: Value) -> Result<Value, String> {
    emit_plugin_operation_progress(app, &args);
    let marketplace = object_string(&args, "marketplace")?;
    let state = app.state::<crate::extensions::ExtensionsState>();
    crate::extensions::marketplace_update(marketplace, app.clone(), state)?;
    Ok(json!({"marketplaces": marketplace_summaries(app)?}))
}

async fn set_plugin_enabled(app: &AppHandle, args: Value) -> Result<Value, String> {
    require_user_plugin_scope(&args)?;
    emit_plugin_operation_progress(app, &args);
    let plugin_id = plugin_target_arg(&args)?;
    let enabled = object(&args)?
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "缺少布尔参数 enabled".to_owned())?;
    if enabled {
        let state = app.state::<crate::extensions::ExtensionsState>();
        let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
        crate::extensions::plugin_enable(plugin_id.clone(), app.clone(), state, runtime).await?;
    } else {
        let state = app.state::<crate::extensions::ExtensionsState>();
        let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
        crate::extensions::plugin_disable(plugin_id.clone(), app.clone(), state, runtime).await?;
    }
    let plugin = plugin_info_for_id(app, &plugin_id)?;
    Ok(json!({"plugin": plugin, "enabled": enabled}))
}

async fn install_plugin(app: &AppHandle, args: Value) -> Result<Value, String> {
    require_user_plugin_scope(&args)?;
    if object(&args)?.get("dryRun").and_then(Value::as_bool) == Some(true) {
        return Err("本地插件安装 dryRun 需要来源物化验证器，当前不执行模拟成功".to_owned());
    }
    emit_plugin_operation_progress(app, &args);
    let source = plugin_source_arg(&args)?;
    let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
    let app_for_operation = app.clone();
    let runtime_for_operation = runtime.inner().clone();
    run_plugin_operation(&args, move |cancellation| async move {
        crate::extensions::plugin_install_with_cancellation(
            source,
            app_for_operation,
            &runtime_for_operation,
            cancellation,
        )
        .await
    })
    .await?;
    plugin_install_result(app)
}

async fn uninstall_plugin(app: &AppHandle, args: Value) -> Result<Value, String> {
    if object(&args)?.get("removeCache").and_then(Value::as_bool) == Some(true) {
        return Err(
            "本地插件卸载不会删除来源缓存；removeCache=true 需要独立缓存清理事务".to_owned(),
        );
    }
    let target = plugin_target_arg(&args)?;
    let before = plugin_management_installed_summary(app, &target).ok();
    let state = app.state::<crate::extensions::ExtensionsState>();
    let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
    crate::extensions::plugin_uninstall(target.clone(), app.clone(), state, runtime).await?;
    let mut result = json!({"diagnostics": []});
    if let Some(summary) = before {
        result["removedPlugin"] = summary;
    }
    Ok(result)
}

fn plugin_ids_in_marketplace<'a, I>(ids: I, marketplace: &str) -> Vec<String>
where
    I: IntoIterator<Item = &'a crate::plugins::PluginId>,
{
    ids.into_iter()
        .filter(|id| {
            id.marketplace
                .as_deref()
                .is_some_and(|installed| installed.eq_ignore_ascii_case(marketplace))
        })
        .map(ToString::to_string)
        .collect()
}

async fn update_plugin(app: &AppHandle, args: Value) -> Result<Value, String> {
    let target = object_string(&args, "pluginId")?;
    let marketplace = object_string(&args, "marketplace")?;
    emit_plugin_operation_progress(app, &args);
    let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
    let app_for_operation = app.clone();
    let runtime_for_operation = runtime.inner().clone();
    run_plugin_operation(&args, move |cancellation| async move {
        if let Some(target) = target {
            let (_, record) = resolve_plugin_record(&app_for_operation, &target)?;
            if let Some(marketplace) = marketplace.as_deref()
                && !record
                    .id
                    .marketplace
                    .as_deref()
                    .is_some_and(|installed| installed.eq_ignore_ascii_case(marketplace))
            {
                return Err(format!("插件 {target} 不属于目标市场 {marketplace}"));
            }
            crate::extensions::plugin_update_with_cancellation(
                Some(record.id.to_string()),
                app_for_operation.clone(),
                &runtime_for_operation,
                cancellation,
            )
            .await?;
        } else if let Some(marketplace) = marketplace {
            let state = plugin_manager(&app_for_operation)?
                .load_state()
                .map_err(|error| error.to_string())?;
            let targets = plugin_ids_in_marketplace(
                state.plugins.iter().map(|record| &record.id),
                &marketplace,
            );
            if targets.is_empty() {
                return Err(format!("找不到市场 {marketplace} 中已安装的插件"));
            }
            // 扩展层的更新事务一次只接受一个插件；按受控状态逐个提交，保持
            // marketplace 过滤语义，同时避免把其它市场插件带入“更新全部”。
            for target in targets {
                crate::extensions::plugin_update_with_cancellation(
                    Some(target),
                    app_for_operation.clone(),
                    &runtime_for_operation,
                    cancellation.clone(),
                )
                .await?;
            }
        } else {
            crate::extensions::plugin_update_with_cancellation(
                None,
                app_for_operation,
                &runtime_for_operation,
                cancellation,
            )
            .await?;
        }
        Ok(())
    })
    .await?;
    plugin_install_result(app)
}

async fn configure_plugin(app: &AppHandle, args: Value) -> Result<Value, String> {
    require_user_plugin_scope(&args)?;
    if args.get("dryRun").and_then(Value::as_bool) == Some(true) {
        return Err("本地插件配置 dryRun 需要独立的校验事务，当前不执行模拟成功".to_owned());
    }
    let plugin_id = plugin_target_arg(&args)?;
    let mut values = args
        .get("options")
        .ok_or_else(|| "缺少对象参数 options".to_owned())
        .and_then(|value| {
            serde_json::from_value::<BTreeMap<String, Value>>(value.clone())
                .map_err(|error| format!("插件 options 必须是 JSON 对象：{error}"))
        })?;
    if let Some(clear) = args.get("clearOptionKeys") {
        let keys = clear
            .as_array()
            .ok_or_else(|| "clearOptionKeys 必须是数组".to_owned())?;
        for key in keys {
            let key = key
                .as_str()
                .filter(|key| !key.is_empty())
                .ok_or_else(|| "clearOptionKeys 只能包含非空字符串".to_owned())?;
            values.remove(key);
        }
    }
    let state = app.state::<crate::extensions::ExtensionsState>();
    let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
    crate::extensions::plugin_user_config_set(
        plugin_id.clone(),
        values,
        Some(false),
        app.clone(),
        state,
        runtime,
    )
    .await?;
    Ok(json!({"pluginId": plugin_id, "diagnostics": []}))
}

async fn reset_plugin_config(app: &AppHandle, args: Value) -> Result<Value, String> {
    require_user_plugin_scope(&args)?;
    let plugin_id = plugin_target_arg(&args)?;
    let state = app.state::<crate::extensions::ExtensionsState>();
    let current = crate::extensions::plugin_user_config_get(plugin_id.clone(), app.clone(), state)?;
    let values = current
        .fields
        .into_iter()
        .filter_map(|field| field.default.map(|value| (field.name, value)))
        .collect::<BTreeMap<_, _>>();
    let state = app.state::<crate::extensions::ExtensionsState>();
    let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
    crate::extensions::plugin_user_config_set(
        plugin_id.clone(),
        values,
        Some(true),
        app.clone(),
        state,
        runtime,
    )
    .await?;
    Ok(json!({"pluginId": plugin_id, "diagnostics": []}))
}

async fn restore_builtin_plugin(app: &AppHandle, args: Value) -> Result<Value, String> {
    let plugin_id = plugin_target_arg(&args)?;
    let id = crate::plugins::PluginId::parse(&plugin_id).map_err(|error| error.to_string())?;
    let root = crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("plugins")
        .join("builtin")
        .join(&id.plugin);
    let metadata = fs::symlink_metadata(&root)
        .map_err(|error| format!("内置插件 {} 不存在于本地 bundle：{error}", id.plugin))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("内置插件 {} 的本地 bundle 无效", id.plugin));
    }
    let runtime = app.state::<std::sync::Arc<crate::agent_runtime::AgentRuntime>>();
    crate::extensions::plugin_install(root.to_string_lossy().into_owned(), app.clone(), runtime)
        .await?;
    Ok(json!({"pluginId": plugin_id, "diagnostics": []}))
}

fn cancel_plugin_operation(args: Value) -> Result<Value, String> {
    let operation_id = required_string(&args, "operationId")?;
    let token = plugin_operation_store()
        .lock()
        .map_err(|_| "插件 operationId 锁已损坏".to_owned())?
        .remove(&operation_id);
    let cancelled = token.is_some();
    if let Some(token) = token {
        token.cancel();
    }
    Ok(json!({
        "operationId": operation_id,
        "cancelled": cancelled,
    }))
}

pub(crate) fn plugin_reference_catalog_entries(app: &AppHandle) -> Result<Vec<Value>, String> {
    let state = plugin_manager(app)?
        .load_state()
        .map_err(|error| error.to_string())?;
    let mut enabled_ids_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut records = Vec::new();
    for record in state.plugins {
        let manifest = crate::plugins::load_plugin_manifest(&record.install_path)
            .map_err(|error| error.to_string())?;
        let plugin_id = record.id.to_string();
        if record.enabled {
            enabled_ids_by_name
                .entry(manifest.name.clone())
                .or_default()
                .push(plugin_id.clone());
        }
        records.push((record, manifest));
    }
    for ids in enabled_ids_by_name.values_mut() {
        ids.sort();
    }
    let mut entries = Vec::new();
    for (record, manifest) in records {
        let groups = plugin_component_groups(&record.install_path, &manifest)?;
        let skill_qualified_names = component_group_names(&groups, "skill")
            .into_iter()
            .filter_map(|name| {
                name.as_str()
                    .map(|name| format!("{}:{name}", manifest.name))
            })
            .map(Value::String)
            .collect::<Vec<_>>();
        let subagent_names = component_group_names(&groups, "agent")
            .into_iter()
            .filter_map(|name| {
                name.as_str()
                    .map(|name| format!("{}:{name}", manifest.name))
            })
            .map(Value::String)
            .collect::<Vec<_>>();
        let mcp_server_names = if record.enabled {
            component_group_names(&groups, "mcp")
                .into_iter()
                .filter_map(|name| {
                    name.as_str()
                        .map(|name| format!("plugin:{}:{name}", manifest.name))
                })
                .map(Value::String)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let conflicts = if record.enabled {
            enabled_ids_by_name
                .get(&manifest.name)
                .into_iter()
                .flatten()
                .filter(|id| id.as_str() != record.id.to_string())
                .cloned()
                .map(Value::String)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut entry = json!({
            "pluginId": record.id.to_string(),
            "name": manifest.name,
            "marketplace": record.id.marketplace.clone().unwrap_or_else(|| "local".to_owned()),
            "enabled": record.enabled,
            "conflictingPluginIds": conflicts,
            "skillQualifiedNames": skill_qualified_names,
            "mcpServerNames": mcp_server_names,
            "subagentNames": subagent_names,
        });
        if let Some(description) = manifest.description.as_deref() {
            entry["description"] = Value::String(description.to_owned());
        }
        entries.push(entry);
    }
    entries.sort_by(|left, right| left["pluginId"].as_str().cmp(&right["pluginId"].as_str()));
    Ok(entries)
}

async fn plugin_reference_catalog(app: &AppHandle, args: Value) -> Result<Value, String> {
    let workspace_path = required_string(&args, "workspacePath")?;
    let requested_workspace = resolve_resource_workspace_root(app, &workspace_path)?;
    if let Some(session_id) = object_string(&args, "sessionId")? {
        let runtime = app
            .try_state::<Arc<crate::agent_runtime::AgentRuntime>>()
            .ok_or_else(|| "Agent Runtime 尚未初始化，无法读取 Session 插件目录".to_owned())?;
        let (_, session_workspace) =
            crate::session_commands::authorized_metadata(runtime.inner(), app, &session_id)?;
        if requested_workspace != session_workspace {
            return Err("workspacePath 与 Session 权威项目目录不一致".to_owned());
        }
        crate::frontend_rpc::session::prepare_session_reference_catalog(
            app,
            runtime.inner(),
            &session_id,
            &session_workspace,
        )
        .await
        .map_err(|error| error.to_string())?;
        let plugins = runtime
            .session_extension_reference_catalog(&session_id)
            .map_err(|error| format!("读取 Session 插件身份目录失败：{error}"))?
            .ok_or_else(|| "Session 尚未发布冻结插件候选，拒绝回退到实时工作区目录".to_owned())?;
        return Ok(json!({
            "authority": "session",
            "plugins": plugins.plugins,
        }));
    }
    Ok(json!({
        "authority": "workspace",
        "plugins": plugin_reference_catalog_entries(app)?,
    }))
}

fn resolve_suggested_plugin_reference(app: &AppHandle, args: Value) -> Result<Value, String> {
    let stable_id = required_string(&args, "stableId")?;
    let _operation_id = required_string(&args, "operationId")?;
    let id = match crate::plugins::PluginId::parse(&stable_id) {
        Ok(id) if id.marketplace.is_some() => id,
        _ => {
            return Ok(json!({
                "stableId": stable_id,
                "status": "unavailable",
                "diagnostics": [{
                    "code": "plugin_suggested_reference_invalid_id",
                    "message": "推荐插件 stableId 必须是 plugin@marketplace",
                    "severity": "error",
                }],
            }));
        }
    };
    if id.marketplace.as_deref() != Some("zcode-plugins-official") {
        return Ok(json!({
            "stableId": stable_id,
            "status": "unavailable",
            "diagnostics": [{
                "code": "plugin_suggested_reference_untrusted_source",
                "message": "推荐插件不是受信任的本地官方市场来源",
                "severity": "error",
                "pluginId": stable_id,
            }],
        }));
    }
    let entries = plugin_reference_catalog_entries(app)?;
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.get("pluginId").and_then(Value::as_str) == Some(stable_id.as_str()))
    {
        let conflict = entry
            .get("conflictingPluginIds")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty());
        let status = if conflict {
            "conflict"
        } else if entry.get("enabled").and_then(Value::as_bool) == Some(true) {
            "ready"
        } else {
            "disabled"
        };
        let mut result = json!({
            "stableId": stable_id,
            "status": status,
            "marketplace": id.marketplace,
            "pluginName": id.plugin,
            "sourceTrust": "official",
            "diagnostics": [],
        });
        if conflict {
            result["diagnostics"] = json!([{
                "code": "plugin_suggested_reference_conflict",
                "message": "推荐插件存在同名冲突，不能自动引用",
                "severity": "error",
            }]);
        }
        return Ok(result);
    }
    let available = marketplace_available_plugins(app)?;
    if available
        .iter()
        .any(|plugin| plugin.get("id").and_then(Value::as_str) == Some(stable_id.as_str()))
    {
        return Ok(json!({
            "stableId": stable_id,
            "status": "missing",
            "marketplace": id.marketplace,
            "pluginName": id.plugin,
            "sourceTrust": "official",
            "diagnostics": [],
        }));
    }
    Ok(json!({
        "stableId": stable_id,
        "status": "unavailable",
        "diagnostics": [{
            "code": "plugin_suggested_reference_unavailable",
            "message": "本地插件市场没有该推荐插件，禁止使用未经验证的远程目录",
            "severity": "error",
            "pluginId": stable_id,
        }],
    }))
}

fn component_types(inventory: &crate::plugins::PluginInventory) -> Vec<Value> {
    [
        (inventory.agents, "agent"),
        (inventory.commands, "command"),
        (inventory.skills, "skill"),
        (inventory.hooks, "hook"),
        (inventory.mcp, "mcp"),
        (inventory.lsp, "lsp"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(_, name)| Value::String(name.to_owned()))
    .collect()
}

fn resolve_plugin_record(
    app: &AppHandle,
    raw: &str,
) -> Result<
    (
        crate::plugins::PluginManager,
        crate::plugins::InstalledPlugin,
    ),
    String,
> {
    let manager = plugin_manager(app)?;
    let requested = crate::plugins::PluginId::parse(raw).map_err(|error| error.to_string())?;
    let state = manager.load_state().map_err(|error| error.to_string())?;
    let matches = state
        .plugins
        .into_iter()
        .filter(|item| {
            item.id.plugin.eq_ignore_ascii_case(&requested.plugin)
                && requested.marketplace.as_deref().is_none_or(|marketplace| {
                    item.id
                        .marketplace
                        .as_deref()
                        .is_some_and(|installed| installed.eq_ignore_ascii_case(marketplace))
                })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [record] => Ok((manager, record.clone())),
        [] => Err(format!("找不到已安装插件 {raw}")),
        _ => Err(format!("多个市场包含插件 {raw}，请使用 plugin@marketplace")),
    }
}

fn plugin_management_installed_summary(app: &AppHandle, raw: &str) -> Result<Value, String> {
    let (_, record) = resolve_plugin_record(app, raw)?;
    plugin_installed_summary(&record)
}

fn insert_optional_string(object: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn plugin_installed_summary(record: &crate::plugins::InstalledPlugin) -> Result<Value, String> {
    let manifest = crate::plugins::load_plugin_manifest(&record.install_path)
        .map_err(|error| error.to_string())?;
    let inventory = crate::plugins::inspect_plugin_components(&record.install_path, &manifest)
        .map_err(|error| error.to_string())?;
    let mut result = json!({
        "id": record.id.to_string(),
        "name": record.id.plugin,
        "marketplace": record.id.marketplace.clone().unwrap_or_else(|| "local".to_owned()),
        "enabled": record.enabled,
        "scope": "user",
        "installPath": crate::path_utils::path_to_frontend(&record.install_path),
        "componentTypes": component_types(&inventory),
    });
    let object = result
        .as_object_mut()
        .ok_or_else(|| "插件摘要构造失败".to_owned())?;
    insert_optional_string(object, "description", manifest.description.as_deref());
    insert_optional_string(object, "version", manifest.version.as_deref());
    Ok(result)
}

fn plugin_info_from_record(record: &crate::plugins::InstalledPlugin) -> Result<Value, String> {
    let manifest = crate::plugins::load_plugin_manifest(&record.install_path)
        .map_err(|error| error.to_string())?;
    let inventory = crate::plugins::inspect_plugin_components(&record.install_path, &manifest)
        .map_err(|error| error.to_string())?;
    let components = plugin_component_groups(&record.install_path, &manifest)?;
    let mcp_server_names = component_group_names(&components, "mcp");
    let mut result = json!({
        "id": record.id.to_string(),
        "name": record.id.plugin,
        "enabled": record.enabled,
        "source": "local",
        "marketplace": record.id.marketplace.clone().unwrap_or_else(|| "local".to_owned()),
        "skillRootCount": inventory.skills,
        "commandRootCount": inventory.commands,
        "components": components,
        "mcpServerNames": mcp_server_names,
        "rootPath": crate::path_utils::path_to_frontend(&record.install_path),
    });
    let object = result
        .as_object_mut()
        .ok_or_else(|| "插件信息构造失败".to_owned())?;
    insert_optional_string(object, "description", manifest.description.as_deref());
    insert_optional_string(object, "version", manifest.version.as_deref());
    Ok(result)
}

fn plugin_info_for_id(app: &AppHandle, raw: &str) -> Result<Value, String> {
    let (_, record) = resolve_plugin_record(app, raw)?;
    plugin_info_from_record(&record)
}

fn installed_plugin_summaries(app: &AppHandle) -> Result<Vec<Value>, String> {
    let state = plugin_manager(app)?
        .load_state()
        .map_err(|error| error.to_string())?;
    state.plugins.iter().map(plugin_installed_summary).collect()
}

fn plugin_install_result(app: &AppHandle) -> Result<Value, String> {
    let installed = installed_plugin_summaries(app)?;
    let dependency_closure = installed
        .iter()
        .filter_map(|plugin| {
            plugin
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "installedPlugins": installed,
        "dependencyClosure": dependency_closure,
        "diagnostics": [],
    }))
}

fn marketplace_summaries(app: &AppHandle) -> Result<Vec<Value>, String> {
    load_marketplace_records(app)?
        .into_iter()
        .map(|record| {
            let (root, manifest) = load_marketplace_record(&record)?;
            Ok(json!({
                "id": manifest.name,
                "name": manifest.name,
                "source": {
                    "path": record.get("path").cloned().unwrap_or(Value::Null),
                    "manifestPath": record.get("manifestPath").cloned().unwrap_or(Value::Null),
                },
                "pluginCount": manifest.plugins.len(),
                "isOfficial": false,
                "installLocation": crate::path_utils::path_to_frontend(&root),
            }))
        })
        .collect()
}

fn marketplace_available_plugins(app: &AppHandle) -> Result<Vec<Value>, String> {
    let installed = plugin_manager(app)?
        .load_state()
        .map_err(|error| error.to_string())?;
    let installed_ids = installed
        .plugins
        .iter()
        .map(|plugin| plugin.id.to_string().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut available = Vec::new();
    for record in load_marketplace_records(app)? {
        let (root, manifest) = load_marketplace_record(&record)?;
        for plugin in manifest.plugins {
            let id = crate::plugins::PluginId::from_components(&plugin.name, Some(&manifest.name))
                .map_err(|error| error.to_string())?;
            let mut types = Vec::new();
            if let crate::plugins::PluginSource::Relative { path } = &plugin.source {
                let plugin_root = root.join(path);
                let plugin_root = fs::canonicalize(&plugin_root).map_err(|error| {
                    format!(
                        "插件市场 {} 的 {} 路径无效：{error}",
                        manifest.name, plugin.name
                    )
                })?;
                if !path_within(&root, &plugin_root) {
                    return Err(format!(
                        "插件市场 {} 的 {} 路径越界",
                        manifest.name, plugin.name
                    ));
                }
                let plugin_manifest = crate::plugins::load_plugin_manifest(&plugin_root)
                    .map_err(|error| error.to_string())?;
                let inventory =
                    crate::plugins::inspect_plugin_components(&plugin_root, &plugin_manifest)
                        .map_err(|error| error.to_string())?;
                types = component_types(&inventory);
            }
            let mut item = json!({
                "id": id.to_string(),
                "name": plugin.name,
                "marketplace": manifest.name.clone(),
                "installed": installed_ids.contains(&id.to_string().to_ascii_lowercase()),
                "componentTypes": types,
            });
            let object = item
                .as_object_mut()
                .ok_or_else(|| "可用插件摘要构造失败".to_owned())?;
            insert_optional_string(object, "description", plugin.description.as_deref());
            insert_optional_string(object, "version", plugin.version.as_deref());
            available.push(item);
        }
    }
    Ok(available)
}

fn builtin_plugin_summaries(app: &AppHandle) -> Result<Vec<Value>, String> {
    let root = crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("plugins")
        .join("builtin");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("读取本地内置插件目录失败：{error}")),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let manifest = crate::plugins::load_plugin_manifest(&entry.path())
            .map_err(|error| format!("内置插件 {name} 清单无效：{error}"))?;
        let inventory = crate::plugins::inspect_plugin_components(&entry.path(), &manifest)
            .map_err(|error| format!("内置插件 {name} 组件清单无效：{error}"))?;
        let mut item = json!({
            "id": format!("{name}@builtin"),
            "name": manifest.name,
            "marketplace": "builtin",
            "installed": false,
            "componentTypes": component_types(&inventory),
        });
        let object = item
            .as_object_mut()
            .ok_or_else(|| "内置插件摘要构造失败".to_owned())?;
        insert_optional_string(object, "description", manifest.description.as_deref());
        insert_optional_string(object, "version", manifest.version.as_deref());
        result.push(item);
    }
    result.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    Ok(result)
}

fn plugin_management_list(app: &AppHandle) -> Result<Value, String> {
    let state = plugin_manager(app)?
        .load_state()
        .map_err(|error| error.to_string())?;
    let plugins = state
        .plugins
        .iter()
        .map(plugin_info_from_record)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"plugins": plugins, "diagnostics": []}))
}

fn plugin_management_overview(app: &AppHandle) -> Result<Value, String> {
    Ok(json!({
        "marketplaces": marketplace_summaries(app)?,
        "availablePlugins": marketplace_available_plugins(app)?,
        "installedPlugins": installed_plugin_summaries(app)?,
        "restorableBuiltins": builtin_plugin_summaries(app)?,
        "diagnostics": [],
        "capability": {"supported": true},
    }))
}

fn validate_plugin(app: &AppHandle, args: Value) -> Result<Value, String> {
    let source = if let Some(source) = object_string(&args, "source")? {
        source
    } else {
        let target = plugin_target_arg(&args)?;
        let (manager, record) = resolve_plugin_record(app, &target)?;
        let _ = manager;
        record.install_path.to_string_lossy().into_owned()
    };
    let root = fs::canonicalize(&source).map_err(|error| format!("插件来源不存在：{error}"))?;
    let manifest =
        crate::plugins::load_plugin_manifest(&root).map_err(|error| error.to_string())?;
    let inventory = crate::plugins::inspect_plugin_components(&root, &manifest)
        .map_err(|error| error.to_string())?;
    let unsupported = inventory.unsupported_hooks.clone();
    let mut runnable = component_types(&inventory)
        .into_iter()
        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    runnable.sort();
    Ok(json!({
        "ok": unsupported.is_empty(),
        "diagnostics": unsupported.iter().map(|hook| json!({
            "code": "unsupported_hook",
            "message": hook,
            "severity": "warning",
        })).collect::<Vec<_>>(),
        "compatibility": {"runnable": runnable, "diagnosticOnly": [], "unsupported": unsupported},
    }))
}

fn plugin_child_path(root: &Path, relative: &str, label: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("{label} 路径必须是插件根内的相对路径"));
    }
    let path = root.join(relative_path);
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("{label} 路径无效 {}：{error}", path.display()))?;
    if !path_within(root, &canonical) {
        return Err(format!("{label} 路径越出插件根目录"));
    }
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| format!("读取 {label} 失败：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("{label} 不允许使用符号链接"));
    }
    Ok(canonical)
}

fn collect_plugin_markdown(
    root: &Path,
    path: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err(format!("插件组件不允许使用符号链接：{}", path.display()));
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|error| format!("读取插件组件目录失败：{error}"))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort();
        for entry in entries {
            collect_plugin_markdown(root, &entry, output)?;
        }
        return Ok(());
    }
    if metadata.is_file()
        && path.extension().and_then(|value| value.to_str()) == Some("md")
        && path_within(root, path)
    {
        output.push(path.to_owned());
    }
    Ok(())
}

fn plugin_component_files(
    root: &Path,
    declarations: &[String],
    default_directory: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut paths = declarations.to_vec();
    if (paths.is_empty() || default_directory == "skills") && root.join(default_directory).is_dir()
    {
        paths.push(default_directory.to_owned());
    }
    if default_directory == "skills" && paths.is_empty() && root.join("SKILL.md").is_file() {
        paths.push("SKILL.md".to_owned());
    }
    let mut result = Vec::new();
    for relative in paths {
        let path = plugin_child_path(root, &relative, "插件组件")?;
        collect_plugin_markdown(root, &path, &mut result)?;
    }
    result.sort();
    result.dedup();
    if default_directory == "skills" {
        result.retain(|path| path.file_name().is_some_and(|name| name == "SKILL.md"));
    }
    Ok(result)
}

fn plugin_component_name(root: &Path, path: &Path) -> String {
    let file_name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("component");
    if file_name.eq_ignore_ascii_case("skill")
        && let Some(parent) = path.parent().and_then(|parent| parent.file_name())
        && parent != root.file_name().unwrap_or_default()
        && let Some(parent) = parent.to_str().filter(|value| !value.is_empty())
    {
        return parent.to_owned();
    }
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn plugin_component_group(kind: &str, names: Vec<String>) -> Option<Value> {
    if names.is_empty() {
        return None;
    }
    let mut names = names;
    names.sort();
    names.dedup();
    Some(json!({
        "kind": kind,
        "items": names.into_iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
    }))
}

fn component_group_names(groups: &[Value], kind: &str) -> Vec<Value> {
    groups
        .iter()
        .find(|group| group.get("kind").and_then(Value::as_str) == Some(kind))
        .and_then(|group| group.get("items"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(|name| Value::String(name.to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn mcp_names_from_file(root: &Path, relative: &str) -> Result<Vec<String>, String> {
    let path = plugin_child_path(root, relative, "插件 MCP 配置")?;
    if !path.is_file() {
        return Err(format!("插件 MCP 配置不是文件：{}", path.display()));
    }
    let bytes = fs::read(&path).map_err(|error| format!("读取插件 MCP 配置失败：{error}"))?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(format!("插件 MCP 配置过大：{}", path.display()));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("插件 MCP 配置格式无效：{error}"))?;
    let object = value
        .get("mcpServers")
        .and_then(Value::as_object)
        .or_else(|| value.as_object())
        .ok_or_else(|| format!("插件 MCP 配置必须是对象：{}", path.display()))?;
    Ok(object.keys().cloned().collect())
}

fn plugin_component_groups(
    root: &Path,
    manifest: &crate::plugins::PluginManifest,
) -> Result<Vec<Value>, String> {
    let mut groups = Vec::new();
    for (kind, declarations, default_directory) in [
        ("agent", &manifest.agents.paths, "agents"),
        ("command", &manifest.commands.paths, "commands"),
        ("skill", &manifest.skills.paths, "skills"),
    ] {
        let names = plugin_component_files(root, declarations, default_directory)?
            .into_iter()
            .map(|path| plugin_component_name(root, &path))
            .collect::<Vec<_>>();
        if let Some(group) = plugin_component_group(kind, names) {
            groups.push(group);
        }
    }
    if let Some(events) = manifest.hooks.as_ref().and_then(Value::as_object) {
        let names = events
            .keys()
            .filter(|name| !name.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if let Some(group) = plugin_component_group("hook", names) {
            groups.push(group);
        }
    }
    let mut mcp_names = manifest
        .mcp_servers
        .inline
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for relative in &manifest.mcp_servers.files {
        mcp_names.extend(mcp_names_from_file(root, relative)?);
    }
    mcp_names.sort();
    mcp_names.dedup();
    if let Some(group) = plugin_component_group("mcp", mcp_names) {
        groups.push(group);
    }
    Ok(groups)
}

fn resolve_plugin_description_source(
    app: &AppHandle,
    args: &Value,
) -> Result<(PathBuf, crate::plugins::PluginManifest), String> {
    let target = plugin_target_arg(args)?;
    if let Ok((_, record)) = resolve_plugin_record(app, &target) {
        let root = fs::canonicalize(&record.install_path).map_err(|error| error.to_string())?;
        let manifest =
            crate::plugins::load_plugin_manifest(&root).map_err(|error| error.to_string())?;
        return Ok((root, manifest));
    }
    let plugin_name = required_string(args, "pluginName")?;
    let marketplace = required_string(args, "marketplace")?;
    for record in load_marketplace_records(app)? {
        let (root, catalog) = load_marketplace_record(&record)?;
        if !catalog.name.eq_ignore_ascii_case(&marketplace) {
            continue;
        }
        let entry = catalog
            .plugins
            .into_iter()
            .find(|plugin| plugin.name.eq_ignore_ascii_case(&plugin_name))
            .ok_or_else(|| format!("市场 {marketplace} 中找不到插件 {plugin_name}"))?;
        let crate::plugins::PluginSource::Relative { path } = entry.source else {
            return Err(format!(
                "插件 {plugin_name}@{marketplace} 的来源必须先安装后才能描述"
            ));
        };
        let plugin_root = plugin_child_path(&root, &path, "市场插件")?;
        let manifest = crate::plugins::load_plugin_manifest(&plugin_root)
            .map_err(|error| error.to_string())?;
        return Ok((plugin_root, manifest));
    }
    Err(format!("找不到插件 {plugin_name}@{marketplace}"))
}

fn describe_plugin_result(app: &AppHandle, args: Value) -> Result<Value, String> {
    let (root, manifest) = resolve_plugin_description_source(app, &args)?;
    let components = plugin_component_groups(&root, &manifest)?;
    let mut result = json!({"components": components});
    let mut metadata = Map::new();
    if let Some(author) = manifest.author.as_ref() {
        insert_optional_string(&mut metadata, "author", author.name.as_deref());
        insert_optional_string(&mut metadata, "authorUrl", author.url.as_deref());
    }
    insert_optional_string(&mut metadata, "homepage", manifest.homepage.as_deref());
    insert_optional_string(&mut metadata, "version", manifest.version.as_deref());
    if !metadata.is_empty() {
        result["metadata"] = Value::Object(metadata);
    }
    Ok(result)
}

// -----------------------------------------------------------------------------
// Commands / hooks / memory / MCP
// -----------------------------------------------------------------------------

fn command_user_roots(app: &AppHandle) -> Result<Vec<PathBuf>, String> {
    let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    // 用户级 Command 归 KeenCode 数据根所有，避免把兼容协议的 ZCode 路径当成
    // 当前应用的真实配置目录，也使原生隔离运行只触碰测试 data 根。
    Ok(vec![root.join("commands")])
}

fn command_project_root(app: &AppHandle, args: &Value) -> Result<PathBuf, String> {
    let path = object(args)?
        .get("workspacePath")
        .or_else(|| object(args).ok().and_then(|value| value.get("projectPath")))
        .and_then(Value::as_str)
        .ok_or_else(|| "项目级 Command 缺少 workspacePath/projectPath".to_owned())?;
    registered_root(app, path)
}

/// 默认对话仅提供读取上下文；删除/启停用户命令时不把它提升为项目写根。
fn command_mutation_workspace(app: &AppHandle, supplied: &str) -> Result<Option<PathBuf>, String> {
    let registered = registered_root(app, supplied);
    let conversation = existing_conversation_workspace_root(app)?;
    command_write_scope_from_registration(supplied, registered, conversation.as_deref())
}

fn command_write_scope_from_registration(
    supplied: &str,
    registered: Result<PathBuf, String>,
    conversation: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    match registered {
        Ok(root) => Ok(Some(root)),
        Err(error) => {
            if conversation.is_some_and(|root| {
                crate::workspace::conversation_read_root(supplied, root).is_ok()
            }) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

fn command_target_root(app: &AppHandle, args: &Value) -> Result<(PathBuf, &'static str), String> {
    let storage = object(args)?
        .get("storageLevel")
        .and_then(Value::as_str)
        .unwrap_or("user");
    match storage {
        "user" | "global" => Ok((command_user_roots(app)?.remove(0), "global")),
        "project" | "workspace" => Ok((
            command_project_root(app, args)?
                .join(".agents")
                .join("commands"),
            "project",
        )),
        other => Err(format!("未知 Command storageLevel：{other}")),
    }
}

fn command_file_name(raw: &str) -> Result<PathBuf, String> {
    let name = raw.trim().trim_start_matches('/');
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err("Command 名称不能为空、过长或包含控制字符".to_owned());
    }
    let mut relative = PathBuf::new();
    for segment in name.split(['/', '\\', ':']) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("Command 名称包含无效路径片段".to_owned());
        }
        if segment.chars().any(char::is_control) {
            return Err("Command 名称包含控制字符".to_owned());
        }
        relative.push(segment);
    }
    relative.set_extension("md");
    Ok(relative)
}

fn command_enabled_overrides(app: &AppHandle) -> Result<Map<String, Value>, String> {
    let path = crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("command-settings.json");
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(&path, 2 * 1024 * 1024, "Command 配置")
            .map_err(|error| error.to_string())?
    else {
        return Ok(Map::new());
    };
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("Command 配置格式无效：{error}"))?;
    Ok(value
        .get("command")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

fn set_command_enabled(app: &AppHandle, path: &Path, enabled: bool) -> Result<(), String> {
    let config_path = crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("command-settings.json");
    let mut document = if let Some(bytes) =
        crate::storage::read_private_bytes_bounded(&config_path, 2 * 1024 * 1024, "Command 配置")
            .map_err(|error| error.to_string())?
    {
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("Command 配置格式无效：{error}"))?
    } else {
        Value::Object(Map::new())
    };
    let root = document
        .as_object_mut()
        .ok_or_else(|| "Command 配置必须是对象".to_owned())?;
    let commands = root
        .entry("command".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "Command 配置 command 必须是对象".to_owned())?;
    let key = path.to_string_lossy().replace('\\', "/");
    if enabled {
        commands.remove(&key);
    } else {
        commands.insert(key, json!({"enable": false}));
    }
    if commands.is_empty() {
        root.remove("command");
    }
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("Command 配置编码失败：{error}"))?;
    crate::storage::atomic_write_private(&config_path, &bytes)
        .map_err(|error| format!("保存 Command 配置失败：{error}"))
}

fn command_is_enabled(path: &Path, overrides: &Map<String, Value>) -> bool {
    let key = path.to_string_lossy().replace('\\', "/");
    overrides
        .get(&key)
        .and_then(|value| value.get("enable"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn command_frontmatter(content: &str) -> (Option<String>, Option<String>, Option<String>, String) {
    let mut name = None;
    let mut description = None;
    let mut argument_hint = None;
    let mut body_start = 0;
    let mut lines = content.split_inclusive('\n').collect::<Vec<_>>();
    if lines
        .first()
        .is_some_and(|line| line.trim_end_matches(['\r', '\n']) == "---")
    {
        let mut end = None;
        for (index, line) in lines.iter().enumerate().skip(1) {
            if line.trim_end_matches(['\r', '\n']) == "---" {
                end = Some(index);
                break;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some((key, value)) = line.split_once(':') {
                let value = value
                    .trim()
                    .trim_matches(|character| character == '"' || character == '\'');
                match key.trim() {
                    "name" => name = Some(value.to_owned()),
                    "description" => description = Some(value.to_owned()),
                    "argumentHint" | "argument_hint" => argument_hint = Some(value.to_owned()),
                    _ => {}
                }
            }
        }
        if let Some(index) = end {
            body_start = lines.iter().take(index + 1).map(|line| line.len()).sum();
        }
    }
    let body = content
        .get(body_start..)
        .unwrap_or_default()
        .trim_start()
        .to_owned();
    lines.clear();
    (name, description, argument_hint, body)
}

fn command_value(
    path: &Path,
    root: &Path,
    scope: &str,
    overrides: &Map<String, Value>,
) -> Result<Value, String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("读取 Command 失败：{error}"))?;
    if content.len() > 2 * 1024 * 1024 {
        return Err(format!("Command 超过 2 MB：{}", path.display()));
    }
    let (front_name, description, argument_hint, prompt) = command_frontmatter(&content);
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let command_name = front_name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("/{}", relative.strip_suffix(".md").unwrap_or(&relative)));
    let command_name = if command_name.starts_with('/') {
        command_name
    } else {
        format!("/{command_name}")
    };
    let enabled = command_is_enabled(path, overrides);
    // 命令自身的 global scope 与设置目录的 user scope 是两个不同的协议枚举；
    // 前端按 location.scope 筛选，否则已保存的用户命令会被列表隐藏。
    let location_scope = if scope == "global" { "user" } else { scope };
    let location = json!({
        "source": "zcode",
        "scope": location_scope,
        "directoryPath": crate::path_utils::path_to_frontend(root),
    });
    let id = format!("zcodeAgent:zcode:{scope}:{command_name}");
    Ok(json!({
        "id": id,
        "name": command_name,
        "prompt": prompt,
        "content": content,
        "filePath": crate::path_utils::path_to_frontend(path),
        "description": description,
        "argumentHint": argument_hint,
        "source": "user",
        "agentSource": "zcodeAgent",
        "location": location,
        "enabled": enabled,
        "scope": scope,
    }))
}

fn plugin_command_value(
    path: &Path,
    plugin_root: &Path,
    namespace: &str,
    relative_path: &Path,
) -> Result<Value, String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("读取插件 Command 失败：{error}"))?;
    if content.len() > 2 * 1024 * 1024 {
        return Err(format!("插件 Command 超过 2 MB：{}", path.display()));
    }
    let (_front_name, front_description, argument_hint, prompt) = command_frontmatter(&content);
    let name = crate::plugins::plugin_command_namespace(namespace, relative_path);
    let description = front_description
        .filter(|value| !value.trim().is_empty())
        .or_else(|| crate::plugins::plugin_command_description(plugin_root, path))
        .unwrap_or_default();
    let id = format!("pluginCommand:{namespace}:{name}");
    Ok(json!({
        "id": id,
        "name": name,
        "prompt": prompt,
        "content": content,
        "filePath": crate::path_utils::path_to_frontend(path),
        "description": description,
        "argumentHint": argument_hint,
        "source": "plugin",
        "agentSource": namespace,
        "location": {"source": "plugin", "scope": "project", "directoryPath": crate::path_utils::path_to_frontend(plugin_root)},
        "enabled": true,
        "scope": "plugin",
    }))
}

fn collect_command_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取 Command 目录失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|error| format!("读取 Command 目录失败：{error}"))?
    {
        let entry = entry.map_err(|error| format!("读取 Command 目录项失败：{error}"))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_command_files(&path, output)?;
        } else if file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("md")
        {
            if output.len() >= MAX_SEARCH_ENTRIES {
                return Err("Command 数量超过限制".to_owned());
            }
            output.push(path);
        }
    }
    Ok(())
}

fn command_config(args: &Value) -> Result<&Map<String, Value>, String> {
    object(args)?
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| "Command 缺少 config 对象".to_owned())
}

fn command_config_string(
    config: &Map<String, Value>,
    name: &str,
    required: bool,
) -> Result<Option<String>, String> {
    match config.get(name) {
        Some(value) => value
            .as_str()
            .map(|value| value.to_owned())
            .ok_or_else(|| format!("Command config.{name} 必须是字符串"))
            .and_then(|value| {
                if required && value.trim().is_empty() {
                    Err(format!("Command config.{name} 不能为空"))
                } else {
                    Ok(Some(value))
                }
            }),
        None if required => Err(format!("Command 缺少 config.{name}")),
        None => Ok(None),
    }
}

fn write_command_file(
    path: &Path,
    config: &Map<String, Value>,
    previous_body: Option<&str>,
) -> Result<(), String> {
    let name = command_config_string(config, "name", true)?.ok_or("Command 名称不能为空")?;
    let prompt = command_config_string(config, "prompt", true)?.ok_or("Command prompt 不能为空")?;
    if prompt.len() > 2 * 1024 * 1024 {
        return Err("Command prompt 超过 2 MB".to_owned());
    }
    let description = command_config_string(config, "description", false)?.unwrap_or_default();
    let argument_hint = command_config_string(config, "argumentHint", false)?;
    let name_yaml =
        serde_json::to_string(name.trim_start_matches('/')).map_err(|error| error.to_string())?;
    let description_yaml =
        serde_json::to_string(&description).map_err(|error| error.to_string())?;
    let argument_line = argument_hint
        .filter(|value| !value.is_empty())
        .map(|value| {
            format!(
                "argumentHint: {}\n",
                serde_json::to_string(&value).unwrap_or_else(|_| "\"\"".to_owned())
            )
        })
        .unwrap_or_default();
    let body = previous_body.unwrap_or(&prompt);
    let content = format!(
        "---\nname: {name_yaml}\ndescription: {description_yaml}\n{argument_line}---\n\n{body}\n"
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建 Command 目录失败：{error}"))?;
    }
    fs::write(path, content.as_bytes()).map_err(|error| format!("写入 Command 失败：{error}"))
}

fn authorize_command_path(
    app: &AppHandle,
    raw: &str,
    workspace: Option<&Path>,
    allow_missing: bool,
) -> Result<PathBuf, String> {
    let input = PathBuf::from(raw);
    if !input.is_absolute() || raw.chars().any(char::is_control) {
        return Err("Command 文件路径必须是绝对路径且不能包含控制字符".to_owned());
    }
    let canonical = if input.exists() {
        fs::canonicalize(&input).map_err(|error| format!("无法解析 Command 路径：{error}"))?
    } else if allow_missing {
        let parent = input.parent().ok_or("Command 路径缺少父目录")?;
        fs::canonicalize(parent)
            .map_err(|error| format!("无法解析 Command 父目录：{error}"))?
            .join(input.file_name().ok_or("Command 路径缺少文件名")?)
    } else {
        return Err(format!("Command 文件不存在：{}", input.display()));
    };
    let mut allowed_roots = command_user_roots(app)?;
    if let Some(workspace) = workspace {
        allowed_roots.push(workspace.join(".agents").join("commands"));
    }
    let allowed = allowed_roots.iter().any(|root| {
        let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        path_within(&canonical_root, &canonical)
    });
    if !allowed {
        return Err("Command 文件路径不在受支持的 user/workspace Command 目录内".to_owned());
    }
    Ok(canonical)
}

fn commands_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "list" => {
            let overrides = command_enabled_overrides(app)?;
            let mut commands = Vec::new();
            let mut plugin_commands = Vec::new();
            let values = object(&args)?;
            let project = values
                .get("workspacePath")
                .or_else(|| values.get("projectPath"))
                .and_then(Value::as_str)
                .map(|path| resolve_resource_workspace_root(app, path))
                .transpose()?;
            if let Some(project) = project.as_ref() {
                for root in [project.join(".agents").join("commands")] {
                    let mut files = Vec::new();
                    collect_command_files(&root, &mut files)?;
                    for file in files {
                        commands.push(command_value(&file, &root, "project", &overrides)?);
                    }
                }
                let snapshot = crate::extensions::plugin_runtime_snapshot(app, project)?;
                for plugin in snapshot.plugins {
                    let namespace = plugin
                        .id
                        .runtime_namespace()
                        .map_err(|error| error.to_string())?;
                    for file in plugin.commands {
                        plugin_commands.push(plugin_command_value(
                            &file.path,
                            &plugin.root,
                            &namespace,
                            &file.relative_path,
                        )?);
                    }
                }
            }
            for root in command_user_roots(app)? {
                let mut files = Vec::new();
                collect_command_files(&root, &mut files)?;
                for file in files {
                    commands.push(command_value(&file, &root, "global", &overrides)?);
                }
            }
            commands.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
            let user_commands = commands.clone();
            let mut all_commands = commands;
            all_commands.extend(plugin_commands.iter().cloned());
            all_commands.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
            Ok(
                json!({"commands": all_commands, "userCommands": user_commands, "pluginCommands": plugin_commands, "capability": {"userScopeAvailable": true, "pluginCommands": !plugin_commands.is_empty()}}),
            )
        }
        "writeCommandFile" => {
            let config = command_config(&args)?;
            let relative = command_file_name(
                &command_config_string(config, "name", true)?.unwrap_or_default(),
            )?;
            let (root, scope) = command_target_root(app, &args)?;
            let target = root.join(relative);
            if target.exists() {
                return Err(format!("Command 文件已存在：{}", target.display()));
            }
            write_command_file(&target, config, None)?;
            set_command_enabled(app, &target, true)?;
            let overrides = command_enabled_overrides(app)?;
            let command = command_value(&target, &root, scope, &overrides)?;
            Ok(json!({"command": command}))
        }
        "updateCommandFile" => {
            let config = command_config(&args)?;
            let relative = command_file_name(
                &command_config_string(config, "name", true)?.unwrap_or_default(),
            )?;
            let (root, scope) = command_target_root(app, &args)?;
            let old = object(&args)?
                .get("oldFilePath")
                .and_then(Value::as_str)
                .map(|value| authorize_command_path(app, value, Some(&root), false))
                .transpose()?;
            let target = root.join(relative);
            if target != old.clone().unwrap_or_default() && target.exists() {
                return Err(format!("Command 文件已存在：{}", target.display()));
            }
            let previous = old
                .as_deref()
                .map(fs::read_to_string)
                .transpose()
                .map_err(|error| format!("读取旧 Command 失败：{error}"))?;
            if let Some(old) = old.as_ref()
                && old != &target
            {
                fs::remove_file(old).map_err(|error| format!("删除旧 Command 失败：{error}"))?;
            }
            write_command_file(
                &target,
                config,
                previous
                    .as_deref()
                    .map(|content| command_frontmatter(content).3)
                    .as_deref(),
            )?;
            set_command_enabled(app, &target, true)?;
            let overrides = command_enabled_overrides(app)?;
            let command = command_value(&target, &root, scope, &overrides)?;
            Ok(json!({"command": command}))
        }
        "deleteCommandFile" => {
            let raw = required_string(&args, "filePath")?;
            let workspace = object(&args)?
                .get("workspacePath")
                .and_then(Value::as_str)
                .map(|path| command_mutation_workspace(app, path))
                .transpose()?
                .flatten();
            let path = authorize_command_path(app, &raw, workspace.as_deref(), false)?;
            fs::remove_file(&path).map_err(|error| format!("删除 Command 失败：{error}"))?;
            set_command_enabled(app, &path, true)?;
            Ok(Value::Null)
        }
        "setCommandEnabled" => {
            let raw = required_string(&args, "filePath")?;
            let enabled = object(&args)?
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| "缺少布尔参数 enabled".to_owned())?;
            let workspace = object(&args)?
                .get("workspacePath")
                .and_then(Value::as_str)
                .map(|path| command_mutation_workspace(app, path))
                .transpose()?
                .flatten();
            let path = authorize_command_path(app, &raw, workspace.as_deref(), false)?;
            set_command_enabled(app, &path, enabled)?;
            Ok(Value::Null)
        }
        "getPrimaryUserCommandsDirectory" => {
            let path = command_user_roots(app)?.remove(0);
            fs::create_dir_all(&path).map_err(|error| format!("创建 Command 目录失败：{error}"))?;
            Ok(json!({"path": crate::path_utils::path_to_frontend(&path)}))
        }
        _ => unsupported_method("commands", method),
    }
}

fn hook_config_paths(
    app: &AppHandle,
    workspace: &Path,
) -> Result<Vec<(PathBuf, &'static str, &'static str)>, String> {
    let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    Ok(vec![
        (root.join("hooks.json"), "user", "keencode"),
        (
            workspace.join(".agents").join("settings.json"),
            "project",
            "agents",
        ),
        (
            workspace.join(".claude").join("settings.json"),
            "project",
            "claude",
        ),
    ])
}

/// 已注册项目中的一份项目级 Hook 配置及其归一化声明。
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceHookFileInput {
    pub(crate) path: PathBuf,
    pub(crate) hooks: Vec<Value>,
}

/// 当前项目 Hook bundle 的授权快照。
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceHookTrustSnapshot {
    pub(crate) bundle_digest: String,
    pub(crate) declaration_digests: BTreeSet<String>,
    pub(crate) trusted_declaration_digests: BTreeSet<String>,
    pub(crate) trust_store_corrupt: bool,
}

/// 读取项目级 Hook 文件；用户级 `hooks.json` 不进入 workspace trust bundle。
pub(crate) fn workspace_hook_inputs(
    app: &AppHandle,
    workspace: &Path,
) -> Result<Vec<WorkspaceHookFileInput>, String> {
    let mut inputs = Vec::new();
    for (path, scope, _) in hook_config_paths(app, workspace)? {
        if scope != "project" {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("读取项目 Hook 配置元数据失败：{error}")),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("项目 Hook 配置必须是普通文件：{}", path.display()));
        }
        inputs.push(WorkspaceHookFileInput {
            path: path.clone(),
            hooks: load_hooks_file(&path, scope)?,
        });
    }
    Ok(inputs)
}

fn canonical_hook_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_hook_value).collect()),
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key.clone(), canonical_hook_value(value));
            }
            Value::Object(canonical)
        }
        value => value.clone(),
    }
}

fn workspace_hook_relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 计算与 `loadHooks` 返回的归一化声明一致的单 Hook 摘要。
pub(crate) fn workspace_hook_declaration_digest(
    workspace: &Path,
    path: &Path,
    hook: &Value,
) -> String {
    let payload = json!([
        "keencode-workspace-hook-declaration-v1",
        workspace_hook_relative_path(workspace, path),
        canonical_hook_value(hook),
    ]);
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(&payload).expect("Hook 摘要输入可编码"));
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn workspace_hook_bundle_digest(
    workspace: &Path,
    inputs: &[WorkspaceHookFileInput],
) -> (String, BTreeSet<String>) {
    let mut files = Vec::with_capacity(inputs.len());
    let mut declarations = BTreeSet::new();
    for input in inputs {
        let mut hooks = Vec::with_capacity(input.hooks.len());
        for hook in &input.hooks {
            declarations.insert(workspace_hook_declaration_digest(
                workspace,
                &input.path,
                hook,
            ));
            hooks.push(canonical_hook_value(hook));
        }
        files.push(json!([
            workspace_hook_relative_path(workspace, &input.path),
            hooks,
        ]));
    }
    let payload = json!(["keencode-workspace-hook-bundle-v1", files]);
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(&payload).expect("Hook bundle 摘要输入可编码"));
    let digest = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (digest, declarations)
}

#[derive(Clone, Debug)]
struct WorkspaceHookTrustMetadata {
    event: String,
    display_command: String,
    source_path: String,
    source_file_index: usize,
    hook_index: usize,
    matcher: Option<String>,
}

/// 从当前 canonical 输入解析授权记录所需的可见绑定字段。
fn workspace_hook_trust_metadata(
    workspace: &Path,
    inputs: &[WorkspaceHookFileInput],
    declaration_digest: &str,
) -> Option<WorkspaceHookTrustMetadata> {
    for (source_file_index, input) in inputs.iter().enumerate() {
        for (hook_index, hook) in input.hooks.iter().enumerate() {
            if workspace_hook_declaration_digest(workspace, &input.path, hook) != declaration_digest
            {
                continue;
            }
            let object = hook.as_object()?;
            let event = object
                .get("event")
                .and_then(Value::as_str)
                .and_then(canonical_workspace_hook_event)?;
            let display_command = object
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| !command.trim().is_empty())?;
            return Some(WorkspaceHookTrustMetadata {
                event: event.to_owned(),
                display_command: display_command.to_owned(),
                source_path: crate::path_utils::path_to_frontend(&input.path),
                source_file_index,
                hook_index,
                matcher: object
                    .get("matcher")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            });
        }
    }
    None
}

fn workspace_hook_review_item_id(declaration_digest: &str) -> String {
    format!("workspace-hook:{declaration_digest}")
}

/// 为 Hooks Settings 和审核流程返回同一份可校验的 workspace 快照。
pub(crate) fn workspace_hook_snapshot_value(
    workspace: &Path,
    inputs: &[WorkspaceHookFileInput],
    snapshot: &WorkspaceHookTrustSnapshot,
) -> Value {
    let source_files = inputs
        .iter()
        .enumerate()
        .map(|(discovery_order, input)| {
            let source_root_enabled = input
                .hooks
                .first()
                .and_then(|hook| hook.get("configuredState"))
                .and_then(|state| state.get("sourceRootEnabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            json!({
                "canonicalPath": crate::path_utils::path_to_frontend(&input.path),
                "baseDir": crate::path_utils::path_to_frontend(input.path.parent().unwrap_or(Path::new("."))),
                "discoveryOrder": discovery_order,
                "configFileKind": "explicit",
                "explicitProjectConfig": true,
                "editable": true,
                "hooksRoot": {"enabled": source_root_enabled},
            })
        })
        .collect::<Vec<_>>();
    let hooks = inputs
        .iter()
        .enumerate()
        .flat_map(|(source_file_index, input)| {
            input
                .hooks
                .iter()
                .enumerate()
                .filter_map(move |(hook_index, hook)| {
                    let object = hook.as_object()?;
                    let event = object.get("event").and_then(Value::as_str)?;
                    let declaration_digest =
                        workspace_hook_declaration_digest(workspace, &input.path, hook);
                    let timeout_seconds = object
                        .get("timeout")
                        .and_then(Value::as_f64)
                        .filter(|value| value.is_finite() && *value > 0.0 && *value <= 3600.0)
                        .unwrap_or(if event == "UserPromptSubmit" {
                            30.0
                        } else {
                            600.0
                        });
                    let timeout_ms = (timeout_seconds * 1000.0).round() as u64;
                    let configured_state = object.get("configuredState");
                    Some(json!({
                        "reviewItemId": workspace_hook_review_item_id(&declaration_digest),
                        "event": event,
                        "matcherIndex": 0,
                        "hookIndex": hook_index,
                        "sourceFileIndex": source_file_index,
                        "sourceRelativePath": workspace_hook_relative_path(workspace, &input.path),
                        "matcher": object.get("matcher").cloned().unwrap_or(Value::Null),
                        "type": object.get("type").and_then(Value::as_str).unwrap_or("command"),
                        "command": object.get("command").and_then(Value::as_str).unwrap_or_default(),
                        "args": object.get("args").cloned().unwrap_or(Value::Array(Vec::new())),
                        "async": object.get("async").cloned().unwrap_or(Value::Bool(false)),
                        "shell": object.get("shell").cloned().unwrap_or(Value::Null),
                        "statusMessage": object.get("statusMessage").cloned().unwrap_or(Value::Null),
                        "resolvedTimeoutMs": timeout_ms,
                        "resolvedMaxOutputBytes": 1024 * 1024,
                        "sourceRootEnabled": configured_state.and_then(|state| state.get("sourceRootEnabled")).and_then(Value::as_bool).unwrap_or(true),
                        "declarationEnabled": configured_state.and_then(|state| state.get("declarationEnabled")).and_then(Value::as_bool).unwrap_or(true),
                        "runtimeHooksEnabled": configured_state.and_then(|state| state.get("runtimeHooksEnabled")).and_then(Value::as_bool).unwrap_or(true),
                        "configuredEnabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                        "editable": object.get("editable").and_then(Value::as_bool).unwrap_or(true),
                        "declarationDigestAlgorithm": "sha256",
                        "hookDeclarationDigest": declaration_digest,
                    }))
                })
        })
        .collect::<Vec<_>>();
    json!({
        "schemaVersion": 1,
        "workspaceIdentity": workspace_identity(workspace),
        "discoveredAt": chrono::Utc::now().to_rfc3339(),
        "sourceFiles": source_files,
        "hooks": hooks,
        "digestAlgorithm": "sha256",
        "bundleDigest": snapshot.bundle_digest,
    })
}

/// 返回当前项目 Hook 的授权摘要，供 Runtime 在同一份配置上做 admission。
pub(crate) fn workspace_hook_trust_snapshot(
    app: &AppHandle,
    workspace: &Path,
) -> Result<Option<WorkspaceHookTrustSnapshot>, String> {
    let inputs = workspace_hook_inputs(app, workspace)?;
    if inputs.is_empty() {
        return Ok(None);
    }
    Ok(Some(workspace_hook_trust_snapshot_from_inputs(
        app, workspace, &inputs,
    )?))
}

fn workspace_hook_trust_snapshot_from_inputs(
    app: &AppHandle,
    workspace: &Path,
    inputs: &[WorkspaceHookFileInput],
) -> Result<WorkspaceHookTrustSnapshot, String> {
    let (bundle_digest, declaration_digests) = workspace_hook_bundle_digest(workspace, inputs);
    let identity = workspace_identity(workspace);
    let mut trusted_declaration_digests = BTreeSet::new();
    let mut trust_store_corrupt = false;
    if let Ok(store) = read_hook_trust_store(app) {
        if let Some(records) = store.get("records").and_then(Value::as_array) {
            for record in records {
                if record.get("workspaceIdentity").and_then(Value::as_str)
                    != Some(identity.as_str())
                    || record.get("decision").and_then(Value::as_str) != Some("trusted")
                    || record.get("bundleDigestAtGrant").and_then(Value::as_str)
                        != Some(bundle_digest.as_str())
                {
                    continue;
                }
                let Some(digest) = record.get("hookDeclarationDigest").and_then(Value::as_str)
                else {
                    continue;
                };
                if declaration_digests.contains(digest) {
                    trusted_declaration_digests.insert(digest.to_owned());
                }
            }
        } else {
            trust_store_corrupt = true;
        }
    } else {
        trust_store_corrupt = true;
    }
    Ok(WorkspaceHookTrustSnapshot {
        bundle_digest,
        declaration_digests,
        trusted_declaration_digests,
        trust_store_corrupt,
    })
}

/// 给 V4 审核 coordinator 返回当前 Hook authority 的不可变输入和 trust 摘要。
///
/// 审核层不得重新读取或归一化 Hook 文件；快照和 Runtime admission 必须继续共享
/// 这里已有的 bundle/declaration digest 计算与 trust store 解析。
pub(crate) fn workspace_hook_review_snapshot(
    app: &AppHandle,
    workspace: &Path,
) -> Result<Option<(Value, WorkspaceHookTrustSnapshot)>, String> {
    let inputs = workspace_hook_inputs(app, workspace)?;
    if inputs.is_empty() {
        return Ok(None);
    }
    let snapshot = workspace_hook_trust_snapshot_from_inputs(app, workspace, &inputs)?;
    let value = workspace_hook_snapshot_value(workspace, &inputs, &snapshot);
    Ok(Some((value, snapshot)))
}

fn hook_config_value(
    raw: &Value,
    event: &str,
    matcher: Option<&str>,
    path: &Path,
    scope: &str,
    index: usize,
) -> Result<Value, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "Hook 定义必须是对象".to_owned())?;
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "Hook 缺少 command".to_owned())?;
    let hook_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("command");
    if !matches!(hook_type, "command" | "process") {
        return Err(format!("Hook type {hook_type} 尚无宿主执行入口"));
    }
    if let Some(args) = object.get("args") {
        let values = args
            .as_array()
            .ok_or_else(|| "Hook args 必须是字符串数组".to_owned())?;
        if values.len() > 1024
            || values.iter().any(|value| {
                value.as_str().is_none_or(|value| {
                    value.len() > 64 * 1024 || value.chars().any(char::is_control)
                })
            })
        {
            return Err("Hook args 必须是有界字符串数组".to_owned());
        }
    }
    if let Some(timeout) = object.get("timeout")
        && !timeout
            .as_f64()
            .is_some_and(|value| value.is_finite() && value > 0.0 && value <= 3600.0)
    {
        return Err("Hook timeout 必须介于 0 和 3600 秒之间".to_owned());
    }
    if let Some(async_value) = object.get("async")
        && async_value.as_bool().is_none()
    {
        return Err("Hook async 必须是布尔值".to_owned());
    }
    if let Some(shell) = object.get("shell")
        && !(shell.as_bool() == Some(true)
            || shell
                .as_str()
                .is_some_and(|value| matches!(value, "bash" | "powershell")))
    {
        return Err("Hook shell 必须是 true、bash 或 powershell".to_owned());
    }
    if command.trim().is_empty()
        || command.len() > 64 * 1024
        || command.chars().any(char::is_control)
    {
        return Err("Hook command 为空、过长或包含控制字符".to_owned());
    }
    let mut value = Map::new();
    value.insert(
        "id".to_owned(),
        Value::String(format!("{}:{event}:{index}", path.display())),
    );
    value.insert("event".to_owned(), Value::String(event.to_owned()));
    if let Some(matcher) = matcher.filter(|value| !value.is_empty()) {
        value.insert("matcher".to_owned(), Value::String(matcher.to_owned()));
    }
    value.insert(
        "type".to_owned(),
        object
            .get("type")
            .cloned()
            .unwrap_or_else(|| Value::String("command".to_owned())),
    );
    value.insert("command".to_owned(), Value::String(command.to_owned()));
    for key in [
        "args",
        "async",
        "shell",
        "statusMessage",
        "timeout",
        "custom",
        "enabled",
        "editable",
    ] {
        if let Some(item) = object.get(key) {
            value.insert(key.to_owned(), item.clone());
        }
    }
    value
        .entry("enabled".to_owned())
        .or_insert(Value::Bool(true));
    value
        .entry("editable".to_owned())
        .or_insert(Value::Bool(true));
    value.insert(
        "location".to_owned(),
        json!({"source": path.parent().and_then(Path::file_name).and_then(|value| value.to_str()).unwrap_or("zcode"), "scope": scope, "directoryPath": crate::path_utils::path_to_frontend(path.parent().unwrap_or(Path::new(".")))}),
    );
    Ok(Value::Object(value))
}

/// 将工作区 Hook 事件归一到 shared 契约中的七个事件名。
fn canonical_workspace_hook_event(value: &str) -> Option<&'static str> {
    let normalized = value
        .chars()
        .filter(|character| !matches!(character, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect::<String>();
    match normalized.as_str() {
        "sessionstart" => Some("SessionStart"),
        "userpromptsubmit" => Some("UserPromptSubmit"),
        "pretooluse" => Some("PreToolUse"),
        "permissionrequest" => Some("PermissionRequest"),
        "posttooluse" => Some("PostToolUse"),
        "posttoolusefailure" => Some("PostToolUseFailure"),
        "stop" => Some("Stop"),
        _ => None,
    }
}

fn load_hooks_file(path: &Path, scope: &str) -> Result<Vec<Value>, String> {
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(path, 8 * 1024 * 1024, "Hook 配置")
            .map_err(|error| error.to_string())?
    else {
        return Ok(Vec::new());
    };
    let document: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("Hook 配置格式无效：{error}"))?;
    let hook_root = document
        .get("hooks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let root_enabled = hook_root
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let hooks = hook_root
        .get("events")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or(hook_root);
    let mut result = Vec::new();
    for (event, entries) in hooks {
        let event = canonical_workspace_hook_event(&event)
            .ok_or_else(|| format!("Hook 事件 {event} 尚无宿主执行入口"))?;
        let entries = entries
            .as_array()
            .ok_or_else(|| format!("Hook 事件 {event} 必须是数组"))?;
        for (index, entry) in entries.iter().enumerate() {
            let (matcher, definitions): (Option<String>, Vec<Value>) =
                if let Some(object) = entry.as_object() {
                    if let Some(items) = object.get("hooks").and_then(Value::as_array) {
                        (
                            object
                                .get("matcher")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            items.clone(),
                        )
                    } else {
                        (
                            object
                                .get("matcher")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            vec![entry.clone()],
                        )
                    }
                } else {
                    return Err(format!("Hook 事件 {event} 的条目必须是对象"));
                };
            for (offset, definition) in definitions.iter().enumerate() {
                let mut hook = hook_config_value(
                    definition,
                    event,
                    matcher.as_deref(),
                    path,
                    scope,
                    index.saturating_mul(1024).saturating_add(offset),
                )?;
                let declaration_enabled =
                    hook.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                if !root_enabled && let Some(object) = hook.as_object_mut() {
                    object.insert("enabled".to_owned(), Value::Bool(false));
                }
                if let Some(object) = hook.as_object_mut() {
                    let configured_enabled = object
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    object.insert(
                        "configuredState".to_owned(),
                        json!({
                            "sourceRootEnabled": root_enabled,
                            "declarationEnabled": declaration_enabled,
                            "runtimeHooksEnabled": true,
                            "configuredEnabled": configured_enabled,
                            "sourcePath": crate::path_utils::path_to_frontend(path),
                        }),
                    );
                }
                result.push(hook);
            }
        }
    }
    Ok(result)
}

fn hook_to_config(value: &Value) -> Result<(String, Option<String>, Value), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Hook 必须是对象".to_owned())?;
    let event = object
        .get("event")
        .and_then(Value::as_str)
        .ok_or_else(|| "Hook 缺少 event".to_owned())?;
    let allowed_events = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
    ];
    if !allowed_events.contains(&event) {
        return Err(format!("未知 Hook event：{event}"));
    }
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "Hook 缺少 command".to_owned())?;
    if command.trim().is_empty() {
        return Err("Hook command 不能为空".to_owned());
    }
    let mut config = Map::new();
    config.insert(
        "type".to_owned(),
        object
            .get("type")
            .cloned()
            .unwrap_or_else(|| Value::String("command".to_owned())),
    );
    config.insert("command".to_owned(), Value::String(command.to_owned()));
    for key in [
        "args",
        "async",
        "shell",
        "statusMessage",
        "timeout",
        "enabled",
        "custom",
    ] {
        if let Some(item) = object.get(key) {
            config.insert(key.to_owned(), item.clone());
        }
    }
    Ok((
        event.to_owned(),
        object
            .get("matcher")
            .and_then(Value::as_str)
            .map(str::to_owned),
        Value::Object(config),
    ))
}

fn save_hooks_file(path: &Path, values: &[Value], private: bool) -> Result<(), String> {
    let mut events: BTreeMap<String, BTreeMap<Option<String>, Vec<Value>>> = BTreeMap::new();
    for value in values {
        let (event, matcher, config) = hook_to_config(value)?;
        events
            .entry(event)
            .or_default()
            .entry(matcher)
            .or_default()
            .push(config);
    }
    let mut event_values = Map::new();
    for (event, matchers) in events {
        let entries = matchers
            .into_iter()
            .map(|(matcher, hooks)| {
                if let Some(matcher) = matcher {
                    json!({"matcher": matcher, "hooks": hooks})
                } else {
                    json!({"hooks": hooks})
                }
            })
            .collect::<Vec<_>>();
        event_values.insert(event, Value::Array(entries));
    }
    let mut document = if let Some(bytes) =
        crate::storage::read_private_bytes_bounded(path, 8 * 1024 * 1024, "Hook 配置")
            .map_err(|error| error.to_string())?
    {
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("Hook 配置格式无效：{error}"))?
    } else {
        Value::Object(Map::new())
    };
    let root = document
        .as_object_mut()
        .ok_or_else(|| "Hook 配置必须是对象".to_owned())?;
    let hook_root = root
        .entry("hooks".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "Hook 配置 hooks 必须是对象".to_owned())?;
    hook_root.insert(
        "enabled".to_owned(),
        Value::Bool(values.iter().any(|value| {
            value
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })),
    );
    hook_root.insert("events".to_owned(), Value::Object(event_values));
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("Hook 配置编码失败：{error}"))?;
    if private {
        crate::storage::atomic_write_private(path, &bytes)
            .map_err(|error| format!("保存 Hook 配置失败：{error}"))
    } else {
        fs::write(path, bytes).map_err(|error| format!("保存项目 Hook 配置失败：{error}"))
    }
}

const HOOK_TRUST_SCHEMA_VERSION: u64 = 1;

fn hook_trust_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("security")
        .join("workspace-hook-trust-v1.json"))
}

fn validate_hook_digest(value: &str, field: &str) -> Result<String, String> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(format!("{field} 必须是 64 位小写 SHA-256 十六进制摘要"));
    }
    Ok(value.to_owned())
}

fn validate_hook_trust_record(record: &Value) -> Result<(), String> {
    let object = record
        .as_object()
        .ok_or_else(|| "Hook trust record 必须是对象".to_owned())?;
    let allowed = [
        "workspaceIdentity",
        "bundleDigestAtGrant",
        "hookDeclarationDigest",
        "digestAlgorithm",
        "decision",
        "grantedAt",
        "lastUsedAt",
        "eventAtGrant",
        "displayCommandAtGrant",
        "sourcePathAtGrant",
        "sourceDiscoveryOrderAtGrant",
        "matcherAtGrant",
        "matcherIndexAtGrant",
        "hookIndexAtGrant",
        "appVersionAtGrant",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("Hook trust record 含有未知字段".to_owned());
    }
    let workspace_identity = object
        .get("workspaceIdentity")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Hook trust record 缺少 workspaceIdentity".to_owned())?;
    if workspace_identity.chars().any(char::is_control) {
        return Err("Hook trust workspaceIdentity 含有控制字符".to_owned());
    }
    validate_hook_digest(
        object
            .get("bundleDigestAtGrant")
            .and_then(Value::as_str)
            .ok_or_else(|| "Hook trust record 缺少 bundleDigestAtGrant".to_owned())?,
        "bundleDigestAtGrant",
    )?;
    validate_hook_digest(
        object
            .get("hookDeclarationDigest")
            .and_then(Value::as_str)
            .ok_or_else(|| "Hook trust record 缺少 hookDeclarationDigest".to_owned())?,
        "hookDeclarationDigest",
    )?;
    if object.get("digestAlgorithm").and_then(Value::as_str) != Some("sha256") {
        return Err("Hook trust record digestAlgorithm 必须为 sha256".to_owned());
    }
    if object.get("decision").and_then(Value::as_str) != Some("trusted") {
        return Err("Hook trust record decision 必须为 trusted".to_owned());
    }
    let granted_at = object
        .get("grantedAt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Hook trust record 缺少 grantedAt".to_owned())?;
    if chrono::DateTime::parse_from_rfc3339(granted_at).is_err() {
        return Err("Hook trust record grantedAt 必须是 RFC3339 时间".to_owned());
    }
    for field in ["lastUsedAt"] {
        if let Some(value) = object.get(field).and_then(Value::as_str)
            && chrono::DateTime::parse_from_rfc3339(value).is_err()
        {
            return Err(format!("Hook trust record {field} 必须是 RFC3339 时间"));
        }
    }
    let event = object
        .get("eventAtGrant")
        .and_then(Value::as_str)
        .ok_or_else(|| "Hook trust record 缺少 eventAtGrant".to_owned())?;
    if canonical_workspace_hook_event(event) != Some(event) {
        return Err("Hook trust record eventAtGrant 不受支持".to_owned());
    }
    for field in ["displayCommandAtGrant", "sourcePathAtGrant"] {
        let value = object
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("Hook trust record 缺少 {field}"))?;
        if value.chars().any(char::is_control) {
            return Err(format!("Hook trust record {field} 含有控制字符"));
        }
    }
    for field in [
        "sourceDiscoveryOrderAtGrant",
        "matcherIndexAtGrant",
        "hookIndexAtGrant",
    ] {
        if let Some(value) = object.get(field)
            && value.as_u64().is_none()
        {
            return Err(format!("Hook trust record {field} 必须是非负整数"));
        }
    }
    if let Some(value) = object.get("matcherAtGrant")
        && !value.is_null()
        && value.as_str().is_none()
    {
        return Err("Hook trust record matcherAtGrant 必须是字符串或 null".to_owned());
    }
    if let Some(value) = object.get("appVersionAtGrant")
        && value.as_str().is_none_or(|value| value.trim().is_empty())
    {
        return Err("Hook trust record appVersionAtGrant 必须是非空字符串".to_owned());
    }
    Ok(())
}

fn read_hook_trust_store(app: &AppHandle) -> Result<Value, String> {
    let path = hook_trust_path(app)?;
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(&path, 4 * 1024 * 1024, "workspace Hook trust")
            .map_err(|error| error.to_string())?
    else {
        return Ok(json!({
            "schemaVersion": HOOK_TRUST_SCHEMA_VERSION,
            "records": [],
        }));
    };
    let store: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("workspace Hook trust 文件格式无效：{error}"))?;
    let object = store
        .as_object()
        .ok_or_else(|| "workspace Hook trust 文件必须是对象".to_owned())?;
    if object
        .keys()
        .any(|key| !["schemaVersion", "records"].contains(&key.as_str()))
    {
        return Err("workspace Hook trust 文件含有未知字段".to_owned());
    }
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(HOOK_TRUST_SCHEMA_VERSION) {
        return Err("workspace Hook trust 文件版本不受支持".to_owned());
    }
    let records = object
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "workspace Hook trust records 必须是数组".to_owned())?;
    if records.len() > 4096 {
        return Err("workspace Hook trust records 数量超过限制".to_owned());
    }
    let mut identities = BTreeSet::new();
    for record in records {
        validate_hook_trust_record(record)?;
        let identity = record
            .get("workspaceIdentity")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let digest = record
            .get("hookDeclarationDigest")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !identities.insert(format!("{identity}\u{0}{digest}")) {
            return Err("workspace Hook trust records 存在重复声明".to_owned());
        }
    }
    Ok(store)
}

fn write_hook_trust_store(app: &AppHandle, store: &Value) -> Result<(), String> {
    let path = hook_trust_path(app)?;
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("workspace Hook trust 编码失败：{error}"))?;
    crate::storage::atomic_write_private(&path, &bytes)
        .map_err(|error| format!("保存 workspace Hook trust 失败：{error}"))
}

fn workspace_identity(workspace: &Path) -> String {
    crate::path_utils::path_to_frontend(workspace)
}

fn grant_workspace_hook_trust(
    app: &AppHandle,
    workspace: &Path,
    args: &Value,
) -> Result<Value, String> {
    let object = object(args)?;
    let bundle_digest = validate_hook_digest(
        object
            .get("bundleDigest")
            .and_then(Value::as_str)
            .ok_or_else(|| "grantWorkspaceHookTrust 缺少 bundleDigest".to_owned())?,
        "bundleDigest",
    )?;
    let hook_digest = validate_hook_digest(
        object
            .get("hookDeclarationDigest")
            .and_then(Value::as_str)
            .ok_or_else(|| "grantWorkspaceHookTrust 缺少 hookDeclarationDigest".to_owned())?,
        "hookDeclarationDigest",
    )?;
    grant_workspace_hook_trust_batch(app, workspace, &bundle_digest, &[hook_digest])
}

/// 在一次 trust-store 写入中授权一批声明，避免逐行授信导致部分成功。
///
/// V4 `respondWorkspaceHookReview` 一次可能选择多行；所有 digest、当前 bundle、
/// metadata 必须先完成校验，随后才允许唯一一次原子写入。
pub(crate) fn grant_workspace_hook_trust_batch(
    app: &AppHandle,
    workspace: &Path,
    bundle_digest: &str,
    hook_digests: &[String],
) -> Result<Value, String> {
    let bundle_digest = validate_hook_digest(bundle_digest, "bundleDigest")?;
    if hook_digests.is_empty() {
        return Err("workspace Hook trust 至少需要一条声明".to_owned());
    }
    let mut unique_digests = BTreeSet::new();
    for digest in hook_digests {
        unique_digests.insert(validate_hook_digest(digest, "hookDeclarationDigest")?);
    }
    let inputs = workspace_hook_inputs(app, workspace)?;
    if inputs.is_empty() {
        return Ok(json!({
            "accepted": false,
            "reasonCode": "workspace_hooks_snapshot_mismatch",
        }));
    }
    let snapshot = workspace_hook_trust_snapshot_from_inputs(app, workspace, &inputs)?;
    if snapshot.trust_store_corrupt {
        return Ok(json!({
            "accepted": false,
            "reasonCode": "workspace_hooks_trust_store_corrupt",
        }));
    }
    if bundle_digest != snapshot.bundle_digest {
        return Ok(json!({
            "accepted": false,
            "reasonCode": "workspace_hooks_snapshot_mismatch",
            "currentBundleDigest": snapshot.bundle_digest,
        }));
    }
    if unique_digests
        .iter()
        .any(|digest| !snapshot.declaration_digests.contains(digest))
    {
        return Ok(json!({
            "accepted": false,
            "reasonCode": "workspace_hooks_snapshot_mismatch",
            "currentBundleDigest": snapshot.bundle_digest,
        }));
    }
    let metadata = unique_digests
        .iter()
        .map(|digest| {
            workspace_hook_trust_metadata(workspace, &inputs, digest)
                .map(|metadata| (digest.clone(), metadata))
        })
        .collect::<Option<Vec<_>>>();
    let Some(metadata) = metadata else {
        return Ok(json!({
            "accepted": false,
            "reasonCode": "workspace_hooks_snapshot_mismatch",
            "currentBundleDigest": snapshot.bundle_digest,
        }));
    };
    let mut store = match read_hook_trust_store(app) {
        Ok(store) => store,
        Err(error) => {
            return Ok(json!({
                "accepted": false,
                "reasonCode": "workspace_hooks_trust_store_corrupt",
                "error": error,
            }));
        }
    };
    let records = store
        .get_mut("records")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "workspace Hook trust records 必须是数组".to_owned())?;
    let identity = workspace_identity(workspace);
    let now = chrono::Utc::now().to_rfc3339();
    for (hook_digest, metadata) in metadata {
        let record = json!({
            "workspaceIdentity": identity,
            "bundleDigestAtGrant": bundle_digest,
            "hookDeclarationDigest": hook_digest,
            "digestAlgorithm": "sha256",
            "decision": "trusted",
            "grantedAt": now,
            "eventAtGrant": metadata.event,
            "displayCommandAtGrant": metadata.display_command,
            "sourcePathAtGrant": metadata.source_path,
            "sourceDiscoveryOrderAtGrant": metadata.source_file_index,
            "matcherAtGrant": metadata.matcher,
            "matcherIndexAtGrant": 0,
            "hookIndexAtGrant": metadata.hook_index,
        });
        if let Some(existing) = records.iter_mut().find(|existing| {
            existing.get("workspaceIdentity").and_then(Value::as_str) == Some(identity.as_str())
                && existing
                    .get("hookDeclarationDigest")
                    .and_then(Value::as_str)
                    == Some(hook_digest.as_str())
        }) {
            *existing = record;
        } else {
            records.push(record);
        }
    }
    write_hook_trust_store(app, &store)?;
    Ok(json!({"accepted": true, "hookDeclarationDigests": unique_digests}))
}

/// 只撤销当前 bundle 中显式列出的声明，供 V4 revoke command 使用。
pub(crate) fn revoke_workspace_hook_trust_declarations(
    app: &AppHandle,
    workspace: &Path,
    bundle_digest: &str,
    hook_digests: &[String],
) -> Result<Value, String> {
    let bundle_digest = validate_hook_digest(bundle_digest, "bundleDigest")?;
    let mut unique_digests = BTreeSet::new();
    for digest in hook_digests {
        unique_digests.insert(validate_hook_digest(digest, "hookDeclarationDigest")?);
    }
    if unique_digests.is_empty() {
        return Err("workspace Hook revoke 至少需要一条声明".to_owned());
    }
    let inputs = workspace_hook_inputs(app, workspace)?;
    if inputs.is_empty() {
        return Ok(json!({"accepted": false, "reasonCode": "workspace_hooks_snapshot_mismatch"}));
    }
    let snapshot = workspace_hook_trust_snapshot_from_inputs(app, workspace, &inputs)?;
    if snapshot.trust_store_corrupt {
        return Ok(json!({
            "accepted": false,
            "reasonCode": "workspace_hooks_trust_store_corrupt",
        }));
    }
    if snapshot.bundle_digest != bundle_digest
        || unique_digests
            .iter()
            .any(|digest| !snapshot.declaration_digests.contains(digest))
    {
        return Ok(json!({
            "accepted": false,
            "reasonCode": "workspace_hooks_snapshot_mismatch",
            "currentBundleDigest": snapshot.bundle_digest,
        }));
    }
    let mut store = read_hook_trust_store(app)
        .map_err(|error| format!("workspace Hook trust store 无法用于撤销：{error}"))?;
    let records = store
        .get_mut("records")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "workspace Hook trust records 必须是数组".to_owned())?;
    let identity = workspace_identity(workspace);
    let before = records.len();
    records.retain(|record| {
        !(record.get("workspaceIdentity").and_then(Value::as_str) == Some(identity.as_str())
            && record
                .get("hookDeclarationDigest")
                .and_then(Value::as_str)
                .is_some_and(|digest| unique_digests.contains(digest)))
    });
    let changed = records.len() != before;
    if changed {
        write_hook_trust_store(app, &store)?;
    }
    Ok(json!({
        "accepted": true,
        "changed": changed,
        "hookDeclarationDigests": unique_digests,
    }))
}

fn revoke_workspace_hook_trust(app: &AppHandle, workspace: &Path) -> Result<(), String> {
    let mut store = match read_hook_trust_store(app) {
        Ok(store) => store,
        // 损坏的 store 已在加载/授信时 fail-closed；保存 Hook 配置时不能
        // 因无法解析而覆盖原有证据。
        Err(_) => return Ok(()),
    };
    let identity = workspace_identity(workspace);
    let records = store
        .get_mut("records")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "workspace Hook trust records 必须是数组".to_owned())?;
    records.retain(|record| {
        record.get("workspaceIdentity").and_then(Value::as_str) != Some(identity.as_str())
    });
    write_hook_trust_store(app, &store)
}

fn hooks_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    let workspace_path = required_string(&args, "workspacePath")?;
    let workspace = registered_root(app, &workspace_path)?;
    match method {
        "loadHooks" => {
            let mut hooks = Vec::new();
            let user_path = crate::storage::root_dir(app)
                .map_err(|error| error.to_string())?
                .join("hooks.json");
            hooks.extend(load_hooks_file(&user_path, "user")?);
            let workspace_snapshot = workspace_hook_trust_snapshot(app, &workspace)?;
            let workspace_inputs = workspace_hook_inputs(app, &workspace)?;
            for (source_file_index, input) in workspace_inputs.iter().enumerate() {
                for source_hook in &input.hooks {
                    let mut hook = source_hook.clone();
                    if let Some(snapshot) = workspace_snapshot.as_ref() {
                        let declaration_digest =
                            workspace_hook_declaration_digest(&workspace, &input.path, &hook);
                        if let Some(object) = hook.as_object_mut() {
                            object.insert(
                                "workspaceHook".to_owned(),
                                json!({
                                    "reviewItemId": workspace_hook_review_item_id(&declaration_digest),
                                    "workspaceIdentity": workspace_identity(&workspace),
                                    "bundleDigest": snapshot.bundle_digest,
                                    "hookDeclarationDigest": declaration_digest,
                                    "sourceFileIndex": source_file_index,
                                    "trustState": if snapshot.trusted_declaration_digests.contains(&declaration_digest) {
                                        "trusted_persistent"
                                    } else {
                                        "pending_trust"
                                    },
                                }),
                            );
                        }
                    }
                    hooks.push(hook);
                }
            }
            let mut result = json!({
                "hooks": hooks,
                "hooksEnabled": hooks.iter().any(|value| value.get("enabled").and_then(Value::as_bool).unwrap_or(true)),
            });
            if let Some(snapshot) = workspace_snapshot.as_ref() {
                result["workspaceHookSnapshot"] =
                    workspace_hook_snapshot_value(&workspace, &workspace_inputs, snapshot);
            }
            if workspace_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.trust_store_corrupt)
            {
                result["trustStoreCorrupt"] = Value::Bool(true);
            }
            Ok(result)
        }
        "saveHooks" => {
            let values = object(&args)?
                .get("hooks")
                .and_then(Value::as_array)
                .ok_or_else(|| "Hook 缺少 hooks 数组".to_owned())?;
            let mut user = Vec::new();
            let mut project = Vec::new();
            for value in values {
                let scope = value
                    .get("location")
                    .and_then(|value| value.get("scope"))
                    .and_then(Value::as_str)
                    .unwrap_or("project");
                if scope == "user" {
                    user.push(value.clone());
                } else {
                    project.push(value.clone());
                }
            }
            let home_config = crate::storage::root_dir(app)
                .map_err(|error| error.to_string())?
                .join("hooks.json");
            let project_config = workspace.join(".agents").join("settings.json");
            if let Some(parent) = home_config.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            if let Some(parent) = project_config.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            save_hooks_file(&home_config, &user, true)?;
            save_hooks_file(&project_config, &project, false)?;
            revoke_workspace_hook_trust(app, &workspace)?;
            Ok(Value::Null)
        }
        "grantWorkspaceHookTrust" => grant_workspace_hook_trust(app, &workspace, &args),
        _ => unsupported_method("hooks", method),
    }
}

fn memory_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("memories"))
}

fn memory_file_name(raw: &str) -> Result<String, String> {
    if raw.is_empty()
        || raw == "."
        || raw == ".."
        || raw
            .chars()
            .any(|character| character == '/' || character == '\\' || character.is_control())
    {
        return Err("Memory 文件名无效".to_owned());
    }
    if !raw.ends_with(".md") {
        return Err("Memory 只允许读取 Markdown 文件".to_owned());
    }
    Ok(raw.to_owned())
}

fn memory_files(root: &Path) -> Result<Vec<(String, PathBuf, fs::Metadata)>, String> {
    let mut files = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(format!("读取 Memory 目录失败：{error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("md")
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        files.push((name, path, metadata));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn memory_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    let root = memory_root(app)?;
    match method {
        "listProjectMemories" => {
            let files = memory_files(&root)?;
            if files.is_empty() {
                return Ok(json!([]));
            }
            let updated_at = files
                .iter()
                .map(|(_, _, metadata)| modified_ms(metadata))
                .max()
                .unwrap_or_default();
            Ok(
                json!([{"id": "keencode-local", "label": "KeenCode 本地记忆", "updatedAt": updated_at, "files": files.into_iter().map(|(name, path, metadata)| json!({"name": name, "path": crate::path_utils::path_to_frontend(&path), "kind": if name == "MEMORY.md" { "index" } else { "item" }, "size": metadata.len(), "updatedAt": modified_ms(&metadata)})).collect::<Vec<_>>() }]),
            )
        }
        "readProjectMemoryFile" => {
            let workspace_id = required_string(&args, "workspaceId")?;
            if workspace_id != "keencode-local" {
                return Err(format!("找不到本地 Memory workspace：{workspace_id}"));
            }
            let name = memory_file_name(&required_string(&args, "fileName")?)?;
            let path = root.join(&name);
            let metadata =
                fs::metadata(&path).map_err(|error| format!("读取 Memory 文件失败：{error}"))?;
            if metadata.len() > 5 * 1024 * 1024 {
                return Err("Memory 文件超过 5 MB 预览上限".to_owned());
            }
            let content = fs::read_to_string(&path)
                .map_err(|error| format!("读取 Memory 文件失败：{error}"))?;
            Ok(json!({"content": content, "updatedAt": modified_ms(&metadata)}))
        }
        "get" | "read" => {
            let name = object(&args)?
                .get("fileName")
                .and_then(Value::as_str)
                .unwrap_or("MEMORY.md");
            let name = memory_file_name(name)?;
            let path = root.join(name);
            if !path.exists() {
                return Ok(Value::String(String::new()));
            }
            Ok(Value::String(
                fs::read_to_string(path).map_err(|error| format!("读取 Memory 失败：{error}"))?,
            ))
        }
        "set" | "write" => {
            let content = required_string(&args, "content")?;
            if content.len() > 800_000 {
                return Err("Memory 内容超过 800 KB".to_owned());
            }
            fs::create_dir_all(&root).map_err(|error| format!("创建 Memory 目录失败：{error}"))?;
            let path = root.join(memory_file_name(
                object(&args)?
                    .get("fileName")
                    .and_then(Value::as_str)
                    .unwrap_or("MEMORY.md"),
            )?);
            crate::storage::atomic_write_private(&path, content.as_bytes())
                .map_err(|error| format!("保存 Memory 失败：{error}"))?;
            Ok(Value::String(content))
        }
        "status" | "getStatus" => {
            let files = memory_files(&root)?;
            let enabled = crate::app_settings::get(app)
                .map_err(|error| error.to_string())?
                .local_memories;
            // MemoryService 的运行标志由其 Tauri 命令私有维护；这里返回持久化快照，
            // 不把“服务已初始化”冒充“流水线正在运行”。
            Ok(
                json!({"enabled": enabled, "root": crate::path_utils::path_to_frontend(&root), "memoryCount": files.len()}),
            )
        }
        "reset" | "clear" => {
            let state = app
                .try_state::<Arc<crate::memories::MemoryService>>()
                .ok_or_else(|| "MemoryService 尚未初始化".to_owned())?;
            state.inner().clear().map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        _ => unsupported_method("memory", method),
    }
}

#[derive(Clone, Copy)]
enum McpConfigShape {
    McpServers,
}

/// MCP 文件同时服务于设置页记录和同步候选，目录来源不能直接当成
/// NativeMcpServerRecord 的 server source，两个协议的字段要分别投影。
struct McpConfigEntry {
    path: PathBuf,
    directory_source: &'static str,
    name: String,
    config: Value,
    enabled: bool,
}

fn mcp_user_descriptors(
    app: &AppHandle,
) -> Result<Vec<(PathBuf, &'static str, McpConfigShape)>, String> {
    let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    Ok(vec![(
        root.join("mcp.json"),
        "zcode",
        McpConfigShape::McpServers,
    )])
}

fn mcp_project_descriptors(workspace: &Path) -> Vec<(PathBuf, &'static str, McpConfigShape)> {
    vec![(
        workspace.join(".agents").join("mcp.json"),
        "agents",
        McpConfigShape::McpServers,
    )]
}

/// 解析资源目录的 workspace 读取根，供 MCP、插件和命令共用。默认对话工作区能承载本地 Session，
/// 但不会登记在 projects.json；它只允许作为读取/运行时 cwd，其他未登记
/// 路径仍必须保留“项目尚未添加”错误，不能把路径错误降级为空配置。
fn resolve_resource_workspace_root(app: &AppHandle, supplied: &str) -> Result<PathBuf, String> {
    match registered_root(app, supplied) {
        Ok(root) => Ok(root),
        Err(error) => {
            let conversation = crate::workspace::conversation_workspace_root(app)?;
            resolve_resource_workspace_root_from_registration(supplied, Err(error), &conversation)
        }
    }
}

/// 仅把精确的 conversation 根转换为读取根；其他登记失败必须返回原错误。
fn resolve_resource_workspace_root_from_registration(
    supplied: &str,
    registered: Result<PathBuf, String>,
    conversation: &Path,
) -> Result<PathBuf, String> {
    registered.or_else(|error| {
        crate::workspace::conversation_read_root(supplied, conversation)
            .map(|_| conversation.to_path_buf())
            .map_err(|_| error)
    })
}

fn mcp_servers_from_document(
    document: &Value,
    shape: McpConfigShape,
) -> Result<Map<String, Value>, String> {
    let value = match shape {
        McpConfigShape::McpServers => document.get("mcpServers"),
    };
    let Some(value) = value else {
        return Ok(Map::new());
    };
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "MCP server map 必须是对象".to_owned())
}

fn read_mcp_servers(
    path: &Path,
    shape: McpConfigShape,
) -> Result<(Value, Map<String, Value>), String> {
    let Some(bytes) = crate::storage::read_private_bytes_bounded(path, 8 * 1024 * 1024, "MCP 配置")
        .map_err(|error| error.to_string())?
    else {
        return Ok((Value::Object(Map::new()), Map::new()));
    };
    let document: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("MCP 配置格式无效：{error}"))?;
    let servers = mcp_servers_from_document(&document, shape)?;
    Ok((document, servers))
}

fn write_mcp_servers(
    path: &Path,
    shape: McpConfigShape,
    servers: Map<String, Value>,
    private: bool,
) -> Result<(), String> {
    let (mut document, _) = read_mcp_servers(path, shape)?;
    let root = document
        .as_object_mut()
        .ok_or_else(|| "MCP 配置必须是对象".to_owned())?;
    match shape {
        McpConfigShape::McpServers => {
            root.insert("mcpServers".to_owned(), Value::Object(servers));
        }
    }
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("MCP 配置编码失败：{error}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建 MCP 配置目录失败：{error}"))?;
    }
    if private {
        crate::storage::atomic_write_private(path, &bytes)
            .map_err(|error| format!("保存 MCP 配置失败：{error}"))
    } else {
        fs::write(path, bytes).map_err(|error| format!("保存项目 MCP 配置失败：{error}"))
    }
}

fn mcp_candidate_id(source: &str, path: &Path, name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(name.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn mcp_server_enabled(config: &Value) -> bool {
    config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
        && config
            .get("enable")
            .and_then(Value::as_bool)
            .unwrap_or(true)
}

fn read_mcp_entries(
    descriptors: impl IntoIterator<Item = (PathBuf, &'static str, McpConfigShape)>,
) -> Result<Vec<McpConfigEntry>, String> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (path, directory_source, shape) in descriptors {
        let (_, servers) = read_mcp_servers(&path, shape)?;
        for (name, config) in servers {
            if !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            let enabled = mcp_server_enabled(&config);
            entries.push(McpConfigEntry {
                path: path.clone(),
                directory_source,
                name,
                config,
                enabled,
            });
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn mcp_native_record_value(entry: &McpConfigEntry, project: Option<&Path>) -> Value {
    let is_project = project.is_some();
    let location_scope = if is_project { "project" } else { "user" };
    let native_scope = if is_project { "workspace" } else { "user" };
    let mut location = json!({
        "source": entry.directory_source,
        "scope": location_scope,
        "directoryPath": crate::path_utils::path_to_frontend(
            entry.path.parent().unwrap_or(Path::new(".")),
        ),
    });
    let mut record = json!({
        "name": entry.name,
        "config": entry.config,
        "enabled": entry.enabled,
        "source": "zcodeagentmcp",
        "scope": native_scope,
        "location": location,
        "file": {
            "format": "json",
            "filePath": crate::path_utils::path_to_frontend(&entry.path),
        },
    });
    if let Some(project) = project {
        let project_path = crate::path_utils::path_to_frontend(project);
        record["projectPath"] = Value::String(project_path.clone());
        location["projectPath"] = Value::String(project_path);
        record["location"] = location;
    }
    record
}

fn mcp_sync_candidate_value(entry: &McpConfigEntry) -> Value {
    json!({
        "id": mcp_candidate_id(entry.directory_source, &entry.path, &entry.name),
        "name": entry.name,
        "config": entry.config,
        "enabled": entry.enabled,
        "source": entry.directory_source,
        "path": crate::path_utils::path_to_frontend(&entry.path),
    })
}

fn mcp_records(app: &AppHandle, project: Option<&Path>) -> Result<Vec<Value>, String> {
    let descriptors = if let Some(project) = project {
        mcp_project_descriptors(project)
    } else {
        mcp_user_descriptors(app)?
    };
    Ok(read_mcp_entries(descriptors)?
        .iter()
        .map(|entry| mcp_native_record_value(entry, project))
        .collect())
}

fn mcp_sync_candidates(app: &AppHandle) -> Result<Vec<Value>, String> {
    Ok(read_mcp_entries(mcp_user_descriptors(app)?)?
        .iter()
        .map(mcp_sync_candidate_value)
        .collect())
}

/// MCP 状态列表请求中的一个安全配置投影。
///
/// 服务层只把名称、传输类型和启用状态带到状态合并逻辑；命令、Header、环境变量
/// 等正文不会进入响应。`config_error` 用于把无效配置绑定到具体 Server，避免把
/// 其他 Server 的真实状态丢掉。
#[derive(Clone, Debug, Eq, PartialEq)]
struct McpStatusDescriptor {
    name: String,
    transport: String,
    enabled: bool,
    config_error: Option<String>,
}

/// 从服务参数读取 UI 显式下发的 MCP Server 名称和传输类型。
///
/// UI 可能从项目 `.agents/mcp.json` 读取配置，而 Runtime 候选只包含已经发布的
/// 当前项目能力。这里保留显式列表以便未进入候选的 Server 返回逐项
/// `status_unavailable`，不能因候选缺失而返回空数组或伪造连接状态。
fn requested_mcp_status_descriptors(
    args: &Value,
) -> Result<Option<Vec<McpStatusDescriptor>>, String> {
    let Some(value) = object(args)?.get("mcpServers") else {
        return Ok(None);
    };
    let servers = value
        .as_array()
        .ok_or_else(|| "参数 mcpServers 必须是数组".to_owned())?;
    let mut descriptors = Vec::with_capacity(servers.len());
    let mut names = BTreeSet::new();
    for server in servers {
        let object = server
            .as_object()
            .ok_or_else(|| "参数 mcpServers 的每项必须是对象".to_owned())?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| "参数 mcpServers 的 Server 缺少非空 name".to_owned())?
            .to_owned();
        validate_component(&name, "MCP Server 名称")?;
        let (allowed_fields, is_stdio) = if object.get("command").is_some() {
            (
                [
                    "name",
                    "command",
                    "args",
                    "env",
                    "isolation",
                    "protocolVersion",
                    "timeoutMs",
                ]
                .as_slice(),
                true,
            )
        } else {
            (
                [
                    "name",
                    "type",
                    "url",
                    "headers",
                    "oauth",
                    "isolation",
                    "protocolVersion",
                    "timeoutMs",
                ]
                .as_slice(),
                false,
            )
        };
        if let Some(field) = object
            .keys()
            .find(|field| !allowed_fields.contains(&field.as_str()))
        {
            return Err(format!("MCP Server {name} 包含未知字段 {field}"));
        }
        if let Some(timeout) = object.get("timeoutMs")
            && (!timeout.is_u64() || timeout.as_u64() == Some(0))
        {
            return Err(format!("MCP Server {name} 的 timeoutMs 必须是正整数"));
        }
        if let Some(isolation) = object.get("isolation")
            && !matches!(isolation.as_str(), Some("session" | "workspace"))
        {
            return Err(format!("MCP Server {name} 的 isolation 无效"));
        }
        if let Some(protocol_version) = object.get("protocolVersion")
            && !matches!(
                protocol_version.as_str(),
                Some("legacy" | "auto" | "2026-07-28")
            )
        {
            return Err(format!("MCP Server {name} 的 protocolVersion 无效"));
        }
        let entry_field = if is_stdio { "env" } else { "headers" };
        if let Some(entries) = object.get(entry_field) {
            let entries = entries
                .as_array()
                .ok_or_else(|| format!("MCP Server {name} 的 {entry_field} 必须是数组"))?;
            for entry in entries {
                let entry = entry
                    .as_object()
                    .ok_or_else(|| format!("MCP Server {name} 的 {entry_field} 项必须是对象"))?;
                let entry_name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| format!("MCP Server {name} 的 {entry_field} 项缺少 name"))?;
                if entry.get("value").and_then(Value::as_str).is_none() {
                    return Err(format!(
                        "MCP Server {name} 的 {entry_field}.{entry_name} value 必须是字符串"
                    ));
                }
                if entry
                    .keys()
                    .any(|field| field != "name" && field != "value")
                {
                    return Err(format!("MCP Server {name} 的 {entry_field} 项包含未知字段"));
                }
            }
        }
        if let Some(args) = object.get("args") {
            let args = args
                .as_array()
                .ok_or_else(|| format!("MCP Server {name} 的 args 必须是数组"))?;
            if args.iter().any(|value| value.as_str().is_none()) {
                return Err(format!("MCP Server {name} 的 args 只能包含字符串"));
            }
        }
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("参数 mcpServers 包含重复 Server：{name}"));
        }
        let descriptor = if object
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| !command.trim().is_empty())
        {
            McpStatusDescriptor {
                name,
                transport: "stdio".to_owned(),
                enabled: true,
                config_error: None,
            }
        } else {
            let transport = object
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("MCP Server {name} 缺少 type"))?;
            if !matches!(transport, "http" | "sse") {
                return Err(format!("MCP Server {name} 的 type 无效"));
            }
            let has_url = object
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|url| !url.trim().is_empty());
            if !has_url {
                return Err(format!("MCP Server {name} 缺少非空 url"));
            }
            McpStatusDescriptor {
                name,
                transport: transport.to_owned(),
                enabled: true,
                config_error: None,
            }
        };
        descriptors.push(descriptor);
    }
    Ok(Some(descriptors))
}

/// 从当前本地 MCP 配置构造状态列表的安全投影。
fn configured_mcp_status_descriptors(
    app: &AppHandle,
    project: &Path,
) -> Result<Vec<McpStatusDescriptor>, String> {
    let mut descriptors = BTreeMap::<String, McpStatusDescriptor>::new();
    for record in mcp_records(app, None)?
        .into_iter()
        .chain(mcp_records(app, Some(project))?)
    {
        let Some(name) = record.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = name.to_owned();
        let config = record.get("config").cloned().unwrap_or(Value::Null);
        let (transport, config_error) = match config.as_object() {
            Some(config) if config.get("command").and_then(Value::as_str).is_some() => {
                ("stdio".to_owned(), None)
            }
            Some(config) if config.get("url").and_then(Value::as_str).is_some() => {
                let transport = config
                    .get("type")
                    .and_then(Value::as_str)
                    .filter(|kind| matches!(*kind, "sse" | "http"))
                    .unwrap_or("http")
                    .to_owned();
                (transport, None)
            }
            _ => (
                "http".to_owned(),
                Some("MCP Server 配置缺少 command 或 url".to_owned()),
            ),
        };
        let descriptor = McpStatusDescriptor {
            name: name.clone(),
            transport,
            enabled: record
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            config_error,
        };
        // 项目配置优先于用户配置；同一名称只保留一个状态行。
        descriptors.insert(name, descriptor);
    }
    Ok(descriptors.into_values().collect())
}

/// 把 Runtime MCP 状态映射成 ZCode `zcodeMcpListResult` 的传输字符串。
fn mcp_status_transport(transport: keencode_acp::McpTransportKind) -> &'static str {
    match transport {
        keencode_acp::McpTransportKind::Stdio => "stdio",
        keencode_acp::McpTransportKind::StreamableHttp => "http",
    }
}

/// 将 Runtime 连接状态映射成源协议状态；源协议没有 `uninitialized`，未发布候选
/// 必须落到带 `status_unavailable` 的 disconnected，而不是 connected。
fn mcp_status_connection(status: keencode_acp::McpConnectionStatus) -> &'static str {
    match status {
        keencode_acp::McpConnectionStatus::Connecting => "connecting",
        keencode_acp::McpConnectionStatus::Connected => "connected",
        keencode_acp::McpConnectionStatus::Disabled => "disabled",
        keencode_acp::McpConnectionStatus::Disconnected
        | keencode_acp::McpConnectionStatus::Uninitialized => "disconnected",
        keencode_acp::McpConnectionStatus::Failed => "failed",
    }
}

/// 生成与当前观察一致的源协议错误分类，不根据错误文案猜测远端身份。
fn mcp_status_failure_kind(
    status: &str,
    error: Option<&str>,
    oauth: keencode_acp::McpOAuthStatus,
) -> Option<&'static str> {
    match status {
        "failed" => Some("connection_failed"),
        "disconnected" if oauth != keencode_acp::McpOAuthStatus::NotRequired => {
            Some("not_authenticated")
        }
        "disconnected" => Some("unexpected_disconnect"),
        _ if error.is_some() => Some("tool_list_failed"),
        _ => None,
    }
}

/// 以当前读取时间标记快照；Runtime 快照本身不保存时钟，不能伪造历史连接时间。
fn mcp_status_snapshot(
    descriptor: &McpStatusDescriptor,
    runtime: Option<&crate::agent_runtime::RuntimeMcpServerSnapshot>,
    observed_at: &str,
) -> Value {
    if let Some(error) = descriptor.config_error.as_deref() {
        return json!({
            "status": "failed",
            "transport": descriptor.transport,
            "toolCount": 0,
            "updatedAt": observed_at,
            "error": error,
            "failureKind": "config_invalid",
        });
    }
    if !descriptor.enabled {
        return json!({
            "status": "disabled",
            "transport": descriptor.transport,
            "toolCount": 0,
            "updatedAt": observed_at,
        });
    }
    let Some(runtime) = runtime else {
        return json!({
            "status": "disconnected",
            "transport": descriptor.transport,
            "toolCount": 0,
            "updatedAt": observed_at,
            "error": "当前项目没有已发布的 MCP 运行态候选",
            "failureKind": "status_unavailable",
        });
    };
    let status = mcp_status_connection(runtime.connection_status);
    let mut snapshot = json!({
        "status": status,
        "transport": mcp_status_transport(runtime.transport),
        "toolCount": runtime.tools_count,
        "updatedAt": observed_at,
    });
    if let Some(error) = runtime.error.as_deref() {
        snapshot["error"] = Value::String(error.to_owned());
    }
    if let Some(failure_kind) =
        mcp_status_failure_kind(status, runtime.error.as_deref(), runtime.oauth_status)
    {
        snapshot["failureKind"] = Value::String(failure_kind.to_owned());
    }
    snapshot
}

/// 连接或读取当前项目的真实 MCP 候选并返回源协议状态快照。
async fn list_workspace_mcp_server_statuses(
    app: &AppHandle,
    args: &Value,
) -> Result<Value, String> {
    let values = object(args)?;
    let workspace_path = values
        .get("workspacePath")
        .or_else(|| values.get("projectPath"))
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| "MCP 状态列表缺少 workspacePath".to_owned())?;
    let project = resolve_resource_workspace_root(app, workspace_path)?;
    let mode = match values.get("mode") {
        None => "connect",
        Some(Value::String(mode)) => mode.as_str(),
        Some(_) => return Err("MCP 状态列表 mode 必须是 connect 或 status".to_owned()),
    };
    if !matches!(mode, "connect" | "status") {
        return Err(format!("MCP 状态列表 mode 无效：{mode}"));
    }
    let requested = requested_mcp_status_descriptors(args)?;
    let runtime = app
        .try_state::<Arc<crate::agent_runtime::AgentRuntime>>()
        .ok_or_else(|| "Agent Runtime 尚未初始化，无法读取 MCP 状态".to_owned())?
        .inner()
        .clone();
    if mode == "connect" {
        // `connect` 是用户可见的刷新动作；强制重建候选才能重新握手并执行
        // tools/list。`status` 分支绝不能走这里，否则 OAuth 轮询会反复拉起进程。
        crate::extensions::ensure_runtime_extension_candidate(app, &project, &runtime, true)
            .await
            .map_err(|error| format!("MCP 连接与工具发现失败：{error}"))?;
    }
    let runtime_snapshot = runtime
        .mcp_runtime_snapshot(&project)
        .map_err(|error| format!("读取 MCP 运行态失败：{error}"))?
        .unwrap_or_default();
    let runtime_by_name = runtime_snapshot
        .iter()
        .map(|snapshot| (snapshot.name.clone(), snapshot))
        .collect::<BTreeMap<_, _>>();
    let descriptors = match requested {
        Some(descriptors) => descriptors,
        None => {
            let mut descriptors = configured_mcp_status_descriptors(app, &project)?;
            let known = descriptors
                .iter()
                .map(|descriptor| descriptor.name.clone())
                .collect::<BTreeSet<_>>();
            descriptors.extend(
                runtime_snapshot
                    .iter()
                    .filter(|snapshot| !known.contains(&snapshot.name))
                    .map(|snapshot| McpStatusDescriptor {
                        name: snapshot.name.clone(),
                        transport: mcp_status_transport(snapshot.transport).to_owned(),
                        enabled: true,
                        config_error: None,
                    }),
            );
            descriptors
        }
    };
    let observed_at = chrono::Utc::now().to_rfc3339();
    let statuses = descriptors
        .iter()
        .map(|descriptor| {
            let runtime = runtime_by_name.get(&descriptor.name).copied();
            let snapshot = if descriptor.transport == "sse" {
                json!({
                    "status": "failed",
                    "transport": "sse",
                    "toolCount": 0,
                    "updatedAt": observed_at,
                    "error": "当前本地 MCP Runtime 不支持 SSE 传输",
                    "failureKind": "config_invalid",
                })
            } else {
                mcp_status_snapshot(descriptor, runtime, &observed_at)
            };
            (descriptor.name.clone(), snapshot)
        })
        .collect::<Map<_, _>>();
    Ok(json!({"statuses": statuses}))
}

async fn mcp_sync_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "loadMcpFromUserDirectory" => {
            let values = object(&args)?;
            let project = values
                .get("workspacePath")
                .or_else(|| values.get("projectPath"))
                .and_then(Value::as_str)
                .map(|path| resolve_resource_workspace_root(app, path))
                .transpose()?;
            Ok(json!({"servers": mcp_records(app, project.as_deref())?}))
        }
        "saveMcpToUserDirectory" => {
            let action = required_string(&args, "action")?;
            let name = required_string(&args, "name")?;
            validate_component(&name, "MCP Server 名称")?;
            let values = object(&args)?;
            let project = values
                .get("projectPath")
                .or_else(|| values.get("workspacePath"))
                .and_then(Value::as_str)
                .map(|path| registered_root(app, path))
                .transpose()?;
            let (path, shape, private) = if let Some(project) = project.as_ref() {
                (
                    project.join(".agents").join("mcp.json"),
                    McpConfigShape::McpServers,
                    false,
                )
            } else {
                let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
                (root.join("mcp.json"), McpConfigShape::McpServers, true)
            };
            let (_, mut servers) = read_mcp_servers(&path, shape)?;
            match action.as_str() {
                "set-enabled" => {
                    let enabled = object(&args)?
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| "MCP set-enabled 缺少 enabled".to_owned())?;
                    let config = servers
                        .get_mut(&name)
                        .ok_or_else(|| format!("找不到 MCP Server {name}"))?;
                    if let Some(object) = config.as_object_mut() {
                        object.insert("enabled".to_owned(), Value::Bool(enabled));
                    } else {
                        return Err(format!("MCP Server {name} 配置必须是对象"));
                    }
                }
                "upsert" => {
                    let config = object(&args)?
                        .get("config")
                        .cloned()
                        .ok_or_else(|| "MCP upsert 缺少 config".to_owned())?;
                    if !config.is_object() {
                        return Err("MCP config 必须是对象".to_owned());
                    }
                    servers.insert(name, config);
                }
                "delete" | "remove" => {
                    if servers.remove(&name).is_none() {
                        return Err(format!("找不到 MCP Server {name}"));
                    }
                }
                other => return Err(format!("未知 MCP 同步 action：{other}")),
            }
            write_mcp_servers(&path, shape, servers, private)?;
            Ok(Value::Null)
        }
        "listLocalUserMcpCandidates" => {
            let data_root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
            Ok(
                json!({"candidates": mcp_sync_candidates(app)?, "localHomeDir": crate::path_utils::path_to_frontend(&data_root)}),
            )
        }
        "exportMcpServers" => {
            let ids = required_array_strings(&args, "serverIds")?;
            let candidates = mcp_sync_candidates(app)?;
            let mut selected = Vec::new();
            for id in ids {
                let item = candidates
                    .iter()
                    .find(|candidate| candidate["id"].as_str() == Some(id.as_str()))
                    .ok_or_else(|| format!("找不到 MCP sync candidate：{id}"))?;
                selected.push(item.clone());
            }
            let data_root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
            Ok(
                json!({"servers": selected, "localHomeDir": crate::path_utils::path_to_frontend(&data_root)}),
            )
        }
        "listWorkspaceMcpServerStatuses" => list_workspace_mcp_server_statuses(app, &args).await,
        "listRemoteUserMcpStatuses" | "checkRemoteUserMcpWriteAccess" => unsupported(
            "mcp-sync",
            method,
            "SSH/WSL/remote workspace MCP synchronization is disabled",
        ),
        "importMcpServers" => {
            if object(&args)?
                .get("overwrite")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return unsupported(
                    "mcp-sync",
                    method,
                    "MCP overwrite requires an explicit transactional owner",
                );
            }
            let servers = object(&args)?
                .get("servers")
                .and_then(Value::as_array)
                .ok_or_else(|| "MCP import 缺少 servers 数组".to_owned())?;
            let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
            let path = root.join("mcp.json");
            let (_, mut existing) = read_mcp_servers(&path, McpConfigShape::McpServers)?;
            let mut results = Vec::new();
            let mut changed = false;
            for server in servers {
                let name = server
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "MCP import server 缺少 name".to_owned())?;
                validate_component(name, "MCP Server 名称")?;
                if existing.contains_key(name) {
                    results.push(json!({"name": name, "status": "skipped", "path": crate::path_utils::path_to_frontend(&path)}));
                    continue;
                }
                let config = server
                    .get("config")
                    .cloned()
                    .ok_or_else(|| format!("MCP Server {name} 缺少 config"))?;
                if !config.is_object() {
                    return Err(format!("MCP Server {name} config 必须是对象"));
                }
                existing.insert(name.to_owned(), config);
                changed = true;
                results.push(json!({"name": name, "status": "synced", "path": crate::path_utils::path_to_frontend(&path)}));
            }
            if changed {
                write_mcp_servers(&path, McpConfigShape::McpServers, existing, true)?;
            }
            Ok(json!({"results": results}))
        }
        _ => unsupported_method("mcp-sync", method),
    }
}

fn sync_call(channel: &str, method: &str) -> Result<Value, String> {
    unsupported(
        channel,
        method,
        "remote settings synchronization is disabled; use local Skills/Plugins services",
    )
}

// -----------------------------------------------------------------------------
// File watcher / client config / subagents / prompt attachment transfer
// -----------------------------------------------------------------------------

#[derive(Clone)]
struct FileWatchState {
    path: PathBuf,
    recursive: bool,
    /// 文件监听器只能被创建它的 RPC 连接消费或释放。
    owner_connection_id: Option<String>,
    lifecycle: Arc<Mutex<FileWatchLifecycle>>,
}

#[derive(Default)]
struct FileWatchLifecycle {
    listeners: usize,
    /// unwatch 后拒绝迟到的订阅；最后一个订阅释放仍允许重新订阅。
    closed: bool,
    native: Option<super::native_file_watcher::NativeFileWatch>,
}

static FILE_WATCHERS: OnceLock<Mutex<HashMap<String, FileWatchState>>> = OnceLock::new();
static FILE_WATCH_SEQUENCE: OnceLock<Mutex<u64>> = OnceLock::new();

fn file_watchers() -> &'static Mutex<HashMap<String, FileWatchState>> {
    FILE_WATCHERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_file_watch_id() -> String {
    let mut sequence = FILE_WATCH_SEQUENCE
        .get_or_init(|| Mutex::new(0))
        .lock()
        .expect("file watcher sequence lock poisoned");
    *sequence = sequence.saturating_add(1);
    format!("watch-{}-{}", now_ms(), *sequence)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnedWorkflowWatchKind {
    Global,
    Project,
}

/// 对 appdata workflow 目录做表示层归一化。前端可能返回 `\\?\` 前缀或不同大小写，
/// 但这些别名不能扩大可访问范围；`..` 直接拒绝，真实目录仍由下方的 symlink 检查保护。
fn normalized_workflow_watch_path(path: &Path) -> Result<String, String> {
    if path.as_os_str().is_empty() {
        return Err("工作流监听路径不能为空".to_owned());
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("工作流监听路径不能包含父目录跳转".to_owned());
    }
    let mut value = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/UNC/") {
        value = format!("//{rest}");
    } else if let Some(rest) = value.strip_prefix("//?/") {
        value = rest.to_owned();
    }
    value = value.trim_end_matches('/').to_owned();
    #[cfg(windows)]
    {
        value.make_ascii_lowercase();
    }
    Ok(value)
}

fn same_workflow_watch_path(left: &Path, right: &Path) -> bool {
    match (
        normalized_workflow_watch_path(left),
        normalized_workflow_watch_path(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn path_is_under_workflow_data_root(path: &Path, data_root: &Path) -> bool {
    let Ok(path) = normalized_workflow_watch_path(path) else {
        return false;
    };
    let Ok(root) = normalized_workflow_watch_path(data_root) else {
        return false;
    };
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn owned_workflow_watch_kind(
    raw_path: &Path,
    data_root: &Path,
    project_storage_dirs: &[PathBuf],
) -> Option<OwnedWorkflowWatchKind> {
    let global_dir = data_root.join("workflows");
    if same_workflow_watch_path(raw_path, &global_dir) {
        return Some(OwnedWorkflowWatchKind::Global);
    }
    project_storage_dirs
        .iter()
        .any(|directory| same_workflow_watch_path(raw_path, directory))
        .then_some(OwnedWorkflowWatchKind::Project)
}

fn ensure_no_symlink_ancestor(path: &Path, data_root: &Path) -> Result<(), String> {
    let mut current = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("工作流监听目录祖先不能是符号链接".to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法检查工作流监听目录祖先：{error}")),
        }
        if same_workflow_watch_path(&current, data_root) {
            return Ok(());
        }
        let Some(parent) = current.parent() else {
            return Err("工作流监听目录超出应用数据根目录".to_owned());
        };
        if parent == current || !path_is_under_workflow_data_root(&current, data_root) {
            return Err("工作流监听目录超出应用数据根目录".to_owned());
        }
        current = parent.to_path_buf();
    }
}

fn ensure_owned_workflow_watch_directory(path: &Path, data_root: &Path) -> Result<PathBuf, String> {
    if !path_is_under_workflow_data_root(path, data_root) {
        return Err("工作流监听目录超出应用数据根目录".to_owned());
    }
    ensure_no_symlink_ancestor(path, data_root)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err("工作流监听目录不能是符号链接".to_owned());
            }
            if !metadata.is_dir() {
                return Err("工作流监听目标不是目录".to_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| format!("无法创建工作流监听目录：{error}"))?;
        }
        Err(error) => return Err(format!("无法检查工作流监听目录：{error}")),
    }
    ensure_no_symlink_ancestor(path, data_root)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法复核工作流监听目录：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("工作流监听目录在创建期间发生变化".to_owned());
    }
    let canonical =
        fs::canonicalize(path).map_err(|error| format!("无法规范化工作流监听目录：{error}"))?;
    let canonical_root = fs::canonicalize(data_root)
        .map_err(|error| format!("无法规范化工作流数据根目录：{error}"))?;
    if !path_within(&canonical_root, &canonical) {
        return Err("工作流监听目录超出应用数据根目录".to_owned());
    }
    Ok(canonical)
}

fn owned_project_workflow_dirs(app: &AppHandle, data_root: &Path) -> Vec<PathBuf> {
    let Ok(projects) = keencode_resources::project_storage_directories(data_root) else {
        return Vec::new();
    };
    projects
        .into_iter()
        .filter_map(|directory| {
            let descriptor_path = directory.join("project.json");
            let bytes = crate::storage::read_private_bytes_bounded(
                &descriptor_path,
                64 * 1024,
                "项目存储描述",
            )
            .ok()
            .flatten()?;
            let project =
                serde_json::from_slice::<keencode_resources::ProjectStorage>(&bytes).ok()?;
            // 项目存储可能保留已移除项目的历史目录；只有当前登记的工作区才允许建立 watcher。
            crate::workspace::registered_project_root(app, &project.path).ok()?;
            Some(directory.join("workflows"))
        })
        .collect()
}

fn owned_workflow_watch_path(app: &AppHandle, raw_path: &str) -> Result<PathBuf, String> {
    if raw_path.trim().is_empty() || raw_path.chars().any(char::is_control) {
        return Err("工作流监听路径不能为空或包含控制字符".to_owned());
    }
    let raw_path = Path::new(raw_path);
    if !raw_path.is_absolute() {
        return Err("工作流监听路径必须是绝对路径".to_owned());
    }
    let data_root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    let project_dirs = owned_project_workflow_dirs(app, &data_root);
    let kind = owned_workflow_watch_kind(raw_path, &data_root, &project_dirs)
        .ok_or_else(|| "工作流监听路径未获授权；只能监听当前登记项目或全局工作流目录".to_owned())?;
    let expected = match kind {
        OwnedWorkflowWatchKind::Global => data_root.join("workflows"),
        OwnedWorkflowWatchKind::Project => project_dirs
            .into_iter()
            .find(|directory| same_workflow_watch_path(raw_path, directory))
            .ok_or_else(|| "项目工作流监听目录已失效".to_owned())?,
    };
    ensure_owned_workflow_watch_directory(&expected, &data_root)
}

fn file_watcher_listener_guard(
    app: AppHandle,
    id: &str,
    connection_id: Option<&str>,
    callback: EventCallback,
) -> Result<Subscription, String> {
    let state = file_watchers()
        .lock()
        .map_err(|_| "文件监听状态锁已损坏".to_owned())?
        .get(id)
        .cloned()
        .ok_or_else(|| format!("找不到文件监听器 {id}"))?;
    if state.owner_connection_id.as_deref() != connection_id {
        return Err(format!("文件监听器 {id} 不属于当前 RPC 连接"));
    }
    let event_subscription = {
        let mut lifecycle = state
            .lifecycle
            .lock()
            .map_err(|_| "文件监听订阅锁已损坏".to_owned())?;
        if lifecycle.closed {
            return Err(format!("文件监听器 {id} 已关闭"));
        }
        // 先验证归属，再绑定接收器，最后启用系统通知；首个事件也有确定的消费者。
        // 原生监听创建失败时，局部 Subscription 析构会撤销这个接收器。
        let event_subscription = service_event_subscription(
            app.clone(),
            &format!("keencode://file-watcher/{id}"),
            Some(id.to_owned()),
            callback,
        );
        if lifecycle.native.is_none() {
            let watch_id = id.to_owned();
            let root = state.path.clone();
            lifecycle.native = Some(super::native_file_watcher::NativeFileWatch::start(
                root.clone(),
                state.recursive,
                move |changed| {
                    let relative = changed
                        .as_ref()
                        .and_then(|path| path.strip_prefix(&root).ok())
                        .map(|path| path.to_string_lossy().into_owned());
                    let payload = file_watcher_event_payload(&watch_id, &root, relative.as_deref());
                    let _ = app.emit(&format!("keencode://file-watcher/{watch_id}"), payload);
                },
            )?);
        }
        lifecycle.listeners = lifecycle.listeners.saturating_add(1);
        event_subscription
    };
    Ok(Subscription::new(move || {
        drop(event_subscription);
        if let Ok(mut lifecycle) = state.lifecycle.lock() {
            lifecycle.listeners = lifecycle.listeners.saturating_sub(1);
            if lifecycle.listeners == 0 {
                // 生命周期锁内完成撤销与 join，避免立即重订阅复用旧线程的 pending 事件。
                drop(lifecycle.native.take());
            }
        }
    }))
}

fn stop_file_watcher(state: FileWatchState) {
    let native = state.lifecycle.lock().ok().and_then(|mut lifecycle| {
        lifecycle.closed = true;
        lifecycle.native.take()
    });
    drop(native);
}

fn file_watcher_event_payload(id: &str, directory: &Path, changed: Option<&str>) -> Value {
    json!({
        "id": id,
        "dirPath": crate::path_utils::path_to_frontend(directory),
        "changedPath": changed
            .map(|path| directory.join(path))
            .map(|path| crate::path_utils::path_to_frontend(&path)),
    })
}

fn watcher_owner_matches(state: &FileWatchState, connection_id: Option<&str>) -> bool {
    watcher_connection_matches(state.owner_connection_id.as_deref(), connection_id)
}

fn watcher_connection_matches(owner: Option<&str>, connection_id: Option<&str>) -> bool {
    owner == connection_id
}

fn file_watcher_call(
    app: &AppHandle,
    method: &str,
    args: Value,
    connection_id: Option<&str>,
) -> Result<Value, String> {
    match method {
        "watch" => {
            let raw_path = required_string(&args, "path")?;
            let data_root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
            let path = if path_is_under_workflow_data_root(Path::new(&raw_path), &data_root) {
                owned_workflow_watch_path(app, &raw_path)?
            } else {
                let (_, path) = registered_path(app, &raw_path, false)?;
                if !path.is_dir() {
                    return Err("file-watcher 只能监听目录".to_owned());
                }
                path
            };
            let recursive = optional_bool(&args, "recursive")?.unwrap_or(true);
            let id = next_file_watch_id();
            let state = FileWatchState {
                path,
                recursive,
                owner_connection_id: connection_id.map(str::to_owned),
                lifecycle: Arc::new(Mutex::new(FileWatchLifecycle::default())),
            };
            file_watchers()
                .lock()
                .map_err(|_| "文件监听状态锁已损坏".to_owned())?
                .insert(id.clone(), state);
            Ok(json!({"id": id}))
        }
        "unwatch" => {
            let id = required_string(&args, "id")?;
            let state = {
                let mut watchers = file_watchers()
                    .lock()
                    .map_err(|_| "文件监听状态锁已损坏".to_owned())?;
                let state = watchers
                    .get(&id)
                    .ok_or_else(|| format!("找不到文件监听器 {id}"))?;
                if !watcher_owner_matches(state, connection_id) {
                    return Err(format!("文件监听器 {id} 不属于当前 RPC 连接"));
                }
                watchers
                    .remove(&id)
                    .ok_or_else(|| format!("找不到文件监听器 {id}"))?
            };
            stop_file_watcher(state);
            Ok(Value::Null)
        }
        "disposeAll" => {
            let states = {
                let mut watchers = file_watchers()
                    .lock()
                    .map_err(|_| "文件监听状态锁已损坏".to_owned())?;
                let ids = watchers
                    .iter()
                    .filter(|(_, state)| watcher_owner_matches(state, connection_id))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                ids.into_iter()
                    .filter_map(|id| watchers.remove(&id))
                    .collect::<Vec<_>>()
            };
            for state in states {
                stop_file_watcher(state);
            }
            Ok(Value::Null)
        }
        _ => unsupported_method("file-watcher", method),
    }
}

fn client_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("client-config.json"))
}

fn client_config_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "getSnapshot" => {
            let _ = optional_bool(&args, "forceRefresh")?;
            let path = client_config_path(app)?;
            let Some(bytes) = crate::storage::read_private_bytes_bounded(
                &path,
                4 * 1024 * 1024,
                "本地客户端配置",
            )
            .map_err(|error| error.to_string())?
            else {
                return Ok(json!({
                    "source": "local",
                    "available": false,
                    "path": crate::path_utils::path_to_frontend(&path),
                }));
            };
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|error| format!("本地客户端配置格式无效：{error}"))?;
            let mut snapshot = value
                .as_object()
                .cloned()
                .ok_or_else(|| "本地客户端配置必须是对象".to_owned())?;
            snapshot.insert("source".to_owned(), Value::String("local".to_owned()));
            snapshot.insert("available".to_owned(), Value::Bool(true));
            snapshot.insert(
                "path".to_owned(),
                Value::String(crate::path_utils::path_to_frontend(&path)),
            );
            Ok(Value::Object(snapshot))
        }
        _ => unsupported_method("client-config", method),
    }
}

fn prompt_attachment_transfer_unsupported_error(method: &str) -> String {
    format!(
        "不支持 prompt-attachment-transfer.{method}：本地桌面未接入远端附件暂存；本地附件请使用零拷贝路径或 V4 attachmentPut"
    )
}

fn prompt_attachment_transfer_unsupported(method: &str) -> Result<Value, String> {
    Err(prompt_attachment_transfer_unsupported_error(method))
}

const SUBAGENT_REASONING_LEVELS: [&str; 7] =
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Agent 文件内部使用 provider::model，对外的 service DTO 仍保持严格的
/// ModelSelection 对象；两种形状只在文件读写边界互相转换，避免 catalog
/// 把前端对象误当成模型别名或把显式 none 退化为继承。
#[derive(Clone, Debug, Eq, PartialEq)]
struct NormalizedSubagentModelSelection {
    provider_id: String,
    model_id: String,
    reasoning_level: Option<String>,
}

fn normalized_subagent_identifier(
    value: Option<&Value>,
    field: &str,
    maximum: usize,
) -> Result<String, String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Subagent modelSelection.{field} 缺失"))?
        .trim();
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(char::is_control)
        || value.contains("::")
    {
        return Err(format!("Subagent modelSelection.{field} 无效"));
    }
    Ok(value.to_owned())
}

fn normalized_subagent_reasoning_level(value: &str) -> Result<String, String> {
    let value = value.trim();
    if SUBAGENT_REASONING_LEVELS.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err("Subagent modelSelection.options.reasoningLevel 不受支持".to_owned())
    }
}

fn normalize_subagent_model_selection(
    value: &Value,
) -> Result<Option<NormalizedSubagentModelSelection>, String> {
    if value.is_null() {
        return Ok(None);
    }
    // 前端对象和 agents-state.json 共用一套字段/none/options:{} 校验，避免
    // 设置页写入成功而 Runtime 目录无法解释同一份选择。
    let normalized = crate::extensions::agent_settings::normalize_model_selection_value(value)?;
    let (provider_id, model_id) = normalized
        .model
        .split_once("::")
        .ok_or_else(|| "Subagent modelSelection 规范化结果无效".to_owned())?;
    Ok(Some(NormalizedSubagentModelSelection {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
        reasoning_level: normalized.reasoning_effort,
    }))
}

fn normalize_subagent_model_reference(
    value: &str,
    reasoning_level: Option<&str>,
) -> Result<NormalizedSubagentModelSelection, String> {
    let (provider_id, model_id) = value
        .trim()
        .split_once("::")
        .ok_or_else(|| "Subagent 文件 model 必须使用 providerId::modelId".to_owned())?;
    let provider_id = normalized_subagent_identifier(
        Some(&Value::String(provider_id.to_owned())),
        "providerId",
        256,
    )?;
    let model_id =
        normalized_subagent_identifier(Some(&Value::String(model_id.to_owned())), "modelId", 512)?;
    Ok(NormalizedSubagentModelSelection {
        provider_id,
        model_id,
        reasoning_level: reasoning_level
            .map(normalized_subagent_reasoning_level)
            .transpose()?,
    })
}

impl NormalizedSubagentModelSelection {
    fn model_reference(&self) -> String {
        format!("{}::{}", self.provider_id, self.model_id)
    }

    fn into_value(self) -> Value {
        let mut value = json!({
            "providerId": self.provider_id,
            "modelId": self.model_id,
        });
        if let Some(reasoning_level) = self.reasoning_level {
            value["options"] = json!({"reasoningLevel": reasoning_level});
        }
        value
    }
}

fn subagent_model_selection_from_file(
    value: Value,
    effort: Option<&str>,
) -> Result<Option<Value>, String> {
    let mut selection = match value {
        Value::Null => return Err("Subagent effort 不能脱离 model 单独存在".to_owned()),
        Value::Object(_) => {
            return Err("Subagent 文件 model 必须使用 providerId::modelId 字符串".to_owned());
        }
        Value::String(value) if matches!(value.trim(), "inherit" | "sonnet" | "opus" | "haiku") => {
            // 这些是 catalog 的正式继承/插件别名语义；service 列表没有插件映射上下文，
            // 因而只省略未解析覆盖，不把别名伪装成可执行的 ModelSelection。
            return Ok(None);
        }
        Value::String(value) => normalize_subagent_model_reference(&value, None)?,
        _ => return Err("Subagent 文件 model 必须是字符串".to_owned()),
    };
    if let Some(effort) = effort {
        let effort = normalized_subagent_reasoning_level(effort)?;
        if selection
            .reasoning_level
            .as_deref()
            .is_some_and(|existing| existing != effort)
        {
            return Err("Subagent 文件 model 与 effort 推理等级冲突".to_owned());
        }
        selection.reasoning_level = Some(effort);
    }
    Ok(Some(selection.into_value()))
}

#[derive(Clone, Debug)]
struct SubagentEntry {
    id: String,
    name: String,
    description: String,
    prompt: String,
    inject_agents_md: bool,
    model_selection: Option<Value>,
    color: Option<String>,
    tools: Option<Vec<String>>,
    disallowed_tools: Vec<String>,
    max_turns: Option<u32>,
    allowed_write_dirs: Vec<String>,
    path: PathBuf,
    project_path: Option<PathBuf>,
    scope: &'static str,
    source: &'static str,
    read_only: bool,
}

/// 扫描根、返回 scope 与项目身份一起传递，避免把 workspace Agent 误报成 user。
type SubagentRoot = (PathBuf, &'static str, Option<PathBuf>);

fn parse_subagent_string_list(
    raw: &str,
    field: &str,
    maximum: usize,
) -> Result<Vec<String>, String> {
    let values = if raw.trim_start().starts_with('[') {
        serde_json::from_str::<Vec<String>>(raw)
            .map_err(|_| format!("Subagent {field} 必须是字符串数组"))?
    } else if raw.trim().is_empty() {
        Vec::new()
    } else {
        raw.split(',')
            .map(|value| value.trim().to_owned())
            .collect()
    };
    if values.len() > maximum {
        return Err(format!("Subagent {field} 超过数量限制"));
    }
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(format!("Subagent {field} 包含无效名称"));
        }
        if !seen.insert(value.to_ascii_lowercase()) {
            return Err(format!("Subagent {field} 包含重复名称"));
        }
        normalized.push(value);
    }
    Ok(normalized)
}

fn parse_subagent_color(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if matches!(
        value,
        "red" | "blue" | "green" | "yellow" | "purple" | "orange" | "pink" | "cyan"
    ) {
        Ok(value.to_owned())
    } else {
        Err("Subagent color 不受支持".to_owned())
    }
}

fn subagent_config_string_list(
    config: &Map<String, Value>,
    field: &str,
    maximum: usize,
) -> Result<Option<Vec<String>>, String> {
    let Some(value) = config.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let values = value
        .as_array()
        .ok_or_else(|| format!("Subagent config.{field} 必须是字符串数组"))?;
    if values.len() > maximum {
        return Err(format!("Subagent config.{field} 超过数量限制"));
    }
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .ok_or_else(|| format!("Subagent config.{field} 必须是字符串数组"))?
            .trim();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(format!("Subagent config.{field} 包含无效名称"));
        }
        if !seen.insert(value.to_ascii_lowercase()) {
            return Err(format!("Subagent config.{field} 包含重复名称"));
        }
        normalized.push(value.to_owned());
    }
    Ok(Some(normalized))
}

fn subagent_config_max_turns(config: &Map<String, Value>) -> Result<Option<u32>, String> {
    let Some(value) = config.get("maxTurns") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_u64()
        .ok_or_else(|| "Subagent config.maxTurns 必须是正整数".to_owned())?;
    let value = u32::try_from(value).map_err(|_| "Subagent config.maxTurns 超出范围".to_owned())?;
    if value == 0 {
        return Err("Subagent config.maxTurns 必须大于 0".to_owned());
    }
    Ok(Some(value))
}

fn subagent_config_color(config: &Map<String, Value>) -> Result<Option<String>, String> {
    let Some(value) = config.get("color") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| "Subagent config.color 必须是字符串".to_owned())?;
    Ok(Some(parse_subagent_color(value)?))
}

fn subagent_config_bool(
    config: &Map<String, Value>,
    field: &str,
    default: bool,
) -> Result<bool, String> {
    match config.get(field) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("Subagent config.{field} 必须是布尔值")),
    }
}

static BUILTIN_SUBAGENTS: &[(&str, &str, &str)] = &[
    (
        "general-purpose",
        "General-purpose agent for researching complex questions, searching for code, and executing multi-step tasks.",
        "",
    ),
    (
        "Explore",
        "Read-only search agent for broad fan-out searches.",
        "",
    ),
];

fn subagent_roots(app: &AppHandle, workspace: Option<&Path>) -> Result<Vec<SubagentRoot>, String> {
    let data = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    let mut roots = Vec::with_capacity(2);
    if let Some(workspace) = workspace {
        roots.push((
            subagent_workspace_scope_root(workspace)?,
            "workspace",
            Some(workspace.to_path_buf()),
        ));
    }
    roots.push((subagent_scope_root(&data, "user", None)?, "user", None));
    Ok(roots)
}

/// Subagent 的持久目录必须落在 KeenCode 应用数据根或已登记项目根下；
/// 这个纯路径函数同时供服务路由和离线 scope 隔离测试使用。
fn subagent_scope_root(
    data_root: &Path,
    scope: &str,
    workspace: Option<&Path>,
) -> Result<PathBuf, String> {
    match scope {
        // AgentRuntime 的全局目录契约是 dataDir/agents；channel 名称仍为 subagents。
        "user" => Ok(data_root.join("agents")),
        "workspace" => workspace
            .map(|root| root.join(".agents").join("agents"))
            .ok_or_else(|| "Subagent workspace scope 缺少已登记项目根".to_owned()),
        other => Err(format!("未知 Subagent scope：{other}")),
    }
}

/// 项目作用域必须沿已登记项目根解析，`.agents` 与 `agents` 不能借符号链接
/// 把 Agent 文件写到项目外；缺失目录仍允许按需创建，便于新项目首次保存。
fn subagent_workspace_scope_root(workspace: &Path) -> Result<PathBuf, String> {
    let workspace = fs::canonicalize(workspace)
        .map_err(|error| format!("无法规范化 Subagent 项目根：{error}"))?;
    if !workspace.is_dir() {
        return Err("Subagent 项目根必须是目录".to_owned());
    }
    let dot_agents = workspace.join(".agents");
    let agents = dot_agents.join("agents");
    for path in [&dot_agents, &agents] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Subagent 项目目录不允许符号链接：{}",
                    path.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("Subagent 项目目录必须是目录：{}", path.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("读取 Subagent 项目目录失败：{error}")),
        }
    }
    Ok(agents)
}

fn subagent_user_root(app: &AppHandle) -> Result<PathBuf, String> {
    let data = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    subagent_scope_root(&data, "user", None)
}

/// 只读取已经创建的 conversation 根，不因一次 catalog 查询凭空创建应用目录。
fn existing_conversation_workspace_root(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let data = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    let candidate = data.join("chat-workspaces").join("conversation");
    match fs::canonicalize(&candidate) {
        Ok(root) if root.is_dir() => Ok(Some(root)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("读取 conversation workspace 失败：{error}")),
    }
}

fn subagent_list_project_argument(
    args: &Value,
    mode: &str,
    conversation_root: Option<&Path>,
) -> Result<Option<String>, String> {
    match mode {
        "settingsUserOnly" => Ok(None),
        "allRuntimeScopes" | "default" => {
            let project = project_argument(args)?;
            // 无项目草稿使用应用数据下的 conversation 根；这里仅停止 workspace/plugin
            // 扫描，仍让 subagent_entries(None) 返回内置与 user catalog。任意其他未登记
            // 路径继续报错，不能把普通 cwd 当成受信任项目。
            if conversation_root.is_some_and(|root| {
                crate::workspace::conversation_read_root(&project, root).is_ok()
            }) {
                Ok(None)
            } else {
                Ok(Some(project))
            }
        }
        other => Err(format!("未知 Subagent list mode：{other}")),
    }
}

fn subagent_path_allowed(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| fs::canonicalize(root).is_ok_and(|root| path_within(&root, path)))
}

fn parse_subagent_file(
    path: &Path,
    scope: &'static str,
    project_path: Option<&Path>,
) -> Result<SubagentEntry, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("读取 Subagent 失败 {}：{error}", path.display()))?;
    if content.len() > 2 * 1024 * 1024 {
        return Err(format!("Subagent 超过 2 MB：{}", path.display()));
    }
    let mut name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("agent")
        .to_owned();
    let mut description = String::new();
    let mut inject_agents_md = true;
    let mut model_selection = None;
    let mut effort = None;
    let mut color = None;
    let mut tools = None;
    let mut disallowed_tools = Vec::new();
    let mut max_turns = None;
    let mut allowed_write_dirs = Vec::new();
    let mut body_start = 0usize;
    let lines = content.split_inclusive('\n').collect::<Vec<_>>();
    if lines
        .first()
        .is_some_and(|line| line.trim_end_matches(['\r', '\n']) == "---")
    {
        let mut end = None;
        for (index, line) in lines.iter().enumerate().skip(1) {
            let line = line.trim_end_matches(['\r', '\n']);
            if line == "---" {
                end = Some(index);
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                let value = value
                    .trim()
                    .trim_matches(|character| character == '"' || character == '\'');
                match key.trim() {
                    "name" if !value.is_empty() => name = value.to_owned(),
                    "description" => description = value.to_owned(),
                    "injectAgentsMd" => {
                        inject_agents_md = value
                            .parse::<bool>()
                            .map_err(|_| "Subagent injectAgentsMd 必须是布尔值".to_owned())?;
                    }
                    "color" => color = Some(parse_subagent_color(value)?),
                    "model" | "modelSelection" => {
                        model_selection = Some(
                            serde_json::from_str(value)
                                .unwrap_or_else(|_| Value::String(value.to_owned())),
                        )
                    }
                    "effort" => effort = Some(value.to_owned()),
                    "tools" => tools = Some(parse_subagent_string_list(value, "tools", 128)?),
                    "disallowedTools" => {
                        disallowed_tools =
                            parse_subagent_string_list(value, "disallowedTools", 128)?
                    }
                    "maxTurns" => {
                        let turns = value
                            .parse::<u32>()
                            .map_err(|_| "Subagent maxTurns 必须是正整数".to_owned())?;
                        if turns == 0 {
                            return Err("Subagent maxTurns 必须大于 0".to_owned());
                        }
                        max_turns = Some(turns);
                    }
                    "allowedWriteDirs" => {
                        allowed_write_dirs =
                            parse_subagent_string_list(value, "allowedWriteDirs", 64)?
                    }
                    _ => {}
                }
            }
        }
        if let Some(end) = end {
            body_start = lines.iter().take(end + 1).map(|line| line.len()).sum();
        }
    }
    let prompt = content
        .get(body_start..)
        .unwrap_or_default()
        .trim_start()
        .to_owned();
    let model_selection = match model_selection {
        Some(value) => subagent_model_selection_from_file(value, effort.as_deref())?,
        None if effort.is_some() => {
            return Err("Subagent effort 不能脱离 model 单独存在".to_owned());
        }
        None => None,
    };
    Ok(SubagentEntry {
        id: format!("user:{scope}:{}", name.to_ascii_lowercase()),
        name,
        description,
        prompt,
        inject_agents_md,
        model_selection,
        color,
        tools,
        disallowed_tools,
        max_turns,
        allowed_write_dirs,
        path: path.to_path_buf(),
        project_path: project_path.map(Path::to_path_buf),
        scope,
        source: "user",
        read_only: false,
    })
}

fn collect_subagent_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取 Subagent 目录失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|error| format!("读取 Subagent 目录失败：{error}"))?
    {
        let entry = entry.map_err(|error| format!("读取 Subagent 目录项失败：{error}"))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_subagent_files(&path, output)?;
        } else if file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("md")
        {
            if output.len() >= MAX_SEARCH_ENTRIES {
                return Err("Subagent 数量超过限制".to_owned());
            }
            output.push(path);
        }
    }
    Ok(())
}

fn subagent_state(app: &AppHandle) -> Result<Map<String, Value>, String> {
    let data_root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    crate::extensions::agent_settings::read_agent_settings_value(&data_root)?
        .as_object()
        .cloned()
        .ok_or_else(|| "Subagent 配置必须是对象".to_owned())
}

fn write_subagent_state(app: &AppHandle, state: Map<String, Value>) -> Result<(), String> {
    let data_root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    crate::extensions::agent_settings::write_agent_settings_value(&data_root, &Value::Object(state))
}

fn subagent_value(entry: &SubagentEntry, enabled: bool, override_model: Option<Value>) -> Value {
    let model_selection = override_model
        .clone()
        .or_else(|| entry.model_selection.clone());
    let mut value = json!({
        "id": entry.id,
        "name": entry.name,
        "description": entry.description,
        "systemPrompt": entry.prompt,
        "injectAgentsMd": entry.inject_agents_md,
        "modelSelection": model_selection,
        "modelSelectionOverride": override_model,
        "path": if entry.path.as_os_str().is_empty() { Value::String(format!("built-in:{}", entry.name)) } else { Value::String(crate::path_utils::path_to_frontend(&entry.path)) },
        "scope": entry.scope,
        "source": entry.source,
        "enabled": enabled,
        "readOnly": entry.read_only,
    });
    if let Some(project_path) = &entry.project_path {
        value["projectPath"] = Value::String(crate::path_utils::path_to_frontend(project_path));
    }
    if let Some(color) = &entry.color {
        value["color"] = Value::String(color.clone());
    }
    if let Some(tools) = &entry.tools {
        value["tools"] = json!(tools);
    }
    if !entry.disallowed_tools.is_empty() {
        value["disallowedTools"] = json!(entry.disallowed_tools);
    }
    if let Some(max_turns) = entry.max_turns {
        value["maxTurns"] = json!(max_turns);
    }
    if !entry.allowed_write_dirs.is_empty() {
        value["allowedWriteDirs"] = json!(entry.allowed_write_dirs);
    }
    value
}

fn subagent_entries(
    app: &AppHandle,
    workspace: Option<&Path>,
) -> Result<Vec<SubagentEntry>, String> {
    let mut entries = BUILTIN_SUBAGENTS
        .iter()
        .map(|(name, description, prompt)| SubagentEntry {
            id: format!("built-in:{name}"),
            name: (*name).to_owned(),
            description: (*description).to_owned(),
            prompt: (*prompt).to_owned(),
            inject_agents_md: true,
            model_selection: None,
            color: None,
            tools: None,
            disallowed_tools: Vec::new(),
            max_turns: None,
            allowed_write_dirs: Vec::new(),
            path: PathBuf::new(),
            project_path: None,
            scope: "built-in",
            source: "built-in",
            read_only: true,
        })
        .collect::<Vec<_>>();
    let mut names = entries
        .iter()
        .map(|entry| entry.name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for (root, scope, project_path) in subagent_roots(app, workspace)? {
        let mut files = Vec::new();
        collect_subagent_files(&root, &mut files)?;
        files.sort();
        for path in files {
            let entry = parse_subagent_file(&path, scope, project_path.as_deref())?;
            if names.insert(entry.name.to_ascii_lowercase()) {
                entries.push(entry);
            }
        }
    }
    // 插件 Agent 必须来自已启用插件的真实运行时快照；不能用空数组掩盖本地插件定义。
    let Some(workspace) = workspace else {
        return Ok(entries);
    };
    let snapshot = crate::extensions::plugin_runtime_snapshot(app, workspace)?;
    for plugin in snapshot.plugins {
        let namespace = plugin
            .id
            .runtime_namespace()
            .map_err(|error| error.to_string())?;
        for component in plugin.agents {
            let mut entry = parse_subagent_file(&component.path, "plugin", Some(workspace))?;
            let stem = component
                .path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("插件 Agent 文件名无效：{}", component.path.display()))?;
            entry.name = format!("{namespace}:{stem}");
            entry.id = crate::extensions::agent_settings::plugin_agent_id(&plugin.id, stem)?;
            entry.source = "plugin";
            entry.scope = "plugin";
            entry.read_only = true;
            if names.insert(entry.name.to_ascii_lowercase()) {
                entries.push(entry);
            }
        }
    }
    Ok(entries)
}

fn subagent_config(args: &Value) -> Result<&Map<String, Value>, String> {
    object(args)?
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| "Subagent 缺少 config 对象".to_owned())
}

fn subagent_config_string(
    config: &Map<String, Value>,
    name: &str,
    required: bool,
) -> Result<Option<String>, String> {
    match config.get(name) {
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("Subagent config.{name} 必须是字符串"))
            .and_then(|value| {
                if required && value.trim().is_empty() {
                    Err(format!("Subagent config.{name} 不能为空"))
                } else {
                    Ok(Some(value))
                }
            }),
        None if required => Err(format!("Subagent 缺少 config.{name}")),
        None => Ok(None),
    }
}

fn subagent_target_root(
    app: &AppHandle,
    args: &Value,
) -> Result<(PathBuf, &'static str, Option<PathBuf>), String> {
    let scope = object(args)?
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("user");
    match scope {
        "user" => Ok((subagent_user_root(app)?, "user", None)),
        "workspace" => {
            let workspace = project_argument(args)?;
            let workspace = registered_root(app, &workspace)?;
            Ok((
                subagent_workspace_scope_root(&workspace)?,
                "workspace",
                Some(workspace),
            ))
        }
        other => Err(format!("未知 Subagent scope：{other}")),
    }
}

fn subagent_file_name(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim().trim_start_matches('/');
    if raw.is_empty() || raw.len() > 128 || raw.chars().any(char::is_control) {
        return Err("Subagent 名称不能为空、过长或包含控制字符".to_owned());
    }
    let mut relative = PathBuf::new();
    for part in raw.split(['/', '\\', ':']) {
        if part.is_empty() || part == "." || part == ".." {
            return Err("Subagent 名称包含无效路径片段".to_owned());
        }
        relative.push(part.to_ascii_lowercase());
    }
    relative.set_extension("md");
    Ok(relative)
}

/// 创建或更新前先校验 Agent 根和目标父目录，避免通过目录/文件符号链接逃出
/// 应用数据根或已登记项目根；删除路径则在 `subagent_path_allowed` 中再次校验。
fn subagent_target_file(root: &Path, name: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|error| format!("创建 Subagent 目录失败：{error}"))?;
    let metadata =
        fs::symlink_metadata(root).map_err(|error| format!("读取 Subagent 目录失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("Subagent 根目录必须是普通目录：{}", root.display()));
    }
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("规范化 Subagent 目录失败：{error}"))?;
    let path = root.join(subagent_file_name(name)?);
    let parent = path
        .parent()
        .ok_or_else(|| format!("Subagent 路径没有父目录：{}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("创建 Subagent 目录失败：{error}"))?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| format!("规范化 Subagent 目录失败：{error}"))?;
    if !path_within(&canonical_root, &canonical_parent) {
        return Err("Subagent 目标目录超出允许范围".to_owned());
    }
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(format!("Subagent 目标必须是普通文件：{}", path.display()));
    }
    Ok(path)
}

fn write_subagent_file(path: &Path, config: &Map<String, Value>) -> Result<(), String> {
    let name = subagent_config_string(config, "name", true)?.ok_or("Subagent 名称不能为空")?;
    if name.len() > 128
        || name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("Subagent 名称只能包含 ASCII 字母、数字、连字符和下划线".to_owned());
    }
    let prompt = subagent_config_string(config, "systemPrompt", true)?
        .or_else(|| {
            subagent_config_string(config, "prompt", false)
                .ok()
                .flatten()
        })
        .ok_or("Subagent systemPrompt 不能为空")?;
    if prompt.len() > 512 * 1024 {
        return Err("Subagent systemPrompt 超过 512 KiB".to_owned());
    }
    let description = subagent_config_string(config, "description", true)?
        .ok_or("Subagent description 不能为空")?;
    let inject_agents_md = subagent_config_bool(config, "injectAgentsMd", true)?;
    let model_selection = config
        .get("modelSelection")
        .map(normalize_subagent_model_selection)
        .transpose()?
        .flatten();
    let color = subagent_config_color(config)?;
    let tools = subagent_config_string_list(config, "tools", 128)?;
    let disallowed_tools = subagent_config_string_list(config, "disallowedTools", 128)?;
    let max_turns = subagent_config_max_turns(config)?;
    let allowed_write_dirs = subagent_config_string_list(config, "allowedWriteDirs", 64)?;
    let mut content = String::from("---\n");
    content.push_str(&format!(
        "name: {}\n",
        serde_json::to_string(&name).unwrap_or_default()
    ));
    content.push_str(&format!(
        "description: {}\n",
        serde_json::to_string(&description).unwrap_or_default()
    ));
    if let Some(color) = color {
        content.push_str(&format!(
            "color: {}\n",
            serde_json::to_string(&color).unwrap_or_default()
        ));
    }
    content.push_str(&format!("injectAgentsMd: {inject_agents_md}\n"));
    if let Some(model) = model_selection {
        content.push_str(&format!(
            "model: {}\n",
            serde_json::to_string(&model.model_reference()).unwrap_or_default()
        ));
        if let Some(reasoning_level) = model.reasoning_level {
            content.push_str(&format!(
                "effort: {}\n",
                serde_json::to_string(&reasoning_level).unwrap_or_default()
            ));
        }
    }
    if let Some(tools) = tools {
        content.push_str(&format!(
            "tools: {}\n",
            serde_json::to_string(&tools).unwrap_or_default()
        ));
    }
    if let Some(disallowed_tools) = disallowed_tools {
        content.push_str(&format!(
            "disallowedTools: {}\n",
            serde_json::to_string(&disallowed_tools).unwrap_or_default()
        ));
    }
    if let Some(max_turns) = max_turns {
        content.push_str(&format!("maxTurns: {max_turns}\n"));
    }
    if let Some(allowed_write_dirs) = allowed_write_dirs {
        content.push_str(&format!(
            "allowedWriteDirs: {}\n",
            serde_json::to_string(&allowed_write_dirs).unwrap_or_default()
        ));
    }
    content.push_str("---\n\n");
    content.push_str(prompt.trim_start());
    content.push('\n');
    crate::extensions::validate_agent_document(&content)
        .map_err(|error| format!("生成的子智能体定义无效：{error}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建 Subagent 目录失败：{error}"))?;
    }
    crate::storage::atomic_write_private(path, content.as_bytes())
        .map_err(|error| format!("写入 Subagent 失败：{error}"))
}

/// 持久化成功后刷新已登记项目的候选；已运行 Turn 继续使用原有冻结候选。
async fn refresh_subagent_runtime(app: &AppHandle) -> Result<(), String> {
    let runtime = app
        .try_state::<Arc<crate::agent_runtime::AgentRuntime>>()
        .ok_or_else(|| "Agent Runtime 尚未初始化，无法刷新 Subagent 候选".to_owned())?;
    crate::extensions::refresh_known_runtime_projects(app, runtime.inner()).await
}

async fn subagents_call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "list" => {
            let mode = object(&args)?
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("default");
            // 设置页的 user scope 是应用级资源，不应因为当前没有项目而失败；
            // 运行时/项目列表则必须通过登记表获得规范项目根，避免把普通 cwd 当成项目。
            let conversation_root = if matches!(mode, "allRuntimeScopes" | "default") {
                existing_conversation_workspace_root(app)?
            } else {
                None
            };
            let workspace =
                subagent_list_project_argument(&args, mode, conversation_root.as_deref())?
                    .map(|project| registered_root(app, &project))
                    .transpose()?;
            let mut state = subagent_state(app)?;
            let disabled = state
                .remove("disabledAgentIds")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_str().map(|value| value.to_ascii_lowercase()))
                .collect::<BTreeSet<_>>();
            let built_in_overrides = state
                .get("builtInModelSelectionOverrides")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let plugin_overrides = state
                .get("pluginAgentModelSelectionOverrides")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let entries = subagent_entries(app, workspace.as_deref())?;
            let values = entries
                .iter()
                .filter(|entry| {
                    mode != "settingsUserOnly" || matches!(entry.source, "built-in" | "user")
                })
                .map(|entry| {
                    let override_model = match entry.source {
                        "built-in" => built_in_overrides
                            .get(&entry.name)
                            .or_else(|| built_in_overrides.get(&entry.name.to_ascii_lowercase()))
                            .cloned(),
                        "plugin" => plugin_overrides.get(&entry.id).cloned(),
                        _ => None,
                    };
                    let enabled = entry.source != "user"
                        || (!disabled.contains(&entry.id.to_ascii_lowercase())
                            && !disabled.contains(&entry.name.to_ascii_lowercase()));
                    subagent_value(entry, enabled, override_model)
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "agents": values,
                "userAgents": values.iter().filter(|value| value["source"] == "user").cloned().collect::<Vec<_>>(),
                "pluginAgents": values.iter().filter(|value| value["source"] == "plugin").cloned().collect::<Vec<_>>(),
                "capability": {"userScopeAvailable": true},
                "diagnostics": [],
            }))
        }
        "setEnabled" => {
            let id =
                required_string(&args, "agentId").or_else(|_| required_string(&args, "name"))?;
            if id.starts_with("built-in:") || id.starts_with("plugin:") {
                return unsupported(
                    "subagents",
                    method,
                    "内置与插件 Agent 的启用状态由宿主/插件运行时管理",
                );
            }
            if !id.starts_with("user:user:") {
                return Err("Subagent 启用状态只允许修改 user scope".to_owned());
            }
            let enabled = object(&args)?
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| "Subagent setEnabled 缺少 enabled".to_owned())?;
            let extensions = app
                .try_state::<crate::extensions::ExtensionsState>()
                .ok_or_else(|| "扩展状态尚未初始化，无法更新 Subagent".to_owned())?;
            let io_guard = extensions.lock_io()?;
            let mut state = subagent_state(app)?;
            let mut disabled = state
                .remove("disabledAgentIds")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect::<BTreeSet<_>>();
            if enabled {
                disabled.retain(|value| !value.eq_ignore_ascii_case(&id));
            } else if !disabled.iter().any(|value| value.eq_ignore_ascii_case(&id)) {
                disabled.insert(id);
            }
            state.insert(
                "disabledAgentIds".to_owned(),
                Value::Array(disabled.into_iter().map(Value::String).collect()),
            );
            write_subagent_state(app, state)?;
            drop(io_guard);
            refresh_subagent_runtime(app).await?;
            Ok(Value::Null)
        }
        "setBuiltInModelOverride" => {
            let id = required_string(&args, "agentName")
                .or_else(|_| required_string(&args, "agentId"))
                .or_else(|_| required_string(&args, "name"))?;
            let model = object(&args)?.get("modelSelection");
            let extensions = app
                .try_state::<crate::extensions::ExtensionsState>()
                .ok_or_else(|| "扩展状态尚未初始化，无法更新 Subagent".to_owned())?;
            let io_guard = extensions.lock_io()?;
            let mut state = subagent_state(app)?;
            let overrides = state
                .entry("builtInModelSelectionOverrides".to_owned())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| {
                    "Subagent 配置 builtInModelSelectionOverrides 必须是对象".to_owned()
                })?;
            if let Some(model) = model.filter(|value| !value.is_null()) {
                let model =
                    crate::extensions::agent_settings::normalize_model_selection_value(model)?;
                overrides.insert(
                    id,
                    crate::extensions::agent_settings::model_selection_value(&model),
                );
            } else {
                overrides.remove(&id);
            }
            write_subagent_state(app, state)?;
            drop(io_guard);
            refresh_subagent_runtime(app).await?;
            Ok(Value::Null)
        }
        "setPluginAgentModelOverride" => {
            let id = required_string(&args, "agentId")?;
            if !id.starts_with("plugin:") {
                return Err("插件 Agent id 必须使用 plugin: 命名空间".to_owned());
            }
            let model = object(&args)?.get("modelSelection");
            let extensions = app
                .try_state::<crate::extensions::ExtensionsState>()
                .ok_or_else(|| "扩展状态尚未初始化，无法更新 Subagent".to_owned())?;
            let io_guard = extensions.lock_io()?;
            let mut state = subagent_state(app)?;
            let overrides = state
                .entry("pluginAgentModelSelectionOverrides".to_owned())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| {
                    "Subagent 配置 pluginAgentModelSelectionOverrides 必须是对象".to_owned()
                })?;
            if let Some(model) = model.filter(|value| !value.is_null()) {
                let model =
                    crate::extensions::agent_settings::normalize_model_selection_value(model)?;
                overrides.insert(
                    id,
                    crate::extensions::agent_settings::model_selection_value(&model),
                );
            } else {
                overrides.remove(&id);
            }
            write_subagent_state(app, state)?;
            drop(io_guard);
            refresh_subagent_runtime(app).await?;
            Ok(Value::Null)
        }
        "getPrimaryUserAgentsDirectory" => {
            let path = subagent_user_root(app)?;
            fs::create_dir_all(&path)
                .map_err(|error| format!("创建 Subagent 目录失败：{error}"))?;
            Ok(json!({"path": crate::path_utils::path_to_frontend(&path)}))
        }
        "createAgent" | "updateAgent" => {
            let config = subagent_config(&args)?;
            let name =
                subagent_config_string(config, "name", true)?.ok_or("Subagent 名称不能为空")?;
            validate_component(&name, "Subagent 名称")?;
            if BUILTIN_SUBAGENTS
                .iter()
                .any(|(builtin, _, _)| builtin.eq_ignore_ascii_case(&name))
            {
                return Err("不能修改内置 Subagent".to_owned());
            }
            let (root, scope, project_path) = subagent_target_root(app, &args)?;
            let path = subagent_target_file(&root, &name)?;
            let extensions = app
                .try_state::<crate::extensions::ExtensionsState>()
                .ok_or_else(|| "扩展状态尚未初始化，无法更新 Subagent".to_owned())?;
            let io_guard = extensions.lock_io()?;
            if method == "createAgent" && path.exists() {
                return Err(format!("Subagent 已存在：{name}"));
            }
            let mut old_path_to_remove = None;
            if method == "updateAgent"
                && let Some(old_path) = object(&args)?.get("oldFilePath").and_then(Value::as_str)
            {
                let old_path = fs::canonicalize(old_path)
                    .map_err(|error| format!("Subagent 原路径无效：{error}"))?;
                let canonical_root = fs::canonicalize(&root).unwrap_or(root.clone());
                if !path_within(&canonical_root, &old_path) {
                    return Err("Subagent 原路径超出允许目录".to_owned());
                }
                old_path_to_remove = Some(old_path);
            }
            write_subagent_file(&path, config)?;
            let canonical_target = fs::canonicalize(&path).unwrap_or(path.clone());
            if let Some(old_path) = old_path_to_remove
                && old_path != canonical_target
            {
                fs::remove_file(old_path)
                    .map_err(|error| format!("删除旧 Subagent 失败：{error}"))?;
            }
            let entry = parse_subagent_file(&path, scope, project_path.as_deref())?;
            drop(io_guard);
            refresh_subagent_runtime(app).await?;
            Ok(json!({"agent": subagent_value(&entry, true, None)}))
        }
        "deleteAgent" => {
            let raw_path = object(&args)?
                .get("filePath")
                .and_then(Value::as_str)
                .ok_or_else(|| "Subagent deleteAgent 缺少 filePath".to_owned())?;
            let path = fs::canonicalize(raw_path)
                .map_err(|error| format!("Subagent 路径无效：{error}"))?;
            let user_root = subagent_user_root(app)?;
            let mut allowed_roots = vec![user_root];
            allowed_roots.extend(
                crate::workspace::project_records(app)?
                    .into_iter()
                    .filter_map(|project| registered_root(app, &project.path).ok())
                    .map(|root| root.join(".agents").join("agents")),
            );
            let allowed = subagent_path_allowed(&path, &allowed_roots);
            if !allowed {
                return Err("只允许删除 user/workspace Subagent".to_owned());
            }
            let extensions = app
                .try_state::<crate::extensions::ExtensionsState>()
                .ok_or_else(|| "扩展状态尚未初始化，无法更新 Subagent".to_owned())?;
            let io_guard = extensions.lock_io()?;
            fs::remove_file(path).map_err(|error| format!("删除 Subagent 失败：{error}"))?;
            drop(io_guard);
            refresh_subagent_runtime(app).await?;
            Ok(Value::Null)
        }
        _ => unsupported_method("subagents", method),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        McpStatusDescriptor, MediaPreviewLease, OwnedWorkflowWatchKind, WorkspaceHookFileInput,
        authorize_media_preview_path, begin_plugin_operation, cancel_plugin_operation,
        ensure_owned_workflow_watch_directory, file_watcher_event_payload, finish_plugin_operation,
        git_branch_comparison_value, git_branch_mutation_issue, git_commit_graph_value,
        git_diff_value, git_local_branches_value, git_refresh_value, mcp_native_record_value,
        mcp_status_snapshot, mcp_sync_candidate_value, media_preview_store, normalize_listen_args,
        normalize_service_args, now_ms, owned_workflow_watch_kind, parse_subagent_file,
        path_is_under_workflow_data_root, plugin_ids_in_marketplace,
        prompt_attachment_transfer_unsupported, read_mcp_servers, read_text_slice,
        requested_mcp_status_descriptors, resolve_resource_workspace_root_from_registration,
        same_workflow_watch_path, scan_skills, skills_list_value, subagent_list_project_argument,
        subagent_path_allowed, subagent_scope_root, subagent_target_file, subagent_value,
        subagent_workspace_scope_root, validate_hook_digest, validate_hook_trust_record,
        watcher_connection_matches, workspace_hook_bundle_digest,
        workspace_hook_declaration_digest, write_subagent_file,
    };
    use crate::workspace::{GitStatusResult, conversation_read_root, parse_git_status, run_git};
    use serde_json::{Map, Value, json};
    use std::time::{Duration, Instant};
    use std::{collections::BTreeSet, path::Path};

    #[test]
    fn workflow_watch_allowlist_accepts_only_owned_global_and_registered_project_dirs() {
        let temporary = tempfile::tempdir().unwrap();
        let data_root = temporary.path().join("keencode");
        let project_storage = data_root.join("projects").join("project-1");
        let project_dirs = vec![project_storage.join("workflows")];
        let global = data_root.join("workflows");
        let project_workflows = project_storage.join("workflows");

        assert_eq!(
            owned_workflow_watch_kind(&global, &data_root, &project_dirs),
            Some(OwnedWorkflowWatchKind::Global)
        );
        assert_eq!(
            owned_workflow_watch_kind(&project_workflows, &data_root, &project_dirs),
            Some(OwnedWorkflowWatchKind::Project)
        );
        assert_eq!(
            owned_workflow_watch_kind(
                &data_root
                    .join("projects")
                    .join("project-2")
                    .join("workflows"),
                &data_root,
                &project_dirs,
            ),
            None
        );
        assert_eq!(
            owned_workflow_watch_kind(
                &temporary
                    .path()
                    .join("user-workspace")
                    .join(".zcode/workflows"),
                &data_root,
                &project_dirs,
            ),
            None
        );
        assert!(path_is_under_workflow_data_root(
            &project_workflows,
            &data_root
        ));
        assert!(!path_is_under_workflow_data_root(
            &temporary.path().join("user-workspace"),
            &data_root
        ));

        let created =
            ensure_owned_workflow_watch_directory(&project_workflows, &data_root).unwrap();
        assert!(created.is_dir());
        assert!(!same_workflow_watch_path(
            Path::new("invalid/.."),
            Path::new("invalid/..")
        ));

        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let linked = data_root.join("linked-project");
        let symlink_created = {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&outside, &linked).is_ok()
            }
            #[cfg(windows)]
            {
                std::os::windows::fs::symlink_dir(&outside, &linked).is_ok()
            }
            #[cfg(not(any(unix, windows)))]
            {
                false
            }
        };
        if symlink_created {
            assert!(
                ensure_owned_workflow_watch_directory(&linked.join("workflows"), &data_root)
                    .is_err()
            );
        }
    }

    #[test]
    fn workflow_watch_connection_owner_is_exact_and_does_not_cross_release() {
        assert!(watcher_connection_matches(
            Some("connection-a"),
            Some("connection-a")
        ));
        assert!(!watcher_connection_matches(
            Some("connection-a"),
            Some("connection-b")
        ));
        assert!(!watcher_connection_matches(Some("connection-a"), None));
        assert!(watcher_connection_matches(None, None));
    }

    #[test]
    fn file_watcher_payload_reports_a_single_native_change() {
        let directory = Path::new("C:/owned/project");
        let payload = file_watcher_event_payload("watch-1", directory, Some("daily.json"));
        assert_eq!(payload["id"], "watch-1");
        assert_eq!(
            payload["dirPath"],
            crate::path_utils::path_to_frontend(directory)
        );
        assert_eq!(
            payload["changedPath"],
            crate::path_utils::path_to_frontend(&directory.join("daily.json"))
        );
    }

    #[test]
    fn read_text_slice_strips_only_absolute_utf8_bom_and_preserves_physical_range() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("bom-crlf.txt");
        let bytes = b"\xEF\xBB\xBFalpha\r\nbeta\r\n";
        std::fs::write(&path, bytes).unwrap();

        let (content, bytes_read, total, truncated, is_binary) =
            read_text_slice(&path, 0, bytes.len() as u64).unwrap();
        assert_eq!(content, "alpha\r\nbeta\r\n");
        assert_eq!(bytes_read, bytes.len());
        assert_eq!(total, bytes.len() as u64);
        assert!(!truncated);
        assert!(!is_binary);

        let (bounded_content, bounded_bytes_read, bounded_total, bounded_truncated, _) =
            read_text_slice(&path, 0, 8).unwrap();
        assert_eq!(bounded_content, "alpha");
        assert_eq!(bounded_bytes_read, 8);
        assert_eq!(bounded_total, bytes.len() as u64);
        assert!(bounded_truncated);

        let (partial_content, partial_bytes_read, partial_total, partial_truncated, _) =
            read_text_slice(&path, 1, 3).unwrap();
        assert_eq!(partial_content, String::from_utf8_lossy(&bytes[1..4]));
        assert_eq!(partial_bytes_read, 3);
        assert_eq!(partial_total, bytes.len() as u64);
        assert!(partial_truncated);

        let (offset_content, offset_bytes_read, offset_total, offset_truncated, offset_binary) =
            read_text_slice(&path, 3, (bytes.len() - 3) as u64).unwrap();
        assert_eq!(offset_content, "alpha\r\nbeta\r\n");
        assert_eq!(offset_bytes_read, bytes.len() - 3);
        assert_eq!(offset_total, bytes.len() as u64);
        assert!(!offset_truncated);
        assert!(!offset_binary);
    }

    #[test]
    fn git_refresh_fixture_preserves_real_staged_unstaged_untracked_and_renamed_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let run = |args: &[&str]| {
            let output = run_git(root, args).expect("git fixture command should start");
            assert!(
                output.status.success(),
                "git fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };

        run(&["init", "-b", "main"]);
        run(&["config", "user.name", "Native Fixture"]);
        run(&["config", "user.email", "native-fixture@example.invalid"]);
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(root.join("rename-me.txt"), "rename\n").unwrap();
        run(&["add", "--all"]);
        run(&["commit", "-m", "原生验收基线 / Native acceptance baseline"]);
        run(&["tag", "v1.0"]);
        let remote_path = temporary.path().join("native-remote.git");
        let remote_text = remote_path.to_string_lossy().into_owned();
        run(&["remote", "add", "origin", &remote_text]);
        run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);

        std::fs::write(root.join("tracked.txt"), "base\nunstaged\n").unwrap();
        std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
        std::fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
        run(&["add", "staged.txt"]);
        run(&["mv", "rename-me.txt", "renamed.txt"]);

        let status_output = run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
        let files = parse_git_status(root, &status_output.stdout).unwrap();
        let status = GitStatusResult {
            available: true,
            files,
            branch: Some("main".to_owned()),
            branches: vec!["main".to_owned()],
            additions: 2,
            deletions: 0,
            has_unstaged_changes: true,
            reason: None,
        };
        let refresh = git_refresh_value(
            &root.to_string_lossy(),
            &status,
            Some(root),
            Some(json!({
                "userName": "Native Fixture",
                "userEmail": "native-fixture@example.invalid",
                "nameSource": "git-config",
                "emailSource": "git-config",
                "scopeLabel": "local"
            })),
        );

        let staged = refresh["stagedChanges"].as_array().unwrap();
        let unstaged = refresh["unstagedChanges"].as_array().unwrap();
        assert!(staged.iter().any(|change| {
            change["repoRelativePath"] == "staged.txt" && change["section"] == "staged"
        }));
        assert!(staged.iter().any(|change| {
            change["repoRelativePath"] == "renamed.txt"
                && change["kind"] == "renamed"
                && change["isStaged"] == true
        }));
        assert!(unstaged.iter().any(|change| {
            change["repoRelativePath"] == "tracked.txt" && change["section"] == "unstaged"
        }));
        assert!(unstaged.iter().any(|change| {
            change["repoRelativePath"] == "untracked.txt"
                && change["section"] == "untracked"
                && change["isUntracked"] == true
        }));
        assert_eq!(refresh["summary"]["isRepository"], true);
        assert_eq!(refresh["summary"]["branchName"], "main");
        assert_eq!(
            refresh["identity"]["userEmail"],
            "native-fixture@example.invalid"
        );
        assert!(refresh.get("workingTree").is_none());

        let graph = git_commit_graph_value(root, 5, 0).unwrap();
        assert_eq!(graph["hasMore"], false);
        assert_eq!(
            graph["commits"][0]["subject"],
            "原生验收基线 / Native acceptance baseline"
        );
        assert!(graph["commits"][0]["hash"].as_str().is_some());
        assert_eq!(graph["commits"][0]["parents"].as_array().unwrap().len(), 0);
        let refs = graph["commits"][0]["refs"].as_array().unwrap();
        assert!(
            refs.iter()
                .any(|reference| { reference["name"] == "main" && reference["kind"] == "head" })
        );
        assert!(
            refs.iter()
                .any(|reference| { reference["name"] == "v1.0" && reference["kind"] == "tag" })
        );
        assert!(refs.iter().any(|reference| {
            reference["name"] == "origin/main" && reference["kind"] == "remote"
        }));

        let tracked_path = root.join("tracked.txt").to_string_lossy().into_owned();
        let unstaged_diff =
            git_diff_value(root, &tracked_path, "unstaged", None, 1_000_000).unwrap();
        assert_eq!(unstaged_diff["path"], tracked_path);
        assert_eq!(unstaged_diff["availability"], "patch");
        assert!(
            unstaged_diff["patch"]
                .as_str()
                .unwrap()
                .contains("unstaged")
        );
        let staged_diff = git_diff_value(root, "staged.txt", "staged", None, 1_000_000).unwrap();
        assert_eq!(staged_diff["availability"], "patch");
        assert!(staged_diff["patch"].as_str().unwrap().contains("staged"));

        let branch_result = git_local_branches_value(root, &status);
        assert_eq!(branch_result["headRefType"], "branch");
        assert_eq!(branch_result["currentBranchName"], "main");
        assert_eq!(branch_result["branches"][0]["name"], "main");
        assert_eq!(branch_result["branches"][0]["isCurrent"], true);
    }

    #[test]
    fn git_branch_comparison_fixture_emits_source_shape_from_real_history() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let run = |args: &[&str]| {
            let output = run_git(root, args).expect("git fixture command should start");
            assert!(
                output.status.success(),
                "git fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run(&["init", "-b", "main"]);
        run(&["config", "user.name", "Native Fixture"]);
        run(&["config", "user.email", "native-fixture@example.invalid"]);
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        run(&["add", "tracked.txt"]);
        run(&["commit", "-m", "原生验收分支基线 / Native branch baseline"]);
        run(&["switch", "-c", "feature"]);
        std::fs::write(root.join("tracked.txt"), "base\nfeature\n").unwrap();
        std::fs::write(root.join("feature.txt"), "feature\n").unwrap();
        run(&["add", "--all"]);
        run(&["commit", "-m", "原生验收分支变更 / Native branch change"]);

        let comparison = git_branch_comparison_value(root).unwrap();
        assert_eq!(comparison["baseRef"].as_str().unwrap().len(), 40);
        assert_eq!(comparison["headRef"].as_str().unwrap().len(), 40);
        assert_eq!(comparison["changes"].as_array().unwrap().len(), 2);
        assert!(
            comparison["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["repoRelativePath"] == "feature.txt")
        );
    }

    /// 分支失败结果使用真实 Git 输出分类，并把覆盖风险中的路径传给 Source。
    #[test]
    fn git_branch_mutation_issue_classifies_real_overwrite_and_known_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let run = |args: &[&str]| {
            let output = run_git(root, args).expect("git fixture command should start");
            assert!(
                output.status.success(),
                "git fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run(&["init", "-b", "main"]);
        run(&["config", "user.name", "Native Fixture"]);
        run(&["config", "user.email", "native-fixture@example.invalid"]);
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        run(&["add", "tracked.txt"]);
        run(&["commit", "-m", "分支冲突基线 / Branch conflict baseline"]);
        run(&["switch", "-c", "feature"]);
        std::fs::write(root.join("tracked.txt"), "feature\n").unwrap();
        run(&["add", "tracked.txt"]);
        run(&["commit", "-m", "分支冲突目标 / Branch conflict target"]);
        run(&["switch", "main"]);
        std::fs::write(root.join("tracked.txt"), "local\n").unwrap();
        let failed = run_git(root, &["switch", "feature"]).unwrap();
        assert!(!failed.status.success());
        let error = format!(
            "{}{}",
            String::from_utf8_lossy(&failed.stdout),
            String::from_utf8_lossy(&failed.stderr)
        );
        let issue = git_branch_mutation_issue(&error, "feature");
        assert_eq!(issue["code"], "tracked-changes-would-be-overwritten");
        assert_eq!(issue["paths"], json!(["tracked.txt"]));

        run(&["restore", "--worktree", "tracked.txt"]);
        run(&["switch", "-c", "untracked-target"]);
        std::fs::write(root.join("collision.txt"), "branch file\n").unwrap();
        run(&["add", "collision.txt"]);
        run(&[
            "commit",
            "-m",
            "未跟踪覆盖目标 / Untracked overwrite target",
        ]);
        run(&["switch", "main"]);
        std::fs::write(root.join("collision.txt"), "local untracked\n").unwrap();
        let untracked_failed = run_git(root, &["switch", "untracked-target"]).unwrap();
        assert!(!untracked_failed.status.success());
        let untracked_error = format!(
            "{}{}",
            String::from_utf8_lossy(&untracked_failed.stdout),
            String::from_utf8_lossy(&untracked_failed.stderr)
        );
        let untracked_issue = git_branch_mutation_issue(&untracked_error, "untracked-target");
        assert_eq!(
            untracked_issue["code"],
            "untracked-changes-would-be-overwritten"
        );
        assert_eq!(untracked_issue["paths"], json!(["collision.txt"]));

        assert_eq!(
            git_branch_mutation_issue("fatal: a branch named 'feature' already exists", "feature")
                ["code"],
            "branch-already-exists"
        );
        assert_eq!(
            git_branch_mutation_issue("fatal: invalid reference: missing", "missing")["code"],
            "target-branch-not-found"
        );
        assert_eq!(
            git_branch_mutation_issue("error: cannot switch branch while cherry-picking", "main")["code"],
            "operation-in-progress"
        );
    }

    #[test]
    fn conversation_subagents_read_rejects_other_unregistered_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let conversation = temporary.path().join("conversation");
        let nested = conversation.join("nested");
        let sibling = temporary.path().join("another-workspace");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let root = std::fs::canonicalize(&conversation).unwrap();
        let alias = conversation.join(".");
        assert_eq!(
            conversation_read_root(&alias.to_string_lossy(), &root).unwrap(),
            root
        );
        assert!(conversation_read_root(&nested.to_string_lossy(), &root).is_err());
        assert!(conversation_read_root(&sibling.to_string_lossy(), &root).is_err());
        assert!(conversation_read_root(&temporary.path().to_string_lossy(), &root).is_err());
    }

    #[test]
    fn skills_list_reads_user_fixture_for_internal_conversation_scope() {
        let temporary = tempfile::tempdir().unwrap();
        let data_root = temporary.path().join("data");
        let conversation = data_root.join("chat-workspaces").join("conversation");
        let user_skill = data_root.join("skills").join("native-skill-fixture");
        std::fs::create_dir_all(&conversation).unwrap();
        std::fs::create_dir_all(&user_skill).unwrap();
        std::fs::write(
            user_skill.join("SKILL.md"),
            "---\nname: native-skill-fixture\ndescription: Native fixture\n---\n# Fixture\n",
        )
        .unwrap();

        let conversation_root = std::fs::canonicalize(&conversation).unwrap();
        let conversation_alias = conversation.join(".");
        assert_eq!(
            conversation_read_root(&conversation_alias.to_string_lossy(), &conversation_root)
                .unwrap(),
            conversation_root
        );

        // 这里使用与 skills_call 相同的真实目录扫描和列表投影；user Skill
        // 必须来自应用 data 根，不能因为当前是 conversation scope 就丢失。
        let roots = vec![
            (
                conversation_root.join(".agents").join("skills"),
                "workspace",
            ),
            (conversation_root.join(".zcode").join("skills"), "workspace"),
            (data_root.join("skills"), "user"),
        ];
        let skills = scan_skills(&roots).unwrap();
        let value = skills_list_value(&skills, &BTreeSet::new());
        let entries = value["skills"].as_array().unwrap();
        let fixture = entries
            .iter()
            .find(|entry| entry["id"] == "native-skill-fixture")
            .expect("user Skill should be listed from the isolated data root");
        assert_eq!(fixture["scope"], "user");
        assert_eq!(fixture["enabled"], true);
        assert_eq!(
            fixture["path"],
            crate::path_utils::path_to_frontend(&user_skill.join("SKILL.md"))
        );
        assert_eq!(value["capability"]["userScopeAvailable"], true);

        let sibling = data_root.join("another-workspace");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(conversation_read_root(&sibling.to_string_lossy(), &conversation_root).is_err());
    }

    #[test]
    fn subagent_scope_storage_keeps_user_and_registered_project_files_separate() {
        let temporary = tempfile::tempdir().unwrap();
        let data_root = temporary.path().join("keencode-data");
        let project_root = temporary.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();

        let user_root = subagent_scope_root(&data_root, "user", None).unwrap();
        let project_agents =
            subagent_scope_root(&data_root, "workspace", Some(&project_root)).unwrap();
        assert_eq!(user_root, data_root.join("agents"));
        assert_eq!(project_agents, project_root.join(".agents").join("agents"));
        assert_eq!(
            subagent_workspace_scope_root(&project_root).unwrap(),
            std::fs::canonicalize(&project_root)
                .unwrap()
                .join(".agents")
                .join("agents")
        );
        assert!(subagent_scope_root(&data_root, "workspace", None).is_err());
        assert!(subagent_scope_root(&data_root, "global", None).is_err());
        assert_eq!(
            subagent_list_project_argument(
                &json!({"mode": "settingsUserOnly"}),
                "settingsUserOnly",
                None,
            )
            .unwrap(),
            None
        );
        assert!(subagent_list_project_argument(&json!({}), "allRuntimeScopes", None).is_err());
        assert!(subagent_list_project_argument(&json!({}), "unsupported", None).is_err());

        // conversation 根只允许返回内置与 user catalog；相邻目录仍必须要求登记项目。
        let conversation_root = temporary.path().join("conversation");
        let other_root = temporary.path().join("another-workspace");
        std::fs::create_dir_all(&conversation_root).unwrap();
        std::fs::create_dir_all(&other_root).unwrap();
        let canonical_conversation_root = std::fs::canonicalize(&conversation_root).unwrap();
        let conversation_alias = conversation_root.join(".");
        assert_eq!(
            subagent_list_project_argument(
                &json!({"workspacePath": conversation_alias.to_string_lossy().to_string()}),
                "allRuntimeScopes",
                Some(&canonical_conversation_root),
            )
            .unwrap(),
            None
        );
        assert_eq!(
            subagent_list_project_argument(
                &json!({"workspacePath": other_root.to_string_lossy().to_string()}),
                "allRuntimeScopes",
                Some(&canonical_conversation_root),
            )
            .unwrap(),
            Some(other_root.to_string_lossy().into_owned())
        );

        let config = json!({
            "name": "reviewer",
            "description": "Review changes",
            "systemPrompt": "Read the diff and report risks."
        })
        .as_object()
        .cloned()
        .unwrap();
        let user_path = subagent_target_file(&user_root, "reviewer").unwrap();
        let project_path = subagent_target_file(&project_agents, "reviewer").unwrap();
        write_subagent_file(&user_path, &config).unwrap();
        write_subagent_file(&project_path, &config).unwrap();

        let user_entry = parse_subagent_file(&user_path, "user", None).unwrap();
        let project_entry =
            parse_subagent_file(&project_path, "workspace", Some(&project_root)).unwrap();
        assert_eq!(user_entry.scope, "user");
        assert!(user_entry.project_path.is_none());
        assert_eq!(project_entry.scope, "workspace");
        assert_eq!(
            project_entry.project_path.as_deref(),
            Some(project_root.as_path())
        );
        assert!(
            subagent_value(&user_entry, true, None)
                .get("projectPath")
                .is_none()
        );
        assert_eq!(
            subagent_value(&project_entry, true, None)["projectPath"],
            crate::path_utils::path_to_frontend(&project_root)
        );

        let user_file = std::fs::canonicalize(&user_path).unwrap();
        let project_file = std::fs::canonicalize(&project_path).unwrap();
        let outside = temporary.path().join("outside.md");
        std::fs::write(&outside, "outside").unwrap();
        let outside_file = std::fs::canonicalize(outside).unwrap();
        let allowed_roots = vec![user_root, project_agents];
        assert!(subagent_path_allowed(&user_file, &allowed_roots));
        assert!(subagent_path_allowed(&project_file, &allowed_roots));
        assert!(!subagent_path_allowed(&outside_file, &allowed_roots));

        std::fs::remove_file(&user_file).unwrap();
        assert!(!user_file.exists());
        assert!(project_file.exists());
    }

    #[test]
    fn subagent_model_selection_and_runtime_fields_round_trip_through_markdown() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("reviewer.md");
        let config = json!({
            "name": "reviewer",
            "description": "Review changes",
            "systemPrompt": "Read the diff and report risks.",
            "injectAgentsMd": false,
            "color": "cyan",
            "modelSelection": {
                "providerId": "native-live-deepseek",
                "modelId": "deepseek-v4.1-flash",
                "options": {"reasoningLevel": "none"}
            },
            "tools": ["Read", "Grep"],
            "disallowedTools": ["Write"],
            "maxTurns": 8,
            "allowedWriteDirs": ["reports"]
        })
        .as_object()
        .cloned()
        .unwrap();

        write_subagent_file(&path, &config).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("model: \"native-live-deepseek::deepseek-v4.1-flash\""));
        assert!(content.contains("effort: \"none\""));
        assert!(content.contains("tools: [\"Read\",\"Grep\"]"));

        let entry = parse_subagent_file(&path, "user", None).unwrap();
        assert_eq!(
            entry.model_selection,
            Some(json!({
                "providerId": "native-live-deepseek",
                "modelId": "deepseek-v4.1-flash",
                "options": {"reasoningLevel": "none"}
            }))
        );
        assert_eq!(entry.color.as_deref(), Some("cyan"));
        assert!(!entry.inject_agents_md);
        assert_eq!(
            entry.tools,
            Some(vec!["Read".to_owned(), "Grep".to_owned()])
        );
        assert_eq!(entry.disallowed_tools, vec!["Write"]);
        assert_eq!(entry.max_turns, Some(8));
        assert_eq!(entry.allowed_write_dirs, vec!["reports"]);
    }

    #[test]
    fn subagent_invalid_model_selection_is_rejected_before_file_write() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("reviewer.md");
        let initial = "---\nname: reviewer\ndescription: Existing\n---\nKeep this prompt\n";
        std::fs::write(&path, initial).unwrap();
        let config = json!({
            "name": "reviewer",
            "description": "Updated",
            "systemPrompt": "Do not write.",
            "modelSelection": {
                "providerId": "provider",
                "modelId": "model",
                "unexpected": true
            }
        })
        .as_object()
        .cloned()
        .unwrap();

        assert!(write_subagent_file(&path, &config).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);
    }

    #[test]
    fn subagent_writer_reuses_catalog_schema_before_atomic_replace() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("reviewer.md");
        let initial = "---\nname: reviewer\ndescription: Existing\n---\nKeep this prompt\n";
        std::fs::write(&path, initial).unwrap();

        let mut invalid_dirs = serde_json::from_value::<Map<String, Value>>(json!({
            "name": "reviewer",
            "description": "Updated",
            "systemPrompt": "Do not write.",
            "allowedWriteDirs": ["../outside"]
        }))
        .unwrap();
        assert!(write_subagent_file(&path, &invalid_dirs).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);

        invalid_dirs.insert("allowedWriteDirs".to_owned(), json!(["reports"]));
        invalid_dirs.insert("tools".to_owned(), json!(["Read", "Read"]));
        assert!(write_subagent_file(&path, &invalid_dirs).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);

        let oversized_prompt = "x".repeat(512 * 1024);
        let oversized = serde_json::from_value::<Map<String, Value>>(json!({
            "name": "reviewer",
            "description": "Updated",
            "systemPrompt": oversized_prompt
        }))
        .unwrap();
        assert!(write_subagent_file(&path, &oversized).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);
    }

    #[test]
    fn mcp_conversation_read_scope_is_explicit_and_missing_config_is_empty() {
        let temporary = tempfile::tempdir().unwrap();
        let conversation = temporary.path().join("conversation");
        let other = temporary.path().join("other");
        std::fs::create_dir_all(&conversation).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let conversation_root = std::fs::canonicalize(&conversation).unwrap();
        let missing_config = temporary.path().join("mcp.json");

        // 缺失的 user/project 配置代表“尚未配置”，不是服务故障。
        let (_, servers) = read_mcp_servers(&missing_config, super::McpConfigShape::McpServers)
            .expect("缺失 MCP 配置应返回空 map");
        assert!(servers.is_empty());

        let conversation_alias = conversation.join(".");
        assert_eq!(
            resolve_resource_workspace_root_from_registration(
                &conversation_alias.to_string_lossy(),
                Err("项目尚未添加".to_owned()),
                &conversation_root,
            )
            .unwrap(),
            conversation_root
        );
        let other_path = other.to_string_lossy();
        let error = resolve_resource_workspace_root_from_registration(
            &other_path,
            Err("项目尚未添加：other".to_owned()),
            &conversation_root,
        )
        .expect_err("未登记的普通目录仍必须拒绝");
        assert_eq!(error, "项目尚未添加：other");
    }

    #[test]
    fn conversation_catalog_reads_do_not_grant_project_command_writes() {
        let temporary = tempfile::tempdir().unwrap();
        let conversation = temporary
            .path()
            .join("chat-workspaces")
            .join("conversation");
        let child = conversation.join("child");
        let sibling = temporary.path().join("chat-workspaces").join("another");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let root = std::fs::canonicalize(&conversation).unwrap();
        let alias = conversation.join(".");
        let supplied = alias.to_string_lossy();
        assert_eq!(
            resolve_resource_workspace_root_from_registration(
                &supplied,
                Err("项目尚未添加".to_owned()),
                &root,
            )
            .unwrap(),
            root,
        );
        assert_eq!(
            super::command_write_scope_from_registration(
                &supplied,
                Err("项目尚未添加".to_owned()),
                Some(&root),
            )
            .unwrap(),
            None,
        );
        for path in [&child, &sibling, temporary.path()] {
            let supplied = path.to_string_lossy();
            assert!(
                resolve_resource_workspace_root_from_registration(
                    &supplied,
                    Err("项目尚未添加".to_owned()),
                    &root,
                )
                .is_err()
            );
            assert!(
                super::command_write_scope_from_registration(
                    &supplied,
                    Err("项目尚未添加".to_owned()),
                    Some(&root),
                )
                .is_err()
            );
        }
        let project = std::fs::canonicalize(&sibling).unwrap();
        assert_eq!(
            super::command_write_scope_from_registration(
                &sibling.to_string_lossy(),
                Ok(project.clone()),
                Some(&root),
            )
            .unwrap(),
            Some(project)
        );
    }

    #[test]
    fn user_command_location_scope_matches_frontend_catalog_filter() {
        let temporary = tempfile::tempdir().unwrap();
        let command = temporary.path().join("scope-check.md");
        std::fs::write(&command, "---\nname: scope-check\n---\n执行隔离命令\n").unwrap();
        let user = super::command_value(&command, temporary.path(), "global", &Map::new()).unwrap();
        assert_eq!(user["scope"], "global");
        assert_eq!(user["location"]["scope"], "user");
        assert_eq!(user["location"]["source"], "zcode");
        assert_eq!(user["name"], "/scope-check");
        assert!(user["prompt"].as_str().unwrap().contains("执行隔离命令"));
        let project =
            super::command_value(&command, temporary.path(), "project", &Map::new()).unwrap();
        assert_eq!(project["scope"], "project");
        assert_eq!(project["location"]["scope"], "project");
        // 保持既有 command ID 稳定，不因修正目录投影而更换禁用/编辑引用。
        assert_eq!(user["id"], "zcodeAgent:zcode:global:/scope-check");
    }

    #[test]
    fn mcp_record_projection_keeps_native_and_sync_scopes_distinct() {
        let temporary = tempfile::tempdir().unwrap();
        let user_path = temporary.path().join("mcp.json");
        std::fs::write(
            &user_path,
            br#"{"mcpServers":{"local-fixture":{"command":"fixture-mcp","args":[]}}}"#,
        )
        .unwrap();
        let user_entry = super::read_mcp_entries(vec![(
            user_path.clone(),
            "zcode",
            super::McpConfigShape::McpServers,
        )])
        .unwrap()
        .pop()
        .expect("MCP user fixture should produce a record");
        let user_record = mcp_native_record_value(&user_entry, None);
        assert_eq!(user_record["source"], "zcodeagentmcp");
        assert_eq!(user_record["scope"], "user");
        assert!(user_record.get("projectPath").is_none());
        assert_eq!(user_record["location"]["source"], "zcode");
        assert_eq!(user_record["location"]["scope"], "user");
        assert_eq!(
            user_record["file"]["filePath"],
            crate::path_utils::path_to_frontend(&user_path)
        );

        let project_root = temporary.path().join("project");
        let project_path = project_root.join(".agents").join("mcp.json");
        std::fs::create_dir_all(project_path.parent().unwrap()).unwrap();
        std::fs::write(
            &project_path,
            br#"{"mcpServers":{"workspace-fixture":{"command":"fixture-mcp","args":["--workspace"]}}}"#,
        )
        .unwrap();
        let project_entry = super::read_mcp_entries(vec![(
            project_path,
            "agents",
            super::McpConfigShape::McpServers,
        )])
        .unwrap()
        .pop()
        .expect("MCP project fixture should produce a record");
        let project_record = mcp_native_record_value(&project_entry, Some(&project_root));
        assert_eq!(project_record["source"], "zcodeagentmcp");
        assert_eq!(project_record["scope"], "workspace");
        assert_eq!(
            project_record["projectPath"],
            crate::path_utils::path_to_frontend(&project_root)
        );
        assert_eq!(project_record["location"]["source"], "agents");
        assert_eq!(project_record["location"]["scope"], "project");
        assert_eq!(
            project_record["location"]["projectPath"],
            crate::path_utils::path_to_frontend(&project_root)
        );

        let candidate = mcp_sync_candidate_value(&user_entry);
        assert_eq!(candidate["source"], "zcode");
        assert!(candidate.get("scope").is_none());
        assert_eq!(candidate["name"], "local-fixture");
    }

    #[test]
    fn normalizes_source_object_arguments_without_losing_nested_values() {
        let args = normalize_service_args(
            "file",
            "readTextFile",
            json!([{"uri":"D:/workspace/readme.md","startLine":2}]),
        )
        .expect("single object source argument should be unwrapped");
        assert_eq!(args["uri"], "D:/workspace/readme.md");
        assert_eq!(args["startLine"], 2);
    }

    #[test]
    fn normalizes_positional_setting_and_lifecycle_arguments() {
        let settings = normalize_service_args(
            "setting",
            "update",
            json!([{"locale":"zh-CN"}, {"providerFamilyDomain":"ignored"}]),
        )
        .expect("setting patch should use the first source argument");
        assert_eq!(settings, json!({"locale":"zh-CN"}));

        let operation = normalize_service_args(
            "prompt-attachment-transfer",
            "adopt",
            json!(["operation-1"]),
        )
        .expect("attachment lifecycle arguments should be named");
        assert_eq!(operation, json!({"operationId":"operation-1"}));

        let onboarding = normalize_service_args(
            "onboarding-record",
            "appendRecord",
            json!([
                "device-1",
                {
                    "occupation": null,
                    "interfaceMode": "coding",
                    "memoryEnabled": false,
                    "proactiveSuggestionsEnabled": true,
                    "completedAt": "2026-01-01T00:00:00Z"
                }
            ]),
        )
        .expect("onboarding source positional arguments should be named");
        assert_eq!(onboarding["deviceMid"], "device-1");
        assert_eq!(onboarding["entry"]["interfaceMode"], "coding");
    }

    #[test]
    fn prompt_attachment_transfer_never_reports_local_path_as_remote_success() {
        for method in ["stage", "adopt", "cancel", "cleanup"] {
            let error = prompt_attachment_transfer_unsupported(method)
                .expect_err("local desktop must reject remote attachment transfer");
            assert!(error.starts_with("不支持 prompt-attachment-transfer."));
            assert!(error.contains(method));
            assert!(error.contains("未接入远端附件暂存"));
        }
    }

    #[test]
    fn normalizes_dynamic_subscription_ids() {
        let args = normalize_listen_args("file-watcher", "onDynamicChange", json!("watch-1"))
            .expect("dynamic listener id should be named");
        assert_eq!(args, json!({"id":"watch-1"}));
    }

    #[test]
    fn rejects_ambiguous_multi_argument_methods() {
        let error = normalize_service_args("file", "readTextFile", json!(["a", "b"]))
            .expect_err("unknown positional signatures must not be guessed");
        assert!(error.contains("未定义的服务位置参数"));
    }

    #[test]
    fn mcp_status_snapshot_marks_missing_runtime_as_unavailable() {
        let descriptor = McpStatusDescriptor {
            name: "project-server".to_owned(),
            transport: "http".to_owned(),
            enabled: true,
            config_error: None,
        };
        let snapshot = mcp_status_snapshot(&descriptor, None, "2026-10-03T00:00:00Z");
        assert_eq!(snapshot["status"], "disconnected");
        assert_eq!(snapshot["failureKind"], "status_unavailable");
        assert_eq!(snapshot["toolCount"], 0);
    }

    #[test]
    fn mcp_status_snapshot_preserves_real_connection_and_tool_count() {
        let descriptor = McpStatusDescriptor {
            name: "stdio-server".to_owned(),
            transport: "stdio".to_owned(),
            enabled: true,
            config_error: None,
        };
        let runtime = crate::agent_runtime::RuntimeMcpServerSnapshot {
            name: descriptor.name.clone(),
            transport: keencode_acp::McpTransportKind::Stdio,
            connection_status: keencode_acp::McpConnectionStatus::Connected,
            tools_count: 3,
            oauth_status: keencode_acp::McpOAuthStatus::NotRequired,
            error: None,
        };
        let snapshot = mcp_status_snapshot(&descriptor, Some(&runtime), "2026-10-03T00:00:00Z");
        assert_eq!(snapshot["status"], "connected");
        assert_eq!(snapshot["transport"], "stdio");
        assert_eq!(snapshot["toolCount"], 3);
        assert!(snapshot.get("failureKind").is_none());
    }

    #[test]
    fn mcp_status_snapshot_classifies_oauth_disconnect_and_config_error() {
        let oauth_descriptor = McpStatusDescriptor {
            name: "oauth-server".to_owned(),
            transport: "http".to_owned(),
            enabled: true,
            config_error: None,
        };
        let oauth_runtime = crate::agent_runtime::RuntimeMcpServerSnapshot {
            name: oauth_descriptor.name.clone(),
            transport: keencode_acp::McpTransportKind::StreamableHttp,
            connection_status: keencode_acp::McpConnectionStatus::Disconnected,
            tools_count: 0,
            oauth_status: keencode_acp::McpOAuthStatus::Idle,
            error: None,
        };
        let oauth_snapshot = mcp_status_snapshot(
            &oauth_descriptor,
            Some(&oauth_runtime),
            "2026-10-03T00:00:00Z",
        );
        assert_eq!(oauth_snapshot["status"], "disconnected");
        assert_eq!(oauth_snapshot["failureKind"], "not_authenticated");

        let invalid_descriptor = McpStatusDescriptor {
            name: "invalid-server".to_owned(),
            transport: "http".to_owned(),
            enabled: true,
            config_error: Some("配置无效".to_owned()),
        };
        let invalid_snapshot =
            mcp_status_snapshot(&invalid_descriptor, None, "2026-10-03T00:00:00Z");
        assert_eq!(invalid_snapshot["status"], "failed");
        assert_eq!(invalid_snapshot["failureKind"], "config_invalid");
        assert_eq!(invalid_snapshot["toolCount"], 0);
    }

    #[test]
    fn requested_mcp_status_descriptors_reject_duplicate_names() {
        let error = requested_mcp_status_descriptors(&json!({
            "mcpServers": [
                {"name": "demo", "command": "demo-mcp"},
                {"name": "DEMO", "type": "http", "url": "https://example.test/mcp"}
            ]
        }))
        .expect_err("duplicate MCP names must not be merged silently");
        assert!(error.contains("重复 Server"));
    }

    #[test]
    fn marketplace_update_selection_excludes_other_marketplaces() {
        let ids = [
            crate::plugins::PluginId::parse("one@official").unwrap(),
            crate::plugins::PluginId::parse("two@community").unwrap(),
            crate::plugins::PluginId::parse("three@OFFICIAL").unwrap(),
        ];
        assert_eq!(
            plugin_ids_in_marketplace(ids.iter(), "official"),
            vec!["one@official", "three@OFFICIAL"]
        );
        assert!(plugin_ids_in_marketplace(ids.iter(), "missing").is_empty());
    }

    #[test]
    fn validates_workspace_hook_trust_digests() {
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(validate_hook_digest(digest, "digest").unwrap(), digest);
        assert!(validate_hook_digest(&digest[..63], "digest").is_err());
        assert!(
            validate_hook_digest(
                "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
                "digest"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_or_untrusted_hook_trust_records() {
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let record = json!({
            "workspaceIdentity": "D:/workspace",
            "bundleDigestAtGrant": digest,
            "hookDeclarationDigest": digest,
            "digestAlgorithm": "sha256",
            "decision": "trusted",
            "grantedAt": "2026-10-03T00:00:00Z",
            "eventAtGrant": "PreToolUse",
            "displayCommandAtGrant": "echo ok",
            "sourcePathAtGrant": "D:/workspace/.agents/settings.json",
            "sourceDiscoveryOrderAtGrant": 0,
            "matcherAtGrant": null,
            "matcherIndexAtGrant": 0,
            "hookIndexAtGrant": 0,
        });
        assert!(validate_hook_trust_record(&record).is_ok());
        assert!(
            validate_hook_trust_record(&json!({
                "workspaceIdentity": "D:/workspace",
                "bundleDigestAtGrant": digest,
                "hookDeclarationDigest": digest,
                "digestAlgorithm": "sha256",
                "decision": "trusted",
                "grantedAt": "2026-10-03T00:00:00Z",
                "eventAtGrant": "PreToolUse",
                "displayCommandAtGrant": "echo ok",
                "sourcePathAtGrant": "D:/workspace/.agents/settings.json",
                "sourceDiscoveryOrderAtGrant": 0,
                "matcherAtGrant": null,
                "matcherIndexAtGrant": 0,
                "hookIndexAtGrant": 0,
                "unknown": true,
            }))
            .is_err()
        );
        assert!(
            validate_hook_trust_record(&json!({
                "workspaceIdentity": "D:/workspace",
                "bundleDigestAtGrant": digest,
                "hookDeclarationDigest": digest,
                "digestAlgorithm": "sha256",
                "decision": "denied",
                "grantedAt": "2026-10-03T00:00:00Z",
                "eventAtGrant": "PreToolUse",
                "displayCommandAtGrant": "echo ok",
                "sourcePathAtGrant": "D:/workspace/.agents/settings.json",
                "sourceDiscoveryOrderAtGrant": 0,
                "matcherAtGrant": null,
                "matcherIndexAtGrant": 0,
                "hookIndexAtGrant": 0,
            }))
            .is_err()
        );
    }

    #[test]
    fn workspace_hook_digests_bind_file_and_declaration_content() {
        let workspace = std::path::Path::new("D:/workspace");
        let path = workspace.join(".agents/settings.json");
        let original = json!({
            "id": "hook-1",
            "event": "PreToolUse",
            "type": "command",
            "command": "echo one",
            "enabled": true,
        });
        let changed = json!({
            "id": "hook-1",
            "event": "PreToolUse",
            "type": "command",
            "command": "echo two",
            "enabled": true,
        });
        let original_input = vec![WorkspaceHookFileInput {
            path: path.clone(),
            hooks: vec![original.clone()],
        }];
        let changed_input = vec![WorkspaceHookFileInput {
            path: path.clone(),
            hooks: vec![changed.clone()],
        }];
        let (original_bundle, original_declarations) =
            workspace_hook_bundle_digest(workspace, &original_input);
        let (changed_bundle, changed_declarations) =
            workspace_hook_bundle_digest(workspace, &changed_input);
        assert_ne!(original_bundle, changed_bundle);
        assert_ne!(original_declarations, changed_declarations);
        assert_eq!(
            original_declarations,
            [workspace_hook_declaration_digest(
                workspace, &path, &original
            )]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn expired_media_lease_is_rejected_by_shared_authorizer() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clip.bin");
        std::fs::write(&path, b"media").unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        let preview_id = format!("media-test-expired-{}", now_ms());
        media_preview_store().lock().unwrap().insert(
            preview_id.clone(),
            MediaPreviewLease {
                path: canonical.clone(),
                media_type: "video/mp4",
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        let result = authorize_media_preview_path(&preview_id, &canonical);
        assert!(result.is_err());
        media_preview_store().lock().unwrap().remove(&preview_id);
    }

    #[test]
    fn plugin_cancel_operation_cancels_the_registered_token() {
        let operation_id = format!("plugin-cancel-test-{}", now_ms());
        let operation = begin_plugin_operation(&json!({"operationId": operation_id}))
            .unwrap()
            .expect("应登记插件操作 token");
        assert!(!operation.1.is_cancelled());
        let cancelled = cancel_plugin_operation(json!({"operationId": operation_id})).unwrap();
        assert_eq!(cancelled["cancelled"], true);
        assert!(operation.1.is_cancelled());
        finish_plugin_operation(Some(&operation));
    }
}
