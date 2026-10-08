//! 本地运行时观测状态、直方图和有界诊断摘要。
//!
//! 该模块只保存脱敏后的摘要，不保存 prompt、模型输出、HTTP header、API Key
//! 或完整请求正文。所有 duration/TTFT 都由产生观测的一侧用单调时钟计算后再
//! 传入；这里的 Unix 时间戳只用于排序和导出，不参与跨进程耗时计算。

use super::sanitize_text;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

/// 观测协议当前版本；改变字段语义时必须递增，避免面板误读旧导出。
pub const OBSERVABILITY_SCHEMA: u32 = 2;
/// 环形缓存的容量上限，保证长时间运行不会由观测本身无限增长。
pub const MAX_METRIC_POINTS: usize = 2_048;
pub const MAX_METRIC_KEYS: usize = 256;
pub const MAX_HISTOGRAMS: usize = 64;
pub const MAX_TRACE_SAMPLES: usize = 512;
pub const MAX_RESOURCE_SAMPLES: usize = 360;
pub const MAX_STARTUP_PHASES: usize = 64;
pub const MAX_CRASH_RECORDS: usize = 32;
pub const MAX_EVENT_RECORDS: usize = 1_024;
pub const MAX_EXPORT_BYTES: usize = 256 * 1024;
pub const MAX_OBSERVATION_TEXT_BYTES: usize = 2_000;
pub const MAX_CRASH_BACKTRACE_BYTES: usize = 8_000;
/// 标签和实时事件是外部边界，除条数外还必须有字节预算。
pub const MAX_TAG_BYTES: usize = 4_096;
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 8_192;

/// 固定的延迟桶边界；最后一个桶表示大于最后边界的所有值。
pub const DEFAULT_HISTOGRAM_BOUNDARIES_MS: &[f64] = &[
    1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0, 30_000.0,
    60_000.0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicy {
    pub metric_points: usize,
    pub trace_samples: usize,
    pub resource_samples: usize,
    pub startup_phases: usize,
    pub crash_records: usize,
    pub event_records: usize,
    pub max_export_bytes: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            metric_points: MAX_METRIC_POINTS,
            trace_samples: MAX_TRACE_SAMPLES,
            resource_samples: MAX_RESOURCE_SAMPLES,
            startup_phases: MAX_STARTUP_PHASES,
            crash_records: MAX_CRASH_RECORDS,
            event_records: MAX_EVENT_RECORDS,
            max_export_bytes: MAX_EXPORT_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricPoint {
    pub name: String,
    pub value: f64,
    pub unit: String,
    /// Unix epoch 毫秒仅用于导出排序；不能和另一个进程的时钟相减。
    pub occurred_at_ms: u64,
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistogramSnapshot {
    pub name: String,
    pub boundaries: Vec<f64>,
    /// 每个桶为独立计数，最后一项是 `+Inf`，不是累计计数。
    pub bucket_counts: Vec<u64>,
    pub count: u64,
    pub sum: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSample {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    /// 产生端记录的 Unix epoch 时间；不用于跨进程时长计算。
    pub started_at_ms: u64,
    /// 产生端用单调时钟计算后的时长。
    pub duration_ms: Option<u64>,
    /// 从请求开始到首个有效输出的单调时钟时长。
    pub ttft_ms: Option<u64>,
    pub status: String,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSample {
    pub occurred_at_ms: u64,
    pub process_id: u32,
    pub cpu_percent: Option<f64>,
    pub resident_bytes: Option<u64>,
    /// Windows `PrivateUsage` 聚合值；其他平台或读取失败时为 `None`。
    pub private_bytes: Option<u64>,
    pub virtual_bytes: Option<u64>,
    /// 当前 GPUI NativeHost 进程采样次数；不包含浏览器或其他外部进程。
    pub process_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupPhase {
    pub phase: String,
    pub occurred_at_ms: u64,
    /// 从同一进程的启动 `Instant` 计算出的 elapsed，不跨进程相减。
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashRecord {
    pub occurred_at_ms: u64,
    pub kind: String,
    pub message: String,
    pub backtrace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilityEvent {
    pub sequence: u64,
    pub occurred_at_ms: u64,
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilitySnapshot {
    pub schema: u32,
    pub captured_at_ms: u64,
    pub retention: RetentionPolicy,
    pub counters: BTreeMap<String, u64>,
    pub gauges: BTreeMap<String, f64>,
    pub metric_points: Vec<MetricPoint>,
    pub histograms: Vec<HistogramSnapshot>,
    pub traces: Vec<TraceSample>,
    pub resource_samples: Vec<ResourceSample>,
    pub startup_phases: Vec<StartupPhase>,
    pub crashes: Vec<CrashRecord>,
    pub events: Vec<ObservabilityEvent>,
}

#[derive(Debug, Clone)]
struct Histogram {
    boundaries: Vec<f64>,
    bucket_counts: Vec<u64>,
    count: u64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
}

impl Histogram {
    fn new(boundaries: &[f64]) -> Self {
        let mut boundaries = boundaries
            .iter()
            .copied()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .collect::<Vec<_>>();
        boundaries.sort_by(f64::total_cmp);
        boundaries.dedup_by(|left, right| left.total_cmp(right).is_eq());
        Self {
            bucket_counts: vec![0; boundaries.len() + 1],
            boundaries,
            count: 0,
            sum: 0.0,
            min: None,
            max: None,
        }
    }

    fn record(&mut self, value: f64) -> bool {
        if !value.is_finite() || value < 0.0 {
            return false;
        }
        let bucket = self
            .boundaries
            .iter()
            .position(|boundary| value <= *boundary)
            .unwrap_or(self.boundaries.len());
        self.bucket_counts[bucket] = self.bucket_counts[bucket].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.sum += value;
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
        true
    }

    fn snapshot(&self, name: &str) -> HistogramSnapshot {
        HistogramSnapshot {
            name: name.to_owned(),
            boundaries: self.boundaries.clone(),
            bucket_counts: self.bucket_counts.clone(),
            count: self.count,
            sum: self.sum,
            min: self.min,
            max: self.max,
        }
    }
}

#[derive(Debug, Default)]
struct StoreState {
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, f64>,
    metric_points: VecDeque<MetricPoint>,
    histograms: BTreeMap<String, Histogram>,
    traces: VecDeque<TraceSample>,
    resource_samples: VecDeque<ResourceSample>,
    startup_phases: VecDeque<StartupPhase>,
    crashes: VecDeque<CrashRecord>,
    events: VecDeque<ObservabilityEvent>,
}

/// 进程内有界观测存储；原生诊断页按需读取，不启动常驻订阅和采样器。
pub struct ObservabilityStore {
    state: Mutex<StoreState>,
    next_sequence: AtomicU64,
    retention: RetentionPolicy,
}

impl Default for ObservabilityStore {
    fn default() -> Self {
        Self::new(RetentionPolicy::default())
    }
}

impl std::fmt::Debug for ObservabilityStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObservabilityStore")
            .field("retention", &self.retention)
            .finish_non_exhaustive()
    }
}

impl ObservabilityStore {
    pub fn new(retention: RetentionPolicy) -> Self {
        // 调用方可在测试中缩小容量，但不能通过自定义策略突破生产上限。
        let retention = RetentionPolicy {
            metric_points: retention.metric_points.min(MAX_METRIC_POINTS),
            trace_samples: retention.trace_samples.min(MAX_TRACE_SAMPLES),
            resource_samples: retention.resource_samples.min(MAX_RESOURCE_SAMPLES),
            startup_phases: retention.startup_phases.min(MAX_STARTUP_PHASES),
            crash_records: retention.crash_records.min(MAX_CRASH_RECORDS),
            event_records: retention.event_records.min(MAX_EVENT_RECORDS),
            max_export_bytes: retention.max_export_bytes.min(MAX_EXPORT_BYTES),
        };
        Self {
            state: Mutex::new(StoreState::default()),
            next_sequence: AtomicU64::new(1),
            retention,
        }
    }

    pub fn increment_counter(&self, name: &str, delta: u64) {
        let name = safe_name(name);
        if let Ok(mut state) = self.state.lock() {
            if !state.counters.contains_key(&name) && state.counters.len() >= MAX_METRIC_KEYS {
                return;
            }
            let value = state.counters.entry(name.clone()).or_default();
            *value = value.saturating_add(delta);
        }
        self.emit("counter", json!({ "name": name, "delta": delta }));
    }

    pub fn set_gauge(&self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        let name = safe_name(name);
        if let Ok(mut state) = self.state.lock() {
            if !state.gauges.contains_key(&name) && state.gauges.len() >= MAX_METRIC_KEYS {
                return;
            }
            state.gauges.insert(name.clone(), value);
        }
        self.emit("gauge", json!({ "name": name, "value": value }));
    }

    pub fn record_metric(
        &self,
        name: &str,
        value: f64,
        unit: &str,
        tags: impl IntoIterator<Item = (String, String)>,
    ) {
        if !value.is_finite() {
            return;
        }
        let point = MetricPoint {
            name: safe_name(name),
            value,
            unit: safe_name(unit),
            occurred_at_ms: epoch_ms(),
            tags: safe_tags(tags),
        };
        if let Ok(mut state) = self.state.lock() {
            push_bounded(
                &mut state.metric_points,
                point.clone(),
                self.retention.metric_points,
            );
        }
        self.emit("metric", serde_json::to_value(point).unwrap_or(Value::Null));
    }

    pub fn record_histogram(&self, name: &str, value_ms: f64) {
        let name = safe_name(name);
        let accepted = if let Ok(mut state) = self.state.lock() {
            if !state.histograms.contains_key(&name) && state.histograms.len() >= MAX_HISTOGRAMS {
                return;
            }
            state
                .histograms
                .entry(name.clone())
                .or_insert_with(|| Histogram::new(DEFAULT_HISTOGRAM_BOUNDARIES_MS))
                .record(value_ms)
        } else {
            false
        };
        if accepted {
            self.emit("histogram", json!({ "name": name, "valueMs": value_ms }));
        }
    }

    /// TTFT 只接受同一产生端用单调时钟算出的非负时长。
    pub fn record_ttft(&self, value_ms: f64) {
        self.record_histogram("runtime.ttft_ms", value_ms);
    }

    pub fn record_trace(&self, trace: TraceSample) {
        let trace = sanitize_trace(trace);
        if let Ok(mut state) = self.state.lock() {
            push_bounded(
                &mut state.traces,
                trace.clone(),
                self.retention.trace_samples,
            );
        }
        self.emit("trace", serde_json::to_value(trace).unwrap_or(Value::Null));
    }

    pub fn record_resource_sample(&self, sample: ResourceSample) {
        let sample = sanitize_resource_sample(sample);
        if let Ok(mut state) = self.state.lock() {
            push_bounded(
                &mut state.resource_samples,
                sample.clone(),
                self.retention.resource_samples,
            );
        }
        self.emit(
            "resource",
            serde_json::to_value(sample).unwrap_or(Value::Null),
        );
    }

    pub fn record_startup_phase(&self, phase: StartupPhase) {
        let phase = StartupPhase {
            phase: safe_name(&phase.phase),
            occurred_at_ms: phase.occurred_at_ms,
            elapsed_ms: phase.elapsed_ms,
        };
        if let Ok(mut state) = self.state.lock() {
            push_bounded(
                &mut state.startup_phases,
                phase.clone(),
                self.retention.startup_phases,
            );
        }
        self.emit(
            "startup",
            serde_json::to_value(phase).unwrap_or(Value::Null),
        );
    }

    pub fn record_crash(&self, crash: CrashRecord) {
        let crash = sanitize_crash_record(crash);
        if let Ok(mut state) = self.state.lock() {
            push_bounded(
                &mut state.crashes,
                crash.clone(),
                self.retention.crash_records,
            );
        }
        self.emit("crash", serde_json::to_value(crash).unwrap_or(Value::Null));
    }

    /// 从上一次进程的 crash spool 恢复记录；恢复本身不产生实时事件。
    pub(crate) fn restore_crash_record(&self, crash: CrashRecord) {
        let crash = sanitize_crash_record(crash);
        if let Ok(mut state) = self.state.lock() {
            push_bounded(&mut state.crashes, crash, self.retention.crash_records);
        }
    }

    pub fn snapshot(&self) -> ObservabilitySnapshot {
        let state = self.state.lock().ok();
        let histograms = state
            .as_ref()
            .map(|state| {
                state
                    .histograms
                    .iter()
                    .map(|(name, histogram)| histogram.snapshot(name))
                    .collect()
            })
            .unwrap_or_default();
        ObservabilitySnapshot {
            schema: OBSERVABILITY_SCHEMA,
            captured_at_ms: epoch_ms(),
            retention: self.retention,
            counters: state
                .as_ref()
                .map(|state| state.counters.clone())
                .unwrap_or_default(),
            gauges: state
                .as_ref()
                .map(|state| state.gauges.clone())
                .unwrap_or_default(),
            metric_points: state
                .as_ref()
                .map(|state| state.metric_points.iter().cloned().collect())
                .unwrap_or_default(),
            histograms,
            traces: state
                .as_ref()
                .map(|state| state.traces.iter().cloned().collect())
                .unwrap_or_default(),
            resource_samples: state
                .as_ref()
                .map(|state| state.resource_samples.iter().cloned().collect())
                .unwrap_or_default(),
            startup_phases: state
                .as_ref()
                .map(|state| state.startup_phases.iter().cloned().collect())
                .unwrap_or_default(),
            crashes: state
                .as_ref()
                .map(|state| state.crashes.iter().cloned().collect())
                .unwrap_or_default(),
            events: state
                .as_ref()
                .map(|state| state.events.iter().cloned().collect())
                .unwrap_or_default(),
        }
    }

    /// 导出仅包含脱敏摘要的 JSON；超出上限时收缩历史数组而不截断 JSON。
    pub fn export_redacted(&self) -> Result<String, String> {
        let snapshot = self.snapshot();
        let serialized = serde_json::to_string_pretty(&snapshot)
            .map_err(|error| format!("序列化运行时观测失败：{error}"))?;
        if serialized.len() <= self.retention.max_export_bytes {
            return Ok(serialized);
        }
        let mut compact = snapshot;
        compact.metric_points.clear();
        compact.events.clear();
        retain_last(&mut compact.traces, 64);
        retain_last(&mut compact.resource_samples, 64);
        retain_last(&mut compact.startup_phases, 32);
        // 超限时逐步收缩历史 Crash，但始终优先保留最新一条；不能用截断字符串，
        // 否则导出的 JSON 会失效且可能把脱敏边界截断在敏感字段中间。
        let crashes = compact.crashes.clone();
        let mut crash_limit = crashes.len().min(16);
        loop {
            compact.crashes = crashes.clone();
            retain_last(&mut compact.crashes, crash_limit);
            let serialized = serde_json::to_string_pretty(&compact)
                .map_err(|error| format!("序列化压缩观测失败：{error}"))?;
            if serialized.len() <= self.retention.max_export_bytes {
                return Ok(serialized);
            }
            if crash_limit == 0 {
                break;
            }
            crash_limit -= 1;
        }

        // 极小的自定义导出上限下，进一步释放次要历史数组，再尝试保留最新 Crash。
        compact.traces.clear();
        compact.resource_samples.clear();
        compact.startup_phases.clear();
        compact.histograms.clear();
        compact.counters.clear();
        compact.gauges.clear();
        compact.crashes = crashes;
        retain_last(&mut compact.crashes, 1);
        let serialized = serde_json::to_string_pretty(&compact)
            .map_err(|error| format!("序列化压缩观测失败：{error}"))?;
        if serialized.len() <= self.retention.max_export_bytes {
            return Ok(serialized);
        }
        Err("运行时观测导出超过容量上限".to_owned())
    }

    fn emit(&self, kind: &str, payload: Value) {
        let payload = bounded_event_payload(payload);
        let event = ObservabilityEvent {
            sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed),
            occurred_at_ms: epoch_ms(),
            kind: safe_name(kind),
            payload,
        };
        if let Ok(mut state) = self.state.lock() {
            push_bounded(
                &mut state.events,
                event.clone(),
                self.retention.event_records,
            );
        }
    }
}

pub(crate) fn sanitize_crash_record(crash: CrashRecord) -> CrashRecord {
    CrashRecord {
        occurred_at_ms: crash.occurred_at_ms,
        kind: safe_name(&crash.kind),
        // Panic payload 可能是用户文本或包含绝对路径；这里二次清理，
        // 即使调用方误传原始值，导出也不会原样保留。
        message: sanitize_crash_text(&crash.message, MAX_OBSERVATION_TEXT_BYTES),
        backtrace: crash
            .backtrace
            .map(|value| sanitize_crash_text(&value, MAX_CRASH_BACKTRACE_BYTES)),
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) {
    if capacity == 0 {
        return;
    }
    while queue.len() >= capacity {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn retain_last<T>(values: &mut Vec<T>, capacity: usize) {
    if values.len() > capacity {
        let first = values.len() - capacity;
        values.drain(..first);
    }
}

fn safe_name(value: &str) -> String {
    bounded_text(value)
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '/') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn bounded_text(value: &str) -> String {
    let sanitized = sanitize_text(value);
    if sanitized.len() <= MAX_OBSERVATION_TEXT_BYTES {
        return sanitized;
    }
    let mut end = MAX_OBSERVATION_TEXT_BYTES;
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...(truncated)", &sanitized[..end])
}

/// Crash 观测只保留诊断结构，不保留可能包含 prompt、文件正文或绝对路径的原文。
fn sanitize_crash_text(value: &str, maximum_bytes: usize) -> String {
    // 先按原始换行/空白识别堆栈中的路径，再把换行转义为单行日志文本；
    // 反向处理会让 `at C:\\...` 与前一行粘连，绕过路径起始判断。
    let value = redact_absolute_paths(value);
    let value = sanitize_text(&value);
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...(truncated)", &value[..end])
}

pub(crate) fn redact_absolute_paths(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut token = String::new();
    let flush = |output: &mut String, token: &mut String| {
        if token.is_empty() {
            return;
        }
        // URL 也包含 `scheme:/`，不能把其中的 `s:/` 误识别为 Windows 盘符；
        // URL 的安全字段交给模型层继续脱敏，保留可定位的 request_id 等上下文。
        let is_uri = token_contains_http_uri(token);
        let is_windows = !is_uri
            && token.as_bytes().windows(3).any(|window| {
                window[0].is_ascii_alphabetic()
                    && window[1] == b':'
                    && matches!(window[2], b'\\' | b'/')
            });
        let is_unc = !is_uri && token.contains("\\\\");
        let is_unix = [
            "/Users/",
            "/home/",
            "/private/",
            "/tmp/",
            "/var/",
            "/opt/",
            "/workspace/",
            "/root/",
            "/mnt/",
            "/srv/",
            "/run/",
            "/etc/",
        ]
        .iter()
        .any(|prefix| !is_uri && token.contains(prefix));
        if is_windows || is_unc || is_unix {
            output.push_str("[PATH]");
            if let Some(suffix) = path_location_suffix(token) {
                output.push_str(suffix);
            }
        } else {
            output.push_str(token);
        }
        token.clear();
    };
    for character in value.chars() {
        if character.is_whitespace() || matches!(character, ')' | ']' | '}' | ',' | ';' | '>') {
            flush(&mut output, &mut token);
            output.push(character);
        } else {
            token.push(character);
        }
    }
    flush(&mut output, &mut token);
    output
}

/// 路径 token 末尾的 `:line:column` 是诊断定位信息，脱敏路径时必须保留。
fn path_location_suffix(value: &str) -> Option<&str> {
    let column_start = value.rfind(':')?;
    let column = &value[column_start + 1..];
    if column.is_empty() || !column.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let line_start = value[..column_start].rfind(':')?;
    let line = &value[line_start + 1..column_start];
    if line.is_empty() || !line.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(&value[line_start..])
}

fn token_contains_http_uri(token: &str) -> bool {
    let lowercase = token.to_ascii_lowercase();
    ["http:", "https:"].into_iter().any(|scheme| {
        lowercase.match_indices(scheme).any(|(index, _)| {
            if index > 0
                && (lowercase.as_bytes()[index - 1].is_ascii_alphanumeric()
                    || matches!(lowercase.as_bytes()[index - 1], b'_' | b'\\' | b'/'))
            {
                return false;
            }
            let bytes = lowercase.as_bytes();
            let mut cursor = index + scheme.len();
            let mut slash_count = 0;
            while cursor < bytes.len() {
                while bytes.get(cursor) == Some(&b'\\') {
                    cursor += 1;
                }
                if bytes.get(cursor) != Some(&b'/') {
                    break;
                }
                slash_count += 1;
                cursor += 1;
            }
            slash_count >= 2
        })
    })
}

/// 返回 panic payload 的稳定类型标签；正文由 `panic_payload_text` 经过统一出口处理。
pub(crate) fn panic_payload_kind(info: &std::panic::PanicHookInfo<'_>) -> &'static str {
    if info.payload().downcast_ref::<&str>().is_some() {
        "str"
    } else if info.payload().downcast_ref::<String>().is_some() {
        "string"
    } else {
        "opaque"
    }
}

/// 提取常见的 Rust panic 文本；未知 payload 使用固定占位符，避免把 Debug 输出写入日志。
pub(crate) fn panic_payload_text<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    if let Some(value) = info.payload().downcast_ref::<&str>() {
        return value;
    }
    if let Some(value) = info.payload().downcast_ref::<String>() {
        return value.as_str();
    }
    "<opaque panic payload>"
}

/// 将 panic 的源码位置转换为脱敏后的单行字段，并保留行列号用于定位。
pub(crate) fn panic_location_text(info: &std::panic::PanicHookInfo<'_>) -> Option<String> {
    info.location().map(|location| {
        let file = sanitize_crash_text(location.file(), MAX_OBSERVATION_TEXT_BYTES);
        format!("{file}:{}:{}", location.line(), location.column())
    })
}

fn format_panic_message_parts(payload_kind: &str, payload: &str, location: Option<&str>) -> String {
    let payload = sanitize_crash_text(payload, MAX_OBSERVATION_TEXT_BYTES);
    let location = location
        .map(|value| sanitize_crash_text(value, MAX_OBSERVATION_TEXT_BYTES))
        .unwrap_or_else(|| "<unknown>".to_owned());
    format!("panic payload type={payload_kind} location={location} message={payload}")
}

/// 构造可直接写入 Crash 和文本日志的 panic 摘要；位置放在正文前，避免超长正文截断定位信息。
pub(crate) fn format_panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let location = panic_location_text(info);
    format_panic_message_parts(
        panic_payload_kind(info),
        panic_payload_text(info),
        location.as_deref(),
    )
}

/// Panic 回溯可能包含绝对路径和超长内部细节；文本日志与结构化 Crash 共用同一
/// 有界脱敏出口，避免只保护导出而把原文留在本地日志中。
pub(crate) fn sanitize_panic_backtrace(value: &str) -> String {
    sanitize_crash_text(value, MAX_CRASH_BACKTRACE_BYTES)
}

pub(crate) fn now_epoch_ms() -> u64 {
    epoch_ms()
}

fn safe_tags(tags: impl IntoIterator<Item = (String, String)>) -> BTreeMap<String, String> {
    let mut output = BTreeMap::new();
    let mut bytes = 0usize;
    for (key, value) in tags.into_iter().take(16) {
        let key = safe_name(&key);
        let value = bounded_text(&value);
        let cost = key.len().saturating_add(value.len());
        if bytes.saturating_add(cost) > MAX_TAG_BYTES {
            break;
        }
        bytes = bytes.saturating_add(cost);
        output.insert(key, value);
    }
    output
}

fn bounded_event_payload(payload: Value) -> Value {
    let Ok(encoded) = serde_json::to_vec(&payload) else {
        return json!({"truncated": true, "reason": "serialization_failed"});
    };
    if encoded.len() <= MAX_EVENT_PAYLOAD_BYTES {
        return payload;
    }
    json!({
        "truncated": true,
        "bytes": encoded.len(),
    })
}

fn sanitize_trace(mut trace: TraceSample) -> TraceSample {
    trace.trace_id = safe_name(&trace.trace_id);
    trace.span_id = safe_name(&trace.span_id);
    trace.parent_span_id = trace.parent_span_id.map(|value| safe_name(&value));
    trace.name = safe_name(&trace.name);
    trace.status = safe_name(&trace.status);
    trace.attributes = safe_tags(trace.attributes);
    trace
}

fn sanitize_resource_sample(mut sample: ResourceSample) -> ResourceSample {
    sample.cpu_percent = sample
        .cpu_percent
        .filter(|value| value.is_finite() && *value >= 0.0);
    sample
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource_sample(index: u64) -> ResourceSample {
        ResourceSample {
            occurred_at_ms: index,
            process_id: 1,
            cpu_percent: Some(index as f64),
            resident_bytes: Some(index),
            private_bytes: None,
            virtual_bytes: None,
            process_count: None,
        }
    }

    #[test]
    fn histogram_uses_fixed_non_cumulative_buckets_and_rejects_invalid_values() {
        let mut histogram = Histogram::new(&[10.0, 100.0]);
        assert!(histogram.record(10.0));
        assert!(histogram.record(101.0));
        assert!(!histogram.record(-1.0));
        assert!(!histogram.record(f64::NAN));
        assert_eq!(histogram.bucket_counts, vec![1, 0, 1]);
        assert_eq!(histogram.count, 2);
        assert_eq!(histogram.min, Some(10.0));
        assert_eq!(histogram.max, Some(101.0));
    }

    #[test]
    fn retention_evicts_old_samples_and_keeps_snapshot_bounded() {
        let store = ObservabilityStore::new(RetentionPolicy {
            metric_points: 2,
            trace_samples: 2,
            resource_samples: 2,
            startup_phases: 2,
            crash_records: 2,
            event_records: 3,
            max_export_bytes: MAX_EXPORT_BYTES,
        });
        for index in 0..5 {
            store.record_metric("test", index as f64, "count", []);
            store.record_resource_sample(resource_sample(index));
        }
        let snapshot = store.snapshot();
        assert_eq!(snapshot.metric_points.len(), 2);
        assert_eq!(snapshot.metric_points[0].value, 3.0);
        assert_eq!(snapshot.resource_samples.len(), 2);
        assert_eq!(snapshot.resource_samples[0].occurred_at_ms, 3);
        assert!(snapshot.events.len() <= 3);
    }

    #[test]
    fn retention_policy_cannot_expand_production_bounds() {
        let store = ObservabilityStore::new(RetentionPolicy {
            metric_points: usize::MAX,
            trace_samples: usize::MAX,
            resource_samples: usize::MAX,
            startup_phases: usize::MAX,
            crash_records: usize::MAX,
            event_records: usize::MAX,
            max_export_bytes: usize::MAX,
        });
        assert_eq!(store.snapshot().retention.metric_points, MAX_METRIC_POINTS);
        assert_eq!(store.snapshot().retention.trace_samples, MAX_TRACE_SAMPLES);
        assert_eq!(
            store.snapshot().retention.resource_samples,
            MAX_RESOURCE_SAMPLES
        );
        assert_eq!(
            store.snapshot().retention.startup_phases,
            MAX_STARTUP_PHASES
        );
        assert_eq!(store.snapshot().retention.crash_records, MAX_CRASH_RECORDS);
        assert_eq!(store.snapshot().retention.event_records, MAX_EVENT_RECORDS);
        assert_eq!(
            store.snapshot().retention.max_export_bytes,
            MAX_EXPORT_BYTES
        );
    }

    #[test]
    fn export_is_valid_json_and_contains_retention_contract() {
        let store = ObservabilityStore::default();
        store.record_crash(CrashRecord {
            occurred_at_ms: 1,
            kind: "panic".to_owned(),
            message: r#"panic payload contains api_key=hidden and C:\Users\secret\project\file.rs"#
                .to_owned(),
            backtrace: Some("at C:\\Users\\secret\\project\\file.rs:10:2".to_owned()),
        });
        let text = store.export_redacted().unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema"], OBSERVABILITY_SCHEMA);
        assert!(text.contains("retention"));
        assert!(!text.contains("hidden"));
        assert!(!text.contains("C:\\Users\\secret"));
    }

    #[test]
    fn crash_text_is_bounded_and_absolute_paths_are_removed() {
        let store = ObservabilityStore::default();
        store.record_crash(CrashRecord {
            occurred_at_ms: 1,
            kind: "panic".to_owned(),
            message: "x".repeat(MAX_OBSERVATION_TEXT_BYTES * 2),
            backtrace: Some(format!("C:\\Users\\name\\{}", "x".repeat(20_000))),
        });
        let crash = &store.snapshot().crashes[0];
        assert!(crash.message.len() <= MAX_OBSERVATION_TEXT_BYTES + "...(truncated)".len());
        let backtrace = crash.backtrace.as_deref().unwrap();
        assert!(!backtrace.contains("C:\\Users\\name"));
        assert!(backtrace.len() <= MAX_CRASH_BACKTRACE_BYTES + "...(truncated)".len());

        store.record_crash(CrashRecord {
            occurred_at_ms: 2,
            kind: "native.window_error".to_owned(),
            message: "native window error".to_owned(),
            backtrace: Some(
                "Error: render failed\n    at C:\\Users\\name\\project\\App.tsx:10:2\nsource=C:\\Users\\name\\project\\main.tsx"
                    .to_owned(),
            ),
        });
        let crash = store.snapshot().crashes.last().cloned().unwrap();
        assert!(!crash.backtrace.unwrap().contains("C:\\Users\\name"));
    }

    #[test]
    fn panic_message_keeps_diagnostic_context_while_redacting_secrets_and_paths() {
        let message = format_panic_message_parts(
            "string",
            "release12 double lease api_key=secret C:\\Users\\secret\\project\\window.rs",
            Some("C:\\Users\\secret\\project\\main.rs:123:45"),
        );

        assert!(message.contains("panic payload type=string"));
        assert!(message.contains("release12 double lease"));
        assert!(message.contains("location=[PATH]:123:45"));
        assert!(message.contains("message="));
        assert!(!message.contains("api_key=secret"));
        assert!(!message.contains("C:\\Users\\secret"));
        assert!(!message.contains("secret\\project"));
        assert!(message.len() <= MAX_OBSERVATION_TEXT_BYTES * 2 + 128);
    }

    #[test]
    fn panic_message_bounds_long_payload_without_losing_location() {
        let message = format_panic_message_parts(
            "str",
            &format!(
                "release12 double lease {}",
                "x".repeat(MAX_OBSERVATION_TEXT_BYTES * 2)
            ),
            Some("C:\\Users\\secret\\project\\main.rs:7:8"),
        );

        assert!(message.starts_with("panic payload type=str location=[PATH]:7:8 message="));
        assert!(message.contains("...(truncated)"));
        assert!(!message.contains("C:\\Users\\secret"));
    }

    #[test]
    fn export_compaction_keeps_latest_crash_and_event_payloads_are_bounded() {
        let store = ObservabilityStore::new(RetentionPolicy {
            crash_records: 32,
            max_export_bytes: 4_096,
            ..RetentionPolicy::default()
        });
        for index in 0..32 {
            store.record_crash(CrashRecord {
                occurred_at_ms: index,
                kind: "panic".to_owned(),
                message: format!("crash-{index}-{}", "x".repeat(120)),
                backtrace: None,
            });
        }
        store.record_metric(
            "large",
            1.0,
            "count",
            [("tag".to_owned(), "x".repeat(20_000))],
        );
        let snapshot = store.snapshot();
        assert_eq!(snapshot.crashes.last().unwrap().occurred_at_ms, 31);
        let exported = store.export_redacted().unwrap();
        assert!(exported.contains("\"occurredAtMs\": 31"));
        assert!(snapshot.events.iter().all(|event| {
            serde_json::to_vec(&event.payload)
                .map(|payload| payload.len() <= MAX_EVENT_PAYLOAD_BYTES)
                .unwrap_or(false)
        }));
    }
}
