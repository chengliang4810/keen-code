//! Tests for config_lsp

use super::*;

#[test]
fn test_config_deserialization() {
    let json = r#"{
        "lspServers": {
            "rust-analyzer": {
                "name": "rust-analyzer",
                "command": "rust-analyzer",
                "args": ["--stdio"],
                "extensionToLanguage": {
                    ".rs": "rust"
                }
            }
        }
    }"#;
    let config: LspConfigFile = serde_json::from_str(json).unwrap();
    assert_eq!(config.lsp_servers.len(), 1);
    let ra = &config.lsp_servers["rust-analyzer"];
    assert_eq!(ra.command, "rust-analyzer");
    assert_eq!(ra.args, vec!["--stdio"]);
    assert_eq!(ra.extension_to_language.get(".rs").unwrap(), "rust");
}

#[test]
fn test_config_with_all_fields() {
    let json = r#"{
        "lspServers": {
            "typescript": {
                "name": "typescript-language-server",
                "command": "typescript-language-server",
                "args": ["--stdio"],
                "env": {"NODE_ENV": "production"},
                "extensionToLanguage": {
                    ".ts": "typescript",
                    ".tsx": "typescriptreact"
                },
                "initializationOptions": {"maxTsServerMemory": 8192},
                "disabled": false,
                "maxRestarts": 5,
                "startupTimeout": 30000
            }
        }
    }"#;
    let config: LspConfigFile = serde_json::from_str(json).unwrap();
    let ts = &config.lsp_servers["typescript"];
    assert_eq!(ts.max_restarts, Some(5));
    assert_eq!(ts.startup_timeout, Some(30000));
    assert_eq!(ts.disabled, Some(false));
    assert!(ts.initialization_options.is_some());
}

#[test]
fn test_expand_env_vars() {
    // 设置环境变量用于测试展开
    std::env::set_var("TEST_LSP_VAR", "expanded_value");
    let mut config = LspServerConfig {
        name: "test".to_string(),
        command: "${TEST_LSP_VAR}/bin/server".to_string(),
        args: vec!["--flag".to_string(), "${TEST_LSP_VAR}".to_string()],
        env: Some(HashMap::from([(
            "CUSTOM".to_string(),
            "${TEST_LSP_VAR}".to_string(),
        )])),
        extension_to_language: HashMap::new(),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    };
    expand_env_vars(&mut config);
    assert_eq!(config.command, "expanded_value/bin/server");
    assert_eq!(config.args[1], "expanded_value");
    assert_eq!(
        config.env.as_ref().unwrap().get("CUSTOM").unwrap(),
        "expanded_value"
    );
}

#[test]
fn test_expand_env_vars_missing() {
    let mut config = LspServerConfig {
        name: "test".to_string(),
        command: "${NONEXISTENT_VAR}/server".to_string(),
        args: vec![],
        env: None,
        extension_to_language: HashMap::new(),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    };
    expand_env_vars(&mut config);
    // 不存在的环境变量原样保留
    assert_eq!(config.command, "${NONEXISTENT_VAR}/server");
}

/// 同一 Host 模板必须为两个 Session 分别绑定 cwd 与 ID，且不得修改原模板。
#[test]
fn test_resolve_lsp_config_for_each_session() {
    let template = LspServerConfig {
        name: "test".to_string(),
        command: "${CLAUDE_PLUGIN_ROOT}/bin/server".to_string(),
        args: vec![
            "--project".to_string(),
            "${CLAUDE_PROJECT_DIR}".to_string(),
            "--session=${CLAUDE_SESSION_ID}".to_string(),
        ],
        env: Some(HashMap::from([
            (
                "CLAUDE_PLUGIN_ROOT".to_string(),
                "/plugins/test".to_string(),
            ),
            (
                "CLAUDE_PROJECT_DIR".to_string(),
                "/stale/project".to_string(),
            ),
            ("CLAUDE_SESSION_ID".to_string(), "stale-session".to_string()),
            (
                "SESSION_CACHE".to_string(),
                "${CLAUDE_PROJECT_DIR}/.cache/${CLAUDE_SESSION_ID}".to_string(),
            ),
        ])),
        extension_to_language: HashMap::new(),
        initialization_options: Some(serde_json::json!({
            "workspace": "${CLAUDE_PROJECT_DIR}",
            "session": "${CLAUDE_SESSION_ID}"
        })),
        disabled: Some(false),
        max_restarts: Some(5),
        startup_timeout: Some(120_000),
        source: None,
    };

    let first = resolve_lsp_config_for_session(&template, "/projects/one", "session-one");
    let second = resolve_lsp_config_for_session(&template, "/projects/two", "session-two");

    assert_eq!(first.command, "/plugins/test/bin/server");
    assert_eq!(first.args[1], "/projects/one");
    assert_eq!(first.args[2], "--session=session-one");
    assert_eq!(second.args[1], "/projects/two");
    assert_eq!(second.args[2], "--session=session-two");
    assert_eq!(
        first.env.as_ref().unwrap().get("SESSION_CACHE"),
        Some(&"/projects/one/.cache/session-one".to_string())
    );
    assert_eq!(
        second.env.as_ref().unwrap().get("SESSION_CACHE"),
        Some(&"/projects/two/.cache/session-two".to_string())
    );
    assert_eq!(
        first.initialization_options,
        Some(serde_json::json!({
            "workspace": "/projects/one",
            "session": "session-one"
        }))
    );
    assert_eq!(
        template.args[1], "${CLAUDE_PROJECT_DIR}",
        "共享模板不得被第一个 Session 改写"
    );
    assert_eq!(
        template
            .env
            .as_ref()
            .unwrap()
            .get("CLAUDE_SESSION_ID")
            .map(String::as_str),
        Some("stale-session")
    );
}

#[test]
fn test_config_default_values() {
    let json = r#"{"lspServers": {"test": {"command": "test-server"}}}"#;
    let config: LspConfigFile = serde_json::from_str(json).unwrap();
    let test = &config.lsp_servers["test"];
    assert!(test.args.is_empty());
    assert!(test.env.is_none());
    assert!(test.extension_to_language.is_empty());
    assert!(test.disabled.is_none());
    assert!(test.max_restarts.is_none());
}

#[test]
fn test_lsp_config_from_plugin_injects_plugin_root_env() {
    let install_path = Path::new("/tmp/plugin-root/my-plugin");
    let config = lsp_config_from_plugin(
        "my-plugin",
        "rust-analyzer",
        "rust-analyzer",
        &["--stdio".to_string()],
        install_path,
        HashMap::from([(".rs".to_string(), "rust".to_string())]),
    );
    // env 注入：配置含 CLAUDE_PLUGIN_ROOT（插件根）
    let env = config.env.as_ref().unwrap();
    assert_eq!(
        env.get("CLAUDE_PLUGIN_ROOT").map(|s| s.as_str()),
        Some("/tmp/plugin-root/my-plugin")
    );
    // 命名空间与 manifest 字段映射保留
    assert_eq!(config.name, "plugin:my-plugin:rust-analyzer");
    assert_eq!(config.command, "rust-analyzer");
    assert_eq!(config.args, vec!["--stdio"]);
    assert_eq!(config.extension_to_language.get(".rs").unwrap(), "rust");
    assert_eq!(
        config.source,
        Some(LspConfigSource::Plugin {
            plugin_name: "my-plugin".to_string()
        })
    );
}

#[test]
fn test_lsp_config_from_plugin_expands_plugin_root_in_command() {
    // 插件根相对命令：${CLAUDE_PLUGIN_ROOT} 仅存在于注入 env，进程环境未定义
    let install_path = Path::new("/tmp/plugin-root/my-plugin");
    let config = lsp_config_from_plugin(
        "my-plugin",
        "my-server",
        "${CLAUDE_PLUGIN_ROOT}/bin/server",
        &[
            "--config".to_string(),
            "${CLAUDE_PLUGIN_ROOT}/config.json".to_string(),
        ],
        install_path,
        HashMap::new(),
    );
    assert_eq!(config.command, "/tmp/plugin-root/my-plugin/bin/server");
    assert_eq!(config.args[1], "/tmp/plugin-root/my-plugin/config.json");
    // 注入 env 原样保留
    let env = config.env.as_ref().unwrap();
    assert_eq!(
        env.get("CLAUDE_PLUGIN_ROOT").map(|s| s.as_str()),
        Some("/tmp/plugin-root/my-plugin")
    );
}

#[test]
fn test_load_global_lsp_config_from_settings_json() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"config":{"lspServers":{"rust-analyzer":{"command":"rust-analyzer","args":["--stdio"]}}}}"#,
    )
    .unwrap();

    let config = load_global_lsp_config(&settings);
    assert_eq!(config.lsp_servers.len(), 1);
    let ra = &config.lsp_servers["rust-analyzer"];
    // name 以 settings.json 的 key 为准（装配/池侧服务器标识）
    assert_eq!(ra.name, "rust-analyzer");
    assert_eq!(ra.command, "rust-analyzer");
    assert_eq!(ra.args, vec!["--stdio"]);
    assert_eq!(
        ra.source,
        Some(LspConfigSource::Global(settings.to_path_buf()))
    );
}

#[test]
fn test_load_global_lsp_config_missing_or_invalid() {
    let temp = tempfile::tempdir().unwrap();
    // 文件不存在 → 空配置
    let missing = temp.path().join("missing.json");
    assert!(load_global_lsp_config(&missing).lsp_servers.is_empty());
    // 无 config.lspServers 字段 → 空配置
    let no_lsp = temp.path().join("settings.json");
    std::fs::write(&no_lsp, r#"{"config":{"mcpServers":{}}}"#).unwrap();
    assert!(load_global_lsp_config(&no_lsp).lsp_servers.is_empty());
    // 非法 JSON → 空配置（不 panic）
    let invalid = temp.path().join("invalid.json");
    std::fs::write(&invalid, "not-json").unwrap();
    assert!(load_global_lsp_config(&invalid).lsp_servers.is_empty());
}
