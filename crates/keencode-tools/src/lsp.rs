//! Provider 中立、按项目共享且受资源上限保护的原生 LSP 客户端工具。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use command_group::{AsyncCommandGroup, AsyncGroupChild};
use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    ToolRegistry, ToolRegistryError, TurnCancellation,
};
use keencode_model::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use url::Url;

/// 单个 LSP 帧允许的最大 JSON 正文字节数。
const MAX_LSP_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// 单个 LSP 帧全部 Header 允许的最大字节数。
pub(crate) const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
/// 发送给 Server 的单个源文件最大字节数。
const MAX_LSP_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;
/// 返回给模型的单次 LSP JSON 最大字节数。
const MAX_LSP_OUTPUT_BYTES: usize = 512 * 1024;
/// 单个文件缓存的诊断条目上限。
const MAX_LSP_DIAGNOSTICS: usize = 500;
/// 单个 Server 最多保留的文件诊断快照数量。
pub(crate) const MAX_LSP_DIAGNOSTIC_DOCUMENTS: usize = 64;
/// publishDiagnostics 文件 URI 允许进入长期缓存的最大 UTF-8 字节数。
const MAX_LSP_DIAGNOSTIC_URI_BYTES: usize = 8 * 1024;
/// LSP Server 名称允许占用的最大 UTF-8 字节数。
const MAX_LSP_SERVER_NAME_BYTES: usize = 256;
/// 不可信 Server 错误说明允许进入 ToolError 的最大 UTF-8 字节数。
const MAX_LSP_SERVER_ERROR_BYTES: usize = 4 * 1024;
/// 进入扩展准备报告的单条安全说明最大 UTF-8 字节数。
const MAX_LSP_DIAGNOSTIC_MESSAGE_BYTES: usize = 1_024;
/// initialize 之外的普通 LSP 请求硬超时。
const LSP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// push diagnostics 未立即到达时允许等待的最长时间。
const LSP_DIAGNOSTICS_WAIT: Duration = Duration::from_millis(1_500);
/// Reader 向串行请求端保留的响应数量。
const LSP_RESPONSE_CHANNEL_CAPACITY: usize = 32;

/// 一个已完成静态插值、可以启动原生 LSP 进程的配置。
#[derive(Clone, Debug, PartialEq)]
pub struct LspServerConfig {
    /// 当前项目内稳定且唯一的 Server 名称。
    pub name: String,
    /// 不经过 Shell 解释的 LSP 可执行程序。
    pub command: String,
    /// 原样传递给 LSP 进程的参数。
    pub args: Vec<String>,
    /// LSP 进程的规范工作目录。
    pub current_dir: PathBuf,
    /// 只覆盖当前 LSP 进程树的环境变量。
    pub environment: BTreeMap<String, String>,
    /// 去掉前导点的文件扩展名到 LSP language ID 映射。
    pub extension_to_language: BTreeMap<String, String>,
    /// 原样放入 initializeParams.initializationOptions 的 JSON。
    pub initialization_options: Option<Value>,
    /// 候选发布阶段启动失败时允许的受控重试次数。
    pub max_restarts: u32,
    /// initialize 请求从启动到完成的硬超时毫秒数。
    pub startup_timeout_ms: u64,
}

/// LSP Runtime 配置无法安全冻结时返回的错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspRuntimeError {
    /// 不包含源文件正文或环境变量值的安全错误说明。
    message: String,
}

impl LspRuntimeError {
    /// 创建一个只携带安全摘要的配置错误。
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LspRuntimeError {
    /// 输出安全错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LspRuntimeError {}

/// LSP 可选扩展准备失败的稳定诊断分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspDiagnosticCode {
    /// 单个 Server 配置无法安全冻结。
    InvalidConfiguration,
    /// 多个 Server 归一化后使用了同一名称。
    DuplicateServer,
    /// 单个 Server 进程无法启动或完成初始化。
    StartupFailed,
}

impl LspDiagnosticCode {
    /// 返回供日志、控制面和测试稳定识别的 ASCII 分类码。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "lsp_invalid_configuration",
            Self::DuplicateServer => "lsp_duplicate_server",
            Self::StartupFailed => "lsp_startup_failed",
        }
    }
}

impl fmt::Display for LspDiagnosticCode {
    /// 输出稳定的 LSP 诊断分类码。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一个可安全交给上层 Session 记录的 LSP 扩展诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspDiagnostic {
    /// 经过边界处理的 Server 名称；无法解析名称时使用固定占位符。
    pub server: String,
    /// 不依赖外部进程正文的稳定诊断分类。
    pub code: LspDiagnosticCode,
    /// 已清理控制字符并截断的安全说明。
    pub message: String,
}

impl LspDiagnostic {
    /// 创建一条不会把无界或控制字符文本带入日志的诊断。
    fn new(server: &str, code: LspDiagnosticCode, message: impl AsRef<str>) -> Self {
        Self {
            server: diagnostic_server_name(server),
            code,
            message: bounded_lsp_diagnostic_message(message.as_ref()),
        }
    }
}

/// 一次 LSP best-effort 构造或启动的结果；可用 Server 继续工作，失败项留在诊断中。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspPreparationReport {
    /// 已成功完成初始化并可执行查询的 Server 名称。
    started_servers: Vec<String>,
    /// 不影响其他 Server 的配置或启动失败。
    diagnostics: Vec<LspDiagnostic>,
}

impl LspPreparationReport {
    /// 返回已完成初始化的 Server 名称快照。
    pub fn started_servers(&self) -> &[String] {
        &self.started_servers
    }

    /// 返回按处理顺序记录的安全诊断。
    pub fn diagnostics(&self) -> &[LspDiagnostic] {
        &self.diagnostics
    }

    /// 返回本次准备是否发生降级。
    pub fn is_degraded(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    /// 将另一阶段的启动结果合并到当前报告。
    pub fn append(&mut self, mut other: Self) {
        self.started_servers.append(&mut other.started_servers);
        self.diagnostics.append(&mut other.diagnostics);
    }

    /// 记录一个已成功启动的 Server 名称。
    fn push_started(&mut self, name: &str) {
        self.started_servers.push(name.to_owned());
    }

    /// 记录一个受边界保护的失败诊断。
    fn push_diagnostic(&mut self, diagnostic: LspDiagnostic) {
        self.diagnostics.push(diagnostic);
    }
}

/// 一个项目候选代次内共享、由宿主显式管理进程生命周期的 LSP Server 集合。
pub struct LspRuntime {
    /// 所有 Server 初始化时使用的规范项目根。
    project_root: PathBuf,
    /// 按稳定名称排序的 Server 实例。
    servers: BTreeMap<String, Arc<LspServer>>,
    /// best-effort 启动阶段确认不可用的 Server 名称。
    unavailable_servers: RwLock<HashSet<String>>,
    /// 是否在 Server 选择时过滤已确认不可用的条目。
    filter_unavailable: AtomicBool,
}

impl LspRuntime {
    /// 校验并冻结一代 LSP 配置；此构造过程不会启动任何外部进程。
    pub fn new(
        project_root: impl AsRef<Path>,
        configs: Vec<LspServerConfig>,
    ) -> Result<Self, LspRuntimeError> {
        let project_root = canonical_directory(project_root.as_ref(), "项目根")?;
        let mut servers = BTreeMap::new();
        for mut config in configs {
            validate_server_config(&mut config)?;
            config.current_dir = canonical_directory(&config.current_dir, "LSP 工作目录")?;
            let name = config.name.clone();
            if servers
                .insert(
                    name.clone(),
                    Arc::new(LspServer {
                        config,
                        state: AsyncMutex::new(LspServerState::default()),
                    }),
                )
                .is_some()
            {
                return Err(LspRuntimeError::new(format!("LSP Server 名称重复：{name}")));
            }
        }
        Ok(Self {
            project_root,
            servers,
            unavailable_servers: RwLock::new(HashSet::new()),
            filter_unavailable: AtomicBool::new(false),
        })
    }

    /// 尽可能冻结 LSP 配置；单个无效 Server 被记录后不会阻断其他候选。
    pub fn new_best_effort(
        project_root: impl AsRef<Path>,
        configs: Vec<LspServerConfig>,
    ) -> Result<(Self, LspPreparationReport), LspRuntimeError> {
        let project_root = canonical_directory(project_root.as_ref(), "项目根")?;
        let mut report = LspPreparationReport::default();
        let mut servers = BTreeMap::new();
        for mut config in configs {
            let requested_name = diagnostic_server_name(&config.name);
            if let Err(error) = validate_server_config(&mut config) {
                report.push_diagnostic(LspDiagnostic::new(
                    &requested_name,
                    LspDiagnosticCode::InvalidConfiguration,
                    error.to_string(),
                ));
                continue;
            }
            let current_dir = match canonical_directory(&config.current_dir, "LSP 工作目录") {
                Ok(current_dir) => current_dir,
                Err(error) => {
                    report.push_diagnostic(LspDiagnostic::new(
                        &config.name,
                        LspDiagnosticCode::InvalidConfiguration,
                        error.to_string(),
                    ));
                    continue;
                }
            };
            config.current_dir = current_dir;
            let name = config.name.clone();
            if servers.contains_key(&name) {
                report.push_diagnostic(LspDiagnostic::new(
                    &name,
                    LspDiagnosticCode::DuplicateServer,
                    format!("LSP Server 名称重复：{name}"),
                ));
                continue;
            }
            servers.insert(
                name,
                Arc::new(LspServer {
                    config,
                    state: AsyncMutex::new(LspServerState::default()),
                }),
            );
        }
        Ok((
            Self {
                project_root,
                servers,
                unavailable_servers: RwLock::new(HashSet::new()),
                filter_unavailable: AtomicBool::new(false),
            },
            report,
        ))
    }

    /// 返回当前候选是否没有任何启用的 LSP Server。
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// 返回当前候选冻结的 Server 数量。
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// 返回此 Runtime 唯一适用的规范项目根。
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// 在扩展候选发布前受控启动并初始化全部 Server，避免只读查询首次调用隐式拉起进程。
    pub async fn start_all(&self) -> Result<(), LspRuntimeError> {
        self.filter_unavailable.store(false, Ordering::Release);
        let cancellation = TurnCancellation::new();
        for server in self.servers.values() {
            if let Err(error) = server
                .ensure_started(&self.project_root, &cancellation)
                .await
            {
                // 候选启动必须具有原子性，不能把前序 Server 留在未发布代次中运行。
                self.shutdown_all().await;
                return Err(LspRuntimeError::new(format!(
                    "LSP Server {} 初始化失败：{}",
                    server.config.name, error.message
                )));
            }
        }
        Ok(())
    }

    /// 尽可能启动全部冻结 Server；单个启动失败只进入诊断，已成功者继续可用。
    pub async fn start_available(&self) -> LspPreparationReport {
        self.filter_unavailable.store(true, Ordering::Release);
        let cancellation = TurnCancellation::new();
        let mut report = LspPreparationReport::default();
        for server in self.servers.values() {
            match server
                .ensure_started(&self.project_root, &cancellation)
                .await
            {
                Ok(()) => {
                    self.unavailable_servers
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&server.config.name);
                    report.push_started(&server.config.name);
                }
                Err(error) => {
                    self.unavailable_servers
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(server.config.name.clone());
                    report.push_diagnostic(LspDiagnostic::new(
                        &server.config.name,
                        LspDiagnosticCode::StartupFailed,
                        error.to_string(),
                    ));
                }
            }
        }
        report
    }

    /// 主动终止并回收此候选拥有的全部 LSP 进程树。
    pub async fn shutdown_all(&self) {
        for server in self.servers.values() {
            server.shutdown().await;
        }
    }

    /// 根据显式 Server 或文件扩展名选择唯一实例。
    fn select_server(
        &self,
        requested: Option<&str>,
        file: Option<&Path>,
    ) -> Result<Arc<LspServer>, ToolError> {
        let filter_unavailable = self.filter_unavailable.load(Ordering::Acquire);
        let unavailable_servers = self
            .unavailable_servers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(name) = requested {
            let server = self.servers.get(name).cloned().ok_or_else(|| {
                ToolError::permanent("lsp_server_not_found", format!("未知 LSP Server：{name}"))
            })?;
            if filter_unavailable && unavailable_servers.contains(name) {
                return Err(ToolError::retryable(
                    "lsp_unavailable",
                    "指定的 LSP Server 初始化失败，请查看扩展诊断后重试",
                ));
            }
            return Ok(server);
        }
        let candidates = if let Some(file) = file {
            let extension = normalized_file_extension(file)?;
            self.servers
                .values()
                .filter(|server| {
                    server.config.extension_to_language.contains_key(&extension)
                        && (!filter_unavailable
                            || !unavailable_servers.contains(&server.config.name))
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.servers
                .values()
                .filter(|server| {
                    !filter_unavailable || !unavailable_servers.contains(&server.config.name)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        match candidates.as_slice() {
            [server] => Ok(Arc::clone(server)),
            [] if filter_unavailable && !self.servers.is_empty() => Err(ToolError::retryable(
                "lsp_unavailable",
                "没有可用的 LSP Server，请查看扩展诊断后重试",
            )),
            [] => Err(ToolError::permanent(
                "lsp_server_not_found",
                "没有与本次调用匹配的 LSP Server",
            )),
            _ => Err(ToolError::permanent(
                "lsp_server_ambiguous",
                "多个 LSP Server 与本次调用匹配，请显式提供 server",
            )),
        }
    }
}

impl Drop for LspRuntime {
    /// Runtime 或扩展候选释放时同步触发所有进程树终止的最后防线。
    fn drop(&mut self) {
        for server in self.servers.values() {
            if let Ok(mut state) = server.state.try_lock()
                && let Some(process) = state.process.take()
            {
                drop(process);
            }
        }
    }
}

/// 把非空 LSP Runtime 作为单个 Provider 中立工具注册到冻结工具表。
///
/// 生产调用方应先等待 [`LspRuntime::start_all`] 成功。工具执行本身绝不启动或
/// 重启外部进程，因此即使误注册未启动的 Runtime，也只会返回明确错误。
pub fn register_lsp_tool(
    registry: &mut ToolRegistry,
    runtime: Arc<LspRuntime>,
) -> Result<(), ToolRegistryError> {
    if runtime.is_empty() {
        return Ok(());
    }
    registry.register(Arc::new(LspTool::new(runtime)))
}

/// 对模型暴露 hover、定义、引用、符号和诊断的只读 LSP 工具。
pub struct LspTool {
    /// 当前扩展候选拥有的项目级 LSP 生命周期。
    runtime: Arc<LspRuntime>,
}

impl LspTool {
    /// 创建一个绑定到不可变项目 LSP 候选的工具。
    pub fn new(runtime: Arc<LspRuntime>) -> Self {
        Self { runtime }
    }
}

impl AgentTool for LspTool {
    /// 返回固定操作集合和严格参数 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "LSP",
            "通过宿主已经启动的原生 Language Server 查询诊断、悬停信息、定义、引用、文档符号或工作区符号。工具调用不会启动或重启外部进程。file 可为相对项目根或项目内绝对路径；line 与 character 均从 1 开始，character 按 LSP UTF-16 code unit 计数。",
            json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["diagnostics", "hover", "definition", "references", "document_symbols", "workspace_symbols"]
                    },
                    "file": { "type": "string", "minLength": 1 },
                    "line": { "type": "integer", "minimum": 1 },
                    "character": { "type": "integer", "minimum": 1 },
                    "query": { "type": "string" },
                    "server": { "type": "string", "minLength": 1 }
                },
                "required": ["operation"],
                "additionalProperties": false
            }),
        )
    }

    /// 所有公开操作只读取文件与已启动 Language Server 的分析结果。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let input = parse_input(input, &self.runtime.project_root)?;
        self.runtime
            .select_server(input.server.as_deref(), input.file.as_deref())?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 不同 Server 可并行，同一 Server 由内部互斥锁保持 JSON-RPC 顺序。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 读取当前文件快照，通过已启动进程查询并返回有界 JSON 结果。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let runtime = Arc::clone(&self.runtime);
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::permanent("cancelled", "LSP 调用已取消"));
            }
            let input = parse_input(&input, &runtime.project_root)?;
            let server = runtime.select_server(input.server.as_deref(), input.file.as_deref())?;
            let result = server.execute(&input, &context.cancellation).await?;
            render_result(&server.config.name, input.operation, result)
        })
    }
}

/// 已通过结构与路径校验的一次 LSP 工具输入。
struct LspInput {
    /// 固定的只读查询操作。
    operation: LspOperation,
    /// 文件型操作使用的项目内规范绝对路径。
    file: Option<PathBuf>,
    /// 一基行号。
    line: Option<u32>,
    /// 一基 UTF-16 字符位置。
    character: Option<u32>,
    /// 工作区符号查询文本。
    query: Option<String>,
    /// 可选的精确 Server 名称。
    server: Option<String>,
}

/// 模型可以请求的固定只读 LSP 操作。
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LspOperation {
    /// 返回当前文件的诊断。
    Diagnostics,
    /// 返回一个位置的悬停信息。
    Hover,
    /// 返回一个位置的定义。
    Definition,
    /// 返回一个位置的全部引用。
    References,
    /// 返回当前文件的符号树。
    DocumentSymbols,
    /// 按文本搜索工作区符号。
    WorkspaceSymbols,
}

impl LspOperation {
    /// 返回进入模型输出的稳定操作名。
    const fn as_str(self) -> &'static str {
        match self {
            Self::Diagnostics => "diagnostics",
            Self::Hover => "hover",
            Self::Definition => "definition",
            Self::References => "references",
            Self::DocumentSymbols => "document_symbols",
            Self::WorkspaceSymbols => "workspace_symbols",
        }
    }

    /// 返回该操作是否必须携带文件。
    const fn requires_file(self) -> bool {
        !matches!(self, Self::WorkspaceSymbols)
    }

    /// 返回该操作是否必须携带位置。
    const fn requires_position(self) -> bool {
        matches!(self, Self::Hover | Self::Definition | Self::References)
    }
}

/// 仅用于拒绝未知字段的原始输入结构。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLspInput {
    /// 固定操作枚举。
    operation: LspOperation,
    /// 相对项目根或项目内绝对文件路径。
    file: Option<String>,
    /// 一基行号。
    line: Option<u32>,
    /// 一基 UTF-16 字符位置。
    character: Option<u32>,
    /// 工作区符号查询文本。
    query: Option<String>,
    /// 可选精确 Server 名称。
    server: Option<String>,
}

/// 把模型 JSON 转为满足操作约束的内部输入。
fn parse_input(input: &Value, project_root: &Path) -> Result<LspInput, ToolError> {
    let raw: RawLspInput = serde_json::from_value(input.clone()).map_err(|error| {
        ToolError::permanent("invalid_input", format!("LSP 工具输入无效：{error}"))
    })?;
    let file = raw
        .file
        .as_deref()
        .map(|value| resolve_project_file(project_root, value))
        .transpose()?;
    if raw.operation.requires_file() && file.is_none() {
        return Err(ToolError::permanent(
            "invalid_input",
            "当前 LSP 操作必须提供 file",
        ));
    }
    if !raw.operation.requires_file() && file.is_some() {
        return Err(ToolError::permanent(
            "invalid_input",
            "workspace_symbols 不接受 file",
        ));
    }
    if raw.operation.requires_position() {
        if raw.line.is_none_or(|value| value == 0) || raw.character.is_none_or(|value| value == 0) {
            return Err(ToolError::permanent(
                "invalid_input",
                "当前位置操作必须提供大于零的 line 与 character",
            ));
        }
    } else if raw.line.is_some() || raw.character.is_some() {
        return Err(ToolError::permanent(
            "invalid_input",
            "当前 LSP 操作不接受 line 或 character",
        ));
    }
    if matches!(raw.operation, LspOperation::WorkspaceSymbols) && raw.query.is_none() {
        return Err(ToolError::permanent(
            "invalid_input",
            "workspace_symbols 必须提供 query（允许空字符串）",
        ));
    }
    if !matches!(raw.operation, LspOperation::WorkspaceSymbols) && raw.query.is_some() {
        return Err(ToolError::permanent(
            "invalid_input",
            "当前 LSP 操作不接受 query",
        ));
    }
    let server = raw
        .server
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(LspInput {
        operation: raw.operation,
        file,
        line: raw.line,
        character: raw.character,
        query: raw.query,
        server,
    })
}

/// 将项目内文件路径规范化并拒绝符号链接逃逸与目录输入。
fn resolve_project_file(project_root: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    if raw.trim().is_empty() {
        return Err(ToolError::permanent("invalid_path", "LSP 文件路径不能为空"));
    }
    let path = Path::new(raw);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let canonical = std::fs::canonicalize(&candidate).map_err(|error| {
        ToolError::permanent("invalid_path", format!("无法访问 LSP 文件：{error}"))
    })?;
    if !canonical.starts_with(project_root) {
        return Err(ToolError::permanent(
            "path_outside_project",
            "LSP 文件必须位于当前项目内",
        ));
    }
    let metadata = std::fs::metadata(&canonical).map_err(|error| {
        ToolError::permanent("invalid_path", format!("无法读取 LSP 文件元数据：{error}"))
    })?;
    if !metadata.is_file() {
        return Err(ToolError::permanent("invalid_path", "LSP 路径不是普通文件"));
    }
    if metadata.len() > MAX_LSP_DOCUMENT_BYTES {
        return Err(ToolError::permanent(
            "lsp_document_too_large",
            format!("LSP 文件超过 {MAX_LSP_DOCUMENT_BYTES} 字节上限"),
        ));
    }
    Ok(canonical)
}

/// 一个配置及其串行可恢复进程状态。
struct LspServer {
    /// 不随进程重启变化的冻结配置。
    config: LspServerConfig,
    /// 保证单个 stdio JSON-RPC 流每次只有一个在途请求。
    state: AsyncMutex<LspServerState>,
}

impl LspServer {
    /// 在候选发布前建立一个可用进程，并按显式 max_restarts 处理启动期故障。
    async fn ensure_started(
        &self,
        project_root: &Path,
        cancellation: &TurnCancellation,
    ) -> Result<(), ToolError> {
        let mut state = self.state.lock().await;
        if state
            .process
            .as_mut()
            .is_some_and(|process| matches!(process.child.try_wait(), Ok(None)))
        {
            return Ok(());
        }
        if let Some(mut process) = state.process.take() {
            process.terminate().await;
        }
        let attempts = self.config.max_restarts.saturating_add(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            match LspProcess::start(&self.config, project_root, cancellation).await {
                Ok(process) => {
                    state.process = Some(process);
                    return Ok(());
                }
                Err(failure) => {
                    let restartable = failure.restartable;
                    last_error = Some(failure.error);
                    if !restartable || attempt + 1 >= attempts {
                        break;
                    }
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| ToolError::retryable("lsp_unavailable", "LSP Server 当前不可用")))
    }

    /// 终止并回收当前 Server 的完整进程树。
    async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        if let Some(mut process) = state.process.take() {
            process.terminate().await;
        }
    }

    /// 仅通过宿主已经启动的进程执行一次操作，任何故障都不会在工具内重启进程。
    async fn execute(
        &self,
        input: &LspInput,
        cancellation: &TurnCancellation,
    ) -> Result<Value, ToolError> {
        let mut state = self.state.lock().await;
        if cancellation.is_cancelled() {
            return Err(ToolError::permanent("cancelled", "LSP 调用已取消"));
        }
        let process = state.process.as_mut().ok_or_else(|| {
            ToolError::retryable(
                "lsp_not_started",
                "LSP Server 尚未由宿主启动，请刷新扩展候选后重试",
            )
        })?;
        process
            .execute_operation(&self.config, input, cancellation)
            .await
            .map_err(|failure| failure.error)
    }
}

/// 单个 LSP Server 的当前进程、管道、文档版本与通知缓存。
struct LspProcess {
    /// command-group 提供的跨平台进程树所有权。
    child: AsyncGroupChild,
    /// JSON-RPC 请求与通知唯一写端。
    stdin: ChildStdin,
    /// Reader 任务投递的响应与连接关闭事件。
    responses: mpsc::Receiver<ReaderEvent>,
    /// publishDiagnostics 的并发安全最新值。
    diagnostics: Arc<Mutex<DiagnosticsState>>,
    /// 每次 publishDiagnostics 递增的可等待代次。
    diagnostic_updates: watch::Receiver<u64>,
    /// 已向当前进程发送过 didOpen 的文件及版本摘要。
    open_documents: HashMap<PathBuf, OpenDocument>,
    /// 下一次 JSON-RPC 请求使用的正整数 ID。
    next_request_id: u64,
    /// 回应 workspace/workspaceFolders 时使用的冻结项目描述。
    workspace_folders: Value,
    /// 拥有 stdout 的后台 Reader 任务。
    reader_task: JoinHandle<()>,
    /// 持续排空 stderr、防止子进程阻塞的后台任务。
    stderr_task: JoinHandle<()>,
}

impl LspProcess {
    /// 启动进程组，完成 initialize/initialized 握手后返回可用连接。
    async fn start(
        config: &LspServerConfig,
        project_root: &Path,
        cancellation: &TurnCancellation,
    ) -> Result<Self, LspCallFailure> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(&config.current_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in &config.environment {
            command.env(name, value);
        }
        let mut group = command.group();
        group.kill_on_drop(true);
        #[cfg(windows)]
        group.creation_flags(0x0800_0000);
        let mut child = group.spawn().map_err(|error| LspCallFailure {
            restartable: error.kind() != io::ErrorKind::NotFound,
            error: ToolError::permanent(
                "lsp_spawn_failed",
                format!("无法启动 LSP Server {}：{error}", config.name),
            ),
            response_code: None,
        })?;
        let stdin = child.inner().stdin.take().ok_or_else(|| LspCallFailure {
            restartable: true,
            error: ToolError::retryable("lsp_pipe_unavailable", "LSP stdin 管道不可用"),
            response_code: None,
        })?;
        let stdout = child.inner().stdout.take().ok_or_else(|| LspCallFailure {
            restartable: true,
            error: ToolError::retryable("lsp_pipe_unavailable", "LSP stdout 管道不可用"),
            response_code: None,
        })?;
        let stderr = child.inner().stderr.take().ok_or_else(|| LspCallFailure {
            restartable: true,
            error: ToolError::retryable("lsp_pipe_unavailable", "LSP stderr 管道不可用"),
            response_code: None,
        })?;
        let diagnostics = Arc::new(Mutex::new(DiagnosticsState::default()));
        let (responses_tx, responses) = mpsc::channel(LSP_RESPONSE_CHANNEL_CAPACITY);
        let (updates_tx, diagnostic_updates) = watch::channel(0_u64);
        let reader_task = tokio::spawn(read_server_messages(
            stdout,
            responses_tx,
            Arc::clone(&diagnostics),
            updates_tx,
        ));
        let stderr_task = tokio::spawn(drain_stderr(stderr));
        let root_uri = file_uri(project_root)?;
        let workspace_folders = json!([{
            "uri": root_uri.clone(),
            "name": project_root
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("workspace")
        }]);
        let mut process = Self {
            child,
            stdin,
            responses,
            diagnostics,
            diagnostic_updates,
            open_documents: HashMap::new(),
            next_request_id: 1,
            workspace_folders: workspace_folders.clone(),
            reader_task,
            stderr_task,
        };
        let initialize = json!({
            "processId": null,
            "clientInfo": { "name": "KeenCode", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "workspaceFolders": workspace_folders,
            "capabilities": {
                "workspace": { "workspaceFolders": true },
                "textDocument": {
                    "publishDiagnostics": { "relatedInformation": true },
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "definition": { "linkSupport": true },
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true }
                }
            },
            "initializationOptions": config.initialization_options.clone().unwrap_or(Value::Null),
            "trace": "off"
        });
        let startup_timeout = Duration::from_millis(config.startup_timeout_ms);
        if let Err(failure) = process
            .request("initialize", initialize, cancellation, startup_timeout)
            .await
        {
            process.terminate().await;
            return Err(failure);
        }
        if let Err(failure) = process.notify("initialized", json!({}), cancellation).await {
            process.terminate().await;
            return Err(failure);
        }
        Ok(process)
    }

    /// 将一个公开操作转换为标准 LSP 请求或诊断等待。
    async fn execute_operation(
        &mut self,
        config: &LspServerConfig,
        input: &LspInput,
        cancellation: &TurnCancellation,
    ) -> Result<Value, LspCallFailure> {
        if let Some(status) = self.child.try_wait().map_err(process_wait_failure)? {
            return Err(LspCallFailure {
                restartable: true,
                error: ToolError::retryable(
                    "lsp_process_exited",
                    format!("LSP Server 已退出：{status}"),
                ),
                response_code: None,
            });
        }
        let document = if let Some(file) = &input.file {
            Some(
                self.synchronize_document(config, file, cancellation)
                    .await?,
            )
        } else {
            None
        };
        let position = || {
            json!({
                "line": input.line.expect("位置操作已校验 line") - 1,
                "character": input.character.expect("位置操作已校验 character") - 1
            })
        };
        match input.operation {
            LspOperation::Diagnostics => {
                let document = document.expect("诊断操作已校验 file");
                self.read_diagnostics(&document.uri, cancellation).await
            }
            LspOperation::Hover => {
                let document = document.expect("悬停操作已校验 file");
                self.request(
                    "textDocument/hover",
                    json!({ "textDocument": { "uri": document.uri }, "position": position() }),
                    cancellation,
                    LSP_REQUEST_TIMEOUT,
                )
                .await
            }
            LspOperation::Definition => {
                let document = document.expect("定义操作已校验 file");
                self.request(
                    "textDocument/definition",
                    json!({ "textDocument": { "uri": document.uri }, "position": position() }),
                    cancellation,
                    LSP_REQUEST_TIMEOUT,
                )
                .await
            }
            LspOperation::References => {
                let document = document.expect("引用操作已校验 file");
                self.request(
                    "textDocument/references",
                    json!({
                        "textDocument": { "uri": document.uri },
                        "position": position(),
                        "context": { "includeDeclaration": true }
                    }),
                    cancellation,
                    LSP_REQUEST_TIMEOUT,
                )
                .await
            }
            LspOperation::DocumentSymbols => {
                let document = document.expect("文档符号操作已校验 file");
                self.request(
                    "textDocument/documentSymbol",
                    json!({ "textDocument": { "uri": document.uri } }),
                    cancellation,
                    LSP_REQUEST_TIMEOUT,
                )
                .await
            }
            LspOperation::WorkspaceSymbols => {
                self.request(
                    "workspace/symbol",
                    json!({ "query": input.query.as_deref().unwrap_or_default() }),
                    cancellation,
                    LSP_REQUEST_TIMEOUT,
                )
                .await
            }
        }
    }

    /// 打开新文件或在正文变化时发送全量 didChange。
    async fn synchronize_document(
        &mut self,
        config: &LspServerConfig,
        file: &Path,
        cancellation: &TurnCancellation,
    ) -> Result<DocumentIdentity, LspCallFailure> {
        let text = tokio::fs::read_to_string(file)
            .await
            .map_err(|error| LspCallFailure {
                restartable: false,
                error: ToolError::permanent(
                    "lsp_document_read_failed",
                    format!("无法读取 LSP 文件：{error}"),
                ),
                response_code: None,
            })?;
        if text.len() as u64 > MAX_LSP_DOCUMENT_BYTES {
            return Err(LspCallFailure {
                restartable: false,
                error: ToolError::permanent(
                    "lsp_document_too_large",
                    format!("LSP 文件超过 {MAX_LSP_DOCUMENT_BYTES} 字节上限"),
                ),
                response_code: None,
            });
        }
        let extension = normalized_file_extension(file).map_err(non_restartable)?;
        let language_id = config
            .extension_to_language
            .get(&extension)
            .cloned()
            .unwrap_or_else(|| extension.clone());
        let uri = file_uri(file)?;
        let digest = text_digest(&text);
        match self.open_documents.get(file) {
            Some(document) if document.digest == digest => {}
            Some(document) => {
                let version = document.version.saturating_add(1);
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [{ "text": text }]
                    }),
                    cancellation,
                )
                .await?;
                self.open_documents
                    .insert(file.to_path_buf(), OpenDocument { version, digest });
            }
            None => {
                self.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id,
                            "version": 1,
                            "text": text
                        }
                    }),
                    cancellation,
                )
                .await?;
                self.open_documents
                    .insert(file.to_path_buf(), OpenDocument { version: 1, digest });
            }
        }
        Ok(DocumentIdentity { uri })
    }

    /// 优先使用标准 pull diagnostics，不支持时回退到 publishDiagnostics 缓存。
    async fn read_diagnostics(
        &mut self,
        uri: &str,
        cancellation: &TurnCancellation,
    ) -> Result<Value, LspCallFailure> {
        let before = self
            .diagnostics
            .lock()
            .map_err(|_| poisoned_diagnostics())?
            .generation;
        match self
            .request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri } }),
                cancellation,
                LSP_REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(result) => return Ok(result),
            Err(failure) if failure.response_code == Some(-32601) => {}
            Err(failure) => return Err(failure),
        }
        let mut updates = self.diagnostic_updates.clone();
        if *updates.borrow() <= before {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(non_restartable(ToolError::permanent("cancelled", "LSP 调用已取消")));
                }
                _ = timeout(LSP_DIAGNOSTICS_WAIT, updates.changed()) => {}
            }
        }
        let state = self
            .diagnostics
            .lock()
            .map_err(|_| poisoned_diagnostics())?;
        Ok(state.by_uri.get(uri).cloned().unwrap_or_else(|| json!([])))
    }

    /// 写入一个请求并等待相同 ID 的响应、取消、超时或连接关闭。
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        cancellation: &TurnCancellation,
        request_timeout: Duration,
    ) -> Result<Value, LspCallFailure> {
        let id = self.next_request_id;
        self.next_request_id =
            self.next_request_id
                .checked_add(1)
                .ok_or_else(|| LspCallFailure {
                    restartable: true,
                    error: ToolError::retryable("lsp_request_id_exhausted", "LSP 请求 ID 已耗尽"),
                    response_code: None,
                })?;
        self.write_message(
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }),
            cancellation,
            request_timeout,
        )
        .await?;
        let deadline = Instant::now() + request_timeout;
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(non_restartable(ToolError::permanent("cancelled", "LSP 调用已取消")));
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(LspCallFailure {
                        restartable: true,
                        error: ToolError::retryable("lsp_request_timeout", format!("LSP 请求 {method} 超时")),
                        response_code: None,
                    });
                }
                event = self.responses.recv() => {
                    match event {
                        Some(ReaderEvent::Response(response)) if response.id == json!(id) => {
                            if let Some(error) = response.error {
                                let code = error.get("code").and_then(Value::as_i64);
                                let message = error
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .map(bounded_server_error_message)
                                    .unwrap_or_else(|| "LSP Server 返回未知错误".to_owned());
                                return Err(LspCallFailure {
                                    restartable: false,
                                    error: ToolError::permanent("lsp_response_error", format!("LSP 请求 {method} 失败：{message}")),
                                    response_code: code,
                                });
                            }
                            return Ok(response.result.unwrap_or(Value::Null));
                        }
                        Some(ReaderEvent::Response(_)) => {
                            // 已取消或超时请求的迟到响应不属于当前调用，安全丢弃。
                        }
                        Some(ReaderEvent::ServerRequest(request)) => {
                            self.respond_to_server_request(request, cancellation).await?;
                        }
                        Some(ReaderEvent::Closed(message)) => {
                            return Err(LspCallFailure {
                                restartable: true,
                                error: ToolError::retryable("lsp_connection_closed", message),
                                response_code: None,
                            });
                        }
                        None => {
                            return Err(LspCallFailure {
                                restartable: true,
                                error: ToolError::retryable("lsp_connection_closed", "LSP Reader 已结束"),
                                response_code: None,
                            });
                        }
                    }
                }
            }
        }
    }

    /// 写入一个无需响应的 JSON-RPC 通知。
    async fn notify(
        &mut self,
        method: &str,
        params: Value,
        cancellation: &TurnCancellation,
    ) -> Result<(), LspCallFailure> {
        self.write_message(
            &json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            }),
            cancellation,
            LSP_REQUEST_TIMEOUT,
        )
        .await
    }

    /// 对常见动态注册与配置请求返回最小只读能力，拒绝 Server 发起的编辑。
    async fn respond_to_server_request(
        &mut self,
        request: IncomingServerRequest,
        cancellation: &TurnCancellation,
    ) -> Result<(), LspCallFailure> {
        let response = match request.method.as_str() {
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create" => {
                json!({ "jsonrpc": "2.0", "id": request.id, "result": null })
            }
            "workspace/configuration" => {
                let count = request
                    .params
                    .as_ref()
                    .and_then(|params| params.get("items"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                json!({
                    "jsonrpc": "2.0",
                    "id": request.id,
                    "result": vec![Value::Null; count]
                })
            }
            "workspace/workspaceFolders" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": request.id,
                    "result": self.workspace_folders.clone()
                })
            }
            "workspace/applyEdit" => json!({
                "jsonrpc": "2.0",
                "id": request.id,
                "result": {
                    "applied": false,
                    "failureReason": "KeenCode LSP 工具只提供只读查询"
                }
            }),
            "window/showMessageRequest" => {
                json!({ "jsonrpc": "2.0", "id": request.id, "result": null })
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": request.id,
                "error": { "code": -32601, "message": "KeenCode 不支持此 Server 请求" }
            }),
        };
        self.write_message(&response, cancellation, LSP_REQUEST_TIMEOUT)
            .await
    }

    /// 按 Content-Length framing 写入一个有界 UTF-8 JSON 消息。
    async fn write_message(
        &mut self,
        message: &Value,
        cancellation: &TurnCancellation,
        write_timeout: Duration,
    ) -> Result<(), LspCallFailure> {
        let body = serde_json::to_vec(message).map_err(|_| LspCallFailure {
            restartable: false,
            error: ToolError::permanent("lsp_encode_failed", "无法编码 LSP JSON-RPC 消息"),
            response_code: None,
        })?;
        if body.len() > MAX_LSP_MESSAGE_BYTES {
            return Err(non_restartable(ToolError::permanent(
                "lsp_message_too_large",
                format!("LSP 消息超过 {MAX_LSP_MESSAGE_BYTES} 字节上限"),
            )));
        }
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let write = async {
            self.stdin.write_all(header.as_bytes()).await?;
            self.stdin.write_all(&body).await?;
            self.stdin.flush().await
        };
        tokio::select! {
            _ = cancellation.cancelled() => Err(LspCallFailure {
                restartable: true,
                error: ToolError::permanent("cancelled", "LSP 调用已取消"),
                response_code: None,
            }),
            result = timeout(write_timeout, write) => match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(write_failure(error)),
                Err(_) => Err(LspCallFailure {
                    restartable: true,
                    error: ToolError::retryable("lsp_write_timeout", "写入 LSP stdin 超时"),
                    response_code: None,
                }),
            }
        }
    }

    /// 强制终止完整进程组并回收后台 Reader。
    async fn terminate(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        self.reader_task.abort();
        self.stderr_task.abort();
    }
}

impl Drop for LspProcess {
    /// 异常退出或候选释放时终止完整进程树并停止管道任务。
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.reader_task.abort();
        self.stderr_task.abort();
    }
}

/// 尚未启动或已经终止的 Server 状态。
#[derive(Default)]
struct LspServerState {
    /// 当前可复用的初始化完成进程。
    process: Option<LspProcess>,
}

/// 当前进程已同步的文件版本与正文摘要。
struct OpenDocument {
    /// didOpen 从 1 开始、每次正文变化递增的版本。
    version: u64,
    /// 用于避免重复 didChange 的非安全正文摘要。
    digest: u64,
}

/// 文件型请求复用的 URI。
struct DocumentIdentity {
    /// 由 url crate 生成的标准 file URI。
    uri: String,
}

/// Reader 投递给串行请求端的事件。
enum ReaderEvent {
    /// 一个携带 ID 的 JSON-RPC 响应。
    Response(IncomingResponse),
    /// Server 主动发起且必须由客户端完成的 JSON-RPC 请求。
    ServerRequest(IncomingServerRequest),
    /// stdout 结束或协议 framing 失效。
    Closed(String),
}

/// 一个尚未由请求端匹配 ID 的响应。
struct IncomingResponse {
    /// JSON-RPC 字符串或数值 ID。
    id: Value,
    /// 成功响应的可选结果。
    result: Option<Value>,
    /// 失败响应的标准 error 对象。
    error: Option<Value>,
}

/// 一个等待客户端返回结果的 Server 主动请求。
struct IncomingServerRequest {
    /// 原样回传的 JSON-RPC ID。
    id: Value,
    /// 决定只读响应策略的标准方法名。
    method: String,
    /// 仅用于确定 workspace/configuration 返回数组长度的参数。
    params: Option<Value>,
}

/// publishDiagnostics 的最新有界投影。
#[derive(Default)]
pub(crate) struct DiagnosticsState {
    /// 每次收到合法通知后递增的代次。
    pub(crate) generation: u64,
    /// 按 file URI 保存的 diagnostics 数组。
    pub(crate) by_uri: HashMap<String, Value>,
    /// 从最久未更新到最新更新排列的 URI，用于固定容量淘汰。
    pub(crate) uri_order: VecDeque<String>,
}

/// 区分宿主受控启动阶段可重试的进程故障和确定性请求错误。
struct LspCallFailure {
    /// 宿主启动阶段是否可按 `max_restarts` 重试；只读工具不会据此重启进程。
    restartable: bool,
    /// 对模型安全的稳定工具错误。
    error: ToolError,
    /// Server JSON-RPC error 的可选数值码。
    response_code: Option<i64>,
}

/// 构造宿主启动阶段也不可重试的确定性失败。
fn non_restartable(error: ToolError) -> LspCallFailure {
    LspCallFailure {
        restartable: false,
        error,
        response_code: None,
    }
}

/// 将进程状态查询错误标记为宿主下次刷新候选时可恢复的故障。
fn process_wait_failure(error: io::Error) -> LspCallFailure {
    LspCallFailure {
        restartable: true,
        error: ToolError::retryable(
            "lsp_process_wait_failed",
            format!("无法读取 LSP 进程状态：{error}"),
        ),
        response_code: None,
    }
}

/// 将 stdin 写入错误标记为宿主下次刷新候选时可恢复的故障。
fn write_failure(error: io::Error) -> LspCallFailure {
    LspCallFailure {
        restartable: true,
        error: ToolError::retryable("lsp_write_failed", format!("写入 LSP stdin 失败：{error}")),
        response_code: None,
    }
}

/// 返回诊断缓存锁中毒的确定性内部错误。
fn poisoned_diagnostics() -> LspCallFailure {
    non_restartable(ToolError::permanent(
        "lsp_state_unavailable",
        "LSP 诊断缓存不可用",
    ))
}

/// 持续读取 stdout，分流响应与 publishDiagnostics 通知。
async fn read_server_messages<R>(
    stdout: R,
    responses: mpsc::Sender<ReaderEvent>,
    diagnostics: Arc<Mutex<DiagnosticsState>>,
    updates: watch::Sender<u64>,
) where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stdout);
    loop {
        let message = match read_lsp_message(&mut reader).await {
            Ok(message) => message,
            Err(error) => {
                let _ = responses
                    .send(ReaderEvent::Closed(format!(
                        "LSP stdout 已关闭或协议无效：{error}"
                    )))
                    .await;
                return;
            }
        };
        if let (Some(id), Some(method)) = (
            message.get("id").cloned(),
            message.get("method").and_then(Value::as_str),
        ) {
            if responses
                .send(ReaderEvent::ServerRequest(IncomingServerRequest {
                    id,
                    method: method.to_owned(),
                    params: message.get("params").cloned(),
                }))
                .await
                .is_err()
            {
                return;
            }
            continue;
        }
        if let Some(id) = message.get("id").cloned()
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            if responses
                .send(ReaderEvent::Response(IncomingResponse {
                    id,
                    result: message.get("result").cloned(),
                    error: message.get("error").cloned(),
                }))
                .await
                .is_err()
            {
                return;
            }
            continue;
        }
        if message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && let Some(params) = message.get("params")
            && let (Some(uri), Some(items)) = (
                params.get("uri").and_then(Value::as_str),
                params.get("diagnostics").and_then(Value::as_array),
            )
            && let Ok(mut state) = diagnostics.lock()
        {
            if let Some(generation) = update_diagnostics_cache(&mut state, uri, items) {
                drop(state);
                updates.send_replace(generation);
            }
        }
    }
}

/// 按 LRU 顺序更新固定容量诊断缓存，并拒绝异常大的 URI 键。
pub(crate) fn update_diagnostics_cache(
    state: &mut DiagnosticsState,
    uri: &str,
    items: &[Value],
) -> Option<u64> {
    if uri.len() > MAX_LSP_DIAGNOSTIC_URI_BYTES {
        return None;
    }
    if let Some(position) = state.uri_order.iter().position(|known| known == uri) {
        state.uri_order.remove(position);
    } else if state.by_uri.len() >= MAX_LSP_DIAGNOSTIC_DOCUMENTS
        && let Some(evicted) = state.uri_order.pop_front()
    {
        state.by_uri.remove(&evicted);
    }
    state.uri_order.push_back(uri.to_owned());
    state
        .by_uri
        .insert(uri.to_owned(), bounded_diagnostics(items));
    state.generation = state.generation.saturating_add(1);
    Some(state.generation)
}

/// 在缓存阶段同时限制诊断数量与序列化容量，避免恶意 Server 长期占用内存。
pub(crate) fn bounded_diagnostics(items: &[Value]) -> Value {
    let mut retained = Vec::new();
    let mut bytes = 2_usize;
    for item in items.iter().take(MAX_LSP_DIAGNOSTICS) {
        let Ok(encoded) = serde_json::to_vec(item) else {
            continue;
        };
        let additional = encoded.len().saturating_add(1);
        if bytes.saturating_add(additional) > MAX_LSP_OUTPUT_BYTES / 2 {
            break;
        }
        bytes = bytes.saturating_add(additional);
        retained.push(item.clone());
    }
    Value::Array(retained)
}

/// 清理控制字符并在 UTF-8 边界内限制不可信 Server 错误说明。
pub(crate) fn bounded_server_error_message(value: &str) -> String {
    let mut bounded = String::with_capacity(value.len().min(MAX_LSP_SERVER_ERROR_BYTES));
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if bounded.len().saturating_add(character.len_utf8()) > MAX_LSP_SERVER_ERROR_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

/// 为扩展诊断保存安全的 Server 名称；非法名称统一隐藏原文。
fn diagnostic_server_name(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_LSP_SERVER_NAME_BYTES
        || trimmed.chars().any(char::is_control)
    {
        "<invalid>".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// 清理控制字符并按 UTF-8 边界截断 LSP 诊断说明。
fn bounded_lsp_diagnostic_message(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() <= MAX_LSP_DIAGNOSTIC_MESSAGE_BYTES {
        return sanitized;
    }
    let mut end = MAX_LSP_DIAGNOSTIC_MESSAGE_BYTES;
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    sanitized[..end].to_owned()
}

/// 持续排空 stderr，不把不可信 Server 文本复制到模型输出或无界缓存。
async fn drain_stderr<R>(mut stderr: R)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

/// 读取一个严格 Content-Length framing 的有界 LSP JSON 对象。
pub(crate) async fn read_lsp_message<R>(reader: &mut R) -> Result<Value, io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut content_length = None;
    let mut header_bytes = 0_usize;
    loop {
        let line = read_bounded_header_line(reader, &mut header_bytes).await?;
        if line.is_empty() {
            break;
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "LSP Header 不是 UTF-8"))?;
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP Header 缺少冒号",
            ));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP Content-Length 重复",
                ));
            }
            let length = value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "LSP Content-Length 无效")
            })?;
            if length == 0 || length > MAX_LSP_MESSAGE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP Content-Length 超出上限",
                ));
            }
            content_length = Some(length);
        }
    }
    let length = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "LSP 消息缺少 Content-Length"))?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "LSP 正文不是有效 JSON"))?;
    if !value.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP 正文必须是 JSON 对象",
        ));
    }
    Ok(value)
}

/// 逐字节读取一行 Header，在分配前执行总容量上限。
async fn read_bounded_header_line<R>(
    reader: &mut R,
    total: &mut usize,
) -> Result<Vec<u8>, io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        reader.read_exact(&mut byte).await?;
        *total = total.saturating_add(1);
        if *total > MAX_LSP_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP Header 超出上限",
            ));
        }
        if byte[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
        line.push(byte[0]);
    }
}

/// 校验配置并把扩展名键归一为不含前导点的小写形式。
fn validate_server_config(config: &mut LspServerConfig) -> Result<(), LspRuntimeError> {
    config.name = config.name.trim().to_owned();
    if config.name.is_empty()
        || config.name.len() > MAX_LSP_SERVER_NAME_BYTES
        || config.name.chars().any(char::is_control)
    {
        return Err(LspRuntimeError::new("LSP Server 名称无效"));
    }
    if config.command.trim().is_empty()
        || config
            .command
            .chars()
            .any(|character| matches!(character, '\0' | '\r' | '\n'))
    {
        return Err(LspRuntimeError::new(format!(
            "LSP Server {} 的 command 无效",
            config.name
        )));
    }
    if config.args.iter().any(|argument| argument.contains('\0')) {
        return Err(LspRuntimeError::new(format!(
            "LSP Server {} 的 args 包含空字符",
            config.name
        )));
    }
    if config.startup_timeout_ms == 0 || config.startup_timeout_ms > 10 * 60 * 1_000 {
        return Err(LspRuntimeError::new(format!(
            "LSP Server {} 的启动超时必须在 1..=600000 毫秒内",
            config.name
        )));
    }
    if config.max_restarts > 16 {
        return Err(LspRuntimeError::new(format!(
            "LSP Server {} 的候选启动重试次数不能超过 16",
            config.name
        )));
    }
    let mut normalized = BTreeMap::new();
    for (extension, language_id) in std::mem::take(&mut config.extension_to_language) {
        let extension = extension
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        let language_id = language_id.trim().to_owned();
        if extension.is_empty()
            || extension.contains(['/', '\\', '\0'])
            || language_id.is_empty()
            || language_id.chars().any(char::is_control)
        {
            return Err(LspRuntimeError::new(format!(
                "LSP Server {} 的 extensionToLanguage 无效",
                config.name
            )));
        }
        if normalized.insert(extension.clone(), language_id).is_some() {
            return Err(LspRuntimeError::new(format!(
                "LSP Server {} 的扩展名映射归一后重复：{extension}",
                config.name
            )));
        }
    }
    if normalized.is_empty() {
        return Err(LspRuntimeError::new(format!(
            "LSP Server {} 必须声明 extensionToLanguage",
            config.name
        )));
    }
    config.extension_to_language = normalized;
    Ok(())
}

/// 规范化一个必须存在的目录。
fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, LspRuntimeError> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| LspRuntimeError::new(format!("无法解析{label}：{error}")))?;
    if !canonical.is_dir() {
        return Err(LspRuntimeError::new(format!("{label}不是目录")));
    }
    Ok(canonical)
}

/// 取得文件的小写扩展名并拒绝无扩展名输入。
fn normalized_file_extension(file: &Path) -> Result<String, ToolError> {
    file.extension()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ToolError::permanent("lsp_extension_missing", "LSP 文件缺少有效扩展名"))
}

/// 使用 url crate 将现有绝对路径转换为标准 file URI。
fn file_uri(path: &Path) -> Result<String, LspCallFailure> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|()| {
            non_restartable(ToolError::permanent(
                "lsp_file_uri_failed",
                "无法将本地路径转换为 LSP file URI",
            ))
        })
}

/// 计算仅用于同一进程内变化检测的正文摘要。
fn text_digest(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// 将一个 LSP 结果包装为稳定、有界的模型输出。
fn render_result(
    server: &str,
    operation: LspOperation,
    result: Value,
) -> Result<ToolOutput, ToolError> {
    let value = json!({
        "server": server,
        "operation": operation.as_str(),
        "result": result
    });
    let text = serde_json::to_string_pretty(&value)
        .map_err(|_| ToolError::permanent("lsp_result_encode_failed", "无法编码 LSP 工具结果"))?;
    if text.len() > MAX_LSP_OUTPUT_BYTES {
        return Err(ToolError::permanent(
            "lsp_output_too_large",
            format!("LSP 结果超过 {MAX_LSP_OUTPUT_BYTES} 字节上限"),
        ));
    }
    Ok(ToolOutput::text(text))
}
