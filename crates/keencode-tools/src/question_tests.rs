//! AskUser 桌面交互边界的确定性测试。

use std::sync::Arc;

use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolContext, TurnCancellation, TurnId,
};
use keencode_model::ToolResultContent;
use serde_json::{Value, json};

use crate::{
    AskUserTool, UserQuestionAnswer, UserQuestionError, UserQuestionFuture, UserQuestionHandler,
    UserQuestionRequest, UserQuestionResponse,
};

/// 返回预置回答并保留最近请求的合成桌面端口。
struct FixedQuestionHandler {
    /// 端口要返回的预置回答。
    response: UserQuestionResponse,
}

impl UserQuestionHandler for FixedQuestionHandler {
    /// 立即返回预置回答。
    fn ask(&self, _request: UserQuestionRequest) -> UserQuestionFuture<'_> {
        let response = self.response.clone();
        Box::pin(async move { Ok(response) })
    }
}

/// 永远不完成，用于验证 Turn 取消关闭问答。
struct PendingQuestionHandler;

impl UserQuestionHandler for PendingQuestionHandler {
    /// 保持等待直到工具层因取消丢弃 Future。
    fn ask(&self, _request: UserQuestionRequest) -> UserQuestionFuture<'_> {
        Box::pin(std::future::pending::<
            Result<UserQuestionResponse, UserQuestionError>,
        >())
    }
}

/// 创建独立问答工具上下文。
fn context(cancellation: TurnCancellation) -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-question").unwrap(),
        turn_id: TurnId::new("turn-question").unwrap(),
        source_agent_id: AgentId::new("agent-question").unwrap(),
        tool_call_id: ToolCallId::new("call-question").unwrap(),
        cancellation,
    }
}

/// 提取问答工具的唯一文本结果并解析 JSON。
fn output_json(output: keencode_agent::ToolOutput) -> Value {
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("AskUser 必须返回唯一文本结果");
    };
    serde_json::from_str(text).expect("AskUser 输出必须是 JSON")
}

/// 回答必须按请求顺序规范化，并支持预设与自定义文本。
#[tokio::test]
async fn ask_user_validates_and_orders_answers() {
    let tool = AskUserTool::new(Arc::new(FixedQuestionHandler {
        response: UserQuestionResponse {
            answers: vec![
                UserQuestionAnswer {
                    id: "path".to_owned(),
                    values: vec!["自定义路径".to_owned()],
                },
                UserQuestionAnswer {
                    id: "mode".to_owned(),
                    values: vec!["安全".to_owned()],
                },
            ],
        },
    }));
    let output = tool
        .execute(
            context(TurnCancellation::new()),
            json!({
                "questions": [
                    {
                        "id": "mode",
                        "prompt": "选择模式",
                        "options": [{"label": "安全", "description": "先确认"}],
                        "allowCustom": false
                    },
                    {
                        "id": "path",
                        "prompt": "输入路径"
                    }
                ]
            }),
        )
        .await
        .expect("有效回答应成功");
    let value = output_json(output);
    assert_eq!(value["answers"][0]["id"], "mode");
    assert_eq!(value["answers"][1]["id"], "path");
}

/// 非法标识、重复选项和不允许的自定义答案都不能越过工具边界。
#[tokio::test]
async fn ask_user_rejects_invalid_questions_and_answers() {
    let invalid_questions = AskUserTool::new(Arc::new(FixedQuestionHandler {
        response: UserQuestionResponse { answers: vec![] },
    }));
    let error = invalid_questions
        .execute(
            context(TurnCancellation::new()),
            json!({"questions": [{"id": "../bad", "prompt": "问题"}]}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_input");

    let invalid_answer = AskUserTool::new(Arc::new(FixedQuestionHandler {
        response: UserQuestionResponse {
            answers: vec![UserQuestionAnswer {
                id: "mode".to_owned(),
                values: vec!["未知".to_owned()],
            }],
        },
    }));
    let error = invalid_answer
        .execute(
            context(TurnCancellation::new()),
            json!({
                "questions": [{
                    "id": "mode",
                    "prompt": "选择模式",
                    "options": [{"label": "安全"}],
                    "allowCustom": false
                }]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_user_response");

    let unanswerable = AskUserTool::new(Arc::new(FixedQuestionHandler {
        response: UserQuestionResponse { answers: vec![] },
    }));
    let error = unanswerable
        .execute(
            context(TurnCancellation::new()),
            json!({
                "questions": [{
                    "id": "blocked",
                    "prompt": "无法回答的问题",
                    "allowCustom": false
                }]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_input");
}

/// Turn 取消必须停止等待桌面问答且返回稳定取消错误。
#[tokio::test]
async fn ask_user_observes_turn_cancellation() {
    let cancellation = TurnCancellation::new();
    let tool = AskUserTool::new(Arc::new(PendingQuestionHandler));
    let future = tool.execute(
        context(cancellation.clone()),
        json!({"questions": [{"id": "choice", "prompt": "请选择"}]}),
    );
    cancellation.cancel();
    let error = future.await.unwrap_err();
    assert_eq!(error.code, "ask_user_cancelled");
}
