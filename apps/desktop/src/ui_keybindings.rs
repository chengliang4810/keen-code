//! 原快捷键页的配置文件读写；仅保存界面规则，不注册系统热键或执行命令。
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Mutex};
use tauri::AppHandle;

const MAX_BYTES: u64 = 128 * 1024;
static IO_LOCK: Mutex<()> = Mutex::new(());

/// 规则沿用原页面格式。宿主约束结构与体积，语法由原页面数据适配解析为 AST。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    key: String,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    when: Option<String>,
}

#[derive(Serialize)]
pub struct Config {
    path: String,
    revision: String,
    rules: Vec<Rule>,
}

/// expectedRevision 绑定实际文件内容；冷启动或并发窗口不能盲目覆盖较新规则。
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SaveInput {
    expected_revision: String,
    rules: Vec<Rule>,
}

fn validate(rules: &[Rule]) -> Result<(), String> {
    if rules.len() > 256 {
        return Err("快捷键规则最多 256 条".into());
    }
    for rule in rules {
        for (name, text, limit) in [
            ("按键", rule.key.as_str(), 64),
            ("命令", rule.command.as_str(), 64),
        ] {
            if text.trim().is_empty()
                || text != text.trim()
                || text.chars().count() > limit
                || text.chars().any(char::is_control)
            {
                return Err(format!("快捷键{name}无效"));
            }
        }
        if rule.when.as_ref().is_some_and(|value| {
            value.trim().is_empty()
                || value != value.trim()
                || value.chars().count() > 256
                || value.chars().any(char::is_control)
        }) {
            return Err("快捷键条件无效".into());
        }
    }
    Ok(())
}

fn load(root: &Path) -> Result<Config, String> {
    let path = root.join("keybindings.json");
    let bytes = crate::storage::read_private_bytes_bounded(&path, MAX_BYTES, "快捷键配置")
        .map_err(|error| error.to_string())?;
    let rules = match &bytes {
        Some(bytes) => serde_json::from_slice::<Vec<Rule>>(bytes)
            .map_err(|error| format!("快捷键配置格式无效：{error}"))?,
        None => Vec::new(),
    };
    validate(&rules)?;
    // 不存在的文件与真实空数组区分开，防止首次创建竞态覆盖另一个窗口的文件。
    let revision = bytes.as_ref().map_or_else(
        || "absent".into(),
        |bytes| format!("{:x}", Sha256::digest(bytes)),
    );
    Ok(Config {
        path: crate::path_utils::path_to_frontend(&path),
        revision,
        rules,
    })
}

fn save(root: &Path, input: SaveInput) -> Result<Config, String> {
    validate(&input.rules)?;
    let current = load(root)?;
    if current.revision != input.expected_revision {
        return Err("快捷键配置已被其他窗口修改，请重新打开设置后重试".into());
    }
    let bytes = serde_json::to_vec_pretty(&input.rules).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("快捷键配置超过体积限制".into());
    }
    crate::storage::atomic_write_private(&root.join("keybindings.json"), &bytes)
        .map_err(|error| error.to_string())?;
    load(root)
}

/// 实际文件路径来自宿主数据根，不能被前端替换为任意文件系统路径。
#[tauri::command(async)]
pub fn ui_keybindings_get(app: AppHandle) -> Result<Config, String> {
    let _guard = IO_LOCK.lock().map_err(|_| "快捷键配置锁损坏")?;
    load(&crate::storage::root_dir(&app).map_err(|error| error.to_string())?)
}

/// 宿主锁覆盖读取、版本校验与原子替换；失败保留原配置。
#[tauri::command(async)]
pub fn ui_keybindings_save(app: AppHandle, input: SaveInput) -> Result<Config, String> {
    let _guard = IO_LOCK.lock().map_err(|_| "快捷键配置锁损坏")?;
    save(
        &crate::storage::root_dir(&app).map_err(|error| error.to_string())?,
        input,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_rules_reject_stale_and_invalid_updates_without_overwriting() {
        let root = tempfile::tempdir().unwrap();
        let first = load(root.path()).unwrap();
        assert_eq!(first.revision, "absent");
        let rule = Rule {
            key: "mod+shift+k".into(),
            command: "sidebar.toggle".into(),
            when: Some("!terminalFocus".into()),
        };
        let saved = save(
            root.path(),
            SaveInput {
                expected_revision: first.revision.clone(),
                rules: vec![rule.clone()],
            },
        )
        .unwrap();
        assert_eq!(load(root.path()).unwrap().rules, vec![rule]);
        let bytes = std::fs::read(&saved.path).unwrap();
        assert!(
            save(
                root.path(),
                SaveInput {
                    expected_revision: first.revision,
                    rules: vec![]
                }
            )
            .is_err()
        );
        assert!(
            save(
                root.path(),
                SaveInput {
                    expected_revision: saved.revision,
                    rules: vec![Rule {
                        key: " ".into(),
                        command: "sidebar.toggle".into(),
                        when: None
                    }]
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&saved.path).unwrap(), bytes);
    }

    #[test]
    fn corrupt_file_is_reported_and_preserved() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("keybindings.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(load(root.path()).is_err());
        assert!(
            save(
                root.path(),
                SaveInput {
                    expected_revision: "absent".into(),
                    rules: vec![]
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"not json");
    }
}
