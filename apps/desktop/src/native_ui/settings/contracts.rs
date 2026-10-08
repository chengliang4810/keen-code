//! GPUI 设置面板与 NativeHost 之间的稳定 typed contract。
//!
//! 这个模块不依赖 Tauri、前端序列化协议或文件路径。父级 NativeHost 将这里的
//! command 映射到现有 Rust domain；面板只保留短生命周期的投影，真实状态仍由
//! provider、extensions、memory、workflow 和 runtime 服务持有。

use crate::analytics::RequestRecordsQuery;
use crate::app_settings::{AppUpdateDownloadSource, PreferencesPatch, TerminalShell};
use crate::app_updates::AppUpdateStatus;
use crate::native_insights::{DiagnosticsSnapshot, UsageSnapshot};
use crate::workflows::{
    DynamicWorkflowRunProgressPayload, WorkflowArtifact, WorkflowArtifactBytes,
    WorkflowArtifactPage, WorkflowEventPage, WorkflowGraphResult, WorkflowNodeResult,
    WorkflowWorkspaceResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// NativeHost 异步操作的统一返回类型，避免把 tokio 类型泄漏到 GPUI 面板。
pub type SettingsFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// 设置服务错误只携带可展示摘要；凭据、原始响应和用户内容不得进入错误对象。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSettingsError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl NativeSettingsError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

impl std::fmt::Display for NativeSettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for NativeSettingsError {}

pub type SettingsResult<T> = Result<T, NativeSettingsError>;

/// 设置左侧导航页。文档、媒体、HTML、浏览器和 Web/Mobile 页面不属于此集合。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SettingsPage {
    #[default]
    General,
    Appearance,
    Keyboard,
    Providers,
    Hooks,
    Resources,
    Automations,
    Workflows,
    Agents,
    Usage,
    Diagnostics,
}

impl SettingsPage {
    pub const ALL: [Self; 11] = [
        Self::General,
        Self::Appearance,
        Self::Keyboard,
        Self::Providers,
        Self::Hooks,
        Self::Resources,
        Self::Automations,
        Self::Workflows,
        Self::Agents,
        Self::Usage,
        Self::Diagnostics,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::General => "常规",
            Self::Appearance => "外观",
            Self::Keyboard => "键盘",
            Self::Providers => "模型与供应商",
            Self::Hooks => "Hooks",
            Self::Resources => "资源",
            Self::Automations => "自动化",
            Self::Workflows => "已保存工作流",
            Self::Agents => "智能体与子智能体",
            Self::Usage => "用量",
            Self::Diagnostics => "诊断",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::General => "终端、行为、自定义指令与更新",
            Self::Appearance => "主题、字号、代码、字体、密度与动效",
            Self::Keyboard => "Composer 和全局快捷键",
            Self::Providers => "供应商、模型目录、视觉和推理档位",
            Self::Hooks => "查看和管理本地 Hook 声明",
            Self::Resources => "插件、MCP、技能、记忆和模板",
            Self::Automations => "定时任务、立即执行和运行历史",
            Self::Workflows => "编辑、执行、产物和运行记录",
            Self::Agents => "目标、子智能体和 agent 模板",
            Self::Usage => "模型请求、Token 用量和任务缓存",
            Self::Diagnostics => "进程资源、启动阶段和脱敏诊断",
        }
    }
}

/// NativeHost 的事件源。窗口销毁时必须调用 [`NativeSettingsSubscription::close`]。
pub trait NativeSettingsSubscription: Send + Sync {
    fn next(&self) -> SettingsFuture<Option<SettingsEvent>>;
    fn close(&self);
}

/// 设置页唯一依赖的 typed service。实现方应直接复用现有 Rust domain，不能在
/// UI 层读取 providers.json、workflow 目录或 memory 文件来制造第二份事实源。
pub trait NativeSettingsService: Send + Sync {
    /// 只加载当前可见页，支持设置页懒加载和页面级缓存失效。
    fn load(&self, page: SettingsPage) -> SettingsFuture<SettingsResult<SettingsSnapshot>>;
    /// 执行一条有明确输入的命令，并返回更新后的页面投影。
    fn execute(&self, command: SettingsCommand)
    -> SettingsFuture<SettingsResult<SettingsSnapshot>>;
    /// 订阅 Rust domain 的变更；UI 只消费事件，不轮询全量目录。
    fn subscribe(&self) -> SettingsResult<Arc<dyn NativeSettingsSubscription>>;
}

/// NativeHost 在设置事实发生变化时发送的最小事件。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SettingsEvent {
    Invalidate(SettingsPage),
    Snapshot(Box<SettingsSnapshot>),
    Notice(SettingsNotice),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettingsNotice {
    pub level: NoticeLevel,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// 一次页面读取的 projection。None 表示该页未被加载，绝不以空数组伪造成功。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub page: SettingsPage,
    pub revision: u64,
    pub general: Option<GeneralSettings>,
    pub keyboard: Option<KeyboardSettings>,
    pub providers: Option<ProviderSettings>,
    pub hooks: Option<HooksSettings>,
    pub resources: Option<ResourceSettings>,
    pub automations: Option<AutomationSettings>,
    pub workflows: Option<WorkflowSettings>,
    pub agents: Option<AgentSettings>,
    pub usage: Option<UsageSettings>,
    pub diagnostics: Option<DiagnosticsSettings>,
    #[serde(default)]
    pub notices: Vec<SettingsNotice>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiDensity {
    #[default]
    Compact,
    Standard,
    Comfortable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeSyntaxTheme {
    #[default]
    #[serde(rename = "github-light")]
    GitHubLight,
    #[serde(rename = "github-dark")]
    GitHubDark,
    #[serde(rename = "ely")]
    Ely,
    #[serde(rename = "quiet")]
    Quiet,
    #[serde(rename = "paper")]
    Paper,
}

/// 代码编辑器和 Diff 共享的已确认显示设置；主题色由运行时按当前模式解析。
/// 嵌套在 `GeneralFile` v2 时使用 camelCase，并拒绝未知字段，和原生验收 runner 的
/// 隔离配置保持同一份严格持久化合同。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CodeAppearanceSettings {
    pub font_family: String,
    pub font_size_px: f32,
    pub show_line_numbers: bool,
    pub wrap_long_lines: bool,
    pub light_theme: CodeSyntaxTheme,
    pub dark_theme: CodeSyntaxTheme,
}

impl Default for CodeAppearanceSettings {
    fn default() -> Self {
        Self {
            font_family: crate::native_ui::style::DEFAULT_MONO_FONT_FAMILY.to_owned(),
            font_size_px: 12.0,
            show_line_numbers: true,
            wrap_long_lines: false,
            light_theme: CodeSyntaxTheme::GitHubLight,
            dark_theme: CodeSyntaxTheme::GitHubDark,
        }
    }
}

fn default_terminal_font_family() -> String {
    crate::app_settings::DEFAULT_TERMINAL_FONT_FAMILY.to_owned()
}

fn default_terminal_inherit_system_profile() -> bool {
    true
}

/// 常规页展示的设备级应用偏好快照；值始终来自 AppSettings，而不是页面本地草稿。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreferencesSnapshot {
    pub app_update_download_source: AppUpdateDownloadSource,
    pub project_directory: String,
    pub task_notifications: bool,
    pub notification_sound: bool,
    pub keep_computer_awake: bool,
    pub close_to_tray: bool,
    pub background_agent_limit: u16,
    pub http_proxy: Option<String>,
    pub http_proxy_no_proxy: Option<String>,
    pub local_memories: bool,
}

impl Default for PreferencesSnapshot {
    fn default() -> Self {
        let settings = crate::app_settings::AppSettings::default();
        Self {
            app_update_download_source: settings.app_update_download_source,
            project_directory: settings.project_directory,
            task_notifications: settings.task_notifications,
            notification_sound: settings.notification_sound,
            keep_computer_awake: settings.keep_computer_awake,
            close_to_tray: settings.close_to_tray,
            background_agent_limit: settings.background_agent_limit,
            http_proxy: settings.http_proxy,
            http_proxy_no_proxy: settings.http_proxy_no_proxy,
            local_memories: settings.local_memories,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneralSettings {
    /// 这五个字段由 GeneralSettings 统一承载，Appearance 页只切换展示路由，
    /// 避免为同一份 NativeHost 持久化事实维护第二套 snapshot 类型。
    pub appearance: AppearanceMode,
    pub font_family: String,
    pub font_size_px: f32,
    pub density: UiDensity,
    pub reduced_motion: bool,
    /// 代码设置与界面外观共用一个确认快照，供代码编辑器、Diff 和预览读取。
    pub code: CodeAppearanceSettings,
    /// Windows 内置终端使用的 Shell；Auto 由宿主按平台和环境解析。
    #[serde(default)]
    pub terminal_shell: TerminalShell,
    /// 终端使用的单一真实字体族名称，不在设置层拼接 CSS fallback 列表。
    #[serde(default = "default_terminal_font_family")]
    pub terminal_font_family: String,
    /// 是否让新建 PTY 加载系统终端 profile；关闭时仍保留基础环境。
    #[serde(default = "default_terminal_inherit_system_profile")]
    pub terminal_inherit_system_profile: bool,
    /// AppSettings 的设备级偏好快照；热更新和重启生效语义由宿主决定。
    #[serde(default)]
    pub preferences: PreferencesSnapshot,
    /// 全局自定义指令；保存后仅对后续新 Turn 生效，已开始 Turn 使用冻结上下文。
    #[serde(default)]
    pub custom_instructions: String,
    /// NativeHost 更新器的最新确认状态；None 表示宿主尚未注入更新端口。
    #[serde(default)]
    pub app_update: Option<AppUpdateStatus>,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            appearance: AppearanceMode::System,
            font_family: "系统默认".to_owned(),
            font_size_px: 14.0,
            density: UiDensity::Compact,
            reduced_motion: true,
            code: CodeAppearanceSettings::default(),
            terminal_shell: TerminalShell::default(),
            terminal_font_family: default_terminal_font_family(),
            terminal_inherit_system_profile: default_terminal_inherit_system_profile(),
            preferences: PreferencesSnapshot::default(),
            custom_instructions: String::new(),
            app_update: None,
        }
    }
}

/// GPUI 原生窗口使用的快捷键。值采用 GPUI keystroke 语法，例如
/// `secondary-n`、`ctrl-shift-p` 或 `enter`；NativeSettingsAdapter 会在写入前
/// 严格校验格式、冲突和保留的 TextInput 编辑键。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardSettings {
    pub composer_submit: String,
    pub new_chat: String,
    pub search: String,
    pub quit: String,
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            // 默认 Enter 发送、Shift+Enter 换行，与 Composer 的可见提示一致。
            composer_submit: "enter".to_owned(),
            new_chat: "secondary-n".to_owned(),
            search: "secondary-k".to_owned(),
            quit: "secondary-q".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GeneralPatch {
    pub appearance: Option<AppearanceMode>,
    pub font_family: Option<String>,
    pub font_size_px: Option<f32>,
    pub density: Option<UiDensity>,
    pub reduced_motion: Option<bool>,
    /// 代码设置按完整值替换，避免各页面维护一份可互相覆盖的局部草稿。
    pub code: Option<CodeAppearanceSettings>,
    pub terminal_shell: Option<TerminalShell>,
    pub terminal_font_family: Option<String>,
    pub terminal_inherit_system_profile: Option<bool>,
    /// 常规页一次只修改一个字段，但所有字段最终在 AppSettings 锁内合并提交。
    pub preferences: Option<PreferencesPatch>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub revision: u64,
    pub providers: Vec<ProviderSummary>,
    pub active: Option<ModelSelection>,
    /// 当前 provider facade/catalog 的能力，不凭 provider 名称推断。
    pub catalog: Vec<ModelCatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_backend: String,
    pub enabled: bool,
    pub executable: bool,
    pub api_key_configured: bool,
    pub models: Vec<ModelSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelSummary {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub executable: bool,
    pub issues: Vec<String>,
    pub supports_vision: bool,
    pub reasoning_efforts: Vec<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub config: ModelConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModelConfig {
    pub supports_vision: bool,
    pub reasoning_efforts: Vec<String>,
    /// 区分“没有手工覆盖”与“显式清空覆盖”；后者必须持久化为空列表，不能回退公共目录。
    #[serde(default)]
    pub reasoning_efforts_configured: bool,
    pub use_recommended_config: bool,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub output_token_field: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub provider_id: String,
    pub model_id: String,
    pub label: String,
    pub protocol: String,
    pub supports_vision: bool,
    pub reasoning_efforts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderPatch {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_backend: Option<String>,
    /// None 保持现有密钥，Some(None) 清空密钥；UI 不回显旧密钥。
    pub api_key: Option<Option<String>>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResourceSettings {
    pub plugins: Vec<PluginSummary>,
    pub mcp_servers: Vec<McpServerSummary>,
    pub skills: Vec<SkillSummary>,
    pub memories: Vec<MemorySummary>,
    pub agent_templates: Vec<AgentTemplateSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub scope: String,
    pub enabled: bool,
    /// 有持久化 marketplace 来源时为 true，表示支持“重新获取”流程；不承诺
    /// 设置页加载时已经完成远程版本比较。
    pub update_available: bool,
    pub executable: bool,
    pub description: Option<String>,
    /// 只包含插件公开配置；敏感配置永不回显到设置投影。
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// 当前插件快照中可调用的 Slash command 名称。
    #[serde(default)]
    pub commands: Vec<String>,
    /// 插件清单声明但当前 Native 执行入口尚未支持的 Hook 事件名。
    #[serde(default)]
    pub unsupported_hooks: Vec<String>,
}

/// Hook 所属范围。项目级 Hook 总是针对当前焦点项目读取，不能由 UI 传入任意路径。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookScope {
    #[default]
    User,
    Project,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    SessionStart,
    SubagentStart,
    UserPromptSubmit,
    #[default]
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PostToolUseFailure,
    OnError,
    PreCompact,
    PostCompact,
    Stop,
}

impl HookEvent {
    pub const ALL: [Self; 11] = [
        Self::SessionStart,
        Self::SubagentStart,
        Self::UserPromptSubmit,
        Self::PreToolUse,
        Self::PermissionRequest,
        Self::PostToolUse,
        Self::PostToolUseFailure,
        Self::OnError,
        Self::PreCompact,
        Self::PostCompact,
        Self::Stop,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::SubagentStart => "SubagentStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PermissionRequest => "PermissionRequest",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::OnError => "OnError",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::Stop => "Stop",
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookType {
    #[default]
    Command,
    Context,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookTrustState {
    #[default]
    NotApplicable,
    PendingTrust,
    TrustedPersistent,
    TrustStoreCorrupt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookSource {
    #[default]
    User,
    Project,
    Plugin,
}

/// 可编辑 Hook 的完整配置；项目路径由设置 domain 根据焦点会话决定。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookConfig {
    pub scope: HookScope,
    pub event: HookEvent,
    #[serde(rename = "type")]
    pub hook_type: HookType,
    #[serde(default)]
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
    /// Context Hook 的追加上下文；仅由 Native Runtime 在允许的阶段消费。
    #[serde(default)]
    pub context: Option<String>,
    /// PreToolUse Context Hook 的阻断消息。
    #[serde(default)]
    pub block: Option<String>,
    /// PreToolUse Context Hook 的结构化输入覆盖。
    #[serde(default)]
    pub input: Option<Value>,
    /// Stop Context Hook 是否允许当前回合继续。
    #[serde(default)]
    pub continue_turn: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_hook_timeout_ms() -> u64 {
    600_000
}

fn default_enabled() -> bool {
    true
}

/// 供列表和 Runtime admission 共用的归一化 Hook 投影。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSummary {
    pub id: String,
    pub scope: HookScope,
    pub event: HookEvent,
    #[serde(rename = "type")]
    pub hook_type: HookType,
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub shell: Option<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub continue_turn: bool,
    pub enabled: bool,
    pub editable: bool,
    pub source: HookSource,
    #[serde(default)]
    pub plugin_id: Option<String>,
    #[serde(default)]
    pub plugin_name: Option<String>,
    #[serde(default)]
    pub plugin_scope: Option<String>,
    #[serde(default)]
    pub workspace_identity: Option<String>,
    #[serde(default)]
    pub bundle_digest: Option<String>,
    #[serde(default)]
    pub hook_declaration_digest: Option<String>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub source_file_index: Option<usize>,
    #[serde(default)]
    pub hook_index: Option<usize>,
    #[serde(default)]
    pub trust_state: HookTrustState,
    #[serde(default)]
    pub read_only_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookWorkspaceSnapshot {
    pub workspace_identity: String,
    pub bundle_digest: String,
    pub hook_count: usize,
    pub trust_store_corrupt: bool,
}

/// 插件 Hook 只读诊断；它们随插件运行时快照展示，不能通过设置页 CRUD 修改。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHookDiagnostic {
    pub plugin_id: String,
    pub plugin_name: String,
    pub scope: String,
    pub event: String,
    #[serde(rename = "type")]
    pub hook_type: String,
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub supported: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HooksSettings {
    pub hooks: Vec<HookSummary>,
    #[serde(default)]
    pub plugin_hooks: Vec<PluginHookDiagnostic>,
    #[serde(default)]
    pub workspace: Option<HookWorkspaceSnapshot>,
    #[serde(default)]
    pub trust_store_corrupt: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServerSummary {
    pub id: String,
    #[serde(default = "default_global_scope")]
    pub scope: String,
    pub name: String,
    pub transport: String,
    pub enabled: bool,
    pub connected: bool,
    pub tool_count: Option<u32>,
    pub error: Option<String>,
    #[serde(default)]
    pub config_json: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub path: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemorySummary {
    pub workspace_id: String,
    pub label: String,
    pub file_name: String,
    pub kind: String,
    pub size: u64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentTemplateSummary {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub enabled: bool,
    pub model: Option<ModelSelection>,
    pub tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub description: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryFile {
    pub workspace_id: String,
    pub file_name: String,
    pub content: String,
    pub updated_at_ms: i64,
}

fn default_global_scope() -> String {
    "global".to_owned()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AutomationSettings {
    pub items: Vec<AutomationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutomationRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub cron_expr: String,
    pub enabled: bool,
    pub recurring: bool,
    pub max_runs: Option<u32>,
    pub next_run_at_ms: Option<i64>,
    pub last_error: Option<String>,
    pub history: Vec<AutomationRun>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutomationRun {
    pub id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub status: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkflowSettings {
    pub items: Vec<WorkflowRecord>,
    /// 最近一次由 Rust WorkflowHost 返回的受控查询结果；它不是 Journal 的第二份事实源。
    #[serde(default)]
    pub inspection: Option<WorkflowInspection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowRecord {
    pub name: String,
    pub scope: String,
    pub path: String,
    pub revision: u64,
    pub description: Option<String>,
    #[serde(default)]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub definition_json: String,
    pub valid: bool,
    pub validation_error: Option<String>,
    pub runs: Vec<WorkflowRun>,
}

/// 设置页可见的 Workflow Journal 查询结果。
///
/// 每次查询只填充一个或少数几个字段，正文仍受 WorkflowHost 的字节和分页限制；
/// Artifact bytes 只作为受控元数据返回给面板，面板不得据此打开媒体或网页预览。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkflowInspection {
    pub run_id: String,
    /// 由 WorkflowHost/Journal 读取的当前运行摘要；不能由页面按钮状态推断。
    #[serde(default)]
    pub run: Option<WorkflowRunInspection>,
    /// 运行开始时冻结的身份摘要；正文定义和解析后的输入不复制到设置投影。
    #[serde(default)]
    pub frozen_run: Option<WorkflowFrozenRunInspection>,
    /// Host 从 Journal 生成的动态进度事件，和原始事件查询共用同一事实源。
    #[serde(default)]
    pub progress_events: Option<Vec<DynamicWorkflowRunProgressPayload>>,
    /// 当前运行是否仍处于 Host 管理的活动执行屏障内。
    #[serde(default)]
    pub active_barrier: Option<WorkflowActiveBarrierInspection>,
    /// 从运行终态或进度事件中提取的稳定错误码；错误正文不进入该字段。
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub events: Option<WorkflowEventPage>,
    #[serde(default)]
    pub graph: Option<WorkflowGraphResult>,
    #[serde(default)]
    pub workspace: Option<WorkflowWorkspaceResult>,
    #[serde(default)]
    pub node_result: Option<WorkflowNodeResult>,
    #[serde(default)]
    pub artifacts: Option<Vec<WorkflowArtifact>>,
    #[serde(default)]
    pub artifact_items: Option<WorkflowArtifactPage>,
    #[serde(default)]
    pub artifact_bytes: Option<WorkflowArtifactBytes>,
}

/// 设置页可展示的运行快照。字段来自 WorkflowHost 的 Journal 归约，避免 UI 自己
/// 用按钮状态拼出“已完成”或“可恢复”。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkflowRunInspection {
    pub run_id: String,
    pub status: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub canonical_hash: Option<String>,
    pub resumable: bool,
}

/// 冻结运行的受控身份摘要。definition、resolved inputs、model 和 budgets 可能含有
/// 大量用户内容，设置页只需要这些字段判断修订一致性和父会话归属。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkflowFrozenRunInspection {
    pub canonical_hash: String,
    pub input_hash: String,
    pub parent_session_id: String,
    pub tool_call_id: String,
    #[serde(default)]
    pub launch_input_id: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub predecessor_run_id: Option<String>,
}

/// Host 对运行活动屏障的权威投影；`active` 只表示当前运行仍可能产生事实。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkflowActiveBarrierInspection {
    pub active: bool,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub status: String,
    pub event_count: u32,
    pub artifact_count: u32,
    pub error: Option<String>,
    /// 终态事件中的稳定错误码；正文保持在 Journal，不复制到列表投影。
    #[serde(default)]
    pub error_code: Option<String>,
    /// 由 WorkflowHost 的恢复门禁计算，UI 不根据 status 猜测。
    #[serde(default)]
    pub resumable: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentSettings {
    pub goal: Option<GoalState>,
    /// 即使当前 Goal 已清除，也保留持久化快照的修订号供下一次 CAS 写入使用。
    #[serde(default)]
    pub goal_revision: u64,
    pub subagents: Vec<SubagentSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalState {
    pub goal_id: String,
    pub title: String,
    pub status: String,
    pub detail: Option<String>,
    pub revision: u64,
    /// Journal 中当前 root turn 的真实运行标识；没有活动运行时为空。
    #[serde(default)]
    pub active_root_turn_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentSummary {
    pub id: String,
    pub name: String,
    pub source: String,
    pub enabled: bool,
    pub model: Option<ModelSelection>,
    pub tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub description: Option<String>,
}

/// Usage 页面保留最近一次已确认查询，分页和筛选只由 NativeInsights 执行。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageSettings {
    pub query: RequestRecordsQuery,
    pub snapshot: UsageSnapshot,
}

/// Diagnostics 导出结果只在一次命令返回中短暂携带，页面不会把它写入磁盘。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticsSettings {
    pub snapshot: DiagnosticsSnapshot,
    #[serde(default)]
    pub exported_json: Option<String>,
}

/// 发送到 Host 的所有状态变更均为显式 typed command，避免 UI 以“成功 toast”替代实际执行。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SettingsCommand {
    SaveGeneral(GeneralPatch),
    /// 外观页与常规页共享 GeneralSettings 快照，但由独立命令表达页面职责。
    SaveAppearance(GeneralPatch),
    SaveKeyboard(KeyboardSettings),
    /// 独立写入数据根的 `AGENTS.md`，不混入常规外观设置文件。
    SaveCustomInstructions {
        instructions: String,
    },
    CheckForUpdates,
    DownloadUpdate,
    InstallUpdate,
    CreateProvider {
        id: String,
        name: String,
        base_url: String,
        api_backend: String,
        api_key: Option<String>,
        models: Vec<String>,
    },
    UpdateProvider {
        provider_id: String,
        patch: ProviderPatch,
    },
    DeleteProvider {
        provider_id: String,
    },
    RefreshProviderCatalog {
        provider_id: String,
    },
    SetActiveModel(ModelSelection),
    CreateModel {
        provider_id: String,
        model_id: String,
        config: ModelConfig,
    },
    PatchModel {
        provider_id: String,
        model_id: String,
        config: ModelConfig,
    },
    DeleteModel {
        provider_id: String,
        model_id: String,
    },
    SetModelEnabled {
        provider_id: String,
        model_id: String,
        enabled: bool,
    },
    TestModel {
        selection: ModelSelection,
    },
    CreateHook(HookConfig),
    UpdateHook {
        hook_id: String,
        scope: HookScope,
        hook: HookConfig,
    },
    DeleteHook {
        hook_id: String,
        scope: HookScope,
    },
    SetHookEnabled {
        hook_id: String,
        scope: HookScope,
        enabled: bool,
    },
    GrantHookTrust {
        workspace_identity: String,
        bundle_digest: String,
        hook_declaration_digest: String,
    },
    RevokeHookTrust {
        workspace_identity: String,
        bundle_digest: String,
        hook_declaration_digest: String,
    },
    SetPluginEnabled {
        plugin_id: String,
        enabled: bool,
    },
    InstallPlugin {
        source: String,
    },
    UpdatePlugin {
        plugin_id: String,
    },
    UninstallPlugin {
        plugin_id: String,
    },
    ConfigurePlugin {
        plugin_id: String,
        values: BTreeMap<String, String>,
    },
    SetMcpEnabled {
        server_id: String,
        enabled: bool,
    },
    AddMcp {
        server: McpServerSummary,
    },
    UpdateMcp {
        server: McpServerSummary,
    },
    RemoveMcp {
        server_id: String,
    },
    InspectMcp {
        server_id: String,
    },
    SetSkillEnabled {
        skill_id: String,
        enabled: bool,
    },
    CopySkillToCommon {
        skill_id: String,
    },
    RemoveSkillFromCommon {
        skill_id: String,
    },
    DeleteSkill {
        skill_id: String,
    },
    CreateSkill {
        skill: SkillSummary,
    },
    UpdateSkill {
        skill: SkillSummary,
    },
    ReadMemory {
        workspace_id: String,
        file_name: String,
    },
    DeleteMemory {
        workspace_id: String,
        file_name: String,
    },
    CreateMemory {
        memory: MemoryFile,
    },
    UpdateMemory {
        memory: MemoryFile,
    },
    CreateAgentTemplate(AgentTemplateSummary),
    UpdateAgentTemplate(AgentTemplateSummary),
    DeleteAgentTemplate {
        agent_id: String,
    },
    SetAgentTemplateEnabled {
        agent_id: String,
        enabled: bool,
    },
    CreateAutomation(AutomationRecord),
    UpdateAutomation(AutomationRecord),
    DeleteAutomation {
        automation_id: String,
    },
    SetAutomationEnabled {
        automation_id: String,
        enabled: bool,
    },
    RunAutomation {
        automation_id: String,
    },
    CancelAutomation {
        run_id: String,
    },
    ListAutomationHistory {
        automation_id: String,
    },
    SaveWorkflow(WorkflowRecord),
    /// 在当前授权项目与全局存储之间移动定义；expected_revision 防止覆盖并发编辑。
    MoveWorkflow {
        name: String,
        from_scope: String,
        to_scope: String,
        expected_revision: u64,
    },
    DeleteWorkflow {
        name: String,
        scope: String,
    },
    RunWorkflow {
        name: String,
        scope: String,
        inputs_json: String,
    },
    CancelWorkflow {
        run_id: String,
    },
    /// 基于 predecessor 的冻结父会话与执行参数铸造新的定义修订 run。
    AmendWorkflow {
        predecessor_run_id: String,
        definition_json: String,
        inputs_json: String,
    },
    /// 从 Journal 冻结快照恢复指定运行。
    ResumeWorkflow {
        run_id: String,
    },
    /// 回答当前父 Session 路由到设置页的 Workflow typed 问答。
    ResolveWorkflowQuestion {
        question_id: String,
        answer: String,
    },
    ListWorkflowRuns {
        name: String,
        scope: String,
    },
    ReadWorkflowEvents {
        run_id: String,
    },
    ReadWorkflowGraph {
        run_id: String,
    },
    ReadWorkflowWorkspace {
        run_id: String,
    },
    ReadWorkflowNodeResult {
        run_id: String,
        site_id: String,
        ordinal: u32,
        max_bytes: usize,
    },
    ListWorkflowArtifacts {
        run_id: String,
    },
    ListWorkflowArtifactItems {
        run_id: String,
        artifact_id: String,
        after_sequence: Option<u64>,
        limit: usize,
    },
    ReadWorkflowArtifact {
        run_id: String,
        artifact_id: String,
        version: u32,
        offset: usize,
        limit: usize,
    },
    SetGoal {
        goal_id: String,
        title: String,
        detail: Option<String>,
        expected_revision: u64,
    },
    ClearGoal {
        goal_id: String,
        expected_revision: u64,
    },
    /// Goal 生命周期命令均由 Host 校验 revision，并落到真实 Journal 运行状态。
    ResumeGoal {
        goal_id: String,
        expected_revision: u64,
    },
    PauseGoal {
        goal_id: String,
        expected_revision: u64,
    },
    CompleteGoal {
        goal_id: String,
        expected_revision: u64,
        evidence: String,
    },
    BlockGoal {
        goal_id: String,
        expected_revision: u64,
        reason: String,
    },
    CancelGoalRun {
        goal_id: String,
        expected_revision: u64,
        turn_id: String,
    },
    CreateSubagent(SubagentSummary),
    UpdateSubagent(SubagentSummary),
    DeleteSubagent {
        agent_id: String,
    },
    SetSubagentEnabled {
        agent_id: String,
        enabled: bool,
    },
    SetSubagentModel {
        agent_id: String,
        model: Option<ModelSelection>,
    },
    QueryUsage(RequestRecordsQuery),
    RefreshDiagnostics,
    ExportDiagnostics,
}
