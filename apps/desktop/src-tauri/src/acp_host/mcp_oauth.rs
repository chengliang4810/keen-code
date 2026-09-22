//! 设置页 OAuth 使用应用级 ACP 扩展通知，不创建或污染任何 Session 投影。

use crate::agent_runtime::{ACP_DELIVERY_EVENT, AgentRuntime};
use crate::mcp_oauth::{McpOAuthEvent, McpOAuthEventSink, McpOAuthServiceError};
use keencode_acp::{McpOAuthEvent as ProtocolEvent, McpOAuthNotification};
use serde::Serialize;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

/// 应用级 OAuth 通知队列的有界容量，不使用会静默丢失终态的广播作为生产投递源。
const OAUTH_NOTIFICATION_CAPACITY: usize = 32;
/// 入队和工具候选刷新都必须有上限，不能反向锁死授权状态机。
const OAUTH_NOTIFICATION_ENQUEUE_TIMEOUT: Duration = Duration::from_secs(5);
/// 每个授权终态等待工具候选重建的最长时间。
const OAUTH_CANDIDATE_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
/// 密钥库或同一条目的状态锁阻塞时，不得无限占用应用唯一通知消费者。
const OAUTH_STATUS_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// 授权事件对当前项目工具候选的唯一处理决策。
#[derive(Debug, PartialEq, Eq)]
enum CandidateRefreshAction {
    /// 授权流程尚未完成，或临时错误没有使现有授权失效。
    Keep,
    /// 授权完成，按最新项目配置重新发现工具。
    Refresh,
    /// 授权已失效或无法确认，只撤销旧工具，等待下一次显式请求才重试。
    Revoke,
}

/// 终态只按当前权威状态影响工具；迟到失败不能撤销后来已恢复的授权。
async fn candidate_refresh_action(
    event: &McpOAuthEvent,
    status: impl Future<Output = Option<keencode_mcp::OAuthStatus>>,
    timeout: Duration,
) -> CandidateRefreshAction {
    match event {
        McpOAuthEvent::AuthorizationRequired { .. } => CandidateRefreshAction::Keep,
        McpOAuthEvent::Authorized { .. } => CandidateRefreshAction::Refresh,
        McpOAuthEvent::Failed { .. } => {
            if matches!(
                tokio::time::timeout(timeout, status).await,
                Ok(Some(keencode_mcp::OAuthStatus::Authorized))
            ) {
                CandidateRefreshAction::Keep
            } else {
                CandidateRefreshAction::Revoke
            }
        }
    }
}

/// 把 Registry 的终态有回执地交给唯一 ACP 传输通道。
pub(super) struct AcpOAuthEventSink {
    /// 单个应用工作者拥有消费端，所有生产事件必须确认入队或明确失败。
    sender: mpsc::Sender<McpOAuthEvent>,
    /// 本地入队最长等待时间；生产使用固定预算，测试可缩短以确定性验证背压。
    enqueue_timeout: Duration,
}

impl AcpOAuthEventSink {
    /// 创建事件适配器，不打开网络连接、不读取密钥库。
    pub(super) fn new(app: &AppHandle) -> Self {
        let (sender, receiver) = mpsc::channel(OAUTH_NOTIFICATION_CAPACITY);
        tauri::async_runtime::spawn(deliver_events(app.clone(), receiver));
        Self {
            sender,
            enqueue_timeout: OAUTH_NOTIFICATION_ENQUEUE_TIMEOUT,
        }
    }
}

/// 与 Session 投递共用 Tauri 通道的标准 JSON-RPC 通知外壳。
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApplicationDelivery {
    /// 不带 Session 身份的应用级 ACP 扩展通知。
    Notification {
        /// 封闭的 OAuth 通知类型，不能夹带任意 JSON 或访问令牌。
        notification: McpOAuthNotification,
    },
}

impl McpOAuthEventSink for AcpOAuthEventSink {
    /// 确认事件进入有界应用队列；不在 Registry 回调内重入候选构建锁。
    fn emit<'a>(
        &'a self,
        event: McpOAuthEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpOAuthServiceError>> + Send + 'a>> {
        Box::pin(async move {
            // 在入队前校验边界，错误事件不能占据队列并拖延后续终态。
            protocol_notification(event.clone())?;
            tokio::time::timeout(self.enqueue_timeout, self.sender.send(event))
                .await
                .map_err(|_| McpOAuthServiceError::EventDelivery)?
                .map_err(|_| McpOAuthServiceError::EventDelivery)
        })
    }
}

/// 在 Registry 释放锁后顺序处理通知；无待决事件时只有一次异步等待，不持续轮询。
async fn deliver_events(app: AppHandle, mut receiver: mpsc::Receiver<McpOAuthEvent>) {
    while let Some(event) = receiver.recv().await {
        let (project_scope, server_name) = match &event {
            McpOAuthEvent::Authorized {
                project_scope,
                server_name,
            }
            | McpOAuthEvent::Failed {
                project_scope,
                server_name,
                ..
            }
            | McpOAuthEvent::AuthorizationRequired {
                project_scope,
                server_name,
                ..
            } => (project_scope, server_name),
        };
        let action = candidate_refresh_action(
            &event,
            async {
                let registry = app
                    .try_state::<Arc<crate::mcp_oauth::McpOAuthRegistry>>()
                    .map(|state| state.inner().clone());
                if let Some(registry) = registry {
                    registry
                        .status(PathBuf::from(project_scope), server_name)
                        .await
                        .ok()
                        .map(|snapshot| snapshot.status)
                } else {
                    None
                }
            },
            OAUTH_STATUS_QUERY_TIMEOUT,
        )
        .await;
        if action != CandidateRefreshAction::Keep {
            let project_root = PathBuf::from(project_scope);
            let runtime = app
                .try_state::<Arc<AgentRuntime>>()
                .map(|state| state.inner().clone());
            if let Some(runtime) = runtime {
                if action == CandidateRefreshAction::Revoke
                    && runtime
                        .revoke_project_mcp_extension_tools(&project_root)
                        .is_err()
                {
                    tracing::error!("无法撤销失效 OAuth 所属项目的 MCP 工具目录");
                }
                // Failed 不能自动重新请求令牌，否则刷新失败会再次产生 Failed，
                // 在通知与候选构建之间形成不受用户操作控制的无限循环。
                if action == CandidateRefreshAction::Refresh {
                    let result = tokio::time::timeout(
                        OAUTH_CANDIDATE_REFRESH_TIMEOUT,
                        crate::extensions::ensure_runtime_extension_candidate(
                            &app,
                            &project_root,
                            &runtime,
                            true,
                        ),
                    )
                    .await;
                    if !matches!(result, Ok(Ok(_))) {
                        tracing::warn!("MCP OAuth 状态已变化，但扩展工具候选刷新失败或超时");
                    }
                }
            }
        }
        let result = protocol_notification(event).and_then(|notification| {
            app.emit(
                ACP_DELIVERY_EVENT,
                ApplicationDelivery::Notification { notification },
            )
            .map_err(|_| McpOAuthServiceError::EventDelivery)
        });
        if result.is_err() {
            // 令牌和状态仍由 Registry/密钥库持有，设置页下次查询读取权威状态。
            tracing::error!("MCP OAuth 通知投递失败，当前状态仍可通过 MCP 列表读取");
        }
    }
}

/// 只复制已脱敏的生命周期字段，并统一 Windows 前后端路径格式。
fn protocol_notification(
    event: McpOAuthEvent,
) -> Result<McpOAuthNotification, McpOAuthServiceError> {
    let event = match event {
        McpOAuthEvent::AuthorizationRequired {
            project_scope,
            server_name,
            authorization_url,
        } => ProtocolEvent::AuthorizationRequired {
            project_path: crate::path_utils::path_text_to_frontend(&project_scope),
            server_name,
            authorization_url,
        },
        McpOAuthEvent::Authorized {
            project_scope,
            server_name,
        } => ProtocolEvent::Authorized {
            project_path: crate::path_utils::path_text_to_frontend(&project_scope),
            server_name,
        },
        McpOAuthEvent::Failed {
            project_scope,
            server_name,
            message,
        } => ProtocolEvent::Failed {
            project_path: crate::path_utils::path_text_to_frontend(&project_scope),
            server_name,
            message,
        },
    };
    McpOAuthNotification::new(event).map_err(|_| McpOAuthServiceError::EventDelivery)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造不含任何凭据、可进入生产边界校验的通知。
    fn authorized_event() -> McpOAuthEvent {
        McpOAuthEvent::Authorized {
            project_scope: "D:/projects/demo".to_owned(),
            server_name: "demo-mcp".to_owned(),
        }
    }

    /// 临时失败或迟到失败不得撤销已恢复的有效授权；失效或缺失状态必须先撤销。
    #[tokio::test]
    async fn oauth_refresh_decision_uses_current_authoritative_status() {
        let event = McpOAuthEvent::Failed {
            project_scope: "D:/projects/demo".to_owned(),
            server_name: "demo-mcp".to_owned(),
            message: "授权请求失败".to_owned(),
        };
        for (status, expected) in [
            (
                Some(keencode_mcp::OAuthStatus::Authorized),
                CandidateRefreshAction::Keep,
            ),
            (
                Some(keencode_mcp::OAuthStatus::Idle),
                CandidateRefreshAction::Revoke,
            ),
            (None, CandidateRefreshAction::Revoke),
        ] {
            assert_eq!(
                candidate_refresh_action(&event, async { status }, Duration::from_secs(1)).await,
                expected,
            );
        }
    }

    /// 状态查询挂起必须有界失败，不能永久阻塞其他项目的授权通知。
    #[tokio::test]
    async fn oauth_refresh_decision_bounds_stalled_status_lookup() {
        let event = McpOAuthEvent::Failed {
            project_scope: "D:/projects/demo".to_owned(),
            server_name: "demo-mcp".to_owned(),
            message: "授权请求失败".to_owned(),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            candidate_refresh_action(&event, std::future::pending(), Duration::from_millis(10)),
        )
        .await
        .expect("挂起状态查询必须在独立外层预算前结束");
        assert_eq!(result, CandidateRefreshAction::Revoke);
    }

    /// 无需查询状态的通知不得触碰密钥库，避免引入多余等待或认证副作用。
    #[tokio::test]
    async fn oauth_refresh_decision_skips_lookup_for_non_failure_events() {
        let required = McpOAuthEvent::AuthorizationRequired {
            project_scope: "D:/projects/demo".to_owned(),
            server_name: "demo-mcp".to_owned(),
            authorization_url: "https://auth.example.test/authorize".to_owned(),
        };
        for (event, expected) in [
            (authorized_event(), CandidateRefreshAction::Refresh),
            (required, CandidateRefreshAction::Keep),
        ] {
            let status = async {
                panic!("非失败事件不应执行状态查询");
            };
            assert_eq!(
                candidate_refresh_action(&event, status, Duration::from_secs(1)).await,
                expected,
            );
        }
    }

    /// 成功仅表示实际入队；消费者关闭时必须明确失败，不能假称已投递。
    #[tokio::test]
    async fn oauth_notification_queue_confirms_admission_and_closed_receiver() {
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = AcpOAuthEventSink {
            sender,
            enqueue_timeout: Duration::from_millis(20),
        };
        sink.emit(authorized_event()).await.unwrap();
        assert_eq!(receiver.recv().await.unwrap(), authorized_event());
        drop(receiver);
        assert_eq!(
            sink.emit(authorized_event()).await,
            Err(McpOAuthServiceError::EventDelivery)
        );
    }

    /// 队列满时在预算内失败，取消等待后不得在未来容量释放时幽灵投递。
    #[tokio::test]
    async fn oauth_notification_queue_timeout_never_delivers_late() {
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = AcpOAuthEventSink {
            sender,
            enqueue_timeout: Duration::from_millis(20),
        };
        sink.emit(authorized_event()).await.unwrap();
        assert_eq!(
            sink.emit(authorized_event()).await,
            Err(McpOAuthServiceError::EventDelivery)
        );
        assert_eq!(receiver.recv().await.unwrap(), authorized_event());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        sink.emit(authorized_event()).await.unwrap();
        assert_eq!(receiver.recv().await.unwrap(), authorized_event());
    }

    /// 含凭据的非法通知必须在入队前被拒绝，不消耗终态队列容量。
    #[tokio::test]
    async fn oauth_notification_queue_rejects_secrets_before_admission() {
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = AcpOAuthEventSink {
            sender,
            enqueue_timeout: Duration::from_millis(20),
        };
        let event = McpOAuthEvent::AuthorizationRequired {
            project_scope: "D:/projects/demo".to_owned(),
            server_name: "demo-mcp".to_owned(),
            authorization_url: "https://auth.example.test/authorize?access_token=private-marker"
                .to_owned(),
        };
        let error = sink.emit(event).await.unwrap_err();
        assert_eq!(error, McpOAuthServiceError::EventDelivery);
        assert!(!format!("{error:?}").contains("private-marker"));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// 没有 Session 的设置页也能收到有明确项目身份的 OAuth 通知。
    #[test]
    fn oauth_notification_has_project_scope_and_no_session_identity() {
        let notification = protocol_notification(McpOAuthEvent::Authorized {
            project_scope: r"\\?\D:\projects\demo".to_owned(),
            server_name: "demo-mcp".to_owned(),
        })
        .unwrap();
        let value =
            serde_json::to_value(ApplicationDelivery::Notification { notification }).unwrap();
        assert_eq!(value["type"], "notification");
        assert_eq!(value["notification"]["method"], "keencode/mcp/oauth");
        assert_eq!(
            value["notification"]["params"]["projectPath"],
            "D:/projects/demo"
        );
        assert_eq!(
            value["notification"]["params"]["type"],
            "mcp_oauth_authorized"
        );
        assert!(value["notification"]["params"].get("sessionId").is_none());
        assert!(value["notification"].get("id").is_none());
    }
}
