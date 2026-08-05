use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, MultiplexBroker,
    UserInteractionBroker,
};

// ─── MockBroker ──────────────────────────────────────────────────────────────

/// 记录是否被调用的 mock broker
struct MockBroker {
    /// 保存接收到的 InteractionContext
    received: Mutex<Option<InteractionContext>>,
    /// 保存响应
    response: InteractionResponse,
}

impl MockBroker {
    fn new(response: InteractionResponse) -> Self {
        Self {
            received: Mutex::new(None),
            response,
        }
    }
}

#[async_trait]
impl UserInteractionBroker for MockBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        *self.received.lock() = Some(ctx);
        self.response.clone()
    }
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

fn make_approval_context() -> InteractionContext {
    InteractionContext::Approval {
        items: vec![crate::interaction::ApprovalItem {
            tool_call_id: "call_1".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: serde_json::json!({}),
        }],
    }
}

// ─── MultiplexBroker 测试 ────────────────────────────────────────────────────

#[tokio::test]
async fn test_multiplex_空broker列表返回空decisions() {
    // Arrange
    let multiplex = MultiplexBroker::new(vec![]);

    // Act
    let resp = multiplex.request(make_approval_context()).await;

    // Assert
    match resp {
        InteractionResponse::Decisions(decisions) => {
            assert!(decisions.is_empty());
        }
        other => panic!("期望 Decisions，实际: {:?}", other),
    }
}

#[tokio::test]
async fn test_multiplex_单broker直接调用() {
    // Arrange
    let expected = InteractionResponse::Decisions(vec![ApprovalDecision::Approve {
        source: Some("mock".to_string()),
    }]);
    let mock = Arc::new(MockBroker::new(expected));
    let multiplex = MultiplexBroker::new(vec![("mock".to_string(), mock.clone())]);
    let ctx = make_approval_context();

    // Act
    let resp = multiplex.request(ctx).await;

    // Assert — 单 broker 走快速路径，直接返回 broker 的响应（不做 tag_source）
    match resp {
        InteractionResponse::Decisions(decisions) => {
            assert_eq!(decisions.len(), 1);
            match &decisions[0] {
                ApprovalDecision::Approve { source } => {
                    assert_eq!(source.as_deref(), Some("mock"));
                }
                other => panic!("期望 Approve，实际: {:?}", other),
            }
        }
        other => panic!("期望 Decisions，实际: {:?}", other),
    }
    // 验证 mock 确实被调用了
    assert!(mock.received.lock().is_some(), "mock broker 应被调用");
}

#[tokio::test]
async fn test_multiplex_多broker竞速先到先得() {
    // Arrange — 两个 mock broker，第一个快速响应 Approve，第二个缓慢响应 Reject
    let fast_response =
        InteractionResponse::Decisions(vec![ApprovalDecision::Approve { source: None }]);
    let slow_response = InteractionResponse::Decisions(vec![ApprovalDecision::Reject {
        reason: "too slow".to_string(),
        source: None,
    }]);

    struct DelayedMock {
        response: InteractionResponse,
        delay_ms: u64,
    }

    #[async_trait]
    impl UserInteractionBroker for DelayedMock {
        async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            self.response.clone()
        }
    }

    let fast = Arc::new(DelayedMock {
        response: fast_response,
        delay_ms: 0,
    });
    let slow = Arc::new(DelayedMock {
        response: slow_response,
        delay_ms: 500,
    });

    let multiplex = MultiplexBroker::new(vec![
        ("fast".to_string(), fast as Arc<dyn UserInteractionBroker>),
        ("slow".to_string(), slow as Arc<dyn UserInteractionBroker>),
    ]);

    // Act
    let resp = multiplex.request(make_approval_context()).await;

    // Assert — fast broker 先响应，结果被 tag_source 标记为 "fast"
    match resp {
        InteractionResponse::Decisions(decisions) => {
            assert_eq!(decisions.len(), 1);
            match &decisions[0] {
                ApprovalDecision::Approve { source } => {
                    assert_eq!(source.as_deref(), Some("fast"));
                }
                other => panic!("期望 Approve from fast，实际: {:?}", other),
            }
        }
        other => panic!("期望 Decisions，实际: {:?}", other),
    }
}
