//! 显式启用的开发评测入口，复用桌面装配，不启动窗口或加载个人扩展。
use super::*;
use std::io::{Read, Write};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct BenchmarkTimeout;

impl fmt::Display for BenchmarkTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("benchmark timed out")
    }
}

impl Error for BenchmarkTimeout {}

struct BenchmarkEmitter(Mutex<std::fs::File>);
impl DeliveryEmitter for BenchmarkEmitter {
    fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError> {
        let mut file = self
            .0
            .lock()
            .map_err(|_| AgentRuntimeError::InitializationFailed)?;
        serde_json::to_writer(&mut *file, delivery)
            .map_err(|_| AgentRuntimeError::InitializationFailed)?;
        file.write_all(b"\n")
            .map_err(|_| AgentRuntimeError::InitializationFailed)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    cwd: PathBuf,
    storage: PathBuf,
    prompts: Vec<String>,
    model: String,
    base_url: String,
    api_backend: String,
    tool_allowlist: Option<Vec<String>>,
    timeout_ms: u64,
    context_window_tokens: Option<u64>,
    max_output_tokens: Option<u32>,
    #[serde(default)]
    supports_vision: bool,
}

/// 从 stdin 读取一次隔离评测请求；凭据仅从环境读取，stdout 输出结果 JSON。
pub async fn run() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: Request = serde_json::from_str(&input)?;
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_millis(request.timeout_ms))
        .context("timeoutMs is too large")?;
    anyhow::ensure!(
        !request.prompts.is_empty() && request.timeout_ms > 0,
        "empty prompts or timeout"
    );
    anyhow::ensure!(
        request.cwd.is_absolute() && request.storage.is_absolute(),
        "absolute paths required"
    );
    // 每次运行必须是新目录，防止误复用个人会话或旧评测历史。
    std::fs::create_dir(&request.storage)?;
    let model = request.model;
    let max_output_tokens = request.max_output_tokens.unwrap_or(8192);
    anyhow::ensure!(max_output_tokens > 0, "maxOutputTokens must be positive");
    let provider = providers::CustomProvider {
        chat_output_token_field: Default::default(),
        read_timeout_seconds: 300,
        id: "benchmark".into(),
        name: "Benchmark".into(),
        models: vec![model.clone()],
        api_backend: request.api_backend,
        base_url: request.base_url,
        api_key: Some(
            std::env::var("KEENCODE_BENCH_API_KEY")
                .context("KEENCODE_BENCH_API_KEY is required")?,
        ),
        context_windows: request
            .context_window_tokens
            .map(|n| [(model.clone(), n)].into_iter().collect())
            .unwrap_or_default(),
        context_1m: Default::default(),
        supports_vision: [(model.clone(), request.supports_vision)]
            .into_iter()
            .collect(),
        max_output_tokens: [(model.clone(), max_output_tokens)].into_iter().collect(),
    };
    let registry = ProviderRegistry::new();
    let mut provider_config = providers::runtime_provider_config(&provider)?;
    // The benchmark already owns the task deadline; do not truncate a live stream at 300s.
    provider_config.request_timeout = Some(Duration::from_millis(request.timeout_ms));
    registry.replace_all(vec![keencode_provider::ProviderRegistration::new(
        provider_config,
        "Benchmark",
        "benchmark",
        keencode_provider::ProviderModelPolicy::Enumerated {
            models: vec![model.clone()],
        },
    )?])?;
    let emitter = Arc::new(BenchmarkEmitter(Mutex::new(std::fs::File::create(
        request.storage.join("acp.jsonl"),
    )?)));
    let mut runtime = AgentRuntime::new_with_registry(&request.storage, emitter, registry)?;
    runtime.benchmark_tool_allowlist = request.tool_allowlist;
    let runtime = Arc::new(runtime);
    let work = async {
        let session = runtime.open_or_create_session(&request.cwd, None, "benchmark")?;
        let id = session.session_id().as_str();
        runtime.set_session_model(id, "benchmark-model", "benchmark", &model)?;
        for (index, prompt) in request.prompts.iter().enumerate() {
            if tokio::time::Instant::now() >= deadline {
                return Err(BenchmarkTimeout.into());
            }
            let turn_id = format!("benchmark-{index}");
            runtime
                .start_root_turn(id, &turn_id, prompt, RootTurnOptions::default())
                .await?;
            while runtime.session_has_active_work(id)? {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let snapshot = session.snapshot()?;
            anyhow::ensure!(
                snapshot.state.turns[&ResourceTurnId::new(&turn_id)?].status
                    == TurnStatus::Completed,
                "benchmark turn did not complete"
            );
        }
        Ok(
            json!({"model":model,"logPath":request.storage.join("sessions").join(id).join("events.jsonl")}),
        )
    };
    let result = complete_benchmark(deadline, CLEANUP_TIMEOUT, work, async {
        // shutdown 同时取消根/子 Agent 和后台 Shell，也覆盖启动屏障尚未完成的情况。
        runtime
            .shutdown()
            .await
            .context("benchmark shutdown failed")
    })
    .await?;
    println!("{result}");
    Ok(())
}

/// 工作期限涵盖启动及执行；失败或超时也必须尝试有界清理，清理失败不能输出成功。
async fn complete_benchmark<T>(
    deadline: tokio::time::Instant,
    cleanup_timeout: Duration,
    work: impl std::future::Future<Output = anyhow::Result<T>>,
    cleanup: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<T> {
    let result = if tokio::time::Instant::now() >= deadline {
        Err(anyhow::Error::new(BenchmarkTimeout))
    } else {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => Err(anyhow::Error::new(BenchmarkTimeout)),
            result = work => result,
        }
    };
    let result = result.and_then(|value| {
        anyhow::ensure!(tokio::time::Instant::now() < deadline, BenchmarkTimeout);
        Ok(value)
    });
    let cleanup = tokio::time::timeout(cleanup_timeout, cleanup)
        .await
        .unwrap_or_else(|_| {
            Err(anyhow::Error::new(BenchmarkTimeout).context("benchmark cleanup timed out"))
        });
    match (result, cleanup) {
        (Err(error), Err(cleanup)) if cleanup.is::<BenchmarkTimeout>() => {
            Err(cleanup.context(format!("work also failed: {error}")))
        }
        (Err(error), Err(cleanup)) => Err(error.context(format!("cleanup also failed: {cleanup}"))),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn root_allowlist_preserves_child_snapshot_and_rejects_invalid_names() {
        let project = tempfile::tempdir().unwrap();
        for names in [
            Some(vec!["Read", "spawn_agent", "Goal"]),
            None,
            Some(vec![]),
            Some(vec!["unknown-tool"]),
            Some(vec!["Read", "Read"]),
        ] {
            let storage = tempfile::tempdir().unwrap();
            let mut runtime = AgentRuntime::new(
                storage.path(),
                Arc::new(BenchmarkEmitter(Mutex::new(tempfile::tempfile().unwrap()))),
            )
            .unwrap();
            runtime.benchmark_tool_allowlist = names
                .as_ref()
                .map(|names| names.iter().map(|name| (*name).to_owned()).collect());
            let runtime = Arc::new(runtime);
            let session = runtime
                .open_or_create_session(project.path(), None, "benchmark-test")
                .unwrap();
            runtime
                .ensure_session_delivery(session.session_id().as_str())
                .unwrap();
            let collaboration = runtime.ensure_collaboration_runtime(
                &session,
                RootAgentSeed {
                    model: "test-model".into(),
                    reasoning_effort: None,
                    plan_guard: PlanGuard::inactive(),
                },
            );
            if matches!(names.as_deref(), Some(["unknown-tool"] | ["Read", "Read"])) {
                assert!(collaboration.is_err());
                runtime.shutdown().await.unwrap();
                continue;
            }
            let collaboration = collaboration.unwrap();
            let mut profile = AgentProfile {
                model: "test-model".into(),
                reasoning_effort: None,
                plan_guard: PlanGuard::inactive(),
                cwd: project.path().to_path_buf(),
                worktree_lease: None,
                tool_snapshot: vec![],
            };
            let assemble = |profile: &AgentProfile, can_spawn_agent| {
                runtime
                    .assemble_agent_tools(
                        &collaboration.execution,
                        collaboration.coordinator.clone(),
                        profile,
                        "general-purpose",
                        profile.plan_guard,
                        AgentCapabilities { can_spawn_agent },
                    )
                    .unwrap()
                    .0
            };
            let root = assemble(&profile, true);
            profile.tool_snapshot = root
                .definitions()
                .into_iter()
                .map(|tool| tool.name)
                .collect();
            if let Some(names) = names {
                assert_eq!(root.len(), names.len());
            } else {
                // 前一个 Runtime 的白名单不能影响新的无白名单实例。
                assert!(root.len() > 3);
            }
            let child = assemble(&profile, false)
                .select_exact(&runtime_tool_snapshot(&profile, false))
                .unwrap();
            let child_names = child
                .definitions()
                .into_iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>();
            assert!(
                !child_names
                    .iter()
                    .any(|name| name == "spawn_agent" || name == "Goal")
            );
            assert!(
                child_names
                    .iter()
                    .all(|name| profile.tool_snapshot.contains(name))
            );
            assert_eq!(child_names.contains(&"Read".to_owned()), !root.is_empty());
            runtime.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn timeout_covers_startup_and_always_runs_cleanup() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            AgentRuntime::new(
                storage.path(),
                Arc::new(BenchmarkEmitter(Mutex::new(tempfile::tempfile().unwrap()))),
            )
            .unwrap(),
        );
        let session = runtime
            .open_or_create_session(project.path(), None, "blocked-startup")
            .unwrap();
        let gate = Arc::new(AsyncMutex::new(()));
        runtime
            .turn_start_gates
            .lock()
            .unwrap()
            .insert(session.session_id().as_str().to_owned(), gate.clone());
        let locked = gate.lock().await;
        let error = complete_benchmark(
            tokio::time::Instant::now() + Duration::from_millis(20),
            Duration::from_secs(1),
            async {
                runtime
                    .start_root_turn(
                        session.session_id().as_str(),
                        "blocked-turn",
                        "test",
                        RootTurnOptions::default(),
                    )
                    .await
                    .map_err(anyhow::Error::from)
            },
            async {
                drop(locked);
                runtime.shutdown().await.map_err(anyhow::Error::from)
            },
        )
        .await
        .unwrap_err();
        assert!(error.is::<BenchmarkTimeout>());
        assert!(runtime.closed.load(Ordering::Acquire));
        assert!(session.snapshot().unwrap().state.turns.is_empty());

        let started = AtomicBool::new(false);
        let error = complete_benchmark(
            tokio::time::Instant::now(),
            Duration::from_secs(1),
            async {
                started.store(true, Ordering::Release);
                Ok(())
            },
            async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(error.is::<BenchmarkTimeout>());
        assert!(!started.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cleanup_is_bounded_and_failure_never_becomes_success() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let error = complete_benchmark(
            deadline,
            Duration::from_millis(20),
            async { Ok("finished") },
            std::future::pending::<anyhow::Result<()>>(),
        )
        .await
        .unwrap_err();
        assert!(error.is::<BenchmarkTimeout>());
        assert!(error.to_string().contains("cleanup timed out"));
        let error = complete_benchmark(
            deadline,
            Duration::from_millis(20),
            async { Err::<(), _>(anyhow::anyhow!("work error")) },
            std::future::pending::<anyhow::Result<()>>(),
        )
        .await
        .unwrap_err();
        assert!(error.is::<BenchmarkTimeout>());
        assert!(error.to_string().contains("work error"));
        let error = complete_benchmark(
            deadline,
            Duration::from_secs(1),
            async { Err::<(), _>(anyhow::Error::new(BenchmarkTimeout)) },
            async { anyhow::bail!("cleanup error") },
        )
        .await
        .unwrap_err();
        assert!(error.is::<BenchmarkTimeout>());
        assert!(error.to_string().contains("cleanup error"));
        assert!(
            complete_benchmark(deadline, Duration::from_secs(1), async { Ok(()) }, async {
                anyhow::bail!("cleanup error")
            },)
            .await
            .is_err()
        );
        assert_eq!(
            complete_benchmark(deadline, Duration::from_secs(1), async { Ok(42) }, async {
                Ok(())
            },)
            .await
            .unwrap(),
            42
        );
    }
}
