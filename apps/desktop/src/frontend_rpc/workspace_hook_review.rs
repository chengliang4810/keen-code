//! V4 workspace Hook 审核 authority。
//!
//! `HooksService` 只负责读取配置和维护 trust store；本模块负责 active session 的
//! review flow、连接绑定、CAS 和可恢复的 pending claim。command receipt 由外层
//! SessionGateway 写入 Runtime Session Journal，避免本模块与 Runtime 形成第二事实源。

use super::services;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tauri::AppHandle;

const JOURNAL_SCHEMA_VERSION: u64 = 1;
const REVIEW_DEADLINE_MS: u64 = 5 * 60 * 1000;
const MAX_PENDING_REVIEWS: usize = 128;
const MAX_REVIEW_ITEMS: usize = 4096;

/// 由 session gateway 传入的 request 上下文；workspace path 必须已经通过宿主授权。
#[derive(Clone, Debug)]
pub(crate) struct RequestContext {
    pub(crate) connection_id: String,
    pub(crate) command_id: String,
    pub(crate) session_id: String,
    pub(crate) task_id: String,
    pub(crate) run_id: String,
    pub(crate) remote_session_id: Option<String>,
    pub(crate) workspace_path: PathBuf,
    pub(crate) workspace_identity: String,
    pub(crate) workspace_label: String,
    pub(crate) bundle_digest: String,
    pub(crate) now_ms: u64,
}

/// respond command 的 immutable target。所有字段都必须来自同一条 pending payload。
#[derive(Clone, Debug)]
pub(crate) struct RespondContext {
    pub(crate) connection_id: String,
    pub(crate) command_id: String,
    pub(crate) session_id: String,
    pub(crate) task_id: String,
    pub(crate) run_id: String,
    pub(crate) remote_session_id: Option<String>,
    pub(crate) workspace_path: PathBuf,
    pub(crate) workspace_identity: String,
    pub(crate) bundle_digest: String,
    pub(crate) review_flow_id: String,
    pub(crate) generation: u64,
    pub(crate) interaction_id: String,
    pub(crate) review_item_ids: Vec<String>,
    pub(crate) now_ms: u64,
}

/// revoke command 既支持现有 flow item，也支持 Settings 使用的 non-flow digest target。
#[derive(Clone, Debug)]
pub(crate) struct RevokeContext {
    pub(crate) connection_id: String,
    pub(crate) command_id: String,
    pub(crate) session_id: String,
    pub(crate) task_id: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) workspace_path: PathBuf,
    pub(crate) workspace_identity: String,
    pub(crate) bundle_digest: String,
    pub(crate) hook_declaration_digests: Vec<String>,
    pub(crate) review_flow_id: Option<String>,
    pub(crate) generation: Option<u64>,
    pub(crate) interaction_id: Option<String>,
    pub(crate) review_item_ids: Vec<String>,
    pub(crate) now_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingReview {
    payload: Value,
    #[serde(default)]
    connection_id: Option<String>,
}

/// 仅保存 Hook trust review 的 immutable payload 和当前进程连接 claim。
///
/// 这不是 Session Journal 的替代品：命令幂等、ACK 和副作用不确定性由父
/// Runtime Session Journal 保存。Hook pending 不能直接塞入现有 SessionEvent，
/// 因为它是 workspace trust snapshot 上的可过期安全 claim，不是 transcript、Turn
/// 或工具事实；复用现有 Elicitation/Permission coordinator 也会丢失该 payload
/// 的冷恢复和 workspace CAS 约束。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewJournal {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    #[serde(rename = "nextGeneration")]
    next_generation: BTreeMap<String, u64>,
    pending: BTreeMap<String, PendingReview>,
}

impl Default for ReviewJournal {
    fn default() -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            next_generation: BTreeMap::new(),
            pending: BTreeMap::new(),
        }
    }
}

/// 每个桌面 app 只有一个 coordinator；进程重启时从同一路径恢复 pending claim。
/// command receipt 由 SessionGateway 写入 Runtime Session Journal，本文件不复制事实。
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceHookReviewCoordinator {
    app: AppHandle,
    journal_path: PathBuf,
    state: Arc<Mutex<ReviewJournal>>,
}

static COORDINATORS: OnceLock<Mutex<HashMap<PathBuf, Arc<WorkspaceHookReviewCoordinator>>>> =
    OnceLock::new();

impl WorkspaceHookReviewCoordinator {
    pub(crate) fn new(app: &AppHandle) -> Result<Self, String> {
        let journal_path = review_journal_path(app)?;
        let journal = load_journal(&journal_path)?;
        Ok(Self {
            app: app.clone(),
            journal_path,
            state: Arc::new(Mutex::new(journal)),
        })
    }

    /// session gateway 使用的 app-scoped authority；不依赖临时 renderer 状态。
    pub(crate) fn for_app(app: &AppHandle) -> Result<Arc<Self>, String> {
        let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
        let coordinators = COORDINATORS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut coordinators = coordinators
            .lock()
            .map_err(|_| "workspace Hook review coordinator 锁已损坏".to_owned())?;
        if let Some(existing) = coordinators.get(&root) {
            return Ok(existing.clone());
        }
        let coordinator = Arc::new(Self::new(app)?);
        coordinators.insert(root, coordinator.clone());
        Ok(coordinator)
    }

    /// 创建或重新绑定当前 bundle 的 pending flow；命令收据由外层 Session gateway 裁决。
    pub(crate) fn request(&self, context: RequestContext) -> Result<Value, String> {
        validate_request_context(&context)?;
        let expected_identity = crate::path_utils::path_to_frontend(&context.workspace_path);
        if context.workspace_identity != expected_identity {
            return Ok(snapshot_mismatch("workspaceIdentity 与授权路径不一致"));
        }
        let Some((snapshot_value, snapshot)) =
            services::workspace_hook_review_snapshot(&self.app, &context.workspace_path)?
        else {
            return Ok(snapshot_mismatch("当前 workspace 没有可审核 Hook"));
        };
        if snapshot.trust_store_corrupt {
            return Ok(json!({
                "type": "requestWorkspaceHookReview",
                "accepted": false,
                "reasonCode": "workspace_hooks_trust_store_corrupt",
            }));
        }
        if context.bundle_digest != snapshot.bundle_digest {
            return Ok(json!({
                "type": "requestWorkspaceHookReview",
                "accepted": false,
                "reasonCode": "workspace_hooks_bundle_changed",
                "currentBundleDigest": snapshot.bundle_digest,
            }));
        }
        let flow_key = flow_key(&context.session_id, &context.workspace_identity);
        let mut state = self.lock_state()?;
        prune_expired(&mut state, context.now_ms);
        let existing_flow_id = state
            .pending
            .iter()
            .find(|(_, pending)| {
                pending.payload.get("sessionId").and_then(Value::as_str)
                    == Some(context.session_id.as_str())
                    && pending
                        .payload
                        .get("workspaceIdentity")
                        .and_then(Value::as_str)
                        == Some(context.workspace_identity.as_str())
            })
            .map(|(flow_id, _)| flow_id.clone());
        if let Some(flow_id) = existing_flow_id {
            let same_bundle = state
                .pending
                .get(&flow_id)
                .and_then(|pending| pending.payload.get("bundleDigest"))
                .and_then(Value::as_str)
                == Some(context.bundle_digest.as_str());
            if !same_bundle {
                state.pending.remove(&flow_id);
            } else {
                let previous = state.clone();
                let Some(pending) = state.pending.get_mut(&flow_id) else {
                    return Err("workspace Hook review flow 在请求期间消失".to_owned());
                };
                pending.connection_id = Some(context.connection_id.clone());
                let result = request_result(pending.payload.clone(), true, "duplicate");
                if let Err(error) = self.persist_locked(&state) {
                    *state = previous;
                    return Err(error);
                }
                return Ok(result);
            }
        }

        if state.pending.len() >= MAX_PENDING_REVIEWS {
            return Err("workspace Hook review pending 数量超过限制".to_owned());
        }
        let generation = next_generation(&mut state, &flow_key)?;
        let review_flow_id = format!("workspace-hook-review:{}:{generation}", context.session_id);
        let interaction_id = format!("workspace-hook-review-interaction:{review_flow_id}");
        let deadline_at = context
            .now_ms
            .checked_add(REVIEW_DEADLINE_MS)
            .ok_or_else(|| "workspace Hook review deadline 溢出".to_owned())?;
        let payload = build_review_payload(
            &context,
            &snapshot_value,
            &snapshot.trusted_declaration_digests,
            review_flow_id.as_str(),
            generation,
            interaction_id.as_str(),
            deadline_at,
        )?;
        let pending = PendingReview {
            payload: payload.clone(),
            connection_id: Some(context.connection_id),
        };
        let previous = state.clone();
        state.pending.insert(review_flow_id.clone(), pending);
        let result = request_result(payload, false, "accepted");
        if let Err(error) = self.persist_locked(&state) {
            *state = previous;
            return Err(error);
        }
        Ok(result)
    }

    /// 只有 request 所属连接或冷恢复后重新 claim 的同一 session 才能完成授权。
    pub(crate) fn respond(&self, context: RespondContext) -> Result<Value, String> {
        validate_respond_context(&context)?;
        let mut state = self.lock_state()?;
        prune_expired(&mut state, context.now_ms);
        let Some(pending) = state.pending.get(&context.review_flow_id) else {
            return Ok(review_superseded(
                "找不到 active workspace Hook review flow",
            ));
        };
        if !target_matches(&pending.payload, &context) {
            return Ok(snapshot_mismatch(
                "workspace Hook review target 与 immutable payload 不一致",
            ));
        }
        if !connection_is_bound(
            pending.connection_id.as_deref(),
            context.connection_id.as_str(),
        ) {
            return Ok(json!({
                "type": "respondWorkspaceHookReview",
                "accepted": false,
                "reasonCode": "workspace_hooks_review_connection_mismatch",
            }));
        }
        let deadline = pending
            .payload
            .get("deadlineAt")
            .and_then(Value::as_u64)
            .ok_or_else(|| "workspace Hook review deadline 缺失".to_owned())?;
        if context.now_ms > deadline {
            let result = timeout_result(&context.review_flow_id);
            let previous = state.clone();
            state.pending.remove(&context.review_flow_id);
            if let Err(error) = self.persist_locked(&state) {
                *state = previous;
                return Err(error);
            }
            return Ok(result);
        }

        let selected = selected_hook_digests(&pending.payload, &context.review_item_ids)?;
        let result = services::grant_workspace_hook_trust_batch(
            &self.app,
            &context.workspace_path,
            &context.bundle_digest,
            &selected,
        )?;
        if result.get("accepted").and_then(Value::as_bool) != Some(true) {
            let reason = result
                .get("reasonCode")
                .and_then(Value::as_str)
                .unwrap_or("workspace_hooks_snapshot_mismatch");
            let terminal = json!({
                "type": "respondWorkspaceHookReview",
                "accepted": false,
                "reasonCode": reason,
                "reviewFlowId": context.review_flow_id,
            });
            let previous = state.clone();
            state.pending.remove(&context.review_flow_id);
            if let Err(error) = self.persist_locked(&state) {
                *state = previous;
                return Err(error);
            }
            return Ok(terminal);
        }

        let terminal = json!({
            "type": "respondWorkspaceHookReview",
            "accepted": true,
            "changed": true,
            "reviewFlowId": context.review_flow_id,
            "generation": context.generation,
            "interactionId": context.interaction_id,
            "hookDeclarationDigests": selected,
        });
        let previous = state.clone();
        state.pending.remove(&context.review_flow_id);
        if let Err(error) = self.persist_locked(&state) {
            *state = previous;
            return Err(error);
        }
        Ok(terminal)
    }

    /// 完整 revoke target 的 authority 校验与批量撤销；不从 renderer snapshot 推断 digest。
    pub(crate) fn revoke(&self, context: RevokeContext) -> Result<Value, String> {
        validate_revoke_context(&context)?;
        let mut state = self.lock_state()?;
        prune_expired(&mut state, context.now_ms);
        let mut digests = context.hook_declaration_digests.clone();
        if let Some(flow_id) = context.review_flow_id.as_deref() {
            let Some(pending) = state.pending.get(flow_id) else {
                return Ok(review_superseded("revoke target 的 review flow 已终结"));
            };
            if !connection_is_bound(
                pending.connection_id.as_deref(),
                context.connection_id.as_str(),
            ) {
                return Ok(json!({
                    "type": "revokeWorkspaceHookTrust",
                    "accepted": false,
                    "reasonCode": "workspace_hooks_review_connection_mismatch",
                }));
            }
            if !revoke_target_matches(&pending.payload, &context) {
                return Ok(snapshot_mismatch(
                    "revoke target 与 immutable payload 不一致",
                ));
            }
            let selected = selected_hook_digests(&pending.payload, &context.review_item_ids)?;
            digests.extend(selected);
        }
        digests.sort();
        digests.dedup();
        let result = services::revoke_workspace_hook_trust_declarations(
            &self.app,
            &context.workspace_path,
            &context.bundle_digest,
            &digests,
        )?;
        if result.get("accepted").and_then(Value::as_bool) != Some(true) {
            let terminal = json!({
                "type": "revokeWorkspaceHookTrust",
                "accepted": false,
                "reasonCode": result
                    .get("reasonCode")
                    .cloned()
                    .unwrap_or_else(|| Value::String("workspace_hooks_snapshot_mismatch".to_owned())),
            });
            return Ok(terminal);
        }
        let previous = state.clone();
        if let Some(flow_id) = context.review_flow_id.as_deref() {
            state.pending.remove(flow_id);
        }
        let terminal = json!({
            "type": "revokeWorkspaceHookTrust",
            "accepted": true,
            "changed": result.get("changed").and_then(Value::as_bool).unwrap_or(false),
            "hookDeclarationDigests": digests,
        });
        if let Err(error) = self.persist_locked(&state) {
            *state = previous;
            return Err(error);
        }
        Ok(terminal)
    }

    /// 连接关闭释放 in-memory claim；Journal 保留 flow，重连后 request 可重新 claim。
    pub(crate) fn connection_closed(&self, connection_id: &str) -> Result<(), String> {
        if connection_id.is_empty() {
            return Ok(());
        }
        let mut state = self.lock_state()?;
        let previous = state.clone();
        for pending in state.pending.values_mut() {
            if pending.connection_id.as_deref() == Some(connection_id) {
                pending.connection_id = None;
            }
        }
        if let Err(error) = self.persist_locked(&state) {
            *state = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Projection 恢复入口；只返回指定 session 的 immutable payload，不暴露其他 session。
    pub(crate) fn pending_for_session(
        &self,
        session_id: &str,
        connection_id: Option<&str>,
        now_ms: u64,
    ) -> Result<Vec<Value>, String> {
        let mut state = self.lock_state()?;
        prune_expired(&mut state, now_ms);
        Ok(state
            .pending
            .values()
            .filter(|pending| {
                pending.payload.get("sessionId").and_then(Value::as_str) == Some(session_id)
                    && connection_id.is_some_and(|connection| {
                        pending.connection_id.as_deref() == Some(connection)
                    })
            })
            .map(|pending| pending.payload.clone())
            .collect())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ReviewJournal>, String> {
        self.state
            .lock()
            .map_err(|_| "workspace Hook review Journal 锁已损坏".to_owned())
    }

    fn persist_locked(&self, state: &ReviewJournal) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("workspace Hook review Journal 编码失败：{error}"))?;
        crate::storage::atomic_write_private(&self.journal_path, &bytes)
            .map_err(|error| format!("保存 workspace Hook review Journal 失败：{error}"))
    }
}

/// 将当前 workspace 的 Hook trust authority 投影成 V4 snapshot 的 soft gate。
///
/// 这里复用 `services::workspace_hook_trust_snapshot` 的 bundle/declaration digest
/// 与 trust-store 解析结果，避免 projection 层建立第二份 Hook 状态。没有配置或
/// 没有待授信声明时返回 null；trust-store 损坏时仍暴露真实的待授信数量，随后
/// review command 会以明确的 `workspace_hooks_trust_store_corrupt` 拒绝授权。
pub(crate) fn workspace_hook_admission(app: &AppHandle, workspace: &Path) -> Result<Value, String> {
    let Some(snapshot) = services::workspace_hook_trust_snapshot(app, workspace)? else {
        return Ok(Value::Null);
    };
    Ok(workspace_hook_admission_value(workspace, &snapshot))
}

fn workspace_hook_admission_value(
    workspace: &Path,
    snapshot: &services::WorkspaceHookTrustSnapshot,
) -> Value {
    let pending_count = snapshot
        .declaration_digests
        .difference(&snapshot.trusted_declaration_digests)
        .count();
    if pending_count == 0 {
        return Value::Null;
    }
    json!({
        "pendingCount": pending_count,
        "bundleDigest": snapshot.bundle_digest,
        "workspaceIdentity": crate::path_utils::path_to_frontend(workspace),
    })
}

fn review_journal_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::storage::root_dir(app)
        .map_err(|error| error.to_string())?
        .join("security")
        .join("workspace-hook-review-v1.json"))
}

fn load_journal(path: &Path) -> Result<ReviewJournal, String> {
    let Some(bytes) = crate::storage::read_private_bytes_bounded(
        path,
        16 * 1024 * 1024,
        "workspace Hook review Journal",
    )
    .map_err(|error| error.to_string())?
    else {
        return Ok(ReviewJournal::default());
    };
    let mut journal: ReviewJournal = serde_json::from_slice(&bytes)
        .map_err(|error| format!("workspace Hook review Journal 格式无效：{error}"))?;
    if journal.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err("workspace Hook review Journal 版本不受支持".to_owned());
    }
    if journal.pending.len() > MAX_PENDING_REVIEWS {
        return Err("workspace Hook review Journal 超过数量限制".to_owned());
    }
    for pending in journal.pending.values() {
        validate_payload_shape(&pending.payload)?;
    }
    // 连接 claim 只在当前进程内有效；冷恢复必须重新经过 request，避免复用旧
    // connectionId 后让新窗口直接看到或回答上一进程的 pending flow。
    for pending in journal.pending.values_mut() {
        pending.connection_id = None;
    }
    Ok(journal)
}

fn validate_request_context(context: &RequestContext) -> Result<(), String> {
    for (name, value) in [
        ("connectionId", context.connection_id.as_str()),
        ("commandId", context.command_id.as_str()),
        ("sessionId", context.session_id.as_str()),
        ("taskId", context.task_id.as_str()),
        ("runId", context.run_id.as_str()),
        ("workspaceIdentity", context.workspace_identity.as_str()),
        ("workspaceLabel", context.workspace_label.as_str()),
        ("bundleDigest", context.bundle_digest.as_str()),
    ] {
        validate_nonempty(name, value)?;
    }
    validate_digest(&context.bundle_digest, "bundleDigest")
}

fn validate_respond_context(context: &RespondContext) -> Result<(), String> {
    for (name, value) in [
        ("connectionId", context.connection_id.as_str()),
        ("commandId", context.command_id.as_str()),
        ("sessionId", context.session_id.as_str()),
        ("taskId", context.task_id.as_str()),
        ("runId", context.run_id.as_str()),
        ("workspaceIdentity", context.workspace_identity.as_str()),
        ("bundleDigest", context.bundle_digest.as_str()),
        ("reviewFlowId", context.review_flow_id.as_str()),
        ("interactionId", context.interaction_id.as_str()),
    ] {
        validate_nonempty(name, value)?;
    }
    validate_digest(&context.bundle_digest, "bundleDigest")?;
    if context.generation == 0 || context.review_item_ids.is_empty() {
        return Err("workspace Hook respond target 缺少 generation 或 reviewItemIds".to_owned());
    }
    validate_unique_nonempty(&context.review_item_ids, "reviewItemIds")
}

fn validate_revoke_context(context: &RevokeContext) -> Result<(), String> {
    for (name, value) in [
        ("connectionId", context.connection_id.as_str()),
        ("commandId", context.command_id.as_str()),
        ("sessionId", context.session_id.as_str()),
        ("workspaceIdentity", context.workspace_identity.as_str()),
        ("bundleDigest", context.bundle_digest.as_str()),
    ] {
        validate_nonempty(name, value)?;
    }
    validate_digest(&context.bundle_digest, "bundleDigest")?;
    if context.review_flow_id.is_none() && context.hook_declaration_digests.is_empty() {
        return Err("workspace Hook revoke 缺少 declarations 或 review flow".to_owned());
    }
    if context.review_flow_id.is_some() && !context.hook_declaration_digests.is_empty() {
        return Err(
            "workspace Hook revoke flow target 不能同时携带 declaration digests".to_owned(),
        );
    }
    if context.review_flow_id.is_some()
        && (context.task_id.as_deref().is_none_or(str::is_empty)
            || context.run_id.as_deref().is_none_or(str::is_empty))
    {
        return Err("workspace Hook revoke flow target 缺少 taskId 或 runId".to_owned());
    }
    validate_unique_nonempty(&context.hook_declaration_digests, "hookDeclarationDigests")?;
    validate_unique_nonempty(&context.review_item_ids, "reviewItemIds")
}

fn validate_nonempty(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(format!(
            "workspace Hook {name} 必须是无控制字符的非空字符串"
        ));
    }
    Ok(())
}

fn validate_digest(value: &str, name: &str) -> Result<(), String> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(format!(
            "workspace Hook {name} 必须是 64 位小写 SHA-256 摘要"
        ));
    }
    Ok(())
}

fn validate_unique_nonempty(values: &[String], name: &str) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for value in values {
        validate_nonempty(name, value)?;
        if !unique.insert(value) {
            return Err(format!("workspace Hook {name} 不允许重复值"));
        }
    }
    Ok(())
}

fn flow_key(session_id: &str, workspace_identity: &str) -> String {
    format!("{session_id}\0{workspace_identity}")
}

fn next_generation(state: &mut ReviewJournal, key: &str) -> Result<u64, String> {
    let next = state.next_generation.entry(key.to_owned()).or_insert(0);
    *next = next
        .checked_add(1)
        .ok_or_else(|| "workspace Hook review generation 溢出".to_owned())?;
    Ok(*next)
}

fn prune_expired(state: &mut ReviewJournal, now_ms: u64) {
    state.pending.retain(|_, pending| {
        pending
            .payload
            .get("deadlineAt")
            .and_then(Value::as_u64)
            .is_none_or(|deadline| deadline >= now_ms)
    });
}

fn request_result(payload: Value, duplicate: bool, status: &str) -> Value {
    json!({
        "type": "requestWorkspaceHookReview",
        "accepted": true,
        "changed": !duplicate,
        "status": status,
        "reviewFlowId": payload.get("reviewFlowId").cloned().unwrap_or(Value::Null),
        "generation": payload.get("generation").cloned().unwrap_or(Value::Null),
        "interactionId": payload.get("interactionId").cloned().unwrap_or(Value::Null),
        "payload": payload,
    })
}

fn snapshot_mismatch(message: &str) -> Value {
    json!({
        "type": "requestWorkspaceHookReview",
        "accepted": false,
        "reasonCode": "workspace_hooks_snapshot_mismatch",
        "message": message,
    })
}

fn review_superseded(message: &str) -> Value {
    json!({
        "type": "respondWorkspaceHookReview",
        "accepted": false,
        "reasonCode": "workspace_hooks_review_superseded",
        "message": message,
    })
}

fn timeout_result(flow_id: &str) -> Value {
    json!({
        "type": "respondWorkspaceHookReview",
        "accepted": false,
        "reasonCode": "workspace_hooks_interaction_timeout",
        "reviewFlowId": flow_id,
    })
}

fn target_matches(payload: &Value, context: &RespondContext) -> bool {
    payload.get("sessionId").and_then(Value::as_str) == Some(context.session_id.as_str())
        && payload.get("taskId").and_then(Value::as_str) == Some(context.task_id.as_str())
        && payload.get("runId").and_then(Value::as_str) == Some(context.run_id.as_str())
        && payload.get("remoteSessionId").and_then(Value::as_str)
            == context.remote_session_id.as_deref()
        && payload.get("workspaceIdentity").and_then(Value::as_str)
            == Some(context.workspace_identity.as_str())
        && payload.get("bundleDigest").and_then(Value::as_str)
            == Some(context.bundle_digest.as_str())
        && payload.get("reviewFlowId").and_then(Value::as_str)
            == Some(context.review_flow_id.as_str())
        && payload.get("generation").and_then(Value::as_u64) == Some(context.generation)
        && payload.get("interactionId").and_then(Value::as_str)
            == Some(context.interaction_id.as_str())
}

fn connection_is_bound(bound: Option<&str>, requested: &str) -> bool {
    bound == Some(requested)
}

fn revoke_target_matches(payload: &Value, context: &RevokeContext) -> bool {
    payload.get("sessionId").and_then(Value::as_str) == Some(context.session_id.as_str())
        && context
            .task_id
            .as_deref()
            .is_none_or(|task_id| payload.get("taskId").and_then(Value::as_str) == Some(task_id))
        && context
            .run_id
            .as_deref()
            .is_none_or(|run_id| payload.get("runId").and_then(Value::as_str) == Some(run_id))
        && payload.get("workspaceIdentity").and_then(Value::as_str)
            == Some(context.workspace_identity.as_str())
        && payload.get("bundleDigest").and_then(Value::as_str)
            == Some(context.bundle_digest.as_str())
        && context.generation.is_none_or(|generation| {
            payload.get("generation").and_then(Value::as_u64) == Some(generation)
        })
        && context.interaction_id.as_deref().is_none_or(|interaction| {
            payload.get("interactionId").and_then(Value::as_str) == Some(interaction)
        })
}

fn selected_hook_digests(payload: &Value, item_ids: &[String]) -> Result<Vec<String>, String> {
    let items = payload
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "workspace Hook review items 缺失".to_owned())?;
    let mut selected = Vec::with_capacity(item_ids.len());
    for item_id in item_ids {
        let item = items
            .iter()
            .find(|item| item.get("reviewItemId").and_then(Value::as_str) == Some(item_id))
            .ok_or_else(|| "workspace Hook reviewItemId 不属于当前 flow".to_owned())?;
        let trust_state = item
            .get("trustState")
            .and_then(Value::as_str)
            .ok_or_else(|| "workspace Hook review item trustState 缺失".to_owned())?;
        if !matches!(trust_state, "pending_trust" | "revoked" | "stale_digest") {
            return Err("workspace Hook review item 当前不可授信".to_owned());
        }
        let digest = item_id
            .strip_prefix("workspace-hook:")
            .ok_or_else(|| "workspace Hook reviewItemId 格式无效".to_owned())?;
        validate_digest(digest, "hookDeclarationDigest")?;
        selected.push(digest.to_owned());
    }
    Ok(selected)
}

fn build_review_payload(
    context: &RequestContext,
    snapshot: &Value,
    trusted_declarations: &BTreeSet<String>,
    review_flow_id: &str,
    generation: u64,
    interaction_id: &str,
    deadline_at: u64,
) -> Result<Value, String> {
    let source_files = snapshot
        .get("sourceFiles")
        .and_then(Value::as_array)
        .ok_or_else(|| "workspace Hook snapshot sourceFiles 缺失".to_owned())?;
    let source_paths = source_files
        .iter()
        .map(|source| {
            source
                .get("canonicalPath")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| "workspace Hook source file path 缺失".to_owned())
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let source_values = source_files
        .iter()
        .map(|source| {
            let path = source
                .get("canonicalPath")
                .and_then(Value::as_str)
                .ok_or_else(|| "workspace Hook source file path 缺失".to_owned())?;
            Ok(json!({
                "path": path,
                "displayPath": path,
                "editable": source.get("editable").and_then(Value::as_bool).unwrap_or(false),
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let hooks = snapshot
        .get("hooks")
        .and_then(Value::as_array)
        .ok_or_else(|| "workspace Hook snapshot hooks 缺失".to_owned())?;
    if hooks.is_empty() || hooks.len() > MAX_REVIEW_ITEMS {
        return Err("workspace Hook review items 数量无效".to_owned());
    }
    let mut events = BTreeSet::new();
    let mut pending_count = 0usize;
    let mut items = Vec::with_capacity(hooks.len());
    for hook in hooks {
        let event = hook
            .get("event")
            .and_then(Value::as_str)
            .ok_or_else(|| "workspace Hook event 缺失".to_owned())?;
        let review_item_id = hook
            .get("reviewItemId")
            .and_then(Value::as_str)
            .ok_or_else(|| "workspace Hook reviewItemId 缺失".to_owned())?;
        let declaration_digest = review_item_id
            .strip_prefix("workspace-hook:")
            .ok_or_else(|| "workspace Hook reviewItemId 格式无效".to_owned())?;
        validate_digest(declaration_digest, "hookDeclarationDigest")?;
        let source_file_index = hook
            .get("sourceFileIndex")
            .and_then(Value::as_u64)
            .ok_or_else(|| "workspace Hook sourceFileIndex 缺失".to_owned())?
            as usize;
        let source_path = hook
            .get("sourceRelativePath")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .or_else(|| source_paths.get(source_file_index).cloned())
            .ok_or_else(|| "workspace Hook sourcePath 缺失".to_owned())?;
        let hook_type = hook
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("command");
        if !matches!(hook_type, "command" | "process") {
            return Err("workspace Hook type 不受支持".to_owned());
        }
        let command = hook
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| "workspace Hook command 缺失".to_owned())?;
        let trust_state = if trusted_declarations.contains(declaration_digest) {
            "trusted_persistent"
        } else {
            pending_count += 1;
            "pending_trust"
        };
        events.insert(event);
        let mut item = json!({
            "reviewItemId": review_item_id,
            "event": event,
            "type": hook_type,
            "displayName": format!("{event} {source_path}"),
            "displayCommand": command,
            "sourcePath": source_path,
            "resolvedTimeoutMs": hook.get("resolvedTimeoutMs").and_then(Value::as_u64).filter(|value| *value > 0).unwrap_or(1),
            "resolvedMaxOutputBytes": hook.get("resolvedMaxOutputBytes").and_then(Value::as_u64).filter(|value| *value > 0).unwrap_or(1),
            "executionMode": if hook.get("async").and_then(Value::as_bool).unwrap_or(false) { "background" } else { "foreground" },
            "configuredEnabled": hook.get("configuredEnabled").and_then(Value::as_bool).unwrap_or(false),
            "editable": hook.get("editable").and_then(Value::as_bool).unwrap_or(false),
            "trustState": trust_state,
        });
        if let Some(matcher) = hook.get("matcher").and_then(Value::as_str) {
            item["matcher"] = Value::String(matcher.to_owned());
        }
        items.push(item);
    }
    let mut payload = json!({
        "kind": "workspaceHookReview",
        "reviewFlowId": review_flow_id,
        "generation": generation,
        "interactionId": interaction_id,
        "sessionId": context.session_id,
        "taskId": context.task_id,
        "runId": context.run_id,
        "workspaceIdentity": context.workspace_identity,
        "workspaceLabel": context.workspace_label,
        "bundleDigest": context.bundle_digest,
        "createdAt": context.now_ms,
        "deadlineAt": deadline_at,
        "sourceFiles": source_values,
        "summary": {
            "eventCount": events.len(),
            "hookCount": items.len(),
            "pendingCount": pending_count,
        },
        "items": items,
        "warningCode": "workspace_hooks_execute_code",
    });
    if let Some(remote_session_id) = context.remote_session_id.as_deref() {
        payload["remoteSessionId"] = Value::String(remote_session_id.to_owned());
    }
    Ok(payload)
}

fn validate_payload_shape(payload: &Value) -> Result<(), String> {
    if payload.get("kind").and_then(Value::as_str) != Some("workspaceHookReview")
        || payload
            .get("reviewFlowId")
            .and_then(Value::as_str)
            .is_none()
        || payload.get("generation").and_then(Value::as_u64).is_none()
        || payload
            .get("interactionId")
            .and_then(Value::as_str)
            .is_none()
        || payload.get("sessionId").and_then(Value::as_str).is_none()
        || payload.get("taskId").and_then(Value::as_str).is_none()
        || payload.get("runId").and_then(Value::as_str).is_none()
        || payload
            .get("workspaceIdentity")
            .and_then(Value::as_str)
            .is_none()
        || payload
            .get("bundleDigest")
            .and_then(Value::as_str)
            .is_none()
        || payload.get("createdAt").and_then(Value::as_u64).is_none()
        || payload.get("deadlineAt").and_then(Value::as_u64).is_none()
        || payload.get("items").and_then(Value::as_array).is_none()
    {
        return Err("workspace Hook review Journal payload 不完整".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        RequestContext, RespondContext, build_review_payload, connection_is_bound,
        selected_hook_digests, target_matches, workspace_hook_admission_value,
    };
    use crate::frontend_rpc::services::WorkspaceHookTrustSnapshot;
    use serde_json::json;
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
    };

    fn digest(ch: char) -> String {
        std::iter::repeat_n(ch, 64).collect()
    }

    #[test]
    fn hook_admission_uses_authority_digests_and_clears_when_fully_trusted() {
        let mut declarations = BTreeSet::new();
        declarations.insert(digest('a'));
        declarations.insert(digest('b'));
        let mut trusted = BTreeSet::new();
        trusted.insert(digest('a'));
        let snapshot = WorkspaceHookTrustSnapshot {
            bundle_digest: digest('c'),
            declaration_digests: declarations.clone(),
            trusted_declaration_digests: trusted,
            trust_store_corrupt: false,
        };
        let admission = workspace_hook_admission_value(Path::new("D:/workspace"), &snapshot);
        assert_eq!(admission["pendingCount"], 1);
        assert_eq!(admission["bundleDigest"], digest('c'));
        assert_eq!(admission["workspaceIdentity"], "D:/workspace");

        let fully_trusted = WorkspaceHookTrustSnapshot {
            trusted_declaration_digests: declarations,
            ..snapshot
        };
        assert!(
            workspace_hook_admission_value(Path::new("D:/workspace"), &fully_trusted).is_null()
        );
    }

    fn context() -> RequestContext {
        RequestContext {
            connection_id: "conn-a".to_owned(),
            command_id: "cmd-a".to_owned(),
            session_id: "session-a".to_owned(),
            task_id: "task-a".to_owned(),
            run_id: "run-a".to_owned(),
            remote_session_id: None,
            workspace_path: PathBuf::from("D:/workspace"),
            workspace_identity: "D:/workspace".to_owned(),
            workspace_label: "workspace".to_owned(),
            bundle_digest: digest('a'),
            now_ms: 100,
        }
    }

    #[test]
    fn strict_payload_has_exact_pending_summary_and_safe_execution_mode() {
        let context = context();
        let declaration = digest('b');
        let payload = build_review_payload(
            &context,
            &json!({
                "sourceFiles": [{"canonicalPath":"D:/workspace/.agents/settings.json","editable":true}],
                "hooks": [{
                    "reviewItemId": format!("workspace-hook:{declaration}"),
                    "event":"PreToolUse",
                    "sourceFileIndex":0,
                    "sourceRelativePath":".agents/settings.json",
                    "type":"command",
                    "command":"echo fixture",
                    "async":false,
                    "resolvedTimeoutMs":1000,
                    "resolvedMaxOutputBytes":1024,
                    "configuredEnabled":true,
                    "editable":true
                }]
            }),
            &BTreeSet::new(),
            "flow-a",
            1,
            "interaction-a",
            1000,
        )
        .unwrap();
        assert_eq!(
            payload["summary"],
            json!({"eventCount":1,"hookCount":1,"pendingCount":1})
        );
        assert_eq!(payload["items"][0]["executionMode"], "foreground");
        assert_eq!(payload["items"][0]["trustState"], "pending_trust");
        assert!(payload.get("remoteSessionId").is_none());
        assert!(payload["items"][0].get("matcher").is_none());
    }

    #[test]
    fn selected_item_must_be_pending_and_exact_digest() {
        let declaration = digest('b');
        let payload = json!({
            "items": [{
                "reviewItemId": format!("workspace-hook:{declaration}"),
                "trustState":"pending_trust"
            }]
        });
        assert_eq!(
            selected_hook_digests(&payload, &[format!("workspace-hook:{declaration}")]).unwrap(),
            vec![declaration]
        );
        assert!(selected_hook_digests(&payload, &["workspace-hook:bad".to_owned()]).is_err());
        assert!(selected_hook_digests(&payload, &["workspace-hook:missing".to_owned()]).is_err());
    }

    #[test]
    fn response_target_rejects_cross_session_and_cross_connection_payloads() {
        let request = context();
        let target = RespondContext {
            connection_id: "conn-a".to_owned(),
            command_id: "cmd-b".to_owned(),
            session_id: request.session_id.clone(),
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            remote_session_id: None,
            workspace_path: request.workspace_path.clone(),
            workspace_identity: request.workspace_identity.clone(),
            bundle_digest: request.bundle_digest.clone(),
            review_flow_id: "flow-a".to_owned(),
            generation: 1,
            interaction_id: "interaction-a".to_owned(),
            review_item_ids: vec![format!("workspace-hook:{}", digest('b'))],
            now_ms: 200,
        };
        let payload = json!({
            "sessionId":"session-a","taskId":"task-a","runId":"run-a",
            "workspaceIdentity":"D:/workspace","bundleDigest":digest('a'),
            "reviewFlowId":"flow-a","generation":1,"interactionId":"interaction-a"
        });
        assert!(target_matches(&payload, &target));
        let mut cross_session = target.clone();
        cross_session.session_id = "session-b".to_owned();
        assert!(!target_matches(&payload, &cross_session));
        let mut cross_connection = target;
        cross_connection.connection_id = "conn-b".to_owned();
        assert!(target_matches(&payload, &cross_connection));
        assert!(!connection_is_bound(Some("conn-a"), "conn-b"));
        assert!(!connection_is_bound(None, "conn-a"));
    }
}
