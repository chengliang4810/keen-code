//! Native GPUI 设置页使用的真实资源域。
//!
//! 这个模块只接收宿主已经解析的 `NativePaths` 和 `AgentRuntime`。它复用现有的
//! PluginManager 和受控本地 JSON 文件，不通过
//! `AppHandle` 或旧前端 RPC 伪造成功状态。

use super::contracts::*;
use super::native_settings::{NativeSettingsDomain, NativeSettingsRuntimePort};
use crate::agent_runtime::AgentRuntime;
use crate::memories::MemoryService;
use crate::native_hooks::{
    self, HookDefinition as NativeHookDefinition, HookHandler as NativeHookHandler,
    HookScope as NativeHookScope,
};
use crate::native_paths::NativePaths;
use crate::plugin_secrets::SystemSecretStore;
use crate::plugins::{
    self, MaterializedPlugin, PluginCommandCatalog, PluginId, PluginInstallSource, PluginManager,
    UserConfigUpdate,
};
use crate::storage;
use keencode_acp::McpConnectionStatus;
use keencode_agent::{GoalController, GoalDraft, GoalPatch, GoalStatus, GoalTransition, HookPhase};
use keencode_resources::{ROOT_AGENT_ID, TurnStatus};
use keencode_runtime::PersistentAgentState;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SETTINGS_JSON_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MEMORY_BYTES: u64 = 800 * 1024;
const MAX_TEMPLATE_BYTES: u64 = 512 * 1024;
/// 资源设置域与 NativeHost 共用 Runtime，避免设置窗口创建第二个状态事实源。
pub struct NativeSettingsRuntimeDomain {
    paths: Arc<NativePaths>,
    runtime: Arc<AgentRuntime>,
    runtime_port: Arc<dyn NativeSettingsRuntimePort>,
    memory_service: Arc<MemoryService>,
    revisions: AtomicU64,
}

impl NativeSettingsRuntimeDomain {
    pub fn new(
        paths: Arc<NativePaths>,
        runtime: Arc<AgentRuntime>,
        runtime_port: Arc<dyn NativeSettingsRuntimePort>,
        memory_service: Arc<MemoryService>,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            runtime,
            runtime_port,
            memory_service,
            revisions: AtomicU64::new(0),
        })
    }

    /// NativeHost 启动完成后调用；端口内部负责 scheduler/WorkflowHost 的启动顺序。
    pub fn start(&self) -> SettingsResult<()> {
        self.runtime_port.start()
    }

    /// NativeHost 释放 Runtime 前调用；实现必须允许重复调用。
    pub fn shutdown(&self) {
        self.runtime_port.shutdown();
    }

    fn next_revision(&self) -> u64 {
        self.revisions
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }

    fn snapshot(&self, page: SettingsPage) -> SettingsSnapshot {
        SettingsSnapshot {
            page,
            revision: self.next_revision(),
            ..SettingsSnapshot::default()
        }
    }

    fn current_project(&self) -> SettingsResult<Option<PathBuf>> {
        let Some(session_id) = self
            .runtime
            .focused_session_id()
            .map_err(|error| domain_error("settings_focus", error))?
        else {
            return Ok(None);
        };
        let session = match self.runtime.runtime_manager().get(session_id) {
            Ok(session) => session,
            Err(_) => return Ok(None),
        };
        let project_root = session
            .read_state(|state| state.project_root.clone())
            .map_err(|error| domain_error("settings_session_snapshot", error))?;
        let project = project_root.trim();
        if project.is_empty() {
            Ok(None)
        } else {
            Ok(Some(PathBuf::from(project)))
        }
    }

    fn current_session(
        &self,
    ) -> SettingsResult<Option<(String, keencode_runtime::RuntimeSession)>> {
        let Some(session_id) = self
            .runtime
            .focused_session_id()
            .map_err(|error| domain_error("settings_focus", error))?
        else {
            return Ok(None);
        };
        let session = match self.runtime.runtime_manager().get(session_id.clone()) {
            Ok(session) => session,
            Err(_) => return Ok(None),
        };
        Ok(Some((session_id, session)))
    }

    /// 资源写入成功后只标记当前项目候选待重建；活动候选仍由 Runtime 保留到
    /// 当前 Turn 收口，下一次 Host 装配候选时才读取新的资源状态。
    fn invalidate_current_project_extension_candidate(&self) -> SettingsResult<()> {
        let Some(project) = self.current_project()? else {
            return Ok(());
        };
        self.runtime
            .invalidate_extension_candidate(&project)
            .map_err(|error| domain_error("settings_extension_invalidate", error))
    }

    /// 插件是全局资源；写入后失效所有已登记项目，避免没有 focused Session 时
    /// 仍继续使用安装、启用状态或 manifest 变化前的候选。已移除的项目目录没有
    /// 可用候选，跳过它们以免阻断本次已经成功提交的插件操作。
    fn invalidate_all_project_extension_candidates(&self) -> SettingsResult<()> {
        let mut projects = BTreeSet::new();
        let records = crate::workspace::project_records(&self.paths)
            .map_err(|error| domain_error("settings_projects_load", error))?;
        for record in records {
            let path = PathBuf::from(record.path);
            if let Ok(path) = fs::canonicalize(path) {
                projects.insert(path);
            }
        }
        if let Some(project) = self.current_project()?
            && let Ok(project) = fs::canonicalize(project)
        {
            projects.insert(project);
        }
        for project in projects {
            self.runtime
                .invalidate_extension_candidate(&project)
                .map_err(|error| domain_error("settings_extension_invalidate", error))?;
        }
        Ok(())
    }

    fn invalidate_extension_candidates_for_scope(&self, scope: &str) -> SettingsResult<()> {
        if scope == "global" || scope.starts_with("plugin:") {
            self.invalidate_all_project_extension_candidates()
        } else {
            self.invalidate_current_project_extension_candidate()
        }
    }

    fn load_hooks(&self) -> SettingsResult<SettingsSnapshot> {
        let project = self
            .current_project()?
            .map(|path| {
                fs::canonicalize(path).map_err(|error| {
                    domain_error(
                        "settings_hooks_project",
                        format!("规范化当前项目失败：{error}"),
                    )
                })
            })
            .transpose()?;
        let native = native_hooks::load(&self.paths, project.as_deref())
            .map_err(|error| domain_error("settings_hooks_load", error))?;
        let plugin_root = project.as_deref().unwrap_or(self.paths.data_root.as_path());
        let manager = PluginManager::new(self.paths.data_root.clone());
        let plugin_snapshot = manager
            .runtime_snapshot(plugin_root, &BTreeMap::new(), &SystemSecretStore)
            .map_err(|error| domain_error("settings_hooks_plugins", error))?;
        let bundle_digest = project.as_deref().map(|_| hook_bundle_digest(&native));
        let hooks = native
            .hooks
            .iter()
            .zip(native.summaries.iter())
            .enumerate()
            .map(|(index, (definition, summary))| {
                hook_summary(
                    definition,
                    summary,
                    project.as_deref(),
                    bundle_digest.as_deref(),
                    index,
                )
            })
            .collect::<SettingsResult<Vec<_>>>()?;
        let mut snapshot = self.snapshot(SettingsPage::Hooks);
        snapshot.hooks = Some(HooksSettings {
            hooks,
            plugin_hooks: plugin_hook_diagnostics(&plugin_snapshot),
            workspace: project.as_deref().map(|root| HookWorkspaceSnapshot {
                workspace_identity: root.to_string_lossy().into_owned(),
                bundle_digest: bundle_digest.clone().unwrap_or_default(),
                hook_count: native.hooks.len(),
                trust_store_corrupt: false,
            }),
            trust_store_corrupt: false,
        });
        Ok(snapshot)
    }

    fn hook_definition_from_config(
        &self,
        id: String,
        config: HookConfig,
    ) -> SettingsResult<NativeHookDefinition> {
        let project = self
            .current_project()?
            .map(|path| {
                fs::canonicalize(path).map_err(|error| {
                    domain_error(
                        "settings_hooks_project",
                        format!("规范化当前项目失败：{error}"),
                    )
                })
            })
            .transpose()?;
        let scope = match config.scope {
            HookScope::User => NativeHookScope::User,
            HookScope::Project => NativeHookScope::Project {
                root: project.ok_or_else(|| {
                    unsupported(
                        "settings_hooks_project_required",
                        "项目 Hook 需要当前焦点项目",
                    )
                })?,
            },
        };
        let phase = hook_phase(config.event)?;
        let handler = match config.hook_type {
            HookType::Command => NativeHookHandler::Command {
                command: config.command,
                args: (!config.args.is_empty()).then_some(config.args),
                shell: config.shell,
                timeout_ms: Some(config.timeout_ms),
                environment: BTreeMap::new(),
            },
            HookType::Context => NativeHookHandler::Context {
                context: config.context,
                block: config.block,
                input: config.input,
                continue_turn: config.continue_turn,
            },
        };
        Ok(NativeHookDefinition {
            id,
            name: format!("{} Hook", config.event.as_str()),
            scope,
            phase,
            matcher: config.matcher,
            handler,
            enabled: config.enabled,
        })
    }

    fn hook_scope_and_revision_for(
        &self,
        hook_id: &str,
        requested_scope: Option<HookScope>,
    ) -> SettingsResult<(NativeHookScope, u64)> {
        let project = self
            .current_project()?
            .map(|path| {
                fs::canonicalize(path).map_err(|error| {
                    domain_error(
                        "settings_hooks_project",
                        format!("规范化当前项目失败：{error}"),
                    )
                })
            })
            .transpose()?;
        let native = native_hooks::load(&self.paths, project.as_deref())
            .map_err(|error| domain_error("settings_hooks_load", error))?;
        let requested_scope = requested_scope
            .map(|scope| self.native_hook_scope(scope, project.as_deref()))
            .transpose()?;
        let definition = native
            .hooks
            .iter()
            .find(|hook| {
                hook.id == hook_id
                    && requested_scope
                        .as_ref()
                        .is_none_or(|scope| &hook.scope == scope)
            })
            .ok_or_else(|| unsupported("settings_hook_not_found", "找不到指定 Hook"))?;
        let revision = match &definition.scope {
            NativeHookScope::User => native.user_revision,
            NativeHookScope::Project { .. } => native.project_revision.unwrap_or_default(),
            NativeHookScope::Plugin { .. } => {
                return Err(unsupported(
                    "settings_hook_read_only",
                    "插件 Hook 不能通过设置页修改",
                ));
            }
        };
        Ok((definition.scope.clone(), revision))
    }

    fn native_hook_scope(
        &self,
        scope: HookScope,
        project: Option<&Path>,
    ) -> SettingsResult<NativeHookScope> {
        match scope {
            HookScope::User => Ok(NativeHookScope::User),
            HookScope::Project => Ok(NativeHookScope::Project {
                root: project
                    .ok_or_else(|| {
                        unsupported(
                            "settings_hooks_project_required",
                            "项目 Hook 需要当前焦点项目",
                        )
                    })?
                    .to_path_buf(),
            }),
        }
    }

    /// Hook ID 必须跨应用重启保持唯一；目标文件的现有 ID 再做一次确认，避免
    /// 冷恢复后序列归零导致 upsert 把旧定义当成新建项覆盖。
    fn new_hook_id(&self, scope: HookScope) -> SettingsResult<String> {
        let project = self
            .current_project()?
            .map(|path| {
                fs::canonicalize(path).map_err(|error| {
                    domain_error(
                        "settings_hooks_project",
                        format!("规范化当前项目失败：{error}"),
                    )
                })
            })
            .transpose()?;
        let native_scope = self.native_hook_scope(scope, project.as_deref())?;
        let native = native_hooks::load(
            &self.paths,
            match &native_scope {
                NativeHookScope::User => None,
                NativeHookScope::Project { root } => Some(root.as_path()),
                NativeHookScope::Plugin { .. } => None,
            },
        )
        .map_err(|error| domain_error("settings_hooks_load", error))?;
        let occupied = native
            .hooks
            .iter()
            .filter(|hook| hook.scope == native_scope)
            .map(|hook| hook.id.as_str())
            .collect::<BTreeSet<_>>();
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| domain_error("settings_hook_id", error))?
            .as_nanos();
        let process_id = std::process::id();
        for attempt in 0..1024_u32 {
            let id = format!(
                "hook-{timestamp_ns}-{process_id}-{}-{attempt}",
                self.next_revision()
            );
            if !occupied.contains(id.as_str()) {
                return Ok(id);
            }
        }
        Err(unsupported(
            "settings_hook_id_exhausted",
            "无法生成唯一 Hook ID",
        ))
    }

    fn execute_hook_command(&self, command: SettingsCommand) -> SettingsResult<SettingsSnapshot> {
        let trust_intent = match &command {
            SettingsCommand::GrantHookTrust { .. } => Some(true),
            SettingsCommand::RevokeHookTrust { .. } => Some(false),
            _ => None,
        };
        match command {
            SettingsCommand::CreateHook(config) => {
                let id = self.new_hook_id(config.scope)?;
                let definition = self.hook_definition_from_config(id, config)?;
                let expected_revision = match &definition.scope {
                    NativeHookScope::User => {
                        native_hooks::load(&self.paths, None)
                            .map_err(|error| domain_error("settings_hooks_load", error))?
                            .user_revision
                    }
                    NativeHookScope::Project { root } => {
                        native_hooks::load(&self.paths, Some(root))
                            .map_err(|error| domain_error("settings_hooks_load", error))?
                            .project_revision
                            .unwrap_or_default()
                    }
                    NativeHookScope::Plugin { .. } => 0,
                };
                native_hooks::upsert(&self.paths, definition, expected_revision)
                    .map_err(|error| domain_error("settings_hook_create", error))?;
            }
            SettingsCommand::UpdateHook {
                hook_id,
                scope: requested_scope,
                hook,
            } => {
                let (scope, expected_revision) =
                    self.hook_scope_and_revision_for(&hook_id, Some(requested_scope))?;
                let definition = self.hook_definition_from_config(hook_id.clone(), hook)?;
                if definition.scope != scope {
                    return Err(unsupported(
                        "settings_hook_scope_changed",
                        "编辑 Hook 时不能改变作用域",
                    ));
                }
                native_hooks::upsert(&self.paths, definition, expected_revision)
                    .map_err(|error| domain_error("settings_hook_update", error))?;
            }
            SettingsCommand::DeleteHook {
                hook_id,
                scope: requested_scope,
            } => {
                let (scope, expected_revision) =
                    self.hook_scope_and_revision_for(&hook_id, Some(requested_scope))?;
                native_hooks::delete(&self.paths, scope, &hook_id, expected_revision)
                    .map_err(|error| domain_error("settings_hook_delete", error))?;
            }
            SettingsCommand::SetHookEnabled {
                hook_id,
                scope: requested_scope,
                enabled,
            } => {
                let (scope, expected_revision) =
                    self.hook_scope_and_revision_for(&hook_id, Some(requested_scope))?;
                native_hooks::set_enabled(&self.paths, scope, &hook_id, enabled, expected_revision)
                    .map_err(|error| domain_error("settings_hook_enabled", error))?;
            }
            SettingsCommand::GrantHookTrust {
                workspace_identity,
                bundle_digest,
                hook_declaration_digest,
            }
            | SettingsCommand::RevokeHookTrust {
                workspace_identity,
                bundle_digest,
                hook_declaration_digest,
            } => {
                let trusted = trust_intent.expect("Hook Trust 命令必须携带准入意图");
                let project = self.current_project()?.ok_or_else(|| {
                    unsupported(
                        "settings_hooks_project_required",
                        "Hook Trust 需要当前焦点项目",
                    )
                })?;
                let project = fs::canonicalize(project).map_err(|error| {
                    domain_error(
                        "settings_hooks_project",
                        format!("规范化当前项目失败：{error}"),
                    )
                })?;
                if workspace_identity != project.to_string_lossy().as_ref() {
                    return Err(unsupported(
                        "settings_hooks_workspace_mismatch",
                        "Hook Trust 不属于当前焦点项目",
                    ));
                }
                let native = native_hooks::load(&self.paths, Some(&project))
                    .map_err(|error| domain_error("settings_hooks_load", error))?;
                if bundle_digest != hook_bundle_digest(&native) {
                    return Err(unsupported(
                        "settings_hooks_snapshot_mismatch",
                        "Hook 配置已变化，请重新加载后再操作 Trust",
                    ));
                }
                let Some((summary, definition)) = native
                    .summaries
                    .iter()
                    .zip(native.hooks.iter())
                    .find(|(summary, _)| summary.digest == hook_declaration_digest)
                else {
                    return Err(unsupported(
                        "settings_hooks_snapshot_mismatch",
                        "找不到当前 Hook 执行摘要",
                    ));
                };
                native_hooks::set_trusted(
                    &self.paths,
                    &project,
                    definition.scope.clone(),
                    &definition.id,
                    trusted,
                    native.trust_revision,
                )
                .map_err(|error| domain_error("settings_hook_trust", error))?;
                let _ = summary;
            }
            _ => {
                return Err(unsupported(
                    "settings_command_owner",
                    "该命令不属于 Hook 设置域",
                ));
            }
        }
        self.invalidate_current_project_extension_candidate()?;
        self.load_hooks()
    }
}

fn hook_phase(event: HookEvent) -> SettingsResult<HookPhase> {
    match event {
        HookEvent::SessionStart => Ok(HookPhase::SessionStart),
        HookEvent::SubagentStart => Ok(HookPhase::SubagentStart),
        HookEvent::UserPromptSubmit => Ok(HookPhase::UserPromptSubmit),
        HookEvent::PreToolUse => Ok(HookPhase::PreToolUse),
        HookEvent::PermissionRequest => Err(unsupported(
            "settings_hook_event_unsupported",
            "PermissionRequest 当前没有 Native 执行入口",
        )),
        HookEvent::PostToolUse => Ok(HookPhase::PostToolUse),
        HookEvent::PostToolUseFailure => Ok(HookPhase::PostToolUseFailure),
        HookEvent::OnError => Ok(HookPhase::OnError),
        HookEvent::PreCompact => Ok(HookPhase::PreCompact),
        HookEvent::PostCompact => Ok(HookPhase::PostCompact),
        HookEvent::Stop => Ok(HookPhase::Stop),
    }
}

fn hook_event(phase: HookPhase) -> HookEvent {
    match phase {
        HookPhase::SessionStart => HookEvent::SessionStart,
        HookPhase::SubagentStart => HookEvent::SubagentStart,
        HookPhase::UserPromptSubmit => HookEvent::UserPromptSubmit,
        HookPhase::PreToolUse => HookEvent::PreToolUse,
        HookPhase::PostToolUse => HookEvent::PostToolUse,
        HookPhase::PostToolUseFailure => HookEvent::PostToolUseFailure,
        HookPhase::OnError => HookEvent::OnError,
        HookPhase::PreCompact => HookEvent::PreCompact,
        HookPhase::PostCompact => HookEvent::PostCompact,
        HookPhase::Stop => HookEvent::Stop,
    }
}

fn hook_bundle_digest(snapshot: &native_hooks::HookStoreSnapshot) -> String {
    let payload = snapshot
        .summaries
        .iter()
        .map(|summary| (&summary.id, &summary.digest))
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&payload).expect("Hook bundle digest 输入必须可序列化");
    format!("{:x}", Sha256::digest(bytes))
}

fn hook_summary(
    definition: &NativeHookDefinition,
    summary: &native_hooks::HookSummary,
    project_root: Option<&Path>,
    bundle_digest: Option<&str>,
    index: usize,
) -> SettingsResult<HookSummary> {
    let (scope, source, source_path) = match &definition.scope {
        NativeHookScope::User => (
            HookScope::User,
            HookSource::User,
            Some("native-hooks.json".to_owned()),
        ),
        NativeHookScope::Project { root } => (
            HookScope::Project,
            HookSource::Project,
            Some(
                root.join(".keencode")
                    .join("hooks.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ),
        NativeHookScope::Plugin { .. } => {
            return Err(unsupported(
                "settings_hook_read_only",
                "插件 Hook 不能进入可编辑 Hook 列表",
            ));
        }
    };
    let event = hook_event(summary.phase);
    let (hook_type, command, args, shell, timeout_ms, context, block, input, continue_turn) =
        match &definition.handler {
            NativeHookHandler::Command {
                command,
                args,
                shell,
                timeout_ms,
                ..
            } => (
                HookType::Command,
                command.clone(),
                args.clone().unwrap_or_default(),
                shell.clone(),
                timeout_ms.unwrap_or(600_000),
                None,
                None,
                None,
                false,
            ),
            NativeHookHandler::Context {
                context,
                block,
                input,
                continue_turn,
            } => (
                HookType::Context,
                String::new(),
                Vec::new(),
                None,
                600_000,
                context.clone(),
                block.clone(),
                input.clone(),
                *continue_turn,
            ),
        };
    let (workspace_identity, bundle_digest, declaration_digest, trust_state) =
        if let Some(project_root) = project_root {
            // 用户与项目 Hook 都绑定当前 workspace；禁用 Hook 不需要新建 trust，
            // 但已有 trust 仍可撤销，避免停用后残留授权无法清理。
            let trust_state = if summary.trusted {
                HookTrustState::TrustedPersistent
            } else if definition.enabled {
                HookTrustState::PendingTrust
            } else {
                HookTrustState::NotApplicable
            };
            (
                Some(project_root.to_string_lossy().into_owned()),
                bundle_digest.map(ToOwned::to_owned),
                Some(summary.digest.clone()),
                trust_state,
            )
        } else {
            (None, None, None, HookTrustState::NotApplicable)
        };
    Ok(HookSummary {
        id: definition.id.clone(),
        scope,
        event,
        hook_type,
        matcher: definition.matcher.clone(),
        command,
        args,
        shell,
        timeout_ms,
        context,
        block,
        input,
        continue_turn,
        enabled: definition.enabled,
        editable: true,
        source,
        plugin_id: None,
        plugin_name: None,
        plugin_scope: None,
        workspace_identity,
        bundle_digest,
        hook_declaration_digest: declaration_digest,
        source_path,
        source_file_index: Some(match source {
            HookSource::User => 0,
            HookSource::Project => 1,
            HookSource::Plugin => index,
        }),
        hook_index: Some(index),
        trust_state,
        read_only_reason: (trust_state == HookTrustState::PendingTrust)
            .then_some("当前项目尚未信任执行摘要".to_owned()),
    })
}

fn normalized_hook_event(value: &str) -> Option<&'static str> {
    let normalized = value
        .chars()
        .filter(|character| !matches!(character, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect::<String>();
    match normalized.as_str() {
        "sessionstart" => Some("SessionStart"),
        "subagentstart" => Some("SubagentStart"),
        "userpromptsubmit" => Some("UserPromptSubmit"),
        "pretooluse" => Some("PreToolUse"),
        "posttooluse" => Some("PostToolUse"),
        "posttoolusefailure" => Some("PostToolUseFailure"),
        "onerror" | "stopfailure" => Some("OnError"),
        "precompact" => Some("PreCompact"),
        "postcompact" => Some("PostCompact"),
        "stop" => Some("Stop"),
        _ => None,
    }
}

fn normalized_plugin_values(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(values) => values.clone(),
        value => vec![value.clone()],
    }
}

fn plugin_hook_group(value: Value) -> (Option<String>, Value) {
    let Value::Object(mut object) = value else {
        return (None, value);
    };
    let matcher = object
        .remove("matcher")
        .and_then(|value| value.as_str().map(ToOwned::to_owned));
    if let Some(hooks) = object.remove("hooks") {
        return (matcher, hooks);
    }
    (matcher, Value::Object(object))
}

fn plugin_hook_diagnostics(snapshot: &plugins::PluginRuntimeSnapshot) -> Vec<PluginHookDiagnostic> {
    let mut diagnostics = Vec::new();
    for plugin in &snapshot.plugins {
        let Some(Value::Object(events)) = plugin.hooks.as_ref() else {
            continue;
        };
        for (event, groups) in events {
            let supported_event = normalized_hook_event(event).is_some();
            for group in normalized_plugin_values(groups) {
                let (matcher, items) = plugin_hook_group(group);
                for item in normalized_plugin_values(&items) {
                    let (hook_type, command, args) = match &item {
                        Value::String(command) => {
                            ("command".to_owned(), command.clone(), Vec::new())
                        }
                        Value::Object(object) => (
                            object
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("command")
                                .to_owned(),
                            object
                                .get("command")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            object
                                .get("args")
                                .and_then(Value::as_array)
                                .map(|values| {
                                    values
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(ToOwned::to_owned)
                                        .collect()
                                })
                                .unwrap_or_default(),
                        ),
                        _ => ("unknown".to_owned(), String::new(), Vec::new()),
                    };
                    let type_supported =
                        matches!(hook_type.as_str(), "command" | "process" | "context");
                    let supported = supported_event && type_supported;
                    let reason = (!supported).then(|| {
                        if !supported_event {
                            format!("Hook 事件 {event} 当前 Native 执行入口未实现")
                        } else {
                            format!("Hook 类型 {hook_type} 当前 Native 执行入口未实现")
                        }
                    });
                    diagnostics.push(PluginHookDiagnostic {
                        plugin_id: plugin.id.to_string(),
                        plugin_name: plugin.id.to_string(),
                        scope: "plugin".to_owned(),
                        event: event.clone(),
                        hook_type,
                        matcher: matcher.clone(),
                        command,
                        args,
                        supported,
                        reason,
                    });
                }
            }
        }
    }
    diagnostics.sort_by(|left, right| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then_with(|| left.event.cmp(&right.event))
            .then_with(|| left.command.cmp(&right.command))
    });
    diagnostics
}

impl NativeSettingsDomain for NativeSettingsRuntimeDomain {
    fn load(&self, page: SettingsPage) -> SettingsResult<SettingsSnapshot> {
        match page {
            SettingsPage::Hooks => self.load_hooks(),
            SettingsPage::Resources => self.load_resources(),
            SettingsPage::Automations | SettingsPage::Workflows => self.runtime_port.load(page),
            SettingsPage::Agents => self.load_agents(),
            SettingsPage::General
            | SettingsPage::Appearance
            | SettingsPage::Keyboard
            | SettingsPage::Providers
            | SettingsPage::Usage
            | SettingsPage::Diagnostics => Err(NativeSettingsError::new(
                "settings_page_owner",
                "该设置页由 NativeSettingsAdapter 直接处理",
            )),
        }
    }

    fn execute(&self, command: SettingsCommand) -> SettingsResult<SettingsSnapshot> {
        match command {
            SettingsCommand::SaveGeneral(_)
            | SettingsCommand::SaveAppearance(_)
            | SettingsCommand::SaveKeyboard(_) => Err(NativeSettingsError::new(
                "settings_page_owner",
                "常规和外观设置由 NativeSettingsAdapter 直接处理",
            )),
            SettingsCommand::CreateHook(_)
            | SettingsCommand::UpdateHook { .. }
            | SettingsCommand::DeleteHook { .. }
            | SettingsCommand::SetHookEnabled { .. }
            | SettingsCommand::GrantHookTrust { .. }
            | SettingsCommand::RevokeHookTrust { .. } => self.execute_hook_command(command),
            SettingsCommand::InstallPlugin { source } => {
                self.install_plugin(&source)?;
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::UpdatePlugin { plugin_id } => {
                self.update_plugin(&plugin_id)?;
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::SetPluginEnabled { plugin_id, enabled } => {
                self.set_plugin_enabled(&plugin_id, enabled)?;
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::UninstallPlugin { plugin_id } => {
                self.uninstall_plugin(&plugin_id)?;
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::ConfigurePlugin { plugin_id, values } => {
                self.configure_plugin(&plugin_id, &values)?;
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::SetMcpEnabled { server_id, enabled } => {
                let (scope, _) = split_mcp_id(&server_id);
                self.set_mcp_enabled(&server_id, enabled)?;
                self.invalidate_extension_candidates_for_scope(scope)?;
                self.load_resources()
            }
            SettingsCommand::RemoveMcp { server_id } => {
                let (scope, _) = split_mcp_id(&server_id);
                self.remove_mcp(&server_id)?;
                self.invalidate_extension_candidates_for_scope(scope)?;
                self.load_resources()
            }
            SettingsCommand::AddMcp { server } | SettingsCommand::UpdateMcp { server } => {
                let scope = server.scope.clone();
                self.save_mcp(&server)?;
                self.invalidate_extension_candidates_for_scope(&scope)?;
                self.load_resources()
            }
            SettingsCommand::InspectMcp { server_id } => self.inspect_mcp(&server_id),
            SettingsCommand::SetSkillEnabled { skill_id, enabled } => {
                self.set_skill_enabled(&skill_id, enabled)?;
                // disabled skill state 位于 data_root，对所有项目的发现结果生效。
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::CopySkillToCommon { skill_id } => {
                self.copy_skill_to_common(&skill_id)?;
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::RemoveSkillFromCommon { skill_id }
            | SettingsCommand::DeleteSkill { skill_id } => {
                self.delete_skill(&skill_id)?;
                // 删除同时更新全局 disabled skill 状态，必须覆盖其他项目候选。
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::CreateSkill { skill } | SettingsCommand::UpdateSkill { skill } => {
                let scope = skill
                    .id
                    .split_once(':')
                    .map(|(scope, _)| scope.to_owned())
                    .ok_or_else(|| unsupported("settings_skill_id", "Skill 标识必须包含作用域"))?;
                self.save_skill(&skill)?;
                self.invalidate_extension_candidates_for_scope(&scope)?;
                self.load_resources()
            }
            SettingsCommand::ReadMemory {
                workspace_id,
                file_name,
            } => {
                let memory = self.read_memory(&workspace_id, &file_name)?;
                self.load_resources_with_memory(memory)
            }
            SettingsCommand::DeleteMemory {
                workspace_id,
                file_name,
            } => {
                self.delete_memory(&workspace_id, &file_name)?;
                self.load_resources()
            }
            SettingsCommand::CreateMemory { memory } | SettingsCommand::UpdateMemory { memory } => {
                self.save_memory(&memory)?;
                let saved = self.read_memory(&memory.workspace_id, &memory.file_name)?;
                self.load_resources_with_memory(saved)
            }
            SettingsCommand::CreateAgentTemplate(template)
            | SettingsCommand::UpdateAgentTemplate(template) => {
                let scope = parse_agent_template_id(&template.id)
                    .map(|(scope, _)| scope.to_owned())
                    .ok_or_else(|| {
                        unsupported("settings_agent_id", "只能修改全局或项目 Agent 模板")
                    })?;
                self.save_agent_template(&template)?;
                self.invalidate_extension_candidates_for_scope(&scope)?;
                self.load_resources()
            }
            SettingsCommand::DeleteAgentTemplate { agent_id } => {
                let scope = parse_agent_template_id(&agent_id)
                    .map(|(scope, _)| scope.to_owned())
                    .ok_or_else(|| {
                        unsupported("settings_agent_id", "只能修改全局或项目 Agent 模板")
                    })?;
                self.delete_agent_template(&agent_id)?;
                self.invalidate_extension_candidates_for_scope(&scope)?;
                self.load_resources()
            }
            SettingsCommand::SetAgentTemplateEnabled { agent_id, enabled } => {
                self.set_agent_enabled(&agent_id, enabled)?;
                self.invalidate_all_project_extension_candidates()?;
                self.load_resources()
            }
            SettingsCommand::CreateAutomation(_)
            | SettingsCommand::UpdateAutomation(_)
            | SettingsCommand::DeleteAutomation { .. }
            | SettingsCommand::SetAutomationEnabled { .. }
            | SettingsCommand::RunAutomation { .. }
            | SettingsCommand::CancelAutomation { .. }
            | SettingsCommand::SaveWorkflow(_)
            | SettingsCommand::MoveWorkflow { .. }
            | SettingsCommand::DeleteWorkflow { .. }
            | SettingsCommand::RunWorkflow { .. }
            | SettingsCommand::CancelWorkflow { .. }
            | SettingsCommand::AmendWorkflow { .. }
            | SettingsCommand::ReadWorkflowGraph { .. }
            | SettingsCommand::ReadWorkflowWorkspace { .. }
            | SettingsCommand::ReadWorkflowNodeResult { .. }
            | SettingsCommand::ListWorkflowArtifacts { .. }
            | SettingsCommand::ListWorkflowArtifactItems { .. }
            | SettingsCommand::ReadWorkflowArtifact { .. }
            | SettingsCommand::ReadWorkflowEvents { .. }
            | SettingsCommand::CreateSubagent(_)
            | SettingsCommand::UpdateSubagent(_)
            | SettingsCommand::DeleteSubagent { .. }
            | SettingsCommand::SetSubagentEnabled { .. }
            | SettingsCommand::SetSubagentModel { .. } => self.execute_agent_command(command),
            SettingsCommand::ResumeWorkflow { run_id } => {
                self.runtime_port.resume_workflow(&run_id)
            }
            SettingsCommand::ResolveWorkflowQuestion {
                question_id,
                answer,
            } => self
                .runtime_port
                .resolve_workflow_question(&question_id, &answer),
            SettingsCommand::ListAutomationHistory { automation_id } => {
                let snapshot = self.runtime_port.load(SettingsPage::Automations)?;
                let exists = snapshot.automations.as_ref().is_some_and(|settings| {
                    settings.items.iter().any(|item| item.id == automation_id)
                });
                if !exists {
                    return Err(unsupported(
                        "settings_automation_not_found",
                        "找不到 Automation",
                    ));
                }
                self.runtime_port
                    .execute(SettingsCommand::ListAutomationHistory { automation_id })
            }
            SettingsCommand::ListWorkflowRuns { name, scope } => {
                let snapshot = self.runtime_port.load(SettingsPage::Workflows)?;
                let exists = snapshot.workflows.as_ref().is_some_and(|settings| {
                    settings
                        .items
                        .iter()
                        .any(|item| item.name == name && item.scope == scope)
                });
                if !exists {
                    return Err(unsupported("settings_workflow_not_found", "找不到工作流"));
                }
                self.runtime_port
                    .execute(SettingsCommand::ListWorkflowRuns { name, scope })
            }
            SettingsCommand::SetGoal {
                goal_id,
                title,
                detail,
                expected_revision,
            } => {
                self.set_goal(&goal_id, &title, detail, expected_revision)?;
                self.load_agents()
            }
            SettingsCommand::ClearGoal {
                goal_id,
                expected_revision,
            } => {
                self.clear_goal(&goal_id, expected_revision)?;
                self.load_agents()
            }
            SettingsCommand::ResumeGoal {
                goal_id,
                expected_revision,
            } => {
                self.resume_goal(&goal_id, expected_revision)?;
                self.load_agents()
            }
            SettingsCommand::PauseGoal {
                goal_id,
                expected_revision,
            } => {
                self.pause_goal(&goal_id, expected_revision)?;
                self.load_agents()
            }
            SettingsCommand::CompleteGoal {
                goal_id,
                expected_revision,
                evidence,
            } => {
                self.complete_goal(&goal_id, expected_revision, &evidence)?;
                self.load_agents()
            }
            SettingsCommand::BlockGoal {
                goal_id,
                expected_revision,
                reason,
            } => {
                self.block_goal(&goal_id, expected_revision, &reason)?;
                self.load_agents()
            }
            SettingsCommand::CancelGoalRun {
                goal_id,
                expected_revision,
                turn_id,
            } => {
                self.cancel_goal_run(&goal_id, expected_revision, &turn_id)?;
                self.load_agents()
            }
            _ => Err(unsupported(
                "settings_command_owner",
                "该命令不属于资源设置域",
            )),
        }
    }
}

impl NativeSettingsRuntimeDomain {
    /// Runtime 端口返回的 Agent 快照不包含设置页自己的 Goal 和模板投影；
    /// 命令完成后重新读取并合并，避免一次子智能体操作清空已加载的设置状态。
    fn execute_agent_command(&self, command: SettingsCommand) -> SettingsResult<SettingsSnapshot> {
        let mut snapshot = self.runtime_port.execute(command)?;
        snapshot.agents = self.load_agents()?.agents;
        Ok(snapshot)
    }

    fn load_resources(&self) -> SettingsResult<SettingsSnapshot> {
        let project = self.current_project()?;
        let mut snapshot = self.snapshot(SettingsPage::Resources);
        snapshot.resources = Some(ResourceSettings {
            plugins: self.plugins()?,
            mcp_servers: self.mcp_servers(project.as_deref())?,
            skills: self.skills(project.as_deref())?,
            memories: self.memories()?,
            agent_templates: self.agent_templates(project.as_deref())?,
        });
        Ok(snapshot)
    }

    /// 只把调用方明确读取或保存的 Memory 正文放入确认快照，列表加载不批量读取正文。
    fn load_resources_with_memory(&self, memory: MemoryFile) -> SettingsResult<SettingsSnapshot> {
        let mut snapshot = self.load_resources()?;
        let resources = snapshot
            .resources
            .as_mut()
            .ok_or_else(|| unsupported("settings_resources_missing", "资源设置投影为空"))?;
        let summary = resources
            .memories
            .iter_mut()
            .find(|item| {
                item.workspace_id == memory.workspace_id && item.file_name == memory.file_name
            })
            .ok_or_else(|| unsupported("settings_memory_not_found", "找不到指定 Memory"))?;
        summary.content = Some(memory.content);
        summary.updated_at_ms = memory.updated_at_ms;
        Ok(snapshot)
    }

    /// 重新读取当前已发布的 MCP 运行态，并拒绝对不存在的 Server 返回伪成功。
    fn inspect_mcp(&self, server_id: &str) -> SettingsResult<SettingsSnapshot> {
        let snapshot = self.load_resources()?;
        let exists = snapshot.resources.as_ref().is_some_and(|resources| {
            resources
                .mcp_servers
                .iter()
                .any(|server| server.id == server_id)
        });
        if !exists {
            return Err(unsupported(
                "settings_mcp_not_found",
                "找不到指定 MCP Server",
            ));
        }
        Ok(snapshot)
    }

    fn plugins(&self) -> SettingsResult<Vec<PluginSummary>> {
        let manager = PluginManager::new(self.paths.data_root.clone());
        let state = manager
            .load_state()
            .map_err(|error| domain_error("settings_plugin_load", error))?;
        let command_names = self.plugin_command_names(&manager);
        let mut items = state
            .plugins
            .into_iter()
            .map(|item| {
                let manifest = plugins::load_plugin_manifest(&item.install_path).ok();
                let inventory = manifest.as_ref().and_then(|manifest| {
                    plugins::inspect_plugin_components(&item.install_path, manifest).ok()
                });
                PluginSummary {
                    id: item.id.to_string(),
                    name: manifest
                        .as_ref()
                        .map(|manifest| manifest.name.clone())
                        .unwrap_or_else(|| item.id.plugin.clone()),
                    version: manifest
                        .as_ref()
                        .and_then(|manifest| manifest.version.clone()),
                    scope: item
                        .id
                        .marketplace
                        .clone()
                        .unwrap_or_else(|| "user".to_owned()),
                    enabled: item.enabled,
                    // 这里表达“存在可重新获取的 marketplace 来源”；真实版本/内容
                    // 只在点击更新时强制物化，避免加载设置页时隐式联网。
                    update_available: matches!(
                        item.source.as_ref(),
                        Some(PluginInstallSource::Marketplace { .. })
                    ),
                    // 可执行状态基于真实组件扫描；清单存在但组件路径损坏时，
                    // 更新按钮必须关闭，避免设置页把坏插件当成可运行插件。
                    executable: item.install_path.is_dir() && inventory.is_some(),
                    description: manifest.and_then(|manifest| manifest.description),
                    config: item
                        .public_user_config
                        .iter()
                        .map(|(key, value)| {
                            let text = value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string());
                            (key.clone(), text)
                        })
                        .collect(),
                    commands: fs::canonicalize(&item.install_path)
                        .ok()
                        .and_then(|path| command_names.get(&path).cloned())
                        .unwrap_or_default(),
                    unsupported_hooks: unsupported_hooks_from_inventory(inventory.as_ref()),
                }
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(items)
    }

    /// 从当前启用插件的真实运行时快照导出 command 目录；损坏插件继续隔离，
    /// 不阻断资源页显示其他已安装插件。
    fn plugin_command_names(&self, manager: &PluginManager) -> BTreeMap<PathBuf, Vec<String>> {
        let project = self
            .current_project()
            .ok()
            .flatten()
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| self.paths.data_root.clone());
        let secrets = SystemSecretStore;
        let Ok(snapshot) = manager.runtime_snapshot(&project, &BTreeMap::new(), &secrets) else {
            return BTreeMap::new();
        };
        let Ok(catalog) = PluginCommandCatalog::from_snapshot(&snapshot) else {
            return BTreeMap::new();
        };
        let mut names = BTreeMap::<PathBuf, Vec<String>>::new();
        for entry in catalog.entries() {
            // 目录投影只展示当前仍可安全读取的文件；执行时仍由 Agent 工具重新校验。
            if plugins::plugin_command_description(&entry.root, &entry.path).is_none() {
                continue;
            }
            let Ok(root) = fs::canonicalize(&entry.root) else {
                continue;
            };
            names.entry(root).or_default().push(entry.name.clone());
        }
        names.values_mut().for_each(|items| items.sort());
        names
    }

    fn mcp_servers(&self, project: Option<&Path>) -> SettingsResult<Vec<McpServerSummary>> {
        let runtime_servers = match project {
            Some(project) => self
                .runtime
                .mcp_runtime_snapshot(project)
                .map_err(|error| domain_error("settings_mcp_runtime", error))?
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut result = Vec::new();
        for (scope, path) in self.mcp_paths(project) {
            let Some(document) = read_optional_json(&path, "MCP 配置")? else {
                continue;
            };
            let Some(servers) = document.get("mcpServers").and_then(Value::as_object) else {
                continue;
            };
            for (name, config) in servers {
                let config = config.as_object();
                let transport = if config.and_then(|value| value.get("url")).is_some() {
                    "http"
                } else {
                    "stdio"
                };
                let enabled = config
                    .and_then(|value| value.get("disabled"))
                    .and_then(Value::as_bool)
                    != Some(true)
                    && config
                        .and_then(|value| value.get("enabled"))
                        .and_then(Value::as_bool)
                        != Some(false);
                let id = mcp_id(scope, name);
                let runtime = runtime_servers.iter().find(|item| item.name == *name);
                result.push(McpServerSummary {
                    id,
                    scope: scope.to_owned(),
                    name: name.clone(),
                    transport: transport.to_owned(),
                    enabled,
                    connected: runtime.is_some_and(|item| {
                        item.connection_status == McpConnectionStatus::Connected
                    }),
                    tool_count: runtime.map(|item| item.tools_count),
                    error: runtime.and_then(|item| item.error.clone()),
                    config_json: config
                        .map(|value| {
                            serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned())
                        })
                        .unwrap_or_else(|| "{}".to_owned()),
                });
            }
        }
        result.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(result)
    }

    fn skills(&self, project: Option<&Path>) -> SettingsResult<Vec<SkillSummary>> {
        let disabled = read_disabled_skills(&self.paths.data_root)?;
        let mut items = Vec::new();
        scan_skill_root(
            &self.paths.data_root.join("skills"),
            "global",
            &disabled,
            &mut items,
        );
        if let Some(project) = project {
            scan_skill_root(
                &project.join(".agents").join("skills"),
                "project",
                &disabled,
                &mut items,
            );
        }
        let manager = PluginManager::new(self.paths.data_root.clone());
        if let Ok(state) = manager.load_state() {
            for plugin in state.plugins {
                scan_skill_root(
                    &plugin.install_path.join("skills"),
                    &format!("plugin:{}", plugin.id),
                    &disabled,
                    &mut items,
                );
            }
        }
        items.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(items)
    }

    fn memories(&self) -> SettingsResult<Vec<MemorySummary>> {
        self.memory_service
            .list_files()
            .map_err(|error| domain_error("settings_memory_list", error))
            .map(|files| {
                files
                    .into_iter()
                    .map(|file| MemorySummary {
                        workspace_id: "global".to_owned(),
                        label: file.file_name.trim_end_matches(".md").to_owned(),
                        file_name: file.file_name,
                        kind: "markdown".to_owned(),
                        size: file.size,
                        updated_at_ms: file.updated_at_ms,
                        content: None,
                    })
                    .collect()
            })
    }
}

fn unsupported_hooks_from_inventory(inventory: Option<&plugins::PluginInventory>) -> Vec<String> {
    inventory
        .map(|inventory| inventory.unsupported_hooks.clone())
        .unwrap_or_default()
}

fn scan_skill_root(
    root: &Path,
    scope: &str,
    disabled: &BTreeSet<String>,
    output: &mut Vec<SkillSummary>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        let Ok(skill_metadata) = fs::symlink_metadata(&skill_file) else {
            continue;
        };
        if skill_metadata.file_type().is_symlink() || !skill_metadata.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .trim()
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let (front_name, description) = read_markdown_header(&skill_file, 64 * 1024);
        let display_name = front_name.unwrap_or_else(|| name.clone());
        let content = read_file_bounded(&skill_file, MAX_MEMORY_BYTES)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        // 稳定 ID 必须使用目录名；front matter 的展示名可随正文编辑而变化，不能作为路径。
        let id = format!("{scope}:{name}");
        let enabled = !disabled.contains(&display_name.to_ascii_lowercase())
            && !disabled.contains(&id.to_ascii_lowercase());
        output.push(SkillSummary {
            id,
            name: display_name,
            scope: scope.to_owned(),
            enabled,
            description,
            path: Some(skill_file.to_string_lossy().into_owned()),
            content,
        });
    }
}

fn read_markdown_header(path: &Path, max_bytes: u64) -> (Option<String>, Option<String>) {
    let Ok(bytes) = read_file_bounded(path, max_bytes) else {
        return (None, None);
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        match key.trim().to_ascii_lowercase().as_str() {
            "name" => name = (!value.is_empty()).then(|| value.to_owned()),
            "description" => description = (!value.is_empty()).then(|| value.to_owned()),
            _ => {}
        }
    }
    (name, description)
}

impl NativeSettingsRuntimeDomain {
    fn agent_templates(&self, project: Option<&Path>) -> SettingsResult<Vec<AgentTemplateSummary>> {
        let disabled = read_disabled_agents(&self.paths.data_root)?;
        let mut result = Vec::new();
        scan_agent_root(
            &self.paths.data_root.join("agents"),
            "global",
            &disabled,
            &mut result,
        )?;
        if let Some(project) = project {
            scan_agent_root(
                &project.join(".keencode").join("agents"),
                "project",
                &disabled,
                &mut result,
            )?;
        }
        let manager = PluginManager::new(self.paths.data_root.clone());
        if let Ok(state) = manager.load_state() {
            for plugin in state.plugins {
                scan_agent_root(
                    &plugin.install_path.join("agents"),
                    &format!("plugin:{}", plugin.id),
                    &disabled,
                    &mut result,
                )?;
            }
        }
        result.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(result)
    }

    fn load_agents(&self) -> SettingsResult<SettingsSnapshot> {
        let mut snapshot = self.snapshot(SettingsPage::Agents);
        let Some((session_id, session)) = self.current_session()? else {
            snapshot.agents = Some(AgentSettings {
                goal: None,
                goal_revision: 0,
                subagents: Vec::new(),
            });
            return Ok(snapshot);
        };
        let mut subagents = session
            .read_state(|state| {
                state
                    .sub_agents
                    .values()
                    .map(|agent| SubagentSummary {
                        id: agent.agent_id.to_string(),
                        name: agent.agent_path.clone(),
                        source: "runtime".to_owned(),
                        enabled: !matches!(
                            agent.status,
                            keencode_resources::SubAgentStatus::Stopped
                        ),
                        model: None,
                        tools: Vec::new(),
                        max_turns: None,
                        description: Some(agent.task.clone()),
                    })
                    .collect::<Vec<_>>()
            })
            .map_err(|error| domain_error("settings_agent_snapshot", error))?;
        let (goal, goal_revision) = self.goal_state(&session_id, &session)?;
        subagents.sort_by(|left, right| left.id.cmp(&right.id));
        snapshot.agents = Some(AgentSettings {
            goal,
            goal_revision,
            subagents,
        });
        Ok(snapshot)
    }
}

fn scan_agent_root(
    root: &Path,
    scope: &str,
    disabled: &BTreeSet<String>,
    output: &mut Vec<AgentTemplateSummary>,
) -> SettingsResult<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(domain_error("settings_agent_list", error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| domain_error("settings_agent_list", error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| domain_error("settings_agent_stat", error))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("md")
        {
            continue;
        }
        if metadata.len() > MAX_TEMPLATE_BYTES {
            continue;
        }
        let id_name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| valid_component(value))
            .unwrap_or_default();
        if id_name.is_empty() {
            continue;
        }
        let id = format!("{scope}:{id_name}");
        let (front, body) = parse_agent_document(&path)?;
        let tools = parse_tools(front.get("tools"));
        let model = front
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let max_turns = front
            .get("maxTurns")
            .or_else(|| front.get("max_turns"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        let description = front
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                body.lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::to_owned)
            });
        output.push(AgentTemplateSummary {
            id: id.clone(),
            name: front
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(id_name)
                .to_owned(),
            scope: scope.to_owned(),
            enabled: !disabled.contains(&id.to_ascii_lowercase())
                && !disabled.contains(&id_name.to_ascii_lowercase()),
            model: model.as_deref().and_then(parse_model_selection),
            tools,
            max_turns,
            description,
            content: Some(body),
        });
    }
    Ok(())
}

fn parse_agent_document(path: &Path) -> SettingsResult<(Map<String, Value>, String)> {
    let bytes = read_file_bounded(path, MAX_TEMPLATE_BYTES)
        .map_err(|error| domain_error("settings_agent_read", error))?;
    let text =
        String::from_utf8(bytes).map_err(|error| domain_error("settings_agent_encoding", error))?;
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return Ok((Map::new(), String::new()));
    };
    if first.trim() != "---" {
        return Ok((Map::new(), text));
    }
    let mut fields = Map::new();
    let mut body_start = None;
    for (index, line) in text.lines().enumerate().skip(1) {
        if line.trim() == "---" {
            body_start = Some(index + 1);
            break;
        }
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if !valid_component(key) {
            continue;
        }
        let raw = raw.trim();
        let value = if raw.starts_with('[') || raw.starts_with('{') {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
        } else if raw.eq_ignore_ascii_case("true") || raw.eq_ignore_ascii_case("false") {
            Value::Bool(raw.eq_ignore_ascii_case("true"))
        } else if let Ok(number) = raw.parse::<u64>() {
            Value::Number(number.into())
        } else {
            Value::String(raw.trim_matches(['"', '\'']).to_owned())
        };
        fields.insert(key.to_owned(), value);
    }
    let body = body_start
        .map(|start| text.lines().skip(start).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    Ok((fields, body))
}

fn parse_tools(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| valid_component(value))
            .map(str::to_owned)
            .collect(),
        Some(Value::String(value)) => value
            .split(',')
            .map(str::trim)
            .filter(|value| valid_component(value))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_model_selection(value: &str) -> Option<ModelSelection> {
    let (provider_id, model_id) = value.split_once("::")?;
    if !valid_component(provider_id) || !valid_component(model_id) {
        return None;
    }
    Some(ModelSelection {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
    })
}

fn read_disabled_agents(root: &Path) -> SettingsResult<BTreeSet<String>> {
    let Some(value) = read_optional_json(&root.join("agents-state.json"), "Agent 设置")? else {
        return Ok(BTreeSet::new());
    };
    Ok(value
        .get("disabledAgentIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
        .collect())
}

fn write_disabled_agents(root: &Path, id: &str, enabled: bool) -> SettingsResult<()> {
    if !valid_component(id) && !id.contains(':') {
        return Err(unsupported("settings_agent_id", "Agent 标识无效"));
    }
    let path = root.join("agents-state.json");
    let mut value = read_optional_json(&path, "Agent 设置")?.unwrap_or_else(|| json!({}));
    let object = value
        .as_object_mut()
        .ok_or_else(|| unsupported("settings_agent_state", "Agent 设置必须是对象"))?;
    let values = object
        .entry("disabledAgentIds")
        .or_insert_with(|| Value::Array(Vec::new()));
    let list = values
        .as_array_mut()
        .ok_or_else(|| unsupported("settings_agent_state", "disabledAgentIds 必须是数组"))?;
    list.retain(|value| {
        value
            .as_str()
            .is_none_or(|stored| !stored.eq_ignore_ascii_case(id))
    });
    if !enabled {
        list.push(Value::String(id.to_owned()));
    }
    write_json(&path, &value, "Agent 设置")
}

fn parse_agent_template_id(id: &str) -> Option<(&str, &str)> {
    let (scope, name) = id.split_once(':')?;
    if !matches!(scope, "global" | "project") || !valid_file_name(name) {
        return None;
    }
    Some((scope, name))
}

fn template_path(root: &Path, project: Option<&Path>, id: &str) -> SettingsResult<PathBuf> {
    let Some((scope, name)) = parse_agent_template_id(id) else {
        return Err(unsupported(
            "settings_agent_id",
            "只能修改全局或项目 Agent 模板",
        ));
    };
    let base = match scope {
        "global" => root.join("agents"),
        "project" => project
            .ok_or_else(|| unsupported("settings_project_required", "项目 Agent 模板需要当前项目"))?
            .join(".keencode")
            .join("agents"),
        _ => unreachable!(),
    };
    Ok(base.join(format!("{name}.md")))
}

impl NativeSettingsRuntimeDomain {
    fn set_agent_enabled(&self, id: &str, enabled: bool) -> SettingsResult<()> {
        write_disabled_agents(&self.paths.data_root, id, enabled)
    }

    fn save_agent_template(&self, template: &AgentTemplateSummary) -> SettingsResult<()> {
        let project = self.current_project()?;
        let path = template_path(&self.paths.data_root, project.as_deref(), &template.id)?;
        let body = match template.content.clone() {
            Some(content) => content,
            None if path.is_file() => parse_agent_document(&path)?.1,
            None => String::new(),
        };
        let mut front = Vec::new();
        front.push("---".to_owned());
        front.push(format!("name: {}", template.name));
        if let Some(description) = &template.description {
            front.push(format!("description: {}", description));
        }
        if let Some(model) = &template.model {
            front.push(format!("model: {}::{}", model.provider_id, model.model_id));
        }
        if !template.tools.is_empty() {
            front.push(format!(
                "tools: [{}]",
                template
                    .tools
                    .iter()
                    .map(|tool| format!("\"{}\"", tool.replace('"', "")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(max_turns) = template.max_turns {
            front.push(format!("maxTurns: {max_turns}"));
        }
        front.push("---".to_owned());
        front.push(body);
        let content = front.join("\n");
        if content.len() as u64 > MAX_TEMPLATE_BYTES {
            return Err(unsupported("settings_agent_size", "Agent 模板超过大小限制"));
        }
        storage::atomic_write_private(&path, content.as_bytes())
            .map_err(|error| domain_error("settings_agent_write", error))
    }

    fn delete_agent_template(&self, id: &str) -> SettingsResult<()> {
        let project = self.current_project()?;
        let path = template_path(&self.paths.data_root, project.as_deref(), id)?;
        remove_regular_file(&path, "Agent 模板")
    }
}

impl NativeSettingsRuntimeDomain {
    fn save_mcp(&self, server: &McpServerSummary) -> SettingsResult<()> {
        let scope = match server.scope.as_str() {
            "global" | "project" => server.scope.as_str(),
            _ => return Err(unsupported("settings_mcp_scope", "MCP 作用域无效")),
        };
        if !valid_file_name(&server.name) {
            return Err(unsupported("settings_mcp_name", "MCP Server 名称无效"));
        }
        let project = self.current_project()?;
        let path = self
            .mcp_paths(project.as_deref())
            .into_iter()
            .find(|(candidate, _)| *candidate == scope)
            .map(|(_, path)| path)
            .ok_or_else(|| unsupported("settings_mcp_scope", "MCP 作用域不可用"))?;
        let mut document = read_optional_json(&path, "MCP 配置")?.unwrap_or_else(|| {
            json!({
                "schema": "keencode/mcp",
                "version": 1,
                "mcpServers": {}
            })
        });
        let config = if server.config_json.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str::<Value>(&server.config_json)
                .map_err(|error| domain_error("settings_mcp_json", error))?
        };
        let mut config = config
            .as_object()
            .cloned()
            .ok_or_else(|| unsupported("settings_mcp_json", "MCP 配置必须是 JSON 对象"))?;
        match server.transport.as_str() {
            "http" => {
                if config
                    .get("url")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(unsupported(
                        "settings_mcp_transport",
                        "HTTP MCP 必须提供 url",
                    ));
                }
                config.remove("command");
            }
            "stdio" => {
                if config
                    .get("command")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(unsupported(
                        "settings_mcp_transport",
                        "stdio MCP 必须提供 command",
                    ));
                }
                config.remove("url");
            }
            _ => return Err(unsupported("settings_mcp_transport", "MCP 传输类型无效")),
        }
        if server.enabled {
            config.remove("disabled");
            config.insert("enabled".to_owned(), Value::Bool(true));
        } else {
            config.insert("disabled".to_owned(), Value::Bool(true));
            config.remove("enabled");
        }
        let servers = document
            .as_object_mut()
            .and_then(|root| root.get_mut("mcpServers"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| unsupported("settings_mcp_shape", "MCP 配置缺少 mcpServers"))?;
        servers.insert(server.name.clone(), Value::Object(config));
        write_json(&path, &document, "MCP 配置")
    }

    fn set_mcp_enabled(&self, id: &str, enabled: bool) -> SettingsResult<()> {
        let project = self.current_project()?;
        let (scope, name) = split_mcp_id(id);
        let path = self
            .mcp_paths(project.as_deref())
            .into_iter()
            .find(|(candidate, _)| *candidate == scope)
            .map(|(_, path)| path)
            .ok_or_else(|| unsupported("settings_mcp_scope", "MCP 作用域不可用"))?;
        update_mcp_document(&path, name, |config| {
            if enabled {
                config.remove("disabled");
                config.insert("enabled".to_owned(), Value::Bool(true));
            } else {
                config.insert("disabled".to_owned(), Value::Bool(true));
                config.remove("enabled");
            }
        })
    }

    fn remove_mcp(&self, id: &str) -> SettingsResult<()> {
        let project = self.current_project()?;
        let (scope, name) = split_mcp_id(id);
        let path = self
            .mcp_paths(project.as_deref())
            .into_iter()
            .find(|(candidate, _)| *candidate == scope)
            .map(|(_, path)| path)
            .ok_or_else(|| unsupported("settings_mcp_scope", "MCP 作用域不可用"))?;
        let Some(mut value) = read_optional_json(&path, "MCP 配置")? else {
            return Err(unsupported("settings_mcp_not_found", "找不到 MCP Server"));
        };
        let servers = value
            .get_mut("mcpServers")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| unsupported("settings_mcp_shape", "MCP 配置缺少 mcpServers"))?;
        if servers.remove(name).is_none() {
            return Err(unsupported("settings_mcp_not_found", "找不到 MCP Server"));
        }
        write_json(&path, &value, "MCP 配置")
    }

    fn mcp_paths(&self, project: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
        let mut paths = vec![("global", self.paths.data_root.join("mcp.json"))];
        if let Some(project) = project {
            paths.push(("project", project.join(".agents").join("mcp.json")));
        }
        paths
    }

    fn set_plugin_enabled(&self, id: &str, enabled: bool) -> SettingsResult<()> {
        let manager = PluginManager::new(self.paths.data_root.clone());
        let parsed =
            PluginId::parse(id).map_err(|error| domain_error("settings_plugin_id", error))?;
        manager
            .set_enabled(&parsed, enabled)
            .map_err(|error| domain_error("settings_plugin_enable", error))?;
        Ok(())
    }

    fn install_plugin(&self, source: &str) -> SettingsResult<()> {
        let source = source.trim();
        if source.is_empty() {
            return Err(unsupported("settings_plugin_source", "插件目录不能为空"));
        }
        if let Some((marketplace_source, requested_plugin)) = source.rsplit_once('#') {
            let marketplace_source = marketplace_source.trim();
            let requested_plugin = requested_plugin.trim();
            if marketplace_source.is_empty() || requested_plugin.is_empty() {
                return Err(unsupported(
                    "settings_plugin_source",
                    "市场来源和插件名称都不能为空",
                ));
            }
            return self.install_marketplace_plugin(marketplace_source, Some(requested_plugin));
        }
        let source_root = PathBuf::from(source);
        if looks_like_marketplace_input(source) || looks_like_marketplace_source(&source_root) {
            return self.install_marketplace_plugin(source, None);
        }
        let manifest = plugins::load_plugin_manifest(&source_root)
            .map_err(|error| domain_error("settings_plugin_manifest", error))?;
        let source_root = fs::canonicalize(&source_root)
            .map_err(|error| domain_error("settings_plugin_source", error))?;
        let id = PluginId::from_components(&manifest.name, Some("local"))
            .map_err(|error| domain_error("settings_plugin_id", error))?;
        let manager = PluginManager::new(self.paths.data_root.clone());
        let mut secrets = SystemSecretStore;
        manager
            .install_from_directory(
                MaterializedPlugin {
                    id,
                    source_root: source_root.clone(),
                    source: Some(PluginInstallSource::Local { path: source_root }),
                },
                UserConfigUpdate::default(),
                &mut secrets,
            )
            .map_err(|error| domain_error("settings_plugin_install", error))
    }

    /// 从 marketplace 清单安装一个插件及其依赖；来源物化由插件域统一审计。
    ///
    /// 设置页使用 `marketplace.json#plugin-name` 选择条目；未指定名称时仅允许
    /// 单插件清单自动选择。远程来源完成下载、解包、清单校验和原子缓存后，
    /// 才进入现有 PluginManager 安装事务。
    fn install_marketplace_plugin(
        &self,
        source: &str,
        requested_plugin: Option<&str>,
    ) -> SettingsResult<()> {
        let cache_root = self.paths.data_root.join("plugins").join("marketplaces");
        let marketplace = plugins::materialize_marketplace_source(source, &cache_root)
            .map_err(|error| domain_error("settings_plugin_marketplace_source", error))?;
        let materialized = plugins::resolve_marketplace_plugin_install_plan(
            &marketplace,
            requested_plugin,
            &cache_root,
        )
        .map_err(|error| domain_error("settings_plugin_marketplace_install_plan", error))?;
        let manager = PluginManager::new(self.paths.data_root.clone());
        let mut secrets = SystemSecretStore;
        manager
            .install_from_directories(materialized, UserConfigUpdate::default(), &mut secrets)
            .map_err(|error| domain_error("settings_plugin_marketplace_install", error))
    }

    fn update_plugin(&self, id: &str) -> SettingsResult<()> {
        let manager = PluginManager::new(self.paths.data_root.clone());
        let parsed =
            PluginId::parse(id).map_err(|error| domain_error("settings_plugin_id", error))?;
        let mut secrets = SystemSecretStore;
        let source = manager
            .install_source(&parsed)
            .map_err(|error| domain_error("settings_plugin_source", error))?;
        match source {
            PluginInstallSource::Local { path } => manager
                .install_from_directory(
                    MaterializedPlugin {
                        id: parsed,
                        source_root: path.clone(),
                        source: Some(PluginInstallSource::Local { path }),
                    },
                    UserConfigUpdate::default(),
                    &mut secrets,
                )
                .map_err(|error| domain_error("settings_plugin_update", error)),
            PluginInstallSource::Marketplace { source } => {
                let cache_root = self.paths.data_root.join("plugins").join("marketplaces");
                let marketplace = plugins::refresh_marketplace_source(&source, &cache_root)
                    .map_err(|error| domain_error("settings_plugin_update_source", error))?;
                let materialized = plugins::resolve_marketplace_plugin_install_plan(
                    &marketplace,
                    Some(&parsed.plugin),
                    &cache_root,
                )
                .map_err(|error| domain_error("settings_plugin_update_plan", error))?;
                if !materialized.iter().any(|item| {
                    item.id
                        .to_string()
                        .eq_ignore_ascii_case(&parsed.to_string())
                }) {
                    return Err(unsupported(
                        "settings_plugin_update_missing",
                        "更新来源中找不到指定插件",
                    ));
                }
                manager
                    .install_from_directories(
                        materialized,
                        UserConfigUpdate::default(),
                        &mut secrets,
                    )
                    .map_err(|error| domain_error("settings_plugin_update", error))
            }
        }
    }

    fn uninstall_plugin(&self, id: &str) -> SettingsResult<()> {
        let manager = PluginManager::new(self.paths.data_root.clone());
        let parsed =
            PluginId::parse(id).map_err(|error| domain_error("settings_plugin_id", error))?;
        let mut secrets = SystemSecretStore;
        manager
            .uninstall(&parsed, &mut secrets)
            .map_err(|error| domain_error("settings_plugin_uninstall", error))?;
        Ok(())
    }

    fn configure_plugin(&self, id: &str, values: &BTreeMap<String, String>) -> SettingsResult<()> {
        let manager = PluginManager::new(self.paths.data_root.clone());
        let parsed =
            PluginId::parse(id).map_err(|error| domain_error("settings_plugin_id", error))?;
        let values = values
            .iter()
            .map(|(name, value)| {
                let parsed =
                    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()));
                (name.clone(), parsed)
            })
            .collect();
        let mut secrets = SystemSecretStore;
        manager
            .update_user_config(
                &parsed,
                crate::plugins::UserConfigUpdate {
                    values,
                    replace: false,
                },
                &mut secrets,
            )
            .map_err(|error| domain_error("settings_plugin_configure", error))
    }

    fn set_skill_enabled(&self, id: &str, enabled: bool) -> SettingsResult<()> {
        let project = self.current_project()?;
        let skills = self.skills(project.as_deref())?;
        let skill = skills
            .iter()
            .find(|skill| skill.id == id || skill.name.eq_ignore_ascii_case(id))
            .ok_or_else(|| unsupported("settings_skill_not_found", "找不到指定 Skill"))?;
        update_disabled_skill_state(&self.paths.data_root, skill, enabled)
    }

    fn copy_skill_to_common(&self, id: &str) -> SettingsResult<()> {
        let project = self.current_project()?;
        let skills = self.skills(project.as_deref())?;
        let skill = skills
            .iter()
            .find(|skill| skill.id == id || skill.name.eq_ignore_ascii_case(id))
            .ok_or_else(|| unsupported("settings_skill_not_found", "找不到指定 Skill"))?;
        if skill.scope.starts_with("plugin:") {
            return Err(unsupported(
                "settings_skill_copy",
                "插件 Skill 不能复制为用户 Skill",
            ));
        }
        let source = skill
            .path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| unsupported("settings_skill_path", "Skill 路径不可用"))?;
        let source_dir = source
            .parent()
            .ok_or_else(|| unsupported("settings_skill_path", "Skill 路径不可用"))?;
        let name = source_dir
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| valid_file_name(value))
            .ok_or_else(|| unsupported("settings_skill_name", "Skill 名称无效"))?;
        let target = self.paths.data_root.join("skills").join(name);
        if target.exists() {
            return Err(unsupported("settings_skill_exists", "通用 Skill 已存在"));
        }
        copy_skill_tree(source_dir, &target)
    }

    fn save_skill(&self, skill: &SkillSummary) -> SettingsResult<()> {
        let (scope, name) = skill
            .id
            .split_once(':')
            .ok_or_else(|| unsupported("settings_skill_id", "Skill 标识必须包含作用域"))?;
        if !matches!(scope, "global" | "project") || !valid_file_name(name) {
            return Err(unsupported("settings_skill_id", "只能修改全局或项目 Skill"));
        }
        if skill.scope.starts_with("plugin:") || scope == "plugin" {
            return Err(unsupported(
                "settings_skill_write",
                "插件 Skill 不能直接编辑",
            ));
        }
        let content = skill
            .content
            .as_deref()
            .ok_or_else(|| unsupported("settings_skill_content", "Skill 内容不能为空"))?;
        if content.is_empty() || content.len() as u64 > MAX_MEMORY_BYTES {
            return Err(unsupported("settings_skill_size", "Skill 内容超过大小限制"));
        }
        let project = self.current_project()?;
        let root = match scope {
            "global" => self.paths.data_root.join("skills"),
            "project" => project
                .ok_or_else(|| unsupported("settings_project_required", "项目 Skill 需要当前项目"))?
                .join(".agents")
                .join("skills"),
            _ => unreachable!(),
        };
        let directory = root.join(name);
        fs::create_dir_all(&directory)
            .map_err(|error| domain_error("settings_skill_write", error))?;
        storage::atomic_write_private(&directory.join("SKILL.md"), content.as_bytes())
            .map_err(|error| domain_error("settings_skill_write", error))
    }

    fn delete_skill(&self, id: &str) -> SettingsResult<()> {
        let project = self.current_project()?;
        let skills = self.skills(project.as_deref())?;
        let skill = skills
            .iter()
            .find(|skill| skill.id == id || skill.name.eq_ignore_ascii_case(id))
            .ok_or_else(|| unsupported("settings_skill_not_found", "找不到指定 Skill"))?;
        if skill.scope.starts_with("plugin:") {
            return Err(unsupported(
                "settings_skill_delete",
                "插件 Skill 必须随插件卸载",
            ));
        }
        let path = skill
            .path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| unsupported("settings_skill_path", "Skill 路径不可用"))?;
        let directory = path
            .parent()
            .ok_or_else(|| unsupported("settings_skill_path", "Skill 路径不可用"))?;
        remove_directory_tree(directory, "Skill")?;
        update_disabled_skill_state(&self.paths.data_root, skill, true)
    }
}

fn read_disabled_skills(root: &Path) -> SettingsResult<BTreeSet<String>> {
    let Some(value) = read_optional_json(&root.join("ui-presentation.json"), "技能状态")?
    else {
        return Ok(BTreeSet::new());
    };
    let values = value
        .get("settings")
        .and_then(Value::as_object)
        .and_then(|settings| settings.get("skills"))
        .and_then(Value::as_object)
        .and_then(|skills| skills.get("disabled"))
        .and_then(Value::as_array);
    Ok(values
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect())
}

fn update_disabled_skill_state(
    root: &Path,
    skill: &SkillSummary,
    enabled: bool,
) -> SettingsResult<()> {
    let path = root.join("ui-presentation.json");
    let mut value = read_optional_json(&path, "技能状态")?.unwrap_or_else(|| {
        json!({
            "schema": "keencode/ui-presentation",
            "version": 1,
            "settings": {},
            "projects": {},
            "threads": {},
            "spaces": {}
        })
    });
    let object = value
        .as_object_mut()
        .ok_or_else(|| unsupported("settings_skill_state", "技能状态必须是对象"))?;
    if object.get("schema").and_then(Value::as_str) != Some("keencode/ui-presentation")
        || object.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(unsupported(
            "settings_skill_state",
            "技能状态 schema 不受支持",
        ));
    }
    let settings = object
        .entry("settings")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| unsupported("settings_skill_state", "技能 settings 必须是对象"))?;
    let skills = settings
        .entry("skills")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| unsupported("settings_skill_state", "技能 settings.skills 必须是对象"))?;
    let disabled = skills
        .entry("disabled")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| unsupported("settings_skill_state", "技能 disabled 必须是数组"))?;
    let names = [skill.id.as_str(), skill.name.as_str()];
    disabled.retain(|value| {
        value
            .as_str()
            .is_none_or(|stored| !names.iter().any(|name| stored.eq_ignore_ascii_case(name)))
    });
    if !enabled {
        disabled.push(Value::String(skill.id.clone()));
    }
    write_json(&path, &value, "技能状态")
}

impl NativeSettingsRuntimeDomain {
    fn read_memory(&self, workspace_id: &str, file_name: &str) -> SettingsResult<MemoryFile> {
        ensure_global_memory_scope(workspace_id)?;
        let file = self
            .memory_service
            .read_file(file_name)
            .map_err(|error| domain_error("settings_memory_read", error))?;
        Ok(MemoryFile {
            workspace_id: "global".to_owned(),
            file_name: file.file_name,
            content: file.content,
            updated_at_ms: file.updated_at_ms,
        })
    }

    fn delete_memory(&self, workspace_id: &str, file_name: &str) -> SettingsResult<()> {
        ensure_global_memory_scope(workspace_id)?;
        self.memory_service
            .delete_file(file_name)
            .map_err(|error| domain_error("settings_memory_delete", error))
    }

    fn save_memory(&self, memory: &MemoryFile) -> SettingsResult<()> {
        ensure_global_memory_scope(&memory.workspace_id)?;
        self.memory_service
            .write_file(&memory.file_name, &memory.content)
            .map(|_| ())
            .map_err(|error| domain_error("settings_memory_write", error))
    }
}

fn ensure_global_memory_scope(workspace_id: &str) -> SettingsResult<()> {
    if workspace_id == "global" {
        Ok(())
    } else {
        Err(unsupported(
            "settings_memory_scope",
            "当前设置只允许操作全局 Memory",
        ))
    }
}

impl NativeSettingsRuntimeDomain {
    fn goal_state(
        &self,
        _session_id: &str,
        session: &keencode_runtime::RuntimeSession,
    ) -> SettingsResult<(Option<GoalState>, u64)> {
        let state =
            PersistentAgentState::open_with_goal_root(session.clone(), self.runtime.storage_root())
                .map_err(|error| domain_error("settings_goal_open", error))?;
        let snapshot = state
            .goal_snapshot()
            .map_err(|error| domain_error("settings_goal_read", error))?;
        let active_root_turn_id = self.active_root_turn_id(session)?;
        let revision = snapshot.revision;
        Ok((
            snapshot.goal.map(|goal| GoalState {
                goal_id: goal.id,
                title: goal.title,
                status: goal_status_name(goal.status).to_owned(),
                detail: goal.description.or(Some(goal.objective)),
                revision,
                active_root_turn_id,
            }),
            revision,
        ))
    }

    /// 只投影 Journal 中仍处于 Running 的根 Turn；子 Agent 的运行不会让 Goal
    /// 终态按钮保持禁用，UI 只能依据这个权威活动屏障做出判断。
    fn active_root_turn_id(
        &self,
        session: &keencode_runtime::RuntimeSession,
    ) -> SettingsResult<Option<String>> {
        let snapshot = session
            .snapshot()
            .map_err(|error| domain_error("settings_goal_turns", error))?;
        Ok(snapshot
            .state
            .turns
            .values()
            .find(|turn| {
                turn.source_agent_id.as_str() == ROOT_AGENT_ID
                    && turn.root_turn_id == turn.turn_id
                    && turn.status == TurnStatus::Running
            })
            .map(|turn| turn.turn_id.as_str().to_owned()))
    }

    fn goal_context(
        &self,
        goal_id: &str,
        expected_revision: u64,
    ) -> SettingsResult<(
        String,
        keencode_runtime::RuntimeSession,
        PersistentAgentState,
        keencode_agent::GoalSnapshot,
    )> {
        let Some((session_id, session)) = self.current_session()? else {
            return Err(unsupported(
                "settings_goal_session",
                "当前没有可操作的 Session",
            ));
        };
        let state =
            PersistentAgentState::open_with_goal_root(session.clone(), self.runtime.storage_root())
                .map_err(|error| domain_error("settings_goal_open", error))?;
        let current = state
            .goal_snapshot()
            .map_err(|error| domain_error("settings_goal_read", error))?;
        if current.revision != expected_revision {
            return Err(unsupported(
                "settings_goal_conflict",
                "Goal 修订已变化，请重新加载",
            ));
        }
        if current.goal.as_ref().is_none_or(|goal| goal.id != goal_id) {
            return Err(unsupported("settings_goal_id", "Goal 标识已变化"));
        }
        Ok((session_id, session, state, current))
    }

    fn ensure_goal_transition_barrier(
        &self,
        session: &keencode_runtime::RuntimeSession,
    ) -> SettingsResult<()> {
        if self.active_root_turn_id(session)?.is_some() {
            return Err(unsupported(
                "settings_goal_turn_active",
                "当前 root Turn 仍在运行，请先取消或等待其结束",
            ));
        }
        Ok(())
    }

    fn set_goal(
        &self,
        goal_id: &str,
        title: &str,
        detail: Option<String>,
        expected_revision: u64,
    ) -> SettingsResult<()> {
        let Some((session_id, session)) = self.current_session()? else {
            return Err(unsupported(
                "settings_goal_session",
                "当前没有可修改的 Session",
            ));
        };
        let state =
            PersistentAgentState::open_with_goal_root(session.clone(), self.runtime.storage_root())
                .map_err(|error| domain_error("settings_goal_open", error))?;
        let current = state
            .goal_snapshot()
            .map_err(|error| domain_error("settings_goal_read", error))?;
        if current.revision != expected_revision {
            return Err(unsupported(
                "settings_goal_conflict",
                "Goal 修订已变化，请重新加载",
            ));
        }
        let change = if let Some(goal) = current.goal {
            if !goal_id.is_empty() && goal.id != goal_id {
                return Err(unsupported("settings_goal_id", "Goal 标识已变化"));
            }
            state
                .update_goal(
                    &format!("settings-goal-update-{}-{}", session_id, expected_revision),
                    GoalPatch {
                        title: Some(title.to_owned()),
                        description: Some(detail),
                        ..GoalPatch::default()
                    },
                )
                .map_err(|error| domain_error("settings_goal_update", error))?
        } else {
            let objective = detail.unwrap_or_default();
            state
                .create_goal(
                    &format!("settings-goal-create-{}-{}", session_id, expected_revision),
                    GoalDraft {
                        title: title.to_owned(),
                        objective,
                        description: None,
                        token_budget: None,
                        progress_percent: None,
                    },
                )
                .map_err(|error| domain_error("settings_goal_create", error))?
        };
        self.runtime.publish_goal_changed(
            &session_id,
            change.current.goal.as_ref().map(|goal| goal.id.clone()),
            change.current.revision,
            change
                .current
                .goal
                .as_ref()
                .map(|goal| goal_status_name(goal.status).to_owned()),
        );
        Ok(())
    }

    fn resume_goal(&self, goal_id: &str, expected_revision: u64) -> SettingsResult<()> {
        let (_session_id, session, _state, current) =
            self.goal_context(goal_id, expected_revision)?;
        let goal = current
            .goal
            .as_ref()
            .expect("goal_context 已确认 Goal 存在");
        if goal.status.is_terminal() {
            return Err(unsupported(
                "settings_goal_terminal",
                "终态 Goal 不能恢复运行",
            ));
        }
        if self.active_root_turn_id(&session)?.is_some() {
            return Err(unsupported(
                "settings_goal_turn_active",
                "当前 root Turn 已在运行",
            ));
        }
        // Resume 需要在共享 Tokio Runtime 上真正启动续跑；NativeServices 负责
        // 持有执行器并调用 AgentRuntime::resume_session_goal，避免在设置域内伪造状态。
        self.runtime_port.execute(SettingsCommand::ResumeGoal {
            goal_id: goal_id.to_owned(),
            expected_revision,
        })?;
        Ok(())
    }

    fn pause_goal(&self, goal_id: &str, expected_revision: u64) -> SettingsResult<()> {
        let (session_id, _session, _state, current) =
            self.goal_context(goal_id, expected_revision)?;
        if current
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status.is_terminal())
        {
            return Err(unsupported("settings_goal_terminal", "终态 Goal 不能暂停"));
        }
        let operation_id = format!("settings-goal-pause-{session_id}-{expected_revision}");
        let _change = self
            .runtime
            .pause_session_goal(&session_id, &operation_id, expected_revision)
            .map_err(|error| domain_error("settings_goal_pause", error))?;
        Ok(())
    }

    fn complete_goal(
        &self,
        goal_id: &str,
        expected_revision: u64,
        evidence: &str,
    ) -> SettingsResult<()> {
        let (session_id, session, state, _) = self.goal_context(goal_id, expected_revision)?;
        self.ensure_goal_transition_barrier(&session)?;
        if evidence.trim().is_empty() {
            return Err(unsupported("settings_goal_evidence", "完成证据不能为空"));
        }
        let operation_id = format!("settings-goal-complete-{session_id}-{expected_revision}");
        let change = state
            .transition_goal_if_revision(
                &operation_id,
                expected_revision,
                goal_id,
                GoalTransition {
                    status: GoalStatus::Completed,
                    blocked_reason: None,
                    completion_evidence: Some(evidence.to_owned()),
                },
            )
            .map_err(|error| domain_error("settings_goal_complete", error))?
            .ok_or_else(|| unsupported("settings_goal_conflict", "Goal 修订已变化，请重新加载"))?;
        self.runtime.publish_goal_changed(
            &session_id,
            change.current.goal.as_ref().map(|goal| goal.id.clone()),
            change.current.revision,
            change
                .current
                .goal
                .as_ref()
                .map(|goal| goal_status_name(goal.status).to_owned()),
        );
        Ok(())
    }

    fn block_goal(
        &self,
        goal_id: &str,
        expected_revision: u64,
        reason: &str,
    ) -> SettingsResult<()> {
        let (session_id, session, state, _current) =
            self.goal_context(goal_id, expected_revision)?;
        self.ensure_goal_transition_barrier(&session)?;
        if reason.trim().is_empty() {
            return Err(unsupported(
                "settings_goal_block_reason",
                "阻塞原因不能为空",
            ));
        }
        let operation_id = format!("settings-goal-block-{session_id}-{expected_revision}");
        let change = state
            .transition_goal_if_revision(
                &operation_id,
                expected_revision,
                goal_id,
                GoalTransition {
                    status: GoalStatus::Blocked,
                    blocked_reason: Some(reason.to_owned()),
                    completion_evidence: None,
                },
            )
            .map_err(|error| domain_error("settings_goal_block", error))?
            .ok_or_else(|| unsupported("settings_goal_conflict", "Goal 修订已变化，请重新加载"))?;
        self.runtime.publish_goal_changed(
            &session_id,
            change.current.goal.as_ref().map(|goal| goal.id.clone()),
            change.current.revision,
            change
                .current
                .goal
                .as_ref()
                .map(|goal| goal_status_name(goal.status).to_owned()),
        );
        Ok(())
    }

    fn cancel_goal_run(
        &self,
        goal_id: &str,
        expected_revision: u64,
        turn_id: &str,
    ) -> SettingsResult<()> {
        let (session_id, session, _state, current) =
            self.goal_context(goal_id, expected_revision)?;
        if current
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status.is_terminal())
        {
            return Err(unsupported(
                "settings_goal_terminal",
                "终态 Goal 没有可取消的运行",
            ));
        }
        if self.active_root_turn_id(&session)?.as_deref() != Some(turn_id) {
            return Err(unsupported(
                "settings_goal_turn_id",
                "目标运行已变化，请重新加载",
            ));
        }
        self.runtime
            .cancel_turn(&session_id, turn_id)
            .map_err(|error| domain_error("settings_goal_cancel", error))?;
        Ok(())
    }

    fn clear_goal(&self, goal_id: &str, expected_revision: u64) -> SettingsResult<()> {
        let Some((session_id, session)) = self.current_session()? else {
            return Err(unsupported(
                "settings_goal_session",
                "当前没有可修改的 Session",
            ));
        };
        let state =
            PersistentAgentState::open_with_goal_root(session.clone(), self.runtime.storage_root())
                .map_err(|error| domain_error("settings_goal_open", error))?;
        let current = state
            .goal_snapshot()
            .map_err(|error| domain_error("settings_goal_read", error))?;
        if current.revision != expected_revision {
            return Err(unsupported(
                "settings_goal_conflict",
                "Goal 修订已变化，请重新加载",
            ));
        }
        if current.goal.as_ref().is_none_or(|goal| goal.id != goal_id) {
            return Err(unsupported("settings_goal_id", "Goal 标识已变化"));
        }
        if current
            .goal
            .as_ref()
            .is_none_or(|goal| !goal.status.is_terminal())
        {
            return Err(unsupported(
                "settings_goal_not_terminal",
                "只有已完成或已阻塞的 Goal 才能清除",
            ));
        }
        self.ensure_goal_transition_barrier(&session)?;
        let change = state
            .clear_goal_if_revision(
                &format!("settings-goal-clear-{}-{}", session_id, expected_revision),
                expected_revision,
            )
            .map_err(|error| domain_error("settings_goal_clear", error))?
            .ok_or_else(|| unsupported("settings_goal_conflict", "Goal 修订已变化，请重新加载"))?;
        self.runtime
            .publish_goal_changed(&session_id, None, change.current.revision, None);
        Ok(())
    }
}

fn goal_status_name(status: keencode_agent::GoalStatus) -> &'static str {
    match status {
        keencode_agent::GoalStatus::Active => "active",
        keencode_agent::GoalStatus::Paused => "paused",
        keencode_agent::GoalStatus::Completed => "completed",
        keencode_agent::GoalStatus::Blocked => "blocked",
    }
}

fn read_file_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("目标必须是普通文件".to_owned());
    }
    if metadata.len() > max_bytes {
        return Err("文件超过大小限制".to_owned());
    }
    fs::read(path).map_err(|error| error.to_string())
}

fn read_optional_json(path: &Path, label: &str) -> SettingsResult<Option<Value>> {
    let bytes = storage::read_private_bytes_bounded(path, MAX_SETTINGS_JSON_BYTES, label)
        .map_err(|error| domain_error("settings_file_read", error))?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .map(Some)
        .map_err(|error| domain_error("settings_file_parse", error))
}

fn write_json(path: &Path, value: &Value, label: &str) -> SettingsResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| domain_error("settings_file_encode", error))?;
    if bytes.len() as u64 > MAX_SETTINGS_JSON_BYTES {
        return Err(unsupported(
            "settings_file_size",
            format!("{label}超过大小限制"),
        ));
    }
    storage::atomic_write_private(path, &bytes)
        .map_err(|error| domain_error("settings_file_write", error))
}

fn update_mcp_document(
    path: &Path,
    name: &str,
    update: impl FnOnce(&mut Map<String, Value>),
) -> SettingsResult<()> {
    let Some(mut value) = read_optional_json(path, "MCP 配置")? else {
        return Err(unsupported("settings_mcp_not_found", "找不到 MCP 配置"));
    };
    let config = value
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.get_mut(name))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| unsupported("settings_mcp_not_found", "找不到 MCP Server"))?;
    update(config);
    write_json(path, &value, "MCP 配置")
}

fn looks_like_marketplace_source(path: &Path) -> bool {
    if path.is_file() {
        return path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("marketplace.json"));
    }
    path.is_dir() && path.join(plugins::MARKETPLACE_MANIFEST).is_file()
}

fn looks_like_marketplace_input(source: &str) -> bool {
    let source = source.trim();
    source.starts_with('{')
        || source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("github:")
        || source.starts_with("git:")
        || source.starts_with("npm:")
        || (source.matches('/').count() == 1
            && !source.starts_with('.')
            && !source.starts_with('/')
            && !source.starts_with('\\')
            && !source.starts_with('~')
            && !source.contains(char::is_whitespace))
}

fn mcp_id(scope: &str, name: &str) -> String {
    if scope == "global" {
        name.to_owned()
    } else {
        format!("{scope}:{name}")
    }
}

fn split_mcp_id(id: &str) -> (&'static str, &str) {
    id.strip_prefix("project:")
        .map_or(("global", id), |name| ("project", name))
}

fn copy_skill_tree(source: &Path, target: &Path) -> SettingsResult<()> {
    let metadata =
        fs::symlink_metadata(source).map_err(|error| domain_error("settings_skill_copy", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsupported("settings_skill_copy", "Skill 源目录无效"));
    }
    copy_directory_recursive(source, target)
}

fn copy_directory_recursive(source: &Path, target: &Path) -> SettingsResult<()> {
    fs::create_dir_all(target).map_err(|error| domain_error("settings_skill_copy", error))?;
    for entry in fs::read_dir(source).map_err(|error| domain_error("settings_skill_copy", error))? {
        let entry = entry.map_err(|error| domain_error("settings_skill_copy", error))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| domain_error("settings_skill_copy", error))?;
        if metadata.file_type().is_symlink() {
            return Err(unsupported(
                "settings_skill_copy",
                "Skill 目录不允许符号链接",
            ));
        }
        if metadata.is_dir() {
            copy_directory_recursive(&source_path, &target_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path)
                .map_err(|error| domain_error("settings_skill_copy", error))?;
        }
    }
    Ok(())
}

fn remove_directory_tree(path: &Path, label: &str) -> SettingsResult<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| domain_error("settings_remove", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsupported("settings_remove", format!("{label} 不是目录")));
    }
    fs::remove_dir_all(path).map_err(|error| domain_error("settings_remove", error))
}

fn remove_regular_file(path: &Path, label: &str) -> SettingsResult<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| domain_error("settings_remove", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(unsupported(
            "settings_remove",
            format!("{label} 不是普通文件"),
        ));
    }
    fs::remove_file(path).map_err(|error| domain_error("settings_remove", error))
}

fn valid_component(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 256
        && value == value.trim()
        && !value.chars().any(char::is_control)
}

fn valid_file_name(value: &str) -> bool {
    valid_component(value) && value != "." && value != ".." && !value.contains(['/', '\\', ':'])
}

fn domain_error(code: &str, error: impl std::fmt::Display) -> NativeSettingsError {
    NativeSettingsError::new(
        code,
        keencode_model::redact_error_secrets_bounded(&error.to_string(), 1_000),
    )
}

fn unsupported(code: &str, message: impl Into<String>) -> NativeSettingsError {
    NativeSettingsError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::{hook_bundle_digest, hook_summary, unsupported_hooks_from_inventory};
    use crate::native_hooks::{self, HookHandler as NativeHookHandler};
    use crate::native_paths::NativePaths;
    use crate::plugins::PluginInventory;
    use keencode_agent::HookPhase;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn test_hook_definition(
        scope: super::NativeHookScope,
        enabled: bool,
    ) -> super::NativeHookDefinition {
        super::NativeHookDefinition {
            id: "shared-hook".to_owned(),
            name: "Shared Hook".to_owned(),
            scope,
            phase: HookPhase::PreToolUse,
            matcher: None,
            handler: NativeHookHandler::Context {
                context: Some("test context".to_owned()),
                block: None,
                input: None,
                continue_turn: false,
            },
            enabled,
        }
    }

    #[test]
    fn plugin_summary_projection_preserves_unsupported_hook_events() {
        let inventory = PluginInventory {
            unsupported_hooks: vec!["FileChanged".to_owned(), "Notification".to_owned()],
            ..PluginInventory::default()
        };

        assert_eq!(
            unsupported_hooks_from_inventory(Some(&inventory)),
            vec!["FileChanged".to_owned(), "Notification".to_owned()]
        );
        assert!(unsupported_hooks_from_inventory(None).is_empty());
    }

    #[test]
    fn user_hook_trust_projection_tracks_pending_grant_revoke_and_scope() {
        let data_root = tempdir().unwrap();
        let project_root = tempdir().unwrap();
        let paths = NativePaths::from_data_root(data_root.path().join("keencode"));
        let project = project_root.path();
        let user_definition = test_hook_definition(super::NativeHookScope::User, true);

        native_hooks::upsert(&paths, user_definition.clone(), 0).unwrap();
        let pending_snapshot = native_hooks::load(&paths, Some(project)).unwrap();
        let pending_native = pending_snapshot
            .summaries
            .iter()
            .find(|summary| summary.id == user_definition.id)
            .unwrap();
        let pending = hook_summary(
            &user_definition,
            pending_native,
            Some(project),
            Some(&hook_bundle_digest(&pending_snapshot)),
            0,
        )
        .unwrap();
        assert_eq!(pending.scope, super::HookScope::User);
        assert_eq!(pending.trust_state, super::HookTrustState::PendingTrust);
        let workspace_identity = project.to_string_lossy().into_owned();
        assert_eq!(
            pending.workspace_identity.as_deref(),
            Some(workspace_identity.as_str())
        );
        assert!(pending.bundle_digest.is_some());
        assert_eq!(
            pending.hook_declaration_digest.as_deref(),
            Some(pending_native.digest.as_str())
        );

        let trusted_snapshot = native_hooks::set_trusted(
            &paths,
            project,
            super::NativeHookScope::User,
            &user_definition.id,
            true,
            pending_snapshot.trust_revision,
        )
        .unwrap();
        let trusted_native = trusted_snapshot
            .summaries
            .iter()
            .find(|summary| summary.id == user_definition.id)
            .unwrap();
        let trusted = hook_summary(
            &user_definition,
            trusted_native,
            Some(project),
            Some(&hook_bundle_digest(&trusted_snapshot)),
            0,
        )
        .unwrap();
        assert_eq!(
            trusted.trust_state,
            super::HookTrustState::TrustedPersistent
        );

        let disabled_definition = test_hook_definition(super::NativeHookScope::User, false);
        let disabled_snapshot = native_hooks::upsert(
            &paths,
            disabled_definition.clone(),
            trusted_snapshot.user_revision,
        )
        .unwrap();
        let disabled_trusted_snapshot = native_hooks::load(&paths, Some(project)).unwrap();
        let disabled_trusted_native = disabled_trusted_snapshot
            .summaries
            .iter()
            .find(|summary| summary.id == disabled_definition.id)
            .unwrap();
        let disabled_trusted = hook_summary(
            &disabled_definition,
            disabled_trusted_native,
            Some(project),
            Some(&hook_bundle_digest(&disabled_trusted_snapshot)),
            0,
        )
        .unwrap();
        assert_eq!(
            disabled_trusted.trust_state,
            super::HookTrustState::TrustedPersistent
        );

        let revoked_snapshot = native_hooks::set_trusted(
            &paths,
            project,
            super::NativeHookScope::User,
            &disabled_definition.id,
            false,
            disabled_snapshot.trust_revision,
        )
        .unwrap();
        let revoked_native = revoked_snapshot
            .summaries
            .iter()
            .find(|summary| summary.id == disabled_definition.id)
            .unwrap();
        let revoked = hook_summary(
            &disabled_definition,
            revoked_native,
            Some(project),
            Some(&hook_bundle_digest(&revoked_snapshot)),
            0,
        )
        .unwrap();
        assert_eq!(revoked.trust_state, super::HookTrustState::NotApplicable);

        let project_definition = test_hook_definition(
            super::NativeHookScope::Project {
                root: PathBuf::from(project),
            },
            true,
        );
        let project_revision = revoked_snapshot.project_revision.unwrap_or_default();
        let project_snapshot =
            native_hooks::upsert(&paths, project_definition.clone(), project_revision).unwrap();
        let project_native = project_snapshot
            .summaries
            .iter()
            .find(|summary| {
                summary.id == project_definition.id
                    && matches!(summary.scope, super::NativeHookScope::Project { .. })
            })
            .unwrap();
        let project_summary = hook_summary(
            &project_definition,
            project_native,
            Some(project),
            Some(&hook_bundle_digest(&project_snapshot)),
            1,
        )
        .unwrap();
        assert_eq!(project_summary.scope, super::HookScope::Project);
        assert_eq!(
            project_summary.trust_state,
            super::HookTrustState::PendingTrust
        );
        assert_ne!(
            trusted.hook_declaration_digest,
            project_summary.hook_declaration_digest
        );
    }
}
