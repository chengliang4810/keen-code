//! 本地请求记录与模型用量统计。
//!
//! Agent 事件只通过有界 `sync_channel::try_send` 投递；序列化后的磁盘写入由
//! 独立线程完成。队列繁忙时宁可丢弃统计，也不阻塞 Agent/ACP 主链路。

use crate::storage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    sync::{
        Mutex,
        mpsc::{self, SyncSender},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::AppHandle;

const QUEUE_CAPACITY: usize = 256;
const RECORD_FILE: &str = "request-records.jsonl";

#[derive(Debug)]
enum AnalyticsEvent {
    Response {
        session_id: String,
        step: usize,
        model: String,
        response: String,
        input_tokens: u64,
        output_tokens: u64,
        estimated: bool,
        request_id: Option<String>,
        at_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    pub id: String,
    pub session_id: String,
    pub model: String,
    pub request_mode: String,
    pub requested_at_ms: u64,
    pub duration_ms: u64,
    pub request: Value,
    pub response: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
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

pub struct AnalyticsRecorder {
    sender: SyncSender<AnalyticsEvent>,
    /// usage_update 事件没有 step 字段，用 per-session 递增计数器保证
    /// RequestRecord.id 唯一（计数器留在本结构内，不放 Tauri 层）。
    step_counters: Mutex<HashMap<String, usize>>,
}

impl AnalyticsRecorder {
    pub fn new(app: &AppHandle) -> anyhow::Result<Self> {
        let path = storage::root_dir(app)?.join(RECORD_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("keencode-analytics".into())
            .spawn(move || {
                while let Ok(AnalyticsEvent::Response {
                    session_id,
                    step,
                    model,
                    response,
                    input_tokens,
                    output_tokens,
                    estimated,
                    request_id,
                    at_ms,
                }) = receiver.recv()
                {
                    let record = RequestRecord {
                        id: format!("{session_id}:{step}:{at_ms}"),
                        session_id,
                        model,
                        request_mode: "async".into(),
                        requested_at_ms: at_ms,
                        duration_ms: 0,
                        request: Value::Null,
                        response,
                        input_tokens,
                        output_tokens,
                        estimated,
                        provider_request_id: request_id,
                    };
                    if let Ok(line) = serde_json::to_string(&record) {
                        if let Ok(mut file) =
                            OpenOptions::new().create(true).append(true).open(&path)
                        {
                            let _ = writeln!(file, "{line}");
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            step_counters: Mutex::new(HashMap::new()),
        })
    }

    /// 从 ACP `session/update` 通知中的 usage_update 提取用量并记录。
    ///
    /// `_meta` 的 inputTokens / outputTokens 必须为数字才记录；
    /// model / requestId / estimated / cacheCreationTokens / cacheReadTokens
    /// 均为可选，缺失时不拦截本次记录。
    pub fn observe_usage_update(&self, session_id: &str, update: &Value) {
        let Some(meta) = update.get("_meta").and_then(Value::as_object) else {
            return;
        };
        let Some(input_tokens) = meta.get("inputTokens").and_then(Value::as_u64) else {
            return;
        };
        let Some(output_tokens) = meta.get("outputTokens").and_then(Value::as_u64) else {
            return;
        };
        let mut counters = self
            .step_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let step = {
            let counter = counters.entry(session_id.to_string()).or_insert(0);
            *counter += 1;
            *counter
        };
        let message = AnalyticsEvent::Response {
            session_id: session_id.to_string(),
            step,
            model: meta
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            response: String::new(),
            input_tokens,
            output_tokens,
            estimated: meta
                .get("estimated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            request_id: meta
                .get("requestId")
                .and_then(Value::as_str)
                .map(str::to_string),
            at_ms: now_ms(),
        };
        let _ = self.sender.try_send(message);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn read_records(app: &AppHandle) -> Result<Vec<RequestRecord>, String> {
    let path = storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join(RECORD_FILE);
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(Vec::new());
    };
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect())
}

#[tauri::command]
pub async fn request_records_list(
    app: AppHandle,
    limit: Option<usize>,
) -> Result<Vec<RequestRecord>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut records = read_records(&app)?;
        records.reverse();
        records.truncate(limit.unwrap_or(200).min(1000));
        Ok(records)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn usage_stats_get(app: AppHandle) -> Result<UsageStats, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let records = read_records(&app)?;
        let mut models = BTreeMap::<String, ModelUsageStat>::new();
        let mut days = BTreeMap::<String, DailyUsageStat>::new();
        let mut total_tokens = 0u64;
        for record in &records {
            let tokens = record.input_tokens.saturating_add(record.output_tokens);
            total_tokens = total_tokens.saturating_add(tokens);
            let model = models
                .entry(record.model.clone())
                .or_insert(ModelUsageStat {
                    model: record.model.clone(),
                    requests: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                });
            model.requests += 1;
            model.input_tokens = model.input_tokens.saturating_add(record.input_tokens);
            model.output_tokens = model.output_tokens.saturating_add(record.output_tokens);
            model.total_tokens = model.total_tokens.saturating_add(tokens);
            let date = unix_ms_to_date(record.requested_at_ms);
            let day = days.entry(date.clone()).or_insert(DailyUsageStat {
                date,
                requests: 0,
                total_tokens: 0,
                model_tokens: BTreeMap::new(),
            });
            day.requests += 1;
            day.total_tokens = day.total_tokens.saturating_add(tokens);
            let day_model_tokens = day.model_tokens.entry(record.model.clone()).or_default();
            *day_model_tokens = day_model_tokens.saturating_add(tokens);
        }
        Ok(UsageStats {
            total_requests: records.len() as u64,
            total_tokens,
            models: models.into_values().collect(),
            days: days.into_values().collect(),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

fn unix_ms_to_date(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|timestamp| {
            timestamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::unix_ms_to_date;

    #[test]
    fn groups_usage_into_a_stable_local_date() {
        assert_ne!(unix_ms_to_date(0), "unknown");
    }
}
