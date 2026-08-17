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
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, State};

const QUEUE_CAPACITY: usize = 256;
const RECORD_FILE: &str = "request-records.jsonl";
/// DOM commit 正常应在同一帧内回报；保留短期完成态以覆盖 done 先到的跨通道乱序。
const RECENT_COMPLETED_TTL_MS: u64 = 5 * 60 * 1_000;

#[derive(Debug)]
enum AnalyticsEvent {
    Response {
        session_id: String,
        turn_id: String,
        step: usize,
        model: String,
        request_mode: &'static str,
        response: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        estimated: bool,
        request_id: Option<String>,
        request_started_at_ms: u64,
        accepted_at_ms: u64,
        first_provider_event_at_ms: Option<u64>,
        first_visible_token_at_ms: Option<u64>,
        completed_at_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub model: String,
    pub request_mode: String,
    pub requested_at_ms: u64,
    pub duration_ms: u64,
    pub accepted_at_ms: u64,
    pub first_provider_event_at_ms: Option<u64>,
    pub first_visible_token_at_ms: Option<u64>,
    pub completed_at_ms: u64,
    pub request: Value,
    pub response: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_hit_rate: Option<f64>,
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
    /// 每个 Session 同时只有一个由 Host 接受的前台 turn；完成前暂存其时间点与 usage。
    pending_turns: Mutex<HashMap<String, PendingTurn>>,
    /// 已完成但仍允许首个 DOM commit 补写的短期 turn；按 (session, client turn) 关联。
    recent_completed_turns: Mutex<HashMap<(String, String), CompletedTurn>>,
}

#[derive(Debug, Clone)]
struct PendingTurn {
    turn_id: String,
    request_started_at_ms: u64,
    accepted_at_ms: u64,
    first_provider_event_at_ms: Option<u64>,
    first_visible_token_at_ms: Option<u64>,
    usages: BTreeMap<usize, PendingUsage>,
}

#[derive(Debug, Clone)]
struct PendingUsage {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    estimated: bool,
    request_id: Option<String>,
}

#[derive(Debug, Clone)]
struct CompletedTurn {
    turn: PendingTurn,
    completed_at_ms: u64,
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
                    turn_id,
                    step,
                    model,
                    request_mode,
                    response,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    estimated,
                    request_id,
                    request_started_at_ms,
                    accepted_at_ms,
                    first_provider_event_at_ms,
                    first_visible_token_at_ms,
                    completed_at_ms,
                }) = receiver.recv()
                {
                    let record = RequestRecord {
                        id: format!("{session_id}:{turn_id}:{step}:{accepted_at_ms}"),
                        session_id,
                        turn_id,
                        model,
                        request_mode: request_mode.into(),
                        requested_at_ms: request_started_at_ms,
                        duration_ms: completed_at_ms.saturating_sub(request_started_at_ms),
                        accepted_at_ms,
                        first_provider_event_at_ms,
                        first_visible_token_at_ms,
                        completed_at_ms,
                        request: Value::Null,
                        response,
                        input_tokens,
                        output_tokens,
                        cache_creation_tokens,
                        cache_read_tokens,
                        cache_hit_rate: cache_hit_rate(cache_read_tokens, input_tokens),
                        estimated,
                        provider_request_id: request_id,
                    };
                    if let Ok(line) = serde_json::to_string(&record)
                        && let Ok(mut file) =
                            OpenOptions::new().create(true).append(true).open(&path)
                    {
                        let _ = writeln!(file, "{line}");
                    }
                }
            })?;
        Ok(Self {
            sender,
            pending_turns: Mutex::new(HashMap::new()),
            recent_completed_turns: Mutex::new(HashMap::new()),
        })
    }

    /// Host 已完成同步校验并原子切换 Streaming；从这个时间点开始计算本轮延迟。
    pub fn begin_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        request_started_at_ms: u64,
        accepted_at_ms: u64,
    ) {
        self.pending_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                session_id.to_owned(),
                PendingTurn {
                    turn_id: turn_id.to_owned(),
                    request_started_at_ms: request_started_at_ms.min(accepted_at_ms),
                    accepted_at_ms,
                    first_provider_event_at_ms: None,
                    first_visible_token_at_ms: None,
                    usages: BTreeMap::new(),
                },
            );
    }

    /// 记录 peri-model 解析出的首个完整 Provider 流事件；同一 turn 只保留最早值。
    pub fn observe_first_provider_event(&self, session_id: &str, turn_id: &str, at_ms: u64) {
        let mut turns = self
            .pending_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(turn) = turns.get_mut(session_id) else {
            return;
        };
        if turn.turn_id != turn_id {
            return;
        }
        turn.first_provider_event_at_ms = Some(
            turn.first_provider_event_at_ms
                .map_or(at_ms, |current| current.min(at_ms)),
        );
    }

    /// 记录前端 Markdown DOM commit 后回报的首个可见 Token 时间。
    ///
    /// 事件可能先于 done，也可能在 done 后到达；后者会用相同 record id 追加修正版，
    /// 读取时取最后一版。ACP delta 抵达 Host 不得调用此方法。
    pub fn observe_first_visible_token(&self, session_id: &str, turn_id: &str, at_ms: u64) -> bool {
        let mut pending = self
            .pending_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(turn) = pending.get_mut(session_id)
            && turn.turn_id == turn_id
        {
            if turn.first_visible_token_at_ms.is_none() {
                turn.first_visible_token_at_ms = Some(at_ms);
                return true;
            }
            return false;
        }

        let mut recent = self
            .recent_completed_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_recent_completed(&mut recent, now_ms());
        let key = (session_id.to_owned(), turn_id.to_owned());
        let Some(completed) = recent.get_mut(&key) else {
            return false;
        };
        if completed.turn.first_visible_token_at_ms.is_some() {
            return false;
        }
        completed.turn.first_visible_token_at_ms = Some(at_ms);
        let patch = completed.clone();
        drop(recent);
        drop(pending);
        self.enqueue_turn_records(session_id, &patch.turn, patch.completed_at_ms);
        true
    }

    /// 从 ACP `session/update` 通知中的 usage_update 提取用量并暂存。
    ///
    /// Peri 3.6.5 为每次真实 LLM 调用发送 usage_update；Host 直接使用协议里的
    /// llmStep 去重，避免重复投递被累计成两次请求。inputTokens / outputTokens
    /// 必须为数字，model、Provider requestId、estimated 与 cache 字段保持可选。
    pub fn observe_usage_update(&self, session_id: &str, turn_id: &str, update: &Value) {
        let Some(meta) = update.get("_meta").and_then(Value::as_object) else {
            return;
        };
        let Some(input_tokens) = meta.get("inputTokens").and_then(Value::as_u64) else {
            return;
        };
        let Some(output_tokens) = meta.get("outputTokens").and_then(Value::as_u64) else {
            return;
        };
        let Some(step) = meta
            .get("llmStep")
            .and_then(Value::as_u64)
            .and_then(|step| usize::try_from(step).ok())
        else {
            return;
        };
        let mut turns = self
            .pending_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(turn) = turns.get_mut(session_id) else {
            return;
        };
        if turn.turn_id != turn_id {
            return;
        }
        let usage = PendingUsage {
            model: meta
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            input_tokens,
            output_tokens,
            cache_creation_tokens: meta.get("cacheCreationTokens").and_then(Value::as_u64),
            cache_read_tokens: meta.get("cacheReadTokens").and_then(Value::as_u64),
            estimated: meta
                .get("estimated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            request_id: meta
                .get("requestId")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        turn.usages.insert(step, usage);
    }

    /// 完成对应 turn，并为本轮每次真实 LLM usage 写入同一组 Host 时间边界。
    pub fn complete_turn(&self, session_id: &str, turn_id: &str, completed_at_ms: u64) {
        let mut pending = self
            .pending_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending
            .get(session_id)
            .is_none_or(|turn| turn.turn_id != turn_id)
        {
            return;
        }
        let Some(turn) = pending.remove(session_id) else {
            return;
        };
        let mut recent = self
            .recent_completed_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_recent_completed(&mut recent, completed_at_ms);
        recent.insert(
            (session_id.to_owned(), turn_id.to_owned()),
            CompletedTurn {
                turn: turn.clone(),
                completed_at_ms,
            },
        );
        // recent 在初始记录入队前不可见；否则 DOM patch 可能先入队，随后被旧版
        // 初始记录覆盖为“最后一版”。try_send 有界且不阻塞，可安全保持短锁。
        self.enqueue_turn_records(session_id, &turn, completed_at_ms);
        drop(recent);
        drop(pending);
    }

    /// 为一个完成 turn 的每次真实 LLM usage 写入同一组时间边界。
    fn enqueue_turn_records(&self, session_id: &str, turn: &PendingTurn, completed_at_ms: u64) {
        if turn.usages.is_empty() {
            let _ = self.sender.try_send(AnalyticsEvent::Response {
                session_id: session_id.to_owned(),
                turn_id: turn.turn_id.clone(),
                step: 0,
                model: "unknown".to_owned(),
                request_mode: "turn",
                response: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                estimated: false,
                request_id: None,
                request_started_at_ms: turn.request_started_at_ms,
                accepted_at_ms: turn.accepted_at_ms,
                first_provider_event_at_ms: turn.first_provider_event_at_ms,
                first_visible_token_at_ms: turn.first_visible_token_at_ms,
                completed_at_ms,
            });
            return;
        }
        for (step, usage) in &turn.usages {
            let _ = self.sender.try_send(AnalyticsEvent::Response {
                session_id: session_id.to_owned(),
                turn_id: turn.turn_id.clone(),
                step: *step,
                model: usage.model.clone(),
                request_mode: "async",
                response: String::new(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                estimated: usage.estimated,
                request_id: usage.request_id.clone(),
                request_started_at_ms: turn.request_started_at_ms,
                accepted_at_ms: turn.accepted_at_ms,
                first_provider_event_at_ms: turn.first_provider_event_at_ms,
                first_visible_token_at_ms: turn.first_visible_token_at_ms,
                completed_at_ms,
            });
        }
    }
}

/// 判断 ACP SessionUpdate 是否为可记录的用量事件。
///
/// ACP 的判别字段是 `sessionUpdate`；`type` 属于旧的错误假设，不能用于筛选。
pub(crate) fn is_usage_update(update: &Value) -> bool {
    update.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
        && update.get("_meta").is_some()
}

fn prune_recent_completed(turns: &mut HashMap<(String, String), CompletedTurn>, reference_ms: u64) {
    turns.retain(|_, turn| {
        reference_ms.saturating_sub(turn.completed_at_ms) <= RECENT_COMPLETED_TTL_MS
    });
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn cache_hit_rate(cache_read_tokens: Option<u64>, input_tokens: u64) -> Option<f64> {
    cache_read_tokens
        .filter(|tokens| input_tokens > 0 && *tokens <= input_tokens)
        .map(|tokens| tokens as f64 / input_tokens as f64)
}

fn read_records(app: &AppHandle) -> Result<Vec<RequestRecord>, String> {
    let path = storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join(RECORD_FILE);
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(Vec::new());
    };
    Ok(dedupe_records(
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect(),
    ))
}

/// done 后的 DOM commit 以相同 record id 追加修正版；读取时保留最后一版。
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

/// 前端在 Markdown 首个 Token 完成 DOM commit 后回报唯一可见时间点。
#[tauri::command]
pub async fn turn_first_visible_observe(
    session_id: String,
    request_id: String,
    at_ms: u64,
    recorder: State<'_, Arc<AnalyticsRecorder>>,
) -> Result<bool, String> {
    if session_id.trim().is_empty() {
        return Err("sessionId 不能为空".to_owned());
    }
    if request_id.trim().is_empty() {
        return Err("requestId 不能为空".to_owned());
    }
    if at_ms == 0 {
        return Err("atMs 必须为有效 Epoch 毫秒".to_owned());
    }
    Ok(recorder.observe_first_visible_token(&session_id, &request_id, at_ms))
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
        Ok(summarize_usage(&records))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 请求统计只聚合真实 LLM usage 行；turn-only 延迟记录不伪造请求或 token。
fn summarize_usage(records: &[RequestRecord]) -> UsageStats {
    let mut models = BTreeMap::<String, ModelUsageStat>::new();
    let mut days = BTreeMap::<String, DailyUsageStat>::new();
    let mut total_requests = 0u64;
    let mut total_tokens = 0u64;
    for record in records {
        if record.request_mode == "turn" {
            continue;
        }
        total_requests = total_requests.saturating_add(1);
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
    UsageStats {
        total_requests,
        total_tokens,
        models: models.into_values().collect(),
        days: days.into_values().collect(),
    }
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
    use super::{
        AnalyticsEvent, AnalyticsRecorder, RequestRecord, cache_hit_rate, dedupe_records,
        is_usage_update, summarize_usage, unix_ms_to_date,
    };
    use serde_json::json;
    use std::{collections::HashMap, sync::Mutex, sync::mpsc};

    #[test]
    fn groups_usage_into_a_stable_local_date() {
        assert_ne!(unix_ms_to_date(0), "unknown");
    }

    /// 用量事件必须使用 ACP 的 sessionUpdate 判别字段并携带元数据。
    #[test]
    fn recognizes_usage_update_by_acp_discriminator() {
        assert!(is_usage_update(&json!({
            "sessionUpdate": "usage_update",
            "_meta": {"inputTokens": 1, "outputTokens": 2}
        })));
        assert!(!is_usage_update(&json!({
            "type": "usage_update",
            "_meta": {"inputTokens": 1, "outputTokens": 2}
        })));
        assert!(!is_usage_update(&json!({
            "sessionUpdate": "usage_update"
        })));
    }

    /// 一个 turn 的 Host/Provider/可见/完成边界必须与每次 usage 一起落盘。
    #[test]
    fn records_correlated_turn_latency_and_cache_usage() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let recorder = AnalyticsRecorder {
            sender,
            pending_turns: Mutex::new(HashMap::new()),
            recent_completed_turns: Mutex::new(HashMap::new()),
        };
        recorder.begin_turn("session-a", "turn-7", 900, 1_000);
        recorder.observe_first_provider_event("session-a", "turn-7", 1_140);
        recorder.observe_first_provider_event("session-a", "turn-7", 1_120);
        assert!(recorder.observe_first_visible_token("session-a", "turn-7", 1_250));
        recorder.observe_usage_update(
            "session-a",
            "turn-7",
            &json!({
                "_meta": {
                    "model": "deepseek-chat",
                    "llmStep": 7,
                    "inputTokens": 100,
                    "outputTokens": 25,
                    "cacheCreationTokens": 5,
                    "cacheReadTokens": 80,
                    "requestId": "req-1"
                }
            }),
        );
        recorder.complete_turn("session-a", "turn-7", 1_500);

        let AnalyticsEvent::Response {
            turn_id,
            step,
            model,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            request_started_at_ms,
            accepted_at_ms,
            first_provider_event_at_ms,
            first_visible_token_at_ms,
            completed_at_ms,
            ..
        } = receiver.try_recv().unwrap();
        assert_eq!(turn_id, "turn-7");
        assert_eq!(step, 7);
        assert_eq!(model, "deepseek-chat");
        assert_eq!((input_tokens, output_tokens), (100, 25));
        assert_eq!(
            (cache_creation_tokens, cache_read_tokens),
            (Some(5), Some(80))
        );
        assert_eq!(request_started_at_ms, 900);
        assert_eq!(accepted_at_ms, 1_000);
        assert_eq!(first_provider_event_at_ms, Some(1_120));
        assert_eq!(first_visible_token_at_ms, Some(1_250));
        assert_eq!(completed_at_ms, 1_500);
        assert!(receiver.try_recv().is_err());
    }

    /// 过期完成边界不能冲掉当前 turn，缓存命中率使用 cacheRead/input。
    #[test]
    fn rejects_stale_completion_and_calculates_cache_hit_rate() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let recorder = AnalyticsRecorder {
            sender,
            pending_turns: Mutex::new(HashMap::new()),
            recent_completed_turns: Mutex::new(HashMap::new()),
        };
        recorder.begin_turn("session-a", "turn-2", 80, 100);
        recorder.observe_usage_update(
            "session-a",
            "stale-turn",
            &json!({
                "_meta": {"llmStep": 1, "inputTokens": 10, "outputTokens": 2}
            }),
        );
        assert!(
            recorder.pending_turns.lock().unwrap()["session-a"]
                .usages
                .is_empty()
        );
        recorder.complete_turn("session-a", "turn-1", 200);
        assert!(receiver.try_recv().is_err());
        assert!(
            recorder
                .pending_turns
                .lock()
                .unwrap()
                .contains_key("session-a")
        );
        assert_eq!(cache_hit_rate(Some(8), 10), Some(0.8));
        assert_eq!(cache_hit_rate(Some(0), 10), Some(0.0));
        assert_eq!(cache_hit_rate(None, 10), None);
        assert_eq!(cache_hit_rate(Some(8), 0), None);
        assert_eq!(cache_hit_rate(Some(11), 10), None);
    }

    /// 无 usage 的早期失败/取消仍保留 turn 延迟，但不得增加真实 LLM 请求数。
    #[test]
    fn records_turn_only_completion_without_faking_cache_or_tokens() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let recorder = AnalyticsRecorder {
            sender,
            pending_turns: Mutex::new(HashMap::new()),
            recent_completed_turns: Mutex::new(HashMap::new()),
        };
        recorder.begin_turn("session-a", "turn-3", 90, 100);
        recorder.complete_turn("session-a", "turn-3", 250);

        let AnalyticsEvent::Response {
            request_mode,
            model,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            accepted_at_ms,
            completed_at_ms,
            ..
        } = receiver.try_recv().unwrap();
        assert_eq!(request_mode, "turn");
        assert_eq!(model, "unknown");
        assert_eq!((input_tokens, output_tokens), (0, 0));
        assert_eq!((cache_creation_tokens, cache_read_tokens), (None, None));
        assert_eq!((accepted_at_ms, completed_at_ms), (100, 250));

        let make_record = |request_mode: &str, input_tokens, output_tokens| RequestRecord {
            id: format!("{request_mode}-record"),
            session_id: "session-a".to_owned(),
            turn_id: "turn-3".to_owned(),
            model: if request_mode == "turn" {
                "unknown".to_owned()
            } else {
                "deepseek-chat".to_owned()
            },
            request_mode: request_mode.to_owned(),
            requested_at_ms: 100,
            duration_ms: 150,
            accepted_at_ms: 100,
            first_provider_event_at_ms: None,
            first_visible_token_at_ms: None,
            completed_at_ms: 250,
            request: serde_json::Value::Null,
            response: String::new(),
            input_tokens,
            output_tokens,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            cache_hit_rate: None,
            estimated: false,
            provider_request_id: None,
        };
        let stats = summarize_usage(&[make_record("turn", 0, 0), make_record("async", 10, 2)]);
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.total_tokens, 12);
        assert_eq!(stats.models.len(), 1);
    }

    /// done 先于 DOM commit 时追加同 ID 修正版，读取去重后必须保留可见时间。
    #[test]
    fn late_dom_commit_patches_completed_turn_record() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let recorder = AnalyticsRecorder {
            sender,
            pending_turns: Mutex::new(HashMap::new()),
            recent_completed_turns: Mutex::new(HashMap::new()),
        };
        let started_at_ms = super::now_ms();
        let accepted_at_ms = started_at_ms + 10;
        let completed_at_ms = started_at_ms + 80;
        let visible_at_ms = started_at_ms + 90;
        recorder.begin_turn("session-a", "turn-late", started_at_ms, accepted_at_ms);
        recorder.complete_turn("session-a", "turn-late", completed_at_ms);
        assert!(recorder.observe_first_visible_token("session-a", "turn-late", visible_at_ms,));

        let first = receiver.try_recv().unwrap();
        let patched = receiver.try_recv().unwrap();
        let (
            AnalyticsEvent::Response {
                first_visible_token_at_ms: first_visible,
                ..
            },
            AnalyticsEvent::Response {
                first_visible_token_at_ms: patched_visible,
                ..
            },
        ) = (first, patched);
        assert_eq!(first_visible, None);
        assert_eq!(patched_visible, Some(visible_at_ms));

        let base = RequestRecord {
            id: "same".to_owned(),
            session_id: "session-a".to_owned(),
            turn_id: "turn-late".to_owned(),
            model: "unknown".to_owned(),
            request_mode: "turn".to_owned(),
            requested_at_ms: 100,
            duration_ms: 80,
            accepted_at_ms: 110,
            first_provider_event_at_ms: None,
            first_visible_token_at_ms: None,
            completed_at_ms: 180,
            request: serde_json::Value::Null,
            response: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            cache_hit_rate: None,
            estimated: false,
            provider_request_id: None,
        };
        let mut patch = base.clone();
        patch.first_visible_token_at_ms = Some(190);
        let records = dedupe_records(vec![base, patch]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].first_visible_token_at_ms, Some(190));
    }
}
