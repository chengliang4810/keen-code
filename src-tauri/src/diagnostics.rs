//! KeenCode 后端诊断日志。
//!
//! 诊断日志的目标是让启动、IPC、ACP 传输和供应商配置问题可以通过运行记录定位，
//! 而不是依赖重新猜测代码路径。敏感值和完整请求正文不会写入日志。

use std::collections::VecDeque;
use std::fs::{OpenOptions, create_dir_all};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use serde_json::Value;
use tauri::AppHandle;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub mod observability;
pub mod process_resources;

const LOG_QUEUE_CAPACITY: usize = 1_024;
const LOG_FLUSH_TIMEOUT_MS: u64 = 5_000;
const LOG_ROTATE_BYTES: u64 = 8 * 1024 * 1024;
const LOG_RETENTION_BYTES: u64 = 256 * 1024 * 1024;
const LOG_RETENTION_DAYS: u64 = 7;
const CRASH_SPOOL_MAX_BYTES: u64 = 128 * 1024;
const CRASH_SPOOL_MAX_RECORDS: usize = observability::MAX_CRASH_RECORDS;

enum LogCommand {
    Line(String),
    Flush(SyncSender<Result<(), String>>),
    Shutdown,
}

/// 后端诊断日志句柄。
pub struct Diagnostics {
    /// 当前日志文件的绝对路径。
    path: PathBuf,
    /// 有界异步日志队列；普通日志写入不得阻塞调用方。
    sender: SyncSender<LogCommand>,
    /// 后台写入线程；测试与进程退出时通过 Drop 收敛。
    worker: Mutex<Option<JoinHandle<()>>>,
    /// 队列满载时被丢弃的低优先级日志数量。
    dropped_logs: AtomicU64,
    /// 与本进程所有启动阶段共享的单调时钟起点。
    startup_started_at: Instant,
    /// 与日志共享生命周期的结构化本地观测状态。
    observability: Arc<observability::ObservabilityStore>,
    /// 跨重启保留最近崩溃摘要；正文已在写入前脱敏且有界。
    crash_spool: PathBuf,
    crash_spool_lock: Mutex<()>,
    /// 进程资源采样器保留相邻样本的 CPU 基线；调用方按需触发，不自启动线程。
    process_resources: Mutex<process_resources::ProcessResourceSampler>,
}

impl Diagnostics {
    /// 根据当前用户的 KeenCode 统一目录创建诊断日志。
    pub fn init(app: &AppHandle, startup_started_at: Instant) -> Arc<Self> {
        let data_dir = crate::storage::root_dir(app).unwrap_or_else(|error| {
            eprintln!("[keencode] 无法获取用户持久化目录，诊断日志回退临时目录: {error}");
            std::env::temp_dir().join("keencode-desktop-data")
        });
        let log_dir = data_dir.join("logs");
        let path = log_dir.join("keencode-desktop.log");
        let file = match open_log_file(&log_dir, &path) {
            Ok(file) => file,
            Err(error) => {
                eprintln!("[keencode] 无法打开诊断日志 {}: {error}", path.display());
                let fallback_dir = std::env::temp_dir().join("keencode-desktop-logs");
                let fallback_path = fallback_dir.join("keencode-desktop.log");
                match open_log_file(&fallback_dir, &fallback_path) {
                    Ok(file) => return Self::from_file(fallback_path, file, startup_started_at),
                    Err(fallback_error) => {
                        eprintln!("[keencode] 临时诊断日志也无法打开: {fallback_error}");
                        let fallback_path = std::env::temp_dir().join("keencode-desktop.log");
                        let file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&fallback_path)
                            .expect("无法创建任何诊断日志文件");
                        return Self::from_file(fallback_path, file, startup_started_at);
                    }
                }
            }
        };
        Self::from_file(path, file, startup_started_at)
    }

    fn from_file(path: PathBuf, file: std::fs::File, startup_started_at: Instant) -> Arc<Self> {
        let _ = prune_log_backups(&path);
        let crash_spool = path.with_extension("crash.jsonl");
        let observability = Arc::new(observability::ObservabilityStore::default());
        restore_crash_spool(&crash_spool, &observability);
        let (sender, receiver) = mpsc::sync_channel(LOG_QUEUE_CAPACITY);
        let worker_path = path.clone();
        let worker = thread::Builder::new()
            .name("keencode-diagnostics-writer".to_owned())
            .spawn(move || run_log_writer(worker_path, file, receiver))
            .expect("无法启动诊断日志写入线程");
        Arc::new(Self {
            path,
            sender,
            worker: Mutex::new(Some(worker)),
            dropped_logs: AtomicU64::new(0),
            startup_started_at,
            observability,
            crash_spool,
            crash_spool_lock: Mutex::new(()),
            process_resources: Mutex::new(process_resources::ProcessResourceSampler::default()),
        })
    }

    /// 为不启动 Tauri 窗口的开发评测进程创建独立诊断日志。
    #[cfg(feature = "benchmark")]
    pub(crate) fn init_benchmark(
        path: PathBuf,
        startup_started_at: Instant,
    ) -> std::io::Result<Arc<Self>> {
        let log_dir = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "日志路径缺少父目录")
        })?;
        let file = open_log_file(log_dir, &path)?;
        Ok(Self::from_file(path, file, startup_started_at))
    }

    /// 接管后台 tracing 和 Rust panic；所有写入复用同一脱敏、限长出口。
    pub fn install(self: &Arc<Self>) {
        if let Err(error) = self.subscriber(false).try_init() {
            self.error("diagnostics.install", error.to_string());
        }
        let sink = Arc::clone(self);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let backtrace = observability::sanitize_panic_backtrace(
                &std::backtrace::Backtrace::force_capture().to_string(),
            );
            let payload_kind = observability::panic_payload_kind(info);
            sink.record_crash(observability::CrashRecord {
                occurred_at_ms: observability::now_epoch_ms(),
                kind: "panic".to_owned(),
                message: format!("panic payload type={payload_kind}"),
                backtrace: Some(backtrace.clone()),
            });
            sink.error(
                "runtime.panic",
                format!("panic payload type={payload_kind}\n{backtrace}"),
            );
            previous(info);
        }));
    }

    /// 评测进程保留全部 tracing 级别，仍统一经过脱敏与单条长度限制。
    #[cfg(feature = "benchmark")]
    pub(crate) fn install_benchmark(self: &Arc<Self>) {
        if let Err(error) = self.subscriber(true).try_init() {
            self.error("diagnostics.install", error.to_string());
        }
        let sink = Arc::clone(self);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let backtrace = observability::sanitize_panic_backtrace(
                &std::backtrace::Backtrace::force_capture().to_string(),
            );
            let payload_kind = observability::panic_payload_kind(info);
            sink.record_crash(observability::CrashRecord {
                occurred_at_ms: observability::now_epoch_ms(),
                kind: "panic".to_owned(),
                message: format!("panic payload type={payload_kind}"),
                backtrace: Some(backtrace.clone()),
            });
            sink.error(
                "runtime.panic",
                format!("panic payload type={payload_kind}\n{backtrace}"),
            );
            previous(info);
        }));
    }

    fn subscriber(self: &Arc<Self>, include_all: bool) -> impl tracing::Subscriber + Send + Sync {
        let sink = Arc::clone(self);
        let layer = tracing_subscriber::fmt::layer()
            .without_time()
            .with_ansi(false)
            .with_file(true)
            .with_line_number(true)
            .with_writer(move || DiagnosticWriter {
                sink: Arc::clone(&sink),
                bytes: Vec::new(),
            });
        let filter = tracing_subscriber::filter::filter_fn(move |metadata| {
            include_all
                || *metadata.level() <= tracing::Level::WARN
                || metadata.target() == "keencode_diagnostics"
                // 运行时观测白名单：keencode-agent 运行时（如每轮提示词缓存
                // 用量的 debug 观测日志）对问题定位有产品价值，按 target
                // 放行到 DEBUG 级；其他组件的 debug 噪音仍被过滤。
                || (metadata.target().starts_with("keencode_agent")
                    && *metadata.level() <= tracing::Level::DEBUG)
        });
        tracing_subscriber::registry().with(filter).with(layer)
    }

    /// 记录可由本地基准脚本稳定解析的启动阶段。
    pub fn startup_phase(&self, phase: &str) {
        let elapsed_ms = self.startup_started_at.elapsed().as_millis();
        self.observability
            .record_startup_phase(observability::StartupPhase {
                phase: phase.to_owned(),
                occurred_at_ms: observability::now_epoch_ms(),
                elapsed_ms: elapsed_ms.min(u128::from(u64::MAX)) as u64,
            });
        self.log(
            "info",
            "startup.metric",
            format!("phase={phase} elapsed_ms={elapsed_ms}"),
        );
        if std::env::var_os("KEENCODE_BENCHMARK").as_deref() == Some(std::ffi::OsStr::new("1")) {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": phase,
                    "elapsedMs": elapsed_ms,
                })
            );
        }
    }

    /// 返回日志文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 返回共享的结构化本地观测存储；调用方只应写入脱敏摘要。
    pub fn observability(&self) -> Arc<observability::ObservabilityStore> {
        Arc::clone(&self.observability)
    }

    /// 写入结构化 Crash，并同步追加跨重启 spool；spool 失败不阻断正常退出路径。
    pub fn record_crash(&self, crash: observability::CrashRecord) {
        let crash = observability::sanitize_crash_record(crash);
        self.observability.record_crash(crash.clone());
        let Ok(_guard) = self.crash_spool_lock.lock() else {
            return;
        };
        append_crash_spool(&self.crash_spool, &crash);
    }

    /// 读取一次当前宿主及 WebView2 子进程资源摘要；失败字段保持 `None`。
    pub fn process_resource_sample(&self) -> process_resources::ProcessResourceSample {
        self.process_resources
            .lock()
            .map(|mut sampler| sampler.sample())
            .unwrap_or_default()
    }

    /// 等待此前排队的日志写入并同步文件；普通日志路径不调用此方法。
    pub fn flush(&self) -> Result<(), String> {
        let (reply, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(LogCommand::Flush(reply))
            .map_err(|_| "诊断日志写入线程已退出".to_owned())?;
        receiver
            .recv_timeout(std::time::Duration::from_millis(LOG_FLUSH_TIMEOUT_MS))
            .map_err(|_| "等待诊断日志写入超时".to_owned())?
    }

    /// 写入一条结构化文本日志。
    pub fn log(&self, level: &str, component: &str, message: impl AsRef<str>) {
        let timestamp = unix_timestamp_millis();
        let line = format!(
            "{timestamp} level={} component={} message={}\n",
            sanitize_text(level),
            sanitize_text(component),
            sanitize_text(message.as_ref())
        );
        match self.sender.try_send(LogCommand::Line(line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped_logs.fetch_add(1, Ordering::Relaxed);
                self.observability
                    .increment_counter("diagnostics.dropped_logs", 1);
                // 诊断路径不可反向阻塞模型或 UI；仅保留 error 的 stderr 兜底。
                if level.eq_ignore_ascii_case("error") {
                    eprintln!(
                        "[keencode] 诊断日志队列已满，错误摘要被丢弃: {}",
                        self.path.display()
                    );
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                if level.eq_ignore_ascii_case("error") {
                    eprintln!("[keencode] 诊断日志线程已退出: {}", self.path.display());
                }
            }
        }
    }

    /// 写入异常摘要并对常见密钥格式做脱敏。
    pub fn error(&self, component: &str, error: impl AsRef<str>) {
        self.log("error", component, error);
    }
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        // Drop 只发生在进程关闭或测试销毁时，允许等待队列排空以避免最后一条
        // 诊断丢失；正常业务日志始终走 try_send，不会走这个阻塞路径。
        let _ = self.sender.send(LogCommand::Shutdown);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

/// tracing 每个事件使用独立缓冲，完整事件脱敏后才进入文件。
struct DiagnosticWriter {
    sink: Arc<Diagnostics>,
    bytes: Vec<u8>,
}

impl Write for DiagnosticWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        // fmt 可能分段写入；按完整记录脱敏，同时限制单个事件的内存。
        let remaining = 16_000usize.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for DiagnosticWriter {
    fn drop(&mut self) {
        if !self.bytes.is_empty() {
            let text = String::from_utf8_lossy(&self.bytes);
            let level = match text.split_whitespace().next() {
                Some("ERROR") => "error",
                Some("WARN") => "warn",
                _ => "info",
            };
            self.sink.log(level, "backend", text.trim_end());
        }
    }
}

/// 在独立线程顺序写入诊断文件；队列容量固定，业务调用方不持有文件锁。
fn run_log_writer(path: PathBuf, mut file: std::fs::File, receiver: Receiver<LogCommand>) {
    let mut writer_error: Option<String> = None;
    while let Ok(command) = receiver.recv() {
        match command {
            LogCommand::Line(line) => {
                if writer_error.is_some() {
                    continue;
                }
                if let Err(error) =
                    rotate_if_needed(&mut file, &path).and_then(|_| file.write_all(line.as_bytes()))
                {
                    let message = format!("写入诊断日志失败 {}: {error}", path.display());
                    eprintln!("[keencode] {message}");
                    writer_error = Some(message);
                }
            }
            LogCommand::Flush(reply) => {
                let result = if let Some(error) = writer_error.clone() {
                    Err(error)
                } else {
                    file.flush()
                        .and_then(|_| file.sync_data())
                        .map_err(|error| format!("同步诊断日志失败：{error}"))
                };
                if let Err(error) = &result {
                    writer_error = Some(error.clone());
                }
                let _ = reply.send(result);
            }
            LogCommand::Shutdown => {
                if writer_error.is_none() {
                    let _ = file.flush().and_then(|_| file.sync_data());
                }
                break;
            }
        }
    }
}

/// 按 8 MiB 分片轮转，并保留最近 7 天且总量不超过 256 MiB 的本地日志。
///
/// 轮转只复制当前追加句柄，随后用独立写句柄截断，兼容 Windows 对打开文件
/// 的共享语义。`.log.1` 保留给现有诊断入口和旧版本用户，带时间戳的副本用于
/// 多次轮转期间的保留策略。
fn rotate_if_needed(file: &mut std::fs::File, path: &Path) -> std::io::Result<()> {
    if file.metadata()?.len() >= LOG_ROTATE_BYTES {
        let backup = path.with_extension("log.1");
        std::fs::copy(path, &backup)?;
        let timestamped = path.with_file_name(format!(
            "{}.{}",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("keencode.log"),
            observability::now_epoch_ms(),
        ));
        let _ = std::fs::copy(path, timestamped);
        // Windows 的追加句柄不含截断权限；短暂打开普通写句柄完成轮转。
        OpenOptions::new().write(true).open(path)?.set_len(0)?;
        prune_log_backups(path)?;
    }
    Ok(())
}

fn prune_log_backups(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return Ok(());
    };
    let prefix = format!("{file_name}.");
    let now = SystemTime::now();
    let age_limit = std::time::Duration::from_secs(LOG_RETENTION_DAYS * 24 * 60 * 60);
    let mut backups = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let candidate = entry.path();
        let Some(name) = candidate.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&candidate)?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(now);
        let age = now.duration_since(modified).unwrap_or_default();
        if age > age_limit {
            let _ = std::fs::remove_file(&candidate);
            continue;
        }
        backups.push((candidate, modified, metadata.len()));
    }
    backups.sort_by_key(|left| std::cmp::Reverse(left.1));
    let mut total = std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    for (candidate, _, size) in backups {
        if total.saturating_add(size) <= LOG_RETENTION_BYTES {
            total = total.saturating_add(size);
        } else {
            let _ = std::fs::remove_file(candidate);
        }
    }
    Ok(())
}

/// 创建日志目录并打开追加写入文件。
fn open_log_file(log_dir: &Path, path: &Path) -> std::io::Result<std::fs::File> {
    create_dir_all(log_dir)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn restore_crash_spool(path: &Path, store: &observability::ObservabilityStore) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut records = VecDeque::with_capacity(CRASH_SPOOL_MAX_RECORDS);
    for record in BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<observability::CrashRecord>(&line).ok())
    {
        if records.len() >= CRASH_SPOOL_MAX_RECORDS {
            records.pop_front();
        }
        records.push_back(record);
    }
    for record in records {
        store.restore_crash_record(record);
    }
}

fn append_crash_spool(path: &Path, crash: &observability::CrashRecord) {
    let Some(parent) = path.parent() else {
        return;
    };
    if create_dir_all(parent).is_err() {
        return;
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    if serde_json::to_writer(&mut file, crash).is_err() || file.write_all(b"\n").is_err() {
        return;
    }
    drop(file);
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    let lines = contents.lines().collect::<Vec<_>>();
    if (contents.len() as u64) <= CRASH_SPOOL_MAX_BYTES && lines.len() <= CRASH_SPOOL_MAX_RECORDS {
        return;
    }
    // 从尾部保留最新记录，同时满足文件字节数和记录数上限。
    let mut retained = Vec::with_capacity(CRASH_SPOOL_MAX_RECORDS.min(lines.len()));
    let mut retained_bytes = 0u64;
    for line in lines.iter().rev() {
        let line_bytes = line.len().saturating_add(1) as u64;
        if retained.len() >= CRASH_SPOOL_MAX_RECORDS {
            break;
        }
        if line_bytes > CRASH_SPOOL_MAX_BYTES {
            continue;
        }
        if retained_bytes.saturating_add(line_bytes) > CRASH_SPOOL_MAX_BYTES {
            break;
        }
        retained.push(*line);
        retained_bytes = retained_bytes.saturating_add(line_bytes);
    }
    retained.reverse();
    let temporary = path.with_extension("crash.jsonl.tmp");
    let Ok(mut compacted) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temporary)
    else {
        return;
    };
    for line in retained {
        if compacted.write_all(line.as_bytes()).is_err() || compacted.write_all(b"\n").is_err() {
            let _ = std::fs::remove_file(&temporary);
            return;
        }
    }
    let _ = compacted.flush();
    let _ = std::fs::remove_file(path);
    let _ = std::fs::rename(temporary, path);
}

/// 返回 Unix 毫秒时间戳，避免额外依赖并保证日志在启动早期可用。
fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// 只保留可定位问题的文本，避免换行伪造日志记录。
fn sanitize_text(value: &str) -> String {
    let mut text =
        keencode_model::redact_error_secrets(&observability::redact_absolute_paths(value))
            .replace('\n', "\\n")
            .replace('\r', "\\r");
    if text.len() > 4_000 {
        let mut end = 4_000;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...(truncated)");
    }
    text
}

/// 递归生成 JSON 结构摘要，只输出键名、类型和长度。
#[cfg(test)]
pub(crate) fn summarize_value_for_log(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(text) => format!("string(len={})", text.len()),
        Value::Array(items) => format!(
            "array(len={}, items={})",
            items.len(),
            items
                .first()
                .map(summarize_value_for_log)
                .unwrap_or_else(|| "none".to_string())
        ),
        Value::Object(object) => {
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            format!("object(keys=[{}])", keys.join(","))
        }
    }
}

/// 为 ACP 会话事件补充可诊断但不包含正文的结构摘要。
#[cfg(test)]
fn summarize_acp_event_for_log(method: &str, params: &Value) -> Option<String> {
    if method != "session/update" {
        return None;
    }
    let update = params.get("update")?;
    let update_tag = update.get("sessionUpdate")?.as_str()?;
    let mut parts = vec![format!("update_tag={update_tag}")];
    let content = update.get("content");
    if let Some(content) = content {
        if let Some(chunk_type) = content.get("type").and_then(Value::as_str) {
            parts.push(format!("chunk_type={chunk_type}"));
        }
        if let Some(text) = content.get("text").and_then(Value::as_str) {
            parts.push(format!("text_len={}", text.len()));
        }
    }
    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::{
        CRASH_SPOOL_MAX_BYTES, CRASH_SPOOL_MAX_RECORDS, LOG_RETENTION_BYTES, LOG_RETENTION_DAYS,
        append_crash_spool, observability, prune_log_backups, restore_crash_spool, sanitize_text,
        summarize_acp_event_for_log, summarize_value_for_log,
    };
    use serde_json::json;
    use std::fs::OpenOptions;
    use std::time::{Duration, SystemTime};

    fn test_sink(path: &std::path::Path) -> std::sync::Arc<super::Diagnostics> {
        super::Diagnostics::from_file(
            path.to_owned(),
            super::open_log_file(path.parent().unwrap(), path).unwrap(),
            std::time::Instant::now(),
        )
    }

    #[test]
    fn tracing_events_reach_file_with_context_and_redaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.log");
        let sink = test_sink(&path);
        tracing::subscriber::with_default(sink.subscriber(false), || {
            let span = tracing::info_span!(target: "keencode_diagnostics", "acp.request", request_id = "request-test", session_id = "session-test");
            let _entered = span.enter();
            tracing::info!(target: "keencode_diagnostics", "request started");
            tracing::error!(error = "apiKey=private-key", "execution failed");
            tracing::debug!("invisible noisy event");
        });
        sink.flush().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("request-test") && text.contains("session-test"));
        assert!(text.contains("level=error") && text.contains("execution failed"));
        assert!(text.contains("request started"));
        assert!(!text.contains("private-key") && !text.contains("invisible noisy event"));
        assert_eq!(text.lines().count(), 2);
    }

    /// 运行时观测白名单只放行 `keencode_agent` target 的 DEBUG 及以上事件。
    #[test]
    fn filter_passes_keencode_agent_runtime_debug_events_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.log");
        let sink = test_sink(&path);
        tracing::subscriber::with_default(sink.subscriber(false), || {
            tracing::debug!(target: "keencode_agent::runner", "模型轮次提示词缓存用量已提交");
            tracing::debug!(target: "hyper::client", "third-party noisy debug");
            tracing::trace!(target: "keencode_agent::runner", "trace stays out");
        });
        sink.flush().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("keencode_agent::runner") && text.contains("缓存用量已提交"));
        assert!(!text.contains("third-party noisy debug"));
        assert!(!text.contains("trace stays out"));
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn rotates_and_preserves_new_errors_as_single_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.log");
        let sink = test_sink(&path);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(8 * 1024 * 1024)
            .unwrap();
        sink.error("frontend\nforged", "failed\nsecond line");
        sink.flush().unwrap();
        assert_eq!(
            std::fs::metadata(path.with_extension("log.1"))
                .unwrap()
                .len(),
            8 * 1024 * 1024
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("failed\\nsecond line"));
    }

    #[test]
    fn prunes_expired_log_backups_by_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.log");
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap();

        let expired = path.with_file_name("diagnostics.log.expired");
        let fresh = path.with_file_name("diagnostics.log.fresh");
        let expired_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&expired)
            .unwrap();
        expired_file
            .set_modified(
                SystemTime::now() - Duration::from_secs(LOG_RETENTION_DAYS * 24 * 60 * 60 + 1),
            )
            .unwrap();
        drop(expired_file);
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&fresh)
            .unwrap();

        prune_log_backups(&path).unwrap();

        assert!(!expired.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn prunes_oldest_log_backups_when_total_retention_bytes_are_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.log");
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap();

        let newest = path.with_file_name("diagnostics.log.202609210001");
        let oldest = path.with_file_name("diagnostics.log.202609210000");
        let newest_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&newest)
            .unwrap();
        newest_file.set_len(150 * 1024 * 1024).unwrap();
        newest_file
            .set_modified(SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        let oldest_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&oldest)
            .unwrap();
        oldest_file.set_len(150 * 1024 * 1024).unwrap();
        oldest_file
            .set_modified(SystemTime::now() - Duration::from_secs(120))
            .unwrap();
        drop(newest_file);
        drop(oldest_file);

        prune_log_backups(&path).unwrap();

        assert!(newest.exists());
        assert!(!oldest.exists());
        assert!(std::fs::metadata(&newest).unwrap().len() < LOG_RETENTION_BYTES);
    }

    #[test]
    fn crash_spool_is_bounded_by_bytes_and_records_and_restores_latest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.crash.jsonl");

        for index in 0..(CRASH_SPOOL_MAX_RECORDS + 8) {
            append_crash_spool(
                &path,
                &observability::CrashRecord {
                    occurred_at_ms: index as u64,
                    kind: "panic".to_owned(),
                    message: format!("crash-{index}-{}", "m".repeat(2_000)),
                    backtrace: Some("b".repeat(8_000)),
                },
            );
        }

        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() <= CRASH_SPOOL_MAX_BYTES);
        let lines = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(lines.len() <= CRASH_SPOOL_MAX_RECORDS);
        assert!(lines.len() < CRASH_SPOOL_MAX_RECORDS);

        let store = observability::ObservabilityStore::default();
        restore_crash_spool(&path, &store);
        let crashes = store.snapshot().crashes;
        assert_eq!(
            crashes.last().unwrap().occurred_at_ms,
            (CRASH_SPOOL_MAX_RECORDS + 7) as u64
        );
        assert!(crashes.first().unwrap().occurred_at_ms > 0);
    }

    #[test]
    fn crash_spool_compacts_record_count_even_when_bytes_fit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small-crashes.jsonl");

        for index in 0..(CRASH_SPOOL_MAX_RECORDS + 8) {
            append_crash_spool(
                &path,
                &observability::CrashRecord {
                    occurred_at_ms: index as u64,
                    kind: "panic".to_owned(),
                    message: format!("crash-{index}"),
                    backtrace: None,
                },
            );
        }

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.len() < CRASH_SPOOL_MAX_BYTES as usize);
        let records = contents
            .lines()
            .map(|line| serde_json::from_str::<observability::CrashRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), CRASH_SPOOL_MAX_RECORDS);
        assert_eq!(records.first().unwrap().occurred_at_ms, 8);
        assert_eq!(records.last().unwrap().occurred_at_ms, 39);
    }

    #[test]
    fn redacts_case_insensitively_without_eating_later_context() {
        let text = sanitize_text(
            "API_KEY=private-one bearer private-two token count request_id=keep Cookie: private-three",
        );
        assert!(!text.contains("private-"));
        assert!(text.contains("token count request_id=keep"));
    }

    #[test]
    fn redacts_secret_like_values() {
        let text = sanitize_text("api_key=sk-test Authorization: Bearer secret-value");
        assert!(!text.contains("sk-test"));
        assert!(!text.contains("secret-value"));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_whitespace_and_quoted_secret_values() {
        let text = sanitize_text(
            r#"{"apiKey": "sk-json-secret", "password": 'two words'} token = plain-secret"#,
        );
        assert!(!text.contains("sk-json-secret"));
        assert!(!text.contains("two words"));
        assert!(!text.contains("plain-secret"));
        assert_eq!(text.matches("[REDACTED]").count(), 3);
    }

    #[test]
    fn redacts_nested_json_and_url_credentials_before_log_normalization() {
        let text = sanitize_text(concat!(
            "HTTP 401 request_id=req-log\n",
            "details={\"apiKey\":\"nested-log-secret\"} ",
            "url=https://user:password@example.invalid/v1?token=query-log-secret&request_id=req-url"
        ));
        assert!(!text.contains("nested-log-secret"));
        assert!(!text.contains("password"));
        assert!(!text.contains("query-log-secret"));
        assert!(text.contains("HTTP 401 request_id=req-log\\n"));
        assert!(text.contains("request_id=req-url"));
        assert!(text.matches("[REDACTED]").count() >= 2);
    }

    #[test]
    fn truncates_unicode_diagnostics_on_a_character_boundary() {
        let text = sanitize_text(&"界".repeat(2_000));
        assert!(text.ends_with("...(truncated)"));
        assert!(text.len() <= 4_000 + "...(truncated)".len());
    }

    #[test]
    fn summarizes_json_without_values() {
        let summary = summarize_value_for_log(&json!({ "apiKey": "hidden", "prompt": "private" }));
        assert!(summary.contains("apiKey"));
        assert!(!summary.contains("hidden"));
        assert!(!summary.contains("private"));
    }

    #[test]
    fn summarizes_acp_text_event_without_content() {
        let params = json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "private reply" }
            }
        });
        let summary = summarize_acp_event_for_log("session/update", &params).unwrap();
        assert_eq!(
            summary,
            "update_tag=agent_message_chunk chunk_type=text text_len=13"
        );
        assert!(!summary.contains("private reply"));
    }
}
