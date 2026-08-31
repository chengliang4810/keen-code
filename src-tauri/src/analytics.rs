//! 本地模型请求记录与 Token 用量统计。
//!
//! 请求事实只来自 `peri-model` 的 HTTP/SSE/retry 边界。每个物理 attempt
//! 无论成功、失败或取消都会经同步 observer 投递到无界本地队列；磁盘 I/O
//! 在独立线程执行，事件中不包含请求正文、响应正文、headers 或凭据。

use crate::storage;
use peri_model::{
    ProviderProtocol, RequestErrorKind, RequestObservation, RequestObservationScope,
    RequestObservationState, RequestObserver,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    sync::mpsc::{self, Sender, SyncSender},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::AppHandle;

const RECORD_FILE: &str = "model-request-records.jsonl";
const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_PAGE_SIZE: usize = 100;

#[derive(Debug)]
enum AnalyticsEvent {
    Request(RequestObservation),
    /// 查询命令使用屏障等待此前排队的记录完成落盘。
    Flush(SyncSender<Result<(), String>>),
}

/// 一次实际模型调用 attempt 的安全本地记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    pub id: String,
    pub logical_request_id: String,
    /// 物理 attempt 从 1 开始；请求构造阶段失败且没有发出 HTTP 时为 0。
    pub attempt: u32,
    pub max_attempts: u32,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub agent_id: Option<String>,
    pub purpose: String,
    pub model: String,
    /// 当前只保存 endpoint host，避免把用户配置中的敏感路径写入磁盘。
    pub provider: String,
    pub protocol: String,
    pub endpoint: Option<String>,
    pub request_mode: String,
    pub status: String,
    pub http_status: Option<u16>,
    pub error_kind: Option<String>,
    pub error: Option<String>,
    pub requested_at_ms: u64,
    pub first_response_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    pub duration_ms: u64,
    /// Provider 是否明确报告了 usage；未报告不能伪装成明确的 0 Token。
    pub usage_reported: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 输出 Token 中由 Provider 明确报告的推理 Token。
    pub reasoning_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub estimated: bool,
    pub provider_request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageStat {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsageStat {
    pub date: String,
    pub requests: u64,
    pub total_tokens: u64,
    pub model_tokens: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub total_requests: u64,
    pub total_tokens: u64,
    pub models: Vec<ModelUsageStat>,
    pub days: Vec<DailyUsageStat>,
}

/// 请求记录分页结果；筛选和分页都在后端执行。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecordsPage {
    pub records: Vec<RequestRecord>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
    pub models: Vec<String>,
    pub statuses: Vec<String>,
}

/// 一个任务（ACP Session）内主 Agent 成功模型请求的缓存用量汇总。
///
/// 原始 Token 数来自 Provider usage；KeenCode 只负责按任务加权汇总，
/// 不根据请求文本或前缀推测缓存命中。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCacheUsage {
    pub session_id: String,
    pub request_count: u64,
    pub input_tokens: u64,
    /// 任一成功主 Agent 请求未报告 usage 或缓存读取量时为 None；明确零命中保留 Some(0)。
    pub cache_read_tokens: Option<u64>,
    /// cache_read_tokens / input_tokens；任务内任一成功主 Agent 请求的证据
    /// 不完整、数值非法或输入 Token 为零时为 None。
    pub cache_hit_rate: Option<f64>,
    /// 最近一次成功主 Agent 请求的上下文用量；用于恢复历史任务右下角状态。
    pub latest_context_tokens: Option<u64>,
    pub latest_context_estimated: bool,
}

pub struct AnalyticsRecorder {
    sender: Sender<AnalyticsEvent>,
}

impl AnalyticsRecorder {
    pub fn new(app: &AppHandle) -> anyhow::Result<Self> {
        let path = storage::root_dir(app)?.join(RECORD_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // 启动阶段先打开文件，存储不可用时立即失败，不把审计丢失伪装成成功。
        let file = open_record_file(&path)?;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("keencode-request-history".into())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                let mut state = ObservationWriterState::default();
                let mut writer_error = None::<String>;
                while let Ok(event) = receiver.recv() {
                    match event {
                        AnalyticsEvent::Request(observation) => {
                            if writer_error.is_some() {
                                continue;
                            }
                            for record in state.observe(observation) {
                                if let Err(error) = append_record(&mut writer, &record) {
                                    // observer 不能把磁盘错误反向注入模型请求；查询屏障会
                                    // 返回同一错误，避免设置页把丢失伪装成“没有记录”。
                                    eprintln!("[keencode] {error}");
                                    writer_error = Some(error);
                                    break;
                                }
                            }
                        }
                        AnalyticsEvent::Flush(reply) => {
                            let result = if let Some(error) = writer_error.clone() {
                                Err(error)
                            } else {
                                writer
                                    .flush()
                                    .and_then(|_| writer.get_ref().sync_data())
                                    .map_err(|error| format!("同步模型请求记录失败：{error}"))
                            };
                            if let Err(error) = &result {
                                writer_error = Some(error.clone());
                            }
                            let _ = reply.send(result);
                        }
                    }
                }
                let _ = writer.flush();
                let _ = writer.get_ref().sync_data();
            })?;
        Ok(Self { sender })
    }

    /// 等待此前已接收的观测写入文件并同步到操作系统存储层。
    ///
    /// 模型请求线程仍只发送短元数据；查询命令在读取 JSONL 前调用此屏障，
    /// 避免设置页刚打开时读到异步 writer 尚未处理的旧快照。
    pub(crate) fn flush(&self) -> Result<(), String> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .send(AnalyticsEvent::Flush(reply))
            .map_err(|_| "模型请求记录 writer 已退出".to_owned())?;
        result
            .recv()
            .map_err(|_| "模型请求记录 writer 未返回 flush 结果".to_owned())?
    }
}

fn open_record_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        if permissions.mode() & 0o777 != 0o600 {
            permissions.set_mode(0o600);
            file.set_permissions(permissions)?;
        }
    }
    Ok(file)
}

fn append_record(writer: &mut BufWriter<fs::File>, record: &RequestRecord) -> Result<(), String> {
    let line = serde_json::to_string(record)
        .map_err(|error| format!("序列化模型请求记录失败：{error}"))?;
    writeln!(writer, "{line}").map_err(|error| format!("写入模型请求记录失败：{error}"))?;
    writer
        .flush()
        .map_err(|error| format!("刷新模型请求记录失败：{error}"))
}

impl RequestObserver for AnalyticsRecorder {
    fn on_request(&self, observation: RequestObservation) {
        // 无界队列只承载安全短元数据；不因统计高峰丢弃已完成请求。
        let _ = self.sender.send(AnalyticsEvent::Request(observation));
    }
}

#[derive(Default)]
struct ObservationWriterState {
    logical_started: HashMap<String, RequestObservation>,
    attempt_started_at: HashMap<(String, u32), u64>,
    completed_attempts: HashMap<String, u32>,
    last_attempt_records: HashMap<String, RequestRecord>,
}

impl ObservationWriterState {
    fn observe(&mut self, observation: RequestObservation) -> Vec<RequestRecord> {
        let logical_id = observation.logical_request_id.clone();
        if observation.scope == RequestObservationScope::Logical
            && observation.state == RequestObservationState::Started
        {
            self.logical_started.insert(logical_id.clone(), observation);
            self.completed_attempts.insert(logical_id, 0);
            return Vec::new();
        }

        if observation.scope == RequestObservationScope::Attempt
            && observation.state == RequestObservationState::Started
        {
            let started_at_ms = observation.at_ms;
            self.attempt_started_at
                .insert((logical_id, observation.attempt), started_at_ms);
            // 先写 running 行，终态再以相同 id 追加覆盖。这样应用崩溃、强制退出
            // 或请求长期挂起时，这次已经发出的物理请求仍可见。
            return vec![record_from_observation(observation, started_at_ms)];
        }

        if observation.scope == RequestObservationScope::Attempt {
            let started_at_ms = self
                .attempt_started_at
                .remove(&(logical_id.clone(), observation.attempt))
                .unwrap_or_else(|| {
                    observation
                        .duration_ms
                        .map(|duration| observation.at_ms.saturating_sub(duration))
                        .unwrap_or(observation.at_ms)
                });
            *self
                .completed_attempts
                .entry(logical_id.clone())
                .or_default() += 1;
            let record = record_from_observation(observation, started_at_ms);
            self.last_attempt_records.insert(logical_id, record.clone());
            return vec![record];
        }

        // logical 结束但没有进入任何物理 attempt：例如 provider request 构造失败
        // 或调用前就已取消。仍写一行 attempt=0，避免失败请求消失。
        let completed_attempts = self.completed_attempts.remove(&logical_id).unwrap_or(0);
        let logical_started = self.logical_started.remove(&logical_id);
        self.attempt_started_at
            .retain(|(request_id, _), _| request_id != &logical_id);
        if completed_attempts > 0 {
            let last_attempt = self.last_attempt_records.remove(&logical_id);
            // 只有 retry exhausted 代表 logical 层比最后一个物理 attempt
            // 更终态；普通 cancelled/failed 不覆盖已经完成的 attempt。
            if observation.state == RequestObservationState::Failed
                && observation.error_kind == Some(RequestErrorKind::RetryExhausted)
            {
                if let Some(mut corrected) = last_attempt {
                    let status = observation_status(&observation);
                    let error_kind = observation.error_kind.as_ref().map(error_kind_name);
                    let error = observation.error_summary;
                    if corrected.status != status
                        || corrected.error_kind != error_kind
                        || corrected.error != error
                    {
                        corrected.status = status;
                        corrected.error_kind = error_kind;
                        corrected.error = error;
                        return vec![corrected];
                    }
                }
            }
            return Vec::new();
        }
        self.last_attempt_records.remove(&logical_id);
        let started_at_ms = logical_started
            .as_ref()
            .map(|started| started.at_ms)
            .or_else(|| {
                observation
                    .duration_ms
                    .map(|duration| observation.at_ms.saturating_sub(duration))
            })
            .unwrap_or(observation.at_ms);
        vec![record_from_observation(observation, started_at_ms)]
    }
}

fn record_from_observation(observation: RequestObservation, requested_at_ms: u64) -> RequestRecord {
    let protocol = protocol_name(&observation.protocol);
    let provider = provider_name(&observation.endpoint, &protocol);
    let endpoint = safe_endpoint(&observation.endpoint);
    let status = observation_status(&observation);
    let error_kind = observation.error_kind.as_ref().map(error_kind_name);
    let usage = observation.usage.as_ref();
    let completed_at_ms =
        (observation.state != RequestObservationState::Started).then_some(observation.at_ms);
    RequestRecord {
        id: format!("{}:{}", observation.logical_request_id, observation.attempt),
        logical_request_id: observation.logical_request_id,
        attempt: observation.attempt,
        max_attempts: observation.max_attempts,
        session_id: observation.session_id,
        turn_id: observation.turn_id,
        agent_id: observation.agent_id,
        purpose: observation
            .purpose
            .filter(|purpose| !purpose.trim().is_empty())
            .unwrap_or_else(|| "model".to_owned()),
        model: observation.model,
        provider,
        protocol,
        endpoint,
        request_mode: match observation.mode {
            peri_model::ModelRequestMode::Stream => "stream",
            peri_model::ModelRequestMode::Sync => "sync",
        }
        .to_owned(),
        status,
        http_status: observation.http_status,
        error_kind,
        error: observation.error_summary,
        requested_at_ms,
        first_response_at_ms: observation.response_headers_at_ms,
        completed_at_ms,
        duration_ms: observation
            .duration_ms
            .unwrap_or_else(|| observation.at_ms.saturating_sub(requested_at_ms)),
        usage_reported: usage.is_some(),
        input_tokens: usage.map_or(0, |usage| u64::from(usage.input_tokens)),
        output_tokens: usage.map_or(0, |usage| u64::from(usage.output_tokens)),
        reasoning_tokens: usage
            .and_then(|usage| usage.reasoning_output_tokens)
            .map(u64::from),
        cache_creation_tokens: usage
            .and_then(|usage| usage.cache_creation_input_tokens)
            .map(u64::from),
        cache_read_tokens: usage
            .and_then(|usage| usage.cache_read_input_tokens)
            .map(u64::from),
        estimated: false,
        provider_request_id: observation.provider_request_id,
    }
}

fn protocol_name(protocol: &ProviderProtocol) -> String {
    match protocol {
        ProviderProtocol::OpenAiCompatible => "openai_compatible".to_owned(),
        ProviderProtocol::Anthropic => "anthropic".to_owned(),
        ProviderProtocol::Other { value } => value.clone(),
    }
}

fn provider_name(endpoint: &str, protocol: &str) -> String {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| protocol.to_owned())
}

/// Persist only the URL origin. The model runtime already applies the same
/// projection, but the analytics boundary must remain safe when it receives a
/// manually constructed observation or a future observer implementation.
fn safe_endpoint(endpoint: &str) -> Option<String> {
    let url = url::Url::parse(endpoint).ok()?;
    let host = url.host_str()?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(format!("{}://{host}{port}", url.scheme()))
}

fn observation_status(observation: &RequestObservation) -> String {
    match observation.state {
        RequestObservationState::Completed => "success".to_owned(),
        RequestObservationState::Cancelled => "cancelled".to_owned(),
        RequestObservationState::Failed => observation
            .error_kind
            .as_ref()
            .map(error_kind_name)
            .unwrap_or_else(|| "failed".to_owned()),
        RequestObservationState::Started => "running".to_owned(),
    }
}

fn error_kind_name(kind: &RequestErrorKind) -> String {
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
    .to_owned()
}

fn read_records(app: &AppHandle) -> Result<Vec<RequestRecord>, String> {
    let path = storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join(RECORD_FILE);
    read_records_from_path(&path)
}

fn read_records_from_path(path: &Path) -> Result<Vec<RequestRecord>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("打开模型请求记录失败：{error}")),
    };
    let mut records = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line =
            line.map_err(|error| format!("读取模型请求记录第 {} 行失败：{error}", index + 1))?;
        let record = serde_json::from_str::<RequestRecord>(&line)
            .map_err(|error| format!("解析模型请求记录第 {} 行失败：{error}", index + 1))?;
        records.push(record);
    }
    Ok(dedupe_records(records))
}

/// 同一 id 的后写行覆盖前写行，为未来补充终态字段保留原子追加能力。
fn dedupe_records(records: Vec<RequestRecord>) -> Vec<RequestRecord> {
    let mut unique = Vec::<RequestRecord>::new();
    let mut indexes = HashMap::<String, usize>::new();
    for record in records {
        if let Some(index) = indexes.get(&record.id).copied() {
            unique[index] = record;
        } else {
            indexes.insert(record.id.clone(), unique.len());
            unique.push(record);
        }
    }
    unique
}

fn filter_records(
    mut records: Vec<RequestRecord>,
    model: Option<&str>,
    status: Option<&str>,
    from_ms: Option<u64>,
    to_ms: Option<u64>,
) -> Vec<RequestRecord> {
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let status = status.map(str::trim).filter(|value| !value.is_empty());
    records.retain(|record| {
        model.is_none_or(|value| record.model == value)
            && status.is_none_or(|value| record.status == value)
            && from_ms.is_none_or(|value| record.requested_at_ms >= value)
            && to_ms.is_none_or(|value| record.requested_at_ms <= value)
    });
    records.sort_by(|left, right| {
        right
            .requested_at_ms
            .cmp(&left.requested_at_ms)
            .then_with(|| right.attempt.cmp(&left.attempt))
    });
    records
}

#[tauri::command]
pub async fn request_records_list(
    app: AppHandle,
    recorder: tauri::State<'_, std::sync::Arc<AnalyticsRecorder>>,
    offset: Option<usize>,
    limit: Option<usize>,
    model: Option<String>,
    status: Option<String>,
    from_ms: Option<u64>,
    to_ms: Option<u64>,
) -> Result<RequestRecordsPage, String> {
    if from_ms.zip(to_ms).is_some_and(|(from, to)| from > to) {
        return Err("起始时间不能晚于结束时间".to_owned());
    }
    let recorder = recorder.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recorder.flush()?;
        let all_records = read_records(&app)?;
        let models = all_records
            .iter()
            .map(|record| record.model.clone())
            .filter(|value| !value.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let statuses = all_records
            .iter()
            .map(|record| record.status.clone())
            .filter(|value| !value.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let records = filter_records(
            all_records,
            model.as_deref(),
            status.as_deref(),
            from_ms,
            to_ms,
        );
        let total = records.len();
        let offset = offset.unwrap_or(0).min(total);
        let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
        let page_records = records.into_iter().skip(offset).take(limit).collect();
        Ok(RequestRecordsPage {
            records: page_records,
            total,
            offset,
            limit,
            has_more: offset.saturating_add(limit) < total,
            models,
            statuses,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn task_cache_usage_get(
    app: AppHandle,
    recorder: tauri::State<'_, std::sync::Arc<AnalyticsRecorder>>,
    session_id: String,
) -> Result<TaskCacheUsage, String> {
    let session_id = session_id.trim().to_owned();
    if session_id.is_empty() {
        return Err("任务 ID 不能为空".to_owned());
    }
    let recorder = recorder.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recorder.flush()?;
        let records = read_records(&app)?;
        Ok(summarize_task_cache_usage(&records, &session_id))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn usage_stats_get(
    app: AppHandle,
    recorder: tauri::State<'_, std::sync::Arc<AnalyticsRecorder>>,
) -> Result<UsageStats, String> {
    let recorder = recorder.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recorder.flush()?;
        let records = read_records(&app)?;
        Ok(summarize_usage(&records))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 按任务汇总主 Agent 的成功模型请求。物理失败、取消、重试失败、标题、
/// 压缩、后台任务和子 Agent 请求均不属于原先单轮缓存率的统计口径。
fn summarize_task_cache_usage(records: &[RequestRecord], session_id: &str) -> TaskCacheUsage {
    let mut seen_record_ids = BTreeSet::new();
    let mut request_count = 0u64;
    let mut input_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_usage_complete = true;
    let mut totals_valid = true;
    let mut latest_context = None;

    for record in records {
        if record.session_id.as_deref() != Some(session_id)
            || record.purpose != "agent"
            || record.status != "success"
        {
            continue;
        }
        if !seen_record_ids.insert(record.id.as_str()) {
            continue;
        }
        request_count = request_count.saturating_add(1);
        // 成功请求但没有 usage 时不能把它当成“未命中”或静默剔除；
        // 任务整体的缓存率必须明确降级为未知。已知请求的 Token 仍保留，
        // 便于诊断，但 cache_hit_rate 不会据此给出部分结果。
        if !record.usage_reported {
            cache_usage_complete = false;
            continue;
        }
        let completed_at = record.completed_at_ms.unwrap_or(record.requested_at_ms);
        if latest_context
            .as_ref()
            .is_none_or(|(latest_at, _, _)| completed_at >= *latest_at)
        {
            latest_context = record
                .input_tokens
                .checked_add(record.output_tokens)
                .map(|tokens| (completed_at, tokens, record.estimated));
        }
        input_tokens = match input_tokens.checked_add(record.input_tokens) {
            Some(total) => total,
            None => {
                totals_valid = false;
                u64::MAX
            }
        };
        match record.cache_read_tokens {
            Some(tokens) => {
                if tokens > record.input_tokens {
                    totals_valid = false;
                }
                cache_read_tokens = match cache_read_tokens.checked_add(tokens) {
                    Some(total) => total,
                    None => {
                        totals_valid = false;
                        u64::MAX
                    }
                };
            }
            None => cache_usage_complete = false,
        }
    }

    let usage_is_complete = request_count > 0
        && cache_usage_complete
        && totals_valid
        && cache_read_tokens <= input_tokens;
    let reported_cache_read_tokens = usage_is_complete.then_some(cache_read_tokens);
    let cache_hit_rate = (usage_is_complete && input_tokens > 0)
        .then_some(cache_read_tokens as f64 / input_tokens as f64);

    TaskCacheUsage {
        session_id: session_id.to_owned(),
        request_count,
        input_tokens,
        cache_read_tokens: reported_cache_read_tokens,
        cache_hit_rate,
        latest_context_tokens: latest_context.map(|(_, tokens, _)| tokens),
        latest_context_estimated: latest_context.is_some_and(|(_, _, estimated)| estimated),
    }
}

/// 用量只聚合成功 attempt；重试失败行不会伪造请求数或 Token。
fn summarize_usage(records: &[RequestRecord]) -> UsageStats {
    let mut models = BTreeMap::<String, ModelUsageStat>::new();
    let mut days = BTreeMap::<String, DailyUsageStat>::new();
    let mut total_requests = 0u64;
    let mut total_tokens = 0u64;
    for record in records.iter().filter(|record| record.status == "success") {
        total_requests = total_requests.saturating_add(1);
        let tokens = record.input_tokens.saturating_add(record.output_tokens);
        total_tokens = total_tokens.saturating_add(tokens);
        let model = models
            .entry(record.model.clone())
            .or_insert_with(|| ModelUsageStat {
                model: record.model.clone(),
                requests: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            });
        model.requests = model.requests.saturating_add(1);
        model.input_tokens = model.input_tokens.saturating_add(record.input_tokens);
        model.output_tokens = model.output_tokens.saturating_add(record.output_tokens);
        model.total_tokens = model.total_tokens.saturating_add(tokens);

        let date = unix_ms_to_date(record.requested_at_ms);
        let day = days.entry(date.clone()).or_insert_with(|| DailyUsageStat {
            date,
            requests: 0,
            total_tokens: 0,
            model_tokens: BTreeMap::new(),
        });
        day.requests = day.requests.saturating_add(1);
        day.total_tokens = day.total_tokens.saturating_add(tokens);
        let day_model_tokens = day.model_tokens.entry(record.model.clone()).or_default();
        *day_model_tokens = day_model_tokens.saturating_add(tokens);
    }
    UsageStats {
        total_requests,
        total_tokens,
        models: models.into_values().collect(),
        days: days.into_values().collect(),
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn unix_ms_to_date(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|timestamp| {
            timestamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::open_record_file;
    use super::{
        ObservationWriterState, RequestRecord, dedupe_records, filter_records,
        read_records_from_path, record_from_observation, summarize_task_cache_usage,
        summarize_usage,
    };
    use peri_model::{
        ModelRequestMode, ProviderProtocol, RequestErrorKind, RequestObservation,
        RequestObservationScope, RequestObservationState, TokenUsage,
    };

    fn observation(
        scope: RequestObservationScope,
        state: RequestObservationState,
        attempt: u32,
    ) -> RequestObservation {
        RequestObservation {
            scope,
            state,
            logical_request_id: "logical-1".to_owned(),
            attempt,
            max_attempts: 6,
            model: "gpt-test".to_owned(),
            protocol: ProviderProtocol::OpenAiCompatible,
            mode: ModelRequestMode::Stream,
            endpoint: "https://api.example.test".to_owned(),
            at_ms: if state == RequestObservationState::Started {
                100
            } else {
                175
            },
            duration_ms: (state != RequestObservationState::Started).then_some(75),
            response_headers_at_ms: (state == RequestObservationState::Completed).then_some(125),
            http_status: (state == RequestObservationState::Completed).then_some(200),
            provider_request_id: Some("provider-1".to_owned()),
            usage: (state == RequestObservationState::Completed).then(|| TokenUsage {
                reasoning_output_tokens: Some(2),
                ..TokenUsage::new(10, 4)
            }),
            error_kind: (state == RequestObservationState::Failed)
                .then_some(RequestErrorKind::Timeout),
            error_summary: (state == RequestObservationState::Failed)
                .then_some("timeout".to_owned()),
            session_id: Some("session-1".to_owned()),
            turn_id: Some("turn-1".to_owned()),
            agent_id: Some("agent-1".to_owned()),
            purpose: Some("primary".to_owned()),
        }
    }

    fn record(id: &str, model: &str, status: &str, requested_at_ms: u64) -> RequestRecord {
        RequestRecord {
            id: id.to_owned(),
            logical_request_id: id.to_owned(),
            attempt: 1,
            max_attempts: 6,
            session_id: None,
            turn_id: None,
            agent_id: None,
            purpose: "primary".to_owned(),
            model: model.to_owned(),
            provider: "api.example.test".to_owned(),
            protocol: "openai_compatible".to_owned(),
            endpoint: Some("https://api.example.test".to_owned()),
            request_mode: "stream".to_owned(),
            status: status.to_owned(),
            http_status: None,
            error_kind: None,
            error: None,
            requested_at_ms,
            first_response_at_ms: None,
            completed_at_ms: Some(requested_at_ms + 10),
            duration_ms: 10,
            usage_reported: true,
            input_tokens: 10,
            output_tokens: 2,
            reasoning_tokens: None,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            estimated: false,
            provider_request_id: None,
        }
    }

    fn task_record(
        id: &str,
        session_id: &str,
        turn_id: &str,
        input_tokens: u64,
        cache_read_tokens: Option<u64>,
    ) -> RequestRecord {
        RequestRecord {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            agent_id: Some("main".to_owned()),
            purpose: "agent".to_owned(),
            input_tokens,
            cache_read_tokens,
            ..record(id, "alpha", "success", 100)
        }
    }

    #[test]
    fn persists_running_attempt_then_replaces_it_with_the_terminal_record() {
        let mut writer = ObservationWriterState::default();
        assert!(
            writer
                .observe(observation(
                    RequestObservationScope::Logical,
                    RequestObservationState::Started,
                    0
                ))
                .is_empty()
        );
        let running = writer.observe(observation(
            RequestObservationScope::Attempt,
            RequestObservationState::Started,
            1,
        ));
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, "logical-1:1");
        assert_eq!(running[0].status, "running");
        assert_eq!(running[0].completed_at_ms, None);
        let records = writer.observe(observation(
            RequestObservationScope::Attempt,
            RequestObservationState::Completed,
            1,
        ));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "logical-1:1");
        assert_eq!(records[0].status, "success");
        assert_eq!((records[0].input_tokens, records[0].output_tokens), (10, 4));
        assert_eq!(records[0].reasoning_tokens, Some(2));
        assert_eq!(records[0].first_response_at_ms, Some(125));
        let persisted = dedupe_records(vec![running[0].clone(), records[0].clone()]);
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].status, "success");
        assert!(
            writer
                .observe(observation(
                    RequestObservationScope::Logical,
                    RequestObservationState::Completed,
                    1
                ))
                .is_empty()
        );
    }

    #[test]
    fn retry_exhausted_corrects_the_last_attempt_without_adding_a_second_row() {
        let mut writer = ObservationWriterState::default();
        writer.observe(observation(
            RequestObservationScope::Logical,
            RequestObservationState::Started,
            0,
        ));
        writer.observe(observation(
            RequestObservationScope::Attempt,
            RequestObservationState::Started,
            1,
        ));
        let first = writer.observe(observation(
            RequestObservationScope::Attempt,
            RequestObservationState::Failed,
            1,
        ));
        assert_eq!(first[0].status, "timeout");

        let mut exhausted = observation(
            RequestObservationScope::Logical,
            RequestObservationState::Failed,
            1,
        );
        exhausted.error_kind = Some(RequestErrorKind::RetryExhausted);
        exhausted.error_summary = Some("retry exhausted".to_owned());
        let correction = writer.observe(exhausted);
        assert_eq!(correction.len(), 1);
        assert_eq!(correction[0].id, first[0].id);
        assert_eq!(correction[0].status, "retry_exhausted");
    }

    #[test]
    fn retry_correction_keeps_the_same_attempt_id_and_prior_failure() {
        let mut writer = ObservationWriterState::default();
        writer.observe(observation(
            RequestObservationScope::Logical,
            RequestObservationState::Started,
            0,
        ));

        let mut rows = Vec::new();
        for attempt in [1, 2] {
            writer.observe(observation(
                RequestObservationScope::Attempt,
                RequestObservationState::Started,
                attempt,
            ));
            let mut failed = observation(
                RequestObservationScope::Attempt,
                RequestObservationState::Failed,
                attempt,
            );
            failed.error_kind = Some(RequestErrorKind::Timeout);
            failed.error_summary = Some("timeout".to_owned());
            rows.extend(writer.observe(failed));
        }

        let mut exhausted = observation(
            RequestObservationScope::Logical,
            RequestObservationState::Failed,
            2,
        );
        exhausted.error_kind = Some(RequestErrorKind::RetryExhausted);
        exhausted.error_summary = Some("retry exhausted".to_owned());
        let correction = writer.observe(exhausted);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["logical-1:1", "logical-1:2"]
        );
        assert_eq!(rows[0].status, "timeout");
        assert_eq!(rows[1].status, "timeout");
        assert_eq!(correction.len(), 1);
        assert_eq!(correction[0].id, "logical-1:2");
        assert_eq!(correction[0].status, "retry_exhausted");

        let mut persisted = rows;
        persisted.extend(correction);
        let persisted = dedupe_records(persisted);
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0].status, "timeout");
        assert_eq!(persisted[1].status, "retry_exhausted");
    }

    #[test]
    fn logical_cancel_does_not_overwrite_a_completed_attempt() {
        let mut writer = ObservationWriterState::default();
        writer.observe(observation(
            RequestObservationScope::Logical,
            RequestObservationState::Started,
            0,
        ));
        writer.observe(observation(
            RequestObservationScope::Attempt,
            RequestObservationState::Started,
            1,
        ));
        let success = writer.observe(observation(
            RequestObservationScope::Attempt,
            RequestObservationState::Completed,
            1,
        ));
        let mut cancelled = observation(
            RequestObservationScope::Logical,
            RequestObservationState::Cancelled,
            1,
        );
        cancelled.error_kind = Some(RequestErrorKind::Cancelled);
        cancelled.error_summary = Some("request cancelled".to_owned());
        assert!(writer.observe(cancelled).is_empty());
        assert_eq!(success.len(), 1);
        assert_eq!(success[0].status, "success");
    }

    #[test]
    fn records_logical_failure_before_any_http_attempt() {
        let mut writer = ObservationWriterState::default();
        writer.observe(observation(
            RequestObservationScope::Logical,
            RequestObservationState::Started,
            0,
        ));
        let records = writer.observe(observation(
            RequestObservationScope::Logical,
            RequestObservationState::Failed,
            0,
        ));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].attempt, 0);
        assert_eq!(records[0].status, "timeout");
    }

    #[test]
    fn filters_and_orders_records_before_pagination() {
        let filtered = filter_records(
            vec![
                record("1", "alpha", "success", 100),
                record("2", "beta", "timeout", 300),
                record("3", "alpha", "success", 200),
            ],
            Some("alpha"),
            Some("success"),
            Some(150),
            None,
        );
        assert_eq!(
            filtered
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["3"]
        );
    }

    #[test]
    fn usage_ignores_failed_attempts_and_never_serializes_bodies() {
        let success = record("ok", "alpha", "success", 100);
        let failed = record("failed", "alpha", "timeout", 200);
        let stats = summarize_usage(&[success.clone(), failed]);
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.total_tokens, 12);
        let json = serde_json::to_string(&success).unwrap();
        assert!(!json.contains("requestBody"));
        assert!(!json.contains("responseBody"));
        assert!(!json.contains("apiKey"));
    }

    #[test]
    fn task_cache_usage_weights_all_turns_by_tokens_and_deduplicates_records() {
        let first = task_record("request-1", "session-1", "turn-1", 100, Some(100));
        let second = task_record("request-2", "session-1", "turn-2", 900, Some(0));
        // Same physical id in another task must not suppress the target task's row.
        let other_session = task_record("request-1", "session-2", "turn-1", 500, Some(500));
        let duplicate = second.clone();

        let stats =
            summarize_task_cache_usage(&[other_session, first, second, duplicate], "session-1");

        assert_eq!(stats.request_count, 2);
        assert_eq!(stats.input_tokens, 1_000);
        assert_eq!(stats.cache_read_tokens, Some(100));
        assert_eq!(stats.cache_hit_rate, Some(0.1));
        assert_eq!(stats.latest_context_tokens, Some(902));
    }

    #[test]
    fn task_cache_usage_keeps_unknown_distinct_from_explicit_zero() {
        let unknown = summarize_task_cache_usage(
            &[
                task_record("request-1", "session-1", "turn-1", 100, Some(40)),
                task_record("request-2", "session-1", "turn-2", 100, None),
            ],
            "session-1",
        );
        assert_eq!(unknown.input_tokens, 200);
        assert_eq!(unknown.cache_read_tokens, None);
        assert_eq!(unknown.cache_hit_rate, None);

        let zero = summarize_task_cache_usage(
            &[task_record(
                "request-3",
                "session-1",
                "turn-3",
                250,
                Some(0),
            )],
            "session-1",
        );
        assert_eq!(zero.cache_read_tokens, Some(0));
        assert_eq!(zero.cache_hit_rate, Some(0.0));
    }

    #[test]
    fn task_cache_usage_excludes_failed_cancelled_and_auxiliary_requests() {
        let mut failed = task_record("failed", "session-1", "turn-1", 900, Some(900));
        failed.status = "timeout".to_owned();
        let mut cancelled = task_record("cancelled", "session-1", "turn-1", 800, Some(800));
        cancelled.status = "cancelled".to_owned();
        let mut title = task_record("title", "session-1", "turn-1", 700, Some(700));
        title.purpose = "title".to_owned();

        let empty = summarize_task_cache_usage(&[failed, cancelled, title], "session-1");
        assert_eq!(empty.request_count, 0);
        assert_eq!(empty.input_tokens, 0);
        assert_eq!(empty.cache_read_tokens, None);
        assert_eq!(empty.cache_hit_rate, None);
    }

    #[test]
    fn task_cache_usage_rejects_impossible_provider_counts() {
        let stats = summarize_task_cache_usage(
            &[task_record(
                "request-1",
                "session-1",
                "turn-1",
                10,
                Some(11),
            )],
            "session-1",
        );
        assert_eq!(stats.cache_read_tokens, None);
        assert_eq!(stats.cache_hit_rate, None);
    }

    #[test]
    fn task_cache_usage_marks_success_without_usage_as_unknown_instead_of_skipping_it() {
        let mut missing_usage = task_record("request-1", "session-1", "turn-1", 900, None);
        missing_usage.usage_reported = false;
        missing_usage.input_tokens = 0;
        let known = task_record("request-2", "session-1", "turn-2", 100, Some(50));

        let stats = summarize_task_cache_usage(&[missing_usage, known], "session-1");

        assert_eq!(stats.request_count, 2);
        assert_eq!(stats.input_tokens, 100);
        assert_eq!(stats.cache_read_tokens, None);
        assert_eq!(stats.cache_hit_rate, None);
    }

    #[test]
    fn endpoint_projection_drops_credentials_path_query_and_fragment() {
        let mut unsafe_observation = observation(
            RequestObservationScope::Logical,
            RequestObservationState::Failed,
            0,
        );
        unsafe_observation.endpoint =
            "https://user:secret@example.test:8443/v1/messages?api_key=top-secret#fragment"
                .to_owned();
        let record = record_from_observation(unsafe_observation, 100);
        assert_eq!(
            record.endpoint.as_deref(),
            Some("https://example.test:8443")
        );
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("top-secret"));
        assert!(!json.contains("/v1/messages"));
        assert!(!json.contains("fragment"));
    }

    #[test]
    fn corrupt_record_line_is_reported_instead_of_becoming_an_empty_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("records.jsonl");
        std::fs::write(&path, b"{not-json}\n").unwrap();

        let error = read_records_from_path(&path).unwrap_err();
        assert!(error.contains("第 1 行"));
    }

    #[cfg(unix)]
    #[test]
    fn request_record_file_is_private_and_repairs_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("records.jsonl");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        drop(open_record_file(&path).unwrap());
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
