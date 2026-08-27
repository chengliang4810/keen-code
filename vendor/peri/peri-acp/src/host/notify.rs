//! ACP Notification dispatch — handles incoming notifications and pushes
//! session update notifications. Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;

use crate::dispatch::config_update;
use agent_client_protocol::schema::v1::{AvailableCommandsUpdate, SessionUpdate};
use peri_acp_types::skills::SkillMetadata;
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{debug, info};

use super::{AcpServerConfig, SessionState};
use crate::provider::LlmProvider;

// ── Notification dispatch ────────────────────────────────────────────────────

pub(crate) fn handle_notification(
    method: &str,
    params: &Value,
    sessions: &mut HashMap<String, SessionState>,
    cfg: &AcpServerConfig,
) {
    match method {
        "session/cancel" => {
            let session_id = extract_session_id(params, "");
            if let Some(state) = sessions.get_mut(session_id) {
                let Some(token) = state.cancel_token.as_ref() else {
                    return;
                };
                // 多读者 + 单 writer lease：cancel 是写入操作，仅 writer 可发起。
                // 协议无客户端身份字段，writer 恒为 session 创建方（"default"）——
                // 观察者（非 writer）的 cancel 请求被忽略（只读）。
                if !state.lease.is_writer("default") {
                    debug!(session_id = %session_id, "Cancel ignored: read-only observer");
                    return;
                }
                token.cancel();
                info!(session_id = %session_id, "Cancel requested");
            }
        }
        "session/config_update" => {
            // Two formats:
            // 1. {"config": PeriConfig} — full config replace (from update_config)
            // 2. {"configId": "model"/"provider", "value": "..."} — partial (from set_config_option)
            if let Some(config_val) = params.get("config") {
                let new_cfg: crate::provider::PeriConfig = match serde_json::from_value(
                    config_val.clone(),
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "config_update notification: invalid config");
                        return;
                    }
                };
                tracing::info!(
                    provider_count = new_cfg.config.providers.len(),
                    "config_update notification: full config replace"
                );
                *cfg.peri_config.write() = new_cfg.clone();
                if let Some(p) = LlmProvider::from_config(&new_cfg) {
                    *cfg.provider.write() = p;
                }
            } else if let (Some(config_id), Some(value)) = (
                params.get("configId").and_then(|v| v.as_str()),
                params.get("value").and_then(|v| v.as_str()),
            ) {
                match config_id {
                    "model" => {
                        let new_provider = {
                            let c = cfg.peri_config.read();
                            let (provider_id, model) = match value.split_once("::") {
                                Some(parts) => parts,
                                None => {
                                    tracing::warn!(
                                        value,
                                        "config_update model must use provider_id::model"
                                    );
                                    return;
                                }
                            };
                            LlmProvider::from_provider_config(
                                &c,
                                provider_id,
                                model,
                                cfg.provider.read().effort().map(str::to_owned),
                                32_000,
                                cfg.provider.read().context_1m(),
                                None,
                            )
                        };
                        if let Some(p) = new_provider {
                            tracing::info!(model = %value, "config_update notification: model changed");
                            *cfg.provider.write() = p;
                        }
                    }
                    other => {
                        tracing::debug!(config_id = %other, "config_update notification: unhandled configId");
                    }
                }
            } else {
                tracing::debug!("config_update notification: missing config/configId");
            }
            // No sessions to invalidate — pool will be built fresh on next session/new
        }
        _ => {
            debug!(method = %method, "Unhandled notification");
        }
    }
}

// ── Notification helpers ───────────────────────────────────────────────────────

/// Extract `sessionId` from JSON-RPC params, returning `default_value` if absent.
pub(crate) fn extract_session_id<'a>(params: &'a Value, default_value: &'a str) -> &'a str {
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or(default_value)
}

/// Build the current set of config options and push a `ConfigOptionUpdate` notification.
pub(crate) async fn send_config_option_update(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    sessions: &HashMap<String, SessionState>,
    cfg: &AcpServerConfig,
) {
    if session_id.is_empty() {
        return;
    }
    let update = {
        let p = sessions
            .get(session_id)
            .map(|session| session.provider.read().clone())
            .unwrap_or_else(|| cfg.provider.read().clone());
        SessionUpdate::ConfigOptionUpdate(config_update::make_config_option_update(&p))
    };
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize ConfigOptionUpdate");
            return;
        }
    };
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}

/// Push an `AvailableCommandsUpdate` notification for the given session.
pub(crate) async fn send_available_commands_update(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    skills: &[SkillMetadata],
    caps: &PeriCaps,
) {
    if session_id.is_empty() {
        return;
    }
    let commands = crate::dispatch::build_available_commands(skills);
    let update = if caps.skill_names {
        let meta = skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        SessionUpdate::AvailableCommandsUpdate(
            AvailableCommandsUpdate::new(commands).meta(
                serde_json::json!({"skillNames": meta})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
    } else {
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(commands))
    };
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize AvailableCommandsUpdate");
            return;
        }
    };
    // Use {"update": ..., "sessionId": ...} format — same as TransportEventSink —
    // so that handle_session_update_peri on the TUI side can parse via params.get("update").
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}

/// 在 `session/new` 响应写出后发送首个 AvailableCommandsUpdate。
///
/// session/new 的 sessionId 只有写入响应后客户端才可用于路由通知；调用方
/// 必须先发送响应，再调用本辅助函数，避免 MpscTransport 客户端先收到无法归属
/// 的命令列表通知。
pub(crate) async fn send_new_session_commands(
    transport: &dyn crate::transport::AcpTransport,
    cfg: &AcpServerConfig,
    sessions: &HashMap<String, SessionState>,
    session_id: &str,
) {
    let Some(session) = sessions.get(session_id) else {
        return;
    };
    let plugin_skill_roots = cfg.plugin_skill_roots.read().clone();
    let skills = cfg
        .skills
        .available_skills(&session.cwd, &plugin_skill_roots);
    let caps = cfg.session_manager.get_caps(session_id);
    send_available_commands_update(transport, session_id, &skills, &caps).await;
}

/// Push a `SessionInfoUpdate` notification after prompt/compact completes,
/// or after a session rename.
pub(crate) async fn send_session_info_update(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
) {
    send_session_info_update_with_title(transport, session_id, None).await;
}

/// Push a `SessionInfoUpdate` notification with an optional title override.
/// Called from the `session/rename` handler and the prediction SetTitle flow.
pub(crate) async fn send_session_info_update_with_title(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    title: Option<&str>,
) {
    use agent_client_protocol::schema::v1::SessionInfoUpdate;
    let mut info = SessionInfoUpdate::new().updated_at(chrono::Utc::now().to_rfc3339());
    if let Some(t) = title {
        info = info.title(t.to_string());
    }
    let update = SessionUpdate::SessionInfoUpdate(info);
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize SessionInfoUpdate");
            return;
        }
    };
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}
// test
