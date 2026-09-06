#![deny(missing_docs)]
#![forbid(unsafe_code)]

//! KeenCode 自研 Agent Runtime 的跨平台内置工具。

mod background;
mod collaboration_tools;
mod command;
mod deferred;
mod environment;
mod filesystem;
mod lsp;
mod mcp;
mod question;
mod search;
mod skill;
mod state_tools;
mod web;
mod worktree;

pub use background::{
    BackgroundCancelReport, BackgroundOutputCursor, BackgroundShutdownReport,
    BackgroundTaskCompletion, BackgroundTaskError, BackgroundTaskInfo, BackgroundTaskManager,
    BackgroundTaskOutput, BackgroundTaskStatus, TaskOutputTool, TaskStopTool,
};
pub use collaboration_tools::{
    CompletedTurnContext, FollowupTaskTool, InterruptAgentTool, ListAgentsTool,
    ResolvedSpawnAgentTemplate, RetryAgentTool, SendMessageTool, SpawnAgentContextSource,
    SpawnAgentTemplateContext, SpawnAgentTemplateResolver, SpawnAgentTool, WaitAgentTool,
    register_collaboration_tools, register_collaboration_tools_with_template_resolver,
    retain_child_agent_tool_snapshot,
};
pub use command::{
    BashTool, BoundedCommandError, BoundedCommandOutput, BoundedCommandRequest, GitTool,
    PowerShellTool, run_bounded_command,
};
pub use deferred::{
    DeferredToolCatalog, DeferredToolCatalogError, ExecuteExtraTool, ToolSearchTool,
    register_deferred_tools,
};
pub use environment::{FileMutationRecorder, PreparedFileMutation, ToolEnvironment, ToolLimits};
pub use filesystem::{EditTool, ReadTool, WriteTool};
pub use lsp::{
    LspDiagnostic, LspDiagnosticCode, LspPreparationReport, LspRuntime, LspRuntimeError,
    LspServerConfig, LspTool, register_lsp_tool,
};
pub use mcp::{
    McpDiagnosticCode, McpToolBridgeError, McpToolBuildReport, McpToolDiagnostic,
    build_mcp_deferred_tools, build_mcp_deferred_tools_best_effort, portable_mcp_tool_name,
    prepare_mcp_server_tools,
};
pub use question::{
    AskUserTool, UserQuestion, UserQuestionAnswer, UserQuestionError, UserQuestionFuture,
    UserQuestionHandler, UserQuestionOption, UserQuestionRequest, UserQuestionResponse,
};
pub use search::{GlobTool, GrepTool};
pub use skill::SkillTool;
pub use state_tools::{GoalTool, PlanTool, TodoWriteTool, register_state_tools};
pub use web::{
    WebFetchTool, WebSearchTool, WebServiceConfig, WebToolRegistrationError, register_web_tools,
};
pub use worktree::{
    GitWorktreeCleanupFailure, GitWorktreeCleanupReport, GitWorktreeCreateRequest,
    GitWorktreeLeaseError, GitWorktreeLeaseManager, GitWorktreeReleaseOutcome, ManagedGitWorktree,
};

use std::sync::Arc;

use keencode_agent::{ToolRegistry, ToolRegistryError};

/// 把文件读取、精确编辑、原子写入、Glob 与 Grep 注册到工具表。
pub fn register_local_tools(
    registry: &mut ToolRegistry,
    environment: Arc<ToolEnvironment>,
) -> Result<(), ToolRegistryError> {
    registry.register(Arc::new(ReadTool::new(environment.clone())))?;
    registry.register(Arc::new(EditTool::new(environment.clone())))?;
    registry.register(Arc::new(WriteTool::new(environment.clone())))?;
    registry.register(Arc::new(GlobTool::new(environment.clone())))?;
    registry.register(Arc::new(GrepTool::new(environment.clone())))?;
    registry.register(Arc::new(BashTool::new(environment.clone())))?;
    registry.register(Arc::new(PowerShellTool::new(environment.clone())))?;
    registry.register(Arc::new(GitTool::new(environment)))?;
    Ok(())
}

/// 使用跨 Turn 共享的 Manager 注册完整本地工具集，包括真实后台 Shell 输出与停止工具。
pub fn register_local_tools_with_background(
    registry: &mut ToolRegistry,
    environment: Arc<ToolEnvironment>,
    background_tasks: Arc<BackgroundTaskManager>,
) -> Result<(), ToolRegistryError> {
    registry.register(Arc::new(ReadTool::new(environment.clone())))?;
    registry.register(Arc::new(EditTool::new(environment.clone())))?;
    registry.register(Arc::new(WriteTool::new(environment.clone())))?;
    registry.register(Arc::new(GlobTool::new(environment.clone())))?;
    registry.register(Arc::new(GrepTool::new(environment.clone())))?;
    registry.register(Arc::new(BashTool::with_background_tasks(
        environment.clone(),
        background_tasks.clone(),
    )))?;
    registry.register(Arc::new(PowerShellTool::with_background_tasks(
        environment.clone(),
        background_tasks.clone(),
    )))?;
    registry.register(Arc::new(GitTool::new(environment)))?;
    registry.register(Arc::new(TaskOutputTool::new(background_tasks.clone())))?;
    registry.register(Arc::new(TaskStopTool::new(background_tasks)))?;
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod question_tests;

#[cfg(test)]
mod collaboration_tools_tests;

#[cfg(test)]
mod background_tests;

#[cfg(test)]
mod deferred_tests;

#[cfg(test)]
mod mcp_tests;

#[cfg(test)]
mod lsp_tests;

#[cfg(test)]
mod skill_tests;
