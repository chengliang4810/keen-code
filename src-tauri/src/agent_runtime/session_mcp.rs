//! Session 独立 MCP 目录、动态变更收据与连接生命周期。

use super::{AgentRuntime, AgentRuntimeError, RuntimeExtensionCandidate, canonical_project_root};
use keencode_acp::schema;
use keencode_acp::{
    McpTransportKind, SessionMcpMutationResponse, SessionMcpServerPhase, SessionMcpServerStatus,
    SessionMcpStatusResponse,
};
use keencode_agent::{
    AgentTool, AgentToolCatalogDelta, AgentToolCatalogUpdateError, AgentToolCatalogUpdateSource,
    ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutputArtifactSink,
};
use keencode_mcp::{
    McpClient, McpClientOptions, McpServerConfig, StdioServerConfig, StreamableHttpConfig,
};
use keencode_model::ToolDefinition;
use keencode_tools::{DeferredToolCatalog, McpDiagnosticCode, prepare_mcp_server_tools};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

/// 单个 Session 可同时保留的标准 MCP Server 上限。
const MAX_SESSION_MCP_SERVERS: usize = 512;
/// 单个 HTTP Server 允许携带的 Header 数量。
const MAX_MCP_HEADERS: usize = 256;
/// 单个 stdio Server 允许携带的环境变量数量。
const MAX_MCP_ENVIRONMENT: usize = 256;
/// 单个 stdio Server 允许携带的参数数量。
const MAX_MCP_ARGUMENTS: usize = 1_024;
/// Server、Header 与环境变量名称的最大 UTF-8 字节数。
const MAX_MCP_NAME_BYTES: usize = 256;
/// URL、命令、单个参数或 Header 值的最大 UTF-8 字节数。
const MAX_MCP_CONFIG_TEXT_BYTES: usize = 64 * 1_024;
/// 单个环境变量值的最大 UTF-8 字节数。
const MAX_MCP_ENV_VALUE_BYTES: usize = 1024 * 1_024;
/// 单个 Session 保留的动态操作收据上限。
const MAX_SESSION_MCP_RECEIPTS: usize = 4_096;

/// Session MCP 控制面拒绝请求的稳定分类；不携带配置或远端错误正文。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionMcpError {
    /// Session、Server 或传输配置不满足边界。
    InvalidConfiguration,
    /// 相同 operationId 被用于不同的业务载荷。
    OperationConflict,
    /// Session 不存在或尚未打开。
    SessionUnavailable,
    /// Session 与调用方给出的项目根不一致。
    ProjectMismatch,
    /// 目录候选与已有 Server 或工具名称冲突。
    CatalogConflict,
    /// 共享状态或目录换代失败。
    StateUnavailable,
    /// Runtime 或 Session MCP 生命周期已经关闭。
    Closed,
}

impl fmt::Display for SessionMcpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "Session MCP 配置无效",
            Self::OperationConflict => "Session MCP operationId 已绑定其他载荷",
            Self::SessionUnavailable => "Session MCP 目标不存在",
            Self::ProjectMismatch => "Session MCP 项目根不匹配",
            Self::CatalogConflict => "Session MCP 目录存在名称冲突",
            Self::StateUnavailable => "Session MCP 状态不可用",
            Self::Closed => "Session MCP 已关闭",
        })
    }
}

/// 项目 MCP 快照的发布身份；撤销与原候选代次明确区分。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ProjectCatalogVersion {
    generation: u64,
    revoked: bool,
}

impl ProjectCatalogVersion {
    /// 比较项目候选发布身份；更高代次优先，同代次撤销状态优先。
    fn supersedes(self, current: Self) -> bool {
        self.generation > current.generation
            || (self.generation == current.generation && self.revoked && !current.revoked)
    }
}

/// 一个不共享项目可变目录的项目 MCP 工具快照。
struct ProjectToolSnapshot {
    version: ProjectCatalogVersion,
    server_names: BTreeSet<String>,
    tools: Vec<Arc<dyn AgentTool>>,
}

impl ProjectToolSnapshot {
    fn empty() -> Self {
        Self {
            version: ProjectCatalogVersion::default(),
            server_names: BTreeSet::new(),
            tools: Vec::new(),
        }
    }
}

/// ACP 配置完成严格转换后的短生命周期连接输入。
struct PreparedServerConfig {
    name: String,
    transport: McpTransportKind,
    fingerprint: String,
    config: McpServerConfig,
}

/// 单个 Session MCP 客户端的共享关闭闸门。
struct SessionMcpClientLease {
    client: McpClient,
    close_started: AtomicBool,
}

impl SessionMcpClientLease {
    fn new(client: McpClient) -> Self {
        Self {
            client,
            close_started: AtomicBool::new(false),
        }
    }

    async fn close(&self) {
        if !self.close_started.swap(true, Ordering::AcqRel) {
            let _ = self.client.close().await;
        }
    }
}

impl Drop for SessionMcpClientLease {
    fn drop(&mut self) {
        if self.close_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let client = self.client.clone();
        tauri::async_runtime::spawn(async move {
            let _ = client.close().await;
        });
    }
}

/// 在最后一个旧目录绑定释放前持有连接的透明工具包装器。
struct SessionMcpOwnedTool {
    inner: Arc<dyn AgentTool>,
    _lease: Arc<SessionMcpClientLease>,
}

impl AgentTool for SessionMcpOwnedTool {
    fn definition(&self) -> ToolDefinition {
        self.inner.definition()
    }

    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        self.inner.effect(input)
    }

    fn concurrency(&self) -> ToolConcurrency {
        self.inner.concurrency()
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        self.inner.timeout()
    }

    fn output_artifact_sink(&self) -> Option<Arc<dyn ToolOutputArtifactSink>> {
        self.inner.output_artifact_sink()
    }

    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        self.inner.execute(context, input)
    }
}

/// 已连接 Server 的安全运行态与工具实现；不保存原始配置正文。
struct SessionServerBinding {
    name: String,
    transport: McpTransportKind,
    config_fingerprint: String,
    tools: Vec<Arc<dyn AgentTool>>,
    lease: Arc<SessionMcpClientLease>,
}

impl SessionServerBinding {
    fn tools_count(&self) -> u32 {
        u32::try_from(self.tools.len()).unwrap_or(u32::MAX)
    }
}

/// 最近一次未发布失败的安全摘要。
#[derive(Clone)]
struct FailedServer {
    transport: McpTransportKind,
    error: String,
}

/// 幂等操作收据只保留请求摘要和安全响应。
#[derive(Clone)]
struct MutationReceipt {
    request_fingerprint: String,
    response: SessionMcpMutationResponse,
}

/// 最近一次尚未被成功模型请求确认的目录换代。
///
/// `from_definitions` 必须保留换代前的完整定义，而不是只保留名称集合：新建
/// Runner 可能发生在 `replace_exact` 已经发布之后，仍需要从旧快照生成通知。
struct PendingCatalogTransition {
    from_generation: u64,
    from_definitions: BTreeMap<String, ToolDefinition>,
    generation: u64,
}

/// Session 目录的已发布与待发布双缓冲状态。
struct SessionMcpState {
    current_project: ProjectToolSnapshot,
    desired_project: ProjectToolSnapshot,
    current_servers: BTreeMap<String, Arc<SessionServerBinding>>,
    desired_servers: BTreeMap<String, Arc<SessionServerBinding>>,
    failed_servers: BTreeMap<String, FailedServer>,
    receipts: BTreeMap<String, MutationReceipt>,
    receipt_order: VecDeque<String>,
    pending_catalog_transition: Option<PendingCatalogTransition>,
    closed: bool,
}

/// 一个 Session 唯一的合成延迟目录和 MCP 生命周期。
pub(super) struct SessionMcpRuntime {
    session_id: String,
    project_root: PathBuf,
    catalog: Arc<DeferredToolCatalog>,
    mutation_gate: AsyncMutex<()>,
    state: Mutex<SessionMcpState>,
}

/// Session 为原子日志变更临时关闭时保留的进程内 MCP 所有权。
pub(crate) struct SuspendedSessionMcp {
    runtime: Mutex<Option<Arc<SessionMcpRuntime>>>,
}

impl SessionMcpRuntime {
    fn new(
        session_id: String,
        project_root: PathBuf,
        project: ProjectToolSnapshot,
    ) -> Result<Arc<Self>, SessionMcpError> {
        let catalog = Arc::new(DeferredToolCatalog::new());
        catalog
            .replace_all(project.tools.clone())
            .map_err(|_| SessionMcpError::CatalogConflict)?;
        Ok(Arc::new(Self {
            session_id,
            project_root,
            catalog,
            mutation_gate: AsyncMutex::new(()),
            state: Mutex::new(SessionMcpState {
                current_project: clone_project_snapshot(&project),
                desired_project: project,
                current_servers: BTreeMap::new(),
                desired_servers: BTreeMap::new(),
                failed_servers: BTreeMap::new(),
                receipts: BTreeMap::new(),
                receipt_order: VecDeque::new(),
                pending_catalog_transition: None,
                closed: false,
            }),
        }))
    }

    fn catalog(&self) -> Arc<DeferredToolCatalog> {
        Arc::clone(&self.catalog)
    }

    fn update_source(self: &Arc<Self>) -> Arc<dyn AgentToolCatalogUpdateSource> {
        Arc::new(SessionToolCatalogUpdateSource::new(Arc::clone(self)))
    }

    fn ensure_project(&self, project_root: &Path) -> Result<(), SessionMcpError> {
        if self.project_root == project_root {
            Ok(())
        } else {
            Err(SessionMcpError::ProjectMismatch)
        }
    }

    /// 排队项目 MCP 新快照；旧候选被忽略，冲突候选仍保留等待后续解除。
    fn queue_project_snapshot(&self, project: ProjectToolSnapshot) -> Result<(), SessionMcpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        if state.closed {
            return Err(SessionMcpError::Closed);
        }
        if !project.version.supersedes(state.desired_project.version) {
            return Ok(());
        }
        for name in &project.server_names {
            state.failed_servers.remove(name);
        }
        let validation = validate_project_catalog(&project, &state.desired_servers);
        state.desired_project = project;
        validation
    }

    /// 动态加载一批 Server；连接与候选校验全部成功后才改变 desired state。
    async fn load(
        &self,
        operation_id: &str,
        servers: Vec<schema::McpServer>,
    ) -> Result<SessionMcpMutationResponse, SessionMcpError> {
        let _gate = self.mutation_gate.lock().await;
        let prepared = prepare_server_configs(servers, &self.project_root)?;
        let request_fingerprint = mutation_fingerprint("load", &prepared, None);
        if let Some(response) = self.receipt(operation_id, &request_fingerprint)? {
            return Ok(response);
        }

        let (project_names, desired, existing_fingerprints, tracked_names) = {
            let state = self
                .state
                .lock()
                .map_err(|_| SessionMcpError::StateUnavailable)?;
            if state.closed {
                return Err(SessionMcpError::Closed);
            }
            (
                state.desired_project.server_names.clone(),
                state.desired_servers.clone(),
                state
                    .desired_servers
                    .iter()
                    .map(|(name, server)| (name.clone(), server.config_fingerprint.clone()))
                    .collect::<BTreeMap<_, _>>(),
                tracked_server_names(&state),
            )
        };
        for server in &prepared {
            if project_names.contains(&server.name) {
                return Err(SessionMcpError::CatalogConflict);
            }
            if let Some(existing) = existing_fingerprints.get(&server.name)
                && existing != &server.fingerprint
            {
                return Err(SessionMcpError::CatalogConflict);
            }
        }
        if tracked_names.len().saturating_add(
            prepared
                .iter()
                .filter(|server| !tracked_names.contains(&server.name))
                .count(),
        ) > MAX_SESSION_MCP_SERVERS
        {
            return Err(SessionMcpError::InvalidConfiguration);
        }

        let mut additions = Vec::new();
        let mut failed = Vec::new();
        for server in prepared
            .into_iter()
            .filter(|server| !desired.contains_key(&server.name))
        {
            match connect_server(server).await {
                Ok(binding) => additions.push(binding),
                Err(failure) => failed.push(failure),
            }
        }
        if !failed.is_empty() {
            close_bindings(&additions).await;
            let response = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| SessionMcpError::StateUnavailable)?;
                // 项目候选可在连接等待期间换代；失败记录也必须在提交点重新
                // 校验 Server 名称，不能让项目候选与 failed 状态同名共存。
                validate_project_catalog(&state.desired_project, &state.desired_servers)?;
                if failed
                    .iter()
                    .any(|(name, _)| state.desired_project.server_names.contains(name))
                {
                    return Err(SessionMcpError::CatalogConflict);
                }
                for (name, failure) in failed {
                    state.failed_servers.insert(name, failure);
                }
                let response =
                    mutation_response_locked(&self.session_id, &self.catalog, &state, false, false);
                insert_receipt(
                    &mut state,
                    operation_id,
                    request_fingerprint,
                    response.clone(),
                );
                response
            };
            return Ok(response);
        }

        let closed = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?
            .closed;
        if closed {
            close_bindings(&additions).await;
            return Err(SessionMcpError::Closed);
        }
        let commit_result = (|| {
            let mut state = self
                .state
                .lock()
                .map_err(|_| SessionMcpError::StateUnavailable)?;
            if state.closed {
                return Err(SessionMcpError::Closed);
            }
            let mut candidate = state.desired_servers.clone();
            for binding in &additions {
                candidate.insert(binding.name.clone(), Arc::clone(binding));
            }
            // 项目候选可在连接等待期间换代；成功绑定提交前必须完整重验项目
            // Server 名称和工具目录，项目候选始终优先且冲突时不发布连接。
            validate_project_catalog(&state.desired_project, &candidate)?;
            let changed = !additions.is_empty();
            for binding in additions.drain(..) {
                state.failed_servers.remove(&binding.name);
                state.desired_servers.insert(binding.name.clone(), binding);
            }
            let response =
                mutation_response_locked(&self.session_id, &self.catalog, &state, changed, false);
            insert_receipt(
                &mut state,
                operation_id,
                request_fingerprint,
                response.clone(),
            );
            Ok(response)
        })();
        if let Err(error) = commit_result {
            // 候选尚未进入 desired state；显式等待新连接关闭，不能依赖 Drop
            // 异步清理把 HTTP DELETE 或 stdio 子进程回收延后到未知时点。
            close_bindings(&additions).await;
            return Err(error);
        }
        commit_result
    }

    /// 动态撤销单个 Session Server，并立即尝试发布解除冲突后的待处理目录。
    async fn unload(
        &self,
        operation_id: &str,
        server_name: &str,
    ) -> Result<SessionMcpMutationResponse, SessionMcpError> {
        let _gate = self.mutation_gate.lock().await;
        let request_fingerprint = mutation_fingerprint("unload", &[], Some(server_name));
        if let Some(response) = self.receipt(operation_id, &request_fingerprint)? {
            return Ok(response);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        if state.closed {
            return Err(SessionMcpError::Closed);
        }
        let changed = state.desired_servers.remove(server_name).is_some();
        state.failed_servers.remove(server_name);
        drop(state);
        // 卸载可能正好解除项目候选与动态 Server 的冲突；在同一变更闸门内
        // 立即发布最新待处理项目目录，避免必须等待下一次 Turn 才收敛。
        if changed {
            self.apply_pending()?;
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        let response =
            mutation_response_locked(&self.session_id, &self.catalog, &state, changed, false);
        insert_receipt(
            &mut state,
            operation_id,
            request_fingerprint,
            response.clone(),
        );
        Ok(response)
    }

    /// 标准 new/load/fork 使用完整列表替换 Session 配置，并在返回前直接发布。
    async fn replace_exact(&self, servers: Vec<schema::McpServer>) -> Result<(), SessionMcpError> {
        let _gate = self.mutation_gate.lock().await;
        let prepared = prepare_server_configs_allow_empty(servers, &self.project_root)?;
        let (project_names, existing) = {
            let state = self
                .state
                .lock()
                .map_err(|_| SessionMcpError::StateUnavailable)?;
            if state.closed {
                return Err(SessionMcpError::Closed);
            }
            (
                state.desired_project.server_names.clone(),
                state.desired_servers.clone(),
            )
        };
        if prepared
            .iter()
            .any(|server| project_names.contains(&server.name))
        {
            return Err(SessionMcpError::CatalogConflict);
        }

        let mut desired = BTreeMap::new();
        let mut new_bindings = Vec::new();
        for server in prepared {
            if let Some(binding) = existing.get(&server.name)
                && binding.config_fingerprint == server.fingerprint
            {
                desired.insert(server.name, Arc::clone(binding));
                continue;
            }
            match connect_server(server).await {
                Ok(binding) => {
                    desired.insert(binding.name.clone(), Arc::clone(&binding));
                    new_bindings.push(binding);
                }
                Err(_) => {
                    close_bindings(&new_bindings).await;
                    return Err(SessionMcpError::InvalidConfiguration);
                }
            }
        }
        let commit_result = (|| {
            let mut state = self
                .state
                .lock()
                .map_err(|_| SessionMcpError::StateUnavailable)?;
            if state.closed {
                return Err(SessionMcpError::Closed);
            }
            validate_project_catalog(&state.desired_project, &desired)?;
            state.desired_servers = desired;
            state.failed_servers.clear();
            state.receipts.clear();
            state.receipt_order.clear();
            Ok(())
        })();
        if let Err(error) = commit_result {
            // 连接期间项目候选可能已经换代；只有本次新建连接需要关闭，复用的
            // 既有绑定仍属于当前 desired state。
            close_bindings(&new_bindings).await;
            return Err(error);
        }
        self.apply_pending()?;
        Ok(())
    }

    fn status(&self) -> Result<SessionMcpStatusResponse, SessionMcpError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        if state.closed {
            return Err(SessionMcpError::Closed);
        }
        Ok(SessionMcpStatusResponse {
            session_id: self.session_id.clone(),
            catalog_generation: self.catalog.generation_and_definitions().0,
            servers: server_statuses(&state),
        })
    }

    /// 在模型采样前的同步安全边界原子发布一次待处理目录。
    fn apply_pending(&self) -> Result<(), SessionMcpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        if state.closed {
            return Err(SessionMcpError::Closed);
        }
        let project_changed = state.current_project.version != state.desired_project.version;
        let servers_changed = !same_server_map(&state.current_servers, &state.desired_servers);
        if !project_changed && !servers_changed {
            return Ok(());
        }
        // 项目候选可能在冲突时已进入 desired state；在真正替换目录前
        // 再次校验当前动态 Server，避免把名称或工具冲突发布到运行态。
        validate_project_catalog(&state.desired_project, &state.desired_servers)?;
        let tools = composite_tools(&state.desired_project.tools, &state.desired_servers);
        let (from_generation, from_definitions) = self.catalog.generation_and_definitions();
        self.catalog
            .replace_all(tools)
            .map_err(|_| SessionMcpError::CatalogConflict)?;
        let generation = self.catalog.generation_and_definitions().0;
        if let Some(pending) = state.pending_catalog_transition.as_mut() {
            // 如果上一个换代尚未被模型请求确认，继续保留最初的旧快照，
            // 让新的 Runner 一次看到从未确认前到最新目录的完整变化。
            pending.generation = generation;
        } else {
            state.pending_catalog_transition = Some(PendingCatalogTransition {
                from_generation,
                from_definitions: definition_map(&from_definitions),
                generation,
            });
        }
        state.current_project = clone_project_snapshot(&state.desired_project);
        state.current_servers = state.desired_servers.clone();
        Ok(())
    }

    /// Provider 成功接受指定目录代次后清除对应的全局待确认状态。
    fn acknowledge_catalog_update(&self, generation: u64) -> Result<(), SessionMcpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        if state
            .pending_catalog_transition
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            state.pending_catalog_transition = None;
        }
        Ok(())
    }

    /// 返回新建 Runner 应从哪个定义快照开始观察目录。
    fn initial_catalog_observation(&self) -> (u64, BTreeMap<String, ToolDefinition>) {
        let (generation, definitions) = self.catalog.generation_and_definitions();
        self.state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .pending_catalog_transition
                    .as_ref()
                    .map(|pending| (pending.from_generation, pending.from_definitions.clone()))
            })
            .unwrap_or_else(|| (generation, definition_map(&definitions)))
    }

    fn receipt(
        &self,
        operation_id: &str,
        request_fingerprint: &str,
    ) -> Result<Option<SessionMcpMutationResponse>, SessionMcpError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        let Some(receipt) = state.receipts.get(operation_id) else {
            return Ok(None);
        };
        if receipt.request_fingerprint != request_fingerprint {
            return Err(SessionMcpError::OperationConflict);
        }
        let mut response = receipt.response.clone();
        response.deduplicated = true;
        Ok(Some(response))
    }

    async fn close(&self) {
        let _gate = self.mutation_gate.lock().await;
        let leases = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
            if state.closed {
                return;
            }
            state.closed = true;
            let mut seen = HashSet::new();
            let leases = state
                .current_servers
                .values()
                .chain(state.desired_servers.values())
                .filter_map(|binding| {
                    let pointer = Arc::as_ptr(&binding.lease) as usize;
                    seen.insert(pointer).then(|| Arc::clone(&binding.lease))
                })
                .collect::<Vec<_>>();
            state.current_servers.clear();
            state.desired_servers.clear();
            state.failed_servers.clear();
            state.receipts.clear();
            state.receipt_order.clear();
            state.pending_catalog_transition = None;
            let _ = self.catalog.replace_all(Vec::new());
            leases
        };
        for lease in leases {
            lease.close().await;
        }
    }
}

/// 每个 Runner 独立保存观察水位，确保并发 Runner 各收到一次相同换代；
/// Provider 成功前保留本轮在途通知。
struct SessionToolCatalogUpdateSource {
    runtime: Arc<SessionMcpRuntime>,
    observed: Mutex<ObservedCatalog>,
}

/// 一个 Runner 对目录的本地观察水位和尚未确认的本轮通知。
struct ObservedCatalog {
    generation: u64,
    definitions: BTreeMap<String, ToolDefinition>,
    in_flight: Option<InFlightCatalogUpdate>,
}

/// Provider 成功前冻结的目录变化与其目标定义快照。
struct InFlightCatalogUpdate {
    delta: AgentToolCatalogDelta,
    target_generation: u64,
    target_definitions: BTreeMap<String, ToolDefinition>,
}

impl SessionToolCatalogUpdateSource {
    fn new(runtime: Arc<SessionMcpRuntime>) -> Self {
        let (generation, definitions) = runtime.initial_catalog_observation();
        Self {
            runtime,
            observed: Mutex::new(ObservedCatalog {
                generation,
                definitions,
                in_flight: None,
            }),
        }
    }
}

impl AgentToolCatalogUpdateSource for SessionToolCatalogUpdateSource {
    fn take_update(&self) -> Result<Option<AgentToolCatalogDelta>, AgentToolCatalogUpdateError> {
        self.runtime
            .apply_pending()
            .map_err(|error| AgentToolCatalogUpdateError::new(error.to_string()))?;
        let (generation, definitions) = self.runtime.catalog.generation_and_definitions();
        let definitions = definition_map(&definitions);
        let mut observed = self
            .observed
            .lock()
            .map_err(|_| AgentToolCatalogUpdateError::new("工具目录观察水位不可用"))?;
        if let Some(in_flight) = &observed.in_flight {
            return Ok(Some(in_flight.delta.clone()));
        }
        if observed.generation == generation {
            return Ok(None);
        }
        let added = definitions
            .keys()
            .filter(|name| !observed.definitions.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        let removed = observed
            .definitions
            .keys()
            .filter(|name| !definitions.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        let changed = definitions
            .iter()
            .filter_map(|(name, definition)| {
                observed
                    .definitions
                    .get(name)
                    .filter(|previous| *previous != definition)
                    .map(|_| name.clone())
            })
            .collect::<Vec<_>>();
        let delta = AgentToolCatalogDelta::new_with_changed(generation, added, removed, changed)
            .map_err(|error| AgentToolCatalogUpdateError::new(error.message().to_owned()))?;
        observed.in_flight = Some(InFlightCatalogUpdate {
            delta: delta.clone(),
            target_generation: generation,
            target_definitions: definitions,
        });
        Ok(Some(delta))
    }

    fn acknowledge_update(&self, generation: u64) -> Result<(), AgentToolCatalogUpdateError> {
        let mut observed = self
            .observed
            .lock()
            .map_err(|_| AgentToolCatalogUpdateError::new("工具目录观察水位不可用"))?;
        let Some(in_flight) = observed.in_flight.as_ref() else {
            if observed.generation == generation {
                return Ok(());
            }
            return Err(AgentToolCatalogUpdateError::new("工具目录确认代次不匹配"));
        };
        if in_flight.target_generation != generation {
            return Err(AgentToolCatalogUpdateError::new("工具目录确认代次不匹配"));
        }
        self.runtime
            .acknowledge_catalog_update(generation)
            .map_err(|error| AgentToolCatalogUpdateError::new(error.to_string()))?;
        let in_flight = observed
            .in_flight
            .take()
            .expect("已检查的目录确认状态应仍存在");
        observed.generation = in_flight.target_generation;
        observed.definitions = in_flight.target_definitions;
        Ok(())
    }
}

impl AgentRuntime {
    /// 返回或创建 Session 唯一 MCP 运行态，同时核验其持久项目绑定。
    pub(super) fn ensure_session_mcp_runtime(
        &self,
        session_id: &str,
        project_root: &Path,
    ) -> Result<Arc<SessionMcpRuntime>, SessionMcpError> {
        let project_root =
            canonical_project_root(project_root).map_err(|_| SessionMcpError::ProjectMismatch)?;
        let candidates = self
            .extension_candidates
            .read()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        let mut runtimes = self
            .session_mcp
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        if let Some(runtime) = runtimes.get(session_id) {
            runtime.ensure_project(&project_root)?;
            // fork/rewind 暂停期间侧车不在全局表中，项目候选传播可能正好越过它。
            // 每次重新绑定 Prompt/状态入口时用当前候选幂等补齐快照；目录冲突已经
            // 进入 desired state，仍交给既有卸载/Reason 边界收敛。
            if let Err(error) =
                runtime.queue_project_snapshot(project_snapshot(candidates.get(&project_root)))
                && error != SessionMcpError::CatalogConflict
            {
                return Err(error);
            }
            return Ok(Arc::clone(runtime));
        }
        let project = project_snapshot(candidates.get(&project_root));
        let runtime = SessionMcpRuntime::new(session_id.to_owned(), project_root, project)?;
        runtimes.insert(session_id.to_owned(), Arc::clone(&runtime));
        Ok(runtime)
    }

    /// 为动态扩展请求加载一批 Session MCP Server。
    pub(crate) async fn load_session_mcp(
        &self,
        session_id: &str,
        operation_id: &str,
        servers: Vec<schema::McpServer>,
    ) -> Result<SessionMcpMutationResponse, SessionMcpError> {
        let project_root = self.session_project_root(session_id)?;
        self.ensure_session_mcp_runtime(session_id, &project_root)?
            .load(operation_id, servers)
            .await
    }

    /// 为动态扩展请求排队撤销一个 Session MCP Server。
    pub(crate) async fn unload_session_mcp(
        &self,
        session_id: &str,
        operation_id: &str,
        server_name: &str,
    ) -> Result<SessionMcpMutationResponse, SessionMcpError> {
        let project_root = self.session_project_root(session_id)?;
        self.ensure_session_mcp_runtime(session_id, &project_root)?
            .unload(operation_id, server_name)
            .await
    }

    /// 查询 Session MCP 已发布与待发布状态。
    pub(crate) fn session_mcp_status(
        &self,
        session_id: &str,
    ) -> Result<SessionMcpStatusResponse, SessionMcpError> {
        let project_root = self.session_project_root(session_id)?;
        self.ensure_session_mcp_runtime(session_id, &project_root)?
            .status()
    }

    /// 用标准 ACP new/load/fork 的完整列表重建 Session MCP 主事实。
    pub(crate) async fn replace_session_mcp_servers(
        &self,
        session_id: &str,
        project_root: &Path,
        servers: Vec<schema::McpServer>,
    ) -> Result<(), SessionMcpError> {
        self.ensure_session_mcp_runtime(session_id, project_root)?
            .replace_exact(servers)
            .await
    }

    /// 为本轮注册目录包装器并创建独立 Runner 观察水位。
    pub(super) fn session_mcp_bindings(
        &self,
        session_id: &str,
        project_root: &Path,
    ) -> Result<
        (
            Arc<DeferredToolCatalog>,
            Arc<dyn AgentToolCatalogUpdateSource>,
        ),
        AgentRuntimeError,
    > {
        let runtime = self
            .ensure_session_mcp_runtime(session_id, project_root)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        Ok((runtime.catalog(), runtime.update_source()))
    }

    /// 项目候选发布或撤销后把新快照排队传播到所有匹配 Session。
    pub(super) fn queue_project_mcp_snapshot_for_sessions(&self, project_root: &Path) {
        let project = match self.extension_candidates.read() {
            Ok(candidates) => candidates.get(project_root).map(|candidate| {
                project_snapshot_with_revocation(
                    candidate,
                    candidate.mcp_revoked.load(Ordering::Acquire),
                )
            }),
            Err(_) => {
                tracing::warn!(
                    target: "extensions.mcp",
                    "扩展候选锁不可用，无法传播 Session MCP 快照"
                );
                return;
            }
        };
        let Some(project) = project else {
            tracing::warn!(
                target: "extensions.mcp",
                project_root = %project_root.display(),
                "扩展候选不存在，跳过 Session MCP 快照传播"
            );
            return;
        };
        let runtimes = match self.session_mcp.lock() {
            Ok(runtimes) => runtimes
                .values()
                .filter(|runtime| runtime.project_root == project_root)
                .cloned()
                .collect::<Vec<_>>(),
            Err(_) => {
                tracing::warn!(target: "extensions.mcp", "Session MCP 运行态锁不可用");
                return;
            }
        };
        for runtime in runtimes {
            if let Err(error) = runtime.queue_project_snapshot(clone_project_snapshot(&project)) {
                tracing::warn!(
                    target: "extensions.mcp",
                    session_id = %runtime.session_id,
                    %error,
                    "项目 MCP 新候选已排队但与 Session 独立目录冲突，等待冲突解除"
                );
            }
        }
    }

    /// Session 关闭已经静止后移除其 MCP 状态并显式等待全部连接关闭。
    pub(super) async fn close_session_mcp(&self, session_id: &str) {
        let runtime = self
            .session_mcp
            .lock()
            .ok()
            .and_then(|mut runtimes| runtimes.remove(session_id));
        if let Some(runtime) = runtime {
            runtime.close().await;
        }
    }

    /// 从常规关闭路径临时摘除 Session MCP，供 fork/rewind 完成后原样恢复。
    pub(crate) fn suspend_session_mcp(
        &self,
        session_id: &str,
    ) -> Result<SuspendedSessionMcp, SessionMcpError> {
        let runtime = self
            .session_mcp
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?
            .remove(session_id);
        Ok(SuspendedSessionMcp {
            runtime: Mutex::new(runtime),
        })
    }

    /// 把临时摘除的 MCP 运行态重新绑定到同一 Session 和项目。
    pub(crate) fn restore_session_mcp(
        &self,
        session_id: &str,
        project_root: &Path,
        suspended: &SuspendedSessionMcp,
    ) -> Result<(), SessionMcpError> {
        let project_root =
            canonical_project_root(project_root).map_err(|_| SessionMcpError::ProjectMismatch)?;
        let mut suspended_runtime = suspended
            .runtime
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        let Some(runtime) = suspended_runtime.as_ref().cloned() else {
            return Ok(());
        };
        runtime.ensure_project(&project_root)?;
        let mut runtimes = self
            .session_mcp
            .lock()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        if runtimes.contains_key(session_id) {
            return Err(SessionMcpError::StateUnavailable);
        }
        runtimes.insert(session_id.to_owned(), runtime);
        // 只有新绑定已经可见后才消费暂停所有权；失败时调用方
        // 仍可移除竞争占位并重试，不会提前关闭原 MCP 连接。
        suspended_runtime.take();
        Ok(())
    }

    fn session_project_root(&self, session_id: &str) -> Result<PathBuf, SessionMcpError> {
        let session = self
            .runtime_manager
            .get(session_id.to_owned())
            .map_err(|_| SessionMcpError::SessionUnavailable)?;
        let snapshot = session
            .snapshot()
            .map_err(|_| SessionMcpError::StateUnavailable)?;
        canonical_project_root(Path::new(&snapshot.state.project_root))
            .map_err(|_| SessionMcpError::ProjectMismatch)
    }
}

fn project_snapshot(candidate: Option<&Arc<RuntimeExtensionCandidate>>) -> ProjectToolSnapshot {
    candidate.map_or_else(ProjectToolSnapshot::empty, |candidate| {
        project_snapshot_with_revocation(candidate, candidate.mcp_revoked.load(Ordering::Acquire))
    })
}

fn project_snapshot_with_revocation(
    candidate: &Arc<RuntimeExtensionCandidate>,
    revoked: bool,
) -> ProjectToolSnapshot {
    let (tools, server_names) = if revoked {
        (Vec::new(), Vec::new())
    } else {
        (
            candidate.contributor.mcp_tool_implementations(),
            candidate.contributor.mcp_server_names(),
        )
    };
    ProjectToolSnapshot {
        version: ProjectCatalogVersion {
            generation: candidate.generation(),
            revoked,
        },
        server_names: server_names.into_iter().collect(),
        tools,
    }
}

fn clone_project_snapshot(snapshot: &ProjectToolSnapshot) -> ProjectToolSnapshot {
    ProjectToolSnapshot {
        version: snapshot.version,
        server_names: snapshot.server_names.clone(),
        tools: snapshot.tools.clone(),
    }
}

fn prepare_server_configs(
    servers: Vec<schema::McpServer>,
    project_root: &Path,
) -> Result<Vec<PreparedServerConfig>, SessionMcpError> {
    if servers.is_empty() {
        return Err(SessionMcpError::InvalidConfiguration);
    }
    prepare_server_configs_allow_empty(servers, project_root)
}

fn prepare_server_configs_allow_empty(
    servers: Vec<schema::McpServer>,
    project_root: &Path,
) -> Result<Vec<PreparedServerConfig>, SessionMcpError> {
    if servers.len() > MAX_SESSION_MCP_SERVERS {
        return Err(SessionMcpError::InvalidConfiguration);
    }
    let mut prepared = servers
        .into_iter()
        .map(|server| convert_server_config(server, project_root))
        .collect::<Result<Vec<_>, _>>()?;
    prepared.sort_by(|left, right| left.name.cmp(&right.name));
    if prepared.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(SessionMcpError::InvalidConfiguration);
    }
    Ok(prepared)
}

/// Stdio 子进程可继承的宿主环境白名单：仅无凭据的定位、语言与代理类变量，
/// 其余宿主变量（含各类密钥）不透传；用户显式配置的 server.env 始终优先。
fn is_inheritable_env_name(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "USERNAME",
        "SHELL",
        "LANG",
        "TMPDIR",
        "TEMP",
        "TMP",
        "TERM",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ];
    let upper = name.to_ascii_uppercase();
    EXACT.contains(&upper.as_str()) || name.starts_with("LC_") || name.starts_with("XDG_")
}

/// Stdio MCP 子进程的继承环境：按白名单过滤宿主变量，用户显式配置的同名变量
/// （ASCII 大小写不敏感）让位于稍后的覆盖写入。
fn stdio_environment(user_env: &[schema::EnvVariable]) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    let mut inherited = HashSet::new();
    for (name, value) in std::env::vars() {
        if !is_inheritable_env_name(&name) {
            continue;
        }
        if user_env
            .iter()
            .any(|variable| variable.name.eq_ignore_ascii_case(&name))
            || !inherited.insert(name.to_ascii_uppercase())
        {
            continue;
        }
        environment.insert(name, value);
    }
    environment
}

/// 拒绝 link-local 与云 metadata 主机，缓解 MCP HTTP SSRF；loopback 与普通私网不受影响。
fn is_blocked_http_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if matches!(
        host.to_ascii_lowercase().as_str(),
        "metadata.google.internal" | "metadata.goog"
    ) {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            (ip.segments()[0] & 0xffc0) == 0xfe80
                || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_link_local())
        }
        Err(_) => false,
    }
}

fn convert_server_config(
    server: schema::McpServer,
    project_root: &Path,
) -> Result<PreparedServerConfig, SessionMcpError> {
    let serialized =
        serde_json::to_vec(&server).map_err(|_| SessionMcpError::InvalidConfiguration)?;
    let fingerprint = sha256_hex(&serialized);
    match server {
        schema::McpServer::Http(server) => {
            validate_name(&server.name)?;
            validate_text(&server.url, MAX_MCP_CONFIG_TEXT_BYTES, false)?;
            let parsed =
                url::Url::parse(&server.url).map_err(|_| SessionMcpError::InvalidConfiguration)?;
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.host_str().is_some_and(is_blocked_http_host)
            {
                return Err(SessionMcpError::InvalidConfiguration);
            }
            if server.headers.len() > MAX_MCP_HEADERS {
                return Err(SessionMcpError::InvalidConfiguration);
            }
            let mut headers = BTreeMap::new();
            let mut normalized_names = HashSet::new();
            for header in server.headers {
                validate_http_header_name(&header.name)?;
                validate_text(&header.value, MAX_MCP_CONFIG_TEXT_BYTES, true)?;
                if !normalized_names.insert(header.name.to_ascii_lowercase()) {
                    return Err(SessionMcpError::InvalidConfiguration);
                }
                headers.insert(header.name, header.value);
            }
            let mut config = StreamableHttpConfig::new(server.url);
            config.headers = headers;
            Ok(PreparedServerConfig {
                name: server.name,
                transport: McpTransportKind::StreamableHttp,
                fingerprint,
                config: McpServerConfig::StreamableHttp(config),
            })
        }
        schema::McpServer::Sse(_) => Err(SessionMcpError::InvalidConfiguration),
        schema::McpServer::Stdio(server) => {
            validate_name(&server.name)?;
            let command = server
                .command
                .to_str()
                .ok_or(SessionMcpError::InvalidConfiguration)?
                .to_owned();
            validate_text(&command, MAX_MCP_CONFIG_TEXT_BYTES, false)?;
            if server.args.len() > MAX_MCP_ARGUMENTS || server.env.len() > MAX_MCP_ENVIRONMENT {
                return Err(SessionMcpError::InvalidConfiguration);
            }
            for argument in &server.args {
                validate_text(argument, MAX_MCP_CONFIG_TEXT_BYTES, true)?;
            }
            let mut environment = stdio_environment(&server.env);
            let mut normalized_names = HashSet::new();
            for variable in server.env {
                validate_environment_name(&variable.name)?;
                validate_text(&variable.value, MAX_MCP_ENV_VALUE_BYTES, true)?;
                if !normalized_names.insert(variable.name.to_ascii_uppercase()) {
                    return Err(SessionMcpError::InvalidConfiguration);
                }
                environment.insert(variable.name, variable.value);
            }
            let mut config = StdioServerConfig::new(command);
            config.args = server.args;
            config.current_dir = Some(project_root.to_path_buf());
            config.environment = environment;
            // 宿主环境已按白名单过滤进 environment；整份继承保持关闭。
            config.inherit_environment = false;
            Ok(PreparedServerConfig {
                name: server.name,
                transport: McpTransportKind::Stdio,
                fingerprint,
                config: McpServerConfig::Stdio(config),
            })
        }
        _ => Err(SessionMcpError::InvalidConfiguration),
    }
}

async fn connect_server(
    server: PreparedServerConfig,
) -> Result<Arc<SessionServerBinding>, (String, FailedServer)> {
    let report = prepare_mcp_server_tools(
        server.name.clone(),
        server.config,
        McpClientOptions::default(),
    )
    .await;
    let strict_failure = report.diagnostics().iter().any(|diagnostic| {
        matches!(
            diagnostic.code,
            McpDiagnosticCode::ServerUnavailable
                | McpDiagnosticCode::ToolDiscoveryFailed
                | McpDiagnosticCode::PortableNameCollision
        )
    });
    let (tools, _, client) = report.into_parts();
    let Some(client) = client else {
        return Err((
            server.name,
            FailedServer {
                transport: server.transport,
                error: "MCP Server 连接或工具发现失败".to_owned(),
            },
        ));
    };
    if strict_failure || tools.is_empty() {
        let _ = client.close().await;
        return Err((
            server.name,
            FailedServer {
                transport: server.transport,
                error: "MCP Server 连接或工具发现失败".to_owned(),
            },
        ));
    }
    let lease = Arc::new(SessionMcpClientLease::new(client));
    let tools = tools
        .into_iter()
        .map(|inner| {
            Arc::new(SessionMcpOwnedTool {
                inner,
                _lease: Arc::clone(&lease),
            }) as Arc<dyn AgentTool>
        })
        .collect();
    Ok(Arc::new(SessionServerBinding {
        name: server.name,
        transport: server.transport,
        config_fingerprint: server.fingerprint,
        tools,
        lease,
    }))
}

async fn close_bindings(bindings: &[Arc<SessionServerBinding>]) {
    for binding in bindings {
        binding.lease.close().await;
    }
}

fn validate_composite_catalog(
    project_tools: &[Arc<dyn AgentTool>],
    servers: &BTreeMap<String, Arc<SessionServerBinding>>,
) -> Result<(), SessionMcpError> {
    let catalog = DeferredToolCatalog::new();
    catalog
        .replace_all(composite_tools(project_tools, servers))
        .map(|_| ())
        .map_err(|_| SessionMcpError::CatalogConflict)
}

/// 在项目候选与 Session Server 的共同提交点校验名称和工具目录。
fn validate_project_catalog(
    project: &ProjectToolSnapshot,
    servers: &BTreeMap<String, Arc<SessionServerBinding>>,
) -> Result<(), SessionMcpError> {
    if project
        .server_names
        .iter()
        .any(|name| servers.contains_key(name))
    {
        return Err(SessionMcpError::CatalogConflict);
    }
    validate_composite_catalog(&project.tools, servers)
}

fn composite_tools(
    project_tools: &[Arc<dyn AgentTool>],
    servers: &BTreeMap<String, Arc<SessionServerBinding>>,
) -> Vec<Arc<dyn AgentTool>> {
    project_tools
        .iter()
        .cloned()
        .chain(
            servers
                .values()
                .flat_map(|binding| binding.tools.iter().cloned()),
        )
        .collect()
}

fn same_server_map(
    left: &BTreeMap<String, Arc<SessionServerBinding>>,
    right: &BTreeMap<String, Arc<SessionServerBinding>>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(name, binding)| {
            right
                .get(name)
                .is_some_and(|candidate| Arc::ptr_eq(binding, candidate))
        })
}

/// 返回当前状态中所有仍可能出现在 Session MCP 状态响应里的 Server 名称。
///
/// 当前和待发布目录可能在 Reason 边界前同时保留不同名称，失败记录也占用同一
/// 个协议容量；三者必须统一计算，不能只限制已成功连接的待发布 Server。
fn tracked_server_names(state: &SessionMcpState) -> BTreeSet<String> {
    state
        .current_servers
        .keys()
        .chain(state.desired_servers.keys())
        .chain(state.failed_servers.keys())
        .cloned()
        .collect()
}

fn server_statuses(state: &SessionMcpState) -> Vec<SessionMcpServerStatus> {
    let mut statuses = BTreeMap::new();
    for (name, binding) in &state.current_servers {
        let status = match state.desired_servers.get(name) {
            Some(desired) if Arc::ptr_eq(binding, desired) => SessionMcpServerPhase::Ready,
            Some(_) => SessionMcpServerPhase::Pending,
            None => SessionMcpServerPhase::PendingUnload,
        };
        statuses.insert(
            name.clone(),
            SessionMcpServerStatus {
                name: name.clone(),
                transport: binding.transport,
                status,
                tools_count: binding.tools_count(),
                error: None,
            },
        );
    }
    for (name, binding) in &state.desired_servers {
        statuses
            .entry(name.clone())
            .or_insert_with(|| SessionMcpServerStatus {
                name: name.clone(),
                transport: binding.transport,
                status: SessionMcpServerPhase::Pending,
                tools_count: binding.tools_count(),
                error: None,
            });
    }
    for (name, failure) in &state.failed_servers {
        statuses
            .entry(name.clone())
            .or_insert_with(|| SessionMcpServerStatus {
                name: name.clone(),
                transport: failure.transport,
                status: SessionMcpServerPhase::Failed,
                tools_count: 0,
                error: Some(failure.error.clone()),
            });
    }
    statuses.into_values().collect()
}

fn mutation_response_locked(
    session_id: &str,
    catalog: &DeferredToolCatalog,
    state: &SessionMcpState,
    changed: bool,
    deduplicated: bool,
) -> SessionMcpMutationResponse {
    SessionMcpMutationResponse {
        session_id: session_id.to_owned(),
        catalog_generation: catalog.generation_and_definitions().0,
        servers: server_statuses(state),
        changed,
        deduplicated,
    }
}

fn insert_receipt(
    state: &mut SessionMcpState,
    operation_id: &str,
    request_fingerprint: String,
    response: SessionMcpMutationResponse,
) {
    if !state.receipts.contains_key(operation_id) {
        while state.receipts.len() >= MAX_SESSION_MCP_RECEIPTS {
            if let Some(oldest) = state.receipt_order.pop_front() {
                state.receipts.remove(&oldest);
            } else {
                break;
            }
        }
        state.receipt_order.push_back(operation_id.to_owned());
    }
    state.receipts.insert(
        operation_id.to_owned(),
        MutationReceipt {
            request_fingerprint,
            response,
        },
    );
}

fn mutation_fingerprint(
    operation: &str,
    servers: &[PreparedServerConfig],
    server_name: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    hash_text(&mut digest, operation);
    if let Some(server_name) = server_name {
        hash_text(&mut digest, server_name);
    }
    for server in servers {
        hash_text(&mut digest, &server.name);
        hash_text(&mut digest, &server.fingerprint);
    }
    sha256_hex(&digest.finalize())
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}

fn sha256_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn definition_map(definitions: &[ToolDefinition]) -> BTreeMap<String, ToolDefinition> {
    definitions
        .iter()
        .map(|definition| (definition.name.clone(), definition.clone()))
        .collect()
}

fn validate_name(value: &str) -> Result<(), SessionMcpError> {
    validate_text(value, MAX_MCP_NAME_BYTES, false)
}

fn validate_text(
    value: &str,
    maximum_bytes: usize,
    allow_empty: bool,
) -> Result<(), SessionMcpError> {
    if (!allow_empty && value.is_empty())
        || value.len() > maximum_bytes
        || value.contains('\0')
        || (!allow_empty && value.trim() != value)
    {
        return Err(SessionMcpError::InvalidConfiguration);
    }
    Ok(())
}

fn validate_http_header_name(value: &str) -> Result<(), SessionMcpError> {
    validate_name(value)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            )
    }) {
        return Err(SessionMcpError::InvalidConfiguration);
    }
    Ok(())
}

fn validate_environment_name(value: &str) -> Result<(), SessionMcpError> {
    validate_name(value)?;
    if value.contains('=') {
        return Err(SessionMcpError::InvalidConfiguration);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_agent::ToolOutput;
    use serde_json::json;
    use std::process::Command;
    use std::sync::OnceLock;
    use std::time::Duration;

    /// 只依赖标准库的 MCP stdio 夹具，同一个测试进程只编译一次。
    fn fake_mcp_binary() -> &'static Path {
        static BINARY: OnceLock<PathBuf> = OnceLock::new();
        BINARY
            .get_or_init(|| {
                let directory = std::env::temp_dir().join(format!(
                    "keencode-session-mcp-tests-{}",
                    std::process::id()
                ));
                std::fs::create_dir_all(&directory).expect("创建 Session MCP 测试目录");
                let source = directory.join("fake_session_mcp.rs");
                let executable = directory.join(if cfg!(windows) {
                    "fake-session-mcp.exe"
                } else {
                    "fake-session-mcp"
                });
                std::fs::write(
                    &source,
                    r###"
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

fn append_from_env(name: &str) {
    let Ok(path) = std::env::var(name) else { return; };
    let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "1").unwrap();
}

fn wait_for_release() {
    let Ok(path) = std::env::var("KEENCODE_SESSION_MCP_TEST_RELEASE") else { return; };
    while !Path::new(&path).exists() {
        thread::sleep(Duration::from_millis(1));
    }
}

fn main() {
    append_from_env("KEENCODE_SESSION_MCP_TEST_STARTED");
    let tool = std::env::args().nth(1).unwrap_or_else(|| "echo".to_owned());
    let protocol = std::env::var("KEENCODE_SESSION_MCP_TEST_PROTOCOL").unwrap();
    let stdin = io::stdin();
    let mut output = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        if line.contains("\"method\":\"notifications/initialized\"") {
            continue;
        }
        let Some(id) = extract_id(&line) else { continue; };
        let response = if line.contains("\"method\":\"initialize\"") {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"{protocol}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fake-session-mcp","version":"1"}}}}}}"#)
        } else if line.contains("\"method\":\"tools/list\"") {
            append_from_env("KEENCODE_SESSION_MCP_TEST_GATE_REACHED");
            wait_for_release();
            if std::env::var_os("KEENCODE_SESSION_MCP_TEST_FAIL").is_some() {
                format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[]}}}}"#)
            } else {
                format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"{tool}","description":"session test tool","inputSchema":{{"type":"object"}},"annotations":{{"readOnlyHint":true}}}}]}}}}"#)
            }
        } else {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#)
        };
        writeln!(output, "{response}").unwrap();
        output.flush().unwrap();
    }
    append_from_env("KEENCODE_SESSION_MCP_TEST_CLOSED");
}

fn extract_id(body: &str) -> Option<&str> {
    let tail = body.split("\"id\":").nth(1)?;
    let end = tail.find(',').unwrap_or(tail.len());
    Some(tail[..end].trim())
}
"###,
                )
                .expect("写入 Session MCP 测试 Server");
                let output = Command::new("rustc")
                    .arg("--edition=2024")
                    .arg(&source)
                    .arg("-o")
                    .arg(&executable)
                    .output()
                    .expect("测试环境必须可以调用 rustc");
                assert!(
                    output.status.success(),
                    "Session MCP 测试 Server 编译失败：{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                executable
            })
            .as_path()
    }

    fn stdio_server(name: &str, tool: &str, started: &Path, closed: &Path) -> schema::McpServer {
        schema::McpServer::Stdio(
            schema::McpServerStdio::new(name, fake_mcp_binary())
                .args(vec![tool.to_owned()])
                .env(vec![
                    schema::EnvVariable::new(
                        "KEENCODE_SESSION_MCP_TEST_PROTOCOL",
                        keencode_mcp::DEFAULT_PROTOCOL_VERSION,
                    ),
                    schema::EnvVariable::new(
                        "KEENCODE_SESSION_MCP_TEST_STARTED",
                        started.to_string_lossy(),
                    ),
                    schema::EnvVariable::new(
                        "KEENCODE_SESSION_MCP_TEST_CLOSED",
                        closed.to_string_lossy(),
                    ),
                ]),
        )
    }

    fn gated_stdio_server(
        name: &str,
        tool: &str,
        started: &Path,
        closed: &Path,
        release: &Path,
        gate_reached: &Path,
        fail: bool,
    ) -> schema::McpServer {
        let schema::McpServer::Stdio(mut server) = stdio_server(name, tool, started, closed) else {
            unreachable!("测试夹具必须使用 stdio Server");
        };
        server.env.push(schema::EnvVariable::new(
            "KEENCODE_SESSION_MCP_TEST_RELEASE",
            release.to_string_lossy(),
        ));
        server.env.push(schema::EnvVariable::new(
            "KEENCODE_SESSION_MCP_TEST_GATE_REACHED",
            gate_reached.to_string_lossy(),
        ));
        if fail {
            server.env.push(schema::EnvVariable::new(
                "KEENCODE_SESSION_MCP_TEST_FAIL",
                "1",
            ));
        }
        schema::McpServer::Stdio(server)
    }

    fn missing_server(name: &str) -> schema::McpServer {
        schema::McpServer::Stdio(schema::McpServerStdio::new(
            name,
            "definitely-missing-keencode-session-mcp-command",
        ))
    }

    fn file_line_count(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn env_name_allowlist_matches_core_variables_only() {
        assert!(is_inheritable_env_name("PATH"));
        assert!(is_inheritable_env_name("Path"));
        assert!(is_inheritable_env_name("LANG"));
        assert!(is_inheritable_env_name("LC_ALL"));
        assert!(is_inheritable_env_name("XDG_DATA_HOME"));
        assert!(is_inheritable_env_name("http_proxy"));
        assert!(!is_inheritable_env_name("ANTHROPIC_API_KEY"));
        assert!(!is_inheritable_env_name("AWS_SECRET_ACCESS_KEY"));
        assert!(!is_inheritable_env_name("GITHUB_TOKEN"));
    }

    #[test]
    fn stdio_environment_blocks_credential_inheritance() {
        let environment = stdio_environment(&[]);
        assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
        assert!(!environment.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(
            environment
                .keys()
                .any(|name| name.eq_ignore_ascii_case("PATH"))
        );
    }

    #[test]
    fn stdio_environment_defers_to_explicit_user_env() {
        let environment = stdio_environment(&[schema::EnvVariable::new("PATH", "custom-path")]);
        assert!(
            !environment
                .keys()
                .any(|name| name.eq_ignore_ascii_case("PATH"))
        );
    }

    #[test]
    fn http_host_filter_blocks_metadata_targets_only() {
        assert!(is_blocked_http_host("169.254.169.254"));
        assert!(is_blocked_http_host("fe80::1"));
        assert!(is_blocked_http_host("::ffff:169.254.169.254"));
        assert!(is_blocked_http_host("metadata.google.internal"));
        assert!(is_blocked_http_host("metadata.goog"));
        assert!(is_blocked_http_host("METADATA.GOOGLE.INTERNAL"));
        assert!(!is_blocked_http_host("127.0.0.1"));
        assert!(!is_blocked_http_host("::1"));
        assert!(!is_blocked_http_host("192.168.1.10"));
        assert!(!is_blocked_http_host("example.com"));
        assert!(!is_blocked_http_host("metadata.internal"));
    }

    #[test]
    fn convert_server_config_filters_stdio_env_and_blocks_metadata_urls() {
        let server =
            schema::McpServer::Stdio(schema::McpServerStdio::new("mcp", "some-command").env(vec![
                schema::EnvVariable::new("PATH", "custom-path"),
                schema::EnvVariable::new("ANTHROPIC_API_KEY", "explicit"),
            ]));
        let prepared =
            convert_server_config(server, Path::new("/tmp")).expect("stdio 配置必须可用");
        let McpServerConfig::Stdio(config) = prepared.config else {
            unreachable!("stdio server 必须产出 stdio 配置");
        };
        assert!(!config.inherit_environment);
        assert_eq!(
            config.environment.get("PATH").map(String::as_str),
            Some("custom-path")
        );
        assert_eq!(
            config
                .environment
                .get("ANTHROPIC_API_KEY")
                .map(String::as_str),
            Some("explicit")
        );

        for url in [
            "http://169.254.169.254/mcp",
            "http://[fe80::1]/mcp",
            "https://metadata.google.internal/mcp",
        ] {
            let server = schema::McpServer::Http(schema::McpServerHttp::new("mcp", url));
            assert!(
                convert_server_config(server, Path::new("/tmp")).is_err(),
                "必须拒绝 {url}"
            );
        }
        for url in ["http://127.0.0.1:8123/mcp", "https://example.com/mcp"] {
            let server = schema::McpServer::Http(schema::McpServerHttp::new("mcp", url));
            assert!(
                convert_server_config(server, Path::new("/tmp")).is_ok(),
                "必须允许 {url}"
            );
        }
    }

    async fn wait_for_file_lines(path: &Path, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while file_line_count(path) < expected {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "等待 {} 出现 {expected} 行超时，当前为 {}",
                path.display(),
                file_line_count(path)
            )
        });
    }

    fn empty_runtime(session_id: &str, project_root: &Path) -> Arc<SessionMcpRuntime> {
        SessionMcpRuntime::new(
            session_id.to_owned(),
            project_root.to_path_buf(),
            ProjectToolSnapshot::empty(),
        )
        .expect("应创建空 Session MCP Runtime")
    }

    struct NamedTool(ToolDefinition);

    impl NamedTool {
        fn new(name: String) -> Arc<Self> {
            Self::with_definition(ToolDefinition::new(
                name,
                "项目目录冲突测试工具",
                json!({ "type": "object" }),
            ))
        }

        fn with_definition(definition: ToolDefinition) -> Arc<Self> {
            Arc::new(Self(definition))
        }
    }

    impl AgentTool for NamedTool {
        fn definition(&self) -> ToolDefinition {
            self.0.clone()
        }

        fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
            Ok(ToolEffect::ReadOnly)
        }

        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }

        fn execute(&self, _context: ToolContext, _input: Value) -> ToolFuture<'_> {
            Box::pin(async { Ok(ToolOutput::text("ok")) })
        }
    }

    /// 只提供项目 MCP 工具的不可变测试候选。
    struct ProjectMcpContributor {
        tools: Vec<Arc<dyn AgentTool>>,
    }

    impl crate::agent_runtime::RuntimeExtensionContributor for ProjectMcpContributor {
        fn register_tools(
            &self,
            _registry: &mut keencode_agent::ToolRegistry,
            _context: &crate::agent_runtime::RuntimeToolContext,
        ) -> Result<(), String> {
            Ok(())
        }

        fn build_hook_runtime(
            &self,
            _context: &crate::agent_runtime::RuntimeToolContext,
        ) -> Result<keencode_agent::HookRuntime, String> {
            Ok(keencode_agent::HookRuntime::empty())
        }

        fn prepare_lsp_runtime(
            &self,
            _context: &crate::agent_runtime::RuntimeToolContext,
        ) -> Result<(), String> {
            Ok(())
        }

        fn mcp_tool_implementations(&self) -> Vec<Arc<dyn AgentTool>> {
            self.tools.clone()
        }

        fn resolve_agent(
            &self,
            _name: &str,
            _parent: &crate::agent_runtime::RuntimeAgentTemplateContext,
        ) -> Result<Option<crate::agent_runtime::RuntimeAgentTemplate>, String> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn mutation_receipts_publish_once_per_runner_and_keep_sessions_isolated() {
        let directory = tempfile::tempdir().unwrap();
        let started = directory.path().join("started");
        let closed = directory.path().join("closed");
        let runtime = empty_runtime("session-a", directory.path());
        let isolated = empty_runtime("session-b", directory.path());
        let first_runner = runtime.update_source();
        let second_runner = runtime.update_source();
        let isolated_runner = isolated.update_source();
        let server = stdio_server("local", "echo", &started, &closed);

        let loaded = runtime
            .load("load-1", vec![server.clone()])
            .await
            .expect("首次加载应排队");
        assert!(loaded.changed);
        assert!(!loaded.deduplicated);
        assert_eq!(loaded.catalog_generation, 1);
        assert_eq!(loaded.servers[0].status, SessionMcpServerPhase::Pending);

        let duplicate = runtime
            .load("load-1", vec![server.clone()])
            .await
            .expect("同载荷重试应命中收据");
        assert!(duplicate.deduplicated);
        assert_eq!(file_line_count(&started), 1, "幂等重试不得重连");
        assert_eq!(
            runtime.load("load-1", vec![missing_server("local")]).await,
            Err(SessionMcpError::OperationConflict)
        );

        let expected_name = keencode_tools::portable_mcp_tool_name("local", "echo").unwrap();
        for source in [&first_runner, &second_runner] {
            let delta = source
                .take_update()
                .expect("目录更新应可读取")
                .expect("每个 Runner 都应收到同一代次");
            assert_eq!(delta.generation(), 2);
            assert_eq!(delta.added(), std::slice::from_ref(&expected_name));
            assert!(delta.removed().is_empty());
            source
                .acknowledge_update(delta.generation())
                .expect("成功投递目录通知后应确认观察水位");
        }
        assert!(first_runner.take_update().unwrap().is_none());
        assert!(isolated_runner.take_update().unwrap().is_none());
        assert!(isolated.catalog().is_empty());
        assert_eq!(
            runtime.status().unwrap().servers[0].status,
            SessionMcpServerPhase::Ready
        );

        let unloaded = runtime.unload("unload-1", "local").await.unwrap();
        assert!(unloaded.changed);
        assert!(
            runtime
                .unload("unload-1", "local")
                .await
                .unwrap()
                .deduplicated
        );
        for source in [&first_runner, &second_runner] {
            let delta = source
                .take_update()
                .unwrap()
                .expect("每个 Runner 都应收到撤销代次");
            assert_eq!(delta.generation(), 3);
            assert!(delta.added().is_empty());
            assert_eq!(delta.removed(), std::slice::from_ref(&expected_name));
            source
                .acknowledge_update(delta.generation())
                .expect("成功投递撤销通知后应确认观察水位");
        }
        wait_for_file_lines(&closed, 1).await;
        runtime.close().await;
        isolated.close().await;
    }

    #[tokio::test]
    async fn unacknowledged_catalog_change_survives_failed_runner_and_reaches_next_runner() {
        let directory = tempfile::tempdir().unwrap();
        let started = directory.path().join("started");
        let closed = directory.path().join("closed");
        let runtime = empty_runtime("session-retry", directory.path());
        runtime
            .load(
                "load-retry",
                vec![stdio_server("local", "echo", &started, &closed)],
            )
            .await
            .unwrap();

        let failed_runner = runtime.update_source();
        let first = failed_runner
            .take_update()
            .unwrap()
            .expect("失败 Runner 应先看到目录变化");
        assert_eq!(first.generation(), 2);

        // 模拟首次 Provider 请求不可恢复失败：没有确认调用，新的 Runner
        // 仍应从待确认换代前的定义快照生成同一通知。
        let retry_runner = runtime.update_source();
        let retry = retry_runner
            .take_update()
            .unwrap()
            .expect("下一 Runner 仍应看到未确认目录变化");
        assert_eq!(retry, first);
        assert_eq!(
            retry.added(),
            &[keencode_tools::portable_mcp_tool_name("local", "echo").unwrap()]
        );

        retry_runner.acknowledge_update(retry.generation()).unwrap();
        let after_success = runtime.update_source();
        assert!(after_success.take_update().unwrap().is_none());
        runtime.close().await;
    }

    #[tokio::test]
    async fn failed_batch_rolls_back_successful_connections_and_deduplicates_failure() {
        let directory = tempfile::tempdir().unwrap();
        let started = directory.path().join("started");
        let closed = directory.path().join("closed");
        let runtime = empty_runtime("session-batch", directory.path());
        let servers = vec![
            stdio_server("a-valid", "echo", &started, &closed),
            missing_server("z-missing"),
        ];

        let response = runtime.load("batch-1", servers.clone()).await.unwrap();
        assert!(!response.changed);
        assert!(!response.deduplicated);
        assert_eq!(response.catalog_generation, 1);
        assert_eq!(response.servers.len(), 1);
        assert_eq!(response.servers[0].name, "z-missing");
        assert_eq!(response.servers[0].status, SessionMcpServerPhase::Failed);
        assert!(runtime.catalog().is_empty());
        assert_eq!(file_line_count(&started), 1);
        wait_for_file_lines(&closed, 1).await;

        let duplicate = runtime.load("batch-1", servers).await.unwrap();
        assert!(duplicate.deduplicated);
        assert!(!duplicate.changed);
        assert_eq!(file_line_count(&started), 1, "失败收据重试不得重连");
        runtime.close().await;
    }

    async fn assert_project_candidate_wins_during_load(fail: bool) {
        let directory = tempfile::tempdir().unwrap();
        let started = directory.path().join("started");
        let closed = directory.path().join("closed");
        let gate_reached = directory.path().join("gate-reached");
        let release = directory.path().join("release");
        let runtime = empty_runtime("session-project-race", directory.path());
        let server = gated_stdio_server(
            "raced-server",
            "echo",
            &started,
            &closed,
            &release,
            &gate_reached,
            fail,
        );
        let loading_runtime = Arc::clone(&runtime);
        let loading =
            tokio::spawn(async move { loading_runtime.load("project-race", vec![server]).await });

        wait_for_file_lines(&gate_reached, 1).await;
        runtime
            .queue_project_snapshot(ProjectToolSnapshot {
                version: ProjectCatalogVersion {
                    generation: 1,
                    revoked: false,
                },
                server_names: BTreeSet::from(["raced-server".to_owned()]),
                tools: Vec::new(),
            })
            .expect("项目候选应能在连接等待期间排队");
        std::fs::write(&release, b"release").expect("应释放连接闸门");

        assert_eq!(
            loading.await.expect("并发加载任务不应 panic"),
            Err(SessionMcpError::CatalogConflict),
            "项目候选必须在成功和失败提交路径都优先"
        );
        wait_for_file_lines(&closed, 1).await;
        {
            let state = runtime.state.lock().unwrap();
            assert!(state.desired_servers.is_empty(), "冲突成功绑定不得泄漏");
            assert!(state.failed_servers.is_empty(), "冲突失败记录不得泄漏");
            assert_eq!(
                state.desired_project.server_names,
                BTreeSet::from(["raced-server".to_owned()])
            );
        }
        assert!(runtime.catalog().is_empty(), "冲突连接不得发布到目录");
        runtime.close().await;
    }

    /// 项目候选在成功连接提交前换代时，动态 Server 必须关闭且不得发布。
    #[tokio::test]
    async fn project_candidate_wins_over_successful_load_race() {
        assert_project_candidate_wins_during_load(false).await;
    }

    /// 项目候选在失败连接提交前换代时，failed Server 名称也不得泄漏。
    #[tokio::test]
    async fn project_candidate_wins_over_failed_load_race() {
        assert_project_candidate_wins_during_load(true).await;
    }

    #[test]
    fn project_catalog_version_prefers_new_generations_and_same_generation_revocation() {
        let generation_one = ProjectCatalogVersion {
            generation: 1,
            revoked: false,
        };
        let generation_one_revoked = ProjectCatalogVersion {
            generation: 1,
            revoked: true,
        };
        let generation_two = ProjectCatalogVersion {
            generation: 2,
            revoked: false,
        };
        assert!(generation_one.supersedes(ProjectCatalogVersion::default()));
        assert!(generation_one_revoked.supersedes(generation_one));
        assert!(!generation_one.supersedes(generation_one_revoked));
        assert!(generation_two.supersedes(generation_one_revoked));
        assert!(!generation_one_revoked.supersedes(generation_two));
    }

    #[tokio::test]
    async fn project_candidate_replaces_same_named_failed_server() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = empty_runtime("session-project-failed-name", directory.path());
        let source = runtime.update_source();
        let failed = runtime
            .load("load-failed-name", vec![missing_server("foo")])
            .await
            .expect("失败 Server 应保留安全状态");
        assert_eq!(failed.servers[0].name, "foo");
        assert_eq!(failed.servers[0].status, SessionMcpServerPhase::Failed);

        runtime
            .queue_project_snapshot(ProjectToolSnapshot {
                version: ProjectCatalogVersion {
                    generation: 1,
                    revoked: false,
                },
                server_names: BTreeSet::from(["foo".to_owned()]),
                tools: vec![NamedTool::new("project-foo".to_owned())],
            })
            .expect("项目候选应接管同名失败 Server");
        {
            let state = runtime.state.lock().unwrap();
            assert!(state.failed_servers.is_empty());
            assert_eq!(state.desired_project.version.generation, 1);
        }

        let delta = source
            .take_update()
            .expect("项目候选目录应可应用")
            .expect("项目候选目录应产生变化");
        assert_eq!(delta.added(), &["project-foo".to_owned()]);
        assert!(runtime.status().unwrap().servers.is_empty());
        runtime.close().await;
    }

    #[tokio::test]
    async fn stale_project_candidates_cannot_regress_revocation_or_generation() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = empty_runtime("session-project-version-order", directory.path());
        let project = |generation: u64, revoked: bool, name: &str| ProjectToolSnapshot {
            version: ProjectCatalogVersion {
                generation,
                revoked,
            },
            server_names: BTreeSet::new(),
            tools: (!revoked)
                .then(|| NamedTool::new(name.to_owned()) as Arc<dyn AgentTool>)
                .into_iter()
                .collect(),
        };

        runtime
            .queue_project_snapshot(project(2, false, "generation-two"))
            .unwrap();
        runtime
            .queue_project_snapshot(project(1, false, "stale-generation-one"))
            .unwrap();
        assert_eq!(
            runtime.state.lock().unwrap().desired_project.version,
            ProjectCatalogVersion {
                generation: 2,
                revoked: false,
            }
        );

        runtime
            .queue_project_snapshot(project(2, true, "same-generation-revoked"))
            .unwrap();
        runtime
            .queue_project_snapshot(project(2, false, "stale-unrevoked"))
            .unwrap();
        assert_eq!(
            runtime.state.lock().unwrap().desired_project.version,
            ProjectCatalogVersion {
                generation: 2,
                revoked: true,
            }
        );

        runtime
            .queue_project_snapshot(project(3, false, "generation-three"))
            .unwrap();
        {
            let state = runtime.state.lock().unwrap();
            assert_eq!(state.desired_project.version.generation, 3);
            assert!(!state.desired_project.version.revoked);
            assert_eq!(
                state.desired_project.tools[0].definition().name,
                "generation-three"
            );
        }
        runtime.close().await;
    }

    #[tokio::test]
    async fn failure_capacity_keeps_status_encodable_and_rejects_new_names() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = empty_runtime("session-mcp-capacity", directory.path());

        for index in 0..MAX_SESSION_MCP_SERVERS {
            let name = format!("missing-{index:03}");
            let response = runtime
                .load(&format!("load-{index:03}"), vec![missing_server(&name)])
                .await
                .expect("容量以内的失败 Server 应保留安全状态");
            assert_eq!(response.servers.len(), index + 1);
        }

        let encoder = keencode_acp::AcpResponseEncoder::new();
        let status = runtime.status().expect("容量以内的状态应可读取");
        assert_eq!(status.servers.len(), MAX_SESSION_MCP_SERVERS);
        encoder
            .encode_result(schema::RequestId::Number(1), &status)
            .expect("第 512 个失败 Server 的状态应可编码");

        let retry = runtime
            .load(
                "retry-existing-failure",
                vec![missing_server("missing-511")],
            )
            .await
            .expect("容量已满时已有失败名称仍可覆盖");
        assert_eq!(retry.servers.len(), MAX_SESSION_MCP_SERVERS);
        encoder
            .encode_result(schema::RequestId::Number(2), &retry)
            .expect("已有失败名称覆盖后的变更响应应可编码");

        let started = directory.path().join("migration-started");
        let closed = directory.path().join("migration-closed");
        let migrated = runtime
            .load(
                "migrate-existing-failure",
                vec![stdio_server("missing-511", "echo", &started, &closed)],
            )
            .await
            .expect("已有失败名称应可在容量满时迁移为成功连接");
        assert_eq!(migrated.servers.len(), MAX_SESSION_MCP_SERVERS);
        assert_eq!(
            migrated
                .servers
                .iter()
                .filter(|server| server.name == "missing-511")
                .count(),
            1
        );
        encoder
            .encode_result(schema::RequestId::Number(3), &migrated)
            .expect("失败迁移后的变更响应应可编码");

        let overflow_started = directory.path().join("overflow-started");
        let overflow_closed = directory.path().join("overflow-closed");
        assert_eq!(
            runtime
                .load(
                    "reject-overflow",
                    vec![stdio_server(
                        "new-overflow",
                        "echo",
                        &overflow_started,
                        &overflow_closed,
                    )],
                )
                .await,
            Err(SessionMcpError::InvalidConfiguration),
            "新名称超过容量时应在连接前拒绝"
        );
        assert_eq!(file_line_count(&overflow_started), 0);
        {
            let state = runtime.state.lock().unwrap();
            assert_eq!(tracked_server_names(&state).len(), MAX_SESSION_MCP_SERVERS);
            assert!(!state.receipts.contains_key("reject-overflow"));
        }
        let status = runtime.status().expect("拒绝超限后已有状态仍应可读取");
        assert_eq!(status.servers.len(), MAX_SESSION_MCP_SERVERS);
        encoder
            .encode_result(schema::RequestId::Number(4), &status)
            .expect("拒绝超限后状态仍应可编码");

        runtime.close().await;
        wait_for_file_lines(&closed, 1).await;
    }

    #[tokio::test]
    async fn project_server_and_tool_conflicts_reject_without_publishing_new_bindings() {
        let directory = tempfile::tempdir().unwrap();
        let preflight_started = directory.path().join("preflight-started");
        let preflight_closed = directory.path().join("preflight-closed");
        let server_name_conflict = SessionMcpRuntime::new(
            "session-server-conflict".to_owned(),
            directory.path().to_path_buf(),
            ProjectToolSnapshot {
                version: ProjectCatalogVersion {
                    generation: 1,
                    revoked: false,
                },
                server_names: BTreeSet::from(["same-server".to_owned()]),
                tools: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(
            server_name_conflict
                .load(
                    "server-conflict",
                    vec![stdio_server(
                        "same-server",
                        "echo",
                        &preflight_started,
                        &preflight_closed,
                    )],
                )
                .await,
            Err(SessionMcpError::CatalogConflict)
        );
        assert_eq!(file_line_count(&preflight_started), 0);
        server_name_conflict.close().await;

        let started = directory.path().join("tool-started");
        let closed = directory.path().join("tool-closed");
        let colliding_name =
            keencode_tools::portable_mcp_tool_name("session-tools", "echo").unwrap();
        let runtime = SessionMcpRuntime::new(
            "session-tool-conflict".to_owned(),
            directory.path().to_path_buf(),
            ProjectToolSnapshot {
                version: ProjectCatalogVersion {
                    generation: 1,
                    revoked: false,
                },
                server_names: BTreeSet::new(),
                tools: vec![NamedTool::new(colliding_name.clone())],
            },
        )
        .unwrap();
        assert_eq!(
            runtime
                .load(
                    "tool-conflict",
                    vec![stdio_server("session-tools", "echo", &started, &closed)],
                )
                .await,
            Err(SessionMcpError::CatalogConflict)
        );
        assert_eq!(file_line_count(&started), 1);
        wait_for_file_lines(&closed, 1).await;
        assert!(runtime.status().unwrap().servers.is_empty());
        assert_eq!(runtime.catalog().definitions()[0].name, colliding_name);
        runtime.close().await;
    }

    #[tokio::test]
    async fn conflicting_project_candidate_waits_for_dynamic_unload() {
        let directory = tempfile::tempdir().unwrap();
        let started = directory.path().join("started");
        let closed = directory.path().join("closed");
        let runtime = empty_runtime("session-project-update", directory.path());
        let source = runtime.update_source();
        runtime
            .load(
                "load-session-tool",
                vec![stdio_server("shared-name", "echo", &started, &closed)],
            )
            .await
            .unwrap();
        let initial_delta = source
            .take_update()
            .unwrap()
            .expect("动态 Server 首次发布应产生目录更新");
        assert_eq!(
            initial_delta.added(),
            &[keencode_tools::portable_mcp_tool_name("shared-name", "echo").unwrap()]
        );
        source
            .acknowledge_update(initial_delta.generation())
            .expect("Provider 已接受初始目录后应推进观察水位");

        assert_eq!(
            runtime.queue_project_snapshot(ProjectToolSnapshot {
                version: ProjectCatalogVersion {
                    generation: 9,
                    revoked: false,
                },
                server_names: BTreeSet::from(["shared-name".to_owned()]),
                tools: vec![NamedTool::new("project-only".to_owned())],
            }),
            Err(SessionMcpError::CatalogConflict)
        );
        assert_eq!(
            source.take_update(),
            Err(AgentToolCatalogUpdateError::new(
                "Session MCP 目录存在名称冲突"
            )),
            "冲突候选已排队，但在动态 Server 释放前不得覆盖当前目录"
        );
        assert_eq!(
            runtime.status().unwrap().servers[0].status,
            SessionMcpServerPhase::Ready
        );
        assert!(runtime.catalog().definitions().iter().any(|definition| {
            definition.name
                == keencode_tools::portable_mcp_tool_name("shared-name", "echo").unwrap()
        }));

        runtime
            .unload("unload-conflict", "shared-name")
            .await
            .expect("释放动态 Server 后应应用最新项目候选");
        let delta = source
            .take_update()
            .unwrap()
            .expect("解除冲突后项目目录应发布");
        assert_eq!(delta.added(), &["project-only".to_owned()]);
        assert_eq!(
            delta.removed(),
            &[keencode_tools::portable_mcp_tool_name("shared-name", "echo").unwrap()]
        );
        assert!(runtime.status().unwrap().servers.is_empty());
        assert_eq!(runtime.catalog().definitions()[0].name, "project-only");
        runtime.close().await;
        wait_for_file_lines(&closed, 1).await;
    }

    #[tokio::test]
    async fn exact_replacement_reuses_identical_bindings_and_rolls_back_failed_reconnects() {
        let directory = tempfile::tempdir().unwrap();
        let started = directory.path().join("started");
        let closed = directory.path().join("closed");
        let runtime = empty_runtime("session-exact", directory.path());
        let first = stdio_server("local", "echo", &started, &closed);

        runtime.replace_exact(vec![first.clone()]).await.unwrap();
        assert_eq!(runtime.status().unwrap().catalog_generation, 2);
        assert_eq!(file_line_count(&started), 1);
        let replacement_runner = runtime.update_source();
        let replacement_delta = replacement_runner
            .take_update()
            .unwrap()
            .expect("replace_exact 发布后新 Runner 应收到目录变化");
        assert_eq!(replacement_delta.generation(), 2);
        assert_eq!(
            replacement_delta.added(),
            &[keencode_tools::portable_mcp_tool_name("local", "echo").unwrap()]
        );
        replacement_runner
            .acknowledge_update(replacement_delta.generation())
            .unwrap();
        runtime.replace_exact(vec![first]).await.unwrap();
        assert_eq!(runtime.status().unwrap().catalog_generation, 2);
        assert_eq!(file_line_count(&started), 1, "相同完整配置不得重连");

        assert_eq!(
            runtime.replace_exact(vec![missing_server("local")]).await,
            Err(SessionMcpError::InvalidConfiguration)
        );
        let retained = runtime.status().unwrap();
        assert_eq!(retained.catalog_generation, 2);
        assert_eq!(retained.servers[0].status, SessionMcpServerPhase::Ready);
        assert_eq!(file_line_count(&closed), 0, "失败替换不得关闭旧绑定");

        runtime
            .replace_exact(vec![stdio_server(
                "local",
                "replacement",
                &started,
                &closed,
            )])
            .await
            .unwrap();
        assert_eq!(runtime.status().unwrap().catalog_generation, 3);
        assert_eq!(file_line_count(&started), 2);
        wait_for_file_lines(&closed, 1).await;
        runtime.close().await;
        wait_for_file_lines(&closed, 2).await;
    }

    #[tokio::test]
    async fn same_name_definition_change_is_reported_as_changed() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = SessionMcpRuntime::new(
            "session-schema-change".to_owned(),
            directory.path().to_path_buf(),
            ProjectToolSnapshot {
                version: ProjectCatalogVersion {
                    generation: 1,
                    revoked: false,
                },
                server_names: BTreeSet::new(),
                tools: vec![NamedTool::with_definition(ToolDefinition::new(
                    "same-tool",
                    "旧定义",
                    json!({ "type": "object", "properties": { "old": { "type": "string" } } }),
                ))],
            },
        )
        .unwrap();
        let source = runtime.update_source();
        runtime
            .queue_project_snapshot(ProjectToolSnapshot {
                version: ProjectCatalogVersion {
                    generation: 2,
                    revoked: false,
                },
                server_names: BTreeSet::new(),
                tools: vec![NamedTool::with_definition(ToolDefinition::new(
                    "same-tool",
                    "新定义",
                    json!({ "type": "object", "properties": { "new": { "type": "number" } } }),
                ))],
            })
            .unwrap();

        let delta = source
            .take_update()
            .unwrap()
            .expect("同名定义变化应产生目录通知");
        assert!(delta.added().is_empty());
        assert!(delta.removed().is_empty());
        assert_eq!(delta.changed(), &["same-tool".to_owned()]);
        source.acknowledge_update(delta.generation()).unwrap();
        runtime.close().await;
    }

    #[tokio::test]
    async fn failed_restore_keeps_suspended_runtime_available_for_retry() {
        let directory = tempfile::tempdir().unwrap();
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let owner = AgentRuntime::new_for_control_test(directory.path().join("data")).unwrap();
        let session = owner
            .open_or_create_session(&project_root, None, "session-mcp-restore")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);
        owner
            .ensure_session_mcp_runtime(&session_id, &project_root)
            .unwrap();
        let suspended = owner.suspend_session_mcp(&session_id).unwrap();

        // 模拟未串行的读路径在临时关闭窗口里创建了竞争侧车。
        owner
            .ensure_session_mcp_runtime(&session_id, &project_root)
            .unwrap();
        assert_eq!(
            owner.restore_session_mcp(&session_id, &project_root, &suspended),
            Err(SessionMcpError::StateUnavailable)
        );
        assert!(
            suspended.runtime.lock().unwrap().is_some(),
            "竞争失败不得消费原侧车所有权"
        );

        owner.close_session_mcp(&session_id).await;
        owner
            .restore_session_mcp(&session_id, &project_root, &suspended)
            .unwrap();
        assert!(suspended.runtime.lock().unwrap().is_none());
        owner.close_session(&session_id).await.unwrap();
    }

    /// 暂停窗口内发布的项目候选不会命中已摘除侧车；恢复后的首次 Prompt 绑定
    /// 必须从当前候选补齐目录，不能继续使用暂停前的旧项目快照。
    #[tokio::test]
    async fn restored_runtime_reconciles_project_candidate_missed_while_suspended() {
        let directory = tempfile::tempdir().unwrap();
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let owner = AgentRuntime::new_for_control_test(directory.path().join("data")).unwrap();
        let session = owner
            .open_or_create_session(&project_root, None, "session-mcp-candidate-restore")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);
        let original = owner
            .ensure_session_mcp_runtime(&session_id, &project_root)
            .unwrap();
        let suspended = owner.suspend_session_mcp(&session_id).unwrap();

        owner
            .publish_extension_candidate(
                &project_root,
                RuntimeExtensionCandidate::new(
                    1,
                    Arc::new(ProjectMcpContributor {
                        tools: vec![NamedTool::new("candidate-after-suspend".to_owned())],
                    }),
                )
                .unwrap(),
            )
            .unwrap();
        owner
            .restore_session_mcp(&session_id, &project_root, &suspended)
            .unwrap();

        let restored = owner
            .ensure_session_mcp_runtime(&session_id, &project_root)
            .unwrap();
        assert!(Arc::ptr_eq(&restored, &original));
        let update = restored
            .update_source()
            .take_update()
            .unwrap()
            .expect("恢复后的绑定应发布暂停期间的新项目候选");
        assert_eq!(update.added(), &["candidate-after-suspend".to_owned()]);
        assert_eq!(
            restored.catalog().definitions()[0].name,
            "candidate-after-suspend"
        );
        owner.close_session(&session_id).await.unwrap();
    }

    #[tokio::test]
    async fn session_mcp_failure_state_is_fresh_after_cold_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let storage_root = directory.path().join("data");

        let runtime = AgentRuntime::new_for_control_test(storage_root.clone()).unwrap();
        let session = runtime
            .open_or_create_session(&project_root, None, "session-mcp-cold-recovery")
            .unwrap();
        let session_id = session.session_id().as_str().to_owned();
        drop(session);
        runtime
            .ensure_session_mcp_runtime(&session_id, &project_root)
            .unwrap();
        let failed = runtime
            .load_session_mcp(
                &session_id,
                "cold-failure",
                vec![missing_server("cold-only")],
            )
            .await
            .unwrap();
        assert_eq!(failed.servers.len(), 1);
        runtime.shutdown().await.unwrap();
        drop(runtime);

        let cold = AgentRuntime::new_for_control_test(storage_root).unwrap();
        cold.open_or_create_session(&project_root, Some(&session_id), "cold-open")
            .unwrap();
        let status = cold.session_mcp_status(&session_id).unwrap();
        assert!(
            status.servers.is_empty(),
            "冷恢复只恢复持久 Session，不应恢复进程内失败记录"
        );
        let retried = cold
            .load_session_mcp(&session_id, "cold-retry", vec![missing_server("cold-only")])
            .await
            .unwrap();
        assert_eq!(retried.servers.len(), 1);
        cold.shutdown().await.unwrap();
    }
}
