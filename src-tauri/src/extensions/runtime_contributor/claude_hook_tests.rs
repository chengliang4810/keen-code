//! Claude Code 协议的真实 shell 与生命周期回归。
use super::*;
use keencode_agent::{AgentId, HookInvocationContext, SessionId, TurnId};
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn standard_decisions_and_additional_context_are_applied() {
    let output = parse_pre_hook_output(
        json!({"hookSpecificOutput": {
            "hookEventName":"PreToolUse", "permissionDecision":"allow",
            "updatedInput":{"command":"pwd"}, "additionalContext":"checked"
        }})
        .to_string(),
    )
    .unwrap();
    assert!(
        matches!(output.action, PreToolUseAction::ModifyInput { input } if input["command"] == "pwd")
    );
    assert_eq!(output.context.len(), 1);
    let output = parse_pre_hook_output(
        json!({"hookSpecificOutput": {
            "permissionDecision":"deny", "permissionDecisionReason":"blocked"
        }})
        .to_string(),
    )
    .unwrap();
    assert!(matches!(output.action, PreToolUseAction::Block { message } if message == "blocked"));
    assert_eq!(
        parse_stop_hook_output(json!({"decision":"block", "reason":"tests missing"}).to_string())
            .unwrap()
            .action,
        StopHookAction::Continue
    );
    assert!(matches_tool(
        &Some("^mcp__.*__write$".to_owned()),
        "mcp__db__write"
    ));
    assert!(!matches_tool(&Some("^Read$".to_owned()), "read"));
}

#[cfg(unix)]
#[tokio::test]
async fn session_start_runs_once_and_prompt_hook_runs_each_turn() {
    let root = tempfile::tempdir().unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let mut hooks = Vec::new();
    for (phase, command) in [
        (
            HookPhase::SessionStart,
            r#"cat > startup-input.json; printf '%s' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"session guidance"}}'"#,
        ),
        (
            HookPhase::UserPromptSubmit,
            r#"printf '%s' '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"prompt guidance"}}'"#,
        ),
    ] {
        hooks.push(
            parse_command_hook(
                format!("test:{phase}"),
                phase,
                None,
                command.to_owned(),
                root.path(),
            )
            .unwrap(),
        );
    }
    let hook = NativeLifecycleHooks {
        hooks,
        plan: PlanGuard::inactive(),
        started: started.clone(),
    };
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("session").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "hello".to_owned(),
        has_history: false,
    };
    assert_eq!(
        hook.turn_start(context.clone())
            .await
            .unwrap()
            .context
            .len(),
        2
    );
    assert_eq!(hook.turn_start(context).await.unwrap().context.len(), 1);
    assert!(started.load(Ordering::Acquire));
    let input: Value =
        serde_json::from_slice(&fs::read(root.path().join("startup-input.json")).unwrap()).unwrap();
    assert_eq!(input["hook_event_name"], "SessionStart");
    assert_eq!(input["session_id"], "session");
    assert_eq!(input["source"], "startup");
    assert_eq!(input["cwd"], root.path().to_string_lossy().as_ref());
}

#[cfg(unix)]
#[tokio::test]
async fn exit_two_blocks_but_exit_one_is_non_blocking() {
    let root = tempfile::tempdir().unwrap();
    let HookSpec::Command(mut spec) = parse_command_hook(
        "test:exit".to_owned(),
        HookPhase::PreToolUse,
        None,
        "printf '%s' denied >&2; exit 2".to_owned(),
        root.path(),
    )
    .unwrap() else {
        panic!()
    };
    let result = execute_hook_command(&spec, &json!({})).await.unwrap();
    assert!(matches!(
        parse_pre_hook_output(result).unwrap().action,
        PreToolUseAction::Block { .. }
    ));
    spec.command = r#"printf '%s' '{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"json reason"}}'; printf '%s' stderr >&2; exit 2"#.to_owned();
    let result = execute_hook_command(&spec, &json!({})).await.unwrap();
    assert!(matches!(parse_pre_hook_output(result).unwrap().action,
        PreToolUseAction::Block { message } if message == "json reason"));
    spec.command = spec.command.replace("exit 2", "exit 1");
    let result = execute_hook_command(&spec, &json!({})).await.unwrap();
    assert!(matches!(parse_pre_hook_output(result).unwrap().action,
        PreToolUseAction::Block { message } if message == "json reason"));
    spec.command = spec.command.replace("exit 1", "exit 2");
    spec.phase = HookPhase::SessionStart;
    assert_eq!(execute_hook_command(&spec, &json!({})).await.unwrap(), "");
    spec.command = "exit 1".to_owned();
    assert_eq!(execute_hook_command(&spec, &json!({})).await.unwrap(), "");
}

#[cfg(unix)]
#[tokio::test]
async fn command_hook_invalid_output_is_non_blocking() {
    let root = tempfile::tempdir().unwrap();
    let HookSpec::Command(spec) = parse_command_hook(
        "test:invalid".to_owned(),
        HookPhase::PreToolUse,
        None,
        r#"printf '%s' '{"hookSpecificOutput":{"permissionDecision":"invalid"}}'"#.to_owned(),
        root.path(),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        run_command_hook(&spec, PlanGuard::inactive(), &json!({}))
            .await
            .unwrap(),
        ""
    );
}

/// 只读已安装插件；显式指定根目录后才执行其已审阅的 SessionStart 脚本。
#[cfg(unix)]
#[tokio::test]
#[ignore = "需要 KEENCODE_PLUGIN_COMPAT_ROOT 指向已审阅的 superpowers 安装目录"]
async fn installed_superpowers_session_start_contract() {
    let root = PathBuf::from(std::env::var_os("KEENCODE_PLUGIN_COMPAT_ROOT").expect("插件根目录"));
    let project = tempfile::tempdir().unwrap();
    let manifest = crate::plugins::load_plugin_manifest(&root).unwrap();
    let inventory = crate::plugins::inspect_plugin_components(&root, &manifest).unwrap();
    assert_eq!(inventory.skills, 14);
    assert_eq!(inventory.hooks, 1);
    let plugin = crate::plugins::extract_components(
        PluginId::parse("superpowers@claude-plugins-official").unwrap(),
        &root,
        &manifest,
        project.path(),
        &BTreeMap::new(),
        &crate::plugins::ResolvedUserConfig::default(),
    )
    .unwrap();
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![plugin],
    };
    let skills = keencode_skills::discover_skills(&runtime_skill_config_from_snapshot(
        project.path().to_path_buf(),
        project.path().to_path_buf(),
        snapshot.clone(),
    ))
    .unwrap();
    assert!(skills.load("superpowers:using-superpowers").is_ok());
    let (mut hooks, diagnostics) = parse_plugin_hooks(&snapshot);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    for hook in &mut hooks {
        if let HookSpec::Command(spec) = hook {
            spec.current_dir = project.path().to_path_buf();
        }
    }
    let lifecycle = NativeLifecycleHooks {
        hooks,
        plan: PlanGuard::inactive(),
        started: Arc::new(AtomicBool::new(false)),
    };
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("compat-session").unwrap(),
            turn_id: TurnId::new("compat-turn").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "compatibility probe".to_owned(),
        has_history: false,
    };
    let result = lifecycle.turn_start(context).await.unwrap();
    assert_eq!(result.context.len(), 1);
    assert!(result.context[0].text.contains("using-superpowers"));
}

#[test]
fn invalid_plugin_hook_is_isolated_and_unsupported_event_is_reported() {
    let plugin = |name: &str, hooks| crate::plugins::RuntimePlugin {
        id: PluginId::parse(&format!("{name}@official")).unwrap(),
        root: PathBuf::from("/plugins"),
        hook_environment: BTreeMap::new(),
        commands: Vec::new(),
        skills: Vec::new(),
        agents: Vec::new(),
        hooks: Some(hooks),
        mcp_servers: BTreeMap::new(),
        lsp_servers: Vec::new(),
    };
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![
            plugin(
                "healthy",
                json!({"PreToolUse":[{"hooks":[{"type":"command","command":"true"}]}], "SessionEnd":[]}),
            ),
            plugin(
                "broken",
                json!({"PreToolUse":[{"hooks":[{"type":"command","command":"true","timeout":-1}]}]}),
            ),
        ],
    };
    let (hooks, diagnostics) = parse_plugin_hooks(&snapshot);
    assert_eq!(hooks.len(), 1);
    assert_eq!(diagnostics.len(), 2);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "plugin_hooks_invalid")
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "plugin_hook_event_unsupported")
    );
}
