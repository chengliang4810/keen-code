//! 原生真实 Provider 验收的有界线级观测。
//!
//! 该模块只在 benchmark 与 native acceptance 同时显式开启时读取 trace 路径。
//! Provider 已经把请求正文、响应正文和 Header 排除在 [`RequestObservation`]
//! 之外；这里进一步省略 endpoint、错误正文和 Provider request id，只落盘短
//! metadata、生命周期、attempt、状态、时间和 usage。

use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result as AnyResult, bail};
use keencode_provider::{
    RequestErrorKind, RequestMode, RequestObservation, RequestObservationScope,
    RequestObservationState, RequestObserver,
};
use serde::Serialize;

use crate::analytics::AnalyticsRecorder;

const TRACE_ENV: &str = "KEENCODE_NATIVE_WIRE_TRACE";
const BENCHMARK_ENV: &str = "KEENCODE_BENCHMARK";
const ACCEPTANCE_ENV: &str = "KEENCODE_NATIVE_ACCEPTANCE";
const TRACE_SCHEMA: &str = "keencode/native-wire-trace";
const TRACE_VERSION: u32 = 1;
const TRACE_QUEUE_CAPACITY: usize = 256;
const MAX_TRACE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TRACE_RECORD_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 512;

/// Analytics 与原生验收 trace 的组合观察器。
///
/// `sink` 为 `None` 时仍保留统一 Provider 观察入口，但不会读取或创建任何
/// 验收文件；因此普通生产启动路径不会因为验收环境变量缺失而改变行为。
pub(crate) struct NativeWireTraceObserver {
    analytics: Arc<AnalyticsRecorder>,
    sink: Option<TraceSink>,
}

impl NativeWireTraceObserver {
    /// 创建组合观察器；只有两个显式开关都为 `1` 且提供路径时才开启落盘。
    pub(crate) fn from_environment(analytics: Arc<AnalyticsRecorder>) -> AnyResult<Arc<Self>> {
        let enabled = std::env::var(BENCHMARK_ENV).as_deref() == Ok("1")
            && std::env::var(ACCEPTANCE_ENV).as_deref() == Ok("1");
        let sink = if enabled {
            let raw_path = std::env::var_os(TRACE_ENV)
                .context("原生验收已开启，但缺少 KEENCODE_NATIVE_WIRE_TRACE")?;
            let path = PathBuf::from(raw_path);
            Some(TraceSink::open(path)?)
        } else {
            None
        };
        Ok(Arc::new(Self { analytics, sink }))
    }

    /// 等待已接收的 trace 元数据同步到文件；禁用时为空操作。
    pub(crate) fn flush(&self) -> std::result::Result<(), String> {
        let Some(sink) = &self.sink else {
            return Ok(());
        };
        sink.flush()
    }
}

impl RequestObserver for NativeWireTraceObserver {
    fn on_request(&self, observation: RequestObservation) {
        // Analytics 自身只接收同一份 Provider 中立观测；clone 不含正文或凭据。
        self.analytics.record_request(observation.clone());
        let Some(sink) = &self.sink else {
            return;
        };
        // 观察接口不能阻塞模型请求；trace 队列满时丢弃新记录，最终报告仍会
        // 通过文件上限和已有记录明确标出实际可观察范围。
        let _ = sink.try_send(TraceRecord::from_observation(&observation));
    }
}

struct TraceSink {
    sender: Option<mpsc::SyncSender<TraceCommand>>,
    join: Option<thread::JoinHandle<()>>,
}

enum TraceCommand {
    Record(Box<TraceRecord>),
    Flush(mpsc::SyncSender<std::result::Result<(), String>>),
}

impl TraceSink {
    fn open(path: PathBuf) -> AnyResult<Self> {
        validate_trace_path(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("打开原生 Provider trace 失败: {}", path.display()))?;
        let existing_bytes = file
            .metadata()
            .with_context(|| format!("读取原生 Provider trace 元数据失败: {}", path.display()))?
            .len();
        if existing_bytes > MAX_TRACE_BYTES {
            bail!(
                "原生 Provider trace 已超过 {} MiB: {}",
                MAX_TRACE_BYTES / (1024 * 1024),
                path.display()
            );
        }
        let (sender, receiver) = mpsc::sync_channel(TRACE_QUEUE_CAPACITY);
        let join = thread::Builder::new()
            .name("keencode-native-wire-trace".to_owned())
            .spawn(move || trace_writer(file, existing_bytes, receiver))
            .context("启动原生 Provider trace writer")?;
        Ok(Self {
            sender: Some(sender),
            join: Some(join),
        })
    }

    fn try_send(&self, record: TraceRecord) -> Result<(), ()> {
        self.sender
            .as_ref()
            .ok_or(())?
            .try_send(TraceCommand::Record(Box::new(record)))
            .map_err(|_| ())
    }

    fn flush(&self) -> std::result::Result<(), String> {
        let (reply, response) = mpsc::sync_channel(0);
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| "原生 Provider trace writer 已退出".to_owned())?;
        sender
            .send(TraceCommand::Flush(reply))
            .map_err(|_| "原生 Provider trace writer 已退出".to_owned())?;
        response
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "等待原生 Provider trace 同步超时".to_owned())?
    }
}

impl Drop for TraceSink {
    fn drop(&mut self) {
        // 关闭 sender 后 writer 会排空已接收记录并 flush；若 writer 已经因 I/O
        // 错误退出，join 只回收线程而不把错误反向注入 Provider。
        self.sender.take();
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

fn trace_writer(file: File, existing_bytes: u64, receiver: mpsc::Receiver<TraceCommand>) {
    let mut writer = BufWriter::new(file);
    let mut bytes_written = existing_bytes;
    while let Ok(command) = receiver.recv() {
        match command {
            TraceCommand::Record(record) => {
                let Ok(mut line) = serde_json::to_vec(record.as_ref()) else {
                    continue;
                };
                line.push(b'\n');
                if line.len() > MAX_TRACE_RECORD_BYTES
                    || bytes_written.saturating_add(line.len() as u64) > MAX_TRACE_BYTES
                {
                    continue;
                }
                if writer.write_all(&line).is_err() {
                    break;
                }
                bytes_written = bytes_written.saturating_add(line.len() as u64);
            }
            TraceCommand::Flush(reply) => {
                let result = writer
                    .flush()
                    .and_then(|_| writer.get_ref().sync_data())
                    .map_err(|error| format!("同步原生 Provider trace 失败: {error}"));
                let failed = result.is_err();
                let _ = reply.send(result);
                if failed {
                    break;
                }
            }
        }
    }
    let _ = writer.flush();
    let _ = writer.get_ref().sync_data();
}

fn validate_trace_path(path: &Path) -> AnyResult<()> {
    if path.as_os_str().is_empty() {
        bail!("KEENCODE_NATIVE_WIRE_TRACE 不能为空");
    }
    if path.as_os_str().to_string_lossy().len() > 2048 {
        bail!("KEENCODE_NATIVE_WIRE_TRACE 路径过长");
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let metadata = fs::symlink_metadata(parent)
            .with_context(|| format!("读取原生 Provider trace 目录失败: {}", parent.display()))?;
        if !metadata.is_dir() {
            bail!("原生 Provider trace 父路径不是目录: {}", parent.display());
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        bail!("原生 Provider trace 路径必须是普通文件: {}", path.display());
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceRecord {
    schema: &'static str,
    version: u32,
    scope: &'static str,
    state: &'static str,
    logical_request_id: String,
    attempt: u32,
    max_attempts: u32,
    model: String,
    protocol: &'static str,
    mode: &'static str,
    at_ms: u64,
    duration_ms: Option<u64>,
    ttft_ms: Option<u64>,
    retry_delay_ms: Option<u64>,
    response_headers_at_ms: Option<u64>,
    http_status: Option<u16>,
    usage: TraceUsage,
    error_kind: Option<&'static str>,
    session_id: Option<String>,
    turn_id: Option<String>,
    agent_id: Option<String>,
    purpose: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl TraceRecord {
    fn from_observation(observation: &RequestObservation) -> Self {
        Self {
            schema: TRACE_SCHEMA,
            version: TRACE_VERSION,
            scope: match observation.scope {
                RequestObservationScope::Logical => "logical",
                RequestObservationScope::Attempt => "attempt",
            },
            state: match observation.state {
                RequestObservationState::Started => "started",
                RequestObservationState::Completed => "completed",
                RequestObservationState::Cancelled => "cancelled",
                RequestObservationState::Failed => "failed",
            },
            logical_request_id: bounded_required(&observation.logical_request_id),
            attempt: observation.attempt,
            max_attempts: observation.max_attempts,
            model: bounded_required(&observation.model),
            protocol: match observation.protocol {
                keencode_model::ProviderProtocol::Messages => "messages",
                keencode_model::ProviderProtocol::ChatCompletions => "chat_completions",
                keencode_model::ProviderProtocol::Responses => "responses",
            },
            mode: match observation.mode {
                RequestMode::Stream => "stream",
                RequestMode::Buffered => "buffered",
            },
            at_ms: observation.at_ms,
            duration_ms: observation.duration_ms,
            ttft_ms: observation.ttft_ms,
            retry_delay_ms: observation.retry_delay_ms,
            response_headers_at_ms: observation.response_headers_at_ms,
            http_status: observation.http_status,
            usage: TraceUsage {
                input_tokens: observation.usage.input_tokens,
                output_tokens: observation.usage.output_tokens,
                reasoning_tokens: observation.usage.reasoning_tokens,
                cache_read_tokens: observation.usage.cache_read_tokens,
                cache_write_tokens: observation.usage.cache_write_tokens,
                total_tokens: observation.usage.total_tokens,
            },
            error_kind: observation.error_kind.map(error_kind_name),
            session_id: bounded_optional(observation.session_id.as_deref()),
            turn_id: bounded_optional(observation.turn_id.as_deref()),
            agent_id: bounded_optional(observation.agent_id.as_deref()),
            purpose: bounded_optional(observation.purpose.as_deref()),
        }
    }
}

fn bounded_required(value: &str) -> String {
    bounded_text(value).unwrap_or_default()
}

fn bounded_optional(value: Option<&str>) -> Option<String> {
    value.and_then(bounded_text)
}

fn bounded_text(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let mut end = value.len().min(MAX_TEXT_BYTES);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_owned())
}

fn error_kind_name(kind: RequestErrorKind) -> &'static str {
    match kind {
        RequestErrorKind::Connection => "connection",
        RequestErrorKind::Timeout => "timeout",
        RequestErrorKind::Tls => "tls",
        RequestErrorKind::Transport => "transport",
        RequestErrorKind::HttpStatus => "http_status",
        RequestErrorKind::Protocol => "protocol",
        RequestErrorKind::StreamInterrupted => "stream_interrupted",
        RequestErrorKind::Cancelled => "cancelled",
        RequestErrorKind::RetryExhausted => "retry_exhausted",
        RequestErrorKind::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_text_preserves_utf8_boundary() {
        let input = "验收".repeat(300);
        let output = bounded_text(&input).expect("非空文本应保留");
        assert!(output.len() <= MAX_TEXT_BYTES);
        assert!(input.starts_with(&output));
        assert!(output.is_char_boundary(output.len()));
    }
}
