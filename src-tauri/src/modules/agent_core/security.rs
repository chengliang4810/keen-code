use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub(super) struct WorkspacePathPolicy(pub PathBuf);

impl rcode_tools::FileAccessPolicy for WorkspacePathPolicy {
    fn check(&self, path: &Path) -> Result<(), rcode_agent::ToolError> {
        check_file_path(&self.0, &path.to_string_lossy())
            .map(|_| ())
            .map_err(|e| rcode_agent::ToolError::permanent("path_denied", e))
    }
}

// 同时检查词法路径和真实目标，防止大小写、NTFS 流和符号链接绕过敏感路径规则。
pub(crate) fn check_file_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.contains('\0') {
        return Err("文件路径无效".into());
    }
    let candidate = Path::new(raw);
    if candidate
        .components()
        .any(|part| matches!(part, Component::Normal(name) if name.to_string_lossy().contains(':')))
    {
        return Err("文件路径不允许 NTFS 替代数据流".into());
    }
    let path = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    deny_secret_path(&path)?;
    let mut parent = path.as_path();
    let mut missing = Vec::new();
    while !parent.exists() {
        missing.push(
            parent
                .file_name()
                .ok_or("文件路径缺少有效父目录")?
                .to_os_string(),
        );
        parent = parent.parent().ok_or("文件路径缺少有效父目录")?;
    }
    let mut canonical = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    for part in missing.into_iter().rev() {
        canonical.push(part);
    }
    if canonical
        .components()
        .any(|p| matches!(p, Component::ParentDir))
        || !canonical.starts_with(root)
    {
        return Err("文件路径超出当前任务的工作区".into());
    }
    deny_secret_path(&canonical)?;
    Ok(canonical)
}

pub(crate) fn deny_secret_path(path: &Path) -> Result<(), String> {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let segments: Vec<_> = normalized
        .split('/')
        .map(|part| {
            part.split(':')
                .next()
                .unwrap_or_default()
                .trim_end_matches(['.', ' '])
        })
        .collect();
    if segments.iter().any(|part| {
        part.starts_with(".env")
            || matches!(
                *part,
                ".ssh"
                    | ".gnupg"
                    | ".aws"
                    | ".azure"
                    | ".kube"
                    | ".docker"
                    | ".git"
                    | ".terraform.d"
                    | "credentials"
                    | "credentials.json"
                    | "secrets.json"
                    | "keychains"
                    | "cookies"
                    | "passwd"
                    | "shadow"
                    | "known_hosts"
                    | "authorized_keys"
                    | ".netrc"
                    | "_netrc"
                    | ".npmrc"
                    | ".pypirc"
                    | ".pgpass"
                    | "htpasswd"
            )
            || ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"]
                .iter()
                .any(|prefix| {
                    part == prefix
                        || part
                            .strip_prefix(prefix)
                            .is_some_and(|tail| tail.starts_with(['.', '_', '-']))
                })
            || [
                ".pem",
                ".key",
                ".p12",
                ".pfx",
                ".asc",
                ".gpg",
                ".keystore",
                ".jks",
            ]
            .iter()
            .any(|suffix| part.ends_with(suffix))
            || ((part.starts_with("secret.") || part.starts_with("secrets."))
                && [".json", ".yml", ".yaml", ".toml", ".env"]
                    .iter()
                    .any(|suffix| part.ends_with(suffix)))
            || ((part.starts_with("service-account")
                || part.starts_with("service_account")
                || part.starts_with("serviceaccount"))
                && part.ends_with(".json"))
    }) || [
        "/.rcode/run",
        "/.rcode/cache/webview",
        "/.config/gh",
        "/.config/git",
        "/.config/gcloud",
        "/.config/op",
        "/library/keychains",
        "/appdata/roaming/microsoft/credentials",
    ]
    .iter()
    .any(|directory| {
        normalized.ends_with(directory) || normalized.contains(&format!("{directory}/"))
    }) {
        return Err("拒绝访问敏感文件或凭据目录".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_filters_secret_entries_and_batch_edits_are_atomic() {
        use rcode_agent::{
            AgentId, AgentTool, SessionId, ToolCallId, ToolContext, TurnCancellation, TurnId,
        };
        use rcode_tools::{GlobTool, GrepTool, MultiEditTool, ToolEnvironment};
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "SECRET needle").unwrap();
        std::fs::write(root.join("public.rs"), "public needle").unwrap();
        let environment = std::sync::Arc::new(
            ToolEnvironment::new(&root)
                .unwrap()
                .with_workspace_guard()
                .with_file_access_policy(std::sync::Arc::new(WorkspacePathPolicy(root.clone()))),
        );
        let context = ToolContext {
            session_id: SessionId::new("test-session").unwrap(),
            turn_id: TurnId::new("test-turn").unwrap(),
            source_agent_id: AgentId::new("main").unwrap(),
            tool_call_id: ToolCallId::new("test-call").unwrap(),
            cancellation: TurnCancellation::new(),
        };
        for tool in [
            std::sync::Arc::new(GlobTool::new(environment.clone()))
                as std::sync::Arc<dyn AgentTool>,
            std::sync::Arc::new(GrepTool::new(environment.clone())),
        ] {
            let output = tool.execute(context.clone(), json!({"path":".","pattern":if tool.definition().name == "Glob" { "**/*" } else { "needle" }})).await.unwrap();
            let text = serde_json::to_string(&output.content).unwrap();
            assert!(text.contains("public.rs"), "{text}");
            assert!(!text.contains("SECRET") && !text.contains(".env"), "{text}");
        }
        let path = root.join("batch.txt");
        let before = b"\xef\xbb\xbfa\r\nc\r\n";
        std::fs::write(&path, before).unwrap();
        let edit = MultiEditTool::new(environment);
        assert!(edit
            .execute(
                context.clone(),
                json!({"file_path":"batch.txt","edits":[
                    {"old_string":"a","new_string":"b"}, {"old_string":"missing","new_string":"x"}
                ]})
            )
            .await
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        edit.execute(
            context,
            json!({"file_path":"batch.txt","edits":[
                {"old_string":"a\nc","new_string":"b\nc"}, {"old_string":"c","new_string":"d"}
            ]}),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"\xef\xbb\xbfb\r\nd\r\n");
    }

    #[test]
    fn sensitive_paths_are_blocked_for_read_and_write() {
        for name in [
            ".env",
            ".ENV.local",
            ".env:stream",
            ".env.",
            ".ssh/key",
            "config/credentials.json",
            "cert.key",
            "/home/me/.rcode/credentials/secrets.json",
            "/home/me/.rcode/run/control.json",
            "C:\\Users\\me\\.rcode\\cache\\webview\\Default\\Preferences",
        ] {
            assert!(deny_secret_path(Path::new(name)).is_err(), "{name}");
        }
        assert!(deny_secret_path(Path::new("src/environment.rs")).is_ok());
        assert!(deny_secret_path(Path::new("/home/me/.rcode/skills/example/SKILL.md")).is_ok());
    }

    #[test]
    fn missing_targets_and_parent_traversal_keep_workspace_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(check_file_path(&root, "src/new/file.rs").is_ok());
        assert!(check_file_path(&root, "../outside.txt").is_err());
        assert!(check_file_path(&root, ".env.local").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cannot_expose_secret_or_escape_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "secret").unwrap();
        std::os::unix::fs::symlink(root.join(".env"), root.join("ordinary.txt")).unwrap();
        std::os::unix::fs::symlink(other.path(), root.join("outside")).unwrap();
        assert!(check_file_path(&root, "ordinary.txt").is_err());
        assert!(check_file_path(&root, "outside/new.txt").is_err());
    }
}
