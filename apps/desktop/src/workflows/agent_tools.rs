//! Workflow AgentTool 适配。
//!
//! 工具只负责把已通过 AgentRunner 计划/权限守卫的调用转给桌面生产 Host。
//! 它不创建 Session、不保存 run 日志，也不直接访问文件系统；根装配层必须
//! 只给普通父会话注册这些工具，并在端口实现中再次拒绝 workflow actor。

use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    ToolRegistry, ToolRegistryError,
};
use keencode_model::ToolDefinition;
use serde_json::{Value, json};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// AgentTool 调用 Host 时携带的可信身份和已校验 JSON 参数。
///
/// `session_id`、`turn_id`、`source_agent_id` 和 `tool_call_id` 都来自
/// [`ToolContext`]，调用方输入不能覆盖。桌面端应使用 `session_id` 查找
/// 真实父 RuntimeSession，并自行注入已授权的 cwd、project storage 与模型快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowToolRequest {
    /// 当前普通父 Session。
    pub session_id: String,
    /// 当前父 Turn。
    pub turn_id: String,
    /// 调用工具的根 Agent 或单层 Agent 身份。
    pub source_agent_id: String,
    /// Runner 从模型调用冻结的工具调用身份。
    pub tool_call_id: String,
    /// Host 方法名，例如 `saveWorkflow` 或 `getWorkflowRun`。
    pub method: String,
    /// 已按工具 Schema 验证的 JSON 参数。
    pub args: Value,
}

/// WorkflowHost 端口返回的安全错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowToolError {
    /// 稳定机器错误码。
    pub code: String,
    /// 不携带凭据或未截断用户正文的展示说明。
    pub message: String,
    /// 是否允许运行时有限重试。
    pub retryable: bool,
}

impl WorkflowToolError {
    /// 创建不可重试错误。
    pub fn permanent(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}

impl fmt::Display for WorkflowToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WorkflowToolError {}

/// Workflow 工具端口的异步返回值。
pub type WorkflowToolFuture =
    Pin<Box<dyn Future<Output = Result<Value, WorkflowToolError>> + Send + 'static>>;

/// 由桌面应用装配的真实 WorkflowHost 调用边界。
///
/// 实现必须完成身份绑定、父会话归属、工作区授权和 actor 禁止检查，然后
/// 调用 [`crate::frontend_rpc::host::workflow_call`] 或等价的生产 Host 路径。
/// 不得使用内存 fake、第二份 run 日志或绕过 AgentRuntime 的权限入口。
pub trait WorkflowToolPort: Send + Sync {
    /// 执行一个已经由 AgentRunner 放行的工作流控制调用。
    fn call(&self, request: WorkflowToolRequest) -> WorkflowToolFuture;
}

/// 将闭包包装为 [`WorkflowToolPort`]，供桌面装配层绑定当前父 Session Host。
pub fn workflow_tool_port<F>(callback: F) -> Arc<dyn WorkflowToolPort>
where
    F: Fn(WorkflowToolRequest) -> WorkflowToolFuture + Send + Sync + 'static,
{
    Arc::new(ClosureWorkflowToolPort { callback })
}

struct ClosureWorkflowToolPort<F> {
    callback: F,
}

impl<F> WorkflowToolPort for ClosureWorkflowToolPort<F>
where
    F: Fn(WorkflowToolRequest) -> WorkflowToolFuture + Send + Sync + 'static,
{
    fn call(&self, request: WorkflowToolRequest) -> WorkflowToolFuture {
        (self.callback)(request)
    }
}

/// 把生产端口中的工作流工具注册进普通父 Agent 的冻结工具表。
///
/// 根 Runtime 应在 `RuntimeSession::is_workflow_actor()` 为 false 时调用此函数。
/// 叶 Agent 只使用受控的既有工具 registry，不会获得这些递归工作流入口。
pub fn register_workflow_tools(
    registry: &mut ToolRegistry,
    port: Arc<dyn WorkflowToolPort>,
) -> Result<(), ToolRegistryError> {
    registry.register(Arc::new(WorkflowAgentTool::new(
        "CreateWorkflow",
        "创建并保存一个纯 JSON WorkflowDefinitionV1；definition 必须是引擎唯一的 JSON v1 结构。",
        "createWorkflow",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
        definition_schema(),
        Arc::clone(&port),
    )))?;
    registry.register(Arc::new(WorkflowAgentTool::new(
        "SaveWorkflow",
        "保存一个已由引擎校验的纯 JSON WorkflowDefinitionV1 到授权的 project 或 global 工作流目录。",
        "saveWorkflow",
        ToolEffect::ChangesState,
        ToolConcurrency::Exclusive,
        definition_schema(),
        Arc::clone(&port),
    )))?;
    registry.register(Arc::new(WorkflowAgentTool::new(
        "GetWorkflowRun",
        "读取当前父会话拥有的 Workflow run 状态摘要；只返回 Journal 权威事实。",
        "getWorkflowRun",
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
        run_id_schema(),
        Arc::clone(&port),
    )))?;
    registry.register(Arc::new(WorkflowAgentTool::new(
        "GetWorkflowRunSituation",
        "读取当前 Workflow run 的状态情况；不启动、不恢复也不修改 run。",
        "runSummary",
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
        run_id_schema(),
        Arc::clone(&port),
    )))?;
    registry.register(Arc::new(WorkflowAgentTool::new(
        "GetWorkflowRunRoster",
        "读取当前 Workflow run 的可见 JSON 图；控制节点仍由 Rust 引擎持有。",
        "graph",
        ToolEffect::ReadOnly,
        ToolConcurrency::ParallelReadOnly,
        run_id_schema(),
        port,
    )))?;
    Ok(())
}

struct WorkflowAgentTool {
    name: &'static str,
    description: &'static str,
    method: &'static str,
    effect: ToolEffect,
    concurrency: ToolConcurrency,
    input_schema: Value,
    port: Arc<dyn WorkflowToolPort>,
}

impl WorkflowAgentTool {
    fn new(
        name: &'static str,
        description: &'static str,
        method: &'static str,
        effect: ToolEffect,
        concurrency: ToolConcurrency,
        input_schema: Value,
        port: Arc<dyn WorkflowToolPort>,
    ) -> Self {
        Self {
            name,
            description,
            method,
            effect,
            concurrency,
            input_schema,
            port,
        }
    }
}

impl AgentTool for WorkflowAgentTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(self.name, self.description, self.input_schema.clone())
    }

    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        validate_tool_input(self.name, input)?;
        Ok(self.effect)
    }

    fn concurrency(&self) -> ToolConcurrency {
        self.concurrency
    }

    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let name = self.name;
        let method = self.method.to_owned();
        let port = Arc::clone(&self.port);
        let cancellation = context.cancellation.clone();
        let request = WorkflowToolRequest {
            session_id: context.session_id.as_str().to_owned(),
            turn_id: context.turn_id.as_str().to_owned(),
            source_agent_id: context.source_agent_id.as_str().to_owned(),
            tool_call_id: context.tool_call_id.as_str().to_owned(),
            method,
            args: input,
        };
        Box::pin(async move {
            validate_tool_input(name, &request.args)?;
            if cancellation.is_cancelled() {
                return Err(ToolError::permanent("cancelled", "当前 Turn 已取消"));
            }
            let result = tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(ToolError::permanent("cancelled", "当前 Turn 已取消"));
                }
                result = port.call(request) => result.map_err(map_port_error),
            }?;
            let output = serde_json::to_string(&result).map_err(|_| {
                ToolError::permanent(
                    "workflow_result_invalid",
                    "WorkflowHost 结果无法编码为 JSON",
                )
            })?;
            Ok(ToolOutput::text(output))
        })
    }
}

fn map_port_error(error: WorkflowToolError) -> ToolError {
    if error.retryable {
        ToolError::retryable(error.code, error.message)
    } else {
        ToolError::permanent(error.code, error.message)
    }
}

fn validate_tool_input(name: &str, input: &Value) -> Result<(), ToolError> {
    let object = input.as_object().ok_or_else(|| {
        ToolError::permanent("invalid_input", format!("{name} 输入必须是 JSON object"))
    })?;
    match name {
        "CreateWorkflow" | "SaveWorkflow" => {
            reject_unknown_fields(object, ["definition", "scope"], name)?;
            if !object.get("definition").is_some_and(Value::is_object) {
                return Err(ToolError::permanent(
                    "invalid_input",
                    format!("{name}.definition 必须是 WorkflowDefinitionV1 JSON object"),
                ));
            }
            if let Some(scope) = object.get("scope")
                && !matches!(scope.as_str(), Some("project" | "global"))
            {
                return Err(ToolError::permanent(
                    "invalid_input",
                    "scope 必须是 project 或 global",
                ));
            }
        }
        "GetWorkflowRun" | "GetWorkflowRunSituation" | "GetWorkflowRunRoster" => {
            reject_unknown_fields(object, ["runId"], name)?;
            if !object
                .get("runId")
                .and_then(Value::as_str)
                .is_some_and(|run_id| !run_id.trim().is_empty())
            {
                return Err(ToolError::permanent(
                    "invalid_input",
                    "runId 必须是非空字符串",
                ));
            }
        }
        _ => {
            return Err(ToolError::permanent(
                "workflow_tool_unknown",
                "未知的 Workflow AgentTool",
            ));
        }
    }
    Ok(())
}

fn reject_unknown_fields<const N: usize>(
    object: &serde_json::Map<String, Value>,
    allowed: [&str; N],
    tool_name: &str,
) -> Result<(), ToolError> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("{tool_name} 不允许字段 {field}"),
        ));
    }
    Ok(())
}

fn definition_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "definition": { "type": "object" },
            "scope": { "type": "string", "enum": ["project", "global"] }
        },
        "required": ["definition"],
        "additionalProperties": false
    })
}

fn run_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "runId": { "type": "string", "minLength": 1, "maxLength": 1024 }
        },
        "required": ["runId"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingPort(Mutex<Vec<WorkflowToolRequest>>);

    impl WorkflowToolPort for RecordingPort {
        fn call(&self, request: WorkflowToolRequest) -> WorkflowToolFuture {
            self.0.lock().unwrap().push(request);
            Box::pin(async { Ok(json!({"ok": true})) })
        }
    }

    #[test]
    fn registry_exposes_only_json_definition_workflow_tools() {
        let mut registry = ToolRegistry::new();
        register_workflow_tools(&mut registry, Arc::new(RecordingPort::default())).unwrap();
        let names = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "CreateWorkflow",
                "GetWorkflowRun",
                "GetWorkflowRunRoster",
                "GetWorkflowRunSituation",
                "SaveWorkflow"
            ]
        );
        assert!(definition_schema()["properties"]["definition"].is_object());
    }

    #[test]
    fn tool_input_rejects_unknown_fields() {
        let error = validate_tool_input(
            "GetWorkflowRun",
            &json!({"runId": "run-1", "sessionId": "forged"}),
        )
        .expect_err("身份字段不能由模型注入");
        assert_eq!(error.code, "invalid_input");
    }

    #[test]
    fn definition_input_rejects_non_json_and_unknown_scope() {
        assert!(validate_tool_input("SaveWorkflow", &json!({})).is_err());
        assert!(
            validate_tool_input(
                "SaveWorkflow",
                &json!({"definition": [], "scope": "project"})
            )
            .is_err()
        );
        assert!(
            validate_tool_input(
                "SaveWorkflow",
                &json!({"definition": {}, "scope": "workspace"})
            )
            .is_err()
        );
        assert!(
            validate_tool_input(
                "SaveWorkflow",
                &json!({"definition": {}, "scope": "project"})
            )
            .is_ok()
        );
    }
}
