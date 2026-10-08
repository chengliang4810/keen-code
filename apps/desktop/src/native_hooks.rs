//! Native Hook 定义、持久化和运行时准入。
//!
//! Hook 定义文件只描述用户或项目声明；是否允许当前项目执行由独立的 trust
//! 文件和当前执行 payload digest 决定。这样项目文件不能自行把命令标记为已信任，
//! 命令、参数、环境、项目根或其引用脚本变化时旧准入会自动失效。

use crate::agent_runtime::RuntimeExtensionDiagnostic;
use crate::native_extension_contributor::{
    HookSpec, attach_native_hook_admission, parse_event_hooks,
};
use crate::native_paths::NativePaths;
use crate::storage;
use fs2::FileExt;
use keencode_agent::HookPhase;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

const HOOKS_SCHEMA: &str = "keencode/native-hooks";
const HOOKS_VERSION: u32 = 1;
const TRUST_SCHEMA: &str = "keencode/native-hook-trust";
const TRUST_VERSION: u32 = 1;
const USER_HOOKS_FILE: &str = "native-hooks.json";
const PROJECT_HOOKS_DIRECTORY: &str = ".keencode";
const PROJECT_HOOKS_FILE: &str = "hooks.json";
const TRUST_FILE: &str = "hooks-trust.json";
const MAX_HOOKS_BYTES: u64 = 2 * 1024 * 1024;
const MAX_HOOK_COUNT: usize = 512;
const MAX_HOOK_ID_BYTES: usize = 128;
const MAX_HOOK_NAME_BYTES: usize = 256;
const MAX_MATCHER_BYTES: usize = 512;
const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 128;
const MAX_ENVIRONMENT_KEY_BYTES: usize = 256;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 32 * 1024;
const MAX_SCRIPT_BYTES: u64 = 8 * 1024 * 1024;

static HOOK_STORAGE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 用户配置或项目配置所属作用域。插件作用域仅用于只读摘要，不能通过 CRUD API 写入。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum HookScope {
    User,
    Project { root: PathBuf },
    Plugin { plugin_id: String },
}

impl HookScope {
    fn key(&self) -> String {
        match self {
            Self::User => "user".to_owned(),
            Self::Project { root } => format!("project:{}", root.to_string_lossy()),
            Self::Plugin { plugin_id } => format!("plugin:{plugin_id}"),
        }
    }

    fn namespace(&self, id: &str) -> String {
        match self {
            Self::User => format!("user:{id}"),
            Self::Project { root } => {
                format!("project:{}:{id}", project_fingerprint(root))
            }
            Self::Plugin { plugin_id } => format!("plugin:{plugin_id}:{id}"),
        }
    }
}

/// Native Hook 支持的两种严格处理器。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum HookHandler {
    /// 命令由现有 bounded command runner 执行；不会在此模块启动进程。
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        environment: BTreeMap<String, String>,
    },
    /// 声明式上下文动作，不启动外部进程。
    Context {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
        #[serde(default, skip_serializing_if = "is_false")]
        continue_turn: bool,
    },
}

/// 用户或项目 Hook 的持久化定义。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct HookDefinition {
    pub id: String,
    /// 仅用于设置页展示；不参与信任 digest。
    pub name: String,
    pub scope: HookScope,
    pub phase: HookPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub handler: HookHandler,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Hook 的只读设置投影。`pending` 为 true 时，运行时不会注册该 Hook。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookSummary {
    pub id: String,
    pub name: String,
    pub scope: HookScope,
    pub phase: HookPhase,
    pub matcher: Option<String>,
    pub kind: String,
    pub enabled: bool,
    pub trusted: bool,
    pub pending: bool,
    pub digest: String,
}

/// 设置域读取和写入 Hook 时使用的冻结投影。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookStoreSnapshot {
    pub hooks: Vec<HookDefinition>,
    pub summaries: Vec<HookSummary>,
    pub revision: u64,
    pub user_revision: u64,
    pub project_revision: Option<u64>,
    pub trust_revision: u64,
}

/// 候选缓存使用的 Hook 输入状态。`readable` 为 false 时只允许完成一次
/// fail-closed 重建，不能把读取异常当成可复用的稳定快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HookInputFingerprint {
    pub(crate) digest: String,
    pub(crate) readable: bool,
}

/// Native command Hook 在执行边界携带的当前准入证明。
///
/// `HookSpec` 仍保存候选代次中的命令快照；进程启动前会用这里的身份重新读取
/// 当前定义、enabled、trust 和可识别脚本摘要，避免 active Turn 沿用旧准入。
#[derive(Clone, Debug)]
pub(crate) struct NativeHookAdmission {
    pub(crate) paths: NativePaths,
    pub(crate) project_root: PathBuf,
    pub(crate) scope: HookScope,
    pub(crate) id: String,
    pub(crate) digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct HookDocument {
    schema: String,
    version: u32,
    revision: u64,
    hooks: Vec<HookDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TrustDocument {
    schema: String,
    version: u32,
    revision: u64,
    entries: Vec<TrustEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TrustEntry {
    project_fingerprint: String,
    scope_key: String,
    hook_id: String,
    admitted_digest: String,
}

impl Default for HookDocument {
    fn default() -> Self {
        Self {
            schema: HOOKS_SCHEMA.to_owned(),
            version: HOOKS_VERSION,
            revision: 0,
            hooks: Vec::new(),
        }
    }
}

impl Default for TrustDocument {
    fn default() -> Self {
        Self {
            schema: TRUST_SCHEMA.to_owned(),
            version: TRUST_VERSION,
            revision: 0,
            entries: Vec::new(),
        }
    }
}

/// 读取当前用户和指定项目的 Hook 定义及信任投影。
pub(crate) fn load(
    paths: &NativePaths,
    project_root: Option<&Path>,
) -> Result<HookStoreSnapshot, String> {
    let _guard = storage_lock(paths)?;
    load_locked(paths, project_root)
}

/// 新增或替换一个用户/项目 Hook。`expected_revision` 是目标作用域文件的 revision。
pub(crate) fn upsert(
    paths: &NativePaths,
    mut definition: HookDefinition,
    expected_revision: u64,
) -> Result<HookStoreSnapshot, String> {
    let _guard = storage_lock(paths)?;
    normalize_definition(&mut definition)?;
    let (path, expected_scope, project_root) = definition_target(paths, &definition.scope)?;
    let mut document = read_hook_document(&path)?;
    validate_hook_document(&document, expected_scope.as_ref(), project_root.as_deref())?;
    if document.revision != expected_revision {
        return Err(revision_conflict(document.revision, expected_revision));
    }
    if let Some(existing) = document
        .hooks
        .iter_mut()
        .find(|hook| hook.id == definition.id)
    {
        *existing = definition;
    } else {
        if document.hooks.len() >= MAX_HOOK_COUNT {
            return Err("Hook 数量超过限制".to_owned());
        }
        document.hooks.push(definition);
    }
    document.hooks.sort_by(|left, right| left.id.cmp(&right.id));
    document.revision = next_revision(document.revision)?;
    write_hook_document(&path, &document)?;
    drop(_guard);
    load(paths, project_root.as_deref())
}

/// 删除一个用户/项目 Hook。插件 Hook 不允许通过此 API 修改。
pub(crate) fn delete(
    paths: &NativePaths,
    scope: HookScope,
    id: &str,
    expected_revision: u64,
) -> Result<HookStoreSnapshot, String> {
    let _guard = storage_lock(paths)?;
    let (path, expected_scope, project_root) = definition_target(paths, &scope)?;
    let Some(normalized_scope) = expected_scope.as_ref() else {
        return Err("Hook 作用域无效".to_owned());
    };
    let mut document = read_hook_document(&path)?;
    validate_hook_document(&document, expected_scope.as_ref(), project_root.as_deref())?;
    if document.revision != expected_revision {
        return Err(revision_conflict(document.revision, expected_revision));
    }
    let before = document.hooks.len();
    document.hooks.retain(|hook| hook.id != id);
    if document.hooks.len() == before {
        return Err(format!("找不到 Hook：{id}"));
    }

    let trust_path = trust_path(paths)?;
    let mut trust = read_trust_document(&trust_path)?;
    validate_trust_document(&trust)?;
    // User scope key 不含项目指纹，所以删除用户 Hook 时要撤销所有项目的准入；
    // Project scope key 已包含规范化项目路径，可精确保留其他项目的同名 Hook。
    let scope_key = normalized_scope.key();
    let trust_before = trust.entries.len();
    trust
        .entries
        .retain(|entry| !(entry.scope_key == scope_key && entry.hook_id == id));
    if trust.entries.len() != trust_before {
        trust.entries.sort_by(|left, right| {
            left.project_fingerprint
                .cmp(&right.project_fingerprint)
                .then_with(|| left.scope_key.cmp(&right.scope_key))
                .then_with(|| left.hook_id.cmp(&right.hook_id))
        });
        trust.revision = next_revision(trust.revision)?;
        // Trust 与 Hook 定义分属两个文件；先持久化撤销，后写定义。若定义写入失败，
        // 旧定义仍会保留，但必须重新获得当前项目准入，避免留下可执行的旧授权。
        write_trust_document(&trust_path, &trust)?;
    }
    document.revision = next_revision(document.revision)?;
    write_hook_document(&path, &document)?;
    drop(_guard);
    load(paths, project_root.as_deref())
}

/// 修改一个用户/项目 Hook 的 enabled 状态。
pub(crate) fn set_enabled(
    paths: &NativePaths,
    scope: HookScope,
    id: &str,
    enabled: bool,
    expected_revision: u64,
) -> Result<HookStoreSnapshot, String> {
    let _guard = storage_lock(paths)?;
    let (path, expected_scope, project_root) = definition_target(paths, &scope)?;
    let mut document = read_hook_document(&path)?;
    validate_hook_document(&document, expected_scope.as_ref(), project_root.as_deref())?;
    if document.revision != expected_revision {
        return Err(revision_conflict(document.revision, expected_revision));
    }
    let hook = document
        .hooks
        .iter_mut()
        .find(|hook| hook.id == id)
        .ok_or_else(|| format!("找不到 Hook：{id}"))?;
    if hook.enabled != enabled {
        hook.enabled = enabled;
        document.revision = next_revision(document.revision)?;
        write_hook_document(&path, &document)?;
    }
    drop(_guard);
    load(paths, project_root.as_deref())
}

/// 为当前项目批准或撤销一个 Hook 的当前执行 payload digest。
pub(crate) fn set_trusted(
    paths: &NativePaths,
    project_root: &Path,
    scope: HookScope,
    id: &str,
    trusted: bool,
    expected_revision: u64,
) -> Result<HookStoreSnapshot, String> {
    let _guard = storage_lock(paths)?;
    let project_root = canonical_project_root(project_root)?;
    let scope = normalize_scope(scope)?;
    if matches!(&scope, HookScope::Plugin { .. }) {
        return Err("插件 Hook 的启用状态由插件管理器控制".to_owned());
    }
    if let HookScope::Project { root } = &scope
        && root != &project_root
    {
        return Err("Hook 作用域与当前项目根不一致".to_owned());
    }
    let definition = read_definition_for_scope(paths, &scope, id)?;
    let digest = execution_payload_digest(&definition, &project_root)?;
    let path = trust_path(paths)?;
    let mut document = read_trust_document(&path)?;
    validate_trust_document(&document)?;
    if document.revision != expected_revision {
        return Err(revision_conflict(document.revision, expected_revision));
    }
    let project_fingerprint = project_fingerprint(&project_root);
    let scope_key = scope.key();
    let mut changed = false;
    if trusted {
        let entry = TrustEntry {
            project_fingerprint: project_fingerprint.clone(),
            scope_key: scope_key.clone(),
            hook_id: id.to_owned(),
            admitted_digest: digest,
        };
        if let Some(existing) = document.entries.iter_mut().find(|existing| {
            existing.project_fingerprint == project_fingerprint
                && existing.scope_key == scope_key
                && existing.hook_id == id
        }) {
            if existing.admitted_digest != entry.admitted_digest {
                *existing = entry;
                changed = true;
            }
        } else {
            document.entries.push(entry);
            changed = true;
        }
    } else {
        let before = document.entries.len();
        document.entries.retain(|entry| {
            !(entry.project_fingerprint == project_fingerprint
                && entry.scope_key == scope_key
                && entry.hook_id == id)
        });
        changed = before != document.entries.len();
    }
    if changed {
        document.entries.sort_by(|left, right| {
            left.project_fingerprint
                .cmp(&right.project_fingerprint)
                .then_with(|| left.scope_key.cmp(&right.scope_key))
                .then_with(|| left.hook_id.cmp(&right.hook_id))
        });
        document.revision = next_revision(document.revision)?;
        write_trust_document(&path, &document)?;
    }
    drop(_guard);
    load(paths, Some(&project_root))
}

/// 为当前项目读取并筛选可注册的用户/项目 Hook。
///
/// 未启用、定义无效或 digest 尚未准入的 Hook 只产生诊断，不会进入
/// `NativeExtensionInputs::hooks`，因而不能影响 prompt、工具决策或启动命令。
pub(crate) fn runtime_hooks(
    paths: &NativePaths,
    project_root: &Path,
) -> (Vec<HookSpec>, Vec<RuntimeExtensionDiagnostic>) {
    let project_root = match canonical_project_root(project_root) {
        Ok(root) => root,
        Err(error) => {
            return (
                Vec::new(),
                vec![hook_diagnostic("<project>", "hook_project_invalid", error)],
            );
        }
    };
    let snapshot = match load(paths, Some(&project_root)) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return (
                Vec::new(),
                vec![hook_diagnostic(
                    "<config>",
                    "hook_config_invalid",
                    bounded_text(&error),
                )],
            );
        }
    };
    let mut hooks = Vec::new();
    let mut diagnostics = Vec::new();
    for definition in &snapshot.hooks {
        if !definition.enabled {
            continue;
        }
        let Some(summary) = snapshot
            .summaries
            .iter()
            .find(|summary| summary.scope == definition.scope && summary.id == definition.id)
        else {
            diagnostics.push(hook_diagnostic(
                &definition.id,
                "hook_summary_missing",
                "Hook 摘要不存在，已跳过执行",
            ));
            continue;
        };
        if !summary.trusted {
            diagnostics.push(hook_diagnostic(
                &definition.id,
                "hook_pending_trust",
                "Hook 尚未批准当前执行摘要",
            ));
            continue;
        }
        let namespace = definition.scope.namespace(&definition.id);
        let (value, environment) = definition_event_value(definition);
        match parse_event_hooks(Some(&value), namespace, &project_root, &environment) {
            Ok(mut parsed) => {
                attach_native_hook_admission(
                    &mut parsed,
                    NativeHookAdmission {
                        paths: paths.clone(),
                        project_root: project_root.clone(),
                        scope: definition.scope.clone(),
                        id: definition.id.clone(),
                        digest: summary.digest.clone(),
                    },
                );
                hooks.append(&mut parsed);
            }
            Err(error) => diagnostics.push(hook_diagnostic(
                &definition.id,
                "hook_definition_invalid",
                bounded_text(&error),
            )),
        }
    }
    (hooks, diagnostics)
}

/// 在命令 Hook 启动进程前重新确认当前定义仍与候选准入一致。
pub(crate) fn validate_runtime_admission(admission: &NativeHookAdmission) -> Result<(), String> {
    let project_root = canonical_project_root(&admission.project_root)?;
    let snapshot = load(&admission.paths, Some(&project_root))?;
    let Some((definition, summary)) =
        snapshot
            .hooks
            .iter()
            .zip(snapshot.summaries.iter())
            .find(|(definition, summary)| {
                definition.id == admission.id
                    && definition.scope == admission.scope
                    && summary.id == admission.id
                    && summary.scope == admission.scope
            })
    else {
        return Err("Native Hook 定义已不存在".to_owned());
    };
    if !definition.enabled {
        return Err("Native Hook 已禁用".to_owned());
    }
    if !summary.trusted {
        return Err("Native Hook 当前未获信任".to_owned());
    }
    if summary.digest != admission.digest {
        return Err("Native Hook 执行摘要已变化".to_owned());
    }
    Ok(())
}

/// 返回当前项目 Hook 候选依赖的所有本地输入状态。
///
/// 配置文件本身即使 JSON 无效，也会把原始字节纳入摘要并标记为不可缓存，
/// 这样外部修复文件后下一次请求仍会重新构建候选。可识别的脚本路径还会
/// 逐一纳入内容摘要，避免脚本在 trust 之后被外部替换而命中旧候选。
pub(crate) fn input_fingerprint(paths: &NativePaths, project_root: &Path) -> HookInputFingerprint {
    let mut readable = true;
    let mut payload = Map::new();
    let root = match canonical_project_root(project_root) {
        Ok(root) => root,
        Err(error) => {
            payload.insert(
                "projectRoot".to_owned(),
                json!({"state": "error", "message": bounded_text(&error)}),
            );
            return HookInputFingerprint {
                digest: digest_json(&Value::Object(payload)),
                readable: false,
            };
        }
    };
    payload.insert(
        "projectRoot".to_owned(),
        Value::String(root.to_string_lossy().into_owned()),
    );

    let mut scripts = BTreeMap::<String, Value>::new();
    let user_path = match user_hooks_path(paths) {
        Ok(path) => path,
        Err(error) => {
            readable = false;
            payload.insert(
                "userHooks".to_owned(),
                json!({"state": "error", "message": bounded_text(&error)}),
            );
            PathBuf::new()
        }
    };
    if !user_path.as_os_str().is_empty() {
        let (state, bytes, state_readable) = input_file_state(&user_path, "Hook 配置");
        readable &= state_readable;
        payload.insert("userHooks".to_owned(), state);
        collect_document_scripts(
            bytes.as_deref(),
            &HookScope::User,
            &root,
            &mut scripts,
            &mut readable,
        );
    }

    let project_path = match project_hooks_path(&root) {
        Ok(path) => path,
        Err(error) => {
            readable = false;
            payload.insert(
                "projectHooks".to_owned(),
                json!({"state": "error", "message": bounded_text(&error)}),
            );
            PathBuf::new()
        }
    };
    if !project_path.as_os_str().is_empty() {
        let (state, bytes, state_readable) = input_file_state(&project_path, "Hook 配置");
        readable &= state_readable;
        payload.insert("projectHooks".to_owned(), state);
        collect_document_scripts(
            bytes.as_deref(),
            &HookScope::Project { root: root.clone() },
            &root,
            &mut scripts,
            &mut readable,
        );
    }

    match trust_path(paths) {
        Ok(path) => {
            let (state, bytes, state_readable) = input_file_state(&path, "Hook trust");
            readable &= state_readable;
            payload.insert("trust".to_owned(), state);
            if let Some(bytes) = bytes {
                let parsed = serde_json::from_slice::<TrustDocument>(
                    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes),
                )
                .map_err(|error| error.to_string())
                .and_then(|document| {
                    validate_trust_document(&document).map_err(|error| error.to_owned())
                });
                if parsed.is_err() {
                    readable = false;
                }
            }
        }
        Err(error) => {
            readable = false;
            payload.insert(
                "trust".to_owned(),
                json!({"state": "error", "message": bounded_text(&error)}),
            );
        }
    }

    payload.insert(
        "scripts".to_owned(),
        Value::Object(scripts.into_iter().collect()),
    );
    HookInputFingerprint {
        digest: digest_json(&Value::Object(payload)),
        readable,
    }
}

fn input_file_state(path: &Path, label: &str) -> (Value, Option<Vec<u8>>, bool) {
    match storage::read_private_bytes_bounded(path, MAX_HOOKS_BYTES, label) {
        Ok(Some(bytes)) => (
            json!({
                "state": "present",
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
            }),
            Some(bytes),
            true,
        ),
        Ok(None) => (json!({"state": "missing"}), None, true),
        Err(error) => (
            json!({
                "state": "error",
                "message": bounded_text(&error.to_string()),
            }),
            None,
            false,
        ),
    }
}

fn collect_document_scripts(
    bytes: Option<&[u8]>,
    expected_scope: &HookScope,
    project_root: &Path,
    scripts: &mut BTreeMap<String, Value>,
    readable: &mut bool,
) {
    let Some(bytes) = bytes else {
        return;
    };
    let document = match serde_json::from_slice::<HookDocument>(
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes),
    ) {
        Ok(document) => document,
        Err(_) => {
            *readable = false;
            return;
        }
    };
    if validate_hook_document(&document, Some(expected_scope), Some(project_root)).is_err() {
        *readable = false;
        return;
    }
    for definition in &document.hooks {
        for script in referenced_script_paths(definition, project_root) {
            let key = script.to_string_lossy().into_owned();
            let (state, state_readable) = script_input_state(&script);
            *readable &= state_readable;
            scripts.insert(key, state);
        }
    }
}

fn script_input_state(path: &Path) -> (Value, bool) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (json!({"state": "missing"}), true);
        }
        Err(error) => {
            return (
                json!({
                    "state": "error",
                    "message": bounded_text(&error.to_string()),
                }),
                false,
            );
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return (
            json!({"state": "error", "message": "Hook 脚本必须是普通文件"}),
            false,
        );
    }
    if metadata.len() > MAX_SCRIPT_BYTES {
        return (
            json!({"state": "error", "message": "Hook 脚本超过大小限制"}),
            false,
        );
    }
    let canonical = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => {
            return (
                json!({
                    "state": "error",
                    "message": bounded_text(&error.to_string()),
                }),
                false,
            );
        }
    };
    match storage::read_private_bytes_bounded(&canonical, MAX_SCRIPT_BYTES, "Hook 脚本") {
        Ok(Some(bytes)) => (
            json!({
                "state": "present",
                "path": canonical.to_string_lossy(),
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
            }),
            true,
        ),
        Ok(None) => (json!({"state": "missing"}), true),
        Err(error) => (
            json!({
                "state": "error",
                "message": bounded_text(&error.to_string()),
            }),
            false,
        ),
    }
}

fn load_locked(
    paths: &NativePaths,
    project_root: Option<&Path>,
) -> Result<HookStoreSnapshot, String> {
    let project_root = project_root.map(canonical_project_root).transpose()?;
    let user_path = user_hooks_path(paths)?;
    let user_document = read_hook_document(&user_path)?;
    validate_hook_document(
        &user_document,
        Some(&HookScope::User),
        project_root.as_deref(),
    )?;

    let (project_document, project_root) = if let Some(project_root) = project_root {
        let path = project_hooks_path(&project_root)?;
        let document = read_hook_document(&path)?;
        validate_hook_document(
            &document,
            Some(&HookScope::Project {
                root: project_root.clone(),
            }),
            Some(&project_root),
        )?;
        (Some(document), Some(project_root))
    } else {
        (None, None)
    };
    let trust_path = trust_path(paths)?;
    let trust = read_trust_document(&trust_path)?;
    validate_trust_document(&trust)?;

    let mut hooks = user_document.hooks;
    if let Some(project_document) = &project_document {
        hooks.extend(project_document.hooks.clone());
    }
    hooks.sort_by(|left, right| {
        left.scope
            .key()
            .cmp(&right.scope.key())
            .then_with(|| left.id.cmp(&right.id))
    });
    let summaries = hooks
        .iter()
        .map(|definition| summary_for(definition, project_root.as_deref(), &trust))
        .collect::<Result<Vec<_>, _>>()?;
    let project_revision = project_document.as_ref().map(|document| document.revision);
    Ok(HookStoreSnapshot {
        hooks,
        summaries,
        revision: user_document
            .revision
            .max(project_revision.unwrap_or_default()),
        user_revision: user_document.revision,
        project_revision,
        trust_revision: trust.revision,
    })
}

fn summary_for(
    definition: &HookDefinition,
    project_root: Option<&Path>,
    trust: &TrustDocument,
) -> Result<HookSummary, String> {
    let (digest, trusted) = if let Some(project_root) = project_root {
        let digest = execution_payload_digest(definition, project_root)?;
        // User 配置由当前用户直接写入数据根，项目配置则必须按当前 canonical
        // workspace 逐条批准；插件不经过这里，插件启用状态在 PluginManager 中决定。
        // 用户 Hook 也绑定当前 workspace 的执行摘要。这样同一份用户配置在
        // 不同项目中不会绕过项目级授权，命令或引用脚本变化后旧准入也会失效。
        let trusted = matches!(&definition.scope, HookScope::Plugin { .. })
            || trust.entries.iter().any(|entry| {
                entry.project_fingerprint == project_fingerprint(project_root)
                    && entry.scope_key == definition.scope.key()
                    && entry.hook_id == definition.id
                    && entry.admitted_digest == digest
            });
        (digest, trusted)
    } else {
        // 用户设置页可能在没有当前项目时加载；此时只能展示定义，不能声称已
        // 针对某个真实工作区计算过执行 digest 或完成 trust。
        (String::new(), false)
    };
    Ok(HookSummary {
        id: definition.id.clone(),
        name: definition.name.clone(),
        scope: definition.scope.clone(),
        phase: definition.phase,
        matcher: definition.matcher.clone(),
        kind: match &definition.handler {
            HookHandler::Command { .. } => "command",
            HookHandler::Context { .. } => "context",
        }
        .to_owned(),
        enabled: definition.enabled,
        trusted,
        pending: definition.enabled && !trusted,
        digest,
    })
}

fn read_definition_for_scope(
    paths: &NativePaths,
    scope: &HookScope,
    id: &str,
) -> Result<HookDefinition, String> {
    let (path, expected_scope, project_root) = definition_target(paths, scope)?;
    let document = read_hook_document(&path)?;
    validate_hook_document(&document, expected_scope.as_ref(), project_root.as_deref())?;
    document
        .hooks
        .into_iter()
        .find(|hook| hook.id == id)
        .ok_or_else(|| format!("找不到 Hook：{id}"))
}

fn definition_target(
    paths: &NativePaths,
    scope: &HookScope,
) -> Result<(PathBuf, Option<HookScope>, Option<PathBuf>), String> {
    match normalize_scope(scope.clone())? {
        HookScope::User => Ok((user_hooks_path(paths)?, Some(HookScope::User), None)),
        HookScope::Project { root } => {
            let path = project_hooks_path(&root)?;
            Ok((
                path,
                Some(HookScope::Project { root: root.clone() }),
                Some(root),
            ))
        }
        HookScope::Plugin { .. } => Err("插件 Hook 不允许通过此 API 修改".to_owned()),
    }
}

fn normalize_definition(definition: &mut HookDefinition) -> Result<(), String> {
    validate_definition_shape(definition)?;
    definition.scope = normalize_scope(definition.scope.clone())?;
    Ok(())
}

fn normalize_scope(scope: HookScope) -> Result<HookScope, String> {
    match scope {
        HookScope::User => Ok(HookScope::User),
        HookScope::Project { root } => Ok(HookScope::Project {
            root: canonical_project_root(&root)?,
        }),
        HookScope::Plugin { plugin_id } => {
            if plugin_id.trim().is_empty() {
                return Err("插件 Hook ID 不能为空".to_owned());
            }
            Ok(HookScope::Plugin { plugin_id })
        }
    }
}

fn validate_hook_document(
    document: &HookDocument,
    expected_scope: Option<&HookScope>,
    project_root: Option<&Path>,
) -> Result<(), String> {
    if document.schema != HOOKS_SCHEMA || document.version != HOOKS_VERSION {
        return Err("Hook 配置 schema 或版本不受支持".to_owned());
    }
    if document.hooks.len() > MAX_HOOK_COUNT {
        return Err("Hook 数量超过限制".to_owned());
    }
    let mut ids = BTreeSet::new();
    for hook in &document.hooks {
        validate_definition_shape(hook)?;
        let scope = normalize_scope(hook.scope.clone())?;
        if expected_scope.is_some_and(|expected| expected != &scope) {
            return Err(format!("Hook {} 的作用域与配置文件不一致", hook.id));
        }
        if let Some(project_root) = project_root
            && let HookScope::Project { root } = &scope
            && root != project_root
        {
            return Err(format!("Hook {} 不属于当前项目根", hook.id));
        }
        if !ids.insert(hook.id.as_str()) {
            return Err(format!("Hook ID 重复：{}", hook.id));
        }
    }
    Ok(())
}

fn validate_definition_shape(definition: &HookDefinition) -> Result<(), String> {
    if !valid_identifier(&definition.id, MAX_HOOK_ID_BYTES) {
        return Err(format!("Hook ID 无效：{}", definition.id));
    }
    if definition.name.trim().is_empty()
        || definition.name != definition.name.trim()
        || definition.name.len() > MAX_HOOK_NAME_BYTES
        || definition.name.chars().any(char::is_control)
    {
        return Err(format!("Hook {} 的显示名称无效", definition.id));
    }
    if let Some(matcher) = &definition.matcher
        && (matcher.len() > MAX_MATCHER_BYTES || matcher.chars().any(char::is_control))
    {
        return Err(format!("Hook {} 的 matcher 无效", definition.id));
    }
    match &definition.handler {
        HookHandler::Command {
            command,
            args,
            shell,
            timeout_ms,
            environment,
        } => {
            if command.trim().is_empty() || command.len() > 16 * 1024 || command.contains('\0') {
                return Err(format!("Hook {} 的 command 无效", definition.id));
            }
            if let Some(args) = args
                && (args.len() > 256
                    || args
                        .iter()
                        .any(|arg| arg.len() > 16 * 1024 || arg.contains('\0')))
            {
                return Err(format!("Hook {} 的 args 无效", definition.id));
            }
            if let Some(shell) = shell
                && !matches!(shell.as_str(), "bash" | "powershell")
            {
                return Err(format!("Hook {} 的 shell 无效", definition.id));
            }
            if let Some(timeout_ms) = timeout_ms
                && !(1..=3_600_000).contains(timeout_ms)
            {
                return Err(format!("Hook {} 的 timeout_ms 无效", definition.id));
            }
            if environment.len() > MAX_ENVIRONMENT_ENTRIES {
                return Err(format!("Hook {} 的环境变量数量超过限制", definition.id));
            }
            for (key, value) in environment {
                if key.is_empty()
                    || key.len() > MAX_ENVIRONMENT_KEY_BYTES
                    || key.chars().any(char::is_control)
                    || value.len() > MAX_ENVIRONMENT_VALUE_BYTES
                    || value.chars().any(char::is_control)
                {
                    return Err(format!("Hook {} 的环境变量无效", definition.id));
                }
            }
        }
        HookHandler::Context {
            context,
            block,
            input,
            continue_turn,
        } => {
            for (field, value) in [("context", context), ("block", block)] {
                if let Some(value) = value
                    && (value.trim().is_empty()
                        || value.len() > MAX_CONTEXT_BYTES
                        || value.chars().any(char::is_control))
                {
                    return Err(format!("Hook {} 的 {field} 无效", definition.id));
                }
            }
            if let Some(input) = input {
                let bytes = serde_json::to_vec(input)
                    .map_err(|error| format!("Hook {} 的 input 无效：{error}", definition.id))?;
                if bytes.len() > MAX_CONTEXT_BYTES {
                    return Err(format!("Hook {} 的 input 超过大小限制", definition.id));
                }
            }
            if definition.phase == HookPhase::Stop && definition.matcher.is_some() {
                return Err(format!("Stop Hook {} 不能声明 matcher", definition.id));
            }
            if definition.phase != HookPhase::Stop && *continue_turn {
                return Err(format!(
                    "只有 Stop Hook {} 可以声明 continue",
                    definition.id
                ));
            }
            if definition.phase != HookPhase::PreToolUse && (block.is_some() || input.is_some()) {
                return Err(format!(
                    "只有 PreToolUse Hook {} 可以修改或阻止工具输入",
                    definition.id
                ));
            }
            if matches!(
                definition.phase,
                HookPhase::OnError | HookPhase::PreCompact | HookPhase::PostCompact
            ) {
                return Err(format!(
                    "{} Hook {} 只支持 command 类型",
                    definition.phase, definition.id
                ));
            }
        }
    }
    Ok(())
}

fn execution_payload_digest(
    definition: &HookDefinition,
    project_root: &Path,
) -> Result<String, String> {
    let project_root = canonical_project_root(project_root)?;
    let mut payload = Map::new();
    payload.insert("id".to_owned(), Value::String(definition.id.clone()));
    payload.insert("scope".to_owned(), Value::String(definition.scope.key()));
    payload.insert(
        "projectRoot".to_owned(),
        Value::String(project_root.to_string_lossy().into_owned()),
    );
    payload.insert(
        "phase".to_owned(),
        serde_json::to_value(definition.phase).map_err(|error| error.to_string())?,
    );
    payload.insert(
        "matcher".to_owned(),
        definition
            .matcher
            .clone()
            .map_or(Value::Null, Value::String),
    );
    payload.insert(
        "handler".to_owned(),
        serde_json::to_value(&definition.handler).map_err(|error| error.to_string())?,
    );
    if let HookHandler::Command { .. } = &definition.handler {
        payload.insert(
            "scripts".to_owned(),
            Value::Array(referenced_script_digests(definition, &project_root)?),
        );
    }
    Ok(digest_json(&Value::Object(payload)))
}

fn referenced_script_digests(
    definition: &HookDefinition,
    project_root: &Path,
) -> Result<Vec<Value>, String> {
    // 这里只对命令参数中能识别为脚本文件的路径纳入 bundle digest；缺失路径
    // 也保留 missing 标记，避免脚本后来出现时沿用旧 trust。shell 动态拼接、
    // PATH 中的可执行文件和远程资源仍由现有 runner/系统权限约束，不把这个摘要
    // 误认为完整命令沙箱。
    if !matches!(&definition.handler, HookHandler::Command { .. }) {
        return Ok(Vec::new());
    }
    let mut paths = BTreeSet::new();
    for candidate in referenced_script_paths(definition, project_root) {
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                paths.insert((
                    candidate.to_string_lossy().into_owned(),
                    "missing".to_owned(),
                ));
                continue;
            }
            Err(error) => return Err(format!("读取 Hook 脚本失败：{error}")),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("Hook 脚本必须是普通文件：{}", candidate.display()));
        }
        if metadata.len() > MAX_SCRIPT_BYTES {
            return Err(format!("Hook 脚本超过大小限制：{}", candidate.display()));
        }
        let canonical = fs::canonicalize(&candidate)
            .map_err(|error| format!("规范化 Hook 脚本失败：{error}"))?;
        let bytes = storage::read_private_bytes_bounded(&canonical, MAX_SCRIPT_BYTES, "Hook 脚本")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("Hook 脚本不存在：{}", canonical.display()))?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        paths.insert((canonical.to_string_lossy().into_owned(), digest));
    }
    Ok(paths
        .into_iter()
        .map(|(path, digest)| json!({"path": path, "sha256": digest}))
        .collect())
}

fn referenced_script_paths(definition: &HookDefinition, project_root: &Path) -> Vec<PathBuf> {
    let HookHandler::Command { command, args, .. } = &definition.handler else {
        return Vec::new();
    };
    let mut candidates = command_tokens(command);
    if let Some(args) = args {
        candidates.extend(args.iter().cloned());
    }
    let mut paths = BTreeSet::new();
    for token in candidates {
        let token = token.trim_matches(['"', '\'']);
        if !looks_like_script_path(token) {
            continue;
        }
        let candidate = PathBuf::from(token);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            project_root.join(candidate)
        };
        paths.insert(candidate);
    }
    paths.into_iter().collect()
}

fn definition_event_value(definition: &HookDefinition) -> (Value, BTreeMap<String, String>) {
    let phase = serde_json::to_string(&definition.phase)
        .expect("HookPhase 序列化必须成功")
        .trim_matches('"')
        .to_owned();
    let (handler, environment) = match &definition.handler {
        HookHandler::Command {
            command,
            args,
            shell,
            timeout_ms,
            environment,
        } => {
            let mut object = Map::new();
            object.insert("type".to_owned(), Value::String("command".to_owned()));
            object.insert("command".to_owned(), Value::String(command.clone()));
            if let Some(args) = args {
                object.insert("args".to_owned(), json!(args));
            }
            if let Some(shell) = shell {
                object.insert("shell".to_owned(), Value::String(shell.clone()));
            }
            if let Some(timeout_ms) = timeout_ms {
                object.insert(
                    "timeout".to_owned(),
                    Value::from((*timeout_ms as f64) / 1000.0),
                );
            }
            (Value::Object(object), environment.clone())
        }
        HookHandler::Context {
            context,
            block,
            input,
            continue_turn,
        } => {
            let mut object = Map::new();
            object.insert("type".to_owned(), Value::String("context".to_owned()));
            if let Some(context) = context {
                object.insert("context".to_owned(), Value::String(context.clone()));
            }
            if let Some(block) = block {
                object.insert("block".to_owned(), Value::String(block.clone()));
            }
            if let Some(input) = input {
                object.insert("input".to_owned(), input.clone());
            }
            if *continue_turn {
                object.insert("continue".to_owned(), Value::Bool(true));
            }
            (Value::Object(object), BTreeMap::new())
        }
    };
    // matcher 缺省表示全匹配；省略字段以符合解析器的 optional matcher 契约。
    // 直接把 None 序列化为 JSON null 会被解析器视为无效 matcher。
    let mut group = Map::new();
    if let Some(matcher) = &definition.matcher {
        group.insert("matcher".to_owned(), Value::String(matcher.clone()));
    }
    group.insert("hooks".to_owned(), json!([handler]));
    (json!({phase: [Value::Object(group)]}), environment)
}

fn user_hooks_path(paths: &NativePaths) -> Result<PathBuf, String> {
    let root = storage::root_dir(paths).map_err(|error| error.to_string())?;
    Ok(root.join(USER_HOOKS_FILE))
}

fn project_hooks_path(project_root: &Path) -> Result<PathBuf, String> {
    validate_directory_component(project_root)?;
    let directory = project_root.join(PROJECT_HOOKS_DIRECTORY);
    if let Ok(metadata) = fs::symlink_metadata(&directory)
        && (metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err(format!(
            "Hook 配置目录必须是普通目录：{}",
            directory.display()
        ));
    }
    Ok(directory.join(PROJECT_HOOKS_FILE))
}

fn trust_path(paths: &NativePaths) -> Result<PathBuf, String> {
    let root = storage::root_dir(paths).map_err(|error| error.to_string())?;
    Ok(root.join(TRUST_FILE))
}

fn read_hook_document(path: &Path) -> Result<HookDocument, String> {
    let Some(bytes) = storage::read_private_bytes_bounded(path, MAX_HOOKS_BYTES, "Hook 配置")
        .map_err(|error| error.to_string())?
    else {
        return Ok(HookDocument::default());
    };
    if bytes.is_empty() {
        return Err(format!("Hook 配置为空：{}", path.display()));
    }
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .map_err(|error| format!("Hook 配置格式无效：{error}"))
}

fn write_hook_document(path: &Path, document: &HookDocument) -> Result<(), String> {
    validate_hook_document(document, None, None)?;
    reject_existing_reparse_point(path)?;
    let mut bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| format!("序列化 Hook 配置失败：{error}"))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_HOOKS_BYTES {
        return Err("Hook 配置超过大小限制".to_owned());
    }
    storage::atomic_write_private(path, &bytes).map_err(|error| error.to_string())
}

fn read_trust_document(path: &Path) -> Result<TrustDocument, String> {
    let Some(bytes) = storage::read_private_bytes_bounded(path, MAX_HOOKS_BYTES, "Hook trust")
        .map_err(|error| error.to_string())?
    else {
        return Ok(TrustDocument::default());
    };
    if bytes.is_empty() {
        return Err(format!("Hook trust 为空：{}", path.display()));
    }
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .map_err(|error| format!("Hook trust 格式无效：{error}"))
}

fn write_trust_document(path: &Path, document: &TrustDocument) -> Result<(), String> {
    validate_trust_document(document)?;
    reject_existing_reparse_point(path)?;
    let mut bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| format!("序列化 Hook trust 失败：{error}"))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_HOOKS_BYTES {
        return Err("Hook trust 超过大小限制".to_owned());
    }
    storage::atomic_write_private(path, &bytes).map_err(|error| error.to_string())
}

fn validate_trust_document(document: &TrustDocument) -> Result<(), String> {
    if document.schema != TRUST_SCHEMA || document.version != TRUST_VERSION {
        return Err("Hook trust schema 或版本不受支持".to_owned());
    }
    if document.entries.len() > MAX_HOOK_COUNT * 4 {
        return Err("Hook trust 条目超过限制".to_owned());
    }
    let mut keys = BTreeSet::new();
    for entry in &document.entries {
        if !is_sha256(&entry.project_fingerprint)
            || !is_sha256(&entry.admitted_digest)
            || !valid_identifier(&entry.hook_id, MAX_HOOK_ID_BYTES)
            || entry.scope_key.trim().is_empty()
            || !keys.insert((
                entry.project_fingerprint.as_str(),
                entry.scope_key.as_str(),
                entry.hook_id.as_str(),
            ))
        {
            return Err("Hook trust 条目无效或重复".to_owned());
        }
    }
    Ok(())
}

struct HookStorageGuard {
    // 进程内锁减少同一 Host 的重复打开；旁边的 lock 文件覆盖多个桌面进程。
    _process_guard: MutexGuard<'static, ()>,
    file: File,
}

impl Drop for HookStorageGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn storage_lock(paths: &NativePaths) -> Result<HookStorageGuard, String> {
    let process_guard = HOOK_STORAGE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Hook 存储锁已损坏".to_owned())?;
    let root = storage::root_dir(paths).map_err(|error| error.to_string())?;
    ensure_directory(&root)?;
    let path = root.join(".hooks.lock");
    reject_existing_reparse_point(&path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_CLOEXEC: i32 = 0o2_000_000;
        const O_NOFOLLOW: i32 = 0o400_000;
        options.custom_flags(O_CLOEXEC | O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("打开 Hook 跨进程锁失败：{error}"))?;
    file.lock_exclusive()
        .map_err(|error| format!("获取 Hook 跨进程锁失败：{error}"))?;
    Ok(HookStorageGuard {
        _process_guard: process_guard,
        file,
    })
}

fn write_parent_for(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_directory(parent)?;
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!("Hook 配置目录必须是普通目录：{}", path.display()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| format!("创建 Hook 配置目录失败：{error}"))?;
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| format!("检查 Hook 配置目录失败：{error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!("Hook 配置目录必须是普通目录：{}", path.display()));
            }
        }
        Err(error) => return Err(format!("检查 Hook 配置目录失败：{error}")),
    }
    Ok(())
}

fn reject_existing_reparse_point(path: &Path) -> Result<(), String> {
    write_parent_for(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                Err(format!("Hook 配置目标必须是普通文件：{}", path.display()))
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("检查 Hook 配置目标失败：{error}")),
    }
}

fn validate_directory_component(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("检查项目根失败：{error}"))?;
    if !metadata.is_dir() {
        return Err("项目根必须是目录".to_owned());
    }
    Ok(())
}

fn canonical_project_root(path: &Path) -> Result<PathBuf, String> {
    validate_directory_component(path)?;
    fs::canonicalize(path).map_err(|error| format!("无法规范化项目目录：{error}"))
}

fn project_fingerprint(path: &Path) -> String {
    format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()))
}

fn digest_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("JSON digest 输入必须可序列化");
    format!("{:x}", Sha256::digest(bytes))
}

fn next_revision(revision: u64) -> Result<u64, String> {
    revision
        .checked_add(1)
        .ok_or_else(|| "Hook revision 已耗尽".to_owned())
}

fn revision_conflict(actual: u64, expected: u64) -> String {
    format!("Hook revision 冲突：当前 {actual}，请求 {expected}")
}

fn valid_identifier(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn default_enabled() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn bounded_text(value: &str) -> String {
    let value = value.replace(['\r', '\n'], " ");
    let mut end = value.len().min(1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn hook_diagnostic(id: &str, code: &str, message: impl Into<String>) -> RuntimeExtensionDiagnostic {
    RuntimeExtensionDiagnostic {
        source: "hook".to_owned(),
        server: id.to_owned(),
        code: code.to_owned(),
        message: message.into(),
        tool: None,
    }
}

fn command_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|token| token.trim_matches(['"', '\'']).to_owned())
        .filter(|token| !token.is_empty())
        .collect()
}

fn looks_like_script_path(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        ".py", ".js", ".mjs", ".cjs", ".ts", ".tsx", ".ps1", ".sh", ".bash", ".cmd", ".bat", ".rb",
        ".pl", ".lua", ".php", ".r",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn paths(root: &Path) -> NativePaths {
        NativePaths::from_data_root(root.to_path_buf())
    }

    fn command_definition(scope: HookScope, id: &str) -> HookDefinition {
        HookDefinition {
            id: id.to_owned(),
            name: "Marker".to_owned(),
            scope,
            phase: HookPhase::PreToolUse,
            matcher: Some("Bash".to_owned()),
            handler: HookHandler::Command {
                command: "python marker.py".to_owned(),
                args: None,
                shell: None,
                timeout_ms: Some(10_000),
                environment: BTreeMap::new(),
            },
            enabled: true,
        }
    }

    #[test]
    fn strict_schema_rejects_unknown_fields() {
        let value = br#"{"schema":"keencode/native-hooks","version":1,"revision":0,"hooks":[],"extra":true}"#;
        let result = serde_json::from_slice::<HookDocument>(value);
        assert!(result.is_err());
    }

    #[test]
    fn digest_excludes_display_name_and_enabled_but_tracks_script_content() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("marker.py"), b"one").unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let mut definition =
            command_definition(HookScope::Project { root: root.clone() }, "marker");
        let first = execution_payload_digest(&definition, &root).unwrap();
        definition.name = "Renamed".to_owned();
        definition.enabled = false;
        assert_eq!(first, execution_payload_digest(&definition, &root).unwrap());
        fs::write(root.join("marker.py"), b"two").unwrap();
        assert_ne!(first, execution_payload_digest(&definition, &root).unwrap());
    }

    #[test]
    fn input_fingerprint_tracks_hook_trust_and_script_content() {
        let data = tempdir().unwrap();
        let project = tempdir().unwrap();
        let paths = paths(data.path());
        let root = fs::canonicalize(project.path()).unwrap();
        fs::write(root.join("marker.py"), b"one").unwrap();
        let definition = command_definition(HookScope::Project { root: root.clone() }, "marker");
        let created = upsert(&paths, definition, 0).unwrap();
        let trusted = set_trusted(
            &paths,
            &root,
            HookScope::Project { root: root.clone() },
            "marker",
            true,
            created.trust_revision,
        )
        .unwrap();
        let first = input_fingerprint(&paths, &root);
        assert!(first.readable);

        upsert(
            &paths,
            command_definition(HookScope::User, "user-marker"),
            0,
        )
        .unwrap();
        let user_changed = input_fingerprint(&paths, &root);
        assert_ne!(first.digest, user_changed.digest);

        fs::write(root.join("marker.py"), b"two").unwrap();
        let script_changed = input_fingerprint(&paths, &root);
        assert_ne!(user_changed.digest, script_changed.digest);

        set_trusted(
            &paths,
            &root,
            HookScope::Project { root: root.clone() },
            "marker",
            false,
            trusted.trust_revision,
        )
        .unwrap();
        let trust_changed = input_fingerprint(&paths, &root);
        assert_ne!(script_changed.digest, trust_changed.digest);
    }

    #[test]
    fn crud_uses_revision_and_digest_trust() {
        let data = tempdir().unwrap();
        let project = tempdir().unwrap();
        let paths = paths(data.path());
        let root = fs::canonicalize(project.path()).unwrap();
        fs::write(root.join("marker.py"), b"one").unwrap();
        let definition = command_definition(HookScope::Project { root: root.clone() }, "marker");
        let created = upsert(&paths, definition, 0).unwrap();
        assert_eq!(created.project_revision, Some(1));
        assert!(created.summaries[0].pending);
        assert!(
            upsert(
                &paths,
                command_definition(HookScope::Project { root: root.clone() }, "marker"),
                0,
            )
            .is_err()
        );
        let trusted = set_trusted(
            &paths,
            &root,
            HookScope::Project { root: root.clone() },
            "marker",
            true,
            0,
        )
        .unwrap();
        assert!(trusted.summaries[0].trusted);
        fs::write(root.join("marker.py"), b"changed").unwrap();
        let changed = load(&paths, Some(&root)).unwrap();
        assert!(changed.summaries[0].pending);
    }

    #[test]
    fn deleting_user_hook_clears_all_workspace_trust_but_preserves_other_scopes() {
        let data = tempdir().unwrap();
        let project_a = tempdir().unwrap();
        let project_b = tempdir().unwrap();
        let paths = paths(data.path());
        let root_a = fs::canonicalize(project_a.path()).unwrap();
        let root_b = fs::canonicalize(project_b.path()).unwrap();

        let shared_user = command_definition(HookScope::User, "shared");
        let created_user = upsert(&paths, shared_user, 0).unwrap();
        let other_user = upsert(
            &paths,
            command_definition(HookScope::User, "other"),
            created_user.user_revision,
        )
        .unwrap();

        let project_scope_a = HookScope::Project {
            root: root_a.clone(),
        };
        let project_scope_b = HookScope::Project {
            root: root_b.clone(),
        };
        let created_project_a = upsert(
            &paths,
            command_definition(project_scope_a.clone(), "shared"),
            0,
        )
        .unwrap();
        upsert(
            &paths,
            command_definition(project_scope_b.clone(), "shared"),
            0,
        )
        .unwrap();

        let trusted_project_a = set_trusted(
            &paths,
            &root_a,
            project_scope_a,
            "shared",
            true,
            created_project_a.trust_revision,
        )
        .unwrap();
        let trusted_project_b = set_trusted(
            &paths,
            &root_b,
            project_scope_b,
            "shared",
            true,
            trusted_project_a.trust_revision,
        )
        .unwrap();
        let trusted_user_a = set_trusted(
            &paths,
            &root_a,
            HookScope::User,
            "shared",
            true,
            trusted_project_b.trust_revision,
        )
        .unwrap();
        let trusted_user_b = set_trusted(
            &paths,
            &root_b,
            HookScope::User,
            "shared",
            true,
            trusted_user_a.trust_revision,
        )
        .unwrap();
        let trusted_other = set_trusted(
            &paths,
            &root_a,
            HookScope::User,
            "other",
            true,
            trusted_user_b.trust_revision,
        )
        .unwrap();

        let trust_before = read_trust_document(&trust_path(&paths).unwrap()).unwrap();
        assert_eq!(trust_before.entries.len(), 5);

        let deleted = delete(&paths, HookScope::User, "shared", other_user.user_revision).unwrap();
        assert_eq!(deleted.user_revision, other_user.user_revision + 1);

        let trust_after = read_trust_document(&trust_path(&paths).unwrap()).unwrap();
        assert_eq!(trust_after.revision, trust_before.revision + 1);
        assert_eq!(
            trust_after
                .entries
                .iter()
                .filter(|entry| entry.scope_key == "user" && entry.hook_id == "shared")
                .count(),
            0
        );
        assert_eq!(
            trust_after
                .entries
                .iter()
                .filter(|entry| entry.hook_id == "shared")
                .count(),
            2
        );
        assert_eq!(
            trust_after
                .entries
                .iter()
                .filter(|entry| entry.scope_key == "user" && entry.hook_id == "other")
                .count(),
            1
        );
        assert_eq!(trust_after.revision, trusted_other.trust_revision + 1);

        let loaded_a = load(&paths, Some(&root_a)).unwrap();
        assert!(
            !loaded_a
                .hooks
                .iter()
                .any(|hook| hook.scope == HookScope::User && hook.id == "shared")
        );
        assert!(loaded_a.hooks.iter().any(|hook| {
            hook.scope
                == (HookScope::Project {
                    root: root_a.clone(),
                })
                && hook.id == "shared"
        }));
    }

    #[test]
    fn delete_revision_conflict_does_not_revoke_trust() {
        let data = tempdir().unwrap();
        let project = tempdir().unwrap();
        let paths = paths(data.path());
        let root = fs::canonicalize(project.path()).unwrap();

        let created = upsert(&paths, command_definition(HookScope::User, "conflict"), 0).unwrap();
        let trusted = set_trusted(
            &paths,
            &root,
            HookScope::User,
            "conflict",
            true,
            created.trust_revision,
        )
        .unwrap();
        let trust_before = read_trust_document(&trust_path(&paths).unwrap()).unwrap();

        let result = delete(
            &paths,
            HookScope::User,
            "conflict",
            trusted.user_revision + 1,
        );
        assert!(result.is_err());

        let trust_after = read_trust_document(&trust_path(&paths).unwrap()).unwrap();
        assert_eq!(trust_after.revision, trust_before.revision);
        assert_eq!(trust_after.entries.len(), trust_before.entries.len());
        assert_eq!(
            trust_after.entries[0].project_fingerprint,
            trust_before.entries[0].project_fingerprint
        );
        assert_eq!(
            trust_after.entries[0].scope_key,
            trust_before.entries[0].scope_key
        );
        assert_eq!(
            trust_after.entries[0].hook_id,
            trust_before.entries[0].hook_id
        );
        assert_eq!(
            trust_after.entries[0].admitted_digest,
            trust_before.entries[0].admitted_digest
        );

        let hook_document = read_hook_document(&user_hooks_path(&paths).unwrap()).unwrap();
        assert_eq!(hook_document.hooks.len(), 1);
        assert_eq!(hook_document.hooks[0].id, "conflict");
    }

    #[test]
    fn project_scope_is_canonicalized_and_isolated() {
        let data = tempdir().unwrap();
        let project = tempdir().unwrap();
        let paths = paths(data.path());
        let root = fs::canonicalize(project.path()).unwrap();
        let definition = command_definition(
            HookScope::Project {
                root: project.path().join(".").to_path_buf(),
            },
            "marker",
        );
        upsert(&paths, definition, 0).unwrap();
        assert_eq!(load(&paths, Some(&root)).unwrap().hooks.len(), 1);
        let other = tempdir().unwrap();
        assert!(load(&paths, Some(other.path())).unwrap().hooks.is_empty());
    }

    #[test]
    fn runtime_hooks_accept_missing_matcher_for_user_and_project() {
        let data = tempdir().unwrap();
        let project = tempdir().unwrap();
        let paths = paths(data.path());
        let root = fs::canonicalize(project.path()).unwrap();

        let definition = |scope, id: &str| HookDefinition {
            id: id.to_owned(),
            name: id.to_owned(),
            scope,
            phase: HookPhase::PreToolUse,
            matcher: None,
            handler: HookHandler::Command {
                command: "echo marker".to_owned(),
                args: None,
                shell: None,
                timeout_ms: Some(10_000),
                environment: BTreeMap::new(),
            },
            enabled: true,
        };

        let project_scope = HookScope::Project { root: root.clone() };
        let project_created = upsert(
            &paths,
            definition(project_scope.clone(), "project-marker"),
            0,
        )
        .unwrap();
        let project_trusted = set_trusted(
            &paths,
            &root,
            project_scope,
            "project-marker",
            true,
            project_created.trust_revision,
        )
        .unwrap();

        upsert(&paths, definition(HookScope::User, "user-marker"), 0).unwrap();
        set_trusted(
            &paths,
            &root,
            HookScope::User,
            "user-marker",
            true,
            project_trusted.trust_revision,
        )
        .unwrap();

        let (hooks, diagnostics) = runtime_hooks(&paths, &root);
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
        assert_eq!(hooks.len(), 2);
        assert!(
            hooks
                .iter()
                .all(|hook| matches!(hook, HookSpec::Command(_)))
        );
    }
}
