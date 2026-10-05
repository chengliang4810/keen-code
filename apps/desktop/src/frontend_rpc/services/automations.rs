//! 本地定时任务服务。
//!
//! ZCode 的 automation 是桌面本地能力，不能退化成 Node scheduler 或云端 bot。
//! 这里保存受限的定义与运行台账，并把每次派发交给唯一的 `AgentRuntime`；任务
//! runId 同时编码到 Runtime turnId，因而 Journal 中的 turn 与本地 run 可以恢复对账。

use chrono::{Datelike, Duration as ChronoDuration, Local, TimeZone, Timelike};
use keencode_acp::ConnectionId;
use keencode_resources::TurnStatus;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{
    Arc, Condvar, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use crate::agent_runtime::{AgentRuntime, RootTurnOptions};

const STORE_FILE: &str = "automations.json";
const STORE_SCHEMA: u32 = 1;
const MAX_AUTOMATIONS: usize = 20;
const MAX_RUNS: usize = 2_000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCHEDULE_SEARCH_MINUTES: i64 = 366 * 24 * 60 * 2;

static STORE_GATE: OnceLock<Mutex<()>> = OnceLock::new();
static SCHEDULER: OnceLock<Arc<Scheduler>> = OnceLock::new();
static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
// 手动运行的连接只在本进程内跟随待派发 run；绝不能写入 automation 台账，
// 否则窗口重载后会把已经失效的 WebView connection 当成审批 owner。
static MANUAL_RUN_CONNECTIONS: OnceLock<Mutex<HashMap<String, ConnectionId>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleRule {
    unit: String,
    interval: u32,
    hour: u32,
    minute: u32,
    anchor_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    weekdays: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    month_days: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    months: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monthly_mode: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutomationRecord {
    automation_id: String,
    title: String,
    cron_expr: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_selection: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    workspace_key: String,
    workspace_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_task_id: Option<String>,
    location_kind: String,
    recurring: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_runs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schedule_rule: Option<ScheduleRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schedule_edited_by_user: Option<bool>,
    run_count: u32,
    scheduled_run_count: u32,
    enabled: bool,
    lifecycle_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run_at: Option<i64>,
    dispatch_status: String,
    dispatch_attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunRecord {
    run_id: String,
    automation_id: String,
    workspace_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduled_at: Option<i64>,
    trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_selection: Option<Value>,
    dispatch_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    attempts: u32,
    created_at: i64,
    updated_at: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    schema: u32,
    automations: Vec<AutomationRecord>,
    runs: Vec<RunRecord>,
}

struct Scheduler {
    app: AppHandle,
    wake: Arc<(Mutex<bool>, Condvar)>,
}

#[derive(Clone)]
struct WorkspaceScope {
    key: String,
    path: PathBuf,
    identity: Option<String>,
}

pub(crate) fn is_automation_method(method: &str) -> bool {
    matches!(
        method,
        "listAutomations"
            | "listAllAutomations"
            | "createAutomation"
            | "updateAutomation"
            | "deleteAutomation"
            | "setAutomationEnabled"
            | "restartAutomation"
            | "runAutomationNow"
            | "listAutomationRuns"
            | "deleteAutomationRun"
    )
}

/// 根装配可在 setup 完成后调用；空任务时线程永久等待，存在任务时只睡到最近触发点。
pub(crate) fn start_scheduler(app: &AppHandle) {
    ensure_scheduler(app.clone());
}

pub(crate) async fn call_automation(
    app: &AppHandle,
    method: &str,
    args: Value,
    connection_id: Option<&str>,
) -> Result<Value, String> {
    if !is_automation_method(method) {
        return Err(format!("未知服务方法：zcode-agent.{method}"));
    }
    ensure_scheduler(app.clone());
    match method {
        "listAutomations" => list_automations(app, &args, false),
        "listAllAutomations" => list_automations(app, &args, true),
        "createAutomation" => create_automation(app, &args),
        "updateAutomation" => update_automation(app, &args),
        "deleteAutomation" => delete_automation(app, &args),
        "setAutomationEnabled" => set_enabled(app, &args),
        "restartAutomation" => restart_automation(app, &args),
        "runAutomationNow" => run_now(app, &args, connection_id),
        "listAutomationRuns" => list_runs(app, &args),
        "deleteAutomationRun" => delete_run(app, &args),
        _ => unreachable!(),
    }
}

pub(crate) fn connection_closed(connection_id: &str) {
    let Ok(connection_id) = ConnectionId::new(connection_id.to_owned()) else {
        return;
    };
    let Some(connections) = MANUAL_RUN_CONNECTIONS.get() else {
        return;
    };
    if let Ok(mut connections) = connections.lock() {
        connections.retain(|_, bound| bound != &connection_id);
    }
}

fn manual_run_connections() -> &'static Mutex<HashMap<String, ConnectionId>> {
    MANUAL_RUN_CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn take_manual_run_connection(run_id: &str) -> Option<ConnectionId> {
    manual_run_connections()
        .lock()
        .ok()
        .and_then(|mut connections| connections.remove(run_id))
}

fn ensure_scheduler(app: AppHandle) -> Arc<Scheduler> {
    SCHEDULER
        .get_or_init(|| {
            let scheduler = Arc::new(Scheduler {
                app,
                wake: Arc::new((Mutex::new(false), Condvar::new())),
            });
            let worker = Arc::clone(&scheduler);
            let _ = thread::Builder::new()
                .name("keencode-automation-scheduler".to_owned())
                .spawn(move || scheduler_loop(worker));
            scheduler
        })
        .clone()
}

fn scheduler_loop(scheduler: Arc<Scheduler>) {
    loop {
        let dispatched = dispatch_due(&scheduler.app);
        if !dispatched.is_empty() {
            for run_id in dispatched {
                let app = scheduler.app.clone();
                tauri::async_runtime::spawn(async move {
                    dispatch_run(app, run_id).await;
                });
            }
            continue;
        }

        let wait_for = next_wait(&scheduler.app);
        let (flag, condition) = &*scheduler.wake;
        let mut woke = flag.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if *woke {
            *woke = false;
            continue;
        }
        match wait_for {
            Some(delay) => {
                let (next, _) = condition
                    .wait_timeout(woke, delay)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                woke = next;
                *woke = false;
            }
            None => {
                woke = condition
                    .wait(woke)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *woke = false;
            }
        }
    }
}

fn wake_scheduler() {
    if let Some(scheduler) = SCHEDULER.get() {
        let (flag, condition) = &*scheduler.wake;
        if let Ok(mut woke) = flag.lock() {
            *woke = true;
            condition.notify_one();
        }
    }
}

fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|root| root.join(STORE_FILE))
        .map_err(|error| format!("无法确定 automation 数据根：{error}"))
}

fn store_lock() -> &'static Mutex<()> {
    STORE_GATE.get_or_init(|| Mutex::new(()))
}

fn read_store(app: &AppHandle) -> Result<StoreFile, String> {
    let path = store_path(app)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StoreFile {
                schema: STORE_SCHEMA,
                ..StoreFile::default()
            });
        }
        Err(error) => return Err(format!("读取 automation 台账失败：{error}")),
    };
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("automation 台账超过 8 MB 限制".to_owned());
    }
    let store: StoreFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("automation 台账格式无效：{error}"))?;
    if store.schema != STORE_SCHEMA {
        return Err(format!("不支持的 automation 台账版本：{}", store.schema));
    }
    Ok(store)
}

fn write_store(app: &AppHandle, store: &StoreFile) -> Result<(), String> {
    let path = store_path(app)?;
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("automation 台账编码失败：{error}"))?;
    crate::storage::atomic_write_private(&path, &bytes)
        .map_err(|error| format!("写入 automation 台账失败：{error}"))
}

fn object(args: &Value) -> Result<&Map<String, Value>, String> {
    args.as_object()
        .ok_or_else(|| "automation 参数必须是对象".to_owned())
}

fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("automation 缺少 {key}"))?;
    let value = value.trim();
    if value.is_empty() || value.len() > 4 * 1024 * 1024 || value.chars().any(char::is_control) {
        return Err(format!("automation {key} 不能为空、过长或包含控制字符"));
    }
    Ok(value.to_owned())
}

fn optional_string(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let mut single = Map::new();
            single.insert(key.to_owned(), value.clone());
            Ok(Some(required_string(&single, key)?))
        }
    }
}

fn workspace_scope(app: &AppHandle, args: &Map<String, Value>) -> Result<WorkspaceScope, String> {
    let raw = required_string(args, "workspacePath")?;
    if matches!(args.get("locationKind").and_then(Value::as_str), Some(kind) if kind != "local") {
        return Err("远程 automation 未启用；只能使用 local workspace".to_owned());
    }
    if args.get("botDeliveryTarget").is_some() {
        return Err("云 bot 投递未启用；automation 只能进入本地 AgentRuntime".to_owned());
    }
    let path = crate::workspace::registered_project_root(app, &raw)?;
    let identity = args
        .get("workspaceIdentity")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let key = identity
        .clone()
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    Ok(WorkspaceScope {
        key,
        path,
        identity,
    })
}

fn validate_mode(value: Option<&Value>) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let mode = value
        .as_str()
        .ok_or_else(|| "automation mode 必须是字符串".to_owned())?;
    if !matches!(
        mode,
        "yolo" | "plan" | "edit" | "auto" | "autoEdit" | "build"
    ) {
        return Err(format!("不支持的 automation mode：{mode}"));
    }
    Ok(Some(mode.to_owned()))
}

fn validate_model_selection(value: Option<&Value>) -> Result<Option<Value>, String> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let map = value
        .as_object()
        .ok_or_else(|| "automation modelSelection 必须是对象".to_owned())?;
    let provider = map
        .get("providerId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "automation modelSelection.providerId 缺失".to_owned())?;
    let model = map
        .get("modelId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "automation modelSelection.modelId 缺失".to_owned())?;
    if provider.len() > 256
        || model.len() > 512
        || provider.chars().any(char::is_control)
        || model.chars().any(char::is_control)
    {
        return Err("automation modelSelection 标识无效".to_owned());
    }
    if let Some(options) = map.get("options") {
        let options = options
            .as_object()
            .ok_or_else(|| "automation modelSelection.options 必须是对象".to_owned())?;
        if let Some(level) = options.get("reasoningLevel")
            && !level.is_null()
            && level.as_str().is_none()
        {
            return Err("automation reasoningLevel 必须是字符串".to_owned());
        }
    }
    Ok(Some(value.clone()))
}

fn parse_schedule_rule(value: Option<&Value>) -> Result<Option<ScheduleRule>, String> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let rule: ScheduleRule = serde_json::from_value(value.clone())
        .map_err(|error| format!("automation scheduleRule 无效：{error}"))?;
    validate_schedule_rule(&rule)?;
    Ok(Some(rule))
}

fn validate_schedule_rule(rule: &ScheduleRule) -> Result<(), String> {
    if !matches!(
        rule.unit.as_str(),
        "minute" | "hourly" | "daily" | "weekly" | "monthly" | "yearly"
    ) {
        return Err("automation scheduleRule.unit 不受支持".to_owned());
    }
    if rule.interval == 0 || (rule.unit == "monthly" && rule.interval > 1_200) {
        return Err("automation scheduleRule.interval 超出范围".to_owned());
    }
    if rule.hour > 23 || rule.minute > 59 {
        return Err("automation scheduleRule 的 hour/minute 无效".to_owned());
    }
    validate_number_list(rule.weekdays.as_deref(), 0, 6, "weekdays")?;
    validate_number_list(rule.month_days.as_deref(), 1, 31, "monthDays")?;
    validate_number_list(rule.months.as_deref(), 1, 12, "months")?;
    if let Some(mode) = rule.monthly_mode.as_deref()
        && !matches!(mode, "date" | "weekday")
    {
        return Err("automation scheduleRule.monthlyMode 无效".to_owned());
    }
    if rule.unit == "weekly" && rule.weekdays.as_ref().is_none_or(Vec::is_empty) {
        return Err("weekly automation 必须包含 weekdays".to_owned());
    }
    if rule.unit == "monthly" {
        if rule.monthly_mode.as_deref() == Some("weekday") {
            if rule.weekdays.as_ref().is_none_or(Vec::is_empty) {
                return Err("monthly weekday automation 必须包含 weekdays".to_owned());
            }
        } else if rule.month_days.as_ref().is_none_or(Vec::is_empty) {
            return Err("monthly automation 必须包含 monthDays".to_owned());
        }
    }
    Ok(())
}

fn validate_number_list(
    values: Option<&[u32]>,
    min: u32,
    max: u32,
    name: &str,
) -> Result<(), String> {
    if let Some(values) = values
        && (values.is_empty() || values.iter().any(|value| *value < min || *value > max))
    {
        return Err(format!("automation scheduleRule.{name} 无效"));
    }
    Ok(())
}

fn parse_cron_set(field: &str, min: u32, max: u32) -> Result<BTreeSet<u32>, String> {
    let mut values = BTreeSet::new();
    for term in field.split(',') {
        let term = term.trim();
        if term.is_empty() {
            return Err("cron 包含空字段".to_owned());
        }
        let (range, step) = term.split_once('/').map_or((term, 1), |(range, raw)| {
            (range, raw.parse::<u32>().unwrap_or(0))
        });
        if step == 0 {
            return Err("cron 步长必须为正整数".to_owned());
        }
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                a.parse::<u32>().map_err(|_| "cron 范围无效".to_owned())?,
                b.parse::<u32>().map_err(|_| "cron 范围无效".to_owned())?,
            )
        } else {
            let value = range
                .parse::<u32>()
                .map_err(|_| "cron 数字无效".to_owned())?;
            (value, value)
        };
        if start < min || end > max || start > end {
            return Err("cron 字段超出范围".to_owned());
        }
        let mut value = start;
        while value <= end {
            values.insert(value);
            match value.checked_add(step) {
                Some(next) if next > value => value = next,
                _ => break,
            }
        }
    }
    if values.is_empty() {
        Err("cron 字段不能为空".to_owned())
    } else {
        Ok(values)
    }
}

fn validate_cron(expr: &str) -> Result<(), String> {
    let fields = expr.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err("cron 必须是五段本地时间表达式".to_owned());
    }
    let _ = parse_cron_set(fields[0], 0, 59)?;
    let _ = parse_cron_set(fields[1], 0, 23)?;
    let _ = parse_cron_set(fields[2], 1, 31)?;
    let _ = parse_cron_set(fields[3], 1, 12)?;
    let _ = parse_cron_set(fields[4], 0, 6)?;
    Ok(())
}

fn infer_rule(expr: &str, anchor_at: i64) -> Option<ScheduleRule> {
    let fields = expr.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 || fields[2..].iter().any(|field| *field != "*") {
        return None;
    }
    if let Some(step) = fields[0]
        .strip_prefix("*/")
        .and_then(|value| value.parse().ok())
    {
        return Some(ScheduleRule {
            unit: "minute".to_owned(),
            interval: step,
            hour: 0,
            minute: 0,
            anchor_at,
            weekdays: None,
            month_days: None,
            months: None,
            monthly_mode: None,
        });
    }
    if let Some(step) = fields[1]
        .strip_prefix("*/")
        .and_then(|value| value.parse().ok())
    {
        return Some(ScheduleRule {
            unit: "hourly".to_owned(),
            interval: step,
            hour: 0,
            minute: 0,
            anchor_at,
            weekdays: None,
            month_days: None,
            months: None,
            monthly_mode: None,
        });
    }
    None
}

fn next_cron(expr: &str, after_ms: i64) -> Option<i64> {
    let fields = expr.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return None;
    }
    let minute = parse_cron_set(fields[0], 0, 59).ok()?;
    let hour = parse_cron_set(fields[1], 0, 23).ok()?;
    let day = parse_cron_set(fields[2], 1, 31).ok()?;
    let month = parse_cron_set(fields[3], 1, 12).ok()?;
    let weekday = parse_cron_set(fields[4], 0, 6).ok()?;
    let start = Local
        .timestamp_millis_opt(after_ms)
        .single()?
        .with_second(0)?
        .with_nanosecond(0)?
        + ChronoDuration::minutes(1);
    for offset in 0..=MAX_SCHEDULE_SEARCH_MINUTES {
        let candidate = start + ChronoDuration::minutes(offset);
        if minute.contains(&candidate.minute())
            && hour.contains(&candidate.hour())
            && day.contains(&candidate.day())
            && month.contains(&candidate.month())
            && weekday.contains(&candidate.weekday().num_days_from_sunday())
        {
            return Some(candidate.timestamp_millis());
        }
    }
    None
}

fn next_rule(rule: &ScheduleRule, after_ms: i64) -> Option<i64> {
    let anchor = rule.anchor_at.max(0);
    match rule.unit.as_str() {
        "minute" => {
            let step = i64::from(rule.interval).checked_mul(60_000)?;
            // source 的 minute carrier 从 anchor 之后一个完整 interval 触发，
            // 不能因为创建请求刚落库就立即重跑一次。
            let n = ((after_ms.saturating_sub(anchor)).div_euclid(step) + 1).max(1);
            Some(anchor.saturating_add(n.saturating_mul(step)))
        }
        "hourly" => {
            let step = i64::from(rule.interval).checked_mul(3_600_000)?;
            let base = Local
                .timestamp_millis_opt(anchor)
                .single()?
                .with_minute(rule.minute)?
                .with_second(0)?
                .with_nanosecond(0)?
                .timestamp_millis();
            let n = ((after_ms.saturating_sub(base)).div_euclid(step) + 1).max(0);
            Some(base.saturating_add(n.saturating_mul(step)))
        }
        "daily" => next_daily(rule, after_ms),
        "weekly" => next_weekly(rule, after_ms),
        "monthly" => next_monthly(rule, after_ms),
        "yearly" => next_yearly(rule, after_ms),
        _ => None,
    }
}

fn next_daily(rule: &ScheduleRule, after_ms: i64) -> Option<i64> {
    let anchor = Local.timestamp_millis_opt(rule.anchor_at).single()?;
    for index in 0..36_600_i64 {
        let date = anchor
            .date_naive()
            .checked_add_signed(ChronoDuration::days(
                index.saturating_mul(i64::from(rule.interval)),
            ))?;
        let candidate = Local
            .with_ymd_and_hms(
                date.year(),
                date.month(),
                date.day(),
                rule.hour,
                rule.minute,
                0,
            )
            .single()?;
        if candidate.timestamp_millis() > after_ms {
            return Some(candidate.timestamp_millis());
        }
    }
    None
}

fn next_weekly(rule: &ScheduleRule, after_ms: i64) -> Option<i64> {
    let anchor = Local.timestamp_millis_opt(rule.anchor_at).single()?;
    let anchor_week = anchor.date_naive()
        - ChronoDuration::days(i64::from(anchor.weekday().num_days_from_monday()));
    let weekdays = rule
        .weekdays
        .clone()
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| vec![1]);
    for week in 0..5_220_i64 {
        if week % i64::from(rule.interval) != 0 {
            continue;
        }
        for weekday in &weekdays {
            let day_offset = (weekday + 6) % 7;
            let date = anchor_week.checked_add_signed(ChronoDuration::days(
                week.saturating_mul(7).saturating_add(i64::from(day_offset)),
            ))?;
            let candidate = Local
                .with_ymd_and_hms(
                    date.year(),
                    date.month(),
                    date.day(),
                    rule.hour,
                    rule.minute,
                    0,
                )
                .single()?;
            if candidate.timestamp_millis() > after_ms {
                return Some(candidate.timestamp_millis());
            }
        }
    }
    None
}

fn next_monthly(rule: &ScheduleRule, after_ms: i64) -> Option<i64> {
    let anchor = Local.timestamp_millis_opt(rule.anchor_at).single()?;
    for offset in (0..=1_200_u32).step_by(rule.interval as usize) {
        let month = anchor
            .date_naive()
            .with_day(1)?
            .checked_add_months(chrono::Months::new(offset))?;
        if rule.monthly_mode.as_deref() == Some("weekday") {
            let weekday = rule
                .weekdays
                .as_ref()
                .and_then(|values| values.first())
                .copied()
                .unwrap_or(1);
            let first = Local
                .with_ymd_and_hms(month.year(), month.month(), 1, rule.hour, rule.minute, 0)
                .single()?;
            let day_offset = (weekday + 7 - first.weekday().num_days_from_sunday()) % 7;
            let date = month.checked_add_signed(ChronoDuration::days(i64::from(day_offset)))?;
            let candidate = Local
                .with_ymd_and_hms(
                    date.year(),
                    date.month(),
                    date.day(),
                    rule.hour,
                    rule.minute,
                    0,
                )
                .single()?;
            if candidate.timestamp_millis() > after_ms {
                return Some(candidate.timestamp_millis());
            }
        } else {
            for day in rule.month_days.clone().unwrap_or_else(|| vec![1]) {
                let Some(date) = month.with_day(day) else {
                    continue;
                };
                let Some(candidate) = Local
                    .with_ymd_and_hms(
                        date.year(),
                        date.month(),
                        date.day(),
                        rule.hour,
                        rule.minute,
                        0,
                    )
                    .single()
                else {
                    continue;
                };
                if candidate.timestamp_millis() > after_ms {
                    return Some(candidate.timestamp_millis());
                }
            }
        }
    }
    None
}

fn next_yearly(rule: &ScheduleRule, after_ms: i64) -> Option<i64> {
    let anchor = Local.timestamp_millis_opt(rule.anchor_at).single()?;
    let months = rule.months.clone().unwrap_or_else(|| vec![anchor.month()]);
    let month_days = rule
        .month_days
        .clone()
        .unwrap_or_else(|| vec![anchor.day()]);
    for offset in (0..400_u32).step_by(rule.interval as usize) {
        let year = anchor.year().saturating_add(offset as i32);
        for month in &months {
            for day in &month_days {
                let Some(candidate) = Local
                    .with_ymd_and_hms(year, *month, *day, rule.hour, rule.minute, 0)
                    .single()
                else {
                    continue;
                };
                if candidate.timestamp_millis() > after_ms {
                    return Some(candidate.timestamp_millis());
                }
            }
        }
    }
    None
}

fn next_for(automation: &AutomationRecord, after_ms: i64) -> Option<i64> {
    automation
        .schedule_rule
        .as_ref()
        .and_then(|rule| next_rule(rule, after_ms))
        .or_else(|| next_cron(&automation.cron_expr, after_ms))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        now_ms(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

/// 将 automation 台账的 runId 映射为资源层可持久化的 TurnId。
///
/// 台账 runId 使用冒号区分 automation、触发方式和本次运行；资源标识
/// 语法不接受冒号。当前 runId 的生成部分均只含小写 ASCII、数字和横线，
/// 因而只编码分隔符即可保持可读性与稳定的一一映射。
fn automation_turn_id(run_id: &str) -> String {
    format!("automation-turn-{}", run_id.replace(':', "--"))
}

fn automation_value(record: &AutomationRecord) -> Value {
    let mut value = Map::new();
    value.insert("automationId".to_owned(), json!(record.automation_id));
    value.insert("title".to_owned(), json!(record.title));
    value.insert("cronExpr".to_owned(), json!(record.cron_expr));
    value.insert("prompt".to_owned(), json!(record.prompt));
    if let Some(model) = &record.model_selection {
        value.insert("modelSelection".to_owned(), model.clone());
    }
    if let Some(mode) = &record.mode {
        value.insert("mode".to_owned(), json!(mode));
    }
    value.insert("workspaceKey".to_owned(), json!(record.workspace_key));
    value.insert("workspacePath".to_owned(), json!(record.workspace_path));
    if let Some(identity) = &record.workspace_identity {
        value.insert("workspaceIdentity".to_owned(), json!(identity));
    }
    if let Some(task) = &record.target_task_id {
        value.insert("targetTaskId".to_owned(), json!(task));
    }
    value.insert("locationKind".to_owned(), json!(record.location_kind));
    value.insert("recurring".to_owned(), json!(record.recurring));
    if let Some(max) = record.max_runs {
        value.insert("maxRuns".to_owned(), json!(max));
    }
    if let Some(end) = record.end_at {
        value.insert("endAt".to_owned(), json!(end));
    }
    if let Some(rule) = &record.schedule_rule {
        value.insert(
            "scheduleRule".to_owned(),
            serde_json::to_value(rule).unwrap_or(Value::Null),
        );
    }
    if let Some(edited) = record.schedule_edited_by_user {
        value.insert("scheduleEditedByUser".to_owned(), json!(edited));
    }
    value.insert("runCount".to_owned(), json!(record.run_count));
    value.insert("enabled".to_owned(), json!(record.enabled));
    value.insert("lifecycleStatus".to_owned(), json!(record.lifecycle_status));
    if let Some(next) = record.next_run_at {
        value.insert("nextRunAt".to_owned(), json!(next));
    }
    if let Some(last) = record.last_run_at {
        value.insert("lastRunAt".to_owned(), json!(last));
    }
    value.insert("dispatchStatus".to_owned(), json!(record.dispatch_status));
    value.insert(
        "dispatchAttempts".to_owned(),
        json!(record.dispatch_attempts),
    );
    if let Some(retry) = record.retry_at {
        value.insert("retryAt".to_owned(), json!(retry));
    }
    if let Some(error) = &record.last_error {
        value.insert("lastError".to_owned(), json!(error));
    }
    value.insert("createdAt".to_owned(), json!(record.created_at));
    value.insert("updatedAt".to_owned(), json!(record.updated_at));
    Value::Object(value)
}

fn run_value(run: &RunRecord) -> Value {
    let mut value = Map::new();
    value.insert("runId".to_owned(), json!(run.run_id));
    value.insert("automationId".to_owned(), json!(run.automation_id));
    value.insert("workspaceKey".to_owned(), json!(run.workspace_key));
    if let Some(scheduled) = run.scheduled_at {
        value.insert("scheduledAt".to_owned(), json!(scheduled));
    }
    value.insert("trigger".to_owned(), json!(run.trigger));
    if let Some(model) = &run.model_selection {
        value.insert("modelSelection".to_owned(), model.clone());
    }
    value.insert("dispatchStatus".to_owned(), json!(run.dispatch_status));
    if let Some(outcome) = &run.outcome {
        value.insert("outcome".to_owned(), json!(outcome));
    }
    if let Some(session) = &run.session_id {
        value.insert("sessionId".to_owned(), json!(session));
    }
    if let Some(error) = &run.error {
        value.insert("error".to_owned(), json!(error));
    }
    value.insert("attempts".to_owned(), json!(run.attempts));
    value.insert("createdAt".to_owned(), json!(run.created_at));
    value.insert("updatedAt".to_owned(), json!(run.updated_at));
    Value::Object(value)
}

fn ensure_limit(store: &StoreFile) -> Result<(), String> {
    if store.automations.len() >= MAX_AUTOMATIONS {
        Err("[AUTOMATION_CREATE_LIMIT_REACHED] automation limit reached".to_owned())
    } else {
        Ok(())
    }
}

fn list_automations(app: &AppHandle, args: &Value, all: bool) -> Result<Value, String> {
    let map = object(args)?;
    let scope = if all {
        None
    } else {
        Some(workspace_scope(app, map)?)
    };
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let store = read_store(app)?;
    let values = store
        .automations
        .iter()
        .filter(|record| {
            scope
                .as_ref()
                .is_none_or(|scope| record.workspace_key == scope.key)
        })
        .map(automation_value)
        .collect::<Vec<_>>();
    Ok(Value::Array(values))
}

fn create_automation(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let now = now_ms();
    let prompt = required_string(map, "prompt")?;
    if prompt.len() > 2 * 1024 * 1024 {
        return Err("automation prompt 超过 2 MB".to_owned());
    }
    let mut cron = required_string(map, "cronExpr")?;
    let relative = map.get("relativeDelayMinutes").and_then(Value::as_i64);
    if let Some(value) = relative
        && !(1..=525_600).contains(&value)
    {
        return Err("relativeDelayMinutes 必须在 1-525600 之间".to_owned());
    }
    let recurring = map
        .get("recurring")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let max_runs = map
        .get("maxRuns")
        .and_then(Value::as_u64)
        .map(|v| u32::try_from(v).map_err(|_| "maxRuns 超出范围".to_owned()))
        .transpose()?;
    let interval_unit = map.get("intervalUnit").and_then(Value::as_str);
    let interval = map
        .get("interval")
        .and_then(Value::as_u64)
        .map(|v| u32::try_from(v).map_err(|_| "interval 超出范围".to_owned()))
        .transpose()?;
    if interval_unit.is_some() != interval.is_some() {
        return Err("intervalUnit 与 interval 必须同时提交".to_owned());
    }
    if interval_unit.is_some() && (relative.is_some() || !recurring || max_runs.is_some()) {
        return Err(
            "interval carrier 不能与一次性、recurring=false 或 maxRuns 同时使用".to_owned(),
        );
    }
    let mut rule = parse_schedule_rule(map.get("scheduleRule"))?;
    if let Some(unit) = interval_unit {
        let interval = interval.ok_or("interval 缺失")?;
        if !(1..=200).contains(&interval)
            || !matches!(
                unit,
                "minute" | "hourly" | "daily" | "weekly" | "monthly" | "yearly"
            )
        {
            return Err("interval carrier 无效".to_owned());
        }
        rule = Some(interval_rule(unit, interval, now));
        cron = carrier_cron(unit, interval);
    }
    if relative.is_some() && (recurring || max_runs.is_some() || rule.is_some()) {
        return Err("relativeDelayMinutes 只能用于一次性任务".to_owned());
    }
    validate_cron(&cron)?;
    if let Some(rule) = &rule {
        validate_schedule_rule(rule)?;
    }
    // source create 会用服务端当前时间重置 scheduleRule.anchorAt，避免调用方
    // 传入旧锚点后首轮在创建前触发或被跳过。
    if let Some(rule) = rule.as_mut() {
        rule.anchor_at = now;
    }
    if rule.is_none() {
        rule = infer_rule(&cron, now);
    }
    let title = optional_string(map, "title")?.unwrap_or_else(|| {
        prompt
            .lines()
            .next()
            .unwrap_or("Automation")
            .chars()
            .take(80)
            .collect()
    });
    let end_at = map.get("endAt").and_then(Value::as_i64);
    if end_at.is_some_and(|end| end <= now) {
        return Err("endAt 必须晚于当前时间".to_owned());
    }
    let record = AutomationRecord {
        automation_id: new_id("automation"),
        title,
        cron_expr: cron,
        prompt,
        model_selection: validate_model_selection(map.get("modelSelection"))?,
        mode: validate_mode(map.get("mode"))?,
        workspace_key: scope.key.clone(),
        workspace_path: scope.path.to_string_lossy().into_owned(),
        workspace_identity: scope.identity,
        target_task_id: optional_string(map, "targetTaskId")?,
        location_kind: "local".to_owned(),
        recurring,
        max_runs,
        end_at,
        schedule_rule: rule,
        schedule_edited_by_user: None,
        run_count: 0,
        scheduled_run_count: 0,
        enabled: true,
        lifecycle_status: "active".to_owned(),
        next_run_at: None,
        last_run_at: None,
        dispatch_status: "idle".to_owned(),
        dispatch_attempts: 0,
        retry_at: None,
        last_error: None,
        created_at: now,
        updated_at: now,
    };
    let next = if let Some(relative) = relative {
        Some(now.saturating_add(relative.saturating_mul(60_000)))
    } else {
        next_for(&record, now.saturating_sub(1))
    };
    if next.is_none() {
        return Err("cron/scheduleRule 在可计算范围内没有下次触发时间".to_owned());
    }
    let mut record = record;
    record.next_run_at = next;
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    ensure_limit(&store)?;
    store.automations.push(record.clone());
    write_store(app, &store)?;
    wake_scheduler();
    Ok(automation_value(&record))
}

fn find_owned_mut<'a>(
    store: &'a mut StoreFile,
    id: &str,
    scope: &WorkspaceScope,
) -> Result<&'a mut AutomationRecord, String> {
    store
        .automations
        .iter_mut()
        .find(|record| record.automation_id == id && record.workspace_key == scope.key)
        .ok_or_else(|| "automation 不存在或不属于当前 workspace".to_owned())
}

fn update_automation(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let id = required_string(map, "automationId")?;
    let now = now_ms();
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    let record = find_owned_mut(&mut store, &id, &scope)?;
    let mut new_cron = record.cron_expr.clone();
    if let Some(value) = map.get("cronExpr") {
        new_cron = value
            .as_str()
            .ok_or("cronExpr 必须是字符串")?
            .trim()
            .to_owned();
        validate_cron(&new_cron)?;
    }
    if let Some(value) = map.get("title") {
        record.title = value
            .as_str()
            .filter(|v| !v.trim().is_empty())
            .ok_or("title 不能为空")?
            .trim()
            .to_owned();
    }
    if let Some(value) = map.get("prompt") {
        record.prompt = value
            .as_str()
            .filter(|v| !v.trim().is_empty())
            .ok_or("prompt 不能为空")?
            .trim()
            .to_owned();
    }
    if map.contains_key("modelSelection") {
        record.model_selection = validate_model_selection(map.get("modelSelection"))?;
    }
    if map.contains_key("mode") {
        record.mode = validate_mode(map.get("mode"))?;
    }
    if let Some(value) = map.get("recurring") {
        record.recurring = value.as_bool().ok_or("recurring 必须是布尔值")?;
    }
    if let Some(value) = map.get("maxRuns") {
        if value.is_null() {
            if !record.recurring {
                return Err("清空 maxRuns 时必须同时设置 recurring=true".to_owned());
            }
            record.max_runs = None;
        } else {
            let number = value
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0)
                .ok_or("maxRuns 必须是正整数")?;
            if record.recurring {
                return Err("recurring=true 不能同时设置 maxRuns 数字上限".to_owned());
            }
            record.max_runs = Some(number);
        }
    }
    if let Some(value) = map.get("endAt") {
        record.end_at = if value.is_null() {
            None
        } else {
            Some(
                value
                    .as_i64()
                    .filter(|end| *end > now)
                    .ok_or("endAt 必须晚于当前时间")?,
            )
        };
    }
    if map.contains_key("scheduleRule") {
        record.schedule_rule = parse_schedule_rule(map.get("scheduleRule"))?;
    }
    let interval_unit = map.get("intervalUnit").and_then(Value::as_str);
    let interval = map
        .get("interval")
        .and_then(Value::as_u64)
        .map(|v| u32::try_from(v).map_err(|_| "interval 超出范围".to_owned()))
        .transpose()?;
    if interval_unit.is_some() != interval.is_some() {
        return Err("intervalUnit 与 interval 必须同时提交".to_owned());
    }
    if let Some(unit) = interval_unit {
        let value = interval.ok_or("interval 缺失")?;
        if !record.recurring || record.max_runs.is_some() || !(1..=200).contains(&value) {
            return Err("interval carrier 只能用于无限 recurring 任务".to_owned());
        }
        record.schedule_rule = Some(interval_rule(unit, value, now));
        new_cron = carrier_cron(unit, value);
    }
    if let Some(value) = map.get("scheduleEditedByUser") {
        record.schedule_edited_by_user =
            Some(value.as_bool().ok_or("scheduleEditedByUser 必须是布尔值")?);
    }
    record.cron_expr = new_cron;
    if let Some(rule) = &record.schedule_rule {
        validate_schedule_rule(rule)?;
    }
    record.updated_at = now;
    record.lifecycle_status = if !record.enabled {
        "paused".to_owned()
    } else {
        "active".to_owned()
    };
    record.next_run_at = if record.enabled {
        next_for(record, now.saturating_sub(1))
    } else {
        record.next_run_at
    };
    if record
        .end_at
        .is_some_and(|end| record.next_run_at.is_none_or(|next| next > end))
    {
        record.next_run_at = None;
        record.lifecycle_status = "completed".to_owned();
    }
    let result = automation_value(record);
    write_store(app, &store)?;
    wake_scheduler();
    Ok(result)
}

fn delete_automation(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let id = required_string(map, "automationId")?;
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    let before = store.automations.len();
    store
        .automations
        .retain(|record| !(record.automation_id == id && record.workspace_key == scope.key));
    if store.automations.len() == before {
        return Err("automation 不存在或不属于当前 workspace".to_owned());
    }
    store.runs.retain(|run| run.automation_id != id);
    write_store(app, &store)?;
    wake_scheduler();
    Ok(Value::Null)
}

fn set_enabled(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let id = required_string(map, "automationId")?;
    let enabled = map
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or("enabled 必须是布尔值")?;
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    let record = find_owned_mut(&mut store, &id, &scope)?;
    record.enabled = enabled;
    record.updated_at = now_ms();
    if enabled {
        record.lifecycle_status = "active".to_owned();
    } else {
        record.lifecycle_status = "paused".to_owned();
    }
    write_store(app, &store)?;
    wake_scheduler();
    Ok(Value::Null)
}

fn restart_automation(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let id = required_string(map, "automationId")?;
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    let record = find_owned_mut(&mut store, &id, &scope)?;
    if record.lifecycle_status != "failed" {
        return Ok(Value::Null);
    }
    let now = now_ms();
    record.enabled = true;
    record.lifecycle_status = "active".to_owned();
    record.dispatch_status = "idle".to_owned();
    record.retry_at = None;
    record.last_error = None;
    record.run_count = 0;
    record.scheduled_run_count = 0;
    record.dispatch_attempts = 0;
    record.next_run_at = next_for(record, now.saturating_sub(1));
    record.updated_at = now;
    write_store(app, &store)?;
    wake_scheduler();
    Ok(Value::Null)
}

fn run_now(app: &AppHandle, args: &Value, connection_id: Option<&str>) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let id = required_string(map, "automationId")?;
    let connection_id = connection_id
        .map(|value| {
            ConnectionId::new(value.to_owned())
                .map_err(|error| format!("automation connection 无效：{error}"))
        })
        .transpose()?;
    // 冷恢复后台账里的 running 可能落后于 Journal；先用 Runtime 终态释放
    // 已结束的 run，再做 duplicate 判定，避免一次重启永久阻塞手动运行。
    reconcile_terminal_runs(app, &id, &scope.key);
    let mut connection_bindings = connection_id
        .as_ref()
        .map(|_| {
            manual_run_connections()
                .lock()
                .map_err(|_| "automation connection 状态不可用".to_owned())
        })
        .transpose()?;
    let now = now_ms();
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    let automation = store
        .automations
        .iter()
        .find(|record| record.automation_id == id && record.workspace_key == scope.key)
        .cloned()
        .ok_or_else(|| "automation 不存在或不属于当前 workspace".to_owned())?;
    if manual_run_is_in_flight(&store, &id, &scope.key) {
        return Ok(json!({"status":"duplicate"}));
    }
    let run_id = queue_manual_run(&mut store, &automation, now);
    trim_runs(&mut store);
    write_store(app, &store)?;
    if let (Some(connection_id), Some(bindings)) = (connection_id, connection_bindings.as_mut()) {
        bindings.insert(run_id.clone(), connection_id);
    }
    wake_scheduler();
    Ok(json!({"status":"queued"}))
}

/// 手动运行的去重只约束同一 automation 与 workspace，避免跨 workspace
/// 的历史记录阻塞当前项目；`claimed` 无终态也必须保留为真实 in-flight。
fn manual_run_is_in_flight(store: &StoreFile, automation_id: &str, workspace_key: &str) -> bool {
    store.runs.iter().any(|run| {
        run.automation_id == automation_id
            && run.workspace_key == workspace_key
            && run.trigger == "manual"
            && (run.outcome.as_deref() == Some("running")
                || (run.dispatch_status == "claimed" && run.outcome.is_none()))
    })
}

fn queue_manual_run(store: &mut StoreFile, automation: &AutomationRecord, now: i64) -> String {
    let run_id = format!("{}:manual:{}", automation.automation_id, new_id("run"));
    store.runs.push(RunRecord {
        run_id: run_id.clone(),
        automation_id: automation.automation_id.clone(),
        workspace_key: automation.workspace_key.clone(),
        scheduled_at: Some(now),
        trigger: "manual".to_owned(),
        model_selection: automation.model_selection.clone(),
        dispatch_status: "claimed".to_owned(),
        outcome: None,
        session_id: None,
        error: None,
        attempts: 1,
        created_at: now,
        updated_at: now,
    });
    run_id
}

fn list_runs(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let id = required_string(map, "automationId")?;
    reconcile_terminal_runs(app, &id, &scope.key);
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let store = read_store(app)?;
    if !store
        .automations
        .iter()
        .any(|record| record.automation_id == id && record.workspace_key == scope.key)
    {
        return Err("automation 不存在或不属于当前 workspace".to_owned());
    }
    let mut runs = store
        .runs
        .iter()
        .filter(|run| run.automation_id == id && run.workspace_key == scope.key)
        .collect::<Vec<_>>();
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    Ok(Value::Array(runs.into_iter().map(run_value).collect()))
}

/// RPC 刷新可能正好落在 watcher 写入终态之前；返回历史前再次以 Runtime
/// Session snapshot 对账，避免把 Journal 已终止的 run 继续投影成 running。
fn reconcile_terminal_runs(app: &AppHandle, automation_id: &str, workspace_key: &str) {
    let candidates = {
        let _guard = store_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Ok(store) = read_store(app) else {
            return;
        };
        store
            .runs
            .iter()
            .filter(|run| {
                run.automation_id == automation_id
                    && run.workspace_key == workspace_key
                    && run.outcome.as_deref() == Some("running")
            })
            .filter_map(|run| {
                run.session_id
                    .as_ref()
                    .map(|session_id| (run.run_id.clone(), session_id.clone()))
            })
            .collect::<Vec<_>>()
    };
    let Some(runtime) = app
        .try_state::<Arc<AgentRuntime>>()
        .map(|state| Arc::clone(state.inner()))
    else {
        return;
    };
    for (run_id, session_id) in candidates {
        let Ok(turn_key) = keencode_resources::TurnId::new(automation_turn_id(&run_id)) else {
            continue;
        };
        let Ok(snapshot) = runtime.session_snapshot(&session_id) else {
            continue;
        };
        let Some(turn) = snapshot.state.turns.get(&turn_key) else {
            continue;
        };
        reconcile_terminal_run(app, &run_id, &turn.status, turn.outcome_message.as_deref());
    }
}

fn reconcile_terminal_run(
    app: &AppHandle,
    run_id: &str,
    status: &TurnStatus,
    message: Option<&str>,
) {
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Ok(mut store) = read_store(app) else {
        return;
    };
    if reconcile_terminal_run_in_store(&mut store, run_id, status, message, now_ms()) {
        let _ = write_store(app, &store);
    }
}

/// 这是 Runtime Journal 终态到 automation 台账的唯一决策入口；测试通过
/// 这个 production port 验证冷恢复后的 duplicate 与重新排队边界。
fn reconcile_terminal_run_in_store(
    store: &mut StoreFile,
    run_id: &str,
    status: &TurnStatus,
    message: Option<&str>,
    now: i64,
) -> bool {
    let Some((outcome, error)) = terminal_turn_outcome(status, message) else {
        return false;
    };
    apply_run_outcome(store, run_id, outcome, error.as_deref(), now)
}

fn delete_run(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let map = object(args)?;
    let scope = workspace_scope(app, map)?;
    let id = required_string(map, "runId")?;
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    let before = store.runs.len();
    store
        .runs
        .retain(|run| !(run.run_id == id && run.workspace_key == scope.key));
    if before == store.runs.len() {
        return Err("automation run 不存在或不属于当前 workspace".to_owned());
    }
    write_store(app, &store)?;
    Ok(Value::Null)
}

fn trim_runs(store: &mut StoreFile) {
    if store.runs.len() > MAX_RUNS {
        store.runs.sort_by_key(|run| run.created_at);
        let excess = store.runs.len() - MAX_RUNS;
        store.runs.drain(0..excess);
    }
}

fn interval_rule(unit: &str, interval: u32, anchor_at: i64) -> ScheduleRule {
    let date = Local
        .timestamp_millis_opt(anchor_at)
        .single()
        .unwrap_or_else(Local::now);
    ScheduleRule {
        unit: unit.to_owned(),
        interval,
        hour: date.hour(),
        minute: date.minute(),
        anchor_at,
        weekdays: (unit == "weekly").then(|| vec![date.weekday().num_days_from_sunday()]),
        month_days: (unit == "monthly").then(|| vec![date.day()]),
        months: (unit == "yearly").then(|| vec![date.month()]),
        monthly_mode: None,
    }
}

fn carrier_cron(unit: &str, interval: u32) -> String {
    match unit {
        "minute" => format!("*/{interval} * * * *"),
        "hourly" => format!("0 */{interval} * * *"),
        "daily" => format!("0 0 */{interval} * *"),
        "weekly" => format!("0 0 * * */{interval}"),
        "monthly" => format!("0 0 1 */{interval} *"),
        "yearly" => format!("0 0 1 1 */{interval}"),
        _ => "0 * * * *".to_owned(),
    }
}

fn next_wait(app: &AppHandle) -> Option<Duration> {
    let _guard = store_lock().lock().ok()?;
    let store = read_store(app).ok()?;
    let now = now_ms();
    let next = store
        .runs
        .iter()
        .filter(|run| {
            run.dispatch_status == "claimed" && run.outcome.is_none() && !is_in_flight(&run.run_id)
        })
        .filter_map(|run| run.scheduled_at.or(Some(now)))
        .min()
        .or_else(|| {
            store
                .automations
                .iter()
                .filter(|record| record.enabled && record.lifecycle_status == "active")
                .filter_map(|record| record.next_run_at)
                .min()
        });
    next.map(|value| Duration::from_millis(value.saturating_sub(now).max(0) as u64))
}

fn is_in_flight(run_id: &str) -> bool {
    IN_FLIGHT
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|set| set.contains(run_id))
        .unwrap_or(true)
}

fn mark_in_flight(run_id: &str) -> bool {
    IN_FLIGHT
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|mut set| set.insert(run_id.to_owned()))
        .unwrap_or(false)
}

fn clear_in_flight(run_id: &str) {
    if let Some(lock) = IN_FLIGHT.get()
        && let Ok(mut set) = lock.lock()
    {
        set.remove(run_id);
    }
}

fn dispatch_due(app: &AppHandle) -> Vec<String> {
    let now = now_ms();
    let _guard = match store_lock().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut store = match read_store(app) {
        Ok(store) => store,
        Err(error) => {
            tracing::error!(%error, "automation 台账读取失败");
            return Vec::new();
        }
    };
    let mut selected = Vec::new();
    for index in 0..store.runs.len() {
        if store.runs[index].dispatch_status == "claimed"
            && store.runs[index].outcome.is_none()
            && !is_in_flight(&store.runs[index].run_id)
            && mark_in_flight(&store.runs[index].run_id)
        {
            selected.push(store.runs[index].run_id.clone());
        }
    }
    for index in 0..store.automations.len() {
        let record = &mut store.automations[index];
        if !record.enabled
            || record.lifecycle_status != "active"
            || record.next_run_at.is_none_or(|next| next > now)
        {
            continue;
        }
        let scheduled = record.next_run_at.unwrap_or(now);
        let run_id = format!("{}:{scheduled}", record.automation_id);
        if store.runs.iter().any(|run| run.run_id == run_id) {
            record.next_run_at = next_for(record, scheduled);
            continue;
        }
        let max_reached =
            !record.recurring && record.scheduled_run_count >= record.max_runs.unwrap_or(1);
        if max_reached {
            record.lifecycle_status = "completed".to_owned();
            record.next_run_at = None;
            continue;
        }
        record.dispatch_status = "claimed".to_owned();
        record.dispatch_attempts = record.dispatch_attempts.saturating_add(1);
        record.updated_at = now;
        record.next_run_at = if record.recurring {
            next_for(record, scheduled)
        } else {
            None
        };
        store.runs.push(RunRecord {
            run_id: run_id.clone(),
            automation_id: record.automation_id.clone(),
            workspace_key: record.workspace_key.clone(),
            scheduled_at: Some(scheduled),
            trigger: "schedule".to_owned(),
            model_selection: record.model_selection.clone(),
            dispatch_status: "claimed".to_owned(),
            outcome: None,
            session_id: None,
            error: None,
            attempts: record.dispatch_attempts,
            created_at: now,
            updated_at: now,
        });
        if mark_in_flight(&run_id) {
            selected.push(run_id);
        }
    }
    trim_runs(&mut store);
    if !selected.is_empty()
        && let Err(error) = write_store(app, &store)
    {
        tracing::error!(%error, "automation 认领持久化失败");
        for run_id in &selected {
            clear_in_flight(run_id);
        }
        return Vec::new();
    }
    selected
}

async fn dispatch_run(app: AppHandle, run_id: String) {
    let result = dispatch_run_inner(&app, &run_id).await;
    clear_in_flight(&run_id);
    if let Err(error) = result {
        tracing::error!(%error, %run_id, "automation 派发失败");
        mark_dispatch_failure(&app, &run_id, &error);
    }
}

async fn dispatch_run_inner(app: &AppHandle, run_id: &str) -> Result<(), String> {
    let elicitation_connection_id = take_manual_run_connection(run_id);
    let (automation, run) = {
        let _guard = store_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let store = read_store(app)?;
        let run = store
            .runs
            .iter()
            .find(|run| run.run_id == run_id)
            .cloned()
            .ok_or_else(|| "automation run 不存在".to_owned())?;
        let automation = store
            .automations
            .iter()
            .find(|record| {
                record.automation_id == run.automation_id
                    && record.workspace_key == run.workspace_key
            })
            .cloned()
            .ok_or_else(|| "automation 定义不存在".to_owned())?;
        (automation, run)
    };
    let runtime = crate::require_owned_runtime(app)?;
    let project = crate::workspace::registered_project_root(app, &automation.workspace_path)?;
    let session = runtime
        .open_or_create_session(&project, automation.target_task_id.as_deref(), run_id)
        .map_err(|error| format!("打开 automation Session 失败：{error}"))?;
    let session_id = session.session_id().as_str().to_owned();
    if let Some(selection) = run
        .model_selection
        .as_ref()
        .or(automation.model_selection.as_ref())
    {
        let selected = selection.as_object().ok_or("modelSelection 必须是对象")?;
        let provider = selected
            .get("providerId")
            .and_then(Value::as_str)
            .ok_or("modelSelection.providerId 缺失")?;
        let model = selected
            .get("modelId")
            .and_then(Value::as_str)
            .ok_or("modelSelection.modelId 缺失")?;
        runtime
            .set_session_model(
                &session_id,
                &format!("automation-model:{run_id}"),
                provider,
                model,
            )
            .map_err(|error| format!("automation 模型不可用：{error}"))?;
        if let Some(level) = selected
            .get("options")
            .and_then(Value::as_object)
            .and_then(|options| options.get("reasoningLevel"))
            .and_then(Value::as_str)
        {
            runtime
                .set_session_effort(&session_id, &format!("automation-effort:{run_id}"), level)
                .map_err(|error| format!("automation 推理等级不可用：{error}"))?;
        }
    }
    let turn_id = automation_turn_id(run_id);
    runtime
        .start_root_turn(
            &session_id,
            &turn_id,
            &automation.prompt,
            RootTurnOptions {
                plan_enabled: automation.mode.as_deref() == Some("plan"),
                developer_context: Some(format!(
                    "Automation ID: {}\nRun ID: {}",
                    automation.automation_id, run.run_id
                )),
                elicitation_connection_id,
                ..RootTurnOptions::default()
            },
        )
        .await
        .map_err(|error| format!("automation AgentRuntime 启动失败：{error}"))?;
    mark_dispatched(app, run_id, &session_id)?;
    let watcher_app = app.clone();
    let watched_run_id = run_id.to_owned();
    tauri::async_runtime::spawn(async move {
        watch_turn(watcher_app, watched_run_id, session_id).await;
    });
    Ok(())
}

fn mark_dispatched(app: &AppHandle, run_id: &str, session_id: &str) -> Result<(), String> {
    let now = now_ms();
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_store(app)?;
    let run = store
        .runs
        .iter_mut()
        .find(|run| run.run_id == run_id)
        .ok_or_else(|| "automation run 不存在".to_owned())?;
    if run.dispatch_status == "dispatched" {
        return Ok(());
    }
    run.dispatch_status = "dispatched".to_owned();
    run.outcome = Some("running".to_owned());
    run.session_id = Some(session_id.to_owned());
    run.error = None;
    run.updated_at = now;
    if let Some(record) = store.automations.iter_mut().find(|record| {
        record.automation_id == run.automation_id && record.workspace_key == run.workspace_key
    }) {
        record.dispatch_status = "dispatched".to_owned();
        record.last_run_at = Some(now);
        record.run_count = record.run_count.saturating_add(1);
        if run.trigger == "schedule" {
            record.scheduled_run_count = record.scheduled_run_count.saturating_add(1);
            if !record.recurring && record.scheduled_run_count >= record.max_runs.unwrap_or(1) {
                record.lifecycle_status = "completed".to_owned();
                record.next_run_at = None;
            }
        }
        record.updated_at = now;
    }
    write_store(app, &store)
}

fn mark_dispatch_failure(app: &AppHandle, run_id: &str, error: &str) {
    let now = now_ms();
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Ok(mut store) = read_store(app) else {
        return;
    };
    let Some(run) = store.runs.iter_mut().find(|run| run.run_id == run_id) else {
        return;
    };
    run.dispatch_status = "failed_to_dispatch".to_owned();
    run.outcome = Some("failed".to_owned());
    run.error = Some(error.chars().take(2_000).collect());
    run.updated_at = now;
    if let Some(record) = store.automations.iter_mut().find(|record| {
        record.automation_id == run.automation_id && record.workspace_key == run.workspace_key
    }) {
        record.dispatch_status = "failed_to_dispatch".to_owned();
        record.lifecycle_status = "failed".to_owned();
        record.last_error = run.error.clone();
        record.updated_at = now;
    }
    let _ = write_store(app, &store);
}

async fn watch_turn(app: AppHandle, run_id: String, session_id: String) {
    let turn_id = automation_turn_id(&run_id);
    loop {
        let Some(runtime) = app
            .try_state::<Arc<AgentRuntime>>()
            .map(|state| Arc::clone(state.inner()))
        else {
            mark_run_outcome(&app, &run_id, "failed", Some("AgentRuntime 不可用"));
            return;
        };
        match runtime.session_snapshot(&session_id) {
            Ok(snapshot) => {
                let turn_key = match keencode_resources::TurnId::new(turn_id.clone()) {
                    Ok(turn_key) => turn_key,
                    Err(error) => {
                        mark_run_outcome(
                            &app,
                            &run_id,
                            "failed",
                            Some(&format!("automation turn 标识无效：{error}")),
                        );
                        return;
                    }
                };
                if let Some(turn) = snapshot.state.turns.get(&turn_key)
                    && let Some((outcome, error)) =
                        terminal_turn_outcome(&turn.status, turn.outcome_message.as_deref())
                {
                    mark_run_outcome(&app, &run_id, outcome, error.as_deref());
                    return;
                }
            }
            Err(error) => {
                mark_run_outcome(
                    &app,
                    &run_id,
                    "failed",
                    Some(&format!("读取 automation Session 失败：{error}")),
                );
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Runtime 终态是 automation run 历史的唯一映射依据；Running 保留给下一次对账。
fn terminal_turn_outcome(
    status: &TurnStatus,
    message: Option<&str>,
) -> Option<(&'static str, Option<String>)> {
    match status {
        TurnStatus::Running => None,
        TurnStatus::Completed => Some(("succeeded", None)),
        TurnStatus::Failed => Some(("failed", message.map(ToOwned::to_owned))),
        TurnStatus::Cancelled => Some(("stopped", message.map(ToOwned::to_owned))),
    }
}

fn mark_run_outcome(app: &AppHandle, run_id: &str, outcome: &str, error: Option<&str>) {
    let now = now_ms();
    let _guard = store_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Ok(mut store) = read_store(app) else {
        return;
    };
    if apply_run_outcome(&mut store, run_id, outcome, error, now) {
        let _ = write_store(app, &store);
    }
}

fn apply_run_outcome(
    store: &mut StoreFile,
    run_id: &str,
    outcome: &str,
    error: Option<&str>,
    now: i64,
) -> bool {
    let Some((automation_id, workspace_key)) = store
        .runs
        .iter_mut()
        .find(|run| run.run_id == run_id)
        .map(|run| {
            run.outcome = Some(outcome.to_owned());
            run.error = error.map(|value| value.chars().take(2_000).collect());
            run.updated_at = now;
            (run.automation_id.clone(), run.workspace_key.clone())
        })
    else {
        return false;
    };
    if let Some(record) = store.automations.iter_mut().find(|record| {
        record.automation_id == automation_id && record.workspace_key == workspace_key
    }) {
        record.dispatch_status = "idle".to_owned();
        record.last_error = error.map(|value| value.chars().take(2_000).collect());
        record.updated_at = now;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionCoordinator;
    use keencode_acp::ConnectionId;
    use keencode_agent::{
        AgentId, SessionId, ToolApprovalDecision, ToolApprovalError, ToolApprovalGate,
        ToolApprovalRequest, ToolCallId, ToolEffect, TurnCancellation, TurnId,
    };

    fn write_request(session_id: &str) -> ToolApprovalRequest {
        ToolApprovalRequest {
            session_id: SessionId::new(session_id).unwrap(),
            turn_id: TurnId::new("automation-turn").unwrap(),
            source_agent_id: AgentId::new("automation-agent").unwrap(),
            tool_call_id: ToolCallId::new("automation-call").unwrap(),
            tool_name: "Shell".to_owned(),
            input: json!({"command": "echo automation"}),
            effect: ToolEffect::ChangesState,
            operation_id: None,
        }
    }

    #[test]
    fn cron_validation_accepts_common_five_field_forms() {
        assert!(validate_cron("*/5 * * * *").is_ok());
        assert!(validate_cron("0 9 * * 1-5").is_ok());
        assert!(validate_cron("0 0 1 * *").is_ok());
        assert!(validate_cron("0 0 1 13 *").is_err());
    }

    #[test]
    fn relative_rule_uses_server_anchor() {
        let now = 1_700_000_000_000;
        let record = AutomationRecord {
            automation_id: "a".to_owned(),
            title: "t".to_owned(),
            cron_expr: "*/1 * * * *".to_owned(),
            prompt: "p".to_owned(),
            model_selection: None,
            mode: None,
            workspace_key: "k".to_owned(),
            workspace_path: "p".to_owned(),
            workspace_identity: None,
            target_task_id: None,
            location_kind: "local".to_owned(),
            recurring: false,
            max_runs: None,
            end_at: None,
            schedule_rule: None,
            schedule_edited_by_user: None,
            run_count: 0,
            scheduled_run_count: 0,
            enabled: true,
            lifecycle_status: "active".to_owned(),
            next_run_at: None,
            last_run_at: None,
            dispatch_status: "idle".to_owned(),
            dispatch_attempts: 0,
            retry_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        assert!(next_for(&record, now).is_some());
    }

    #[test]
    fn automation_run_id_is_encoded_as_a_valid_turn_id() {
        let turn_id = automation_turn_id("automation-1:manual:run-2");

        assert_eq!(turn_id, "automation-turn-automation-1--manual--run-2");
        assert!(keencode_resources::TurnId::new(turn_id).is_ok());
    }

    #[tokio::test]
    async fn manual_dispatch_connection_reaches_real_permission_coordinator() {
        let run_id = "automation-test:manual:bound";
        let connection = ConnectionId::new("rpc-automation-test").unwrap();
        manual_run_connections()
            .lock()
            .unwrap()
            .insert(run_id.to_owned(), connection.clone());
        let routed = take_manual_run_connection(run_id).expect("手动 run 应取出连接身份");
        assert_eq!(routed, connection);
        assert!(take_manual_run_connection(run_id).is_none());

        let coordinator = PermissionCoordinator::new();
        coordinator
            .bind_session_connection("automation-session", routed.clone())
            .unwrap();
        let pending =
            coordinator.request(write_request("automation-session"), TurnCancellation::new());
        assert_eq!(
            coordinator.pending_count_for_session("automation-session"),
            1
        );
        let view = coordinator
            .pending_views_for_connection("automation-session", &routed)
            .pop()
            .expect("有效手动连接应看到 Write 权限 pending");
        coordinator
            .resolve_from_connection(
                "automation-session",
                &routed,
                &view.interaction_id,
                r#"{"optionId":"allowOnce"}"#,
            )
            .unwrap();
        assert_eq!(pending.await.unwrap(), ToolApprovalDecision::Approved);

        let scheduler_coordinator = PermissionCoordinator::new();
        assert!(matches!(
            scheduler_coordinator
                .request(write_request("scheduler-session"), TurnCancellation::new())
                .await,
            Err(ToolApprovalError::ConnectionClosed)
        ));

        let orphan_run = "automation-test:manual:closed";
        manual_run_connections()
            .lock()
            .unwrap()
            .insert(orphan_run.to_owned(), connection.clone());
        connection_closed(connection.as_str());
        assert!(take_manual_run_connection(orphan_run).is_none());
    }

    #[test]
    fn terminal_turn_outcome_preserves_runtime_status_and_message() {
        assert_eq!(terminal_turn_outcome(&TurnStatus::Running, None), None);
        assert_eq!(
            terminal_turn_outcome(&TurnStatus::Completed, None),
            Some(("succeeded", None))
        );
        assert_eq!(
            terminal_turn_outcome(&TurnStatus::Failed, Some("timeout")),
            Some(("failed", Some("timeout".to_owned())))
        );
        assert_eq!(
            terminal_turn_outcome(&TurnStatus::Cancelled, Some("cancelled")),
            Some(("stopped", Some("cancelled".to_owned())))
        );
    }

    fn test_automation() -> AutomationRecord {
        AutomationRecord {
            automation_id: "automation-cold-reconcile".to_owned(),
            title: "cold reconcile".to_owned(),
            cron_expr: "0 0 * * *".to_owned(),
            prompt: "test".to_owned(),
            model_selection: None,
            mode: None,
            workspace_key: "workspace-cold-reconcile".to_owned(),
            workspace_path: "C:/workspace-cold-reconcile".to_owned(),
            workspace_identity: None,
            target_task_id: None,
            location_kind: "local".to_owned(),
            recurring: false,
            max_runs: Some(2),
            end_at: None,
            schedule_rule: None,
            schedule_edited_by_user: None,
            run_count: 1,
            scheduled_run_count: 0,
            enabled: true,
            lifecycle_status: "active".to_owned(),
            next_run_at: None,
            last_run_at: Some(42),
            dispatch_status: "dispatched".to_owned(),
            dispatch_attempts: 1,
            retry_at: None,
            last_error: None,
            created_at: 1,
            updated_at: 42,
        }
    }

    fn cold_running_store() -> StoreFile {
        let automation = test_automation();
        StoreFile {
            schema: STORE_SCHEMA,
            automations: vec![automation.clone()],
            runs: vec![RunRecord {
                run_id: "automation-cold-reconcile:manual:run-old".to_owned(),
                automation_id: automation.automation_id,
                workspace_key: automation.workspace_key,
                scheduled_at: Some(42),
                trigger: "manual".to_owned(),
                model_selection: None,
                dispatch_status: "dispatched".to_owned(),
                outcome: Some("running".to_owned()),
                session_id: Some("session-cold-reconcile".to_owned()),
                error: None,
                attempts: 1,
                created_at: 42,
                updated_at: 42,
            }],
        }
    }

    #[test]
    fn terminal_journal_outcomes_release_cold_running_dedupe_and_queue_next_run() {
        for (status, expected_outcome) in [
            (TurnStatus::Cancelled, "stopped"),
            (TurnStatus::Failed, "failed"),
            (TurnStatus::Completed, "succeeded"),
        ] {
            let mut store = cold_running_store();
            let automation = store.automations[0].clone();
            let old_run_id = store.runs[0].run_id.clone();
            assert!(manual_run_is_in_flight(
                &store,
                &automation.automation_id,
                &automation.workspace_key
            ));
            assert!(reconcile_terminal_run_in_store(
                &mut store,
                &old_run_id,
                &status,
                Some("Journal terminal"),
                100,
            ));
            assert_eq!(store.runs[0].outcome.as_deref(), Some(expected_outcome));
            assert!(!manual_run_is_in_flight(
                &store,
                &automation.automation_id,
                &automation.workspace_key
            ));

            let new_run_id = queue_manual_run(&mut store, &automation, 101);
            assert!(store.runs.iter().any(|run| run.run_id == new_run_id));
            assert!(manual_run_is_in_flight(
                &store,
                &automation.automation_id,
                &automation.workspace_key
            ));
        }
    }

    #[test]
    fn running_journal_status_keeps_manual_duplicate_gate() {
        let mut store = cold_running_store();
        let automation = store.automations[0].clone();
        let old_run_id = store.runs[0].run_id.clone();
        assert!(!reconcile_terminal_run_in_store(
            &mut store,
            &old_run_id,
            &TurnStatus::Running,
            None,
            100,
        ));
        assert!(manual_run_is_in_flight(
            &store,
            &automation.automation_id,
            &automation.workspace_key
        ));
        assert_eq!(store.runs.len(), 1);
    }
}
