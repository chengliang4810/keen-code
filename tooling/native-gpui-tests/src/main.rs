//! Ely/GPUI 原生窗口验收器。
//!
//! 该程序只在 Windows 上执行真正的桌面窗口动作：通过 Win32 找到被测进程的
//! HWND，使用 SendInput 输入，使用 PrintWindow 截图，并通过 UI Automation 读取
//! AccessKit 暴露的根节点属性。它不加载 WebView、CDP、JavaScript 或前端夹具。
//! Provider 配置只被复制到本次运行的隔离数据根；报告只写配置路径和无凭据摘要。

#![deny(unsafe_op_in_unsafe_fn)]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
#[cfg(windows)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(windows)]
use std::thread::JoinHandle;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA,
    ERROR_PIPE_NOT_CONNECTED, FALSE, GetLastError, HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::System::IO::CancelSynchronousIo;
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};

#[cfg(windows)]
mod windows_runner;

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_WINDOW_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_ACTION_TIMEOUT_MS: u64 = 180_000;
const MAX_PLAN_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TRACE_BYTES: usize = 4 * 1024 * 1024;
// Goal 文档沿用 resources DocumentLimits 的默认上限，断言器只读取这一有界范围。
const MAX_GOAL_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;
const GOAL_DOCUMENT_SCHEMA: &str = "keencode/goal";
const GOAL_DOCUMENT_VERSION: u64 = 2;
// NativeServices 的 Automation 台账使用 8 MiB 上限；runner 只读取同样的有界范围。
const MAX_AUTOMATION_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;
// 与 NativeServices 的 Automation schema 2 对齐；旧版本台账由被测服务拒绝。
const AUTOMATION_DOCUMENT_SCHEMA: u64 = 2;
// 每次滚轮动作最多注入 16 个标准 Windows wheel notch，避免计划把异常大值
// 直接交给 SendInput；需要更长页面时由计划显式拆成多个 scroll 动作。
const MAX_SCROLL_DELTA_Y: i32 = 120 * 16;
// UIA 点击只允许有限次真实滚轮；目标没有移动时立即失败，避免把一个失效
// 的滚动容器拖到动作总超时。
const ACCESSIBILITY_AUTO_SCROLL_DELTA_Y: i32 = 120 * 8;
const MAX_ACCESSIBILITY_AUTO_SCROLLS: usize = 16;
#[cfg(windows)]
const MAX_PROCESS_OUTPUT_BYTES: usize = 128 * 1024;
#[cfg(windows)]
const PROCESS_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct Options {
    binary: PathBuf,
    /// 逐项原样传递给被测进程，不经过 shell 拼接或二次解析。
    binary_args: Vec<String>,
    provider_config: Option<PathBuf>,
    plan: Option<PathBuf>,
    output: PathBuf,
    /// 只读复用上一轮保留的 isolation/data/project，不在该根写入初始化文件。
    reuse_isolation: Option<PathBuf>,
    real_provider: bool,
    window_timeout_ms: u64,
    action_timeout_ms: u64,
    keep_isolation: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Plan {
    #[serde(default = "default_plan_version")]
    version: u32,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    requires_real_provider: bool,
    /// 需要从隔离 Session Journal 观察真实 Turn 开始和终态事件。
    #[serde(default)]
    requires_journal: bool,
    /// 需要被测 Provider 运行时写入非空且已脱敏的线级网络 trace。
    #[serde(default)]
    requires_network_trace: bool,
    /// 只允许在 `--reuse-isolation` 指向既有运行根时执行，避免冷恢复计划
    /// 在新建空根上被误报为通过。
    #[serde(default)]
    requires_reuse_isolation: bool,
    /// 新建隔离根时写入 NativeHost 读取的真实常规设置文件；复用模式只读该文件。
    #[serde(default)]
    native_general_settings: NativeGeneralSettings,
    /// 视觉验收的固定环境声明。runner 不改变桌面显示器状态，只在计划中留下可复核边界。
    #[serde(default)]
    visual: Option<VisualPlan>,
    #[serde(default)]
    actions: Vec<Action>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NativeGeneralSettings {
    #[serde(default = "default_acceptance_appearance")]
    appearance: String,
    #[serde(default = "default_acceptance_font_family")]
    font_family: String,
    #[serde(default = "default_acceptance_font_size_px")]
    font_size_px: f32,
    #[serde(default = "default_acceptance_density")]
    density: String,
    #[serde(default = "default_acceptance_reduced_motion")]
    reduced_motion: bool,
}

impl Default for NativeGeneralSettings {
    fn default() -> Self {
        Self {
            appearance: default_acceptance_appearance(),
            font_family: default_acceptance_font_family(),
            font_size_px: default_acceptance_font_size_px(),
            density: default_acceptance_density(),
            reduced_motion: default_acceptance_reduced_motion(),
        }
    }
}

impl NativeGeneralSettings {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.appearance.as_str(), "system" | "light" | "dark") {
            return Err(format!(
                "native_general_settings.appearance 不受支持：{}",
                self.appearance
            ));
        }
        if self.font_family.trim().is_empty()
            || self.font_family.len() > 256
            || self.font_family.chars().any(char::is_control)
        {
            return Err("native_general_settings.fontFamily 无效".to_owned());
        }
        if !self.font_size_px.is_finite() || !(11.0..=20.0).contains(&self.font_size_px) {
            return Err("native_general_settings.fontSizePx 必须在 11 到 20 之间".to_owned());
        }
        if !matches!(
            self.density.as_str(),
            "compact" | "standard" | "comfortable"
        ) {
            return Err(format!(
                "native_general_settings.density 不受支持：{}",
                self.density
            ));
        }
        Ok(())
    }
}

fn default_acceptance_appearance() -> String {
    // 深色是固定源基线主题；浅色计划显式覆盖此默认值。
    "dark".to_owned()
}

fn default_acceptance_font_family() -> String {
    "系统默认".to_owned()
}

fn default_acceptance_font_size_px() -> f32 {
    14.0
}

fn default_acceptance_density() -> String {
    "compact".to_owned()
}

fn default_acceptance_reduced_motion() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct VisualPlan {
    logical_width: u32,
    logical_height: u32,
    physical_client_width: u32,
    physical_client_height: u32,
    dpi: u32,
    device_scale_factor: u32,
    theme: String,
    source_baseline: String,
}

impl VisualPlan {
    fn validate(&self) -> Result<(), String> {
        if self.logical_width != 1280
            || self.logical_height != 820
            || self.physical_client_width != 2560
            || self.physical_client_height != 1640
            || self.dpi != 192
            || self.device_scale_factor != 2
        {
            return Err(
                "visual 必须固定为逻辑 1280x820、物理 client 2560x1640、DPI 192、DPR 2".to_owned(),
            );
        }
        if !matches!(self.theme.as_str(), "dark" | "light") {
            return Err(format!("visual.theme 不受支持：{}", self.theme));
        }
        if self.source_baseline.trim().is_empty() || self.source_baseline.len() > 512 {
            return Err("visual.sourceBaseline 必须是非空短路径".to_owned());
        }
        Ok(())
    }
}

fn default_plan_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Wait {
        ms: u64,
    },
    Focus,
    Click {
        x: i32,
        y: i32,
    },
    /// 在目标 HWND 的 client 坐标注入真实 Win32 垂直滚轮事件。
    ///
    /// `delta_y` 使用 Windows wheel units，负值向下滚动；坐标和 wheel 值
    /// 会在计划加载及运行时分别校验，避免把未约束输入交给 SendInput。
    Scroll {
        x: i32,
        y: i32,
        delta_y: i32,
    },
    TypeText {
        text: String,
    },
    Key {
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Screenshot {
        label: String,
    },
    Accessibility {
        label: String,
    },
    /// 等待真实 UI Automation/AccessKit 子树中出现精确名称。
    ///
    /// 该动作只读取被测窗口公开的 UIA 事实，不调用 RPC，也不向业务注入事件。
    WaitForAccessibility {
        label: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// 等待真实 UIA 名称、可见性和 enabled 状态满足唯一匹配后，点击其 bounds 中心。
    ///
    /// bounds 来自 UIA `CurrentBoundingRectangle` 的实时节点属性；runner 会将
    /// 屏幕矩形中心转换为目标 client 坐标，并校验矩形交集与最终点击点边界。
    ClickAccessibility {
        label: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Metrics {
        #[serde(default)]
        label: Option<String>,
    },
    /// 等待隔离 Session Journal 中出现指定类型的权威事件。
    ///
    /// 该动作只读取 events.jsonl，不通过 RPC、窗口内脚本或业务旁路判断成功。
    WaitForJournal {
        event_type: String,
        #[serde(default = "default_wait_count")]
        count: usize,
        #[serde(default)]
        timeout_ms: Option<u64>,
        /// 可选的 JSON Pointer -> 标量值精确匹配条件，指向当前 Journal 事件对象。
        #[serde(default)]
        matching: BTreeMap<String, Value>,
        /// 独立于 `matching` 的 JSON Pointer -> 字符串子串匹配条件；仅对字符串值生效。
        #[serde(default)]
        matching_text_contains: BTreeMap<String, String>,
        /// 可选的 JSON Pointer -> 隔离项目相对路径匹配；允许 Journal 使用绝对路径记录。
        #[serde(default)]
        matching_project_paths: BTreeMap<String, String>,
        /// 可选的物理 Journal 记录 Session 过滤条件。
        #[serde(default)]
        session_id: Option<String>,
        /// 可选的事件 payload Turn 过滤条件。
        #[serde(default)]
        turn_id: Option<String>,
    },
    /// 断言本轮相对 Journal baseline 的精确新增数量，避免旧会话历史满足断言。
    AssertJournalCount {
        event_type: String,
        count: usize,
        /// 可选的 JSON Pointer -> 标量值精确匹配条件，指向当前 Journal 事件对象。
        #[serde(default)]
        matching: BTreeMap<String, Value>,
        /// 独立于 `matching` 的 JSON Pointer -> 字符串子串匹配条件；仅对字符串值生效。
        #[serde(default)]
        matching_text_contains: BTreeMap<String, String>,
        /// 可选的 JSON Pointer -> 隔离项目相对路径匹配；允许 Journal 使用绝对路径记录。
        #[serde(default)]
        matching_project_paths: BTreeMap<String, String>,
        /// 可选的物理 Journal 记录 Session 过滤条件。
        #[serde(default)]
        session_id: Option<String>,
        /// 可选的事件 payload Turn 过滤条件。
        #[serde(default)]
        turn_id: Option<String>,
    },
    /// 等待隔离项目内的真实输出文件，并可要求文件包含一个非敏感标记。
    WaitForFile {
        path: String,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// 立即读取隔离根中的普通文件，只提供事实断言，不等待或写入文件。
    AssertFile {
        path: String,
        #[serde(default)]
        scope: FileScope,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        min_bytes: Option<u64>,
        /// 可选的 JSON Pointer -> 完整 JSON 值精确匹配条件；适合校验数组或对象。
        #[serde(default)]
        json_pointer_equals: BTreeMap<String, Value>,
    },
    /// 只读确认隔离根内没有生成指定文件，用于验证未执行的扩展不会留下副作用。
    AssertFileAbsent {
        path: String,
        #[serde(default)]
        scope: FileScope,
    },
    /// 只读断言隔离 data/goals 中唯一会话 Goal 文档的精确 JSON Pointer 值。
    AssertGoalState {
        matching: BTreeMap<String, Value>,
    },
    /// 只读断言 data/automations.json 中按唯一 title 定位的 Automation 记录。
    /// matching 的 Pointer 相对于该记录；history_outcomes 统计同一 automationId 的
    /// runs，其中 scheduled 来自 run.scheduled=true，其余键匹配 run.outcome。
    AssertAutomationState {
        title: String,
        #[serde(default)]
        matching: BTreeMap<String, Value>,
        #[serde(default)]
        history_outcomes: BTreeMap<String, usize>,
    },
    /// 只读检查隔离项目的 Git 根、HEAD、工作区和 linked worktree。
    AssertGit {
        #[serde(default)]
        require_clean: bool,
        #[serde(default)]
        commit_message_contains: Option<String>,
        #[serde(default)]
        worktree_count: Option<usize>,
        #[serde(default)]
        worktree_branch_contains: Option<String>,
    },
    AssertWindow {
        #[serde(default)]
        title_contains: Option<String>,
        #[serde(default)]
        class_contains: Option<String>,
        #[serde(default)]
        client_width: Option<u32>,
        #[serde(default)]
        client_height: Option<u32>,
        #[serde(default)]
        dpi: Option<u32>,
    },
    /// 按物理 client 尺寸调整真实 HWND；计划应在后续 wait 后用 assert_window 验证结果。
    ResizeClient {
        width: u32,
        height: u32,
    },
    Repeat {
        count: u32,
        actions: Vec<Action>,
    },
    Close,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FileScope {
    #[default]
    Project,
    Data,
}

fn default_wait_count() -> usize {
    1
}

#[derive(Debug, Serialize)]
struct RunReport {
    schema: &'static str,
    schema_version: u32,
    runner: &'static str,
    window_driver: &'static str,
    started_at_ms: u128,
    finished_at_ms: u128,
    binary_path: String,
    binary_sha256: String,
    binary_identity: BinaryIdentity,
    reused_from: Option<ReuseSummary>,
    provider: ProviderSummary,
    plan: PlanSummary,
    process: ProcessSummary,
    actions: Vec<ActionResult>,
    evidence: EvidenceSummary,
    status: &'static str,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProviderSummary {
    config_path: Option<String>,
    copied_to_isolated_data: bool,
    provider_count: Option<usize>,
    active_provider_id: Option<String>,
    active_model_id: Option<String>,
    real_provider_authorized: bool,
}

#[derive(Debug, Serialize)]
struct PlanSummary {
    path: Option<String>,
    name: Option<String>,
    version: u32,
    requires_real_provider: bool,
    requires_journal: bool,
    requires_network_trace: bool,
    requires_reuse_isolation: bool,
    native_general_settings: NativeGeneralSettings,
    visual: Option<VisualPlan>,
    action_count: usize,
}

#[derive(Debug, Serialize)]
struct ProcessSummary {
    pid: u32,
    hwnd: Option<String>,
    title: Option<String>,
    class_name: Option<String>,
    exit_code: Option<i32>,
    /// 区分被测程序自然退出与 runner 等待超时后的强制终止，避免把 kill 的退出码误判为应用异常。
    termination: &'static str,
    isolation_root: String,
    project_root: String,
}

#[derive(Debug, Serialize)]
struct BinaryIdentity {
    path: String,
    sha256: String,
    /// 标明 hash 来自当前启动还是来源报告，避免冷恢复报告混淆两个 binary。
    source: &'static str,
}

#[derive(Debug, Serialize)]
struct ReuseSummary {
    isolation_root: String,
    source_report: String,
    source_binary: BinaryIdentity,
}

#[derive(Debug, Serialize)]
struct ActionResult {
    ordinal: usize,
    kind: String,
    status: &'static str,
    elapsed_ms: u128,
    evidence: Option<Value>,
    error: Option<String>,
}

#[derive(Debug, Serialize, Default)]
struct EvidenceSummary {
    screenshots: Vec<String>,
    accessibility_trees: Vec<String>,
    metrics: Vec<String>,
    process_output: Option<String>,
    journal: JournalEvidence,
    network_trace: TraceEvidence,
    gpu: GpuEvidence,
}

#[derive(Debug, Serialize, Default)]
struct JournalEvidence {
    data_root: String,
    file_count: usize,
    files: Vec<JournalFile>,
    event_file_count: usize,
    event_bytes: u64,
    /// 进程启动前已存在的事件数量；本轮计数均从该边界扣除。
    baseline_event_count: usize,
    baseline_turn_started_count: usize,
    baseline_terminal_event_count: usize,
    turn_started_count: usize,
    terminal_event_count: usize,
    total_turn_started_count: usize,
    total_terminal_event_count: usize,
    note: &'static str,
}

#[derive(Debug, Serialize)]
struct JournalFile {
    relative_path: String,
    bytes: u64,
    extension: Option<String>,
}

#[derive(Debug, Serialize, Default)]
struct TraceEvidence {
    path: String,
    status: &'static str,
    bytes: usize,
    note: &'static str,
}

#[derive(Debug, Serialize, Default)]
struct GpuEvidence {
    status: &'static str,
    source: &'static str,
    note: &'static str,
}

#[derive(Debug)]
struct ProviderInput {
    path: Option<PathBuf>,
    redaction_values: Vec<String>,
    summary: ProviderSummary,
}

#[derive(Debug)]
struct ReuseContext {
    isolation_root: PathBuf,
    data_root: PathBuf,
    project_root: PathBuf,
    source_report: PathBuf,
    source_binary: BinaryIdentity,
}

#[cfg(windows)]
#[derive(Debug, Default)]
struct JournalBaseline {
    event_counts: BTreeMap<String, usize>,
    /// 每个计划 Journal 查询在进程启动前的计数；无条件查询仍回退到 event_counts。
    query_counts: BTreeMap<String, usize>,
}

#[cfg(windows)]
#[derive(Debug, Clone, Serialize)]
struct JournalQuery {
    event_type: String,
    matching: BTreeMap<String, Value>,
    /// 与计划动作的 matching_text_contains 对应，只对 JSON 字符串执行子串匹配。
    matching_text_contains: BTreeMap<String, String>,
    matching_project_paths: BTreeMap<String, String>,
    project_root: PathBuf,
    session_id: Option<String>,
    turn_id: Option<String>,
}

#[cfg(windows)]
impl JournalQuery {
    fn new(
        event_type: &str,
        matching: &BTreeMap<String, Value>,
        matching_text_contains: &BTreeMap<String, String>,
        matching_project_paths: &BTreeMap<String, String>,
        project_root: &Path,
        session_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Self {
        Self {
            event_type: event_type.to_owned(),
            matching: matching.clone(),
            matching_text_contains: matching_text_contains.clone(),
            matching_project_paths: matching_project_paths.clone(),
            project_root: project_root.to_owned(),
            session_id: session_id.map(str::to_owned),
            turn_id: turn_id.map(str::to_owned),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.event_type.is_empty()
            || self.event_type.len() > 128
            || self.event_type.chars().any(char::is_control)
        {
            return Err("Journal event_type 无效".to_owned());
        }
        for (pointer, expected) in &self.matching {
            if pointer.len() > 512
                || pointer.chars().any(char::is_control)
                || !is_valid_json_pointer(pointer)
            {
                return Err(format!("Journal matching JSON Pointer 无效：{pointer}"));
            }
            if !matches!(
                expected,
                Value::String(_) | Value::Bool(_) | Value::Number(_)
            ) {
                return Err(format!(
                    "Journal matching 只支持字符串、布尔值或数字：{pointer}"
                ));
            }
        }
        for (pointer, expected) in &self.matching_text_contains {
            if pointer.len() > 512
                || pointer.chars().any(char::is_control)
                || !is_valid_json_pointer(pointer)
            {
                return Err(format!(
                    "Journal matching_text_contains JSON Pointer 无效：{pointer}"
                ));
            }
            if expected.is_empty() {
                return Err(format!(
                    "Journal matching_text_contains 不能为空：{pointer}"
                ));
            }
            if expected.len() > MAX_JOURNAL_TEXT_CONTAINS_BYTES {
                return Err(format!(
                    "Journal matching_text_contains 超过 {} 字节：{pointer}",
                    MAX_JOURNAL_TEXT_CONTAINS_BYTES
                ));
            }
        }
        let normalized_project_root = normalize_windows_path(&self.project_root.to_string_lossy())
            .map_err(|error| format!("Journal project_root 无效：{error}"))?;
        if normalized_project_root.root.is_none() {
            return Err("Journal project_root 必须是绝对 Windows 路径".to_owned());
        }
        for (pointer, expected) in &self.matching_project_paths {
            if pointer.len() > 512
                || pointer.chars().any(char::is_control)
                || !is_valid_json_pointer(pointer)
            {
                return Err(format!(
                    "Journal matching_project_paths JSON Pointer 无效：{pointer}"
                ));
            }
            let normalized = normalize_windows_path(expected).map_err(|error| {
                format!("Journal matching_project_paths 路径无效：{pointer}: {error}")
            })?;
            if normalized.root.is_some() || normalized.components.is_empty() {
                return Err(format!(
                    "Journal matching_project_paths 必须是项目根内的相对路径：{pointer}"
                ));
            }
        }
        for (name, value) in [
            ("session_id", self.session_id.as_deref()),
            ("turn_id", self.turn_id.as_deref()),
        ] {
            if value.is_some_and(|value| {
                value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
            }) {
                return Err(format!("Journal {name} 无效"));
            }
        }
        Ok(())
    }

    fn cache_key(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| format!("编码 Journal 查询失败：{error}"))
    }

    fn has_filters(&self) -> bool {
        !self.matching.is_empty()
            || !self.matching_text_contains.is_empty()
            || !self.matching_project_paths.is_empty()
            || self.session_id.is_some()
            || self.turn_id.is_some()
    }

    fn matches(&self, event: &Value, session_id: Option<&str>) -> bool {
        if event.get("type").and_then(Value::as_str) != Some(self.event_type.as_str()) {
            return false;
        }
        if self
            .session_id
            .as_deref()
            .is_some_and(|expected| session_id != Some(expected))
        {
            return false;
        }
        if self
            .turn_id
            .as_deref()
            .is_some_and(|expected| journal_turn_id(event) != Some(expected))
        {
            return false;
        }
        self.matching.iter().all(|(pointer, expected)| {
            event
                .pointer(pointer)
                .is_some_and(|actual| actual == expected)
        }) && self
            .matching_text_contains
            .iter()
            .all(|(pointer, expected)| {
                !expected.is_empty()
                    && event
                        .pointer(pointer)
                        .and_then(Value::as_str)
                        .is_some_and(|actual| actual.contains(expected.as_str()))
            })
            && self
                .matching_project_paths
                .iter()
                .all(|(pointer, expected)| {
                    event
                        .pointer(pointer)
                        .and_then(Value::as_str)
                        .is_some_and(|actual| {
                            same_project_path(&self.project_root, actual, expected)
                        })
                })
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Eq, PartialEq)]
struct NormalizedWindowsPath {
    root: Option<String>,
    components: Vec<String>,
}

#[cfg(windows)]
fn normalize_windows_path(input: &str) -> Result<NormalizedWindowsPath, String> {
    if input.is_empty() || input.chars().any(char::is_control) {
        return Err("路径为空或包含控制字符".to_owned());
    }
    let mut path = input.replace('/', "\\");
    if path.len() > 4_096 {
        return Err("路径超过 4096 个字符".to_owned());
    }

    const EXTENDED_UNC_PREFIX: &str = r"\\?\UNC\";
    const EXTENDED_PREFIX: &str = r"\\?\";
    const DEVICE_PREFIX: &str = r"\\.\";
    if path
        .get(..EXTENDED_UNC_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(EXTENDED_UNC_PREFIX))
    {
        path = format!(r"\\{}", &path[EXTENDED_UNC_PREFIX.len()..]);
    } else if path
        .get(..EXTENDED_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(EXTENDED_PREFIX))
        || path
            .get(..DEVICE_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(DEVICE_PREFIX))
    {
        path = path[EXTENDED_PREFIX.len()..].to_owned();
    }

    if path.starts_with(r"\\") {
        let parts = path
            .strip_prefix(r"\\")
            .expect("starts_with 已确认 UNC 前缀")
            .split('\\')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if parts.len() < 2 || parts[..2].iter().any(|part| *part == "." || *part == "..") {
            return Err("UNC 路径缺少有效 server/share".to_owned());
        }
        let root = format!(r"\\{}\{}", parts[0], parts[1]);
        let components = normalize_windows_components(parts.into_iter().skip(2))?;
        return Ok(NormalizedWindowsPath {
            root: Some(root),
            components,
        });
    }

    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        if !bytes[0].is_ascii_alphabetic() || (bytes.len() > 2 && bytes[2] != b'\\') {
            return Err("不支持 drive-relative Windows 路径".to_owned());
        }
        let root = path[..2].to_owned();
        let components = normalize_windows_components(path[2..].split('\\'))?;
        return Ok(NormalizedWindowsPath {
            root: Some(root),
            components,
        });
    }
    if path.starts_with('\\') {
        return Err("不支持没有 drive 或 UNC 根的 Windows 路径".to_owned());
    }
    Ok(NormalizedWindowsPath {
        root: None,
        components: normalize_windows_components(path.split('\\'))?,
    })
}

#[cfg(windows)]
fn normalize_windows_components<'a>(
    components: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for component in components {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            if normalized.pop().is_none() {
                return Err("路径越出其 lexical 根".to_owned());
            }
        } else {
            normalized.push(component.to_owned());
        }
    }
    Ok(normalized)
}

#[cfg(windows)]
fn same_project_path(project_root: &Path, actual: &str, expected_relative: &str) -> bool {
    let Ok(project_root) = normalize_windows_path(&project_root.to_string_lossy()) else {
        return false;
    };
    let Ok(expected) = normalize_windows_path(expected_relative) else {
        return false;
    };
    if project_root.root.is_none() || expected.root.is_some() || expected.components.is_empty() {
        return false;
    }
    let expected = NormalizedWindowsPath {
        root: project_root.root.clone(),
        components: project_root
            .components
            .iter()
            .cloned()
            .chain(expected.components)
            .collect(),
    };
    let Ok(actual) = normalize_windows_path(actual) else {
        return false;
    };
    let actual = if actual.root.is_some() {
        actual
    } else {
        NormalizedWindowsPath {
            root: project_root.root.clone(),
            components: project_root
                .components
                .iter()
                .cloned()
                .chain(actual.components)
                .collect(),
        }
    };
    actual.root.as_deref().is_some_and(|actual_root| {
        expected.root.as_deref().is_some_and(|expected_root| {
            actual_root.eq_ignore_ascii_case(expected_root)
                && actual.components.len() == expected.components.len()
                && actual
                    .components
                    .iter()
                    .zip(&expected.components)
                    .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        })
    })
}

fn is_valid_json_pointer(pointer: &str) -> bool {
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') {
        return false;
    }
    let mut chars = pointer.chars();
    while let Some(character) = chars.next() {
        if character == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

#[cfg(windows)]
fn journal_turn_id(event: &Value) -> Option<&str> {
    [
        "/payload/turn_id",
        "/payload/turnId",
        "/payload/request/turnId",
        "/payload/request/turn_id",
    ]
    .iter()
    .find_map(|pointer| event.pointer(pointer).and_then(Value::as_str))
}

#[cfg(windows)]
#[derive(Debug, Default)]
struct CapturedProcessStream {
    bytes: Vec<u8>,
    truncated: bool,
    timed_out: bool,
    read_error: bool,
}

#[cfg(windows)]
struct ProcessStreamReader {
    receiver: Receiver<CapturedProcessStream>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    thread_handle: Option<usize>,
}

#[cfg(windows)]
struct ProcessOutputCapture {
    stdout: ProcessStreamReader,
    stderr: ProcessStreamReader,
}

#[cfg(windows)]
struct CapturedProcessOutput {
    stdout: CapturedProcessStream,
    stderr: CapturedProcessStream,
}

#[cfg(windows)]
impl ProcessOutputCapture {
    fn start(child: &mut std::process::Child) -> Result<Self, String> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "被测进程 stdout 未建立有限捕获管道".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "被测进程 stderr 未建立有限捕获管道".to_owned())?;
        let stdout = ProcessStreamReader::start(stdout)?;
        let stderr = ProcessStreamReader::start(stderr)?;
        Ok(Self { stdout, stderr })
    }

    fn finish(mut self, timeout: Duration) -> CapturedProcessOutput {
        let deadline = Instant::now() + timeout;
        let stdout = self.stdout.finish(deadline);
        let stderr = self.stderr.finish(deadline);
        CapturedProcessOutput { stdout, stderr }
    }
}

#[cfg(windows)]
impl ProcessStreamReader {
    fn start(reader: impl Read + Send + AsRawHandle + 'static) -> Result<Self, String> {
        let (capture_tx, capture_rx) = mpsc::sync_channel(1);
        let (handle_tx, handle_rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let thread = std::thread::spawn(move || {
            let thread_handle = match duplicate_current_thread_handle() {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = handle_tx.send(Err(error));
                    return;
                }
            };
            if handle_tx.send(Ok(thread_handle)).is_err() {
                // 父线程已经无法取消这一路读取，释放复制出的句柄后退出。
                unsafe {
                    let _ = CloseHandle(thread_handle as HANDLE);
                }
                return;
            }
            let capture = read_process_stream_from_pipe(reader, &thread_cancel);
            let _ = capture_tx.send(capture);
        });
        let thread_handle = match handle_rx.recv() {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error);
            }
            Err(error) => {
                let _ = thread.join();
                return Err(format!("读取线程未提供可取消句柄：{error}"));
            }
        };
        Ok(Self {
            receiver: capture_rx,
            cancel,
            thread: Some(thread),
            thread_handle: Some(thread_handle),
        })
    }

    fn finish(&mut self, deadline: Instant) -> CapturedProcessStream {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.receiver.recv_timeout(remaining) {
            Ok(capture) => {
                self.join_thread();
                self.close_thread_handle();
                capture
            }
            Err(RecvTimeoutError::Timeout) => self.cancel_and_join(true),
            Err(RecvTimeoutError::Disconnected) => {
                self.join_thread();
                self.close_thread_handle();
                CapturedProcessStream {
                    read_error: true,
                    ..CapturedProcessStream::default()
                }
            }
        }
    }

    fn cancel_and_join(&mut self, timed_out: bool) -> CapturedProcessStream {
        self.cancel.store(true, Ordering::Release);
        let thread_handle = self.thread_handle;
        if let Some(thread_handle) = thread_handle {
            // 子进程的后代可能继承管道写端；关闭主进程写端并不能让这个 read 返回。
            // CancelSynchronousIo 作用于真实线程句柄；句柄要保留到读取线程 join 完成。
            unsafe {
                let _ = CancelSynchronousIo(thread_handle as HANDLE);
            }
        }
        let mut capture = self
            .receiver
            .recv()
            .unwrap_or_else(|_| CapturedProcessStream {
                read_error: true,
                ..CapturedProcessStream::default()
            });
        self.join_thread();
        self.close_thread_handle();
        capture.timed_out = timed_out;
        capture
    }

    fn close_thread_handle(&mut self) {
        if let Some(thread_handle) = self.thread_handle.take() {
            unsafe {
                let _ = CloseHandle(thread_handle as HANDLE);
            }
        }
    }

    fn join_thread(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessStreamReader {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.cancel_and_join(false);
        } else {
            self.close_thread_handle();
        }
    }
}

#[cfg(windows)]
fn duplicate_current_thread_handle() -> Result<usize, String> {
    let mut handle: HANDLE = std::ptr::null_mut();
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut handle,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 || handle.is_null() {
        let error = unsafe { GetLastError() };
        return Err(format!("复制读取线程句柄失败：Win32 error {error}"));
    }
    Ok(handle as usize)
}

#[cfg(windows)]
fn read_process_stream_from_pipe(
    mut reader: impl Read + AsRawHandle,
    cancel: &AtomicBool,
) -> CapturedProcessStream {
    let mut capture = CapturedProcessStream {
        bytes: Vec::with_capacity(8 * 1024),
        ..CapturedProcessStream::default()
    };
    let mut buffer = [0u8; 8 * 1024];
    let pipe_handle = reader.as_raw_handle() as HANDLE;
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }

        let mut available = 0u32;
        let peek_result = unsafe {
            PeekNamedPipe(
                pipe_handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if peek_result == 0 {
            // 写端全部关闭时，PeekNamedPipe 会报告 broken/no-data；这表示正常 EOF。
            let error = unsafe { GetLastError() };
            if matches!(
                error,
                ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
            ) {
                break;
            }
            capture.read_error = true;
            break;
        }
        if available == 0 {
            // 空管道不进入同步 read，避免取消发生在 read 尚未建立期间。
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        // 只读取 PeekNamedPipe 已确认存在的字节，确保同步 read 不会等待未来数据。
        let read_len = (available as usize).min(buffer.len());
        match reader.read(&mut buffer[..read_len]) {
            Ok(0) => break,
            Ok(read) => append_process_stream_bytes(&mut capture, &buffer[..read]),
            Err(_) => {
                capture.read_error = true;
                break;
            }
        }
    }
    capture
}

#[cfg(windows)]
fn append_process_stream_bytes(capture: &mut CapturedProcessStream, bytes: &[u8]) {
    let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(capture.bytes.len());
    if remaining > 0 {
        capture
            .bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
    }
    if bytes.len() > remaining {
        capture.truncated = true;
    }
}

#[cfg(windows)]
fn write_startup_failure_evidence(
    run_root: &Path,
    reason: &str,
    exit_code: Option<i32>,
    termination: &'static str,
    output: &CapturedProcessOutput,
    redaction_values: &[String],
) -> Result<PathBuf, String> {
    let stdout_path = run_root.join("startup-failure-stdout.txt");
    let stderr_path = run_root.join("startup-failure-stderr.txt");
    let stdout_text = redact_process_output(&output.stdout, redaction_values);
    let stderr_text = redact_process_output(&output.stderr, redaction_values);
    fs::write(&stdout_path, stdout_text.as_bytes())
        .map_err(|error| format!("写入启动 stdout 证据失败：{error}"))?;
    fs::write(&stderr_path, stderr_text.as_bytes())
        .map_err(|error| format!("写入启动 stderr 证据失败：{error}"))?;

    let manifest_path = run_root.join("startup-failure.json");
    let manifest = json!({
        "schema": "keencode/ely-native-startup-failure",
        "status": "startup-failed",
        "reason": redact_sensitive_text(reason, redaction_values),
        "exitCode": exit_code,
        "termination": termination,
        "stdout": process_output_summary(&stdout_path, &output.stdout, stdout_text.len()),
        "stderr": process_output_summary(&stderr_path, &output.stderr, stderr_text.len()),
        "limits": {
            "maxBytesPerStream": MAX_PROCESS_OUTPUT_BYTES,
            "drainTimeoutMs": PROCESS_OUTPUT_DRAIN_TIMEOUT.as_millis(),
        },
        "note": "stdout/stderr 由独立读取线程持续排空；达到上限后丢弃后续字节，落盘前脱敏 URL、Key、Token 和敏感字段",
    });
    write_json(&manifest_path, &manifest)?;
    Ok(manifest_path)
}

#[cfg(windows)]
// 窗口建立后的收尾也单独保存进程输出，避免动作或退出失败时只剩摘要而丢失诊断正文。
fn write_process_output_evidence(
    run_root: &Path,
    exit_code: Option<i32>,
    termination: &'static str,
    error: Option<&str>,
    output: &CapturedProcessOutput,
    redaction_values: &[String],
) -> Result<PathBuf, String> {
    let stdout_text = redact_process_output(&output.stdout, redaction_values);
    let stderr_text = redact_process_output(&output.stderr, redaction_values);
    let manifest_path = run_root.join("process-output.json");
    let manifest = json!({
        "schema": "keencode/ely-native-process-output",
        "status": "captured",
        "exitCode": exit_code,
        "termination": termination,
        "error": error.map(|value| redact_sensitive_text(value, redaction_values)),
        "stdout": {
            "text": stdout_text,
            "capturedBytes": output.stdout.bytes.len(),
            "truncated": output.stdout.truncated,
            "timedOut": output.stdout.timed_out,
            "readError": output.stdout.read_error,
        },
        "stderr": {
            "text": stderr_text,
            "capturedBytes": output.stderr.bytes.len(),
            "truncated": output.stderr.truncated,
            "timedOut": output.stderr.timed_out,
            "readError": output.stderr.read_error,
        },
        "limits": {
            "maxBytesPerStream": MAX_PROCESS_OUTPUT_BYTES,
            "drainTimeoutMs": PROCESS_OUTPUT_DRAIN_TIMEOUT.as_millis(),
        },
        "note": "stdout/stderr 由独立读取线程持续排空；达到上限后丢弃后续字节，正文在落盘前脱敏 URL、Key、Token 和敏感字段",
    });
    write_json(&manifest_path, &manifest)?;
    Ok(manifest_path)
}

#[cfg(windows)]
fn process_output_summary(
    path: &Path,
    capture: &CapturedProcessStream,
    written_bytes: usize,
) -> Value {
    json!({
        "path": path.display().to_string(),
        "capturedBytes": capture.bytes.len(),
        "writtenBytes": written_bytes,
        "truncated": capture.truncated,
        "timedOut": capture.timed_out,
        "readError": capture.read_error,
    })
}

#[cfg(windows)]
fn redact_process_output(capture: &CapturedProcessStream, redaction_values: &[String]) -> String {
    let mut text =
        redact_sensitive_text(&String::from_utf8_lossy(&capture.bytes), redaction_values);
    if capture.truncated {
        text.push_str("\n[capture-truncated]\n");
    }
    if capture.timed_out {
        text.push_str("\n[capture-timeout]\n");
    }
    if capture.read_error {
        text.push_str("\n[capture-read-error]\n");
    }
    text
}

#[cfg(windows)]
fn redact_sensitive_text(input: &str, redaction_values: &[String]) -> String {
    let mut text = input.to_owned();
    for value in redaction_values {
        if !value.is_empty() {
            text = text.replace(value, "[redacted]");
        }
    }
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line_contains_sensitive_field(line) {
            output.push_str("[redacted-sensitive-line]");
            if line.ends_with('\n') {
                output.push('\n');
            }
        } else {
            output.push_str(&redact_url_tokens(line));
        }
    }
    output
}

#[cfg(windows)]
fn line_contains_sensitive_field(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "authorization",
        "bearer ",
        "api-key",
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "private-key",
        "private_key",
        "base-url",
        "base_url",
        "baseurl",
        "endpoint",
    ]
    .iter()
    .any(|field| lower.contains(field))
}

#[cfg(windows)]
fn redact_url_tokens(value: &str) -> String {
    const PREFIXES: [&str; 4] = ["https://", "http://", "wss://", "ws://"];
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        let rest = &value[index..];
        if let Some(prefix) = PREFIXES.iter().find(|prefix| rest.starts_with(*prefix)) {
            output.push_str("[redacted-url]");
            index += prefix.len();
            while index < value.len() {
                let character = value[index..].chars().next().expect("URL 字符边界应有效");
                if character.is_whitespace()
                    || matches!(character, '"' | '\'' | '<' | '>' | ')' | ']' | '}' | ',')
                {
                    break;
                }
                index += character.len_utf8();
            }
            continue;
        }
        let character = rest.chars().next().expect("文本字符边界应有效");
        output.push(character);
        index += character.len_utf8();
    }
    output
}

fn main() {
    #[cfg(windows)]
    if let Some(result) = windows_runner::maybe_run_uia_helper() {
        if let Err(error) = result {
            eprintln!("Ely UIA helper 失败：{error}");
            std::process::exit(2);
        }
        return;
    }
    if let Err(error) = run() {
        eprintln!("Ely 原生验收失败：{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut options = Options::parse(env::args().skip(1))?;
    validate_binary(&options.binary)?;
    // 被测进程会切换到隔离项目目录；先固定绝对路径，避免相对路径被重解释。
    options.binary = fs::canonicalize(&options.binary)
        .map_err(|error| format!("规范化被测程序路径失败：{error}"))?;
    let plan = load_plan(options.plan.as_deref(), options.real_provider)?;
    if plan.requires_real_provider && !options.real_provider {
        return Err("计划要求真实 Provider，但没有 --real-provider；拒绝伪造模型验收".to_owned());
    }
    fs::create_dir_all(&options.output)
        .map_err(|error| format!("创建输出目录失败 {}: {error}", options.output.display()))?;
    options.output = fs::canonicalize(&options.output)
        .map_err(|error| format!("规范化输出目录失败：{error}"))?;
    let reuse_context = options
        .reuse_isolation
        .as_deref()
        .map(validate_reuse_isolation)
        .transpose()?;
    if let Some(reuse_context) = reuse_context.as_ref() {
        validate_reuse_output(&options.output, reuse_context)?;
    }
    let provider = load_provider_for_run(&options, reuse_context.as_ref())?;
    if options.real_provider && provider.path.is_none() {
        return Err(
            "真实 Provider 运行缺少配置：新运行需 --provider-config，冷恢复需既有 data/providers.json"
                .to_owned(),
        );
    }
    let run = run_native(options, provider, plan, reuse_context)?;
    let report_path = run.0.join("report.json");
    write_json(&report_path, &run.1)?;
    println!("原生验收报告：{}", report_path.display());
    if run.1.status != "passed" {
        return Err(run
            .1
            .error
            .clone()
            .unwrap_or_else(|| "原生窗口动作失败".to_owned()));
    }
    Ok(())
}

fn load_provider_for_run(
    options: &Options,
    reuse_context: Option<&ReuseContext>,
) -> Result<ProviderInput, String> {
    let Some(reuse_context) = reuse_context else {
        return load_provider_input(options.provider_config.as_deref(), options.real_provider);
    };
    let existing = reuse_context.data_root.join("providers.json");
    if existing.exists() {
        let mut provider = load_provider_input(Some(&existing), options.real_provider)?;
        if let Some(config_path) = options.provider_config.as_deref() {
            // 冷恢复始终让应用读取既有 providers.json；命令行配置仅补充脱敏值，
            // 绝不复制、替换或覆盖复用根中的 Provider 文件。
            let supplied = load_provider_input(Some(config_path), options.real_provider)?;
            provider.redaction_values.extend(supplied.redaction_values);
            provider
                .redaction_values
                .sort_by_key(|value| std::cmp::Reverse(value.len()));
            provider.redaction_values.dedup();
        }
        return Ok(provider);
    }
    if options.provider_config.is_some() {
        return Err(
            "冷恢复隔离根缺少 data/providers.json；为避免覆盖历史根，不能复制新的 Provider 配置"
                .to_owned(),
        );
    }
    load_provider_input(None, options.real_provider)
}

fn validate_reuse_output(output: &Path, reuse_context: &ReuseContext) -> Result<(), String> {
    let source_run_root = reuse_context
        .source_report
        .parent()
        .ok_or_else(|| "冷恢复来源报告缺少 native-run 父目录".to_owned())?;
    if path_contains(source_run_root, output)
        || path_contains(output, source_run_root)
        || path_contains(output, &reuse_context.isolation_root)
        || path_contains(&reuse_context.isolation_root, output)
    {
        return Err(
            "冷恢复 output 必须与来源 native-run 完全分离，不能覆盖或嵌套来源目录".to_owned(),
        );
    }
    let run_root = output.join("native-run");
    if fs::symlink_metadata(&run_root).is_ok() {
        return Err(format!(
            "冷恢复要求新的 output/native-run 目录，目标已存在：{}",
            run_root.display()
        ));
    }
    Ok(())
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut binary = None;
        let mut binary_args = Vec::new();
        let mut provider_config = None;
        let mut plan = None;
        let mut output = None;
        let mut reuse_isolation = None;
        let mut real_provider = false;
        let mut window_timeout_ms = DEFAULT_WINDOW_TIMEOUT_MS;
        let mut action_timeout_ms = DEFAULT_ACTION_TIMEOUT_MS;
        let mut keep_isolation = false;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--binary" => binary = Some(PathBuf::from(next_arg(&mut args, "--binary")?)),
                "--binary-arg" => {
                    binary_args.push(next_arg(&mut args, "--binary-arg")?);
                }
                "--provider-config" => {
                    provider_config = Some(PathBuf::from(next_arg(&mut args, "--provider-config")?))
                }
                "--plan" => plan = Some(PathBuf::from(next_arg(&mut args, "--plan")?)),
                "--output" => output = Some(PathBuf::from(next_arg(&mut args, "--output")?)),
                "--reuse-isolation" => {
                    reuse_isolation = Some(PathBuf::from(next_arg(&mut args, "--reuse-isolation")?))
                }
                "--real-provider" => real_provider = true,
                "--window-timeout-ms" => {
                    window_timeout_ms = parse_bounded_u64(
                        &next_arg(&mut args, "--window-timeout-ms")?,
                        "--window-timeout-ms",
                        1,
                        300_000,
                    )?;
                }
                "--action-timeout-ms" => {
                    action_timeout_ms = parse_bounded_u64(
                        &next_arg(&mut args, "--action-timeout-ms")?,
                        "--action-timeout-ms",
                        1,
                        900_000,
                    )?;
                }
                "--keep-isolation" => keep_isolation = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                unknown => return Err(format!("未知参数 {unknown}；使用 --help 查看用法")),
            }
        }
        let binary = binary.ok_or_else(|| "缺少 --binary <keencode/ely 可执行文件>".to_owned())?;
        let output = output.unwrap_or_else(|| {
            PathBuf::from("out/ely-native").join(format!("run-{}", timestamp_ms()))
        });
        Ok(Self {
            binary,
            binary_args,
            provider_config,
            plan,
            output,
            reuse_isolation,
            real_provider,
            window_timeout_ms,
            action_timeout_ms,
            keep_isolation,
        })
    }
}

fn print_help() {
    println!(
        "用法：cargo run -p keencode-native-gpui-tests -- --binary PATH [--binary-arg ARG ...] [--plan PATH] \
         [--provider-config PATH --real-provider] [--output DIR] [--reuse-isolation DIR]\n\n\
         通过 Win32 SendInput/PrintWindow/UI Automation 驱动真实桌面窗口；不使用 CDP、\
         每个 --binary-arg 都作为一个原样参数传给被测进程，可重复使用（例如\
         --binary-arg --user-data-dir=PATH）；\
         WebView 或 JavaScript。默认创建新的隔离根；--reuse-isolation 只复用既有 data/project，\
         本轮报告、trace、截图和指标仍写入新的 output。"
    );
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} 需要参数"))
}

fn parse_bounded_u64(value: &str, flag: &str, min: u64, max: u64) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{flag} 必须为整数"))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!("{flag} 必须在 {min}..{max} 范围内"));
    }
    Ok(parsed)
}

fn validate_binary(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("读取被测程序失败 {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("被测程序必须是普通文件：{}", path.display()));
    }
    Ok(())
}

fn validate_reuse_isolation(path: &Path) -> Result<ReuseContext, String> {
    let isolation_root = validate_existing_directory(path, "冷恢复隔离根")?;
    if isolation_root.file_name().and_then(|value| value.to_str()) != Some("isolation") {
        return Err("--reuse-isolation 必须指向既有 native-run/isolation 目录".to_owned());
    }
    let data_root = validate_existing_directory(&isolation_root.join("data"), "冷恢复 data 根")?;
    let project_root =
        validate_existing_directory(&isolation_root.join("project"), "冷恢复 project 根")?;
    // 这些文件是 runner 首次启动时建立的冷恢复边界。只接受完整的既有根，
    // 防止复用模式悄悄退化为重新初始化并覆盖历史设置或 fixture。
    for (path, label) in [
        (data_root.join("settings.json"), "冷恢复 settings.json"),
        (
            data_root.join("native-general-settings.json"),
            "冷恢复 native-general-settings.json",
        ),
        (project_root.join("README.md"), "冷恢复 README.md fixture"),
        (
            project_root.join("src").join("facts.txt"),
            "冷恢复 facts.txt fixture",
        ),
    ] {
        validate_existing_file(&path, label)?;
    }
    let source_report = isolation_root
        .parent()
        .ok_or_else(|| "冷恢复隔离根缺少 native-run 父目录".to_owned())?
        .join("report.json");
    let source_report = validate_existing_file(&source_report, "冷恢复来源 report.json")?;
    let source_report_size = fs::metadata(&source_report)
        .map_err(|error| format!("读取冷恢复来源报告元数据失败：{error}"))?
        .len();
    if source_report_size > MAX_PLAN_BYTES {
        return Err("冷恢复来源 report.json 超过 2 MiB，拒绝载入".to_owned());
    }
    let report_bytes =
        fs::read(&source_report).map_err(|error| format!("读取冷恢复来源报告失败：{error}"))?;
    let report: Value = serde_json::from_slice(&report_bytes)
        .map_err(|error| format!("冷恢复来源 report.json 不是合法 JSON：{error}"))?;
    let recorded_isolation = report
        .pointer("/process/isolation_root")
        .and_then(Value::as_str)
        .ok_or_else(|| "冷恢复来源报告缺少 process.isolation_root".to_owned())?;
    let recorded_isolation = validate_existing_directory(
        Path::new(recorded_isolation),
        "冷恢复来源报告中的 isolation_root",
    )?;
    if !paths_equal(&recorded_isolation, &isolation_root) {
        return Err("冷恢复来源报告与指定 isolation 根不一致".to_owned());
    }
    let source_binary_path = report
        .get("binary_path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "冷恢复来源报告缺少 binary_path".to_owned())?;
    let source_binary_sha256 = report
        .get("binary_sha256")
        .and_then(Value::as_str)
        .filter(|value| {
            value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
        .ok_or_else(|| "冷恢复来源报告缺少有效 binary_sha256".to_owned())?;
    Ok(ReuseContext {
        isolation_root,
        data_root,
        project_root,
        source_report,
        source_binary: BinaryIdentity {
            path: source_binary_path.to_owned(),
            sha256: source_binary_sha256.to_owned(),
            source: "prior run report",
        },
    })
}

fn validate_existing_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("读取{label}失败 {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label}必须是非符号链接目录：{}", path.display()));
    }
    fs::canonicalize(path).map_err(|error| format!("解析{label}失败 {}: {error}", path.display()))
}

fn validate_existing_file(path: &Path, label: &str) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("读取{label}失败 {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "{label}必须是非符号链接普通文件：{}",
            path.display()
        ));
    }
    fs::canonicalize(path).map_err(|error| format!("解析{label}失败 {}: {error}", path.display()))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn path_contains(parent: &Path, child: &Path) -> bool {
    if paths_equal(parent, child) {
        return true;
    }
    #[cfg(windows)]
    {
        let parent = parent
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_ascii_lowercase();
        let child = child.to_string_lossy().to_ascii_lowercase();
        child.starts_with(&format!("{parent}\\")) || child.starts_with(&format!("{parent}/"))
    }
    #[cfg(not(windows))]
    {
        child.starts_with(parent)
    }
}

fn load_provider_input(path: Option<&Path>, real_provider: bool) -> Result<ProviderInput, String> {
    let Some(path) = path else {
        return Ok(ProviderInput {
            path: None,
            redaction_values: Vec::new(),
            summary: ProviderSummary {
                config_path: None,
                copied_to_isolated_data: false,
                provider_count: None,
                active_provider_id: None,
                active_model_id: None,
                real_provider_authorized: real_provider,
            },
        });
    };
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("读取 Provider 配置失败 {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("Provider 配置必须是普通文件：{}", path.display()));
    }
    if metadata.len() > 2 * 1024 * 1024 {
        return Err("Provider 配置超过 2 MiB，拒绝载入".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| format!("读取 Provider 配置失败: {error}"))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Provider 配置不是合法 JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "Provider 配置根必须为 JSON 对象".to_owned())?;
    let providers = object.get("providers").and_then(Value::as_array);
    let redaction_values = collect_redaction_values(&value);
    let summary = ProviderSummary {
        config_path: Some(path.display().to_string()),
        copied_to_isolated_data: false,
        provider_count: providers.map(Vec::len),
        active_provider_id: object
            .get("activeProviderId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        active_model_id: object
            .get("activeModelId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        real_provider_authorized: real_provider,
    };
    Ok(ProviderInput {
        path: Some(path.to_owned()),
        redaction_values,
        summary,
    })
}

fn collect_redaction_values(value: &Value) -> Vec<String> {
    fn visit(value: &Value, key: Option<&str>, output: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (name, child) in object {
                    visit(child, Some(name), output);
                }
            }
            Value::Array(array) => {
                for child in array {
                    visit(child, key, output);
                }
            }
            Value::String(text)
                if key.is_some_and(|name| {
                    let lower = name.to_ascii_lowercase();
                    lower.contains("key")
                        || lower.contains("token")
                        || lower.contains("secret")
                        || lower.contains("password")
                        || lower.contains("auth")
                        || lower.contains("baseurl")
                        || lower.contains("base_url")
                        || lower == "url"
                        || lower.contains("endpoint")
                }) && text.len() >= 4 =>
            {
                output.push(text.clone());
            }
            _ => {}
        }
    }
    let mut output = Vec::new();
    visit(value, None, &mut output);
    output.sort_by_key(|value| std::cmp::Reverse(value.len()));
    output.dedup();
    output
}

fn load_plan(path: Option<&Path>, real_provider: bool) -> Result<Plan, String> {
    let Some(path) = path else {
        let plan = Plan {
            version: SCHEMA_VERSION,
            name: Some("ely-native-default".to_owned()),
            requires_real_provider: real_provider,
            requires_journal: real_provider,
            requires_network_trace: real_provider,
            requires_reuse_isolation: false,
            native_general_settings: NativeGeneralSettings::default(),
            visual: None,
            actions: if real_provider {
                vec![
                    Action::Focus,
                    Action::Screenshot {
                        label: "startup".to_owned(),
                    },
                    Action::Accessibility {
                        label: "startup".to_owned(),
                    },
                    Action::Key {
                        key: "tab".to_owned(),
                        modifiers: Vec::new(),
                    },
                    Action::TypeText {
                        text: "Ely 原生验收：请流式回答 native smoke。".to_owned(),
                    },
                    Action::Key {
                        key: "enter".to_owned(),
                        modifiers: Vec::new(),
                    },
                    Action::Wait { ms: 2_000 },
                    Action::Screenshot {
                        label: "after-submit".to_owned(),
                    },
                    Action::Metrics {
                        label: Some("after-submit".to_owned()),
                    },
                ]
            } else {
                vec![
                    Action::Focus,
                    Action::Screenshot {
                        label: "startup".to_owned(),
                    },
                    Action::Accessibility {
                        label: "startup".to_owned(),
                    },
                    Action::Metrics {
                        label: Some("startup".to_owned()),
                    },
                ]
            },
        };
        validate_plan_actions(&plan.actions)?;
        return Ok(plan);
    };
    let metadata = fs::metadata(path)
        .map_err(|error| format!("读取验收计划失败 {}: {error}", path.display()))?;
    if metadata.len() > MAX_PLAN_BYTES {
        return Err("验收计划超过 2 MiB".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| format!("读取验收计划失败: {error}"))?;
    let plan: Plan = serde_json::from_slice(&bytes)
        .map_err(|error| format!("验收计划不是合法 JSON: {error}"))?;
    if plan.version != SCHEMA_VERSION {
        return Err(format!("不支持的验收计划版本 {}", plan.version));
    }
    if plan.actions.is_empty() {
        return Err("验收计划必须包含至少一个原生动作".to_owned());
    }
    validate_plan_actions(&plan.actions)?;
    plan.native_general_settings.validate()?;
    if let Some(visual) = plan.visual.as_ref() {
        visual.validate()?;
        if plan.native_general_settings.appearance != visual.theme {
            return Err(
                "visual.theme 必须与 native_general_settings.appearance 保持一致".to_owned(),
            );
        }
    }
    Ok(Plan {
        requires_real_provider: plan.requires_real_provider || real_provider,
        requires_journal: plan.requires_journal || plan.requires_real_provider || real_provider,
        requires_network_trace: plan.requires_network_trace
            || plan.requires_real_provider
            || real_provider,
        requires_reuse_isolation: plan.requires_reuse_isolation,
        ..plan
    })
}

fn validate_plan_actions(actions: &[Action]) -> Result<(), String> {
    for (index, action) in actions.iter().enumerate() {
        validate_plan_action(action, &format!("actions[{index}]"))?;
    }
    Ok(())
}

fn validate_plan_action(action: &Action, path: &str) -> Result<(), String> {
    match action {
        Action::Scroll { x, y, delta_y } => {
            if *x < 0 || *y < 0 {
                return Err(format!("{path}.scroll 的 client 坐标必须为非负：({x},{y})"));
            }
            validate_scroll_delta(*delta_y)
                .map_err(|error| format!("{path}.scroll 无效：{error}"))?;
        }
        Action::Repeat { actions, .. } => {
            for (index, nested) in actions.iter().enumerate() {
                validate_plan_action(nested, &format!("{path}.actions[{index}]"))?;
            }
        }
        Action::AssertGoalState { matching } => validate_goal_matching(matching, path)?,
        Action::AssertFile {
            json_pointer_equals,
            ..
        } => validate_json_pointer_equals(json_pointer_equals, path)?,
        Action::AssertAutomationState {
            title,
            matching,
            history_outcomes,
        } => validate_automation_matching(title, matching, history_outcomes, path)?,
        _ => {}
    }
    Ok(())
}

fn validate_goal_matching(matching: &BTreeMap<String, Value>, path: &str) -> Result<(), String> {
    for (pointer, expected) in matching {
        if pointer.len() > 512
            || pointer.chars().any(char::is_control)
            || !is_valid_json_pointer(pointer)
        {
            return Err(format!("{path}.matching JSON Pointer 无效：{pointer}"));
        }
        if !matches!(
            expected,
            Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null
        ) {
            return Err(format!(
                "{path}.matching 只支持字符串、布尔值、数字或 null：{pointer}"
            ));
        }
    }
    Ok(())
}

fn validate_json_pointer_equals(
    matching: &BTreeMap<String, Value>,
    path: &str,
) -> Result<(), String> {
    for pointer in matching.keys() {
        if pointer.len() > 512
            || pointer.chars().any(char::is_control)
            || !is_valid_json_pointer(pointer)
        {
            return Err(format!(
                "{path}.json_pointer_equals JSON Pointer 无效：{pointer}"
            ));
        }
    }
    Ok(())
}

fn validate_automation_matching(
    title: &str,
    matching: &BTreeMap<String, Value>,
    history_outcomes: &BTreeMap<String, usize>,
    path: &str,
) -> Result<(), String> {
    if title.trim().is_empty() || title.len() > 512 || title.chars().any(char::is_control) {
        return Err(format!("{path}.title 必须是非空且有限的文本"));
    }
    validate_goal_matching(matching, path)?;
    for outcome in history_outcomes.keys() {
        if !matches!(
            outcome.as_str(),
            "scheduled" | "running" | "cancelled" | "completed"
        ) {
            return Err(format!(
                "{path}.history_outcomes 只支持 scheduled、running、cancelled、completed：{outcome}"
            ));
        }
    }
    Ok(())
}

fn validate_scroll_delta(delta_y: i32) -> Result<(), String> {
    let magnitude = delta_y.unsigned_abs();
    if delta_y == 0 || magnitude > MAX_SCROLL_DELTA_Y as u32 {
        return Err(format!(
            "delta_y 必须在 -{MAX_SCROLL_DELTA_Y}..{MAX_SCROLL_DELTA_Y} 范围内且不能为 0：{delta_y}"
        ));
    }
    Ok(())
}

fn run_native(
    options: Options,
    mut provider: ProviderInput,
    plan: Plan,
    reuse_context: Option<ReuseContext>,
) -> Result<(PathBuf, RunReport), String> {
    #[cfg(not(windows))]
    {
        let _ = (options, provider, plan, reuse_context);
        return Err("Ely 原生窗口验收器只支持 Windows；Linux/macOS 不伪造 UI 通过".to_owned());
    }
    #[cfg(windows)]
    {
        if plan.requires_reuse_isolation && reuse_context.is_none() {
            return Err(
                "当前验收计划要求复用既有 isolation；请提供 --reuse-isolation，拒绝在新根上伪造冷恢复".to_owned(),
            );
        }
        windows_runner::initialize_process_dpi_awareness()?;
        let run_root = options.output.join("native-run");
        let screenshots = run_root.join("screenshots");
        let accessibility = run_root.join("accessibility");
        let metrics = run_root.join("metrics");
        fs::create_dir_all(&run_root).map_err(|error| error.to_string())?;
        fs::create_dir_all(&screenshots).map_err(|error| error.to_string())?;
        fs::create_dir_all(&accessibility).map_err(|error| error.to_string())?;
        fs::create_dir_all(&metrics).map_err(|error| error.to_string())?;
        let (isolation_root, data_root, project_root, owns_isolation) =
            if let Some(reuse_context) = reuse_context.as_ref() {
                (
                    reuse_context.isolation_root.clone(),
                    reuse_context.data_root.clone(),
                    reuse_context.project_root.clone(),
                    false,
                )
            } else {
                let isolation_root = run_root.join("isolation");
                let data_root = isolation_root.join("data");
                let project_root = isolation_root.join("project");
                fs::create_dir_all(&data_root).map_err(|error| error.to_string())?;
                fs::create_dir_all(&project_root).map_err(|error| error.to_string())?;
                write_isolated_settings(&data_root, &plan.native_general_settings)?;
                write_isolated_project(&project_root)?;
                if let Some(provider_path) = provider.path.as_deref() {
                    let destination = data_root.join("providers.json");
                    fs::copy(provider_path, &destination).map_err(|error| {
                        format!(
                            "复制 Provider 配置到隔离数据根失败 {}: {error}",
                            destination.display()
                        )
                    })?;
                    provider.summary.copied_to_isolated_data = true;
                }
                (isolation_root, data_root, project_root, true)
            };
        // 复用模式的 settings、Provider 和项目 fixture 已由来源运行建立；这里仅
        // 读取它们，并把本轮 trace 与图形证据写入全新的 run_root。
        let journal_baseline = capture_journal_baseline(&data_root, &project_root, &plan.actions)?;
        if !owns_isolation {
            provider.summary.copied_to_isolated_data = false;
        }
        let trace_path = run_root.join("network-trace.jsonl");
        let start_ms = timestamp_ms();
        let binary_sha256 = stable_sha256(&options.binary)
            .map_err(|error| format!("计算被测程序 SHA-256 失败：{error}"))?;
        let binary_identity = BinaryIdentity {
            path: options.binary.display().to_string(),
            sha256: binary_sha256.clone(),
            source: "current invocation",
        };
        let mut command = Command::new(&options.binary);
        command
            .args(&options.binary_args)
            .current_dir(&project_root)
            .env("KEENCODE_BENCHMARK", "1")
            .env("KEENCODE_BENCHMARK_DATA_DIR", &data_root)
            .env("KEENCODE_NATIVE_ACCEPTANCE", "1")
            .env("KEENCODE_NATIVE_ACCEPTANCE_OUTPUT", &run_root)
            .env("KEENCODE_NATIVE_ACCEPTANCE_PROJECT", &project_root)
            .env("KEENCODE_NATIVE_WIRE_TRACE", &trace_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("启动被测程序失败 {}: {error}", options.binary.display()))?;
        let output_capture = match ProcessOutputCapture::start(&mut child) {
            Ok(capture) => capture,
            Err(error) => {
                let _ = wait_or_kill(&mut child, Duration::from_secs(5));
                return Err(error);
            }
        };
        let pid = child.id();
        let window = match windows_runner::wait_for_window(
            pid,
            Duration::from_millis(options.window_timeout_ms),
            &mut child,
        ) {
            Ok(window) => window,
            Err(error) => {
                let process_exit = wait_or_kill(&mut child, Duration::from_secs(5));
                let output = output_capture.finish(PROCESS_OUTPUT_DRAIN_TIMEOUT);
                let evidence_path = write_startup_failure_evidence(
                    &run_root,
                    &error,
                    process_exit.exit_code,
                    process_exit.termination,
                    &output,
                    &provider.redaction_values,
                )?;
                return Err(format!(
                    "{error}; 启动失败证据：{}",
                    evidence_path.display()
                ));
            }
        };
        let mut driver = window;
        let mut results = Vec::new();
        let mut evidence = EvidenceSummary {
            gpu: GpuEvidence {
                status: "pending",
                source: "Windows ETW/GPU Engine provider not enabled by this offline-safe runner",
                note: "需要独立 GPU 采样绑定 PID 和采样窗口；CPU/截图不能替代 GPU 证据",
            },
            ..EvidenceSummary::default()
        };
        let action_deadline = Instant::now() + Duration::from_millis(options.action_timeout_ms);
        let mut action_ordinal = 0usize;
        let mut run_error = None;
        let mut close_action_completed = false;
        {
            let mut context = ActionContext {
                driver: &mut driver,
                evidence: &mut evidence,
                data_root: &data_root,
                project_root: &project_root,
                journal_baseline: &journal_baseline,
                screenshots: &screenshots,
                accessibility: &accessibility,
                metrics: &metrics,
                results: &mut results,
                ordinal: &mut action_ordinal,
                deadline: action_deadline,
            };
            for action in &plan.actions {
                match execute_action(action, &mut context) {
                    Ok(()) => {
                        if matches!(action, Action::Close) {
                            close_action_completed = true;
                        }
                    }
                    Err(error) => {
                        run_error = Some(error);
                        break;
                    }
                }
            }
        }
        // 计划中的 Close 已经走过真实 Ctrl+Q 退出链；进程自然退出后 HWND 可能立即失效，
        // 再补发一次会把动作全通过的验收误报为“关闭输入失败”。没有显式 Close 时仍保留
        // runner 的兜底关闭，确保计划异常中断也不会遗留被测进程。
        let close_error = if close_action_completed {
            None
        } else {
            driver.close_window().err()
        };
        let process_exit = wait_or_kill(&mut child, Duration::from_secs(5));
        let exit_code = process_exit.exit_code;
        let output = output_capture.finish(PROCESS_OUTPUT_DRAIN_TIMEOUT);
        if run_error.is_none() {
            run_error = close_error
                .map(|error| format!("发送 Ctrl+Q 关闭输入失败：{error}"))
                .or_else(|| match process_exit.termination {
                    "natural" if exit_code == Some(0) => None,
                    "natural" => Some(match exit_code {
                        Some(code) => format!("被测程序自然退出但退出码非零：{code}"),
                        None => "被测程序自然退出但没有可用退出码".to_owned(),
                    }),
                    "killed-after-timeout" => Some(
                        "被测程序未能在关闭等待期限内退出，runner 已强制终止；退出码不用于判断应用异常"
                            .to_owned(),
                    ),
                    _ => {
                        Some("runner 等待被测进程退出时失败，退出码不可用于判断应用异常".to_owned())
                    }
                });
        }
        let process_output_path = write_process_output_evidence(
            &run_root,
            exit_code,
            process_exit.termination,
            run_error.as_deref(),
            &output,
            &provider.redaction_values,
        )?;
        evidence.process_output = Some(process_output_path.display().to_string());
        evidence.journal = summarize_data_root(&data_root, &journal_baseline)?;
        evidence.network_trace = write_redacted_trace(&trace_path, &provider.redaction_values)?;
        if run_error.is_none()
            && let Err(error) = assert_runtime_evidence(&plan, &evidence)
        {
            run_error = Some(error);
        }
        let finished_ms = timestamp_ms();
        let info = driver.info();
        let status = if run_error.is_none() {
            "passed"
        } else {
            "failed"
        };
        let report = RunReport {
            schema: "keencode/ely-native-e2e",
            schema_version: SCHEMA_VERSION,
            runner: "keencode-native-gpui-tests",
            window_driver: "Win32 EnumWindows + PerMonitorV2 + SendInput + PrintWindow + UI Automation",
            started_at_ms: start_ms,
            finished_at_ms: finished_ms,
            binary_path: options.binary.display().to_string(),
            binary_sha256,
            binary_identity,
            reused_from: reuse_context.as_ref().map(|reuse_context| ReuseSummary {
                isolation_root: reuse_context.isolation_root.display().to_string(),
                source_report: reuse_context.source_report.display().to_string(),
                source_binary: BinaryIdentity {
                    path: reuse_context.source_binary.path.clone(),
                    sha256: reuse_context.source_binary.sha256.clone(),
                    source: reuse_context.source_binary.source,
                },
            }),
            provider: provider.summary,
            plan: PlanSummary {
                path: options.plan.map(|path| path.display().to_string()),
                name: plan.name,
                version: plan.version,
                requires_real_provider: plan.requires_real_provider,
                requires_journal: plan.requires_journal,
                requires_network_trace: plan.requires_network_trace,
                requires_reuse_isolation: plan.requires_reuse_isolation,
                native_general_settings: plan.native_general_settings.clone(),
                visual: plan.visual.clone(),
                action_count: plan.actions.len(),
            },
            process: ProcessSummary {
                pid,
                hwnd: info.hwnd,
                title: info.title,
                class_name: info.class_name,
                exit_code,
                termination: process_exit.termination,
                isolation_root: isolation_root.display().to_string(),
                project_root: project_root.display().to_string(),
            },
            actions: results,
            evidence,
            status,
            error: run_error,
        };
        if owns_isolation && !options.keep_isolation {
            // 仅删除本次新建的隔离目录；输出证据目录保留，便于复核。
            let _ = fs::remove_dir_all(&isolation_root);
        }
        Ok((run_root, report))
    }
}

#[cfg(windows)]
struct ActionContext<'a> {
    driver: &'a mut windows_runner::NativeWindowDriver,
    evidence: &'a mut EvidenceSummary,
    data_root: &'a Path,
    project_root: &'a Path,
    journal_baseline: &'a JournalBaseline,
    screenshots: &'a Path,
    accessibility: &'a Path,
    metrics: &'a Path,
    results: &'a mut Vec<ActionResult>,
    ordinal: &'a mut usize,
    deadline: Instant,
}

#[cfg(windows)]
fn execute_action(action: &Action, context: &mut ActionContext<'_>) -> Result<(), String> {
    let started = Instant::now();
    let current = *context.ordinal;
    *context.ordinal += 1;
    let kind = action_kind(action);
    // 所有动作都经过同一个 outcome 结算点；即使总时限已到，也要为当前动作
    // 写入失败 ActionResult 并采集窗口现场，避免报告只停在前一动作。
    let mut special_failure_evidence = Vec::new();
    let outcome = if Instant::now() >= context.deadline {
        Err("动作总超时".to_owned())
    } else {
        (|| match action {
            Action::Wait { ms } => {
                let remaining = context.deadline.saturating_duration_since(Instant::now());
                let wait = Duration::from_millis(*ms);
                if wait > remaining {
                    std::thread::sleep(remaining);
                    Err("等待动作超过动作总超时".to_owned())
                } else {
                    std::thread::sleep(wait);
                    Ok(None)
                }
            }
            Action::Focus => context.driver.focus().map(|_| None),
            Action::Click { x, y } => context.driver.click(*x, *y).map(|_| None),
            Action::Scroll { x, y, delta_y } => {
                context.driver.scroll(*x, *y, *delta_y).map(|_| None)
            }
            Action::TypeText { text } => context.driver.type_text(text).map(|_| None),
            Action::Key { key, modifiers } => context.driver.key(key, modifiers).map(|_| None),
            Action::Screenshot { label } => {
                let path = evidence_path(context.screenshots, current, label, "bmp");
                let capture = context.driver.screenshot(&path)?;
                context
                    .evidence
                    .screenshots
                    .push(path.display().to_string());
                Ok(Some(capture))
            }
            Action::Accessibility { label } => {
                let path = evidence_path(context.accessibility, current, label, "json");
                let tree = context.driver.accessibility_tree()?;
                write_json(&path, &tree)?;
                context
                    .evidence
                    .accessibility_trees
                    .push(path.display().to_string());
                Ok(Some(tree))
            }
            Action::WaitForAccessibility { label, timeout_ms } => (|| {
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));
                let wait_deadline = Instant::now()
                    .checked_add(timeout)
                    .unwrap_or(context.deadline)
                    .min(context.deadline);
                wait_for_accessibility_label(context.driver, label, wait_deadline)?;
                Ok(Some(json!({
                    "label": label,
                    "matched": true,
                })))
            })(),
            Action::ClickAccessibility { label, timeout_ms } => (|| {
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));
                let wait_deadline = Instant::now()
                    .checked_add(timeout)
                    .unwrap_or(context.deadline)
                    .min(context.deadline);
                let mut target =
                    wait_for_accessibility_target(context.driver, label, wait_deadline)?;
                let mut wheel_count = 0usize;
                loop {
                    let (client_width, client_height) = context.driver.client_size()?;
                    let target_top_left =
                        context.driver.screen_to_client(target.left, target.top)?;
                    let target_bottom_right = context
                        .driver
                        .screen_to_client(target.right, target.bottom)?;
                    let target_client_rect = AccessibilityClientRect {
                        left: target_top_left.0,
                        top: target_top_left.1,
                        right: target_bottom_right.0,
                        bottom: target_bottom_right.1,
                    };
                    match accessibility_click_decision(
                        target_client_rect,
                        client_width,
                        client_height,
                    )? {
                        AccessibilityClickDecision::Click { x, y } => {
                            validate_client_point(x, y, client_width, client_height)?;
                            context.driver.click(x, y)?;
                            return Ok(Some(json!({
                                "label": label,
                                "nodeIndex": target.index,
                                "wheelCount": wheel_count,
                                "screenRect": {
                                    "left": target.left,
                                    "top": target.top,
                                    "right": target.right,
                                    "bottom": target.bottom,
                                },
                                "clientRect": {
                                    "left": target_client_rect.left,
                                    "top": target_client_rect.top,
                                    "right": target_client_rect.right,
                                    "bottom": target_client_rect.bottom,
                                },
                                "clientCenter": {
                                    "x": x,
                                    "y": y,
                                },
                                "clientWidth": client_width,
                                "clientHeight": client_height,
                                "input": "SendInput",
                            })));
                        }
                        AccessibilityClickDecision::Scroll { x, y, delta_y } => {
                            if Instant::now() >= wait_deadline {
                                return Err(format!(
                                    "UIA 目标自动滚动超时：label={label} wheelCount={wheel_count}"
                                ));
                            }
                            if wheel_count >= MAX_ACCESSIBILITY_AUTO_SCROLLS {
                                return Err(format!(
                                    "UIA 目标自动滚动超过最大次数：label={label} max={MAX_ACCESSIBILITY_AUTO_SCROLLS}"
                                ));
                            }
                            context.driver.scroll(x, y, delta_y)?;
                            wheel_count = wheel_count.saturating_add(1);
                            let settle = Duration::from_millis(100)
                                .min(wait_deadline.saturating_duration_since(Instant::now()));
                            if !settle.is_zero() {
                                std::thread::sleep(settle);
                            }
                            let next_target = wait_for_accessibility_target(
                                context.driver,
                                label,
                                wait_deadline,
                            )?;
                            if target.left == next_target.left
                                && target.top == next_target.top
                                && target.right == next_target.right
                                && target.bottom == next_target.bottom
                            {
                                return Err(format!(
                                    "UIA 目标自动滚动后 bounds 未变化：label={label} nodeIndex={} bounds=({},{})->({},{})",
                                    target.index,
                                    target.left,
                                    target.top,
                                    target.right,
                                    target.bottom,
                                ));
                            }
                            target = next_target;
                        }
                    }
                }
            })(),
            Action::Metrics { label } => {
                let label = label.as_deref().unwrap_or("sample");
                let path = evidence_path(context.metrics, current, label, "json");
                let sample = context.driver.metrics()?;
                write_json(&path, &sample)?;
                context.evidence.metrics.push(path.display().to_string());
                Ok(Some(sample))
            }
            Action::WaitForJournal {
                event_type,
                count,
                timeout_ms,
                matching,
                matching_text_contains,
                matching_project_paths,
                session_id,
                turn_id,
            } => {
                let query = JournalQuery::new(
                    event_type,
                    matching,
                    matching_text_contains,
                    matching_project_paths,
                    context.project_root,
                    session_id.as_deref(),
                    turn_id.as_deref(),
                );
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(180_000));
                let wait_deadline = Instant::now()
                    .checked_add(timeout)
                    .unwrap_or(context.deadline)
                    .min(context.deadline);
                let observed = match wait_for_journal_event(
                    context.data_root,
                    &query,
                    *count,
                    context.journal_baseline,
                    wait_deadline,
                ) {
                    Ok(observed) => observed,
                    Err(error) => {
                        if error.contains("Journal 事件超时") {
                            special_failure_evidence =
                                capture_journal_wait_failure_evidence(context, current, event_type);
                            if !special_failure_evidence.is_empty() {
                                return Err(format!(
                                    "{error}；失败现场证据：{}",
                                    special_failure_evidence.join(", ")
                                ));
                            }
                            return Err(error);
                        }
                        return Err(error);
                    }
                };
                Ok(Some(json!({
                    "eventType": event_type,
                    "requiredCount": count,
                    "observedCount": observed,
                })))
            }
            Action::AssertJournalCount {
                event_type,
                count,
                matching,
                matching_text_contains,
                matching_project_paths,
                session_id,
                turn_id,
            } => {
                let query = JournalQuery::new(
                    event_type,
                    matching,
                    matching_text_contains,
                    matching_project_paths,
                    context.project_root,
                    session_id.as_deref(),
                    turn_id.as_deref(),
                );
                let observed =
                    count_new_journal_events(context.data_root, &query, context.journal_baseline)?;
                if observed != *count {
                    return Err(format!(
                        "本轮新增 Journal 事件数量不匹配：event_type={event_type} required={count} observed={observed}"
                    ));
                }
                Ok(Some(json!({
                    "eventType": event_type,
                    "expectedCount": count,
                    "observedCount": observed,
                    "relativeToBaseline": true,
                })))
            }
            Action::WaitForFile {
                path,
                contains,
                timeout_ms,
            } => {
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(180_000));
                let wait_deadline = Instant::now()
                    .checked_add(timeout)
                    .unwrap_or(context.deadline)
                    .min(context.deadline);
                let bytes = wait_for_project_file(
                    context.project_root,
                    path,
                    contains.as_deref(),
                    wait_deadline,
                )?;
                Ok(Some(json!({
                    "path": path,
                    "bytes": bytes,
                    "containsMatched": contains.is_some(),
                })))
            }
            Action::AssertFile {
                path,
                scope,
                contains,
                min_bytes,
                json_pointer_equals,
            } => {
                let root = match scope {
                    FileScope::Project => context.project_root,
                    FileScope::Data => context.data_root,
                };
                Ok(Some(assert_isolated_file(
                    root,
                    *scope,
                    path,
                    contains.as_deref(),
                    *min_bytes,
                    json_pointer_equals,
                )?))
            }
            Action::AssertFileAbsent { path, scope } => {
                let root = match scope {
                    FileScope::Project => context.project_root,
                    FileScope::Data => context.data_root,
                };
                Ok(Some(assert_isolated_file_absent(root, *scope, path)?))
            }
            Action::AssertGoalState { matching } => {
                Ok(Some(assert_goal_state(context.data_root, matching)?))
            }
            Action::AssertAutomationState {
                title,
                matching,
                history_outcomes,
            } => Ok(Some(assert_automation_state(
                context.data_root,
                title,
                matching,
                history_outcomes,
            )?)),
            Action::AssertGit {
                require_clean,
                commit_message_contains,
                worktree_count,
                worktree_branch_contains,
            } => Ok(Some(assert_isolated_git(
                context.project_root,
                *require_clean,
                commit_message_contains.as_deref(),
                *worktree_count,
                worktree_branch_contains.as_deref(),
            )?)),
            Action::AssertWindow {
                title_contains,
                class_contains,
                client_width,
                client_height,
                dpi,
            } => {
                let info = context.driver.info();
                if title_contains.as_deref().is_some_and(|expected| {
                    !info.title.as_deref().unwrap_or_default().contains(expected)
                }) {
                    return Err("窗口标题不满足断言".to_owned());
                }
                if class_contains.as_deref().is_some_and(|expected| {
                    !info
                        .class_name
                        .as_deref()
                        .unwrap_or_default()
                        .contains(expected)
                }) {
                    return Err("窗口类名不满足断言".to_owned());
                }
                let geometry = if client_width.is_some() || client_height.is_some() || dpi.is_some()
                {
                    Some(context.driver.metrics()?)
                } else {
                    None
                };
                if let Some(expected) = client_width {
                    let observed = geometry
                        .as_ref()
                        .and_then(|value| value.pointer("/window/clientRect/width"))
                        .and_then(Value::as_i64)
                        .and_then(|value| u32::try_from(value).ok());
                    if observed != Some(*expected) {
                        return Err(format!(
                            "窗口 client 宽度不满足断言：期望 {expected}，实际 {:?}",
                            observed
                        ));
                    }
                }
                if let Some(expected) = client_height {
                    let observed = geometry
                        .as_ref()
                        .and_then(|value| value.pointer("/window/clientRect/height"))
                        .and_then(Value::as_i64)
                        .and_then(|value| u32::try_from(value).ok());
                    if observed != Some(*expected) {
                        return Err(format!(
                            "窗口 client 高度不满足断言：期望 {expected}，实际 {:?}",
                            observed
                        ));
                    }
                }
                if let Some(expected) = dpi {
                    let observed = geometry
                        .as_ref()
                        .and_then(|value| value.pointer("/window/dpi"))
                        .and_then(Value::as_i64)
                        .and_then(|value| u32::try_from(value).ok());
                    if observed != Some(*expected) {
                        return Err(format!(
                            "窗口 DPI 不满足断言：期望 {expected}，实际 {:?}",
                            observed
                        ));
                    }
                }
                Ok(Some(json!({
                    "title": info.title,
                    "className": info.class_name,
                    "geometry": geometry,
                })))
            }
            Action::ResizeClient { width, height } => {
                Ok(Some(context.driver.resize_client(*width, *height)?))
            }
            Action::Repeat { count, actions } => {
                if *count == 0 || *count > 64 {
                    return Err("repeat.count 必须在 1..64 范围内".to_owned());
                }
                for _ in 0..*count {
                    for nested in actions {
                        execute_action(nested, context)?;
                    }
                }
                Ok(Some(
                    json!({"count": count, "nestedActionCount": actions.len()}),
                ))
            }
            Action::Close => context.driver.close_window().map(|_| None),
        })()
    };
    match outcome {
        Ok(evidence_value) => {
            context.results.push(ActionResult {
                ordinal: current,
                kind,
                status: "passed",
                elapsed_ms: started.elapsed().as_millis(),
                evidence: evidence_value,
                error: None,
            });
            Ok(())
        }
        Err(error) => {
            // Journal/UIA 超时沿用专用证据名称；其它失败统一追加当前 HWND 的截图和
            // accessibility tree。专用分支已经采集时不再重复读窗口。
            if special_failure_evidence.is_empty()
                && matches!(
                    action,
                    Action::WaitForAccessibility { .. } | Action::ClickAccessibility { .. }
                )
            {
                special_failure_evidence =
                    capture_journal_wait_failure_evidence(context, current, "uia-target");
            }
            if special_failure_evidence.is_empty() {
                special_failure_evidence = capture_action_failure_evidence(context, current, &kind);
            }
            action_failure(
                current,
                kind,
                started,
                error,
                special_failure_evidence,
                context.results,
            )
        }
    }
}

#[cfg(windows)]
fn action_failure(
    ordinal: usize,
    kind: String,
    started: Instant,
    error: String,
    evidence_paths: Vec<String>,
    results: &mut Vec<ActionResult>,
) -> Result<(), String> {
    results.push(ActionResult {
        ordinal,
        kind,
        status: "failed",
        elapsed_ms: started.elapsed().as_millis(),
        evidence: (!evidence_paths.is_empty()).then(|| {
            json!({
                "failureEvidence": evidence_paths,
            })
        }),
        error: Some(error.clone()),
    });
    Err(error)
}

#[cfg(windows)]
fn action_kind(action: &Action) -> String {
    match action {
        Action::Wait { .. } => "wait",
        Action::Focus => "focus",
        Action::Click { .. } => "click",
        Action::Scroll { .. } => "scroll",
        Action::TypeText { .. } => "type_text",
        Action::Key { .. } => "key",
        Action::Screenshot { .. } => "screenshot",
        Action::Accessibility { .. } => "accessibility",
        Action::WaitForAccessibility { .. } => "wait_for_accessibility",
        Action::ClickAccessibility { .. } => "click_accessibility",
        Action::Metrics { .. } => "metrics",
        Action::WaitForJournal { .. } => "wait_for_journal",
        Action::AssertJournalCount { .. } => "assert_journal_count",
        Action::WaitForFile { .. } => "wait_for_file",
        Action::AssertFile { .. } => "assert_file",
        Action::AssertFileAbsent { .. } => "assert_file_absent",
        Action::AssertGoalState { .. } => "assert_goal_state",
        Action::AssertAutomationState { .. } => "assert_automation_state",
        Action::AssertGit { .. } => "assert_git",
        Action::AssertWindow { .. } => "assert_window",
        Action::ResizeClient { .. } => "resize_client",
        Action::Repeat { .. } => "repeat",
        Action::Close => "close",
    }
    .to_owned()
}

/// 在真实 client 坐标系内校验计划点击点，避免把越界坐标交给 SendInput。
fn validate_client_point(x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
    if x < 0 || y < 0 || u32::try_from(x).unwrap_or(u32::MAX) >= width {
        return Err(format!(
            "UIA 点击 x 坐标越出 client 边界：point=({x},{y}) client={width}x{height}"
        ));
    }
    if u32::try_from(y).unwrap_or(u32::MAX) >= height {
        return Err(format!(
            "UIA 点击 y 坐标越出 client 边界：point=({x},{y}) client={width}x{height}"
        ));
    }
    Ok(())
}

/// UIA helper 返回的是 bounded subtree；只在标准 name/value 属性上做精确匹配。
fn accessibility_tree_contains_name(tree: &Value, label: &str) -> bool {
    let root_matches = tree
        .pointer("/rootProperties/name/value")
        .and_then(Value::as_str)
        .is_some_and(|value| value == label);
    if root_matches {
        return true;
    }
    tree.pointer("/subtree/nodes")
        .and_then(Value::as_array)
        .is_some_and(|nodes| {
            nodes.iter().any(|node| {
                node.pointer("/properties/name/value")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value == label)
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AccessibilityTarget {
    index: usize,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AccessibilityClientRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessibilityClickDecision {
    Click { x: i32, y: i32 },
    Scroll { x: i32, y: i32, delta_y: i32 },
}

fn accessibility_click_decision(
    rect: AccessibilityClientRect,
    client_width: u32,
    client_height: u32,
) -> Result<AccessibilityClickDecision, String> {
    if client_width == 0 || client_height == 0 {
        return Err(format!(
            "UIA 点击目标的 client 为空：client={}x{}",
            client_width, client_height
        ));
    }
    let left = i64::from(rect.left);
    let top = i64::from(rect.top);
    let right = i64::from(rect.right);
    let bottom = i64::from(rect.bottom);
    if left >= right || top >= bottom {
        return Err(format!(
            "UIA bounds 转换后矩形无效：left={left} top={top} right={right} bottom={bottom}"
        ));
    }

    let client_right = i64::from(client_width);
    let client_bottom = i64::from(client_height);
    let intersection_left = left.max(0);
    let intersection_right = right.min(client_right);
    if intersection_left >= intersection_right {
        return Err(format!(
            "UIA bounds 与 client 无水平交集：bounds=({left},{top})-({right},{bottom}) client={}x{}",
            client_width, client_height
        ));
    }

    // 水平只要存在真实交集，就用交集中心作为安全输入点；垂直方向必须先让
    // 目标中心进入 client，不能把越界中心钳到边界后伪装成可点击。
    let input_x = i32::try_from(intersection_left + (intersection_right - intersection_left) / 2)
        .map_err(|_| "UIA bounds 水平交集中心超出 client 坐标范围".to_owned())?;
    let target_center_y = top + (bottom - top) / 2;
    if target_center_y >= 0 && target_center_y < client_bottom {
        return Ok(AccessibilityClickDecision::Click {
            x: input_x,
            y: i32::try_from(target_center_y)
                .map_err(|_| "UIA bounds 中心 y 超出 client 坐标范围".to_owned())?,
        });
    }

    let wheel_y = i32::try_from(client_bottom / 2)
        .map_err(|_| "client 中部 y 超出 client 坐标范围".to_owned())?;
    let delta_y = if target_center_y < 0 {
        ACCESSIBILITY_AUTO_SCROLL_DELTA_Y
    } else {
        -ACCESSIBILITY_AUTO_SCROLL_DELTA_Y
    };
    Ok(AccessibilityClickDecision::Scroll {
        x: input_x,
        y: wheel_y,
        delta_y,
    })
}

fn accessibility_target_from_tree(
    tree: &Value,
    label: &str,
) -> Result<AccessibilityTarget, String> {
    let nodes = tree
        .pointer("/subtree/nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "UIA 子树不可用，无法定位控件".to_owned())?;
    let mut exact_count = 0usize;
    let mut candidates = Vec::new();
    let mut invalid_bounds_count = 0usize;
    let mut non_interactive_role_count = 0usize;
    for node in nodes {
        let name = node
            .pointer("/properties/name/value")
            .and_then(Value::as_str);
        if name != Some(label) {
            continue;
        }
        exact_count = exact_count.saturating_add(1);
        if !accessibility_node_has_clickable_control_type(node) {
            non_interactive_role_count = non_interactive_role_count.saturating_add(1);
            continue;
        }
        let enabled = node
            .pointer("/properties/isEnabled/value")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // 保留带有效屏幕 bounds 的 offscreen 节点，让 ClickAccessibility 用真实
        // wheel 把它滚回 client；缺失该 UIA 属性仍按不完整节点拒绝。
        let has_offscreen_property = node
            .pointer("/properties/isOffscreen/value")
            .and_then(Value::as_bool)
            .is_some();
        if !enabled || !has_offscreen_property {
            continue;
        }
        let Some(rect) = accessibility_bounds(node) else {
            invalid_bounds_count = invalid_bounds_count.saturating_add(1);
            continue;
        };
        candidates.push(AccessibilityTarget {
            index: node
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_default(),
            left: rect.0,
            top: rect.1,
            right: rect.2,
            bottom: rect.3,
        });
    }
    match candidates.as_slice() {
        [target] => Ok(*target),
        [] => {
            let truncated = tree
                .pointer("/subtree/nodesTruncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Err(format!(
                "UIA 标签没有唯一可点击节点：label={label} exact={exact_count} rejectedRoles={non_interactive_role_count} invalidBounds={invalid_bounds_count} nodesTruncated={truncated}"
            ))
        }
        _ => Err(format!(
            "UIA 标签匹配到多个可点击节点：label={label} count={}",
            candidates.len()
        )),
    }
}

/// 点击只接受标准 UIA 的真实可交互控件；Text、Group 以及缺失/未知 role
/// 仍可通过 wait_for_accessibility 观察，但不能成为 SendInput 的点击证据。
fn accessibility_node_has_clickable_control_type(node: &Value) -> bool {
    matches!(
        node.pointer("/properties/controlType/value")
            .and_then(Value::as_u64),
        Some(
            50000 // Button
                | 50002 // CheckBox / AccessKit Switch
                | 50003 // ComboBox
                | 50004 // Edit
                | 50005 // Hyperlink
                | 50007 // ListItem
                | 50009 // Menu
                | 50010 // MenuBar
                | 50011 // MenuItem
                | 50013 // RadioButton
                | 50015 // Slider
                | 50016 // Spinner
                | 50018 // Tab
                | 50019 // TabItem
        )
    )
}

fn accessibility_bounds(node: &Value) -> Option<(i32, i32, i32, i32)> {
    let value = node.pointer("/properties/boundingRectangle/value")?;
    let left = value
        .get("left")?
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())?;
    let top = value
        .get("top")?
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())?;
    let right = value
        .get("right")?
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())?;
    let bottom = value
        .get("bottom")?
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())?;
    (left < right && top < bottom).then_some((left, top, right, bottom))
}

#[cfg(windows)]
fn wait_for_accessibility_label(
    driver: &windows_runner::NativeWindowDriver,
    label: &str,
    deadline: Instant,
) -> Result<(), String> {
    validate_accessibility_label(label)?;
    let mut last_error = None;
    loop {
        match driver.accessibility_tree() {
            Ok(tree) if accessibility_tree_contains_name(&tree, label) => return Ok(()),
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
        if Instant::now() >= deadline {
            let detail = last_error
                .map(|error| format!("；最后一次 UIA 读取失败：{error}"))
                .unwrap_or_default();
            return Err(format!("等待 UIA 标签超时：{label}{detail}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn wait_for_accessibility_target(
    driver: &windows_runner::NativeWindowDriver,
    label: &str,
    deadline: Instant,
) -> Result<AccessibilityTarget, String> {
    validate_accessibility_label(label)?;
    loop {
        let last_error = match driver.accessibility_tree() {
            Ok(tree) => match accessibility_target_from_tree(&tree, label) {
                Ok(target) => return Ok(target),
                Err(error) => error,
            },
            Err(error) => error,
        };
        if Instant::now() >= deadline {
            return Err(format!(
                "等待可点击 UIA 标签超时：{label}；最后一次 UIA 定位失败：{last_error}"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn validate_accessibility_label(label: &str) -> Result<(), String> {
    if label.is_empty() || label.len() > 256 || label.chars().any(char::is_control) {
        return Err("UIA 标签必须是非空、有限且无控制字符的文本".to_owned());
    }
    Ok(())
}

#[cfg(windows)]
fn capture_action_failure_evidence(
    context: &mut ActionContext<'_>,
    ordinal: usize,
    kind: &str,
) -> Vec<String> {
    let label = format!("action-failure-{kind}");
    capture_window_failure_evidence(context, ordinal, &label)
}

#[cfg(windows)]
fn capture_journal_wait_failure_evidence(
    context: &mut ActionContext<'_>,
    ordinal: usize,
    event_type: &str,
) -> Vec<String> {
    let label = format!("journal-timeout-{event_type}");
    capture_window_failure_evidence(context, ordinal, &label)
}

#[cfg(windows)]
fn capture_window_failure_evidence(
    context: &mut ActionContext<'_>,
    ordinal: usize,
    label: &str,
) -> Vec<String> {
    let mut paths = Vec::new();
    let screenshot_path = evidence_path(context.screenshots, ordinal, label, "bmp");
    if context.driver.screenshot(&screenshot_path).is_ok() {
        let path = screenshot_path.display().to_string();
        context.evidence.screenshots.push(path.clone());
        paths.push(path);
    }
    let accessibility_path = evidence_path(context.accessibility, ordinal, label, "json");
    if let Ok(tree) = context.driver.accessibility_tree()
        && write_json(&accessibility_path, &tree).is_ok()
    {
        let path = accessibility_path.display().to_string();
        context.evidence.accessibility_trees.push(path.clone());
        paths.push(path);
    }
    paths
}

#[cfg(windows)]
fn evidence_path(root: &Path, ordinal: usize, label: &str, extension: &str) -> PathBuf {
    let safe = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    root.join(format!("{ordinal:04}-{safe}.{extension}"))
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct ProcessWaitResult {
    exit_code: Option<i32>,
    termination: &'static str,
}

#[cfg(windows)]
fn wait_or_kill(child: &mut std::process::Child, timeout: Duration) -> ProcessWaitResult {
    // Windows 强制终止常见地返回 1；必须先记录 termination，再决定退出码是否代表应用异常。
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return ProcessWaitResult {
                    exit_code: status.code(),
                    termination: "natural",
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                return ProcessWaitResult {
                    exit_code: child.wait().ok().and_then(|status| status.code()),
                    termination: "killed-after-timeout",
                };
            }
            Err(_) => {
                return ProcessWaitResult {
                    exit_code: None,
                    termination: "wait-error",
                };
            }
        }
    }
}

#[cfg(windows)]
fn write_isolated_settings(
    data_root: &Path,
    general_settings: &NativeGeneralSettings,
) -> Result<(), String> {
    // 真实关闭动作使用 Ctrl+Q；验收隔离配置显式关闭 close-to-tray，避免把
    // “隐藏到托盘后等待超时”误判成被测进程正常退出。该配置只存在本次隔离根。
    fs::write(
        data_root.join("settings.json"),
        concat!(
            "{\n",
            "  \"schema\": \"keencode/app-settings\",\n",
            "  \"version\": 1,\n",
            "  \"closeToTray\": false\n",
            "}\n"
        ),
    )
    .map_err(|error| format!("写入原生验收隔离设置失败：{error}"))?;
    let general_file = json!({
        "schema": "keencode/native-general-settings",
        "version": 2,
        "appearance": &general_settings.appearance,
        "fontFamily": &general_settings.font_family,
        "fontSizePx": general_settings.font_size_px,
        "density": &general_settings.density,
        "reducedMotion": general_settings.reduced_motion,
        "code": {
            "fontFamily": "Consolas",
            "fontSizePx": 12,
            "showLineNumbers": true,
            "wrapLongLines": false,
            "lightTheme": "github-light",
            "darkTheme": "github-dark"
        }
    });
    let general_path = data_root.join("native-general-settings.json");
    let bytes = serde_json::to_vec_pretty(&general_file)
        .map_err(|error| format!("编码原生验收常规设置失败：{error}"))?;
    fs::write(&general_path, bytes).map_err(|error| {
        format!(
            "写入原生验收常规设置失败 {}: {error}",
            general_path.display()
        )
    })
}

#[cfg(windows)]
fn write_isolated_project(project_root: &Path) -> Result<(), String> {
    fs::create_dir_all(project_root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        project_root.join("README.md"),
        "Ely native acceptance isolated project\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        project_root.join("src").join("facts.txt"),
        "read-write-bash acceptance fixture\n",
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(windows)]
fn summarize_data_root(root: &Path, baseline: &JournalBaseline) -> Result<JournalEvidence, String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files, 0)?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let total_event_counts = collect_journal_event_counts(root)?;
    let baseline_event_count = baseline
        .event_counts
        .values()
        .fold(0usize, |total, count| total.saturating_add(*count));
    let baseline_turn_started_count = baseline
        .event_counts
        .get("turn_started")
        .copied()
        .unwrap_or_default();
    let baseline_terminal_event_count = baseline
        .event_counts
        .get("turn_completed")
        .copied()
        .unwrap_or_default()
        .saturating_add(
            baseline
                .event_counts
                .get("turn_stopped")
                .copied()
                .unwrap_or_default(),
        );
    let total_turn_started_count = total_event_counts
        .get("turn_started")
        .copied()
        .unwrap_or_default();
    let total_terminal_event_count = total_event_counts
        .get("turn_completed")
        .copied()
        .unwrap_or_default()
        .saturating_add(
            total_event_counts
                .get("turn_stopped")
                .copied()
                .unwrap_or_default(),
        );
    let mut evidence = JournalEvidence {
        data_root: root.display().to_string(),
        file_count: files.len(),
        files,
        event_file_count: 0,
        event_bytes: 0,
        baseline_event_count,
        baseline_turn_started_count,
        baseline_terminal_event_count,
        turn_started_count: total_turn_started_count.saturating_sub(baseline_turn_started_count),
        terminal_event_count: total_terminal_event_count
            .saturating_sub(baseline_terminal_event_count),
        total_turn_started_count,
        total_terminal_event_count,
        note: "记录隔离数据根内的相对路径、大小和有限 Journal 计数；turn_started_count/terminal_event_count 只统计本轮相对 baseline 的新增事件；不输出 Journal 正文",
    };
    let event_files = evidence
        .files
        .iter()
        .filter(|file| file.relative_path.ends_with("events.jsonl"))
        .map(|file| (file.relative_path.clone(), file.bytes))
        .collect::<Vec<_>>();
    for (_relative_path, bytes) in event_files {
        evidence.event_file_count += 1;
        evidence.event_bytes = evidence.event_bytes.saturating_add(bytes);
    }
    Ok(evidence)
}

const MAX_JOURNAL_SCAN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PROJECT_FILE_READ_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(windows)]
// 限制计划提供的子串长度，避免 Journal 查询在单个条件上消耗无界内存和时间。
const MAX_JOURNAL_TEXT_CONTAINS_BYTES: usize = 4 * 1024;

#[cfg(windows)]
fn wait_for_journal_event(
    root: &Path,
    query: &JournalQuery,
    required_count: usize,
    baseline: &JournalBaseline,
    deadline: Instant,
) -> Result<usize, String> {
    query.validate()?;
    if required_count == 0 {
        return Err("wait_for_journal 的 event_type/count 无效".to_owned());
    }
    let baseline_count = journal_baseline_count(query, baseline)?;
    loop {
        let total = count_journal_events(root, query)?;
        let observed = total.saturating_sub(baseline_count);
        if observed >= required_count {
            return Ok(observed);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "等待本轮新增 Journal 事件超时：event_type={} required={required_count} baseline={baseline_count} total={total} observed={observed}",
                query.event_type
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn count_new_journal_events(
    root: &Path,
    query: &JournalQuery,
    baseline: &JournalBaseline,
) -> Result<usize, String> {
    query.validate()?;
    let total = count_journal_events(root, query)?;
    let baseline_count = journal_baseline_count(query, baseline)?;
    Ok(total.saturating_sub(baseline_count))
}

#[cfg(windows)]
fn journal_baseline_count(
    query: &JournalQuery,
    baseline: &JournalBaseline,
) -> Result<usize, String> {
    if query.has_filters() {
        let key = query.cache_key()?;
        Ok(baseline.query_counts.get(&key).copied().unwrap_or_default())
    } else {
        Ok(baseline
            .event_counts
            .get(&query.event_type)
            .copied()
            .unwrap_or_default())
    }
}

#[cfg(windows)]
fn capture_journal_baseline(
    root: &Path,
    project_root: &Path,
    actions: &[Action],
) -> Result<JournalBaseline, String> {
    let queries = collect_journal_queries(project_root, actions)?;
    Ok(JournalBaseline {
        event_counts: collect_journal_event_counts(root)?,
        query_counts: collect_journal_query_counts(root, &queries)?,
    })
}

#[cfg(windows)]
fn collect_journal_event_counts(root: &Path) -> Result<BTreeMap<String, usize>, String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files, 0)?;
    let mut counts = BTreeMap::new();
    for file in files
        .into_iter()
        .filter(|file| file.relative_path.ends_with("events.jsonl"))
    {
        collect_journal_event_counts_in_file(&root.join(file.relative_path), &mut counts)?;
    }
    Ok(counts)
}

#[cfg(windows)]
fn collect_journal_event_counts_in_file(
    path: &Path,
    counts: &mut BTreeMap<String, usize>,
) -> Result<(), String> {
    visit_journal_event_file(path, &mut |event, _session_id| {
        if let Some(event_type) = event.get("type").and_then(Value::as_str) {
            let count = counts.entry(event_type.to_owned()).or_default();
            *count = count.saturating_add(1);
        }
    })
}

#[cfg(windows)]
fn collect_journal_queries(
    project_root: &Path,
    actions: &[Action],
) -> Result<BTreeMap<String, JournalQuery>, String> {
    let mut queries = BTreeMap::new();
    collect_journal_queries_from_actions(project_root, actions, &mut queries)?;
    Ok(queries)
}

#[cfg(windows)]
fn collect_journal_queries_from_actions(
    project_root: &Path,
    actions: &[Action],
    queries: &mut BTreeMap<String, JournalQuery>,
) -> Result<(), String> {
    for action in actions {
        match action {
            Action::WaitForJournal {
                event_type,
                matching,
                matching_text_contains,
                matching_project_paths,
                session_id,
                turn_id,
                ..
            }
            | Action::AssertJournalCount {
                event_type,
                matching,
                matching_text_contains,
                matching_project_paths,
                session_id,
                turn_id,
                ..
            } => {
                let query = JournalQuery::new(
                    event_type,
                    matching,
                    matching_text_contains,
                    matching_project_paths,
                    project_root,
                    session_id.as_deref(),
                    turn_id.as_deref(),
                );
                query.validate()?;
                let key = query.cache_key()?;
                queries.entry(key).or_insert(query);
            }
            Action::Repeat { actions, .. } => {
                collect_journal_queries_from_actions(project_root, actions, queries)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(windows)]
fn collect_journal_query_counts(
    root: &Path,
    queries: &BTreeMap<String, JournalQuery>,
) -> Result<BTreeMap<String, usize>, String> {
    let mut counts = queries
        .keys()
        .map(|key| (key.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    if queries.is_empty() {
        return Ok(counts);
    }
    let mut files = Vec::new();
    collect_files(root, root, &mut files, 0)?;
    for file in files
        .into_iter()
        .filter(|file| file.relative_path.ends_with("events.jsonl"))
    {
        collect_journal_query_counts_in_file(&root.join(file.relative_path), queries, &mut counts)?;
    }
    Ok(counts)
}

#[cfg(windows)]
fn collect_journal_query_counts_in_file(
    path: &Path,
    queries: &BTreeMap<String, JournalQuery>,
    counts: &mut BTreeMap<String, usize>,
) -> Result<(), String> {
    visit_journal_event_file(path, &mut |event, session_id| {
        for (key, query) in queries {
            if query.matches(event, session_id) {
                let count = counts.entry(key.clone()).or_default();
                *count = count.saturating_add(1);
            }
        }
    })
}

#[cfg(windows)]
fn count_journal_events(root: &Path, query: &JournalQuery) -> Result<usize, String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files, 0)?;
    let mut count = 0usize;
    for file in files
        .into_iter()
        .filter(|file| file.relative_path.ends_with("events.jsonl"))
    {
        count_journal_events_in_file(&root.join(file.relative_path), query, &mut count)?;
    }
    Ok(count)
}

#[cfg(windows)]
fn count_journal_events_in_file(
    path: &Path,
    query: &JournalQuery,
    count: &mut usize,
) -> Result<(), String> {
    visit_journal_event_file(path, &mut |event, session_id| {
        if query.matches(event, session_id) {
            *count = count.saturating_add(1);
        }
    })
}

#[cfg(windows)]
fn visit_journal_event_file<F>(path: &Path, visitor: &mut F) -> Result<(), String>
where
    F: FnMut(&Value, Option<&str>),
{
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // 目录快照与原子替换之间可能有极短窗口：events.jsonl 已被枚举，
            // 但在打开前被替换或删除。下一轮 wait_for_journal 会继续扫描；
            // 根目录缺失和其它 I/O 错误仍由调用方直接报告。
            return Ok(());
        }
        Err(error) => {
            return Err(format!("读取 Journal 事件失败 {}: {error}", path.display()));
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_SCAN_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取 Journal 事件失败 {}: {error}", path.display()))?;
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        visit_journal_event_value(&value, None, visitor);
    }
    Ok(())
}

#[cfg(windows)]
fn visit_journal_event_value<F>(value: &Value, inherited_session_id: Option<&str>, visitor: &mut F)
where
    F: FnMut(&Value, Option<&str>),
{
    let session_id = value
        .get("session")
        .and_then(Value::as_str)
        .or(inherited_session_id);
    visitor(value, session_id);
    if let Some(events) = value
        .get("payload")
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("events"))
        .and_then(Value::as_array)
    {
        for event in events {
            visit_journal_event_value(event, session_id, visitor);
        }
    }
}

#[cfg(windows)]
fn wait_for_project_file(
    project_root: &Path,
    relative_path: &str,
    expected_text: Option<&str>,
    deadline: Instant,
) -> Result<u64, String> {
    let relative = Path::new(relative_path);
    if relative_path.is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
                    | std::path::Component::ParentDir
            )
        })
    {
        return Err("wait_for_file 只允许项目根内的相对路径".to_owned());
    }
    let root =
        fs::canonicalize(project_root).map_err(|error| format!("解析隔离项目根失败：{error}"))?;
    let path = root.join(relative);
    loop {
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!("等待的项目输出不是普通文件：{relative_path}"));
            }
            if metadata.len() > MAX_PROJECT_FILE_READ_BYTES {
                return Err(format!(
                    "等待的项目输出超过 {} 字节：{relative_path}",
                    MAX_PROJECT_FILE_READ_BYTES
                ));
            }
            let canonical = fs::canonicalize(&path)
                .map_err(|error| format!("解析项目输出失败 {relative_path}：{error}"))?;
            if !canonical.starts_with(&root) {
                return Err(format!("项目输出路径越出隔离项目根：{relative_path}"));
            }
            if let Some(expected_text) = expected_text {
                let bytes = fs::read(&canonical)
                    .map_err(|error| format!("读取项目输出失败 {relative_path}：{error}"))?;
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| format!("项目输出不是 UTF-8 文本：{relative_path}"))?;
                if !text.contains(expected_text) {
                    if Instant::now() >= deadline {
                        return Err(format!("项目输出未出现要求标记：{relative_path}"));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            }
            return Ok(metadata.len());
        }
        if Instant::now() >= deadline {
            return Err(format!("等待项目输出超时：{relative_path}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn validate_relative_file_path(relative_path: &str, action: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative_path);
    if relative_path.is_empty()
        || relative_path.chars().any(char::is_control)
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
                    | std::path::Component::ParentDir
            )
        })
    {
        return Err(format!("{action} 只允许隔离根内的相对路径"));
    }
    Ok(relative.to_owned())
}

#[cfg(windows)]
fn assert_isolated_file(
    root: &Path,
    scope: FileScope,
    relative_path: &str,
    expected_text: Option<&str>,
    min_bytes: Option<u64>,
    json_pointer_equals: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let relative = validate_relative_file_path(relative_path, "assert_file")?;
    if expected_text.is_some_and(|text| {
        text.is_empty()
            || text.len() > MAX_PROJECT_FILE_READ_BYTES as usize
            || text.chars().any(char::is_control)
    }) {
        return Err("assert_file 的 contains 必须是非空、有限且无控制字符的文本".to_owned());
    }
    let root = fs::canonicalize(root).map_err(|error| format!("解析隔离根失败：{error}"))?;
    let path = root.join(&relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("读取断言文件失败 {relative_path}：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "assert_file 目标必须是非符号链接普通文件：{relative_path}"
        ));
    }
    if metadata.len() > MAX_PROJECT_FILE_READ_BYTES {
        return Err(format!(
            "assert_file 目标超过 {} 字节：{relative_path}",
            MAX_PROJECT_FILE_READ_BYTES
        ));
    }
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("解析断言文件失败 {relative_path}：{error}"))?;
    if !path_contains(&root, &canonical) {
        return Err(format!("assert_file 目标路径越出隔离根：{relative_path}"));
    }
    let bytes = fs::read(&canonical)
        .map_err(|error| format!("读取断言文件失败 {relative_path}：{error}"))?;
    let byte_count = bytes.len() as u64;
    if byte_count > MAX_PROJECT_FILE_READ_BYTES {
        return Err(format!(
            "assert_file 目标读取时超过 {} 字节：{relative_path}",
            MAX_PROJECT_FILE_READ_BYTES
        ));
    }
    if let Some(min_bytes) = min_bytes
        && byte_count < min_bytes
    {
        return Err(format!(
            "assert_file 目标小于要求的最小字节数：path={relative_path} required={min_bytes} observed={byte_count}"
        ));
    }
    let contains_matched = if let Some(expected_text) = expected_text {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| format!("assert_file 目标不是 UTF-8 文本：{relative_path}"))?;
        if !text.contains(expected_text) {
            return Err(format!("assert_file 目标未包含要求文本：{relative_path}"));
        }
        true
    } else {
        false
    };
    let json_pointer_equals_matched = if json_pointer_equals.is_empty() {
        0
    } else {
        let document: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("assert_file 目标不是合法 JSON：{relative_path}: {error}"))?;
        for (pointer, expected) in json_pointer_equals {
            let actual = document.pointer(pointer).ok_or_else(|| {
                format!("assert_file 目标缺少 JSON Pointer：path={relative_path} pointer={pointer}")
            })?;
            if actual != expected {
                return Err(format!(
                    "assert_file JSON Pointer 不匹配：path={relative_path} pointer={pointer}"
                ));
            }
        }
        json_pointer_equals.len()
    };
    Ok(json!({
        "scope": file_scope_name(scope),
        "path": relative_path,
        "bytes": byte_count,
        "minBytes": min_bytes,
        "containsMatched": contains_matched,
        "jsonPointerEqualsMatched": json_pointer_equals_matched,
        "readOnly": true,
    }))
}

#[cfg(windows)]
fn assert_isolated_file_absent(
    root: &Path,
    scope: FileScope,
    relative_path: &str,
) -> Result<Value, String> {
    let relative = validate_relative_file_path(relative_path, "assert_file_absent")?;
    let root = fs::canonicalize(root).map_err(|error| format!("解析隔离根失败：{error}"))?;
    let path = root.join(&relative);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(json!({
            "scope": file_scope_name(scope),
            "path": relative_path,
            "absent": true,
            "readOnly": true,
        })),
        Ok(_) => Err(format!("assert_file_absent 目标不应存在：{relative_path}")),
        Err(error) => Err(format!("读取不存在文件断言失败 {relative_path}：{error}")),
    }
}

#[cfg(windows)]
fn assert_goal_state(
    data_root: &Path,
    matching: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    validate_goal_matching(matching, "assert_goal_state")?;

    let data_metadata = fs::symlink_metadata(data_root)
        .map_err(|error| format!("读取 Goal data 根失败：{error}"))?;
    if data_metadata.file_type().is_symlink() || !data_metadata.is_dir() {
        return Err("Goal data 根必须是非符号链接目录".to_owned());
    }
    let data_root =
        fs::canonicalize(data_root).map_err(|error| format!("解析 Goal data 根失败：{error}"))?;
    let goals_path = data_root.join("goals");
    let goals_metadata = fs::symlink_metadata(&goals_path)
        .map_err(|error| format!("读取 Goal 目录失败：{error}"))?;
    if goals_metadata.file_type().is_symlink() || !goals_metadata.is_dir() {
        return Err("Goal 目录必须是 data 根内的非符号链接目录".to_owned());
    }
    let goals_root =
        fs::canonicalize(&goals_path).map_err(|error| format!("解析 Goal 目录失败：{error}"))?;
    if !path_contains(&data_root, &goals_root) {
        return Err("Goal 目录路径越出隔离 data 根".to_owned());
    }

    let mut candidates = Vec::new();
    for entry in
        fs::read_dir(&goals_root).map_err(|error| format!("读取 Goal 目录失败：{error}"))?
    {
        let entry = entry.map_err(|error| format!("读取 Goal 目录项失败：{error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with("session-goal-")
            && name.ends_with(".json")
            && name.len() > "session-goal-.json".len()
        {
            candidates.push(entry.path());
        }
    }
    if candidates.len() != 1 {
        return Err(format!(
            "Goal 文档必须恰好有一个 session-goal-*.json 文件，实际 {} 个",
            candidates.len()
        ));
    }

    let goal_path = &candidates[0];
    let metadata =
        fs::symlink_metadata(goal_path).map_err(|error| format!("读取 Goal 文档失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Goal 文档必须是非符号链接普通文件".to_owned());
    }
    if metadata.len() > MAX_GOAL_DOCUMENT_BYTES {
        return Err(format!("Goal 文档超过 {} 字节", MAX_GOAL_DOCUMENT_BYTES));
    }
    let canonical =
        fs::canonicalize(goal_path).map_err(|error| format!("解析 Goal 文档失败：{error}"))?;
    if !path_contains(&goals_root, &canonical) {
        return Err("Goal 文档路径越出隔离 data/goals 根".to_owned());
    }
    let bytes = fs::read(&canonical).map_err(|error| format!("读取 Goal 文档失败：{error}"))?;
    let byte_count = bytes.len() as u64;
    if byte_count > MAX_GOAL_DOCUMENT_BYTES {
        return Err(format!(
            "Goal 文档读取时超过 {} 字节",
            MAX_GOAL_DOCUMENT_BYTES
        ));
    }
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Goal 文档不是合法 JSON：{error}"))?;
    let schema = document
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "Goal 文档缺少 schema".to_owned())?;
    if schema != GOAL_DOCUMENT_SCHEMA {
        return Err(format!("Goal 文档 schema 不匹配：{schema}"));
    }
    let version = document
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Goal 文档缺少 version".to_owned())?;
    if version != GOAL_DOCUMENT_VERSION {
        return Err(format!(
            "Goal 文档 version 不匹配：期望 {}，实际 {version}",
            GOAL_DOCUMENT_VERSION
        ));
    }
    let mut matched_pointers = Vec::with_capacity(matching.len());
    for (pointer, expected) in matching {
        let actual = document
            .pointer(pointer)
            .ok_or_else(|| format!("Goal 文档缺少 JSON Pointer：{pointer}"))?;
        if actual != expected {
            return Err(format!("Goal 文档 JSON Pointer 不匹配：{pointer}"));
        }
        matched_pointers.push(pointer.clone());
    }

    Ok(json!({
        "path": "data/goals/session-goal-[redacted].json",
        "bytes": byte_count,
        "schema": schema,
        "version": version,
        "matched": true,
        "matchingCount": matching.len(),
        "matchedPointers": matched_pointers,
        "readOnly": true,
    }))
}

#[cfg(windows)]
fn assert_automation_state(
    data_root: &Path,
    title: &str,
    matching: &BTreeMap<String, Value>,
    history_outcomes: &BTreeMap<String, usize>,
) -> Result<Value, String> {
    validate_automation_matching(title, matching, history_outcomes, "assert_automation_state")?;

    let data_metadata = fs::symlink_metadata(data_root)
        .map_err(|error| format!("读取 Automation data 根失败：{error}"))?;
    if data_metadata.file_type().is_symlink() || !data_metadata.is_dir() {
        return Err("Automation data 根必须是非符号链接目录".to_owned());
    }
    let data_root = fs::canonicalize(data_root)
        .map_err(|error| format!("解析 Automation data 根失败：{error}"))?;
    let path = data_root.join("automations.json");
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("读取 Automation 台账失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Automation 台账必须是 data 根内的非符号链接普通文件".to_owned());
    }
    if metadata.len() > MAX_AUTOMATION_DOCUMENT_BYTES {
        return Err(format!(
            "Automation 台账超过 {} 字节",
            MAX_AUTOMATION_DOCUMENT_BYTES
        ));
    }
    let canonical =
        fs::canonicalize(&path).map_err(|error| format!("解析 Automation 台账失败：{error}"))?;
    if !path_contains(&data_root, &canonical) {
        return Err("Automation 台账路径越出隔离 data 根".to_owned());
    }
    let bytes =
        fs::read(&canonical).map_err(|error| format!("读取 Automation 台账失败：{error}"))?;
    let byte_count = bytes.len() as u64;
    if byte_count > MAX_AUTOMATION_DOCUMENT_BYTES {
        return Err(format!(
            "Automation 台账读取时超过 {} 字节",
            MAX_AUTOMATION_DOCUMENT_BYTES
        ));
    }
    let document: Value =
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .map_err(|error| format!("Automation 台账不是合法 JSON：{error}"))?;
    let schema = document
        .get("schema")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Automation 台账缺少数字 schema".to_owned())?;
    if schema != AUTOMATION_DOCUMENT_SCHEMA {
        return Err(format!(
            "Automation 台账 schema 不匹配：期望 {}，实际 {schema}",
            AUTOMATION_DOCUMENT_SCHEMA
        ));
    }
    let automations = document
        .get("automations")
        .and_then(Value::as_array)
        .ok_or_else(|| "Automation 台账缺少有效的 automations 列表".to_owned())?;
    let runs = document
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| "Automation 台账缺少有效的 runs 列表".to_owned())?;
    if automations.iter().any(|value| !value.is_object()) {
        return Err("Automation 台账包含非对象 automations 条目".to_owned());
    }
    if runs.iter().any(|value| !value.is_object()) {
        return Err("Automation 台账包含非对象 runs 条目".to_owned());
    }

    let matches = automations
        .iter()
        .filter(|record| record.get("title").and_then(Value::as_str) == Some(title))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "Automation title 必须唯一匹配一个 record，实际 {} 个",
            matches.len()
        ));
    }
    let record = matches[0];
    let automation_id = record
        .get("automationId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "匹配的 Automation record 缺少有效 automationId".to_owned())?;
    let mut matched_pointers = Vec::with_capacity(matching.len());
    for (pointer, expected) in matching {
        let actual = record
            .pointer(pointer)
            .ok_or_else(|| format!("Automation record 缺少 JSON Pointer：{pointer}"))?;
        if actual != expected {
            return Err(format!("Automation record JSON Pointer 不匹配：{pointer}"));
        }
        matched_pointers.push(pointer.clone());
    }

    let mut observed_outcomes = BTreeMap::from([
        ("scheduled".to_owned(), 0usize),
        ("running".to_owned(), 0usize),
        ("cancelled".to_owned(), 0usize),
        ("completed".to_owned(), 0usize),
    ]);
    for run in runs {
        if run.get("automationId").and_then(Value::as_str) != Some(automation_id) {
            continue;
        }
        let outcome = run
            .get("outcome")
            .and_then(Value::as_str)
            .ok_or_else(|| "匹配的 Automation history 缺少 outcome".to_owned())?;
        if run
            .get("scheduled")
            .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
        {
            return Err("Automation history 的 scheduled 字段类型无效".to_owned());
        }
        if run.get("scheduled").and_then(Value::as_bool) == Some(true) || outcome == "scheduled" {
            let count = observed_outcomes
                .get_mut("scheduled")
                .expect("scheduled 是固定的 history outcome");
            *count = count.saturating_add(1);
        }
        if outcome != "scheduled"
            && let Some(count) = observed_outcomes.get_mut(outcome)
        {
            *count = count.saturating_add(1);
        }
    }
    for (outcome, expected) in history_outcomes {
        let observed = observed_outcomes.get(outcome).copied().unwrap_or_default();
        if observed != *expected {
            return Err(format!(
                "Automation history outcome 数量不匹配：{outcome} 期望 {expected}，实际 {observed}"
            ));
        }
    }

    Ok(json!({
        "path": "data/automations.json",
        "bytes": byte_count,
        "schema": schema,
        "title": title,
        "recordMatched": true,
        "matchingCount": matching.len(),
        "matchedPointers": matched_pointers,
        "historyOutcomeCounts": observed_outcomes,
        "readOnly": true,
    }))
}

#[cfg(windows)]
fn file_scope_name(scope: FileScope) -> &'static str {
    match scope {
        FileScope::Project => "project",
        FileScope::Data => "data",
    }
}

#[cfg(windows)]
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;

#[cfg(windows)]
#[derive(Debug, Clone)]
struct GitWorktreeFact {
    branch: Option<String>,
}

#[cfg(windows)]
fn run_read_only_git(project_root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(project_root)
        // 断言器只读项目状态，移除外部环境对仓库定位的影响，并禁止可选锁写入。
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .output()
        .map_err(|error| format!("启动只读 Git 命令失败：{error}"))?;
    if output.stdout.len() > MAX_GIT_OUTPUT_BYTES || output.stderr.len() > MAX_GIT_OUTPUT_BYTES {
        return Err("只读 Git 命令输出超过限制".to_owned());
    }
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim().chars().take(512).collect::<String>();
        return Err(format!("只读 Git 命令失败：{detail}"));
    }
    String::from_utf8(output.stdout).map_err(|_| "只读 Git 命令输出不是 UTF-8".to_owned())
}

#[cfg(windows)]
fn parse_git_worktrees(raw: &str) -> Vec<GitWorktreeFact> {
    let mut worktrees = Vec::new();
    let mut current = None;
    for line in raw.replace("\r\n", "\n").lines() {
        if line.strip_prefix("worktree ").is_some() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            current = Some(GitWorktreeFact { branch: None });
        } else if let Some(worktree) = current.as_mut()
            && let Some(branch) = line.strip_prefix("branch ")
        {
            worktree.branch = Some(
                branch
                    .trim()
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch.trim())
                    .to_owned(),
            );
        }
    }
    if let Some(worktree) = current {
        worktrees.push(worktree);
    }
    worktrees
}

#[cfg(windows)]
fn normalize_git_match(value: &str) -> String {
    value.replace('/', "\\").to_ascii_lowercase()
}

#[cfg(windows)]
fn assert_isolated_git(
    project_root: &Path,
    require_clean: bool,
    commit_message_contains: Option<&str>,
    expected_worktree_count: Option<usize>,
    worktree_branch_contains: Option<&str>,
) -> Result<Value, String> {
    for (label, value) in [
        ("commit_message_contains", commit_message_contains),
        ("worktree_branch_contains", worktree_branch_contains),
    ] {
        if value.is_some_and(|value| {
            value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
        }) {
            return Err(format!("assert_git 的 {label} 无效"));
        }
    }
    let expected_root =
        fs::canonicalize(project_root).map_err(|error| format!("解析 Git 项目根失败：{error}"))?;
    let reported_root = run_read_only_git(project_root, &["rev-parse", "--show-toplevel"])?;
    let reported_root = fs::canonicalize(reported_root.trim())
        .map_err(|error| format!("解析 Git 报告根失败：{error}"))?;
    if !paths_equal(&reported_root, &expected_root) {
        return Err(format!(
            "Git 根不是当前隔离项目：reported={} expected={}",
            reported_root.display(),
            expected_root.display()
        ));
    }
    let head = run_read_only_git(project_root, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_owned();
    if !matches!(head.len(), 40 | 64) || !head.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err("Git HEAD 不是有效 commit id".to_owned());
    }
    let subject = run_read_only_git(project_root, &["log", "-1", "--format=%s"])?
        .trim()
        .to_owned();
    if let Some(expected) = commit_message_contains
        && !subject.contains(expected)
    {
        return Err(format!(
            "Git HEAD 提交标题不匹配：required={expected} observed={subject}"
        ));
    }
    let status = if require_clean {
        let status = run_read_only_git(
            project_root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        if !status.trim().is_empty() {
            return Err("Git 工作区不是 clean，真实提交结果未完成".to_owned());
        }
        true
    } else {
        false
    };
    let worktree_raw = run_read_only_git(project_root, &["worktree", "list", "--porcelain"])?;
    let worktrees = parse_git_worktrees(&worktree_raw);
    if let Some(expected) = expected_worktree_count
        && worktrees.len() != expected
    {
        return Err(format!(
            "Git worktree 数量不匹配：required={expected} observed={}",
            worktrees.len()
        ));
    }
    let expected_branch = worktree_branch_contains.map(normalize_git_match);
    let matching_worktrees = worktrees
        .iter()
        .filter(|worktree| {
            expected_branch.as_deref().is_none_or(|expected| {
                worktree
                    .branch
                    .as_deref()
                    .is_some_and(|branch| normalize_git_match(branch).contains(expected))
            })
        })
        .count();
    if expected_branch.is_some() && matching_worktrees == 0 {
        return Err("Git worktree 中没有匹配要求分支的 linked worktree".to_owned());
    }
    Ok(json!({
        "repositoryRootMatchesProject": true,
        "head": head,
        "headSubject": subject,
        "cleanChecked": require_clean,
        "clean": require_clean.then_some(status),
        "worktreeCount": worktrees.len(),
        "matchingWorktrees": matching_worktrees,
        "readOnly": true,
    }))
}

fn assert_runtime_evidence(plan: &Plan, evidence: &EvidenceSummary) -> Result<(), String> {
    if plan.requires_journal {
        let journal = &evidence.journal;
        if journal.event_file_count == 0 || journal.event_bytes == 0 {
            return Err("真实验收缺少非空 Session Journal events.jsonl".to_owned());
        }
        if journal.turn_started_count == 0 || journal.terminal_event_count == 0 {
            return Err("Session Journal 缺少本轮新增的真实 TurnStarted 与终态事件".to_owned());
        }
    }
    if plan.requires_network_trace {
        let trace = &evidence.network_trace;
        if !matches!(trace.status, "captured" | "truncated") || trace.bytes == 0 {
            return Err("真实验收缺少非空 Provider network trace".to_owned());
        }
    }
    Ok(())
}

#[cfg(windows)]
fn collect_files(
    root: &Path,
    current: &Path,
    output: &mut Vec<JournalFile>,
    depth: usize,
) -> Result<(), String> {
    if depth > 12 || output.len() >= 2_000 {
        return Ok(());
    }
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound && current != root => {
            // Journal/temporary subdirectories can disappear after enumeration while the
            // Host atomically rotates its files. Treat that child as an empty scan; a
            // missing root remains a real setup/read failure.
            return Ok(());
        }
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_files(root, &path, output, depth + 1)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            output.push(JournalFile {
                relative_path: relative,
                bytes: metadata.len(),
                extension: path
                    .extension()
                    .map(|value| value.to_string_lossy().into_owned()),
            });
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_redacted_trace(path: &Path, redaction_values: &[String]) -> Result<TraceEvidence, String> {
    if !path.exists() {
        let value = json!({
            "schema": "keencode/native-wire-trace",
            "status": "pending",
            "reason": "被测生产运行时没有在 KEENCODE_NATIVE_WIRE_TRACE 写入线级证据",
        });
        write_json(path, &value)?;
        return Ok(TraceEvidence {
            path: path.display().to_string(),
            status: "pending",
            bytes: 0,
            note: "等待生产 Provider 运行时接入脱敏 wire trace；本工具不会读取认证 Header",
        });
    }
    // 只读取上限加一个字节来判断截断，避免异常 Provider trace 让验收器一次性分配
    // 不受控的内存；写回时仍只保留上限以内的脱敏内容。
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_TRACE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let truncated = bytes.len() > MAX_TRACE_BYTES;
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_TRACE_BYTES)]).into_owned();
    for secret in redaction_values {
        if !secret.is_empty() {
            text = text.replace(secret, "[redacted]");
        }
    }
    fs::write(path, text.as_bytes()).map_err(|error| error.to_string())?;
    Ok(TraceEvidence {
        path: path.display().to_string(),
        status: if truncated { "truncated" } else { "captured" },
        bytes: text.len(),
        note: "仅保留被测运行时提供的 JSONL；认证 Header、Key、URL 凭据均在落盘前脱敏",
    })
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("创建 JSON 证据失败 {}: {error}", path.display()))?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

// 按块计算 binary 身份，避免对大型桌面可执行文件建立完整内存副本。
fn stable_sha256(path: &Path) -> Result<String, io::Error> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required_plan() -> Plan {
        Plan {
            version: SCHEMA_VERSION,
            name: Some("test".to_owned()),
            requires_real_provider: true,
            requires_journal: true,
            requires_network_trace: true,
            requires_reuse_isolation: false,
            native_general_settings: NativeGeneralSettings::default(),
            visual: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn visual_plan_and_general_settings_validate_fixed_acceptance_boundary() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "native_general_settings": {
                "appearance": "light",
                "fontFamily": "系统默认",
                "fontSizePx": 14,
                "density": "compact",
                "reducedMotion": true
            },
            "visual": {
                "logicalWidth": 1280,
                "logicalHeight": 820,
                "physicalClientWidth": 2560,
                "physicalClientHeight": 1640,
                "dpi": 192,
                "deviceScaleFactor": 2,
                "theme": "light",
                "sourceBaseline": "out/native-live/zcode-source-baseline-29628c9-dpr2"
            },
            "actions": [{"type": "focus"}]
        }))
        .expect("视觉计划应能解析");
        plan.native_general_settings
            .validate()
            .expect("常规设置 fixture 应满足 NativeHost 约束");
        plan.visual
            .as_ref()
            .expect("视觉计划应包含固定环境")
            .validate()
            .expect("视觉计划应满足固定尺寸和 DPI 约束");
    }

    #[test]
    fn invalid_visual_plan_is_rejected_before_native_run() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "visual": {
                "logicalWidth": 1024,
                "logicalHeight": 768,
                "physicalClientWidth": 1024,
                "physicalClientHeight": 768,
                "dpi": 96,
                "deviceScaleFactor": 1,
                "theme": "dark",
                "sourceBaseline": "baseline"
            },
            "actions": [{"type": "focus"}]
        }))
        .expect("计划结构应能解析");
        let error = plan
            .visual
            .as_ref()
            .expect("测试计划应包含视觉声明")
            .validate()
            .expect_err("非固定视觉环境必须拒绝");
        assert!(error.contains("1280x820"));
    }

    fn evidence(
        event_file_count: usize,
        event_bytes: u64,
        turn_started_count: usize,
        terminal_event_count: usize,
        trace_status: &'static str,
        trace_bytes: usize,
    ) -> EvidenceSummary {
        EvidenceSummary {
            journal: JournalEvidence {
                event_file_count,
                event_bytes,
                turn_started_count,
                terminal_event_count,
                ..JournalEvidence::default()
            },
            network_trace: TraceEvidence {
                status: trace_status,
                bytes: trace_bytes,
                ..TraceEvidence::default()
            },
            ..EvidenceSummary::default()
        }
    }

    #[test]
    fn required_evidence_rejects_screenshot_only_run() {
        let error =
            assert_runtime_evidence(&required_plan(), &evidence(1, 512, 0, 0, "captured", 128))
                .expect_err("缺少 Turn 事件时不能通过真实验收");
        assert!(error.contains("TurnStarted"));
    }

    #[test]
    fn required_evidence_accepts_journal_and_network_trace() {
        assert_runtime_evidence(&required_plan(), &evidence(1, 512, 1, 1, "captured", 128))
            .expect("真实 Journal 和 network trace 应允许通过");
    }

    #[test]
    fn options_parse_supports_explicit_cold_recovery_root() {
        let options = Options::parse(
            [
                "--binary",
                "target/release/keencode-desktop.exe",
                "--reuse-isolation",
                "out/first/native-run/isolation",
                "--output",
                "out/cold",
                "--real-provider",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("冷恢复参数应解析");
        assert_eq!(
            options.reuse_isolation,
            Some(PathBuf::from("out/first/native-run/isolation"))
        );
        assert_eq!(options.output, PathBuf::from("out/cold"));
        assert!(options.binary_args.is_empty());
        assert!(options.real_provider);
    }

    #[test]
    fn options_parse_preserves_repeated_binary_args_verbatim() {
        let options = Options::parse(
            [
                "--binary",
                "target/release/keencode-desktop.exe",
                "--binary-arg",
                r"--user-data-dir=C:\native\zcode-user-data",
                "--binary-arg",
                "--enable-feature=native-acceptance",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("重复的被测进程参数应解析");
        assert_eq!(
            options.binary_args,
            vec![
                r"--user-data-dir=C:\native\zcode-user-data".to_owned(),
                "--enable-feature=native-acceptance".to_owned(),
            ]
        );
    }

    #[test]
    fn options_parse_rejects_binary_arg_without_value() {
        let error = Options::parse(
            [
                "--binary",
                "target/release/keencode-desktop.exe",
                "--binary-arg",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect_err("缺少 --binary-arg 值必须失败");
        assert_eq!(error, "--binary-arg 需要参数");
    }

    #[test]
    fn reuse_isolation_requires_existing_report_and_fixtures() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-reuse-test-{}",
            std::process::id()
        ));
        let isolation = root.join("prior").join("native-run").join("isolation");
        let data = isolation.join("data");
        let project = isolation.join("project");
        std::fs::create_dir_all(project.join("src")).expect("创建冷恢复 fixture 目录");
        std::fs::create_dir_all(&data).expect("创建冷恢复 data 目录");
        std::fs::write(data.join("settings.json"), b"{}\n").expect("写入 settings fixture");
        std::fs::write(
            data.join("native-general-settings.json"),
            r#"{"schema":"keencode/native-general-settings","version":1,"appearance":"dark","fontFamily":"系统默认","fontSizePx":14,"density":"compact","reducedMotion":true}"#.as_bytes(),
        )
        .expect("写入 native general settings fixture");
        std::fs::write(project.join("README.md"), b"fixture\n").expect("写入 README fixture");
        std::fs::write(project.join("src").join("facts.txt"), b"facts\n")
            .expect("写入 facts fixture");
        let isolation = std::fs::canonicalize(&isolation).expect("解析冷恢复 fixture 根");
        let report = isolation.parent().unwrap().join("report.json");
        let value = json!({
            "binary_path": "C:/bin/keencode-desktop.exe",
            "binary_sha256": "a".repeat(64),
            "process": {"isolation_root": isolation.display().to_string()}
        });
        std::fs::write(&report, serde_json::to_vec(&value).unwrap()).expect("写入来源报告");

        let context = validate_reuse_isolation(&isolation).expect("完整隔离根应可复用");
        assert_eq!(context.isolation_root, isolation);
        assert_eq!(context.source_binary.sha256, "a".repeat(64));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plan_supports_authoritative_wait_actions_without_exposing_markers() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "actions": [
                {"type": "wait_for_journal", "event_type": "turn_completed"},
                {"type": "assert_journal_count", "event_type": "turn_completed", "count": 1},
                {"type": "wait_for_file", "path": "out/result.txt", "contains": "marker"}
            ]
        }))
        .expect("等待动作应能解析");
        assert!(matches!(
            plan.actions[0],
            Action::WaitForJournal { count: 1, .. }
        ));
        assert!(matches!(plan.actions[1], Action::AssertJournalCount { .. }));
        assert!(matches!(plan.actions[2], Action::WaitForFile { .. }));
        let result = json!({"path":"out/result.txt","bytes":12,"containsMatched":true});
        assert!(result.get("contains").is_none());
    }

    #[test]
    fn plan_supports_read_only_file_and_git_fact_assertions() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "requires_reuse_isolation": true,
            "actions": [
                {
                    "type": "assert_file",
                    "scope": "data",
                    "path": "settings.json",
                    "contains": "closeToTray",
                    "min_bytes": 1
                },
                {
                    "type": "assert_git",
                    "require_clean": true,
                    "commit_message_contains": "native acceptance commit",
                    "worktree_count": 2,
                    "worktree_branch_contains": "native-acceptance-worktree"
                }
            ]
        }))
        .expect("只读事实断言应能解析");
        assert!(plan.requires_reuse_isolation);
        assert!(matches!(plan.actions[0], Action::AssertFile { .. }));
        assert!(matches!(plan.actions[1], Action::AssertGit { .. }));
    }

    #[test]
    fn plan_supports_exact_json_pointer_file_assertions() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "actions": [{
                "type": "assert_file",
                "scope": "data",
                "path": "providers.json",
                "json_pointer_equals": {
                    "/providers/0/disabledModels": ["deepseek-v4.1-flash"]
                }
            }]
        }))
        .expect("精确 JSON Pointer 文件断言应能解析");
        validate_plan_actions(&plan.actions).expect("精确 JSON Pointer 应通过计划校验");
        let Action::AssertFile {
            json_pointer_equals,
            ..
        } = &plan.actions[0]
        else {
            panic!("应解析为 assert_file");
        };
        let expected = json!(["deepseek-v4.1-flash"]);
        assert_eq!(
            json_pointer_equals.get("/providers/0/disabledModels"),
            Some(&expected)
        );
    }

    #[test]
    fn plan_supports_goal_state_scalar_and_null_matching() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "actions": [{
                "type": "assert_goal_state",
                "matching": {
                    "/goal/status": "active",
                    "/goal/completion_evidence": null,
                    "/goal/progress_percent": 42,
                    "/goal/is_running": true
                }
            }]
        }))
        .expect("Goal 状态断言应能解析");
        validate_plan_actions(&plan.actions).expect("Goal 状态匹配应通过校验");
        assert!(matches!(plan.actions[0], Action::AssertGoalState { .. }));

        let invalid_pointer: Action = serde_json::from_value(json!({
            "type": "assert_goal_state",
            "matching": {"goal/status": "active"}
        }))
        .expect("非法 Pointer 动作结构仍应能解析");
        assert!(validate_plan_actions(&[invalid_pointer]).is_err());

        let invalid_value: Action = serde_json::from_value(json!({
            "type": "assert_goal_state",
            "matching": {"/goal": {"status": "active"}}
        }))
        .expect("非法值类型动作结构仍应能解析");
        assert!(validate_plan_actions(&[invalid_value]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn assert_goal_state_reads_one_document_and_redacts_path() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-goal-state-{}",
            std::process::id()
        ));
        let goals = root.join("data").join("goals");
        std::fs::create_dir_all(&goals).expect("创建 Goal fixture 目录");
        let document = json!({
            "schema": GOAL_DOCUMENT_SCHEMA,
            "version": GOAL_DOCUMENT_VERSION,
            "scope": "session",
            "revision": 1,
            "goal": {
                "status": "active",
                "completion_evidence": null,
                "blocked_reason": null
            },
            "retired_goal_ids": [],
            "operation_receipts": []
        });
        let goal_path = goals.join("session-goal-test.json");
        std::fs::write(
            &goal_path,
            serde_json::to_vec(&document).expect("编码 Goal fixture"),
        )
        .expect("写入 Goal fixture");
        let matching = BTreeMap::from([
            ("/goal/status".to_owned(), json!("active")),
            ("/goal/completion_evidence".to_owned(), Value::Null),
            ("/goal/blocked_reason".to_owned(), Value::Null),
        ]);

        let evidence = assert_goal_state(&root.join("data"), &matching).expect("Goal 断言应通过");
        assert_eq!(evidence["path"], "data/goals/session-goal-[redacted].json");
        assert_eq!(
            evidence["bytes"].as_u64(),
            Some(std::fs::metadata(&goal_path).unwrap().len())
        );
        assert_eq!(evidence["schema"], GOAL_DOCUMENT_SCHEMA);
        assert_eq!(evidence["version"], GOAL_DOCUMENT_VERSION);
        assert_eq!(evidence["matched"], true);
        assert_eq!(evidence["matchingCount"], 3);
        assert_eq!(evidence["readOnly"], true);
        assert!(
            !serde_json::to_string(&evidence)
                .expect("编码证据")
                .contains("session-goal-test")
        );
        let mismatch = BTreeMap::from([("/goal/status".to_owned(), json!("paused"))]);
        let error =
            assert_goal_state(&root.join("data"), &mismatch).expect_err("Goal 状态不匹配必须拒绝");
        assert!(error.contains("不匹配"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn assert_goal_state_rejects_bad_document_and_multiple_candidates() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-goal-invalid-{}",
            std::process::id()
        ));
        let goals = root.join("data").join("goals");
        std::fs::create_dir_all(&goals).expect("创建 Goal fixture 目录");
        let matching = BTreeMap::from([("/goal/status".to_owned(), json!("active"))]);
        let bad_path = goals.join("session-goal-bad.json");
        std::fs::write(
            &bad_path,
            serde_json::to_vec(&json!({
                "schema": "wrong/goal",
                "version": GOAL_DOCUMENT_VERSION,
                "goal": {"status": "active"}
            }))
            .expect("编码错误 schema"),
        )
        .expect("写入错误 schema");
        let error =
            assert_goal_state(&root.join("data"), &matching).expect_err("错误 schema 必须拒绝");
        assert!(error.contains("schema"));

        std::fs::write(
            &bad_path,
            serde_json::to_vec(&json!({
                "schema": GOAL_DOCUMENT_SCHEMA,
                "version": 1,
                "goal": {"status": "active"}
            }))
            .expect("编码错误 version"),
        )
        .expect("写入错误 version");
        let error =
            assert_goal_state(&root.join("data"), &matching).expect_err("错误 version 必须拒绝");
        assert!(error.contains("version"));

        std::fs::write(
            goals.join("session-goal-second.json"),
            serde_json::to_vec(&json!({
                "schema": GOAL_DOCUMENT_SCHEMA,
                "version": GOAL_DOCUMENT_VERSION
            }))
            .expect("编码第二个 Goal"),
        )
        .expect("写入第二个 Goal");
        let error =
            assert_goal_state(&root.join("data"), &matching).expect_err("多个 Goal 文档必须拒绝");
        assert!(error.contains("恰好有一个"));

        std::fs::remove_file(goals.join("session-goal-second.json")).expect("删除第二个 Goal");
        std::fs::write(
            &bad_path,
            vec![b'x'; (MAX_GOAL_DOCUMENT_BYTES + 1) as usize],
        )
        .expect("写入超限 Goal");
        let error =
            assert_goal_state(&root.join("data"), &matching).expect_err("超限 Goal 文档必须拒绝");
        assert!(error.contains("超过"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn assert_goal_state_rejects_goal_directory_symlink_escape() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-goal-symlink-{}",
            std::process::id()
        ));
        let data = root.join("data");
        let outside = root.join("outside-goals");
        std::fs::create_dir_all(&data).expect("创建 data fixture 目录");
        std::fs::create_dir_all(&outside).expect("创建外部 Goal 目录");
        std::fs::write(
            outside.join("session-goal-outside.json"),
            serde_json::to_vec(&json!({
                "schema": GOAL_DOCUMENT_SCHEMA,
                "version": GOAL_DOCUMENT_VERSION,
                "goal": {"status": "active"}
            }))
            .expect("编码外部 Goal"),
        )
        .expect("写入外部 Goal");
        if std::os::windows::fs::symlink_dir(&outside, data.join("goals")).is_err() {
            let _ = std::fs::remove_dir_all(root);
            return;
        }
        let matching = BTreeMap::from([("/goal/status".to_owned(), json!("active"))]);
        let error = assert_goal_state(&data, &matching).expect_err("Goal 目录符号链接越界必须拒绝");
        assert!(error.contains("非符号链接") || error.contains("越出"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plan_supports_automation_state_title_and_history_matching() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "actions": [{
                "type": "assert_automation_state",
                "title": "Native workbench automation",
                "matching": {
                    "/enabled": true,
                    "/dispatchStatus": "idle",
                    "/lastError": null
                },
                "history_outcomes": {
                    "scheduled": 1,
                    "running": 0,
                    "cancelled": 1,
                    "completed": 2
                }
            }]
        }))
        .expect("Automation 状态断言应能解析");
        validate_plan_actions(&plan.actions).expect("Automation 状态匹配应通过校验");
        assert!(matches!(
            plan.actions[0],
            Action::AssertAutomationState { .. }
        ));

        let invalid_outcome: Action = serde_json::from_value(json!({
            "type": "assert_automation_state",
            "title": "Native workbench automation",
            "history_outcomes": {"failed": 1}
        }))
        .expect("非法 outcome 动作结构仍应能解析");
        assert!(validate_plan_actions(&[invalid_outcome]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn assert_automation_state_matches_real_document_and_history_facts() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-automation-state-{}",
            std::process::id()
        ));
        let data = root.join("data");
        std::fs::create_dir_all(&data).expect("创建 Automation fixture 目录");
        let title = "Native workbench automation";
        let automation_id = "automation-native-workbench-1";
        let document = json!({
            "schema": AUTOMATION_DOCUMENT_SCHEMA,
            "automations": [{
                "automationId": automation_id,
                "title": title,
                "prompt": "不要把 prompt 写入断言证据",
                "cronExpr": "0 9 * * 1-5",
                "workspacePath": "local",
                "locationKind": "local",
                "recurring": true,
                "maxRuns": null,
                "runCount": 4,
                "scheduledRunCount": 3,
                "enabled": true,
                "lifecycleStatus": "active",
                "nextRunAt": 4070908800000i64,
                "dispatchStatus": "idle",
                "dispatchAttempts": 4,
                "lastError": null,
                "createdAt": 1,
                "updatedAt": 2
            }],
            "runs": [
                {"runId": "manual-completed", "automationId": automation_id, "outcome": "completed"},
                {"runId": "scheduled-completed", "automationId": automation_id, "outcome": "completed", "scheduled": true},
                {"runId": "scheduled-running", "automationId": automation_id, "outcome": "running", "scheduled": true},
                {"runId": "scheduled-cancelled", "automationId": automation_id, "outcome": "cancelled", "scheduled": true},
                {"runId": "other", "automationId": "other-automation", "outcome": "completed", "scheduled": true}
            ]
        });
        let path = data.join("automations.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&document).expect("编码 Automation fixture"),
        )
        .expect("写入 Automation fixture");
        let matching = BTreeMap::from([
            ("/enabled".to_owned(), json!(true)),
            ("/dispatchStatus".to_owned(), json!("idle")),
            ("/runCount".to_owned(), json!(4)),
            ("/lastError".to_owned(), Value::Null),
        ]);
        let expected_history = BTreeMap::from([
            ("scheduled".to_owned(), 3usize),
            ("running".to_owned(), 1usize),
            ("cancelled".to_owned(), 1usize),
            ("completed".to_owned(), 2usize),
        ]);

        let evidence = assert_automation_state(&data, title, &matching, &expected_history)
            .expect("Automation 状态断言应通过");
        assert_eq!(evidence["path"], "data/automations.json");
        assert_eq!(
            evidence["bytes"].as_u64(),
            Some(std::fs::metadata(&path).unwrap().len())
        );
        assert_eq!(evidence["schema"], AUTOMATION_DOCUMENT_SCHEMA);
        assert_eq!(evidence["title"], title);
        assert_eq!(evidence["recordMatched"], true);
        assert_eq!(evidence["matchingCount"], 4);
        assert_eq!(evidence["historyOutcomeCounts"]["scheduled"], 3);
        assert_eq!(evidence["historyOutcomeCounts"]["running"], 1);
        assert_eq!(evidence["historyOutcomeCounts"]["cancelled"], 1);
        assert_eq!(evidence["historyOutcomeCounts"]["completed"], 2);
        assert_eq!(evidence["readOnly"], true);
        assert!(
            !serde_json::to_string(&evidence)
                .expect("编码断言证据")
                .contains("不要把 prompt 写入断言证据")
        );

        let mismatch = BTreeMap::from([("/dispatchStatus".to_owned(), json!("running"))]);
        let error = assert_automation_state(&data, title, &mismatch, &BTreeMap::new())
            .expect_err("Automation field 不匹配必须拒绝");
        assert!(error.contains("不匹配"));
        let history_mismatch = BTreeMap::from([("completed".to_owned(), 1usize)]);
        let error = assert_automation_state(&data, title, &BTreeMap::new(), &history_mismatch)
            .expect_err("Automation history 数量不匹配必须拒绝");
        assert!(error.contains("数量不匹配"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn assert_automation_state_rejects_schema_duplicates_size_and_symlink() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-automation-invalid-{}",
            std::process::id()
        ));
        let data = root.join("data");
        let outside = root.join("outside");
        std::fs::create_dir_all(&data).expect("创建 Automation data 目录");
        std::fs::create_dir_all(&outside).expect("创建外部目录");
        let title = "Native workbench automation";
        let record = json!({
            "automationId": "automation-1",
            "title": title,
            "prompt": "fixture",
        });
        let base = |schema| {
            json!({
                "schema": schema,
                "automations": [record.clone()],
                "runs": []
            })
        };
        let path = data.join("automations.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&base(99)).expect("编码错误 schema"),
        )
        .expect("写入错误 schema");
        let error = assert_automation_state(&data, title, &BTreeMap::new(), &BTreeMap::new())
            .expect_err("错误 schema 必须拒绝");
        assert!(error.contains("schema"));

        let duplicate = json!({
            "schema": AUTOMATION_DOCUMENT_SCHEMA,
            "automations": [record.clone(), record.clone()],
            "runs": []
        });
        std::fs::write(
            &path,
            serde_json::to_vec(&duplicate).expect("编码重复 title"),
        )
        .expect("写入重复 title");
        let error = assert_automation_state(&data, title, &BTreeMap::new(), &BTreeMap::new())
            .expect_err("重复 title 必须拒绝");
        assert!(error.contains("唯一"));

        std::fs::write(
            &path,
            vec![b'x'; (MAX_AUTOMATION_DOCUMENT_BYTES + 1) as usize],
        )
        .expect("写入超限 Automation 台账");
        let error = assert_automation_state(&data, title, &BTreeMap::new(), &BTreeMap::new())
            .expect_err("超限 Automation 台账必须拒绝");
        assert!(error.contains("超过"));

        std::fs::remove_file(&path).expect("删除本地 Automation 台账");
        let outside_path = outside.join("automations.json");
        std::fs::write(
            &outside_path,
            serde_json::to_vec(&base(AUTOMATION_DOCUMENT_SCHEMA)).unwrap(),
        )
        .expect("写入外部 Automation 台账");
        if std::os::windows::fs::symlink_file(&outside_path, &path).is_err() {
            let _ = std::fs::remove_dir_all(root);
            return;
        }
        let error = assert_automation_state(&data, title, &BTreeMap::new(), &BTreeMap::new())
            .expect_err("Automation 台账符号链接越界必须拒绝");
        assert!(error.contains("非符号链接") || error.contains("越出"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plan_supports_resize_client_action() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "actions": [{"type": "resize_client", "width": 2560, "height": 1640}]
        }))
        .expect("resize_client 动作应能解析");
        assert!(matches!(
            plan.actions.first(),
            Some(Action::ResizeClient {
                width: 2560,
                height: 1640
            })
        ));
    }

    #[test]
    fn plan_supports_scroll_action_and_rejects_unbounded_values() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "actions": [{"type": "scroll", "x": 900, "y": 1300, "delta_y": -1200}]
        }))
        .expect("scroll 动作应能解析");
        validate_plan_actions(&plan.actions).expect("有效 scroll 计划应通过校验");
        assert!(matches!(
            plan.actions.first(),
            Some(Action::Scroll {
                x: 900,
                y: 1300,
                delta_y: -1200
            })
        ));

        let invalid_coordinate = vec![Action::Scroll {
            x: -1,
            y: 100,
            delta_y: -120,
        }];
        assert!(validate_plan_actions(&invalid_coordinate).is_err());
        let invalid_delta = vec![Action::Scroll {
            x: 100,
            y: 100,
            delta_y: MAX_SCROLL_DELTA_Y + 1,
        }];
        assert!(validate_plan_actions(&invalid_delta).is_err());
    }

    #[test]
    fn plan_supports_accessibility_wait_and_click_actions() {
        let plan: Plan = serde_json::from_value(json!({
            "version": 1,
            "actions": [
                {"type": "wait_for_accessibility", "label": "允许", "timeout_ms": 1000},
                {"type": "click_accessibility", "label": "允许"}
            ]
        }))
        .expect("UIA 等待和点击动作应能解析");
        assert!(matches!(
            plan.actions.first(),
            Some(Action::WaitForAccessibility { .. })
        ));
        match plan.actions.get(1) {
            Some(Action::ClickAccessibility { label, timeout_ms }) => {
                assert_eq!(label, "允许");
                assert_eq!(*timeout_ms, None);
            }
            _ => panic!("第二个动作应解析为 UIA 点击"),
        }
    }

    #[test]
    fn accessibility_locator_matches_exact_name_property_only() {
        let tree = json!({
            "rootProperties": {"name": {"value": "KeenCode"}},
            "subtree": {"nodes": [
                {"properties": {"name": {"value": "允许"}}},
                {"properties": {"name": {"value": "允许所有"}}}
            ]}
        });
        assert!(accessibility_tree_contains_name(&tree, "允许"));
        assert!(!accessibility_tree_contains_name(&tree, "允许所有工具"));
        assert!(!accessibility_tree_contains_name(&tree, "Keen"));
    }

    #[test]
    fn accessibility_click_rejects_negative_and_out_of_bounds_points() {
        assert!(validate_client_point(0, 0, 100, 100).is_ok());
        assert!(validate_client_point(-1, 0, 100, 100).is_err());
        assert!(validate_client_point(100, 0, 100, 100).is_err());
        assert!(validate_client_point(0, 100, 100, 100).is_err());
    }

    #[test]
    fn accessibility_target_requires_unique_enabled_bounds() {
        let tree = json!({
            "subtree": {
                "nodes": [
                    {
                        "index": 7,
                        "properties": {
                            "name": {"value": "允许"},
                            "controlType": {"value": 50000},
                            "isEnabled": {"value": true},
                            "isOffscreen": {"value": false},
                            "boundingRectangle": {"value": {"left": 10, "top": 20, "right": 110, "bottom": 70}}
                        }
                    },
                    {
                        "index": 8,
                        "properties": {
                            "name": {"value": "允许"},
                            "controlType": {"value": 50000},
                            "isEnabled": {"value": false},
                            "isOffscreen": {"value": false},
                            "boundingRectangle": {"value": {"left": 10, "top": 80, "right": 110, "bottom": 130}}
                        }
                    }
                ],
                "nodesTruncated": false
            }
        });
        assert_eq!(
            accessibility_target_from_tree(&tree, "允许"),
            Ok(AccessibilityTarget {
                index: 7,
                left: 10,
                top: 20,
                right: 110,
                bottom: 70,
            })
        );

        let offscreen = json!({
            "subtree": {"nodes": [
                {"index": 12, "properties": {"name": {"value": "下方按钮"}, "controlType": {"value": 50000}, "isEnabled": {"value": true}, "isOffscreen": {"value": true}, "boundingRectangle": {"value": {"left": 10, "top": 900, "right": 110, "bottom": 950}}}}
            ]}
        });
        assert_eq!(
            accessibility_target_from_tree(&offscreen, "下方按钮")
                .expect("带有效 bounds 的 offscreen 控件应交给自动滚动定位"),
            AccessibilityTarget {
                index: 12,
                left: 10,
                top: 900,
                right: 110,
                bottom: 950,
            }
        );

        let duplicate = json!({
            "subtree": {"nodes": [
                {"index": 1, "properties": {"name": {"value": "允许"}, "controlType": {"value": 50000}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 0, "top": 0, "right": 10, "bottom": 10}}}},
                {"index": 2, "properties": {"name": {"value": "允许"}, "controlType": {"value": 50004}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 10, "top": 0, "right": 20, "bottom": 10}}}}
            ]}
        });
        assert!(accessibility_target_from_tree(&duplicate, "允许").is_err());
    }

    #[test]
    fn accessibility_target_requires_real_interactive_role() {
        let label_and_switch = json!({
            "subtree": {"nodes": [
                {"index": 1, "properties": {"name": {"value": "启用功能"}, "controlType": {"value": 50020}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 0, "top": 0, "right": 10, "bottom": 10}}}},
                {"index": 2, "properties": {"name": {"value": "启用功能"}, "controlType": {"value": 50002}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 10, "top": 0, "right": 20, "bottom": 10}}}}
            ]}
        });
        assert_eq!(
            accessibility_target_from_tree(&label_and_switch, "启用功能")
                .expect("静态 Label 与 Switch 同名时应定位 Switch")
                .index,
            2
        );

        let label_and_input = json!({
            "subtree": {"nodes": [
                {"index": 3, "properties": {"name": {"value": "项目目录"}, "controlType": {"value": 50020}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 0, "top": 20, "right": 10, "bottom": 30}}}},
                {"index": 4, "properties": {"name": {"value": "项目目录"}, "controlType": {"value": 50004}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 10, "top": 20, "right": 20, "bottom": 30}}}}
            ]}
        });
        assert_eq!(
            accessibility_target_from_tree(&label_and_input, "项目目录")
                .expect("静态 Label 与 Input 同名时应定位 Input")
                .index,
            4
        );

        let two_real_controls = json!({
            "subtree": {"nodes": [
                {"index": 5, "properties": {"name": {"value": "重复控件"}, "controlType": {"value": 50002}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 0, "top": 40, "right": 10, "bottom": 50}}}},
                {"index": 6, "properties": {"name": {"value": "重复控件"}, "controlType": {"value": 50004}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 10, "top": 40, "right": 20, "bottom": 50}}}}
            ]}
        });
        assert!(accessibility_target_from_tree(&two_real_controls, "重复控件").is_err());

        let static_label_only = json!({
            "subtree": {"nodes": [
                {"index": 7, "properties": {"name": {"value": "仅文本"}, "controlType": {"value": 50020}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 0, "top": 60, "right": 10, "bottom": 70}}}}
            ]}
        });
        assert!(accessibility_target_from_tree(&static_label_only, "仅文本").is_err());

        let missing_role = json!({
            "subtree": {"nodes": [
                {"index": 8, "properties": {"name": {"value": "缺少角色"}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 0, "top": 80, "right": 10, "bottom": 90}}}}
            ]}
        });
        assert!(accessibility_target_from_tree(&missing_role, "缺少角色").is_err());

        let unknown_role = json!({
            "subtree": {"nodes": [
                {"index": 9, "properties": {"name": {"value": "未知角色"}, "controlType": {"value": 59999}, "isEnabled": {"value": true}, "isOffscreen": {"value": false}, "boundingRectangle": {"value": {"left": 0, "top": 100, "right": 10, "bottom": 110}}}}
            ]}
        });
        assert!(accessibility_target_from_tree(&unknown_role, "未知角色").is_err());
    }

    #[test]
    fn accessibility_click_decision_uses_intersection_and_scroll_direction() {
        assert_eq!(
            accessibility_click_decision(
                AccessibilityClientRect {
                    left: -20,
                    top: 10,
                    right: 40,
                    bottom: 30,
                },
                100,
                100,
            ),
            Ok(AccessibilityClickDecision::Click { x: 20, y: 20 })
        );
        assert_eq!(
            accessibility_click_decision(
                AccessibilityClientRect {
                    left: -20,
                    top: 90,
                    right: 40,
                    bottom: 130,
                },
                100,
                100,
            ),
            Ok(AccessibilityClickDecision::Scroll {
                x: 20,
                y: 50,
                delta_y: -ACCESSIBILITY_AUTO_SCROLL_DELTA_Y,
            })
        );
        assert_eq!(
            accessibility_click_decision(
                AccessibilityClientRect {
                    left: 10,
                    top: -70,
                    right: 40,
                    bottom: -30,
                },
                100,
                100,
            ),
            Ok(AccessibilityClickDecision::Scroll {
                x: 25,
                y: 50,
                delta_y: ACCESSIBILITY_AUTO_SCROLL_DELTA_Y,
            })
        );
        assert!(
            accessibility_click_decision(
                AccessibilityClientRect {
                    left: 100,
                    top: 10,
                    right: 120,
                    bottom: 30,
                },
                100,
                100,
            )
            .expect_err("完全水平越界必须拒绝")
            .contains("水平交集")
        );
    }

    #[cfg(windows)]
    #[test]
    fn read_only_git_worktree_parser_keeps_branch_facts() {
        let worktrees = parse_git_worktrees(
            "worktree C:\\isolated\\project\nHEAD abc\nbranch refs/heads/main\n\nworktree C:\\isolated\\project-worktree-1\nHEAD def\nbranch refs/heads/native-acceptance-worktree\n",
        );
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(
            worktrees[1].branch.as_deref(),
            Some("native-acceptance-worktree")
        );
    }

    #[cfg(windows)]
    #[test]
    fn journal_scan_ignores_missing_child_but_reports_missing_root() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-journal-scan-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("创建 Journal scan fixture 目录");
        let missing_child = root.join("journal-tmp");
        let mut files = Vec::new();
        assert!(collect_files(&root, &missing_child, &mut files, 1).is_ok());

        std::fs::remove_dir_all(&root).expect("删除 Journal scan fixture 根目录");
        let error = collect_files(&root, &root, &mut files, 0)
            .expect_err("缺失 Journal 根目录必须报告读取错误");
        assert!(!error.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn journal_action_parses_matching_session_and_turn_filters() {
        let action: Action = serde_json::from_value(json!({
            "type": "wait_for_journal",
            "event_type": "tool_requested",
            "count": 1,
            "matching": {
                "/payload/request/toolName": "Read",
                "/payload/request/effect": "read_only"
            },
            "matching_text_contains": {
                "/payload/request/arguments/description": "marker"
            },
            "matching_project_paths": {
                "/payload/request/arguments/file_path": "src/facts.txt"
            },
            "session_id": "session-a",
            "turn_id": "turn-a"
        }))
        .expect("Journal 查询动作应能解析");
        match action {
            Action::WaitForJournal {
                matching,
                matching_text_contains,
                matching_project_paths,
                session_id,
                turn_id,
                ..
            } => {
                assert_eq!(
                    matching.get("/payload/request/toolName"),
                    Some(&json!("Read"))
                );
                assert_eq!(
                    matching.get("/payload/request/effect"),
                    Some(&json!("read_only"))
                );
                assert_eq!(
                    matching_text_contains.get("/payload/request/arguments/description"),
                    Some(&"marker".to_owned())
                );
                assert_eq!(
                    matching_project_paths.get("/payload/request/arguments/file_path"),
                    Some(&"src/facts.txt".to_owned())
                );
                assert_eq!(session_id.as_deref(), Some("session-a"));
                assert_eq!(turn_id.as_deref(), Some("turn-a"));
            }
            _ => panic!("应解析为 wait_for_journal"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn journal_action_rejects_unknown_matching_project_path_field() {
        let error = serde_json::from_value::<Action>(json!({
            "type": "wait_for_journal",
            "event_type": "tool_requested",
            "matching_project_path": {
                "/payload/request/arguments/file_path": "src/facts.txt"
            }
        }))
        .expect_err("拼错的路径匹配字段必须被拒绝");
        assert!(error.to_string().contains("matching_project_path"));
    }

    #[cfg(windows)]
    #[test]
    fn all_formal_native_plans_parse_actions_without_unknown_fields() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for entry in std::fs::read_dir(root).expect("读取原生计划目录") {
            let path = entry.expect("读取计划目录项").path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path).expect("读取正式计划");
            let _plan: Plan = serde_json::from_slice(&bytes)
                .unwrap_or_else(|error| panic!("正式计划解析失败 {}: {error}", path.display()));
        }
    }

    #[cfg(windows)]
    #[test]
    fn journal_query_matches_atomic_batch_events_and_excludes_other_sessions_and_turns() {
        let matching = BTreeMap::from([
            ("/payload/request/toolName".to_owned(), json!("Read")),
            (
                "/payload/request/arguments/file_path".to_owned(),
                json!("src/facts.txt"),
            ),
            ("/payload/request/effect".to_owned(), json!("read_only")),
        ]);
        let matching_text_contains = BTreeMap::from([(
            "/payload/request/arguments/description".to_owned(),
            "marker".to_owned(),
        )]);
        let query = JournalQuery::new(
            "tool_requested",
            &matching,
            &matching_text_contains,
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            Some("session-a"),
            Some("turn-a"),
        );
        query.validate().expect("有效 Journal 查询应通过校验");
        let physical_record = json!({
            "type": "atomic_batch",
            "session": "session-a",
            "payload": {
                "events": [
                    {
                        "type": "tool_requested",
                        "payload": {
                            "request": {
                                "turnId": "turn-a",
                                "toolName": "Read",
                                "arguments": {
                                    "file_path": "src/facts.txt",
                                    "description": "target marker"
                                },
                                "effect": "read_only"
                            }
                        }
                    },
                    {
                        "type": "tool_requested",
                        "payload": {
                            "request": {
                                "turnId": "turn-b",
                                "toolName": "Read",
                                "arguments": {
                                    "file_path": "src/facts.txt",
                                    "description": "other marker"
                                },
                                "effect": "read_only"
                            }
                        }
                    },
                    {
                        "type": "tool_requested",
                        "session": "session-b",
                        "payload": {
                            "request": {
                                "turnId": "turn-a",
                                "toolName": "Read",
                                "arguments": {
                                    "file_path": "src/facts.txt",
                                    "description": "other session marker"
                                },
                                "effect": "read_only"
                            }
                        }
                    }
                ]
            }
        });
        let mut matched = 0usize;
        visit_journal_event_value(&physical_record, None, &mut |event, session_id| {
            if query.matches(event, session_id) {
                matched += 1;
            }
        });
        assert_eq!(matched, 1);

        let missing_pointer = BTreeMap::from([(
            "/payload/request/arguments/missing".to_owned(),
            json!("value"),
        )]);
        let missing_query = JournalQuery::new(
            "tool_requested",
            &missing_pointer,
            &BTreeMap::new(),
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            Some("session-a"),
            Some("turn-a"),
        );
        assert!(
            !missing_query.matches(&physical_record["payload"]["events"][0], Some("session-a"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn journal_query_matches_project_relative_and_absolute_paths_strictly() {
        let matching = BTreeMap::from([
            ("/payload/request/toolName".to_owned(), json!("Read")),
            ("/payload/request/effect".to_owned(), json!("read_only")),
        ]);
        let matching_project_paths = BTreeMap::from([(
            "/payload/request/arguments/file_path".to_owned(),
            "src/facts.txt".to_owned(),
        )]);
        let project_root = Path::new(r"D:\isolated\project");
        let query = JournalQuery::new(
            "tool_requested",
            &matching,
            &BTreeMap::new(),
            &matching_project_paths,
            project_root,
            Some("session-a"),
            Some("turn-a"),
        );
        query.validate().expect("项目路径匹配应通过校验");

        for path in [
            "src/facts.txt",
            r"src\facts.txt",
            r"D:\isolated\project\src\facts.txt",
            r"\\?\D:\isolated\project\src\facts.txt",
        ] {
            let event = json!({
                "type": "tool_requested",
                "payload": {
                    "request": {
                        "turnId": "turn-a",
                        "toolName": "Read",
                        "arguments": {"file_path": path},
                        "effect": "read_only"
                    }
                }
            });
            assert!(
                query.matches(&event, Some("session-a")),
                "应匹配同一项目目标：{path}"
            );
        }

        for path in [
            "src/other.txt",
            r"D:\other\project\src\facts.txt",
            r"D:\isolated\project\..\other\facts.txt",
            r"..\facts.txt",
        ] {
            let event = json!({
                "type": "tool_requested",
                "payload": {
                    "request": {
                        "turnId": "turn-a",
                        "toolName": "Read",
                        "arguments": {"file_path": path},
                        "effect": "read_only"
                    }
                }
            });
            assert!(
                !query.matches(&event, Some("session-a")),
                "不应匹配其它文件、root 或逃逸路径：{path}"
            );
        }

        let escaped = BTreeMap::from([(
            "/payload/request/arguments/file_path".to_owned(),
            r"..\facts.txt".to_owned(),
        )]);
        let invalid_query = JournalQuery::new(
            "tool_requested",
            &matching,
            &BTreeMap::new(),
            &escaped,
            project_root,
            Some("session-a"),
            Some("turn-a"),
        );
        assert!(
            invalid_query.validate().is_err(),
            "计划期望路径不能越出项目根"
        );
    }

    #[cfg(windows)]
    #[test]
    fn journal_query_baseline_uses_same_project_path_key_for_cold_resume() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-journal-path-baseline-{}",
            std::process::id()
        ));
        let data_root = root.join("data");
        let project_root = root.join("project");
        let events_path = data_root
            .join("projects")
            .join("project-1")
            .join("session-1")
            .join("events.jsonl");
        std::fs::create_dir_all(events_path.parent().expect("事件目录")).expect("创建事件目录");
        std::fs::create_dir_all(&project_root).expect("创建项目根");
        let absolute_fact_path = project_root.join("src").join("facts.txt");
        let event = json!({
            "type": "tool_requested",
            "session": "session-a",
            "payload": {
                "request": {
                    "turnId": "turn-a",
                    "toolName": "Read",
                    "arguments": {
                        "file_path": absolute_fact_path.display().to_string(),
                        "description": "baseline-marker-existing"
                    },
                    "effect": "read_only"
                }
            }
        });
        // Journal 每条物理记录都以换行结束；缺失换行会把后续追加拼成非法 JSON。
        std::fs::write(
            &events_path,
            format!("{}\n", serde_json::to_string(&event).expect("编码事件")),
        )
        .expect("写入事件");

        let action: Action = serde_json::from_value(json!({
            "type": "wait_for_journal",
            "event_type": "tool_requested",
            "count": 1,
            "matching": {
                "/payload/request/toolName": "Read",
                "/payload/request/effect": "read_only"
            },
            "matching_text_contains": {
                "/payload/request/arguments/description": "baseline-marker"
            },
            "matching_project_paths": {
                "/payload/request/arguments/file_path": "src/facts.txt"
            },
            "session_id": "session-a",
            "turn_id": "turn-a"
        }))
        .expect("路径匹配动作应能解析");
        let baseline =
            capture_journal_baseline(&data_root, &project_root, std::slice::from_ref(&action))
                .expect("应能捕获项目路径查询 baseline");
        let queries = collect_journal_queries(&project_root, std::slice::from_ref(&action))
            .expect("应能构造项目路径查询");
        let query = queries.values().next().expect("应存在路径查询");
        assert_eq!(
            count_journal_events(&data_root, query).expect("扫描 baseline"),
            1
        );

        let mut append = OpenOptions::new()
            .append(true)
            .open(&events_path)
            .expect("打开事件文件追加第二轮");
        writeln!(
            append,
            "{}",
            serde_json::to_string(&event).expect("编码第二轮事件")
        )
        .expect("追加第二轮事件");
        assert_eq!(
            count_journal_events(&data_root, query).expect("扫描追加后的完整记录"),
            2
        );
        assert_eq!(
            count_new_journal_events(&data_root, query, &baseline)
                .expect("冷恢复查询应只计新增事件"),
            1
        );
        let unfiltered_query = JournalQuery::new(
            "tool_requested",
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &project_root,
            None,
            None,
        );
        assert_eq!(
            count_new_journal_events(&data_root, &unfiltered_query, &baseline)
                .expect("无过滤器查询应使用事件类型 baseline"),
            1
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_query_matches_scalar_string_boolean_and_number_values_exactly() {
        let matching = BTreeMap::from([
            ("/payload/name".to_owned(), json!("sample")),
            ("/payload/enabled".to_owned(), json!(true)),
            ("/payload/count".to_owned(), json!(2)),
        ]);
        let query = JournalQuery::new(
            "sample",
            &matching,
            &BTreeMap::new(),
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            None,
            None,
        );
        query.validate().expect("标量 Journal 查询应通过校验");
        assert!(query.matches(
            &json!({
                "type": "sample",
                "payload": {"name": "sample", "enabled": true, "count": 2}
            }),
            None
        ));
        assert!(!query.matches(
            &json!({
                "type": "sample",
                "payload": {"name": "sample", "enabled": true, "count": 3}
            }),
            None
        ));
        assert!(!query.matches(
            &json!({
                "type": "sample",
                "payload": {"name": "sample suffix", "enabled": true, "count": 2}
            }),
            None
        ));
    }

    #[cfg(windows)]
    #[test]
    fn journal_query_text_contains_requires_non_empty_string_values() {
        let matching_text_contains =
            BTreeMap::from([("/payload/text".to_owned(), "sample".to_owned())]);
        let query = JournalQuery::new(
            "sample",
            &BTreeMap::new(),
            &matching_text_contains,
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            None,
            None,
        );
        query.validate().expect("文本子串 Journal 查询应通过校验");
        let matching_event = json!({
            "type": "sample",
            "payload": {"text": "sample suffix"}
        });
        assert!(query.matches(&matching_event, None));
        assert!(!query.matches(
            &json!({"type": "sample", "payload": {"missing": "sample suffix"}}),
            None
        ));
        for value in [json!(true), json!(42), Value::Null] {
            assert!(!query.matches(&json!({"type": "sample", "payload": {"text": value}}), None));
        }

        let empty = BTreeMap::from([("/payload/text".to_owned(), String::new())]);
        let empty_query = JournalQuery::new(
            "sample",
            &BTreeMap::new(),
            &empty,
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            None,
            None,
        );
        assert!(empty_query.validate().is_err(), "空子串必须被拒绝");
        assert!(
            !empty_query.matches(&matching_event, None),
            "空子串不得匹配事件"
        );

        let too_long = BTreeMap::from([(
            "/payload/text".to_owned(),
            "x".repeat(MAX_JOURNAL_TEXT_CONTAINS_BYTES + 1),
        )]);
        let too_long_query = JournalQuery::new(
            "sample",
            &BTreeMap::new(),
            &too_long,
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            None,
            None,
        );
        assert!(too_long_query.validate().is_err(), "过长子串必须被拒绝");

        let invalid_pointer = BTreeMap::from([("/payload/~2text".to_owned(), "sample".to_owned())]);
        let invalid_pointer_query = JournalQuery::new(
            "sample",
            &BTreeMap::new(),
            &invalid_pointer,
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            None,
            None,
        );
        assert!(
            invalid_pointer_query.validate().is_err(),
            "无效 JSON Pointer 必须被拒绝"
        );
    }

    #[cfg(windows)]
    #[test]
    fn journal_query_cache_key_includes_text_contains_filter() {
        let without_text = JournalQuery::new(
            "sample",
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            None,
            None,
        );
        let matching_text_contains =
            BTreeMap::from([("/payload/text".to_owned(), "sample".to_owned())]);
        let with_text = JournalQuery::new(
            "sample",
            &BTreeMap::new(),
            &matching_text_contains,
            &BTreeMap::new(),
            Path::new(r"D:\isolated\project"),
            None,
            None,
        );
        assert_ne!(
            without_text.cache_key().expect("无过滤器查询应可编码"),
            with_text.cache_key().expect("文本过滤查询应可编码")
        );
        assert!(with_text.has_filters());
    }

    #[cfg(windows)]
    #[test]
    fn process_output_capture_cancels_inherited_pipe_and_joins_reader() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-pipe-fixture-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("创建 pipe fixture 目录");
        let source = root.join("fixture.rs");
        let binary = root.join("fixture.exe");
        std::fs::write(
            &source,
            r#"
use std::{env, process::{Command, Stdio}, thread, time::Duration};

fn main() {
    match env::args().nth(1).as_deref() {
        Some("normal") => {
            println!("normal-fixture");
        }
        Some("parent") => {
            Command::new(env::current_exe().unwrap())
                .arg("grandchild")
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
        }
        Some("grandchild") => {
            println!("pipe-fixture");
            thread::sleep(Duration::from_secs(2));
        }
        _ => {}
    }
}
"#,
        )
        .expect("写入 Rust pipe fixture");
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let compile = Command::new(compiler)
            .args([source.as_os_str(), std::ffi::OsStr::new("-O")])
            .arg("-o")
            .arg(&binary)
            .status()
            .expect("启动 Rust pipe fixture 编译器");
        assert!(compile.success(), "Rust pipe fixture 编译失败");

        let mut normal_child = Command::new(&binary)
            .arg("normal")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动正常退出的 Rust pipe fixture");
        let normal_capture =
            ProcessOutputCapture::start(&mut normal_child).expect("建立正常 pipe 读取器");
        normal_child.wait().expect("等待正常 Rust pipe fixture");
        let normal_output = normal_capture.finish(Duration::from_secs(1));
        assert!(!normal_output.stdout.timed_out);
        assert_eq!(normal_output.stdout.bytes, b"normal-fixture\n");

        let mut child = Command::new(&binary)
            .arg("parent")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 Rust pipe fixture 父进程");
        let capture = ProcessOutputCapture::start(&mut child).expect("建立 pipe 读取器");
        child.wait().expect("等待 Rust pipe fixture 父进程");
        let started = Instant::now();
        let output = capture.finish(Duration::from_millis(200));
        let capture_elapsed = started.elapsed();
        // 等待 fixture 的后代自然退出，避免测试把临时二进制留在 Windows 锁定状态。
        std::thread::sleep(Duration::from_millis(2_500));
        let _ = std::fs::remove_dir_all(root);

        assert!(capture_elapsed < Duration::from_secs(1));
        assert!(
            output.stdout.timed_out || output.stderr.timed_out,
            "后代仍持有 pipe 写端时至少一路读取必须经过取消"
        );
    }

    #[cfg(windows)]
    #[test]
    fn process_output_capture_is_bounded_and_redacted() {
        let mut input = vec![b'x'; MAX_PROCESS_OUTPUT_BYTES + 1];
        input.extend_from_slice(
            b"\nconnecting to https://private.example/v1\napiKey=sk-private-key\n",
        );
        let mut capture = CapturedProcessStream::default();
        append_process_stream_bytes(&mut capture, &input);
        assert_eq!(capture.bytes.len(), MAX_PROCESS_OUTPUT_BYTES);
        assert!(capture.truncated);

        let text = redact_process_output(
            &CapturedProcessStream {
                bytes: b"connecting to https://private.example/v1\napiKey=sk-private-key\n"
                    .to_vec(),
                ..capture
            },
            &["sk-private-key".to_owned()],
        );
        assert!(!text.contains("private.example"));
        assert!(!text.contains("sk-private-key"));
        assert!(text.contains("[redacted-url]"));
        assert!(text.contains("[redacted-sensitive-line]"));
    }

    #[cfg(windows)]
    #[test]
    fn process_output_evidence_writes_redacted_streams_and_exit_error() {
        let root = std::env::temp_dir().join(format!(
            "keencode-native-gpui-process-output-evidence-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("创建进程输出证据目录");
        let output = CapturedProcessOutput {
            stdout: CapturedProcessStream {
                bytes: b"stdout secret-value\n".to_vec(),
                truncated: true,
                ..CapturedProcessStream::default()
            },
            stderr: CapturedProcessStream {
                bytes: b"stderr https://private.example/error\n".to_vec(),
                read_error: true,
                ..CapturedProcessStream::default()
            },
        };
        let path = write_process_output_evidence(
            &root,
            Some(1),
            "natural",
            Some("process secret-value failed"),
            &output,
            &["secret-value".to_owned()],
        )
        .expect("写入进程输出证据");
        let value: Value = serde_json::from_slice(&std::fs::read(&path).expect("读取证据"))
            .expect("解析进程输出证据");

        assert_eq!(value["status"], "captured");
        assert_eq!(value["exitCode"], 1);
        assert_eq!(value["termination"], "natural");
        assert_eq!(value["error"], "process [redacted] failed");
        assert_eq!(value["stdout"]["capturedBytes"], 20);
        assert_eq!(value["stdout"]["truncated"], true);
        assert!(
            !value["stdout"]["text"]
                .as_str()
                .expect("stdout 文本")
                .contains("secret-value")
        );
        assert_eq!(value["stderr"]["readError"], true);
        assert!(
            !value["stderr"]["text"]
                .as_str()
                .expect("stderr 文本")
                .contains("private.example")
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
