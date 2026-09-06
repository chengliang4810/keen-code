use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use keencode_model::{
    ContentBlock, MessageRole, ModelRequest, ModelResponse, ProviderProtocol, StopReason,
    TokenUsage,
};
use keencode_provider::{WireExchange, encode_wire_request};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::{
    ProviderEntry, RuntimeOptions, all_protocols, escape_untrusted_inline_text, hex_digest,
    hex_encode, is_dangerous_display_character, protocol_name, response_mode_name,
    validate_inline_value,
};
use crate::wire_shape::{WireResponseShapeEvidence, inspect_wire_response_shape};
/// 单一用户 KeenCode 数据目录中的固定匿名全局锁文件名。
const LIVE_TEST_PROCESS_LOCK_FILE: &str = "keencode-provider-live-test.global.lock";
/// 当前真实测试恢复清单的唯一受支持结构版本。
const RESUME_SCHEMA_VERSION: &str = "6";
/// 只允许作为精确补测来源读取的上一版恢复清单结构。
const RETRY_SOURCE_RESUME_SCHEMA_VERSION: &str = "5";
/// 能力、门禁、Fixture 与错误语义共同组成的 Harness 契约身份。
const HARNESS_CONTRACT_ID: &str = "keencode-provider-live-contract-v16";
/// 只允许作为精确补测来源读取的上一版 Harness 契约。
const RETRY_SOURCE_HARNESS_CONTRACT_ID: &str = "keencode-provider-live-contract-v14";
/// 当前线级 Fixture Envelope 的唯一受支持结构版本。
const FIXTURE_SCHEMA_VERSION: &str = "6";
/// 当前最终结果报告的唯一受支持结构版本。
const RUN_REPORT_SCHEMA_VERSION: &str = "10";
/// 只允许作为精确补测基础事实读取的上一版最终报告结构。
const RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION: &str = "9";
/// 当前提交日志封装的唯一受支持结构版本。
const JOURNAL_SCHEMA_VERSION: &str = "4";
/// 当前恢复状态证明和完成态产物封印共同使用的结构版本。
const FACT_AUTHENTICATION_SCHEMA_VERSION: &str = "1";
/// 普通运行替代补测选择摘要参与 Journal MAC 的固定域。
const ORDINARY_JOURNAL_SELECTION_DOMAIN: &str = "ordinary-run-v1";
/// 首条 Journal 记录之前的固定链头，禁止把其他链的中间记录移作首条。
const JOURNAL_INITIAL_MAC: &str =
    "hmac-sha256:0000000000000000000000000000000000000000000000000000000000000000";
/// 隔离恢复来源声明的唯一结构版本。
const RECOVERY_LINEAGE_SCHEMA_VERSION: &str = "1";
/// 同契约构建之间只替换可执行文件身份的标准隔离恢复策略。
const DIRECT_RECOVERY_POLICY: &str = "only_source_executable_sha256_may_differ_v1";
/// 从已明确接受的 v14 未完成运行升级，并只重跑无法按当前契约复核记录的策略。
const LEGACY_RECOVERY_POLICY: &str = "legacy_v14_contract_upgrade_reruns_unreplayable_records_v1";
/// v14 取消探测在前序重试只留下在线传输终态时使用的固定不可复核原因。
const LEGACY_UNREPLAYABLE_CANCELLATION_REASON: &str =
    "线级交换只记录了在线传输终态，磁盘无法独立重放该外部失败";
/// v14 取消探测的全部尝试均未收到 HTTP 响应头时使用的固定不可复核原因。
const LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON: &str = "线级交换没有收到 HTTP 响应头";
/// 线级响应超过捕获预算时使用的固定不可复核原因。
const TRUNCATED_RESPONSE_REPLAY_REASON: &str = "线级响应超过 Fixture 捕获上限，正文已截断";
/// 线级响应不是可安全保存的 UTF-8 时使用的固定不可复核原因。
const INVALID_UTF8_RESPONSE_REPLAY_REASON: &str = "线级响应不是可持久化并重放的 UTF-8 JSON 或 SSE";
/// 隔离升级明确排除旧取消记录并在新运行重新发送该 tuple 的固定原因。
const LEGACY_CANCELLATION_RERUN_REASON: &str =
    "旧取消探测的最终本地释放成功，但前序传输失败无法从磁盘独立重放；当前运行必须重新验证该 tuple";
/// 精确补测选择清单与来源摘要的唯一结构版本。
const RETRY_SELECTION_SCHEMA_VERSION: &str = "2";
/// 精确补测事实合并产物的唯一结构版本。
const CONSOLIDATED_REPORT_SCHEMA_VERSION: &str = "2";
/// 当前 Provider HMAC、Journal 链和完成态封印共同提供的来源认证等级。
const AUTHENTICATED_SOURCE_LEVEL: &str = "provider_hmac_v1";
/// 用户显式接受但无法追溯验证的上一版基础来源等级。
const LEGACY_UNAUTHENTICATED_SOURCE_LEVEL: &str = "legacy_unauthenticated_opt_in";
/// 只选择截止序号内可重试、限流或服务端失败事实的固定策略。
const RETRY_SELECTION_POLICY: &str =
    "failed_retryable_or_rate_limit_or_server_error_through_sequence_v1";
/// 恢复副本完全验证前保留的失败关闭标记；存在时禁止常规恢复或作为新来源。
const RECOVERY_INCOMPLETE_MARKER_FILE: &str = ".keencode-recovery-incomplete";
/// 当前脱敏扫描报告的唯一受支持结构版本。
const REDACTION_REPORT_SCHEMA_VERSION: &str = "6";
/// JSON 字符串内嵌 JSON、JSONL 或 SSE 的敏感内容递归扫描深度上限。
const MAX_EMBEDDED_PATH_SCAN_DEPTH: usize = 4;
/// 正文不能在不改变语义的前提下安全持久化时使用的固定原因。
const UNAVAILABLE_RESPONSE_BODY_REASON: &str = "线级响应未通过安全持久化门禁，正文已省略";
/// 首个请求后的请求可能携带模型输出，因此统一省略的固定原因。
const OMITTED_SUBSEQUENT_REQUEST_REASON: &str =
    "后续线级请求可能携带远端派生历史，语义请求与协议请求正文已省略";
/// 在线 Adapter 解析使用的最大单事件预算，恢复时仅复核其边界值。
const MAX_FIXTURE_EVENT_BYTES: usize = 16 * 1024 * 1024;
/// 单个恢复清单允许占用的最大字节数。
const MAX_RESUME_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;
/// 提交日志允许占用的最大总字节数。
const MAX_PROGRESS_JOURNAL_BYTES: u64 = 256 * 1024 * 1024;
/// 提交日志单条完整记录或崩溃尾部允许占用的最大字节数。
const MAX_PROGRESS_JOURNAL_LINE_BYTES: usize = 2 * 1024 * 1024;
/// 单个提交日志允许保存的最大记录数。
const MAX_PROGRESS_JOURNAL_RECORDS: usize = 65_536;
/// 单个 Fixture 文件允许占用的最大字节数。
const MAX_FIXTURE_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// 不可变 Fixture 在同目录完成同步前使用的保留临时文件名前缀。
const FIXTURE_STAGING_PREFIX: &str = ".keencode-fixture-stage-";
/// 一个恢复目录允许包含的最大直属 Fixture 文件数。
const MAX_FIXTURE_FILE_COUNT: usize = 65_536;
/// Fixture 总预算同时约束大量模型、协议模式和能力产生的结构证据。
const MAX_FIXTURE_TOTAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// 全目录扫描允许读取的最大单文件字节数。
const MAX_ARTIFACT_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// 全目录扫描允许枚举的最大文件与目录项总数。
const MAX_ARTIFACT_ENTRY_COUNT: usize = 131_072;
/// 最终报告、日志和 Fixture 同时存在，按 Fixture 总预算的两倍限制全目录磁盘输入。
const MAX_ARTIFACT_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Windows 原子替换遇到短暂扫描器占用时允许的固定重试退避。
const WINDOWS_REPLACE_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(2),
    Duration::from_millis(5),
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
];
/// 所有认证令牌、客户端秘密和签名参数使用的字段同义词。
const AUTHENTICATION_FIELD_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "apikey",
    "api_key",
    "x-auth-token",
    "access-token",
    "access_token",
    "refresh-token",
    "refresh_token",
    "id-token",
    "id_token",
    "auth-token",
    "auth_token",
    "bearer-token",
    "bearer_token",
    "client-secret",
    "client_secret",
    "client-key",
    "client_key",
    "session-token",
    "session_token",
    "password",
    "passwd",
    "credential",
    "signature",
    "x-amz-signature",
    "x-goog-signature",
];
/// Cookie Header 与 JSON 字段使用的字段同义词。
const COOKIE_FIELD_NAMES: &[&str] = &["cookie", "set-cookie"];
/// Windows 文件属性中表示符号链接、目录联接等重解析点的固定标志位。
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// Windows 目录固定句柄允许其他句柄共享读取的标准标志位。
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;
/// Windows 目录固定句柄允许应用继续在已固定目录内提交产物。
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// Windows 允许以文件句柄打开目录的固定标志位。
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// Windows 打开最终重解析点本身而不跟随其目标的固定标志位。
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// 一次真实兼容性运行的唯一结构化事实源。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunReport {
    /// 报告结构版本。
    pub(crate) schema_version: &'static str,
    /// 当前运行的不可变元数据。
    pub(crate) run: RunMetadata,
    /// 不含凭据的 Provider 快照。
    pub(crate) providers: Vec<ProviderRecord>,
    /// 每个 Provider 的实时模型目录结果。
    pub(crate) catalogs: Vec<CatalogRecord>,
    /// 每个模型、协议、响应模式和能力的真实探测结果。
    pub(crate) probes: Vec<ProbeRecord>,
    /// 从探测记录确定性生成的汇总。
    pub(crate) summary: SummaryRecord,
}

/// 从已完成目录严格读取但不直接信任其派生汇总的最终报告。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRunReport {
    /// 来源最终报告结构版本。
    schema_version: String,
    /// 来源运行元数据。
    run: RunMetadata,
    /// 来源 Provider 快照，校验时与当前配置重新生成值逐字比较。
    providers: Vec<serde_json::Value>,
    /// 来源模型目录事实。
    catalogs: Vec<CatalogRecord>,
    /// 来源探测事实。
    probes: Vec<ProbeRecord>,
    /// 来源派生汇总，校验时从探测事实重新计算。
    summary: serde_json::Value,
}

/// 离线合并产物对一个不可变来源目录的内容摘要引用。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsolidatedSourceReference {
    /// 来源运行标识。
    run_id: String,
    /// 来源运行记录的 Git 构建标识。
    runtime_commit: String,
    /// 来源事实认证等级，禁止把显式 legacy 信任洗成当前认证结果。
    authentication: String,
    /// 来源恢复清单结构版本。
    resume_schema_version: String,
    /// 来源 Harness 契约标识。
    harness_contract_id: String,
    /// 来源最终报告结构版本。
    report_schema_version: String,
    /// 来源恢复清单完整摘要。
    resume_sha256: String,
    /// 来源提交日志完整摘要。
    journal_sha256: String,
    /// 来源最终结果完整摘要。
    result_sha256: String,
    /// 来源脱敏报告完整摘要。
    redaction_report_sha256: String,
}

/// 一条合并后的有效事实及其原始 tuple 和产物来源。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsolidatedProbeRecord {
    /// `base` 或 `retry`，用于解析记录内相对 Fixture 路径。
    artifact_source: &'static str,
    /// 该 tuple 在基础运行中的原始稳定键。
    source_stable_key: String,
    /// 实际产生当前有效事实的运行标识。
    observation_run_id: String,
    /// 保持原始稳定键、断言、错误和 Fixture 引用不变的完整事实。
    record: ProbeRecord,
}

/// 不改写任一来源、由精确 tuple 替换规则确定性生成的离线合并报告。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsolidatedRunReport {
    /// 离线合并报告结构版本。
    schema_version: &'static str,
    /// 合并完成时间。
    generated_at: String,
    /// 完整矩阵基础运行的不可变内容引用。
    base: ConsolidatedSourceReference,
    /// 精确补测运行的不可变内容引用。
    retry: ConsolidatedSourceReference,
    /// 经过摘要校验的完整精确选择。
    selection: RetrySelectionManifest,
    /// 从当前配置重建并与基础报告核对的 Provider 快照。
    providers: Vec<ProviderRecord>,
    /// 基础运行已经验证的实时模型目录事实。
    catalogs: Vec<CatalogRecord>,
    /// 每个基础 tuple 唯一有效的原始或补测事实。
    probes: Vec<ConsolidatedProbeRecord>,
    /// 从有效事实重新计算的确定性汇总。
    summary: SummaryRecord,
}

impl RunReport {
    /// 创建尚未完成任何网络探测的报告。
    pub(crate) fn new(run: RunMetadata) -> Self {
        Self {
            schema_version: RUN_REPORT_SCHEMA_VERSION,
            run,
            providers: Vec::new(),
            catalogs: Vec::new(),
            probes: Vec::new(),
            summary: SummaryRecord::default(),
        }
    }

    /// 依据当前全部探测记录重新计算汇总，避免增量计数漂移。
    pub(crate) fn refresh_summary(&mut self) {
        self.summary = SummaryRecord::from_probes(&self.probes);
    }

    /// 验证最终结果中的当前恢复构建与每条导入记录的来源构建严格成对绑定。
    fn validate_recovery_lineage(&self, current_executable_sha256: &str) -> Result<(), String> {
        validate_recovery_binding(&self.run, current_executable_sha256, self.probes.iter())
    }
}

/// 一次测试运行的环境与策略快照。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RunMetadata {
    /// 当前运行的唯一标识。
    pub(crate) run_id: String,
    /// 运行开始的 UTC RFC3339 时间。
    pub(crate) started_at: String,
    /// 运行结束的 UTC RFC3339 时间；未完成时为空。
    pub(crate) finished_at: Option<String>,
    /// 构建时可获得的 Git 提交标识。
    pub(crate) runtime_commit: String,
    /// 当前 Adapter crate 版本。
    pub(crate) adapter_version: String,
    /// 当前操作系统名称。
    pub(crate) os: String,
    /// 当前 CPU 架构名称。
    pub(crate) arch: String,
    /// 单个用例的最大尝试次数。
    pub(crate) max_attempts_per_case: usize,
    /// 单次请求总超时秒数。
    pub(crate) request_timeout_secs: u64,
    /// 当前实现使用的全局并发度。
    pub(crate) global_concurrency: usize,
    /// 当前运行实际选择的能力名称。
    pub(crate) capabilities: Vec<String>,
    /// 是否请求了无模型过滤的完整能力矩阵。
    pub(crate) full_matrix: bool,
    /// 是否只运行 Provider 级负向诊断。
    pub(crate) diagnostics_only: bool,
    /// 基础能力门禁的稳定策略版本。
    pub(crate) base_gate_policy: String,
    /// 从遗失原可执行文件的运行隔离恢复时保存的完整来源身份。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recovery_lineage: Option<RecoveryLineage>,
    /// 精确补测运行绑定的来源与选择摘要；普通运行为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retry_lineage: Option<RetryLineage>,
}

impl RunMetadata {
    /// 从运行参数创建不包含本机路径和凭据的快照。
    pub(crate) fn new(run_id: String, options: &RuntimeOptions) -> Result<Self, String> {
        Ok(Self {
            run_id,
            started_at: timestamp()?,
            finished_at: None,
            runtime_commit: option_env!("KEENCODE_GIT_COMMIT")
                .unwrap_or("working-tree")
                .to_owned(),
            adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            max_attempts_per_case: options.max_attempts,
            request_timeout_secs: options.request_timeout_secs,
            global_concurrency: 1,
            capabilities: options
                .capabilities
                .iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
            full_matrix: options.full_matrix,
            diagnostics_only: options.diagnostics_only,
            base_gate_policy: "text_per_model_protocol_response_mode_v1".to_owned(),
            recovery_lineage: None,
            retry_lineage: None,
        })
    }
}

/// 隔离恢复副本对原始运行与当前恢复构建的不可变审计绑定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecoveryLineage {
    /// 恢复来源声明结构版本。
    pub(crate) schema_version: String,
    /// 原始运行的稳定标识。
    pub(crate) source_run_id: String,
    /// 原始运行记录的已提交 Git 构建标识。
    pub(crate) source_runtime_commit: String,
    /// 原始运行恢复身份记录且用户显式确认的可执行文件摘要。
    pub(crate) source_executable_sha256: String,
    /// 专用恢复开始前原始 `resume.json` 完整字节摘要。
    pub(crate) source_resume_sha256: String,
    /// 专用恢复开始前已严格解析的完整提交日志摘要。
    pub(crate) source_journal_sha256: String,
    /// 来源恢复清单结构版本；旧版 Lineage 未记录时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_resume_schema_version: Option<String>,
    /// 来源 Harness 契约标识；旧版 Lineage 未记录时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_harness_contract_id: Option<String>,
    /// 创建隔离副本的当前可执行文件摘要。
    pub(crate) recovery_executable_sha256: String,
    /// 创建隔离副本的 UTC 时间。
    pub(crate) recovered_at: String,
    /// 从来源运行导入且不得再次请求的已确认记录数。
    pub(crate) imported_records: usize,
    /// 从来源运行逐字节复制并重新验证的 Fixture 数量。
    pub(crate) imported_fixtures: usize,
    /// 来源运行本身已有的恢复来源链；为空表示来源是第一代运行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<Box<RecoveryLineage>>,
    /// 未作为已确认事实导入、必须由当前运行重新请求的旧记录。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) rerun_records: Vec<RecoveryRerunRecord>,
    /// 明确限定只豁免来源可执行文件字节身份的固定恢复策略。
    pub(crate) policy: String,
}

/// 隔离升级中被排除并安排重新验证的一条旧记录身份。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecoveryRerunRecord {
    /// 来源运行中的完整稳定键。
    pub(crate) source_stable_key: String,
    /// 来源记录唯一引用的内容寻址 Fixture 相对路径。
    pub(crate) source_fixture_path: String,
    /// 来源 Fixture Payload 已保存的内容摘要。
    pub(crate) source_fixture_content_sha256: String,
    /// 来源记录的脱敏 Provider 稳定标识。
    pub(crate) provider_id: String,
    /// 来源记录的脱敏模型标识。
    pub(crate) model: String,
    /// 来源记录使用的厂商协议。
    pub(crate) protocol: String,
    /// 来源记录使用的响应模式。
    pub(crate) response_mode: String,
    /// 来源记录使用的能力名称。
    pub(crate) capability: String,
    /// 为什么旧事实不能导入且必须重新请求的固定原因。
    pub(crate) reason: String,
}

/// 单条复用记录对全局恢复来源声明的明确引用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecoveredProbeOrigin {
    /// 产生该真实请求事实的原始运行标识。
    pub(crate) source_run_id: String,
    /// 产生该真实请求事实的原始 Git 构建标识。
    pub(crate) source_runtime_commit: String,
    /// 产生该真实请求事实的原始可执行文件摘要。
    pub(crate) source_executable_sha256: String,
}

/// 精确补测运行对已完成来源及固定选择集合的不可变审计绑定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RetryLineage {
    /// 精确补测选择结构版本。
    pub(crate) schema_version: String,
    /// 被筛选的已完成来源运行标识。
    pub(crate) source_run_id: String,
    /// 来源运行记录的 Git 构建标识。
    pub(crate) source_runtime_commit: String,
    /// 来源运行记录的可执行文件完整摘要。
    pub(crate) source_executable_sha256: String,
    /// 来源事实认证等级；legacy 基础运行永久保留显式未认证标记。
    pub(crate) source_authentication: String,
    /// 来源恢复清单结构版本。
    pub(crate) source_resume_schema_version: String,
    /// 来源 Harness 契约标识。
    pub(crate) source_harness_contract_id: String,
    /// 来源最终报告结构版本。
    pub(crate) source_report_schema_version: String,
    /// 选择创建时来源恢复清单的完整摘要。
    pub(crate) source_resume_sha256: String,
    /// 选择创建时来源提交日志的完整摘要。
    pub(crate) source_journal_sha256: String,
    /// 选择创建时来源最终事实报告的完整摘要。
    pub(crate) source_result_sha256: String,
    /// 选择创建时来源脱敏报告的完整摘要。
    pub(crate) source_redaction_report_sha256: String,
    /// 只允许补测的 Provider 稳定标识。
    pub(crate) provider_id: String,
    /// 选择允许读取的来源提交日志最大序号。
    pub(crate) through_sequence: u64,
    /// 固定失败筛选策略。
    pub(crate) policy: String,
    /// 精确选择的 tuple 数量。
    pub(crate) selected_records: usize,
    /// 对完整选择负载计算的规范 SHA-256。
    pub(crate) selection_sha256: String,
}

/// 一条来源失败事实对应的稳定、无正文精确补测 tuple。
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RetryCase {
    /// 来源提交日志中的严格连续序号。
    pub(crate) source_sequence: u64,
    /// 来源运行原始探测记录的稳定键。
    pub(crate) source_stable_key: String,
    /// 与来源记录独立核对的稳定 tuple 摘要。
    pub(crate) tuple_key: String,
    /// Provider 稳定标识。
    pub(crate) provider_id: String,
    /// 精确模型标识。
    pub(crate) model: String,
    /// 精确协议稳定名称。
    pub(crate) protocol: String,
    /// 精确响应模式稳定名称。
    pub(crate) response_mode: String,
    /// 精确能力稳定名称。
    pub(crate) capability: String,
}

/// 写入补测目录并参与恢复身份校验的完整选择清单。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RetrySelectionManifest {
    /// 不包含自身摘要字段的固定来源和选择元数据。
    pub(crate) lineage: RetryLineage,
    /// 严格按来源日志序号排列的唯一补测 tuple。
    pub(crate) cases: Vec<RetryCase>,
}

/// 一个 Provider 不含凭据且可参与恢复身份比较的固定快照。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResumeProviderIdentity {
    /// Provider 稳定标识。
    provider_id: String,
    /// 以 Provider 凭据为 Key、对去凭据配置计算的域分离 HMAC-SHA256。
    config_fingerprint: String,
    /// 使用本轮随机盐与真实凭据生成的不可跨运行关联证明。
    credential_proof: String,
}

/// 恢复前必须逐字段完全一致的 Harness、构建、配置和运行参数身份。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResumeIdentity {
    /// 恢复清单结构版本。
    schema_version: String,
    /// 当前能力与证据契约的固定身份。
    harness_contract_id: String,
    /// 当前可执行文件完整字节的 SHA-256。
    executable_sha256: String,
    /// 每轮随机生成且只用于凭据 HMAC 域分离的公开盐。
    run_salt: String,
    /// 编译进当前 Harness 的 crate 版本。
    adapter_version: String,
    /// 按 Provider 标识排序的无秘密配置身份。
    providers: Vec<ResumeProviderIdentity>,
    /// 本轮固定覆盖的三种协议。
    protocols: Vec<String>,
    /// 本轮固定覆盖的两种响应模式。
    response_modes: Vec<String>,
    /// 当前选择且按稳定枚举顺序排列的能力。
    capabilities: Vec<String>,
    /// 每个确定性场景允许的最大逻辑尝试次数。
    max_attempts: usize,
    /// 单次 HTTP 请求总超时秒数。
    request_timeout_secs: u64,
    /// 是否只请求模型目录。
    catalog_only: bool,
    /// 是否只运行 Provider 级负向诊断。
    diagnostics_only: bool,
    /// 是否选择完整能力矩阵。
    full_matrix: bool,
    /// 用户显式选择的 Provider 稳定标识。
    provider_filters: Vec<String>,
    /// 用户显式选择的精确模型标识。
    model_filters: Vec<String>,
    /// 精确补测运行绑定的选择摘要；普通运行为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_selection_sha256: Option<String>,
}

/// 一个 Provider 对同一份 typed Resume 核心生成的独立状态证明。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResumeStateProof {
    /// 状态证明结构版本。
    schema_version: String,
    /// 与恢复身份逐字匹配的 Provider 稳定标识。
    provider_id: String,
    /// 使用该 Provider 凭据计算的 HMAC-SHA256。
    hmac_sha256: String,
}

/// 完成态封印中的一项固定相对路径与原始文件字节摘要。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompletionArtifactDigest {
    /// 运行目录内使用正斜杠的固定相对路径。
    path: String,
    /// 对文件原始字节计算的 SHA-256。
    sha256: String,
}

/// 不包含 `resume.json` 自身的无循环完成态事实产物封印。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompletionArtifactSeal {
    /// 完成态产物封印结构版本。
    schema_version: String,
    /// 封印时完整 Journal 的最终连续序号。
    journal_sequence: u64,
    /// 封印时完整 Journal 的链尾 MAC。
    journal_tail_mac: String,
    /// 按相对路径严格排序且不重复的全部权威事实产物摘要。
    artifacts: Vec<CompletionArtifactDigest>,
}

impl ResumeIdentity {
    /// 从当前可执行文件、运行参数和已选择 Provider 计算严格恢复身份。
    fn current(
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        run_salt: &str,
    ) -> Result<Self, String> {
        Self::current_with_retry_selection(options, providers, run_salt, None)
    }

    /// 计算普通或精确补测运行的完整恢复身份。
    fn current_with_retry_selection(
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        run_salt: &str,
        retry_selection_sha256: Option<String>,
    ) -> Result<Self, String> {
        let retry_selection_sha256_ref = retry_selection_sha256.as_deref();
        let mut provider_identities = providers
            .iter()
            .map(|provider| {
                Ok(ResumeProviderIdentity {
                    provider_id: provider.redact_text(&provider.id),
                    config_fingerprint: provider.fingerprint()?,
                    credential_proof: match retry_selection_sha256_ref {
                        Some(selection_sha256) => {
                            provider.credential_retry_resume_proof(run_salt, selection_sha256)
                        }
                        None => provider.credential_resume_proof(run_salt),
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        provider_identities.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
        Ok(Self {
            schema_version: RESUME_SCHEMA_VERSION.to_owned(),
            harness_contract_id: HARNESS_CONTRACT_ID.to_owned(),
            executable_sha256: current_executable_sha256()?,
            run_salt: run_salt.to_owned(),
            adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
            providers: provider_identities,
            protocols: all_protocols()
                .into_iter()
                .map(|protocol| protocol_name(protocol).to_owned())
                .collect(),
            response_modes: [
                keencode_provider::WireResponseMode::Buffered,
                keencode_provider::WireResponseMode::Streaming,
            ]
            .into_iter()
            .map(|mode| response_mode_name(mode).to_owned())
            .collect(),
            capabilities: options
                .capabilities
                .iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
            max_attempts: options.max_attempts,
            request_timeout_secs: options.request_timeout_secs,
            catalog_only: options.catalog_only,
            diagnostics_only: options.diagnostics_only,
            full_matrix: options.full_matrix,
            provider_filters: options.provider_filters.iter().cloned().collect(),
            model_filters: options.model_filters.iter().cloned().collect(),
            retry_selection_sha256,
        })
    }

    /// 校验恢复清单中的所有加钥证明均使用唯一受支持的规范文本格式。
    fn validate_hmac_proof_formats(&self) -> Result<(), String> {
        if self.providers.iter().any(|provider| {
            !valid_hmac_sha256_proof(&provider.config_fingerprint)
                || !valid_hmac_sha256_proof(&provider.credential_proof)
        }) {
            return Err("恢复身份中的 Provider 配置指纹或凭据证明 HMAC 格式无效".to_owned());
        }
        Ok(())
    }
}

/// 原子恢复清单，同时保存身份、候选集合和每个稳定键的最后完整记录。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResumeManifest {
    /// 必须与当前进程逐字段完全一致的不可变身份。
    identity: ResumeIdentity,
    /// 首次运行创建且恢复时保持不变的运行元数据。
    pub(crate) run: RunMetadata,
    /// 每个 Provider 在首次目录请求后冻结的排序候选模型集合。
    candidate_sets: BTreeMap<String, Vec<String>>,
    /// 每个稳定探测键最后一次原子提交的完整脱敏记录。
    records: BTreeMap<String, ProbeRecord>,
    /// 已经并入当前快照的 JSONL 提交日志最大连续序号。
    journal_sequence: u64,
    /// 当前快照已确认 Journal 前缀的链尾 MAC；显式 legacy v5 来源没有该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    journal_tail_mac: Option<String>,
    /// 全部模型探测和最终产物是否已经验收完成。
    pub(crate) finished: bool,
    /// 精确补测运行冻结的完整选择；普通运行为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_selection: Option<RetrySelectionManifest>,
    /// 每个恢复身份 Provider 对去除本字段后的 typed Manifest 核心生成的状态证明。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    state_proofs: Vec<ResumeStateProof>,
    /// 完成态全部权威事实产物的无循环摘要封印；未完成运行与 legacy 来源为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion_artifact_seal: Option<CompletionArtifactSeal>,
}

/// 明确排除 `stateProofs` 后参与每个 Provider 状态 HMAC 的 typed Manifest 核心。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResumeManifestStateCore<'a> {
    /// 不可变恢复身份。
    identity: &'a ResumeIdentity,
    /// 运行元数据。
    run: &'a RunMetadata,
    /// 已冻结的 Provider 候选集合。
    candidate_sets: &'a BTreeMap<String, Vec<String>>,
    /// 已确认的完整探测事实。
    records: &'a BTreeMap<String, ProbeRecord>,
    /// 已确认 Journal 最大连续序号。
    journal_sequence: u64,
    /// 已确认 Journal 前缀链尾 MAC。
    journal_tail_mac: &'a Option<String>,
    /// 运行是否完整结束。
    finished: bool,
    /// 补测运行冻结的完整选择。
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_selection: &'a Option<RetrySelectionManifest>,
    /// 完成态无循环事实产物封印。
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_artifact_seal: &'a Option<CompletionArtifactSeal>,
}

impl ResumeManifest {
    /// 在首个真实请求前创建只含身份且候选集合为空的新清单。
    pub(crate) fn new(
        run: RunMetadata,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
    ) -> Result<Self, String> {
        let run_salt = new_run_salt()?;
        Ok(Self {
            identity: ResumeIdentity::current(options, providers, &run_salt)?,
            run,
            candidate_sets: BTreeMap::new(),
            records: BTreeMap::new(),
            journal_sequence: 0,
            journal_tail_mac: Some(JOURNAL_INITIAL_MAC.to_owned()),
            finished: false,
            retry_selection: None,
            state_proofs: Vec::new(),
            completion_artifact_seal: None,
        })
    }

    /// 创建只允许执行选择清单中精确 tuple 的可恢复补测清单。
    pub(crate) fn new_retry(
        run: RunMetadata,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        retry_selection: RetrySelectionManifest,
    ) -> Result<Self, String> {
        retry_selection.validate()?;
        let run_salt = new_run_salt()?;
        let identity = ResumeIdentity::current_with_retry_selection(
            options,
            providers,
            &run_salt,
            Some(retry_selection.lineage.selection_sha256.clone()),
        )?;
        Ok(Self {
            identity,
            run,
            candidate_sets: BTreeMap::new(),
            records: BTreeMap::new(),
            journal_sequence: 0,
            journal_tail_mac: Some(JOURNAL_INITIAL_MAC.to_owned()),
            finished: false,
            retry_selection: Some(retry_selection),
            state_proofs: Vec::new(),
            completion_artifact_seal: None,
        })
    }

    /// 返回 Journal MAC 中绑定的补测选择摘要或普通运行固定域。
    fn journal_selection_domain(&self) -> &str {
        self.retry_selection
            .as_ref()
            .map_or(ORDINARY_JOURNAL_SELECTION_DOMAIN, |selection| {
                selection.lineage.selection_sha256.as_str()
            })
    }

    /// 对明确排除状态证明字段的 typed Manifest 核心生成规范 JSON 字节。
    fn canonical_state_core(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&ResumeManifestStateCore {
            identity: &self.identity,
            run: &self.run,
            candidate_sets: &self.candidate_sets,
            records: &self.records,
            journal_sequence: self.journal_sequence,
            journal_tail_mac: &self.journal_tail_mac,
            finished: self.finished,
            retry_selection: &self.retry_selection,
            completion_artifact_seal: &self.completion_artifact_seal,
        })
        .map_err(|error| format!("无法序列化恢复清单状态核心：{error}"))
    }

    /// 使用当前配置中的每个身份 Provider 计算严格排序的状态证明集合。
    fn calculated_state_proofs(
        &self,
        providers: &[&ProviderEntry],
    ) -> Result<Vec<ResumeStateProof>, String> {
        if self.identity.providers.is_empty()
            || self
                .identity
                .providers
                .windows(2)
                .any(|pair| pair[0].provider_id >= pair[1].provider_id)
        {
            return Err("恢复身份 Provider 必须非空、严格排序且不能重复".to_owned());
        }
        let core = self.canonical_state_core()?;
        self.identity
            .providers
            .iter()
            .map(|identity| {
                let provider = resolve_resume_provider(&identity.provider_id, providers)?;
                Ok(ResumeStateProof {
                    schema_version: FACT_AUTHENTICATION_SCHEMA_VERSION.to_owned(),
                    provider_id: identity.provider_id.clone(),
                    hmac_sha256: provider.resume_state_proof(&self.identity.run_salt, &core),
                })
            })
            .collect()
    }

    /// 在读取 Journal 或调和任何事实前校验磁盘 Resume 的每 Provider 状态证明。
    fn validate_persisted_state_proofs(&self, providers: &[&ProviderEntry]) -> Result<(), String> {
        if self.identity.schema_version == RETRY_SOURCE_RESUME_SCHEMA_VERSION {
            if self.identity.harness_contract_id != RETRY_SOURCE_HARNESS_CONTRACT_ID
                || self.journal_tail_mac.is_some()
                || !self.state_proofs.is_empty()
                || self.completion_artifact_seal.is_some()
            {
                return Err("显式 legacy v5 来源不能携带或伪装当前事实认证字段".to_owned());
            }
            return Ok(());
        }
        if self.identity.schema_version != RESUME_SCHEMA_VERSION
            || self.identity.harness_contract_id != HARNESS_CONTRACT_ID
        {
            return Err("当前恢复清单的 Resume 与 Harness 契约版本组合无效".to_owned());
        }
        if self
            .journal_tail_mac
            .as_deref()
            .is_none_or(|value| !valid_hmac_sha256_proof(value))
        {
            return Err("当前恢复清单缺少格式有效的 Journal 链尾 MAC".to_owned());
        }
        if self.state_proofs.iter().any(|proof| {
            proof.schema_version != FACT_AUTHENTICATION_SCHEMA_VERSION
                || !valid_hmac_sha256_proof(&proof.hmac_sha256)
        }) {
            return Err("恢复清单状态证明版本或 HMAC 格式无效".to_owned());
        }
        let expected = self.calculated_state_proofs(providers)?;
        if self.state_proofs != expected {
            return Err("恢复清单 typed 状态核心未通过当前 Provider 凭据认证".to_owned());
        }
        match (self.finished, self.completion_artifact_seal.as_ref()) {
            (true, None) => return Err("当前完成态恢复清单缺少事实产物封印".to_owned()),
            (false, Some(_)) => return Err("未完成恢复清单不能携带完成态事实产物封印".to_owned()),
            _ => {}
        }
        Ok(())
    }

    /// 生成一次写盘专用快照，绑定当前 Journal 链尾并清除上一版状态证明。
    fn persisted_snapshot(&self, journal_tail_mac: &str) -> Result<Self, String> {
        if !valid_hmac_sha256_proof(journal_tail_mac) {
            return Err("待写入恢复清单的 Journal 链尾 MAC 格式无效".to_owned());
        }
        let mut persisted = self.clone();
        persisted.journal_tail_mac = Some(journal_tail_mac.to_owned());
        persisted.state_proofs.clear();
        Ok(persisted)
    }

    /// 拒绝当前构建、配置、协议、模式、能力或运行参数与原清单的任一差异。
    pub(crate) fn validate_identity(
        &self,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        if self.identity.schema_version != RESUME_SCHEMA_VERSION {
            return Err(format!(
                "恢复清单 schema 不受支持：{}",
                self.identity.schema_version
            ));
        }
        self.identity.validate_hmac_proof_formats()?;
        self.validate_retry_selection_binding()?;
        let current = ResumeIdentity::current_with_retry_selection(
            options,
            providers,
            &self.identity.run_salt,
            self.retry_selection
                .as_ref()
                .map(|selection| selection.lineage.selection_sha256.clone()),
        )?;
        if self.identity != current {
            return Err(
                "恢复身份冲突：可执行文件、Harness 契约、Provider 配置、凭据或租户、协议、模式、能力或运行参数已变化"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// 专用隔离恢复只允许明确支持的构建或契约升级，其余身份必须逐字段完全一致。
    pub(crate) fn validate_recovery_source_identity(
        &self,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        expected_source_executable_sha256: &str,
        allow_unauthenticated_legacy: bool,
    ) -> Result<(), String> {
        let current_contract = self.identity.schema_version == RESUME_SCHEMA_VERSION
            && self.identity.harness_contract_id == HARNESS_CONTRACT_ID;
        let legacy_contract = self.identity.schema_version == RETRY_SOURCE_RESUME_SCHEMA_VERSION
            && self.identity.harness_contract_id == RETRY_SOURCE_HARNESS_CONTRACT_ID;
        if !current_contract && !legacy_contract {
            return Err(format!(
                "隔离恢复来源的 Resume 与 Harness 契约组合不受支持：{}/{}",
                self.identity.schema_version, self.identity.harness_contract_id
            ));
        }
        if legacy_contract && !allow_unauthenticated_legacy {
            return Err(
                "v14 未完成来源缺少当前事实认证；必须显式提供 --allow-unauthenticated-legacy-base 才能建立只读隔离升级"
                    .to_owned(),
            );
        }
        self.identity.validate_hmac_proof_formats()?;
        if !valid_sha256_digest(expected_source_executable_sha256) {
            return Err(
                "--expected-source-executable-sha256 必须是 sha256: 加 64 位小写十六进制"
                    .to_owned(),
            );
        }
        if !valid_sha256_digest(&self.identity.executable_sha256) {
            return Err("隔离恢复来源的可执行文件 SHA-256 格式无效".to_owned());
        }
        if self.identity.executable_sha256 != expected_source_executable_sha256 {
            return Err("用户确认的来源可执行文件 SHA-256 与原始 resume.json 不一致".to_owned());
        }
        if self.run.retry_lineage.is_some() || self.retry_selection.is_some() {
            return Err("隔离恢复不允许从精确补测运行建立恢复链".to_owned());
        }
        if current_contract {
            // 当前契约的派生来源必须重新通过状态 HMAC 与逐代 Lineage 校验，不能只信任
            // 调用方已经加载过的内存对象；这也阻断了把篡改后的派生来源洗白为新来源。
            self.validate_persisted_state_proofs(providers)?;
            self.validate_recovery_lineage()?;
        }
        let mut current = ResumeIdentity::current(options, providers, &self.identity.run_salt)?;
        let current_executable_sha256 = current.executable_sha256.clone();
        current.schema_version = self.identity.schema_version.clone();
        current.harness_contract_id = self.identity.harness_contract_id.clone();
        current.executable_sha256 = self.identity.executable_sha256.clone();
        if legacy_contract {
            for identity in &mut current.providers {
                let provider = resolve_resume_provider(&identity.provider_id, providers)?;
                identity.credential_proof =
                    provider.legacy_credential_resume_proof(&self.identity.run_salt);
            }
        }
        if self.identity != current {
            return Err(
                "隔离恢复身份冲突：除受支持的 Harness 升级和已显式确认的来源可执行文件外，Provider 配置、凭据或租户、协议、模式、能力或运行参数已变化"
                    .to_owned(),
            );
        }
        if current_executable_sha256 == self.identity.executable_sha256 {
            return Err("来源可执行文件仍与当前构建一致；请使用常规 --resume".to_owned());
        }
        Ok(())
    }

    /// 校验已完成补测来源的结构、构建摘要、当前 Provider 配置和唯一目标身份。
    fn validate_retry_source_identity(
        &self,
        providers: &[&ProviderEntry],
        provider_id: &str,
        expected_source_executable_sha256: &str,
    ) -> Result<(), String> {
        self.retry_source_report_schema()?;
        self.identity.validate_hmac_proof_formats()?;
        if !valid_sha256_digest(expected_source_executable_sha256)
            || self.identity.executable_sha256 != expected_source_executable_sha256
        {
            return Err("精确补测来源可执行文件摘要与用户确认值不一致".to_owned());
        }
        if !self.finished || self.run.finished_at.is_none() {
            return Err("精确补测只能从已经完整结束的运行创建".to_owned());
        }
        if self.retry_selection.is_some() || self.run.retry_lineage.is_some() {
            return Err("精确补测来源不能是另一份精确补测运行".to_owned());
        }
        let mut current_provider_identities = providers
            .iter()
            .map(|provider| {
                let credential_proof =
                    if self.identity.schema_version == RETRY_SOURCE_RESUME_SCHEMA_VERSION {
                        provider.legacy_credential_resume_proof(&self.identity.run_salt)
                    } else {
                        provider.credential_resume_proof(&self.identity.run_salt)
                    };
                Ok(ResumeProviderIdentity {
                    provider_id: provider.redact_text(&provider.id),
                    config_fingerprint: provider.fingerprint()?,
                    credential_proof,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        current_provider_identities.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
        if self.identity.providers != current_provider_identities {
            return Err("精确补测来源的 Provider 配置、凭据或租户身份与当前配置不一致".to_owned());
        }
        if !self
            .identity
            .providers
            .iter()
            .any(|provider| provider.provider_id == provider_id)
            || !self.candidate_sets.contains_key(provider_id)
        {
            return Err("精确补测 Provider 不属于来源运行的冻结候选集合".to_owned());
        }
        Ok(())
    }

    /// 返回与来源 Resume 和 Harness 版本严格成对绑定的最终报告 Schema。
    fn retry_source_report_schema(&self) -> Result<&'static str, String> {
        match (
            self.identity.schema_version.as_str(),
            self.identity.harness_contract_id.as_str(),
        ) {
            (RETRY_SOURCE_RESUME_SCHEMA_VERSION, RETRY_SOURCE_HARNESS_CONTRACT_ID) => {
                Ok(RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION)
            }
            (RESUME_SCHEMA_VERSION, HARNESS_CONTRACT_ID) => Ok(RUN_REPORT_SCHEMA_VERSION),
            _ => Err("精确补测来源的 Resume、Harness 与最终报告版本组合不受支持".to_owned()),
        }
    }

    /// 校验一份已完成补测运行自身的结构、Provider 配置与选择绑定。
    fn validate_completed_retry_identity(
        &self,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        if self.identity.schema_version != RESUME_SCHEMA_VERSION
            || self.identity.harness_contract_id != HARNESS_CONTRACT_ID
            || !self.finished
            || self.run.finished_at.is_none()
        {
            return Err("补测运行 schema、Harness 契约或完成状态无效".to_owned());
        }
        self.identity.validate_hmac_proof_formats()?;
        self.validate_retry_selection_binding()?;
        let selection_sha256 = self
            .retry_selection
            .as_ref()
            .map(|selection| selection.lineage.selection_sha256.as_str())
            .ok_or_else(|| "补测运行缺少精确选择摘要".to_owned())?;
        let mut current_provider_identities = providers
            .iter()
            .filter(|provider| {
                self.identity
                    .providers
                    .iter()
                    .any(|identity| identity.provider_id == provider.redact_text(&provider.id))
            })
            .map(|provider| {
                Ok(ResumeProviderIdentity {
                    provider_id: provider.redact_text(&provider.id),
                    config_fingerprint: provider.fingerprint()?,
                    credential_proof: provider
                        .credential_retry_resume_proof(&self.identity.run_salt, selection_sha256),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        current_provider_identities.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
        if current_provider_identities != self.identity.providers {
            return Err("补测运行的 Provider 配置、凭据或租户身份与当前配置不一致".to_owned());
        }
        Ok(())
    }

    /// 验证恢复副本中每条导入记录都明确绑定唯一来源构建与全局 Lineage。
    fn validate_recovery_lineage(&self) -> Result<(), String> {
        validate_recovery_binding(
            &self.run,
            &self.identity.executable_sha256,
            self.records.values(),
        )?;
        self.validate_retry_selection_binding()
    }

    /// 返回当前运行冻结的精确补测选择；普通运行为空。
    pub(crate) fn retry_selection(&self) -> Option<&RetrySelectionManifest> {
        self.retry_selection.as_ref()
    }

    /// 返回精确补测恢复身份要求的 Provider 标识和能力集合。
    pub(crate) fn retry_runtime_shape(&self) -> Result<Option<(String, BTreeSet<String>)>, String> {
        let Some(selection) = &self.retry_selection else {
            return Ok(None);
        };
        selection.validate()?;
        Ok(Some(selection.runtime_shape()))
    }

    /// 返回已经写入恢复清单的唯一探测记录数量。
    pub(crate) fn record_count(&self) -> usize {
        self.records.len()
    }

    /// 在写入日志前确认探测记录没有越过精确补测清单冻结的 tuple 边界。
    pub(crate) fn validate_probe_scope(&self, probe: &ProbeRecord) -> Result<(), String> {
        let Some(selection) = &self.retry_selection else {
            return Ok(());
        };
        if selection
            .cases
            .iter()
            .any(|case| retry_case_matches_record(&self.run.run_id, case, probe))
        {
            Ok(())
        } else {
            Err("精确补测拒绝写入选择清单之外或身份不一致的探测记录".to_owned())
        }
    }

    /// 验证补测选择、运行级来源摘要、恢复身份和已提交记录严格闭合。
    fn validate_retry_selection_binding(&self) -> Result<(), String> {
        match (&self.retry_selection, &self.run.retry_lineage) {
            (None, None) => {
                if self.identity.retry_selection_sha256.is_some() {
                    return Err("普通运行不能保存精确补测选择摘要".to_owned());
                }
                Ok(())
            }
            (Some(selection), Some(lineage)) => {
                selection.validate()?;
                if lineage != &selection.lineage
                    || self.identity.retry_selection_sha256.as_deref()
                        != Some(selection.lineage.selection_sha256.as_str())
                {
                    return Err("精确补测选择、运行级 Lineage 与恢复身份摘要不一致".to_owned());
                }
                let selected = selection
                    .cases
                    .iter()
                    .map(|case| {
                        retry_case_key(
                            &self.run.run_id,
                            &case.provider_id,
                            &case.model,
                            &case.protocol,
                            &case.response_mode,
                            &case.capability,
                        )
                    })
                    .collect::<BTreeSet<_>>();
                if selected.len() != selection.cases.len() {
                    return Err("精确补测选择映射到当前运行后产生重复稳定键".to_owned());
                }
                if self.records.keys().any(|key| !selected.contains(key))
                    || self
                        .records
                        .values()
                        .any(|record| self.validate_probe_scope(record).is_err())
                {
                    return Err("精确补测运行包含选择清单之外的探测记录".to_owned());
                }
                Ok(())
            }
            _ => Err("精确补测选择与运行级 Lineage 必须同时存在".to_owned()),
        }
    }

    /// 首次冻结候选模型集合；恢复时只追加新模型且永不删除历史模型。
    pub(crate) fn register_candidates(
        &mut self,
        provider_id: &str,
        candidates: impl IntoIterator<Item = String>,
    ) -> Result<Vec<String>, String> {
        let mut candidates = candidates.into_iter().collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        let frozen = self
            .candidate_sets
            .entry(provider_id.to_owned())
            .or_default();
        for candidate in candidates {
            if let Err(index) = frozen.binary_search(&candidate) {
                frozen.insert(index, candidate);
            }
        }
        Ok(frozen.clone())
    }

    /// 按已同步的日志序号提交一条记录；同键只接受内容完全相同的幂等重放。
    pub(crate) fn commit_probe(&mut self, sequence: u64, probe: ProbeRecord) -> Result<(), String> {
        self.validate_probe_scope(&probe)?;
        if sequence != self.journal_sequence + 1 {
            return Err(format!(
                "恢复日志序号不连续：期望 {}，实际 {sequence}",
                self.journal_sequence + 1
            ));
        }
        insert_idempotent_record(&mut self.records, probe.stable_key(), probe, "恢复清单")?;
        self.journal_sequence = sequence;
        Ok(())
    }
}

/// 强制当前运行身份、运行级恢复来源与每条导入记录的来源身份成对一致。
fn validate_recovery_binding<'a>(
    run: &RunMetadata,
    current_executable_sha256: &str,
    records: impl IntoIterator<Item = &'a ProbeRecord>,
) -> Result<(), String> {
    if !valid_sha256_digest(current_executable_sha256) {
        return Err("当前恢复运行的可执行文件 SHA-256 格式无效".to_owned());
    }
    let recovered_records = records
        .into_iter()
        .filter(|record| record.recovered_from.is_some())
        .collect::<Vec<_>>();
    let Some(lineage) = &run.recovery_lineage else {
        if recovered_records.is_empty() {
            return Ok(());
        }
        return Err("恢复运行包含导入记录但缺少运行级恢复来源声明".to_owned());
    };
    let lineage_chain =
        validate_recovery_lineage_chain(lineage, current_executable_sha256, &run.run_id)?;
    if lineage.source_run_id == run.run_id
        || lineage.source_executable_sha256 == lineage.recovery_executable_sha256
        || recovered_records.len() != lineage.imported_records
    {
        return Err("恢复来源声明的当前运行、当前构建、来源构建或导入记录计数不一致".to_owned());
    }
    let mut imported_fixtures = BTreeSet::new();
    for record in &recovered_records {
        let origin = record
            .recovered_from
            .as_ref()
            .expect("已筛选的恢复记录必然包含来源");
        if !lineage_chain
            .iter()
            .any(|candidate| recovery_origin_matches_lineage(origin, candidate))
        {
            return Err("导入记录与运行级恢复来源声明不一致".to_owned());
        }
        imported_fixtures.extend(record.fixture_paths.iter().cloned());
    }
    if imported_fixtures.len() != lineage.imported_fixtures {
        return Err("恢复来源声明的导入 Fixture 计数不一致".to_owned());
    }
    for candidate in lineage_chain {
        let parent_records = candidate
            .parent
            .as_ref()
            .map_or(0, |parent| parent.imported_records);
        let parent_fixtures = candidate
            .parent
            .as_ref()
            .map_or(0, |parent| parent.imported_fixtures);
        let expected_records = candidate
            .imported_records
            .checked_sub(parent_records)
            .ok_or_else(|| "恢复来源链的导入记录计数发生倒退".to_owned())?;
        let expected_fixtures = candidate
            .imported_fixtures
            .checked_sub(parent_fixtures)
            .ok_or_else(|| "恢复来源链的导入 Fixture 计数发生倒退".to_owned())?;
        let direct_records = recovered_records
            .iter()
            .filter(|record| {
                record
                    .recovered_from
                    .as_ref()
                    .is_some_and(|origin| recovery_origin_matches_lineage(origin, candidate))
            })
            .collect::<Vec<_>>();
        if direct_records.len() != expected_records {
            return Err("恢复来源链中各代导入记录计数与记录级来源不一致".to_owned());
        }
        let direct_fixtures = direct_records
            .into_iter()
            .flat_map(|record| record.fixture_paths.iter().cloned())
            .collect::<BTreeSet<_>>();
        if direct_fixtures.len() != expected_fixtures {
            return Err("恢复来源链中各代导入 Fixture 计数与记录级来源不一致".to_owned());
        }
    }
    Ok(())
}

/// 校验恢复来源链的版本、摘要、连续构建身份与重新请求清单，并返回从近到远的链条。
fn validate_recovery_lineage_chain<'a>(
    lineage: &'a RecoveryLineage,
    current_executable_sha256: &str,
    current_run_id: &str,
) -> Result<Vec<&'a RecoveryLineage>, String> {
    let mut chain = Vec::new();
    let mut cursor = Some(lineage);
    let mut expected_recovery_executable = current_executable_sha256;
    let mut source_run_ids = BTreeSet::new();
    while let Some(candidate) = cursor {
        if chain.len() >= 16 {
            return Err("恢复来源链超过 16 层安全上限".to_owned());
        }
        if candidate.schema_version != RECOVERY_LINEAGE_SCHEMA_VERSION {
            return Err("恢复来源声明版本或策略不受支持".to_owned());
        }
        for digest in [
            &candidate.source_executable_sha256,
            &candidate.source_resume_sha256,
            &candidate.source_journal_sha256,
            &candidate.recovery_executable_sha256,
        ] {
            if !valid_sha256_digest(digest) {
                return Err("恢复来源声明包含格式无效的 SHA-256".to_owned());
            }
        }
        if candidate.recovery_executable_sha256 != expected_recovery_executable
            || candidate.source_executable_sha256 == candidate.recovery_executable_sha256
            || candidate.source_run_id.is_empty()
            || candidate.source_run_id == current_run_id
            || contains_unsafe_single_line(&candidate.source_run_id)
            || contains_unsafe_single_line(&candidate.source_runtime_commit)
        {
            return Err(
                "恢复来源声明的当前运行、当前构建、来源构建或导入记录计数不一致".to_owned(),
            );
        }
        if !source_run_ids.insert(candidate.source_run_id.clone()) {
            return Err("恢复来源链包含重复来源身份".to_owned());
        }
        match candidate.policy.as_str() {
            DIRECT_RECOVERY_POLICY => {
                if !candidate.rerun_records.is_empty()
                    || (candidate.parent.is_some()
                        && !matches!(
                            (
                                candidate.source_resume_schema_version.as_deref(),
                                candidate.source_harness_contract_id.as_deref(),
                            ),
                            (Some(RESUME_SCHEMA_VERSION), Some(HARNESS_CONTRACT_ID))
                        ))
                    || !matches!(
                        (
                            candidate.source_resume_schema_version.as_deref(),
                            candidate.source_harness_contract_id.as_deref(),
                        ),
                        (None, None) | (Some(RESUME_SCHEMA_VERSION), Some(HARNESS_CONTRACT_ID),)
                    )
                {
                    return Err("标准隔离恢复来源声明包含不允许的升级或重新请求字段".to_owned());
                }
            }
            LEGACY_RECOVERY_POLICY => {
                if candidate.source_resume_schema_version.as_deref()
                    != Some(RETRY_SOURCE_RESUME_SCHEMA_VERSION)
                    || candidate.source_harness_contract_id.as_deref()
                        != Some(RETRY_SOURCE_HARNESS_CONTRACT_ID)
                    || candidate.rerun_records.is_empty()
                {
                    return Err("v14 隔离升级来源声明缺少固定版本或重新请求记录".to_owned());
                }
                validate_recovery_rerun_records(&candidate.rerun_records)?;
            }
            _ => return Err("恢复来源声明版本或策略不受支持".to_owned()),
        }
        chain.push(candidate);
        expected_recovery_executable = &candidate.source_executable_sha256;
        cursor = candidate.parent.as_deref();
    }
    Ok(chain)
}

/// 判断记录级来源是否精确对应恢复链中的某一代来源。
fn recovery_origin_matches_lineage(
    origin: &RecoveredProbeOrigin,
    lineage: &RecoveryLineage,
) -> bool {
    origin.source_run_id == lineage.source_run_id
        && origin.source_runtime_commit == lineage.source_runtime_commit
        && origin.source_executable_sha256 == lineage.source_executable_sha256
}

/// 校验重新请求记录严格排序、身份字段安全且只描述唯一支持的旧取消异常。
fn validate_recovery_rerun_records(records: &[RecoveryRerunRecord]) -> Result<(), String> {
    let mut previous_key: Option<&str> = None;
    for record in records {
        if previous_key.is_some_and(|previous| previous >= record.source_stable_key.as_str()) {
            return Err("恢复重新请求记录必须按稳定键严格排序且不能重复".to_owned());
        }
        previous_key = Some(&record.source_stable_key);
        let stable_digest = record
            .source_stable_key
            .strip_prefix("probe-key-v1:sha256:")
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            })
            .ok_or_else(|| "恢复重新请求记录的来源稳定键格式无效".to_owned())?;
        let _ = stable_digest;
        validate_fixture_relative_path(&record.source_fixture_path)?;
        if !valid_sha256_digest(&record.source_fixture_content_sha256)
            || record.capability != "cancellation"
            || record.reason != LEGACY_CANCELLATION_RERUN_REASON
            || !matches!(
                record.protocol.as_str(),
                "anthropic_messages" | "openai_chat_completions" | "openai_responses"
            )
            || !matches!(record.response_mode.as_str(), "buffered" | "streaming")
            || [record.provider_id.as_str(), record.model.as_str()]
                .into_iter()
                .any(|value| value.is_empty() || contains_unsafe_single_line(value))
        {
            return Err("恢复重新请求记录包含无效版本、能力、协议、模式或身份字段".to_owned());
        }
        let content_digest = record
            .source_fixture_content_sha256
            .strip_prefix("sha256:")
            .expect("已验证的 SHA-256 必须包含固定前缀");
        let stable_key_digest = domain_separated_hex(
            b"keencode-provider-fixture-stable-key-v2",
            &[record.source_stable_key.as_bytes()],
        );
        let expected_path = format!("fixtures/{stable_key_digest}-{content_digest}.json");
        if record.source_fixture_path != expected_path {
            return Err("恢复重新请求记录的 Fixture 路径与来源稳定键或内容摘要不一致".to_owned());
        }
    }
    Ok(())
}

/// 读取当前可执行文件字节并返回不包含路径的 SHA-256 构建身份。
fn current_executable_sha256() -> Result<String, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("无法定位当前 Provider 测试可执行文件：{error}"))?;
    let bytes = fs::read(executable)
        .map_err(|error| format!("无法读取当前 Provider 测试可执行文件：{error}"))?;
    Ok(sha256_digest(&bytes))
}

/// 返回不包含原始字节的规范 SHA-256 证据。
fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_digest(bytes))
}

/// 把结构化值序列化为唯一的缩进 JSON 文本，并固定以单个 LF 结束。
fn serialize_json_artifact<T: Serialize>(name: &str, value: &T) -> Result<String, String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("无法序列化 {name}：{error}"))?;
    Ok(format!("{text}\n"))
}

/// 对已经打开的有界字节流增量计算 SHA-256，避免为大型事实产物分配同等内存。
fn sha256_digest_reader(
    mut reader: impl Read,
    max_bytes: u64,
    label: &str,
) -> Result<(String, u64), String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("无法增量读取 {label}：{error}"))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).expect("固定缓冲区长度总能表示为 u64"))
            .ok_or_else(|| format!("{label} 长度溢出"))?;
        if total > max_bytes {
            return Err(format!("{label} 在读取期间超过 {max_bytes} 字节安全上限"));
        }
        hasher.update(&buffer[..read]);
    }
    Ok((format!("sha256:{}", hex_encode(&hasher.finalize())), total))
}

/// 跨平台保存已经打开并验证的普通文件稳定对象身份。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegularFileIdentity {
    /// Windows 卷序列号与 128 位文件标识。
    #[cfg(windows)]
    windows: WindowsObjectIdentity,
    /// Unix 设备号与 inode。
    #[cfg(unix)]
    unix: UnixObjectIdentity,
}

/// 稳定普通文件句柄需要的访问方式。
#[derive(Clone, Copy, Eq, PartialEq)]
enum StableFileAccess {
    /// 只读取文件内容，并阻止 Windows 上的并发写入与替换。
    ReadOnly,
    /// 读取并允许当前句柄原地修复文件，同时排斥其他写入者。
    ReadWrite,
    /// 只在文件末尾追加，同时排斥其他写入者。
    Append,
    /// 仅用于空锁文件协调，允许其他进程打开同一文件后参与文件锁竞争。
    Lock,
    /// 仅以读取权限复核对象身份，同时兼容 Windows 已持有的独占写句柄。
    Verify,
}

/// 稳定普通文件句柄的创建语义。
#[derive(Clone, Copy)]
enum StableFileCreation {
    /// 文件必须已经存在。
    Existing,
    /// 文件不存在时创建，存在时打开同一普通文件。
    CreateIfMissing,
}

/// 从已打开普通文件的句柄元数据读取当前平台可用的稳定对象身份。
fn regular_file_identity_from_open_handle(
    file: &File,
    metadata: &fs::Metadata,
    label: &str,
) -> Result<RegularFileIdentity, String> {
    #[cfg(windows)]
    {
        let _ = metadata;
        Ok(RegularFileIdentity {
            windows: windows_object_identity_from_handle(file, label)?,
        })
    }
    #[cfg(unix)]
    {
        let _ = (file, label);
        Ok(RegularFileIdentity {
            unix: UnixObjectIdentity::from_metadata(metadata),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, metadata, label);
        Ok(RegularFileIdentity {})
    }
}

/// 复核路径仍指向已打开的同一普通文件对象；不支持稳定身份的平台只复核类型。
fn verify_regular_file_path_identity(
    path: &Path,
    expected: &RegularFileIdentity,
    label: &str,
) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法复核 {label} 元数据：{error}"))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(format!(
            "{label} 在读取期间已变为链接、重解析点或非普通文件"
        ));
    }
    #[cfg(windows)]
    {
        let verification = open_windows_regular_file_handle(path, &format!("{label} 复核文件"))?;
        let verification_metadata = verification
            .metadata()
            .map_err(|error| format!("无法读取 {label} 复核句柄元数据：{error}"))?;
        if is_link_or_reparse(&verification_metadata) || !verification_metadata.is_file() {
            return Err(format!("{label} 复核句柄不是非重解析普通文件"));
        }
        let actual =
            regular_file_identity_from_open_handle(&verification, &verification_metadata, label)?;
        if &actual != expected {
            return Err(format!("{label} 路径所指文件对象在读取期间发生变化"));
        }
    }
    #[cfg(unix)]
    {
        let verification = open_unix_regular_file_handle(
            path,
            StableFileAccess::Verify,
            StableFileCreation::Existing,
            &format!("{label} 复核文件"),
        )?;
        let verification_metadata = verification
            .metadata()
            .map_err(|error| format!("无法读取 {label} Unix 复核句柄元数据：{error}"))?;
        if !verification_metadata.is_file()
            || &(RegularFileIdentity {
                unix: UnixObjectIdentity::from_metadata(&verification_metadata),
            }) != expected
        {
            return Err(format!("{label} 路径所指文件对象在读取期间发生变化"));
        }
    }
    Ok(())
}

/// 以不跟随最终重解析点的策略打开有界普通文件，并闭合路径检查与句柄身份。
fn open_bounded_regular_file(
    path: &Path,
    max_bytes: u64,
    expected_len: Option<u64>,
    expected_identity: Option<&RegularFileIdentity>,
    label: &str,
) -> Result<(File, u64, RegularFileIdentity), String> {
    let path_metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法读取 {label} 元数据：{error}"))?;
    if is_link_or_reparse(&path_metadata) || !path_metadata.is_file() {
        return Err(format!("{label} 必须是普通文件且不能是链接或重解析点"));
    }
    if path_metadata.len() > max_bytes {
        return Err(format!("{label} 超过 {max_bytes} 字节安全上限"));
    }
    #[cfg(windows)]
    let file = open_windows_regular_file_handle(path, label)?;
    #[cfg(unix)]
    let file = open_unix_regular_file_handle(
        path,
        StableFileAccess::ReadOnly,
        StableFileCreation::Existing,
        label,
    )?;
    #[cfg(not(any(unix, windows)))]
    let file = File::open(path).map_err(|error| format!("无法打开 {label}：{error}"))?;
    let handle_metadata = file
        .metadata()
        .map_err(|error| format!("无法读取已打开 {label} 元数据：{error}"))?;
    if is_link_or_reparse(&handle_metadata)
        || !handle_metadata.is_file()
        || handle_metadata.len() > max_bytes
    {
        return Err(format!(
            "已打开 {label} 不是非重解析普通文件或超过 {max_bytes} 字节安全上限"
        ));
    }
    let identity = regular_file_identity_from_open_handle(&file, &handle_metadata, label)?;
    #[cfg(unix)]
    if identity.unix != UnixObjectIdentity::from_metadata(&path_metadata) {
        return Err(format!("{label} 在路径检查与打开之间被替换"));
    }
    if expected_len.is_some_and(|expected| expected != handle_metadata.len()) {
        return Err(format!("{label} 在枚举与打开之间长度发生变化"));
    }
    if expected_identity.is_some_and(|expected| expected != &identity) {
        return Err(format!("{label} 在枚举与打开之间文件对象发生变化"));
    }
    verify_regular_file_path_identity(path, &identity, label)?;
    Ok((file, handle_metadata.len(), identity))
}

/// 通过同一普通文件句柄校验身份与长度并增量计算摘要，拒绝枚举到读取间的替换。
fn sha256_digest_regular_file(
    path: &Path,
    max_bytes: u64,
    expected_len: Option<u64>,
    expected_identity: Option<&RegularFileIdentity>,
    label: &str,
) -> Result<String, String> {
    let (mut file, opened_len, identity) =
        open_bounded_regular_file(path, max_bytes, expected_len, expected_identity, label)?;
    let (digest, actual_len) = sha256_digest_reader(
        (&mut file).take(max_bytes.saturating_add(1)),
        max_bytes,
        label,
    )?;
    if actual_len != opened_len {
        return Err(format!("{label} 在摘要期间长度发生变化"));
    }
    let final_metadata = file
        .metadata()
        .map_err(|error| format!("无法复核已打开 {label} 元数据：{error}"))?;
    if final_metadata.len() != opened_len
        || regular_file_identity_from_open_handle(&file, &final_metadata, label)? != identity
    {
        return Err(format!("{label} 在摘要期间文件身份或长度发生变化"));
    }
    verify_regular_file_path_identity(path, &identity, label)?;
    Ok(digest)
}

/// 为跨运行合并生成不包含运行标识的稳定探测 tuple 摘要。
pub(crate) fn retry_tuple_key(
    provider_id: &str,
    model: &str,
    protocol: &str,
    response_mode: &str,
    capability: &str,
) -> String {
    format!(
        "probe-tuple-v1:sha256:{}",
        domain_separated_hex(
            b"keencode-provider-probe-tuple-v1",
            &[
                provider_id.as_bytes(),
                model.as_bytes(),
                protocol.as_bytes(),
                response_mode.as_bytes(),
                capability.as_bytes(),
            ],
        )
    )
}

/// 为同一精确 tuple 计算指定运行中的标准探测稳定键。
fn retry_case_key(
    run_id: &str,
    provider_id: &str,
    model: &str,
    protocol: &str,
    response_mode: &str,
    capability: &str,
) -> String {
    probe_stable_key(
        run_id,
        provider_id,
        model,
        protocol,
        response_mode,
        capability,
    )
}

/// 判断一条补测事实是否逐字段匹配冻结清单中的唯一 tuple 和当前运行稳定键。
fn retry_case_matches_record(run_id: &str, case: &RetryCase, record: &ProbeRecord) -> bool {
    record.stable_key
        == retry_case_key(
            run_id,
            &case.provider_id,
            &case.model,
            &case.protocol,
            &case.response_mode,
            &case.capability,
        )
        && record.provider_id == case.provider_id
        && record.model == case.model
        && record.protocol == case.protocol
        && record.response_mode == case.response_mode
        && record.capability == case.capability
}

/// 只参与选择摘要计算且明确排除摘要自身的规范负载。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RetrySelectionDigestPayload<'a> {
    /// 选择结构版本。
    schema_version: &'a str,
    /// 来源运行标识。
    source_run_id: &'a str,
    /// 来源 Git 构建标识。
    source_runtime_commit: &'a str,
    /// 来源可执行文件摘要。
    source_executable_sha256: &'a str,
    /// 来源事实认证等级。
    source_authentication: &'a str,
    /// 来源恢复清单结构版本。
    source_resume_schema_version: &'a str,
    /// 来源 Harness 契约标识。
    source_harness_contract_id: &'a str,
    /// 来源最终报告结构版本。
    source_report_schema_version: &'a str,
    /// 来源恢复清单摘要。
    source_resume_sha256: &'a str,
    /// 来源提交日志摘要。
    source_journal_sha256: &'a str,
    /// 来源最终报告摘要。
    source_result_sha256: &'a str,
    /// 来源脱敏报告摘要。
    source_redaction_report_sha256: &'a str,
    /// 唯一目标 Provider。
    provider_id: &'a str,
    /// 来源提交日志序号上限。
    through_sequence: u64,
    /// 固定失败选择策略。
    policy: &'a str,
    /// 选择的完整稳定 tuple。
    cases: &'a [RetryCase],
}

impl RetrySelectionManifest {
    /// 返回补测恢复身份唯一允许的 Provider 和普通能力集合。
    pub(crate) fn runtime_shape(&self) -> (String, BTreeSet<String>) {
        (
            self.lineage.provider_id.clone(),
            self.cases
                .iter()
                .map(|case| case.capability.clone())
                .collect(),
        )
    }

    /// 返回排除摘要自身后对完整来源与 tuple 集合计算的规范摘要。
    fn calculated_sha256(&self) -> Result<String, String> {
        let payload = RetrySelectionDigestPayload {
            schema_version: &self.lineage.schema_version,
            source_run_id: &self.lineage.source_run_id,
            source_runtime_commit: &self.lineage.source_runtime_commit,
            source_executable_sha256: &self.lineage.source_executable_sha256,
            source_authentication: &self.lineage.source_authentication,
            source_resume_schema_version: &self.lineage.source_resume_schema_version,
            source_harness_contract_id: &self.lineage.source_harness_contract_id,
            source_report_schema_version: &self.lineage.source_report_schema_version,
            source_resume_sha256: &self.lineage.source_resume_sha256,
            source_journal_sha256: &self.lineage.source_journal_sha256,
            source_result_sha256: &self.lineage.source_result_sha256,
            source_redaction_report_sha256: &self.lineage.source_redaction_report_sha256,
            provider_id: &self.lineage.provider_id,
            through_sequence: self.lineage.through_sequence,
            policy: &self.lineage.policy,
            cases: &self.cases,
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("无法序列化精确补测选择摘要负载：{error}"))?;
        Ok(sha256_digest(&bytes))
    }

    /// 校验完整选择清单的版本、摘要、排序、Provider 和稳定 tuple 身份。
    pub(crate) fn validate(&self) -> Result<(), String> {
        let lineage = &self.lineage;
        if lineage.schema_version != RETRY_SELECTION_SCHEMA_VERSION
            || lineage.policy != RETRY_SELECTION_POLICY
        {
            return Err("精确补测选择版本或固定筛选策略不受支持".to_owned());
        }
        let valid_source_provenance = matches!(
            (
                lineage.source_authentication.as_str(),
                lineage.source_resume_schema_version.as_str(),
                lineage.source_harness_contract_id.as_str(),
                lineage.source_report_schema_version.as_str(),
            ),
            (
                AUTHENTICATED_SOURCE_LEVEL,
                RESUME_SCHEMA_VERSION,
                HARNESS_CONTRACT_ID,
                RUN_REPORT_SCHEMA_VERSION,
            ) | (
                LEGACY_UNAUTHENTICATED_SOURCE_LEVEL,
                RETRY_SOURCE_RESUME_SCHEMA_VERSION,
                RETRY_SOURCE_HARNESS_CONTRACT_ID,
                RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION,
            )
        );
        if !valid_source_provenance {
            return Err("精确补测选择的来源认证等级与版本组合无效".to_owned());
        }
        if lineage.through_sequence == 0
            || lineage.selected_records == 0
            || lineage.selected_records != self.cases.len()
        {
            return Err("精确补测选择的序号上限或记录计数无效".to_owned());
        }
        for digest in [
            &lineage.source_executable_sha256,
            &lineage.source_resume_sha256,
            &lineage.source_journal_sha256,
            &lineage.source_result_sha256,
            &lineage.source_redaction_report_sha256,
            &lineage.selection_sha256,
        ] {
            if !valid_sha256_digest(digest) {
                return Err("精确补测选择包含格式无效的 SHA-256".to_owned());
            }
        }
        if lineage.source_run_id.is_empty()
            || lineage.provider_id.is_empty()
            || contains_unsafe_single_line(&lineage.source_run_id)
            || contains_unsafe_single_line(&lineage.provider_id)
        {
            return Err("精确补测选择包含空标识或危险显示字符".to_owned());
        }
        let mut previous_sequence = 0_u64;
        let mut tuple_keys = BTreeSet::new();
        for case in &self.cases {
            if case.source_sequence <= previous_sequence
                || case.source_sequence > lineage.through_sequence
                || case.provider_id != lineage.provider_id
            {
                return Err("精确补测 tuple 未按来源序号严格排序或越过选择边界".to_owned());
            }
            previous_sequence = case.source_sequence;
            if [
                case.source_stable_key.as_str(),
                case.tuple_key.as_str(),
                case.model.as_str(),
                case.protocol.as_str(),
                case.response_mode.as_str(),
                case.capability.as_str(),
            ]
            .into_iter()
            .any(|value| value.is_empty() || contains_unsafe_single_line(value))
            {
                return Err("精确补测 tuple 包含空字段或危险显示字符".to_owned());
            }
            if case.source_stable_key
                != retry_case_key(
                    &lineage.source_run_id,
                    &case.provider_id,
                    &case.model,
                    &case.protocol,
                    &case.response_mode,
                    &case.capability,
                )
                || case.tuple_key
                    != retry_tuple_key(
                        &case.provider_id,
                        &case.model,
                        &case.protocol,
                        &case.response_mode,
                        &case.capability,
                    )
            {
                return Err("精确补测 tuple 的来源稳定键或 tuple 摘要不一致".to_owned());
            }
            if !matches!(
                case.protocol.as_str(),
                "anthropic_messages" | "openai_chat_completions" | "openai_responses"
            ) || !matches!(case.response_mode.as_str(), "buffered" | "streaming")
                || !is_known_probe_capability(&case.capability)
                || case.capability == "stream_interruption"
            {
                return Err("精确补测 tuple 包含未知协议、响应模式或能力".to_owned());
            }
            if !tuple_keys.insert(case.tuple_key.clone()) {
                return Err("精确补测选择包含重复 tuple".to_owned());
            }
        }
        if self.calculated_sha256()? != lineage.selection_sha256 {
            return Err("精确补测选择摘要与规范负载不一致".to_owned());
        }
        Ok(())
    }
}

/// 判断不可信单行字段是否包含终端、换行或 Unicode 方向控制字符。
fn contains_unsafe_single_line(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(character, '\r' | '\n' | '\t') || is_dangerous_display_character(character)
    })
}

/// 严格校验最终报告与完成后的恢复清单、当前 Provider 配置和确定性汇总一致。
fn validate_stored_run_report(
    bytes: &[u8],
    manifest: &ResumeManifest,
    providers: &[&ProviderEntry],
    accepted_schema_versions: &[&str],
) -> Result<StoredRunReport, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "最终报告必须是有效 UTF-8".to_owned())?;
    ensure_safe_artifact(text, providers)?;
    let report: StoredRunReport =
        serde_json::from_str(text).map_err(|error| format!("最终报告结构无效：{error}"))?;
    if !accepted_schema_versions.contains(&report.schema_version.as_str()) {
        return Err(format!(
            "最终报告 schema 不受支持：{}",
            report.schema_version
        ));
    }
    if !manifest.finished || report.run.finished_at.is_none() {
        return Err("最终报告或对应恢复清单尚未完成".to_owned());
    }
    if serde_json::to_value(&report.run).map_err(|error| error.to_string())?
        != serde_json::to_value(&manifest.run).map_err(|error| error.to_string())?
    {
        return Err("最终报告运行元数据与恢复清单不一致".to_owned());
    }

    let mut expected_providers = BTreeMap::new();
    for identity in &manifest.identity.providers {
        let provider = resolve_resume_provider(&identity.provider_id, providers)?;
        let record = ProviderRecord::from_provider(provider)?;
        expected_providers.insert(
            record.provider_id.clone(),
            serde_json::to_value(record)
                .map_err(|error| format!("无法序列化 Provider 核对值：{error}"))?,
        );
    }
    let mut stored_providers = BTreeMap::new();
    for value in &report.providers {
        let provider_id = value
            .get("providerId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "最终报告 Provider 记录缺少稳定 providerId".to_owned())?;
        if stored_providers
            .insert(provider_id.to_owned(), value.clone())
            .is_some()
        {
            return Err("最终报告包含重复 Provider 记录".to_owned());
        }
    }
    if stored_providers != expected_providers {
        return Err("最终报告 Provider 快照与当前配置或恢复身份不一致".to_owned());
    }

    let stored_records = collect_unique_probe_records(&report.probes, "最终报告")?;
    if serde_json::to_value(&stored_records).map_err(|error| error.to_string())?
        != serde_json::to_value(&manifest.records).map_err(|error| error.to_string())?
    {
        return Err("最终报告探测事实与恢复清单不一致".to_owned());
    }
    let expected_summary = SummaryRecord::from_probes(&report.probes);
    if serde_json::to_value(expected_summary).map_err(|error| error.to_string())? != report.summary
    {
        return Err("最终报告汇总不能由探测事实确定性重建".to_owned());
    }
    validate_catalog_completion(manifest, &report.catalogs, &report.probes, providers)?;
    Ok(report)
}

/// 目录发现失败时仍可进入完成态的显式 Provider 级结论。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogCompletionState {
    /// 所有 Provider 的目录均已完整通过校验。
    Complete,
    /// 目录保留真实失败证据，但冻结候选集合的全部矩阵已形成终态。
    DiscoveryFailedWithCompleteFrozenMatrix,
}

/// 校验完成态的目录事实、冻结候选集合以及每个应有探测 tuple 是否完整闭合。
///
/// 目录失败不会被改写成成功；只有在失败 Provider 的冻结候选、诊断案例和所选
/// 矩阵全部存在唯一已提交终态时，才返回显式的目录失败完成结论。
pub(crate) fn validate_catalog_completion(
    manifest: &ResumeManifest,
    catalogs: &[CatalogRecord],
    probes: &[ProbeRecord],
    providers: &[&ProviderEntry],
) -> Result<CatalogCompletionState, String> {
    // 精确补测本身不执行模型目录，空目录是其固定结构，不参与普通目录完成判定。
    if manifest.retry_selection.is_some() {
        if !catalogs.is_empty() {
            return Err("精确补测最终报告不能包含模型目录事实".to_owned());
        }
        return Ok(CatalogCompletionState::Complete);
    }

    // 若没有候选集合和目录，允许最小的零请求测试夹具继续复用；真实普通运行在
    // 首次目录回调后一定会冻结至少一个 Provider 候选集合。
    if catalogs.is_empty() && manifest.candidate_sets.is_empty() {
        return Ok(CatalogCompletionState::Complete);
    }
    if catalogs.is_empty() {
        return Err("普通完成运行必须为每个 Provider 保存模型目录事实".to_owned());
    }

    validate_resume_candidate_sets(manifest, providers)?;
    let expected_provider_ids = manifest
        .identity
        .providers
        .iter()
        .map(|identity| identity.provider_id.clone())
        .collect::<BTreeSet<_>>();
    if catalogs.len() != expected_provider_ids.len() {
        return Err("最终报告必须为每个恢复身份 Provider 保存且只保存一条模型目录事实".to_owned());
    }

    let expected_protocols = all_protocols()
        .into_iter()
        .map(|protocol| protocol_name(protocol).to_owned())
        .collect::<Vec<_>>();
    if manifest.identity.protocols != expected_protocols {
        return Err("完成态恢复身份的协议集合不是当前固定三协议矩阵".to_owned());
    }
    let expected_response_modes = vec!["buffered".to_owned(), "streaming".to_owned()];
    if manifest.identity.response_modes != expected_response_modes {
        return Err("完成态恢复身份的响应模式集合不是当前固定双模式矩阵".to_owned());
    }

    let mut catalogs_by_provider = BTreeMap::new();
    let mut failed_catalogs = 0_usize;
    for catalog in catalogs {
        if !expected_provider_ids.contains(&catalog.provider_id) {
            return Err("最终报告模型目录引用未知 Provider".to_owned());
        }
        if catalogs_by_provider
            .insert(catalog.provider_id.clone(), catalog)
            .is_some()
        {
            return Err("最终报告模型目录包含重复 Provider".to_owned());
        }

        let provider = resolve_resume_provider(&catalog.provider_id, providers)?;
        let frozen_candidates = manifest
            .candidate_sets
            .get(&catalog.provider_id)
            .ok_or_else(|| format!("Provider {} 缺少冻结候选集合", catalog.provider_id))?;
        let frozen_set = frozen_candidates.iter().cloned().collect::<BTreeSet<_>>();
        let mut catalog_candidates = BTreeSet::new();
        for candidate in &catalog.candidates {
            if validate_inline_value("最终报告目录候选模型标识", &candidate.model).is_err()
                || provider.redact_text(&candidate.model) != candidate.model
            {
                return Err(format!(
                    "Provider {} 的最终报告目录候选模型无法还原当前安全身份",
                    catalog.provider_id
                ));
            }
            if !catalog_candidates.insert(candidate.model.clone()) {
                return Err(format!(
                    "Provider {} 的最终报告目录包含重复候选模型",
                    catalog.provider_id
                ));
            }
            if !frozen_set.contains(&candidate.model) {
                return Err(format!(
                    "Provider {} 的最终报告目录候选模型不属于冻结集合",
                    catalog.provider_id
                ));
            }
        }
        if catalog_candidates != frozen_set {
            return Err(format!(
                "Provider {} 的最终报告目录候选集合与冻结集合不一致",
                catalog.provider_id
            ));
        }

        let mut discovered_models = catalog.discovered_models.clone();
        discovered_models.sort();
        discovered_models.dedup();
        if discovered_models != catalog.discovered_models
            || catalog.discovered_models.iter().any(|model| {
                validate_inline_value("最终报告目录实时模型标识", model).is_err()
                    || provider.redact_text(model) != *model
            })
        {
            return Err(format!(
                "Provider {} 的最终报告实时目录模型集合不是规范安全集合",
                catalog.provider_id
            ));
        }

        match catalog.status.as_str() {
            "success" if catalog.normalized_error.is_none() => {}
            "success" => {
                return Err(format!(
                    "Provider {} 的成功目录不能携带失败错误证据",
                    catalog.provider_id
                ));
            }
            "failed" if catalog.normalized_error.is_some() => {
                failed_catalogs = failed_catalogs
                    .checked_add(1)
                    .ok_or_else(|| "失败目录数量溢出".to_owned())?;
                if manifest.identity.catalog_only {
                    return Err("仅目录运行的失败模型目录不能声明完成".to_owned());
                }
            }
            "failed" => {
                return Err(format!(
                    "Provider {} 的失败目录缺少真实归一化错误证据",
                    catalog.provider_id
                ));
            }
            _ => {
                return Err(format!(
                    "Provider {} 的模型目录状态不受支持：{}",
                    catalog.provider_id, catalog.status
                ));
            }
        }
    }
    if catalogs_by_provider.len() != expected_provider_ids.len() {
        return Err("最终报告缺少恢复身份 Provider 的模型目录事实".to_owned());
    }
    for provider_id in &expected_provider_ids {
        if !manifest.candidate_sets.contains_key(provider_id) {
            return Err(format!("Provider {provider_id} 缺少冻结候选集合"));
        }
    }

    let stored_records = collect_unique_probe_records(probes, "最终报告")?;
    if serde_json::to_value(&stored_records).map_err(|error| error.to_string())?
        != serde_json::to_value(&manifest.records).map_err(|error| error.to_string())?
    {
        return Err("完成态最终报告探测事实与恢复清单不一致".to_owned());
    }

    let mut actual_tuples = BTreeMap::<String, String>::new();
    for (key, record) in &stored_records {
        validate_reusable_record_with_current_gap(manifest, key, record, providers)?;
        let tuple = completion_probe_tuple_key(record);
        if actual_tuples
            .insert(tuple, record.provider_id.clone())
            .is_some()
        {
            return Err("完成态探测事实包含重复模型、协议、响应模式和能力 tuple".to_owned());
        }
    }

    // 无论目录是否成功，都必须证明当前恢复身份要求的完整 tuple 矩阵已经逐项
    // 提交；目录失败时额外保留显式失败完成态，不能把缺失矩阵伪装成成功覆盖。
    for provider_id in &expected_provider_ids {
        let frozen_candidates = manifest
            .candidate_sets
            .get(provider_id)
            .ok_or_else(|| format!("Provider {provider_id} 缺少冻结候选集合"))?;
        if failed_catalogs > 0 && manifest.identity.full_matrix && frozen_candidates.is_empty() {
            return Err(format!(
                "Provider {provider_id} 的目录失败且完整矩阵没有冻结候选模型，不能声明完成"
            ));
        }
        let mut expected_tuples = BTreeSet::new();
        let add_tuple = |tuples: &mut BTreeSet<String>,
                         model: &str,
                         protocol: &str,
                         mode: &str,
                         capability: &str| {
            tuples.insert(retry_tuple_key(
                provider_id,
                model,
                protocol,
                mode,
                capability,
            ));
        };

        if !manifest.identity.catalog_only && !manifest.identity.diagnostics_only {
            let mut capabilities = BTreeSet::from(["text".to_owned()]);
            for capability in &manifest.identity.capabilities {
                if !is_known_probe_capability(capability) {
                    return Err(format!("完成态恢复身份包含未知能力：{capability}"));
                }
                capabilities.insert(capability.clone());
            }
            for model in frozen_candidates {
                for protocol in &manifest.identity.protocols {
                    for mode in &manifest.identity.response_modes {
                        for capability in &capabilities {
                            add_tuple(&mut expected_tuples, model, protocol, mode, capability);
                        }
                    }
                }
            }
        }
        if manifest.identity.full_matrix || manifest.identity.diagnostics_only {
            for protocol in &manifest.identity.protocols {
                for mode in &manifest.identity.response_modes {
                    add_tuple(
                        &mut expected_tuples,
                        "keencode-authentication-probe",
                        protocol,
                        mode,
                        "diagnostic_invalid_authentication",
                    );
                    // 缺失模型的具体 ID 绑定到运行或来源 runId；比较时由
                    // completion_probe_tuple_key 统一归一化为诊断占位符。
                    add_tuple(
                        &mut expected_tuples,
                        COMPLETION_MISSING_MODEL_PLACEHOLDER,
                        protocol,
                        mode,
                        "diagnostic_missing_model",
                    );
                }
            }
        }

        let actual_provider_tuples = stored_records
            .values()
            .filter(|record| record.provider_id == *provider_id)
            .map(completion_probe_tuple_key)
            .collect::<BTreeSet<_>>();
        if actual_provider_tuples != expected_tuples {
            return Err(format!(
                "Provider {provider_id} 的冻结候选矩阵缺少或多出已提交终态 tuple（应有 {} 条，实际 {} 条）",
                expected_tuples.len(),
                actual_provider_tuples.len()
            ));
        }
    }

    if actual_tuples.len() != stored_records.len() {
        return Err("完成态探测事实的 tuple 唯一性与记录数量不一致".to_owned());
    }
    if failed_catalogs == 0 {
        Ok(CatalogCompletionState::Complete)
    } else {
        Ok(CatalogCompletionState::DiscoveryFailedWithCompleteFrozenMatrix)
    }
}

/// 缺失模型诊断在跨运行恢复时可能绑定来源 runId，完成校验只比较其固定槽位。
const COMPLETION_MISSING_MODEL_PLACEHOLDER: &str = "<diagnostic_missing_model>";

/// 为完成矩阵校验生成不含运行标识的 tuple；缺失模型诊断使用固定槽位。
fn completion_probe_tuple_key(record: &ProbeRecord) -> String {
    let model = if record.capability == "diagnostic_missing_model" {
        COMPLETION_MISSING_MODEL_PLACEHOLDER
    } else {
        record.model.as_str()
    };
    retry_tuple_key(
        &record.provider_id,
        model,
        &record.protocol,
        &record.response_mode,
        &record.capability,
    )
}

/// 按稳定键收集最终事实，并拒绝任何重复项，即使两条记录逐字节完全相同。
fn collect_unique_probe_records(
    probes: &[ProbeRecord],
    source: &str,
) -> Result<BTreeMap<String, ProbeRecord>, String> {
    let mut records = BTreeMap::new();
    for record in probes {
        if records
            .insert(record.stable_key(), record.clone())
            .is_some()
        {
            return Err(format!("{source}包含重复探测稳定键"));
        }
    }
    Ok(records)
}

/// 校验持久化脱敏报告的严格结构、安全文本与当前唯一零命中版本。
fn validate_stored_redaction_report(
    bytes: &[u8],
    providers: &[&ProviderEntry],
) -> Result<StoredRedactionReport, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "脱敏报告必须是有效 UTF-8".to_owned())?;
    ensure_safe_artifact(text, providers)?;
    let report: StoredRedactionReport =
        serde_json::from_str(text).map_err(|error| format!("脱敏报告结构无效：{error}"))?;
    if report.schema_version != REDACTION_REPORT_SCHEMA_VERSION || !report.passed {
        return Err("脱敏报告版本不受支持或验收未通过".to_owned());
    }
    if report.exact_credential_matches != 0
        || report.secret_token_matches != 0
        || report.masked_credential_suffix_matches != 0
        || report.authentication_header_matches != 0
        || report.cookie_matches != 0
        || report.absolute_path_matches != 0
        || report.dangerous_display_character_matches != 0
        || report.non_synthetic_prompt_matches != 0
        || report.non_utf8_artifacts != 0
    {
        return Err("脱敏报告包含非零命中".to_owned());
    }
    Ok(report)
}

/// 把相对或绝对路径化为不访问文件系统的绝对词法路径，并消解点组件。
fn lexical_absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("恢复输出根目录不能为空".to_owned());
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("无法确定恢复输出根目录的当前目录：{error}"))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err("恢复输出根目录不能越过文件系统根目录".to_owned());
                }
            }
        }
    }
    if !normalized.is_absolute() {
        return Err("恢复输出根目录无法解析为绝对路径".to_owned());
    }
    Ok(normalized)
}

/// 按当前平台路径语义判断候选路径是否等于或位于指定根目录内。
fn path_is_same_or_descendant(candidate: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let candidate = candidate.components().collect::<Vec<_>>();
        let root = root.components().collect::<Vec<_>>();
        candidate.len() >= root.len()
            && candidate.iter().zip(root.iter()).all(|(left, right)| {
                left.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
            })
    }
    #[cfg(not(windows))]
    {
        candidate.starts_with(root)
    }
}

/// 按当前平台路径语义判断两个规范路径是否相同。
fn paths_equal(left: &Path, right: &Path) -> bool {
    path_is_same_or_descendant(left, right) && path_is_same_or_descendant(right, left)
}

/// 一次无写入输出根校验及其最近既有目录身份。
struct ValidatedRecoveryOutputRoot {
    /// 消解既有边界与点组件后的预期输出根。
    resolved: PathBuf,
    /// 校验时最近的既有普通目录。
    existing_anchor: PathBuf,
    /// Windows 最近既有目录的真实文件系统身份。
    #[cfg(windows)]
    existing_anchor_identity: WindowsObjectIdentity,
    /// Windows 生命周期内阻止最近既有目录重命名或替换的句柄。
    #[cfg(windows)]
    _existing_anchor_handle: File,
    /// Unix 最近既有目录的设备号与 inode 身份。
    #[cfg(unix)]
    existing_anchor_identity: UnixObjectIdentity,
}

impl ValidatedRecoveryOutputRoot {
    /// 在目标创建前后复核最近既有目录仍是同一个文件系统对象。
    fn verify_existing_anchor(&self) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.existing_anchor)
            .map_err(|error| format!("无法复核恢复输出根目录既有边界：{error}"))?;
        if !metadata.is_dir() || is_link_or_reparse(&metadata) {
            return Err("恢复输出根目录既有边界已变为非普通目录或重解析点".to_owned());
        }
        #[cfg(windows)]
        {
            let held_identity = windows_object_identity_from_handle(
                &self._existing_anchor_handle,
                "恢复输出根目录既有边界固定句柄",
            )?;
            let current_handle = open_pinned_windows_directory(
                &self.existing_anchor,
                "恢复输出根目录既有边界复核目录",
            )?;
            let current_identity = windows_object_identity_from_handle(
                &current_handle,
                "恢复输出根目录既有边界复核句柄",
            )?;
            if held_identity != self.existing_anchor_identity
                || current_identity != self.existing_anchor_identity
            {
                return Err("恢复输出根目录既有边界的文件系统身份发生变化".to_owned());
            }
        }
        #[cfg(unix)]
        if UnixObjectIdentity::from_metadata(&metadata) != self.existing_anchor_identity {
            return Err("恢复输出根目录既有边界的文件系统身份发生变化".to_owned());
        }
        // 其他平台只保留普通目录与重解析点检测，不声明具备稳定对象身份复核能力。
        Ok(())
    }
}

/// Unix 文件系统可稳定读取的文件或目录对象身份。
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnixObjectIdentity {
    /// Unix 文件系统设备号。
    device: u64,
    /// Unix inode 编号。
    inode: u64,
}

#[cfg(unix)]
impl UnixObjectIdentity {
    /// 从已确认的普通文件或目录元数据提取 Unix 设备号与 inode。
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// 在不创建任何目录的前提下解析输出根，并拒绝来源树内及重解析点边界路径。
fn validate_recovery_output_root(
    source_run: &Path,
    output_root: &Path,
) -> Result<ValidatedRecoveryOutputRoot, String> {
    let intended = lexical_absolute_path(output_root)?;
    let mut existing = intended.as_path();
    let existing_metadata = loop {
        match fs::symlink_metadata(existing) {
            Ok(metadata) => break metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing = existing
                    .parent()
                    .ok_or_else(|| "恢复输出根目录没有既有父目录".to_owned())?;
            }
            Err(error) => return Err(format!("无法读取恢复输出根目录边界：{error}")),
        }
    };
    if !existing_metadata.is_dir() || is_link_or_reparse(&existing_metadata) {
        return Err("恢复输出根目录的最近既有边界必须是普通目录且不能是重解析点".to_owned());
    }
    for ancestor in existing.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|error| format!("无法验证恢复输出根目录既有路径组件：{error}"))?;
        if is_link_or_reparse(&metadata) {
            return Err("恢复输出根目录不能经过符号链接、目录联接或重解析点".to_owned());
        }
    }
    let canonical_existing = fs::canonicalize(existing)
        .map_err(|error| format!("无法规范化恢复输出根目录既有边界：{error}"))?;
    let suffix = intended
        .strip_prefix(existing)
        .map_err(|_| "恢复输出根目录边界关系无效".to_owned())?;
    let resolved = canonical_existing.join(suffix);
    if path_is_same_or_descendant(&resolved, source_run) {
        return Err("恢复输出根目录不能等于或位于只读来源运行目录内".to_owned());
    }
    #[cfg(windows)]
    let existing_anchor_handle =
        open_pinned_windows_directory(&canonical_existing, "恢复输出根目录最近既有边界")?;
    #[cfg(windows)]
    let existing_anchor_identity = windows_object_identity_from_handle(
        &existing_anchor_handle,
        "恢复输出根目录最近既有边界句柄",
    )?;
    #[cfg(unix)]
    let existing_anchor_identity = UnixObjectIdentity::from_metadata(
        &fs::symlink_metadata(&canonical_existing)
            .map_err(|error| format!("无法读取恢复输出根目录规范既有边界：{error}"))?,
    );
    Ok(ValidatedRecoveryOutputRoot {
        resolved,
        existing_anchor: canonical_existing,
        #[cfg(any(unix, windows))]
        existing_anchor_identity,
        #[cfg(windows)]
        _existing_anchor_handle: existing_anchor_handle,
    })
}

/// 对一个或多个只读来源执行创建前后路径、锚点身份和实际父目录闭环校验。
fn create_verified_derived_target<F>(
    source_runs: &[&Path],
    output_root: &Path,
    run_id: &str,
    pre_create_hook: F,
) -> Result<ReportStore, String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    if source_runs.is_empty() {
        return Err("派生运行至少需要一个只读来源".to_owned());
    }
    validate_recovery_run_id(run_id)?;
    let mut initially_validated = Vec::with_capacity(source_runs.len());
    for source_run in source_runs {
        initially_validated.push(validate_recovery_output_root(source_run, output_root)?);
    }
    let expected_output_root = &initially_validated[0].resolved;
    if initially_validated
        .iter()
        .any(|validated| !paths_equal(&validated.resolved, expected_output_root))
    {
        return Err("派生运行输出根在多来源边界验证期间发生变化".to_owned());
    }

    pre_create_hook(expected_output_root)?;
    for validated in &initially_validated {
        validated.verify_existing_anchor()?;
    }
    let mut confirmed = Vec::with_capacity(source_runs.len());
    for source_run in source_runs {
        confirmed.push(validate_recovery_output_root(
            source_run,
            expected_output_root,
        )?);
    }
    if confirmed
        .iter()
        .any(|validated| !paths_equal(&validated.resolved, expected_output_root))
    {
        return Err("派生运行输出根在两次创建前验证之间发生变化".to_owned());
    }
    for validated in &confirmed {
        validated.verify_existing_anchor()?;
    }

    let destination = ReportStore::create(expected_output_root, run_id)?;
    if let Err(error) = destination.write_recovery_incomplete_marker() {
        return Err(retained_recovery_target_error(error));
    }
    let actual_output_root = destination
        .run_dir()
        .parent()
        .expect("成功创建的派生目标必然是输出根的单层子目录");
    let mut post_create = Vec::with_capacity(source_runs.len());
    for source_run in source_runs {
        post_create.push(
            validate_recovery_output_root(source_run, actual_output_root)
                .map_err(retained_recovery_target_error)?,
        );
    }
    if !paths_equal(actual_output_root, expected_output_root)
        || post_create.iter().any(|validated| {
            !paths_equal(&validated.resolved, actual_output_root)
                || !paths_equal(&validated.resolved, expected_output_root)
        })
    {
        return Err(retained_recovery_target_error(
            "派生运行输出根在验证与目标创建之间发生变化".to_owned(),
        ));
    }
    for validated in initially_validated
        .iter()
        .chain(confirmed.iter())
        .chain(post_create.iter())
    {
        validated
            .verify_existing_anchor()
            .map_err(retained_recovery_target_error)?;
    }
    Ok(destination)
}

/// 验证恢复运行标识只能形成输出根的单层普通子目录。
fn validate_recovery_run_id(run_id: &str) -> Result<(), String> {
    let mut components = Path::new(run_id).components();
    if run_id.is_empty()
        || !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err("恢复运行标识必须是单个普通路径组件".to_owned());
    }
    Ok(())
}

/// 拒绝带失败关闭标记的未完成恢复副本，避免其被恢复或再次作为来源。
fn reject_recovery_incomplete_marker(run_dir: &Path) -> Result<(), String> {
    let marker = run_dir.join(RECOVERY_INCOMPLETE_MARKER_FILE);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            if is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err("恢复副本失败关闭标记必须是普通文件且不能是重解析点".to_owned());
            }
            Err("指定运行目录是未完整验证的隔离恢复副本，拒绝打开".to_owned())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法检查恢复副本失败关闭标记：{error}")),
    }
}

/// 标记恢复副本已创建后的失败为安全保留，不对任何不确定路径执行递归删除。
fn retained_recovery_target_error(error: String) -> String {
    format!("{error}；已保留带失败关闭标记的未完成恢复目标，未执行递归删除")
}

/// 从操作系统 CSPRNG 生成每轮唯一盐；盐公开写盘但不会跨运行复用。
fn new_run_salt() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| format!("无法生成恢复凭据证明随机盐：{error}"))?;
    Ok(bytes.iter().fold(String::new(), |mut output, byte| {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
        output
    }))
}

/// 脱敏后的单个 Provider 配置快照。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderRecord {
    /// Provider 稳定标识。
    pub(crate) provider_id: String,
    /// 不包含认证信息、查询或用户信息的基础端点。
    pub(crate) base_endpoint_redacted: String,
    /// 以 Provider 凭据为 Key、对去凭据配置计算的域分离 HMAC-SHA256。
    pub(crate) config_fingerprint: String,
    /// 凭据的来源类别。
    pub(crate) credential_source: &'static str,
    /// 配置是否提供非空凭据。
    pub(crate) credential_present: bool,
}

impl ProviderRecord {
    /// 从仅在内存中持有凭据的 Provider 创建安全快照。
    pub(crate) fn from_provider(provider: &ProviderEntry) -> Result<Self, String> {
        Ok(Self {
            provider_id: provider.redact_text(&provider.id),
            base_endpoint_redacted: provider.redact_text(&provider.redacted_base_endpoint()?),
            config_fingerprint: provider.fingerprint()?,
            credential_source: "providers_file",
            credential_present: true,
        })
    }
}

/// 一个候选模型及其进入测试集合的来源。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CandidateModelRecord {
    /// 精确模型标识。
    pub(crate) model: String,
    /// 是否来自用户配置。
    pub(crate) configured: bool,
    /// 是否来自本次实时目录。
    pub(crate) discovered: bool,
    /// 是否来自本次运行的显式模型参数。
    pub(crate) explicit: bool,
    /// 是否仅来自恢复清单中已经冻结、但本次目录未再返回的模型集合。
    pub(crate) frozen_from_resume: bool,
}

/// 一个 Provider 的实时目录结果。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CatalogRecord {
    /// Provider 稳定标识。
    pub(crate) provider_id: String,
    /// 成功时为 `success`，否则为 `failed`。
    pub(crate) status: String,
    /// 实际尝试次数。
    pub(crate) attempts: usize,
    /// 全部尝试总耗时毫秒数。
    pub(crate) latency_ms: u128,
    /// 实际读取的目录页数。
    pub(crate) pages: usize,
    /// 去重前原始条目数。
    pub(crate) raw_count: usize,
    /// 缺少稳定 ID 的条目数。
    pub(crate) invalid_count: usize,
    /// 去重后的实时模型 ID。
    pub(crate) discovered_models: Vec<String>,
    /// 最终配置与目录并集。
    pub(crate) candidates: Vec<CandidateModelRecord>,
    /// 失败时的归一化错误。
    pub(crate) normalized_error: Option<NormalizedError>,
}

/// 单个模型、协议和线上响应模式的真实结果。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProbeRecord {
    /// 由当前 run 与原始 Provider/模型 tuple 生成的版本化结构摘要。
    pub(crate) stable_key: String,
    /// Provider 稳定标识。
    pub(crate) provider_id: String,
    /// 精确模型标识。
    pub(crate) model: String,
    /// 三种协议之一的稳定名称。
    pub(crate) protocol: String,
    /// `buffered` 或 `streaming`。
    pub(crate) response_mode: String,
    /// 当前记录独立验证的能力名称。
    pub(crate) capability: String,
    /// 请求使用的脱敏资源路径。
    pub(crate) endpoint_path: String,
    /// `passed`、`contract_violation`、`failed` 或 `skipped`。
    pub(crate) status: String,
    /// 实际 HTTP 调用次数。
    pub(crate) attempts: usize,
    /// 包含退避时间的总耗时毫秒数。
    pub(crate) latency_ms: u128,
    /// 本用例要求的固定合成文本；取消探测没有最终文本要求。
    pub(crate) expected_text: Option<String>,
    /// 实际请求使用的精确 Harness 标记；零请求跳过记录为空。
    pub(crate) synthetic_marker: Option<String>,
    /// 实际模型文本的长度与运行级 HMAC，不保存任意远端正文。
    pub(crate) actual_text_evidence: Option<ActualTextEvidence>,
    /// 不含原始模型正文和远端标识的成功响应证据。
    pub(crate) response: Option<ResponseEvidence>,
    /// 可复核当前能力语义的逐项断言。
    pub(crate) assertions: Vec<SemanticAssertion>,
    /// 仅取消探测产生的本地释放边界证据。
    pub(crate) cancellation: Option<CancellationEvidence>,
    /// 基础门禁阻止本用例执行时的未验证证据。
    pub(crate) skip_evidence: Option<SkipEvidence>,
    /// 当前记录写入的磁盘结构证据 Fixture 相对路径。
    pub(crate) fixture_paths: Vec<String>,
    /// 非空时表示该记录由隔离恢复从指定旧构建逐字节导入，并未由当前构建重发请求。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recovered_from: Option<RecoveredProbeOrigin>,
    /// 原始响应仍在内存时由同一 Adapter 复核的结果。
    pub(crate) fixture_replay: Option<FixtureReplayEvidence>,
    /// 失败时的归一化错误。
    pub(crate) normalized_error: Option<NormalizedError>,
    /// 每个真实 HTTP 交换对应且不含正文、任意名称或值的响应结构证据。
    pub(crate) wire_response_shapes: Vec<WireResponseShapeEvidence>,
    /// 仅在写 Fixture 前保留且永不直接进入结果 JSON 的线级交换。
    #[serde(skip)]
    pub(crate) wire_exchanges: Vec<WireExchange>,
    /// 仅在写 Fixture 前保存逐交换在线归一化期望，永不直接进入结果 JSON。
    #[serde(skip)]
    pub(crate) wire_exchange_outcomes: Vec<FixtureExchangeOutcome>,
}

/// 为测试失败路径提供不泄露 Provider、模型、正文或线级交换内容的调试表示。
impl std::fmt::Debug for ProbeRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProbeRecord")
            .finish_non_exhaustive()
    }
}

/// 不暴露模型正文但可用于核对当前进程在线解析结果的证据。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActualTextEvidence {
    /// UTF-8 正文的精确字节数。
    pub(crate) utf8_bytes: u64,
    /// 使用当前 Provider 凭据与版本化稳定键计算的 HMAC-SHA256。
    pub(crate) hmac_sha256: String,
}

impl ActualTextEvidence {
    /// 从内存中的真实文本构造不可逆的运行级证据。
    pub(crate) fn from_text(provider: &ProviderEntry, stable_key: &str, text: &str) -> Self {
        Self {
            utf8_bytes: u64::try_from(text.len()).expect("实际响应文本的内存长度必须能表示为 u64"),
            hmac_sha256: provider.response_text_proof(stable_key, text),
        }
    }
}

impl ProbeRecord {
    /// 返回写入记录且由原始身份 tuple 生成的唯一恢复键。
    pub(crate) fn stable_key(&self) -> String {
        self.stable_key.clone()
    }

    /// 判断记录是否是同一运行内必须直接复用的已提交终态。
    fn reusable(&self) -> bool {
        matches!(
            self.status.as_str(),
            "passed" | "contract_violation" | "failed" | "skipped" | "unverified"
        )
    }
}

/// 隔离恢复在创建目标目录前得到的完整导入与重新请求计划。
struct RecoveryImportPlan {
    /// 可以按原事实身份导入且不会重新发送请求的记录。
    records: BTreeMap<String, ProbeRecord>,
    /// 需要逐字节复制到新运行的唯一 Fixture 路径。
    fixture_paths: BTreeSet<String>,
    /// 旧契约下无法独立复核、必须在新运行重新请求的记录。
    rerun_records: Vec<RecoveryRerunRecord>,
    /// 本次恢复写入运行级来源声明的固定策略。
    policy: &'static str,
}

/// 校验单条恢复记录时使用的只读 Fixture 预期。
struct RecordFixtureExpectations<'a> {
    /// 记录响应必须包含的唯一探测标记。
    marker: &'a str,
    /// 来源运行目录中已经过路径与类型校验的 Fixture 集合。
    disk_fixtures: &'a BTreeSet<String>,
}

/// 以版本域和长度前缀编码原始身份，避免分隔符碰撞和脱敏值碰撞。
pub(crate) fn probe_stable_key(
    run_id: &str,
    provider_id: &str,
    model: &str,
    protocol: &str,
    response_mode: &str,
    capability: &str,
) -> String {
    let mut bytes = b"keencode-provider-probe-key-v1".to_vec();
    for part in [
        run_id,
        provider_id,
        model,
        protocol,
        response_mode,
        capability,
    ] {
        let part_len = u64::try_from(part.len()).expect("探测身份字段长度必须能表示为 u64");
        bytes.extend_from_slice(&part_len.to_be_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    format!("probe-key-v1:sha256:{}", hex_digest(&bytes))
}

/// 对版本域和每个长度前缀字段计算 SHA-256，避免分隔符碰撞和跨用途复用。
pub(crate) fn domain_separated_hex(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut bytes = Vec::from(domain);
    for part in parts {
        let part_len = u64::try_from(part.len()).expect("域分离字段长度必须能表示为 u64");
        bytes.extend_from_slice(&part_len.to_be_bytes());
        bytes.extend_from_slice(part);
    }
    hex_digest(&bytes)
}

/// 从完整版本化探测键派生主请求标记，诊断与普通探测使用不同前缀和 Hash 域。
pub(crate) fn marker_from_probe_stable_key(stable_key: &str, diagnostic: bool) -> String {
    let (prefix, domain) = if diagnostic {
        (
            "KC_DIAG_",
            b"keencode-provider-diagnostic-marker-v1".as_slice(),
        )
    } else {
        ("KC_OK_", b"keencode-provider-probe-marker-v1".as_slice())
    };
    let digest = domain_separated_hex(domain, &[stable_key.as_bytes()]);
    format!("{prefix}{}", &digest[..16])
}

/// 从主标记使用独立版本域派生多轮首轮标记，禁止形成可交换的裸摘要。
pub(crate) fn first_turn_marker(main_marker: &str) -> String {
    let digest = domain_separated_hex(
        b"keencode-provider-first-turn-marker-v1",
        &[main_marker.as_bytes()],
    );
    format!("KC_FIRST_{}", &digest[..16])
}

/// 一个不会保存任意远端正文的响应事实摘要。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResponseEvidence {
    /// Provider 是否报告了非空响应标识，而不保存标识本身。
    pub(crate) response_id_present: bool,
    /// 已脱敏的 Provider 实际模型名称。
    pub(crate) reported_model_redacted: Option<String>,
    /// 稳定的统一结束原因。
    pub(crate) stop_reason: String,
    /// 按响应顺序记录的内容块类型。
    pub(crate) content_block_types: Vec<String>,
    /// 普通文本块数量。
    pub(crate) text_block_count: usize,
    /// 推理块数量。
    pub(crate) reasoning_block_count: usize,
    /// 工具调用块数量。
    pub(crate) tool_call_count: usize,
    /// Provider 明确报告的可选 Token 用量。
    pub(crate) usage: TokenUsage,
}

impl ResponseEvidence {
    /// 从已解析响应提取不包含任意正文和调用参数的事实摘要。
    pub(crate) fn from_response(response: &ModelResponse, provider: &ProviderEntry) -> Self {
        let mut content_block_types = Vec::with_capacity(response.content.len());
        let mut text_block_count = 0;
        let mut reasoning_block_count = 0;
        let mut tool_call_count = 0;
        for block in &response.content {
            let kind = match block {
                ContentBlock::Text { .. } => {
                    text_block_count += 1;
                    "text"
                }
                ContentBlock::Reasoning { .. } => {
                    reasoning_block_count += 1;
                    "reasoning"
                }
                ContentBlock::Image { .. } => "image",
                ContentBlock::ToolCall { .. } => {
                    tool_call_count += 1;
                    "tool_call"
                }
                ContentBlock::ToolResult { .. } => "tool_result",
            };
            content_block_types.push(kind.to_owned());
        }
        Self {
            response_id_present: response
                .metadata
                .response_id
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty()),
            reported_model_redacted: response
                .metadata
                .model
                .as_deref()
                .map(|value| provider.redact_text(value)),
            stop_reason: stop_reason_name(&response.stop_reason).to_owned(),
            content_block_types,
            text_block_count,
            reasoning_block_count,
            tool_call_count,
            usage: response.usage.clone(),
        }
    }
}

/// 单项语义契约断言及其不含远端正文的说明。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SemanticAssertion {
    /// 可用于自动聚合的稳定断言名称。
    pub(crate) name: String,
    /// 当前断言是否通过。
    pub(crate) passed: bool,
    /// 不包含原始模型输出的中文事实说明。
    pub(crate) detail: String,
}

impl SemanticAssertion {
    /// 创建一项仅记录布尔结果和固定说明的断言。
    pub(crate) fn new(name: &str, passed: bool, detail: &str) -> Self {
        Self {
            name: name.to_owned(),
            passed,
            detail: detail.to_owned(),
        }
    }
}

/// 丢弃在途调用时能够直接证明的本地取消事实。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CancellationEvidence {
    /// 预定触发本地取消的窗口毫秒数。
    pub(crate) cancel_after_ms: u64,
    /// 取消计时器是否先于完整响应获胜。
    pub(crate) local_future_dropped: bool,
    /// 流式模式在取消前是否至少收到一个统一事件。
    pub(crate) first_event_received: bool,
    /// 远端是否在取消窗口前已经完整结束。
    pub(crate) completed_before_cancel: bool,
    /// 本地 Future 或 Stream 被丢弃前的实测耗时。
    pub(crate) observed_latency_ms: u128,
    /// 本探测不能证明远端停止生成或计费，固定为 `false`。
    pub(crate) remote_termination_proven: bool,
}

/// 不含远端任意正文且可用于统计的统一错误快照。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NormalizedError {
    /// 稳定错误种类。
    pub(crate) kind: String,
    /// 原始错误说明清理后的长度与截断事实，不保存正文或可枚举摘要。
    pub(crate) message_evidence: ErrorMessageEvidence,
    /// 上层是否可以有限重试。
    pub(crate) retryable: bool,
    /// 远端 HTTP 状态；不可获得时为空。
    pub(crate) http_status: Option<u16>,
}

/// 任意远端错误说明的非正文证据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ErrorMessageEvidence {
    /// 清理控制字符并限制长度后的错误说明 UTF-8 字节数。
    pub(crate) utf8_bytes: u64,
    /// 凭据脱敏后的错误说明是否超过固定字符上限。
    pub(crate) truncated: bool,
}

impl ErrorMessageEvidence {
    /// 从内存错误说明构造只含长度与截断事实的不可枚举证据。
    pub(crate) fn from_text(text: &str) -> Self {
        let mut normalized = String::new();
        let mut truncated = false;
        for (index, character) in text.chars().enumerate() {
            if index == 1000 {
                truncated = true;
                break;
            }
            normalized.push(if character.is_control() {
                ' '
            } else {
                character
            });
        }
        if normalized.trim().is_empty() {
            normalized = "模型服务返回空错误".to_owned();
        }
        Self {
            utf8_bytes: u64::try_from(normalized.len()).expect("错误说明长度必须能表示为 u64"),
            truncated,
        }
    }
}

/// 基础文本门禁阻止高级能力请求时保留的无敏感证据。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SkipEvidence {
    /// 固定为 `unverified`，明确本能力没有发起线上请求。
    pub(crate) verification: String,
    /// 不含远端正文的稳定跳过原因。
    pub(crate) reason: String,
    /// 阻止本能力的基础文本记录稳定键。
    pub(crate) blocked_by: String,
    /// 基础文本记录的最终状态。
    pub(crate) gate_status: String,
    /// 基础文本失败时的稳定错误种类。
    pub(crate) error_kind: Option<String>,
    /// 基础文本失败是否属于可重试错误。
    pub(crate) retryable: Option<bool>,
    /// 基础文本失败时可获得的 HTTP 状态。
    pub(crate) http_status: Option<u16>,
}

/// 一条探测记录的内存 Adapter 复核及磁盘可复核边界结论。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FixtureReplayEvidence {
    /// `passed`、`failed`、`unavailable` 或 `not_applicable`。
    pub(crate) status: String,
    /// 本能力实际捕获的 HTTP 交换数。
    pub(crate) exchange_count: usize,
    /// 已由目标协议 Adapter 成功离线归约的交换数。
    pub(crate) replayed_exchanges: usize,
    /// 不包含正文的稳定原因；完整通过时为空。
    pub(crate) reason: Option<String>,
}

/// 一次运行的确定性统计。
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SummaryRecord {
    /// Provider 兼容与本地 Conformance 的全部事实记录数。
    pub(crate) total_probes: usize,
    /// 可进入 Provider、模型与协议远端兼容口径的记录数。
    pub(crate) provider_compatibility_probes: usize,
    /// Provider 远端兼容口径中实际执行的记录数。
    pub(crate) executed_probes: usize,
    /// Provider 远端兼容口径中精确文本和结构均通过的记录数。
    pub(crate) passed: usize,
    /// Provider 远端兼容口径中请求成功但语义契约失败的记录数。
    pub(crate) contract_violations: usize,
    /// Provider 远端兼容口径中的归一化错误记录数。
    pub(crate) failed: usize,
    /// Provider 远端兼容口径中被基础门禁阻止的记录数。
    pub(crate) skipped: usize,
    /// Provider 远端兼容口径中不能得出能力结论的记录数。
    pub(crate) unverified: usize,
    /// 实际发送到目标 Provider 的 HTTP 尝试总数；不包含本地回环请求。
    pub(crate) total_attempts: usize,
    /// 只发送到 Harness 本地回环服务的 HTTP 尝试总数。
    pub(crate) local_loopback_attempts: usize,
    /// Provider 明确报告的输入 Token 总和。
    pub(crate) reported_input_tokens: u64,
    /// Provider 明确报告的输出 Token 总和。
    pub(crate) reported_output_tokens: u64,
    /// 不进入 Provider/model 兼容率的客户端与 Adapter 本地 Conformance 汇总。
    pub(crate) local_conformance: LocalConformanceSummaryRecord,
    /// 按能力名称聚合的确定性统计。
    pub(crate) by_capability: BTreeMap<String, CapabilitySummaryRecord>,
}

/// 客户端与 Adapter 本地 Conformance 的独立状态汇总。
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalConformanceSummaryRecord {
    /// 本地 Conformance 记录总数。
    pub(crate) total: usize,
    /// 实际执行的本地 Conformance 记录数。
    pub(crate) executed: usize,
    /// 本地 Conformance 通过数。
    pub(crate) passed: usize,
    /// 本地 Conformance 契约不符合数。
    pub(crate) contract_violations: usize,
    /// 本地 Conformance 失败数。
    pub(crate) failed: usize,
    /// 本地 Conformance 跳过数。
    pub(crate) skipped: usize,
    /// 本地 Conformance 未验证数。
    pub(crate) unverified: usize,
}

/// 单一能力的运行结果计数。
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CapabilitySummaryRecord {
    /// `provider_compatibility_remote` 或明确的 `*_conformance_local_only` 范围。
    pub(crate) scope: String,
    /// 当前能力的探测总数。
    pub(crate) total: usize,
    /// 当前能力实际执行的探测数。
    pub(crate) executed: usize,
    /// 当前能力通过的探测数。
    pub(crate) passed: usize,
    /// 当前能力的语义契约失败数。
    pub(crate) contract_violations: usize,
    /// 当前能力的调用或解析失败数。
    pub(crate) failed: usize,
    /// 当前能力被基础门禁阻止的记录数。
    pub(crate) skipped: usize,
    /// 当前能力尚未验证的记录数。
    pub(crate) unverified: usize,
}

/// 返回能力事实所属的固定验证范围。
fn capability_scope(capability: &str) -> &'static str {
    match capability {
        "stream_interruption" => "adapter_conformance_local_only",
        "cancellation" => "client_conformance_local_only",
        _ => "provider_compatibility_remote",
    }
}

/// 判断能力是否只能证明本地客户端或 Adapter 行为。
fn is_local_conformance(capability: &str) -> bool {
    capability_scope(capability) != "provider_compatibility_remote"
}

/// 判断跳过记录是否同时属于未验证状态。
fn skipped_is_unverified(probe: &ProbeRecord) -> bool {
    probe
        .skip_evidence
        .as_ref()
        .is_some_and(|evidence| evidence.verification == "unverified")
}

impl LocalConformanceSummaryRecord {
    /// 把一条 local-only 记录累加到独立汇总，不污染 Provider 兼容率。
    fn observe(&mut self, probe: &ProbeRecord) {
        self.total += 1;
        if probe.status == "skipped" {
            self.skipped += 1;
            if skipped_is_unverified(probe) {
                self.unverified += 1;
            }
            return;
        }
        self.executed += 1;
        match probe.status.as_str() {
            "passed" => self.passed += 1,
            "contract_violation" => self.contract_violations += 1,
            "unverified" => self.unverified += 1,
            _ => self.failed += 1,
        }
    }
}

impl CapabilitySummaryRecord {
    /// 把一条记录累加到对应能力，并保留该能力固定的验证范围。
    fn observe(&mut self, probe: &ProbeRecord) {
        let scope = capability_scope(&probe.capability);
        if self.scope.is_empty() {
            self.scope = scope.to_owned();
        } else {
            debug_assert_eq!(self.scope, scope);
        }
        self.total += 1;
        if probe.status == "skipped" {
            self.skipped += 1;
            if skipped_is_unverified(probe) {
                self.unverified += 1;
            }
            return;
        }
        self.executed += 1;
        match probe.status.as_str() {
            "passed" => self.passed += 1,
            "contract_violation" => self.contract_violations += 1,
            "unverified" => self.unverified += 1,
            _ => self.failed += 1,
        }
    }
}

impl SummaryRecord {
    /// 从事实记录重新聚合统计；缺失 Usage 不按零上报，只不进入总和。
    fn from_probes(probes: &[ProbeRecord]) -> Self {
        let mut summary = Self {
            total_probes: probes.len(),
            ..Self::default()
        };
        for probe in probes {
            if probe.capability == "stream_interruption" {
                summary.local_loopback_attempts += probe.attempts;
            } else {
                summary.total_attempts += probe.attempts;
            }
            let capability = summary
                .by_capability
                .entry(probe.capability.clone())
                .or_default();
            capability.observe(probe);
            if is_local_conformance(&probe.capability) {
                summary.local_conformance.observe(probe);
                if let Some(response) = &probe.response {
                    add_usage(&mut summary, &response.usage);
                }
                continue;
            }
            summary.provider_compatibility_probes += 1;
            if probe.status == "skipped" {
                summary.skipped += 1;
                if skipped_is_unverified(probe) {
                    summary.unverified += 1;
                }
                continue;
            }
            summary.executed_probes += 1;
            match probe.status.as_str() {
                "passed" => {
                    summary.passed += 1;
                }
                "contract_violation" => {
                    summary.contract_violations += 1;
                }
                "unverified" => {
                    summary.unverified += 1;
                }
                _ => {
                    summary.failed += 1;
                }
            }
            if let Some(response) = &probe.response {
                add_usage(&mut summary, &response.usage);
            }
        }
        summary
    }
}

/// 运行目录后代路径在打开前必须满足的最终节点类型。
#[derive(Clone, Copy)]
enum RunPathKind {
    /// 最终节点必须是普通目录。
    Directory,
    /// 最终节点必须是普通文件。
    File,
}

/// 判断元数据是否代表符号链接、Windows 目录联接或其他重解析点。
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Linux 与 macOS `openat` 的最小 FFI 边界；所有路径均限制为单个相对组件。
#[cfg(unix)]
#[allow(unsafe_code)]
mod unix_open_ffi {
    use std::ffi::{CString, OsStr, c_char, c_int, c_uint};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    use super::{StableFileAccess, StableFileCreation};

    /// Linux `O_CREAT`。
    #[cfg(target_os = "linux")]
    const O_CREAT: c_int = 0o100;
    /// Linux `O_EXCL`。
    #[cfg(target_os = "linux")]
    const O_EXCL: c_int = 0o200;
    /// Linux `O_APPEND`。
    #[cfg(target_os = "linux")]
    const O_APPEND: c_int = 0o2000;
    /// Linux `O_DIRECTORY`。
    #[cfg(target_os = "linux")]
    const O_DIRECTORY: c_int = 0o200000;
    /// Linux `O_NOFOLLOW`。
    #[cfg(target_os = "linux")]
    const O_NOFOLLOW: c_int = 0o400000;
    /// Linux `O_CLOEXEC`。
    #[cfg(target_os = "linux")]
    const O_CLOEXEC: c_int = 0o2000000;

    /// macOS `O_CREAT`。
    #[cfg(target_os = "macos")]
    const O_CREAT: c_int = 0x0200;
    /// macOS `O_EXCL`。
    #[cfg(target_os = "macos")]
    const O_EXCL: c_int = 0x0800;
    /// macOS `O_APPEND`。
    #[cfg(target_os = "macos")]
    const O_APPEND: c_int = 0x0008;
    /// macOS `O_DIRECTORY`。
    #[cfg(target_os = "macos")]
    const O_DIRECTORY: c_int = 0x0010_0000;
    /// macOS `O_NOFOLLOW`。
    #[cfg(target_os = "macos")]
    const O_NOFOLLOW: c_int = 0x0100;
    /// macOS `O_CLOEXEC`。
    #[cfg(target_os = "macos")]
    const O_CLOEXEC: c_int = 0x0100_0000;

    /// POSIX 只读访问值。
    const O_RDONLY: c_int = 0;
    /// POSIX 只写访问值。
    const O_WRONLY: c_int = 1;
    /// POSIX 读写访问值。
    const O_RDWR: c_int = 2;
    /// 新建 Harness 文件只授予当前用户读写权限。
    const OWNER_READ_WRITE_MODE: c_uint = 0o600;

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    compile_error!("KeenCode Provider Harness 的 Unix 稳定打开目前仅支持 Linux 与 macOS");

    unsafe extern "C" {
        /// 相对于已经固定的目录文件描述符打开最终节点。
        fn openat(directory_fd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    }

    /// 把单个 Unix 文件名转换为不含 NUL 的 C 字符串。
    fn component_c_string(component: &OsStr, label: &str) -> Result<CString, String> {
        let bytes = component.as_bytes();
        if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
            return Err(format!("{label} 必须是单个普通 Unix 路径组件"));
        }
        CString::new(bytes).map_err(|_| format!("{label} 路径组件不能包含 NUL 字节"))
    }

    /// 相对于固定目录打开不跟随最终链接的普通文件句柄。
    pub(super) fn open_regular_at(
        directory: &File,
        component: &OsStr,
        access: StableFileAccess,
        creation: StableFileCreation,
        label: &str,
    ) -> Result<File, String> {
        let component = component_c_string(component, label)?;
        let access_flags = match access {
            StableFileAccess::ReadOnly | StableFileAccess::Verify => O_RDONLY,
            StableFileAccess::ReadWrite | StableFileAccess::Lock => O_RDWR,
            StableFileAccess::Append => O_WRONLY | O_APPEND,
        };
        let creation_flags = match creation {
            StableFileCreation::Existing => 0,
            StableFileCreation::CreateIfMissing => O_CREAT,
        };
        // SAFETY: directory_fd 在调用期间有效；component 是单个 NUL 结尾组件；
        // O_NOFOLLOW 禁止最终符号链接，mode 只在 O_CREAT 生效。
        let descriptor = unsafe {
            openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                access_flags | creation_flags | O_NOFOLLOW | O_CLOEXEC,
                OWNER_READ_WRITE_MODE,
            )
        };
        if descriptor < 0 {
            return Err(format!(
                "无法相对于固定目录安全打开 {label}：{}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: openat 成功返回当前函数独占拥有的新文件描述符。
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    /// 相对于固定目录打开不跟随最终链接的普通目录句柄。
    pub(super) fn open_directory_at(
        directory: &File,
        component: &OsStr,
        label: &str,
    ) -> Result<File, String> {
        let component = component_c_string(component, label)?;
        // SAFETY: directory_fd 在调用期间有效；component 是单个 NUL 结尾组件；
        // O_DIRECTORY 与 O_NOFOLLOW 共同要求最终节点为真实目录。
        let descriptor = unsafe {
            openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC,
                0,
            )
        };
        if descriptor < 0 {
            return Err(format!(
                "无法相对于固定目录安全打开 {label}：{}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: openat 成功返回当前函数独占拥有的新文件描述符。
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    /// 返回绝对路径打开时使用的 Unix no-follow 与 close-on-exec 标志。
    pub(super) const fn absolute_no_follow_flags() -> c_int {
        O_NOFOLLOW | O_CLOEXEC
    }

    /// 返回绝对目录打开时附加的 Unix 目录标志。
    pub(super) const fn absolute_directory_flags() -> c_int {
        O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
    }

    /// 返回新文件排他创建需要的标志，供未来严格创建入口复用。
    #[allow(dead_code)]
    pub(super) const fn exclusive_create_flags() -> c_int {
        O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC
    }
}

/// Windows 文件或目录在指定文件系统卷内的真实对象身份。
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowsObjectIdentity {
    /// 文件系统卷序列号。
    volume_serial_number: u64,
    /// 当前目录不可由时间戳或属性伪造的 128 位文件标识。
    file_id: [u8; 16],
}

/// Windows FileId 查询的唯一 FFI 边界；安全封装保证句柄有效、C 布局和缓冲区大小精确。
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_file_identity_ffi {
    use std::fs::File;
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::{AsRawHandle, RawHandle};

    use super::WindowsObjectIdentity;

    /// `GetFileInformationByHandleEx` 查询真实文件标识使用的 `FileIdInfo` 枚举值。
    const WINDOWS_FILE_ID_INFO_CLASS: i32 = 18;

    /// Windows FFI 返回的 128 位文件系统对象标识。
    #[repr(C)]
    struct WindowsFileId128 {
        /// 文件系统定义的 16 字节不透明对象标识。
        identifier: [u8; 16],
    }

    /// Windows FFI 返回的卷序列号与 128 位文件标识组合。
    #[repr(C)]
    struct WindowsFileIdInfo {
        /// 文件系统卷序列号。
        volume_serial_number: u64,
        /// 当前句柄所指对象的 128 位文件标识。
        file_id: WindowsFileId128,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// 从已打开 Windows 文件或目录句柄查询指定类别的底层文件系统信息。
        fn GetFileInformationByHandleEx(
            file_handle: RawHandle,
            file_information_class: i32,
            file_information: *mut std::ffi::c_void,
            buffer_size: u32,
        ) -> i32;
    }

    /// 以安全接口查询有效目录句柄的卷序列号与 128 位文件标识。
    pub(super) fn query_directory_identity(
        directory: &File,
        label: &str,
    ) -> Result<WindowsObjectIdentity, String> {
        let buffer_size = u32::try_from(size_of::<WindowsFileIdInfo>())
            .map_err(|_| format!("{label} 文件标识缓冲区大小无法表示为 Windows DWORD"))?;
        let mut information = MaybeUninit::<WindowsFileIdInfo>::uninit();
        // SAFETY: 缓冲区按 FILE_ID_INFO 的 C 布局和精确大小分配，句柄在调用期间保持有效。
        let succeeded = unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                WINDOWS_FILE_ID_INFO_CLASS,
                information.as_mut_ptr().cast(),
                buffer_size,
            )
        };
        if succeeded == 0 {
            return Err(format!(
                "无法从 {label} 取得 Windows 真实文件标识：{}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: Windows 已返回成功并完整初始化与传入大小一致的 FILE_ID_INFO 缓冲区。
        let information = unsafe { information.assume_init() };
        Ok(WindowsObjectIdentity {
            volume_serial_number: information.volume_serial_number,
            file_id: information.file_id.identifier,
        })
    }
}

/// Windows 下从已打开目录句柄取得卷序列号与 128 位真实文件标识。
#[cfg(windows)]
fn windows_object_identity_from_handle(
    directory: &File,
    label: &str,
) -> Result<WindowsObjectIdentity, String> {
    windows_file_identity_ffi::query_directory_identity(directory, label)
}

/// Windows 下按统一访问、创建和共享策略打开不跟随最终重解析点的普通文件句柄。
#[cfg(windows)]
fn open_windows_regular_file_handle_with(
    path: &Path,
    access: StableFileAccess,
    creation: StableFileCreation,
    label: &str,
) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    match access {
        StableFileAccess::ReadOnly | StableFileAccess::Verify => {
            options.read(true);
        }
        StableFileAccess::ReadWrite | StableFileAccess::Lock => {
            options.read(true).write(true);
        }
        StableFileAccess::Append => {
            options.append(true);
        }
    }
    if matches!(creation, StableFileCreation::CreateIfMissing) {
        options.create(true);
    }
    let share_mode = if matches!(access, StableFileAccess::Lock | StableFileAccess::Verify) {
        // 锁文件由文件锁决定唯一所有者；身份复核句柄必须兼容当前进程已持有的写句柄。
        FILE_SHARE_READ | FILE_SHARE_WRITE
    } else {
        // 事实文件只共享读取，同时阻止内容写入、删除、重命名或替换。
        FILE_SHARE_READ
    };
    options
        .share_mode(share_mode)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("无法安全打开并固定 {label}：{error}"))
}

/// Windows 下以只读、拒绝写共享且不跟随最终重解析点的方式打开普通文件。
#[cfg(windows)]
fn open_windows_regular_file_handle(path: &Path, label: &str) -> Result<File, String> {
    open_windows_regular_file_handle_with(
        path,
        StableFileAccess::ReadOnly,
        StableFileCreation::Existing,
        label,
    )
}

/// Unix 下按统一访问与创建策略打开不跟随最终符号链接的绝对普通文件路径。
#[cfg(unix)]
fn open_unix_regular_file_handle(
    path: &Path,
    access: StableFileAccess,
    creation: StableFileCreation,
    label: &str,
) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    match access {
        StableFileAccess::ReadOnly | StableFileAccess::Verify => {
            options.read(true);
        }
        StableFileAccess::ReadWrite | StableFileAccess::Lock => {
            options.read(true).write(true);
        }
        StableFileAccess::Append => {
            options.append(true);
        }
    }
    if matches!(creation, StableFileCreation::CreateIfMissing) {
        options.create(true).mode(0o600);
    }
    options
        .custom_flags(unix_open_ffi::absolute_no_follow_flags())
        .open(path)
        .map_err(|error| format!("无法安全打开并固定 {label}：{error}"))
}

/// Windows 下按固定共享与重解析点策略打开一个目录句柄。
#[cfg(windows)]
fn open_windows_directory_handle(path: &Path, label: &str) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        // 允许运行时继续写入子项，但故意不共享删除访问，从而阻止目录重命名与替换。
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("无法打开并固定 {label}：{error}"))
}

/// Windows 下确认一个已打开目录句柄指向非重解析普通目录。
#[cfg(windows)]
fn validate_open_windows_directory_handle(directory: &File, label: &str) -> Result<(), String> {
    let metadata = directory
        .metadata()
        .map_err(|error| format!("无法读取 {label} 已打开句柄元数据：{error}"))?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err(format!("{label} 已打开句柄不是普通目录或指向重解析点"));
    }
    Ok(())
}

/// Windows 下以共享读写但拒绝共享删除的句柄固定普通目录，并用真实文件 ID 闭合竞态。
#[cfg(windows)]
fn open_pinned_windows_directory(path: &Path, label: &str) -> Result<File, String> {
    let before_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取 {label} 打开前元数据：{error}"))?;
    if !before_metadata.is_dir() || is_link_or_reparse(&before_metadata) {
        return Err(format!("{label} 必须是普通目录且不能是重解析点"));
    }
    let directory = open_windows_directory_handle(path, label)?;
    validate_open_windows_directory_handle(&directory, label)?;
    let opened_identity = windows_object_identity_from_handle(&directory, label)?;
    let after_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取 {label} 打开后元数据：{error}"))?;
    if !after_metadata.is_dir() || is_link_or_reparse(&after_metadata) {
        return Err(format!("{label} 在打开后已变为非普通目录或重解析点"));
    }
    let verification = open_windows_directory_handle(path, &format!("{label} 打开后复核目录"))?;
    validate_open_windows_directory_handle(&verification, &format!("{label} 打开后复核目录"))?;
    let verified_identity =
        windows_object_identity_from_handle(&verification, &format!("{label} 打开后复核句柄"))?;
    if verified_identity != opened_identity {
        return Err(format!("{label} 在打开后已被替换"));
    }
    Ok(directory)
}

/// Windows 下按父目录到子目录顺序持有报告固定布局的全部目录句柄。
#[cfg(windows)]
struct WindowsReportDirectoryPins {
    /// 固定运行目录的输出根，阻止生命周期内重命名或替换。
    _output_root: File,
    /// 固定当前运行目录，阻止生命周期内重命名或替换。
    _run_dir: File,
    /// 固定不可变 Fixture 目录，阻止生命周期内重命名或替换。
    _fixtures_dir: File,
    /// 固定脱敏日志目录，阻止生命周期内重命名或替换。
    _sanitized_logs_dir: File,
}

#[cfg(windows)]
impl WindowsReportDirectoryPins {
    /// 从输出根开始按父先子后顺序打开并固定报告目录链。
    fn open(run_dir: &Path) -> Result<Self, String> {
        let output_root = run_dir
            .parent()
            .ok_or_else(|| "运行目录缺少输出根父目录".to_owned())?;
        let output_root = open_pinned_windows_directory(output_root, "真实测试输出根目录")?;
        let run_directory = open_pinned_windows_directory(run_dir, "真实测试运行目录")?;
        let fixtures_dir =
            open_pinned_windows_directory(&run_dir.join("fixtures"), "Fixture 目录")?;
        let sanitized_logs_dir =
            open_pinned_windows_directory(&run_dir.join("sanitized-logs"), "脱敏日志目录")?;
        Ok(Self {
            _output_root: output_root,
            _run_dir: run_directory,
            _fixtures_dir: fixtures_dir,
            _sanitized_logs_dir: sanitized_logs_dir,
        })
    }
}

/// Unix 下以 no-follow 语义固定一个绝对普通目录，并闭合最终节点检查与打开竞态。
#[cfg(unix)]
fn open_pinned_unix_directory(path: &Path, label: &str) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let before = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取 {label} 打开前元数据：{error}"))?;
    if !before.is_dir() || is_link_or_reparse(&before) {
        return Err(format!("{label} 必须是普通目录且不能是符号链接"));
    }
    let expected = UnixObjectIdentity::from_metadata(&before);
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(unix_open_ffi::absolute_directory_flags())
        .open(path)
        .map_err(|error| format!("无法安全打开并固定 {label}：{error}"))?;
    let opened = directory
        .metadata()
        .map_err(|error| format!("无法读取 {label} 已打开句柄元数据：{error}"))?;
    if !opened.is_dir()
        || is_link_or_reparse(&opened)
        || UnixObjectIdentity::from_metadata(&opened) != expected
    {
        return Err(format!("{label} 在路径检查与打开之间被替换"));
    }
    let after = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取 {label} 打开后元数据：{error}"))?;
    if !after.is_dir()
        || is_link_or_reparse(&after)
        || UnixObjectIdentity::from_metadata(&after) != expected
    {
        return Err(format!("{label} 在打开后已被替换"));
    }
    Ok(directory)
}

/// Unix 下确认相对打开的目录句柄是普通目录。
#[cfg(unix)]
fn validate_open_unix_directory_handle(directory: &File, label: &str) -> Result<(), String> {
    let metadata = directory
        .metadata()
        .map_err(|error| format!("无法读取 {label} 已打开句柄元数据：{error}"))?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err(format!("{label} 已打开句柄不是普通目录"));
    }
    Ok(())
}

/// Unix 下按输出根、运行目录和两个固定子目录保存原始来源目录文件描述符。
#[cfg(unix)]
struct UnixReportDirectoryPins {
    /// 固定规范输出根，作为运行目录相对打开的唯一父目录。
    output_root: File,
    /// 固定当前运行目录，所有事实文件均相对此描述符打开。
    run_dir: File,
    /// 固定不可变 Fixture 目录。
    fixtures_dir: File,
    /// 固定脱敏日志目录。
    sanitized_logs_dir: File,
}

/// 限制 ReportStore 文件访问只能落在运行根或两个固定子目录的直属普通文件。
fn validate_fixed_report_file_relative_path(relative: &Path, label: &str) -> Result<(), String> {
    use std::path::Component;

    let components = relative.components().collect::<Vec<_>>();
    match components.as_slice() {
        [Component::Normal(_)] => Ok(()),
        [Component::Normal(directory), Component::Normal(_)]
            if *directory == std::ffi::OsStr::new("fixtures")
                || *directory == std::ffi::OsStr::new("sanitized-logs") =>
        {
            Ok(())
        }
        _ => Err(format!("{label} 必须位于报告固定目录的直属层级")),
    }
}

#[cfg(unix)]
impl UnixReportDirectoryPins {
    /// 从规范输出根开始逐级使用 `openat + O_NOFOLLOW` 固定报告目录链。
    fn open(run_dir: &Path) -> Result<Self, String> {
        let output_root_path = run_dir
            .parent()
            .ok_or_else(|| "运行目录缺少输出根父目录".to_owned())?;
        let run_name = run_dir
            .file_name()
            .ok_or_else(|| "运行目录缺少最终路径组件".to_owned())?;
        let output_root = open_pinned_unix_directory(output_root_path, "真实测试输出根目录")?;
        let run_directory =
            unix_open_ffi::open_directory_at(&output_root, run_name, "真实测试运行目录")?;
        validate_open_unix_directory_handle(&run_directory, "真实测试运行目录")?;
        let fixtures_dir = unix_open_ffi::open_directory_at(
            &run_directory,
            std::ffi::OsStr::new("fixtures"),
            "Fixture 目录",
        )?;
        validate_open_unix_directory_handle(&fixtures_dir, "Fixture 目录")?;
        let sanitized_logs_dir = unix_open_ffi::open_directory_at(
            &run_directory,
            std::ffi::OsStr::new("sanitized-logs"),
            "脱敏日志目录",
        )?;
        validate_open_unix_directory_handle(&sanitized_logs_dir, "脱敏日志目录")?;
        let pins = Self {
            output_root,
            run_dir: run_directory,
            fixtures_dir,
            sanitized_logs_dir,
        };
        pins.verify_layout(run_dir)?;
        Ok(pins)
    }

    /// 复核路径命名仍逐级指向已固定的输出根、运行目录和两个固定子目录。
    fn verify_layout(&self, run_dir: &Path) -> Result<(), String> {
        let output_root_path = run_dir
            .parent()
            .ok_or_else(|| "运行目录缺少输出根父目录".to_owned())?;
        let run_name = run_dir
            .file_name()
            .ok_or_else(|| "运行目录缺少最终路径组件".to_owned())?;
        let current_output =
            open_pinned_unix_directory(output_root_path, "真实测试输出根目录复核")?;
        if unix_directory_identity(&current_output, "真实测试输出根目录复核")?
            != unix_directory_identity(&self.output_root, "真实测试输出根固定句柄")?
        {
            return Err("真实测试输出根目录身份发生变化".to_owned());
        }
        let current_run =
            unix_open_ffi::open_directory_at(&self.output_root, run_name, "真实测试运行目录复核")?;
        if unix_directory_identity(&current_run, "真实测试运行目录复核")?
            != unix_directory_identity(&self.run_dir, "真实测试运行目录固定句柄")?
        {
            return Err("真实测试运行目录身份发生变化".to_owned());
        }
        for (name, held, label) in [
            ("fixtures", &self.fixtures_dir, "Fixture 目录"),
            ("sanitized-logs", &self.sanitized_logs_dir, "脱敏日志目录"),
        ] {
            let current = unix_open_ffi::open_directory_at(
                &self.run_dir,
                std::ffi::OsStr::new(name),
                &format!("{label}复核"),
            )?;
            if unix_directory_identity(&current, &format!("{label}复核"))?
                != unix_directory_identity(held, &format!("{label}固定句柄"))?
            {
                return Err(format!("{label}身份发生变化"));
            }
        }
        Ok(())
    }

    /// 根据固定报告布局选择相对普通文件的父目录句柄与最终文件名。
    fn parent_and_name<'a>(
        &'a self,
        relative: &'a Path,
        label: &str,
    ) -> Result<(&'a File, &'a std::ffi::OsStr), String> {
        validate_fixed_report_file_relative_path(relative, label)?;
        let components = relative.iter().collect::<Vec<_>>();
        match components.as_slice() {
            [name] => Ok((&self.run_dir, name)),
            [directory, name] if *directory == std::ffi::OsStr::new("fixtures") => {
                Ok((&self.fixtures_dir, name))
            }
            [directory, name] if *directory == std::ffi::OsStr::new("sanitized-logs") => {
                Ok((&self.sanitized_logs_dir, name))
            }
            _ => Err(format!("{label} 必须位于报告固定目录的直属层级")),
        }
    }

    /// 相对于固定目录打开普通文件，并在返回前复核目录链及最终文件对象身份。
    #[allow(clippy::too_many_arguments)]
    fn open_regular_file(
        &self,
        run_dir: &Path,
        relative: &Path,
        access: StableFileAccess,
        creation: StableFileCreation,
        max_bytes: u64,
        expected_len: Option<u64>,
        expected_identity: Option<&RegularFileIdentity>,
        label: &str,
    ) -> Result<(File, u64, RegularFileIdentity), String> {
        self.verify_layout(run_dir)?;
        let (parent, name) = self.parent_and_name(relative, label)?;
        let file = unix_open_ffi::open_regular_at(parent, name, access, creation, label)?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("无法读取已打开 {label} 元数据：{error}"))?;
        if !metadata.is_file() || is_link_or_reparse(&metadata) || metadata.len() > max_bytes {
            return Err(format!(
                "已打开 {label} 不是普通文件或超过 {max_bytes} 字节安全上限"
            ));
        }
        let identity = regular_file_identity_from_open_handle(&file, &metadata, label)?;
        if expected_len.is_some_and(|expected| expected != metadata.len()) {
            return Err(format!("{label} 在枚举与打开之间长度发生变化"));
        }
        if expected_identity.is_some_and(|expected| expected != &identity) {
            return Err(format!("{label} 在枚举与打开之间文件对象发生变化"));
        }
        let verification = unix_open_ffi::open_regular_at(
            parent,
            name,
            StableFileAccess::Verify,
            StableFileCreation::Existing,
            &format!("{label} 复核文件"),
        )?;
        let verification_metadata = verification
            .metadata()
            .map_err(|error| format!("无法读取 {label} 复核句柄元数据：{error}"))?;
        if !verification_metadata.is_file()
            || regular_file_identity_from_open_handle(&verification, &verification_metadata, label)?
                != identity
        {
            return Err(format!("{label} 路径所指文件对象在打开期间发生变化"));
        }
        self.verify_layout(run_dir)?;
        Ok((file, metadata.len(), identity))
    }

    /// 复核固定目录中的最终路径仍指向预期普通文件对象。
    fn verify_regular_file_identity(
        &self,
        run_dir: &Path,
        relative: &Path,
        expected: &RegularFileIdentity,
        label: &str,
    ) -> Result<(), String> {
        let (parent, name) = self.parent_and_name(relative, label)?;
        let verification = unix_open_ffi::open_regular_at(
            parent,
            name,
            StableFileAccess::Verify,
            StableFileCreation::Existing,
            &format!("{label} 最终复核文件"),
        )?;
        let metadata = verification
            .metadata()
            .map_err(|error| format!("无法读取 {label} 最终复核句柄元数据：{error}"))?;
        if !metadata.is_file()
            || &regular_file_identity_from_open_handle(&verification, &metadata, label)? != expected
        {
            return Err(format!("{label} 路径所指文件对象在操作期间发生变化"));
        }
        self.verify_layout(run_dir)
    }
}

/// 从 Unix 已打开目录句柄读取设备号与 inode 身份。
#[cfg(unix)]
fn unix_directory_identity(directory: &File, label: &str) -> Result<UnixObjectIdentity, String> {
    let metadata = directory
        .metadata()
        .map_err(|error| format!("无法读取 {label} 目录句柄元数据：{error}"))?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err(format!("{label} 句柄不是普通目录"));
    }
    Ok(UnixObjectIdentity::from_metadata(&metadata))
}

/// 校验运行根是非链接普通目录，并返回其规范绝对路径。
fn validated_run_root(run_dir: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(run_dir)
        .map_err(|error| format!("无法读取运行目录元数据：{error}"))?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err("运行路径必须是既有普通目录且不能是符号链接或重解析点".to_owned());
    }
    let canonical =
        fs::canonicalize(run_dir).map_err(|error| format!("无法规范化运行目录：{error}"))?;
    let canonical_metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("无法确认规范运行目录元数据：{error}"))?;
    if is_link_or_reparse(&canonical_metadata) || !canonical_metadata.is_dir() {
        return Err("规范运行路径必须指向普通目录".to_owned());
    }
    Ok(canonical)
}

/// 逐组件拒绝链接和重解析点，并确认既有后代的规范路径仍位于运行根内。
fn validated_run_descendant(
    run_dir: &Path,
    relative: &Path,
    expected_kind: RunPathKind,
    label: &str,
) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative.components().next().is_none()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!("{label} 必须是运行目录内的规范相对路径"));
    }
    let canonical_root = validated_run_root(run_dir)?;
    let components = relative.components().collect::<Vec<_>>();
    let mut current = canonical_root.clone();
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("无法读取 {label} 路径组件元数据：{error}"))?;
        if is_link_or_reparse(&metadata) {
            return Err(format!(
                "{label} 路径组件不能是符号链接、目录联接或重解析点"
            ));
        }
        let is_final = index + 1 == components.len();
        if !is_final && !metadata.is_dir() {
            return Err(format!("{label} 的中间路径组件必须是普通目录"));
        }
        if is_final {
            let kind_matches = match expected_kind {
                RunPathKind::Directory => metadata.is_dir(),
                RunPathKind::File => metadata.is_file(),
            };
            if !kind_matches {
                return Err(format!("{label} 的最终节点类型无效"));
            }
        }
        let canonical = fs::canonicalize(&current)
            .map_err(|error| format!("无法规范化 {label} 路径组件：{error}"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(format!("{label} 的规范路径越过当前运行目录"));
        }
        current = canonical;
    }
    Ok(current)
}

/// 在任何真实网络请求前验证输出文件系统支持不可变 Fixture 所需的同目录硬链接。
fn verify_fixture_hard_link_support(run_dir: &Path) -> Result<(), String> {
    let fixture_dir = validated_run_descendant(
        run_dir,
        Path::new("fixtures"),
        RunPathKind::Directory,
        "Fixture 硬链接预检目录",
    )?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("无法生成 Fixture 硬链接预检标识：{error}"))?
        .as_nanos();
    let source = fixture_dir.join(format!(
        "{FIXTURE_STAGING_PREFIX}preflight.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let destination = fixture_dir.join(format!(
        "{FIXTURE_STAGING_PREFIX}preflight-link.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&source)
        .map_err(|error| format!("无法创建 Fixture 硬链接预检文件：{error}"))?;
    if let Err(error) = file
        .write_all(b"keencode-fixture-link-preflight")
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&source);
        return Err(format!("无法写入并同步 Fixture 硬链接预检文件：{error}"));
    }
    drop(file);
    if let Err(error) = fs::hard_link(&source, &destination) {
        let _ = fs::remove_file(&source);
        let _ = fs::remove_file(&destination);
        return Err(format!(
            "当前输出文件系统不支持不可变 Fixture 所需的同目录硬链接，真实请求尚未开始：{error}"
        ));
    }
    fs::remove_file(&destination)
        .and_then(|_| fs::remove_file(&source))
        .map_err(|error| format!("无法清理 Fixture 硬链接预检文件：{error}"))
}

/// 通过同一稳定普通文件句柄检查身份和长度，再以二次增长防护读取有界字节。
fn read_bounded_regular_file(
    path: &Path,
    max_bytes: u64,
    expected_len: Option<u64>,
    expected_identity: Option<&RegularFileIdentity>,
    label: &str,
) -> Result<Vec<u8>, String> {
    let (mut file, opened_len, identity) =
        open_bounded_regular_file(path, max_bytes, expected_len, expected_identity, label)?;
    let capacity = usize::try_from(opened_len)
        .map_err(|_| format!("{label} 长度不能表示为当前平台内存大小"))?;
    let mut bytes = Vec::with_capacity(capacity);
    (&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法有界读取 {label}：{error}"))?;
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_len != opened_len {
        return Err(format!("{label} 在读取期间长度发生变化"));
    }
    let final_metadata = file
        .metadata()
        .map_err(|error| format!("无法复核已打开 {label} 元数据：{error}"))?;
    if final_metadata.len() != opened_len
        || regular_file_identity_from_open_handle(&file, &final_metadata, label)? != identity
    {
        return Err(format!("{label} 在读取期间文件身份或长度发生变化"));
    }
    verify_regular_file_path_identity(path, &identity, label)?;
    Ok(bytes)
}

/// 使用 `fill_buf` 在分配前执行单行上限，返回行字节及是否以换行完整结束。
fn read_limited_journal_line<R: BufRead>(
    reader: &mut R,
) -> Result<Option<(Vec<u8>, bool)>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("无法读取恢复提交日志缓冲区：{error}"))?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some((line, false)))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let next_len = line
            .len()
            .checked_add(consumed)
            .ok_or_else(|| "恢复提交日志单行长度溢出".to_owned())?;
        if next_len > MAX_PROGRESS_JOURNAL_LINE_BYTES {
            return Err(format!(
                "恢复提交日志单行超过 {} 字节安全上限",
                MAX_PROGRESS_JOURNAL_LINE_BYTES
            ));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some((line, true)));
        }
    }
}

/// 管理崩溃可恢复检查点和最终脱敏产物。
pub(crate) struct ReportStore {
    /// 当前运行的独立输出目录。
    run_dir: PathBuf,
    /// Windows 生命周期内按父先子后固定输出根、运行目录和两个固定子目录的句柄。
    #[cfg(windows)]
    _directory_pins: WindowsReportDirectoryPins,
    /// Unix 生命周期内按父先子后固定输出根、运行目录和两个固定子目录的文件描述符。
    #[cfg(unix)]
    _directory_pins: UnixReportDirectoryPins,
    /// 每完成一个用例立即追加的检查点文件。
    checkpoint_path: PathBuf,
    /// 下一条提交日志必须使用的连续序号。
    next_journal_sequence: Cell<u64>,
    /// 最近一次完整加载或追加后已同步 Journal 的精确字节长度。
    journal_byte_len: Cell<u64>,
    /// 当前进程持有到运行结束的跨进程独占锁及其稳定父目录句柄。
    _lock_file: HeldExclusiveLock,
    /// 已经写入提交日志的稳定键，阻止同一用例在同一运行中重复提交。
    journal_records: RefCell<BTreeMap<String, ProbeRecord>>,
    /// 当前已同步 Journal 的链尾 MAC；初始值为固定链头。
    journal_tail_mac: RefCell<String>,
    /// 首次 Resume 写入或冷加载后冻结的 Journal 认证上下文。
    journal_authentication: RefCell<Option<JournalAuthenticationContext>>,
}

/// 一次已完成运行只读核验成功后可安全展示的固定计数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletedRunVerification {
    /// 恢复身份中通过当前配置解析的 Provider 数量。
    pub(crate) provider_count: usize,
    /// 通过 Journal、Resume 和最终报告交叉校验的记录数量。
    pub(crate) record_count: usize,
    /// 通过内容寻址、响应重放和记录绑定校验的 Fixture 数量。
    pub(crate) fixture_count: usize,
    /// 通过链式认证且与 Resume 一致的 Journal 最终序号。
    pub(crate) journal_sequence: u64,
    /// 通过完成态封印且重新计算一致的事实产物数量。
    pub(crate) seal_artifact_count: usize,
}

/// 持有跨进程独占文件锁，并在需要时同时固定锁文件的父目录身份。
struct HeldExclusiveLock {
    /// 实际取得独占锁且保持到生命周期结束的普通文件句柄。
    _file: File,
    /// 锁文件使用的稳定父目录句柄；ReportStore 还会额外持有完整报告目录 Pins。
    _parent_directory: File,
}

/// ReportStore 在追加记录时使用且不包含凭据的 Journal 认证上下文。
#[derive(Clone, Eq, PartialEq)]
struct JournalAuthenticationContext {
    /// 当前运行稳定标识。
    run_id: String,
    /// 当前运行公开随机盐。
    run_salt: String,
    /// 补测选择摘要或普通运行固定域。
    selection_domain: String,
}

/// 保证当前用户同一时刻只有一个 Provider 真实测试进程。
pub(crate) struct LiveTestProcessLock {
    /// 持有到主运行结束的跨进程独占文件锁与稳定父目录句柄。
    _lock: HeldExclusiveLock,
}

impl LiveTestProcessLock {
    /// 从稳定用户数据目录取得全局真实测试锁，不受临时目录环境变量影响。
    pub(crate) fn acquire(user_data_directory: &Path) -> Result<Self, String> {
        let lock_path = prepare_global_lock_path(user_data_directory)?;
        Self::acquire_at(&lock_path)
    }

    /// 从指定底层路径取得全局锁，便于无网络跨进程回归验证。
    fn acquire_at(lock_path: &Path) -> Result<Self, String> {
        let lock_path = validated_global_lock_path(lock_path)?;
        let lock = acquire_exclusive_lock_file(
            &lock_path,
            "Provider 真实测试全局锁",
            "已有另一个 Provider 真实测试进程正在运行；拒绝并行发送真实请求",
        )?;
        Ok(Self { _lock: lock })
    }
}

/// 一条先于恢复清单提交并经过 `sync_all` 的追加日志记录。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeJournalEntry<'a> {
    /// 当前日志封装的唯一受支持版本。
    schema_version: &'static str,
    /// 从一开始严格连续递增的提交序号。
    sequence: u64,
    /// 上一条已认证记录的 MAC；首条使用固定链头。
    previous_mac: &'a str,
    /// 覆盖运行盐、选择域、序号、前序 MAC 与规范记录的 Provider HMAC。
    record_mac: &'a str,
    /// 已脱敏且 Fixture 已经落盘的完整探测记录。
    record: &'a ProbeRecord,
}

/// 从冷恢复日志严格反序列化的一条拥有型记录。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnedProbeJournalEntry {
    /// 必须等于当前唯一日志结构版本。
    schema_version: String,
    /// 从一开始严格连续递增的提交序号。
    sequence: u64,
    /// 上一条链式 MAC；显式 legacy v5 来源没有该字段。
    #[serde(default)]
    previous_mac: Option<String>,
    /// 当前记录的 Provider HMAC；显式 legacy v5 来源没有该字段。
    #[serde(default)]
    record_mac: Option<String>,
    /// 已脱敏且 Fixture 已经落盘的完整探测记录。
    record: ProbeRecord,
}

/// 提交日志尾部不完整时允许的显式处理策略。
#[derive(Clone, Copy)]
enum JournalTailPolicy {
    /// 常规原目录恢复可以同步截断崩溃留下的尾部半行。
    RepairInPlace,
    /// 隔离恢复来源保持完全只读，发现尾部半行即拒绝。
    ReadOnlyReject,
}

impl ReportStore {
    /// 创建本次运行的隔离目录与固定子目录。
    pub(crate) fn create(output_root: &Path, run_id: &str) -> Result<Self, String> {
        let run_dir = output_root.join(run_id);
        fs::create_dir_all(output_root)
            .map_err(|error| format!("无法创建真实测试输出根目录：{error}"))?;
        fs::create_dir(&run_dir).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                "真实测试运行目录已经存在；必须使用 --resume 显式恢复，拒绝覆盖".to_owned()
            } else {
                format!("无法创建真实测试运行目录：{error}")
            }
        })?;
        let run_dir = validated_run_root(&run_dir)?;
        fs::create_dir_all(run_dir.join("fixtures"))
            .map_err(|error| format!("无法创建 Fixture 目录：{error}"))?;
        fs::create_dir_all(run_dir.join("sanitized-logs"))
            .map_err(|error| format!("无法创建脱敏日志目录：{error}"))?;
        validated_run_descendant(
            &run_dir,
            Path::new("fixtures"),
            RunPathKind::Directory,
            "Fixture 目录",
        )?;
        validated_run_descendant(
            &run_dir,
            Path::new("sanitized-logs"),
            RunPathKind::Directory,
            "脱敏日志目录",
        )?;
        verify_fixture_hard_link_support(&run_dir)?;
        #[cfg(windows)]
        let directory_pins = WindowsReportDirectoryPins::open(&run_dir)?;
        #[cfg(unix)]
        let directory_pins = UnixReportDirectoryPins::open(&run_dir)?;
        #[cfg(unix)]
        let lock_file = acquire_pinned_run_lock(&directory_pins, &run_dir, false)?;
        #[cfg(not(unix))]
        let lock_file = acquire_run_lock(&run_dir)?;
        let checkpoint_path = run_dir.join("sanitized-logs").join("progress.jsonl");
        Ok(Self {
            run_dir,
            #[cfg(any(unix, windows))]
            _directory_pins: directory_pins,
            checkpoint_path,
            next_journal_sequence: Cell::new(1),
            journal_byte_len: Cell::new(0),
            _lock_file: lock_file,
            journal_records: RefCell::new(BTreeMap::new()),
            journal_tail_mac: RefCell::new(JOURNAL_INITIAL_MAC.to_owned()),
            journal_authentication: RefCell::new(None),
        })
    }

    /// 打开用户明确指定的既有运行目录而不创建或回退到其他路径。
    pub(crate) fn open_resume(run_dir: &Path) -> Result<Self, String> {
        let run_dir = validated_run_root(run_dir)?;
        reject_recovery_incomplete_marker(&run_dir)?;
        validated_run_descendant(
            &run_dir,
            Path::new("fixtures"),
            RunPathKind::Directory,
            "恢复 Fixture 目录",
        )?;
        validated_run_descendant(
            &run_dir,
            Path::new("sanitized-logs"),
            RunPathKind::Directory,
            "恢复脱敏日志目录",
        )?;
        verify_fixture_hard_link_support(&run_dir)?;
        #[cfg(windows)]
        let directory_pins = WindowsReportDirectoryPins::open(&run_dir)?;
        #[cfg(unix)]
        let directory_pins = UnixReportDirectoryPins::open(&run_dir)?;
        #[cfg(unix)]
        let lock_file = acquire_pinned_run_lock(&directory_pins, &run_dir, false)?;
        #[cfg(not(unix))]
        let lock_file = acquire_run_lock(&run_dir)?;
        Ok(Self {
            checkpoint_path: run_dir.join("sanitized-logs").join("progress.jsonl"),
            run_dir,
            #[cfg(any(unix, windows))]
            _directory_pins: directory_pins,
            next_journal_sequence: Cell::new(1),
            journal_byte_len: Cell::new(0),
            _lock_file: lock_file,
            journal_records: RefCell::new(BTreeMap::new()),
            journal_tail_mac: RefCell::new(JOURNAL_INITIAL_MAC.to_owned()),
            journal_authentication: RefCell::new(None),
        })
    }

    /// 以只读来源语义打开用户明确指定的隔离恢复目录。
    ///
    /// 该入口要求既有锁文件，不创建目录、锁文件或硬链接预检文件；持有独占锁只用于
    /// 阻止合作进程并发改写来源，后续加载仍须使用 `ReadOnlyReject` 策略。
    pub(crate) fn open_recovery_source(run_dir: &Path) -> Result<Self, String> {
        let run_dir = validated_run_root(run_dir)?;
        reject_recovery_incomplete_marker(&run_dir)?;
        validated_run_descendant(
            &run_dir,
            Path::new("fixtures"),
            RunPathKind::Directory,
            "只读恢复来源 Fixture 目录",
        )?;
        validated_run_descendant(
            &run_dir,
            Path::new("sanitized-logs"),
            RunPathKind::Directory,
            "只读恢复来源脱敏日志目录",
        )?;
        #[cfg(windows)]
        let directory_pins = WindowsReportDirectoryPins::open(&run_dir)?;
        #[cfg(unix)]
        let directory_pins = UnixReportDirectoryPins::open(&run_dir)?;
        #[cfg(unix)]
        let lock_file = acquire_pinned_run_lock(&directory_pins, &run_dir, true)?;
        #[cfg(not(unix))]
        let lock_file = acquire_existing_run_lock(&run_dir)?;
        Ok(Self {
            checkpoint_path: run_dir.join("sanitized-logs").join("progress.jsonl"),
            run_dir,
            #[cfg(any(unix, windows))]
            _directory_pins: directory_pins,
            next_journal_sequence: Cell::new(1),
            journal_byte_len: Cell::new(0),
            _lock_file: lock_file,
            journal_records: RefCell::new(BTreeMap::new()),
            journal_tail_mac: RefCell::new(JOURNAL_INITIAL_MAC.to_owned()),
            journal_authentication: RefCell::new(None),
        })
    }

    /// 创建只表示“隔离恢复尚未完整验证”的固定失败关闭标记。
    fn write_recovery_incomplete_marker(&self) -> Result<(), String> {
        let marker = self.run_dir.join(RECOVERY_INCOMPLETE_MARKER_FILE);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&marker)
            .map_err(|error| format!("无法创建恢复副本失败关闭标记：{error}"))?;
        file.write_all(b"keencode recovery copy incomplete\n")
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("无法写入并同步恢复副本失败关闭标记：{error}"))
    }

    /// 在全部来源复核与新身份校验通过后删除失败关闭标记。
    fn clear_recovery_incomplete_marker(&self) -> Result<(), String> {
        let marker = validated_run_descendant(
            &self.run_dir,
            Path::new(RECOVERY_INCOMPLETE_MARKER_FILE),
            RunPathKind::File,
            "恢复副本失败关闭标记",
        )?;
        fs::remove_file(marker).map_err(|error| format!("无法清除恢复副本失败关闭标记：{error}"))
    }

    /// 返回可向用户展示的当前运行目录。
    pub(crate) fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// 从当前 Manifest 提取追加 Journal 所需且不包含凭据的固定认证上下文。
    fn journal_authentication_context(manifest: &ResumeManifest) -> JournalAuthenticationContext {
        JournalAuthenticationContext {
            run_id: manifest.run.run_id.clone(),
            run_salt: manifest.identity.run_salt.clone(),
            selection_domain: manifest.journal_selection_domain().to_owned(),
        }
    }

    /// 安装或核对当前 Store 的 Journal 认证上下文，禁止运行中切换盐或选择域。
    fn bind_journal_authentication(&self, manifest: &ResumeManifest) -> Result<(), String> {
        let expected = Self::journal_authentication_context(manifest);
        let mut current = self.journal_authentication.borrow_mut();
        if current.as_ref().is_some_and(|value| value != &expected) {
            return Err("同一运行目录的 Journal 认证上下文发生变化".to_owned());
        }
        *current = Some(expected);
        Ok(())
    }

    /// 在只读来源锁仍由当前 Store 持有时创建并验证带失败关闭标记的补测目标。
    pub(crate) fn create_retry_target(
        &self,
        output_root: &Path,
        run_id: &str,
    ) -> Result<Self, String> {
        create_verified_derived_target(&[self.run_dir()], output_root, run_id, |_| Ok(()))
    }

    /// 在选择 Sidecar 与恢复清单均已安全写入后解除补测目标的失败关闭状态。
    pub(crate) fn complete_retry_target_setup(&self) -> Result<(), String> {
        self.clear_recovery_incomplete_marker()
            .map_err(retained_recovery_target_error)
    }

    /// 在任何日志打开或截断前验证父目录与既有直属文件的完整安全路径。
    fn validated_checkpoint_path(&self) -> Result<Option<PathBuf>, String> {
        let log_dir = validated_run_descendant(
            &self.run_dir,
            Path::new("sanitized-logs"),
            RunPathKind::Directory,
            "脱敏日志目录",
        )?;
        match fs::symlink_metadata(&self.checkpoint_path) {
            Ok(_) => {
                let checkpoint = validated_run_descendant(
                    &self.run_dir,
                    Path::new("sanitized-logs/progress.jsonl"),
                    RunPathKind::File,
                    "恢复提交日志",
                )?;
                if checkpoint.parent() != Some(log_dir.as_path()) {
                    return Err("恢复提交日志必须是 sanitized-logs 的直属普通文件".to_owned());
                }
                Ok(Some(checkpoint))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("无法读取恢复提交日志元数据：{error}")),
        }
    }

    /// 从固定报告目录打开普通文件，并返回同一稳定对象的句柄、长度与平台身份。
    #[allow(clippy::too_many_arguments)]
    fn open_stable_run_file(
        &self,
        relative: &Path,
        access: StableFileAccess,
        creation: StableFileCreation,
        max_bytes: u64,
        expected_len: Option<u64>,
        expected_identity: Option<&RegularFileIdentity>,
        label: &str,
    ) -> Result<(File, u64, RegularFileIdentity), String> {
        validate_fixed_report_file_relative_path(relative, label)?;
        #[cfg(unix)]
        {
            self._directory_pins.open_regular_file(
                &self.run_dir,
                relative,
                access,
                creation,
                max_bytes,
                expected_len,
                expected_identity,
                label,
            )
        }
        #[cfg(windows)]
        {
            let path = self.run_dir.join(relative);
            if matches!(creation, StableFileCreation::Existing) {
                validated_run_descendant(&self.run_dir, relative, RunPathKind::File, label)?;
            } else {
                let parent = relative.parent().unwrap_or_else(|| Path::new(""));
                if parent.as_os_str().is_empty() {
                    validated_run_root(&self.run_dir)?;
                } else {
                    validated_run_descendant(
                        &self.run_dir,
                        parent,
                        RunPathKind::Directory,
                        &format!("{label} 父目录"),
                    )?;
                }
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
                        return Err(format!("{label} 必须是普通文件且不能是链接或重解析点"));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("无法读取 {label} 元数据：{error}")),
                }
            }
            let file = open_windows_regular_file_handle_with(&path, access, creation, label)?;
            let metadata = file
                .metadata()
                .map_err(|error| format!("无法读取已打开 {label} 元数据：{error}"))?;
            if is_link_or_reparse(&metadata) || !metadata.is_file() || metadata.len() > max_bytes {
                return Err(format!(
                    "已打开 {label} 不是非重解析普通文件或超过 {max_bytes} 字节安全上限"
                ));
            }
            let identity = regular_file_identity_from_open_handle(&file, &metadata, label)?;
            if expected_len.is_some_and(|expected| expected != metadata.len()) {
                return Err(format!("{label} 在枚举与打开之间长度发生变化"));
            }
            if expected_identity.is_some_and(|expected| expected != &identity) {
                return Err(format!("{label} 在枚举与打开之间文件对象发生变化"));
            }
            let current = validated_run_descendant(
                &self.run_dir,
                relative,
                RunPathKind::File,
                &format!("{label} 打开后复核"),
            )?;
            if !paths_equal(&current, &path) {
                return Err(format!("{label} 打开后规范路径发生变化"));
            }
            let verification = open_windows_regular_file_handle_with(
                &path,
                StableFileAccess::Verify,
                StableFileCreation::Existing,
                &format!("{label} 复核文件"),
            )?;
            let verification_metadata = verification
                .metadata()
                .map_err(|error| format!("无法读取 {label} 复核句柄元数据：{error}"))?;
            if is_link_or_reparse(&verification_metadata)
                || !verification_metadata.is_file()
                || regular_file_identity_from_open_handle(
                    &verification,
                    &verification_metadata,
                    label,
                )? != identity
            {
                return Err(format!("{label} 路径所指文件对象在打开期间发生变化"));
            }
            Ok((file, metadata.len(), identity))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.run_dir.join(relative);
            let mut options = OpenOptions::new();
            match access {
                StableFileAccess::ReadOnly | StableFileAccess::Verify => {
                    options.read(true);
                }
                StableFileAccess::ReadWrite | StableFileAccess::Lock => {
                    options.read(true).write(true);
                }
                StableFileAccess::Append => {
                    options.append(true);
                }
            }
            if matches!(creation, StableFileCreation::CreateIfMissing) {
                options.create(true);
            }
            let file = options
                .open(&path)
                .map_err(|error| format!("无法打开 {label}：{error}"))?;
            let metadata = file
                .metadata()
                .map_err(|error| format!("无法读取已打开 {label} 元数据：{error}"))?;
            if !metadata.is_file() || metadata.len() > max_bytes {
                return Err(format!(
                    "已打开 {label} 不是普通文件或超过 {max_bytes} 字节安全上限"
                ));
            }
            let identity = regular_file_identity_from_open_handle(&file, &metadata, label)?;
            if expected_len.is_some_and(|expected| expected != metadata.len())
                || expected_identity.is_some_and(|expected| expected != &identity)
            {
                return Err(format!("{label} 在枚举与打开之间发生变化"));
            }
            Ok((file, metadata.len(), identity))
        }
    }

    /// 复核固定目录中的最终文件名仍指向先前打开的同一普通文件对象。
    fn verify_stable_run_file_identity(
        &self,
        relative: &Path,
        expected: &RegularFileIdentity,
        label: &str,
    ) -> Result<(), String> {
        validate_fixed_report_file_relative_path(relative, label)?;
        #[cfg(unix)]
        {
            self._directory_pins.verify_regular_file_identity(
                &self.run_dir,
                relative,
                expected,
                label,
            )
        }
        #[cfg(windows)]
        {
            let path = validated_run_descendant(&self.run_dir, relative, RunPathKind::File, label)?;
            let verification = open_windows_regular_file_handle_with(
                &path,
                StableFileAccess::Verify,
                StableFileCreation::Existing,
                &format!("{label} 最终复核文件"),
            )?;
            let metadata = verification
                .metadata()
                .map_err(|error| format!("无法读取 {label} 最终复核句柄元数据：{error}"))?;
            if is_link_or_reparse(&metadata)
                || !metadata.is_file()
                || &regular_file_identity_from_open_handle(&verification, &metadata, label)?
                    != expected
            {
                return Err(format!("{label} 路径所指文件对象在操作期间发生变化"));
            }
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = validated_run_descendant(&self.run_dir, relative, RunPathKind::File, label)?;
            verify_regular_file_path_identity(&path, expected, label)
        }
    }

    /// 通过固定目录中的同一稳定句柄读取有界普通文件，并在消费后复核身份与长度。
    fn read_bounded_run_file_snapshot(
        &self,
        relative: &Path,
        max_bytes: u64,
        expected_len: Option<u64>,
        expected_identity: Option<&RegularFileIdentity>,
        label: &str,
    ) -> Result<Vec<u8>, String> {
        let (mut file, opened_len, identity) = self.open_stable_run_file(
            relative,
            StableFileAccess::ReadOnly,
            StableFileCreation::Existing,
            max_bytes,
            expected_len,
            expected_identity,
            label,
        )?;
        let capacity = usize::try_from(opened_len)
            .map_err(|_| format!("{label} 长度不能表示为当前平台内存大小"))?;
        let mut bytes = Vec::with_capacity(capacity);
        (&mut file)
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| format!("无法有界读取 {label}：{error}"))?;
        let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let final_metadata = file
            .metadata()
            .map_err(|error| format!("无法复核已打开 {label} 元数据：{error}"))?;
        if actual_len != opened_len
            || final_metadata.len() != opened_len
            || regular_file_identity_from_open_handle(&file, &final_metadata, label)? != identity
        {
            return Err(format!("{label} 在读取期间文件身份或长度发生变化"));
        }
        self.verify_stable_run_file_identity(relative, &identity, label)?;
        Ok(bytes)
    }

    /// 验证运行目录后代后按指定小型上限读取普通文件。
    fn read_bounded_run_file(
        &self,
        relative: &Path,
        max_bytes: u64,
        label: &str,
    ) -> Result<Vec<u8>, String> {
        self.read_bounded_run_file_snapshot(relative, max_bytes, None, None, label)
    }

    /// 通过固定目录中的同一稳定句柄计算摘要，并在消费后复核身份与长度。
    fn sha256_bounded_run_file_snapshot(
        &self,
        relative: &Path,
        max_bytes: u64,
        expected_len: Option<u64>,
        expected_identity: Option<&RegularFileIdentity>,
        label: &str,
    ) -> Result<String, String> {
        let (mut file, opened_len, identity) = self.open_stable_run_file(
            relative,
            StableFileAccess::ReadOnly,
            StableFileCreation::Existing,
            max_bytes,
            expected_len,
            expected_identity,
            label,
        )?;
        let (digest, actual_len) = sha256_digest_reader(
            (&mut file).take(max_bytes.saturating_add(1)),
            max_bytes,
            label,
        )?;
        let final_metadata = file
            .metadata()
            .map_err(|error| format!("无法复核已打开 {label} 元数据：{error}"))?;
        if actual_len != opened_len
            || final_metadata.len() != opened_len
            || regular_file_identity_from_open_handle(&file, &final_metadata, label)? != identity
        {
            return Err(format!("{label} 在摘要期间文件身份或长度发生变化"));
        }
        self.verify_stable_run_file_identity(relative, &identity, label)?;
        Ok(digest)
    }

    /// 验证运行目录后代后通过固定缓冲区计算普通文件摘要，避免为大文件分配等量内存。
    fn sha256_bounded_run_file(
        &self,
        relative: &Path,
        max_bytes: u64,
        label: &str,
    ) -> Result<String, String> {
        self.sha256_bounded_run_file_snapshot(relative, max_bytes, None, None, label)
    }

    /// 在首个真实请求前或每条探测提交后原子替换严格恢复清单。
    pub(crate) fn write_resume_manifest(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        if manifest.identity.schema_version != RESUME_SCHEMA_VERSION
            || manifest.identity.harness_contract_id != HARNESS_CONTRACT_ID
        {
            return Err("只能写入当前 Resume 与 Harness 契约版本".to_owned());
        }
        self.bind_journal_authentication(manifest)?;
        let mut persisted = manifest.persisted_snapshot(&self.journal_tail_mac.borrow())?;
        if persisted.finished {
            if persisted.completion_artifact_seal.is_none() {
                return Err("完成恢复清单必须由完成流程提供基于受信事实生成的产物封印".to_owned());
            }
            self.verify_completion_artifact_seal(&persisted)?;
        } else if persisted.completion_artifact_seal.is_some() {
            return Err("未完成恢复清单不能写入完成态事实产物封印".to_owned());
        }
        persisted.state_proofs = persisted.calculated_state_proofs(providers)?;
        self.write_json("resume.json", &persisted, providers)
    }

    /// 读取、脱敏检查并严格解析当前唯一版本的恢复清单。
    pub(crate) fn load_resume_manifest(
        &self,
        providers: &[&ProviderEntry],
    ) -> Result<ResumeManifest, String> {
        self.load_manifest_with_policy(
            providers,
            &[RESUME_SCHEMA_VERSION],
            JOURNAL_SCHEMA_VERSION,
            JournalTailPolicy::RepairInPlace,
        )
    }

    /// 只读加载隔离恢复来源，既不修复日志尾部也不改写任何来源文件。
    pub(crate) fn load_recovery_source_manifest(
        &self,
        providers: &[&ProviderEntry],
        allow_unauthenticated_legacy: bool,
    ) -> Result<ResumeManifest, String> {
        let accepted_resume_schemas = if allow_unauthenticated_legacy {
            &[RETRY_SOURCE_RESUME_SCHEMA_VERSION, RESUME_SCHEMA_VERSION][..]
        } else {
            &[RESUME_SCHEMA_VERSION][..]
        };
        self.load_manifest_with_policy(
            providers,
            accepted_resume_schemas,
            JOURNAL_SCHEMA_VERSION,
            JournalTailPolicy::ReadOnlyReject,
        )
    }

    /// 只读加载已完成精确补测来源，并允许上一版 v14 事实作为显式来源。
    pub(crate) fn load_retry_source_manifest(
        &self,
        providers: &[&ProviderEntry],
        allow_unauthenticated_legacy: bool,
    ) -> Result<ResumeManifest, String> {
        let accepted_resume_schemas = if allow_unauthenticated_legacy {
            &[RETRY_SOURCE_RESUME_SCHEMA_VERSION, RESUME_SCHEMA_VERSION][..]
        } else {
            &[RESUME_SCHEMA_VERSION][..]
        };
        self.load_manifest_with_policy(
            providers,
            accepted_resume_schemas,
            JOURNAL_SCHEMA_VERSION,
            JournalTailPolicy::ReadOnlyReject,
        )
    }

    /// 在不修复、不写入和不发起网络请求的前提下完整核验一份已完成运行。
    pub(crate) async fn verify_completed_run(
        &self,
        providers: &[&ProviderEntry],
    ) -> Result<CompletedRunVerification, String> {
        let manifest = self.load_recovery_source_manifest(providers, false)?;
        if !manifest.finished || manifest.run.finished_at.is_none() {
            return Err("指定运行尚未完成，不能执行已完成运行核验".to_owned());
        }

        self.verify_committed_fixtures(&manifest, providers).await?;
        let result_bytes = self.read_bounded_run_file(
            Path::new("result.json"),
            MAX_ARTIFACT_FILE_BYTES,
            "完成来源最终报告",
        )?;
        let stored_report = validate_stored_run_report(
            &result_bytes,
            &manifest,
            providers,
            &[RUN_REPORT_SCHEMA_VERSION],
        )?;
        drop(result_bytes);
        let _snapshot = self.completed_source_snapshot(&manifest, providers)?;

        let fixture_count = manifest
            .records
            .values()
            .try_fold(0_usize, |count, record| {
                count
                    .checked_add(record.fixture_paths.len())
                    .ok_or_else(|| "完成来源 Fixture 数量溢出".to_owned())
            })?;
        let seal_artifact_count = manifest
            .completion_artifact_seal
            .as_ref()
            .map_or(0, |seal| seal.artifacts.len());
        Ok(CompletedRunVerification {
            provider_count: manifest.identity.providers.len(),
            record_count: stored_report.probes.len(),
            fixture_count,
            journal_sequence: manifest.journal_sequence,
            seal_artifact_count,
        })
    }

    /// 读取完成来源的脱敏报告，释放原始字节后执行全目录重扫，并返回已核对的内容摘要。
    fn read_and_verify_completed_redaction_report(
        &self,
        providers: &[&ProviderEntry],
    ) -> Result<String, String> {
        let bytes = self.read_bounded_run_file(
            Path::new("redaction-report.json"),
            MAX_ARTIFACT_FILE_BYTES,
            "完成来源脱敏报告",
        )?;
        let digest = sha256_digest(&bytes);
        let stored = validate_stored_redaction_report(&bytes, providers)?;
        drop(bytes);
        let actual = self.scan_artifacts(providers)?;
        if stored != StoredRedactionReport::from(actual) {
            return Err("完成来源脱敏报告与当前全目录真实重扫结果不一致".to_owned());
        }
        Ok(digest)
    }

    /// 对完成来源的全部受审计产物计算路径到内容摘要映射，用于检测合并期间的任意改写。
    fn completed_source_snapshot(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<BTreeMap<String, String>, String> {
        let redaction_sha256 = self.read_and_verify_completed_redaction_report(providers)?;
        let mut paths = collect_artifact_paths(&self.run_dir)?;
        paths.sort_by(|left, right| left.relative.cmp(&right.relative));
        let mut snapshot = BTreeMap::new();
        for artifact in paths {
            let digest = self.sha256_bounded_run_file_snapshot(
                Path::new(&artifact.relative),
                MAX_ARTIFACT_FILE_BYTES,
                Some(artifact.byte_len),
                Some(&artifact.identity),
                &format!("完成来源快照产物 {}", artifact.relative),
            )?;
            snapshot.insert(artifact.relative, digest);
        }
        snapshot.insert("redaction-report.json".to_owned(), redaction_sha256);
        let expected_resume = serialize_json_artifact("resume.json", manifest)?;
        verify_completed_snapshot_bytes(
            &snapshot,
            "resume.json",
            expected_resume.as_bytes(),
            "完成来源恢复清单",
        )?;
        if manifest.identity.schema_version != RETRY_SOURCE_RESUME_SCHEMA_VERSION {
            let seal = manifest
                .completion_artifact_seal
                .as_ref()
                .ok_or_else(|| "当前完成来源缺少事实产物封印".to_owned())?;
            for artifact in &seal.artifacts {
                if completed_snapshot_digest(&snapshot, &artifact.path, "封印产物")?
                    != artifact.sha256
                {
                    return Err(format!(
                        "完成来源快照未通过 Resume 产物封印：{}",
                        artifact.path
                    ));
                }
            }
            if snapshot.len() != seal.artifacts.len().saturating_add(1) {
                return Err("完成来源快照包含 Resume 封印之外的未知事实产物".to_owned());
            }
        }
        Ok(snapshot)
    }

    /// 从已完成来源日志按固定策略构造不含正文的精确补测选择。
    pub(crate) async fn create_retry_selection(
        &self,
        source_manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
        provider_id: &str,
        through_sequence: u64,
        expected_source_executable_sha256: &str,
    ) -> Result<RetrySelectionManifest, String> {
        source_manifest.validate_retry_source_identity(
            providers,
            provider_id,
            expected_source_executable_sha256,
        )?;
        if through_sequence == 0 || through_sequence > source_manifest.journal_sequence {
            return Err(format!(
                "精确补测截止序号必须位于 1..={} 的已提交范围内",
                source_manifest.journal_sequence
            ));
        }
        self.verify_committed_fixtures(source_manifest, providers)
            .await?;
        let journal = self.load_progress_journal(
            source_manifest,
            providers,
            JOURNAL_SCHEMA_VERSION,
            JournalTailPolicy::ReadOnlyReject,
        )?;
        if journal.len() != source_manifest.records.len()
            || source_manifest.journal_sequence
                != u64::try_from(journal.len())
                    .map_err(|_| "精确补测来源日志记录数不能表示为 u64".to_owned())?
        {
            return Err("已完成补测来源的恢复清单与提交日志记录数不一致".to_owned());
        }
        let cases = select_retry_cases(&journal, provider_id, through_sequence);
        drop(journal);
        let source_resume_sha256 = self.sha256_bounded_run_file(
            Path::new("resume.json"),
            MAX_RESUME_MANIFEST_BYTES,
            "精确补测来源恢复清单",
        )?;
        let source_journal_sha256 = self.sha256_bounded_run_file(
            Path::new("sanitized-logs/progress.jsonl"),
            MAX_PROGRESS_JOURNAL_BYTES,
            "精确补测来源提交日志",
        )?;
        let source_result_bytes = self.read_bounded_run_file(
            Path::new("result.json"),
            MAX_ARTIFACT_FILE_BYTES,
            "精确补测来源最终报告",
        )?;
        let source_result_sha256 = sha256_digest(&source_result_bytes);
        let source_report_schema = source_manifest.retry_source_report_schema()?;
        validate_stored_run_report(
            &source_result_bytes,
            source_manifest,
            providers,
            &[source_report_schema],
        )?;
        drop(source_result_bytes);
        let source_redaction_report_sha256 =
            self.read_and_verify_completed_redaction_report(providers)?;
        if cases.is_empty() {
            return Err("固定补测策略没有选出任何失败 tuple，拒绝创建空补测运行".to_owned());
        }
        let mut selection = RetrySelectionManifest {
            lineage: RetryLineage {
                schema_version: RETRY_SELECTION_SCHEMA_VERSION.to_owned(),
                source_run_id: source_manifest.run.run_id.clone(),
                source_runtime_commit: source_manifest.run.runtime_commit.clone(),
                source_executable_sha256: source_manifest.identity.executable_sha256.clone(),
                source_authentication: if source_manifest.identity.schema_version
                    == RETRY_SOURCE_RESUME_SCHEMA_VERSION
                {
                    LEGACY_UNAUTHENTICATED_SOURCE_LEVEL.to_owned()
                } else {
                    AUTHENTICATED_SOURCE_LEVEL.to_owned()
                },
                source_resume_schema_version: source_manifest.identity.schema_version.clone(),
                source_harness_contract_id: source_manifest.identity.harness_contract_id.clone(),
                source_report_schema_version: source_report_schema.to_owned(),
                source_resume_sha256,
                source_journal_sha256,
                source_result_sha256,
                source_redaction_report_sha256,
                provider_id: provider_id.to_owned(),
                through_sequence,
                policy: RETRY_SELECTION_POLICY.to_owned(),
                selected_records: cases.len(),
                selection_sha256: String::new(),
            },
            cases,
        };
        selection.lineage.selection_sha256 = selection.calculated_sha256()?;
        selection.validate()?;
        Ok(selection)
    }

    /// 在首个补测请求前持久化并扫描完整精确选择清单。
    pub(crate) fn write_retry_selection(
        &self,
        selection: &RetrySelectionManifest,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        selection.validate()?;
        self.write_json("retry-selection.json", selection, providers)
    }

    /// 只读加载并核对补测选择 Sidecar；恢复路径绝不创建、修复或覆盖该文件。
    pub(crate) fn load_and_verify_retry_selection_sidecar(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        let Some(selection) = manifest.retry_selection() else {
            return Ok(());
        };
        let bytes = self.read_bounded_run_file(
            Path::new("retry-selection.json"),
            MAX_ARTIFACT_FILE_BYTES,
            "补测恢复独立选择清单",
        )?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "补测恢复独立选择清单必须是有效 UTF-8".to_owned())?;
        ensure_safe_artifact(text, providers)?;
        let stored: RetrySelectionManifest = serde_json::from_str(text)
            .map_err(|error| format!("补测恢复独立选择清单结构无效：{error}"))?;
        if &stored != selection {
            return Err("补测恢复独立选择清单与恢复清单不一致".to_owned());
        }
        stored.validate()?;
        Ok(())
    }

    /// 使用明确版本和尾部策略加载、对账并扫描一份恢复清单。
    fn load_manifest_with_policy(
        &self,
        providers: &[&ProviderEntry],
        accepted_resume_schemas: &[&str],
        journal_schema: &str,
        tail_policy: JournalTailPolicy,
    ) -> Result<ResumeManifest, String> {
        let bytes = self.read_bounded_run_file(
            Path::new("resume.json"),
            MAX_RESUME_MANIFEST_BYTES,
            "恢复清单",
        )?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| "恢复清单必须是有效 UTF-8".to_owned())?;
        ensure_safe_artifact(text, providers)?;
        let mut manifest: ResumeManifest =
            serde_json::from_str(text).map_err(|error| format!("恢复清单结构无效：{error}"))?;
        if !accepted_resume_schemas.contains(&manifest.identity.schema_version.as_str()) {
            return Err(format!(
                "恢复清单 schema 不受支持：{}",
                manifest.identity.schema_version
            ));
        }
        manifest.identity.validate_hmac_proof_formats()?;
        manifest.validate_persisted_state_proofs(providers)?;
        if manifest.identity.schema_version == RESUME_SCHEMA_VERSION {
            let canonical = serialize_json_artifact("恢复清单", &manifest)?;
            if canonical.as_bytes() != bytes {
                return Err("恢复清单不是 Harness 唯一规范 JSON 编码".to_owned());
            }
        }
        for (key, record) in &manifest.records {
            if key != &record.stable_key() {
                return Err("恢复清单记录键与记录身份不一致".to_owned());
            }
        }
        let journal =
            self.load_progress_journal(&manifest, providers, journal_schema, tail_policy)?;
        reconcile_progress_journal(&mut manifest, &journal)?;
        manifest.validate_recovery_lineage()?;
        let mut journal_records = BTreeMap::new();
        for entry in &journal {
            insert_idempotent_record(
                &mut journal_records,
                entry.record.stable_key(),
                entry.record.clone(),
                "恢复提交日志",
            )?;
        }
        self.journal_records.replace(journal_records);
        self.next_journal_sequence
            .set(manifest.journal_sequence.saturating_add(1));
        let journal_tail_mac = authenticated_journal_tail(&manifest, &journal)?;
        *self.journal_tail_mac.borrow_mut() = journal_tail_mac;
        self.bind_journal_authentication(&manifest)?;
        self.verify_completion_artifact_seal(&manifest)?;
        let scan = self.scan_artifacts(providers)?;
        if !scan.passed {
            return Err("恢复运行目录未通过秘密、隐私或纯合成提示词扫描".to_owned());
        }
        Ok(manifest)
    }

    /// 读取全部换行终止的提交记录，并安全截断崩溃留下的尾部半行。
    fn load_progress_journal(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
        journal_schema: &str,
        tail_policy: JournalTailPolicy,
    ) -> Result<Vec<OwnedProbeJournalEntry>, String> {
        let Some(_) = self.validated_checkpoint_path()? else {
            self.journal_byte_len.set(0);
            return Ok(Vec::new());
        };
        let access = if matches!(tail_policy, JournalTailPolicy::RepairInPlace) {
            StableFileAccess::ReadWrite
        } else {
            StableFileAccess::ReadOnly
        };
        let (file, original_len, identity) = self.open_stable_run_file(
            Path::new("sanitized-logs/progress.jsonl"),
            access,
            StableFileCreation::Existing,
            MAX_PROGRESS_JOURNAL_BYTES,
            None,
            None,
            "恢复提交日志",
        )?;
        let mut reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut records = BTreeMap::new();
        let mut expected_sequence = 1_u64;
        let mut expected_previous_mac = JOURNAL_INITIAL_MAC.to_owned();
        let mut complete_len = 0_u64;
        let mut line_index = 0_usize;
        let mut incomplete_tail = false;
        while let Some((mut line, terminated)) = read_limited_journal_line(&mut reader)? {
            line_index = line_index
                .checked_add(1)
                .ok_or_else(|| "恢复提交日志行数溢出".to_owned())?;
            if !terminated {
                incomplete_tail = true;
                break;
            }
            let raw_text = std::str::from_utf8(&line)
                .map_err(|_| format!("恢复提交日志第 {line_index} 行必须是有效 UTF-8"))?;
            ensure_safe_artifact(raw_text, providers)?;
            complete_len = complete_len
                .checked_add(
                    u64::try_from(line.len())
                        .map_err(|_| "恢复提交日志完整行长度不能表示为 u64".to_owned())?,
                )
                .ok_or_else(|| "恢复提交日志完整字节偏移溢出".to_owned())?;
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = std::str::from_utf8(&line)
                .map_err(|_| format!("恢复提交日志第 {line_index} 行必须是有效 UTF-8"))?;
            if line.is_empty() {
                return Err(format!("恢复提交日志第 {line_index} 行为空"));
            }
            if entries.len() >= MAX_PROGRESS_JOURNAL_RECORDS {
                return Err(format!(
                    "恢复提交日志记录数超过 {} 条安全上限",
                    MAX_PROGRESS_JOURNAL_RECORDS
                ));
            }
            let entry: OwnedProbeJournalEntry = serde_json::from_str(line)
                .map_err(|error| format!("恢复提交日志第 {line_index} 行结构无效：{error}"))?;
            if entry.schema_version != journal_schema {
                return Err(format!(
                    "恢复提交日志第 {} 行 schema 不受支持：{}",
                    line_index, entry.schema_version
                ));
            }
            if entry.sequence != expected_sequence {
                return Err(format!(
                    "恢复提交日志序号不连续：第 {} 行期望 {expected_sequence}，实际 {}",
                    line_index, entry.sequence
                ));
            }
            expected_sequence = expected_sequence
                .checked_add(1)
                .ok_or_else(|| "恢复提交日志序号溢出".to_owned())?;
            if manifest.identity.schema_version == RESUME_SCHEMA_VERSION {
                let previous_mac = entry
                    .previous_mac
                    .as_deref()
                    .ok_or_else(|| format!("恢复提交日志第 {line_index} 行缺少前序链式 MAC"))?;
                let record_mac = entry
                    .record_mac
                    .as_deref()
                    .ok_or_else(|| format!("恢复提交日志第 {line_index} 行缺少记录 HMAC"))?;
                if !valid_hmac_sha256_proof(previous_mac)
                    || !valid_hmac_sha256_proof(record_mac)
                    || previous_mac != expected_previous_mac
                {
                    return Err(format!(
                        "恢复提交日志第 {line_index} 行的链式 MAC 格式或前序绑定无效"
                    ));
                }
                if !manifest
                    .identity
                    .providers
                    .iter()
                    .any(|identity| identity.provider_id == entry.record.provider_id)
                {
                    return Err(format!(
                        "恢复提交日志第 {line_index} 行引用了恢复身份之外的 Provider"
                    ));
                }
                let provider = resolve_resume_provider(&entry.record.provider_id, providers)?;
                let canonical_record = serde_json::to_vec(&entry.record)
                    .map_err(|error| format!("无法规范序列化 Journal 探测记录：{error}"))?;
                let expected_record_mac = provider.journal_record_proof(
                    &manifest.identity.run_salt,
                    manifest.journal_selection_domain(),
                    entry.sequence,
                    previous_mac,
                    &canonical_record,
                );
                if record_mac != expected_record_mac {
                    return Err(format!(
                        "恢复提交日志第 {line_index} 行未通过当前 Provider 凭据认证"
                    ));
                }
                expected_previous_mac = record_mac.to_owned();
            } else if entry.previous_mac.is_some() || entry.record_mac.is_some() {
                return Err(format!(
                    "显式 legacy v5 来源的恢复提交日志第 {line_index} 行不能携带当前认证字段"
                ));
            }
            insert_idempotent_record(
                &mut records,
                entry.record.stable_key(),
                entry.record.clone(),
                "恢复提交日志",
            )?;
            entries.push(entry);
        }
        let file = reader.into_inner();
        let final_metadata = file
            .metadata()
            .map_err(|error| format!("无法确认已读取恢复提交日志句柄：{error}"))?;
        if !final_metadata.is_file()
            || final_metadata.len() != original_len
            || regular_file_identity_from_open_handle(&file, &final_metadata, "恢复提交日志")?
                != identity
        {
            return Err("恢复提交日志在读取期间文件身份或长度发生变化".to_owned());
        }
        self.verify_stable_run_file_identity(
            Path::new("sanitized-logs/progress.jsonl"),
            &identity,
            "恢复提交日志",
        )?;
        if incomplete_tail || complete_len < original_len {
            if matches!(tail_policy, JournalTailPolicy::ReadOnlyReject) {
                return Err(
                    "隔离恢复来源提交日志包含尾部半行；只读恢复拒绝修复或猜测来源状态".to_owned(),
                );
            }
            self.verify_stable_run_file_identity(
                Path::new("sanitized-logs/progress.jsonl"),
                &identity,
                "待修复恢复提交日志",
            )?;
            file.set_len(complete_len)
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("无法截断并同步恢复提交日志尾部半行：{error}"))?;
            let repaired_metadata = file
                .metadata()
                .map_err(|error| format!("无法确认已修复恢复提交日志句柄：{error}"))?;
            if repaired_metadata.len() != complete_len
                || regular_file_identity_from_open_handle(
                    &file,
                    &repaired_metadata,
                    "已修复恢复提交日志",
                )? != identity
            {
                return Err("恢复提交日志尾部修复后文件身份或长度不一致".to_owned());
            }
            self.verify_stable_run_file_identity(
                Path::new("sanitized-logs/progress.jsonl"),
                &identity,
                "已修复恢复提交日志",
            )?;
        }
        self.journal_byte_len.set(complete_len);
        Ok(entries)
    }

    /// 把严格验证的旧运行事实复制到使用新运行标识和当前构建身份的隔离目录。
    pub(crate) async fn create_recovery_copy(
        &self,
        source_manifest: &ResumeManifest,
        output_root: &Path,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        expected_source_executable_sha256: &str,
        allow_unauthenticated_legacy: bool,
    ) -> Result<(Self, ResumeManifest), String> {
        let new_run_id = new_run_id()?;
        self.create_recovery_copy_with_run_id_and_post_copy_hook(
            source_manifest,
            output_root,
            options,
            providers,
            expected_source_executable_sha256,
            allow_unauthenticated_legacy,
            new_run_id,
            |_| Ok(()),
        )
        .await
    }

    /// 使用确定运行标识和复制后故障钩执行恢复，供所有权与清理路径做无网络回归验证。
    #[allow(clippy::too_many_arguments)]
    async fn create_recovery_copy_with_run_id_and_post_copy_hook<F>(
        &self,
        source_manifest: &ResumeManifest,
        output_root: &Path,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        expected_source_executable_sha256: &str,
        allow_unauthenticated_legacy: bool,
        new_run_id: String,
        post_copy_hook: F,
    ) -> Result<(Self, ResumeManifest), String>
    where
        F: FnOnce(&Self) -> Result<(), String>,
    {
        self.create_recovery_copy_with_run_id_and_hooks(
            source_manifest,
            output_root,
            options,
            providers,
            expected_source_executable_sha256,
            allow_unauthenticated_legacy,
            new_run_id,
            |_| Ok(()),
            post_copy_hook,
        )
        .await
    }

    /// 在创建前后分别注入确定性故障，验证路径竞态与拥有目标清理边界。
    #[allow(clippy::too_many_arguments)]
    async fn create_recovery_copy_with_run_id_and_hooks<P, F>(
        &self,
        source_manifest: &ResumeManifest,
        output_root: &Path,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        expected_source_executable_sha256: &str,
        allow_unauthenticated_legacy: bool,
        new_run_id: String,
        pre_create_hook: P,
        post_copy_hook: F,
    ) -> Result<(Self, ResumeManifest), String>
    where
        P: FnOnce(&Path) -> Result<(), String>,
        F: FnOnce(&Self) -> Result<(), String>,
    {
        source_manifest.validate_recovery_source_identity(
            options,
            providers,
            expected_source_executable_sha256,
            allow_unauthenticated_legacy,
        )?;
        if source_manifest.finished {
            return Err("隔离恢复来源已经完成，拒绝建立派生运行".to_owned());
        }
        validate_recovery_run_id(&new_run_id)?;
        let validated_output_root = validate_recovery_output_root(&self.run_dir, output_root)?;
        let import_plan =
            self.recovery_import_plan(source_manifest, providers, allow_unauthenticated_legacy)?;

        let source_resume_bytes = self.read_bounded_run_file(
            Path::new("resume.json"),
            MAX_RESUME_MANIFEST_BYTES,
            "隔离恢复来源清单",
        )?;
        let source_journal_bytes = match self.validated_checkpoint_path()? {
            Some(_) => self.read_bounded_run_file(
                Path::new("sanitized-logs/progress.jsonl"),
                MAX_PROGRESS_JOURNAL_BYTES,
                "隔离恢复来源提交日志",
            )?,
            None if source_manifest.journal_sequence == 0 => Vec::new(),
            None => return Err("隔离恢复来源清单引用了不存在的提交日志".to_owned()),
        };
        if source_manifest.journal_sequence
            != u64::try_from(source_manifest.records.len())
                .map_err(|_| "隔离恢复来源记录数不能表示为 u64".to_owned())?
        {
            return Err("隔离恢复来源日志序号与唯一记录数不一致".to_owned());
        }

        let source_resume_sha256 = sha256_digest(&source_resume_bytes);
        let source_journal_sha256 = sha256_digest(&source_journal_bytes);
        pre_create_hook(&validated_output_root.resolved)?;
        validated_output_root.verify_existing_anchor()?;
        let confirmed_output_root =
            validate_recovery_output_root(&self.run_dir, &validated_output_root.resolved)?;
        if !paths_equal(
            &validated_output_root.resolved,
            &confirmed_output_root.resolved,
        ) {
            return Err("恢复输出根目录在两次创建前验证之间发生变化".to_owned());
        }
        confirmed_output_root.verify_existing_anchor()?;
        let destination = ReportStore::create(&confirmed_output_root.resolved, &new_run_id)?;
        if let Err(error) = destination.write_recovery_incomplete_marker() {
            return Err(retained_recovery_target_error(error));
        }
        let actual_output_root = destination
            .run_dir()
            .parent()
            .expect("成功创建的恢复目标必然是输出根的单层子目录");
        let post_create_output_root =
            validate_recovery_output_root(&self.run_dir, actual_output_root)
                .map_err(retained_recovery_target_error)?;
        if !paths_equal(actual_output_root, &post_create_output_root.resolved)
            || !paths_equal(actual_output_root, &confirmed_output_root.resolved)
        {
            return Err(retained_recovery_target_error(
                "恢复输出根目录在验证与目标创建之间发生变化".to_owned(),
            ));
        }
        validated_output_root
            .verify_existing_anchor()
            .map_err(retained_recovery_target_error)?;
        confirmed_output_root
            .verify_existing_anchor()
            .map_err(retained_recovery_target_error)?;
        post_create_output_root
            .verify_existing_anchor()
            .map_err(retained_recovery_target_error)?;
        let populated = self.populate_recovery_copy(
            &destination,
            source_manifest,
            &import_plan,
            options,
            providers,
            new_run_id,
            &source_resume_sha256,
            &source_journal_sha256,
        );
        let recovered_manifest = match populated {
            Ok(manifest) => manifest,
            Err(error) => return Err(retained_recovery_target_error(error)),
        };

        let source_unchanged = (|| -> Result<(), String> {
            post_copy_hook(self)?;
            let resume_after = self.read_bounded_run_file(
                Path::new("resume.json"),
                MAX_RESUME_MANIFEST_BYTES,
                "隔离恢复后来源清单",
            )?;
            let journal_after = match self.validated_checkpoint_path()? {
                Some(_) => self.read_bounded_run_file(
                    Path::new("sanitized-logs/progress.jsonl"),
                    MAX_PROGRESS_JOURNAL_BYTES,
                    "隔离恢复后来源提交日志",
                )?,
                None => Vec::new(),
            };
            if resume_after != source_resume_bytes || journal_after != source_journal_bytes {
                return Err("隔离恢复期间来源清单或提交日志发生变化，拒绝继续".to_owned());
            }
            Ok(())
        })();
        if let Err(error) = source_unchanged {
            return Err(retained_recovery_target_error(error));
        }
        destination
            .clear_recovery_incomplete_marker()
            .map_err(retained_recovery_target_error)?;
        Ok((destination, recovered_manifest))
    }

    /// 向已经独占创建的恢复目录复制 Fixture、重建 Journal 并写入双重 Lineage。
    #[allow(clippy::too_many_arguments)]
    fn populate_recovery_copy(
        &self,
        destination: &ReportStore,
        source_manifest: &ResumeManifest,
        import_plan: &RecoveryImportPlan,
        options: &RuntimeOptions,
        providers: &[&ProviderEntry],
        new_run_id: String,
        source_resume_sha256: &str,
        source_journal_sha256: &str,
    ) -> Result<ResumeManifest, String> {
        let source_executable_sha256 = source_manifest.identity.executable_sha256.clone();
        let recovery_executable_sha256 = current_executable_sha256()?;
        let imported_fixture_paths = import_plan.fixture_paths.clone();
        let lineage = RecoveryLineage {
            schema_version: RECOVERY_LINEAGE_SCHEMA_VERSION.to_owned(),
            source_run_id: source_manifest.run.run_id.clone(),
            source_runtime_commit: source_manifest.run.runtime_commit.clone(),
            source_executable_sha256: source_executable_sha256.clone(),
            source_resume_sha256: source_resume_sha256.to_owned(),
            source_journal_sha256: source_journal_sha256.to_owned(),
            source_resume_schema_version: Some(source_manifest.identity.schema_version.clone()),
            source_harness_contract_id: Some(source_manifest.identity.harness_contract_id.clone()),
            recovery_executable_sha256: recovery_executable_sha256.clone(),
            recovered_at: timestamp()?,
            imported_records: import_plan.records.len(),
            imported_fixtures: imported_fixture_paths.len(),
            parent: source_manifest.run.recovery_lineage.clone().map(Box::new),
            rerun_records: import_plan.rerun_records.clone(),
            policy: import_plan.policy.to_owned(),
        };
        let origin = RecoveredProbeOrigin {
            source_run_id: lineage.source_run_id.clone(),
            source_runtime_commit: lineage.source_runtime_commit.clone(),
            source_executable_sha256,
        };
        let mut records = import_plan.records.clone();
        for record in records.values_mut() {
            if record.recovered_from.is_none() {
                record.recovered_from = Some(origin.clone());
            }
        }
        let mut run = RunMetadata::new(new_run_id, options)?;
        run.recovery_lineage = Some(lineage.clone());
        let identity = ResumeIdentity::current(options, providers, &new_run_salt()?)?;
        if identity.executable_sha256 != recovery_executable_sha256 {
            return Err("恢复副本身份计算期间当前可执行文件字节发生变化".to_owned());
        }
        let mut recovered = ResumeManifest {
            identity,
            run,
            candidate_sets: source_manifest.candidate_sets.clone(),
            journal_sequence: u64::try_from(records.len())
                .map_err(|_| "恢复副本记录数不能表示为 u64".to_owned())?,
            records,
            journal_tail_mac: Some(JOURNAL_INITIAL_MAC.to_owned()),
            finished: false,
            retry_selection: None,
            state_proofs: Vec::new(),
            completion_artifact_seal: None,
        };
        recovered.validate_recovery_lineage()?;

        for relative in &imported_fixture_paths {
            validate_fixture_relative_path(relative)?;
            let bytes = self.read_bounded_run_file(
                Path::new(relative),
                MAX_FIXTURE_FILE_BYTES,
                &format!("隔离恢复来源 Fixture {relative}"),
            )?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| format!("隔离恢复来源 Fixture 必须是有效 UTF-8：{relative}"))?;
            destination.write_immutable_relative_text(relative, text, providers)?;
        }

        let mut journal = String::new();
        let mut previous_mac = JOURNAL_INITIAL_MAC.to_owned();
        for (index, record) in recovered.records.values().enumerate() {
            let sequence = u64::try_from(index)
                .map_err(|_| "恢复副本日志序号不能表示为 u64".to_owned())?
                .checked_add(1)
                .ok_or_else(|| "恢复副本日志序号溢出".to_owned())?;
            let provider = resolve_resume_provider(&record.provider_id, providers)?;
            let canonical_record = serde_json::to_vec(record)
                .map_err(|error| format!("无法规范序列化恢复副本探测记录：{error}"))?;
            let record_mac = provider.journal_record_proof(
                &recovered.identity.run_salt,
                recovered.journal_selection_domain(),
                sequence,
                &previous_mac,
                &canonical_record,
            );
            let entry = ProbeJournalEntry {
                schema_version: JOURNAL_SCHEMA_VERSION,
                sequence,
                previous_mac: &previous_mac,
                record_mac: &record_mac,
                record,
            };
            let line = serde_json::to_string(&entry)
                .map_err(|error| format!("无法序列化恢复副本提交日志：{error}"))?;
            ensure_safe_artifact(&line, providers)?;
            if line.len().saturating_add(1) > MAX_PROGRESS_JOURNAL_LINE_BYTES {
                return Err(format!(
                    "恢复副本提交日志单行超过 {} 字节安全上限",
                    MAX_PROGRESS_JOURNAL_LINE_BYTES
                ));
            }
            if journal
                .len()
                .checked_add(line.len())
                .and_then(|length| length.checked_add(1))
                .is_none_or(|length| {
                    u64::try_from(length).unwrap_or(u64::MAX) > MAX_PROGRESS_JOURNAL_BYTES
                })
            {
                return Err(format!(
                    "恢复副本提交日志超过 {} 字节安全上限",
                    MAX_PROGRESS_JOURNAL_BYTES
                ));
            }
            journal.push_str(&line);
            journal.push('\n');
            previous_mac = record_mac;
        }
        recovered.journal_tail_mac = Some(previous_mac.clone());
        replace_file_contents(&destination.checkpoint_path, &journal, "恢复副本提交日志")?;
        *destination.journal_tail_mac.borrow_mut() = previous_mac;
        destination.journal_byte_len.set(
            u64::try_from(journal.len())
                .map_err(|_| "恢复副本提交日志长度不能表示为 u64".to_owned())?,
        );
        destination
            .next_journal_sequence
            .set(recovered.journal_sequence.saturating_add(1));
        destination.write_json("recovery-lineage.json", &lineage, providers)?;
        destination.write_resume_manifest(&recovered, providers)?;
        let loaded = destination.load_resume_manifest(providers)?;
        loaded.validate_identity(options, providers)?;
        Ok(loaded)
    }

    /// 在恢复身份验证后删除 Fixture 已同步但 Journal 尚未完整提交留下的合法孤儿。
    pub(crate) fn repair_uncommitted_fixtures(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<usize, String> {
        let repaired_staging = self.remove_abandoned_fixture_staging_files()?;
        let disk_fixtures = collect_resume_fixture_files(&self.run_dir)?;
        let mut referenced = BTreeSet::new();
        for record in manifest.records.values() {
            for relative in &record.fixture_paths {
                validate_fixture_relative_path(relative)?;
                if !referenced.insert(relative.clone()) {
                    return Err(format!("多个已提交记录重复引用 Fixture：{relative}"));
                }
            }
        }
        let mut removable = Vec::new();
        for relative in disk_fixtures.difference(&referenced) {
            let bytes = self.read_bounded_run_file(
                Path::new(relative),
                MAX_FIXTURE_FILE_BYTES,
                &format!("待修复孤儿 Fixture {relative}"),
            )?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| format!("待修复孤儿 Fixture 必须是有效 UTF-8：{relative}"))?;
            ensure_safe_artifact(text, providers)?;
            let fixture = parse_fixture_envelope(text)?;
            if fixture.payload.run_id != manifest.run.run_id {
                return Err(format!("孤儿 Fixture 不属于当前恢复运行：{relative}"));
            }
            if manifest.records.contains_key(&fixture.payload.stable_key) {
                return Err(format!(
                    "孤儿 Fixture 的稳定键已提交但引用路径冲突：{relative}"
                ));
            }
            let expected_relative =
                fixture_relative_path(&fixture.payload, &fixture.content_sha256)?;
            if relative != &expected_relative {
                return Err(format!(
                    "孤儿 Fixture 文件名与内容寻址摘要不一致：{relative}"
                ));
            }
            let expected_marker = marker_from_probe_stable_key(
                &fixture.payload.stable_key,
                fixture.payload.capability.starts_with("diagnostic_"),
            );
            validate_fixture_prompts(&fixture, &expected_marker)?;
            validate_fixture_request_binding(&fixture)?;
            removable.push(relative.clone());
        }
        for relative in &removable {
            let path = validated_run_descendant(
                &self.run_dir,
                Path::new(relative),
                RunPathKind::File,
                "待删除孤儿 Fixture",
            )?;
            fs::remove_file(&path)
                .map_err(|error| format!("无法删除未提交孤儿 Fixture {relative}：{error}"))?;
        }
        repaired_staging
            .checked_add(removable.len())
            .ok_or_else(|| "恢复清理的 Fixture 文件数量溢出".to_owned())
    }

    /// 删除独占运行目录内由原子 Fixture 写入中断留下的保留临时普通文件。
    fn remove_abandoned_fixture_staging_files(&self) -> Result<usize, String> {
        let fixture_dir = validated_run_descendant(
            &self.run_dir,
            Path::new("fixtures"),
            RunPathKind::Directory,
            "恢复 Fixture 目录",
        )?;
        let mut staging = Vec::new();
        for entry in fs::read_dir(&fixture_dir)
            .map_err(|error| format!("无法枚举恢复 Fixture 目录：{error}"))?
        {
            let entry = entry.map_err(|error| format!("无法读取恢复 Fixture 目录项：{error}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "恢复 Fixture 文件名必须是有效 Unicode".to_owned())?;
            if !is_fixture_staging_name(&name) {
                continue;
            }
            let relative = format!("fixtures/{name}");
            let path = validated_run_descendant(
                &self.run_dir,
                Path::new(&relative),
                RunPathKind::File,
                "待删除 Fixture 临时文件",
            )?;
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("无法读取 Fixture 临时文件元数据：{error}"))?;
            if is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err(format!(
                    "Fixture 临时路径必须是直属普通文件且不能是链接或子目录：{name}"
                ));
            }
            if metadata.len() > MAX_FIXTURE_FILE_BYTES {
                return Err(format!(
                    "Fixture 临时文件 {name} 超过 {MAX_FIXTURE_FILE_BYTES} 字节安全上限"
                ));
            }
            staging.push((relative, path));
        }
        for (relative, path) in &staging {
            fs::remove_file(path)
                .map_err(|error| format!("无法删除中断的 Fixture 临时文件 {relative}：{error}"))?;
        }
        Ok(staging.len())
    }

    /// 校验一条记录引用的唯一 Fixture，并返回已经完成结构与身份绑定验证的 Envelope。
    fn verify_record_fixture(
        &self,
        manifest: &ResumeManifest,
        key: &str,
        record: &ProbeRecord,
        providers: &[&ProviderEntry],
        expectations: RecordFixtureExpectations<'_>,
        referenced_fixtures: &mut BTreeSet<String>,
    ) -> Result<Option<ProbeFixtureEnvelope>, String> {
        if record.attempts == 0 {
            if !record.fixture_paths.is_empty() {
                return Err(format!("恢复记录 {key} 未发送请求却引用了 Fixture"));
            }
            return Ok(None);
        }
        if record.fixture_paths.len() != 1 {
            return Err(format!(
                "恢复记录 {key} 已发送请求，必须且只能引用一个 Fixture"
            ));
        }
        let relative = &record.fixture_paths[0];
        validate_fixture_relative_path(relative)?;
        if !referenced_fixtures.insert(relative.clone()) {
            return Err(format!("多个恢复记录重复引用 Fixture：{relative}"));
        }
        if !expectations.disk_fixtures.contains(relative) {
            return Err(format!("恢复记录 {key} 的完整 Fixture 不存在：{relative}"));
        }
        let bytes = self.read_bounded_run_file(
            Path::new(relative),
            MAX_FIXTURE_FILE_BYTES,
            &format!("恢复 Fixture {relative}"),
        )?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| format!("恢复 Fixture 必须是有效 UTF-8：{relative}"))?;
        ensure_safe_artifact(text, providers)?;
        let fixture = parse_synthetic_fixture(text, expectations.marker)?;
        validate_fixture_record_binding(manifest, record, relative, &fixture)?;
        Ok(Some(fixture))
    }

    /// 在只读来源上生成导入计划；唯一允许排除的是显式接受的 v14 取消重试缺口。
    fn recovery_import_plan(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
        allow_unauthenticated_legacy: bool,
    ) -> Result<RecoveryImportPlan, String> {
        validate_resume_candidate_sets(manifest, providers)?;
        let legacy_contract = manifest.identity.schema_version
            == RETRY_SOURCE_RESUME_SCHEMA_VERSION
            && manifest.identity.harness_contract_id == RETRY_SOURCE_HARNESS_CONTRACT_ID;
        if legacy_contract && !allow_unauthenticated_legacy {
            return Err("未显式接受的 v14 来源不能生成隔离升级计划".to_owned());
        }
        let disk_fixtures = collect_resume_fixture_files(&self.run_dir)?;
        let mut referenced_fixtures = BTreeSet::new();
        let mut imported_fixture_paths = BTreeSet::new();
        let mut records = BTreeMap::new();
        let mut rerun_records = Vec::new();
        for (key, record) in &manifest.records {
            if !record.reusable() {
                return Err(format!("隔离恢复来源记录 {key} 不是已提交终态"));
            }
            let validation_error = format!("恢复记录 {key} 的取消提前完成状态没有通过真实响应重放");
            let (mut rerun, expected_marker, verify_completed_cancellation_gap) =
                match validate_reusable_record_with_current_gap(manifest, key, record, providers) {
                    Ok((_marker, _verify_gap))
                        if legacy_contract
                            && legacy_unreplayable_cancellation_record(
                                key,
                                record,
                                &validation_error,
                            ) =>
                    {
                        (
                            true,
                            marker_from_probe_stable_key(&record.stable_key, false),
                            false,
                        )
                    }
                    Ok((marker, verify_gap)) => (false, marker, verify_gap),
                    Err(error)
                        if legacy_contract
                            && legacy_unreplayable_cancellation_record(key, record, &error) =>
                    {
                        (
                            true,
                            marker_from_probe_stable_key(&record.stable_key, false),
                            false,
                        )
                    }
                    Err(error) => return Err(error),
                };
            let fixture = self.verify_record_fixture(
                manifest,
                key,
                record,
                providers,
                RecordFixtureExpectations {
                    marker: &expected_marker,
                    disk_fixtures: &disk_fixtures,
                },
                &mut referenced_fixtures,
            )?;
            if verify_completed_cancellation_gap
                && fixture.as_ref().is_none_or(|fixture| {
                    !current_completed_cancellation_replay_gap_fixture(record, fixture)
                })
            {
                return Err(validation_error);
            }
            if !rerun {
                if let Some(fixture) = fixture.as_ref() {
                    if let Err(error) = verify_disk_fixture(record, fixture) {
                        if legacy_contract
                            && legacy_unreplayable_failed_cancellation_fixture(
                                record, fixture, &error,
                            )
                        {
                            rerun = true;
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
            if rerun {
                let fixture = fixture
                    .as_ref()
                    .expect("需要重新请求的旧取消记录必然发送过真实请求");
                rerun_records.push(RecoveryRerunRecord {
                    source_stable_key: record.stable_key.clone(),
                    source_fixture_path: record.fixture_paths[0].clone(),
                    source_fixture_content_sha256: fixture.content_sha256.clone(),
                    provider_id: record.provider_id.clone(),
                    model: record.model.clone(),
                    protocol: record.protocol.clone(),
                    response_mode: record.response_mode.clone(),
                    capability: record.capability.clone(),
                    reason: LEGACY_CANCELLATION_RERUN_REASON.to_owned(),
                });
                continue;
            }
            imported_fixture_paths.extend(record.fixture_paths.iter().cloned());
            records.insert(key.clone(), record.clone());
        }
        if referenced_fixtures != disk_fixtures {
            let orphaned = disk_fixtures
                .difference(&referenced_fixtures)
                .cloned()
                .collect::<Vec<_>>();
            return Err(format!(
                "Fixture 目录包含未被已提交终态记录唯一引用的孤儿文件：{}",
                orphaned.join("、")
            ));
        }
        rerun_records.sort_by(|left, right| left.source_stable_key.cmp(&right.source_stable_key));
        validate_recovery_rerun_records(&rerun_records)?;
        Ok(RecoveryImportPlan {
            records,
            fixture_paths: imported_fixture_paths,
            rerun_records,
            policy: if legacy_contract {
                LEGACY_RECOVERY_POLICY
            } else {
                DIRECT_RECOVERY_POLICY
            },
        })
    }

    /// 返回 Fixture 完整存在且语义确定的可复用记录，其余记录必须重新执行。
    pub(crate) async fn reusable_records(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<BTreeMap<String, ProbeRecord>, String> {
        validate_resume_candidate_sets(manifest, providers)?;
        let disk_fixtures = collect_resume_fixture_files(&self.run_dir)?;
        let mut referenced_fixtures = BTreeSet::new();
        let mut reusable = BTreeMap::new();
        for (key, record) in &manifest.records {
            if !record.reusable() {
                continue;
            }
            let validation_error = format!("恢复记录 {key} 的取消提前完成状态没有通过真实响应重放");
            let (expected_marker, verify_completed_cancellation_gap) =
                validate_reusable_record_with_current_gap(manifest, key, record, providers)?;
            let fixture = self.verify_record_fixture(
                manifest,
                key,
                record,
                providers,
                RecordFixtureExpectations {
                    marker: &expected_marker,
                    disk_fixtures: &disk_fixtures,
                },
                &mut referenced_fixtures,
            )?;
            if verify_completed_cancellation_gap
                && fixture.as_ref().is_none_or(|fixture| {
                    !current_completed_cancellation_replay_gap_fixture(record, fixture)
                })
            {
                return Err(validation_error);
            }
            if let Some(fixture) = fixture.as_ref() {
                verify_disk_fixture(record, fixture)?;
            }
            let lookup_key = reusable_lookup_key(manifest, record, providers)?;
            if reusable.insert(lookup_key, record.clone()).is_some() {
                return Err("恢复记录映射到当前运行后产生重复稳定键".to_owned());
            }
        }
        if referenced_fixtures != disk_fixtures {
            let orphaned = disk_fixtures
                .difference(&referenced_fixtures)
                .cloned()
                .collect::<Vec<_>>();
            return Err(format!(
                "Fixture 目录包含未被已提交终态记录唯一引用的孤儿文件：{}",
                orphaned.join("、")
            ));
        }
        Ok(reusable)
    }

    /// 在首次运行声明完成前，从磁盘重新验证每条已提交终态及其唯一 Fixture。
    pub(crate) async fn verify_committed_fixtures(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        let verified = self.reusable_records(manifest, providers).await?;
        if verified.len() != manifest.records.len() {
            return Err("完成前存在未进入可复用终态的已提交探测记录".to_owned());
        }
        Ok(())
    }

    /// 把一个已脱敏探测记录立即追加到 JSONL 检查点。
    pub(crate) fn append_probe(
        &self,
        run_id: &str,
        probe: &mut ProbeRecord,
        providers: &[&ProviderEntry],
    ) -> Result<u64, String> {
        let stable_key = probe.stable_key();
        if self.journal_records.borrow().contains_key(&stable_key) {
            return Err(format!(
                "探测稳定键 {stable_key} 已经提交，拒绝重复写入日志"
            ));
        }
        if self.journal_records.borrow().len() >= MAX_PROGRESS_JOURNAL_RECORDS {
            return Err(format!(
                "探测检查点记录数超过 {} 条安全上限",
                MAX_PROGRESS_JOURNAL_RECORDS
            ));
        }
        let prepared_fixture = self.prepare_probe_fixture(run_id, probe, providers)?;
        let sequence = self.next_journal_sequence.get();
        let authentication = self
            .journal_authentication
            .borrow()
            .clone()
            .ok_or_else(|| "追加探测记录前必须先写入或加载已认证 Resume".to_owned())?;
        if authentication.run_id != run_id {
            return Err("探测记录运行标识与 Journal 认证上下文不一致".to_owned());
        }
        let provider = resolve_resume_provider(&probe.provider_id, providers)?;
        let canonical_record = serde_json::to_vec(&probe)
            .map_err(|error| format!("无法规范序列化待提交探测记录：{error}"))?;
        let previous_mac = self.journal_tail_mac.borrow().clone();
        let record_mac = provider.journal_record_proof(
            &authentication.run_salt,
            &authentication.selection_domain,
            sequence,
            &previous_mac,
            &canonical_record,
        );
        let entry = ProbeJournalEntry {
            schema_version: JOURNAL_SCHEMA_VERSION,
            sequence,
            previous_mac: &previous_mac,
            record_mac: &record_mac,
            record: probe,
        };
        let mut line = serde_json::to_string(&entry)
            .map_err(|error| format!("无法序列化探测检查点：{error}"))?;
        ensure_safe_artifact(&line, providers)?;
        line.push('\n');
        if line.len() > MAX_PROGRESS_JOURNAL_LINE_BYTES {
            return Err(format!(
                "探测检查点单行超过 {} 字节安全上限",
                MAX_PROGRESS_JOURNAL_LINE_BYTES
            ));
        }
        let creation = if self.validated_checkpoint_path()?.is_some() {
            StableFileCreation::Existing
        } else {
            StableFileCreation::CreateIfMissing
        };
        let expected_journal_len = self.journal_byte_len.get();
        let (mut file, opened_len, identity) = self.open_stable_run_file(
            Path::new("sanitized-logs/progress.jsonl"),
            StableFileAccess::Append,
            creation,
            MAX_PROGRESS_JOURNAL_BYTES,
            Some(expected_journal_len),
            None,
            "探测检查点",
        )?;
        let line_bytes =
            u64::try_from(line.len()).map_err(|_| "探测检查点单行长度不能表示为 u64".to_owned())?;
        let written_len = opened_len
            .checked_add(line_bytes)
            .ok_or_else(|| "探测检查点写入后的文件长度发生整数溢出".to_owned())?;
        if written_len > MAX_PROGRESS_JOURNAL_BYTES {
            return Err(format!(
                "探测检查点写入后将超过 {} 字节安全上限",
                MAX_PROGRESS_JOURNAL_BYTES
            ));
        }
        self.verify_stable_run_file_identity(
            Path::new("sanitized-logs/progress.jsonl"),
            &identity,
            "探测检查点",
        )?;
        if let Some((relative, fixture_text)) = prepared_fixture {
            self.write_immutable_relative_text(&relative, &fixture_text, providers)?;
        }
        self.verify_stable_run_file_identity(
            Path::new("sanitized-logs/progress.jsonl"),
            &identity,
            "写入前探测检查点",
        )?;
        file.write_all(line.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("无法写入并同步探测检查点：{error}"))?;
        let written_metadata = file
            .metadata()
            .map_err(|error| format!("无法确认已写入探测检查点句柄：{error}"))?;
        if written_metadata.len() != written_len
            || regular_file_identity_from_open_handle(&file, &written_metadata, "已写入探测检查点")?
                != identity
        {
            return Err("探测检查点写入后文件身份或长度不一致".to_owned());
        }
        self.verify_stable_run_file_identity(
            Path::new("sanitized-logs/progress.jsonl"),
            &identity,
            "已写入探测检查点",
        )?;
        probe.wire_exchanges.clear();
        probe.wire_exchange_outcomes.clear();
        self.journal_records
            .borrow_mut()
            .insert(stable_key, probe.clone());
        *self.journal_tail_mac.borrow_mut() = record_mac;
        self.next_journal_sequence.set(sequence.saturating_add(1));
        self.journal_byte_len.set(written_len);
        Ok(sequence)
    }

    /// 在写盘前完成 Fixture 的全部序列化、脱敏、语义和路径预检。
    fn prepare_probe_fixture(
        &self,
        run_id: &str,
        probe: &mut ProbeRecord,
        providers: &[&ProviderEntry],
    ) -> Result<Option<(String, String)>, String> {
        if probe.wire_exchanges.len() != probe.wire_exchange_outcomes.len()
            || probe.wire_exchanges.len() != probe.wire_response_shapes.len()
        {
            return Err("线级交换、逐交换在线归一化期望与响应结构证据数量不一致".to_owned());
        }
        if probe.wire_exchanges.is_empty() {
            return Ok(None);
        }
        let synthetic_marker = probe
            .synthetic_marker
            .as_deref()
            .ok_or_else(|| "存在真实 HTTP 交换的记录必须保存精确合成标记".to_owned())?;
        let protocol = fixture_protocol(&probe.protocol)?;
        let inspected_shapes = probe
            .wire_exchanges
            .iter()
            .map(|exchange| {
                inspect_wire_response_shape(
                    protocol,
                    exchange.response_status,
                    exchange.response_content_type.as_deref(),
                    &exchange.response_body,
                    exchange.response_body_eof_observed,
                    exchange.response_body_truncated,
                )
            })
            .collect::<Vec<_>>();
        if inspected_shapes != probe.wire_response_shapes {
            return Err("响应结构证据与线级交换独立重算结果不一致".to_owned());
        }
        let streaming = match probe.response_mode.as_str() {
            "buffered" => false,
            "streaming" => true,
            value => return Err(format!("探测记录包含未知响应模式：{value}")),
        };
        let first_exchange = probe
            .wire_exchanges
            .first()
            .expect("非空线级交换必须存在首个请求");
        let (request_marker, request_capability) =
            fixture_request_expectation(&probe.capability, synthetic_marker, 0);
        validate_initial_semantic_request(&first_exchange.model_request, &probe.model)?;
        let semantic_request = serde_json::to_value(&first_exchange.model_request)
            .map_err(|error| format!("无法序列化 Provider 中立请求：{error}"))?;
        strict_semantic_request(&semantic_request)?;
        let encoded = encode_wire_request(protocol, &first_exchange.model_request, streaming)
            .map_err(|error| format!("Fixture 首个统一请求无法由目标 Adapter 编码：{error}"))?;
        if encoded != first_exchange.request_body {
            return Err(
                "Fixture 首个 Provider 中立请求经目标 Adapter 编码后与实际请求不一致".to_owned(),
            );
        }
        validate_synthetic_request_body(
            &probe.protocol,
            &first_exchange.request_body,
            &request_marker,
            request_capability,
        )?;
        let exchanges = probe
            .wire_exchanges
            .iter()
            .zip(&probe.wire_exchange_outcomes)
            .zip(&probe.wire_response_shapes)
            .enumerate()
            .map(|(index, ((exchange, outcome), response_shape))| {
                FixtureExchange::from_wire(exchange, outcome, response_shape, index == 0)
            })
            .collect::<Result<Vec<_>, String>>()?;
        let persisted_outcomes = exchanges
            .iter()
            .map(persisted_fixture_replay_outcome)
            .collect::<Vec<_>>();
        let payload = ProbeFixturePayload {
            run_id: run_id.to_owned(),
            stable_key: probe.stable_key(),
            provider_id: probe.provider_id.clone(),
            model: probe.model.clone(),
            protocol: probe.protocol.clone(),
            response_mode: probe.response_mode.clone(),
            capability: probe.capability.clone(),
            synthetic_marker: probe.synthetic_marker.clone(),
            synthetic_only: true,
            exchanges,
            expected_response: probe.response.clone(),
            expected_actual_text_evidence: probe.actual_text_evidence.clone(),
            expected_error: probe.normalized_error.clone(),
            expected_cancellation: probe.cancellation.clone(),
            replay: None,
        };
        let mut fixture = ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256: String::new(),
            payload,
        };
        let replay = fixture_replay_evidence(&fixture, &persisted_outcomes);
        fixture.payload.replay = Some(replay.clone());
        probe.fixture_replay = Some(replay);
        fixture.content_sha256 = fixture_payload_sha256(&fixture.payload)?;
        let relative = fixture_relative_path(&fixture.payload, &fixture.content_sha256)?;
        let text = serde_json::to_string_pretty(&fixture)
            .map_err(|error| format!("无法序列化探测 Fixture：{error}"))?;
        probe.fixture_paths = vec![relative];
        let text = format!("{text}\n");
        ensure_safe_artifact(&text, providers)?;
        validate_synthetic_fixture(&text)?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_FIXTURE_FILE_BYTES {
            return Err(format!(
                "探测 Fixture 超过 {MAX_FIXTURE_FILE_BYTES} 字节单文件上限"
            ));
        }
        Ok(Some((probe.fixture_paths[0].clone(), text)))
    }

    /// 从完成态 Manifest 构造固定且不包含 Resume、锁和失败关闭标记的产物路径集合。
    fn completion_artifact_paths(&self, manifest: &ResumeManifest) -> Result<Vec<String>, String> {
        let mut paths = BTreeSet::from([
            "sanitized-logs/progress.jsonl".to_owned(),
            "result.json".to_owned(),
            "compatibility-matrix.md".to_owned(),
            "summary.md".to_owned(),
            "redaction-report.json".to_owned(),
        ]);
        if manifest.retry_selection.is_some() {
            paths.insert("retry-selection.json".to_owned());
        }
        if manifest.run.recovery_lineage.is_some() {
            paths.insert("recovery-lineage.json".to_owned());
        }
        let mut referenced_fixtures = BTreeSet::new();
        for record in manifest.records.values() {
            for relative in &record.fixture_paths {
                validate_fixture_relative_path(relative)?;
                if !referenced_fixtures.insert(relative.clone()) {
                    return Err(format!("完成态产物封印包含重复 Fixture 引用：{relative}"));
                }
            }
        }
        let disk_fixtures = collect_resume_fixture_files(&self.run_dir)?;
        if disk_fixtures != referenced_fixtures {
            return Err("完成态产物封印要求磁盘全部 Fixture 与已提交记录引用严格一致".to_owned());
        }
        paths.extend(disk_fixtures);
        self.validate_completed_artifact_layout(manifest, &paths)?;
        Ok(paths.into_iter().collect())
    }

    /// 枚举完成目录的三个固定层级，并拒绝封印集合之外的临时文件、未知文件或目录。
    fn validate_completed_artifact_layout(
        &self,
        manifest: &ResumeManifest,
        sealed_paths: &BTreeSet<String>,
    ) -> Result<(), String> {
        let allowed_root_files = sealed_paths
            .iter()
            .filter(|path| !path.contains('/'))
            .cloned()
            .chain([
                "resume.json".to_owned(),
                ".keencode-live-test.lock".to_owned(),
            ])
            .collect::<BTreeSet<_>>();
        for entry in fs::read_dir(&self.run_dir)
            .map_err(|error| format!("无法枚举完成态运行目录：{error}"))?
        {
            let entry = entry.map_err(|error| format!("无法读取完成态运行目录项：{error}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "完成态运行目录项名称必须是有效 Unicode".to_owned())?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| format!("无法读取完成态运行目录项元数据：{error}"))?;
            if is_link_or_reparse(&metadata) {
                return Err(format!("完成态运行目录包含链接或重解析点未知产物：{name}"));
            }
            if metadata.is_dir() && matches!(name.as_str(), "fixtures" | "sanitized-logs") {
                continue;
            }
            if metadata.is_file() && allowed_root_files.contains(&name) {
                continue;
            }
            let kind = if name.ends_with(".tmp") || name.starts_with('.') {
                "临时产物"
            } else {
                "未知产物"
            };
            return Err(format!("完成态运行目录包含未授权{kind}：{name}"));
        }

        let log_dir = validated_run_descendant(
            &self.run_dir,
            Path::new("sanitized-logs"),
            RunPathKind::Directory,
            "完成态脱敏日志目录",
        )?;
        for entry in
            fs::read_dir(log_dir).map_err(|error| format!("无法枚举完成态脱敏日志目录：{error}"))?
        {
            let entry = entry.map_err(|error| format!("无法读取完成态脱敏日志目录项：{error}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "完成态脱敏日志目录项名称必须是有效 Unicode".to_owned())?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| format!("无法读取完成态脱敏日志目录项元数据：{error}"))?;
            if name != "progress.jsonl" || is_link_or_reparse(&metadata) || !metadata.is_file() {
                let kind = if name.ends_with(".tmp") || name.starts_with('.') {
                    "临时产物"
                } else {
                    "未知产物"
                };
                return Err(format!("完成态脱敏日志目录包含未授权{kind}：{name}"));
            }
        }
        if manifest.journal_sequence > 0 && self.validated_checkpoint_path()?.is_none() {
            return Err("完成态 Journal 非空但固定日志文件不存在".to_owned());
        }
        Ok(())
    }

    /// 增量计算一个完成态权威产物摘要；零记录运行把缺失 Journal 规范为空字节。
    fn completion_artifact_sha256(
        &self,
        manifest: &ResumeManifest,
        relative: &str,
    ) -> Result<String, String> {
        if relative == "sanitized-logs/progress.jsonl" {
            return match self.validated_checkpoint_path()? {
                Some(_) => self.sha256_bounded_run_file_snapshot(
                    Path::new("sanitized-logs/progress.jsonl"),
                    MAX_PROGRESS_JOURNAL_BYTES,
                    None,
                    None,
                    "完成态提交日志",
                ),
                None if manifest.journal_sequence == 0 => Ok(sha256_digest(&[])),
                None => Err("完成态恢复清单引用了不存在的提交日志".to_owned()),
            };
        }
        let max_bytes = if relative.starts_with("fixtures/") {
            MAX_FIXTURE_FILE_BYTES
        } else {
            MAX_ARTIFACT_FILE_BYTES
        };
        self.sha256_bounded_run_file_snapshot(
            Path::new(relative),
            max_bytes,
            None,
            None,
            &format!("完成态事实产物 {relative}"),
        )
    }

    /// 重新认证完整 Journal，并要求磁盘字节等于由认证记录重建的唯一 JSONL 编码。
    fn trusted_journal_artifact_sha256(
        &self,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<String, String> {
        let journal = self.load_progress_journal(
            manifest,
            providers,
            JOURNAL_SCHEMA_VERSION,
            JournalTailPolicy::ReadOnlyReject,
        )?;
        if journal.len() != manifest.records.len()
            || manifest.journal_sequence
                != u64::try_from(journal.len())
                    .map_err(|_| "完成态 Journal 记录数不能表示为 u64".to_owned())?
        {
            return Err("完成态 Journal 与恢复清单记录数量不一致".to_owned());
        }
        let mut reconciled = manifest.clone();
        reconcile_progress_journal(&mut reconciled, &journal)?;
        authenticated_journal_tail(&reconciled, &journal)?;

        let mut hasher = Sha256::new();
        for entry in &journal {
            let previous_mac = entry
                .previous_mac
                .as_deref()
                .ok_or_else(|| "当前完成态 Journal 缺少前序 MAC".to_owned())?;
            let record_mac = entry
                .record_mac
                .as_deref()
                .ok_or_else(|| "当前完成态 Journal 缺少记录 MAC".to_owned())?;
            let canonical = serde_json::to_vec(&ProbeJournalEntry {
                schema_version: JOURNAL_SCHEMA_VERSION,
                sequence: entry.sequence,
                previous_mac,
                record_mac,
                record: &entry.record,
            })
            .map_err(|error| format!("无法重建规范完成态 Journal：{error}"))?;
            hasher.update(&canonical);
            hasher.update(b"\n");
        }
        let trusted = format!("sha256:{}", hex_encode(&hasher.finalize()));
        let actual = self.completion_artifact_sha256(manifest, "sanitized-logs/progress.jsonl")?;
        if actual != trusted {
            return Err("完成态 Journal 原始字节不是认证记录的唯一规范编码".to_owned());
        }
        Ok(trusted)
    }

    /// 重新校验一个 Fixture 与其认证记录的全部绑定，并返回唯一规范字节摘要。
    fn trusted_fixture_artifact_sha256(
        &self,
        manifest: &ResumeManifest,
        relative: &str,
        providers: &[&ProviderEntry],
    ) -> Result<String, String> {
        let mut matches = manifest
            .records
            .iter()
            .filter(|(_, record)| record.fixture_paths.iter().any(|path| path == relative));
        let (stable_key, record) = matches
            .next()
            .ok_or_else(|| format!("完成态 Fixture 没有认证记录引用：{relative}"))?;
        if matches.next().is_some() {
            return Err(format!("完成态 Fixture 被多条认证记录引用：{relative}"));
        }
        let validation_error =
            format!("恢复记录 {stable_key} 的取消提前完成状态没有通过真实响应重放");
        let (expected_marker, verify_completed_cancellation_gap) =
            validate_reusable_record_with_current_gap(manifest, stable_key, record, providers)?;
        let bytes = self.read_bounded_run_file(
            Path::new(relative),
            MAX_FIXTURE_FILE_BYTES,
            &format!("完成态 Fixture {relative}"),
        )?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| format!("完成态 Fixture 必须是 UTF-8：{relative}"))?;
        ensure_safe_artifact(text, providers)?;
        let fixture = parse_synthetic_fixture(text, &expected_marker)?;
        validate_fixture_record_binding(manifest, record, relative, &fixture)?;
        if verify_completed_cancellation_gap
            && !current_completed_cancellation_replay_gap_fixture(record, &fixture)
        {
            return Err(validation_error);
        }
        verify_disk_fixture(record, &fixture)?;
        let canonical = serialize_json_artifact(relative, &fixture)?;
        if bytes != canonical.as_bytes() {
            return Err(format!("完成态 Fixture 不是唯一规范 JSON 编码：{relative}"));
        }
        Ok(sha256_digest(canonical.as_bytes()))
    }

    /// 比较内存生成的受信文本与当前磁盘文件，并只返回预期字节摘要。
    fn trusted_generated_artifact_sha256(
        &self,
        manifest: &ResumeManifest,
        relative: &str,
        expected: &str,
    ) -> Result<String, String> {
        let trusted = sha256_digest(expected.as_bytes());
        let actual = self.completion_artifact_sha256(manifest, relative)?;
        if actual != trusted {
            return Err(format!("完成态产物与内存生成的预期字节不一致：{relative}"));
        }
        Ok(trusted)
    }

    /// 仅从内存生成文本、认证 Journal 和内容绑定 Fixture 构造待签名完成态封印。
    fn trusted_completion_artifact_seal(
        &self,
        manifest: &ResumeManifest,
        generated: &BTreeMap<String, String>,
        providers: &[&ProviderEntry],
    ) -> Result<CompletionArtifactSeal, String> {
        if !manifest.finished || manifest.run.finished_at.is_none() {
            return Err("只有带结束时间的完成运行才能生成受信事实产物封印".to_owned());
        }
        let expected_generated = BTreeSet::from([
            "result.json".to_owned(),
            "compatibility-matrix.md".to_owned(),
            "summary.md".to_owned(),
            "redaction-report.json".to_owned(),
        ]);
        if generated.keys().cloned().collect::<BTreeSet<_>>() != expected_generated {
            return Err("完成流程生成的内存事实产物集合不完整或包含未知路径".to_owned());
        }
        let journal_tail_mac = manifest
            .journal_tail_mac
            .as_deref()
            .filter(|value| valid_hmac_sha256_proof(value))
            .ok_or_else(|| "受信完成态封印缺少有效 Journal 链尾 MAC".to_owned())?;
        let mut artifacts = Vec::new();
        for path in self.completion_artifact_paths(manifest)? {
            let sha256 = if let Some(text) = generated.get(&path) {
                self.trusted_generated_artifact_sha256(manifest, &path, text)?
            } else if path == "sanitized-logs/progress.jsonl" {
                self.trusted_journal_artifact_sha256(manifest, providers)?
            } else if path.starts_with("fixtures/") {
                self.trusted_fixture_artifact_sha256(manifest, &path, providers)?
            } else if path == "retry-selection.json" {
                let selection = manifest
                    .retry_selection
                    .as_ref()
                    .ok_or_else(|| "完成态选择 Sidecar 缺少内存来源".to_owned())?;
                let text = serialize_json_artifact(&path, selection)?;
                self.trusted_generated_artifact_sha256(manifest, &path, &text)?
            } else if path == "recovery-lineage.json" {
                let lineage = manifest
                    .run
                    .recovery_lineage
                    .as_ref()
                    .ok_or_else(|| "完成态恢复 Lineage 缺少内存来源".to_owned())?;
                let text = serialize_json_artifact(&path, lineage)?;
                self.trusted_generated_artifact_sha256(manifest, &path, &text)?
            } else {
                return Err(format!("完成态封印出现没有受信来源的产物：{path}"));
            };
            artifacts.push(CompletionArtifactDigest { path, sha256 });
        }
        Ok(CompletionArtifactSeal {
            schema_version: FACT_AUTHENTICATION_SCHEMA_VERSION.to_owned(),
            journal_sequence: manifest.journal_sequence,
            journal_tail_mac: journal_tail_mac.to_owned(),
            artifacts,
        })
    }

    /// 对全部固定事实产物原始字节计算无循环完成态封印。
    fn calculate_completion_artifact_seal(
        &self,
        manifest: &ResumeManifest,
    ) -> Result<CompletionArtifactSeal, String> {
        if !manifest.finished || manifest.run.finished_at.is_none() {
            return Err("只有带结束时间的完成运行才能生成事实产物封印".to_owned());
        }
        let journal_tail_mac = manifest
            .journal_tail_mac
            .as_deref()
            .filter(|value| valid_hmac_sha256_proof(value))
            .ok_or_else(|| "完成态事实产物封印缺少有效 Journal 链尾 MAC".to_owned())?;
        let artifacts = self
            .completion_artifact_paths(manifest)?
            .into_iter()
            .map(|path| {
                let sha256 = self.completion_artifact_sha256(manifest, &path)?;
                Ok(CompletionArtifactDigest { path, sha256 })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(CompletionArtifactSeal {
            schema_version: FACT_AUTHENTICATION_SCHEMA_VERSION.to_owned(),
            journal_sequence: manifest.journal_sequence,
            journal_tail_mac: journal_tail_mac.to_owned(),
            artifacts,
        })
    }

    /// 在完成来源任何事实被复用前重算并核对完成态产物封印。
    fn verify_completion_artifact_seal(&self, manifest: &ResumeManifest) -> Result<(), String> {
        if manifest.identity.schema_version == RETRY_SOURCE_RESUME_SCHEMA_VERSION
            || !manifest.finished
        {
            return Ok(());
        }
        let stored = manifest
            .completion_artifact_seal
            .as_ref()
            .ok_or_else(|| "当前完成来源缺少事实产物封印".to_owned())?;
        if stored.schema_version != FACT_AUTHENTICATION_SCHEMA_VERSION
            || stored.journal_sequence != manifest.journal_sequence
            || stored.journal_tail_mac.as_str()
                != manifest.journal_tail_mac.as_deref().unwrap_or_default()
            || !valid_hmac_sha256_proof(&stored.journal_tail_mac)
            || stored
                .artifacts
                .iter()
                .any(|artifact| !valid_sha256_digest(&artifact.sha256))
        {
            return Err("完成态事实产物封印的版本、Journal 绑定或摘要格式无效".to_owned());
        }
        let expected = self.calculate_completion_artifact_seal(manifest)?;
        if stored != &expected {
            return Err("完成态权威事实产物未通过 Resume 封印校验".to_owned());
        }
        Ok(())
    }

    /// 写入最终 JSON、Markdown 与脱敏报告，并返回本次内存生成的精确文本。
    fn write_run_report_artifacts(
        &self,
        report: &RunReport,
        providers: &[&ProviderEntry],
    ) -> Result<BTreeMap<String, String>, String> {
        let result = serialize_json_artifact("result.json", report)?;
        let matrix = compatibility_matrix(report);
        let summary = summary_markdown(report);
        self.write_text("result.json", &result, providers)?;
        self.write_text("compatibility-matrix.md", &matrix, providers)?;
        self.write_text("summary.md", &summary, providers)?;
        let redaction = self.scan_artifacts(providers)?;
        let redaction_text = serialize_json_artifact("redaction-report.json", &redaction)?;
        self.write_text("redaction-report.json", &redaction_text, providers)?;
        if !redaction.passed {
            return Err("脱敏验收失败：产物包含禁止写盘的敏感模式".to_owned());
        }
        Ok(BTreeMap::from([
            ("result.json".to_owned(), result),
            ("compatibility-matrix.md".to_owned(), matrix),
            ("summary.md".to_owned(), summary),
            ("redaction-report.json".to_owned(), redaction_text),
        ]))
    }

    /// 原子写入未完成运行的最终样式报告；该入口不会生成完成态认证封印。
    pub(crate) fn finalize(
        &self,
        report: &RunReport,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        let current_executable_sha256 = current_executable_sha256()?;
        report.validate_recovery_lineage(&current_executable_sha256)?;
        self.write_run_report_artifacts(report, providers)?;
        Ok(())
    }

    /// 从内存报告与已认证磁盘事实生成封印、签署最终 Resume，并在返回前再次回读验收。
    pub(crate) fn finalize_completed(
        &self,
        report: &RunReport,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        self.finalize_completed_with_hooks(report, manifest, providers, || Ok(()), || Ok(()))
    }

    /// 在两个完成态持久化边界执行确定性钩子，供竞态与篡改失败关闭回归测试使用。
    fn finalize_completed_with_hooks<A, R>(
        &self,
        report: &RunReport,
        manifest: &ResumeManifest,
        providers: &[&ProviderEntry],
        after_artifacts_written: A,
        after_resume_written: R,
    ) -> Result<(), String>
    where
        A: FnOnce() -> Result<(), String>,
        R: FnOnce() -> Result<(), String>,
    {
        let current_executable_sha256 = current_executable_sha256()?;
        report.validate_recovery_lineage(&current_executable_sha256)?;
        if report.run.finished_at.is_none() {
            return Err("完成态报告必须包含结束时间".to_owned());
        }
        let mut completed = manifest.persisted_snapshot(&self.journal_tail_mac.borrow())?;
        completed.run = report.run.clone();
        completed.finished = true;
        completed.completion_artifact_seal = None;

        let generated = self.write_run_report_artifacts(report, providers)?;
        after_artifacts_written()?;
        let result = generated
            .get("result.json")
            .expect("完成流程始终生成 result.json");
        validate_stored_run_report(
            result.as_bytes(),
            &completed,
            providers,
            &[RUN_REPORT_SCHEMA_VERSION],
        )?;
        completed.completion_artifact_seal =
            Some(self.trusted_completion_artifact_seal(&completed, &generated, providers)?);
        completed.state_proofs = completed.calculated_state_proofs(providers)?;
        let resume_text = serialize_json_artifact("resume.json", &completed)?;
        self.write_text("resume.json", &resume_text, providers)?;
        after_resume_written()?;

        completed.validate_persisted_state_proofs(providers)?;
        self.verify_completion_artifact_seal(&completed)?;
        self.read_and_verify_completed_redaction_report(providers)?;
        let resume_path = validated_run_descendant(
            &self.run_dir,
            Path::new("resume.json"),
            RunPathKind::File,
            "最终完成恢复清单",
        )?;
        let actual_resume_sha256 = sha256_digest_regular_file(
            &resume_path,
            MAX_RESUME_MANIFEST_BYTES,
            Some(
                u64::try_from(resume_text.len())
                    .map_err(|_| "最终恢复清单长度不能表示为 u64".to_owned())?,
            ),
            None,
            "最终完成恢复清单",
        )?;
        if actual_resume_sha256 != sha256_digest(resume_text.as_bytes()) {
            return Err("最终完成恢复清单回读摘要与已签署内存字节不一致".to_owned());
        }
        Ok(())
    }

    /// 写入离线合并 JSON、两份有效矩阵 Markdown，并执行同一套全目录脱敏扫描。
    fn finalize_consolidated(
        &self,
        report: &ConsolidatedRunReport,
        markdown_report: &RunReport,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        self.write_json("consolidated-result.json", report, providers)?;
        self.write_text(
            "compatibility-matrix.md",
            &compatibility_matrix(markdown_report),
            providers,
        )?;
        self.write_text("summary.md", &summary_markdown(markdown_report), providers)?;
        let redaction = self.scan_artifacts(providers)?;
        self.write_json("redaction-report.json", &redaction, providers)?;
        if !redaction.passed {
            return Err("离线合并脱敏验收失败：产物包含禁止写盘的敏感模式".to_owned());
        }
        Ok(())
    }

    /// 安全序列化并原子替换一个 JSON 产物。
    fn write_json<T: Serialize>(
        &self,
        name: &str,
        value: &T,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        let text = serialize_json_artifact(name, value)?;
        self.write_text(name, &text, providers)
    }

    /// 在写盘前检查全部禁止模式并原子替换一个文本产物。
    fn write_text(
        &self,
        name: &str,
        text: &str,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        ensure_safe_artifact(text, providers)?;
        let max_bytes = if name == "resume.json" {
            MAX_RESUME_MANIFEST_BYTES
        } else {
            MAX_ARTIFACT_FILE_BYTES
        };
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(format!("{name} 超过 {max_bytes} 字节写盘上限"));
        }
        let destination = self.run_dir.join(name);
        replace_file_contents(&destination, text, name)
    }

    /// 在运行目录内原子写入一个已经校验为相对路径的 UTF-8 产物。
    #[cfg(test)]
    fn write_relative_text(
        &self,
        relative: &str,
        text: &str,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        ensure_safe_artifact(text, providers)?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_FIXTURE_FILE_BYTES {
            return Err(format!(
                "不可变 Fixture 超过 {} 字节单文件上限",
                MAX_FIXTURE_FILE_BYTES
            ));
        }
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("产物路径必须位于当前运行目录内".to_owned());
        }
        if relative.starts_with("fixtures/") {
            validate_synthetic_fixture(text)?;
        }
        let destination = self.run_dir.join(relative_path);
        let parent = destination
            .parent()
            .ok_or_else(|| "产物路径缺少父目录".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| format!("无法创建产物目录：{error}"))?;
        replace_file_contents(&destination, text, relative)
    }

    /// 在运行目录内创建内容寻址的不可变 UTF-8 产物；既有路径只接受逐字节相同内容。
    fn write_immutable_relative_text(
        &self,
        relative: &str,
        text: &str,
        providers: &[&ProviderEntry],
    ) -> Result<(), String> {
        ensure_safe_artifact(text, providers)?;
        validate_fixture_relative_path(relative)?;
        let relative_path = Path::new(relative);
        validate_synthetic_fixture(text)?;
        let destination = self.run_dir.join(relative_path);
        let parent = destination
            .parent()
            .ok_or_else(|| "Fixture 路径缺少父目录".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 Fixture 目录：{error}"))?;
        validated_run_descendant(
            &self.run_dir,
            Path::new("fixtures"),
            RunPathKind::Directory,
            "Fixture 写入目录",
        )?;
        if destination.exists() {
            return verify_existing_immutable_fixture(&self.run_dir, relative_path, text);
        }
        let file_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "不可变 Fixture 文件名必须是有效 Unicode".to_owned())?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("无法生成 Fixture 临时文件标识：{error}"))?
            .as_nanos();
        let mut temporary = None;
        let mut temporary_file = None;
        for attempt in 0..16_u8 {
            let candidate = parent.join(format!(
                "{FIXTURE_STAGING_PREFIX}{file_name}.{}.{}.{attempt}.tmp",
                std::process::id(),
                nonce
            ));
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&candidate)
            {
                Ok(file) => {
                    temporary = Some(candidate);
                    temporary_file = Some(file);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("无法创建不可变 Fixture 临时文件：{error}")),
            }
        }
        let temporary = temporary.ok_or_else(|| "无法分配不可变 Fixture 临时文件".to_owned())?;
        let mut temporary_file = temporary_file.expect("Fixture 临时路径与句柄总是同时创建");
        if let Err(error) = temporary_file
            .write_all(text.as_bytes())
            .and_then(|_| temporary_file.sync_all())
        {
            drop(temporary_file);
            let _ = fs::remove_file(&temporary);
            return Err(format!("无法写入并同步不可变 Fixture 临时文件：{error}"));
        }
        drop(temporary_file);
        match fs::hard_link(&temporary, &destination) {
            Ok(()) => {
                fs::remove_file(&temporary)
                    .map_err(|error| format!("无法删除已提交 Fixture 临时文件：{error}"))?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary)
                    .map_err(|cleanup| format!("无法删除冲突的 Fixture 临时文件：{cleanup}"))?;
                verify_existing_immutable_fixture(&self.run_dir, relative_path, text)
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(format!("无法原子提交不可变 Fixture：{error}"))
            }
        }
    }

    /// 扫描本次运行的全部 UTF-8 产物并返回真实命中计数。
    fn scan_artifacts(&self, providers: &[&ProviderEntry]) -> Result<RedactionScanReport, String> {
        let mut paths = collect_artifact_paths(&self.run_dir)?;
        paths.sort_by(|left, right| left.relative.cmp(&right.relative));
        let mut report = RedactionScanReport {
            schema_version: REDACTION_REPORT_SCHEMA_VERSION,
            passed: true,
            exact_credential_matches: 0,
            secret_token_matches: 0,
            masked_credential_suffix_matches: 0,
            authentication_header_matches: 0,
            cookie_matches: 0,
            absolute_path_matches: 0,
            dangerous_display_character_matches: 0,
            non_synthetic_prompt_matches: 0,
            non_utf8_artifacts: 0,
            scanned_artifacts: Vec::with_capacity(paths.len()),
        };
        let mut actual_total_bytes = 0_u64;
        for artifact in paths {
            let ArtifactFile {
                relative,
                byte_len,
                identity,
            } = artifact;
            let bytes = self.read_bounded_run_file_snapshot(
                Path::new(&relative),
                MAX_ARTIFACT_FILE_BYTES,
                Some(byte_len),
                Some(&identity),
                &format!("待脱敏扫描产物 {relative}"),
            )?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != byte_len {
                return Err(format!(
                    "待脱敏扫描产物 {relative} 在枚举与读取之间长度发生变化"
                ));
            }
            actual_total_bytes = actual_total_bytes
                .checked_add(
                    u64::try_from(bytes.len())
                        .map_err(|_| "待脱敏扫描产物长度不能表示为 u64".to_owned())?,
                )
                .ok_or_else(|| "待脱敏扫描产物实际总字节数溢出".to_owned())?;
            if actual_total_bytes > MAX_ARTIFACT_TOTAL_BYTES {
                return Err(format!(
                    "待脱敏扫描产物实际总字节数超过 {} 字节安全上限",
                    MAX_ARTIFACT_TOTAL_BYTES
                ));
            }
            report.scanned_artifacts.push(relative.clone());
            let Ok(text) = std::str::from_utf8(&bytes) else {
                report.non_utf8_artifacts += 1;
                continue;
            };
            if relative.starts_with("fixtures/")
                && relative.ends_with(".json")
                && validate_synthetic_fixture(text).is_err()
            {
                report.non_synthetic_prompt_matches += 1;
            }
            report.exact_credential_matches += providers
                .iter()
                .map(|provider| provider.output_credential_match_count(text))
                .sum::<usize>();
            report.secret_token_matches += count_secret_tokens(text);
            report.masked_credential_suffix_matches += count_masked_credential_suffixes(text);
            report.authentication_header_matches +=
                count_sensitive_assignments(text, AUTHENTICATION_FIELD_NAMES);
            report.cookie_matches += count_sensitive_assignments(text, COOKIE_FIELD_NAMES);
            report.absolute_path_matches += count_artifact_absolute_paths(text);
            report.dangerous_display_character_matches +=
                count_artifact_dangerous_display_characters(text);
        }
        report.passed = report.exact_credential_matches == 0
            && report.secret_token_matches == 0
            && report.masked_credential_suffix_matches == 0
            && report.authentication_header_matches == 0
            && report.cookie_matches == 0
            && report.absolute_path_matches == 0
            && report.dangerous_display_character_matches == 0
            && report.non_synthetic_prompt_matches == 0
            && report.non_utf8_artifacts == 0;
        Ok(report)
    }
}

/// 从已经严格验证的提交日志中按唯一固定策略选择精确补测 tuple。
fn select_retry_cases(
    journal: &[OwnedProbeJournalEntry],
    provider_id: &str,
    through_sequence: u64,
) -> Vec<RetryCase> {
    journal
        .iter()
        .filter(|entry| entry.sequence <= through_sequence)
        .filter_map(|entry| {
            let record = &entry.record;
            let selected_error = record.normalized_error.as_ref().is_some_and(|error| {
                error.retryable || matches!(error.kind.as_str(), "rate_limit" | "server_error")
            });
            if record.provider_id != provider_id
                || record.status != "failed"
                || !selected_error
                || !is_known_probe_capability(&record.capability)
                || record.capability == "stream_interruption"
            {
                return None;
            }
            Some(RetryCase {
                source_sequence: entry.sequence,
                source_stable_key: record.stable_key.clone(),
                tuple_key: retry_tuple_key(
                    &record.provider_id,
                    &record.model,
                    &record.protocol,
                    &record.response_mode,
                    &record.capability,
                ),
                provider_id: record.provider_id.clone(),
                model: record.model.clone(),
                protocol: record.protocol.clone(),
                response_mode: record.response_mode.clone(),
                capability: record.capability.clone(),
            })
        })
        .collect()
}

/// 校验恢复清单中的 Provider 候选集合仍可由当前选择配置无歧义解释。
fn validate_resume_candidate_sets(
    manifest: &ResumeManifest,
    providers: &[&ProviderEntry],
) -> Result<(), String> {
    for (provider_id, candidates) in &manifest.candidate_sets {
        let provider = resolve_resume_provider(provider_id, providers)?;
        let mut normalized = candidates.clone();
        normalized.sort();
        normalized.dedup();
        if &normalized != candidates {
            return Err(format!(
                "恢复清单 Provider {provider_id} 的冻结候选模型必须严格排序且不能重复"
            ));
        }
        for model in candidates {
            if model.trim().is_empty() || provider.redact_text(model) != *model {
                return Err(format!(
                    "恢复清单 Provider {provider_id} 包含无法还原原始身份的候选模型"
                ));
            }
        }
    }
    Ok(())
}

/// 从当前选择配置中按脱敏 Provider 标识解析唯一的原始 Provider。
fn resolve_resume_provider<'a>(
    provider_id: &str,
    providers: &[&'a ProviderEntry],
) -> Result<&'a ProviderEntry, String> {
    let mut matches = providers
        .iter()
        .copied()
        .filter(|provider| provider.redact_text(&provider.id) == provider_id);
    let provider = matches
        .next()
        .ok_or_else(|| format!("恢复记录引用了未选择的 Provider：{provider_id}"))?;
    if matches.next().is_some() {
        return Err(format!(
            "恢复记录 Provider 脱敏标识存在歧义，无法重算原始身份：{provider_id}"
        ));
    }
    Ok(provider)
}

/// 返回诊断缺失模型探测与运行期实现完全一致的确定性模型标识。
fn resume_missing_model_id(
    provider: &ProviderEntry,
    protocol: &str,
    response_mode: &str,
    run_id: &str,
) -> String {
    let digest = domain_separated_hex(
        b"keencode-provider-missing-model-v1",
        &[
            run_id.as_bytes(),
            provider.id.as_bytes(),
            protocol.as_bytes(),
            response_mode.as_bytes(),
        ],
    );
    format!("keencode-missing-{}", &digest[..20])
}

/// 校验一条终态记录的原始身份、能力集合、标记和基础状态不变量。
fn validate_reusable_record(
    manifest: &ResumeManifest,
    key: &str,
    record: &ProbeRecord,
    providers: &[&ProviderEntry],
) -> Result<String, String> {
    if key != record.stable_key {
        return Err(format!("恢复记录 {key} 的 Map 键与记录稳定键不一致"));
    }
    if !manifest.identity.protocols.contains(&record.protocol)
        || !matches!(
            record.protocol.as_str(),
            "anthropic_messages" | "openai_chat_completions" | "openai_responses"
        )
    {
        return Err(format!("恢复记录 {key} 包含未知协议：{}", record.protocol));
    }
    if !manifest
        .identity
        .response_modes
        .contains(&record.response_mode)
        || !matches!(record.response_mode.as_str(), "buffered" | "streaming")
    {
        return Err(format!(
            "恢复记录 {key} 包含未知响应模式：{}",
            record.response_mode
        ));
    }
    let diagnostic = matches!(
        record.capability.as_str(),
        "diagnostic_invalid_authentication" | "diagnostic_missing_model"
    );
    if diagnostic {
        if !(manifest.identity.full_matrix || manifest.identity.diagnostics_only) {
            return Err(format!(
                "恢复记录 {key} 包含本轮未启用的诊断能力：{}",
                record.capability
            ));
        }
    } else if !is_known_probe_capability(&record.capability)
        || (record.capability != "text"
            && !manifest.identity.capabilities.contains(&record.capability))
    {
        return Err(format!(
            "恢复记录 {key} 包含本轮未启用的能力：{}",
            record.capability
        ));
    }

    let provider = resolve_resume_provider(&record.provider_id, providers)?;
    let candidates = manifest
        .candidate_sets
        .get(&record.provider_id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let evidence_run_id = record
        .recovered_from
        .as_ref()
        .map_or(manifest.run.run_id.as_str(), |origin| {
            origin.source_run_id.as_str()
        });
    let raw_model = match record.capability.as_str() {
        "diagnostic_invalid_authentication" => "keencode-authentication-probe".to_owned(),
        "diagnostic_missing_model" => resume_missing_model_id(
            provider,
            &record.protocol,
            &record.response_mode,
            evidence_run_id,
        ),
        _ => {
            if !candidates.contains(&record.model) {
                return Err(format!(
                    "恢复记录 {key} 的模型不属于 Provider {} 的冻结候选集合",
                    record.provider_id
                ));
            }
            record.model.clone()
        }
    };
    if provider.redact_text(&raw_model) != record.model {
        return Err(format!(
            "恢复记录 {key} 的模型脱敏身份无法还原到当前原始模型"
        ));
    }
    let expected_key = probe_stable_key(
        evidence_run_id,
        &provider.id,
        &raw_model,
        &record.protocol,
        &record.response_mode,
        &record.capability,
    );
    if record.stable_key != expected_key {
        return Err(format!("恢复记录 {key} 的稳定键与原始身份 tuple 不一致"));
    }
    let expected_marker = marker_from_probe_stable_key(&expected_key, diagnostic);
    if record.attempts == 0 {
        if record.synthetic_marker.is_some() {
            return Err(format!("恢复记录 {key} 未发送请求却保存了合成标记"));
        }
    } else if record.synthetic_marker.as_deref() != Some(expected_marker.as_str()) {
        return Err(format!("恢复记录 {key} 的合成标记未由稳定键精确派生"));
    }
    if record
        .expected_text
        .as_deref()
        .is_some_and(|value| value != expected_marker)
    {
        return Err(format!("恢复记录 {key} 的预期文本与派生标记不一致"));
    }
    validate_probe_record_invariants(manifest, key, record)?;
    Ok(expected_marker)
}

/// 在当前契约下接受唯一严格定义的取消重放缺口，并标记其 Fixture 需要额外绑定校验。
fn validate_reusable_record_with_current_gap(
    manifest: &ResumeManifest,
    key: &str,
    record: &ProbeRecord,
    providers: &[&ProviderEntry],
) -> Result<(String, bool), String> {
    match validate_reusable_record(manifest, key, record, providers) {
        Ok(marker) => Ok((marker, false)),
        Err(error)
            if manifest.identity.schema_version == RESUME_SCHEMA_VERSION
                && manifest.identity.harness_contract_id == HARNESS_CONTRACT_ID
                && current_completed_cancellation_replay_gap_record(key, record, &error) =>
        {
            Ok((
                marker_from_probe_stable_key(&record.stable_key, false),
                true,
            ))
        }
        Err(error) => Err(error),
    }
}

/// 把导入记录映射到新运行将要查询的稳定键，同时保留记录自身的旧证据身份。
fn reusable_lookup_key(
    manifest: &ResumeManifest,
    record: &ProbeRecord,
    providers: &[&ProviderEntry],
) -> Result<String, String> {
    if record.recovered_from.is_none() {
        return Ok(record.stable_key.clone());
    }
    let provider = resolve_resume_provider(&record.provider_id, providers)?;
    let raw_model = match record.capability.as_str() {
        "diagnostic_invalid_authentication" => "keencode-authentication-probe".to_owned(),
        "diagnostic_missing_model" => resume_missing_model_id(
            provider,
            &record.protocol,
            &record.response_mode,
            &manifest.run.run_id,
        ),
        _ => record.model.clone(),
    };
    Ok(probe_stable_key(
        &manifest.run.run_id,
        &provider.id,
        &raw_model,
        &record.protocol,
        &record.response_mode,
        &record.capability,
    ))
}

/// 判断普通能力是否属于当前 Harness 唯一支持的能力集合。
fn is_known_probe_capability(capability: &str) -> bool {
    matches!(
        capability,
        "text"
            | "tool_calling"
            | "parallel_tool_calling"
            | "tool_result_round_trip"
            | "tool_result_image_round_trip"
            | "multi_turn"
            | "reasoning"
            | "usage"
            | "prompt_caching"
            | "structured_output"
            | "output_limit"
            | "invalid_parameter"
            | "context_overflow"
            | "stream_interruption"
            | "cancellation"
    )
}

/// 识别 v14 已知的两种取消重试持久化缺口，避免把任意损坏记录降级为可重跑。
fn legacy_unreplayable_cancellation_record(
    key: &str,
    record: &ProbeRecord,
    validation_error: &str,
) -> bool {
    let expected_error = format!("恢复记录 {key} 的取消提前完成状态没有通过真实响应重放");
    let failed_assertions = record
        .assertions
        .iter()
        .filter(|assertion| !assertion.passed)
        .collect::<Vec<_>>();
    validation_error == expected_error
        && record.recovered_from.is_none()
        && record.status == "unverified"
        && record.capability == "cancellation"
        && record.attempts > 1
        && record.expected_text.is_none()
        && record.synthetic_marker.is_some()
        && record.actual_text_evidence.is_none()
        && record.response.is_none()
        && record.normalized_error.is_none()
        && record.skip_evidence.is_none()
        && record.fixture_paths.len() == 1
        && record.cancellation.as_ref().is_some_and(|cancellation| {
            cancellation.local_future_dropped
                && !cancellation.completed_before_cancel
                && !cancellation.remote_termination_proven
        })
        && record.fixture_replay.as_ref().is_some_and(|replay| {
            replay.status == "unavailable"
                && replay.exchange_count > 1
                && replay.replayed_exchanges < replay.exchange_count
                && matches!(
                    replay.reason.as_deref(),
                    Some(
                        LEGACY_UNREPLAYABLE_CANCELLATION_REASON
                            | LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON
                    )
                )
        })
        && failed_assertions.len() == 1
        && failed_assertions[0].name == "wire_adapter_replay"
}

/// 识别 v14 把最终传输错误写入记录、却未写入逐交换在线终态的取消失败缺口。
fn legacy_unreplayable_failed_cancellation_fixture(
    record: &ProbeRecord,
    fixture: &ProbeFixtureEnvelope,
    verification_error: &str,
) -> bool {
    let failed_assertions = record
        .assertions
        .iter()
        .filter(|assertion| !assertion.passed)
        .collect::<Vec<_>>();
    verification_error == "取消失败只有显式在线传输终态可以声明为响应不可从磁盘复核"
        && record.recovered_from.is_none()
        && record.status == "failed"
        && record.capability == "cancellation"
        && record.attempts > 0
        && record.expected_text.is_none()
        && record.synthetic_marker.is_some()
        && record.actual_text_evidence.is_none()
        && record.response.is_none()
        && record.skip_evidence.is_none()
        && record.fixture_paths.len() == 1
        && record.normalized_error.as_ref().is_some_and(|error| {
            error.kind == "transport" && error.retryable && error.http_status.is_none()
        })
        && record.cancellation.as_ref().is_some_and(|cancellation| {
            !cancellation.local_future_dropped
                && !cancellation.first_event_received
                && !cancellation.completed_before_cancel
                && !cancellation.remote_termination_proven
        })
        && record.fixture_replay.as_ref().is_some_and(|replay| {
            replay.status == "unavailable"
                && replay.exchange_count == record.attempts
                && replay.replayed_exchanges == 0
                && replay.reason.as_deref() == Some(LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON)
        })
        && failed_assertions.len() == 1
        && failed_assertions[0].name == "wire_adapter_replay"
        && fixture.payload.exchanges.len() == record.attempts
        && fixture.payload.exchanges.iter().all(|exchange| {
            exchange.response_shape.http_status.is_none()
                && exchange.observed_terminal_error.is_none()
                && matches!(
                    &exchange.expected_outcome,
                    FixtureExchangeOutcome::Unavailable { reason }
                        if reason == LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON
                )
        })
}

/// 识别当前契约下“本地取消已证明、前序交换无法离线重放”的唯一未验证形态。
fn unverified_cancellation_replay_gap(
    record: &ProbeRecord,
    replay: &FixtureReplayEvidence,
) -> bool {
    let failed_assertions = record
        .assertions
        .iter()
        .filter(|assertion| !assertion.passed)
        .collect::<Vec<_>>();
    record.status == "unverified"
        && record.capability == "cancellation"
        && record.attempts > 1
        && record.expected_text.is_none()
        && record.actual_text_evidence.is_none()
        && record.response.is_none()
        && record.normalized_error.is_none()
        && record.skip_evidence.is_none()
        && record.cancellation.as_ref().is_some_and(|cancellation| {
            cancellation.local_future_dropped
                && !cancellation.completed_before_cancel
                && !cancellation.remote_termination_proven
        })
        && replay.status == "unavailable"
        && replay.exchange_count == record.attempts
        && replay.replayed_exchanges < replay.exchange_count
        && replay
            .reason
            .as_deref()
            .is_some_and(is_known_unavailable_replay_reason)
        && failed_assertions.len() == 1
        && failed_assertions[0].name == "wire_adapter_replay"
}

/// 识别当前契约中“前序交换不可复核、最终响应先于取消完成”的严格记录形态。
fn current_completed_cancellation_replay_gap_record(
    key: &str,
    record: &ProbeRecord,
    validation_error: &str,
) -> bool {
    let expected_error = format!("恢复记录 {key} 的取消提前完成状态没有通过真实响应重放");
    let assertion = |name: &str, passed: bool| {
        record
            .assertions
            .iter()
            .any(|assertion| assertion.name == name && assertion.passed == passed)
    };
    validation_error == expected_error
        && record.status == "unverified"
        && record.capability == "cancellation"
        && record.attempts > 1
        && record.expected_text.is_none()
        && record.synthetic_marker.is_some()
        && record.response.is_some()
        && record.actual_text_evidence.is_some()
        && record.normalized_error.is_none()
        && record.skip_evidence.is_none()
        && record.fixture_paths.len() == 1
        && record.assertions.len() == 5
        && assertion("stream_event_received_before_cancel", true)
        && assertion("local_cancel_timer_won", false)
        && assertion("in_flight_future_dropped", false)
        && assertion("remote_termination_not_claimed", true)
        && assertion("wire_adapter_replay", false)
        && record.cancellation.as_ref().is_some_and(|cancellation| {
            !cancellation.local_future_dropped
                && cancellation.first_event_received
                && cancellation.completed_before_cancel
                && !cancellation.remote_termination_proven
                && cancellation.observed_latency_ms >= u128::from(cancellation.cancel_after_ms)
        })
        && record.fixture_replay.as_ref().is_some_and(|replay| {
            replay.status == "unavailable"
                && replay.exchange_count == record.attempts
                && replay.replayed_exchanges < replay.exchange_count
                && matches!(
                    replay.reason.as_deref(),
                    Some(
                        LEGACY_UNREPLAYABLE_CANCELLATION_REASON
                            | LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON
                    )
                )
        })
        && record
            .wire_response_shapes
            .last()
            .and_then(|shape| shape.http_status)
            .is_some_and(|status| (200..300).contains(&status))
}

/// 把提前完成记录绑定到同一 Fixture 的前序不可复核事实与最终统一响应，拒绝只改状态字段。
fn current_completed_cancellation_replay_gap_fixture(
    record: &ProbeRecord,
    fixture: &ProbeFixtureEnvelope,
) -> bool {
    let Some(replay) = record.fixture_replay.as_ref() else {
        return false;
    };
    let Some(reason) = replay.reason.as_deref() else {
        return false;
    };
    let Some((final_exchange, prior_exchanges)) = fixture.payload.exchanges.split_last() else {
        return false;
    };
    let prior_gap_matches = prior_exchanges.iter().any(|exchange| {
        matches!(
            &exchange.expected_outcome,
            FixtureExchangeOutcome::Unavailable { reason: prior_reason }
                if prior_reason == reason
        ) || (reason == LEGACY_UNREPLAYABLE_CANCELLATION_REASON
            && matches!(
                exchange.expected_outcome,
                FixtureExchangeOutcome::ObservedTerminalError { .. }
            ))
    });
    fixture.payload.exchanges.len() == record.attempts
        && prior_gap_matches
        && final_exchange
            .response_shape
            .http_status
            .is_some_and(|status| (200..300).contains(&status))
        && final_exchange.observed_terminal_error.is_none()
        && matches!(
            &final_exchange.expected_outcome,
            FixtureExchangeOutcome::Response {
                response,
                actual_text_evidence,
            } if record.response.as_ref() == Some(response)
                && record.actual_text_evidence.as_ref() == Some(actual_text_evidence)
        )
}

/// 只接受当前线级捕获实现能够产生的固定不可复核原因。
fn is_known_unavailable_replay_reason(reason: &str) -> bool {
    matches!(
        reason,
        UNAVAILABLE_RESPONSE_BODY_REASON
            | LEGACY_UNREPLAYABLE_CANCELLATION_REASON
            | LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON
            | TRUNCATED_RESPONSE_REPLAY_REASON
            | INVALID_UTF8_RESPONSE_REPLAY_REASON
    )
}

/// 校验状态、尝试次数、响应、错误、重放和跳过依赖之间的最小一致性。
fn validate_probe_record_invariants(
    manifest: &ResumeManifest,
    key: &str,
    record: &ProbeRecord,
) -> Result<(), String> {
    if record.attempts > manifest.identity.max_attempts {
        return Err(format!(
            "恢复记录 {key} 的尝试次数超过本轮上限 {}",
            manifest.identity.max_attempts
        ));
    }
    let mut assertion_names = BTreeSet::new();
    if record
        .assertions
        .iter()
        .any(|assertion| !assertion_names.insert(assertion.name.as_str()))
    {
        return Err(format!("恢复记录 {key} 包含重复语义断言名称"));
    }
    if record.response.is_some() != record.actual_text_evidence.is_some() {
        return Err(format!("恢复记录 {key} 的成功响应与实际文本证据不成对"));
    }
    if record
        .actual_text_evidence
        .as_ref()
        .is_some_and(|evidence| !valid_hmac_sha256_proof(&evidence.hmac_sha256))
    {
        return Err(format!("恢复记录 {key} 的实际文本 HMAC 格式无效"));
    }
    if record.normalized_error.as_ref().is_some_and(|error| {
        error.message_evidence.utf8_bytes == 0 || error.message_evidence.utf8_bytes > 4_000
    }) {
        return Err(format!("恢复记录 {key} 的错误说明证据格式或长度无效"));
    }
    for response_shape in &record.wire_response_shapes {
        response_shape
            .validate()
            .map_err(|error| format!("恢复记录 {key} 的响应结构证据无效：{error}"))?;
    }
    if record.capability == "cancellation" {
        if record.attempts > 0 && record.cancellation.is_none() {
            return Err(format!("恢复记录 {key} 的取消能力缺少取消证据"));
        }
        if let Some(cancellation) = &record.cancellation {
            if cancellation.remote_termination_proven
                || (cancellation.local_future_dropped && cancellation.completed_before_cancel)
            {
                return Err(format!("恢复记录 {key} 的取消边界事实互相冲突"));
            }
        }
    } else if record.cancellation.is_some() {
        return Err(format!("恢复记录 {key} 的非取消能力携带了取消证据"));
    }

    if record.attempts == 0 {
        if !matches!(record.status.as_str(), "failed" | "skipped") {
            return Err(format!("恢复记录 {key} 零次请求只能是 failed 或 skipped"));
        }
        if record.response.is_some()
            || record.actual_text_evidence.is_some()
            || record.fixture_replay.is_some()
            || record.cancellation.is_some()
            || !record.wire_response_shapes.is_empty()
        {
            return Err(format!("恢复记录 {key} 零次请求却携带了远端响应证据"));
        }
        if record.status == "failed" {
            if record.skip_evidence.is_some()
                || record
                    .normalized_error
                    .as_ref()
                    .is_none_or(|error| error.kind != "configuration")
            {
                return Err(format!("恢复记录 {key} 的零请求失败缺少本地配置错误"));
            }
        } else {
            validate_skip_evidence(manifest, key, record)?;
        }
        return Ok(());
    }

    if record.skip_evidence.is_some() || record.status == "skipped" {
        return Err(format!("恢复记录 {key} 已发送请求却声明为 skipped"));
    }
    let replay = record
        .fixture_replay
        .as_ref()
        .ok_or_else(|| format!("恢复记录 {key} 已发送请求但缺少 Fixture 重放结论"))?;
    if record.wire_response_shapes.len() != replay.exchange_count {
        return Err(format!(
            "恢复记录 {key} 的响应结构证据与 Fixture 交换数量不一致"
        ));
    }
    if !matches!(
        replay.status.as_str(),
        "passed" | "failed" | "unavailable" | "not_applicable"
    ) || replay.replayed_exchanges > replay.exchange_count
        || (replay.status == "passed"
            && (replay.exchange_count == 0 || replay.replayed_exchanges != replay.exchange_count))
    {
        return Err(format!("恢复记录 {key} 的 Fixture 重放统计无效"));
    }
    match record.status.as_str() {
        "passed" => {
            if record.assertions.iter().any(|assertion| !assertion.passed)
                || (!matches!(replay.status.as_str(), "passed" | "not_applicable")
                    && !replay_unavailable_by_body_omission(replay))
            {
                return Err(format!("恢复记录 {key} 的 passed 状态与断言或重放结论冲突"));
            }
            let expected_error_capability = record.capability.starts_with("diagnostic_")
                || matches!(
                    record.capability.as_str(),
                    "invalid_parameter" | "context_overflow" | "stream_interruption"
                );
            if expected_error_capability {
                if record.normalized_error.is_none() || record.response.is_some() {
                    return Err(format!(
                        "恢复记录 {key} 的错误契约通过记录与响应或统一错误冲突"
                    ));
                }
                if replay.status != "passed" && !replay_unavailable_by_body_omission(replay) {
                    return Err(format!("恢复记录 {key} 的错误契约与 Fixture 重放状态冲突"));
                }
            } else if record.capability == "cancellation" {
                let cancellation = record
                    .cancellation
                    .as_ref()
                    .ok_or_else(|| format!("恢复记录 {key} 的取消通过状态缺少边界事实"))?;
                if !cancellation.local_future_dropped
                    || cancellation.completed_before_cancel
                    || record.response.is_some()
                    || record.normalized_error.is_some()
                    || (replay.status != "not_applicable"
                        && !replay_unavailable_by_body_omission(replay))
                {
                    return Err(format!(
                        "恢复记录 {key} 的取消通过状态与响应、错误或重放结论冲突"
                    ));
                }
            } else if record.response.is_none()
                || record.normalized_error.is_some()
                || (replay.status != "passed" && !replay_unavailable_by_body_omission(replay))
            {
                return Err(format!(
                    "恢复记录 {key} 的成功能力与响应、错误或重放结论冲突"
                ));
            }
        }
        "contract_violation" => {
            if record.assertions.is_empty()
                || record.assertions.iter().all(|assertion| assertion.passed)
                || (record.response.is_none() && record.normalized_error.is_none())
            {
                return Err(format!("恢复记录 {key} 的 contract_violation 缺少失败事实"));
            }
        }
        "failed" => {
            if record.normalized_error.is_none() || record.response.is_some() {
                return Err(format!("恢复记录 {key} 的 failed 状态与响应或错误冲突"));
            }
            if record.capability == "cancellation" {
                let cancellation = record
                    .cancellation
                    .as_ref()
                    .ok_or_else(|| format!("恢复记录 {key} 的取消失败状态缺少边界事实"))?;
                if cancellation.local_future_dropped
                    || cancellation.completed_before_cancel
                    || !matches!(replay.status.as_str(), "passed" | "unavailable")
                {
                    return Err(format!(
                        "恢复记录 {key} 的取消失败状态没有绑定可重放错误或明确的传输终态"
                    ));
                }
            }
        }
        "unverified" => {
            if record.assertions.iter().all(|assertion| assertion.passed) {
                return Err(format!("恢复记录 {key} 的 unverified 状态缺少未通过断言"));
            }
            if record.capability == "cancellation" {
                let cancellation = record
                    .cancellation
                    .as_ref()
                    .ok_or_else(|| format!("恢复记录 {key} 的取消提前完成状态缺少边界事实"))?;
                let completed_before_cancel = !cancellation.local_future_dropped
                    && cancellation.completed_before_cancel
                    && record.response.is_some()
                    && record.actual_text_evidence.is_some()
                    && record.normalized_error.is_none()
                    && (replay.status == "passed" || replay_unavailable_by_body_omission(replay));
                if !completed_before_cancel && !unverified_cancellation_replay_gap(record, replay) {
                    return Err(format!(
                        "恢复记录 {key} 的取消提前完成状态没有通过真实响应重放"
                    ));
                }
            }
        }
        _ => return Err(format!("恢复记录 {key} 包含未知终态：{}", record.status)),
    }
    Ok(())
}

/// 判断持久化重放缺失是否只由“不保存任意远端响应正文”的固定策略造成。
fn replay_unavailable_by_body_omission(replay: &FixtureReplayEvidence) -> bool {
    replay.status == "unavailable"
        && replay.reason.as_deref() == Some(UNAVAILABLE_RESPONSE_BODY_REASON)
}

/// 验证证据是固定前缀加 64 位小写十六进制 HMAC-SHA256。
fn valid_hmac_sha256_proof(value: &str) -> bool {
    value.strip_prefix("hmac-sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

/// 验证证据是固定前缀加 64 位小写十六进制 SHA-256。
fn valid_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

/// 校验零请求跳过记录确实由同一 tuple 的基础文本门禁阻止。
fn validate_skip_evidence(
    manifest: &ResumeManifest,
    key: &str,
    record: &ProbeRecord,
) -> Result<(), String> {
    let skip = record
        .skip_evidence
        .as_ref()
        .ok_or_else(|| format!("恢复记录 {key} 的 skipped 状态缺少跳过证据"))?;
    if record.capability == "text"
        || record.normalized_error.is_some()
        || record.response.is_some()
        || record.fixture_replay.is_some()
        || !record.assertions.is_empty()
        || skip.verification != "unverified"
    {
        return Err(format!("恢复记录 {key} 的 skipped 状态字段不一致"));
    }
    let gate = manifest
        .records
        .get(&skip.blocked_by)
        .ok_or_else(|| format!("恢复记录 {key} 引用的基础文本门禁不存在"))?;
    let gate_error = gate.normalized_error.as_ref();
    if !gate.reusable()
        || gate.capability != "text"
        || gate.provider_id != record.provider_id
        || gate.model != record.model
        || gate.protocol != record.protocol
        || gate.response_mode != record.response_mode
        || gate.status == "passed"
        || skip.gate_status != gate.status
        || skip.error_kind.as_deref() != gate_error.map(|error| error.kind.as_str())
        || skip.retryable != gate_error.map(|error| error.retryable)
        || skip.http_status != gate_error.and_then(|error| error.http_status)
    {
        return Err(format!("恢复记录 {key} 的基础门禁快照与实际记录不一致"));
    }
    Ok(())
}

/// 判断目录项是否属于不可变 Fixture 原子提交使用的保留临时命名空间。
fn is_fixture_staging_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix(FIXTURE_STAGING_PREFIX)
        .and_then(|value| value.strip_suffix(".tmp"))
    else {
        return false;
    };
    !body.is_empty()
        && body.len() <= 240
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// 验证既有内容寻址 Fixture 仍为直属普通文件且与待提交字节完全一致。
fn verify_existing_immutable_fixture(
    run_dir: &Path,
    relative_path: &Path,
    text: &str,
) -> Result<(), String> {
    let existing_path = validated_run_descendant(
        run_dir,
        relative_path,
        RunPathKind::File,
        "既有不可变 Fixture",
    )?;
    let existing = read_bounded_regular_file(
        &existing_path,
        MAX_FIXTURE_FILE_BYTES,
        None,
        None,
        "既有不可变 Fixture",
    )?;
    if existing == text.as_bytes() {
        Ok(())
    } else {
        Err("不可变 Fixture 路径已经存在不同内容，拒绝覆盖".to_owned())
    }
}

/// 严格枚举直属 Fixture 普通 JSON 文件，并拒绝目录、链接和非内容寻址名称。
fn collect_resume_fixture_files(run_dir: &Path) -> Result<BTreeSet<String>, String> {
    let fixture_dir = validated_run_descendant(
        run_dir,
        Path::new("fixtures"),
        RunPathKind::Directory,
        "恢复 Fixture 目录",
    )?;
    let mut files = BTreeSet::new();
    let mut total_bytes = 0_u64;
    for entry in
        fs::read_dir(&fixture_dir).map_err(|error| format!("无法枚举恢复 Fixture 目录：{error}"))?
    {
        if files.len() >= MAX_FIXTURE_FILE_COUNT {
            return Err(format!(
                "恢复 Fixture 文件数超过 {} 个安全上限",
                MAX_FIXTURE_FILE_COUNT
            ));
        }
        let entry = entry.map_err(|error| format!("无法读取恢复 Fixture 目录项：{error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "恢复 Fixture 文件名必须是有效 Unicode".to_owned())?;
        let relative = format!("fixtures/{name}");
        let path = validated_run_descendant(
            run_dir,
            Path::new(&relative),
            RunPathKind::File,
            "恢复 Fixture",
        )?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("无法读取恢复 Fixture 文件元数据：{error}"))?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(format!(
                "fixtures 只能包含直属普通文件且不能包含链接或子目录：{name}"
            ));
        }
        if metadata.len() > MAX_FIXTURE_FILE_BYTES {
            return Err(format!(
                "恢复 Fixture {name} 超过 {} 字节单文件上限",
                MAX_FIXTURE_FILE_BYTES
            ));
        }
        total_bytes = total_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "恢复 Fixture 总字节数溢出".to_owned())?;
        if total_bytes > MAX_FIXTURE_TOTAL_BYTES {
            return Err(format!(
                "恢复 Fixture 总字节数超过 {} 字节安全上限",
                MAX_FIXTURE_TOTAL_BYTES
            ));
        }
        if !is_content_addressed_fixture_name(&name) {
            return Err(format!("Fixture 文件名不是当前内容寻址 JSON 格式：{name}"));
        }
        files.insert(relative);
    }
    Ok(files)
}

/// 判断文件名是否由两个 64 位小写十六进制摘要组成。
fn is_content_addressed_fixture_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    let Some((stable_digest, content_digest)) = stem.split_once('-') else {
        return false;
    };
    [stable_digest, content_digest].into_iter().all(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

/// 限制记录只能引用 `fixtures/` 下单层内容寻址 JSON 文件。
fn validate_fixture_relative_path(relative: &str) -> Result<(), String> {
    let Some(name) = relative.strip_prefix("fixtures/") else {
        return Err(format!("恢复 Fixture 路径不在 fixtures 目录：{relative}"));
    };
    if name.contains(['/', '\\']) || !is_content_addressed_fixture_name(name) {
        return Err(format!(
            "恢复 Fixture 路径不是直属内容寻址 JSON：{relative}"
        ));
    }
    Ok(())
}

/// 把 Fixture Payload 的两个摘要编码为唯一相对文件名。
fn fixture_relative_path(
    payload: &ProbeFixturePayload,
    content_sha256: &str,
) -> Result<String, String> {
    let content_digest = content_sha256
        .strip_prefix("sha256:")
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .ok_or_else(|| "Fixture Payload 摘要格式无效".to_owned())?;
    let stable_key_digest = domain_separated_hex(
        b"keencode-provider-fixture-stable-key-v2",
        &[payload.stable_key.as_bytes()],
    );
    Ok(format!(
        "fixtures/{stable_key_digest}-{content_digest}.json"
    ))
}

/// 校验 Fixture Envelope 的全部身份与预期字段都和对应 ProbeRecord 一致。
fn validate_fixture_record_binding(
    manifest: &ResumeManifest,
    record: &ProbeRecord,
    relative: &str,
    fixture: &ProbeFixtureEnvelope,
) -> Result<(), String> {
    let payload = &fixture.payload;
    let expected_relative = fixture_relative_path(payload, &fixture.content_sha256)?;
    if relative != expected_relative {
        return Err(format!(
            "恢复 Fixture 文件名与稳定键或 Payload 内容摘要不一致：{relative}"
        ));
    }
    let evidence_run_id = record
        .recovered_from
        .as_ref()
        .map_or(manifest.run.run_id.as_str(), |origin| {
            origin.source_run_id.as_str()
        });
    if payload.run_id != evidence_run_id
        || payload.stable_key != record.stable_key
        || payload.provider_id != record.provider_id
        || payload.model != record.model
        || payload.protocol != record.protocol
        || payload.response_mode != record.response_mode
        || payload.capability != record.capability
        || payload.synthetic_marker != record.synthetic_marker
        || payload.expected_response != record.response
        || payload.expected_actual_text_evidence != record.actual_text_evidence
        || payload.expected_error != record.normalized_error
        || payload.expected_cancellation != record.cancellation
        || payload.replay != record.fixture_replay
    {
        return Err(format!(
            "恢复 Fixture Payload 与记录身份、Marker 或预期结果不一致：{relative}"
        ));
    }
    if payload
        .replay
        .as_ref()
        .is_none_or(|replay| replay.exchange_count != payload.exchanges.len())
    {
        return Err(format!(
            "恢复 Fixture 的交换数量与记录重放证据不一致：{relative}"
        ));
    }
    let fixture_shapes = payload
        .exchanges
        .iter()
        .map(|exchange| exchange.response_shape.clone())
        .collect::<Vec<_>>();
    if fixture_shapes != record.wire_response_shapes {
        return Err(format!(
            "恢复 Fixture 的响应结构证据与 ProbeRecord 不一致：{relative}"
        ));
    }
    Ok(())
}

/// 校验全局锁父目录和固定文件名，并返回规范绝对路径。
fn validated_global_lock_path(lock_path: &Path) -> Result<PathBuf, String> {
    let file_name = lock_path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "Provider 真实测试全局锁缺少文件名".to_owned())?;
    if file_name != std::ffi::OsStr::new(LIVE_TEST_PROCESS_LOCK_FILE) {
        return Err("Provider 真实测试全局锁必须使用固定匿名文件名".to_owned());
    }
    let parent = lock_path
        .parent()
        .ok_or_else(|| "Provider 真实测试全局锁缺少父目录".to_owned())?;
    let metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("无法读取 Provider 真实测试全局锁父目录：{error}"))?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(
            "Provider 真实测试全局锁父路径必须是普通目录且不能是符号链接或重解析点".to_owned(),
        );
    }
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| format!("无法规范化 Provider 真实测试全局锁父目录：{error}"))?;
    let canonical_metadata = fs::symlink_metadata(&canonical_parent)
        .map_err(|error| format!("无法确认 Provider 真实测试全局锁父目录：{error}"))?;
    if is_link_or_reparse(&canonical_metadata) || !canonical_metadata.is_dir() {
        return Err("Provider 真实测试全局锁规范父路径必须是普通目录".to_owned());
    }
    Ok(canonical_parent.join(file_name))
}

/// 创建并校验稳定用户数据目录，再返回唯一的进程级锁路径。
fn prepare_global_lock_path(user_data_directory: &Path) -> Result<PathBuf, String> {
    if !user_data_directory.is_absolute() {
        return Err("Provider 真实测试用户数据目录必须是绝对路径".to_owned());
    }
    let parent = user_data_directory
        .parent()
        .ok_or_else(|| "Provider 真实测试用户数据目录缺少父目录".to_owned())?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("无法读取 Provider 真实测试用户目录：{error}"))?;
    if is_link_or_reparse(&parent_metadata) || !parent_metadata.is_dir() {
        return Err("Provider 真实测试用户目录必须是普通目录且不能是符号链接或重解析点".to_owned());
    }
    match fs::create_dir(user_data_directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!("无法创建 Provider 真实测试用户数据目录：{error}"));
        }
    }
    validated_global_lock_path(&user_data_directory.join(LIVE_TEST_PROCESS_LOCK_FILE))
}

/// 复核锁文件父路径仍指向已固定的同一普通目录对象。
fn verify_lock_parent_identity(
    parent_path: &Path,
    parent: &File,
    label: &str,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let current = open_pinned_unix_directory(parent_path, &format!("{label}父目录复核"))?;
        if unix_directory_identity(&current, &format!("{label}父目录复核"))?
            != unix_directory_identity(parent, &format!("{label}固定父目录"))?
        {
            return Err(format!("{label}父目录身份发生变化"));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let current = open_pinned_windows_directory(parent_path, &format!("{label}父目录复核"))?;
        if windows_object_identity_from_handle(&current, &format!("{label}父目录复核"))?
            != windows_object_identity_from_handle(parent, &format!("{label}固定父目录"))?
        {
            return Err(format!("{label}父目录身份发生变化"));
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = fs::symlink_metadata(parent_path)
            .map_err(|error| format!("无法复核{label}父目录：{error}"))?;
        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
            return Err(format!("{label}父路径必须保持为普通目录"));
        }
        let _ = parent;
        Ok(())
    }
}

/// 相对于已经固定的父目录打开锁文件，禁止跟随最终链接并返回稳定对象身份。
fn open_lock_file_in_parent(
    parent: &File,
    parent_path: &Path,
    file_name: &std::ffi::OsStr,
    creation: StableFileCreation,
    label: &str,
) -> Result<(File, RegularFileIdentity), String> {
    #[cfg(windows)]
    let _ = parent;
    #[cfg(unix)]
    let _ = parent_path;
    #[cfg(unix)]
    let file =
        unix_open_ffi::open_regular_at(parent, file_name, StableFileAccess::Lock, creation, label)?;
    #[cfg(windows)]
    let file = open_windows_regular_file_handle_with(
        &parent_path.join(file_name),
        StableFileAccess::Lock,
        creation,
        label,
    )?;
    #[cfg(not(any(unix, windows)))]
    let file = {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        if matches!(creation, StableFileCreation::CreateIfMissing) {
            options.create(true);
        }
        options
            .open(parent_path.join(file_name))
            .map_err(|error| format!("无法打开{label}：{error}"))?
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("无法确认已打开{label}元数据：{error}"))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() || metadata.len() != 0 {
        return Err(format!(
            "{label}必须是保持为空的普通文件且不能是符号链接或重解析点"
        ));
    }
    let identity = regular_file_identity_from_open_handle(&file, &metadata, label)?;
    Ok((file, identity))
}

/// 复核固定父目录中的锁文件名仍指向预期普通文件对象。
fn verify_lock_file_identity(
    parent: &File,
    parent_path: &Path,
    file_name: &std::ffi::OsStr,
    expected: &RegularFileIdentity,
    label: &str,
) -> Result<(), String> {
    #[cfg(windows)]
    let _ = parent;
    #[cfg(unix)]
    let _ = parent_path;
    #[cfg(unix)]
    let verification = unix_open_ffi::open_regular_at(
        parent,
        file_name,
        StableFileAccess::Verify,
        StableFileCreation::Existing,
        &format!("{label}复核文件"),
    )?;
    #[cfg(windows)]
    let verification = open_windows_regular_file_handle_with(
        &parent_path.join(file_name),
        StableFileAccess::Verify,
        StableFileCreation::Existing,
        &format!("{label}复核文件"),
    )?;
    #[cfg(not(any(unix, windows)))]
    let verification = File::open(parent_path.join(file_name))
        .map_err(|error| format!("无法打开{label}复核文件：{error}"))?;
    let metadata = verification
        .metadata()
        .map_err(|error| format!("无法读取{label}复核句柄元数据：{error}"))?;
    if is_link_or_reparse(&metadata)
        || !metadata.is_file()
        || metadata.len() != 0
        || &regular_file_identity_from_open_handle(&verification, &metadata, label)? != expected
    {
        return Err(format!("{label}路径所指锁文件对象发生变化"));
    }
    Ok(())
}

/// 在固定父目录中打开锁文件、取得非阻塞独占锁并闭合目录项身份竞态。
fn acquire_lock_in_pinned_parent(
    parent: File,
    parent_path: &Path,
    file_name: &std::ffi::OsStr,
    creation: StableFileCreation,
    label: &str,
    busy_message: &str,
) -> Result<HeldExclusiveLock, String> {
    verify_lock_parent_identity(parent_path, &parent, label)?;
    let (file, identity) =
        open_lock_file_in_parent(&parent, parent_path, file_name, creation, label)?;
    verify_lock_file_identity(&parent, parent_path, file_name, &identity, label)?;
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if is_lock_contention(&error) => {
            return Err(busy_message.to_owned());
        }
        Err(error) => return Err(format!("无法取得{label}独占锁：{error}")),
    }
    verify_lock_file_identity(&parent, parent_path, file_name, &identity, label)?;
    verify_lock_parent_identity(parent_path, &parent, label)?;
    Ok(HeldExclusiveLock {
        _file: file,
        _parent_directory: parent,
    })
}

/// 创建或打开普通锁文件，并非阻塞地取得跨进程独占锁。
fn acquire_exclusive_lock_file(
    lock_path: &Path,
    label: &str,
    busy_message: &str,
) -> Result<HeldExclusiveLock, String> {
    let parent_path = lock_path
        .parent()
        .ok_or_else(|| format!("{label}缺少父目录"))?;
    let file_name = lock_path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("{label}缺少文件名"))?;
    #[cfg(unix)]
    let parent = open_pinned_unix_directory(parent_path, &format!("{label}父目录"))?;
    #[cfg(windows)]
    let parent = open_pinned_windows_directory(parent_path, &format!("{label}父目录"))?;
    #[cfg(not(any(unix, windows)))]
    let parent =
        File::open(parent_path).map_err(|error| format!("无法打开{label}父目录：{error}"))?;
    acquire_lock_in_pinned_parent(
        parent,
        parent_path,
        file_name,
        StableFileCreation::CreateIfMissing,
        label,
        busy_message,
    )
}

/// 识别 Unix 非阻塞锁竞争和 Windows `ERROR_LOCK_VIOLATION` 的等价语义。
fn is_lock_contention(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        const ERROR_LOCK_VIOLATION: i32 = 33;
        error.raw_os_error() == Some(ERROR_LOCK_VIOLATION)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 相对于 Unix 固定运行目录打开锁文件；只读来源入口严格禁止补建。
#[cfg(unix)]
fn acquire_pinned_run_lock(
    pins: &UnixReportDirectoryPins,
    run_dir: &Path,
    existing_only: bool,
) -> Result<HeldExclusiveLock, String> {
    pins.verify_layout(run_dir)?;
    let parent = pins
        .run_dir
        .try_clone()
        .map_err(|error| format!("无法复制真实测试运行目录固定句柄：{error}"))?;
    let creation = if existing_only {
        StableFileCreation::Existing
    } else {
        StableFileCreation::CreateIfMissing
    };
    let (label, busy_message) = if existing_only {
        (
            "只读恢复来源运行锁",
            "恢复来源正在被另一个 Provider 真实测试进程使用",
        )
    } else {
        (
            "真实测试运行锁",
            "恢复目录正在被另一个 Provider 真实测试进程使用",
        )
    };
    acquire_lock_in_pinned_parent(
        parent,
        run_dir,
        std::ffi::OsStr::new(".keencode-live-test.lock"),
        creation,
        label,
        busy_message,
    )
}

/// 创建或打开运行锁文件，并把跨进程独占锁保持到 `ReportStore` 被销毁。
#[cfg(not(unix))]
fn acquire_run_lock(run_dir: &Path) -> Result<HeldExclusiveLock, String> {
    acquire_exclusive_lock_file(
        &run_dir.join(".keencode-live-test.lock"),
        "真实测试运行锁",
        "恢复目录正在被另一个 Provider 真实测试进程使用",
    )
}

/// 打开既有运行锁并取得独占锁；专用于不能在来源目录创建任何文件的隔离恢复。
#[cfg(not(unix))]
fn acquire_existing_run_lock(run_dir: &Path) -> Result<HeldExclusiveLock, String> {
    let lock_path = validated_run_descendant(
        run_dir,
        Path::new(".keencode-live-test.lock"),
        RunPathKind::File,
        "只读恢复来源运行锁",
    )?;
    let parent_path = lock_path
        .parent()
        .ok_or_else(|| "只读恢复来源运行锁缺少父目录".to_owned())?;
    #[cfg(unix)]
    let parent = open_pinned_unix_directory(parent_path, "只读恢复来源运行锁父目录")?;
    #[cfg(windows)]
    let parent = open_pinned_windows_directory(parent_path, "只读恢复来源运行锁父目录")?;
    #[cfg(not(any(unix, windows)))]
    let parent = File::open(parent_path)
        .map_err(|error| format!("无法打开只读恢复来源运行锁父目录：{error}"))?;
    acquire_lock_in_pinned_parent(
        parent,
        parent_path,
        std::ffi::OsStr::new(".keencode-live-test.lock"),
        StableFileCreation::Existing,
        "只读恢复来源运行锁",
        "恢复来源正在被另一个 Provider 真实测试进程使用",
    )
}

/// 在目标同目录写入、同步并以 `rename` 的 replace 语义提交，失败时旧文件始终保留。
fn replace_file_contents(destination: &Path, text: &str, label: &str) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "产物路径缺少父目录".to_owned())?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "产物文件名必须是有效 Unicode".to_owned())?;
    validate_replace_destination(destination, label)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("无法生成产物临时文件标识：{error}"))?
        .as_nanos();
    let mut temporary = None;
    let mut file = None;
    for attempt in 0..16_u8 {
        let candidate = parent.join(format!(
            ".{file_name}.{}.{}.{attempt}.tmp",
            std::process::id(),
            nonce
        ));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&candidate)
        {
            Ok(opened) => {
                temporary = Some(candidate);
                file = Some(opened);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("无法创建 {label} 临时文件：{error}")),
        }
    }
    let temporary = temporary.ok_or_else(|| format!("无法为 {label} 分配临时文件"))?;
    let mut file = file.expect("临时文件路径与句柄总是同时创建");
    if let Err(error) = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        return Err(append_replace_cleanup_error(
            format!("无法写入并同步 {label}：{error}"),
            cleanup_replace_temporary(&temporary, label),
        ));
    }
    drop(file);
    commit_temporary_replace_with(
        &temporary,
        destination,
        text.as_bytes(),
        label,
        |source, target| fs::rename(source, target),
        std::thread::sleep,
        is_transient_windows_replace_error,
    )
}

/// 原子替换前拒绝目标链接、重解析点、目录或其他特殊文件。
fn validate_replace_destination(destination: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
            Err(format!("{label} 目标必须是普通文件且不能是链接或重解析点"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法读取 {label} 目标元数据：{error}")),
    }
}

/// 使用可注入的 rename、退避和错误分类器提交临时文件，便于确定性覆盖竞态分支。
fn commit_temporary_replace_with<R, S, P>(
    temporary: &Path,
    destination: &Path,
    expected: &[u8],
    label: &str,
    mut rename: R,
    mut sleep: S,
    is_retryable: P,
) -> Result<(), String>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
    S: FnMut(Duration),
    P: Fn(&std::io::Error) -> bool,
{
    let outcome = (|| {
        let mut retries = WINDOWS_REPLACE_RETRY_DELAYS.iter();
        loop {
            match rename(temporary, destination) {
                Ok(()) => return Ok(()),
                Err(error) if is_retryable(&error) => {
                    match replace_source_exists(temporary, label)? {
                        true => validate_replace_destination(destination, label)?,
                        false if destination_matches_bytes(destination, expected, label)? => {
                            return Ok(());
                        }
                        false => {
                            return Err(format!(
                                "无法确定 {label} 原子提交结果：rename 返回 {error}，临时源已消失且目标不是预期内容"
                            ));
                        }
                    }
                    let Some(delay) = retries.next() else {
                        return Err(format!("无法原子提交 {label}，短暂占用重试已耗尽：{error}"));
                    };
                    sleep(*delay);
                    match replace_source_exists(temporary, label)? {
                        true => {}
                        false if destination_matches_bytes(destination, expected, label)? => {
                            return Ok(());
                        }
                        false => {
                            return Err(format!(
                                "无法确定 {label} 原子提交结果：退避期间临时源消失且目标不是预期内容"
                            ));
                        }
                    }
                }
                Err(error) => return Err(format!("无法原子提交 {label}：{error}")),
            }
        }
    })();
    match outcome {
        Ok(()) => Ok(()),
        Err(error) => Err(append_replace_cleanup_error(
            error,
            cleanup_replace_temporary(temporary, label),
        )),
    }
}

/// 在每次重试前确认临时源仍是同目录普通文件。
fn replace_source_exists(temporary: &Path, label: &str) -> Result<bool, String> {
    match fs::symlink_metadata(temporary) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
            Err(format!("{label} 临时源在原子提交期间不再是普通文件"))
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("无法确认 {label} 临时源状态：{error}")),
    }
}

/// 当 rename 返回错误但临时源已经消失时，确认目标是否实际完成了期望提交。
fn destination_matches_bytes(
    destination: &Path,
    expected: &[u8],
    label: &str,
) -> Result<bool, String> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
            Err(format!("{label} 目标在原子提交期间不再是普通文件"))
        }
        Ok(metadata) => {
            if metadata.len() != u64::try_from(expected.len()).unwrap_or(u64::MAX) {
                return Ok(false);
            }
            fs::read(destination)
                .map(|actual| actual == expected)
                .map_err(|error| format!("无法核对 {label} 原子提交结果：{error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("无法读取 {label} 提交目标元数据：{error}")),
    }
}

/// 只在 Windows 对访问拒绝、共享冲突和锁冲突执行固定短退避。
fn is_transient_windows_replace_error(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        matches!(
            error.raw_os_error(),
            Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
        )
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

/// 有界清理尚未提交的临时文件，扫描器短暂占用时使用相同 Windows 退避策略。
fn cleanup_replace_temporary(temporary: &Path, label: &str) -> Result<(), String> {
    let mut retries = WINDOWS_REPLACE_RETRY_DELAYS.iter();
    loop {
        match fs::remove_file(temporary) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if is_transient_windows_replace_error(&error) => {
                let Some(delay) = retries.next() else {
                    return Err(format!("无法清理 {label} 临时文件：{error}"));
                };
                std::thread::sleep(*delay);
            }
            Err(error) => return Err(format!("无法清理 {label} 临时文件：{error}")),
        }
    }
}

/// 在写入或提交错误后附加临时文件清理失败，确保遗留状态不会被静默忽略。
fn append_replace_cleanup_error(error: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => format!("{error}；此外，{cleanup}"),
    }
}

/// 校验 Manifest 已确认前缀，再按日志序号把更晚的完整提交确定性合并进来。
fn reconcile_progress_journal(
    manifest: &mut ResumeManifest,
    journal: &[OwnedProbeJournalEntry],
) -> Result<(), String> {
    let journal_max = journal.last().map_or(0, |entry| entry.sequence);
    if manifest.journal_sequence > journal_max {
        return Err(format!(
            "恢复清单引用了不存在的提交日志序号 {}，日志最大序号为 {journal_max}",
            manifest.journal_sequence
        ));
    }
    if manifest.identity.schema_version == RESUME_SCHEMA_VERSION {
        let expected_prefix_tail = if manifest.journal_sequence == 0 {
            JOURNAL_INITIAL_MAC
        } else {
            journal
                .get(manifest.journal_sequence as usize - 1)
                .and_then(|entry| entry.record_mac.as_deref())
                .ok_or_else(|| "恢复清单引用的 Journal 前缀缺少链尾 MAC".to_owned())?
        };
        if manifest.journal_tail_mac.as_deref() != Some(expected_prefix_tail) {
            return Err("恢复清单声明的 Journal 链尾与已认证日志前缀不一致".to_owned());
        }
    }
    let mut committed_prefix = BTreeMap::new();
    for entry in journal.iter().take(manifest.journal_sequence as usize) {
        insert_idempotent_record(
            &mut committed_prefix,
            entry.record.stable_key(),
            entry.record.clone(),
            "恢复日志已确认前缀",
        )?;
    }
    let expected = serde_json::to_value(&manifest.records)
        .map_err(|error| format!("无法比较恢复清单记录：{error}"))?;
    let actual = serde_json::to_value(&committed_prefix)
        .map_err(|error| format!("无法比较恢复日志记录：{error}"))?;
    if expected != actual {
        return Err("恢复清单与其已确认的提交日志前缀冲突".to_owned());
    }
    for entry in journal.iter().skip(manifest.journal_sequence as usize) {
        let key = entry.record.stable_key();
        insert_idempotent_record(
            &mut manifest.records,
            key,
            entry.record.clone(),
            "恢复日志未确认后缀",
        )?;
        manifest.journal_sequence = entry.sequence;
        if manifest.identity.schema_version == RESUME_SCHEMA_VERSION {
            manifest.journal_tail_mac = entry.record_mac.clone();
        }
    }
    Ok(())
}

/// 返回完成认证与调和后 Store 应继续使用的当前 Journal 链尾。
fn authenticated_journal_tail(
    manifest: &ResumeManifest,
    journal: &[OwnedProbeJournalEntry],
) -> Result<String, String> {
    if manifest.identity.schema_version == RETRY_SOURCE_RESUME_SCHEMA_VERSION {
        return Ok(JOURNAL_INITIAL_MAC.to_owned());
    }
    let expected = if let Some(entry) = journal.last() {
        entry
            .record_mac
            .as_deref()
            .ok_or_else(|| "当前 Journal 末条记录缺少链尾 MAC".to_owned())?
    } else {
        JOURNAL_INITIAL_MAC
    };
    if manifest.journal_tail_mac.as_deref() != Some(expected) {
        return Err("恢复清单调和后的 Journal 链尾与认证日志不一致".to_owned());
    }
    Ok(expected.to_owned())
}

/// 向稳定键集合加入记录；完全相同的重复提交可重放，不同内容视为损坏。
fn insert_idempotent_record(
    records: &mut BTreeMap<String, ProbeRecord>,
    key: String,
    record: ProbeRecord,
    source: &str,
) -> Result<(), String> {
    match records.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(record);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(entry)
            if probe_records_equal(entry.get(), &record)? =>
        {
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(entry) => Err(format!(
            "{source} 中稳定键 {} 对应了不同内容，拒绝最后写入覆盖",
            entry.key()
        )),
    }
}

/// 按实际持久化 JSON 比较记录，忽略不会序列化的内存线级交换。
fn probe_records_equal(left: &ProbeRecord, right: &ProbeRecord) -> Result<bool, String> {
    let left = serde_json::to_value(left).map_err(|error| format!("无法比较探测记录：{error}"))?;
    let right =
        serde_json::to_value(right).map_err(|error| format!("无法比较探测记录：{error}"))?;
    Ok(left == right)
}

/// 一个不可变 Fixture 文件的版本、内容摘要与规范 Payload Envelope。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeFixtureEnvelope {
    /// Fixture Envelope 的唯一受支持版本。
    schema_version: String,
    /// 对规范序列化 Payload 使用独立版本域计算的 SHA-256。
    content_sha256: String,
    /// 与单个探测记录一一绑定的完整脱敏事实。
    payload: ProbeFixturePayload,
}

/// 一条探测写盘后的完整身份、线级交换和预期事实。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeFixturePayload {
    /// 当前运行的稳定标识。
    run_id: String,
    /// 当前探测的版本化稳定键。
    stable_key: String,
    /// 脱敏后的 Provider 稳定标识。
    provider_id: String,
    /// 脱敏后的模型标识。
    model: String,
    /// 当前请求使用的协议名称。
    protocol: String,
    /// 当前请求使用的响应模式。
    response_mode: String,
    /// 当前探测的能力名称。
    capability: String,
    /// 当前记录实际使用的精确合成标记；零请求记录为空。
    synthetic_marker: Option<String>,
    /// 所有持久化请求是否只包含 Harness 合成数据。
    synthetic_only: bool,
    /// 单轮或多轮能力实际发生的全部 HTTP 交换。
    exchanges: Vec<FixtureExchange>,
    /// 最终成功响应的 Provider 中立摘要。
    expected_response: Option<ResponseEvidence>,
    /// 最终成功响应正文的字节数与运行级 HMAC。
    expected_actual_text_evidence: Option<ActualTextEvidence>,
    /// 最终失败的统一错误摘要。
    expected_error: Option<NormalizedError>,
    /// 取消探测只绑定在线本地计时事实，不参与响应重放。
    expected_cancellation: Option<CancellationEvidence>,
    /// 写盘前已经执行的离线 Adapter 重放结论。
    replay: Option<FixtureReplayEvidence>,
}

/// 对 Fixture v6 Payload 的规范 JSON 使用独立版本域计算内容摘要。
fn fixture_payload_sha256(payload: &ProbeFixturePayload) -> Result<String, String> {
    let bytes = serde_json::to_vec(payload)
        .map_err(|error| format!("无法规范序列化 Fixture Payload：{error}"))?;
    Ok(format!(
        "sha256:{}",
        domain_separated_hex(b"keencode-provider-fixture-payload-v6", &[&bytes])
    ))
}

/// 一次不含认证 Header、请求正文或响应正文的线级结构证据。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureExchange {
    /// 纯合成首个请求的完整绑定，或后续可能携带远端历史时的省略证明。
    request: FixtureRequestEvidence,
    /// 当前 ProviderClient 实际使用的单个 SSE 事件字节上限。
    max_event_bytes: usize,
    /// 不包含正文、任意名称或值的响应状态、媒体类型、格式和结构证据。
    response_shape: WireResponseShapeEvidence,
    /// 在线 Provider 实际返回的脱敏统一终态错误；本地丢弃在途调用时为空。
    observed_terminal_error: Option<NormalizedError>,
    /// 在线阶段按同一 Adapter 归一化得到的逐交换期望结果。
    expected_outcome: FixtureExchangeOutcome,
}

/// 一次线级请求可以安全写盘的请求侧证据。
#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum FixtureRequestEvidence {
    /// 首个请求已在内存完成纯合成模板与 Adapter 编码绑定校验。
    SyntheticFirstRequest {
        /// Provider 中立请求的消息数量。
        semantic_message_count: usize,
        /// Provider 中立请求的工具定义数量。
        semantic_tool_count: usize,
        /// Adapter 实际请求顶层字段数量。
        wire_top_level_field_count: usize,
    },
    /// 后续请求可能包含模型文本、推理或工具参数，因此整体省略。
    SubsequentRequestOmitted {
        /// 固定且不包含任何远端数据的省略原因。
        reason: String,
    },
}

/// 单个线级交换在在线阶段形成的期望结果；磁盘只复核可持久化事实。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum FixtureExchangeOutcome {
    /// 2xx JSON 或 SSE 已归一化为完整统一响应。
    Response {
        /// 不包含任意正文的统一响应证据。
        response: ResponseEvidence,
        /// 归并后文本的字节数与运行级 HMAC，不保存正文。
        actual_text_evidence: ActualTextEvidence,
    },
    /// 非 2xx 或 2xx Adapter 错误已归一化为统一错误。
    Error {
        /// 必须逐字段完全一致的统一错误。
        error: NormalizedError,
    },
    /// 无法由磁盘响应重新制造的发送前或传输层终态，仅绑定在线观察值。
    ObservedTerminalError {
        /// 与 ProbeRecord 对齐但不计作磁盘响应复核成功的统一错误。
        error: NormalizedError,
    },
    /// 响应不完整或不可持久化，因此只能保留请求与固定原因。
    Unavailable {
        /// 不包含响应正文的稳定原因。
        reason: String,
    },
    /// 本地取消场景只证明请求已开始，不声称远端终止或响应可重放。
    RequestOnly,
}

impl FixtureExchange {
    /// 从内存线级交换创建不会包含配置凭据的 Fixture。
    fn from_wire(
        exchange: &WireExchange,
        expected_outcome: &FixtureExchangeOutcome,
        response_shape: &WireResponseShapeEvidence,
        persist_request: bool,
    ) -> Result<Self, String> {
        let request = if persist_request {
            FixtureRequestEvidence::SyntheticFirstRequest {
                semantic_message_count: exchange.model_request.messages.len(),
                semantic_tool_count: exchange.model_request.tools.len(),
                wire_top_level_field_count: exchange
                    .request_body
                    .as_object()
                    .map(serde_json::Map::len)
                    .unwrap_or(0),
            }
        } else {
            FixtureRequestEvidence::SubsequentRequestOmitted {
                reason: OMITTED_SUBSEQUENT_REQUEST_REASON.to_owned(),
            }
        };
        let observed_terminal_error = exchange
            .terminal_error
            .as_ref()
            .map(|_| match expected_outcome {
                FixtureExchangeOutcome::Error { error }
                | FixtureExchangeOutcome::ObservedTerminalError { error } => Ok(error.clone()),
                FixtureExchangeOutcome::Response { .. }
                | FixtureExchangeOutcome::Unavailable { .. }
                | FixtureExchangeOutcome::RequestOnly => {
                    Err("线级交换保存了终态错误，但在线归一化结果不是错误".to_owned())
                }
            })
            .transpose()?;
        Ok(Self {
            request,
            max_event_bytes: exchange.max_event_bytes,
            response_shape: response_shape.clone(),
            observed_terminal_error,
            expected_outcome: expected_outcome.clone(),
        })
    }
}

/// 根据持久化策略计算磁盘复核结果，禁止把仅有在线证据的响应算作正文重放成功。
fn persisted_fixture_replay_outcome(exchange: &FixtureExchange) -> FixtureExchangeOutcome {
    match &exchange.expected_outcome {
        FixtureExchangeOutcome::Response { .. } | FixtureExchangeOutcome::Error { .. } => {
            FixtureExchangeOutcome::Unavailable {
                reason: UNAVAILABLE_RESPONSE_BODY_REASON.to_owned(),
            }
        }
        outcome => outcome.clone(),
    }
}

/// 严格解析 Fixture Envelope 与自身内容摘要。
fn parse_fixture_envelope(text: &str) -> Result<ProbeFixtureEnvelope, String> {
    let fixture: ProbeFixtureEnvelope = serde_json::from_str(text)
        .map_err(|error| format!("Fixture 必须是有效 v6 JSON：{error}"))?;
    if fixture.schema_version != FIXTURE_SCHEMA_VERSION {
        return Err(format!(
            "Fixture schema 不受支持：{}",
            fixture.schema_version
        ));
    }
    let expected_sha256 = fixture_payload_sha256(&fixture.payload)?;
    if fixture.content_sha256 != expected_sha256 {
        return Err("Fixture Payload 内容摘要不一致".to_owned());
    }
    if !fixture.payload.synthetic_only {
        return Err("Fixture 请求缺少纯合成提示词证明，拒绝写盘".to_owned());
    }
    if fixture.payload.exchanges.is_empty() {
        return Err("Fixture 缺少线级请求，无法验证合成提示词".to_owned());
    }
    Ok(fixture)
}

/// 把 Fixture 中的稳定协议名称还原为统一枚举。
fn fixture_protocol(protocol: &str) -> Result<ProviderProtocol, String> {
    match protocol {
        "anthropic_messages" => Ok(ProviderProtocol::Messages),
        "openai_chat_completions" => Ok(ProviderProtocol::ChatCompletions),
        "openai_responses" => Ok(ProviderProtocol::Responses),
        _ => Err(format!("Fixture 包含未知协议：{protocol}")),
    }
}

/// 严格反序列化 Provider 中立请求，并用逐字段往返拒绝未知字段或省略字段。
fn strict_semantic_request(value: &serde_json::Value) -> Result<ModelRequest, String> {
    let request: ModelRequest = serde_json::from_value(value.clone())
        .map_err(|error| format!("Fixture Provider 中立请求无法反序列化：{error}"))?;
    let round_trip = serde_json::to_value(&request)
        .map_err(|error| format!("Fixture Provider 中立请求无法重新序列化：{error}"))?;
    if &round_trip != value {
        return Err("Fixture Provider 中立请求包含未知字段、字段夹带或被省略的规范字段".to_owned());
    }
    request
        .validate()
        .map_err(|error| format!("Fixture Provider 中立请求不满足统一层不变量：{error}"))?;
    Ok(request)
}

/// 校验首个统一请求只包含当前 Harness 生成的用户文本，不夹带隐藏元数据或历史块。
fn validate_initial_semantic_request(
    request: &ModelRequest,
    expected_model: &str,
) -> Result<(), String> {
    if request.model != expected_model {
        return Err("Fixture 首个统一请求的模型标识与探测记录不一致".to_owned());
    }
    if !request.metadata.is_empty() {
        return Err("Fixture 首个统一请求不得包含追踪元数据".to_owned());
    }
    if request.messages.len() != 1 {
        return Err("Fixture 首个统一请求必须只有一条 Harness 用户消息".to_owned());
    }
    for message in &request.messages {
        if message.role != MessageRole::User {
            return Err("Fixture 首个统一请求不得包含系统、assistant 或工具历史".to_owned());
        }
        if message.content.is_empty()
            || message
                .content
                .iter()
                .any(|block| !matches!(block, ContentBlock::Text { .. }))
        {
            return Err("Fixture 首个统一请求只允许 Harness 用户文本块".to_owned());
        }
    }
    Ok(())
}

/// 逐交换验证首请求结构摘要、后续省略策略和事件上限。
fn validate_fixture_request_binding(fixture: &ProbeFixtureEnvelope) -> Result<(), String> {
    let protocol = fixture_protocol(&fixture.payload.protocol)?;
    match fixture.payload.response_mode.as_str() {
        "buffered" | "streaming" => {}
        value => return Err(format!("Fixture 包含未知响应模式：{value}")),
    }
    for (index, exchange) in fixture.payload.exchanges.iter().enumerate() {
        if exchange.max_event_bytes == 0 || exchange.max_event_bytes > MAX_FIXTURE_EVENT_BYTES {
            return Err(format!(
                "Fixture maxEventBytes 必须位于 1..={MAX_FIXTURE_EVENT_BYTES}"
            ));
        }
        exchange
            .response_shape
            .validate()
            .map_err(|error| format!("Fixture 响应结构证据无效：{error}"))?;
        if exchange.response_shape.protocol != protocol {
            return Err("Fixture 响应结构证据协议与 Payload 协议不一致".to_owned());
        }
        match &exchange.request {
            FixtureRequestEvidence::SyntheticFirstRequest {
                semantic_message_count,
                semantic_tool_count,
                wire_top_level_field_count,
            } => {
                if index > 0 {
                    return Err("Fixture 后续线级请求必须按远端历史策略整体省略".to_owned());
                }
                if *semantic_message_count != 1 || *wire_top_level_field_count == 0 {
                    return Err("Fixture 首请求结构摘要不满足 Harness 不变量".to_owned());
                }
                if *semantic_tool_count > 2 {
                    return Err("Fixture 首请求工具定义数量超过 Harness 上限".to_owned());
                }
            }
            FixtureRequestEvidence::SubsequentRequestOmitted { reason } => {
                if index == 0 {
                    return Err("Fixture 首个线级请求不得省略".to_owned());
                }
                if reason != OMITTED_SUBSEQUENT_REQUEST_REASON {
                    return Err("Fixture 后续请求省略原因不受支持".to_owned());
                }
            }
        }
    }
    Ok(())
}

/// 返回指定交换必须使用的合成标记与能力模板。
fn fixture_request_expectation<'a>(
    capability: &'a str,
    synthetic_marker: &str,
    index: usize,
) -> (String, &'a str) {
    if index == 0 && capability == "multi_turn" {
        (first_turn_marker(synthetic_marker), "text")
    } else if index == 0 && capability == "tool_result_image_round_trip" {
        (
            first_turn_marker(synthetic_marker),
            "tool_result_image_round_trip",
        )
    } else {
        (synthetic_marker.to_owned(), capability)
    }
}

/// 验证 Fixture Payload 使用调用方给定的精确合成标记。
fn validate_fixture_prompts(
    fixture: &ProbeFixtureEnvelope,
    expected_marker: &str,
) -> Result<(), String> {
    if fixture.payload.synthetic_marker.as_deref() != Some(expected_marker) {
        return Err("Fixture Payload 合成标记与稳定键派生标记不一致".to_owned());
    }
    Ok(())
}

/// 严格解析 Fixture，并要求请求中出现的标记与调用方给定标记完全一致。
fn parse_synthetic_fixture(
    text: &str,
    expected_marker: &str,
) -> Result<ProbeFixtureEnvelope, String> {
    let fixture = parse_fixture_envelope(text)?;
    validate_fixture_prompts(&fixture, expected_marker)?;
    validate_fixture_request_binding(&fixture)?;
    Ok(fixture)
}

/// 写入与全目录扫描使用 Payload 自身标记验证请求；恢复路径另行绑定稳定键。
fn validate_synthetic_fixture(text: &str) -> Result<(), String> {
    let fixture = parse_fixture_envelope(text)?;
    let expected_marker = fixture
        .payload
        .synthetic_marker
        .as_deref()
        .ok_or_else(|| "Fixture 缺少精确合成标记".to_owned())?;
    validate_fixture_prompts(&fixture, expected_marker)?;
    validate_fixture_request_binding(&fixture)
}

/// 根据能力契约判断最后一个交换是否具有预期的线级响应种类。
fn fixture_requirement_matches(
    fixture: &ProbeFixtureEnvelope,
    exchange: Option<&FixtureExchange>,
    outcome: Option<&FixtureExchangeOutcome>,
) -> bool {
    match fixture.payload.capability.as_str() {
        "cancellation" => {
            fixture
                .payload
                .expected_cancellation
                .as_ref()
                .is_some_and(|cancellation| {
                    if cancellation.local_future_dropped && !cancellation.completed_before_cancel {
                        matches!(outcome, Some(FixtureExchangeOutcome::RequestOnly))
                    } else if cancellation.completed_before_cancel
                        && !cancellation.local_future_dropped
                    {
                        matches!(outcome, Some(FixtureExchangeOutcome::Response { .. }))
                    } else if !cancellation.local_future_dropped
                        && !cancellation.completed_before_cancel
                    {
                        matches!(
                            outcome,
                            Some(
                                FixtureExchangeOutcome::Error { .. }
                                    | FixtureExchangeOutcome::ObservedTerminalError { .. }
                            )
                        )
                    } else {
                        false
                    }
                })
        }
        "diagnostic_invalid_authentication"
        | "diagnostic_missing_model"
        | "invalid_parameter"
        | "context_overflow" => exchange.is_some_and(|exchange| {
            exchange
                .response_shape
                .http_status
                .is_some_and(|status| !(200..300).contains(&status))
                && matches!(outcome, Some(FixtureExchangeOutcome::Error { .. }))
        }),
        "stream_interruption" => exchange.is_some_and(|exchange| {
            exchange
                .response_shape
                .http_status
                .is_some_and(|status| (200..300).contains(&status))
                && matches!(outcome, Some(FixtureExchangeOutcome::Error { .. }))
        }),
        "tool_result_image_round_trip" if fixture_chat_image_unsupported(fixture) => exchange
            .is_some_and(|exchange| {
                exchange
                    .response_shape
                    .http_status
                    .is_some_and(|status| (200..300).contains(&status))
                    && matches!(
                        outcome,
                        Some(
                            FixtureExchangeOutcome::Response { .. }
                                | FixtureExchangeOutcome::Unavailable { .. }
                        )
                    )
            }),
        _ => matches!(outcome, Some(FixtureExchangeOutcome::Response { .. })),
    }
}

/// 识别 Chat Completions 在本地 Adapter 拒绝图片工具结果的持久化组合。
fn fixture_chat_image_unsupported(fixture: &ProbeFixtureEnvelope) -> bool {
    fixture.payload.capability == "tool_result_image_round_trip"
        && fixture.payload.protocol == "openai_chat_completions"
        && fixture
            .payload
            .expected_error
            .as_ref()
            .is_some_and(|error| {
                error.kind == "unsupported_capability"
                    && !error.retryable
                    && error.http_status.is_none()
            })
}

/// 逐字段比较磁盘可复核的最终结果与 Fixture 保存的 ProbeRecord 终态。
fn fixture_final_outcome_matches(
    fixture: &ProbeFixtureEnvelope,
    outcome: Option<&FixtureExchangeOutcome>,
) -> bool {
    match (
        &fixture.payload.expected_response,
        &fixture.payload.expected_actual_text_evidence,
        &fixture.payload.expected_error,
        outcome,
    ) {
        (
            Some(expected_response),
            Some(expected_text),
            None,
            Some(FixtureExchangeOutcome::Response {
                response,
                actual_text_evidence,
            }),
        ) => expected_response == response && expected_text == actual_text_evidence,
        (Some(_), Some(_), Some(_), Some(FixtureExchangeOutcome::Unavailable { reason }))
            if fixture_chat_image_unsupported(fixture)
                && reason == UNAVAILABLE_RESPONSE_BODY_REASON =>
        {
            true
        }
        (None, None, Some(expected), Some(FixtureExchangeOutcome::Error { error })) => {
            expected == error
        }
        (
            None,
            None,
            Some(expected),
            Some(FixtureExchangeOutcome::ObservedTerminalError { error }),
        ) => expected == error,
        (None, None, None, Some(FixtureExchangeOutcome::RequestOnly))
            if fixture.payload.capability == "cancellation"
                && fixture
                    .payload
                    .expected_cancellation
                    .as_ref()
                    .is_some_and(|cancellation| {
                        cancellation.local_future_dropped && !cancellation.completed_before_cancel
                    }) =>
        {
            true
        }
        (Some(_), _, _, _) | (_, Some(_), _, _) | (_, _, Some(_), _) | (None, None, None, _) => {
            false
        }
    }
}

/// 从磁盘实际重放结果重新计算整条记录的 FixtureReplayEvidence。
fn fixture_replay_evidence(
    fixture: &ProbeFixtureEnvelope,
    outcomes: &[FixtureExchangeOutcome],
) -> FixtureReplayEvidence {
    let exchange_count = outcomes.len();
    let replayed_exchanges = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome,
                FixtureExchangeOutcome::Response { .. } | FixtureExchangeOutcome::Error { .. }
            )
        })
        .count();
    let unavailable_reason = outcomes.iter().find_map(|outcome| match outcome {
        FixtureExchangeOutcome::Unavailable { reason } => Some(reason.clone()),
        FixtureExchangeOutcome::ObservedTerminalError { .. } => {
            Some("线级交换只记录了在线传输终态，磁盘无法独立重放该外部失败".to_owned())
        }
        FixtureExchangeOutcome::Response { .. }
        | FixtureExchangeOutcome::Error { .. }
        | FixtureExchangeOutcome::RequestOnly => None,
    });
    let request_only_exchanges = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, FixtureExchangeOutcome::RequestOnly))
        .count();
    let request_only_final = matches!(outcomes.last(), Some(FixtureExchangeOutcome::RequestOnly));
    let requirement_matches =
        fixture_requirement_matches(fixture, fixture.payload.exchanges.last(), outcomes.last());
    let expected_matches = fixture_final_outcome_matches(fixture, outcomes.last());
    let complete = unavailable_reason.is_none()
        && replayed_exchanges + request_only_exchanges == exchange_count
        && request_only_exchanges == usize::from(request_only_final)
        && requirement_matches
        && expected_matches;
    let reason = unavailable_reason
        .or_else(|| {
            (!requirement_matches)
                .then(|| "最终线级交换没有复现当前能力要求的响应或错误类型".to_owned())
        })
        .or_else(|| {
            (!expected_matches).then(|| {
                "磁盘复核的最终 response、actual_text_evidence 或 normalized_error 与在线记录不一致"
                    .to_owned()
            })
        });
    let unavailable = outcomes.iter().any(|outcome| {
        matches!(
            outcome,
            FixtureExchangeOutcome::Unavailable { .. }
                | FixtureExchangeOutcome::ObservedTerminalError { .. }
        )
    });
    FixtureReplayEvidence {
        status: if complete && request_only_final {
            "not_applicable".to_owned()
        } else if complete {
            "passed".to_owned()
        } else if unavailable {
            "unavailable".to_owned()
        } else {
            "failed".to_owned()
        },
        exchange_count,
        replayed_exchanges,
        reason: reason.or_else(|| {
            request_only_final.then(|| {
                "本地取消计时器获胜并丢弃最后一个在途 Future 或 Stream；此前交换仍已逐一重放"
                    .to_owned()
            })
        }),
    }
}

/// 每次从磁盘 Fixture 重新编码请求并按正文省略策略重算复核状态。
fn verify_disk_fixture(record: &ProbeRecord, fixture: &ProbeFixtureEnvelope) -> Result<(), String> {
    validate_fixture_request_binding(fixture)?;
    let mut outcomes = Vec::with_capacity(fixture.payload.exchanges.len());
    for (index, exchange) in fixture.payload.exchanges.iter().enumerate() {
        let request_only = fixture.payload.capability == "cancellation"
            && index + 1 == fixture.payload.exchanges.len()
            && fixture
                .payload
                .expected_cancellation
                .as_ref()
                .is_some_and(|cancellation| {
                    cancellation.local_future_dropped
                        && !cancellation.completed_before_cancel
                        && fixture.payload.expected_response.is_none()
                        && fixture.payload.expected_actual_text_evidence.is_none()
                        && fixture.payload.expected_error.is_none()
                });
        let outcome = if request_only {
            FixtureExchangeOutcome::RequestOnly
        } else {
            persisted_fixture_replay_outcome(exchange)
        };
        outcomes.push(outcome);
    }
    let replay = fixture_replay_evidence(fixture, &outcomes);
    let final_observed_terminal_error = fixture
        .payload
        .exchanges
        .last()
        .and_then(|exchange| exchange.observed_terminal_error.as_ref());
    if fixture.payload.capability == "cancellation"
        && fixture.payload.expected_error.is_some()
        && replay.status == "unavailable"
        && final_observed_terminal_error != fixture.payload.expected_error.as_ref()
    {
        return Err("取消失败只有显式在线传输终态可以声明为响应不可从磁盘复核".to_owned());
    }
    if fixture.payload.replay.as_ref() != Some(&replay)
        || record.fixture_replay.as_ref() != Some(&replay)
    {
        return Err(
            "Fixture 磁盘重算的 FixtureReplayEvidence 与 Payload 或 ProbeRecord 不一致".to_owned(),
        );
    }
    Ok(())
}

/// 按三种厂商协议的真实字段结构检查纯用户合成提示词，并拒绝远端历史。
fn validate_synthetic_request_body(
    protocol: &str,
    request_body: &serde_json::Value,
    expected_marker: &str,
    capability: &str,
) -> Result<(), String> {
    let allow_first_turn = capability == "multi_turn";
    let prompt_count = match protocol {
        "anthropic_messages" => {
            validate_messages_prompts(request_body, expected_marker, allow_first_turn)?
        }
        "openai_chat_completions" => {
            validate_chat_prompts(request_body, expected_marker, allow_first_turn)?
        }
        "openai_responses" => {
            validate_responses_prompts(request_body, expected_marker, allow_first_turn)?
        }
        _ => {
            return Err(format!(
                "Fixture 包含未知协议 {protocol}，无法验证合成提示词"
            ));
        }
    };
    if prompt_count == 0 {
        return Err("Fixture 请求没有可结构化验证的 Harness 合成提示词".to_owned());
    }
    Ok(())
}

/// 检查 Anthropic Messages 的纯 user 文本并拒绝 assistant 与工具结果历史。
fn validate_messages_prompts(
    body: &serde_json::Value,
    expected_marker: &str,
    allow_first_turn: bool,
) -> Result<usize, String> {
    if body.get("system").is_some() {
        return Err("Harness 不生成 Messages system 提示词".to_owned());
    }
    let messages = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Messages Fixture 缺少 messages 数组".to_owned())?;
    let mut count = 0;
    for message in messages {
        let role = message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Messages Fixture 消息缺少 role".to_owned())?;
        match role {
            "assistant" => {
                return Err("Fixture 首个 Messages 请求不得包含 assistant 远端历史".to_owned());
            }
            "user" => {
                let content = message
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| "Messages user 内容必须是数组".to_owned())?;
                for block in content {
                    match block.get("type").and_then(serde_json::Value::as_str) {
                        Some("text") => {
                            validate_harness_text(
                                json_string(block, "text")?,
                                expected_marker,
                                allow_first_turn,
                            )?;
                            count += 1;
                        }
                        Some("tool_result") => {
                            return Err("Fixture 首个 Messages 请求不得包含工具结果历史".to_owned());
                        }
                        _ => return Err("Messages user 含有 Harness 未生成的内容类型".to_owned()),
                    }
                }
            }
            _ => return Err(format!("Harness 不生成 Messages {role} 角色")),
        }
    }
    Ok(count)
}

/// 检查 OpenAI Chat Completions 的纯 user 文本并拒绝 assistant 与 tool 历史。
fn validate_chat_prompts(
    body: &serde_json::Value,
    expected_marker: &str,
    allow_first_turn: bool,
) -> Result<usize, String> {
    let messages = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Chat Completions Fixture 缺少 messages 数组".to_owned())?;
    let mut count = 0;
    for message in messages {
        let role = message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Chat Completions Fixture 消息缺少 role".to_owned())?;
        match role {
            "assistant" => {
                return Err(
                    "Fixture 首个 Chat Completions 请求不得包含 assistant 远端历史".to_owned(),
                );
            }
            "user" => {
                let content = json_string(message, "content")?;
                validate_harness_text(content, expected_marker, allow_first_turn)?;
                count += 1;
            }
            "tool" => {
                return Err("Fixture 首个 Chat Completions 请求不得包含工具结果历史".to_owned());
            }
            "system" | "developer" => {
                return Err(format!("Harness 不生成 Chat Completions {role} 提示词"));
            }
            _ => return Err(format!("Harness 不生成 Chat Completions {role} 角色")),
        }
    }
    Ok(count)
}

/// 检查 OpenAI Responses 的纯 user input_text 并拒绝远端派生 item。
fn validate_responses_prompts(
    body: &serde_json::Value,
    expected_marker: &str,
    allow_first_turn: bool,
) -> Result<usize, String> {
    let input = body
        .get("input")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Responses Fixture 缺少 input 数组".to_owned())?;
    let mut count = 0;
    for item in input {
        match item.get("type").and_then(serde_json::Value::as_str) {
            Some("message") => {
                let role = item
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Responses message 缺少 role".to_owned())?;
                match role {
                    "assistant" => {
                        return Err(
                            "Fixture 首个 Responses 请求不得包含 assistant 远端历史".to_owned()
                        );
                    }
                    "user" => {
                        let content = item
                            .get("content")
                            .and_then(serde_json::Value::as_array)
                            .ok_or_else(|| "Responses user 内容必须是数组".to_owned())?;
                        for part in content {
                            if part.get("type").and_then(serde_json::Value::as_str)
                                != Some("input_text")
                            {
                                return Err("Harness 不生成非文本 Responses user 输入".to_owned());
                            }
                            validate_harness_text(
                                json_string(part, "text")?,
                                expected_marker,
                                allow_first_turn,
                            )?;
                            count += 1;
                        }
                    }
                    "system" | "developer" => {
                        return Err(format!("Harness 不生成 Responses {role} 提示词"));
                    }
                    _ => return Err(format!("Harness 不生成 Responses {role} 角色")),
                }
            }
            Some("function_call_output") => {
                return Err("Fixture 首个 Responses 请求不得包含工具结果历史".to_owned());
            }
            Some("function_call" | "reasoning") => {
                return Err("Fixture 首个 Responses 请求不得包含远端调用或推理历史".to_owned());
            }
            _ => return Err("Responses input 含有 Harness 未生成的项目类型".to_owned()),
        }
    }
    Ok(count)
}

/// 读取一个必须存在的 JSON 字符串字段。
fn json_string<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("Fixture 字段 {field} 必须是字符串"))
}

/// 只接受 Harness 当前源码中能够生成的完整提示词模板。
fn validate_harness_text(
    text: &str,
    expected_marker: &str,
    allow_first_turn: bool,
) -> Result<(), String> {
    if is_harness_text(text, expected_marker, allow_first_turn) {
        Ok(())
    } else {
        Err("Fixture 提示词不是 Harness 生成的纯合成模板，或使用了错误标记".to_owned())
    }
}

/// 判断文本是否与固定模板及当前稳定键允许的精确标记完全一致。
fn is_harness_text(text: &str, expected_marker: &str, allow_first_turn: bool) -> bool {
    if text == "严格按提供的 JSON Schema 生成唯一结果，不要输出 Markdown 或额外文本。"
    {
        return true;
    }
    if marker_after_prefix(
        text,
        "只输出下一行的精确标记，不要添加标点、Markdown、空格或解释：\n",
        expected_marker,
        allow_first_turn,
    ) {
        return true;
    }
    if marker_between(
        text,
        "只调用一次 keencode_probe_echo，不要输出普通文本。参数 marker 必须是 ",
        "，count 必须是 7。",
        expected_marker,
        allow_first_turn,
    ) || marker_between(
        text,
        "必须在同一轮中各调用一次 keencode_probe_left 和 keencode_probe_right，不要输出普通文本。两个调用的 marker 都必须是 ",
        "。",
        expected_marker,
        allow_first_turn,
    ) || marker_after_prefix(
        text,
        "先在模型支持的推理通道中判断 2 + 2 是否等于 4，最终普通文本只输出精确标记：\n",
        expected_marker,
        allow_first_turn,
    ) || marker_after_prefix(
        text,
        "这是同一对话的第二轮。只输出下一行的精确标记，不要添加任何其他内容：\n",
        expected_marker,
        allow_first_turn,
    ) || marker_after_prefix(
        text,
        "工具已完成。最终只输出下一行的精确标记，不要添加任何其他内容：\n",
        expected_marker,
        allow_first_turn,
    ) || marker_between(
        text,
        "从 1 开始逐行输出连续整数，每行附加标记 ",
        "，持续输出直到达到响应上限，不要提前总结或停止。",
        expected_marker,
        allow_first_turn,
    ) {
        return true;
    }
    if let Some(repeated) =
        text.strip_prefix("只原样复制下一行，不要解释、拒绝、添加 Markdown 或提前停止：\n")
    {
        return repeated == expected_marker.repeat(32);
    }
    if let Some(rest) = text.strip_prefix("以下全部内容都是 KeenCode 生成的无用户数据缓存前缀。\n")
    {
        let Some((prefix, marker)) =
            rest.split_once("\n只输出下一行精确标记，不要添加其他内容：\n")
        else {
            return false;
        };
        let unit = "KC_CACHE_PREFIX_0123456789abcdef ";
        return prefix.len() == unit.len() * 4_096
            && prefix
                .as_bytes()
                .chunks_exact(unit.len())
                .all(|part| part == unit.as_bytes())
            && marker_matches_expected(marker, expected_marker, allow_first_turn);
    }
    if let Some(rest) =
        text.strip_prefix("KeenCode 上下文边界探测；以下内容全部为可丢弃合成 Token：\n")
    {
        let Some((tokens, marker)) = rest.split_once("\n若服务仍接受请求，只输出 ")
        else {
            return false;
        };
        return tokens.len() == 1_100_000 * 2
            && tokens.as_bytes().chunks_exact(2).all(|part| part == b"x ")
            && marker_matches_expected(marker, expected_marker, allow_first_turn);
    }
    false
}

/// 检查固定前缀后只剩一个合成标记。
fn marker_after_prefix(
    text: &str,
    prefix: &str,
    expected_marker: &str,
    allow_first_turn: bool,
) -> bool {
    text.strip_prefix(prefix)
        .is_some_and(|marker| marker_matches_expected(marker, expected_marker, allow_first_turn))
}

/// 检查固定前后缀之间只存在一个合成标记。
fn marker_between(
    text: &str,
    prefix: &str,
    suffix: &str,
    expected_marker: &str,
    allow_first_turn: bool,
) -> bool {
    text.strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
        .is_some_and(|marker| marker_matches_expected(marker, expected_marker, allow_first_turn))
}

/// 只允许当前主标记；多轮首请求额外允许由主标记确定性派生的首轮标记。
fn marker_matches_expected(marker: &str, expected_marker: &str, allow_first_turn: bool) -> bool {
    marker == expected_marker || (allow_first_turn && marker == first_turn_marker(expected_marker))
}

/// 一次完成后真实执行的敏感产物扫描结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RedactionScanReport {
    /// 扫描报告结构版本。
    schema_version: &'static str,
    /// 所有禁止模式是否均为零命中。
    passed: bool,
    /// 完整 Provider 凭据命中的文件数。
    exact_credential_matches: usize,
    /// 常见长 Token 样式的命中数。
    secret_token_matches: usize,
    /// 多星号后仍携带凭据后缀的命中数。
    masked_credential_suffix_matches: usize,
    /// 未脱敏认证 Header 值的命中数。
    authentication_header_matches: usize,
    /// 未脱敏 Cookie Header 或 JSON 值的命中数。
    cookie_matches: usize,
    /// Windows 或用户目录绝对路径的命中数。
    absolute_path_matches: usize,
    /// 除结构化 CR、LF、TAB 外的控制字符与 Unicode 危险显示字符命中数。
    dangerous_display_character_matches: usize,
    /// Fixture 缺少纯合成提示词证明或明确包含非合成提示词的数量。
    non_synthetic_prompt_matches: usize,
    /// 无法按 UTF-8 审计的产物数。
    non_utf8_artifacts: usize,
    /// 实际读取并扫描的相对产物路径。
    scanned_artifacts: Vec<String>,
}

/// 从完成来源严格反序列化的脱敏扫描报告，不允许未知字段藏匿未扫描正文。
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRedactionReport {
    /// 脱敏扫描报告结构版本。
    schema_version: String,
    /// 所有禁止模式是否均为零命中。
    passed: bool,
    /// 完整 Provider 凭据命中的文件数。
    exact_credential_matches: usize,
    /// 常见长 Token 样式的命中数。
    secret_token_matches: usize,
    /// 多星号后仍携带凭据后缀的命中数。
    masked_credential_suffix_matches: usize,
    /// 未脱敏认证 Header 值的命中数。
    authentication_header_matches: usize,
    /// 未脱敏 Cookie Header 或 JSON 值的命中数。
    cookie_matches: usize,
    /// Windows 或用户目录绝对路径的命中数。
    absolute_path_matches: usize,
    /// 除结构化 CR、LF、TAB 外的控制字符与 Unicode 危险显示字符命中数。
    dangerous_display_character_matches: usize,
    /// Fixture 缺少纯合成提示词证明或明确包含非合成提示词的数量。
    non_synthetic_prompt_matches: usize,
    /// 无法按 UTF-8 审计的产物数。
    non_utf8_artifacts: usize,
    /// 实际读取并扫描的相对产物路径。
    scanned_artifacts: Vec<String>,
}

impl From<RedactionScanReport> for StoredRedactionReport {
    /// 消费当前真实扫描结果并转换为可与来源持久化报告逐字段比较的拥有型值。
    fn from(report: RedactionScanReport) -> Self {
        Self {
            schema_version: report.schema_version.to_owned(),
            passed: report.passed,
            exact_credential_matches: report.exact_credential_matches,
            secret_token_matches: report.secret_token_matches,
            masked_credential_suffix_matches: report.masked_credential_suffix_matches,
            authentication_header_matches: report.authentication_header_matches,
            cookie_matches: report.cookie_matches,
            absolute_path_matches: report.absolute_path_matches,
            dangerous_display_character_matches: report.dangerous_display_character_matches,
            non_synthetic_prompt_matches: report.non_synthetic_prompt_matches,
            non_utf8_artifacts: report.non_utf8_artifacts,
            scanned_artifacts: report.scanned_artifacts,
        }
    }
}

/// 一个已通过枚举阶段类型、路径和资源预算校验的待扫描文件。
struct ArtifactFile {
    /// 相对于规范运行根且使用正斜杠的稳定路径。
    relative: String,
    /// 枚举时从元数据读取的文件字节数。
    byte_len: u64,
    /// 枚举时从不跟随重解析点的普通文件句柄取得的稳定对象身份。
    identity: RegularFileIdentity,
}

/// 迭代枚举运行目录普通文件，并在收集前执行路径、数量和总字节预算。
fn collect_artifact_paths(root: &Path) -> Result<Vec<ArtifactFile>, String> {
    let canonical_root = validated_run_root(root)?;
    let mut directories = vec![canonical_root.clone()];
    let mut output = Vec::new();
    let mut entry_count = 0_usize;
    let mut total_bytes = 0_u64;
    while let Some(directory) = directories.pop() {
        let directory_metadata = fs::symlink_metadata(&directory)
            .map_err(|error| format!("无法读取脱敏扫描目录元数据：{error}"))?;
        if is_link_or_reparse(&directory_metadata) || !directory_metadata.is_dir() {
            return Err("脱敏扫描路径包含链接、重解析点或非普通目录".to_owned());
        }
        for entry in
            fs::read_dir(&directory).map_err(|error| format!("无法枚举脱敏扫描目录：{error}"))?
        {
            entry_count = entry_count
                .checked_add(1)
                .ok_or_else(|| "脱敏扫描目录项数量溢出".to_owned())?;
            if entry_count > MAX_ARTIFACT_ENTRY_COUNT {
                return Err(format!(
                    "脱敏扫描目录项超过 {} 个安全上限",
                    MAX_ARTIFACT_ENTRY_COUNT
                ));
            }
            let entry = entry.map_err(|error| format!("无法读取脱敏扫描目录项：{error}"))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("无法读取脱敏扫描文件元数据：{error}"))?;
            if is_link_or_reparse(&metadata) {
                return Err("脱敏扫描目录不允许包含符号链接、目录联接或重解析点".to_owned());
            }
            let canonical = fs::canonicalize(&path)
                .map_err(|error| format!("无法规范化脱敏扫描路径：{error}"))?;
            if !canonical.starts_with(&canonical_root) {
                return Err("脱敏扫描路径的规范结果越过运行目录".to_owned());
            }
            if metadata.is_dir() {
                directories.push(canonical);
                continue;
            }
            if !metadata.is_file() {
                return Err("脱敏扫描目录只能包含普通文件或普通目录".to_owned());
            }
            if metadata.len() > MAX_ARTIFACT_FILE_BYTES {
                return Err(format!(
                    "脱敏扫描单文件超过 {} 字节安全上限",
                    MAX_ARTIFACT_FILE_BYTES
                ));
            }
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| "脱敏扫描文件总字节数溢出".to_owned())?;
            if total_bytes > MAX_ARTIFACT_TOTAL_BYTES {
                return Err(format!(
                    "脱敏扫描文件总字节数超过 {} 字节安全上限",
                    MAX_ARTIFACT_TOTAL_BYTES
                ));
            }
            let relative = path
                .strip_prefix(&canonical_root)
                .map_err(|_| "脱敏扫描文件不在运行目录内".to_owned())?
                .to_str()
                .ok_or_else(|| "脱敏扫描相对路径必须是有效 Unicode".to_owned())?
                .replace('\\', "/");
            if relative == ".keencode-live-test.lock" || relative == "redaction-report.json" {
                continue;
            }
            let (_, byte_len, identity) = open_bounded_regular_file(
                &path,
                MAX_ARTIFACT_FILE_BYTES,
                Some(metadata.len()),
                None,
                &format!("待枚举产物 {relative}"),
            )?;
            output.push(ArtifactFile {
                relative,
                byte_len,
                identity,
            });
        }
    }
    Ok(output)
}

/// 只读验证基础运行与补测运行，并把精确 tuple 替换结果写入全新离线目录。
pub(crate) async fn consolidate_retry_runs(
    base_dir: &Path,
    retry_dir: &Path,
    output_root: &Path,
    providers: &[&ProviderEntry],
    allow_unauthenticated_legacy_base: bool,
) -> Result<PathBuf, String> {
    consolidate_retry_runs_with_hooks(
        base_dir,
        retry_dir,
        output_root,
        providers,
        allow_unauthenticated_legacy_base,
        |_| Ok(()),
        |_, _| Ok(()),
        |_, _| Ok(()),
        |_, _| Ok(()),
    )
    .await
}

/// 使用确定性创建、来源读取和完成后钩执行离线合并，供竞态与来源变化回归验证。
#[allow(clippy::too_many_arguments)]
async fn consolidate_retry_runs_with_hooks<P, B, A, F>(
    base_dir: &Path,
    retry_dir: &Path,
    output_root: &Path,
    providers: &[&ProviderEntry],
    allow_unauthenticated_legacy_base: bool,
    pre_create_hook: P,
    before_source_read_hook: B,
    after_source_read_hook: A,
    post_finalize_hook: F,
) -> Result<PathBuf, String>
where
    P: FnOnce(&Path) -> Result<(), String>,
    B: FnOnce(&ReportStore, &ReportStore) -> Result<(), String>,
    A: FnOnce(&ReportStore, &ReportStore) -> Result<(), String>,
    F: FnOnce(&ReportStore, &ReportStore) -> Result<(), String>,
{
    let retry_store = ReportStore::open_recovery_source(retry_dir)?;
    let retry_manifest = retry_store.load_retry_source_manifest(providers, false)?;
    retry_manifest.validate_completed_retry_identity(providers)?;
    retry_store.load_and_verify_retry_selection_sidecar(&retry_manifest, providers)?;
    let retry_source_snapshot =
        retry_store.completed_source_snapshot(&retry_manifest, providers)?;
    let selection = retry_manifest
        .retry_selection()
        .cloned()
        .ok_or_else(|| "待合并补测运行缺少精确选择清单".to_owned())?;

    let base_store = ReportStore::open_recovery_source(base_dir)?;
    let base_manifest =
        base_store.load_retry_source_manifest(providers, allow_unauthenticated_legacy_base)?;
    let base_source_snapshot = base_store.completed_source_snapshot(&base_manifest, providers)?;
    let rebuilt_selection = base_store
        .create_retry_selection(
            &base_manifest,
            providers,
            &selection.lineage.provider_id,
            selection.lineage.through_sequence,
            &selection.lineage.source_executable_sha256,
        )
        .await?;
    if rebuilt_selection != selection {
        return Err("补测选择不能从当前基础运行按固定策略确定性重建".to_owned());
    }

    let base_reference = source_reference(&base_manifest, &base_source_snapshot)?;
    if base_reference.resume_sha256 != selection.lineage.source_resume_sha256
        || base_reference.journal_sha256 != selection.lineage.source_journal_sha256
        || base_reference.result_sha256 != selection.lineage.source_result_sha256
        || base_reference.redaction_report_sha256
            != selection.lineage.source_redaction_report_sha256
        || base_reference.run_id != selection.lineage.source_run_id
        || base_reference.runtime_commit != selection.lineage.source_runtime_commit
    {
        return Err("基础运行内容摘要或运行身份与补测选择 Lineage 不一致".to_owned());
    }
    let retry_reference = source_reference(&retry_manifest, &retry_source_snapshot)?;

    before_source_read_hook(&base_store, &retry_store)?;
    let base_result_bytes = base_store.read_bounded_run_file(
        Path::new("result.json"),
        MAX_ARTIFACT_FILE_BYTES,
        "基础运行最终报告",
    )?;
    let retry_result_bytes = retry_store.read_bounded_run_file(
        Path::new("result.json"),
        MAX_ARTIFACT_FILE_BYTES,
        "补测运行最终报告",
    )?;
    after_source_read_hook(&base_store, &retry_store)?;
    verify_completed_snapshot_bytes(
        &base_source_snapshot,
        "result.json",
        &base_result_bytes,
        "基础运行最终报告",
    )?;
    verify_completed_snapshot_bytes(
        &retry_source_snapshot,
        "result.json",
        &retry_result_bytes,
        "补测运行最终报告",
    )?;
    let base_report_schema = base_manifest.retry_source_report_schema()?;
    let base_report = validate_stored_run_report(
        &base_result_bytes,
        &base_manifest,
        providers,
        &[base_report_schema],
    )?;
    let retry_report = validate_stored_run_report(
        &retry_result_bytes,
        &retry_manifest,
        providers,
        &[RUN_REPORT_SCHEMA_VERSION],
    )?;
    if !retry_report.catalogs.is_empty()
        || retry_report.probes.len() != selection.cases.len()
        || retry_manifest.record_count() != selection.cases.len()
    {
        return Err("补测运行包含模型目录、额外探测或缺少选择 tuple".to_owned());
    }

    let mut retry_by_source_key = BTreeMap::new();
    for case in &selection.cases {
        let retry_key = retry_case_key(
            &retry_manifest.run.run_id,
            &case.provider_id,
            &case.model,
            &case.protocol,
            &case.response_mode,
            &case.capability,
        );
        let retry_record = retry_manifest
            .records
            .get(&retry_key)
            .ok_or_else(|| "补测运行缺少选择清单中的精确 tuple".to_owned())?;
        if retry_tuple_key(
            &retry_record.provider_id,
            &retry_record.model,
            &retry_record.protocol,
            &retry_record.response_mode,
            &retry_record.capability,
        ) != case.tuple_key
            || retry_by_source_key
                .insert(case.source_stable_key.clone(), retry_record.clone())
                .is_some()
        {
            return Err("补测事实与来源稳定键或 tuple 摘要不一致".to_owned());
        }
    }

    let mut effective_records = Vec::with_capacity(base_report.probes.len());
    let mut consolidated_probes = Vec::with_capacity(base_report.probes.len());
    for base_record in &base_report.probes {
        if let Some(retry_record) = retry_by_source_key.remove(&base_record.stable_key) {
            effective_records.push(retry_record.clone());
            consolidated_probes.push(ConsolidatedProbeRecord {
                artifact_source: "retry",
                source_stable_key: base_record.stable_key.clone(),
                observation_run_id: retry_manifest.run.run_id.clone(),
                record: retry_record,
            });
        } else {
            effective_records.push(base_record.clone());
            consolidated_probes.push(ConsolidatedProbeRecord {
                artifact_source: "base",
                source_stable_key: base_record.stable_key.clone(),
                observation_run_id: base_manifest.run.run_id.clone(),
                record: base_record.clone(),
            });
        }
    }
    if !retry_by_source_key.is_empty() {
        return Err("基础运行缺少补测选择引用的来源稳定键".to_owned());
    }
    collect_unique_probe_records(&effective_records, "离线合并有效结果")?;
    let unique_source_keys = consolidated_probes
        .iter()
        .map(|probe| probe.source_stable_key.as_str())
        .collect::<BTreeSet<_>>();
    if unique_source_keys.len() != consolidated_probes.len() {
        return Err("离线合并结果包含重复来源稳定键".to_owned());
    }

    let provider_records = provider_records_for_manifest(&base_manifest, providers)?;
    let effective_summary = SummaryRecord::from_probes(&effective_records);
    let consolidated = ConsolidatedRunReport {
        schema_version: CONSOLIDATED_REPORT_SCHEMA_VERSION,
        generated_at: timestamp()?,
        base: base_reference,
        retry: retry_reference,
        selection: selection.clone(),
        providers: provider_records.clone(),
        catalogs: base_report.catalogs.clone(),
        probes: consolidated_probes,
        summary: effective_summary,
    };
    let mut markdown_report = RunReport {
        schema_version: RUN_REPORT_SCHEMA_VERSION,
        run: base_report.run,
        providers: provider_records,
        catalogs: base_report.catalogs,
        probes: effective_records,
        summary: SummaryRecord::default(),
    };
    markdown_report.refresh_summary();

    if base_store.completed_source_snapshot(&base_manifest, providers)? != base_source_snapshot
        || retry_store.completed_source_snapshot(&retry_manifest, providers)?
            != retry_source_snapshot
    {
        return Err("离线合并来源在读取与目标创建前发生变化".to_owned());
    }
    let run_id = new_run_id()?.replacen("live-", "consolidated-", 1);
    let destination = create_verified_derived_target(
        &[base_store.run_dir(), retry_store.run_dir()],
        output_root,
        &run_id,
        pre_create_hook,
    )?;
    destination
        .finalize_consolidated(&consolidated, &markdown_report, providers)
        .map_err(retained_recovery_target_error)?;
    post_finalize_hook(&base_store, &retry_store).map_err(retained_recovery_target_error)?;
    if base_store
        .completed_source_snapshot(&base_manifest, providers)
        .map_err(retained_recovery_target_error)?
        != base_source_snapshot
        || retry_store
            .completed_source_snapshot(&retry_manifest, providers)
            .map_err(retained_recovery_target_error)?
            != retry_source_snapshot
    {
        return Err(retained_recovery_target_error(
            "离线合并期间只读来源内容发生变化".to_owned(),
        ));
    }
    destination
        .clear_recovery_incomplete_marker()
        .map_err(retained_recovery_target_error)?;
    Ok(destination.run_dir().to_path_buf())
}

/// 从恢复清单声明的 Provider 集合按配置顺序重建安全报告快照。
fn provider_records_for_manifest(
    manifest: &ResumeManifest,
    providers: &[&ProviderEntry],
) -> Result<Vec<ProviderRecord>, String> {
    providers
        .iter()
        .filter(|provider| {
            manifest
                .identity
                .providers
                .iter()
                .any(|identity| identity.provider_id == provider.redact_text(&provider.id))
        })
        .map(|provider| ProviderRecord::from_provider(provider))
        .collect()
}

/// 从已经校验的完成来源快照读取指定固定路径的摘要。
fn completed_snapshot_digest<'a>(
    snapshot: &'a BTreeMap<String, String>,
    relative: &str,
    label: &str,
) -> Result<&'a str, String> {
    snapshot
        .get(relative)
        .map(String::as_str)
        .ok_or_else(|| format!("已经校验的完成来源快照缺少{label}：{relative}"))
}

/// 核对随后实际消费的文件字节仍与此前冻结的完成来源快照完全一致。
fn verify_completed_snapshot_bytes(
    snapshot: &BTreeMap<String, String>,
    relative: &str,
    bytes: &[u8],
    label: &str,
) -> Result<(), String> {
    let expected = completed_snapshot_digest(snapshot, relative, label)?;
    if sha256_digest(bytes) != expected {
        return Err(format!("{label}实际消费字节与已经校验的来源快照不一致"));
    }
    Ok(())
}

/// 从已经校验的完成来源快照提取四个权威摘要并生成不含路径的内容引用。
fn source_reference(
    manifest: &ResumeManifest,
    snapshot: &BTreeMap<String, String>,
) -> Result<ConsolidatedSourceReference, String> {
    let digest = |relative: &str, label: &str| {
        completed_snapshot_digest(snapshot, relative, label).map(str::to_owned)
    };
    Ok(ConsolidatedSourceReference {
        run_id: manifest.run.run_id.clone(),
        runtime_commit: manifest.run.runtime_commit.clone(),
        authentication: if manifest.identity.schema_version == RETRY_SOURCE_RESUME_SCHEMA_VERSION {
            LEGACY_UNAUTHENTICATED_SOURCE_LEVEL.to_owned()
        } else {
            AUTHENTICATED_SOURCE_LEVEL.to_owned()
        },
        resume_schema_version: manifest.identity.schema_version.clone(),
        harness_contract_id: manifest.identity.harness_contract_id.clone(),
        report_schema_version: manifest.retry_source_report_schema()?.to_owned(),
        resume_sha256: digest("resume.json", "恢复清单")?,
        journal_sha256: digest("sanitized-logs/progress.jsonl", "提交日志")?,
        result_sha256: digest("result.json", "最终报告")?,
        redaction_report_sha256: digest("redaction-report.json", "脱敏报告")?,
    })
}

/// 生成无需外部 UUID 依赖的单机唯一运行标识。
pub(crate) fn new_run_id() -> Result<String, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("系统时间早于 Unix 纪元：{error}"))?
        .as_millis();
    Ok(format!("live-{millis}-{}", std::process::id()))
}

/// 返回当前 UTC RFC3339 时间。
pub(crate) fn timestamp() -> Result<String, String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| format!("无法格式化 UTC 时间：{error}"))
}

/// 把 Provider 报告的可选 Usage 加入统计，缺失字段不伪造为上报值。
fn add_usage(summary: &mut SummaryRecord, usage: &TokenUsage) {
    if let Some(value) = usage.input_tokens {
        summary.reported_input_tokens = summary.reported_input_tokens.saturating_add(value);
    }
    if let Some(value) = usage.output_tokens {
        summary.reported_output_tokens = summary.reported_output_tokens.saturating_add(value);
    }
}

/// 在任何文件创建前拒绝完整凭据、通用秘密、认证字段、绝对路径和其他禁止模式。
fn ensure_safe_artifact(text: &str, providers: &[&ProviderEntry]) -> Result<(), String> {
    let exact_credential_matches = providers
        .iter()
        .map(|provider| provider.output_credential_match_count(text))
        .sum::<usize>();
    if exact_credential_matches > 0 {
        return Err("脱敏检查失败：候选产物包含完整 Provider 凭据".to_owned());
    }
    if contains_masked_credential_suffix(text) {
        return Err("脱敏检查失败：候选产物包含授权凭据的掩码后缀".to_owned());
    }
    if count_secret_tokens(text) > 0 {
        return Err("脱敏检查失败：候选产物包含通用秘密 Token 样式".to_owned());
    }
    if count_sensitive_assignments(text, AUTHENTICATION_FIELD_NAMES) > 0 {
        return Err("脱敏检查失败：候选产物包含未脱敏认证字段".to_owned());
    }
    if count_sensitive_assignments(text, COOKIE_FIELD_NAMES) > 0 {
        return Err("脱敏检查失败：候选产物包含未脱敏 Cookie 字段".to_owned());
    }
    if count_artifact_dangerous_display_characters(text) > 0 {
        return Err("显示安全检查失败：候选产物包含控制字符或 Unicode 方向格式字符".to_owned());
    }
    let (drive_signatures, extended_paths, user_paths) = artifact_absolute_path_evidence(text);
    let drive_paths = drive_signatures.values().sum::<usize>();
    if drive_paths + extended_paths + user_paths > 0 {
        return Err(format!(
            "隐私检查失败：候选产物包含绝对路径模式（盘符 {drive_paths} {drive_signatures:?}、扩展路径 {extended_paths}、用户目录 {user_paths}）"
        ));
    }
    Ok(())
}

/// 检测常见的多星号授权凭据后缀而不保留匹配正文。
fn contains_masked_credential_suffix(text: &str) -> bool {
    count_masked_credential_suffixes(text) > 0
}

/// 统计常见的多星号授权凭据后缀而不保留匹配正文。
fn count_masked_credential_suffixes(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut matches = 0;
    while index < bytes.len() {
        if bytes[index] != b'*' {
            index += 1;
            continue;
        }
        let stars_start = index;
        while index < bytes.len() && bytes[index] == b'*' {
            index += 1;
        }
        if index - stars_start < 3 {
            continue;
        }
        let suffix_start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'-'))
        {
            index += 1;
        }
        if index - suffix_start >= 3 {
            matches += 1;
        }
    }
    matches
}

/// 统计形似长 `sk-` 凭据的连续 ASCII Token。
fn count_secret_tokens(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut matches = 0;
    while index + 3 <= bytes.len() {
        if bytes[index..].starts_with(b"sk-") {
            let start = index;
            index += 3;
            while bytes.get(index).is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.')
            }) {
                index += 1;
            }
            if index - start >= 11 {
                matches += 1;
            }
            continue;
        }
        index += 1;
    }
    matches
}

/// 统计原始 Header、JSON 字段、重复 JSON 键及内嵌结构中的认证与 Cookie 值。
fn count_sensitive_assignments(text: &str, fields: &[&str]) -> usize {
    count_sensitive_assignments_at_depth(text, fields, 0)
}

/// 每层同时检查原始文本与 JSON 解码字符串，取较大值避免同一 JSON 赋值重复计数。
fn count_sensitive_assignments_at_depth(text: &str, fields: &[&str], depth: usize) -> usize {
    let lower = text.to_ascii_lowercase();
    let raw_matches = fields
        .iter()
        .map(|field| count_sensitive_field_assignments(&lower, field.trim_end_matches(':')))
        .sum::<usize>();
    if depth >= MAX_EMBEDDED_PATH_SCAN_DEPTH {
        return raw_matches;
    }
    raw_matches.max(count_json_sensitive_assignments(text, fields, depth + 1))
}

/// 遍历一个字段的全部赋值位置，允许 JSON 空白与转义但不允许首个安全值遮蔽后续明文。
fn count_sensitive_field_assignments(lower: &str, field: &str) -> usize {
    let bytes = lower.as_bytes();
    let field_text = field;
    let field = field_text.as_bytes();
    let mut offset = 0_usize;
    let mut matches = 0_usize;
    while offset + field.len() <= bytes.len() {
        let Some(relative) = lower[offset..].find(field_text) else {
            break;
        };
        let start = offset + relative;
        let end = start + field.len();
        let boundary_before =
            start == 0 || !matches!(bytes[start - 1], b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-');
        let boundary_after =
            end == bytes.len() || !matches!(bytes[end], b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-');
        offset = end.max(start + 1);
        if !boundary_before || !boundary_after {
            continue;
        }
        let mut index = end;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'\\' | b'"' | b'\''))
        {
            index += 1;
        }
        if !bytes
            .get(index)
            .is_some_and(|byte| matches!(*byte, b':' | b'='))
        {
            continue;
        }
        index += 1;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'\\' | b'"' | b'\''))
        {
            index += 1;
        }
        let value = &lower[index..];
        if value.is_empty() || starts_with_exact_redaction_placeholder(value) {
            continue;
        }
        matches += 1;
    }
    matches
}

/// 只把完整占位符视为安全值，拒绝 `[REDACTED]opaque-secret` 一类后缀夹带。
fn starts_with_exact_redaction_placeholder(value: &str) -> bool {
    ["[redacted]", "bearer [redacted]", "basic [redacted]"]
        .iter()
        .any(|placeholder| {
            value
                .strip_prefix(placeholder)
                .is_some_and(redaction_placeholder_has_safe_terminator)
        })
}

/// 占位符后只允许明确的字段终止边界，空格、Tab 或转义引号不能遮蔽后缀正文。
fn redaction_placeholder_has_safe_terminator(remainder: &str) -> bool {
    if remainder.is_empty() {
        return true;
    }
    let mut tail = remainder;
    if let Some(stripped) = tail
        .strip_prefix("\\\"")
        .or_else(|| tail.strip_prefix("\\'"))
    {
        tail = stripped;
    } else if tail.starts_with('\"') || tail.starts_with('\'') {
        tail = &tail[1..];
    }
    let tail = tail.trim_start_matches([' ', '\t']);
    tail.is_empty()
        || tail
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(*byte, b',' | b'}' | b']' | b';' | b'\r' | b'\n'))
}

/// 无损遍历 JSON 字符串 Token，保留重复键并识别 Unicode 转义后的敏感字段名。
fn count_json_sensitive_assignments(text: &str, fields: &[&str], depth: usize) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0_usize;
    let mut matches = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'\"' {
            index += 1;
            continue;
        }
        let Some((decoded, end)) = decode_json_string_at(text, index) else {
            index += 1;
            continue;
        };
        matches = matches.saturating_add(count_sensitive_assignments_at_depth(
            &decoded, fields, depth,
        ));

        let mut after = end;
        while bytes.get(after).is_some_and(u8::is_ascii_whitespace) {
            after += 1;
        }
        if bytes.get(after) == Some(&b':')
            && fields
                .iter()
                .any(|field| decoded.eq_ignore_ascii_case(field.trim_end_matches(':')))
        {
            after += 1;
            while bytes.get(after).is_some_and(u8::is_ascii_whitespace) {
                after += 1;
            }
            let redacted = if bytes.get(after) == Some(&b'\"') {
                decode_json_string_at(text, after).is_some_and(|(value, _)| {
                    matches!(
                        value.to_ascii_lowercase().as_str(),
                        "[redacted]" | "bearer [redacted]" | "basic [redacted]"
                    )
                })
            } else {
                false
            };
            if !redacted {
                matches = matches.saturating_add(1);
            }
        }
        index = end;
    }
    matches
}

/// 从指定双引号位置解码一个完整 JSON 字符串并返回结束字节下标。
fn decode_json_string_at(text: &str, start: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'\"') {
        return None;
    }
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'\"' {
            let end = index + 1;
            let decoded = serde_json::from_str::<String>(&text[start..end]).ok()?;
            return Some((decoded, end));
        }
        index += 1;
    }
    None
}

/// 统计原始产物及内嵌 JSON 字符串解码后的控制字符和 Unicode 危险显示字符。
fn count_artifact_dangerous_display_characters(text: &str) -> usize {
    count_dangerous_display_characters_at_depth(text, 0)
}

/// 有界递归扫描 JSON 字符串，取原始层和解码层较大值以免重复计算同一字符。
fn count_dangerous_display_characters_at_depth(text: &str, depth: usize) -> usize {
    let raw_matches = text
        .chars()
        .filter(|character| is_dangerous_display_character(*character))
        .count();
    if depth >= MAX_EMBEDDED_PATH_SCAN_DEPTH {
        return raw_matches;
    }
    let bytes = text.as_bytes();
    let mut index = 0_usize;
    let mut decoded_matches = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'\"' {
            index += 1;
            continue;
        }
        let Some((decoded, end)) = decode_json_string_at(text, index) else {
            index += 1;
            continue;
        };
        decoded_matches = decoded_matches.saturating_add(
            count_dangerous_display_characters_at_depth(&decoded, depth + 1),
        );
        index = end;
    }
    raw_matches.max(decoded_matches)
}

/// 统计 Windows 盘符、扩展路径和常见用户目录绝对路径。
#[cfg(test)]
fn count_absolute_paths(text: &str) -> usize {
    let (drive_paths, extended_paths, user_paths) = absolute_path_counts(text);
    drive_paths + extended_paths + user_paths
}

/// 按产物的真实数据结构统计绝对路径，避免把 JSON 转义字母误当盘符。
fn count_artifact_absolute_paths(text: &str) -> usize {
    let (drive_signatures, extended_paths, user_paths) = artifact_absolute_path_evidence(text);
    drive_signatures.values().sum::<usize>() + extended_paths + user_paths
}

/// 每层先扫描原始文本，再递归扫描 JSON、JSONL 和 SSE 解码后的键与字符串值。
fn artifact_absolute_path_evidence(text: &str) -> (BTreeMap<String, usize>, usize, usize) {
    let mut evidence = (BTreeMap::new(), 0_usize, 0_usize);
    scan_artifact_text(text, 0, &mut evidence);
    evidence
}

/// 在有界深度内同时保留原始层证据并继续解码，避免重复键或转义键遮蔽路径。
fn scan_artifact_text(
    text: &str,
    depth: usize,
    evidence: &mut (BTreeMap<String, usize>, usize, usize),
) {
    merge_absolute_path_evidence(evidence, text);
    if depth < MAX_EMBEDDED_PATH_SCAN_DEPTH {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            scan_artifact_json_value(&value, depth + 1, evidence);
            return;
        }
        if scan_artifact_json_lines(text, depth + 1, evidence) {
            return;
        }
        scan_artifact_sse(text, depth + 1, evidence);
    }
}

/// 递归扫描 JSON 对象键和字符串值，忽略不可携带路径的标量。
fn scan_artifact_json_value(
    value: &serde_json::Value,
    depth: usize,
    evidence: &mut (BTreeMap<String, usize>, usize, usize),
) {
    match value {
        serde_json::Value::String(value) => scan_artifact_text(value, depth, evidence),
        serde_json::Value::Array(values) => {
            for value in values {
                scan_artifact_json_value(value, depth, evidence);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                scan_artifact_text(key, depth, evidence);
                scan_artifact_json_value(value, depth, evidence);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// 仅在至少有两行且每个非空行都是 JSON 时按 JSONL 扫描。
fn scan_artifact_json_lines(
    text: &str,
    depth: usize,
    evidence: &mut (BTreeMap<String, usize>, usize, usize),
) -> bool {
    let parsed = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>();
    let Ok(values) = parsed else {
        return false;
    };
    if values.len() < 2 {
        return false;
    }
    for value in &values {
        scan_artifact_json_value(value, depth, evidence);
    }
    true
}

/// 按 SSE 事件规则合并 `data` 行，其他字段值按普通文本扫描。
fn scan_artifact_sse(
    text: &str,
    depth: usize,
    evidence: &mut (BTreeMap<String, usize>, usize, usize),
) -> bool {
    if !text.lines().any(|line| line.starts_with("data:")) {
        return false;
    }
    let normalized = text.replace("\r\n", "\n");
    for block in normalized.split("\n\n") {
        if block.is_empty() {
            continue;
        }
        let mut data = Vec::new();
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.strip_prefix(' ').unwrap_or(value));
            } else if let Some(comment) = line.strip_prefix(':') {
                scan_artifact_text(
                    comment.strip_prefix(' ').unwrap_or(comment),
                    depth,
                    evidence,
                );
            } else if let Some((_, value)) = line.split_once(':') {
                scan_artifact_text(value.strip_prefix(' ').unwrap_or(value), depth, evidence);
            } else {
                scan_artifact_text(line, depth, evidence);
            }
        }
        if !data.is_empty() {
            scan_artifact_text(&data.join("\n"), depth, evidence);
        }
    }
    true
}

/// 把当前已解码文本的路径证据累加到整个产物的证据中。
fn merge_absolute_path_evidence(
    evidence: &mut (BTreeMap<String, usize>, usize, usize),
    text: &str,
) {
    for (signature, count) in drive_path_signatures(text) {
        *evidence.0.entry(signature).or_insert(0) += count;
    }
    let (_, extended_paths, user_paths) = absolute_path_counts(text);
    evidence.1 += extended_paths;
    evidence.2 += user_paths;
}

/// 分别统计 Windows 盘符、扩展路径和常见用户目录绝对路径，错误中只暴露计数。
fn absolute_path_counts(text: &str) -> (usize, usize, usize) {
    let drive_paths = drive_path_signatures(text).values().sum::<usize>();
    let extended_paths =
        text.matches("\\\\?\\").count() + text.matches("\\\\.\\").count() + count_unc_paths(text);
    let user_paths = [
        "/home/",
        "/root/",
        "/Users/",
        "/private/",
        "/tmp/",
        "/var/folders/",
    ]
    .iter()
    .map(|prefix| text.matches(prefix).count())
    .sum::<usize>();
    (drive_paths, extended_paths, user_paths)
}

/// 统计 `\\server\share\...` 形式且不属于扩展路径或设备路径的 Windows UNC 路径。
fn count_unc_paths(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut matches = 0_usize;
    let mut index = 0_usize;
    while index + 4 < bytes.len() {
        if (index > 0 && bytes[index - 1] == b'\\')
            || bytes[index] != b'\\'
            || bytes[index + 1] != b'\\'
            || matches!(bytes[index + 2], b'?' | b'.' | b'\\' | b'/' | b' ' | b'\t')
        {
            index += 1;
            continue;
        }
        let server_end = bytes[index + 2..]
            .iter()
            .position(|byte| *byte == b'\\')
            .map(|relative| index + 2 + relative);
        let Some(server_end) = server_end else {
            break;
        };
        let share_start = server_end + 1;
        let share_len = bytes[share_start..]
            .iter()
            .take_while(|byte| !matches!(**byte, b'\\' | b'/' | b' ' | b'\t' | b'\r' | b'\n'))
            .count();
        if server_end > index + 2 && share_len > 0 {
            matches += 1;
            index = share_start + share_len;
        } else {
            index += 1;
        }
    }
    matches
}

/// 只保留疑似盘符的三个 ASCII 字符及计数，便于在不泄露路径正文时诊断误报。
fn drive_path_signatures(text: &str) -> BTreeMap<String, usize> {
    let bytes = text.as_bytes();
    let mut signatures = BTreeMap::new();
    for index in 0..bytes.len().saturating_sub(2) {
        let starts_new_token =
            index == 0 || (!bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'\\');
        if starts_new_token
            && bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && matches!(bytes[index + 2], b'\\' | b'/')
        {
            let signature = String::from_utf8_lossy(&bytes[index..=index + 2]).into_owned();
            *signatures.entry(signature).or_insert(0) += 1;
        }
    }
    signatures
}

/// 从事实记录生成 Provider、模型、协议和能力四维兼容矩阵。
fn compatibility_matrix(report: &RunReport) -> String {
    let mut rows = BTreeMap::<(String, String, String, String), BTreeMap<String, String>>::new();
    let mut local_rows =
        BTreeMap::<(String, String, String), BTreeMap<String, BTreeMap<String, usize>>>::new();
    for probe in &report.probes {
        if is_local_conformance(&probe.capability) {
            *local_rows
                .entry((
                    capability_scope(&probe.capability).to_owned(),
                    probe.protocol.clone(),
                    probe.capability.clone(),
                ))
                .or_default()
                .entry(probe.response_mode.clone())
                .or_default()
                .entry(matrix_probe_status(probe))
                .or_default() += 1;
            continue;
        }
        rows.entry((
            probe.provider_id.clone(),
            probe.model.clone(),
            probe.protocol.clone(),
            probe.capability.clone(),
        ))
        .or_default()
        .insert(probe.response_mode.clone(), matrix_probe_status(probe));
    }

    let mut output = String::from(
        "# Provider 三协议真实兼容矩阵\n\n| Provider | 模型 | 协议 | 能力 | 非流式 | 流式 |\n|---|---|---|---|---|---|\n",
    );
    for ((provider, model, protocol, capability), modes) in rows {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            markdown_cell(&provider),
            markdown_cell(&model),
            markdown_cell(&protocol),
            markdown_cell(&capability),
            modes.get("buffered").map_or("未执行", String::as_str),
            modes.get("streaming").map_or("未执行", String::as_str),
        ));
    }
    if !local_rows.is_empty() {
        output.push_str(
            "\n## 本地 Client/Adapter Conformance\n\n以下结果是 local-only 运行时事实，不计入 Provider、模型或远端协议兼容率。\n\n| 范围 | 协议 | 能力 | 非流式 | 流式 | 证据边界 |\n|---|---|---|---|---|---|\n",
        );
        for ((scope, protocol, capability), modes) in local_rows {
            output.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                markdown_cell(&scope),
                markdown_cell(&protocol),
                markdown_cell(&capability),
                local_status_counts(modes.get("buffered")),
                local_status_counts(modes.get("streaming")),
                markdown_cell(local_conformance_boundary(&capability)),
            ));
        }
    }
    output
}

/// 把重复执行的 local-only 状态压缩为不关联 Provider/model 的计数摘要。
fn local_status_counts(statuses: Option<&BTreeMap<String, usize>>) -> String {
    statuses.map_or_else(
        || "未执行".to_owned(),
        |statuses| {
            statuses
                .iter()
                .map(|(status, count)| format!("{}×{count}", markdown_cell(status)))
                .collect::<Vec<_>>()
                .join("、")
        },
    )
}

/// 返回 local-only 能力不能外推到远端 Provider 的固定证据边界。
fn local_conformance_boundary(capability: &str) -> &'static str {
    match capability {
        "stream_interruption" => "仅验证本地回环 2xx 截断流与 Adapter；未请求目标 Provider",
        "cancellation" => "仅证明本地 Future/Stream 被丢弃；未证明远端停止生成、停止计费或支持取消",
        _ => "未知 local-only 能力不得外推为 Provider 兼容结论",
    }
}

/// 把跳过记录渲染为同时包含未验证原因的矩阵状态。
fn matrix_probe_status(probe: &ProbeRecord) -> String {
    if probe.status != "skipped" {
        return probe.status.clone();
    }
    probe
        .skip_evidence
        .as_ref()
        .map(|evidence| format!("skipped ({}:{})", evidence.verification, evidence.reason))
        .unwrap_or_else(|| "skipped (unverified:missing_skip_evidence)".to_owned())
}

/// 从汇总值生成便于人工阅读的简短报告。
fn summary_markdown(report: &RunReport) -> String {
    let mut output = format!(
        "# Provider 真实兼容性测试汇总\n\n- 全部事实记录：{}\n- 运行标识：`{}`\n- Provider 远端兼容案例：{}\n- Provider 远端实际执行：{}\n- Provider 远端通过：{}\n- Provider 远端契约不符合：{}\n- Provider 远端失败：{}\n- Provider 远端跳过：{}\n- Provider 远端未验证：{}\n- Local-only Conformance 案例：{}\n- Local-only Conformance 实际执行：{}\n- Local-only Conformance 通过：{}\n- Local-only Conformance 契约不符合：{}\n- Local-only Conformance 失败：{}\n- Local-only Conformance 跳过：{}\n- Local-only Conformance 未验证：{}\n- 目标 Provider 远端请求尝试：{}\n- Harness 本地回环请求尝试：{}\n- Provider 报告输入 Token 合计：{}\n- Provider 报告输出 Token 合计：{}\n\n## Local-only 证据边界\n\n- `stream_interruption` 是 `adapter_conformance_local_only`：仅请求 Harness 本地回环服务，不代表任何 Provider 或模型支持断流恢复。\n- `cancellation` 是 `client_conformance_local_only`：通过仅证明本地 Future/Stream 已丢弃；`remoteTerminationProven=false`，不证明远端停止生成、停止计费或支持取消。\n\n## 恢复与 exactly-once 边界\n\n- Fixture 与追加提交日志完成本地同步后，即使恢复清单尚未更新，冷恢复也会复用该确定性结果。\n- 如果远端响应已经完成，但进程在 Fixture 或提交日志完成本地同步前终止，本地没有足够证据证明结果已提交；三种厂商协议也没有统一的幂等请求键，因此恢复时可能重新发送该请求。此窗口无法由客户端完全消除。\n",
        report.summary.total_probes,
        report.run.run_id,
        report.summary.provider_compatibility_probes,
        report.summary.executed_probes,
        report.summary.passed,
        report.summary.contract_violations,
        report.summary.failed,
        report.summary.skipped,
        report.summary.unverified,
        report.summary.local_conformance.total,
        report.summary.local_conformance.executed,
        report.summary.local_conformance.passed,
        report.summary.local_conformance.contract_violations,
        report.summary.local_conformance.failed,
        report.summary.local_conformance.skipped,
        report.summary.local_conformance.unverified,
        report.summary.total_attempts,
        report.summary.local_loopback_attempts,
        report.summary.reported_input_tokens,
        report.summary.reported_output_tokens,
    );
    if let Some(lineage) = &report.run.recovery_lineage {
        let lineage_depth =
            std::iter::successors(Some(lineage), |current| current.parent.as_deref()).count();
        output.push_str(&format!(
            "\n## 隔离恢复来源\n\n- 当前运行由新构建继续执行，已导入记录不会重新发送请求。\n- 来源运行标识：{}\n- 来源 Runtime Commit：{}\n- 来源可执行文件 SHA-256：{}\n- 恢复构建可执行文件 SHA-256：{}\n- 来源 Resume SHA-256：{}\n- 来源 Journal SHA-256：{}\n- 来源 Resume Schema：{}\n- 来源 Harness 契约：{}\n- 恢复来源链层数：{}\n- 导入记录：{}\n- 导入 Fixture：{}\n- 隔离升级重新请求记录：{}\n- 恢复策略：{}\n",
            markdown_cell(&lineage.source_run_id),
            markdown_cell(&lineage.source_runtime_commit),
            markdown_cell(&lineage.source_executable_sha256),
            markdown_cell(&lineage.recovery_executable_sha256),
            markdown_cell(&lineage.source_resume_sha256),
            markdown_cell(&lineage.source_journal_sha256),
            markdown_cell(
                lineage
                    .source_resume_schema_version
                    .as_deref()
                    .unwrap_or("legacy-unrecorded"),
            ),
            markdown_cell(
                lineage
                    .source_harness_contract_id
                    .as_deref()
                    .unwrap_or("legacy-unrecorded"),
            ),
            lineage_depth,
            lineage.imported_records,
            lineage.imported_fixtures,
            lineage.rerun_records.len(),
            markdown_cell(&lineage.policy),
        ));
    }
    output.push_str(
        "\n## 分能力统计\n\n| 验证范围 | 能力 | 总数 | 实际执行 | 通过 | 契约不符合 | 失败 | 跳过 | 未验证 |\n|---|---|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for (capability, summary) in &report.summary.by_capability {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            markdown_cell(&summary.scope),
            markdown_cell(capability),
            summary.total,
            summary.executed,
            summary.passed,
            summary.contract_violations,
            summary.failed,
            summary.skipped,
            summary.unverified,
        ));
    }
    output
}

/// 返回结束原因的稳定报告名称。
fn stop_reason_name(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::Completed => "completed",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxOutputTokens => "max_output_tokens",
        StopReason::ContentFilter => "content_filter",
        StopReason::Cancelled => "cancelled",
        StopReason::Other { .. } => "other",
    }
}

/// 把不可信表格值编码为只呈现纯文本的 Markdown，阻断链接、图片与原始 HTML 注入。
fn markdown_cell(value: &str) -> String {
    use std::fmt::Write as _;

    let value = escape_untrusted_inline_text(&value.replace(['\r', '\n'], " "));
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_punctuation() {
            write!(&mut escaped, "&#{};", u32::from(character)).expect("写入 String 不会失败");
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProbeKind;
    use crate::probe::normalize_error;
    use std::collections::BTreeSet;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;

    /// 在 Unix 测试环境创建目录符号链接。
    #[cfg(unix)]
    fn create_directory_link(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("测试目录符号链接应可创建");
    }

    /// 在 Windows 测试环境创建无需开发者模式权限的目录联接。
    #[cfg(windows)]
    fn create_directory_link(target: &Path, link: &Path) {
        let output = Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .expect("应能执行 mklink /J");
        assert!(
            output.status.success(),
            "创建测试目录联接失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// 删除测试创建的目录链接本身而不影响目标目录。
    #[cfg(any(unix, windows))]
    fn remove_directory_link(link: &Path) {
        #[cfg(unix)]
        fs::remove_file(link).expect("应能删除测试目录符号链接");
        #[cfg(windows)]
        fs::remove_dir(link).expect("应能删除测试目录联接");
    }

    /// 创建只包含合成凭据的报告测试 Provider。
    fn provider() -> ProviderEntry {
        serde_json::from_value(serde_json::json!({
            "id": "provider",
            "name": "测试",
            "baseUrl": "https://example.com/v1",
            "models": ["model"],
            "apiBackend": "responses",
            "apiKey": "fixture-secret-value"
        }))
        .expect("报告测试 Provider 应可解析")
    }

    /// 验证固定缓冲区摘要与内存摘要一致，并在读取第一个越界字节时立即拒绝。
    #[test]
    fn sha256_digest_reader_分块摘要一致且拒绝越界() {
        let bytes = (0..200_000_u32)
            .map(|index| u8::try_from(index % 251).expect("余数始终能表示为字节"))
            .collect::<Vec<_>>();
        let (digest, byte_len) = sha256_digest_reader(
            std::io::Cursor::new(&bytes),
            u64::try_from(bytes.len()).expect("测试字节长度应能表示为 u64"),
            "测试分块字节",
        )
        .expect("合法分块字节应能完成摘要");
        assert_eq!(digest, sha256_digest(&bytes));
        assert_eq!(
            byte_len,
            u64::try_from(bytes.len()).expect("测试字节长度应能表示为 u64")
        );
        let error = sha256_digest_reader(
            std::io::Cursor::new(&bytes),
            u64::try_from(bytes.len() - 1).expect("测试越界上限应能表示为 u64"),
            "测试越界字节",
        )
        .expect_err("读取到上限后的第一个字节时必须拒绝");
        assert!(error.contains("在读取期间超过"));
    }

    /// 使用纯合成字节创建一份固定、非敏感的测试响应结构证据。
    fn test_response_shape(
        protocol: ProviderProtocol,
        status: Option<u16>,
        content_type: Option<&str>,
        body: &[u8],
        eof_observed: bool,
        capture_truncated: bool,
    ) -> WireResponseShapeEvidence {
        inspect_wire_response_shape(
            protocol,
            status,
            content_type,
            body,
            eof_observed,
            capture_truncated,
        )
    }

    /// 创建仅用于报告聚合测试的探测记录。
    fn probe(capability: &str, response_mode: &str, status: &str) -> ProbeRecord {
        let stable_key = probe_stable_key(
            "run",
            "provider",
            "model",
            "openai_responses",
            response_mode,
            capability,
        );
        ProbeRecord {
            stable_key,
            provider_id: "provider".to_owned(),
            model: "model".to_owned(),
            protocol: "openai_responses".to_owned(),
            response_mode: response_mode.to_owned(),
            capability: capability.to_owned(),
            endpoint_path: "/v1/responses".to_owned(),
            status: status.to_owned(),
            attempts: 1,
            latency_ms: 10,
            expected_text: None,
            synthetic_marker: None,
            actual_text_evidence: None,
            response: None,
            assertions: Vec::new(),
            cancellation: None,
            skip_evidence: None,
            fixture_paths: Vec::new(),
            recovered_from: None,
            fixture_replay: None,
            normalized_error: None,
            wire_response_shapes: Vec::new(),
            wire_exchanges: Vec::new(),
            wire_exchange_outcomes: Vec::new(),
        }
    }

    /// 创建能够通过同步恢复门禁且由 append_probe 写出真实内容寻址 Fixture 的文本记录。
    fn passed_text_probe(response_mode: &str) -> ProbeRecord {
        let mut record = probe("text", response_mode, "passed");
        let marker = marker_from_probe_stable_key(&record.stable_key, false);
        record.expected_text = Some(marker.clone());
        record.synthetic_marker = Some(marker.clone());
        record.actual_text_evidence = Some(ActualTextEvidence::from_text(
            &provider(),
            &record.stable_key,
            &marker,
        ));
        record.response = Some(ResponseEvidence {
            response_id_present: true,
            reported_model_redacted: Some("model".to_owned()),
            stop_reason: "completed".to_owned(),
            content_block_types: vec!["text".to_owned()],
            text_block_count: 1,
            reasoning_block_count: 0,
            tool_call_count: 0,
            usage: TokenUsage::default(),
        });
        record.assertions = vec![SemanticAssertion::new(
            "text_exact",
            true,
            "合成标记完全一致",
        )];
        record.fixture_replay = Some(FixtureReplayEvidence {
            status: "passed".to_owned(),
            exchange_count: 1,
            replayed_exchanges: 1,
            reason: None,
        });
        let model_request = text_model_request(&marker);
        record.wire_exchanges = vec![WireExchange {
            model_request,
            max_event_bytes: 64 * 1024,
            request_body: encode_wire_request(
                ProviderProtocol::Responses,
                &text_model_request(&marker),
                response_mode == "streaming",
            )
            .expect("测试 Responses 请求应按响应模式编码"),
            response_status: Some(200),
            response_content_type: Some("application/json".to_owned()),
            response_body: serde_json::to_vec(&serde_json::json!({
                "id": "response-fixture",
                "object": "response",
                "model": "model",
                "status": "completed",
                "output": [{
                    "id": "message-fixture",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": marker}]
                }]
            }))
            .expect("测试响应应可序列化"),
            response_body_truncated: false,
            response_body_eof_observed: true,
            terminal_error: None,
        }];
        record.wire_response_shapes = record
            .wire_exchanges
            .iter()
            .map(|exchange| {
                inspect_wire_response_shape(
                    ProviderProtocol::Responses,
                    exchange.response_status,
                    exchange.response_content_type.as_deref(),
                    &exchange.response_body,
                    exchange.response_body_eof_observed,
                    exchange.response_body_truncated,
                )
            })
            .collect();
        record.wire_exchange_outcomes = vec![FixtureExchangeOutcome::Response {
            response: record.response.clone().expect("通过记录应包含响应证据"),
            actual_text_evidence: record
                .actual_text_evidence
                .clone()
                .expect("通过记录应包含实际文本证据"),
        }];
        record
    }

    /// 构造 v14 曾写出的“最终本地取消成功但前序传输终态不可重放”记录。
    fn legacy_unreplayable_cancellation_probe(run_id: &str) -> ProbeRecord {
        let stable_key = probe_stable_key(
            run_id,
            "provider",
            "model",
            "openai_responses",
            "streaming",
            "cancellation",
        );
        let marker = marker_from_probe_stable_key(&stable_key, false);
        let model_request = text_model_request(&marker);
        let request_body = encode_wire_request(ProviderProtocol::Responses, &model_request, true)
            .expect("取消测试请求应能编码");
        let transport_error = keencode_model::ModelError::Transport {
            message: "synthetic transport failure".to_owned(),
            retryable: true,
        };
        let exchanges = vec![
            WireExchange {
                model_request: model_request.clone(),
                max_event_bytes: 64 * 1024,
                request_body: request_body.clone(),
                response_status: None,
                response_content_type: None,
                response_body: Vec::new(),
                response_body_truncated: false,
                response_body_eof_observed: false,
                terminal_error: Some(transport_error.clone()),
            },
            WireExchange {
                model_request,
                max_event_bytes: 64 * 1024,
                request_body,
                response_status: Some(200),
                response_content_type: Some("text/event-stream".to_owned()),
                response_body: b"event: response.created\n\n".to_vec(),
                response_body_truncated: false,
                response_body_eof_observed: false,
                terminal_error: None,
            },
        ];
        let wire_response_shapes = exchanges
            .iter()
            .map(|exchange| {
                inspect_wire_response_shape(
                    ProviderProtocol::Responses,
                    exchange.response_status,
                    exchange.response_content_type.as_deref(),
                    &exchange.response_body,
                    exchange.response_body_eof_observed,
                    exchange.response_body_truncated,
                )
            })
            .collect();
        ProbeRecord {
            stable_key,
            provider_id: "provider".to_owned(),
            model: "model".to_owned(),
            protocol: "openai_responses".to_owned(),
            response_mode: "streaming".to_owned(),
            capability: "cancellation".to_owned(),
            endpoint_path: "/v1/responses".to_owned(),
            status: "unverified".to_owned(),
            attempts: 2,
            latency_ms: 10,
            expected_text: None,
            synthetic_marker: Some(marker),
            actual_text_evidence: None,
            response: None,
            assertions: vec![
                SemanticAssertion::new("stream_event_received_before_cancel", true, "已收到首事件"),
                SemanticAssertion::new("local_cancel_timer_won", true, "取消计时器获胜"),
                SemanticAssertion::new("in_flight_future_dropped", true, "在途调用已丢弃"),
                SemanticAssertion::new("remote_termination_not_claimed", true, "未声称远端终止"),
                SemanticAssertion::new(
                    "wire_adapter_replay",
                    false,
                    LEGACY_UNREPLAYABLE_CANCELLATION_REASON,
                ),
            ],
            cancellation: Some(CancellationEvidence {
                cancel_after_ms: 250,
                local_future_dropped: true,
                first_event_received: true,
                completed_before_cancel: false,
                observed_latency_ms: 250,
                remote_termination_proven: false,
            }),
            skip_evidence: None,
            fixture_paths: Vec::new(),
            recovered_from: None,
            fixture_replay: Some(FixtureReplayEvidence {
                status: "unavailable".to_owned(),
                exchange_count: 2,
                replayed_exchanges: 0,
                reason: Some(LEGACY_UNREPLAYABLE_CANCELLATION_REASON.to_owned()),
            }),
            normalized_error: None,
            wire_response_shapes,
            wire_exchanges: exchanges,
            wire_exchange_outcomes: vec![
                FixtureExchangeOutcome::ObservedTerminalError {
                    error: normalize_error(&provider(), &transport_error),
                },
                FixtureExchangeOutcome::RequestOnly,
            ],
        }
    }

    /// 构造前序请求无响应头、最终响应先于取消完成的当前契约记录。
    fn completed_before_cancel_probe(run_id: &str) -> ProbeRecord {
        let mut record = legacy_unreplayable_cancellation_probe(run_id);
        let marker = record
            .synthetic_marker
            .clone()
            .expect("取消测试记录必须包含合成标记");
        record.latency_ms = 500;
        record.wire_exchanges[0].terminal_error = None;
        record.wire_exchanges[1].response_content_type = Some("application/json".to_owned());
        record.wire_exchanges[1].response_body = serde_json::to_vec(&serde_json::json!({
            "id": "response-cancellation-fixture",
            "object": "response",
            "model": "model",
            "status": "completed",
            "output": [{
                "id": "message-cancellation-fixture",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": marker}]
            }]
        }))
        .expect("取消提前完成响应应可序列化");
        record.wire_exchanges[1].response_body_eof_observed = true;
        record.wire_response_shapes = record
            .wire_exchanges
            .iter()
            .map(|exchange| {
                inspect_wire_response_shape(
                    ProviderProtocol::Responses,
                    exchange.response_status,
                    exchange.response_content_type.as_deref(),
                    &exchange.response_body,
                    exchange.response_body_eof_observed,
                    exchange.response_body_truncated,
                )
            })
            .collect();
        let response = ResponseEvidence {
            response_id_present: true,
            reported_model_redacted: Some("model".to_owned()),
            stop_reason: "completed".to_owned(),
            content_block_types: vec!["text".to_owned()],
            text_block_count: 1,
            reasoning_block_count: 0,
            tool_call_count: 0,
            usage: TokenUsage::default(),
        };
        let actual_text_evidence =
            ActualTextEvidence::from_text(&provider(), &record.stable_key, &marker);
        record.response = Some(response.clone());
        record.actual_text_evidence = Some(actual_text_evidence.clone());
        record.assertions = vec![
            SemanticAssertion::new("stream_event_received_before_cancel", true, "已收到首事件"),
            SemanticAssertion::new("local_cancel_timer_won", false, "完整响应先于取消计时器"),
            SemanticAssertion::new("in_flight_future_dropped", false, "在途调用已完整结束"),
            SemanticAssertion::new("remote_termination_not_claimed", true, "未声称远端终止"),
            SemanticAssertion::new(
                "wire_adapter_replay",
                false,
                LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON,
            ),
        ];
        record.cancellation = Some(CancellationEvidence {
            cancel_after_ms: 250,
            local_future_dropped: false,
            first_event_received: true,
            completed_before_cancel: true,
            observed_latency_ms: 500,
            remote_termination_proven: false,
        });
        record.fixture_replay = None;
        record.wire_exchange_outcomes = vec![
            FixtureExchangeOutcome::Unavailable {
                reason: LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON.to_owned(),
            },
            FixtureExchangeOutcome::Response {
                response,
                actual_text_evidence,
            },
        ];
        record
    }

    /// 验证 v14 两种已观察到的取消传输缺口可以重跑，其他原因仍失败关闭。
    #[test]
    fn legacy_cancellation_gap_只接受两种固定不可重放原因() {
        let mut record = legacy_unreplayable_cancellation_probe("run");
        record.fixture_paths = vec!["fixtures/legacy-cancellation.json".to_owned()];
        let key = record.stable_key.clone();
        let validation_error = format!("恢复记录 {key} 的取消提前完成状态没有通过真实响应重放");
        assert!(legacy_unreplayable_cancellation_record(
            &key,
            &record,
            &validation_error
        ));

        record
            .fixture_replay
            .as_mut()
            .expect("旧取消记录必须包含重放结论")
            .reason = Some(LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON.to_owned());
        assert!(legacy_unreplayable_cancellation_record(
            &key,
            &record,
            &validation_error
        ));

        record
            .fixture_replay
            .as_mut()
            .expect("旧取消记录必须包含重放结论")
            .reason = Some("不同的不可复核原因".to_owned());
        assert!(!legacy_unreplayable_cancellation_record(
            &key,
            &record,
            &validation_error
        ));
    }

    /// 验证当前契约的本地取消重放缺口可离线生成恢复计划，且仍由 Fixture 证明末次 RequestOnly。
    #[tokio::test]
    async fn recovery_import_plan_接受本地取消重放缺口() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let source_root = std::env::temp_dir().join(format!(
            "keencode-provider-unverified-cancellation-source-{}-{unique}",
            std::process::id()
        ));
        let source =
            ReportStore::create(&source_root, "source").expect("应能创建本地取消重放缺口来源目录");
        let provider = provider();
        let mut options = runtime_options();
        options.max_attempts = 2;
        options.capabilities = BTreeSet::from([ProbeKind::Cancellation]);
        let run = RunMetadata::new("source".to_owned(), &options)
            .expect("应能创建本地取消重放缺口运行元数据");
        let mut manifest = ResumeManifest::new(run, &options, &[&provider])
            .expect("应能创建本地取消重放缺口恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结本地取消重放缺口候选模型");
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入本地取消重放缺口初始清单");

        let mut record = legacy_unreplayable_cancellation_probe("source");
        for exchange in &mut record.wire_exchanges {
            exchange.response_status = None;
            exchange.response_content_type = None;
            exchange.response_body.clear();
            exchange.response_body_truncated = false;
            exchange.response_body_eof_observed = false;
            exchange.terminal_error = None;
        }
        let protocol = ProviderProtocol::Responses;
        record.wire_response_shapes = record
            .wire_exchanges
            .iter()
            .map(|exchange| {
                inspect_wire_response_shape(
                    protocol,
                    exchange.response_status,
                    exchange.response_content_type.as_deref(),
                    &exchange.response_body,
                    exchange.response_body_eof_observed,
                    exchange.response_body_truncated,
                )
            })
            .collect();
        record.wire_exchange_outcomes = vec![
            FixtureExchangeOutcome::Unavailable {
                reason: LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON.to_owned(),
            },
            FixtureExchangeOutcome::RequestOnly,
        ];
        record.assertions = vec![
            SemanticAssertion::new("stream_event_received_before_cancel", true, "已收到首事件"),
            SemanticAssertion::new("local_cancel_timer_won", true, "取消计时器获胜"),
            SemanticAssertion::new("in_flight_future_dropped", true, "在途调用已丢弃"),
            SemanticAssertion::new("remote_termination_not_claimed", true, "未声称远端终止"),
            SemanticAssertion::new(
                "wire_adapter_replay",
                false,
                LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON,
            ),
        ];
        record.cancellation = Some(CancellationEvidence {
            cancel_after_ms: 500,
            local_future_dropped: true,
            first_event_received: true,
            completed_before_cancel: false,
            observed_latency_ms: 500,
            remote_termination_proven: false,
        });
        record.fixture_replay = Some(FixtureReplayEvidence {
            status: "unavailable".to_owned(),
            exchange_count: 2,
            replayed_exchanges: 0,
            reason: Some(LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON.to_owned()),
        });

        manifest
            .validate_probe_scope(&record)
            .expect("当前本地取消记录应位于运行范围内");
        let sequence = source
            .append_probe("source", &mut record, &[&provider])
            .expect("应能离线写出本地取消重放缺口 Fixture");
        manifest
            .commit_probe(sequence, record)
            .expect("应能提交本地取消重放缺口记录");
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入本地取消重放缺口已提交清单");
        let source_run_dir = source.run_dir().to_path_buf();
        drop(source);

        let source = ReportStore::open_recovery_source(&source_run_dir)
            .expect("应能只读打开本地取消重放缺口来源");
        let loaded = source
            .load_recovery_source_manifest(&[&provider], false)
            .expect("应能只读加载本地取消重放缺口来源");
        let reusable = source
            .reusable_records(&loaded, &[&provider])
            .await
            .expect("普通恢复应能复用本地取消重放缺口记录");
        assert_eq!(reusable.len(), 1);
        let plan = source
            .recovery_import_plan(&loaded, &[&provider], false)
            .expect("当前契约应能为本地取消重放缺口生成离线恢复计划");
        assert_eq!(plan.records.len(), 1);
        assert!(plan.rerun_records.is_empty());
        assert_eq!(plan.fixture_paths.len(), 1);
        assert_eq!(
            plan.records
                .values()
                .next()
                .expect("恢复计划应包含本地取消记录")
                .status,
            "unverified"
        );

        drop(source);
        fs::remove_dir_all(&source_root).expect("应能清理本地取消重放缺口来源目录");
    }

    /// 验证当前契约可导入前序传输缺口后的提前完成响应，并仍拒绝把它误算为取消成功。
    #[tokio::test]
    async fn recovery_import_plan_接受前序缺口后的取消提前完成响应() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let source_root = std::env::temp_dir().join(format!(
            "keencode-provider-completed-cancellation-source-{}-{unique}",
            std::process::id()
        ));
        let source =
            ReportStore::create(&source_root, "source").expect("应能创建取消提前完成来源目录");
        let provider = provider();
        let mut options = runtime_options();
        options.max_attempts = 2;
        options.capabilities = BTreeSet::from([ProbeKind::Cancellation]);
        let run = RunMetadata::new("source".to_owned(), &options)
            .expect("应能创建取消提前完成运行元数据");
        let mut manifest =
            ResumeManifest::new(run, &options, &[&provider]).expect("应能创建取消提前完成恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结取消提前完成候选模型");
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入取消提前完成初始清单");

        let mut record = completed_before_cancel_probe("source");
        manifest
            .validate_probe_scope(&record)
            .expect("取消提前完成记录应位于运行范围内");
        let sequence = source
            .append_probe("source", &mut record, &[&provider])
            .expect("应能写出取消提前完成 Fixture");
        assert_eq!(record.status, "unverified");
        assert_eq!(
            record
                .fixture_replay
                .as_ref()
                .expect("取消提前完成记录必须包含重放结论")
                .reason
                .as_deref(),
            Some(LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON)
        );
        manifest
            .commit_probe(sequence, record)
            .expect("应能提交取消提前完成记录");
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入取消提前完成已提交清单");
        let source_run_dir = source.run_dir().to_path_buf();
        drop(source);

        let source = ReportStore::open_recovery_source(&source_run_dir)
            .expect("应能只读打开取消提前完成来源");
        let loaded = source
            .load_recovery_source_manifest(&[&provider], false)
            .expect("应能认证取消提前完成来源");
        let reusable = source
            .reusable_records(&loaded, &[&provider])
            .await
            .expect("普通恢复应能复用前序缺口后的取消提前完成记录");
        assert_eq!(reusable.len(), 1);
        assert_eq!(
            reusable
                .values()
                .next()
                .expect("普通恢复应返回取消提前完成记录")
                .status,
            "unverified"
        );
        let fixture_path = loaded
            .records
            .values()
            .next()
            .and_then(|record| record.fixture_paths.first())
            .expect("取消提前完成记录必须引用 Fixture")
            .clone();
        source
            .trusted_fixture_artifact_sha256(&loaded, &fixture_path, &[&provider])
            .expect("完成态 Fixture 封印应接受前序缺口后的取消提前完成响应");
        let plan = source
            .recovery_import_plan(&loaded, &[&provider], false)
            .expect("当前契约应能导入有完整最终响应的取消未验证记录");
        assert_eq!(plan.records.len(), 1);
        assert!(plan.rerun_records.is_empty());
        assert_eq!(plan.fixture_paths.len(), 1);

        drop(source);
        fs::remove_dir_all(&source_root).expect("应能清理取消提前完成来源目录");
    }

    /// 验证 v14 只有完全匹配的无响应头取消失败 Fixture 才能进入重新请求清单。
    #[test]
    fn legacy_failed_cancellation_gap_要求记录与逐交换形态完全匹配() {
        let mut record = legacy_unreplayable_cancellation_probe("run");
        let transport_error = match &record.wire_exchange_outcomes[0] {
            FixtureExchangeOutcome::ObservedTerminalError { error } => error.clone(),
            _ => panic!("测试记录首个交换必须包含传输终态"),
        };
        let replay = FixtureReplayEvidence {
            status: "unavailable".to_owned(),
            exchange_count: record.attempts,
            replayed_exchanges: 0,
            reason: Some(LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON.to_owned()),
        };
        record.status = "failed".to_owned();
        record.assertions = vec![SemanticAssertion::new(
            "wire_adapter_replay",
            false,
            LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON,
        )];
        record.cancellation = Some(CancellationEvidence {
            cancel_after_ms: 500,
            local_future_dropped: false,
            first_event_received: false,
            completed_before_cancel: false,
            observed_latency_ms: 500,
            remote_termination_proven: false,
        });
        record.fixture_paths = vec!["fixtures/legacy-failed-cancellation.json".to_owned()];
        record.fixture_replay = Some(replay.clone());
        record.normalized_error = Some(transport_error.clone());
        let unavailable_exchange = FixtureExchange {
            request: FixtureRequestEvidence::SubsequentRequestOmitted {
                reason: OMITTED_SUBSEQUENT_REQUEST_REASON.to_owned(),
            },
            max_event_bytes: 64 * 1024,
            response_shape: test_response_shape(
                ProviderProtocol::Responses,
                None,
                None,
                b"",
                false,
                false,
            ),
            observed_terminal_error: None,
            expected_outcome: FixtureExchangeOutcome::Unavailable {
                reason: LEGACY_NO_RESPONSE_HEADERS_CANCELLATION_REASON.to_owned(),
            },
        };
        let mut fixture = ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256: String::new(),
            payload: ProbeFixturePayload {
                run_id: "run".to_owned(),
                stable_key: record.stable_key.clone(),
                provider_id: record.provider_id.clone(),
                model: record.model.clone(),
                protocol: record.protocol.clone(),
                response_mode: record.response_mode.clone(),
                capability: record.capability.clone(),
                synthetic_marker: record.synthetic_marker.clone(),
                synthetic_only: true,
                exchanges: vec![unavailable_exchange; record.attempts],
                expected_response: None,
                expected_actual_text_evidence: None,
                expected_error: Some(transport_error.clone()),
                expected_cancellation: record.cancellation.clone(),
                replay: Some(replay),
            },
        };
        let verification_error = "取消失败只有显式在线传输终态可以声明为响应不可从磁盘复核";
        assert!(legacy_unreplayable_failed_cancellation_fixture(
            &record,
            &fixture,
            verification_error
        ));

        fixture.payload.exchanges[0].observed_terminal_error = Some(transport_error);
        assert!(!legacy_unreplayable_failed_cancellation_fixture(
            &record,
            &fixture,
            verification_error
        ));
    }

    /// 创建没有发出请求且只携带本地配置错误的可恢复记录。
    fn failed_configuration_probe(response_mode: &str) -> ProbeRecord {
        let mut record = probe("text", response_mode, "failed");
        record.attempts = 0;
        record.normalized_error = Some(NormalizedError {
            kind: "configuration".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text("测试配置失败"),
            retryable: false,
            http_status: None,
        });
        record
    }

    /// 创建可精确控制运行、Provider、能力和错误分类的失败补测事实。
    fn failed_retry_probe(
        run_id: &str,
        provider_id: &str,
        model: &str,
        capability: &str,
        status: &str,
        error_kind: &str,
        retryable: bool,
    ) -> ProbeRecord {
        let mut record = probe(capability, "buffered", status);
        record.provider_id = provider_id.to_owned();
        record.model = model.to_owned();
        record.stable_key = probe_stable_key(
            run_id,
            provider_id,
            model,
            "openai_responses",
            "buffered",
            capability,
        );
        record.normalized_error = Some(NormalizedError {
            kind: error_kind.to_owned(),
            message_evidence: ErrorMessageEvidence::from_text("synthetic retry selection error"),
            retryable,
            http_status: None,
        });
        record
    }

    /// 把合成事实包装为一条固定 v4 提交日志记录。
    fn retry_journal_entry(sequence: u64, record: ProbeRecord) -> OwnedProbeJournalEntry {
        OwnedProbeJournalEntry {
            schema_version: JOURNAL_SCHEMA_VERSION.to_owned(),
            sequence,
            previous_mac: None,
            record_mac: None,
            record,
        }
    }

    /// 创建摘要自洽且只包含一个文本 tuple 的精确补测选择。
    fn retry_selection(source_run_id: &str) -> RetrySelectionManifest {
        let source_stable_key = probe_stable_key(
            source_run_id,
            "provider",
            "model",
            "openai_responses",
            "buffered",
            "text",
        );
        let case = RetryCase {
            source_sequence: 1,
            source_stable_key,
            tuple_key: retry_tuple_key("provider", "model", "openai_responses", "buffered", "text"),
            provider_id: "provider".to_owned(),
            model: "model".to_owned(),
            protocol: "openai_responses".to_owned(),
            response_mode: "buffered".to_owned(),
            capability: "text".to_owned(),
        };
        let digest = format!("sha256:{}", "a".repeat(64));
        let mut selection = RetrySelectionManifest {
            lineage: RetryLineage {
                schema_version: RETRY_SELECTION_SCHEMA_VERSION.to_owned(),
                source_run_id: source_run_id.to_owned(),
                source_runtime_commit: "synthetic-commit".to_owned(),
                source_executable_sha256: digest.clone(),
                source_authentication: LEGACY_UNAUTHENTICATED_SOURCE_LEVEL.to_owned(),
                source_resume_schema_version: RETRY_SOURCE_RESUME_SCHEMA_VERSION.to_owned(),
                source_harness_contract_id: RETRY_SOURCE_HARNESS_CONTRACT_ID.to_owned(),
                source_report_schema_version: RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION.to_owned(),
                source_resume_sha256: digest.clone(),
                source_journal_sha256: digest.clone(),
                source_result_sha256: digest.clone(),
                source_redaction_report_sha256: digest,
                provider_id: "provider".to_owned(),
                through_sequence: 1,
                policy: RETRY_SELECTION_POLICY.to_owned(),
                selected_records: 1,
                selection_sha256: String::new(),
            },
            cases: vec![case],
        };
        selection.lineage.selection_sha256 = selection
            .calculated_sha256()
            .expect("合成补测选择应可计算摘要");
        selection
    }

    /// 写出一份可被只读来源校验完整重载的完成运行测试夹具。
    fn write_completed_retry_test_run(
        output_root: &Path,
        run_id: &str,
        provider: &ProviderEntry,
        record: ProbeRecord,
        retry_selection: Option<RetrySelectionManifest>,
        legacy_source: bool,
    ) -> (PathBuf, String) {
        let store = ReportStore::create(output_root, run_id).expect("应能创建完成运行目录");
        let mut options = runtime_options();
        if let Some(selection) = &retry_selection {
            let (provider_id, capabilities) = selection.runtime_shape();
            options
                .apply_retry_runtime_shape(provider_id, capabilities)
                .expect("补测运行形状应有效");
        }
        let mut run = RunMetadata::new(run_id.to_owned(), &options).expect("应能创建运行元数据");
        if let Some(selection) = &retry_selection {
            run.retry_lineage = Some(selection.lineage.clone());
        }
        let mut manifest = match retry_selection {
            Some(selection) => ResumeManifest::new_retry(run, &options, &[provider], selection)
                .expect("应能创建补测恢复清单"),
            None => ResumeManifest::new(run, &options, &[provider]).expect("应能创建基础恢复清单"),
        };
        manifest
            .register_candidates(&record.provider_id, [record.model.clone()])
            .expect("应能冻结单个候选模型");
        if let Some(selection) = manifest.retry_selection() {
            store
                .write_retry_selection(selection, &[provider])
                .expect("应能写入补测选择清单");
        }
        store
            .write_resume_manifest(&manifest, &[provider])
            .expect("应能写入初始恢复清单");

        let mut completed_records = vec![record];
        if manifest.retry_selection().is_none() {
            for protocol in [
                "anthropic_messages",
                "openai_chat_completions",
                "openai_responses",
            ] {
                for response_mode in ["buffered", "streaming"] {
                    if completed_records.iter().any(|record| {
                        record.protocol == protocol && record.response_mode == response_mode
                    }) {
                        continue;
                    }
                    let template = completed_records
                        .first()
                        .expect("完整矩阵测试夹具必须包含基础记录");
                    let mut coverage = failed_configuration_probe(response_mode);
                    coverage.provider_id = template.provider_id.clone();
                    coverage.model = template.model.clone();
                    coverage.protocol = protocol.to_owned();
                    coverage.response_mode = response_mode.to_owned();
                    coverage.endpoint_path = match protocol {
                        "anthropic_messages" => "/v1/messages",
                        "openai_chat_completions" => "/v1/chat/completions",
                        "openai_responses" => "/v1/responses",
                        _ => unreachable!("测试矩阵只包含固定三种协议"),
                    }
                    .to_owned();
                    coverage.stable_key = probe_stable_key(
                        run_id,
                        &provider.id,
                        &coverage.model,
                        protocol,
                        response_mode,
                        &coverage.capability,
                    );
                    completed_records.push(coverage);
                }
            }
        }

        for record in &mut completed_records {
            manifest
                .validate_probe_scope(record)
                .expect("完成矩阵记录必须位于当前运行范围");
            let sequence = store
                .append_probe(run_id, record, &[provider])
                .expect("应能写入完成矩阵提交日志");
            manifest
                .commit_probe(sequence, record.clone())
                .expect("应能提交完成矩阵恢复事实");
        }
        store
            .write_resume_manifest(&manifest, &[provider])
            .expect("应能写入已提交恢复清单");

        let mut report = RunReport::new(manifest.run.clone());
        report.providers =
            vec![ProviderRecord::from_provider(provider).expect("应能生成 Provider 测试快照")];
        if manifest.retry_selection().is_none() {
            report.catalogs = vec![CatalogRecord {
                provider_id: provider.redact_text(&provider.id),
                status: "success".to_owned(),
                attempts: 1,
                latency_ms: 0,
                pages: 1,
                raw_count: 1,
                invalid_count: 0,
                discovered_models: Vec::new(),
                candidates: vec![CandidateModelRecord {
                    model: provider.redact_text(&completed_records[0].model),
                    configured: true,
                    discovered: false,
                    explicit: false,
                    frozen_from_resume: false,
                }],
                normalized_error: None,
            }];
        }
        report.probes = completed_records;
        report.refresh_summary();
        report.run.finished_at = Some(timestamp().expect("应能生成完成时间"));
        store
            .finalize_completed(&report, &manifest, &[provider])
            .expect("应能完成单记录测试运行");
        manifest.run = report.run.clone();
        manifest.finished = true;
        if legacy_source {
            report.schema_version = RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION;
            store
                .write_json("result.json", &report, &[provider])
                .expect("应能写入显式 legacy 最终报告");
            manifest.identity.schema_version = RETRY_SOURCE_RESUME_SCHEMA_VERSION.to_owned();
            manifest.identity.harness_contract_id = RETRY_SOURCE_HARNESS_CONTRACT_ID.to_owned();
            for identity in &mut manifest.identity.providers {
                identity.credential_proof =
                    provider.legacy_credential_resume_proof(&manifest.identity.run_salt);
            }
            manifest.journal_tail_mac = None;
            manifest.state_proofs.clear();
            manifest.completion_artifact_seal = None;
            store
                .write_json("resume.json", &manifest, &[provider])
                .expect("应能写入显式 legacy v5 来源清单");
            let legacy_journal = fs::read_to_string(&store.checkpoint_path)
                .expect("应能读取待降级的测试 Journal")
                .lines()
                .map(|line| {
                    let mut value: serde_json::Value =
                        serde_json::from_str(line).expect("当前 Journal 行应是有效 JSON");
                    let object = value.as_object_mut().expect("Journal 行必须是 JSON 对象");
                    object.remove("previousMac");
                    object.remove("recordMac");
                    serde_json::to_string(&value).expect("legacy Journal 行应可序列化")
                })
                .collect::<Vec<_>>()
                .join("\n");
            replace_file_contents(
                &store.checkpoint_path,
                &format!("{legacy_journal}\n"),
                "显式 legacy 测试 Journal",
            )
            .expect("应能写入显式 legacy 测试 Journal");
        }
        let executable_sha256 = manifest.identity.executable_sha256.clone();
        let run_dir = store.run_dir().to_path_buf();
        drop(store);
        (run_dir, executable_sha256)
    }

    /// 创建不依赖运行参数和系统时间的报告。
    fn report_with_probes(probes: Vec<ProbeRecord>) -> RunReport {
        let mut report = RunReport::new(RunMetadata {
            run_id: "run".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            finished_at: None,
            runtime_commit: "test".to_owned(),
            adapter_version: "test".to_owned(),
            os: "test".to_owned(),
            arch: "test".to_owned(),
            max_attempts_per_case: 1,
            request_timeout_secs: 1,
            global_concurrency: 1,
            capabilities: vec!["text".to_owned(), "reasoning".to_owned()],
            full_matrix: false,
            diagnostics_only: false,
            base_gate_policy: "text_per_model_protocol_response_mode_v1".to_owned(),
            recovery_lineage: None,
            retry_lineage: None,
        });
        report.probes = probes;
        report.refresh_summary();
        report
    }

    /// 创建不含本机路径且可参与严格恢复身份计算的测试参数。
    fn runtime_options() -> RuntimeOptions {
        RuntimeOptions {
            user_data_directory: PathBuf::new(),
            config_path: PathBuf::new(),
            verify_run_dir: None,
            output_root: PathBuf::new(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 1,
            request_timeout_secs: 5,
            catalog_only: false,
            diagnostics_only: false,
            capabilities: BTreeSet::from([ProbeKind::Text]),
            full_matrix: false,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        }
    }

    /// 验证成功目录也必须闭合当前恢复身份要求的三协议双模式矩阵。
    #[test]
    fn validate_catalog_completion_成功目录拒绝缺失矩阵() {
        let provider = provider();
        let options = runtime_options();
        let run = RunMetadata::new("run".to_owned(), &options)
            .expect("应能创建成功目录完成校验运行元数据");
        let mut manifest = ResumeManifest::new(run, &options, &[&provider])
            .expect("应能创建成功目录完成校验恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结成功目录完成校验候选集合");
        let catalog = CatalogRecord {
            provider_id: "provider".to_owned(),
            status: "success".to_owned(),
            attempts: 1,
            latency_ms: 0,
            pages: 1,
            raw_count: 1,
            invalid_count: 0,
            discovered_models: Vec::new(),
            candidates: vec![CandidateModelRecord {
                model: "model".to_owned(),
                configured: true,
                discovered: false,
                explicit: false,
                frozen_from_resume: false,
            }],
            normalized_error: None,
        };
        let record = failed_configuration_probe("buffered");
        manifest
            .commit_probe(1, record.clone())
            .expect("应能提交成功目录缺失矩阵测试记录");

        let error = validate_catalog_completion(&manifest, &[catalog], &[record], &[&provider])
            .expect_err("成功目录缺少矩阵 tuple 时不能声明完成");
        assert!(error.contains("冻结候选矩阵") || error.contains("应有 6 条，实际 1 条"));
    }

    /// 验证完整模式的失败目录即使诊断十二个 tuple 全部完成，也不能用空候选集合封印。
    #[test]
    fn validate_catalog_completion_full失败目录空候选拒绝封印() {
        let provider = provider();
        let mut options = runtime_options();
        options.full_matrix = true;
        let run = RunMetadata::new("run".to_owned(), &options)
            .expect("应能创建完整失败目录校验运行元数据");
        let mut manifest = ResumeManifest::new(run, &options, &[&provider])
            .expect("应能创建完整失败目录校验恢复清单");
        manifest
            .register_candidates("provider", Vec::<String>::new())
            .expect("应能冻结空候选集合");

        let catalog = CatalogRecord {
            provider_id: "provider".to_owned(),
            status: "failed".to_owned(),
            attempts: 1,
            latency_ms: 0,
            pages: 0,
            raw_count: 0,
            invalid_count: 0,
            discovered_models: Vec::new(),
            candidates: Vec::new(),
            normalized_error: Some(NormalizedError {
                kind: "transport".to_owned(),
                message_evidence: ErrorMessageEvidence::from_text("synthetic catalog failure"),
                retryable: true,
                http_status: None,
            }),
        };
        let mut probes = Vec::new();
        let run_id = manifest.run.run_id.clone();
        for protocol in [
            "anthropic_messages",
            "openai_chat_completions",
            "openai_responses",
        ] {
            for response_mode in ["buffered", "streaming"] {
                for (model, capability) in [
                    (
                        "keencode-authentication-probe".to_owned(),
                        "diagnostic_invalid_authentication",
                    ),
                    (
                        resume_missing_model_id(&provider, protocol, response_mode, &run_id),
                        "diagnostic_missing_model",
                    ),
                ] {
                    let mut record = failed_configuration_probe(response_mode);
                    record.model = model;
                    record.protocol = protocol.to_owned();
                    record.response_mode = response_mode.to_owned();
                    record.capability = capability.to_owned();
                    record.stable_key = probe_stable_key(
                        &run_id,
                        &provider.id,
                        &record.model,
                        protocol,
                        response_mode,
                        capability,
                    );
                    let sequence = u64::try_from(probes.len() + 1)
                        .expect("诊断测试记录数应能表示为 Journal 序号");
                    manifest
                        .commit_probe(sequence, record.clone())
                        .expect("应能提交完整诊断测试记录");
                    probes.push(record);
                }
            }
        }
        assert_eq!(probes.len(), 12);

        let error = validate_catalog_completion(&manifest, &[catalog], &probes, &[&provider])
            .expect_err("完整失败目录没有冻结候选时不能封印诊断矩阵");
        assert!(error.contains("没有冻结候选模型"), "实际错误：{error}");
    }

    /// 创建一份尚未完成且没有探测记录的最小完成流程测试状态。
    fn empty_completion_state(
        output_root: &Path,
        run_id: &str,
    ) -> (ReportStore, ProviderEntry, ResumeManifest, RunReport) {
        let store = ReportStore::create(output_root, run_id).expect("应能创建完成流程测试目录");
        let provider = provider();
        let options = runtime_options();
        let run =
            RunMetadata::new(run_id.to_owned(), &options).expect("应能创建完成流程运行元数据");
        let manifest =
            ResumeManifest::new(run, &options, &[&provider]).expect("应能创建完成流程恢复清单");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入完成流程初始恢复清单");
        let mut report = RunReport::new(manifest.run.clone());
        report.providers =
            vec![ProviderRecord::from_provider(&provider).expect("应能创建完成流程 Provider 快照")];
        report.refresh_summary();
        report.run.finished_at = Some(timestamp().expect("应能生成完成流程结束时间"));
        (store, provider, manifest, report)
    }

    /// 递归快照测试运行目录中的相对文件集合和完整字节。
    fn snapshot_run_files(run_dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut snapshot = BTreeMap::new();
        let mut pending = vec![run_dir.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).expect("应能枚举测试运行目录") {
                let entry = entry.expect("测试运行目录项应可读取");
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).expect("测试运行目录项元数据应可读取");
                assert!(!is_link_or_reparse(&metadata), "测试快照不得包含链接");
                if metadata.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path
                        .strip_prefix(run_dir)
                        .expect("测试运行文件必须位于运行根内")
                        .to_path_buf();
                    let bytes = if relative == Path::new(".keencode-live-test.lock") {
                        assert_eq!(metadata.len(), 0, "测试运行锁必须保持为空文件");
                        Vec::new()
                    } else {
                        fs::read(path).expect("测试运行文件应可读取")
                    };
                    snapshot.insert(relative, bytes);
                }
            }
        }
        snapshot
    }

    /// 为只读完成运行核验测试创建相互隔离的临时输出根目录。
    fn verification_test_root(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "keencode-provider-verify-{prefix}-{}-{unique}",
            std::process::id()
        ))
    }

    /// 创建带一个真实合成 Fixture 且其余 tuple 为本地失败事实的完成运行。
    fn completed_verification_test_run(prefix: &str) -> (PathBuf, PathBuf, ProviderEntry) {
        let output_root = verification_test_root(prefix);
        let provider = provider();
        let (run_dir, _) = write_completed_retry_test_run(
            &output_root,
            "run",
            &provider,
            passed_text_probe("buffered"),
            None,
            false,
        );
        (output_root, run_dir, provider)
    }

    /// 验证只读完成运行核验失败时不会改写运行目录中的任何路径或字节。
    async fn assert_verification_rejected_without_mutation(
        run_dir: &Path,
        providers: &[&ProviderEntry],
    ) -> String {
        let before = snapshot_run_files(run_dir);
        let source = ReportStore::open_recovery_source(run_dir).expect("应能只读打开核验来源");
        assert_eq!(
            before,
            snapshot_run_files(run_dir),
            "打开只读核验来源不得改写运行目录"
        );
        let error = source
            .verify_completed_run(providers)
            .await
            .expect_err("篡改或未完成来源必须被只读核验拒绝");
        drop(source);
        assert_eq!(
            before,
            snapshot_run_files(run_dir),
            "只读完成运行核验失败前后目录路径和字节必须完全一致"
        );
        error
    }

    /// 验证完整运行可由当前 Provider 配置离线核对并返回固定安全计数。
    #[tokio::test]
    async fn verify_completed_run_成功返回固定计数() {
        let (output_root, run_dir, provider) = completed_verification_test_run("success");
        let source_files_before = snapshot_run_files(&run_dir);
        let source = ReportStore::open_recovery_source(&run_dir).expect("应能打开完成核验来源");
        assert_eq!(
            source_files_before,
            snapshot_run_files(&run_dir),
            "打开成功核验来源不得改写运行目录"
        );
        let verification = source
            .verify_completed_run(&[&provider])
            .await
            .expect("正确完成运行应通过只读核验");
        assert_eq!(
            verification,
            CompletedRunVerification {
                provider_count: 1,
                record_count: 6,
                fixture_count: 1,
                journal_sequence: 6,
                seal_artifact_count: 6,
            }
        );
        drop(source);
        assert_eq!(
            source_files_before,
            snapshot_run_files(&run_dir),
            "成功完成运行核验前后目录路径和字节必须完全一致"
        );
        fs::remove_dir_all(&output_root).expect("应能清理完成核验成功测试目录");
    }

    /// 验证未完成运行、错误凭据和三类权威事实篡改均失败且保持来源不变。
    #[tokio::test]
    async fn verify_completed_run_拒绝未完成错误凭据和篡改且不修改来源() {
        let output_root = verification_test_root("unfinished");
        let (store, provider, _, _) = empty_completion_state(&output_root, "run");
        let run_dir = store.run_dir().to_path_buf();
        drop(store);
        let error = assert_verification_rejected_without_mutation(&run_dir, &[&provider]).await;
        assert!(error.contains("尚未完成"), "实际未完成错误：{error}");
        fs::remove_dir_all(&output_root).expect("应能清理未完成核验测试目录");

        let (output_root, run_dir, _provider) = completed_verification_test_run("credential");
        let wrong_provider: ProviderEntry = serde_json::from_value(serde_json::json!({
            "id": "provider",
            "name": "测试",
            "baseUrl": "https://example.com/v1",
            "models": ["model"],
            "apiBackend": "responses",
            "apiKey": "different-fixture-secret-value"
        }))
        .expect("应能创建错误凭据 Provider");
        let error =
            assert_verification_rejected_without_mutation(&run_dir, &[&wrong_provider]).await;
        assert!(error.contains("凭据认证"), "实际错误凭据错误：{error}");
        fs::remove_dir_all(&output_root).expect("应能清理错误凭据核验测试目录");

        let (output_root, run_dir, provider) = completed_verification_test_run("fixture-tamper");
        let fixture_path = fs::read_dir(run_dir.join("fixtures"))
            .expect("应能枚举核验 Fixture 目录")
            .map(|entry| entry.expect("核验 Fixture 项应可读取").path())
            .next()
            .expect("完成核验夹具应包含 Fixture");
        let mut fixture_bytes = fs::read(&fixture_path).expect("应能读取待篡改 Fixture");
        fixture_bytes.push(b'\n');
        fs::write(&fixture_path, fixture_bytes).expect("应能篡改核验 Fixture");
        let error = assert_verification_rejected_without_mutation(&run_dir, &[&provider]).await;
        assert!(
            error.contains("事实产物") || error.contains("封印"),
            "实际 Fixture 错误：{error}"
        );
        fs::remove_dir_all(&output_root).expect("应能清理 Fixture 篡改测试目录");

        let (output_root, run_dir, provider) = completed_verification_test_run("result-tamper");
        let result_path = run_dir.join("result.json");
        let mut result_bytes = fs::read(&result_path).expect("应能读取待篡改最终报告");
        result_bytes.push(b'\n');
        fs::write(&result_path, result_bytes).expect("应能篡改核验最终报告");
        let error = assert_verification_rejected_without_mutation(&run_dir, &[&provider]).await;
        assert!(
            error.contains("事实产物") || error.contains("封印"),
            "实际最终报告错误：{error}"
        );
        fs::remove_dir_all(&output_root).expect("应能清理最终报告篡改测试目录");

        let (output_root, run_dir, provider) = completed_verification_test_run("journal-tamper");
        let journal_path = run_dir.join("sanitized-logs/progress.jsonl");
        let journal_text = fs::read_to_string(&journal_path).expect("应能读取待篡改 Journal");
        let mut journal_lines = journal_text.lines();
        let first_journal_line = journal_lines.next().expect("Journal 应包含首行");
        let mut journal_entry: serde_json::Value =
            serde_json::from_str(first_journal_line).expect("Journal 首行应为 JSON");
        journal_entry["record"]["latencyMs"] = serde_json::json!(999_u64);
        let remaining_journal = journal_lines.collect::<Vec<_>>().join("\n");
        fs::write(
            &journal_path,
            if remaining_journal.is_empty() {
                format!(
                    "{}\n",
                    serde_json::to_string(&journal_entry).expect("篡改 Journal 应可序列化")
                )
            } else {
                format!(
                    "{}\n{remaining_journal}\n",
                    serde_json::to_string(&journal_entry).expect("篡改 Journal 应可序列化")
                )
            },
        )
        .expect("应能篡改核验 Journal");
        let error = assert_verification_rejected_without_mutation(&run_dir, &[&provider]).await;
        assert!(
            error.contains("恢复提交日志") || error.contains("凭据认证"),
            "实际 Journal 错误：{error}"
        );
        fs::remove_dir_all(&output_root).expect("应能清理 Journal 篡改测试目录");
    }

    /// 构造只含一个真实协议请求正文的完整 Fixture v6 Envelope 文本。
    fn synthetic_fixture(
        protocol: &str,
        synthetic_marker: &str,
        request_body: serde_json::Value,
    ) -> String {
        synthetic_fixture_with_proof(protocol, synthetic_marker, request_body, true)
    }

    /// 构造可显式控制纯合成证明的 Fixture v6，供写入拒绝路径测试使用。
    fn synthetic_fixture_with_proof(
        protocol: &str,
        synthetic_marker: &str,
        request_body: serde_json::Value,
        synthetic_only: bool,
    ) -> String {
        let semantic_request = text_model_request(synthetic_marker);
        let wire_top_level_field_count = request_body
            .as_object()
            .map(serde_json::Map::len)
            .unwrap_or(0);
        let payload = ProbeFixturePayload {
            run_id: "run".to_owned(),
            stable_key: probe_stable_key("run", "provider", "model", protocol, "buffered", "text"),
            provider_id: "provider".to_owned(),
            model: "model".to_owned(),
            protocol: protocol.to_owned(),
            response_mode: "buffered".to_owned(),
            capability: "text".to_owned(),
            synthetic_marker: Some(synthetic_marker.to_owned()),
            synthetic_only,
            exchanges: vec![FixtureExchange {
                request: FixtureRequestEvidence::SyntheticFirstRequest {
                    semantic_message_count: semantic_request.messages.len(),
                    semantic_tool_count: semantic_request.tools.len(),
                    wire_top_level_field_count,
                },
                max_event_bytes: 64 * 1024,
                response_shape: test_response_shape(
                    fixture_protocol(protocol).expect("测试协议必须受支持"),
                    Some(200),
                    Some("application/json"),
                    br#"{}"#,
                    true,
                    false,
                ),
                observed_terminal_error: None,
                expected_outcome: FixtureExchangeOutcome::Unavailable {
                    reason: "测试 Fixture 未执行响应重放".to_owned(),
                },
            }],
            expected_response: None,
            expected_actual_text_evidence: None,
            expected_error: None,
            expected_cancellation: None,
            replay: None,
        };
        let content_sha256 =
            fixture_payload_sha256(&payload).expect("测试 Fixture Payload 应可规范序列化");
        serde_json::to_string(&ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256,
            payload,
        })
        .expect("测试 Fixture v6 Envelope 应可序列化")
    }

    /// 构造报告测试使用的最小 Provider 中立文本请求。
    fn text_model_request(synthetic_marker: &str) -> ModelRequest {
        ModelRequest::new(
            "model",
            vec![keencode_model::Message::text(
                keencode_model::MessageRole::User,
                format!(
                    "只输出下一行的精确标记，不要添加标点、Markdown、空格或解释：\n{synthetic_marker}"
                ),
            )],
        )
    }

    /// 构造同时覆盖 assistant、工具往返、推理续传、工具 Schema 与结构化输出的统一请求。
    fn complex_model_request(protocol: ProviderProtocol, synthetic_marker: &str) -> ModelRequest {
        let continuation = match protocol {
            ProviderProtocol::Messages => keencode_model::OpaqueReasoningState::new(
                "messages-thinking-signature-v1",
                serde_json::json!("synthetic-signature"),
            ),
            ProviderProtocol::ChatCompletions => keencode_model::OpaqueReasoningState::new(
                "chat-reasoning-state-v1",
                serde_json::json!({"synthetic": true}),
            ),
            ProviderProtocol::Responses => keencode_model::OpaqueReasoningState::new(
                "responses-reasoning-item-v1",
                serde_json::json!({
                    "id": "reasoning-synthetic",
                    "encrypted_content": "synthetic-state"
                }),
            ),
        };
        let mut request = ModelRequest::new(
            "model",
            vec![
                keencode_model::Message::text(
                    keencode_model::MessageRole::System,
                    "只处理 Harness 合成数据",
                ),
                keencode_model::Message::text(
                    keencode_model::MessageRole::User,
                    format!(
                        "只输出下一行的精确标记，不要添加标点、Markdown、空格或解释：\n{synthetic_marker}"
                    ),
                ),
                keencode_model::Message::new(
                    keencode_model::MessageRole::Assistant,
                    vec![
                        ContentBlock::Reasoning {
                            reasoning: keencode_model::ReasoningContent {
                                text: "合成推理".to_owned(),
                                summary: Some("合成摘要".to_owned()),
                                continuation: Some(continuation),
                            },
                        },
                        ContentBlock::text("调用合成工具"),
                        ContentBlock::ToolCall {
                            tool_call: keencode_model::ToolCall::new(
                                "call-synthetic",
                                "keencode_probe_echo",
                                serde_json::json!({"value": 7}),
                            ),
                        },
                    ],
                ),
                keencode_model::Message::new(
                    keencode_model::MessageRole::Tool,
                    vec![ContentBlock::ToolResult {
                        tool_result: keencode_model::ToolResult::text(
                            "call-synthetic",
                            synthetic_marker,
                            false,
                        ),
                    }],
                ),
            ],
        );
        request.tools = vec![keencode_model::ToolDefinition::new(
            "keencode_probe_echo",
            "返回合成整数",
            serde_json::json!({
                "type": "object",
                "properties": {"value": {"type": "integer"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        )];
        request.parallel_tool_calls = Some(true);
        request.reasoning = Some(keencode_model::ReasoningConfig {
            effort: Some(keencode_model::ReasoningEffort::High),
            max_tokens: None,
            include_summary: true,
        });
        request.structured_output = Some(keencode_model::StructuredOutputConfig {
            name: "synthetic_result".to_owned(),
            description: Some("Harness 合成结构化结果".to_owned()),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"marker": {"type": "string"}},
                "required": ["marker"],
                "additionalProperties": false
            }),
            strict: true,
        });
        request.max_output_tokens = Some(32_768);
        request.temperature = Some(0.25);
        if protocol != ProviderProtocol::Responses {
            request.stop_sequences = vec!["KC_STOP_SYNTHETIC".to_owned()];
        }
        request
            .metadata
            .insert("fixture".to_owned(), "complex".to_owned());
        request
    }

    /// 构造只要求精确返回合成标记的 Responses 请求正文。
    fn responses_text_request(synthetic_marker: &str) -> serde_json::Value {
        encode_wire_request(
            ProviderProtocol::Responses,
            &text_model_request(synthetic_marker),
            false,
        )
        .expect("测试 Responses 请求应可编码")
    }

    /// 验证缺失 Usage 不会被伪造成上报 Token。
    #[test]
    fn add_usage_只累加明确值() {
        let mut summary = SummaryRecord::default();
        add_usage(
            &mut summary,
            &TokenUsage {
                input_tokens: Some(4),
                output_tokens: None,
                reasoning_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                total_tokens: None,
            },
        );
        assert_eq!(summary.reported_input_tokens, 4);
        assert_eq!(summary.reported_output_tokens, 0);
    }

    /// 验证最终结果只输出当前 v10 Schema，避免文档与持久化版本漂移。
    #[test]
    fn run_report_使用schema_v10() {
        let report = report_with_probes(Vec::new());
        let serialized = serde_json::to_value(&report).expect("最终报告应可序列化");
        assert_eq!(report.schema_version, "10");
        assert_eq!(serialized["schemaVersion"], serde_json::json!("10"));
    }

    /// 验证攻击者复制完全相同 Probe 并同步重算汇总时仍在输入唯一性门禁失败。
    #[test]
    fn validate_stored_run_report_拒绝完全相同的重复probe() {
        let provider = provider();
        let options = runtime_options();
        let mut run =
            RunMetadata::new("run".to_owned(), &options).expect("应能创建最终报告运行元数据");
        run.finished_at = Some("2026-01-01T00:00:00Z".to_owned());
        let mut manifest = ResumeManifest::new(run.clone(), &options, &[&provider])
            .expect("应能创建最终报告恢复清单");
        let record = failed_configuration_probe("buffered");
        manifest
            .commit_probe(1, record.clone())
            .expect("应能提交唯一基础事实");
        manifest.run = run.clone();
        manifest.finished = true;

        let mut report = RunReport::new(run);
        report.providers =
            vec![ProviderRecord::from_provider(&provider).expect("应能创建 Provider 快照")];
        report.probes = vec![record.clone(), record];
        report.refresh_summary();
        let bytes = serde_json::to_vec(&report).expect("应能编码重复事实攻击报告");

        assert!(
            validate_stored_run_report(
                &bytes,
                &manifest,
                &[&provider],
                &[RUN_REPORT_SCHEMA_VERSION],
            )
            .err()
            .expect("完全相同重复 Probe 即使汇总自洽也必须拒绝")
            .contains("重复探测稳定键")
        );
    }

    /// 验证固定补测策略只选择边界内目标 Provider 的可重试、限流和服务端失败。
    #[test]
    fn select_retry_cases_严格限制来源边界和失败类别() {
        let journal = vec![
            retry_journal_entry(
                1,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-a",
                    "text",
                    "failed",
                    "transport",
                    true,
                ),
            ),
            retry_journal_entry(
                2,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-b",
                    "usage",
                    "failed",
                    "rate_limit",
                    false,
                ),
            ),
            retry_journal_entry(
                3,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-c",
                    "reasoning",
                    "failed",
                    "server_error",
                    false,
                ),
            ),
            retry_journal_entry(
                4,
                failed_retry_probe(
                    "source",
                    "other-provider",
                    "model-d",
                    "text",
                    "failed",
                    "server_error",
                    true,
                ),
            ),
            retry_journal_entry(
                5,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-e",
                    "text",
                    "passed",
                    "server_error",
                    true,
                ),
            ),
            retry_journal_entry(
                6,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-f",
                    "text",
                    "failed",
                    "client_error",
                    false,
                ),
            ),
            retry_journal_entry(
                7,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-g",
                    "stream_interruption",
                    "failed",
                    "transport",
                    true,
                ),
            ),
            retry_journal_entry(
                8,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-h",
                    "diagnostic_invalid_authentication",
                    "failed",
                    "server_error",
                    true,
                ),
            ),
            retry_journal_entry(
                9,
                failed_retry_probe(
                    "source",
                    "provider",
                    "model-i",
                    "text",
                    "failed",
                    "rate_limit",
                    true,
                ),
            ),
        ];

        let selected = select_retry_cases(&journal, "provider", 8);
        assert_eq!(
            selected
                .iter()
                .map(|case| case.source_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            selected
                .iter()
                .map(|case| case.model.as_str())
                .collect::<Vec<_>>(),
            vec!["model-a", "model-b", "model-c"]
        );
    }

    /// 验证补测选择摘要和每个来源稳定键即使一起被改写也会失败关闭。
    #[test]
    fn retry_selection_拒绝摘要与tuple篡改() {
        let selection = retry_selection("source");
        selection.validate().expect("原始补测选择应有效");

        let mut digest_tampered = selection.clone();
        digest_tampered.lineage.selection_sha256 = format!("sha256:{}", "b".repeat(64));
        assert!(
            digest_tampered
                .validate()
                .expect_err("替换选择摘要必须失败")
                .contains("摘要")
        );

        let mut tuple_tampered = selection;
        tuple_tampered.cases[0].model = "other-model".to_owned();
        tuple_tampered.lineage.selection_sha256 = tuple_tampered
            .calculated_sha256()
            .expect("篡改负载仍可重新计算摘要");
        assert!(
            tuple_tampered
                .validate()
                .expect_err("重新摘要也不能换绑来源 tuple")
                .contains("来源稳定键或 tuple 摘要")
        );
    }

    /// 验证来源认证等级与三项版本身份均被序列化并逐字段参与选择摘要。
    #[test]
    fn retry_selection_来源认证与版本字段全部参与摘要() {
        let selection = retry_selection("source");
        let serialized = serde_json::to_value(&selection).expect("补测选择应可序列化");
        let lineage = serialized["lineage"]
            .as_object()
            .expect("补测选择 Lineage 必须是对象");
        for (name, expected) in [
            (
                "sourceAuthentication",
                selection.lineage.source_authentication.as_str(),
            ),
            (
                "sourceResumeSchemaVersion",
                selection.lineage.source_resume_schema_version.as_str(),
            ),
            (
                "sourceHarnessContractId",
                selection.lineage.source_harness_contract_id.as_str(),
            ),
            (
                "sourceReportSchemaVersion",
                selection.lineage.source_report_schema_version.as_str(),
            ),
        ] {
            assert_eq!(
                lineage.get(name).and_then(serde_json::Value::as_str),
                Some(expected)
            );
        }

        let original_digest = selection
            .calculated_sha256()
            .expect("原始补测选择应可计算摘要");
        let mutations: [fn(&mut RetryLineage); 4] = [
            |value| value.source_authentication = AUTHENTICATED_SOURCE_LEVEL.to_owned(),
            |value| value.source_resume_schema_version = RESUME_SCHEMA_VERSION.to_owned(),
            |value| value.source_harness_contract_id = HARNESS_CONTRACT_ID.to_owned(),
            |value| value.source_report_schema_version = RUN_REPORT_SCHEMA_VERSION.to_owned(),
        ];
        for mutate in mutations {
            let mut changed = selection.clone();
            mutate(&mut changed.lineage);
            assert_ne!(
                changed
                    .calculated_sha256()
                    .expect("单字段变化后仍应可计算选择摘要"),
                original_digest,
                "来源认证或版本字段变化必须改变选择摘要"
            );
        }
    }

    /// 验证攻击者即使重算全部公开选择摘要，也不能在不知道凭据时替换补测 tuple。
    #[test]
    fn retry_identity_凭据证明绑定选择摘要() {
        let provider = provider();
        let selection = retry_selection("source");
        let mut options = runtime_options();
        let (provider_id, capabilities) = selection.runtime_shape();
        options
            .apply_retry_runtime_shape(provider_id, capabilities)
            .expect("补测运行形状应有效");
        let mut run =
            RunMetadata::new("retry-run".to_owned(), &options).expect("应能创建补测运行元数据");
        run.retry_lineage = Some(selection.lineage.clone());
        let mut manifest = ResumeManifest::new_retry(run, &options, &[&provider], selection)
            .expect("应能创建补测恢复身份");
        manifest
            .validate_identity(&options, &[&provider])
            .expect("未篡改补测恢复身份应有效");

        let tampered = manifest
            .retry_selection
            .as_mut()
            .expect("补测恢复清单应包含选择");
        tampered.cases[0].model = "replacement-model".to_owned();
        tampered.cases[0].source_stable_key = retry_case_key(
            &tampered.lineage.source_run_id,
            &tampered.cases[0].provider_id,
            &tampered.cases[0].model,
            &tampered.cases[0].protocol,
            &tampered.cases[0].response_mode,
            &tampered.cases[0].capability,
        );
        tampered.cases[0].tuple_key = retry_tuple_key(
            &tampered.cases[0].provider_id,
            &tampered.cases[0].model,
            &tampered.cases[0].protocol,
            &tampered.cases[0].response_mode,
            &tampered.cases[0].capability,
        );
        tampered.lineage.selection_sha256 = tampered
            .calculated_sha256()
            .expect("篡改后的公开选择摘要仍可重算");
        manifest.run.retry_lineage = Some(tampered.lineage.clone());
        manifest.identity.retry_selection_sha256 = Some(tampered.lineage.selection_sha256.clone());

        assert!(
            manifest
                .validate_identity(&options, &[&provider])
                .expect_err("未知凭据的公开摘要重算必须被补测 HMAC 拒绝")
                .contains("恢复身份冲突")
        );
    }

    /// 验证补测恢复清单在日志落盘前拒绝新增 tuple 或伪造已选稳定键的记录。
    #[test]
    fn retry_manifest_提交记录不得扩大冻结选择() {
        let provider = provider();
        let options = runtime_options();
        let selection = retry_selection("source");
        let mut run =
            RunMetadata::new("retry-run".to_owned(), &options).expect("应能创建补测运行元数据");
        run.retry_lineage = Some(selection.lineage.clone());
        let mut manifest = ResumeManifest::new_retry(run, &options, &[&provider], selection)
            .expect("应能创建补测恢复清单");

        let allowed = failed_retry_probe(
            "retry-run",
            "provider",
            "model",
            "text",
            "failed",
            "configuration",
            true,
        );
        manifest
            .commit_probe(1, allowed)
            .expect("冻结清单中的唯一 tuple 应可提交");

        let extra = failed_retry_probe(
            "retry-run",
            "provider",
            "extra-model",
            "text",
            "failed",
            "server_error",
            true,
        );
        assert!(
            manifest
                .commit_probe(2, extra)
                .expect_err("选择之外的 tuple 必须在提交前拒绝")
                .contains("选择清单之外")
        );

        let mut forged = failed_retry_probe(
            "retry-run",
            "provider",
            "extra-model",
            "text",
            "failed",
            "server_error",
            true,
        );
        forged.stable_key = retry_case_key(
            "retry-run",
            "provider",
            "model",
            "openai_responses",
            "buffered",
            "text",
        );
        assert!(
            manifest
                .validate_probe_scope(&forged)
                .expect_err("只伪造稳定键不能换绑 tuple 字段")
                .contains("身份不一致")
        );
    }

    /// 验证补测恢复 Sidecar 缺失、被替换或被篡改时只读失败且绝不自动重建。
    #[test]
    fn retry_selection_sidecar_恢复只读拒绝缺失替换和篡改() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-retry-sidecar-{}-{unique}",
            std::process::id()
        ));
        let store =
            ReportStore::create(&output_root, "retry-run").expect("应能创建补测 Sidecar 测试目录");
        let provider = provider();
        let selection = retry_selection("source");
        let mut options = runtime_options();
        let (provider_id, capabilities) = selection.runtime_shape();
        options
            .apply_retry_runtime_shape(provider_id, capabilities)
            .expect("补测运行形状应有效");
        let mut run =
            RunMetadata::new("retry-run".to_owned(), &options).expect("应能创建补测运行元数据");
        run.retry_lineage = Some(selection.lineage.clone());
        let manifest = ResumeManifest::new_retry(run, &options, &[&provider], selection.clone())
            .expect("应能创建补测恢复清单");
        let sidecar_path = store.run_dir().join("retry-selection.json");

        assert!(
            store
                .load_and_verify_retry_selection_sidecar(&manifest, &[&provider])
                .expect_err("缺失 Sidecar 必须失败")
                .contains("独立选择清单")
        );
        assert!(!sidecar_path.exists(), "缺失 Sidecar 不得被自动创建");

        let different = retry_selection("different-source");
        store
            .write_retry_selection(&different, &[&provider])
            .expect("应能写入另一份有效 Sidecar");
        assert!(
            store
                .load_and_verify_retry_selection_sidecar(&manifest, &[&provider])
                .expect_err("被替换的有效 Sidecar 必须失败")
                .contains("不一致")
        );

        store
            .write_retry_selection(&selection, &[&provider])
            .expect("应能恢复原始 Sidecar");
        let original = fs::read(&sidecar_path).expect("应能读取原始 Sidecar");
        let mut tampered: serde_json::Value =
            serde_json::from_slice(&original).expect("Sidecar 应是有效 JSON");
        tampered["lineage"]["selectionSha256"] =
            serde_json::json!(format!("sha256:{}", "b".repeat(64)));
        fs::write(
            &sidecar_path,
            serde_json::to_vec_pretty(&tampered).expect("应能编码篡改 Sidecar"),
        )
        .expect("应能写入篡改 Sidecar");
        assert!(
            store
                .load_and_verify_retry_selection_sidecar(&manifest, &[&provider])
                .expect_err("篡改 Sidecar 必须失败")
                .contains("不一致")
        );
        assert_eq!(
            fs::read(&sidecar_path).expect("失败后 Sidecar 应保留原字节"),
            serde_json::to_vec_pretty(&tampered).expect("应能重建篡改字节"),
            "只读核对不得覆盖篡改 Sidecar"
        );

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理补测 Sidecar 测试目录");
    }

    /// 验证补测目标拒绝写入只读来源及其子树，同时允许在来源祖先输出根创建兄弟目录。
    #[test]
    fn retry_target_隔离来源并允许祖先根下兄弟目录() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-retry-target-{}-{unique}",
            std::process::id()
        ));
        let source_root = temporary_root.join("runs");
        let source =
            ReportStore::create(&source_root, "source-run").expect("应能创建只读来源测试目录");

        for (index, forbidden_root) in [
            source.run_dir().to_path_buf(),
            source.run_dir().join("fixtures"),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                source
                    .create_retry_target(&forbidden_root, &format!("forbidden-{index}"))
                    .err()
                    .expect("补测目标不能等于或位于来源内")
                    .contains("不能等于或位于只读来源运行目录内")
            );
        }

        let target = source
            .create_retry_target(&source_root, "retry-run")
            .expect("来源位于输出根子目录时应允许创建兄弟补测目录");
        let canonical_source_root =
            fs::canonicalize(&source_root).expect("应能规范化补测输出祖先根");
        assert!(
            target
                .run_dir()
                .parent()
                .is_some_and(|parent| paths_equal(parent, &canonical_source_root)),
            "补测目标必须是输出根的直属兄弟运行目录"
        );
        assert!(source.run_dir().is_dir(), "目标创建不得改变来源目录");
        target
            .complete_retry_target_setup()
            .expect("完整目标测试应能清除失败关闭标记");

        drop(target);
        drop(source);
        fs::remove_dir_all(&temporary_root).expect("应能清理补测目标隔离测试目录");
    }

    /// 验证 Resume、Harness 与 Result 的完整版本组合只接受 v5/v14/v9 和 v6/v15/v10。
    #[test]
    fn retry_source_schema_覆盖resume_harness_result完整组合矩阵() {
        let provider = provider();
        let options = runtime_options();
        let mut run = RunMetadata::new("source".to_owned(), &options).expect("应能创建来源元数据");
        run.finished_at = Some(timestamp().expect("应能生成完成时间"));
        let mut manifest =
            ResumeManifest::new(run.clone(), &options, &[&provider]).expect("应能创建来源清单");
        let record = failed_configuration_probe("buffered");
        manifest
            .commit_probe(1, record.clone())
            .expect("应能提交来源事实");
        manifest.finished = true;
        let mut report = RunReport::new(run);
        report.providers =
            vec![ProviderRecord::from_provider(&provider).expect("应能创建 Provider 快照")];
        report.probes = vec![record];
        report.refresh_summary();
        let report_value = serde_json::to_value(&report).expect("应能序列化来源报告");

        for resume_schema in [RETRY_SOURCE_RESUME_SCHEMA_VERSION, RESUME_SCHEMA_VERSION] {
            for harness_contract in [RETRY_SOURCE_HARNESS_CONTRACT_ID, HARNESS_CONTRACT_ID] {
                for result_schema in [
                    RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION,
                    RUN_REPORT_SCHEMA_VERSION,
                ] {
                    let mut candidate_manifest = manifest.clone();
                    candidate_manifest.identity.schema_version = resume_schema.to_owned();
                    candidate_manifest.identity.harness_contract_id = harness_contract.to_owned();
                    let valid = matches!(
                        (resume_schema, harness_contract, result_schema),
                        (
                            RETRY_SOURCE_RESUME_SCHEMA_VERSION,
                            RETRY_SOURCE_HARNESS_CONTRACT_ID,
                            RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION,
                        ) | (
                            RESUME_SCHEMA_VERSION,
                            HARNESS_CONTRACT_ID,
                            RUN_REPORT_SCHEMA_VERSION,
                        )
                    );
                    let pair = candidate_manifest.retry_source_report_schema();
                    if !matches!(
                        (resume_schema, harness_contract),
                        (
                            RETRY_SOURCE_RESUME_SCHEMA_VERSION,
                            RETRY_SOURCE_HARNESS_CONTRACT_ID,
                        ) | (RESUME_SCHEMA_VERSION, HARNESS_CONTRACT_ID)
                    ) {
                        assert!(
                            pair.expect_err("交叉 Resume 与 Harness 必须失败")
                                .contains("版本组合不受支持")
                        );
                        assert!(!valid);
                        continue;
                    }

                    let expected_report_schema =
                        pair.expect("合法 Resume 与 Harness 应形成报告版本");
                    let mut candidate_report = report_value.clone();
                    candidate_report["schemaVersion"] = serde_json::json!(result_schema);
                    let bytes = serde_json::to_vec(&candidate_report).expect("应能编码候选报告");
                    let validation = validate_stored_run_report(
                        &bytes,
                        &candidate_manifest,
                        &[&provider],
                        &[expected_report_schema],
                    );
                    if valid {
                        validation.expect("两组完整合法版本组合必须通过");
                    } else {
                        assert!(
                            validation
                                .err()
                                .expect("交叉 Result 版本必须失败")
                                .contains("最终报告 schema 不受支持")
                        );
                    }
                }
            }
        }
    }

    /// 验证 legacy 完成基础来源默认失败关闭，只有调用方显式 opt-in 才能读取。
    #[test]
    fn legacy基础来源_默认拒绝且仅显式opt_in接受() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-legacy-default-reject-{}-{unique}",
            std::process::id()
        ));
        let provider = provider();
        let mut record = failed_retry_probe(
            "base-run",
            "provider",
            "model",
            "text",
            "failed",
            "configuration",
            true,
        );
        record.attempts = 0;
        let (run_dir, _) =
            write_completed_retry_test_run(&output_root, "base-run", &provider, record, None, true);
        let store = ReportStore::open_recovery_source(&run_dir).expect("应能打开 legacy 基础来源");
        assert!(
            store
                .load_retry_source_manifest(&[&provider], false)
                .err()
                .expect("未显式 opt-in 时必须拒绝 legacy 基础来源")
                .contains("schema 不受支持：5")
        );
        let accepted = store
            .load_retry_source_manifest(&[&provider], true)
            .expect("显式 opt-in 后应接受结构完整的 legacy 基础来源");
        assert_eq!(
            accepted.identity.schema_version,
            RETRY_SOURCE_RESUME_SCHEMA_VERSION
        );
        assert_eq!(
            accepted.identity.harness_contract_id,
            RETRY_SOURCE_HARNESS_CONTRACT_ID
        );

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理 legacy 默认拒绝测试目录");
    }

    /// 验证当前与显式 legacy 来源的四项 Lineage 摘要都等于实际来源原始字节。
    #[tokio::test]
    async fn retry_selection_lineage_四项摘要等于来源原始字节() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-selection-digests-{}-{unique}",
            std::process::id()
        ));
        let provider = provider();
        for legacy_source in [false, true] {
            let run_id = if legacy_source {
                "legacy-source"
            } else {
                "current-source"
            };
            let mut record = failed_retry_probe(
                run_id,
                "provider",
                "model",
                "text",
                "failed",
                "configuration",
                true,
            );
            record.attempts = 0;
            let (run_dir, executable_sha256) = write_completed_retry_test_run(
                &output_root,
                run_id,
                &provider,
                record,
                None,
                legacy_source,
            );
            let store = ReportStore::open_recovery_source(&run_dir).expect("应能打开完成选择来源");
            let manifest = store
                .load_retry_source_manifest(&[&provider], legacy_source)
                .expect("应能按明确策略加载完成选择来源");
            let selection = store
                .create_retry_selection(&manifest, &[&provider], "provider", 1, &executable_sha256)
                .await
                .expect("应能从完成来源构造精确补测选择");
            for (actual, relative) in [
                (
                    selection.lineage.source_resume_sha256.as_str(),
                    "resume.json",
                ),
                (
                    selection.lineage.source_journal_sha256.as_str(),
                    "sanitized-logs/progress.jsonl",
                ),
                (
                    selection.lineage.source_result_sha256.as_str(),
                    "result.json",
                ),
                (
                    selection.lineage.source_redaction_report_sha256.as_str(),
                    "redaction-report.json",
                ),
            ] {
                let expected = sha256_digest(
                    &fs::read(run_dir.join(relative)).expect("应能读取来源摘要对应原始字节"),
                );
                assert_eq!(actual, expected, "来源摘要必须绑定原始字节：{relative}");
            }
            drop(store);
        }
        fs::remove_dir_all(&output_root).expect("应能清理选择摘要测试目录");
    }

    /// 验证合法 v6 来源仅机械改写公开版本字段时，legacy opt-in 仍由独立凭据证明域拒绝降级。
    #[tokio::test]
    async fn retry_source_v6机械降级为legacy仍因凭据证明域失败() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-mechanical-legacy-downgrade-{}-{unique}",
            std::process::id()
        ));
        let provider = provider();
        let mut record = failed_retry_probe(
            "base-run",
            "provider",
            "model",
            "text",
            "failed",
            "server_error",
            true,
        );
        record.attempts = 0;
        record
            .normalized_error
            .as_mut()
            .expect("测试零请求记录应包含错误证据")
            .kind = "configuration".to_owned();
        let (run_dir, executable_sha256) = write_completed_retry_test_run(
            &output_root,
            "base-run",
            &provider,
            record,
            None,
            false,
        );

        let resume_path = run_dir.join("resume.json");
        let mut resume: serde_json::Value =
            serde_json::from_slice(&fs::read(&resume_path).expect("应能读取当前 v6 Resume"))
                .expect("当前 v6 Resume 应是有效 JSON");
        let original_credential_proof =
            resume["identity"]["providers"][0]["credentialProof"].clone();
        resume["identity"]["schemaVersion"] = serde_json::json!(RETRY_SOURCE_RESUME_SCHEMA_VERSION);
        resume["identity"]["harnessContractId"] =
            serde_json::json!(RETRY_SOURCE_HARNESS_CONTRACT_ID);
        let resume_object = resume.as_object_mut().expect("Resume 根必须是对象");
        resume_object.remove("journalTailMac");
        resume_object.remove("stateProofs");
        resume_object.remove("completionArtifactSeal");
        assert_eq!(
            resume["identity"]["providers"][0]["credentialProof"], original_credential_proof,
            "机械降级不得替换原 v6 凭据证明"
        );
        fs::write(
            &resume_path,
            serde_json::to_vec_pretty(&resume).expect("应能编码机械降级 Resume"),
        )
        .expect("应能写入机械降级 Resume");

        let result_path = run_dir.join("result.json");
        let mut result: serde_json::Value =
            serde_json::from_slice(&fs::read(&result_path).expect("应能读取当前 v10 Result"))
                .expect("当前 v10 Result 应是有效 JSON");
        result["schemaVersion"] = serde_json::json!(RETRY_SOURCE_RUN_REPORT_SCHEMA_VERSION);
        fs::write(
            &result_path,
            serde_json::to_vec_pretty(&result).expect("应能编码机械降级 Result"),
        )
        .expect("应能写入机械降级 Result");

        let journal_path = run_dir.join("sanitized-logs/progress.jsonl");
        let legacy_journal = fs::read_to_string(&journal_path)
            .expect("应能读取当前认证 Journal")
            .lines()
            .map(|line| {
                let mut entry: serde_json::Value =
                    serde_json::from_str(line).expect("Journal 行应是有效 JSON");
                let object = entry.as_object_mut().expect("Journal 行必须是对象");
                object.remove("previousMac");
                object.remove("recordMac");
                serde_json::to_string(&entry).expect("应能编码机械降级 Journal 行")
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&journal_path, format!("{legacy_journal}\n")).expect("应能写入机械降级 Journal");

        let store =
            ReportStore::open_recovery_source(&run_dir).expect("应能打开机械降级后的来源目录");
        let manifest = store
            .load_retry_source_manifest(&[&provider], true)
            .expect("机械降级的公开结构应能进入凭据域验证");
        let error = store
            .create_retry_selection(&manifest, &[&provider], "provider", 1, &executable_sha256)
            .await
            .expect_err("原 v6 凭据证明不能伪装为 legacy v1 证明");
        assert!(
            error.contains("Provider 配置、凭据或租户身份与当前配置不一致"),
            "必须由凭据证明域差异拒绝机械降级，实际错误：{error}"
        );

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理机械降级测试目录");
    }

    /// 验证离线合并的 opt-in 只适用于基础来源，补测来源始终必须使用当前事实认证版本。
    #[tokio::test]
    async fn consolidation_legacy补测来源即使opt_in也拒绝() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-legacy-retry-reject-{}-{unique}",
            std::process::id()
        ));
        let provider = provider();
        let mut base_record = failed_retry_probe(
            "base-run",
            "provider",
            "model",
            "text",
            "failed",
            "configuration",
            true,
        );
        base_record.attempts = 0;
        let (base_dir, base_executable_sha256) = write_completed_retry_test_run(
            &temporary_root.join("base"),
            "base-run",
            &provider,
            base_record,
            None,
            true,
        );
        let base_store =
            ReportStore::open_recovery_source(&base_dir).expect("应能打开 legacy 基础来源");
        let base_manifest = base_store
            .load_retry_source_manifest(&[&provider], true)
            .expect("显式 opt-in 应接受 legacy 基础来源");
        let selection = base_store
            .create_retry_selection(
                &base_manifest,
                &[&provider],
                "provider",
                1,
                &base_executable_sha256,
            )
            .await
            .expect("应能从 legacy 基础来源创建精确选择");
        drop(base_store);

        let mut retry_record = failed_retry_probe(
            "retry-run",
            "provider",
            "model",
            "text",
            "failed",
            "configuration",
            true,
        );
        retry_record.attempts = 0;
        let (retry_dir, _) = write_completed_retry_test_run(
            &temporary_root.join("retry"),
            "retry-run",
            &provider,
            retry_record,
            Some(selection),
            true,
        );

        let error = consolidate_retry_runs(
            &base_dir,
            &retry_dir,
            &temporary_root.join("consolidated"),
            &[&provider],
            true,
        )
        .await
        .expect_err("legacy opt-in 不得放宽补测来源认证要求");
        assert!(
            error.contains("恢复清单 schema 不受支持：5"),
            "补测来源必须在读取时拒绝 legacy Schema，实际错误：{error}"
        );

        fs::remove_dir_all(&temporary_root).expect("应能清理 legacy 补测拒绝测试目录");
    }

    /// 验证完成来源即使新增内容安全的文件，也会按固定布局拒绝临时或未知产物。
    #[test]
    fn completed_source_拒绝安全未知产物而非依赖脱敏差异() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-unknown-completed-artifact-{}-{unique}",
            std::process::id()
        ));
        let provider = provider();
        let mut record = failed_configuration_probe("buffered");
        record.attempts = 0;
        let (run_dir, _) =
            write_completed_retry_test_run(&output_root, "run", &provider, record, None, false);
        fs::write(run_dir.join("orphan.tmp"), b"safe orphan\n")
            .expect("应能注入不含敏感内容的未知产物");

        let store = ReportStore::open_recovery_source(&run_dir).expect("应能打开完成来源");
        let error = store
            .load_retry_source_manifest(&[&provider], false)
            .err()
            .expect("固定完成布局必须拒绝未知产物");
        assert!(
            error.contains("未知产物") || error.contains("临时产物"),
            "必须由固定布局门禁明确拒绝，实际错误：{error}"
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理未知产物测试目录");
    }

    /// 验证 Windows Store 持有期间完整目录链不能被重命名替换，销毁后句柄会释放。
    #[cfg(windows)]
    #[test]
    fn report_store_windows_固定输出根运行目录与固定子目录() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-directory-pin-{}-{unique}",
            std::process::id()
        ));
        let output_root = temporary_root.join("runs");
        let store = ReportStore::create(&output_root, "run").expect("应能创建目录固定测试运行");
        for (index, path) in [
            output_root.clone(),
            store.run_dir().to_path_buf(),
            store.run_dir().join("fixtures"),
            store.run_dir().join("sanitized-logs"),
        ]
        .into_iter()
        .enumerate()
        {
            let moved = path.with_extension(format!("moved-{index}"));
            match fs::rename(&path, &moved) {
                Err(error) => assert!(
                    matches!(error.raw_os_error(), Some(5 | 32 | 33)),
                    "目录固定应返回 Windows 访问或共享冲突，实际：{error}"
                ),
                Ok(()) => {
                    fs::rename(&moved, &path).expect("失败断言前应恢复被错误移动的目录");
                    panic!("Store 存活期间目录仍可被重命名：{}", path.display());
                }
            }
        }
        drop(store);
        let moved_output = temporary_root.join("runs-after-drop");
        fs::rename(&output_root, &moved_output).expect("Store 销毁后目录固定句柄必须释放");
        fs::remove_dir_all(&temporary_root).expect("应能清理目录固定测试目录");
    }

    /// 验证 Windows 目录身份来自句柄返回的卷序列号和 128 位文件标识，而非可碰撞时间属性。
    #[cfg(windows)]
    #[test]
    fn windows_directory_identity_从句柄读取真实文件标识() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-directory-identity-{}-{unique}",
            std::process::id()
        ));
        let first_path = temporary_root.join("first");
        let second_path = temporary_root.join("second");
        fs::create_dir_all(&first_path).expect("应能创建首个文件标识测试目录");
        fs::create_dir(&second_path).expect("应能创建第二个文件标识测试目录");
        let first = open_pinned_windows_directory(&first_path, "首个文件标识测试目录")
            .expect("应能打开首个文件标识测试目录");
        let second = open_pinned_windows_directory(&second_path, "第二个文件标识测试目录")
            .expect("应能打开第二个文件标识测试目录");
        let first_identity = windows_object_identity_from_handle(&first, "首个测试目录")
            .expect("应能从首个目录句柄取得真实文件标识");
        let second_identity = windows_object_identity_from_handle(&second, "第二个测试目录")
            .expect("应能从第二个目录句柄取得真实文件标识");
        assert_eq!(
            first_identity.volume_serial_number, second_identity.volume_serial_number,
            "同一临时根下目录应位于同一文件系统卷"
        );
        assert_ne!(
            first_identity.file_id, second_identity.file_id,
            "不同目录必须具有不同的 128 位文件标识"
        );
        drop((first, second));
        fs::remove_dir_all(&temporary_root).expect("应能清理文件标识测试目录");
    }

    /// 验证 Windows 事实文件使用 128 位文件标识，并通过共享模式排斥并发写入者。
    #[cfg(windows)]
    #[test]
    fn report_store_windows_事实文件固定身份并拒绝写共享() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-file-identity-{}-{unique}",
            std::process::id()
        ));
        let output_root = temporary_root.join("runs");
        let store = ReportStore::create(&output_root, "run").expect("应能创建事实文件固定测试运行");
        let first_path = store.run_dir().join("result.json");
        let second_path = store.run_dir().join("resume.json");
        fs::write(&first_path, b"first\n").expect("应能写入首个事实文件");
        fs::write(&second_path, b"second\n").expect("应能写入第二个事实文件");

        let (first, _, first_identity) = store
            .open_stable_run_file(
                Path::new("result.json"),
                StableFileAccess::ReadOnly,
                StableFileCreation::Existing,
                64,
                Some(6),
                None,
                "首个 Windows 事实文件",
            )
            .expect("应能稳定打开首个事实文件");
        let (_, _, second_identity) = store
            .open_stable_run_file(
                Path::new("resume.json"),
                StableFileAccess::ReadOnly,
                StableFileCreation::Existing,
                64,
                Some(7),
                None,
                "第二个 Windows 事实文件",
            )
            .expect("应能稳定打开第二个事实文件");
        assert_eq!(
            first_identity.windows.volume_serial_number,
            second_identity.windows.volume_serial_number,
            "同一运行目录下事实文件应位于同一文件系统卷"
        );
        assert_ne!(
            first_identity.windows.file_id, second_identity.windows.file_id,
            "不同事实文件必须具有不同的 128 位文件标识"
        );
        let write_error = OpenOptions::new()
            .write(true)
            .open(&first_path)
            .expect_err("稳定事实文件句柄存活期间必须拒绝其他写入者");
        assert!(
            matches!(write_error.raw_os_error(), Some(5 | 32 | 33)),
            "并发写入应返回 Windows 访问或共享冲突，实际：{write_error}"
        );

        drop(first);
        drop(store);
        fs::remove_dir_all(&temporary_root).expect("应能清理 Windows 事实文件固定测试目录");
    }

    /// 验证 Unix 最终文件符号链接不会被 ReportStore 跟随到运行目录外。
    #[cfg(unix)]
    #[test]
    fn report_store_unix_拒绝最终文件符号链接() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-final-symlink-{}-{unique}",
            std::process::id()
        ));
        let output_root = temporary_root.join("runs");
        fs::create_dir_all(&temporary_root).expect("应能创建最终链接测试根目录");
        let outside = temporary_root.join("outside.json");
        fs::write(&outside, b"outside\n").expect("应能写入运行目录外文件");
        let store = ReportStore::create(&output_root, "run").expect("应能创建最终链接测试运行");
        std::os::unix::fs::symlink(&outside, store.run_dir().join("result.json"))
            .expect("应能创建最终文件符号链接");

        let error = store
            .read_bounded_run_file(Path::new("result.json"), 64, "最终链接事实文件")
            .expect_err("最终文件符号链接必须被拒绝");
        assert!(
            error.contains("符号链接") || error.contains("链接") || error.contains("安全打开"),
            "错误必须来自 no-follow 最终节点门禁，实际：{error}"
        );
        assert_eq!(
            fs::read(&outside).expect("运行目录外文件应仍可读取"),
            b"outside\n",
            "拒绝最终链接不得改写运行目录外文件"
        );

        drop(store);
        fs::remove_dir_all(&temporary_root).expect("应能清理最终链接测试目录");
    }

    /// 验证 Unix 固定运行目录或固定子目录被换出后，后续事实访问会失败关闭。
    #[cfg(unix)]
    #[test]
    fn report_store_unix_固定目录换出后拒绝访问() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-directory-swap-{}-{unique}",
            std::process::id()
        ));
        let output_root = temporary_root.join("runs");

        let run_store = ReportStore::create(&output_root, "run").expect("应能创建运行目录换出测试");
        let run_path = run_store.run_dir().to_path_buf();
        let moved_run = output_root.join("run-held");
        fs::rename(&run_path, &moved_run).expect("Unix 应允许换出已打开运行目录");
        fs::create_dir_all(run_path.join("fixtures")).expect("应能创建替代 Fixture 目录");
        fs::create_dir_all(run_path.join("sanitized-logs")).expect("应能创建替代脱敏日志目录");
        fs::write(run_path.join("resume.json"), b"replacement\n")
            .expect("应能写入替代运行事实文件");
        let run_error = run_store
            .read_bounded_run_file(Path::new("resume.json"), 64, "换出后的运行事实文件")
            .expect_err("运行目录被换出后必须拒绝访问替代目录");
        assert!(
            run_error.contains("运行目录身份发生变化"),
            "必须由固定运行目录身份门禁拒绝，实际：{run_error}"
        );
        drop(run_store);

        let child_store =
            ReportStore::create(&output_root, "child").expect("应能创建固定子目录换出测试");
        let logs = child_store.run_dir().join("sanitized-logs");
        let moved_logs = child_store.run_dir().join("sanitized-logs-held");
        fs::rename(&logs, &moved_logs).expect("Unix 应允许换出已打开固定子目录");
        fs::create_dir(&logs).expect("应能创建替代脱敏日志目录");
        fs::write(logs.join("progress.jsonl"), b"replacement\n").expect("应能写入替代日志");
        let child_error = child_store
            .read_bounded_run_file(
                Path::new("sanitized-logs/progress.jsonl"),
                64,
                "换出后的提交日志",
            )
            .expect_err("固定子目录被换出后必须拒绝访问替代目录");
        assert!(
            child_error.contains("脱敏日志目录身份发生变化"),
            "必须由固定子目录身份门禁拒绝，实际：{child_error}"
        );

        drop(child_store);
        fs::remove_dir_all(&temporary_root).expect("应能清理固定目录换出测试目录");
    }

    /// 验证 Unix 只读来源锁的最终符号链接被拒绝且不会触碰链接目标。
    #[cfg(unix)]
    #[test]
    fn recovery_source_unix_拒绝运行锁最终符号链接() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-source-lock-symlink-{}-{unique}",
            std::process::id()
        ));
        let run_dir = temporary_root.join("source");
        fs::create_dir_all(run_dir.join("fixtures")).expect("应能创建来源 Fixture 目录");
        fs::create_dir_all(run_dir.join("sanitized-logs")).expect("应能创建来源脱敏日志目录");
        let outside = temporary_root.join("outside.lock");
        fs::write(&outside, b"").expect("应能创建运行目录外锁目标");
        std::os::unix::fs::symlink(&outside, run_dir.join(".keencode-live-test.lock"))
            .expect("应能创建来源锁符号链接");

        let error = ReportStore::open_recovery_source(&run_dir)
            .err()
            .expect("只读来源锁最终符号链接必须被拒绝");
        assert!(
            error.contains("只读恢复来源运行锁"),
            "错误必须标识只读来源运行锁，实际：{error}"
        );
        assert_eq!(
            fs::read(&outside).expect("运行目录外锁目标应仍可读取"),
            b"",
            "拒绝来源锁链接不得改写或锁定外部目标"
        );

        fs::remove_dir_all(&temporary_root).expect("应能清理来源锁链接测试目录");
    }

    /// 验证 Unix Journal 最终节点发生 ABA 换出时，追加和截断只作用于原稳定句柄。
    #[cfg(unix)]
    #[test]
    fn report_store_unix_journal_aba不写入或截断外部目标() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-journal-aba-{}-{unique}",
            std::process::id()
        ));
        let output_root = temporary_root.join("runs");
        fs::create_dir_all(&temporary_root).expect("应能创建 Journal ABA 测试根目录");
        let outside = temporary_root.join("outside.jsonl");
        fs::write(&outside, b"outside-content\n").expect("应能写入运行目录外 Journal 目标");
        let store =
            ReportStore::create(&output_root, "run").expect("应能创建 Journal ABA 测试运行");
        let checkpoint = store.run_dir().join("sanitized-logs/progress.jsonl");

        fs::write(&checkpoint, b"original\n").expect("应能写入原始追加目标");
        let (mut append_file, _, append_identity) = store
            .open_stable_run_file(
                Path::new("sanitized-logs/progress.jsonl"),
                StableFileAccess::Append,
                StableFileCreation::Existing,
                64,
                Some(9),
                None,
                "Journal ABA 追加句柄",
            )
            .expect("应能打开 Journal 稳定追加句柄");
        fs::remove_file(&checkpoint).expect("应能换出原始追加目录项");
        std::os::unix::fs::symlink(&outside, &checkpoint).expect("应能注入外部追加链接");
        assert!(
            store
                .verify_stable_run_file_identity(
                    Path::new("sanitized-logs/progress.jsonl"),
                    &append_identity,
                    "Journal ABA 追加复核",
                )
                .is_err(),
            "最终目录项被换出后身份复核必须失败"
        );
        append_file
            .write_all(b"held-only\n")
            .and_then(|_| append_file.sync_all())
            .expect("原稳定追加句柄仍应只写入已换出的 inode");
        assert_eq!(
            fs::read(&outside).expect("外部追加目标应仍可读取"),
            b"outside-content\n",
            "稳定追加句柄不得跟随后来注入的外部链接"
        );
        drop(append_file);
        fs::remove_file(&checkpoint).expect("应能移除外部追加链接");

        fs::write(&checkpoint, b"repair-tail").expect("应能写入原始修复目标");
        let (repair_file, _, repair_identity) = store
            .open_stable_run_file(
                Path::new("sanitized-logs/progress.jsonl"),
                StableFileAccess::ReadWrite,
                StableFileCreation::Existing,
                64,
                Some(11),
                None,
                "Journal ABA 修复句柄",
            )
            .expect("应能打开 Journal 稳定修复句柄");
        fs::remove_file(&checkpoint).expect("应能换出原始修复目录项");
        std::os::unix::fs::symlink(&outside, &checkpoint).expect("应能注入外部修复链接");
        assert!(
            store
                .verify_stable_run_file_identity(
                    Path::new("sanitized-logs/progress.jsonl"),
                    &repair_identity,
                    "Journal ABA 修复复核",
                )
                .is_err(),
            "修复前最终目录项被换出后身份复核必须失败"
        );
        repair_file
            .set_len(0)
            .and_then(|_| repair_file.sync_all())
            .expect("原稳定修复句柄仍应只截断已换出的 inode");
        assert_eq!(
            fs::read(&outside).expect("外部修复目标应仍可读取"),
            b"outside-content\n",
            "稳定修复句柄不得跟随后来注入的外部链接"
        );

        drop(repair_file);
        drop(store);
        fs::remove_dir_all(&temporary_root).expect("应能清理 Journal ABA 测试目录");
    }

    /// 验证离线合并只替换精确 tuple，并拒绝独立选择或基础来源内容被改写。
    #[tokio::test]
    async fn consolidate_retry_runs_精确覆盖并拒绝来源篡改() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-retry-consolidation-{}-{unique}",
            std::process::id()
        ));
        let base_root = temporary_root.join("base");
        let retry_root = temporary_root.join("retry");
        let consolidated_root = temporary_root.join("consolidated");
        let provider = provider();

        let mut base_record = failed_retry_probe(
            "base-run",
            "provider",
            "model",
            "text",
            "failed",
            "configuration",
            true,
        );
        base_record.attempts = 0;
        let base_source_stable_key = base_record.stable_key.clone();
        let (base_dir, base_executable_sha256) = write_completed_retry_test_run(
            &base_root,
            "base-run",
            &provider,
            base_record,
            None,
            true,
        );

        let base_store =
            ReportStore::open_recovery_source(&base_dir).expect("应能只读打开基础来源");
        let base_manifest = base_store
            .load_retry_source_manifest(&[&provider], true)
            .expect("应能加载上一版完成来源");
        let selection = base_store
            .create_retry_selection(
                &base_manifest,
                &[&provider],
                "provider",
                1,
                &base_executable_sha256,
            )
            .await
            .expect("应能从基础失败事实构建精确选择");
        assert_eq!(selection.cases.len(), 1);
        assert_eq!(selection.cases[0].source_stable_key, base_source_stable_key);
        drop(base_store);

        let mut retry_record = failed_retry_probe(
            "retry-run",
            "provider",
            "model",
            "text",
            "failed",
            "configuration",
            false,
        );
        retry_record.attempts = 0;
        let retry_stable_key = retry_record.stable_key.clone();
        let (retry_dir, _) = write_completed_retry_test_run(
            &retry_root,
            "retry-run",
            &provider,
            retry_record,
            Some(selection.clone()),
            false,
        );

        let consolidated_dir = consolidate_retry_runs(
            &base_dir,
            &retry_dir,
            &consolidated_root,
            &[&provider],
            true,
        )
        .await
        .expect("合法基础运行和补测运行应可离线合并");
        let consolidated: serde_json::Value = serde_json::from_slice(
            &fs::read(consolidated_dir.join("consolidated-result.json"))
                .expect("应能读取离线合并报告"),
        )
        .expect("离线合并报告应是有效 JSON");
        assert_eq!(
            consolidated["schemaVersion"].as_str(),
            Some(CONSOLIDATED_REPORT_SCHEMA_VERSION)
        );
        assert_eq!(
            consolidated["probes"].as_array().map(Vec::len),
            Some(6),
            "基础运行必须携带三协议双模式的完整矩阵"
        );
        assert_eq!(consolidated["probes"][0]["artifactSource"], "retry");
        assert_eq!(
            consolidated["probes"][0]["sourceStableKey"],
            base_source_stable_key
        );
        assert_eq!(
            consolidated["probes"][0]["record"]["stableKey"],
            retry_stable_key
        );
        assert_eq!(consolidated["summary"]["failed"], 6);
        for artifact in [
            "compatibility-matrix.md",
            "summary.md",
            "redaction-report.json",
        ] {
            assert!(
                consolidated_dir.join(artifact).is_file(),
                "离线合并缺少产物：{artifact}"
            );
        }

        let base_result_path = base_dir.join("result.json");
        let original_base_result = fs::read(&base_result_path).expect("应能读取基础报告");
        let mut duplicated_base: serde_json::Value =
            serde_json::from_slice(&original_base_result).expect("基础报告应是有效 JSON");
        let duplicated_probe = duplicated_base["probes"][0].clone();
        duplicated_base["probes"]
            .as_array_mut()
            .expect("基础报告 probes 应为数组")
            .push(duplicated_probe);
        let duplicated_records =
            serde_json::from_value::<Vec<ProbeRecord>>(duplicated_base["probes"].clone())
                .expect("重复 Probe 数组仍应可解析");
        duplicated_base["summary"] =
            serde_json::to_value(SummaryRecord::from_probes(&duplicated_records))
                .expect("应能重算重复 Probe 汇总");
        fs::write(
            &base_result_path,
            serde_json::to_vec_pretty(&duplicated_base).expect("应能编码重复 Probe 基础报告"),
        )
        .expect("应能写入重复 Probe 基础报告");
        assert!(
            consolidate_retry_runs(
                &base_dir,
                &retry_dir,
                &consolidated_root,
                &[&provider],
                true,
            )
            .await
            .expect_err("合并输入必须在选择替换前拒绝重复基础事实")
            .contains("重复探测稳定键")
        );
        fs::write(&base_result_path, &original_base_result).expect("应能恢复原始基础报告");

        for forbidden_root in [&base_dir, &retry_dir] {
            assert!(
                consolidate_retry_runs(&base_dir, &retry_dir, forbidden_root, &[&provider], true,)
                    .await
                    .expect_err("离线合并目标不能等于任一只读来源")
                    .contains("不能等于或位于只读来源运行目录内")
            );
        }
        let ancestor_output =
            consolidate_retry_runs(&base_dir, &retry_dir, &temporary_root, &[&provider], true)
                .await
                .expect("两份来源位于输出祖先下时应允许创建兄弟合并目录");
        assert!(ancestor_output.is_dir());

        let mut transient_base_result: serde_json::Value =
            serde_json::from_slice(&original_base_result).expect("基础报告应能用于瞬时替换测试");
        transient_base_result["catalogs"] = serde_json::json!([{
            "providerId": "provider",
            "status": "success",
            "attempts": 1,
            "latencyMs": 0,
            "pages": 1,
            "rawCount": 1,
            "invalidCount": 0,
            "discoveredModels": ["transient-model"],
            "candidates": [{
                "model": "transient-model",
                "configured": false,
                "discovered": true,
                "explicit": false,
                "frozenFromResume": false
            }],
            "normalizedError": null
        }]);
        let mut transient_base_result_bytes =
            serde_json::to_vec_pretty(&transient_base_result).expect("应能编码瞬时替换报告");
        transient_base_result_bytes.push(b'\n');
        let snapshot_consumption_output = temporary_root.join("snapshot-consumption-output");
        let snapshot_consumption_error = consolidate_retry_runs_with_hooks(
            &base_dir,
            &retry_dir,
            &snapshot_consumption_output,
            &[&provider],
            true,
            |_| Ok(()),
            |base_store, _| {
                fs::write(
                    base_store.run_dir().join("result.json"),
                    &transient_base_result_bytes,
                )
                .map_err(|error| format!("无法注入瞬时基础报告替换：{error}"))
            },
            |base_store, _| {
                fs::write(
                    base_store.run_dir().join("result.json"),
                    &original_base_result,
                )
                .map_err(|error| format!("无法恢复瞬时基础报告替换：{error}"))
            },
            |_, _| Ok(()),
        )
        .await
        .expect_err("实际消费的报告字节与初始快照不同时必须失败");
        assert!(
            snapshot_consumption_error.contains("实际消费字节与已经校验的来源快照不一致"),
            "必须由消费字节摘要绑定拒绝瞬时替换，实际错误：{snapshot_consumption_error}"
        );
        assert_eq!(
            fs::read(&base_result_path).expect("应能复核恢复后的基础报告"),
            original_base_result,
            "瞬时替换测试必须在错误返回前恢复基础来源"
        );
        assert!(
            !snapshot_consumption_output.exists(),
            "消费字节摘要失败必须发生在创建合并目标之前"
        );

        let base_summary_path = base_dir.join("summary.md");
        let original_base_summary = fs::read(&base_summary_path).expect("应能快照基础来源摘要报告");
        let source_change_output = temporary_root.join("source-change-output");
        let source_change_error = consolidate_retry_runs_with_hooks(
            &base_dir,
            &retry_dir,
            &source_change_output,
            &[&provider],
            true,
            |_| Ok(()),
            |_, _| Ok(()),
            |_, _| Ok(()),
            |base_store, _| {
                fs::write(base_store.run_dir().join("summary.md"), b"# changed\n")
                    .map_err(|error| format!("无法注入来源中途变化：{error}"))
            },
        )
        .await
        .expect_err("合并完成前来源内容变化必须失败关闭");
        assert!(source_change_error.contains("来源内容发生变化"));
        assert!(source_change_error.contains("失败关闭标记"));
        let retained_targets = fs::read_dir(&source_change_output)
            .expect("来源变化后应保留合并输出根")
            .map(|entry| entry.expect("应能读取失败关闭目标目录项").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert_eq!(
            retained_targets.len(),
            1,
            "来源变化应只保留一个失败关闭目标"
        );
        let retained_target = &retained_targets[0];
        assert!(
            retained_target
                .join(RECOVERY_INCOMPLETE_MARKER_FILE)
                .is_file(),
            "来源变化后的离线合并目标必须真实保留失败关闭标记"
        );
        let retained_open_error = ReportStore::open_resume(retained_target)
            .err()
            .expect("失败关闭的离线合并目标不得作为普通恢复运行打开");
        assert!(retained_open_error.contains("未完整验证的隔离恢复副本"));
        fs::write(&base_summary_path, original_base_summary).expect("应能恢复基础来源摘要报告");

        let swapped_output = temporary_root.join("swapped-output");
        let original_output = temporary_root.join("swapped-output-original");
        let external_output = temporary_root.join("swapped-output-external");
        fs::create_dir(&swapped_output).expect("应能创建待替换合并输出根");
        fs::create_dir(&external_output).expect("应能创建外部合并输出目录");
        let anchor_error = consolidate_retry_runs_with_hooks(
            &base_dir,
            &retry_dir,
            &swapped_output,
            &[&provider],
            true,
            |_| {
                fs::rename(&swapped_output, &original_output)
                    .map_err(|error| format!("无法替换合并输出根：{error}"))?;
                create_directory_link(&external_output, &swapped_output);
                Ok(())
            },
            |_, _| Ok(()),
            |_, _| Ok(()),
            |_, _| Ok(()),
        )
        .await
        .expect_err("输出根锚点在创建前被联接替换时必须拒绝");
        assert!(
            anchor_error.contains("无法替换合并输出根")
                || anchor_error.contains("重解析点")
                || anchor_error.contains("文件系统身份发生变化"),
            "目录 pin 或锚点复核必须拒绝输出根替换，实际错误：{anchor_error}"
        );
        if original_output.exists() {
            remove_directory_link(&swapped_output);
            fs::rename(&original_output, &swapped_output).expect("应能恢复原合并输出根");
        } else {
            assert!(
                swapped_output.is_dir(),
                "目录 pin 直接阻止 rename 时原输出根必须保持不变"
            );
        }
        assert!(
            fs::read_dir(&external_output)
                .expect("应能检查外部合并目录")
                .next()
                .is_none(),
            "锚点变化必须在外部目录写入前失败"
        );

        let retry_selection_path = retry_dir.join("retry-selection.json");
        let original_retry_selection =
            fs::read(&retry_selection_path).expect("应能读取独立补测选择清单");
        let mut tampered_selection: serde_json::Value =
            serde_json::from_slice(&original_retry_selection).expect("补测选择应是有效 JSON");
        tampered_selection["lineage"]["selectionSha256"] =
            serde_json::json!(format!("sha256:{}", "b".repeat(64)));
        fs::write(
            &retry_selection_path,
            serde_json::to_vec_pretty(&tampered_selection).expect("应能序列化篡改选择"),
        )
        .expect("应能写入篡改选择");
        let selection_error = consolidate_retry_runs(
            &base_dir,
            &retry_dir,
            &consolidated_root,
            &[&provider],
            true,
        )
        .await
        .expect_err("独立选择清单被改写时必须拒绝");
        assert!(
            selection_error.contains("事实产物未通过 Resume 封印"),
            "应由完成态封印先拒绝选择篡改，实际错误：{selection_error}"
        );
        fs::write(&retry_selection_path, original_retry_selection)
            .expect("应能恢复独立补测选择清单");

        let mut base_result = fs::read(&base_result_path).expect("应能读取基础报告");
        base_result.push(b'\n');
        fs::write(&base_result_path, base_result).expect("应能写入内容摘要变化");
        let result_error = consolidate_retry_runs(
            &base_dir,
            &retry_dir,
            &consolidated_root,
            &[&provider],
            true,
        )
        .await
        .expect_err("基础来源任一内容摘要变化时必须拒绝");
        assert!(
            result_error.contains("事实产物未通过 Resume 封印")
                || result_error.contains("确定性重建")
                || result_error.contains("内容摘要"),
            "应由完成态封印先拒绝最终报告篡改，实际错误：{result_error}"
        );

        fs::remove_dir_all(&temporary_root).expect("应能清理离线合并测试目录");
    }

    /// 验证 Resume 状态保持原样时，任何 Journal 事实改写都会先被链式 Provider MAC 拒绝。
    #[test]
    fn load_resume_manifest_状态未改仍拒绝journal事实篡改() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-journal-mac-tamper-{}-{unique}",
            std::process::id()
        ));
        let store =
            ReportStore::create(&output_root, "run").expect("应能创建 Journal MAC 测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结候选模型");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入初始已认证 Resume");
        let mut record = passed_text_probe("buffered");
        let sequence = store
            .append_probe("run", &mut record, &[&provider])
            .expect("应能写入已认证 Journal 记录");
        manifest
            .commit_probe(sequence, record)
            .expect("应能提交 Resume 记录");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入已提交 Resume");
        let run_dir = store.run_dir().to_path_buf();
        drop(store);

        let journal_path = run_dir.join("sanitized-logs/progress.jsonl");
        let journal_text = fs::read_to_string(&journal_path).expect("应能读取 Journal");
        let mut entry: serde_json::Value =
            serde_json::from_str(journal_text.trim_end()).expect("Journal 应是有效 JSONL");
        entry["record"]["latencyMs"] = serde_json::json!(999_u64);
        fs::write(
            &journal_path,
            format!(
                "{}\n",
                serde_json::to_string(&entry).expect("篡改 Journal 应可序列化")
            ),
        )
        .expect("应能模拟改写 Journal 事实");

        let resumed = ReportStore::open_resume(&run_dir).expect("应能重新取得运行锁");
        let error = resumed
            .load_resume_manifest(&[&provider])
            .err()
            .expect("Journal 事实改写必须在调和前失败");
        assert!(
            error.contains("Provider 凭据认证"),
            "应由 Journal 链式 MAC 拒绝，实际错误：{error}"
        );
        drop(resumed);
        fs::remove_dir_all(&output_root).expect("应能清理 Journal MAC 测试目录");
    }

    /// 验证保留原始 stateProofs 时，typed Manifest 核心的每个顶层字段变化都会由状态证明拒绝。
    #[test]
    fn resume_state_proofs_保留原证明逐顶层字段拒绝typed核心篡改() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-state-proof-fields-{}-{unique}",
            std::process::id()
        ));
        let store =
            ReportStore::create(&output_root, "run").expect("应能创建状态证明逐字段测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结候选模型");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入初始已认证 Resume");
        let mut record = failed_configuration_probe("buffered");
        record.attempts = 0;
        let sequence = store
            .append_probe("run", &mut record, &[&provider])
            .expect("应能写入已认证 Journal");
        manifest
            .commit_probe(sequence, record)
            .expect("应能提交 Resume 记录");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入带原始状态证明的 Resume");

        let resume_path = store.run_dir().join("resume.json");
        let original_bytes = fs::read(&resume_path).expect("应能读取原始已认证 Resume");
        let original: serde_json::Value =
            serde_json::from_slice(&original_bytes).expect("原始 Resume 应是有效 JSON");
        let original_proofs = original["stateProofs"].clone();
        assert!(
            original_proofs
                .as_array()
                .is_some_and(|proofs| !proofs.is_empty()),
            "原始 Resume 必须包含至少一个 Provider 状态证明"
        );
        let stable_key = original["records"]
            .as_object()
            .and_then(|records| records.keys().next())
            .cloned()
            .expect("原始 Resume 必须包含一条记录");

        let mut identity = original.clone();
        identity["identity"]["adapterVersion"] = serde_json::json!("tampered-version");
        let mut run = original.clone();
        run["run"]["runtimeCommit"] = serde_json::json!("tampered-commit");
        let mut candidate_sets = original.clone();
        candidate_sets["candidateSets"]["provider"] = serde_json::json!(["model", "other-model"]);
        let mut records = original.clone();
        records["records"][stable_key.as_str()]["latencyMs"] = serde_json::json!(777_u64);
        let mut journal_sequence = original.clone();
        journal_sequence["journalSequence"] = serde_json::json!(sequence + 1);
        let mut journal_tail_mac = original.clone();
        journal_tail_mac["journalTailMac"] =
            serde_json::json!(format!("hmac-sha256:{}", "1".repeat(64)));
        let mut finished = original.clone();
        finished["finished"] = serde_json::json!(true);
        let mut retry_selection_field = original.clone();
        retry_selection_field["retrySelection"] =
            serde_json::to_value(retry_selection("source")).expect("应能序列化测试补测选择");
        let mut completion_artifact_seal = original.clone();
        completion_artifact_seal["completionArtifactSeal"] = serde_json::json!({
            "schemaVersion": FACT_AUTHENTICATION_SCHEMA_VERSION,
            "journalSequence": sequence,
            "journalTailMac": original["journalTailMac"].clone(),
            "artifacts": [],
        });

        for (field, tampered) in [
            ("identity", identity),
            ("run", run),
            ("candidateSets", candidate_sets),
            ("records", records),
            ("journalSequence", journal_sequence),
            ("journalTailMac", journal_tail_mac),
            ("finished", finished),
            ("retrySelection", retry_selection_field),
            ("completionArtifactSeal", completion_artifact_seal),
        ] {
            assert_eq!(
                tampered["stateProofs"], original_proofs,
                "篡改 {field} 时必须逐字保留原始 stateProofs"
            );
            fs::write(
                &resume_path,
                serde_json::to_vec_pretty(&tampered).expect("应能编码逐字段篡改 Resume"),
            )
            .expect("应能写入逐字段篡改 Resume");
            let error = store
                .load_resume_manifest(&[&provider])
                .err()
                .expect("保留原始状态证明的 typed 核心篡改必须失败");
            assert!(
                error.contains("typed 状态核心未通过当前 Provider 凭据认证"),
                "字段 {field} 必须由状态证明拒绝，实际错误：{error}"
            );
        }

        fs::write(&resume_path, original_bytes).expect("应能恢复原始已认证 Resume");
        store
            .load_resume_manifest(&[&provider])
            .expect("恢复原始字节后 Resume 必须重新通过验证");
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理状态证明逐字段测试目录");
    }

    /// 验证最终产物写出后、完成封印生成前的确定性篡改必然失败关闭。
    #[test]
    fn finalize_completed_产物写出后篡改不得生成完成封印() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-before-seal-tamper-{}-{unique}",
            std::process::id()
        ));
        let (store, provider, manifest, report) = empty_completion_state(&output_root, "run");
        let summary_path = store.run_dir().join("summary.md");
        let error = store
            .finalize_completed_with_hooks(
                &report,
                &manifest,
                &[&provider],
                || {
                    fs::write(&summary_path, b"# tampered\n")
                        .map_err(|error| format!("无法注入封印前产物篡改：{error}"))
                },
                || Ok(()),
            )
            .expect_err("封印生成前的产物篡改必须失败关闭");
        assert!(
            error.contains("完成态产物与内存生成的预期字节不一致"),
            "必须在生成完成封印时拒绝磁盘产物变化，实际错误：{error}"
        );
        let persisted: ResumeManifest = serde_json::from_slice(
            &fs::read(store.run_dir().join("resume.json")).expect("应能读取失败后的恢复清单"),
        )
        .expect("失败后的恢复清单仍应是有效 JSON");
        assert!(!persisted.finished, "封印前失败后恢复清单必须保持未完成");
        assert!(
            persisted.completion_artifact_seal.is_none(),
            "封印前失败后不得留下完成态产物封印"
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理封印前篡改测试目录");
    }

    /// 验证最终 Resume 写出后、返回前回读前的同长度篡改必然被摘要拒绝。
    #[test]
    fn finalize_completed_resume写出后篡改由最终回读拒绝() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-resume-readback-tamper-{}-{unique}",
            std::process::id()
        ));
        let (store, provider, manifest, report) = empty_completion_state(&output_root, "run");
        let resume_path = store.run_dir().join("resume.json");
        let error = store
            .finalize_completed_with_hooks(
                &report,
                &manifest,
                &[&provider],
                || Ok(()),
                || {
                    let mut bytes = fs::read(&resume_path)
                        .map_err(|error| format!("无法读取待篡改最终 Resume：{error}"))?;
                    let whitespace = bytes
                        .iter()
                        .position(|byte| *byte == b' ')
                        .ok_or_else(|| "最终 Resume 缺少可替换空白".to_owned())?;
                    bytes[whitespace] = b'\t';
                    fs::write(&resume_path, bytes)
                        .map_err(|error| format!("无法注入最终 Resume 同长度篡改：{error}"))
                },
            )
            .expect_err("最终 Resume 写出后的同长度篡改必须失败关闭");
        assert!(
            error.contains("最终完成恢复清单回读摘要"),
            "必须由最终 Resume 回读摘要拒绝同长度篡改，实际错误：{error}"
        );
        let run_dir = store.run_dir().to_path_buf();
        drop(store);
        let reopened = ReportStore::open_resume(&run_dir).expect("应能重新取得篡改运行的目录锁");
        let reopen_error = reopened
            .load_resume_manifest(&[&provider])
            .err()
            .expect("同长度篡改的最终 Resume 在重新打开后仍必须失败");
        assert!(
            reopen_error.contains("唯一规范 JSON 编码"),
            "重新打开必须拒绝非规范 Resume 字节，实际错误：{reopen_error}"
        );
        drop(reopened);
        fs::remove_dir_all(&output_root).expect("应能清理 Resume 回读篡改测试目录");
    }

    /// 验证完成恢复运行的独立 Lineage Sidecar 被改写后会由 Resume 产物封印拒绝。
    #[test]
    fn completed_recovery_lineage_sidecar_篡改后由seal拒绝() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-lineage-seal-{}-{unique}",
            std::process::id()
        ));
        let (store, provider, mut manifest, mut report) =
            empty_completion_state(&output_root, "recovered-run");
        let recovery_executable_sha256 = manifest.identity.executable_sha256.clone();
        let source_executable_sha256 = sha256_digest(b"synthetic-source-executable");
        assert_ne!(source_executable_sha256, recovery_executable_sha256);
        let lineage = RecoveryLineage {
            schema_version: RECOVERY_LINEAGE_SCHEMA_VERSION.to_owned(),
            source_run_id: "source-run".to_owned(),
            source_runtime_commit: "source-commit".to_owned(),
            source_executable_sha256,
            source_resume_sha256: sha256_digest(b"synthetic-source-resume"),
            source_journal_sha256: sha256_digest(b"synthetic-source-journal"),
            source_resume_schema_version: None,
            source_harness_contract_id: None,
            recovery_executable_sha256,
            recovered_at: "2026-01-01T00:00:00Z".to_owned(),
            imported_records: 0,
            imported_fixtures: 0,
            parent: None,
            rerun_records: Vec::new(),
            policy: DIRECT_RECOVERY_POLICY.to_owned(),
        };
        manifest.run.recovery_lineage = Some(lineage.clone());
        store
            .write_json("recovery-lineage.json", &lineage, &[&provider])
            .expect("应能写入恢复 Lineage Sidecar");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能把恢复 Lineage 写入已认证 Resume");
        report.run = manifest.run.clone();
        report.run.finished_at = Some(timestamp().expect("应能生成恢复完成时间"));
        store
            .finalize_completed(&report, &manifest, &[&provider])
            .expect("未篡改的恢复运行应能完成并生成封印");
        let run_dir = store.run_dir().to_path_buf();
        drop(store);

        let sidecar_path = run_dir.join("recovery-lineage.json");
        let mut bytes = fs::read(&sidecar_path).expect("应能读取已封印的恢复 Lineage Sidecar");
        let whitespace = bytes
            .iter()
            .position(|byte| *byte == b' ')
            .expect("恢复 Lineage Sidecar 应包含可替换空白");
        bytes[whitespace] = b'\t';
        fs::write(&sidecar_path, bytes).expect("应能注入恢复 Lineage Sidecar 同长度篡改");

        let source = ReportStore::open_recovery_source(&run_dir)
            .expect("应能以只读来源方式打开被篡改的完成运行");
        let error = source
            .load_retry_source_manifest(&[&provider], false)
            .err()
            .expect("恢复 Lineage Sidecar 篡改必须在来源复用前失败");
        assert!(
            error.contains("Resume 封印校验"),
            "必须由完成态 Resume 封印拒绝 Lineage Sidecar 篡改，实际错误：{error}"
        );
        drop(source);
        fs::remove_dir_all(&output_root).expect("应能清理恢复 Lineage 封印测试目录");
    }

    /// 验证选择不变且所有公开事实产物同步改写时，Resume 与 Consolidation 都无法越过状态 MAC。
    #[tokio::test]
    async fn retry_fact_authentication_拒绝selection不变的全套自洽篡改() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "keencode-provider-full-fact-tamper-{}-{unique}",
            std::process::id()
        ));
        let provider = provider();
        let mut base_record = failed_retry_probe(
            "base-run",
            "provider",
            "model",
            "text",
            "failed",
            "configuration",
            true,
        );
        base_record.attempts = 0;
        let (base_dir, base_executable_sha256) = write_completed_retry_test_run(
            &temporary_root.join("base"),
            "base-run",
            &provider,
            base_record,
            None,
            true,
        );
        let base_store =
            ReportStore::open_recovery_source(&base_dir).expect("应能打开 legacy 基础来源");
        let base_manifest = base_store
            .load_retry_source_manifest(&[&provider], true)
            .expect("应能加载 legacy 基础来源");
        let selection = base_store
            .create_retry_selection(
                &base_manifest,
                &[&provider],
                "provider",
                1,
                &base_executable_sha256,
            )
            .await
            .expect("应能创建固定补测选择");
        drop(base_store);

        let retry_record = passed_text_probe("buffered");
        let (retry_dir, _) = write_completed_retry_test_run(
            &temporary_root.join("retry"),
            "run",
            &provider,
            retry_record,
            Some(selection),
            false,
        );
        let selection_path = retry_dir.join("retry-selection.json");
        let selection_before = fs::read(&selection_path).expect("应能快照未改写的选择清单");
        let resume_path = retry_dir.join("resume.json");
        let mut resume: serde_json::Value =
            serde_json::from_slice(&fs::read(&resume_path).expect("应能读取当前 Resume"))
                .expect("当前 Resume 应是有效 JSON");
        let stable_key = resume["records"]
            .as_object()
            .and_then(|records| records.keys().next())
            .cloned()
            .expect("补测 Resume 应包含唯一记录");
        resume["records"][stable_key.as_str()]["latencyMs"] = serde_json::json!(777_u64);
        let fixture_relative = resume["records"][stable_key.as_str()]["fixturePaths"][0]
            .as_str()
            .expect("补测记录应引用唯一 Fixture")
            .to_owned();

        let journal_path = retry_dir.join("sanitized-logs/progress.jsonl");
        let mut journal: serde_json::Value = serde_json::from_str(
            fs::read_to_string(&journal_path)
                .expect("应能读取补测 Journal")
                .trim_end(),
        )
        .expect("补测 Journal 应是有效 JSONL");
        journal["record"]["latencyMs"] = serde_json::json!(777_u64);
        fs::write(
            &journal_path,
            format!(
                "{}\n",
                serde_json::to_string(&journal).expect("篡改 Journal 应可序列化")
            ),
        )
        .expect("应能同步改写补测 Journal");

        let result_path = retry_dir.join("result.json");
        let mut result: serde_json::Value =
            serde_json::from_slice(&fs::read(&result_path).expect("应能读取补测结果"))
                .expect("补测结果应是有效 JSON");
        result["probes"][0]["latencyMs"] = serde_json::json!(777_u64);
        fs::write(
            &result_path,
            serde_json::to_vec_pretty(&result).expect("篡改结果应可序列化"),
        )
        .expect("应能同步改写补测结果");

        for relative in [
            fixture_relative.as_str(),
            "compatibility-matrix.md",
            "summary.md",
            "redaction-report.json",
        ] {
            let path = retry_dir.join(relative);
            let mut bytes = fs::read(&path).expect("应能读取待同步改写的事实产物");
            bytes.extend_from_slice(b" \n");
            fs::write(path, bytes).expect("应能同步改写事实产物字节");
        }

        for artifact in resume["completionArtifactSeal"]["artifacts"]
            .as_array_mut()
            .expect("完成态 Resume 应包含产物封印")
        {
            let relative = artifact["path"]
                .as_str()
                .expect("封印项应包含相对路径")
                .to_owned();
            let bytes = fs::read(retry_dir.join(&relative)).expect("应能重算公开产物摘要");
            artifact["sha256"] = serde_json::json!(sha256_digest(&bytes));
        }
        for proof in resume["stateProofs"]
            .as_array_mut()
            .expect("当前 Resume 应包含 Provider 状态证明")
        {
            proof["hmacSha256"] = serde_json::json!(format!("hmac-sha256:{}", "1".repeat(64)));
        }
        fs::write(
            &resume_path,
            serde_json::to_vec_pretty(&resume).expect("篡改 Resume 应可序列化"),
        )
        .expect("应能同步改写 Resume 与公开摘要");
        assert_eq!(
            fs::read(&selection_path).expect("选择清单应保持可读"),
            selection_before,
            "攻击场景必须保持 retry selection 完全不变"
        );

        let resumed = ReportStore::open_resume(&retry_dir).expect("应能打开篡改后的补测目录");
        let resume_error = resumed
            .load_resume_manifest(&[&provider])
            .err()
            .expect("同步改写不能被常规恢复复用");
        assert!(
            resume_error.contains("typed 状态核心"),
            "Resume 必须先拒绝状态 MAC，实际错误：{resume_error}"
        );
        drop(resumed);

        let consolidation_error = consolidate_retry_runs(
            &base_dir,
            &retry_dir,
            &temporary_root.join("consolidated"),
            &[&provider],
            true,
        )
        .await
        .expect_err("同步改写不能被离线合并接受");
        assert!(
            consolidation_error.contains("typed 状态核心"),
            "Consolidation 必须在复用前拒绝状态 MAC，实际错误：{consolidation_error}"
        );
        fs::remove_dir_all(&temporary_root).expect("应能清理全套事实篡改测试目录");
    }

    /// 验证最终 Provider 记录只输出规范格式的加钥配置指纹。
    #[test]
    fn provider_record_配置指纹使用规范hmac() {
        let record = ProviderRecord::from_provider(&provider()).expect("Provider 记录应可创建");
        assert!(valid_hmac_sha256_proof(&record.config_fingerprint));
        let serialized = serde_json::to_value(&record).expect("Provider 记录应可序列化");
        assert_eq!(
            serialized["configFingerprint"].as_str(),
            Some(record.config_fingerprint.as_str())
        );
        assert!(
            serialized["configFingerprint"]
                .as_str()
                .is_some_and(|value| value.starts_with("hmac-sha256:"))
        );
    }

    /// 验证 Markdown 单元格只保留纯文本语义且不会破坏矩阵列结构。
    #[test]
    fn markdown_cell_编码表格分隔符和显示控制字符() {
        assert_eq!(markdown_cell("a|b\nc"), "a&#124;b c");
        assert_eq!(
            markdown_cell(r#"\![]()<>&`|*_"#),
            "&#92;&#33;&#91;&#93;&#40;&#41;&#60;&#62;&#38;&#96;&#124;&#42;&#95;"
        );
        let escaped = markdown_cell("safe\u{001b}]0;owned\u{0007}\u{202e}");
        assert!(!escaped.chars().any(is_dangerous_display_character));
        assert!(escaped.contains("&#92;u&#123;001b&#125;"));
    }

    /// 验证远端 `/models` 返回的恶意 ID 不能在兼容矩阵中形成链接、图片或原始 HTML。
    #[test]
    fn compatibility_matrix_把恶意目录模型id编码为纯文本() {
        let malicious = r#"model\name![pixel](https://attacker.invalid/pixel)<img src=x onerror=alert(1)>&`|*_"#;
        let mut record = probe("text", "buffered", "passed");
        record.model = malicious.to_owned();
        let matrix = compatibility_matrix(&report_with_probes(vec![record]));

        assert!(!matrix.contains(malicious));
        assert!(!matrix.contains("![pixel]("));
        assert!(!matrix.contains("<img"));
        assert!(!matrix.contains("https://"));
        assert!(matrix.contains(
            "model&#92;name&#33;&#91;pixel&#93;&#40;https&#58;&#47;&#47;attacker&#46;invalid&#47;pixel&#41;&#60;img"
        ));
    }

    /// 验证 JSON Unicode 转义、内嵌 JSON 与原始 Markdown 中的危险显示字符都会被发现。
    #[test]
    fn dangerous_display_scanner_覆盖原始与解码结构() {
        assert_eq!(
            count_artifact_dangerous_display_characters(r#"{"model":"safe\u001b]0;owned\u0007"}"#),
            2
        );
        let embedded = serde_json::json!({
            "body": serde_json::json!({"model": "safe\u{202e}owned"}).to_string()
        })
        .to_string();
        assert!(count_artifact_dangerous_display_characters(&embedded) >= 1);
        assert_eq!(
            count_artifact_dangerous_display_characters("# report\nsafe\u{2067}owned\n"),
            1
        );
    }

    /// 验证写盘前检查能拒绝常见的授权凭据掉码后缀。
    #[test]
    fn contains_masked_credential_suffix_区分授权后缀与普通星号() {
        assert!(contains_masked_credential_suffix("认证失败：****b930"));
        assert!(contains_masked_credential_suffix("api key ***abc-123"));
        assert!(!contains_masked_credential_suffix("Markdown **bold**"));
        assert!(!contains_masked_credential_suffix("单纯遮盖 ****"));
    }

    /// 验证认证 Header、Cookie、长 Token 与绝对路径均由真实扫描器识别。
    #[test]
    fn sensitive_pattern_scanners_拒绝未脱敏产物() {
        assert_eq!(
            count_sensitive_assignments(
                "Authorization: Bearer value\nx-api-key: [REDACTED]",
                &["authorization:", "x-api-key:"]
            ),
            1
        );
        assert_eq!(
            count_sensitive_assignments("{\"cookie\":\"session-value\"}", &["cookie:"]),
            1
        );
        assert_eq!(
            count_sensitive_assignments(
                r#"{"responseBodyUtf8":"{\"set-cookie\":\"session-value\"}"}"#,
                &["set-cookie:"]
            ),
            1
        );
        assert_eq!(
            count_sensitive_assignments(
                r#"{"authorization" : "[REDACTED]", "authorization":"Bearer opaque", "access_token" = "query-secret"}"#,
                &["authorization", "access_token"]
            ),
            2
        );
        assert_eq!(
            count_sensitive_assignments(
                r#"{"authorization":"[REDACTED]opaque-secret"}"#,
                &["authorization"]
            ),
            1
        );
        for smuggled in [
            "access_token: [REDACTED] opaque-secret",
            "Authorization: Bearer [REDACTED]\tsecret",
            r#"authorization: [REDACTED]\"opaque-secret"#,
        ] {
            assert_eq!(
                count_sensitive_assignments(smuggled, &["authorization", "access_token"]),
                1,
                "占位符后的正文不得被终止符判断遮蔽"
            );
        }
        for safe in [
            "authorization: [REDACTED]",
            "authorization: Bearer [REDACTED]\nnext: safe",
            r#"{"authorization":"[REDACTED]","safe":true}"#,
            r#"{\"authorization\":\"[REDACTED]\",\"safe\":true}"#,
        ] {
            assert_eq!(
                count_sensitive_assignments(safe, &["authorization"]),
                0,
                "完整占位符和明确字段终止边界必须保持可写"
            );
        }
        assert_eq!(
            count_sensitive_assignments(
                r#"{"\u0061uthorization":"Bearer opaque", "authorization":"[REDACTED]"}"#,
                &["authorization"]
            ),
            1
        );
        assert_eq!(
            count_sensitive_assignments(
                r#"{"authorization":"[REDACTED]", "authorization":"Bearer duplicate"}"#,
                &["authorization"]
            ),
            1
        );
        assert_eq!(
            count_sensitive_assignments(
                "event: synthetic\nauthorization: Bearer sse-value\ndata: {}\n\n",
                &["authorization"]
            ),
            1
        );
        let synthetic_prefixed_secret = ["sk", "1234567890abcdef"].join("-");
        assert_eq!(
            count_secret_tokens(&format!("token={synthetic_prefixed_secret}")),
            1
        );
        assert_eq!(count_absolute_paths("source=C:\\Users\\example\\file"), 1);
        assert!(count_absolute_paths("source=/root/private/file") >= 1);
        assert_eq!(
            count_absolute_paths(r"source=\\server\share\private\file"),
            1
        );
        assert_eq!(
            count_absolute_paths("request failed for url (https://example.invalid/v1/models)"),
            0
        );
    }

    /// 验证 JSON 转义换行不会伪造 `n:\\`，但换行后的真实盘符路径仍会被发现。
    #[test]
    fn artifact_path_scanner_解码json后区分转义字母与真实盘符() {
        assert_eq!(
            count_artifact_absolute_paths(r#"{"actualText":"safe\n:\\synthetic"}"#),
            0
        );
        assert!(
            count_artifact_absolute_paths(
                r#"{"actualText":"safe\nC:\\Users\\example\\result.txt"}"#
            ) >= 1
        );
    }

    /// 验证 JSONL 和 `responseBodyUtf8` 内嵌 SSE/JSON 都按解码后字符串扫描。
    #[test]
    fn artifact_path_scanner_递归扫描jsonl与内嵌sse() {
        let response_body = concat!(
            "event: response.created\r\n",
            "data: {\"response\":{\"instructions\":\"C:\\\\Users\\\\service\\\\prompt\"}}\r\n",
            "\r\n"
        );
        let first = serde_json::json!({"responseBodyUtf8": response_body}).to_string();
        let second = serde_json::json!({"actualText": "safe\n:\\synthetic"}).to_string();
        let jsonl = format!("{first}\n{second}\n");
        let (signatures, extended_paths, user_paths) = artifact_absolute_path_evidence(&jsonl);
        assert!(signatures.values().sum::<usize>() >= 1);
        assert!(signatures.get("C:\\").is_some_and(|count| *count >= 1));
        assert_eq!(extended_paths, 0);
        assert_eq!(user_paths, 0);
    }

    /// 验证对象键和非结构化 Markdown 仍应应用绝对路径门禁。
    #[test]
    fn artifact_path_scanner_扫描对象键与普通文本() {
        assert!(count_artifact_absolute_paths(r#"{"C:\\Users\\example\\key":"safe"}"#) >= 1);
        assert_eq!(
            count_artifact_absolute_paths("# report\nsource=/home/example/project\n"),
            1
        );
    }

    /// 验证异常远端正文只以长度和 HMAC 进入记录，不会写入原文或旧字段。
    #[test]
    fn actual_text_evidence_序列化不包含异常正文() {
        let provider = provider();
        let raw = "unexpected C:\\Users\\remote\\private.txt";
        let mut record = probe("text", "buffered", "contract_violation");
        record.response = Some(ResponseEvidence {
            response_id_present: true,
            reported_model_redacted: Some("model".to_owned()),
            stop_reason: "completed".to_owned(),
            content_block_types: vec!["text".to_owned()],
            text_block_count: 1,
            reasoning_block_count: 0,
            tool_call_count: 0,
            usage: TokenUsage::default(),
        });
        record.actual_text_evidence = Some(ActualTextEvidence::from_text(
            &provider,
            &record.stable_key,
            raw,
        ));
        let serialized = serde_json::to_string(&record).expect("证据化记录应可序列化");
        assert!(!serialized.contains(raw));
        assert!(!serialized.contains("C:\\\\Users"));
        assert!(!serialized.contains("\"actualText\":"));
        assert!(serialized.contains("\"actualTextEvidence\":"));
        assert!(serialized.contains("hmac-sha256:"));
        assert_eq!(count_artifact_absolute_paths(&serialized), 0);
    }

    /// 验证远端错误说明不进入产物，且恢复拒绝被篡改的长度证据。
    #[test]
    fn error_message_evidence_不含正文且恢复拒绝畸形摘要() {
        let raw = "remote internal prompt C:\\Users\\service\\secret";
        let evidence = ErrorMessageEvidence::from_text(raw);
        let serialized = serde_json::to_string(&evidence).expect("错误说明证据应能序列化");
        assert!(!serialized.contains(raw));
        assert!(!serialized.contains("internal prompt"));
        assert_eq!(evidence.utf8_bytes, raw.len() as u64);
        assert!(!evidence.truncated);
        assert!(!serialized.contains("sha256"));

        let oversized = ErrorMessageEvidence::from_text(&"界".repeat(1_001));
        assert_eq!(oversized.utf8_bytes, 3_000);
        assert!(oversized.truncated);

        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let mut record = failed_configuration_probe("buffered");
        record
            .normalized_error
            .as_mut()
            .expect("测试记录应包含错误")
            .message_evidence
            .utf8_bytes = 4_001;
        let key = record.stable_key();
        assert!(
            validate_probe_record_invariants(&manifest, &key, &record)
                .expect_err("越界错误长度必须阻断恢复")
                .contains("错误说明证据")
        );
    }

    /// 验证三种协议只有按真实字段承载固定 Harness 模板时才可声明纯合成。
    #[test]
    fn synthetic_fixture_按三协议结构验证固定模板() {
        let marker = "KC_OK_0123456789abcdef";
        let request = text_model_request(marker);
        for (protocol, provider_protocol) in [
            ("anthropic_messages", ProviderProtocol::Messages),
            ("openai_chat_completions", ProviderProtocol::ChatCompletions),
            ("openai_responses", ProviderProtocol::Responses),
        ] {
            let request_body = encode_wire_request(provider_protocol, &request, false)
                .expect("测试统一请求应可由三协议编码");
            validate_synthetic_fixture(&synthetic_fixture(protocol, marker, request_body))
                .expect("固定 Harness 模板应通过结构验证");
        }
    }

    /// 验证复杂统一请求只在内存严格往返，磁盘仅接受纯首请求结构摘要。
    #[test]
    fn fixture_v6_复杂统一请求内存门禁且磁盘仅保存首请求结构() {
        let marker = "KC_OK_0123456789abcdef";
        for protocol in all_protocols() {
            for streaming in [false, true] {
                let request = complex_model_request(protocol, marker);
                request.validate().expect("复杂统一请求应满足模型层不变量");
                let semantic_request =
                    serde_json::to_value(&request).expect("复杂统一请求应可规范序列化");
                assert_eq!(
                    strict_semantic_request(&semantic_request).expect("复杂统一请求应可严格往返"),
                    request
                );
                let request_body = encode_wire_request(protocol, &request, streaming)
                    .expect("复杂统一请求应可由目标 Adapter 编码");
                assert!(validate_initial_semantic_request(&request, "model").is_err());

                let initial_request = text_model_request(marker);
                let initial_request_body =
                    encode_wire_request(protocol, &initial_request, streaming)
                        .expect("纯合成首请求应可由目标 Adapter 编码");
                let fixture = ProbeFixtureEnvelope {
                    schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
                    content_sha256: "test-only".to_owned(),
                    payload: ProbeFixturePayload {
                        run_id: "run".to_owned(),
                        stable_key: "stable".to_owned(),
                        provider_id: "provider".to_owned(),
                        model: "model".to_owned(),
                        protocol: protocol_name(protocol).to_owned(),
                        response_mode: if streaming { "streaming" } else { "buffered" }.to_owned(),
                        capability: "tool_result_round_trip".to_owned(),
                        synthetic_marker: Some(marker.to_owned()),
                        synthetic_only: true,
                        exchanges: vec![FixtureExchange {
                            request: FixtureRequestEvidence::SyntheticFirstRequest {
                                semantic_message_count: initial_request.messages.len(),
                                semantic_tool_count: initial_request.tools.len(),
                                wire_top_level_field_count: initial_request_body
                                    .as_object()
                                    .map(serde_json::Map::len)
                                    .unwrap_or(0),
                            },
                            max_event_bytes: 64 * 1024,
                            response_shape: test_response_shape(
                                protocol, None, None, b"", false, false,
                            ),
                            observed_terminal_error: None,
                            expected_outcome: FixtureExchangeOutcome::RequestOnly,
                        }],
                        expected_response: None,
                        expected_actual_text_evidence: None,
                        expected_error: None,
                        expected_cancellation: None,
                        replay: None,
                    },
                };
                validate_fixture_request_binding(&fixture)
                    .expect("纯合成首请求的结构摘要应满足磁盘证据不变量");
                assert!(
                    request_body.is_object(),
                    "复杂请求仍必须完成实际 Adapter 编码"
                );

                let mut smuggled = semantic_request.clone();
                smuggled["tools"][0]["smuggled"] = serde_json::json!(true);
                assert!(
                    strict_semantic_request(&smuggled)
                        .expect_err("嵌套未知字段必须被逐字段往返拒绝")
                        .contains("未知字段")
                );
                let mut missing = semantic_request.clone();
                missing
                    .get_mut("structuredOutput")
                    .and_then(serde_json::Value::as_object_mut)
                    .expect("结构化输出必须是对象")
                    .remove("strict");
                assert!(strict_semantic_request(&missing).is_err());
                let mut body_tampered = fixture.clone();
                match &mut body_tampered.payload.exchanges[0].request {
                    FixtureRequestEvidence::SyntheticFirstRequest {
                        wire_top_level_field_count,
                        ..
                    } => {
                        *wire_top_level_field_count = 0;
                    }
                    FixtureRequestEvidence::SubsequentRequestOmitted { .. } => {
                        panic!("首个测试请求必须保存为纯合成请求")
                    }
                }
                assert!(
                    validate_fixture_request_binding(&body_tampered)
                        .expect_err("空的 Adapter 结构摘要必须失败")
                        .contains("结构摘要")
                );
                let mut oversized = fixture.clone();
                oversized.payload.exchanges[0].max_event_bytes = MAX_FIXTURE_EVENT_BYTES + 1;
                assert!(
                    validate_fixture_request_binding(&oversized)
                        .expect_err("过大的事件解析预算必须失败")
                        .contains("maxEventBytes")
                );
            }
        }
    }

    /// 验证三协议、双响应模式下正文省略、错误证据与取消均可由磁盘策略复核。
    #[test]
    fn fixture_v6_磁盘策略覆盖三协议双模式四类结果() {
        let provider = provider();
        let marker = "KC_OK_0123456789abcdef";
        for protocol in all_protocols() {
            for streaming in [false, true] {
                for case in ["success", "http_error", "adapter_error", "cancellation"] {
                    let request = text_model_request(marker);
                    let request_body = encode_wire_request(protocol, &request, streaming)
                        .expect("测试统一请求应可编码");
                    let (capability, status, content_type, online_outcome, cancellation) =
                        match case {
                            "success" => (
                                "text",
                                Some(200),
                                Some("application/json".to_owned()),
                                FixtureExchangeOutcome::Response {
                                    response: ResponseEvidence {
                                        response_id_present: false,
                                        reported_model_redacted: Some("model".to_owned()),
                                        stop_reason: "stop".to_owned(),
                                        content_block_types: vec!["text".to_owned()],
                                        text_block_count: 1,
                                        reasoning_block_count: 0,
                                        tool_call_count: 0,
                                        usage: TokenUsage::default(),
                                    },
                                    actual_text_evidence: ActualTextEvidence::from_text(
                                        &provider, "stable", marker,
                                    ),
                                },
                                false,
                            ),
                            "http_error" => (
                                "invalid_parameter",
                                Some(400),
                                Some("application/json".to_owned()),
                                FixtureExchangeOutcome::Error {
                                    error: NormalizedError {
                                        kind: "invalid_request".to_owned(),
                                        message_evidence: ErrorMessageEvidence::from_text(
                                            "synthetic invalid",
                                        ),
                                        retryable: false,
                                        http_status: Some(400),
                                    },
                                },
                                false,
                            ),
                            "adapter_error" => (
                                "stream_interruption",
                                Some(200),
                                Some("application/json".to_owned()),
                                FixtureExchangeOutcome::Error {
                                    error: NormalizedError {
                                        kind: "protocol".to_owned(),
                                        message_evidence: ErrorMessageEvidence::from_text(
                                            "synthetic adapter failure",
                                        ),
                                        retryable: false,
                                        http_status: Some(200),
                                    },
                                },
                                false,
                            ),
                            "cancellation" => (
                                "cancellation",
                                None,
                                None,
                                FixtureExchangeOutcome::RequestOnly,
                                true,
                            ),
                            _ => unreachable!("测试结果类型由固定数组限制"),
                        };
                    let exchange = FixtureExchange {
                        request: FixtureRequestEvidence::SyntheticFirstRequest {
                            semantic_message_count: request.messages.len(),
                            semantic_tool_count: request.tools.len(),
                            wire_top_level_field_count: request_body
                                .as_object()
                                .map(serde_json::Map::len)
                                .unwrap_or(0),
                        },
                        max_event_bytes: 64 * 1024,
                        response_shape: test_response_shape(
                            protocol,
                            status,
                            content_type.as_deref(),
                            b"",
                            status.is_some(),
                            false,
                        ),
                        observed_terminal_error: None,
                        expected_outcome: online_outcome.clone(),
                    };
                    let persisted_outcome = if cancellation {
                        FixtureExchangeOutcome::RequestOnly
                    } else {
                        persisted_fixture_replay_outcome(&exchange)
                    };
                    let (expected_response, expected_actual_text_evidence, expected_error) =
                        match &online_outcome {
                            FixtureExchangeOutcome::Response {
                                response,
                                actual_text_evidence,
                            } => (
                                Some(response.clone()),
                                Some(actual_text_evidence.clone()),
                                None,
                            ),
                            FixtureExchangeOutcome::Error { error } => {
                                (None, None, Some(error.clone()))
                            }
                            FixtureExchangeOutcome::ObservedTerminalError { .. } => {
                                panic!("固定测试响应不能只依赖在线传输终态")
                            }
                            FixtureExchangeOutcome::RequestOnly => (None, None, None),
                            FixtureExchangeOutcome::Unavailable { .. } => {
                                panic!("固定测试响应必须可以重放")
                            }
                        };
                    let mut fixture = ProbeFixtureEnvelope {
                        schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
                        content_sha256: String::new(),
                        payload: ProbeFixturePayload {
                            run_id: "run".to_owned(),
                            stable_key: "stable".to_owned(),
                            provider_id: "provider".to_owned(),
                            model: "model".to_owned(),
                            protocol: protocol_name(protocol).to_owned(),
                            response_mode: if streaming { "streaming" } else { "buffered" }
                                .to_owned(),
                            capability: capability.to_owned(),
                            synthetic_marker: Some(marker.to_owned()),
                            synthetic_only: true,
                            exchanges: vec![exchange],
                            expected_response: expected_response.clone(),
                            expected_actual_text_evidence: expected_actual_text_evidence.clone(),
                            expected_error: expected_error.clone(),
                            expected_cancellation: cancellation.then_some(CancellationEvidence {
                                cancel_after_ms: 500,
                                local_future_dropped: true,
                                first_event_received: false,
                                completed_before_cancel: false,
                                observed_latency_ms: 500,
                                remote_termination_proven: false,
                            }),
                            replay: None,
                        },
                    };
                    let replay = fixture_replay_evidence(&fixture, &[persisted_outcome]);
                    fixture.payload.replay = Some(replay.clone());
                    fixture.content_sha256 = fixture_payload_sha256(&fixture.payload)
                        .expect("测试 Fixture 应可计算摘要");
                    let mut record = probe(
                        capability,
                        if streaming { "streaming" } else { "buffered" },
                        "passed",
                    );
                    record.response = expected_response;
                    record.actual_text_evidence = expected_actual_text_evidence;
                    record.normalized_error = expected_error;
                    record.fixture_replay = Some(replay.clone());
                    verify_disk_fixture(&record, &fixture)
                        .expect("固定矩阵 Fixture 必须从磁盘表示完整重算");
                    assert_eq!(
                        replay.status,
                        if cancellation {
                            "not_applicable"
                        } else {
                            "unavailable"
                        }
                    );
                    assert_eq!(replay.exchange_count, 1);
                    assert_eq!(replay.replayed_exchanges, 0);
                }
            }
        }
    }

    /// 验证 Chat 图片工具结果的响应与本地 Adapter 错误组合在正文省略后仍可复核。
    #[test]
    fn fixture_chat图片工具结果_允许响应与unsupported组合() {
        let provider = provider();
        let marker = "KC_OK_0123456789abcdef";
        let response = ResponseEvidence {
            response_id_present: true,
            reported_model_redacted: Some("model".to_owned()),
            stop_reason: "tool_use".to_owned(),
            content_block_types: vec!["tool_call".to_owned()],
            text_block_count: 0,
            reasoning_block_count: 0,
            tool_call_count: 1,
            usage: TokenUsage::default(),
        };
        let actual_text_evidence = ActualTextEvidence::from_text(&provider, "stable", "");
        let unsupported = NormalizedError {
            kind: "unsupported_capability".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text(
                "Chat Completions 不支持图片工具结果",
            ),
            retryable: false,
            http_status: None,
        };
        let exchange = FixtureExchange {
            request: FixtureRequestEvidence::SyntheticFirstRequest {
                semantic_message_count: 1,
                semantic_tool_count: 1,
                wire_top_level_field_count: 4,
            },
            max_event_bytes: 64 * 1024,
            response_shape: test_response_shape(
                ProviderProtocol::ChatCompletions,
                Some(200),
                Some("application/json"),
                br#"{}"#,
                true,
                false,
            ),
            observed_terminal_error: None,
            expected_outcome: FixtureExchangeOutcome::Response {
                response: response.clone(),
                actual_text_evidence: actual_text_evidence.clone(),
            },
        };
        let mut fixture = ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256: String::new(),
            payload: ProbeFixturePayload {
                run_id: "run".to_owned(),
                stable_key: "stable".to_owned(),
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
                protocol: "openai_chat_completions".to_owned(),
                response_mode: "buffered".to_owned(),
                capability: "tool_result_image_round_trip".to_owned(),
                synthetic_marker: Some(marker.to_owned()),
                synthetic_only: true,
                exchanges: vec![exchange.clone()],
                expected_response: Some(response),
                expected_actual_text_evidence: Some(actual_text_evidence),
                expected_error: Some(unsupported),
                expected_cancellation: None,
                replay: None,
            },
        };
        let persisted = persisted_fixture_replay_outcome(&exchange);
        assert!(fixture_chat_image_unsupported(&fixture));
        assert!(fixture_requirement_matches(
            &fixture,
            Some(&exchange),
            Some(&persisted)
        ));
        assert!(fixture_final_outcome_matches(&fixture, Some(&persisted)));
        let replay = fixture_replay_evidence(&fixture, &[persisted]);
        assert_eq!(replay.status, "unavailable");
        assert_eq!(
            replay.reason.as_deref(),
            Some(UNAVAILABLE_RESPONSE_BODY_REASON)
        );
        fixture.payload.replay = Some(replay.clone());
        let mut record = probe(
            "tool_result_image_round_trip",
            "buffered",
            "contract_violation",
        );
        record.response = fixture.payload.expected_response.clone();
        record.actual_text_evidence = fixture.payload.expected_actual_text_evidence.clone();
        record.normalized_error = fixture.payload.expected_error.clone();
        record.fixture_replay = Some(replay);
        verify_disk_fixture(&record, &fixture)
            .expect("Chat 图片工具结果的本地不支持组合应能完成磁盘复核");
    }

    /// 验证取消仅允许最后一个真实在途调用使用 RequestOnly，其他终态不伪造磁盘响应复核。
    #[test]
    fn cancellation_fixture_区分本地丢弃提前终态与传输观察() {
        let provider = provider();
        let protocol = ProviderProtocol::Responses;
        let marker = "KC_OK_0123456789abcdef";
        let request = text_model_request(marker);
        let request_body =
            encode_wire_request(protocol, &request, false).expect("取消测试请求应可编码");
        let synthetic_request = || FixtureRequestEvidence::SyntheticFirstRequest {
            semantic_message_count: request.messages.len(),
            semantic_tool_count: request.tools.len(),
            wire_top_level_field_count: request_body
                .as_object()
                .map(serde_json::Map::len)
                .unwrap_or(0),
        };
        let http_error = NormalizedError {
            kind: "authentication".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text("synthetic authentication failure"),
            retryable: false,
            http_status: Some(401),
        };
        let http_error_exchange = FixtureExchange {
            request: synthetic_request(),
            max_event_bytes: 64 * 1024,
            response_shape: test_response_shape(
                protocol,
                Some(401),
                Some("application/json"),
                br#"{}"#,
                true,
                false,
            ),
            observed_terminal_error: Some(http_error.clone()),
            expected_outcome: FixtureExchangeOutcome::Error {
                error: http_error.clone(),
            },
        };
        let persisted_http_error = persisted_fixture_replay_outcome(&http_error_exchange);
        assert!(matches!(
            persisted_http_error,
            FixtureExchangeOutcome::Unavailable { .. }
        ));

        let failed_cancellation = CancellationEvidence {
            cancel_after_ms: 500,
            local_future_dropped: false,
            first_event_received: false,
            completed_before_cancel: false,
            observed_latency_ms: 10,
            remote_termination_proven: false,
        };
        let mut failed_fixture = ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256: String::new(),
            payload: ProbeFixturePayload {
                run_id: "run".to_owned(),
                stable_key: "stable".to_owned(),
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
                protocol: protocol_name(protocol).to_owned(),
                response_mode: "buffered".to_owned(),
                capability: "cancellation".to_owned(),
                synthetic_marker: Some(marker.to_owned()),
                synthetic_only: true,
                exchanges: vec![http_error_exchange.clone()],
                expected_response: None,
                expected_actual_text_evidence: None,
                expected_error: Some(http_error.clone()),
                expected_cancellation: Some(failed_cancellation),
                replay: None,
            },
        };
        let failed_replay = fixture_replay_evidence(
            &failed_fixture,
            &[persisted_fixture_replay_outcome(&http_error_exchange)],
        );
        failed_fixture.payload.replay = Some(failed_replay.clone());
        let mut failed_record = probe("cancellation", "buffered", "failed");
        failed_record.fixture_replay = Some(failed_replay);
        verify_disk_fixture(&failed_record, &failed_fixture)
            .expect("明确保存在线 HTTP 终态的取消失败应允许磁盘复核为不可用");

        let mut missing_terminal = failed_fixture.clone();
        missing_terminal.payload.exchanges[0].observed_terminal_error = None;
        assert_eq!(
            verify_disk_fixture(&failed_record, &missing_terminal)
                .expect_err("没有显式在线终态的取消失败不得把省略的响应正文声明为不可复核"),
            "取消失败只有显式在线传输终态可以声明为响应不可从磁盘复核"
        );

        let dropped_exchange = FixtureExchange {
            request: FixtureRequestEvidence::SubsequentRequestOmitted {
                reason: OMITTED_SUBSEQUENT_REQUEST_REASON.to_owned(),
            },
            max_event_bytes: 64 * 1024,
            response_shape: test_response_shape(protocol, None, None, b"", false, false),
            observed_terminal_error: None,
            expected_outcome: FixtureExchangeOutcome::RequestOnly,
        };
        let dropped = FixtureExchangeOutcome::RequestOnly;
        let local_cancel = CancellationEvidence {
            cancel_after_ms: 500,
            local_future_dropped: true,
            first_event_received: false,
            completed_before_cancel: false,
            observed_latency_ms: 500,
            remote_termination_proven: false,
        };
        let local_fixture = ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256: String::new(),
            payload: ProbeFixturePayload {
                run_id: "run".to_owned(),
                stable_key: "stable".to_owned(),
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
                protocol: protocol_name(protocol).to_owned(),
                response_mode: "buffered".to_owned(),
                capability: "cancellation".to_owned(),
                synthetic_marker: Some(marker.to_owned()),
                synthetic_only: true,
                exchanges: vec![http_error_exchange, dropped_exchange],
                expected_response: None,
                expected_actual_text_evidence: None,
                expected_error: None,
                expected_cancellation: Some(local_cancel),
                replay: None,
            },
        };
        let replay = fixture_replay_evidence(&local_fixture, &[persisted_http_error, dropped]);
        assert_eq!(replay.status, "unavailable");
        assert_eq!(replay.exchange_count, 2);
        assert_eq!(replay.replayed_exchanges, 0);
        assert_eq!(
            replay.reason.as_deref(),
            Some(UNAVAILABLE_RESPONSE_BODY_REASON)
        );

        let mut options = runtime_options();
        options.max_attempts = 2;
        options.capabilities = BTreeSet::from([ProbeKind::Cancellation]);
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建取消恢复运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建取消恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结取消恢复候选模型");
        let mut resumable = probe("cancellation", "buffered", "passed");
        resumable.attempts = 2;
        resumable.synthetic_marker =
            Some(marker_from_probe_stable_key(&resumable.stable_key, false));
        resumable.assertions = vec![SemanticAssertion::new(
            "local_future_dropped",
            true,
            "取消窗口获胜并释放本地调用",
        )];
        resumable.cancellation = local_fixture.payload.expected_cancellation.clone();
        resumable.fixture_replay = Some(replay.clone());
        resumable.wire_response_shapes = local_fixture
            .payload
            .exchanges
            .iter()
            .map(|exchange| exchange.response_shape.clone())
            .collect();
        let resumable_key = resumable.stable_key();
        validate_reusable_record(&manifest, &resumable_key, &resumable, &[&provider])
            .expect("前轮正文省略且末轮本地取消成功的两次尝试记录应可恢复");

        for invalid_reason in [None, Some("不受支持的不可重放原因".to_owned())] {
            let mut tampered = resumable.clone();
            tampered
                .fixture_replay
                .as_mut()
                .expect("测试记录应包含重放证据")
                .reason = invalid_reason;
            assert!(
                validate_probe_record_invariants(&manifest, &resumable_key, &tampered).is_err(),
                "unavailable 只有固定正文省略原因才允许恢复"
            );
        }

        let mut not_dropped = resumable.clone();
        not_dropped
            .cancellation
            .as_mut()
            .expect("测试记录应包含取消证据")
            .local_future_dropped = false;
        assert!(validate_probe_record_invariants(&manifest, &resumable_key, &not_dropped).is_err());

        let mut completed_before_cancel = resumable.clone();
        completed_before_cancel
            .cancellation
            .as_mut()
            .expect("测试记录应包含取消证据")
            .completed_before_cancel = true;
        assert!(
            validate_probe_record_invariants(&manifest, &resumable_key, &completed_before_cancel)
                .is_err()
        );

        let mut remote_termination_claimed = resumable.clone();
        remote_termination_claimed
            .cancellation
            .as_mut()
            .expect("测试记录应包含取消证据")
            .remote_termination_proven = true;
        assert!(
            validate_probe_record_invariants(
                &manifest,
                &resumable_key,
                &remote_termination_claimed
            )
            .is_err()
        );

        let mut response_added = resumable.clone();
        response_added.response = Some(ResponseEvidence {
            response_id_present: false,
            reported_model_redacted: Some("model".to_owned()),
            stop_reason: "stop".to_owned(),
            content_block_types: vec!["text".to_owned()],
            text_block_count: 1,
            reasoning_block_count: 0,
            tool_call_count: 0,
            usage: TokenUsage::default(),
        });
        response_added.actual_text_evidence = Some(ActualTextEvidence::from_text(
            &provider,
            &resumable_key,
            "synthetic response",
        ));
        assert!(
            validate_probe_record_invariants(&manifest, &resumable_key, &response_added).is_err()
        );

        let mut error_added = resumable.clone();
        error_added.normalized_error = Some(http_error);
        assert!(validate_probe_record_invariants(&manifest, &resumable_key, &error_added).is_err());

        let completed = FixtureExchangeOutcome::Response {
            response: ResponseEvidence {
                response_id_present: false,
                reported_model_redacted: Some("model".to_owned()),
                stop_reason: "stop".to_owned(),
                content_block_types: vec!["text".to_owned()],
                text_block_count: 1,
                reasoning_block_count: 0,
                tool_call_count: 0,
                usage: TokenUsage::default(),
            },
            actual_text_evidence: ActualTextEvidence::from_text(&provider, "stable", marker),
        };
        let completed_exchange = FixtureExchange {
            request: synthetic_request(),
            max_event_bytes: 64 * 1024,
            response_shape: test_response_shape(
                protocol,
                Some(200),
                Some("application/json"),
                br#"{}"#,
                true,
                false,
            ),
            observed_terminal_error: None,
            expected_outcome: completed.clone(),
        };
        let (expected_response, expected_actual_text_evidence) = match &completed {
            FixtureExchangeOutcome::Response {
                response,
                actual_text_evidence,
            } => (response.clone(), actual_text_evidence.clone()),
            _ => panic!("提前完整响应必须重放为 Response"),
        };
        let completed_fixture = ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256: String::new(),
            payload: ProbeFixturePayload {
                run_id: "run".to_owned(),
                stable_key: "stable".to_owned(),
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
                protocol: protocol_name(protocol).to_owned(),
                response_mode: "buffered".to_owned(),
                capability: "cancellation".to_owned(),
                synthetic_marker: Some(marker.to_owned()),
                synthetic_only: true,
                exchanges: vec![completed_exchange],
                expected_response: Some(expected_response),
                expected_actual_text_evidence: Some(expected_actual_text_evidence),
                expected_error: None,
                expected_cancellation: Some(CancellationEvidence {
                    cancel_after_ms: 500,
                    local_future_dropped: false,
                    first_event_received: false,
                    completed_before_cancel: true,
                    observed_latency_ms: 10,
                    remote_termination_proven: false,
                }),
                replay: None,
            },
        };
        let completed_replay = fixture_replay_evidence(
            &completed_fixture,
            &[persisted_fixture_replay_outcome(
                &completed_fixture.payload.exchanges[0],
            )],
        );
        assert_eq!(completed_replay.status, "unavailable");
        assert_eq!(completed_replay.replayed_exchanges, 0);

        let transport_error = NormalizedError {
            kind: "transport".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text("synthetic connection reset"),
            retryable: true,
            http_status: None,
        };
        let transport_exchange = FixtureExchange {
            request: synthetic_request(),
            max_event_bytes: 64 * 1024,
            response_shape: test_response_shape(protocol, None, None, b"", false, false),
            observed_terminal_error: Some(transport_error.clone()),
            expected_outcome: FixtureExchangeOutcome::ObservedTerminalError {
                error: transport_error.clone(),
            },
        };
        let observed = persisted_fixture_replay_outcome(&transport_exchange);
        let transport_fixture = ProbeFixtureEnvelope {
            schema_version: FIXTURE_SCHEMA_VERSION.to_owned(),
            content_sha256: String::new(),
            payload: ProbeFixturePayload {
                run_id: "run".to_owned(),
                stable_key: "stable".to_owned(),
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
                protocol: protocol_name(protocol).to_owned(),
                response_mode: "buffered".to_owned(),
                capability: "cancellation".to_owned(),
                synthetic_marker: Some(marker.to_owned()),
                synthetic_only: true,
                exchanges: vec![transport_exchange],
                expected_response: None,
                expected_actual_text_evidence: None,
                expected_error: Some(transport_error),
                expected_cancellation: Some(CancellationEvidence {
                    cancel_after_ms: 500,
                    local_future_dropped: false,
                    first_event_received: false,
                    completed_before_cancel: false,
                    observed_latency_ms: 10,
                    remote_termination_proven: false,
                }),
                replay: None,
            },
        };
        let transport_replay = fixture_replay_evidence(&transport_fixture, &[observed]);
        assert_eq!(transport_replay.status, "unavailable");
        assert_eq!(transport_replay.replayed_exchanges, 0);
    }

    /// 验证零交换记录也必须满足交换、在线结果和结构证据三向计数一致。
    #[test]
    fn prepare_fixture_零交换拒绝孤立在线结果或结构证据() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-empty-wire-count-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建计数门禁目录");
        let provider = provider();

        let mut outcome_only = probe("text", "buffered", "failed");
        outcome_only.wire_exchange_outcomes = vec![FixtureExchangeOutcome::RequestOnly];
        let outcome_error = store
            .prepare_probe_fixture("run", &mut outcome_only, &[&provider])
            .expect_err("零交换不得携带孤立在线结果");
        assert!(outcome_error.contains("数量不一致"));

        let mut shape_only = probe("text", "buffered", "failed");
        shape_only.wire_response_shapes = vec![test_response_shape(
            ProviderProtocol::Responses,
            None,
            None,
            b"",
            false,
            false,
        )];
        let shape_error = store
            .prepare_probe_fixture("run", &mut shape_only, &[&provider])
            .expect_err("零交换不得携带孤立响应结构证据");
        assert!(shape_error.contains("数量不一致"));

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理计数门禁目录");
    }

    /// 验证任意正文与错误标记在内存门禁阶段失败，磁盘不会创建 Fixture 或 Journal。
    #[test]
    fn prepare_fixture_三协议写盘前拒绝任意正文和错误标记() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-prompt-guard-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建提示词门禁目录");
        let provider = provider();
        let expected = "KC_OK_0123456789abcdef";
        let wrong = "KC_OK_fedcba9876543210";

        for protocol in all_protocols() {
            for prompt in [
                format!("任意用户正文 {expected}"),
                format!("只输出下一行的精确标记，不要添加标点、Markdown、空格或解释：\n{wrong}"),
            ] {
                let mut request = text_model_request(expected);
                request.messages = vec![keencode_model::Message::text(
                    keencode_model::MessageRole::User,
                    prompt,
                )];
                let request_body = encode_wire_request(protocol, &request, false)
                    .expect("测试提示词应可由目标 Adapter 编码");
                let mut record = probe("text", "buffered", "failed");
                record.protocol = protocol_name(protocol).to_owned();
                record.synthetic_marker = Some(expected.to_owned());
                record.wire_exchanges = vec![WireExchange {
                    model_request: request,
                    max_event_bytes: 64 * 1024,
                    request_body,
                    response_status: None,
                    response_content_type: None,
                    response_body: Vec::new(),
                    response_body_truncated: false,
                    response_body_eof_observed: false,
                    terminal_error: None,
                }];
                record.wire_response_shapes = record
                    .wire_exchanges
                    .iter()
                    .map(|exchange| {
                        inspect_wire_response_shape(
                            protocol,
                            exchange.response_status,
                            exchange.response_content_type.as_deref(),
                            &exchange.response_body,
                            exchange.response_body_eof_observed,
                            exchange.response_body_truncated,
                        )
                    })
                    .collect();
                record.wire_exchange_outcomes = vec![FixtureExchangeOutcome::RequestOnly];
                assert!(
                    store
                        .prepare_probe_fixture("run", &mut record, &[&provider])
                        .is_err(),
                    "任意正文或错误标记必须在写盘前失败"
                );
            }
        }

        assert!(
            fs::read_dir(store.run_dir().join("fixtures"))
                .expect("应能读取空 Fixture 目录")
                .next()
                .is_none()
        );
        assert!(!store.checkpoint_path.exists());
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理提示词门禁目录");
    }

    /// 验证多轮首请求可写结构证据，后续远端文本、推理、工具名和参数整体省略。
    #[test]
    fn prepare_fixture_多轮首请求成功且后续请求不落盘() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-subsequent-omission-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建后续省略测试目录");
        let provider = provider();
        let marker = "KC_OK_0123456789abcdef";
        let first_marker = first_turn_marker(marker);
        let first_request = text_model_request(&first_marker);
        let first_body = encode_wire_request(ProviderProtocol::Responses, &first_request, false)
            .expect("多轮首请求应可编码");
        let mut subsequent_request = complex_model_request(ProviderProtocol::Responses, marker);
        subsequent_request
            .messages
            .push(keencode_model::Message::new(
                keencode_model::MessageRole::Assistant,
                vec![
                    ContentBlock::text("REMOTE_TEXT_SENTINEL"),
                    ContentBlock::Reasoning {
                        reasoning: keencode_model::ReasoningContent::new(
                            "REMOTE_REASONING_SENTINEL",
                        ),
                    },
                    ContentBlock::ToolCall {
                        tool_call: keencode_model::ToolCall::new(
                            "REMOTE_CALL_SENTINEL",
                            "REMOTE_TOOL_SENTINEL",
                            serde_json::json!({"argument": "REMOTE_ARGUMENT_SENTINEL"}),
                        ),
                    },
                ],
            ));
        let subsequent_body =
            encode_wire_request(ProviderProtocol::Responses, &subsequent_request, false)
                .expect("带远端历史的后续请求应可编码");
        let mut record = probe("multi_turn", "buffered", "passed");
        record.synthetic_marker = Some(marker.to_owned());
        record.wire_exchanges = vec![
            WireExchange {
                model_request: first_request,
                max_event_bytes: 64 * 1024,
                request_body: first_body,
                response_status: Some(200),
                response_content_type: Some("application/json".to_owned()),
                response_body: Vec::new(),
                response_body_truncated: false,
                response_body_eof_observed: true,
                terminal_error: None,
            },
            WireExchange {
                model_request: subsequent_request,
                max_event_bytes: 64 * 1024,
                request_body: subsequent_body,
                response_status: Some(200),
                response_content_type: Some("application/json".to_owned()),
                response_body: Vec::new(),
                response_body_truncated: false,
                response_body_eof_observed: true,
                terminal_error: None,
            },
        ];
        record.wire_response_shapes = record
            .wire_exchanges
            .iter()
            .map(|exchange| {
                inspect_wire_response_shape(
                    ProviderProtocol::Responses,
                    exchange.response_status,
                    exchange.response_content_type.as_deref(),
                    &exchange.response_body,
                    exchange.response_body_eof_observed,
                    exchange.response_body_truncated,
                )
            })
            .collect();
        record.wire_exchange_outcomes = vec![
            FixtureExchangeOutcome::RequestOnly,
            FixtureExchangeOutcome::RequestOnly,
        ];

        let (_, text) = store
            .prepare_probe_fixture("run", &mut record, &[&provider])
            .expect("多轮结构证据应通过内存门禁")
            .expect("有交换的记录必须生成 Fixture");
        for forbidden in [
            "REMOTE_TEXT_SENTINEL",
            "REMOTE_REASONING_SENTINEL",
            "REMOTE_CALL_SENTINEL",
            "REMOTE_TOOL_SENTINEL",
            "REMOTE_ARGUMENT_SENTINEL",
        ] {
            assert!(!text.contains(forbidden), "后续远端历史不得进入 Fixture");
        }
        let fixture = parse_fixture_envelope(&text).expect("生成的多轮 Fixture 应可解析");
        assert!(matches!(
            fixture.payload.exchanges[0].request,
            FixtureRequestEvidence::SyntheticFirstRequest { .. }
        ));
        assert!(matches!(
            fixture.payload.exchanges[1].request,
            FixtureRequestEvidence::SubsequentRequestOmitted { .. }
        ));

        let mut first_omitted = fixture.clone();
        first_omitted.payload.exchanges[0].request =
            FixtureRequestEvidence::SubsequentRequestOmitted {
                reason: OMITTED_SUBSEQUENT_REQUEST_REASON.to_owned(),
            };
        assert!(validate_fixture_request_binding(&first_omitted).is_err());
        let mut later_persisted = fixture.clone();
        later_persisted.payload.exchanges[1].request =
            FixtureRequestEvidence::SyntheticFirstRequest {
                semantic_message_count: 1,
                semantic_tool_count: 0,
                wire_top_level_field_count: 1,
            };
        assert!(validate_fixture_request_binding(&later_persisted).is_err());
        let mut unsupported_reason = fixture;
        unsupported_reason.payload.exchanges[1].request =
            FixtureRequestEvidence::SubsequentRequestOmitted {
                reason: "不受支持的省略原因".to_owned(),
            };
        assert!(validate_fixture_request_binding(&unsupported_reason).is_err());

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理后续省略测试目录");
    }

    /// 验证三协议首请求中的 assistant、工具、推理历史及元数据不能绕过内存门禁。
    #[test]
    fn prepare_fixture_三协议拒绝历史块和隐藏元数据() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-history-guard-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建历史门禁目录");
        let provider = provider();
        let marker = "KC_OK_0123456789abcdef";

        for protocol in all_protocols() {
            let mut requests = vec![complex_model_request(protocol, marker)];
            let mut metadata_request = text_model_request(marker);
            metadata_request
                .metadata
                .insert("hidden".to_owned(), "REMOTE_METADATA_SENTINEL".to_owned());
            requests.push(metadata_request);
            let reasoning_request = ModelRequest::new(
                "model",
                vec![keencode_model::Message::new(
                    keencode_model::MessageRole::Assistant,
                    vec![ContentBlock::Reasoning {
                        reasoning: keencode_model::ReasoningContent {
                            text: "REMOTE_REASONING_SENTINEL".to_owned(),
                            summary: Some("REMOTE_SUMMARY_SENTINEL".to_owned()),
                            continuation: None,
                        },
                    }],
                )],
            );
            assert!(
                validate_initial_semantic_request(&reasoning_request, "model").is_err(),
                "仅含推理块的 assistant 历史必须由语义门禁直接拒绝"
            );

            for request in requests {
                let request_body = encode_wire_request(protocol, &request, false)
                    .expect("历史门禁请求必须先能按协议编码");
                let mut record = probe("text", "buffered", "failed");
                record.protocol = protocol_name(protocol).to_owned();
                record.synthetic_marker = Some(marker.to_owned());
                record.wire_exchanges = vec![WireExchange {
                    model_request: request,
                    max_event_bytes: 64 * 1024,
                    request_body,
                    response_status: None,
                    response_content_type: None,
                    response_body: Vec::new(),
                    response_body_truncated: false,
                    response_body_eof_observed: false,
                    terminal_error: None,
                }];
                record.wire_response_shapes = record
                    .wire_exchanges
                    .iter()
                    .map(|exchange| {
                        inspect_wire_response_shape(
                            protocol,
                            exchange.response_status,
                            exchange.response_content_type.as_deref(),
                            &exchange.response_body,
                            exchange.response_body_eof_observed,
                            exchange.response_body_truncated,
                        )
                    })
                    .collect();
                record.wire_exchange_outcomes = vec![FixtureExchangeOutcome::RequestOnly];
                assert!(
                    store
                        .prepare_probe_fixture("run", &mut record, &[&provider])
                        .is_err(),
                    "任何首请求远端历史或元数据都必须在写盘前失败"
                );
            }
        }

        assert!(
            fs::read_dir(store.run_dir().join("fixtures"))
                .expect("应能读取历史门禁 Fixture 目录")
                .next()
                .is_none()
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理历史门禁目录");
    }

    /// 验证真实交换无论正文内容如何都只保存在线归一化证据，不保存响应字节。
    #[test]
    fn fixture_exchange_统一省略真实响应正文() {
        let provider = provider();
        let expected = FixtureExchangeOutcome::Response {
            response: ResponseEvidence {
                response_id_present: false,
                reported_model_redacted: Some("model".to_owned()),
                stop_reason: "completed".to_owned(),
                content_block_types: vec!["text".to_owned()],
                text_block_count: 1,
                reasoning_block_count: 0,
                tool_call_count: 0,
                usage: TokenUsage::default(),
            },
            actual_text_evidence: ActualTextEvidence::from_text(&provider, "stable", "synthetic"),
        };
        let raw = br#"{"credential":"fixture-secret-value"}"#.to_vec();
        let persisted_shape = test_response_shape(
            ProviderProtocol::Responses,
            Some(200),
            Some("application/json; secret=fixture-content-type-secret"),
            &raw,
            true,
            false,
        );
        let persisted = FixtureExchange::from_wire(
            &WireExchange {
                model_request: text_model_request("KC_OK_0123456789abcdef"),
                max_event_bytes: 64 * 1024,
                request_body: serde_json::json!({"input": "synthetic"}),
                response_status: Some(200),
                response_content_type: Some(
                    "application/json; secret=fixture-content-type-secret".to_owned(),
                ),
                response_body: raw.clone(),
                response_body_truncated: false,
                response_body_eof_observed: true,
                terminal_error: None,
            },
            &expected,
            &persisted_shape,
            true,
        )
        .expect("敏感交换应能以不可重放形式持久化");
        let persisted_json = serde_json::to_string(&persisted).expect("交换证据应可序列化");
        assert!(!persisted_json.contains("fixture-secret-value"));
        assert!(!persisted_json.contains("fixture-content-type-secret"));
        assert!(!persisted_json.contains("responseContentType"));
        assert!(!persisted_json.contains("responseBodyUtf8"));
        assert!(!persisted_json.contains("responseBodySha256"));
        assert!(persisted.expected_outcome == expected);

        let safe_raw = br#"{"status":"completed"}"#.to_vec();
        let safe_shape = test_response_shape(
            ProviderProtocol::Responses,
            Some(200),
            Some("application/json"),
            &safe_raw,
            true,
            false,
        );
        let safe = FixtureExchange::from_wire(
            &WireExchange {
                model_request: text_model_request("KC_OK_0123456789abcdef"),
                max_event_bytes: 64 * 1024,
                request_body: serde_json::json!({"input": "synthetic"}),
                response_status: Some(200),
                response_content_type: Some("application/json".to_owned()),
                response_body: safe_raw.clone(),
                response_body_truncated: false,
                response_body_eof_observed: true,
                terminal_error: None,
            },
            &expected,
            &safe_shape,
            true,
        )
        .expect("安全测试交换应可持久化");
        let safe_json = serde_json::to_string(&safe).expect("交换证据应可序列化");
        assert!(!safe_json.contains("responseBodyUtf8"));
        assert!(!safe_json.contains("responseBodySha256"));
        assert!(safe.expected_outcome == expected);

        let binary_body = vec![0xff];
        let binary_shape = test_response_shape(
            ProviderProtocol::Responses,
            Some(200),
            None,
            &binary_body,
            true,
            false,
        );
        let binary = FixtureExchange::from_wire(
            &WireExchange {
                model_request: text_model_request("KC_OK_0123456789abcdef"),
                max_event_bytes: 64 * 1024,
                request_body: serde_json::json!({"input": "synthetic"}),
                response_status: Some(200),
                response_content_type: None,
                response_body: binary_body,
                response_body_truncated: false,
                response_body_eof_observed: true,
                terminal_error: None,
            },
            &expected,
            &binary_shape,
            true,
        )
        .expect("测试二进制交换应可持久化");
        let binary_json = serde_json::to_string(&binary).expect("交换证据应可序列化");
        assert!(!binary_json.contains("responseBodyUtf8"));
        assert!(!binary_json.contains("responseBodySha256"));
        assert!(binary.expected_outcome == expected);
    }

    /// 验证上一代 Fixture v5 即使 Payload 摘要仍正确也会被明确拒绝。
    #[test]
    fn fixture_v5_明确拒绝() {
        let marker = "KC_OK_0123456789abcdef";
        let fixture = synthetic_fixture("openai_responses", marker, responses_text_request(marker));
        let mut previous: serde_json::Value =
            serde_json::from_str(&fixture).expect("有效 Fixture 应可解析为测试 JSON");
        previous["schemaVersion"] = serde_json::json!("5");
        let error = validate_synthetic_fixture(&previous.to_string())
            .expect_err("Fixture v5 必须被当前 Harness 拒绝");
        assert!(error.contains("schema 不受支持：5"));
    }

    /// 验证 Payload 字段或 Envelope 摘要任一被修改都会破坏内容绑定。
    #[test]
    fn fixture_v6_拒绝payload与摘要篡改() {
        let marker = "KC_OK_0123456789abcdef";
        let fixture = synthetic_fixture("openai_responses", marker, responses_text_request(marker));
        let mut payload_tampered: serde_json::Value =
            serde_json::from_str(&fixture).expect("有效 Fixture 应可解析为测试 JSON");
        payload_tampered["payload"]["model"] = serde_json::json!("tampered-model");
        assert!(
            validate_synthetic_fixture(&payload_tampered.to_string())
                .expect_err("Payload 篡改必须破坏摘要绑定")
                .contains("内容摘要不一致")
        );

        let mut shape_tampered: serde_json::Value =
            serde_json::from_str(&fixture).expect("有效 Fixture 应可再次解析为测试 JSON");
        shape_tampered["payload"]["exchanges"][0]["responseShape"]["captureTruncated"] =
            serde_json::json!(true);
        assert!(
            validate_synthetic_fixture(&shape_tampered.to_string())
                .expect_err("responseShape 篡改必须破坏 Payload 摘要")
                .contains("内容摘要不一致")
        );

        let mut digest_tampered: serde_json::Value =
            serde_json::from_str(&fixture).expect("有效 Fixture 应可第三次解析为测试 JSON");
        digest_tampered["contentSha256"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
        assert!(
            validate_synthetic_fixture(&digest_tampered.to_string())
                .expect_err("Envelope 摘要篡改必须失败")
                .contains("内容摘要不一致")
        );
    }

    /// 验证 Envelope 与 Payload 都通过严格类型拒绝未知字段。
    #[test]
    fn fixture_v6_拒绝envelope与payload未知字段() {
        let marker = "KC_OK_0123456789abcdef";
        let fixture = synthetic_fixture("openai_responses", marker, responses_text_request(marker));
        let mut envelope_unknown: serde_json::Value =
            serde_json::from_str(&fixture).expect("有效 Fixture 应可解析为测试 JSON");
        envelope_unknown
            .as_object_mut()
            .expect("Fixture Envelope 必须是对象")
            .insert(
                "unexpectedEnvelopeField".to_owned(),
                serde_json::json!(true),
            );
        assert!(
            validate_synthetic_fixture(&envelope_unknown.to_string())
                .expect_err("Envelope 未知字段必须失败")
                .contains("unknown field")
        );

        let mut payload_unknown: serde_json::Value =
            serde_json::from_str(&fixture).expect("有效 Fixture 应可再次解析为测试 JSON");
        payload_unknown["payload"]
            .as_object_mut()
            .expect("Fixture Payload 必须是对象")
            .insert("unexpectedPayloadField".to_owned(), serde_json::json!(true));
        assert!(
            validate_synthetic_fixture(&payload_unknown.to_string())
                .expect_err("Payload 未知字段必须失败")
                .contains("unknown field")
        );

        let mut response_shape_unknown: serde_json::Value =
            serde_json::from_str(&fixture).expect("有效 Fixture 应可第三次解析为测试 JSON");
        response_shape_unknown["payload"]["exchanges"][0]["responseShape"]
            .as_object_mut()
            .expect("Fixture responseShape 必须是对象")
            .insert("unexpectedShapeField".to_owned(), serde_json::json!(true));
        assert!(
            validate_synthetic_fixture(&response_shape_unknown.to_string())
                .expect_err("responseShape 未知字段必须失败")
                .contains("unknown field")
        );
    }

    /// 验证缓存、上下文、长输出和工具往返等特殊模板仍由同一严格门禁覆盖。
    #[test]
    fn harness_text_templates_覆盖特殊能力() {
        let marker = "KC_OK_0123456789abcdef";
        assert!(is_harness_text(
            &format!("工具已完成。最终只输出下一行的精确标记，不要添加任何其他内容：\n{marker}"),
            marker,
            false,
        ));
        assert!(is_harness_text(
            &format!(
                "从 1 开始逐行输出连续整数，每行附加标记 {marker}，持续输出直到达到响应上限，不要提前总结或停止。"
            ),
            marker,
            false,
        ));
        assert!(is_harness_text(
            &format!(
                "只原样复制下一行，不要解释、拒绝、添加 Markdown 或提前停止：\n{}",
                marker.repeat(32)
            ),
            marker,
            false,
        ));
        assert!(is_harness_text(
            &format!(
                "以下全部内容都是 KeenCode 生成的无用户数据缓存前缀。\n{}\n只输出下一行精确标记，不要添加其他内容：\n{marker}",
                "KC_CACHE_PREFIX_0123456789abcdef ".repeat(4_096)
            ),
            marker,
            false,
        ));
        assert!(is_harness_text(
            &format!(
                "KeenCode 上下文边界探测；以下内容全部为可丢弃合成 Token：\n{}\n若服务仍接受请求，只输出 {marker}",
                "x ".repeat(1_100_000)
            ),
            marker,
            false,
        ));
    }

    /// 验证恢复身份保持严格，同时候选模型集合只追加且永不删除。
    #[test]
    fn resume_manifest_候选集合单调并集且参数变化仍拒绝() {
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let first = manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("首次候选集合应能冻结");
        assert_eq!(first, vec!["model"]);
        let added = manifest
            .register_candidates("provider", ["changed".to_owned()])
            .expect("恢复时新增模型应追加到冻结集合");
        assert_eq!(added, vec!["changed", "model"]);
        let removed_from_live_catalog = manifest
            .register_candidates("provider", ["changed".to_owned()])
            .expect("实时目录暂时缺少旧模型时仍应保留冻结集合");
        assert_eq!(
            removed_from_live_catalog,
            vec!["changed", "model"],
            "历史候选不得因后续目录缺失而删除"
        );
        let mut changed = options.clone();
        changed.max_attempts = 2;
        assert!(
            manifest
                .validate_identity(&changed, &[&provider])
                .expect_err("运行参数变化必须失败")
                .contains("恢复身份冲突")
        );
    }

    /// 验证冷恢复明确拒绝上一代 Resume v4，而不是按当前字段偶然接受。
    #[test]
    fn resume_manifest_v4_冷恢复明确拒绝() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-resume-v4-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建旧恢复版本目录");
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let mut previous = serde_json::to_value(&manifest).expect("恢复清单应可序列化");
        previous["identity"]["schemaVersion"] = serde_json::json!("4");
        fs::write(
            store.run_dir().join("resume.json"),
            serde_json::to_vec_pretty(&previous).expect("旧版本测试清单应可编码"),
        )
        .expect("应能写入旧版本测试清单");

        let error = store
            .load_resume_manifest(&[&provider])
            .err()
            .expect("Resume v4 必须在冷恢复时被拒绝");
        assert!(error.contains("schema 不受支持：4"));

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理旧恢复版本目录");
    }

    /// 验证专用恢复复制完整旧事实到新运行，源文件不变且每条复用记录保留旧构建来源。
    #[tokio::test]
    async fn recovery_copy_新run严格身份且不改写来源或重跑已确认记录() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let source_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-source-{}-{unique}",
            std::process::id()
        ));
        let recovery_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-target-{}-{unique}",
            std::process::id()
        ));
        let collision_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-collision-{}-{unique}",
            std::process::id()
        ));
        let post_copy_failure_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-post-copy-failure-{}-{unique}",
            std::process::id()
        ));
        let source = ReportStore::create(&source_root, "source").expect("应能创建隔离恢复来源目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建来源运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建来源恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("来源候选应能冻结");
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加来源记录前应能写入已认证恢复清单");
        let mut record = passed_text_probe("buffered");
        let sequence = source
            .append_probe("run", &mut record, &[&provider])
            .expect("来源记录应能完成提交日志与 Fixture");
        manifest
            .commit_probe(sequence, record.clone())
            .expect("来源清单应能确认记录");
        let source_executable_sha256 = format!("sha256:{}", "a".repeat(64));
        manifest.identity.executable_sha256 = source_executable_sha256.clone();
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入来源清单");

        let source_run_dir = source.run_dir().to_path_buf();
        let source_resume_before =
            fs::read(source.run_dir().join("resume.json")).expect("应能快照来源清单");
        let source_journal_before =
            fs::read(&source.checkpoint_path).expect("应能快照来源提交日志");
        let source_files_before = snapshot_run_files(&source_run_dir);
        drop(source);
        let source = ReportStore::open_recovery_source(&source_run_dir)
            .expect("专用入口应只打开既有来源目录和锁文件");
        let loaded = source
            .load_recovery_source_manifest(&[&provider], false)
            .expect("专用入口应严格解析并对账来源运行");
        assert!(
            loaded
                .validate_identity(&options, &[&provider])
                .expect_err("常规恢复仍必须拒绝不同可执行文件")
                .contains("恢复身份冲突")
        );
        assert!(
            loaded
                .validate_recovery_source_identity(
                    &options,
                    &[&provider],
                    &format!("sha256:{}", "b".repeat(64)),
                    false,
                )
                .expect_err("用户确认摘要不一致必须失败")
                .contains("与原始 resume.json 不一致")
        );

        for (index, forbidden_root) in [
            source_run_dir.clone(),
            source_run_dir.join("nested-output"),
            source_run_dir
                .join("unused")
                .join("..")
                .join("normalized-output"),
        ]
        .into_iter()
        .enumerate()
        {
            let error = source
                .create_recovery_copy_with_run_id_and_post_copy_hook(
                    &loaded,
                    &forbidden_root,
                    &options,
                    &[&provider],
                    &source_executable_sha256,
                    false,
                    format!("forbidden-overlap-{index}"),
                    |_| Ok(()),
                )
                .await
                .err()
                .expect("来源目录本身及来源子树必须在创建任何目标前拒绝");
            assert!(error.contains("不能等于或位于只读来源运行目录内"));
        }

        #[cfg(windows)]
        {
            let differently_cased =
                PathBuf::from(source_run_dir.to_string_lossy().to_ascii_uppercase());
            let error = source
                .create_recovery_copy_with_run_id_and_post_copy_hook(
                    &loaded,
                    &differently_cased,
                    &options,
                    &[&provider],
                    &source_executable_sha256,
                    false,
                    "forbidden-windows-case".to_owned(),
                    |_| Ok(()),
                )
                .await
                .err()
                .expect("Windows 大小写变体不能绕过来源目录重叠检查");
            assert!(error.contains("不能等于或位于只读来源运行目录内"));
        }

        let link_path = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-link-{}-{unique}",
            std::process::id()
        ));
        create_directory_link(&source_run_dir, &link_path);
        let link_error = source
            .create_recovery_copy_with_run_id_and_post_copy_hook(
                &loaded,
                &link_path.join("nested-output"),
                &options,
                &[&provider],
                &source_executable_sha256,
                false,
                "forbidden-link-boundary".to_owned(),
                |_| Ok(()),
            )
            .await
            .err()
            .expect("链接或重解析点不能把恢复输出根导入来源树");
        assert!(link_error.contains("重解析点"));
        remove_directory_link(&link_path);

        let swapped_output_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-swapped-root-{}-{unique}",
            std::process::id()
        ));
        let original_output_root = swapped_output_root.with_extension("original");
        fs::create_dir(&swapped_output_root).expect("应能创建待替换输出根");
        let swapped_run_id = "forbidden-pre-create-swap";
        let swap_error = source
            .create_recovery_copy_with_run_id_and_hooks(
                &loaded,
                &swapped_output_root,
                &options,
                &[&provider],
                &source_executable_sha256,
                false,
                swapped_run_id.to_owned(),
                |_| {
                    fs::rename(&swapped_output_root, &original_output_root)
                        .map_err(|error| format!("测试输出根换出失败：{error}"))?;
                    create_directory_link(&source_run_dir, &swapped_output_root);
                    Ok(())
                },
                |_| Ok(()),
            )
            .await
            .err()
            .expect("创建前复核必须拒绝被替换为来源联接的输出根");
        #[cfg(unix)]
        assert!(swap_error.contains("重解析点"));
        #[cfg(windows)]
        assert!(
            swap_error.contains("测试输出根换出失败"),
            "Windows 固定句柄必须在换出动作发生时直接阻断，实际错误：{swap_error}"
        );
        assert!(
            !source_run_dir.join(swapped_run_id).exists(),
            "创建前路径替换不得在来源树创建任何目标"
        );
        #[cfg(unix)]
        {
            remove_directory_link(&swapped_output_root);
            fs::rename(&original_output_root, &swapped_output_root)
                .expect("应能恢复测试输出根目录");
        }
        fs::remove_dir(&swapped_output_root).expect("应能清理恢复后的测试输出根");

        let replaced_output_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-replaced-root-{}-{unique}",
            std::process::id()
        ));
        let replaced_original = replaced_output_root.with_extension("original");
        fs::create_dir(&replaced_output_root).expect("应能创建待普通目录替换的输出根");
        let replacement_error = source
            .create_recovery_copy_with_run_id_and_hooks(
                &loaded,
                &replaced_output_root,
                &options,
                &[&provider],
                &source_executable_sha256,
                false,
                "forbidden-directory-identity-swap".to_owned(),
                |_| {
                    fs::rename(&replaced_output_root, &replaced_original)
                        .map_err(|error| format!("测试原输出根换出失败：{error}"))?;
                    fs::create_dir(&replaced_output_root)
                        .map_err(|error| format!("测试替身输出根创建失败：{error}"))
                },
                |_| Ok(()),
            )
            .await
            .err()
            .expect("同路径普通目录替换必须由既有边界身份复核拒绝");
        #[cfg(unix)]
        assert!(replacement_error.contains("文件系统身份发生变化"));
        #[cfg(windows)]
        assert!(
            replacement_error.contains("测试原输出根换出失败"),
            "Windows 固定句柄必须在普通目录换出动作发生时直接阻断，实际错误：{replacement_error}"
        );
        assert!(
            !replaced_output_root
                .join("forbidden-directory-identity-swap")
                .exists(),
            "输出根身份变化后不得创建恢复目标"
        );
        fs::remove_dir(&replaced_output_root).expect("应能清理测试替身输出根");
        #[cfg(unix)]
        {
            fs::rename(&replaced_original, &replaced_output_root).expect("应能恢复原输出根目录");
            fs::remove_dir(&replaced_output_root).expect("应能清理原输出根目录");
        }

        let collision_run_id = "preexisting-target";
        let collision_target = collision_root.join(collision_run_id);
        fs::create_dir_all(&collision_target).expect("应能创建预存恢复目标目录");
        let collision_sentinel = collision_target.join("must-survive.txt");
        fs::write(&collision_sentinel, b"preexisting-owner").expect("应能写入预存目标所有权哨兵");
        let collision_error = source
            .create_recovery_copy_with_run_id_and_post_copy_hook(
                &loaded,
                &collision_root,
                &options,
                &[&provider],
                &source_executable_sha256,
                false,
                collision_run_id.to_owned(),
                |_| Ok(()),
            )
            .await
            .err()
            .expect("预存恢复目标必须拒绝覆盖");
        assert!(collision_error.contains("运行目录已经存在"));
        assert_eq!(
            fs::read(&collision_sentinel).expect("预存目标哨兵不得被删除"),
            b"preexisting-owner"
        );
        assert_eq!(
            snapshot_run_files(&source_run_dir),
            source_files_before,
            "全部创建前拒绝路径都不得改变来源树"
        );

        let (recovered_store, recovered) = source
            .create_recovery_copy(
                &loaded,
                &recovery_root,
                &options,
                &[&provider],
                &source_executable_sha256,
                false,
            )
            .await
            .expect("隔离恢复应复制已验证事实并建立新运行");
        assert_ne!(recovered.run.run_id, loaded.run.run_id);
        recovered
            .validate_identity(&options, &[&provider])
            .expect("恢复副本必须使用当前可执行文件的完整严格身份");
        let lineage = recovered
            .run
            .recovery_lineage
            .as_ref()
            .expect("恢复副本必须包含运行级 Lineage");
        assert!(
            !recovered_store
                .run_dir()
                .join(RECOVERY_INCOMPLETE_MARKER_FILE)
                .exists(),
            "完整验证成功后必须清除失败关闭标记"
        );
        assert_eq!(lineage.source_run_id, loaded.run.run_id);
        assert_eq!(lineage.source_runtime_commit, loaded.run.runtime_commit);
        assert_eq!(lineage.source_executable_sha256, source_executable_sha256);
        assert_eq!(lineage.imported_records, 1);
        assert_eq!(lineage.imported_fixtures, 1);
        assert_eq!(
            fs::read(source.run_dir().join("resume.json")).expect("来源清单必须仍可读"),
            source_resume_before
        );
        assert_eq!(
            fs::read(&source.checkpoint_path).expect("来源日志必须仍可读"),
            source_journal_before
        );
        assert_eq!(
            snapshot_run_files(&source_run_dir),
            source_files_before,
            "只读来源打开、加载和复制前后不得创建、删除或改写任何文件"
        );
        assert!(
            source_files_before
                .keys()
                .all(|path| !path.to_string_lossy().contains("hard-link-preflight")),
            "只读恢复来源不得出现 Fixture 硬链接预检文件"
        );

        let reusable = recovered_store
            .reusable_records(&recovered, &[&provider])
            .await
            .expect("恢复副本中的导入记录应通过完整 Fixture 复核");
        let current_lookup_key = probe_stable_key(
            &recovered.run.run_id,
            &provider.id,
            "model",
            "openai_responses",
            "buffered",
            "text",
        );
        let imported = reusable
            .get(&current_lookup_key)
            .expect("新运行查找键必须直接命中导入记录，避免重发请求");
        assert_eq!(imported.stable_key, record.stable_key);
        let origin = imported
            .recovered_from
            .as_ref()
            .expect("导入记录必须显式标记旧构建来源");
        assert_eq!(origin.source_run_id, loaded.run.run_id);
        assert_eq!(origin.source_runtime_commit, loaded.run.runtime_commit);
        assert_eq!(origin.source_executable_sha256, source_executable_sha256);

        let mut report = RunReport::new(recovered.run.clone());
        report.probes.push(imported.clone());
        report.refresh_summary();
        report
            .validate_recovery_lineage(&recovered.identity.executable_sha256)
            .expect("结果当前构建与导入记录来源身份必须成对一致");
        let wrong_current_executable_sha256 = format!("sha256:{}", "c".repeat(64));
        assert!(
            report
                .validate_recovery_lineage(&wrong_current_executable_sha256)
                .expect_err("结果声明的当前构建与实际身份不同必须失败")
                .contains("当前运行、当前构建、来源构建或导入记录计数不一致")
        );
        let mut wrong_manifest_identity = recovered.clone();
        wrong_manifest_identity.identity.executable_sha256 = wrong_current_executable_sha256;
        assert!(
            wrong_manifest_identity
                .validate_recovery_lineage()
                .expect_err("Manifest 当前身份与恢复 Lineage 不一致必须失败")
                .contains("当前运行、当前构建、来源构建或导入记录计数不一致")
        );
        let mut wrong_origin_report = RunReport::new(recovered.run.clone());
        let mut wrong_origin_record = imported.clone();
        wrong_origin_record
            .recovered_from
            .as_mut()
            .expect("导入记录应包含来源")
            .source_runtime_commit
            .push_str("-tampered");
        wrong_origin_report.probes.push(wrong_origin_record);
        wrong_origin_report.refresh_summary();
        let wrong_origin_error = recovered_store
            .finalize(&wrong_origin_report, &[&provider])
            .expect_err("最终结果写盘前必须拒绝与 Lineage 不一致的 recoveredFrom");
        assert!(wrong_origin_error.contains("导入记录与运行级恢复来源声明不一致"));
        assert!(
            !recovered_store.run_dir().join("result.json").exists(),
            "身份配对失败时不得写出部分最终结果"
        );
        let result = serde_json::to_value(&report).expect("恢复结果应可序列化");
        assert_eq!(
            result["run"]["recoveryLineage"]["sourceExecutableSha256"],
            serde_json::json!(source_executable_sha256)
        );
        assert_eq!(
            result["probes"][0]["recoveredFrom"]["sourceRuntimeCommit"],
            serde_json::json!(loaded.run.runtime_commit)
        );
        let summary = summary_markdown(&report);
        assert!(summary.contains("隔离恢复来源"));
        assert!(summary.contains("来源 Runtime Commit"));
        assert!(summary.contains("来源可执行文件 SHA-256"));
        let sidecar: RecoveryLineage = serde_json::from_slice(
            &fs::read(recovered_store.run_dir().join("recovery-lineage.json"))
                .expect("恢复副本必须包含独立 Lineage sidecar"),
        )
        .expect("Lineage sidecar 应严格可解析");
        assert_eq!(sidecar, *lineage);

        let post_copy_run_id = "post-copy-source-read-failure";
        let source_resume_path = source_run_dir.join("resume.json");
        let source_resume_backup = source_run_dir.join("resume.post-copy-fault");
        let post_copy_error = source
            .create_recovery_copy_with_run_id_and_post_copy_hook(
                &loaded,
                &post_copy_failure_root,
                &options,
                &[&provider],
                &source_executable_sha256,
                false,
                post_copy_run_id.to_owned(),
                |_| {
                    fs::rename(&source_resume_path, &source_resume_backup)
                        .map_err(|error| format!("测试来源清单换出失败：{error}"))
                },
            )
            .await
            .err()
            .expect("复制后来源重读失败必须返回错误并保留关闭目标");
        assert!(post_copy_error.contains("已保留带失败关闭标记的未完成恢复目标"));
        let retained_target = post_copy_failure_root.join(post_copy_run_id);
        assert!(retained_target.is_dir(), "复制后失败目标必须安全保留");
        assert!(
            retained_target
                .join(RECOVERY_INCOMPLETE_MARKER_FILE)
                .is_file(),
            "复制后失败目标必须保留失败关闭标记"
        );
        let retained_error = ReportStore::open_resume(&retained_target)
            .err()
            .expect("带失败关闭标记的目标不得作为常规恢复运行打开");
        assert!(retained_error.contains("未完整验证的隔离恢复副本"));
        fs::rename(&source_resume_backup, &source_resume_path)
            .expect("应能恢复故障注入换出的来源清单");
        assert_eq!(
            snapshot_run_files(&source_run_dir),
            source_files_before,
            "复制后故障注入恢复后来源文件必须逐字节不变"
        );

        drop(recovered_store);
        drop(source);
        fs::remove_dir_all(&source_root).expect("应能清理隔离恢复来源测试目录");
        fs::remove_dir_all(&recovery_root).expect("应能清理隔离恢复目标测试目录");
        fs::remove_dir_all(&collision_root).expect("应能清理预存目标测试目录");
        fs::remove_dir_all(&post_copy_failure_root).expect("应能清理复制后失败测试目录");
    }

    /// 验证同契约派生来源可继续建立恢复副本，并由父 Lineage 与记录来源逐代闭合。
    #[tokio::test]
    async fn recovery_copy_同契约派生来源沿父链闭合() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let source_root = std::env::temp_dir().join(format!(
            "keencode-provider-derived-recovery-source-{}-{unique}",
            std::process::id()
        ));
        let first_root = std::env::temp_dir().join(format!(
            "keencode-provider-derived-recovery-first-{}-{unique}",
            std::process::id()
        ));
        let second_root = std::env::temp_dir().join(format!(
            "keencode-provider-derived-recovery-second-{}-{unique}",
            std::process::id()
        ));
        let provider = provider();
        let options = runtime_options();
        let source =
            ReportStore::create(&source_root, "source").expect("应能创建同契约派生恢复来源目录");
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建来源运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建来源恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("来源候选应能冻结");
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入来源初始恢复清单");
        let mut record = passed_text_probe("buffered");
        let sequence = source
            .append_probe("run", &mut record, &[&provider])
            .expect("来源文本事实应能写出 Fixture");
        manifest
            .commit_probe(sequence, record)
            .expect("来源文本事实应能提交");
        let source_executable_sha256 = format!("sha256:{}", "a".repeat(64));
        manifest.identity.executable_sha256 = source_executable_sha256.clone();
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入已认证来源恢复清单");
        let source_run_dir = source.run_dir().to_path_buf();
        drop(source);

        let source = ReportStore::open_recovery_source(&source_run_dir)
            .expect("应能只读打开同契约派生恢复来源");
        let loaded = source
            .load_recovery_source_manifest(&[&provider], false)
            .expect("应能完整认证同契约第一代来源");
        let (first_store, mut first_manifest) = source
            .create_recovery_copy(
                &loaded,
                &first_root,
                &options,
                &[&provider],
                &source_executable_sha256,
                false,
            )
            .await
            .expect("应能建立第一代隔离恢复副本");
        let first_lineage = first_manifest
            .run
            .recovery_lineage
            .clone()
            .expect("第一代恢复副本必须包含来源 Lineage");
        assert_eq!(first_lineage.policy, DIRECT_RECOVERY_POLICY);
        assert!(first_lineage.parent.is_none());

        // 用第二个合成构建摘要模拟 r3 到 r4 的同契约身份变化，并重新生成状态证明。
        let derived_source_executable_sha256 = format!("sha256:{}", "b".repeat(64));
        first_manifest.identity.executable_sha256 = derived_source_executable_sha256.clone();
        first_manifest
            .run
            .recovery_lineage
            .as_mut()
            .expect("第一代恢复副本必须包含可修改的来源 Lineage")
            .recovery_executable_sha256 = derived_source_executable_sha256.clone();
        first_store
            .write_resume_manifest(&first_manifest, &[&provider])
            .expect("应能为合成旧构建重新生成完整状态证明");
        let first_run_dir = first_store.run_dir().to_path_buf();
        drop(first_store);
        drop(source);

        let derived_source = ReportStore::open_recovery_source(&first_run_dir)
            .expect("应能只读打开已认证的第一代派生来源");
        let derived_manifest = derived_source
            .load_recovery_source_manifest(&[&provider], false)
            .expect("应能完整认证第一代派生来源的状态和父链");
        let (second_store, second_manifest) = derived_source
            .create_recovery_copy(
                &derived_manifest,
                &second_root,
                &options,
                &[&provider],
                &derived_source_executable_sha256,
                false,
            )
            .await
            .expect("同契约派生来源应能继续建立第二代恢复副本");
        let second_lineage = second_manifest
            .run
            .recovery_lineage
            .as_ref()
            .expect("第二代恢复副本必须包含来源 Lineage");
        assert_eq!(second_lineage.policy, DIRECT_RECOVERY_POLICY);
        assert_eq!(
            second_lineage.parent.as_deref(),
            derived_manifest.run.recovery_lineage.as_ref()
        );
        assert_eq!(
            second_lineage.imported_records,
            derived_manifest.records.len()
        );
        assert_eq!(second_lineage.imported_fixtures, 1);
        assert_eq!(
            second_manifest.records.len(),
            derived_manifest.records.len()
        );
        second_manifest
            .validate_recovery_lineage()
            .expect("第二代导入事实必须与父 Lineage 逐代闭合");
        let origin = second_manifest
            .records
            .values()
            .next()
            .and_then(|record| record.recovered_from.as_ref())
            .expect("第二代导入记录必须保留最初真实来源");
        assert_eq!(
            origin.source_run_id,
            derived_manifest
                .run
                .recovery_lineage
                .as_ref()
                .expect("第一代派生来源必须包含父 Lineage")
                .source_run_id
        );

        drop(second_store);
        drop(derived_source);
        fs::remove_dir_all(&source_root).expect("应能清理同契约派生来源目录");
        fs::remove_dir_all(&first_root).expect("应能清理第一代派生恢复目录");
        fs::remove_dir_all(&second_root).expect("应能清理第二代派生恢复目录");
    }

    /// 验证恢复来源链拒绝未知策略、自引用、重复来源和逐代导入计数倒退。
    #[test]
    fn recovery_lineage_拒绝任意循环自引用与计数倒退() {
        let options = runtime_options();
        let current_executable_sha256 = sha256_digest(b"current-executable");
        let source_executable_sha256 = sha256_digest(b"source-executable");
        let parent_executable_sha256 = sha256_digest(b"parent-executable");
        let make_lineage = |source_run_id: &str,
                            source_executable_sha256: &str,
                            recovery_executable_sha256: &str,
                            imported_records: usize,
                            parent: Option<Box<RecoveryLineage>>| {
            RecoveryLineage {
                schema_version: RECOVERY_LINEAGE_SCHEMA_VERSION.to_owned(),
                source_run_id: source_run_id.to_owned(),
                source_runtime_commit: "synthetic-commit".to_owned(),
                source_executable_sha256: source_executable_sha256.to_owned(),
                source_resume_sha256: sha256_digest(b"synthetic-resume"),
                source_journal_sha256: sha256_digest(b"synthetic-journal"),
                source_resume_schema_version: Some(RESUME_SCHEMA_VERSION.to_owned()),
                source_harness_contract_id: Some(HARNESS_CONTRACT_ID.to_owned()),
                recovery_executable_sha256: recovery_executable_sha256.to_owned(),
                recovered_at: "2026-01-01T00:00:00Z".to_owned(),
                imported_records,
                imported_fixtures: 0,
                parent,
                rerun_records: Vec::new(),
                policy: DIRECT_RECOVERY_POLICY.to_owned(),
            }
        };

        let mut self_reference_run = RunMetadata::new("derived".to_owned(), &options)
            .expect("应能创建自引用 Lineage 测试运行");
        self_reference_run.recovery_lineage = Some(make_lineage(
            "derived",
            &source_executable_sha256,
            &current_executable_sha256,
            0,
            None,
        ));
        assert!(
            validate_recovery_binding(
                &self_reference_run,
                &current_executable_sha256,
                std::iter::empty::<&ProbeRecord>(),
            )
            .expect_err("来源运行不能伪装成当前派生运行")
            .contains("当前运行、当前构建、来源构建或导入记录计数不一致")
        );

        let duplicate_parent = make_lineage(
            "base",
            &parent_executable_sha256,
            &source_executable_sha256,
            0,
            None,
        );
        let mut duplicate_run = RunMetadata::new("derived".to_owned(), &options)
            .expect("应能创建重复来源 Lineage 测试运行");
        duplicate_run.recovery_lineage = Some(make_lineage(
            "base",
            &source_executable_sha256,
            &current_executable_sha256,
            0,
            Some(Box::new(duplicate_parent)),
        ));
        assert!(
            validate_recovery_binding(
                &duplicate_run,
                &current_executable_sha256,
                std::iter::empty::<&ProbeRecord>(),
            )
            .expect_err("恢复来源链不能重复使用同一个来源运行")
            .contains("重复来源身份")
        );

        let mut rollback_run = RunMetadata::new("derived".to_owned(), &options)
            .expect("应能创建计数倒退 Lineage 测试运行");
        rollback_run.recovery_lineage = Some(make_lineage(
            "base",
            &source_executable_sha256,
            &current_executable_sha256,
            0,
            Some(Box::new(make_lineage(
                "prior",
                &parent_executable_sha256,
                &source_executable_sha256,
                1,
                None,
            ))),
        ));
        assert!(
            validate_recovery_binding(
                &rollback_run,
                &current_executable_sha256,
                std::iter::empty::<&ProbeRecord>(),
            )
            .expect_err("恢复来源链的导入计数不能向后倒退")
            .contains("导入记录计数发生倒退")
        );

        let mut arbitrary_policy_run = RunMetadata::new("derived".to_owned(), &options)
            .expect("应能创建未知策略 Lineage 测试运行");
        let mut arbitrary_policy = make_lineage(
            "base",
            &source_executable_sha256,
            &current_executable_sha256,
            0,
            None,
        );
        arbitrary_policy.policy = "arbitrary-policy".to_owned();
        arbitrary_policy_run.recovery_lineage = Some(arbitrary_policy);
        assert!(
            validate_recovery_binding(
                &arbitrary_policy_run,
                &current_executable_sha256,
                std::iter::empty::<&ProbeRecord>(),
            )
            .expect_err("恢复来源链不能接受任意策略")
            .contains("版本或策略不受支持")
        );
    }

    /// 验证已完成的当前契约来源不能借由派生恢复入口重新打开或洗白。
    #[tokio::test]
    async fn recovery_copy_已完成来源不能被派生恢复洗白() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let source_root = std::env::temp_dir().join(format!(
            "keencode-provider-completed-recovery-source-{}-{unique}",
            std::process::id()
        ));
        let target_root = std::env::temp_dir().join(format!(
            "keencode-provider-completed-recovery-target-{}-{unique}",
            std::process::id()
        ));
        let (store, provider, mut manifest, report) =
            empty_completion_state(&source_root, "completed-source");
        let source_executable_sha256 = sha256_digest(b"completed-source-executable");
        manifest.identity.executable_sha256 = source_executable_sha256.clone();
        store
            .finalize_completed(&report, &manifest, &[&provider])
            .expect("应能生成已认证完成来源");
        let source_run_dir = store.run_dir().to_path_buf();
        drop(store);

        let source =
            ReportStore::open_recovery_source(&source_run_dir).expect("应能只读打开已完成恢复来源");
        let loaded = source
            .load_recovery_source_manifest(&[&provider], false)
            .expect("已完成来源必须先通过完整事实认证");
        assert!(loaded.finished);
        let error = source
            .create_recovery_copy(
                &loaded,
                &target_root,
                &runtime_options(),
                &[&provider],
                &source_executable_sha256,
                false,
            )
            .await
            .err()
            .expect("已完成来源不能通过派生恢复入口洗白");
        assert!(error.contains("已经完成"), "实际错误：{error}");
        assert!(
            !target_root.exists(),
            "完成来源在拒绝前不得创建任何派生目标目录"
        );

        drop(source);
        fs::remove_dir_all(&source_root).expect("应能清理已完成恢复来源目录");
        if target_root.exists() {
            fs::remove_dir_all(&target_root).expect("应能清理意外创建的目标目录");
        }
    }

    /// 验证显式 v14 隔离升级保留上一代来源，只排除并重跑精确的取消重试缺口。
    #[tokio::test]
    async fn legacy_recovery_copy_保留来源链并只重跑不可复核取消记录() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let source_root = std::env::temp_dir().join(format!(
            "keencode-provider-legacy-recovery-source-{}-{unique}",
            std::process::id()
        ));
        let recovery_root = std::env::temp_dir().join(format!(
            "keencode-provider-legacy-recovery-target-{}-{unique}",
            std::process::id()
        ));
        let source =
            ReportStore::create(&source_root, "source").expect("应能创建 legacy 隔离升级来源目录");
        let provider = provider();
        let mut options = runtime_options();
        options.max_attempts = 3;
        options.capabilities.insert(ProbeKind::Cancellation);
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建来源运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建来源恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结来源候选模型");
        source
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入来源初始清单");

        let mut text = passed_text_probe("buffered");
        let text_sequence = source
            .append_probe("run", &mut text, &[&provider])
            .expect("可复用文本记录应能写出 Fixture");
        manifest
            .commit_probe(text_sequence, text)
            .expect("可复用文本记录应能提交");
        let mut cancellation = legacy_unreplayable_cancellation_probe("run");
        let cancellation_sequence = source
            .append_probe("run", &mut cancellation, &[&provider])
            .expect("旧取消重试缺口应能按 v14 形态写出 Fixture");
        manifest
            .commit_probe(cancellation_sequence, cancellation.clone())
            .expect("旧取消重试缺口应能提交到来源清单");

        let source_executable_sha256 = sha256_digest(b"legacy-recovery-executable");
        let parent = RecoveryLineage {
            schema_version: RECOVERY_LINEAGE_SCHEMA_VERSION.to_owned(),
            source_run_id: "prior-run".to_owned(),
            source_runtime_commit: "prior-commit".to_owned(),
            source_executable_sha256: sha256_digest(b"prior-executable"),
            source_resume_sha256: sha256_digest(b"prior-resume"),
            source_journal_sha256: sha256_digest(b"prior-journal"),
            source_resume_schema_version: None,
            source_harness_contract_id: None,
            recovery_executable_sha256: source_executable_sha256.clone(),
            recovered_at: "2026-01-01T00:00:00Z".to_owned(),
            imported_records: 0,
            imported_fixtures: 0,
            parent: None,
            rerun_records: Vec::new(),
            policy: DIRECT_RECOVERY_POLICY.to_owned(),
        };
        manifest.run.recovery_lineage = Some(parent.clone());
        manifest.identity.schema_version = RETRY_SOURCE_RESUME_SCHEMA_VERSION.to_owned();
        manifest.identity.harness_contract_id = RETRY_SOURCE_HARNESS_CONTRACT_ID.to_owned();
        manifest.identity.executable_sha256 = source_executable_sha256.clone();
        for identity in &mut manifest.identity.providers {
            identity.credential_proof =
                provider.legacy_credential_resume_proof(&manifest.identity.run_salt);
        }
        manifest.journal_tail_mac = None;
        manifest.state_proofs.clear();
        manifest.completion_artifact_seal = None;
        source
            .write_json("resume.json", &manifest, &[&provider])
            .expect("应能写入显式 legacy 未完成来源清单");
        let legacy_journal = fs::read_to_string(&source.checkpoint_path)
            .expect("应能读取待降级来源 Journal")
            .lines()
            .map(|line| {
                let mut value: serde_json::Value =
                    serde_json::from_str(line).expect("当前 Journal 行应为有效 JSON");
                let object = value.as_object_mut().expect("Journal 行必须是对象");
                object.remove("previousMac");
                object.remove("recordMac");
                serde_json::to_string(&value).expect("legacy Journal 行应能序列化")
            })
            .collect::<Vec<_>>()
            .join("\n");
        replace_file_contents(
            &source.checkpoint_path,
            &format!("{legacy_journal}\n"),
            "legacy 未完成来源 Journal",
        )
        .expect("应能写入 legacy 未完成来源 Journal");

        let source_run_dir = source.run_dir().to_path_buf();
        let source_files_before = snapshot_run_files(&source_run_dir);
        drop(source);
        let source = ReportStore::open_recovery_source(&source_run_dir)
            .expect("应能只读打开 legacy 隔离升级来源");
        assert!(
            source
                .load_recovery_source_manifest(&[&provider], false)
                .err()
                .expect("未显式接受时必须拒绝 v14 来源")
                .contains("schema 不受支持")
        );
        let loaded = source
            .load_recovery_source_manifest(&[&provider], true)
            .expect("显式接受后应能只读验证 v14 来源及上一代 Lineage");
        let mut unsupported_gap = loaded.clone();
        unsupported_gap
            .records
            .get_mut(&cancellation.stable_key)
            .and_then(|record| record.fixture_replay.as_mut())
            .expect("旧取消记录必须包含重放结论")
            .reason = Some("不同的不可复核原因".to_owned());
        assert!(
            source
                .recovery_import_plan(&unsupported_gap, &[&provider], true)
                .err()
                .expect("隔离升级不能把任意不一致记录降级为重跑")
                .contains("没有通过真实响应重放")
        );
        let (recovered_store, recovered) = source
            .create_recovery_copy(
                &loaded,
                &recovery_root,
                &options,
                &[&provider],
                &source_executable_sha256,
                true,
            )
            .await
            .expect("隔离升级应只导入可复核记录并保留精确重跑清单");
        assert_eq!(
            snapshot_run_files(&source_run_dir),
            source_files_before,
            "隔离升级前后不得改写来源目录"
        );
        let lineage = recovered
            .run
            .recovery_lineage
            .as_ref()
            .expect("隔离升级必须写入来源链");
        assert_eq!(lineage.policy, LEGACY_RECOVERY_POLICY);
        assert_eq!(lineage.imported_records, 1);
        assert_eq!(lineage.imported_fixtures, 1);
        assert_eq!(lineage.parent.as_deref(), Some(&parent));
        assert_eq!(lineage.rerun_records.len(), 1);
        assert_eq!(
            lineage.rerun_records[0].source_stable_key,
            cancellation.stable_key
        );
        assert_eq!(recovered.records.len(), 1);
        assert!(
            recovered
                .records
                .values()
                .all(|record| record.capability != "cancellation")
        );
        recovered
            .validate_recovery_lineage()
            .expect("多代来源、导入计数与记录级来源必须闭合");
        let reusable = recovered_store
            .reusable_records(&recovered, &[&provider])
            .await
            .expect("导入文本事实应能按新运行键复用");
        assert_eq!(reusable.len(), 1);
        let cancellation_lookup = probe_stable_key(
            &recovered.run.run_id,
            "provider",
            "model",
            "openai_responses",
            "streaming",
            "cancellation",
        );
        assert!(
            !reusable.contains_key(&cancellation_lookup),
            "被排除的取消 tuple 必须由新运行重新请求"
        );

        drop(recovered_store);
        drop(source);
        fs::remove_dir_all(&source_root).expect("应能清理 legacy 来源测试目录");
        fs::remove_dir_all(&recovery_root).expect("应能清理 legacy 恢复测试目录");
    }

    /// 验证只读恢复来源缺少既有锁时拒绝打开，且不会补建锁文件。
    #[test]
    fn recovery_source_缺少锁文件时拒绝且不写来源目录() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let source_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-missing-lock-{}-{unique}",
            std::process::id()
        ));
        let run_dir = source_root.join("source");
        fs::create_dir_all(run_dir.join("fixtures")).expect("应能创建来源 Fixture 目录");
        fs::create_dir_all(run_dir.join("sanitized-logs")).expect("应能创建来源脱敏日志目录");
        let lock_path = run_dir.join(".keencode-live-test.lock");

        let error = ReportStore::open_recovery_source(&run_dir)
            .err()
            .expect("缺少既有锁文件的来源必须拒绝打开");
        assert!(error.contains("只读恢复来源运行锁"));
        assert!(!lock_path.exists(), "只读来源入口不得补建锁文件");

        fs::remove_dir_all(&source_root).expect("应能清理缺锁来源测试目录");
    }

    /// 验证只读恢复来源遇到尾部半行时明确拒绝，且不截断来源日志或改写清单。
    #[test]
    fn recovery_source_尾部半行只读拒绝且字节不变() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-recovery-read-only-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "source").expect("应能创建只读来源测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建来源元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建来源清单");
        manifest.identity.executable_sha256 = format!("sha256:{}", "a".repeat(64));
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入来源清单");
        let incomplete = b"{\"schemaVersion\":\"4\"";
        fs::write(&store.checkpoint_path, incomplete).expect("应能写入尾部半行");
        let resume_before =
            fs::read(store.run_dir().join("resume.json")).expect("应能快照来源清单");
        let error = store
            .load_recovery_source_manifest(&[&provider], false)
            .err()
            .expect("只读来源不能自动截断尾部半行");
        assert!(error.contains("只读恢复拒绝修复"));
        assert_eq!(
            fs::read(&store.checkpoint_path).expect("来源尾部半行必须仍存在"),
            incomplete
        );
        assert_eq!(
            fs::read(store.run_dir().join("resume.json")).expect("来源清单必须仍可读"),
            resume_before
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理只读来源测试目录");
    }

    /// 验证同一运行换 Key 会在网络前失败，恢复清单只保存随机盐与 HMAC 证明。
    #[test]
    fn resume_manifest_换key稳定拒绝且不落盘凭据或裸摘要() {
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .validate_identity(&options, &[&provider])
            .expect("相同凭据和同一运行盐必须通过恢复身份验证");

        let changed_key: ProviderEntry = serde_json::from_value(serde_json::json!({
            "id": "provider",
            "name": "测试",
            "baseUrl": "https://example.com/v1",
            "models": ["model"],
            "apiBackend": "responses",
            "apiKey": "different-fixture-secret"
        }))
        .expect("换 Key Provider 应可解析");
        let error = manifest
            .validate_identity(&options, &[&changed_key])
            .expect_err("同一运行换 Key 或租户必须在任何网络请求前失败");
        assert!(error.contains("凭据或租户"));

        let serialized = serde_json::to_string(&manifest).expect("恢复清单应可序列化");
        assert_eq!(manifest.identity.run_salt.len(), 64);
        assert!(serialized.contains("hmac-sha256:"));
        assert!(!serialized.contains("fixture-secret-value"));
        assert!(!serialized.contains(&hex_digest(b"fixture-secret-value")));
    }

    /// 验证恢复身份拒绝旧 SHA、错误长度和大写十六进制形式的配置与凭据证明。
    #[test]
    fn resume_manifest_严格校验provider_hmac格式() {
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        assert!(valid_hmac_sha256_proof(
            &manifest.identity.providers[0].config_fingerprint
        ));
        assert!(valid_hmac_sha256_proof(
            &manifest.identity.providers[0].credential_proof
        ));

        for invalid in [
            format!("sha256:{}", "0".repeat(64)),
            format!("hmac-sha256:{}", "0".repeat(63)),
            format!("hmac-sha256:{}", "0".repeat(65)),
            format!("hmac-sha256:{}", "A".repeat(64)),
        ] {
            let mut malformed = manifest.clone();
            malformed.identity.providers[0].config_fingerprint = invalid;
            assert!(
                malformed
                    .validate_identity(&options, &[&provider])
                    .expect_err("畸形配置 HMAC 必须在恢复请求前失败")
                    .contains("HMAC 格式无效")
            );
        }

        let mut malformed_credential = manifest;
        malformed_credential.identity.providers[0].credential_proof =
            format!("hmac-sha256:{}", "g".repeat(64));
        assert!(
            malformed_credential
                .validate_identity(&options, &[&provider])
                .expect_err("畸形凭据 HMAC 必须在恢复请求前失败")
                .contains("HMAC 格式无效")
        );
    }

    /// 验证两个新运行使用不同随机盐，凭据证明不能跨运行稳定关联。
    #[test]
    fn resume_manifest_随机盐使同一凭据跨运行证明不同() {
        let provider = provider();
        let options = runtime_options();
        let first = ResumeManifest::new(
            RunMetadata::new("first".to_owned(), &options).expect("应能创建首个运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建首个恢复清单");
        let second = ResumeManifest::new(
            RunMetadata::new("second".to_owned(), &options).expect("应能创建第二个运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建第二个恢复清单");
        assert_ne!(first.identity.run_salt, second.identity.run_salt);
        assert_ne!(
            first.identity.providers[0].credential_proof,
            second.identity.providers[0].credential_proof
        );
    }

    /// 验证可注入 rename 在短暂失败后按固定次数退避，且只在源仍存在时重试。
    #[test]
    fn atomic_replace_短暂占用后成功且清理临时源() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-provider-replace-retry-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("应能创建 rename 重试测试目录");
        let destination = root.join("state.json");
        let temporary = root.join(".state.json.test.tmp");
        fs::write(&destination, b"old").expect("应能写入旧目标");
        fs::write(&temporary, b"new").expect("应能写入临时源");
        let attempts = std::cell::Cell::new(0_usize);
        let delays = RefCell::new(Vec::new());
        commit_temporary_replace_with(
            &temporary,
            &destination,
            b"new",
            "测试状态",
            |source, target| {
                let current = attempts.get() + 1;
                attempts.set(current);
                if current <= 2 {
                    Err(std::io::Error::from_raw_os_error(5))
                } else {
                    fs::rename(source, target)
                }
            },
            |delay| delays.borrow_mut().push(delay),
            |_| true,
        )
        .expect("两次短暂占用后应成功提交");
        assert_eq!(attempts.get(), 3);
        assert_eq!(*delays.borrow(), WINDOWS_REPLACE_RETRY_DELAYS[..2].to_vec());
        assert_eq!(fs::read(&destination).expect("应能读取新目标"), b"new");
        assert!(!temporary.exists());
        fs::remove_dir_all(&root).expect("应能清理 rename 重试测试目录");
    }

    /// 验证全部短退避耗尽后旧文件仍完整存在，且临时源不会遗留。
    #[test]
    fn atomic_replace_重试耗尽保留旧文件并清理临时源() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-provider-replace-exhausted-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("应能创建 rename 耗尽测试目录");
        let destination = root.join("state.json");
        let temporary = root.join(".state.json.test.tmp");
        fs::write(&destination, b"old").expect("应能写入旧目标");
        fs::write(&temporary, b"new").expect("应能写入临时源");
        let attempts = std::cell::Cell::new(0_usize);
        let delays = std::cell::Cell::new(0_usize);
        let error = commit_temporary_replace_with(
            &temporary,
            &destination,
            b"new",
            "测试状态",
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::from_raw_os_error(33))
            },
            |_| delays.set(delays.get() + 1),
            |_| true,
        )
        .expect_err("持续占用必须在硬上限后失败");
        assert!(error.contains("重试已耗尽"));
        assert_eq!(attempts.get(), WINDOWS_REPLACE_RETRY_DELAYS.len() + 1);
        assert_eq!(delays.get(), WINDOWS_REPLACE_RETRY_DELAYS.len());
        assert_eq!(fs::read(&destination).expect("旧目标必须仍可读"), b"old");
        assert!(!temporary.exists());
        fs::remove_dir_all(&root).expect("应能清理 rename 耗尽测试目录");
    }

    /// 验证 rename 已提交却返回错误时依据源和目标状态收敛为成功，不重复覆盖。
    #[test]
    fn atomic_replace_错误返回后识别已提交状态() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-provider-replace-committed-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("应能创建 rename 已提交测试目录");
        let destination = root.join("state.json");
        let temporary = root.join(".state.json.test.tmp");
        fs::write(&destination, b"old").expect("应能写入旧目标");
        fs::write(&temporary, b"new").expect("应能写入临时源");
        let attempts = std::cell::Cell::new(0_usize);
        commit_temporary_replace_with(
            &temporary,
            &destination,
            b"new",
            "测试状态",
            |source, target| {
                attempts.set(attempts.get() + 1);
                fs::rename(source, target).expect("注入行为应先真实完成 rename");
                Err(std::io::Error::from_raw_os_error(5))
            },
            |_| panic!("已经提交的结果不应进入退避"),
            |_| true,
        )
        .expect("目标已成为预期内容时应识别为提交成功");
        assert_eq!(attempts.get(), 1);
        assert_eq!(fs::read(&destination).expect("应能读取新目标"), b"new");
        assert!(!temporary.exists());
        fs::remove_dir_all(&root).expect("应能清理 rename 已提交测试目录");
    }

    /// 验证 Windows 只把访问拒绝、共享冲突和锁冲突视为可短暂重试。
    #[cfg(windows)]
    #[test]
    fn atomic_replace_windows_只重试指定三类错误() {
        for code in [5, 32, 33] {
            assert!(is_transient_windows_replace_error(
                &std::io::Error::from_raw_os_error(code)
            ));
        }
        assert!(!is_transient_windows_replace_error(
            &std::io::Error::from_raw_os_error(2)
        ));
    }

    /// 验证同目录临时文件通过 rename replace 覆盖目标，读者不会观察到删除窗口。
    #[test]
    fn write_text_替换既有文件时没有删除窗口() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-atomic-replace-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建替换测试目录");
        let provider = provider();
        store
            .write_text("state.txt", "state-0", &[&provider])
            .expect("应能写入初始状态");
        let path = store.run_dir().join("state.txt");
        let running = Arc::new(AtomicBool::new(true));
        let missing = Arc::new(AtomicUsize::new(0));
        let reader_running = Arc::clone(&running);
        let reader_missing = Arc::clone(&missing);
        let reader = thread::spawn(move || {
            while reader_running.load(Ordering::Acquire) {
                match fs::read_to_string(&path) {
                    Ok(value) => assert!(value.starts_with("state-")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        reader_missing.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(error) => panic!("读取原子替换目标失败：{error}"),
                }
            }
        });
        for index in 1..=64 {
            store
                .write_text("state.txt", &format!("state-{index}"), &[&provider])
                .expect("原子替换应成功");
        }
        running.store(false, Ordering::Release);
        reader.join().expect("读取线程应正常结束");
        assert_eq!(missing.load(Ordering::Relaxed), 0);
        assert_eq!(
            fs::read_to_string(store.run_dir().join("state.txt")).expect("应能读取最终替换内容"),
            "state-64"
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理替换测试目录");
    }

    /// 验证 JSONL 已同步而 Manifest 落后时，冷恢复会吸收提交且忽略尾部半行。
    #[tokio::test]
    async fn resume_manifest_从提交日志冷恢复落后记录() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-journal-recovery-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建恢复测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("首次候选集合应能冻结");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("应能写入落后的初始清单");

        let mut record = passed_text_probe("buffered");
        let sequence = store
            .append_probe("run", &mut record, &[&provider])
            .expect("日志提交应完成本地同步");
        assert_eq!(sequence, 1);
        let expected_shapes = record.wire_response_shapes.clone();
        let mut journal = OpenOptions::new()
            .append(true)
            .open(&store.checkpoint_path)
            .expect("应能模拟写入尾部半行");
        journal
            .write_all(b"{\"schemaVersion\":\"1\"")
            .expect("应能模拟崩溃尾部半行");
        journal.sync_all().expect("尾部半行应可同步用于恢复测试");
        drop(journal);

        let loaded = store
            .load_resume_manifest(&[&provider])
            .expect("冷恢复应合并完整日志并截断尾部半行");
        assert_eq!(loaded.journal_sequence, 1);
        assert_eq!(loaded.records.len(), 1);
        let loaded_record = loaded.records.values().next().expect("应恢复一条日志记录");
        assert_eq!(loaded_record.wire_response_shapes, expected_shapes);
        let fixture_relative = loaded_record
            .fixture_paths
            .first()
            .expect("恢复记录应引用 Fixture");
        let fixture_text = fs::read_to_string(store.run_dir().join(fixture_relative))
            .expect("应能读取恢复记录 Fixture");
        let fixture = parse_fixture_envelope(&fixture_text).expect("恢复 Fixture 应严格有效");
        let fixture_shapes = fixture
            .payload
            .exchanges
            .iter()
            .map(|exchange| exchange.response_shape.clone())
            .collect::<Vec<_>>();
        assert_eq!(fixture_shapes, expected_shapes);
        let reusable = store
            .reusable_records(&loaded, &[&provider])
            .await
            .expect("恢复记录 Fixture 应完整");
        assert_eq!(reusable.len(), 1);
        assert_eq!(
            reusable
                .values()
                .next()
                .expect("应返回一条可复用记录")
                .wire_response_shapes,
            expected_shapes
        );
        assert!(
            fs::read(&store.checkpoint_path)
                .expect("应能读取截断后的日志")
                .ends_with(b"\n")
        );
        store
            .write_resume_manifest(&loaded, &[&provider])
            .expect("应能把合并结果写回恢复清单");
        let reloaded = store
            .load_resume_manifest(&[&provider])
            .expect("修复后的清单应与日志前缀完全一致");
        assert_eq!(reloaded.journal_sequence, 1);
        assert_eq!(
            reloaded
                .records
                .values()
                .next()
                .expect("重新加载后仍应保留记录")
                .wire_response_shapes,
            expected_shapes
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理恢复测试目录");
    }

    /// 验证崩溃留下的多字节 UTF-8 半字符只会被当作未完整尾部截断。
    #[test]
    fn load_progress_journal_安全截断不完整utf8尾部() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-journal-utf8-tail-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建 UTF-8 尾部测试目录");
        let provider = provider();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &runtime_options()).expect("应能创建运行元数据"),
            &runtime_options(),
            &[&provider],
        )
        .expect("应能创建恢复清单");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加前应能写入已认证恢复清单");
        let mut record = passed_text_probe("buffered");
        store
            .append_probe("run", &mut record, &[&provider])
            .expect("应能写入一条完整日志");
        let complete = fs::read(&store.checkpoint_path).expect("应能读取完整日志前缀");
        let mut journal = OpenOptions::new()
            .append(true)
            .open(&store.checkpoint_path)
            .expect("应能模拟写入多字节 UTF-8 尾部");
        journal
            .write_all(&[0xe4, 0xb8])
            .expect("应能写入三字节字符的前两个字节");
        journal.sync_all().expect("不完整 UTF-8 尾部应可同步");
        drop(journal);

        let entries = store
            .load_progress_journal(
                &manifest,
                &[&provider],
                JOURNAL_SCHEMA_VERSION,
                JournalTailPolicy::RepairInPlace,
            )
            .expect("不完整 UTF-8 尾部不应阻断完整日志恢复");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            fs::read(&store.checkpoint_path).expect("应能读取截断后的日志"),
            complete
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理 UTF-8 尾部测试目录");
    }

    /// 验证恢复日志的父目录链接到运行目录外时，外部文件不会被打开或截断。
    #[test]
    fn load_progress_journal_拒绝目录链接且不截断外部文件() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-journal-link-{}-{unique}",
            std::process::id()
        ));
        let external_root = std::env::temp_dir().join(format!(
            "keencode-provider-journal-external-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建链接测试目录");
        let link_provider = provider();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &runtime_options()).expect("应能创建运行元数据"),
            &runtime_options(),
            &[&link_provider],
        )
        .expect("应能创建恢复清单");
        store
            .write_resume_manifest(&manifest, &[&link_provider])
            .expect("应能写入链接注入前的有效恢复清单");
        fs::create_dir(&external_root).expect("应能创建运行目录外的测试目录");
        let external_checkpoint = external_root.join("progress.jsonl");
        let external_content = b"{\"schemaVersion\":\"1\"";
        fs::write(&external_checkpoint, external_content).expect("应能写入外部恢复日志");
        let external_len = fs::metadata(&external_checkpoint)
            .expect("应能读取外部恢复日志元数据")
            .len();
        let run_dir = store.run_dir().to_path_buf();
        let log_dir = run_dir.join("sanitized-logs");
        drop(store);
        fs::remove_dir(&log_dir).expect("原始脱敏日志目录应为空并可删除");
        create_directory_link(&external_root, &log_dir);

        let error = ReportStore::open_resume(&run_dir)
            .err()
            .expect("恢复 Store 必须在打开任何日志文件前拒绝目录链接");
        assert!(error.contains("符号链接") || error.contains("重解析点"));
        assert_eq!(
            fs::metadata(&external_checkpoint)
                .expect("外部恢复日志仍应存在")
                .len(),
            external_len
        );
        assert_eq!(
            fs::read(&external_checkpoint).expect("应能读取未改动的外部恢复日志"),
            external_content
        );

        remove_directory_link(&log_dir);
        fs::remove_dir_all(&output_root).expect("应能清理链接测试目录");
        fs::remove_dir_all(&external_root).expect("应能清理外部测试目录");
    }

    /// 验证同一运行目录在一个 Store 存活期间不能被第二个进程语义的 Store 打开。
    #[test]
    fn report_store_持有跨进程独占锁直到运行结束() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-exclusive-lock-{}-{unique}",
            std::process::id()
        ));
        let first = ReportStore::create(&output_root, "run").expect("首个 Store 应取得独占锁");
        let error = ReportStore::open_resume(first.run_dir())
            .err()
            .expect("第二个 Store 必须因独占锁失败");
        assert!(error.contains("正在被另一个"), "意外锁错误：{error}");
        let run_dir = first.run_dir().to_path_buf();
        drop(first);
        let resumed = ReportStore::open_resume(&run_dir).expect("首个 Store 释放后应能重新取得锁");
        drop(resumed);
        fs::remove_dir_all(&output_root).expect("应能清理独占锁测试目录");
    }

    /// 由父测试启动的独立进程，验证锁在进程边界上的阻塞与释放语义。
    #[test]
    fn report_store_跨进程锁子进程() {
        let Some(run_dir) = std::env::var_os("KEENCODE_PROVIDER_LOCK_TEST_DIR") else {
            return;
        };
        let expectation =
            std::env::var("KEENCODE_PROVIDER_LOCK_TEST_EXPECT").expect("锁子进程必须收到预期结果");
        match expectation.as_str() {
            "blocked" => {
                let error = ReportStore::open_resume(Path::new(&run_dir))
                    .err()
                    .expect("父进程持锁时子进程必须失败");
                assert!(error.contains("正在被另一个"), "意外锁错误：{error}");
            }
            "released" => {
                let store = ReportStore::open_resume(Path::new(&run_dir))
                    .expect("父进程销毁 Store 后子进程必须取得锁");
                drop(store);
            }
            other => panic!("未知锁测试预期：{other}"),
        }
    }

    /// 验证第二个操作系统进程不能持有同一运行锁，且 RAII 销毁后立即释放。
    #[test]
    fn report_store_跨进程独占锁并在销毁后释放() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-process-lock-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("父进程应取得运行锁");
        let run_dir = store.run_dir().to_path_buf();
        let test_binary = std::env::current_exe().expect("应能定位当前测试二进制");
        let blocked = Command::new(&test_binary)
            .args(["--exact", "report::tests::report_store_跨进程锁子进程"])
            .env("KEENCODE_PROVIDER_LOCK_TEST_DIR", &run_dir)
            .env("KEENCODE_PROVIDER_LOCK_TEST_EXPECT", "blocked")
            .status()
            .expect("应能启动持锁验证子进程");
        assert!(blocked.success());

        drop(store);
        let released = Command::new(&test_binary)
            .args(["--exact", "report::tests::report_store_跨进程锁子进程"])
            .env("KEENCODE_PROVIDER_LOCK_TEST_DIR", &run_dir)
            .env("KEENCODE_PROVIDER_LOCK_TEST_EXPECT", "released")
            .status()
            .expect("应能启动释放验证子进程");
        assert!(released.success());
        fs::remove_dir_all(&output_root).expect("应能清理跨进程锁测试目录");
    }

    /// 由父测试启动的独立进程，验证全局锁不受运行目录和输出根影响。
    #[test]
    fn live_test_process_lock_跨进程锁子进程() {
        let Some(user_data_directory) =
            std::env::var_os("KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_DATA_DIR")
        else {
            return;
        };
        let output_root = PathBuf::from(
            std::env::var_os("KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_OUTPUT")
                .expect("全局锁子进程必须收到独立输出根"),
        );
        let expectation = std::env::var("KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_EXPECT")
            .expect("全局锁子进程必须收到预期结果");
        match expectation.as_str() {
            "blocked" => {
                let error = LiveTestProcessLock::acquire(Path::new(&user_data_directory))
                    .err()
                    .expect("父进程持有全局锁时子进程必须失败");
                assert!(
                    error.contains("拒绝并行发送真实请求"),
                    "意外全局锁错误：{error}"
                );
                assert!(!output_root.exists());
            }
            "released" => {
                let process_lock = LiveTestProcessLock::acquire(Path::new(&user_data_directory))
                    .expect("父进程释放后子进程必须取得全局锁");
                let store = ReportStore::create(&output_root, "child-run")
                    .expect("释放后应能在不同输出根创建运行");
                drop(store);
                drop(process_lock);
            }
            other => panic!("未知全局锁测试预期：{other}"),
        }
    }

    /// 验证不同临时目录、Run 目录与输出根仍共享同一用户级真实请求锁。
    #[test]
    fn live_test_process_lock_阻止不同输出根的跨进程运行() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "keencode-provider-global-process-lock-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&base).expect("应能创建全局锁测试根目录");
        let user_data_directory = base.join("user-data");
        let child_temp_directory = base.join("child-temp");
        fs::create_dir(&child_temp_directory).expect("应能创建不同的子进程临时目录");
        let first_output_root = base.join("output-a");
        let second_output_root = base.join("output-b");
        let process_lock = LiveTestProcessLock::acquire(&user_data_directory)
            .expect("父进程应创建用户数据目录并取得全局锁");
        let store = ReportStore::create(&first_output_root, "parent-run")
            .expect("父进程应能创建第一个输出根");
        let test_binary = std::env::current_exe().expect("应能定位当前测试二进制");
        let blocked = Command::new(&test_binary)
            .args([
                "--exact",
                "report::tests::live_test_process_lock_跨进程锁子进程",
            ])
            .env(
                "KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_DATA_DIR",
                &user_data_directory,
            )
            .env(
                "KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_OUTPUT",
                &second_output_root,
            )
            .env("KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_EXPECT", "blocked")
            .env("TEMP", &child_temp_directory)
            .env("TMP", &child_temp_directory)
            .env("TMPDIR", &child_temp_directory)
            .status()
            .expect("应能启动全局锁阻断验证子进程");
        assert!(blocked.success());
        assert!(!second_output_root.exists());

        drop(store);
        drop(process_lock);
        let released = Command::new(&test_binary)
            .args([
                "--exact",
                "report::tests::live_test_process_lock_跨进程锁子进程",
            ])
            .env(
                "KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_DATA_DIR",
                &user_data_directory,
            )
            .env(
                "KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_OUTPUT",
                &second_output_root,
            )
            .env("KEENCODE_PROVIDER_GLOBAL_LOCK_TEST_EXPECT", "released")
            .env("TEMP", &child_temp_directory)
            .env("TMP", &child_temp_directory)
            .env("TMPDIR", &child_temp_directory)
            .status()
            .expect("应能启动全局锁释放验证子进程");
        assert!(released.success());
        assert!(second_output_root.join("child-run").is_dir());
        fs::remove_dir_all(&base).expect("应能清理全局锁跨进程测试目录");
    }

    /// 验证新写入路径直接拒绝同一个稳定键的重复 Journal 提交。
    #[test]
    fn append_probe_拒绝重复稳定键() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-duplicate-journal-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建重复日志测试目录");
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加前应能写入已认证恢复清单");
        let mut record = probe("text", "buffered", "failed");
        assert_eq!(
            store
                .append_probe("run", &mut record, &[&provider])
                .expect("首次提交应成功"),
            1
        );
        let error = store
            .append_probe("run", &mut record, &[&provider])
            .expect_err("相同稳定键不得再次追加");
        assert!(error.contains("拒绝重复写入"));
        let journal = fs::read_to_string(&store.checkpoint_path).expect("应能读取提交日志");
        assert_eq!(journal.lines().count(), 1);
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理重复日志测试目录");
    }

    /// 验证冷恢复绝不采用同一稳定键的最后不同内容覆盖先前记录。
    #[test]
    fn reconcile_progress_journal_拒绝同键冲突内容() {
        let mut first = probe("text", "buffered", "failed");
        let mut second = first.clone();
        second.status = "passed".to_owned();
        first.wire_exchanges.clear();
        second.wire_exchanges.clear();
        let journal = vec![
            OwnedProbeJournalEntry {
                schema_version: JOURNAL_SCHEMA_VERSION.to_owned(),
                sequence: 1,
                previous_mac: None,
                record_mac: None,
                record: first,
            },
            OwnedProbeJournalEntry {
                schema_version: JOURNAL_SCHEMA_VERSION.to_owned(),
                sequence: 2,
                previous_mac: None,
                record_mac: None,
                record: second,
            },
        ];
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let error = reconcile_progress_journal(&mut manifest, &journal)
            .expect_err("同键不同内容必须被判定为日志损坏");
        assert!(error.contains("拒绝最后写入覆盖"));
    }

    /// 验证 Manifest 提交同时拒绝序号跳跃和同一稳定键的不同内容。
    #[test]
    fn resume_manifest_拒绝序号跳跃与同键冲突() {
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let first = probe("text", "buffered", "failed");
        let sequence_error = manifest
            .commit_probe(2, first.clone())
            .expect_err("首条提交不能跳过序号一");
        assert!(sequence_error.contains("序号不连续"));
        manifest
            .commit_probe(1, first.clone())
            .expect("连续首条提交应成功");
        let mut conflicting = first;
        conflicting.status = "passed".to_owned();
        let key_error = manifest
            .commit_probe(2, conflicting)
            .expect_err("同一稳定键的不同内容必须失败");
        assert!(key_error.contains("拒绝最后写入覆盖"));
        assert_eq!(manifest.journal_sequence, 1);
        assert_eq!(manifest.records.len(), 1);
    }

    /// 验证有完整 Fixture 的请求终态和零请求配置失败可复用，非终态不会复用。
    #[tokio::test]
    async fn reusable_records_覆盖全部已提交终态() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-terminal-resume-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建终态恢复测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结候选模型");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加终态记录前应能写入已认证恢复清单");
        let mut passed = passed_text_probe("buffered");
        let passed_sequence = store
            .append_probe("run", &mut passed, &[&provider])
            .expect("通过记录应写出内容寻址 Fixture");
        manifest
            .commit_probe(passed_sequence, passed)
            .expect("通过记录应提交到恢复清单");
        let mut failed = failed_configuration_probe("streaming");
        let failed_sequence = store
            .append_probe("run", &mut failed, &[&provider])
            .expect("零请求配置失败应写入日志但不创建 Fixture");
        manifest
            .commit_probe(failed_sequence, failed)
            .expect("零请求配置失败应提交到恢复清单");
        let active = probe("active", "buffered", "running");
        manifest.records.insert(active.stable_key(), active);

        let reusable = store
            .reusable_records(&manifest, &[&provider])
            .await
            .expect("有完整证据的终态记录应通过恢复筛选");
        assert_eq!(reusable.len(), 2);
        for record in reusable.values() {
            assert!(matches!(record.status.as_str(), "passed" | "failed"));
        }
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理终态恢复测试目录");
    }

    /// 验证发送过请求的终态不能利用空 `fixturePaths` 绕过恢复证据门禁。
    #[tokio::test]
    async fn reusable_records_拒绝已请求记录缺少fixture() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-missing-fixture-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建缺失 Fixture 目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结候选模型");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加换绑记录前应能写入已认证恢复清单");
        let mut record = passed_text_probe("buffered");
        record.wire_exchanges.clear();
        manifest.records.insert(record.stable_key(), record);

        let error = store
            .reusable_records(&manifest, &[&provider])
            .await
            .expect_err("发送过请求但没有 Fixture 的记录必须失败");
        assert!(error.contains("必须且只能引用一个 Fixture"));
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理缺失 Fixture 测试目录");
    }

    /// 验证 Fixture 自身摘要正确时，Payload 仍必须逐字段绑定对应记录。
    #[tokio::test]
    async fn reusable_records_拒绝fixture与record换绑() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-fixture-rebind-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建换绑测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结候选模型");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加换绑记录前应能写入已认证恢复清单");
        let mut record = passed_text_probe("buffered");
        store
            .append_probe("run", &mut record, &[&provider])
            .expect("应能先写出合法内容寻址 Fixture");
        record
            .response
            .as_mut()
            .expect("测试通过记录必须有响应证据")
            .reported_model_redacted = Some("other-model".to_owned());
        manifest.records.insert(record.stable_key(), record);

        let error = store
            .reusable_records(&manifest, &[&provider])
            .await
            .expect_err("记录预期响应与 Fixture Payload 不同必须失败");
        assert!(error.contains("Payload 与记录身份、Marker 或预期结果不一致"));
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理换绑测试目录");
    }

    /// 验证 responseShape 即使重新计算 Envelope 摘要和内容寻址路径也不能与记录换绑。
    #[test]
    fn fixture_v6_response_shape重摘要后仍拒绝换绑() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-shape-rebind-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建结构换绑目录");
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加结构记录前应能写入已认证恢复清单");
        let mut record = passed_text_probe("buffered");
        store
            .append_probe("run", &mut record, &[&provider])
            .expect("应能写出合法结构证据 Fixture");
        let original_relative = record
            .fixture_paths
            .first()
            .expect("记录应引用结构证据 Fixture");
        let fixture_text = fs::read_to_string(store.run_dir().join(original_relative))
            .expect("应能读取结构证据 Fixture");
        let mut fixture = parse_fixture_envelope(&fixture_text).expect("原 Fixture 应严格有效");
        let response_shape = &mut fixture
            .payload
            .exchanges
            .first_mut()
            .expect("Fixture 应包含一次交换")
            .response_shape;
        response_shape.capture_truncated = !response_shape.capture_truncated;
        response_shape
            .validate()
            .expect("捕获截断与 EOF 是可独立并存的 Wire 事实");
        fixture.content_sha256 =
            fixture_payload_sha256(&fixture.payload).expect("篡改后的 Payload 应可重新计算摘要");
        let rebound_relative = fixture_relative_path(&fixture.payload, &fixture.content_sha256)
            .expect("篡改后的 Fixture 应可生成自洽内容寻址路径");

        let error =
            validate_fixture_record_binding(&manifest, &record, &rebound_relative, &fixture)
                .expect_err("重摘要后的 responseShape 仍不得与原记录换绑");
        assert!(error.contains("响应结构证据与 ProbeRecord 不一致"));

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理结构换绑目录");
    }

    /// 验证内容合法但没有任何已提交终态引用的 Fixture 仍会阻断恢复。
    #[tokio::test]
    async fn reusable_records_拒绝孤儿fixture() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-orphan-fixture-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建孤儿测试目录");
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let stable_key = probe_stable_key(
            "run",
            "provider",
            "model",
            "openai_responses",
            "buffered",
            "text",
        );
        let marker = marker_from_probe_stable_key(&stable_key, false);
        let text = synthetic_fixture("openai_responses", &marker, responses_text_request(&marker));
        let fixture = parse_fixture_envelope(&text).expect("测试 Fixture 应能严格解析");
        let relative = fixture_relative_path(&fixture.payload, &fixture.content_sha256)
            .expect("测试 Fixture 应能生成内容寻址路径");
        store
            .write_immutable_relative_text(&relative, &text, &[&provider])
            .expect("应能写入合法但未引用的 Fixture");

        let error = store
            .reusable_records(&manifest, &[&provider])
            .await
            .expect_err("孤儿 Fixture 必须阻断恢复");
        assert!(error.contains("孤儿文件"));
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理孤儿 Fixture 测试目录");
    }

    /// 验证恢复协议仅删除结构、摘要、Marker 和运行身份都完整的未提交 Fixture。
    #[tokio::test]
    async fn repair_uncommitted_fixtures_清理合法孤儿并恢复空运行() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-repair-orphan-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建孤儿修复目录");
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let stable_key = probe_stable_key(
            "run",
            "provider",
            "model",
            "openai_responses",
            "buffered",
            "text",
        );
        let marker = marker_from_probe_stable_key(&stable_key, false);
        let text = synthetic_fixture("openai_responses", &marker, responses_text_request(&marker));
        let fixture = parse_fixture_envelope(&text).expect("测试 Fixture 应能严格解析");
        let relative = fixture_relative_path(&fixture.payload, &fixture.content_sha256)
            .expect("测试 Fixture 应能生成内容寻址路径");
        store
            .write_immutable_relative_text(&relative, &text, &[&provider])
            .expect("应能写入待修复的合法孤儿 Fixture");
        let staging_name = format!("{FIXTURE_STAGING_PREFIX}partial.1.1.0.tmp");
        let staging_path = store.run_dir().join("fixtures").join(&staging_name);
        fs::write(&staging_path, b"{\"partial\":")
            .expect("应能模拟 Fixture 临时文件中途写入后崩溃");

        assert_eq!(
            store
                .repair_uncommitted_fixtures(&manifest, &[&provider])
                .expect("合法未提交 Fixture 应能修复"),
            2
        );
        assert!(!store.run_dir().join(&relative).exists());
        assert!(!staging_path.exists());
        assert!(
            store
                .reusable_records(&manifest, &[&provider])
                .await
                .expect("修复后的空运行应可继续")
                .is_empty()
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理孤儿修复测试目录");
    }

    /// 验证恢复修复不会删除结构损坏且无法证明由当前运行创建的孤儿 Fixture。
    #[test]
    fn repair_uncommitted_fixtures_畸形孤儿失败且保留原文件() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-malformed-orphan-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建畸形孤儿测试目录");
        let provider = provider();
        let options = runtime_options();
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let relative = format!("fixtures/{}-{}.json", "0".repeat(64), "1".repeat(64));
        let malformed = b"{";
        fs::write(store.run_dir().join(&relative), malformed)
            .expect("应能注入结构损坏的孤儿 Fixture");

        let error = store
            .repair_uncommitted_fixtures(&manifest, &[&provider])
            .expect_err("畸形孤儿不得被当作可安全清理的崩溃残留");
        assert!(error.contains("Fixture 必须是有效"));
        assert_eq!(
            fs::read(store.run_dir().join(&relative)).expect("恢复失败后畸形孤儿必须保留"),
            malformed
        );

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理畸形孤儿测试目录");
    }

    /// 验证已提交 Stable Key 出现第二个自洽 Fixture 时拒绝清理并保留冲突证据。
    #[test]
    fn repair_uncommitted_fixtures_已提交稳定键路径冲突失败且保留文件() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-conflicting-orphan-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建路径冲突测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加冲突记录前应能写入已认证恢复清单");
        let mut record = passed_text_probe("buffered");
        let sequence = store
            .append_probe("run", &mut record, &[&provider])
            .expect("应能写入已提交记录的原始 Fixture");
        manifest
            .commit_probe(sequence, record.clone())
            .expect("应能提交原始 Fixture 记录");
        let committed_relative = record
            .fixture_paths
            .first()
            .expect("已提交记录必须引用原始 Fixture")
            .clone();
        let committed_text = fs::read_to_string(store.run_dir().join(&committed_relative))
            .expect("应能读取原始 Fixture");
        let mut conflicting =
            parse_fixture_envelope(&committed_text).expect("原始 Fixture 应能严格解析");
        conflicting.payload.model = "conflicting-model".to_owned();
        conflicting.content_sha256 =
            fixture_payload_sha256(&conflicting.payload).expect("应能重算冲突 Payload 摘要");
        let conflicting_relative =
            fixture_relative_path(&conflicting.payload, &conflicting.content_sha256)
                .expect("应能生成冲突 Fixture 的内容寻址路径");
        assert_ne!(committed_relative, conflicting_relative);
        let conflicting_text = format!(
            "{}\n",
            serde_json::to_string_pretty(&conflicting).expect("应能序列化自洽冲突 Fixture")
        );
        store
            .write_immutable_relative_text(&conflicting_relative, &conflicting_text, &[&provider])
            .expect("应能写入同 Stable Key 的第二个自洽 Fixture");

        let error = store
            .repair_uncommitted_fixtures(&manifest, &[&provider])
            .expect_err("已提交 Stable Key 的第二个 Fixture 必须被视为冲突");
        assert!(error.contains("稳定键已提交但引用路径冲突"));
        assert!(store.run_dir().join(&committed_relative).exists());
        assert_eq!(
            fs::read_to_string(store.run_dir().join(&conflicting_relative))
                .expect("恢复失败后冲突 Fixture 必须保留"),
            conflicting_text
        );

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理路径冲突测试目录");
    }

    /// 验证不同稳定键不能共享同一份 Fixture，即使两条记录各自满足状态不变量。
    #[tokio::test]
    async fn reusable_records_拒绝重复fixture引用() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-duplicate-fixture-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建重复引用测试目录");
        let provider = provider();
        let options = runtime_options();
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结候选模型");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加重复引用记录前应能写入已认证恢复清单");
        let buffered = passed_text_probe("buffered");
        let streaming = passed_text_probe("streaming");
        let (mut first, mut second) = if buffered.stable_key < streaming.stable_key {
            (buffered, streaming)
        } else {
            (streaming, buffered)
        };
        store
            .append_probe("run", &mut first, &[&provider])
            .expect("排序靠前记录应先写出合法 Fixture");
        second.wire_exchanges.clear();
        second.fixture_paths = first.fixture_paths.clone();
        manifest.records.insert(first.stable_key(), first);
        manifest.records.insert(second.stable_key(), second);

        let error = store
            .reusable_records(&manifest, &[&provider])
            .await
            .expect_err("两个稳定键引用同一 Fixture 必须失败");
        assert!(error.contains("重复引用 Fixture"));
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理重复引用测试目录");
    }

    /// 验证无效认证诊断使用固定合成模型，不会随冻结候选集合首项变化。
    #[tokio::test]
    async fn reusable_records_无效认证诊断绑定固定模型() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-auth-model-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建诊断测试目录");
        let provider = provider();
        let mut options = runtime_options();
        options.full_matrix = true;
        let mut manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        manifest
            .register_candidates("provider", ["aaa-new-first".to_owned(), "model".to_owned()])
            .expect("应能冻结会改变排序首项的候选集合");
        let mut record = failed_configuration_probe("buffered");
        record.model = "keencode-authentication-probe".to_owned();
        record.capability = "diagnostic_invalid_authentication".to_owned();
        record.stable_key = probe_stable_key(
            "run",
            "provider",
            &record.model,
            &record.protocol,
            &record.response_mode,
            &record.capability,
        );
        manifest.records.insert(record.stable_key(), record);

        assert_eq!(
            store
                .reusable_records(&manifest, &[&provider])
                .await
                .expect("固定诊断模型不应受候选排序变化影响")
                .len(),
            1
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理诊断模型测试目录");
    }

    /// 验证冷读取明确拒绝上一代 Journal v3。
    #[test]
    fn load_progress_journal_v3_明确拒绝() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-journal-v3-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建旧日志版本目录");
        let provider = provider();
        let record = probe("text", "buffered", "failed");
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &runtime_options()).expect("应能创建运行元数据"),
            &runtime_options(),
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let entry = ProbeJournalEntry {
            schema_version: "3",
            sequence: 1,
            previous_mac: JOURNAL_INITIAL_MAC,
            record_mac: JOURNAL_INITIAL_MAC,
            record: &record,
        };
        let line = format!(
            "{}\n",
            serde_json::to_string(&entry).expect("旧版本日志应可序列化")
        );
        fs::write(&store.checkpoint_path, line).expect("应能写入旧版本测试日志");

        let error = store
            .load_progress_journal(
                &manifest,
                &[&provider],
                JOURNAL_SCHEMA_VERSION,
                JournalTailPolicy::RepairInPlace,
            )
            .err()
            .expect("Journal v3 必须被当前 Harness 拒绝");
        assert!(error.contains("schema 不受支持：3"));

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理旧日志版本目录");
    }

    /// 验证磁盘 Journal 的首条序号不是一时，冷读取立即拒绝。
    #[test]
    fn load_progress_journal_拒绝序号跳跃() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-sequence-conflict-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建序号测试目录");
        let provider = provider();
        let record = probe("text", "buffered", "failed");
        let manifest = ResumeManifest::new(
            RunMetadata::new("run".to_owned(), &runtime_options()).expect("应能创建运行元数据"),
            &runtime_options(),
            &[&provider],
        )
        .expect("应能创建恢复清单");
        let entry = ProbeJournalEntry {
            schema_version: JOURNAL_SCHEMA_VERSION,
            sequence: 2,
            previous_mac: JOURNAL_INITIAL_MAC,
            record_mac: JOURNAL_INITIAL_MAC,
            record: &record,
        };
        let line = format!(
            "{}\n",
            serde_json::to_string(&entry).expect("应能序列化跳跃序号日志")
        );
        fs::write(&store.checkpoint_path, line).expect("应能写入跳跃序号日志");
        let error = store
            .load_progress_journal(
                &manifest,
                &[&provider],
                JOURNAL_SCHEMA_VERSION,
                JournalTailPolicy::RepairInPlace,
            )
            .err()
            .expect("冷读取必须拒绝跳跃序号");
        assert!(error.contains("序号不连续"));
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理序号测试目录");
    }

    /// 验证长度前缀稳定键不会因 Provider 和模型中的分隔符产生碰撞。
    #[test]
    fn probe_stable_key_拒绝分隔符碰撞() {
        let left = probe_stable_key("run", "provider|model", "x", "p", "m", "c");
        let right = probe_stable_key("run", "provider", "model|x", "p", "m", "c");
        assert_ne!(left, right);
        assert!(left.starts_with("probe-key-v1:sha256:"));
    }

    /// 验证主标记从完整稳定键派生，首轮标记再从主标记派生且不受分隔符碰撞影响。
    #[test]
    fn marker_沿稳定键派生并拒绝分隔符碰撞() {
        let left_key = probe_stable_key("run", "provider|model", "x", "p", "m", "c");
        let right_key = probe_stable_key("run", "provider", "model|x", "p", "m", "c");
        let left_main = marker_from_probe_stable_key(&left_key, false);
        let right_main = marker_from_probe_stable_key(&right_key, false);
        let left_first = first_turn_marker(&left_main);
        let right_first = first_turn_marker(&right_main);

        assert_ne!(left_key, right_key);
        assert_ne!(left_main, right_main);
        assert_ne!(left_first, right_first);
        assert_ne!(left_first, left_main);
        assert!(left_main.starts_with("KC_OK_"));
        assert!(left_first.starts_with("KC_FIRST_"));
        assert_ne!(
            marker_from_probe_stable_key(&left_key, true),
            left_main,
            "诊断标记必须使用独立域"
        );
    }

    /// 验证不可变 Fixture 同路径可幂等重放相同字节，但拒绝任何不同内容。
    #[test]
    fn immutable_fixture_同路径只接受相同字节() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-immutable-fixture-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建不可变测试目录");
        let provider = provider();
        let relative = format!("fixtures/{}-{}.json", "1".repeat(64), "2".repeat(64));
        let first_marker = "KC_OK_0123456789abcdef";
        let first = synthetic_fixture(
            "openai_responses",
            first_marker,
            responses_text_request(first_marker),
        );
        store
            .write_immutable_relative_text(&relative, &first, &[&provider])
            .expect("首次不可变写入应成功");
        store
            .write_immutable_relative_text(&relative, &first, &[&provider])
            .expect("逐字节相同的重复写入应幂等成功");

        let second_marker = "KC_OK_fedcba9876543210";
        let second = synthetic_fixture(
            "openai_responses",
            second_marker,
            responses_text_request(second_marker),
        );
        assert!(
            store
                .write_immutable_relative_text(&relative, &second, &[&provider])
                .expect_err("同路径不同内容必须失败")
                .contains("不同内容")
        );
        assert_eq!(
            fs::read_to_string(store.run_dir().join(&relative)).expect("应能读取既有 Fixture"),
            first
        );
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理不可变测试目录");
    }

    /// 验证全部禁止模式和非合成 Fixture 都会在目标文件或临时文件创建前失败。
    #[test]
    fn write_paths_在落盘前拒绝不安全内容() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-prewrite-guard-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建写盘门禁目录");
        let provider = provider();

        let synthetic_prefixed_secret = ["sk", "1234567890abcdef"].join("-");
        let unsafe_fixture = serde_json::json!({
            "syntheticOnly": true,
            "requestBody": {"token": synthetic_prefixed_secret}
        })
        .to_string();
        let fixture_error = store
            .write_relative_text("fixtures/unsafe.json", &unsafe_fixture, &[&provider])
            .expect_err("通用秘密 Token 必须在 Fixture 写盘前被拒绝");
        assert!(fixture_error.contains("通用秘密 Token"));
        assert!(
            !store
                .run_dir()
                .join("fixtures")
                .join("unsafe.json")
                .exists()
        );
        assert!(
            !store
                .run_dir()
                .join("fixtures")
                .join(".unsafe.json.tmp")
                .exists()
        );

        let marker = "KC_OK_0123456789abcdef";
        let non_synthetic = synthetic_fixture_with_proof(
            "openai_responses",
            marker,
            serde_json::json!({
                "model": "model",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!(
                            "只输出下一行的精确标记，不要添加标点、Markdown、空格或解释：\n{marker}"
                        )
                    }]
                }]
            }),
            false,
        );
        let prompt_error = store
            .write_relative_text("fixtures/non-synthetic.json", &non_synthetic, &[&provider])
            .expect_err("非合成提示词 Fixture 必须在写盘前被拒绝");
        assert!(prompt_error.contains("纯合成提示词"));
        assert!(
            !store
                .run_dir()
                .join("fixtures")
                .join("non-synthetic.json")
                .exists()
        );
        assert!(
            !store
                .run_dir()
                .join("fixtures")
                .join(".non-synthetic.json.tmp")
                .exists()
        );

        let text_error = store
            .write_text(
                "unsafe.md",
                "Authorization: Bearer synthetic-sensitive-value",
                &[&provider],
            )
            .expect_err("认证 Header 必须在普通产物写盘前被拒绝");
        assert!(text_error.contains("认证字段"));
        assert!(!store.run_dir().join("unsafe.md").exists());
        assert!(!store.run_dir().join(".unsafe.md.tmp").exists());
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理写盘门禁测试目录");
    }

    /// 验证完成扫描读取实际文件并把命中反映到失败报告。
    #[test]
    fn scan_artifacts_不硬编码零命中() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-redaction-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建扫描测试目录");
        fs::write(
            store.run_dir().join("fixtures").join("unsafe.json"),
            "fixture-secret-value\nAuthorization: Bearer unsafe-value\nC:\\Users\\example",
        )
        .expect("应能写入扫描测试产物");
        let provider = provider();
        let report = store.scan_artifacts(&[&provider]).expect("真实扫描应完成");
        assert!(!report.passed);
        assert_eq!(report.exact_credential_matches, 1);
        assert_eq!(report.authentication_header_matches, 1);
        assert_eq!(report.absolute_path_matches, 1);
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理扫描测试目录");
    }

    /// 验证来源脱敏报告拒绝未知字段、秘密字段及与当前真实重扫不一致的声明。
    #[test]
    fn stored_redaction_report_严格结构并与真实重扫一致() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-stored-redaction-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建来源脱敏报告测试目录");
        let provider = provider();
        let scan = store
            .scan_artifacts(&[&provider])
            .expect("空运行目录应能完成真实扫描");
        let valid_bytes = serde_json::to_vec_pretty(&scan).expect("应能编码真实扫描报告");
        validate_stored_redaction_report(&valid_bytes, &[&provider])
            .expect("严格的真实零命中报告应有效");

        let mut unknown: serde_json::Value =
            serde_json::from_slice(&valid_bytes).expect("真实扫描报告应是有效 JSON");
        unknown["unexpected"] = serde_json::json!(true);
        assert!(
            validate_stored_redaction_report(
                &serde_json::to_vec(&unknown).expect("应能编码未知字段报告"),
                &[&provider],
            )
            .expect_err("未知字段必须被严格结构拒绝")
            .contains("unknown field")
        );

        unknown["unexpected"] = serde_json::json!("fixture-secret-value");
        assert!(
            validate_stored_redaction_report(
                &serde_json::to_vec(&unknown).expect("应能编码秘密字段报告"),
                &[&provider],
            )
            .expect_err("报告自身秘密必须先被安全门禁拒绝")
            .contains("完整 Provider 凭据")
        );

        store
            .write_json("redaction-report.json", &scan, &[&provider])
            .expect("应能写入原始真实扫描报告");
        let mut inconsistent: serde_json::Value =
            serde_json::from_slice(&valid_bytes).expect("真实扫描报告应是有效 JSON");
        inconsistent["scannedArtifacts"] = serde_json::json!(["invented-safe.txt"]);
        fs::write(
            store.run_dir().join("redaction-report.json"),
            serde_json::to_vec_pretty(&inconsistent).expect("应能编码不一致脱敏报告"),
        )
        .expect("应能写入不一致脱敏报告");
        assert!(
            store
                .read_and_verify_completed_redaction_report(&[&provider])
                .expect_err("持久化报告与真实重扫不一致时必须拒绝")
                .contains("真实重扫结果不一致")
        );

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理来源脱敏报告测试目录");
    }

    /// 验证完成来源不再跳过任意临时文件，遗留临时文件中的敏感内容会失败关闭。
    #[test]
    fn completed_source_redaction_扫描敏感临时文件() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-redaction-tmp-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建临时文件扫描测试目录");
        let provider = provider();
        let scan = store
            .scan_artifacts(&[&provider])
            .expect("初始目录应能完成真实扫描");
        store
            .write_json("redaction-report.json", &scan, &[&provider])
            .expect("应能写入初始脱敏报告");
        fs::write(store.run_dir().join("orphan.tmp"), b"fixture-secret-value")
            .expect("应能模拟遗留敏感临时文件");

        let rescanned = store
            .scan_artifacts(&[&provider])
            .expect("遗留临时文件必须被真实扫描");
        assert!(
            rescanned
                .scanned_artifacts
                .iter()
                .any(|path| path == "orphan.tmp")
        );
        assert_eq!(rescanned.exact_credential_matches, 1);
        assert!(
            store
                .read_and_verify_completed_redaction_report(&[&provider])
                .expect_err("敏感临时文件必须使完成来源失败关闭")
                .contains("真实重扫结果不一致")
        );

        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理临时文件扫描测试目录");
    }

    /// 验证 Fixture 一旦包含真实配置凭据，最终验收会写明命中并返回失败。
    #[test]
    fn finalize_在fixture含凭据时失败() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-finalize-secret-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建最终验收目录");
        fs::write(
            store.run_dir().join("fixtures").join("unsafe.json"),
            r#"{"syntheticOnly":true,"request":"fixture-secret-value"}"#,
        )
        .expect("应能注入待拒绝的测试 Fixture");
        let provider = provider();
        let error = store
            .finalize(&report_with_probes(Vec::new()), &[&provider])
            .expect_err("含完整凭据的 Fixture 必须使最终验收失败");
        assert!(error.contains("脱敏验收失败"));
        let scan: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(store.run_dir().join("redaction-report.json"))
                .expect("失败时仍应写出脱敏扫描报告"),
        )
        .expect("脱敏扫描报告应是有效 JSON");
        assert_eq!(scan["passed"], false);
        assert_eq!(scan["exactCredentialMatches"], 1);
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理最终验收测试目录");
    }

    /// 验证缺少纯合成提示词证明的 Fixture 会被最终扫描门禁拒绝。
    #[test]
    fn scan_artifacts_拒绝非合成提示词fixture() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-non-synthetic-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建提示词扫描目录");
        fs::write(
            store.run_dir().join("fixtures").join("unsafe.json"),
            r#"{"syntheticOnly":false,"requestBody":{"input":"用户真实内容"}}"#,
        )
        .expect("应能写入非合成提示词测试 Fixture");
        let provider = provider();
        let scan = store
            .scan_artifacts(&[&provider])
            .expect("非合成提示词扫描应完成");
        assert!(!scan.passed);
        assert_eq!(scan.non_synthetic_prompt_matches, 1);
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理提示词扫描测试目录");
    }

    /// 验证同一模型协议模式的不同能力不会在兼容矩阵中互相覆盖。
    #[test]
    fn compatibility_matrix_按能力建立独立行() {
        let report = report_with_probes(vec![
            probe("text", "buffered", "passed"),
            probe("reasoning", "buffered", "contract_violation"),
        ]);
        let matrix = compatibility_matrix(&report);
        assert!(matrix.contains("| text | passed | 未执行 |"));
        assert!(matrix.contains("| reasoning | contract_violation | 未执行 |"));
    }

    /// 验证本地回环断流和本地取消不会抬高 Provider 远端兼容统计。
    #[test]
    fn summary_record_local_only能力不污染provider兼容口径() {
        let mut remote = probe("text", "buffered", "passed");
        remote.attempts = 2;

        let mut interruption = probe("stream_interruption", "streaming", "passed");
        interruption.attempts = 1;
        interruption.normalized_error = Some(NormalizedError {
            kind: "stream_interrupted".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text("测试截断流"),
            retryable: true,
            http_status: None,
        });

        let mut cancellation = probe("cancellation", "buffered", "passed");
        cancellation.attempts = 3;
        cancellation.cancellation = Some(CancellationEvidence {
            cancel_after_ms: 100,
            local_future_dropped: true,
            first_event_received: false,
            completed_before_cancel: false,
            observed_latency_ms: 100,
            remote_termination_proven: false,
        });

        let report = report_with_probes(vec![remote, interruption, cancellation]);
        let summary = &report.summary;
        assert_eq!(summary.total_probes, 3);
        assert_eq!(summary.provider_compatibility_probes, 1);
        assert_eq!(summary.executed_probes, 1);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.contract_violations, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.local_conformance.total, 2);
        assert_eq!(summary.local_conformance.executed, 2);
        assert_eq!(summary.local_conformance.passed, 2);
        assert_eq!(summary.total_attempts, 5);
        assert_eq!(summary.local_loopback_attempts, 1);
        assert_eq!(
            summary
                .by_capability
                .get("stream_interruption")
                .expect("断流能力应有独立汇总")
                .scope,
            "adapter_conformance_local_only"
        );
        assert_eq!(
            summary
                .by_capability
                .get("cancellation")
                .expect("取消能力应有独立汇总")
                .scope,
            "client_conformance_local_only"
        );
        assert_eq!(
            summary
                .by_capability
                .get("text")
                .expect("文本能力应有独立汇总")
                .scope,
            "provider_compatibility_remote"
        );
    }

    /// 验证 local-only 矩阵按协议和模式聚合，且不暴露为 Provider 或模型兼容行。
    #[test]
    fn compatibility_matrix_local_only能力不关联provider或模型() {
        let mut first = probe("stream_interruption", "buffered", "passed");
        first.provider_id = "LOCAL_PROVIDER_ONE_MUST_NOT_APPEAR".to_owned();
        first.model = "LOCAL_MODEL_ONE_MUST_NOT_APPEAR".to_owned();
        let mut second = probe("stream_interruption", "buffered", "passed");
        second.provider_id = "LOCAL_PROVIDER_TWO_MUST_NOT_APPEAR".to_owned();
        second.model = "LOCAL_MODEL_TWO_MUST_NOT_APPEAR".to_owned();
        let mut cancellation = probe("cancellation", "streaming", "failed");
        cancellation.provider_id = "LOCAL_PROVIDER_THREE_MUST_NOT_APPEAR".to_owned();
        cancellation.model = "LOCAL_MODEL_THREE_MUST_NOT_APPEAR".to_owned();

        let matrix = compatibility_matrix(&report_with_probes(vec![first, second, cancellation]));
        assert!(matrix.contains("## 本地 Client/Adapter Conformance"));
        assert!(matrix.contains("adapter&#95;conformance&#95;local&#95;only"));
        assert!(matrix.contains("client&#95;conformance&#95;local&#95;only"));
        assert!(matrix.contains("passed×2"));
        assert!(matrix.contains("failed×1"));
        assert!(!matrix.contains("LOCAL_PROVIDER_"));
        assert!(!matrix.contains("LOCAL_MODEL_"));
        assert!(matrix.contains("未请求目标 Provider"));
        assert!(matrix.contains("未证明远端停止生成"));
    }

    /// 验证摘要明确展示 local-only 边界以及远端与回环请求尝试的拆分。
    #[test]
    fn summary_markdown_明确local_only证据边界与请求拆分() {
        let mut remote = probe("text", "buffered", "passed");
        remote.attempts = 2;
        let mut interruption = probe("stream_interruption", "streaming", "passed");
        interruption.attempts = 1;
        let mut cancellation = probe("cancellation", "buffered", "passed");
        cancellation.attempts = 3;
        cancellation.cancellation = Some(CancellationEvidence {
            cancel_after_ms: 100,
            local_future_dropped: true,
            first_event_received: false,
            completed_before_cancel: false,
            observed_latency_ms: 100,
            remote_termination_proven: false,
        });

        let markdown = summary_markdown(&report_with_probes(vec![
            remote,
            interruption,
            cancellation,
        ]));
        assert!(markdown.contains("- 全部事实记录：3"));
        assert!(markdown.contains("- Provider 远端兼容案例：1"));
        assert!(markdown.contains("- Provider 远端通过：1"));
        assert!(markdown.contains("- Local-only Conformance 案例：2"));
        assert!(markdown.contains("- Local-only Conformance 通过：2"));
        assert!(markdown.contains("- 目标 Provider 远端请求尝试：5"));
        assert!(markdown.contains("- Harness 本地回环请求尝试：1"));
        assert!(markdown.contains("remoteTerminationProven=false"));
        assert!(markdown.contains("不代表任何 Provider 或模型支持断流恢复"));
        assert!(markdown.contains("不证明远端停止生成、停止计费或支持取消"));
    }

    /// 验证总计与分能力统计使用统一状态语义。
    #[test]
    fn summary_record_区分契约失败和调用失败() {
        let report = report_with_probes(vec![
            probe("text", "buffered", "passed"),
            probe("reasoning", "buffered", "contract_violation"),
            probe("reasoning", "streaming", "failed"),
        ]);
        assert_eq!(report.summary.passed, 1);
        assert_eq!(report.summary.contract_violations, 1);
        assert_eq!(report.summary.failed, 1);
        let reasoning = report
            .summary
            .by_capability
            .get("reasoning")
            .expect("推理能力应有独立汇总");
        assert_eq!(reasoning.total, 2);
        assert_eq!(reasoning.contract_violations, 1);
        assert_eq!(reasoning.failed, 1);
    }

    /// 验证基础门禁跳过不会被误计为远端失败。
    #[test]
    fn summary_record_单独统计跳过与未验证() {
        let mut skipped = probe("reasoning", "streaming", "skipped");
        skipped.attempts = 0;
        skipped.skip_evidence = Some(SkipEvidence {
            verification: "unverified".to_owned(),
            reason: "base_text_transient_failure".to_owned(),
            blocked_by: "provider|model|openai_responses|streaming|text".to_owned(),
            gate_status: "failed".to_owned(),
            error_kind: Some("rate_limit".to_owned()),
            retryable: Some(true),
            http_status: Some(429),
        });
        let report = report_with_probes(vec![probe("text", "streaming", "failed"), skipped]);
        assert_eq!(report.summary.executed_probes, 1);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.summary.skipped, 1);
        assert_eq!(report.summary.unverified, 1);
        let reasoning = report
            .summary
            .by_capability
            .get("reasoning")
            .expect("推理能力应有独立汇总");
        assert_eq!(reasoning.executed, 0);
        assert_eq!(reasoning.failed, 0);
        assert_eq!(reasoning.skipped, 1);
        assert_eq!(reasoning.unverified, 1);
        assert!(
            compatibility_matrix(&report)
                .contains("skipped (unverified:base_text_transient_failure)")
        );
    }
}
