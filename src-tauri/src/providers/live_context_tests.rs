//! 真实 Provider 上下文压缩、工具生命周期与冷恢复的脱敏验证。
//!
//! 该模块预期作为 `src-tauri/src/providers.rs` 的测试子模块加入。普通测试只验证
//! 报告写入与脱敏边界；真实 Provider 测试显式标记为 ignored，只有用户提供完整环境变量
//! 并主动执行时才会访问远端。

use anyhow::{Context, Result, bail};
use keencode_agent::{
    AgentRunner, AgentTool, ContextManager, ContextPolicy, JsonContextTokenEstimator, PlanGuard,
    RunLimits, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    ToolRegistry, TurnRequest,
};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelResponse, ProviderCapabilities, ToolDefinition,
};
use keencode_provider::{
    ProviderClient, RequestErrorKind, RequestMode, RequestObservation, RequestObservationScope,
    RequestObservationState, RequestObserver,
};
use keencode_resources::{
    ContextCompressionTrigger as ResourceCompressionTrigger, SessionEvent, SessionEventRecord,
    SessionStatus, TurnStatus,
};
use keencode_runtime::{
    CreateSessionRequest, OpenSessionResult, RuntimeConfig, RuntimeSession, RuntimeTurnRequest,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{CustomProvider, runtime_provider_config};

/// 真实测试的基础地址由调用进程从用户授权测试配置读取，不建立旧文件兼容入口。
const LIVE_BASE_URL_ENV: &str = "KEENCODE_LIVE_BASE_URL";
/// 认证只通过当前测试进程环境传入，不写入源码、配置副本或报告。
const LIVE_API_KEY_ENV: &str = "KEENCODE_LIVE_API_KEY";
/// 用户测试来源所用协议，用于仅在内存中转换完整端点。
const LIVE_SOURCE_PROTOCOL_ENV: &str = "KEENCODE_LIVE_SOURCE_PROTOCOL";
/// 真实测试选择的 Provider 标识环境变量。
const LIVE_PROVIDER_ID_ENV: &str = "KEENCODE_LIVE_PROVIDER_ID";
/// 真实测试选择的模型标识环境变量。
const LIVE_MODEL_ENV: &str = "KEENCODE_LIVE_MODEL";
/// 真实测试期望的协议环境变量。
const LIVE_PROTOCOL_ENV: &str = "KEENCODE_LIVE_PROTOCOL";
/// 真实测试脱敏证据目录环境变量。
const LIVE_EVIDENCE_DIR_ENV: &str = "KEENCODE_LIVE_EVIDENCE_DIR";
/// 真实测试报告使用的源代码版本环境变量。
const LIVE_SOURCE_VERSION_ENV: &str = "KEENCODE_LIVE_SOURCE_VERSION";
/// 真实测试生成的固定报告文件名。
const LIVE_REPORT_FILE_NAME: &str = "live-context-compression-report.json";
/// 失败阶段报告的固定文件名；不包含底层错误正文。
const LIVE_STAGE_REPORT_FILE_NAME: &str = "live-context-stage-report.json";
/// 真实测试在证据目录下创建的唯一合成 Runtime 持久化目录名。
const LIVE_RUNTIME_DIR_NAME: &str = "runtime-session";
/// 报告 schema 的固定版本。
const LIVE_REPORT_SCHEMA: &str = "keencode/live-context-compression/v1";
/// 真实测试人为收紧的模型上下文窗口。
const LIVE_CONTEXT_WINDOW: u64 = 16_384;
/// 主 Agent Round 使用的最大输出 Token，避免噪声测试失控。
const LIVE_ROUND_OUTPUT_TOKENS: u32 = 256;
/// 摘要调用和能力快照使用的最大输出 Token。
const LIVE_SUMMARY_OUTPUT_TOKENS: u32 = 1_024;
/// 在首次事实 Turn 后追加的合成噪声 Turn 数。
const LIVE_SYNTHETIC_NOISE_ROUNDS: usize = 18;
/// 每个噪声 Turn 中的唯一合成文本单元数。
const LIVE_SYNTHETIC_UNITS_PER_ROUND: usize = 64;

/// 首轮必须被冷恢复后的真实模型回答重新提及的目标字段和值。
const FACT_TARGET: &str = "给atlas新增重试";
/// 首轮事实中的用户约束字段和值。
const FACT_CONSTRAINT: &str = "最多3次且不加依赖";
/// 首轮事实中的相对文件路径字段和值。
const FACT_RELATIVE_PATH: &str = "src/retry.rs";
/// 首轮事实中的失败原因字段和值。
const FACT_FAILURE: &str = "429";
/// 首轮事实中的 Goal 字段和值。
const FACT_GOAL: &str = "完成可靠重试";
/// 首轮事实中的 Todo 字段和值。
const FACT_TODO: &str = "补退避测试";
/// 首轮事实中的子任务状态字段和值。
const FACT_SUBTASK: &str = "worker-a运行中等待结果";
/// 首轮事实中的下一步字段和值。
const FACT_NEXT_STEP: &str = "收到结果后跑测试";

/// 只保存固定枚举和 HTTP 状态，不保存端点、请求标识、正文或认证字段。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveHttpObservation {
    /// 当前观测属于逻辑调用还是实际 HTTP 尝试。
    scope: &'static str,
    /// 当前观测的固定生命周期状态。
    state: &'static str,
    /// 真实 Adapter 使用的协议。
    protocol: &'static str,
    /// 实际传输采用流式或完整响应。
    mode: &'static str,
    /// 已接收的 HTTP 状态；未收到响应头时为空。
    http_status: Option<u16>,
    /// Provider 中立的失败分类；无失败时为空。
    error_kind: Option<&'static str>,
}

/// 有界记录真实 Provider 生命周期，记录失败不得影响生产请求。
#[derive(Default)]
struct LiveHttpObserver {
    /// 最多保存一千条脱敏短观测。
    observations: Mutex<Vec<LiveHttpObservation>>,
    /// 队列超限或锁损坏时阻止验收报告宣称完整。
    incomplete: AtomicBool,
}

impl LiveHttpObserver {
    /// 在验收阶段取得完整观测，失败时不伪造空的成功记录。
    fn snapshot(&self) -> Result<Vec<LiveHttpObservation>> {
        if self.incomplete.load(Ordering::Relaxed) {
            bail!("真实 HTTP 观测不完整");
        }
        self.observations
            .lock()
            .map(|records| records.clone())
            .map_err(|_| anyhow::anyhow!("真实 HTTP 观测锁损坏"))
    }
}

impl RequestObserver for LiveHttpObserver {
    /// 立即丢弃任何自由文本，只记录实际请求所用的固定枚举和状态码。
    fn on_request(&self, value: RequestObservation) {
        let record = LiveHttpObservation {
            scope: match value.scope {
                RequestObservationScope::Logical => "logical",
                RequestObservationScope::Attempt => "attempt",
            },
            state: match value.state {
                RequestObservationState::Started => "started",
                RequestObservationState::Completed => "completed",
                RequestObservationState::Cancelled => "cancelled",
                RequestObservationState::Failed => "failed",
            },
            protocol: match value.protocol {
                keencode_model::ProviderProtocol::Messages => "messages",
                keencode_model::ProviderProtocol::ChatCompletions => "chat_completions",
                keencode_model::ProviderProtocol::Responses => "responses",
            },
            mode: match value.mode {
                RequestMode::Stream => "streaming",
                RequestMode::Buffered => "buffered",
            },
            http_status: value.http_status,
            error_kind: value.error_kind.map(|kind| match kind {
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
            }),
        };
        if let Ok(mut records) = self.observations.lock()
            && records.len() < 1_000
        {
            records.push(record);
            return;
        }
        self.incomplete.store(true, Ordering::Relaxed);
    }
}

/// 工具调用后只保留统计数量，不持久化工具输入或输出正文。
struct SyntheticObservationTool {
    /// 真实工具实现被调用的次数。
    calls: Arc<AtomicUsize>,
}

impl AgentTool for SyntheticObservationTool {
    /// 返回只读合成观测工具的固定定义。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "record_synthetic_observation",
            "记录一条仅用于上下文压缩测试的合成观测，不访问文件、网络或用户数据。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "observation": {"type": "string"}
                },
                "required": ["observation"],
                "additionalProperties": false
            }),
        )
    }

    /// 固定声明该工具为只读，不改变项目状态。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 只读工具允许在同一 Round 内按 Agent 规则并行执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 记录真实执行次数并返回脱敏的短结果。
    fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(ToolOutput::text("synthetic observation recorded")) })
    }
}

/// 一个已完成真实 Turn 的报告投影，不含 Prompt、Response 或 Transcript 正文。
struct TurnObservation {
    /// Turn 的合成稳定标识。
    turn_id: String,
    /// Runner 是否以完整成功终态结束。
    success: bool,
    /// Runner 发起的模型 Round 数量。
    rounds: u32,
    /// Runner 实际开始执行的工具 Step 数量。
    steps: u32,
    /// Runner 产生并提交的上下文压缩数量。
    compactions: usize,
}

/// 报告中单个 Turn 的脱敏统计。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveTurnReport {
    /// Turn 的稳定标识。
    turn_id: String,
    /// 是否完整成功。
    success: bool,
    /// 模型 Round 数量。
    rounds: u32,
    /// 实际工具 Step 数量。
    steps: u32,
    /// 上下文压缩数量。
    compactions: usize,
    /// Provider 明确报告的累计输入 Token；未知时为 null。
    input_tokens: Option<u64>,
    /// Provider 明确报告的累计输出 Token；未知时为 null。
    output_tokens: Option<u64>,
}

/// 报告中单条压缩的可审计数值。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveCompactionReport {
    /// 压缩触发原因。
    trigger: &'static str,
    /// 压缩前估算 Token。
    estimated_tokens_before: u64,
    /// 压缩后估算 Token。
    estimated_tokens_after: u64,
    /// 本次减少的估算 Token。
    reduced_tokens: u64,
}

/// 报告中工具请求与完成事件的配对统计。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveToolLifecycleReport {
    /// `ToolRequested` 事件数量。
    requested: usize,
    /// `ToolExecutionStarted` 事件数量。
    started: usize,
    /// `ToolCompleted` 事件数量。
    completed: usize,
    /// 每个请求 ID 是否恰好与一个完成事件配对。
    request_ids_paired: bool,
    /// 合成工具实现被真实调用的次数。
    synthetic_executions: usize,
}

/// 报告中八个事实标记的保留结果。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveFactRetention {
    /// 目标事实是否保留。
    target: bool,
    /// 用户约束是否保留。
    constraint: bool,
    /// 相对文件路径是否保留。
    relative_path: bool,
    /// 失败原因是否保留。
    failure: bool,
    /// Goal 是否保留。
    goal: bool,
    /// Todo 是否保留。
    todo: bool,
    /// 子任务状态是否保留。
    subtask: bool,
    /// 下一步是否保留。
    next_step: bool,
}

impl LiveFactRetention {
    /// 判断八个字段和值是否全部在最终回答中成对保留。
    fn all_retained(&self) -> bool {
        self.target
            && self.constraint
            && self.relative_path
            && self.failure
            && self.goal
            && self.todo
            && self.subtask
            && self.next_step
    }
}

/// 真实上下文测试的最终脱敏报告。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveContextReport {
    /// 固定报告 schema。
    schema_version: &'static str,
    /// 固定成功状态。
    status: &'static str,
    /// 本次真实调用使用的协议。
    protocol: String,
    /// 本次真实调用使用的模型。
    model: String,
    /// 由调用方显式提供的源代码/运行版本。
    source_version: String,
    /// 各真实 Turn 的统计。
    turns: Vec<LiveTurnReport>,
    /// 已提交的压缩统计。
    compactions: Vec<LiveCompactionReport>,
    /// 工具生命周期配对统计。
    tool_lifecycle: LiveToolLifecycleReport,
    /// 冷恢复后的事实保留布尔值。
    facts: LiveFactRetention,
    /// 冷恢复后仍可从证据目录重开的 Journal 事件数量。
    journal_events: usize,
    /// 证据目录下保留合成 Journal 和 Artifact 的相对目录名。
    runtime_storage_dir: &'static str,
    /// 测试源文件 SHA-256，不包含源文件正文。
    test_file_sha256: String,
    /// 来自生产 ProviderClient 的实际 HTTP 与逻辑请求短观测。
    http_observations: Vec<LiveHttpObservation>,
    /// 已与权威首轮 Transcript 核对的事实输入摘要，不包含正文。
    initial_facts_sha256: String,
    /// 冷打开前后完全一致的有效 Transcript 摘要。
    cold_transcript_sha256: String,
    /// 冷打开前后完全一致的有效 Transcript 修订号。
    cold_transcript_revision: u64,
}

/// 失败时写入证据目录的固定阶段快照，不携带底层错误、Prompt 或 Response。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveStageReport {
    /// 固定阶段报告 schema。
    schema_version: &'static str,
    /// 固定失败状态。
    status: &'static str,
    /// 最近开始执行的固定测试阶段。
    stage: &'static str,
    /// 已成功完成且已形成终态的 Turn 数量。
    completed_turns: usize,
    /// 是否已经创建了证据目录下的持久 Runtime 根目录。
    runtime_storage_prepared: bool,
    /// 失败前实际请求的固定分类和 HTTP 状态，不包含底层错误正文。
    http_observations: Vec<LiveHttpObservation>,
}

/// 读取必须由用户显式设置且不能是空白的测试环境变量。
fn required_live_env(name: &str) -> Result<String> {
    let value = env::var(name).with_context(|| format!("缺少真实测试环境变量 {name}"))?;
    if value.trim().is_empty() {
        bail!("真实测试环境变量 {name} 不能是空白");
    }
    Ok(value)
}

/// 将任意字节编码为固定小写 SHA-256 文本。
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 仅判断报告字符串是否显式携带常见绝对路径形态。
fn looks_like_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("\\\\")
        || (bytes.len() >= 3 && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'))
}

/// 归一化报告字段名，便于拒绝不同大小写和分隔符的敏感别名。
fn normalize_report_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// 判断报告字段是否不应出现在脱敏产物中。
fn is_forbidden_report_key(key: &str) -> bool {
    matches!(
        normalize_report_key(key).as_str(),
        "apikey"
            | "authorization"
            | "xapikey"
            | "headers"
            | "prompt"
            | "response"
            | "transcript"
            | "providerconfig"
            | "baseurl"
            | "projectroot"
            | "evidencedir"
    )
}

/// 递归检查报告字段、字符串值与路径，不保存或回显违规正文。
fn validate_redacted_report(value: &Value, location: &str) -> Result<()> {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if is_forbidden_report_key(key) {
                    bail!("报告包含禁止字段 {location}.{key}");
                }
                validate_redacted_report(child, &format!("{location}.{key}"))?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_redacted_report(child, &format!("{location}[{index}]"))?;
            }
        }
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if looks_like_absolute_path(text)
                || [
                    "authorization:",
                    "x-api-key:",
                    "bearer ",
                    "api_key=",
                    "api-key=",
                ]
                .iter()
                .any(|pattern| lower.contains(pattern))
            {
                bail!("报告包含绝对路径或认证 Header 文本，位置 {location}");
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

/// 确认目录存在且不是链接或其他特殊文件。
fn ensure_plain_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("创建真实测试目录失败：{}", path.display()))?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("读取真实测试目录失败：{}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("真实测试目标必须是普通目录");
    }
    Ok(())
}

/// 在证据目录下创建一次性的合成 Runtime 根目录，保留 Journal 供人工审计。
fn prepare_runtime_storage(evidence_dir: &Path) -> Result<PathBuf> {
    ensure_plain_directory(evidence_dir)?;
    let runtime_root = evidence_dir.join(LIVE_RUNTIME_DIR_NAME);
    match fs::symlink_metadata(&runtime_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("真实测试 Runtime 目录不是普通目录")
        }
        Ok(_) => bail!("真实测试 Runtime 目录已经存在，请使用新的证据目录"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&runtime_root).with_context(|| "创建真实测试 Runtime 目录失败")?;
        }
        Err(error) => return Err(error).context("读取真实测试 Runtime 目录失败"),
    }
    ensure_plain_directory(&runtime_root)?;
    Ok(runtime_root)
}

/// 将已通过递归脱敏检查的 JSON 产物写入用户明确指定的目录。
fn write_json_artifact(evidence_dir: &Path, file_name: &str, value: &Value) -> Result<PathBuf> {
    ensure_plain_directory(evidence_dir)?;
    let destination = evidence_dir.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && metadata.file_type().is_symlink()
    {
        bail!("真实测试报告目标不能是符号链接");
    }
    let bytes = serde_json::to_vec_pretty(value).context("编码真实测试 JSON 产物失败")?;
    fs::write(&destination, bytes)
        .with_context(|| format!("写入真实测试 JSON 产物失败：{file_name}"))?;
    Ok(destination)
}

/// 将已通过递归脱敏检查的报告写入用户明确指定的目录。
fn write_redacted_report(evidence_dir: &Path, report: &Value) -> Result<PathBuf> {
    validate_redacted_report(report, "report")?;
    write_json_artifact(evidence_dir, LIVE_REPORT_FILE_NAME, report)
}

/// 失败时写入固定阶段报告；调用方不得把底层错误正文带入产物或 panic。
fn write_stage_report(
    evidence_dir: &Path,
    stage: &'static str,
    completed_turns: usize,
    runtime_storage_prepared: bool,
    http: &LiveHttpObserver,
) -> Result<PathBuf> {
    let report = serde_json::to_value(LiveStageReport {
        schema_version: "keencode/live-context-stage/v1",
        status: "failed",
        stage,
        completed_turns,
        runtime_storage_prepared,
        http_observations: http.snapshot()?,
    })
    .context("编码真实测试阶段报告失败")?;
    validate_redacted_report(&report, "stageReport")?;
    write_json_artifact(evidence_dir, LIVE_STAGE_REPORT_FILE_NAME, &report)
}

/// 将物理 Journal 事件递归展开，保证 AtomicBatch 内事件也进入审计。
fn flatten_event<'a>(event: &'a SessionEvent, output: &mut Vec<&'a SessionEvent>) {
    if let SessionEvent::AtomicBatch { events } = event {
        for child in events {
            flatten_event(child, output);
        }
    } else {
        output.push(event);
    }
}

/// 分页读取当前 Session 的全部权威 Journal 事件。
fn read_all_events(session: &RuntimeSession) -> Result<Vec<SessionEventRecord>> {
    let mut records = Vec::new();
    let mut after = None;
    loop {
        let page = session
            .replay(after, 512)
            .context("读取真实测试 Journal 分页失败")?;
        let next = page.next_after;
        let has_more = page.has_more;
        records.extend(page.records);
        if !has_more {
            break;
        }
        let Some(next) = next else {
            bail!("Journal 分页声称还有事件但没有下一游标");
        };
        if after == Some(next) {
            bail!("Journal 分页游标没有前进");
        }
        after = Some(next);
    }
    Ok(records)
}

/// 返回事件记录的非嵌套审计视图。
fn flattened_events(records: &[SessionEventRecord]) -> Vec<&SessionEvent> {
    let mut events = Vec::new();
    for record in records {
        flatten_event(&record.event, &mut events);
    }
    events
}

/// 将资源层压缩触发原因转换为报告固定文本。
fn compression_trigger_name(trigger: &ResourceCompressionTrigger) -> &'static str {
    match trigger {
        ResourceCompressionTrigger::Budget => "budget",
        ResourceCompressionTrigger::ProviderOverflow => "provider_overflow",
    }
}

/// 收集所有已提交压缩的数值，并拒绝没有形成缩减的事件。
fn collect_compaction_reports(events: &[&SessionEvent]) -> Result<Vec<LiveCompactionReport>> {
    let mut reports = Vec::new();
    for event in events {
        let SessionEvent::CompactionApplied { compaction, .. } = event else {
            continue;
        };
        if compaction.estimated_tokens_before <= compaction.estimated_tokens_after {
            bail!("Journal 中存在没有形成 Token 缩减的压缩事件");
        }
        reports.push(LiveCompactionReport {
            trigger: compression_trigger_name(&compaction.trigger),
            estimated_tokens_before: compaction.estimated_tokens_before,
            estimated_tokens_after: compaction.estimated_tokens_after,
            reduced_tokens: compaction
                .estimated_tokens_before
                .saturating_sub(compaction.estimated_tokens_after),
        });
    }
    Ok(reports)
}

/// 验证所有工具请求 ID 恰好由一个完成事件收束，并返回脱敏统计。
fn collect_tool_lifecycle_report(
    events: &[&SessionEvent],
    synthetic_executions: usize,
) -> LiveToolLifecycleReport {
    let mut requested = BTreeMap::<String, usize>::new();
    let mut started = BTreeMap::<String, usize>::new();
    let mut completed = BTreeMap::<String, usize>::new();
    let mut valid = true;
    for event in events {
        match event {
            SessionEvent::ToolRequested { request } => {
                valid &= request.tool_name == "record_synthetic_observation"
                    && request.arguments
                        == serde_json::json!({"observation":"SYNTHETIC_OBSERVATION_INITIAL"})
                    && request.effect == keencode_resources::ToolEffect::ReadOnly;
                *requested
                    .entry(request.request_id.as_str().to_owned())
                    .or_default() += 1;
            }
            SessionEvent::ToolExecutionStarted { request_id } => {
                *started.entry(request_id.as_str().to_owned()).or_default() += 1;
            }
            SessionEvent::ToolCompleted {
                request_id,
                outcome,
            } => {
                valid &= outcome.status == keencode_resources::ToolCompletionStatus::Succeeded;
                *completed.entry(request_id.as_str().to_owned()).or_default() += 1;
            }
            _ => {}
        }
    }
    let request_ids_paired = valid
        && requested == started
        && started == completed
        && requested.len() == synthetic_executions
        && requested.values().all(|count| *count == 1)
        && completed.values().all(|count| *count == 1);
    LiveToolLifecycleReport {
        requested: requested.values().sum(),
        started: started.values().sum(),
        completed: completed.values().sum(),
        request_ids_paired,
        synthetic_executions,
    }
}

/// 汇总一个 Turn 的明确输入或输出 Token；任何未知值都保持 None。
fn sum_optional_tokens(total: Option<u64>, next: Option<u64>) -> Option<u64> {
    Some(total?.saturating_add(next?))
}

/// 从 Journal 的模型 Round 事件汇总指定 Turn 用量。
fn turn_usage(events: &[&SessionEvent], turn_id: &str) -> (Option<u64>, Option<u64>) {
    let mut seen = false;
    let mut input = Some(0_u64);
    let mut output = Some(0_u64);
    for event in events {
        let SessionEvent::ModelRoundCompleted {
            turn_id: event_turn,
            usage,
            ..
        } = event
        else {
            continue;
        };
        if event_turn.as_str() != turn_id {
            continue;
        }
        seen = true;
        input = sum_optional_tokens(input, usage.input_tokens);
        output = sum_optional_tokens(output, usage.output_tokens);
    }
    if seen { (input, output) } else { (None, None) }
}

/// 从完整模型响应中只提取普通文本块，避免把推理或工具参数写入报告。
fn response_text(response: &ModelResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Reasoning { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::ToolResult { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 返回八项事实字段和值在冷恢复后的真实最终回答中是否成对出现。
fn fact_retention(text: &str) -> LiveFactRetention {
    LiveFactRetention {
        target: contains_fact(text, "目标", FACT_TARGET),
        constraint: contains_fact(text, "约束", FACT_CONSTRAINT),
        relative_path: contains_fact(text, "文件", FACT_RELATIVE_PATH),
        failure: contains_fact(text, "失败原因", FACT_FAILURE),
        goal: contains_fact(text, "Goal", FACT_GOAL),
        todo: contains_fact(text, "Todo", FACT_TODO),
        subtask: contains_fact(text, "子任务", FACT_SUBTASK),
        next_step: contains_fact(text, "下一步", FACT_NEXT_STEP),
    }
}

/// 判断文本中是否保留了一个字段和值的明确对应关系，而非只出现孤立标记。
fn contains_fact(text: &str, field: &str, value: &str) -> bool {
    [
        format!("{field}={value}"),
        format!("{field}:{value}"),
        format!("{field}：{value}"),
        format!("\"{field}\":\"{value}\""),
        format!("\"{field}\": \"{value}\""),
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// 构造首轮包含八项有意义事实且明确要求真实调用合成工具的用户输入。
fn initial_fact_prompt() -> String {
    format!(
        concat!(
            "这是脱敏上下文压缩验证的首轮。请先调用 record_synthetic_observation，",
            "observation 固定使用 SYNTHETIC_OBSERVATION_INITIAL；工具完成后只用简短文本确认事实。",
            "请原样保留下面八个字段和值的对应关系，后续回合不得翻译或改写值：",
            "目标={FACT_TARGET}；约束={FACT_CONSTRAINT}；文件={FACT_RELATIVE_PATH}；",
            "失败原因={FACT_FAILURE}；Goal={FACT_GOAL}；Todo={FACT_TODO}；",
            "子任务={FACT_SUBTASK}；下一步={FACT_NEXT_STEP}。"
        ),
        FACT_TARGET = FACT_TARGET,
        FACT_CONSTRAINT = FACT_CONSTRAINT,
        FACT_RELATIVE_PATH = FACT_RELATIVE_PATH,
        FACT_FAILURE = FACT_FAILURE,
        FACT_GOAL = FACT_GOAL,
        FACT_TODO = FACT_TODO,
        FACT_SUBTASK = FACT_SUBTASK,
        FACT_NEXT_STEP = FACT_NEXT_STEP
    )
}

/// 构造不重复历史事实、但足以持续推动上下文压缩的合成噪声。
fn synthetic_noise_prompt(round: usize) -> String {
    let mut prompt = format!(
        "这是脱敏压缩测试的合成噪声第 {round} 轮。不要虚构或重复历史事实，不要调用工具，\
         仅用一句话确认已收到本轮噪声，然后继续等待下一轮。唯一噪声单元："
    );
    for unit in 0..LIVE_SYNTHETIC_UNITS_PER_ROUND {
        prompt.push_str(&format!(
            "synthetic_round_{round:02}_unit_{unit:03}_opaque_payload_6D2A9F07;"
        ));
    }
    prompt
}

/// 构造冷恢复后不携带预期答案的泛化回忆问题。
fn cold_recall_prompt() -> &'static str {
    "请回顾最初任务中仍然有效的八个事实。只按“字段=值”逐行列出你能从历史上下文可靠恢复的完整对应关系，必须保留原文值，不要猜测、不要调用工具。"
}

/// 返回协议对应的标准生成端点路径。
fn protocol_endpoint_path(protocol: &str) -> Result<&'static str> {
    match protocol {
        "messages" => Ok("/messages"),
        "chat_completions" => Ok("/chat/completions"),
        "responses" => Ok("/responses"),
        _ => bail!("真实测试协议不受支持"),
    }
}

/// 从当前 Provider 配置复制一个仅在内存中切换协议的测试 Fixture。
fn provider_fixture_for_protocol(
    provider: &CustomProvider,
    protocol: &str,
) -> Result<CustomProvider> {
    let requested_suffix = protocol_endpoint_path(protocol)?;
    let source_suffix = protocol_endpoint_path(&provider.api_backend)?;
    let exact_endpoint = provider.base_url.ends_with('#');
    let without_marker = provider
        .base_url
        .strip_suffix('#')
        .unwrap_or(provider.base_url.as_str())
        .trim_end_matches('/');
    let base = without_marker
        .strip_suffix(source_suffix)
        .or_else(|| without_marker.strip_suffix("/messages"))
        .or_else(|| without_marker.strip_suffix("/chat/completions"))
        .or_else(|| without_marker.strip_suffix("/responses"))
        .unwrap_or(without_marker)
        .trim_end_matches('/');
    let mut base_url = format!("{base}{requested_suffix}");
    if exact_endpoint {
        base_url.push('#');
    }
    Ok(CustomProvider {
        api_backend: protocol.to_owned(),
        base_url,
        ..provider.clone()
    })
}

/// 返回真实长会话固定使用的上下文压缩策略。
fn live_context_policy() -> ContextPolicy {
    ContextPolicy {
        precompress_enabled: true,
        trigger_percent: 70,
        target_percent: 45,
        reserved_output_tokens: LIVE_SUMMARY_OUTPUT_TOKENS as u64,
        forced_target_percent: 45,
        minimum_recent_units: 2,
        summary_max_output_tokens: LIVE_SUMMARY_OUTPUT_TOKENS,
    }
}

/// 为真实 Provider 构造同一套上下文管理器，保证热运行和冷恢复策略一致。
fn live_context_manager(
    provider: Arc<dyn keencode_model::ModelProvider>,
) -> Result<ContextManager> {
    ContextManager::new(
        live_context_policy(),
        Arc::new(JsonContextTokenEstimator),
        Arc::new(keencode_agent::ProviderContextCompressor::new(provider)),
    )
    .context("创建真实上下文管理器失败")
}

/// 创建一个使用当前有效根 Agent Transcript 的 Runtime Turn 请求。
fn runtime_turn(
    session: &RuntimeSession,
    turn_id: &str,
    model: &str,
    prompt: String,
) -> Result<RuntimeTurnRequest> {
    let input = Message::text(MessageRole::User, prompt.clone());
    let root_agent =
        keencode_resources::AgentId::new("root").context("创建真实测试根 Agent 标识失败")?;
    let mut transcript = session
        .model_transcript_for_agent(&root_agent)
        .context("读取真实测试有效 Transcript 失败")?;
    transcript.push(input.clone());
    let mut request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .context("创建真实测试 Agent Session 标识失败")?,
        keencode_agent::TurnId::new(turn_id).context("创建真实测试 Turn 标识失败")?,
        keencode_agent::AgentId::new("root").context("创建真实测试 Agent 标识失败")?,
        model,
        transcript,
        PlanGuard::inactive(),
    );
    request.model_request_mut().max_output_tokens = Some(LIVE_ROUND_OUTPUT_TOKENS);
    Ok(RuntimeTurnRequest::root(request, vec![input], prompt))
}

/// 将一次 Runtime 结果投影为不含模型正文的 Turn 统计。
fn observe_turn(turn_id: &str, result: &keencode_agent::TurnResult) -> Result<TurnObservation> {
    if !result.is_success() {
        bail!("真实 Provider Turn 未成功完成：{turn_id}");
    }
    Ok(TurnObservation {
        turn_id: turn_id.to_owned(),
        success: result.is_success(),
        rounds: result.state.round_count(),
        steps: result.state.step_count(),
        compactions: result.compactions.len(),
    })
}

/// 真实测试执行阶段与已完成 Turn 的脱敏进度。
struct LiveRunProgress {
    /// 最近开始执行的固定阶段名。
    stage: &'static str,
    /// 已成功形成终态的 Turn 数量。
    completed_turns: usize,
    /// 是否已经创建证据目录下的持久 Runtime 根目录。
    runtime_storage_prepared: bool,
    /// 热运行和冷恢复共用的脱敏观测集合，不共享 ProviderClient 本身。
    http: Arc<LiveHttpObserver>,
}

/// 运行完整真实 Provider 长会话，并在成功或失败时写入脱敏产物。
async fn run_live_context_test() -> Result<()> {
    let evidence_dir = PathBuf::from(required_live_env(LIVE_EVIDENCE_DIR_ENV)?);
    let mut progress = LiveRunProgress {
        stage: "startup",
        completed_turns: 0,
        runtime_storage_prepared: false,
        http: Arc::new(LiveHttpObserver::default()),
    };
    let result = run_live_context_test_inner(&evidence_dir, &mut progress).await;
    match result {
        Ok(report) => {
            progress.stage = "report.write";
            let value = serde_json::to_value(report).context("编码脱敏真实测试报告失败")?;
            if let Err(error) = write_redacted_report(&evidence_dir, &value) {
                write_stage_report(
                    &evidence_dir,
                    progress.stage,
                    progress.completed_turns,
                    progress.runtime_storage_prepared,
                    &progress.http,
                )?;
                return Err(error);
            }
            Ok(())
        }
        Err(error) => {
            write_stage_report(
                &evidence_dir,
                progress.stage,
                progress.completed_turns,
                progress.runtime_storage_prepared,
                &progress.http,
            )?;
            Err(error)
        }
    }
}

/// 在阶段快照中执行真实 Provider 长会话；底层错误只向上层传递，不写入产物。
async fn run_live_context_test_inner(
    evidence_dir: &Path,
    progress: &mut LiveRunProgress,
) -> Result<LiveContextReport> {
    progress.stage = "config.read";
    let provider_id = required_live_env(LIVE_PROVIDER_ID_ENV)?;
    let model = required_live_env(LIVE_MODEL_ENV)?;
    let protocol = required_live_env(LIVE_PROTOCOL_ENV)?;
    let source_version = required_live_env(LIVE_SOURCE_VERSION_ENV)?;

    // 真实测试只消费显式进程输入，正式配置仍只接受当前唯一 schema。
    let selected = CustomProvider {
        id: provider_id,
        name: "Live test provider".to_owned(),
        base_url: required_live_env(LIVE_BASE_URL_ENV)?,
        api_backend: required_live_env(LIVE_SOURCE_PROTOCOL_ENV)?,
        api_key: env::var(LIVE_API_KEY_ENV).ok(),
        models: vec![model.clone()],
        context_windows: Default::default(),
        context_1m: Default::default(),
        supports_vision: Default::default(),
    };
    let fixture = provider_fixture_for_protocol(&selected, &protocol)
        .context("构造当前协议的内存 Provider Fixture 失败")?;
    let configured_secret = fixture.api_key.as_deref();

    progress.stage = "provider.create";
    let mut provider_config =
        runtime_provider_config(&fixture).context("构造真实 Provider 配置失败")?;
    let capabilities = ProviderCapabilities {
        streaming: true,
        tool_calling: true,
        max_context_tokens: Some(LIVE_CONTEXT_WINDOW),
        max_output_tokens: Some(u64::from(LIVE_SUMMARY_OUTPUT_TOKENS)),
        ..ProviderCapabilities::default()
    };
    provider_config.default_capabilities = capabilities.clone();
    provider_config
        .model_capabilities
        .insert(model.clone(), capabilities);
    let provider: Arc<dyn keencode_model::ModelProvider> = Arc::new(
        ProviderClient::new(provider_config.clone())
            .context("创建真实 Provider Client 失败")?
            .with_request_observer(progress.http.clone()),
    );

    progress.stage = "session.create";
    let storage_root = prepare_runtime_storage(evidence_dir)?;
    progress.runtime_storage_prepared = true;
    let session_id = "live-context-compression";
    let session = RuntimeSession::create_session(
        RuntimeConfig::new(storage_root.clone()),
        CreateSessionRequest {
            session_id: session_id.to_owned(),
            title: "Live context compression test".to_owned(),
            project_root: "synthetic-project-root".to_owned(),
        },
    )
    .context("创建真实测试 Runtime Session 失败")?;

    let tool_calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(SyntheticObservationTool {
            calls: tool_calls.clone(),
        }))
        .context("注册合成只读工具失败")?;
    let context = live_context_manager(provider.clone())?;
    let runner = AgentRunner::new(provider.clone(), tools, RunLimits::default())
        .with_context_manager(context);
    let bound = session.bind_agent_runner(runner);
    let mut observations = Vec::with_capacity(LIVE_SYNTHETIC_NOISE_ROUNDS + 2);

    progress.stage = "turn.initial";
    let first_turn_id = "live-facts";
    let initial_prompt = initial_fact_prompt();
    let first = bound
        .run_turn(runtime_turn(
            &session,
            first_turn_id,
            &model,
            initial_prompt.clone(),
        )?)
        .await
        .context("真实 Provider 首轮执行失败")?;
    observations.push(observe_turn(first_turn_id, &first)?);
    progress.completed_turns += 1;
    let initial_transcript =
        session.model_transcript_for_agent(&keencode_resources::AgentId::new("root")?)?;
    if !initial_transcript.iter().any(|message| {
        message.role == MessageRole::User
            && message.content.iter().any(
                |block| matches!(block, ContentBlock::Text { text } if text == &initial_prompt),
            )
    }) {
        bail!("权威首轮 Transcript 没有保存待验收的事实输入");
    }
    drop(initial_transcript);

    for round in 0..LIVE_SYNTHETIC_NOISE_ROUNDS {
        progress.stage = "turn.synthetic_noise";
        let turn_id = format!("live-noise-{round:02}");
        let result = bound
            .run_turn(runtime_turn(
                &session,
                &turn_id,
                &model,
                synthetic_noise_prompt(round),
            )?)
            .await
            .with_context(|| format!("真实 Provider 合成噪声 Turn 执行失败：{round}"))?;
        observations.push(observe_turn(&turn_id, &result)?);
        progress.completed_turns += 1;
    }

    progress.stage = "journal.validate_before_reopen";
    let records_before_cold_open = read_all_events(&session)?;
    let events_before_cold_open = flattened_events(&records_before_cold_open);
    let compactions_before_cold_open = collect_compaction_reports(&events_before_cold_open)?;
    if compactions_before_cold_open
        .iter()
        .filter(|report| report.trigger == "budget")
        .count()
        < 2
    {
        bail!("真实长会话没有形成至少两次预算触发的已提交自动压缩");
    }
    let tool_lifecycle_before_cold_open =
        collect_tool_lifecycle_report(&events_before_cold_open, tool_calls.load(Ordering::SeqCst));
    if tool_lifecycle_before_cold_open.synthetic_executions == 0
        || !tool_lifecycle_before_cold_open.request_ids_paired
    {
        bail!("真实合成工具调用没有形成完整请求/完成配对");
    }
    let pre_cold_snapshot = session
        .snapshot()
        .context("读取冷打开前 Runtime Snapshot 失败")?;
    if pre_cold_snapshot.recovery_required || pre_cold_snapshot.state.status != SessionStatus::Idle
    {
        bail!("冷打开前 Runtime 没有处于健康 Idle 状态");
    }

    let root_agent =
        keencode_resources::AgentId::new("root").context("创建冷恢复根 Agent 标识失败")?;
    let before_cold_transcript = session
        .model_transcript_for_agent(&root_agent)
        .context("读取冷打开前有效 Transcript 失败")?;
    let before_cold_digest = sha256_hex(&serde_json::to_vec(&before_cold_transcript)?);
    let before_cold_revision = pre_cold_snapshot.state.transcript_revision;
    drop(before_cold_transcript);
    drop(bound);
    drop(session);
    drop(provider);

    progress.stage = "session.cold_reopen";
    let reopened =
        match RuntimeSession::open_session(RuntimeConfig::new(storage_root.clone()), session_id)
            .context("冷打开真实测试 Session 失败")?
        {
            OpenSessionResult::Ready(session) => session,
            OpenSessionResult::Corrupt(_) => bail!("真实测试 Session 冷打开返回损坏报告"),
        };
    let cold_snapshot = reopened.snapshot().context("读取冷打开 Snapshot 失败")?;
    if cold_snapshot.recovery_required || cold_snapshot.state.status != SessionStatus::Idle {
        bail!("冷恢复后 Runtime 没有处于健康 Idle 状态");
    }
    if cold_snapshot
        .state
        .turns
        .values()
        .any(|turn| turn.status != TurnStatus::Completed)
    {
        bail!("冷恢复后存在未完成的合成 Turn");
    }

    let effective_after_cold_open = reopened
        .model_transcript_for_agent(&root_agent)
        .context("读取冷恢复后的有效 Transcript 失败")?;
    if effective_after_cold_open.is_empty() {
        bail!("冷恢复后的有效 Transcript 为空");
    }
    if cold_snapshot.state.transcript_revision != before_cold_revision
        || sha256_hex(&serde_json::to_vec(&effective_after_cold_open)?) != before_cold_digest
    {
        bail!("冷恢复前后有效 Transcript 或修订号不一致");
    }

    progress.stage = "turn.cold_recall";
    let cold_provider: Arc<dyn keencode_model::ModelProvider> = Arc::new(
        ProviderClient::new(provider_config)
            .context("重新创建冷恢复 Provider Client 失败")?
            .with_request_observer(progress.http.clone()),
    );
    let cold_context = live_context_manager(cold_provider.clone())?;
    let cold_runner = AgentRunner::new(cold_provider, ToolRegistry::new(), RunLimits::default())
        .with_context_manager(cold_context);
    let cold_bound = reopened.bind_agent_runner(cold_runner);
    let cold_turn_id = "live-cold-recall";
    let cold_result = cold_bound
        .run_turn(runtime_turn(
            &reopened,
            cold_turn_id,
            &model,
            cold_recall_prompt().to_owned(),
        )?)
        .await
        .context("真实 Provider 冷恢复回忆 Turn 执行失败")?;
    observations.push(observe_turn(cold_turn_id, &cold_result)?);
    progress.completed_turns += 1;
    let final_response = cold_result
        .final_response
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("冷恢复回忆 Turn 缺少最终模型响应"))?;
    let facts = fact_retention(&response_text(final_response));
    if !facts.all_retained() {
        bail!("冷恢复后的真实模型回答没有保留全部字段和值");
    }
    drop(cold_bound);

    progress.stage = "journal.validate_after_reopen";
    let records_after_cold_open = read_all_events(&reopened)?;
    let events_after_cold_open = flattened_events(&records_after_cold_open);
    let compactions = collect_compaction_reports(&events_after_cold_open)?;
    if compactions.len() < compactions_before_cold_open.len() {
        bail!("冷恢复后 Journal 压缩记录数量减少");
    }
    progress.stage = "tool.validate_after_reopen";
    let tool_lifecycle =
        collect_tool_lifecycle_report(&events_after_cold_open, tool_calls.load(Ordering::SeqCst));
    if !tool_lifecycle.request_ids_paired || tool_lifecycle.synthetic_executions == 0 {
        bail!("冷恢复后工具生命周期配对或执行统计不完整");
    }
    // 全部权威结果已读入内存；先关闭 Runtime 释放 Windows 文件锁，再逐文件检查落盘证据。
    drop(reopened);
    progress.stage = "evidence.validate_persistence";
    validate_persisted_runtime_evidence(&storage_root, configured_secret)
        .context("合成 Runtime 证据脱敏检查失败")?;

    let turns = observations
        .into_iter()
        .map(|observation| {
            let (input_tokens, output_tokens) =
                turn_usage(&events_after_cold_open, &observation.turn_id);
            LiveTurnReport {
                turn_id: observation.turn_id,
                success: observation.success,
                rounds: observation.rounds,
                steps: observation.steps,
                compactions: observation.compactions,
                input_tokens,
                output_tokens,
            }
        })
        .collect();
    progress.stage = "http.validate";
    let http_observations = progress.http.snapshot()?;
    if http_observations
        .iter()
        .any(|event| event.protocol != protocol)
        || http_observations
            .iter()
            .filter(|event| {
                event.scope == "attempt"
                    && event.state == "completed"
                    && event
                        .http_status
                        .is_some_and(|status| (200..300).contains(&status))
            })
            .count()
            < LIVE_SYNTHETIC_NOISE_ROUNDS + 2
    {
        bail!("真实 HTTP 观测不能证明选定协议完成了全部模型调用");
    }
    Ok(LiveContextReport {
        schema_version: LIVE_REPORT_SCHEMA,
        status: "passed",
        protocol,
        model,
        source_version,
        turns,
        compactions,
        tool_lifecycle,
        facts,
        journal_events: records_after_cold_open.len(),
        runtime_storage_dir: LIVE_RUNTIME_DIR_NAME,
        test_file_sha256: sha256_hex(include_bytes!("live_context_tests.rs")),
        http_observations,
        initial_facts_sha256: sha256_hex(initial_prompt.as_bytes()),
        cold_transcript_sha256: before_cold_digest,
        cold_transcript_revision: before_cold_revision,
    })
}

/// 检查持久化的合成 Journal/Artifact 内容不包含真实凭据或认证线格式。
fn validate_persisted_runtime_evidence(root: &Path, secret: Option<&str>) -> Result<()> {
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        let entries = fs::read_dir(&directory).with_context(|| "读取合成 Runtime 证据目录失败")?;
        for entry in entries {
            let entry = entry.with_context(|| "读取合成 Runtime 证据项失败")?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).with_context(|| "读取合成 Runtime 证据项元数据失败")?;
            if metadata.file_type().is_symlink() {
                bail!("合成 Runtime 证据目录不能包含符号链接");
            }
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            if !metadata.is_file() {
                bail!("合成 Runtime 证据目录只能包含普通文件和目录");
            }
            let bytes = fs::read(&path).with_context(|| "读取合成 Runtime 证据文件失败")?;
            let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            if [
                "authorization:",
                "x-api-key:",
                "bearer ",
                "api_key=",
                "api-key=",
            ]
            .iter()
            .any(|pattern| text.contains(pattern))
            {
                bail!("合成 Runtime 证据包含认证文本");
            }
            if let Some(secret) = secret
                && !secret.is_empty()
                && String::from_utf8_lossy(&bytes).contains(secret)
            {
                bail!("合成 Runtime 证据包含 Provider 凭据");
            }
        }
    }
    Ok(())
}

/// 普通本地测试只验证报告写入和敏感字段/路径拒绝，不访问网络。
#[test]
fn redacted_report_writer_rejects_sensitive_fields_and_paths() {
    let directory = tempfile::tempdir().expect("创建报告测试目录失败");
    let safe = serde_json::json!({
        "schemaVersion": LIVE_REPORT_SCHEMA,
        "protocol": "responses",
        "model": "synthetic-model",
        "facts": {"goal": true}
    });
    let destination = write_redacted_report(directory.path(), &safe).expect("安全报告应写入");
    let original = fs::read(&destination).expect("应读取安全报告");

    let sensitive = serde_json::json!({"apiKey": "synthetic-secret"});
    let error = write_redacted_report(directory.path(), &sensitive)
        .expect_err("包含 API Key 字段的报告必须被拒绝");
    assert!(error.to_string().contains("禁止字段"));
    assert_eq!(
        fs::read(&destination).expect("拒绝后报告仍应存在"),
        original
    );

    let absolute_path = serde_json::json!({"sourceVersion": "C:\\Users\\private\\project"});
    let error = write_redacted_report(directory.path(), &absolute_path)
        .expect_err("包含绝对路径的报告必须被拒绝");
    assert!(error.to_string().contains("绝对路径"));
    assert_eq!(
        fs::read(&destination).expect("拒绝后报告仍应存在"),
        original
    );
}

/// 协议切换只能改变内存中的测试 Fixture，不得修改用户配置或凭据字段。
#[test]
fn provider_fixture_protocol_conversion_is_explicit_and_non_mutating() {
    let original = CustomProvider {
        id: "gateway".to_owned(),
        models: vec!["test-model".to_owned()],
        base_url: "https://provider.example/v1/responses".to_owned(),
        name: "Gateway".to_owned(),
        api_backend: "responses".to_owned(),
        api_key: Some("synthetic-key".to_owned()),
        context_windows: Default::default(),
        context_1m: Default::default(),
        supports_vision: Default::default(),
    };
    let converted =
        provider_fixture_for_protocol(&original, "messages").expect("应构造 Messages 内存 Fixture");
    assert_eq!(converted.api_backend, "messages");
    assert_eq!(converted.base_url, "https://provider.example/v1/messages");
    assert_eq!(converted.api_key, original.api_key);
    assert_eq!(original.api_backend, "responses");
    assert_eq!(original.base_url, "https://provider.example/v1/responses");
}

/// 事实校验必须要求字段和值成对出现，孤立值或孤立字段均不能通过。
#[test]
fn fact_retention_requires_field_value_pairs() {
    let complete = "目标=给atlas新增重试；约束=最多3次且不加依赖；文件=src/retry.rs；失败原因=429；Goal=完成可靠重试；Todo=补退避测试；子任务=worker-a运行中等待结果；下一步=收到结果后跑测试";
    assert!(fact_retention(complete).all_retained());
    assert!(!fact_retention("给atlas新增重试 429 src/retry.rs").all_retained());
}

/// 真实 Provider 测试必须显式启用，普通 cargo test 不得联网。
#[tokio::test]
#[ignore = "需要显式 Provider 配置和真实远端环境变量；普通测试不联网"]
async fn live_context_compression_and_cold_recovery() {
    if run_live_context_test().await.is_err() {
        panic!("真实上下文压缩测试失败；请读取证据目录中的阶段报告");
    }
}

/// 构造包含所有当前自由文本观测字段的合成请求，供脱敏边界测试使用。
fn request_observation_with_secret_text(marker: &str) -> RequestObservation {
    RequestObservation {
        scope: RequestObservationScope::Logical,
        state: RequestObservationState::Failed,
        logical_request_id: format!("{marker}-logical-request-id"),
        attempt: 1,
        max_attempts: 3,
        model: format!("{marker}-model"),
        protocol: keencode_model::ProviderProtocol::Responses,
        mode: RequestMode::Buffered,
        endpoint: format!("https://{marker}.example.test/v1/responses"),
        at_ms: 1_700_000_000_000,
        duration_ms: Some(42),
        response_headers_at_ms: Some(1_700_000_000_010),
        http_status: Some(503),
        provider_request_id: Some(format!("{marker}-provider-request-id")),
        usage: keencode_model::TokenUsage::unknown(),
        error_kind: Some(RequestErrorKind::HttpStatus),
        error_summary: Some(format!("{marker}-error-summary")),
        session_id: Some(format!("{marker}-session-id")),
        turn_id: Some(format!("{marker}-turn-id")),
        agent_id: Some(format!("{marker}-agent-id")),
        purpose: Some(format!("{marker}-purpose")),
    }
}

/// 快照只允许写出固定字段，并且不得泄漏观测中的任何自由文本。
#[test]
fn live_http_observer_snapshot_redacts_free_text_and_keeps_fixed_fields() {
    let marker = "SECRET-LIKE-OBSERVATION-TEXT";
    let observer = LiveHttpObserver::default();

    observer.on_request(request_observation_with_secret_text(marker));

    let snapshot = observer.snapshot().expect("单条脱敏观测应能生成快照");
    let snapshot_json = serde_json::to_value(&snapshot).expect("脱敏快照应能编码为 JSON");
    assert_eq!(
        snapshot_json,
        serde_json::json!([{
            "scope": "logical",
            "state": "failed",
            "protocol": "responses",
            "mode": "buffered",
            "httpStatus": 503,
            "errorKind": "http_status"
        }])
    );
    assert!(
        !snapshot_json.to_string().contains(marker),
        "快照不得包含请求观测中的敏感自由文本"
    );
}

/// 快照容量恰好达到一千条时成功，追加第一千零一条后必须报不完整错误。
#[test]
fn live_http_observer_snapshot_rejects_the_1001st_observation() {
    let observer = LiveHttpObserver::default();

    for index in 0..1_000 {
        observer.on_request(request_observation_with_secret_text(&format!(
            "SECRET-LIKE-{index}"
        )));
    }

    let snapshot = observer.snapshot().expect("恰好一千条观测时快照应成功");
    assert_eq!(snapshot.len(), 1_000);
    assert_eq!(
        observer
            .observations
            .lock()
            .expect("容量边界检查时观测锁应可读取")
            .len(),
        1_000
    );

    observer.on_request(request_observation_with_secret_text("SECRET-LIKE-1000"));

    assert_eq!(
        observer
            .observations
            .lock()
            .expect("超限后观测锁仍应可读取")
            .len(),
        1_000,
        "第一千零一条观测不得增长有界队列"
    );
    let error = observer
        .snapshot()
        .err()
        .expect("第一千零一条观测必须使快照明确失败");
    assert_eq!(error.to_string(), "真实 HTTP 观测不完整");
}

/// 观测锁已中毒时投递不得 panic，快照必须明确失败而不是伪造成功。
#[test]
fn live_http_observer_poisoned_mutex_does_not_panic_and_snapshot_fails() {
    let observer = LiveHttpObserver::default();
    let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = observer.observations.lock().expect("中毒前观测锁应能获取");
        panic!("仅用于制造测试锁中毒");
    }));
    assert!(poison_result.is_err(), "测试必须先制造 Mutex 中毒状态");
    assert!(observer.observations.is_poisoned());

    let on_request_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer.on_request(request_observation_with_secret_text(
            "SECRET-LIKE-POISONED-MUTEX",
        ));
    }));
    assert!(
        on_request_result.is_ok(),
        "Mutex 中毒时 on_request 不得向生产请求传播 panic"
    );

    let error = observer
        .snapshot()
        .err()
        .expect("Mutex 中毒时 snapshot 必须明确失败");
    assert_eq!(error.to_string(), "真实 HTTP 观测不完整");
}

/// Windows 的独占 Runtime 文件锁必须先释放，离线证据扫描才能读取全部普通文件。
#[cfg(windows)]
#[test]
fn persisted_runtime_evidence_scan_requires_released_session_locks() {
    let directory = tempfile::tempdir().expect("创建持久证据测试目录失败");
    let session = RuntimeSession::create_session(
        RuntimeConfig::new(directory.path().to_owned()),
        CreateSessionRequest {
            session_id: "evidence-lock-test".to_owned(),
            title: "持久证据锁测试".to_owned(),
            project_root: "synthetic-project-root".to_owned(),
        },
    )
    .expect("创建持有独占锁的测试 Session 失败");
    let error = validate_persisted_runtime_evidence(directory.path(), None)
        .expect_err("持有 Windows 独占文件锁时扫描不能成功");
    assert!(
        error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.raw_os_error() == Some(33))
        }),
        "必须确认实际 Windows 锁冲突，而不是用无关失败验证关闭顺序"
    );
    drop(session);
    validate_persisted_runtime_evidence(directory.path(), None)
        .expect("释放 Session 锁后完整证据扫描应成功");
}
