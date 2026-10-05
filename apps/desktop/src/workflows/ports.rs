//! WorkflowHost 与资源层、AgentRuntime/WorkflowEngine 的适配端口。
//!
//! Host 不提供假的默认 Driver。生产装配必须注入真实 AgentRuntime 适配器；测试只能显式
//! 注入测试 Driver，因此不会把“内存跑通”误当成桌面运行时已接通。

use super::types::*;
use keencode_resources::{ArtifactCapacity, SessionMessage};
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub type WorkflowFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// 父 Journal 已确认、等待 Runtime 再次核验的 actor 身份。
///
/// `nodeAddress` 和 `projectRoot` 也参与绑定，避免只凭一个 Session ID 读取同项目中
/// 不属于当前 run 的历史；生产 reader 必须用 actor 自己的 `actor-bound` Journal 和
/// Session 快照核对这些字段。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowActorBinding {
    pub actor_session_id: String,
    pub parent_session_id: String,
    pub run_id: String,
    pub node_id: String,
    pub node_address: Value,
    pub project_root: String,
}

/// 读取已由父 Workflow Journal 绑定且经 actor 身份核验的 Session Transcript。
///
/// Actor 的工具事实仍在 actor 自己的 Resource Journal 中；Host 只通过这个只读端口
/// 取得冷恢复/在线都相同的 `SessionMessage`，再投影回已有的 `world-read/world-run`
/// workspace 契约。实现方必须按传入的完整绑定读取并校验权威 Runtime，不得从日志文件
/// 或前端缓存拼造记录。
pub trait WorkflowActorTranscriptReader: Send + Sync {
    fn read_transcript(
        &self,
        binding: &WorkflowActorBinding,
    ) -> Result<Option<Vec<SessionMessage>>, WorkflowJournalError>;
}

/// 将 AgentRuntime 的只读 Transcript 回调包装为 WorkflowHost 端口。
pub fn workflow_actor_transcript_reader<F>(callback: F) -> Arc<dyn WorkflowActorTranscriptReader>
where
    F: Fn(&WorkflowActorBinding) -> Result<Option<Vec<SessionMessage>>, WorkflowJournalError>
        + Send
        + Sync
        + 'static,
{
    Arc::new(ClosureWorkflowActorTranscriptReader { callback })
}

struct ClosureWorkflowActorTranscriptReader<F> {
    callback: F,
}

impl<F> WorkflowActorTranscriptReader for ClosureWorkflowActorTranscriptReader<F>
where
    F: Fn(&WorkflowActorBinding) -> Result<Option<Vec<SessionMessage>>, WorkflowJournalError>
        + Send
        + Sync
        + 'static,
{
    fn read_transcript(
        &self,
        binding: &WorkflowActorBinding,
    ) -> Result<Option<Vec<SessionMessage>>, WorkflowJournalError> {
        (self.callback)(binding)
    }
}

/// 真实桌面问答路由。工作流没有可伪造的父 Agent Turn，问答必须交给
/// AgentRuntime 当前待决的 Elicitation 请求；该端口只携带稳定请求 ID 与用户答案。
pub trait WorkflowQuestionResolver: Send + Sync {
    fn resolve(
        &self,
        qid: String,
        answer: String,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>>;
}

/// 将桌面 Runtime 的待决问答回调包装为 Host 可注入的 resolver。
pub fn workflow_question_resolver<F>(callback: F) -> Arc<dyn WorkflowQuestionResolver>
where
    F: Fn(String, String) -> WorkflowFuture<Result<(), WorkflowRuntimeError>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(ClosureWorkflowQuestionResolver { callback })
}

struct ClosureWorkflowQuestionResolver<F> {
    callback: F,
}

impl<F> WorkflowQuestionResolver for ClosureWorkflowQuestionResolver<F>
where
    F: Fn(String, String) -> WorkflowFuture<Result<(), WorkflowRuntimeError>>
        + Send
        + Sync
        + 'static,
{
    fn resolve(
        &self,
        qid: String,
        answer: String,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>> {
        (self.callback)(qid, answer)
    }
}

/// Journal 提交的结果。Resource Journal 负责分配严格连续的 run sequence。
#[derive(Clone, Debug)]
pub struct CommittedWorkflowEvent {
    pub event: WorkflowJournalEvent,
}

/// Journal 中已存在的启动事实，用于按父会话和 `launchInputId` 做持久幂等查询。
///
/// 该读面只返回 Journal 里的事实，不缓存运行状态；Host 会继续读取同一 run 的完整
/// 事件流校验 definition/input/model 等冻结值后才决定复用或返回冲突。
#[derive(Clone, Debug)]
pub struct WorkflowLaunchRecord {
    pub run_id: String,
    pub event: WorkflowJournalEvent,
}

/// Workflow Journal 的最小读写面。
///
/// 生产实现应把 `commit_event` 映射为 `RuntimeSession::append_workflow_event`，把查询映射
/// 为同一 SessionState/ArtifactStore 的读面。运行 Host 只保留有界投影，不把投影当事实源。
pub trait WorkflowJournalPort: Send + Sync {
    fn commit_event(
        &self,
        operation_id: &str,
        event: WorkflowJournalEvent,
    ) -> Result<CommittedWorkflowEvent, WorkflowJournalError>;

    fn list_events(
        &self,
        run_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<WorkflowEventPage, WorkflowJournalError>;

    /// 读取 run 的完整事件流，仅供恢复和冻结值校验使用；对外查询必须走分页接口。
    fn all_events(&self, run_id: &str) -> Result<Vec<WorkflowJournalEvent>, WorkflowJournalError>;

    fn list_runs(
        &self,
        parent_session_id: Option<&str>,
        name: Option<&str>,
        scope: Option<WorkflowScope>,
        limit: usize,
    ) -> Result<WorkflowRunsResult, WorkflowJournalError>;

    /// 在权威 Journal 中查找同一父会话下全部同 `launchInputId` 的启动事实。
    ///
    /// 运行历史列表有面向 UI 的数量上限，不能用于可靠的幂等判断；生产实现应从
    /// SessionState 的完整 workflow event map 读取这里的结果。
    fn find_launches(
        &self,
        parent_session_id: &str,
        launch_input_id: &str,
    ) -> Result<Vec<WorkflowLaunchRecord>, WorkflowJournalError>;

    fn run_summary(&self, run_id: &str)
    -> Result<Option<WorkflowRunSummary>, WorkflowJournalError>;

    /// 在新的 WorkflowHost 接管同一父会话时，把上一次进程遗留的未结算执行标记为中断。
    ///
    /// 这是冷启动边界上的一次性协调点，不是内存状态缓存；生产 Journal 必须把结果
    /// 追加回同一个父 Session Journal。测试/非持久实现没有冷恢复事实时保持空操作。
    fn reconcile_cold_runs(&self) -> Result<(), WorkflowJournalError> {
        Ok(())
    }

    /// Journal-backed workspace transcript: the result body is fetched separately.
    fn list_workspace_nodes(
        &self,
        run_id: &str,
    ) -> Result<Option<WorkflowWorkspaceResult>, WorkflowJournalError>;

    fn read_node_result(
        &self,
        run_id: &str,
        site_id: &str,
        ordinal: u32,
        max_bytes: usize,
    ) -> Result<Option<WorkflowNodeResult>, WorkflowJournalError>;

    fn list_artifacts(
        &self,
        run_id: &str,
    ) -> Result<Option<Vec<WorkflowArtifact>>, WorkflowJournalError>;

    fn list_artifact_items(
        &self,
        run_id: &str,
        artifact_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<WorkflowArtifactPage, WorkflowJournalError>;

    fn read_artifact(
        &self,
        run_id: &str,
        artifact_id: &str,
        version: u32,
        offset: usize,
        limit: usize,
    ) -> Result<Option<WorkflowArtifactBytes>, WorkflowJournalError>;

    /// 读取同一父 Session ArtifactStore 的当前容量；预检不等于跨进程 reservation。
    fn artifact_capacity(&self) -> Result<ArtifactCapacity, WorkflowJournalError>;
}

/// 运行请求中的冻结值。定义哈希、输入、模型和预算一旦启动不能由 resume 改写。
#[derive(Clone, Debug)]
pub struct WorkflowExecutionRequest {
    pub run_id: String,
    pub definition: WorkflowDefinition,
    pub canonical_hash: String,
    pub parent_session_id: String,
    pub tool_call_id: Option<String>,
    pub launch_input_id: Option<String>,
    /// 已按定义声明补全并校验的顶层 JSON object；保持引擎的原始输入形状。
    pub inputs: Value,
    pub cwd: PathBuf,
    pub model_selection: Option<Value>,
    pub budgets: Option<Value>,
    pub predecessor_run_id: Option<String>,
}

/// Host 在写入启动/恢复事实之前要求 Driver 执行的无副作用预检阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowPreflightMode {
    /// 全新 run；没有可复用的完成节点。
    Start,
    /// 从 Journal 恢复；已确认完成的 Tool/Artifact 节点可以复用。
    Resume,
}

/// AgentRuntime/WorkflowEngine 向 Host 回报进度时使用的单向 sink。
pub trait WorkflowProgressSink: Send + Sync {
    /// 发布已经由 Journal 提交的事件；此处不能再次写 Journal，否则并发事件会重复。
    fn publish(
        &self,
        payload: DynamicWorkflowRunProgressPayload,
        artifacts: Vec<ArtifactUse>,
    ) -> Result<(), WorkflowJournalError>;
}

/// 正式运行适配器。实现必须调用真实 WorkflowEngine 与 AgentRuntime，不得以文件日志替代。
pub trait WorkflowExecutionDriver: Send + Sync {
    /// 在任何 Journal 启动/恢复事实和副作用之前检查编译预算、Artifact 容量等条件。
    ///
    /// 测试 Driver 可保持默认无操作；生产 Engine Driver 必须接真实 Journal/Store，
    /// 并在容量不足时返回带 `artifact_capacity` 稳定前缀的错误。
    fn preflight(
        &self,
        _request: &WorkflowExecutionRequest,
        _mode: WorkflowPreflightMode,
    ) -> Result<(), WorkflowRuntimeError> {
        Ok(())
    }

    /// 在 Host 提交 `run-started`/`run-resumed` 后、派生异步执行任务前预留运行资源。
    ///
    /// 取消请求可能紧跟启动请求到达；生产 Driver 应在此处注册取消令牌，不能把注册
    /// 推迟到 `start`/`resume` future 第一次被 poll 时。测试 Driver 默认无资源需要预留。
    fn prepare(&self, _run_id: &str) -> Result<(), WorkflowRuntimeError> {
        Ok(())
    }

    fn start(
        &self,
        request: WorkflowExecutionRequest,
        sink: Arc<dyn WorkflowProgressSink>,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>>;

    fn cancel(&self, run_id: String) -> WorkflowFuture<Result<(), WorkflowRuntimeError>>;

    fn resume(
        &self,
        request: WorkflowExecutionRequest,
        sink: Arc<dyn WorkflowProgressSink>,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>>;

    fn resolve_question(
        &self,
        qid: String,
        answer: String,
    ) -> WorkflowFuture<Result<(), WorkflowRuntimeError>>;
}
