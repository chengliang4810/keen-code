//! ZCode 终端契约直接驱动已有原生 PTY；不启动 Node 宿主或另建进程执行器。

use super::dispatch::{EventCallback, GatewayContext, RpcError, Subscription};
use crate::terminal::TerminalManager;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tauri::{Listener, Manager};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static OWNERS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
const MAX_TERMINALS_PER_CONNECTION: usize = 16;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_PENDING_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

fn owners() -> &'static Mutex<HashMap<String, String>> {
    OWNERS.get_or_init(Mutex::default)
}

fn error(message: impl std::fmt::Display) -> RpcError {
    RpcError::new("terminal.error", message.to_string())
}

fn authorize(ctx: &GatewayContext, id: &str) -> Result<(), RpcError> {
    if ctx.is_closed() || owners().lock().get(id) != Some(&ctx.connection_id) {
        return Err(error("终端不属于当前连接或连接已关闭"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    cols: u16,
    rows: u16,
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    id: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resize {
    id: String,
    cols: u16,
    rows: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dispose {
    id: String,
}

/// PromiseCancel 可终止异步调用但不能中断 spawn_blocking；两端共同收割未交付的 PTY。
struct CreationReservation {
    ctx: GatewayContext,
    id: String,
    aborted: Arc<AtomicBool>,
    delivered: bool,
}

impl Drop for CreationReservation {
    fn drop(&mut self) {
        if self.delivered {
            return;
        }
        self.aborted.store(true, Ordering::Release);
        owners().lock().remove(&self.id);
        let (app, id) = (self.ctx.app.clone(), self.id.clone());
        tauri::async_runtime::spawn_blocking(move || {
            let _ = crate::terminal::terminal_close(id, app.state::<Arc<TerminalManager>>());
        });
    }
}

pub(super) async fn call(
    ctx: GatewayContext,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    match method {
        "create" => {
            let args: Create = serde_json::from_value(args).map_err(error)?;
            if args.cols == 0 || args.rows == 0 {
                return Err(error("终端行列必须大于零"));
            }
            // 无项目终端使用应用数据沙箱；项目子目录仍经过已登记根的路径授权。
            let cwd = match args.cwd {
                Some(cwd) => crate::workspace::registered_project_root(&ctx.app, &cwd)
                    .or_else(|_| {
                        crate::workspace::authorize_existing_absolute(
                            &ctx.app,
                            std::path::Path::new(&cwd),
                        )
                    })
                    .map_err(error)?,
                None => crate::workspace::app_data_session_root(&ctx.app).map_err(error)?,
            };
            if !cwd.is_dir() {
                return Err(error("终端 cwd 必须为目录"));
            }
            let settings = crate::app_settings::get(&ctx.app).map_err(error)?;
            let shell = crate::terminal::terminal_shells_list()
                .into_iter()
                .find(|shell| {
                    shell.id == settings.terminal_shell
                        || settings.terminal_shell == crate::app_settings::TerminalShell::Auto
                })
                .map(|shell| shell.path)
                .ok_or_else(|| error("终端 Shell 不可用"))?;
            let id = format!(
                "zcode-{}-{}",
                ctx.connection_id,
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            );
            {
                let mut entries = owners().lock();
                if ctx.is_closed()
                    || entries
                        .values()
                        .filter(|owner| *owner == &ctx.connection_id)
                        .count()
                        >= MAX_TERMINALS_PER_CONNECTION
                {
                    return Err(error("终端连接已关闭或达到连接数量上限"));
                }
                entries.insert(id.clone(), ctx.connection_id.clone());
            }
            let aborted = Arc::new(AtomicBool::new(false));
            let mut reservation = CreationReservation {
                ctx: ctx.clone(),
                id: id.clone(),
                aborted: aborted.clone(),
                delivered: false,
            };
            let create_ctx = ctx.clone();
            let create_id = id.clone();
            let created = tauri::async_runtime::spawn_blocking(move || {
                crate::terminal::terminal_create(
                    create_id.clone(),
                    cwd.to_string_lossy().into_owned(),
                    Some(args.cols),
                    Some(args.rows),
                    Some(HashMap::from([("TERM_PROGRAM".into(), "KeenCode".into())])),
                    create_ctx.app.clone(),
                    create_ctx.app.state::<Arc<TerminalManager>>(),
                )?;
                if create_ctx.is_closed() || aborted.load(Ordering::Acquire) {
                    crate::terminal::terminal_close(
                        create_id,
                        create_ctx.app.state::<Arc<TerminalManager>>(),
                    )?;
                    return Err("终端创建期间连接已关闭".into());
                }
                Ok::<_, String>(())
            })
            .await
            .map_err(error)
            .and_then(|result| result.map_err(error));
            if let Err(failure) = created {
                owners().lock().remove(&id);
                return Err(failure);
            }
            reservation.delivered = true;
            Ok(
                json!({"id": id, "shell": shell, "fontFamily": settings.terminal_font_family,
                "fontFamilySource": "custom", "windowsPty": if cfg!(windows) {json!({"backend":"conpty"})} else {Value::Null}}),
            )
        }
        "write" => {
            let args: Write = serde_json::from_value(args).map_err(error)?;
            authorize(&ctx, &args.id)?;
            if args.data.len() > MAX_INPUT_BYTES {
                return Err(error("终端输入超过大小上限"));
            }
            crate::terminal::terminal_write(
                args.id,
                args.data.into_bytes(),
                ctx.app.state::<Arc<TerminalManager>>(),
            )
            .await
            .map_err(error)?;
            Ok(Value::Null)
        }
        "resize" => {
            let args: Resize = serde_json::from_value(args).map_err(error)?;
            authorize(&ctx, &args.id)?;
            if args.cols == 0 || args.rows == 0 {
                return Err(error("终端行列必须大于零"));
            }
            crate::terminal::terminal_resize(
                args.id,
                args.cols,
                args.rows,
                ctx.app.state::<Arc<TerminalManager>>(),
            )
            .map_err(error)?;
            Ok(Value::Null)
        }
        "dispose" => {
            let args: Dispose = serde_json::from_value(args).map_err(error)?;
            authorize(&ctx, &args.id)?;
            owners().lock().remove(&args.id);
            tauri::async_runtime::spawn_blocking(move || {
                crate::terminal::terminal_close(args.id, ctx.app.state::<Arc<TerminalManager>>())
            })
            .await
            .map_err(error)?
            .map_err(error)?;
            Ok(Value::Null)
        }
        _ => Err(error(format!("未知终端方法：{method}"))),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Output {
    id: String,
    data: Vec<u8>,
    byte_offset: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    data: Vec<u8>,
    byte_offset: u64,
    exit_code: Option<u32>,
}

fn snapshot(ctx: &GatewayContext, id: &str) -> Result<Option<Snapshot>, RpcError> {
    crate::terminal::terminal_snapshot(id.to_owned(), ctx.app.state::<Arc<TerminalManager>>())
        .map_err(error)?
        .map(|value| {
            serde_json::to_value(value)
                .map_err(error)
                .and_then(|value| serde_json::from_value(value).map_err(error))
        })
        .transpose()
}

/// 快照和事件按累计字节去重；UTF-8 尾字节跨块保留，避免中文输出变成替换字符。
#[derive(Default)]
struct DataProjection {
    initialized: bool,
    queued: Vec<Output>,
    queued_bytes: usize,
    overflowed: bool,
    offset: u64,
    tail: Vec<u8>,
}

impl DataProjection {
    fn consume(&mut self, data: &[u8], end: u64) -> String {
        if end <= self.offset {
            return String::new();
        }
        let start = end.saturating_sub(data.len() as u64);
        if start > self.offset {
            self.tail.clear();
        }
        let skip = self.offset.saturating_sub(start).min(data.len() as u64) as usize;
        self.tail.extend_from_slice(&data[skip..]);
        self.offset = end;
        let mut result = String::new();
        loop {
            match std::str::from_utf8(&self.tail) {
                Ok(text) => {
                    result.push_str(text);
                    self.tail.clear();
                    break;
                }
                Err(failure) => {
                    let valid = failure.valid_up_to();
                    result.push_str(
                        std::str::from_utf8(&self.tail[..valid]).expect("已验证 UTF-8 前缀"),
                    );
                    self.tail.drain(..valid);
                    if let Some(invalid) = failure.error_len() {
                        result.push('\u{fffd}');
                        self.tail.drain(..invalid);
                    } else {
                        break;
                    }
                }
            }
        }
        result
    }
}

pub(super) fn listen(
    ctx: &GatewayContext,
    event: &str,
    args: Value,
    callback: EventCallback,
) -> Result<Subscription, RpcError> {
    let id = args
        .as_str()
        .ok_or_else(|| error("终端动态事件参数必须为终端标识"))?
        .to_owned();
    authorize(ctx, &id)?;
    let app = ctx.app.clone();
    match event {
        "onDynamicData" => {
            let state = Arc::new(Mutex::new(DataProjection::default()));
            let pending = state.clone();
            let expected = id.clone();
            let emit = callback.clone();
            let listener = app.listen("terminal://output", move |event| {
                if let Ok(output) = serde_json::from_str::<Output>(event.payload()) {
                    if output.id != expected {
                        return;
                    }
                    let mut projection = pending.lock();
                    if !projection.initialized {
                        projection.queued_bytes += output.data.len();
                        if projection.queued_bytes > MAX_PENDING_OUTPUT_BYTES {
                            projection.overflowed = true;
                        } else {
                            projection.queued.push(output);
                        }
                        return;
                    }
                    let text = projection.consume(&output.data, output.byte_offset);
                    if !text.is_empty() {
                        let _ = emit(json!(text));
                    }
                }
            });
            // 不持投影锁读取原生快照：输出发布同时持有 history 锁，反向加锁会死锁。
            let initial = match snapshot(ctx, &id) {
                Ok(Some(initial)) => initial,
                Ok(None) => {
                    app.unlisten(listener);
                    return Err(error("终端已关闭"));
                }
                Err(failure) => {
                    app.unlisten(listener);
                    return Err(failure);
                }
            };
            let mut projection = state.lock();
            if projection.overflowed {
                app.unlisten(listener);
                return Err(error("终端首帧缓冲超过上限，请重新打开终端"));
            }
            let text = projection.consume(&initial.data, initial.byte_offset);
            if !text.is_empty()
                && let Err(failure) = callback(json!(text))
            {
                app.unlisten(listener);
                return Err(failure);
            }
            let queued = std::mem::take(&mut projection.queued);
            for output in queued {
                let text = projection.consume(&output.data, output.byte_offset);
                if !text.is_empty()
                    && let Err(failure) = callback(json!(text))
                {
                    app.unlisten(listener);
                    return Err(failure);
                }
            }
            projection.initialized = true;
            drop(projection);
            Ok(Subscription::new(move || app.unlisten(listener)))
        }
        "onDynamicExit" => {
            let alive = Arc::new(AtomicBool::new(true));
            let sent = Arc::new(AtomicBool::new(false));
            let exit_ctx = ctx.clone();
            let expected = id.clone();
            let live = alive.clone();
            let once = sent.clone();
            let emit = callback.clone();
            let listener = app.listen("terminal://exited", move |event| {
                if serde_json::from_str::<Value>(event.payload())
                    .ok()
                    .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
                    .as_deref()
                    != Some(&expected)
                {
                    return;
                }
                let (ctx, id, live, once, emit) = (
                    exit_ctx.clone(),
                    expected.clone(),
                    live.clone(),
                    once.clone(),
                    emit.clone(),
                );
                // EOF 只启动按需等待；只有 OS 确认退出后才发布真实状态，不伪造 exit=0。
                tauri::async_runtime::spawn(async move {
                    while live.load(Ordering::Acquire) && !ctx.is_closed() {
                        match snapshot(&ctx, &id) {
                            Ok(Some(snapshot)) if snapshot.exit_code.is_some() => {
                                if !once.swap(true, Ordering::AcqRel) {
                                    let _ = emit(json!(snapshot.exit_code));
                                }
                                break;
                            }
                            Ok(Some(_)) => tokio::time::sleep(Duration::from_millis(50)).await,
                            _ => break,
                        }
                    }
                });
            });
            let initial = match snapshot(ctx, &id) {
                Ok(value) => value,
                Err(failure) => {
                    app.unlisten(listener);
                    return Err(failure);
                }
            };
            if let Some(code) = initial.and_then(|snapshot| snapshot.exit_code)
                && !sent.swap(true, Ordering::AcqRel)
                && let Err(failure) = callback(json!(code))
            {
                app.unlisten(listener);
                return Err(failure);
            }
            Ok(Subscription::new(move || {
                alive.store(false, Ordering::Release);
                app.unlisten(listener);
            }))
        }
        _ => Err(error(format!("未知终端事件：{event}"))),
    }
}

pub(super) fn close_connection(ctx: &GatewayContext) {
    let ids: Vec<_> = {
        let mut entries = owners().lock();
        let ids = entries
            .iter()
            .filter(|(_, owner)| *owner == &ctx.connection_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in &ids {
            entries.remove(id);
        }
        ids
    };
    if ids.is_empty() {
        return;
    }
    let app = ctx.app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        for id in ids {
            let _ = crate::terminal::terminal_close(id, app.state::<Arc<TerminalManager>>());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::DataProjection;
    #[test]
    fn snapshot_live_overlap_and_split_utf8_are_lossless() {
        let mut state = DataProjection::default();
        assert_eq!(state.consume(b"hello", 5), "hello");
        assert_eq!(state.consume(b"llo world", 11), " world");
        let chinese = "中文".as_bytes();
        assert_eq!(state.consume(&chinese[..2], 13), "");
        assert_eq!(state.consume(&chinese[2..4], 15), "中");
        assert_eq!(state.consume(&chinese[4..], 17), "文");
        assert_eq!(state.consume(chinese, 17), "");
    }
    #[test]
    fn truncated_snapshot_and_invalid_bytes_preserve_offsets() {
        let mut state = DataProjection::default();
        assert_eq!(state.consume(b"last", 100), "last");
        assert_eq!(state.consume(&[0xff, b'!'], 102), "\u{fffd}!");
        assert_eq!(state.offset, 102);
    }
}
