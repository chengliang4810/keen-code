use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::interaction::{
    channel_types::ChannelNotification, channel_types::PermissionResponse, ApprovalDecision,
    ApprovalItem, ChannelBroker, ChannelNotificationSender, ChannelState, InteractionContext,
    InteractionResponse, UserInteractionBroker,
};

// ─── Mock ChannelNotificationSender ──────────────────────────────────────────

/// 记录所有 send_notification 调用
struct MockNotificationSender {
    calls: Mutex<Vec<(String, String, serde_json::Value)>>,
}

impl MockNotificationSender {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ChannelNotificationSender for MockNotificationSender {
    async fn send_notification(
        &self,
        server_name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), String> {
        self.calls
            .lock()
            .push((server_name.to_string(), method.to_string(), params));
        Ok(())
    }
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

fn make_approval_item(name: &str) -> ApprovalItem {
    ApprovalItem {
        tool_call_id: format!("call_{}", name),
        tool_name: name.to_string(),
        tool_input: serde_json::json!({}),
    }
}

// ─── ChannelState 同步测试 ───────────────────────────────────────────────────

#[test]
fn test_channel_state_authorize_添加授权() {
    // Arrange
    let state = ChannelState::new();

    // Act
    state.authorize("my-server", "server:my-server".to_string());

    // Assert
    let authorized = state.authorized.read();
    assert_eq!(
        authorized.get("my-server"),
        Some(&"server:my-server".to_string())
    );
}

#[test]
fn test_channel_state_authorize_覆盖旧授权() {
    // Arrange
    let state = ChannelState::new();
    state.authorize("my-server", "old_source".to_string());

    // Act
    state.authorize("my-server", "new_source".to_string());

    // Assert
    let authorized = state.authorized.read();
    assert_eq!(authorized.len(), 1);
    assert_eq!(authorized.get("my-server"), Some(&"new_source".to_string()));
}

#[test]
fn test_channel_state_revoke_移除授权() {
    // Arrange
    let state = ChannelState::new();
    state.authorize("my-server", "server:my-server".to_string());

    // Act
    state.revoke("my-server");

    // Assert
    let authorized = state.authorized.read();
    assert!(authorized.is_empty());
}

#[test]
fn test_channel_state_revoke_不存在的server无异常() {
    // Arrange
    let state = ChannelState::new();

    // Act & Assert — 不应 panic
    state.revoke("nonexistent");
    assert!(state.authorized.read().is_empty());
}

#[test]
fn test_channel_state_close_all_清空所有授权() {
    // Arrange
    let state = ChannelState::new();
    state.authorize("server-a", "source:a".to_string());
    state.authorize("server-b", "source:b".to_string());

    // Act
    state.close_all();

    // Assert
    assert!(state.authorized.read().is_empty());
}

#[test]
fn test_channel_state_register_session() {
    // Arrange
    let state = ChannelState::new();
    let (tx, _rx) = mpsc::unbounded_channel::<ChannelNotification>();

    // Act
    state.register_session("sess-1".to_string(), tx);

    // Assert
    let txs = state.channel_msg_txs.read();
    assert!(txs.contains_key("sess-1"));
}

#[test]
fn test_channel_state_unregister_session() {
    // Arrange
    let state = ChannelState::new();
    let (tx, _rx) = mpsc::unbounded_channel::<ChannelNotification>();
    state.register_session("sess-1".to_string(), tx);

    // Act
    state.unregister_session("sess-1");

    // Assert
    assert!(state.channel_msg_txs.read().is_empty());
}

#[test]
fn test_channel_state_unregister_不存在的session无异常() {
    // Arrange
    let state = ChannelState::new();

    // Act & Assert — 不应 panic
    state.unregister_session("nonexistent");
    assert!(state.channel_msg_txs.read().is_empty());
}

// ─── ChannelBroker 异步测试 ──────────────────────────────────────────────────

#[tokio::test]
async fn test_channel_broker_无授权server全部reject() {
    // Arrange
    let state = ChannelState::new();
    let sender = Arc::new(MockNotificationSender::new());
    let broker = ChannelBroker::new(state, sender);
    let items = vec![make_approval_item("Bash"), make_approval_item("Write")];
    let ctx = InteractionContext::Approval { items };

    // Act
    let resp = broker.request(ctx).await;

    // Assert — 无授权 server 时立即返回 Reject，不等待超时
    match resp {
        InteractionResponse::Decisions(decisions) => {
            assert_eq!(decisions.len(), 2);
            for d in &decisions {
                match d {
                    ApprovalDecision::Reject { reason, source } => {
                        assert_eq!(reason, "no authorized channels");
                        assert!(source.is_none());
                    }
                    other => panic!("期望 Reject，实际: {:?}", other),
                }
            }
        }
        other => panic!("期望 Decisions，实际: {:?}", other),
    }
}

#[tokio::test]
async fn test_channel_broker_有授权server发送通知() {
    // Arrange
    let state = ChannelState::new();
    state.authorize("wechat-server", "server:wechat".to_string());
    let sender = Arc::new(MockNotificationSender::new());
    let broker = ChannelBroker::new(state, sender.clone());
    let items = vec![make_approval_item("Bash")];
    let ctx = InteractionContext::Approval { items };

    // Act — 在后台运行 request，它会在 pending_permissions 中注册 oneshot 并等待响应
    let handle = tokio::spawn(async move { broker.request(ctx).await });

    // 等待足够时间让 broker 发送通知并进入等待状态
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Assert — 通知已通过 MockNotificationSender 发出
    let calls = sender.calls.lock();
    assert_eq!(calls.len(), 1, "应为 1 个 item 发送 1 条通知");
    assert_eq!(calls[0].0, "wechat-server");
    assert_eq!(calls[0].1, "notifications/claude/permission_request");

    // 通过 pending_permissions 找到 oneshot sender，发送 Reject 以结束等待
    // （不 cleanup，handle abort 即可）
    handle.abort();
}

#[tokio::test]
async fn test_channel_broker_questions返回空answers() {
    // Arrange
    let state = ChannelState::new();
    let sender = Arc::new(MockNotificationSender::new());
    let broker = ChannelBroker::new(state, sender);
    let ctx = InteractionContext::Questions { requests: vec![] };

    // Act
    let resp = broker.request(ctx).await;

    // Assert
    match resp {
        InteractionResponse::Answers(answers) => {
            assert!(answers.is_empty());
        }
        other => panic!("期望 Answers，实际: {:?}", other),
    }
}

#[tokio::test]
async fn test_channel_broker_授权后响应approve() {
    // Arrange
    let state = ChannelState::new();
    state.authorize("test-server", "server:test".to_string());
    let sender = Arc::new(MockNotificationSender::new());
    let broker = ChannelBroker::new(state.clone(), sender);
    let items = vec![make_approval_item("Read")];
    let ctx = InteractionContext::Approval { items };

    // 启动后台任务：在短暂延迟后通过 pending_permissions 发送 Approve 响应
    let state_clone = state.clone();
    let approver = tokio::spawn(async move {
        // 等待 broker 注册 pending permission
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut pending = state_clone.pending_permissions.lock();
        // 取出第一个 entry（key + oneshot sender）
        if let Some(request_id) = pending.keys().next().cloned() {
            if let Some(tx) = pending.remove(&request_id) {
                let resp = PermissionResponse {
                    request_id,
                    approved: true,
                    reason: String::new(),
                };
                let _ = tx.send(resp);
            }
        }
    });

    // Act
    let resp = broker.request(ctx).await;
    let _ = approver.await;

    // Assert
    match resp {
        InteractionResponse::Decisions(decisions) => {
            assert_eq!(decisions.len(), 1);
            match &decisions[0] {
                ApprovalDecision::Approve { source } => {
                    assert_eq!(source.as_deref(), Some("channel"));
                }
                other => panic!("期望 Approve，实际: {:?}", other),
            }
        }
        other => panic!("期望 Decisions，实际: {:?}", other),
    }
}

#[tokio::test]
async fn test_channel_broker_授权后响应reject() {
    // Arrange
    let state = ChannelState::new();
    state.authorize("test-server", "server:test".to_string());
    let sender = Arc::new(MockNotificationSender::new());
    let broker = ChannelBroker::new(state.clone(), sender);
    let items = vec![make_approval_item("Bash"), make_approval_item("Write")];
    let ctx = InteractionContext::Approval { items };

    let state_clone = state.clone();
    let rejecter = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut pending = state_clone.pending_permissions.lock();
        if let Some(request_id) = pending.keys().next().cloned() {
            if let Some(tx) = pending.remove(&request_id) {
                let resp = PermissionResponse {
                    request_id,
                    approved: false,
                    reason: "user denied".to_string(),
                };
                let _ = tx.send(resp);
            }
        }
    });

    // Act
    let resp = broker.request(ctx).await;
    let _ = rejecter.await;

    // Assert
    match resp {
        InteractionResponse::Decisions(decisions) => {
            assert_eq!(decisions.len(), 2);
            for d in &decisions {
                match d {
                    ApprovalDecision::Reject { reason, source } => {
                        assert_eq!(reason, "user denied");
                        assert_eq!(source.as_deref(), Some("channel"));
                    }
                    other => panic!("期望 Reject，实际: {:?}", other),
                }
            }
        }
        other => panic!("期望 Decisions，实际: {:?}", other),
    }
}
