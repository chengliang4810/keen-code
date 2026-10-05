//! 原页面偏好持久化。技能名称开关由扩展运行时共同读取；消息、运行状态与凭据仍由既有后端管理。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;
use tauri::AppHandle;

const SCHEMA: &str = "keencode/ui-presentation";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
static IO_LOCK: Mutex<()> = Mutex::new(());

/// 原首页创建普通聊天工作区时使用宿主数据目录，开发/正式/隔离验收自然分开。
#[tauri::command]
pub fn ui_workspace_paths(app: AppHandle) -> Result<Value, String> {
    let root = crate::storage::root_dir(&app).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "chatWorkspaceRoot": crate::path_utils::path_to_frontend(&root.join("chat-workspaces")),
    }))
}

/// 只存界面偏好和实体的展示元数据，不存可从 ACP 回放的会话事实。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UiPresentation {
    schema: String,
    version: u32,
    settings: Map<String, Value>,
    projects: BTreeMap<String, Value>,
    threads: BTreeMap<String, Value>,
    spaces: BTreeMap<String, Value>,
}

impl Default for UiPresentation {
    fn default() -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            version: 1,
            settings: Map::new(),
            projects: BTreeMap::new(),
            threads: BTreeMap::new(),
            spaces: BTreeMap::new(),
        }
    }
}

fn load(app: &AppHandle) -> Result<UiPresentation, String> {
    let root = crate::storage::root_dir(app).map_err(|error| error.to_string())?;
    load_from_root(&root)
}

fn load_from_root(root: &std::path::Path) -> Result<UiPresentation, String> {
    let path = root.join("ui-presentation.json");
    let bytes = crate::storage::read_private_bytes_bounded(&path, MAX_BYTES, "界面元数据")
        .map_err(|error| error.to_string())?;
    let Some(bytes) = bytes else {
        return Ok(UiPresentation::default());
    };
    let state: UiPresentation =
        serde_json::from_slice(&bytes).map_err(|error| format!("界面元数据格式无效：{error}"))?;
    if state.schema != SCHEMA || state.version != 1 {
        return Err("界面元数据 schema 无效".to_owned());
    }
    // 磁盘内容也属于输入边界，不能让手工修改或损坏文件覆盖 ACP 权威字段。
    validate_patch("settings", None, &Value::Object(state.settings.clone()))?;
    for (scope, entries) in [
        ("projects", &state.projects),
        ("threads", &state.threads),
        ("spaces", &state.spaces),
    ] {
        for (id, value) in entries {
            validate_patch(scope, Some(id), value)?;
        }
    }
    Ok(state)
}

/// 原技能设置的唯一持久化值。目录与下一轮运行候选共同读取，不能仅隐藏选择器。
pub(crate) fn disabled_skill_names(
    root: &std::path::Path,
) -> Result<std::collections::BTreeSet<String>, String> {
    let state = load_from_root(root)?;
    Ok(state
        .settings
        .get("skills")
        .and_then(|skills| skills.get("disabled"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|name| name.trim().to_lowercase())
        .collect())
}

fn validate_patch(scope: &str, id: Option<&str>, patch: &Value) -> Result<(), String> {
    let Some(object) = patch.as_object() else {
        return Err("界面元数据补丁必须是对象".to_owned());
    };
    let allowed: &[&str] = match scope {
        "settings" => &[
            "enableAssistantStreaming",
            "enableProviderUpdateChecks",
            "defaultThreadEnvMode",
            "addProjectBaseDirectory",
            "textGenerationModelSelection",
            "providers",
            "skills",
            "onboardingCompletedAt",
        ],
        "projects" => &[
            "kind",
            "defaultModelSelection",
            "scripts",
            "isPinned",
            "spaceId",
            "createdAt",
            "updatedAt",
        ],
        "threads" => &[
            "pendingWorktree",
            "forkImportedTurnIds",
            "notes",
            "pinnedMessages",
            "settledAt",
            "branch",
            "associatedWorktreePath",
            "associatedWorktreeBranch",
            "associatedWorktreeRef",
            "forkSourceThreadId",
            "sidechatSourceThreadId",
            "createdAt",
            "updatedAt",
        ],
        "spaces" => &[
            "id",
            "name",
            "icon",
            "sortOrder",
            "createdAt",
            "updatedAt",
            "deletedAt",
        ],
        _ => return Err("未知界面元数据作用域".to_owned()),
    };
    if scope != "settings"
        && id.is_none_or(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
    {
        return Err("界面实体标识无效".to_owned());
    }
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("补丁包含非界面元数据字段".to_owned());
    }
    // 不允许把凭据或运行事实藏在模型选项、供应商偏好等嵌套对象中。
    validate_nested(patch, 0)?;
    for (key, value) in object {
        let valid = match key.as_str() {
            "enableAssistantStreaming"
            | "enableProviderUpdateChecks"
            | "isPinned"
            | "pendingWorktree" => value.is_boolean(),
            "scripts" | "pinnedMessages" => value.is_array(),
            "forkImportedTurnIds" => value.as_array().is_some_and(|ids| {
                ids.len() <= 10_000
                    && ids.iter().all(|id| {
                        id.as_str().is_some_and(|id| {
                            !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
                        })
                    })
            }),
            "defaultModelSelection" => value.is_null() || value.is_object(),
            "skills" => value.as_object().is_some_and(|fields| {
                fields.keys().all(|key| key == "disabled")
                    && fields.get("disabled").is_none_or(|disabled| {
                        disabled.as_array().is_some_and(|names| {
                            names.len() <= 10_000
                                && names.iter().all(|name| {
                                    name.as_str().is_some_and(|name| {
                                        !name.trim().is_empty()
                                            && name.chars().count() <= 256
                                            && !name.chars().any(char::is_control)
                                    })
                                })
                        })
                    })
            }),
            "textGenerationModelSelection" | "providers" => value.is_object(),
            "sortOrder" => value.as_u64().is_some(),
            "spaceId"
            | "settledAt"
            | "branch"
            | "associatedWorktreePath"
            | "associatedWorktreeBranch"
            | "associatedWorktreeRef"
            | "deletedAt"
            | "onboardingCompletedAt"
            | "forkSourceThreadId"
            | "sidechatSourceThreadId" => value.is_null() || value.is_string(),
            _ => value.is_string(),
        };
        if !valid {
            return Err(format!("界面元数据字段类型无效：{key}"));
        }
    }
    if serde_json::to_vec(patch)
        .map_err(|error| error.to_string())?
        .len()
        > 1024 * 1024
    {
        return Err("界面元数据补丁过大".to_owned());
    }
    Ok(())
}

fn validate_nested(value: &Value, depth: usize) -> Result<(), String> {
    if depth > 16 {
        return Err("界面元数据嵌套过深".to_owned());
    }
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let normalized = key.replace('_', "").to_ascii_lowercase();
                if [
                    "apikey",
                    "password",
                    "serverpassword",
                    "accesstoken",
                    "refreshtoken",
                    "secret",
                    "messages",
                    "runtimestate",
                ]
                .contains(&normalized.as_str())
                {
                    return Err("界面元数据不允许存储凭据或会话运行事实".to_owned());
                }
                validate_nested(value, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_nested(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// 合并嵌套设置时保留其他供应商/字段；null 是原页面的显式清空值。
fn merge(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (target, patch) => *target = patch,
    }
}

#[tauri::command]
pub fn ui_presentation_get(app: AppHandle) -> Result<UiPresentation, String> {
    let _guard = IO_LOCK.lock().map_err(|_| "界面元数据锁已损坏")?;
    load(&app)
}

/// 宿主交接只读取该会话的展示关联；执行目录仍需单独从 Session 权威日志读取。
pub(crate) fn thread_metadata(app: &AppHandle, id: &str) -> Result<Value, String> {
    let _guard = IO_LOCK.lock().map_err(|_| "界面元数据锁已损坏")?;
    Ok(load(app)?.threads.get(id).cloned().unwrap_or(Value::Null))
}

/// 只检查仍存在的权威 Session 的工作树关联；已删除会话遗留的展示字段不能阻止清理。
pub(crate) fn associated_worktree_paths(
    app: &AppHandle,
    session_ids: &[&str],
) -> Result<Vec<String>, String> {
    let _guard = IO_LOCK.lock().map_err(|_| "界面元数据锁已损坏")?;
    let presentation = load(app)?;
    Ok(collect_associated_paths(&presentation, session_ids))
}

fn collect_associated_paths(presentation: &UiPresentation, session_ids: &[&str]) -> Vec<String> {
    session_ids
        .iter()
        .filter_map(|id| presentation.threads.get(*id))
        .filter_map(|value| value["associatedWorktreePath"].as_str().map(str::to_owned))
        .collect()
}

/// 原子保存展示补丁；remove 只移除该实体的展示元数据，不删除会话或项目文件。
#[tauri::command]
pub fn ui_presentation_patch(
    app: AppHandle,
    scope: String,
    id: Option<String>,
    patch: Value,
    remove: Option<bool>,
) -> Result<UiPresentation, String> {
    ui_presentation_batch(
        app,
        vec![UiPresentationPatch {
            scope,
            id,
            patch,
            remove: remove.unwrap_or(false),
        }],
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UiPresentationPatch {
    scope: String,
    id: Option<String>,
    patch: Value,
    #[serde(default)]
    remove: bool,
}

/// 空间批量分配/排序在内存全部校验后一次落盘，失败不能留下部分修改。
fn apply_batch(
    state: &mut UiPresentation,
    patches: Vec<UiPresentationPatch>,
) -> Result<(), String> {
    if patches.is_empty() || patches.len() > 4096 {
        return Err("界面元数据批量补丁数量无效".to_owned());
    }
    let mut total = 0;
    for item in &patches {
        validate_patch(&item.scope, item.id.as_deref(), &item.patch)?;
        if item.remove && item.scope == "settings" {
            return Err("不能删除设置作用域".to_owned());
        }
        total += serde_json::to_vec(&item.patch)
            .map_err(|error| error.to_string())?
            .len();
        if total > MAX_BYTES as usize {
            return Err("界面元数据批量补丁过大".to_owned());
        }
    }
    for UiPresentationPatch {
        scope,
        id,
        patch,
        remove,
    } in patches
    {
        if scope == "settings" {
            let mut settings = Value::Object(std::mem::take(&mut state.settings));
            merge(&mut settings, patch);
            state.settings = settings.as_object().ok_or("界面设置格式无效")?.clone();
        } else {
            let entries = match scope.as_str() {
                "projects" => &mut state.projects,
                "threads" => &mut state.threads,
                "spaces" => &mut state.spaces,
                _ => unreachable!(),
            };
            let id = id.ok_or("缺少界面实体标识")?;
            if remove {
                entries.remove(&id);
            } else {
                merge(
                    entries
                        .entry(id)
                        .or_insert_with(|| Value::Object(Map::new())),
                    patch,
                );
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn ui_presentation_batch(
    app: AppHandle,
    patches: Vec<UiPresentationPatch>,
) -> Result<UiPresentation, String> {
    let _guard = IO_LOCK.lock().map_err(|_| "界面元数据锁已损坏")?;
    let mut state = load(&app)?;
    apply_batch(&mut state, patches)?;
    let bytes = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("界面元数据超过大小限制".to_owned());
    }
    let path = crate::storage::root_dir(&app)
        .map_err(|error| error.to_string())?
        .join("ui-presentation.json");
    crate::storage::atomic_write_private(&path, &bytes).map_err(|error| error.to_string())?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn worktree_removal_checks_live_associations_without_reviving_deleted_threads() {
        let mut state = UiPresentation::default();
        state.threads.insert(
            "live".into(),
            json!({"associatedWorktreePath":"D:/live-tree"}),
        );
        state.threads.insert(
            "deleted".into(),
            json!({"associatedWorktreePath":"D:/deleted-tree"}),
        );
        state
            .threads
            .insert("cleared".into(), json!({"associatedWorktreePath":null}));
        assert_eq!(
            collect_associated_paths(&state, &["live", "cleared", "unknown"]),
            vec!["D:/live-tree"]
        );
        assert!(collect_associated_paths(&state, &[]).is_empty());
    }

    #[test]
    fn skill_toggles_restore_and_block_actual_loading_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        for directory in [
            root.path().join("skills/proof"),
            project.join(".agents/skills/proof"),
        ] {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("SKILL.md"),
                "---\nname: proof\ndescription: synthetic proof\n---\nbody",
            )
            .unwrap();
        }
        let mut state = UiPresentation::default();
        state
            .settings
            .insert("skills".into(), json!({"disabled": [" PROOF ", "proof"]}));
        let path = root.path().join("ui-presentation.json");
        crate::storage::atomic_write_private(&path, &serde_json::to_vec(&state).unwrap()).unwrap();
        let disabled = disabled_skill_names(root.path()).unwrap();
        assert_eq!(
            disabled,
            std::collections::BTreeSet::from(["proof".to_owned()])
        );
        let config = keencode_skills::SkillDiscoveryConfig::new(root.path(), &project)
            .with_disabled_names(disabled);
        let catalog = keencode_skills::discover_skills(&config).unwrap();
        assert_eq!(catalog.entries().len(), 1);
        assert!(!catalog.entries()[0].enabled);
        assert!(catalog.load("proof").is_err());
        state
            .settings
            .insert("skills".into(), json!({"disabled": []}));
        crate::storage::atomic_write_private(&path, &serde_json::to_vec(&state).unwrap()).unwrap();
        let config = keencode_skills::SkillDiscoveryConfig::new(root.path(), &project)
            .with_disabled_names(disabled_skill_names(root.path()).unwrap());
        assert!(
            keencode_skills::discover_skills(&config)
                .unwrap()
                .load("proof")
                .is_ok()
        );
        for invalid in [
            json!({"disabled": [42]}),
            json!({"disabled": [" "]}),
            json!({"disabled": ["x\ny"]}),
            json!({"disabled": [], "extra": true}),
        ] {
            assert!(validate_patch("settings", None, &json!({"skills": invalid})).is_err());
        }
    }

    #[test]
    fn batch_rejects_invalid_member_before_applying_any_change() {
        let mut state = UiPresentation::default();
        let patch = |scope: &str, id: &str, patch: Value| UiPresentationPatch {
            scope: scope.to_owned(),
            id: Some(id.to_owned()),
            patch,
            remove: false,
        };
        let invalid = vec![
            patch("projects", "a", json!({"spaceId":"space-a"})),
            patch("projects", "b", json!({"spaceId":42})),
        ];
        assert!(apply_batch(&mut state, invalid).is_err());
        assert!(state.projects.is_empty());
        apply_batch(
            &mut state,
            vec![
                patch("projects", "a", json!({"spaceId":"space-a"})),
                patch("projects", "b", json!({"spaceId":"space-a"})),
            ],
        )
        .unwrap();
        assert_eq!(state.projects["a"]["spaceId"], "space-a");
        assert_eq!(state.projects["b"]["spaceId"], "space-a");
    }

    #[test]
    fn presentation_cannot_write_runtime_facts_or_secret_fields() {
        assert!(validate_patch("threads", Some("session-a"), &json!({"messages": []})).is_err());
        assert!(validate_patch("settings", None, &json!({"apiKey": "secret"})).is_err());
        assert!(
            validate_patch(
                "settings",
                None,
                &json!({"providers": {"keencode": {"api_key": "secret"}}})
            )
            .is_err()
        );
        assert!(
            validate_patch("projects", Some("project-a"), &json!({"isPinned": "true"})).is_err()
        );
        assert!(validate_patch("projects", Some(""), &json!({"isPinned": true})).is_err());
        assert!(
            validate_patch(
                "projects",
                Some("project-a"),
                &json!({"isPinned": true, "spaceId": null})
            )
            .is_ok()
        );
    }

    #[test]
    fn nested_patch_preserves_other_values_and_explicit_null() {
        let mut settings = json!({"providers": {"a": {"enabled": true, "customModels": ["one"]}, "b": {"enabled": false}}});
        merge(
            &mut settings,
            json!({"providers": {"a": {"enabled": false}}, "onboardingCompletedAt": null}),
        );
        assert_eq!(settings["providers"]["a"]["customModels"], json!(["one"]));
        assert_eq!(settings["providers"]["b"]["enabled"], false);
        assert_eq!(settings["onboardingCompletedAt"], Value::Null);
    }
}
