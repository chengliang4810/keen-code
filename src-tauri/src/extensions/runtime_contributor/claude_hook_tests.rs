//! Claude Code 协议的真实 shell 与生命周期回归。
use super::*;
#[cfg(unix)]
use keencode_agent::{
    AgentId, HookError, HookInvocationContext, SessionId, TurnCancellation, TurnId,
};
#[cfg(unix)]
use std::collections::HashSet;
#[cfg(unix)]
use std::sync::Mutex;

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
    let started = Arc::new(Mutex::new(HashSet::new()));
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
        lifecycle_start_state: LifecycleStartState::with_started(started.clone()),
        agent_type: "general-purpose".to_owned(),
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
    hook.turn_start_prepare(&context);
    assert_eq!(
        hook.turn_start(context.clone())
            .await
            .unwrap()
            .context
            .len(),
        2
    );
    hook.turn_start_delivered(&context);
    hook.turn_start_prepare(&context);
    assert_eq!(hook.turn_start(context).await.unwrap().context.len(), 1);
    assert!(
        started
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );
    let input: Value =
        serde_json::from_slice(&fs::read(root.path().join("startup-input.json")).unwrap()).unwrap();
    assert_eq!(input["hook_event_name"], "SessionStart");
    assert_eq!(input["session_id"], "session");
    assert_eq!(input["source"], "startup");
    assert_eq!(input["cwd"], root.path().to_string_lossy().as_ref());
}

#[cfg(unix)]
#[tokio::test]
async fn blocked_first_prompt_does_not_repeat_session_start() {
    let root = tempfile::tempdir().unwrap();
    let started = Arc::new(Mutex::new(HashSet::new()));
    let hook = NativeLifecycleHooks {
        hooks: vec![
            parse_command_hook(
                "test:session-start".to_owned(),
                HookPhase::SessionStart,
                None,
                r#"printf 'session-start\n' >> attempts.txt; printf '%s' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"session guidance"}}'"#
                    .to_owned(),
                root.path(),
            )
            .unwrap(),
            parse_command_hook(
                "test:user-prompt-submit".to_owned(),
                HookPhase::UserPromptSubmit,
                None,
                r#"printf 'user-prompt-submit\n' >> attempts.txt; printf '%s' '{"decision":"block","reason":"retry this prompt"}'"#
                    .to_owned(),
                root.path(),
            )
            .unwrap(),
        ],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: LifecycleStartState::with_started(started.clone()),
        agent_type: "general-purpose".to_owned(),
    };
    let mut registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    registry.register(Arc::new(hook)).unwrap();
    let runtime = HookRuntime::new(registry, HookLimits::default()).unwrap();
    let mut context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("blocked-prompt-session").unwrap(),
            turn_id: TurnId::new("blocked-prompt-turn-1").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "first prompt".to_owned(),
        has_history: false,
    };

    for turn_id in ["blocked-prompt-turn-1", "blocked-prompt-turn-2"] {
        context.invocation.turn_id = TurnId::new(turn_id).unwrap();
        let error = runtime
            .run_turn_start(context.clone(), &TurnCancellation::new())
            .await
            .expect_err("被阻断的首个 Prompt 应允许重试");
        assert!(matches!(
            error,
            HookError::Callback { code, .. } if code == "hook_prompt_blocked"
        ));
    }

    let attempts = fs::read_to_string(root.path().join("attempts.txt")).unwrap();
    assert_eq!(
        attempts
            .lines()
            .filter(|line| *line == "session-start")
            .count(),
        1,
        "UserPromptSubmit 阻断不能释放已完成的 SessionStart lease"
    );
    assert_eq!(
        attempts
            .lines()
            .filter(|line| *line == "user-prompt-submit")
            .count(),
        2
    );
    assert!(
        started
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_session_start_late_success_cannot_complete_reloaded_candidate() {
    let root = tempfile::tempdir().unwrap();
    let state = LifecycleStartState::new();
    let first_hook = NativeLifecycleHooks {
        hooks: vec![parse_command_hook(
            "test:late-session-start".to_owned(),
            HookPhase::SessionStart,
            None,
            r#"cat >/dev/null; printf started > entered; while [ ! -f release ]; do sleep 0.01; done; printf finished > finished; printf '%s' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"late"}}'"#
                .to_owned(),
            root.path(),
        )
        .unwrap()],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: state.clone(),
        agent_type: "general-purpose".to_owned(),
    };
    let mut first_registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    first_registry
        .register(Arc::new(first_hook))
        .expect("首个 SessionStart Hook 应成功注册");
    let first_runtime = HookRuntime::new(
        first_registry,
        HookLimits {
            max_callback_ms: 30_000,
            ..HookLimits::default()
        },
    )
    .unwrap();
    let first_context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("late-session").unwrap(),
            turn_id: TurnId::new("late-turn-1").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "first".to_owned(),
        has_history: false,
    };
    let cancellation = TurnCancellation::new();
    let task_cancellation = cancellation.clone();
    let first_task = tokio::spawn(async move {
        first_runtime
            .run_turn_start(first_context, &task_cancellation)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !root.path().join("entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("首个 Hook 应在取消前进入命令");
    cancellation.cancel();
    let first_result = first_task.await.unwrap();
    assert!(matches!(first_result, Err(HookError::Cancelled { .. })));
    assert!(
        !state
            .started()
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );

    let second_hook = NativeLifecycleHooks {
        hooks: vec![parse_command_hook(
            "test:reloaded-session-start".to_owned(),
            HookPhase::SessionStart,
            None,
            r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"reloaded"}}'"#
                .to_owned(),
            root.path(),
        )
        .unwrap()],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: state.clone(),
        agent_type: "general-purpose".to_owned(),
    };
    let mut second_registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    second_registry
        .register(Arc::new(second_hook))
        .expect("重载候选 SessionStart Hook 应成功注册");
    let second_runtime = HookRuntime::new(second_registry, HookLimits::default()).unwrap();
    let second_context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("late-session").unwrap(),
            turn_id: TurnId::new("late-turn-2").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "reload".to_owned(),
        has_history: true,
    };
    let additions = second_runtime
        .run_turn_start(second_context, &TurnCancellation::new())
        .await
        .unwrap();
    assert_eq!(additions.len(), 1);
    assert_eq!(additions[0].text, "reloaded");

    let completed_before_release = state.callback_completion_count();
    std::fs::write(root.path().join("release"), "release").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.callback_completion_count() <= completed_before_release {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("旧 Hook 应在 release 后完成回调 Future");
    assert!(root.path().join("finished").is_file());
    assert!(
        state
            .started()
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_prompt_hook_retries_session_start_after_late_completion() {
    let root = tempfile::tempdir().unwrap();
    let state = LifecycleStartState::new();
    let first_hook = NativeLifecycleHooks {
        hooks: vec![
            parse_command_hook(
                "test:session-start-before-prompt-cancel".to_owned(),
                HookPhase::SessionStart,
                None,
                r#"printf 'session-start\n' >> attempts.txt; printf '%s' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"session guidance"}}'"#
                    .to_owned(),
                root.path(),
            )
            .unwrap(),
            parse_command_hook(
                "test:prompt-blocked-by-cancel".to_owned(),
                HookPhase::UserPromptSubmit,
                None,
                r#"printf entered > prompt-entered; while [ ! -f release ]; do sleep 0.01; done; printf finished > prompt-finished; printf '%s' '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"prompt guidance"}}'"#
                    .to_owned(),
                root.path(),
            )
            .unwrap(),
        ],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: state.clone(),
        agent_type: "general-purpose".to_owned(),
    };
    let mut first_registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    first_registry
        .register(Arc::new(first_hook))
        .expect("首个两阶段 Hook 应成功注册");
    let first_runtime = HookRuntime::new(first_registry, HookLimits::default()).unwrap();
    let first_context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("cancelled-prompt-session").unwrap(),
            turn_id: TurnId::new("cancelled-prompt-turn-1").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "first".to_owned(),
        has_history: false,
    };
    let cancellation = TurnCancellation::new();
    let task_cancellation = cancellation.clone();
    let first_task = tokio::spawn(async move {
        first_runtime
            .run_turn_start(first_context, &task_cancellation)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !root.path().join("prompt-entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("UserPromptSubmit 应在取消前进入命令");
    cancellation.cancel();
    let first_result = first_task.await.unwrap();
    assert!(matches!(first_result, Err(HookError::Cancelled { .. })));
    assert!(
        !state
            .started()
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );

    let second_hook = NativeLifecycleHooks {
        hooks: vec![parse_command_hook(
            "test:session-start-after-prompt-cancel".to_owned(),
            HookPhase::SessionStart,
            None,
            r#"printf 'session-start\n' >> attempts.txt; printf '%s' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"reloaded guidance"}}'"#
                .to_owned(),
            root.path(),
        )
        .unwrap()],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: state.clone(),
        agent_type: "general-purpose".to_owned(),
    };
    let mut second_registry = HookRegistry::with_circuit_store(HookCircuitStore::new());
    second_registry
        .register(Arc::new(second_hook))
        .expect("重载候选 SessionStart Hook 应成功注册");
    let second_runtime = HookRuntime::new(second_registry, HookLimits::default()).unwrap();
    let second_context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("cancelled-prompt-session").unwrap(),
            turn_id: TurnId::new("cancelled-prompt-turn-2").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "reload".to_owned(),
        has_history: true,
    };
    let additions = second_runtime
        .run_turn_start(second_context, &TurnCancellation::new())
        .await
        .unwrap();
    assert_eq!(additions.len(), 1);
    assert_eq!(additions[0].text, "reloaded guidance");

    std::fs::write(root.path().join("release"), "release").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !root.path().join("prompt-finished").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("旧 UserPromptSubmit worker 应在释放后完成");
    let attempts = fs::read_to_string(root.path().join("attempts.txt")).unwrap();
    assert_eq!(
        attempts
            .lines()
            .filter(|line| *line == "session-start")
            .count(),
        2
    );
    assert!(
        state
            .started()
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn context_lifecycle_hooks_inject_at_registered_phases() {
    let root = tempfile::tempdir().unwrap();
    let context_hook = |name: &str, phase, matcher, context: &str| {
        parse_hook_spec(
            name.to_owned(),
            phase,
            matcher,
            json!({"type":"context", "context":context}),
            root.path(),
        )
        .unwrap()
    };
    let hook = NativeLifecycleHooks {
        hooks: vec![
            context_hook(
                "test:session-context",
                HookPhase::SessionStart,
                Some("startup".to_owned()),
                "session context",
            ),
            context_hook(
                "test:prompt-context",
                HookPhase::UserPromptSubmit,
                None,
                "prompt context",
            ),
            context_hook(
                "test:subagent-context",
                HookPhase::SubagentStart,
                Some("general-purpose".to_owned()),
                "subagent context",
            ),
        ],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: LifecycleStartState::new(),
        agent_type: "general-purpose".to_owned(),
    };
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("context-session").unwrap(),
            turn_id: TurnId::new("context-turn").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "hello".to_owned(),
        has_history: false,
    };

    hook.turn_start_prepare(&context);
    let first = hook.turn_start(context.clone()).await.unwrap();
    hook.turn_start_delivered(&context);
    assert_eq!(
        first
            .context
            .iter()
            .map(|addition| addition.text.as_str())
            .collect::<Vec<_>>(),
        ["session context", "prompt context"]
    );
    hook.turn_start_prepare(&context);
    let second = hook.turn_start(context).await.unwrap();
    assert_eq!(second.context[0].text, "prompt context");

    let child_context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("context-session").unwrap(),
            turn_id: TurnId::new("context-child-turn").unwrap(),
            source_agent_id: AgentId::new("child-agent").unwrap(),
        },
        prompt: "child task".to_owned(),
        has_history: false,
    };
    let child_first = hook.turn_start(child_context.clone()).await.unwrap();
    assert_eq!(child_first.context[0].text, "subagent context");
    assert!(hook.turn_start(child_context).await.unwrap().context.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_one_time_lifecycle_hook_is_retried() {
    let root = tempfile::tempdir().unwrap();
    let hook = NativeLifecycleHooks {
        hooks: vec![parse_command_hook(
            "test:retry-subagent".to_owned(),
            HookPhase::SubagentStart,
            None,
            r#"printf 'attempt\n' >> attempts.txt; printf '%s' '{"continue":false,"stopReason":"retry"}'"#
                .to_owned(),
            root.path(),
        )
        .unwrap()],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: LifecycleStartState::new(),
        agent_type: "worker".to_owned(),
    };
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("retry-session").unwrap(),
            turn_id: TurnId::new("retry-turn").unwrap(),
            source_agent_id: AgentId::new("retry-child").unwrap(),
        },
        prompt: "task".to_owned(),
        has_history: false,
    };

    for _ in 0..2 {
        hook.turn_start_prepare(&context);
        let error = hook
            .turn_start(context.clone())
            .await
            .expect_err("失败的一次性 Hook 必须保留重试机会");
        assert_eq!(error.code, "hook_stopped");
    }
    assert_eq!(
        fs::read_to_string(root.path().join("attempts.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[cfg(unix)]
#[tokio::test]
async fn failed_command_lifecycle_hook_is_retried() {
    let root = tempfile::tempdir().unwrap();
    let hook = NativeLifecycleHooks {
        hooks: vec![parse_command_hook(
            "test:failed-session".to_owned(),
            HookPhase::SessionStart,
            None,
            r#"printf 'attempt\n' >> attempts.txt; exit 1"#.to_owned(),
            root.path(),
        )
        .unwrap()],
        plan: PlanGuard::inactive(),
        started: Arc::new(Mutex::new(HashSet::new())),
        agent_type: "worker".to_owned(),
    };
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("failed-session").unwrap(),
            turn_id: TurnId::new("failed-turn").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "task".to_owned(),
        has_history: false,
    };

    for _ in 0..2 {
        let error = hook
            .turn_start(context.clone())
            .await
            .expect_err("失败的 SessionStart Hook 必须保留重试机会");
        assert_eq!(error.code, "hook_command_failed");
    }
    assert_eq!(
        fs::read_to_string(root.path().join("attempts.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(
        !hook
            .started
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn outer_timeout_releases_lifecycle_lease() {
    let root = tempfile::tempdir().unwrap();
    let hook = NativeLifecycleHooks {
        hooks: vec![parse_command_hook(
            "test:timeout-session".to_owned(),
            HookPhase::SessionStart,
            None,
            r#"touch started; sleep 10"#.to_owned(),
            root.path(),
        )
        .unwrap()],
        plan: PlanGuard::inactive(),
        started: Arc::new(Mutex::new(HashSet::new())),
        agent_type: "worker".to_owned(),
    };
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("timeout-session").unwrap(),
            turn_id: TurnId::new("timeout-turn").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "task".to_owned(),
        has_history: false,
    };

    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            hook.turn_start(context.clone())
        )
        .await
        .is_err()
    );
    assert!(
        !hook
            .started
            .lock()
            .unwrap()
            .contains(&("root".to_owned(), HookPhase::SessionStart))
    );
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
        lifecycle_start_state: LifecycleStartState::new(),
        agent_type: "general-purpose".to_owned(),
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
    lifecycle.turn_start_prepare(&context);
    let result = lifecycle.turn_start(context.clone()).await.unwrap();
    lifecycle.turn_start_delivered(&context);
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

#[cfg(unix)]
#[tokio::test]
async fn subagent_start_runs_once_per_child_and_injects_context() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        parse_hook_phase("SubagentStart"),
        Some(HookPhase::SubagentStart)
    );
    let hook = NativeLifecycleHooks {
        hooks: vec![parse_command_hook(
            "test:subagent".to_owned(),
            HookPhase::SubagentStart,
            Some("^worker$".to_owned()),
            r#"cat > child-input.json; printf '%s' '{"hookSpecificOutput":{"hookEventName":"SubagentStart","additionalContext":"PONYTAIL MODE ACTIVE"}}'"#.to_owned(),
            root.path(),
        ).unwrap()],
        plan: PlanGuard::inactive(),
        lifecycle_start_state: LifecycleStartState::new(),
        agent_type: "worker".to_owned(),
    };
    let mut context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("session").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            source_agent_id: AgentId::new("root").unwrap(),
        },
        prompt: "task".to_owned(),
        has_history: true,
    };
    hook.turn_start_prepare(&context);
    assert!(
        hook.turn_start(context.clone())
            .await
            .unwrap()
            .context
            .is_empty()
    );
    hook.turn_start_delivered(&context);
    for child in ["child-one", "child-two"] {
        context.invocation.source_agent_id = AgentId::new(child).unwrap();
        hook.turn_start_prepare(&context);
        assert_eq!(
            hook.turn_start(context.clone())
                .await
                .unwrap()
                .context
                .len(),
            1
        );
        hook.turn_start_delivered(&context);
        hook.turn_start_prepare(&context);
        assert!(
            hook.turn_start(context.clone())
                .await
                .unwrap()
                .context
                .is_empty()
        );
        let input: Value =
            serde_json::from_slice(&fs::read(root.path().join("child-input.json")).unwrap())
                .unwrap();
        assert_eq!(input["hook_event_name"], "SubagentStart");
        assert_eq!(input["agent_id"], child);
        assert_eq!(input["agent_type"], "worker");
    }
    let nonmatching = NativeLifecycleHooks {
        agent_type: "explorer".to_owned(),
        ..hook
    };
    context.invocation.source_agent_id = AgentId::new("child-three").unwrap();
    nonmatching.turn_start_prepare(&context);
    assert!(
        nonmatching
            .turn_start(context.clone())
            .await
            .unwrap()
            .context
            .is_empty()
    );
    nonmatching.turn_start_delivered(&context);
}

/// 在隔离状态目录内执行已安装 ponytail 的真实子代理脚本。
#[cfg(unix)]
#[tokio::test]
#[ignore = "需要 KEENCODE_PONYTAIL_ROOT 指向已审阅的 ponytail 安装目录"]
async fn installed_ponytail_subagent_start_contract() {
    let root = PathBuf::from(std::env::var_os("KEENCODE_PONYTAIL_ROOT").expect("插件根目录"));
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join(".ponytail-active"), "full").unwrap();
    let manifest = crate::plugins::load_plugin_manifest(&root).unwrap();
    let plugin = crate::plugins::extract_components(
        PluginId::parse("ponytail@ponytail").unwrap(),
        &root,
        &manifest,
        project.path(),
        &BTreeMap::new(),
        &crate::plugins::ResolvedUserConfig::default(),
    )
    .unwrap();
    let (hooks, diagnostics) = parse_plugin_hooks(&PluginRuntimeSnapshot {
        plugins: vec![plugin],
    });
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let hooks = hooks
        .into_iter()
        .filter_map(|hook| match hook {
            HookSpec::Command(mut spec) if spec.phase == HookPhase::SubagentStart => {
                spec.current_dir = project.path().to_path_buf();
                spec.environment.insert(
                    "PLUGIN_DATA".to_owned(),
                    project.path().display().to_string(),
                );
                spec.environment.insert(
                    "PONYTAIL_SUBAGENT_MATCHER".to_owned(),
                    "^general-purpose$".to_owned(),
                );
                Some(HookSpec::Command(spec))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!hooks.is_empty());
    let lifecycle = NativeLifecycleHooks {
        hooks,
        plan: PlanGuard::inactive(),
        lifecycle_start_state: LifecycleStartState::new(),
        agent_type: "general-purpose".to_owned(),
    };
    let context = TurnStartHookContext {
        invocation: HookInvocationContext {
            session_id: SessionId::new("ponytail-session").unwrap(),
            turn_id: TurnId::new("ponytail-turn").unwrap(),
            source_agent_id: AgentId::new("ponytail-child").unwrap(),
        },
        prompt: "Implement the assigned task".to_owned(),
        has_history: true,
    };
    lifecycle.turn_start_prepare(&context);
    let result = lifecycle.turn_start(context.clone()).await.unwrap();
    lifecycle.turn_start_delivered(&context);
    assert!(!result.context.is_empty());
    assert!(format!("{:?}", result.context).contains("Ponytail"));
}
