//! KeenCode 三协议 Provider 真实兼容性测试器。
//!
//! 凭据只能从用户级 `providers.json` 读取。程序对配置模型和实时目录模型执行
//! 独立的 Messages、Chat Completions 与 Responses 能力请求，并在写盘前检查完整凭据。

#![deny(unsafe_code)]

mod config;
mod probe;
mod report;
mod wire_shape;

use std::cell::RefCell;
use std::path::Path;

use config::{ProvidersFile, RetryOptions, RuntimeOptions, escape_untrusted_inline_text};
use probe::{probe_provider, probe_selected_retry_cases};
use report::{
    CatalogCompletionState, LiveTestProcessLock, ProviderRecord, ReportStore, ResumeManifest,
    RunMetadata, RunReport, consolidate_retry_runs, new_run_id, timestamp,
    validate_catalog_completion,
};

/// 启动异步测试运行并使用非零退出码报告基础设施失败。
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!(
            "Provider 真实测试失败：{}",
            escape_untrusted_inline_text(&error)
        );
        std::process::exit(1);
    }
}

/// 执行配置加载、实时探测、逐项检查点和最终报告生成。
async fn run() -> Result<(), String> {
    let mut options = RuntimeOptions::parse()?;
    if let Some(run_dir) = options.verify_run_dir.clone() {
        return verify_completed_run(&options, &run_dir).await;
    }
    let _process_lock = LiveTestProcessLock::acquire(&options.user_data_directory)?;
    let providers_file = ProvidersFile::load(&options.config_path)?;
    let all_providers = providers_file.providers.iter().collect::<Vec<_>>();
    if let Some(consolidation) = options.consolidation.clone() {
        let output = consolidate_retry_runs(
            &consolidation.base_dir,
            &consolidation.retry_dir,
            &options.output_root,
            &all_providers,
            options.allow_unauthenticated_legacy_base,
        )
        .await?;
        println!(
            "离线补测合并完成：{}",
            escape_untrusted_inline_text(&output.display().to_string())
        );
        return Ok(());
    }
    if let Some(retry) = options.retry.clone() {
        return create_retry_run(options, &providers_file, &all_providers, retry).await;
    }

    if let Some(run_dir) = options.resume_dir.clone() {
        let store = ReportStore::open_resume(&run_dir)?;
        let manifest = store.load_resume_manifest(&all_providers)?;
        if let Some((provider_id, capabilities)) = manifest.retry_runtime_shape()? {
            options.reject_explicit_retry_resume_scope()?;
            options.apply_retry_runtime_shape(provider_id, capabilities)?;
        }
        let selected = providers_file.selected(&options.provider_filters)?;
        manifest.validate_identity(&options, &selected)?;
        store.load_and_verify_retry_selection_sidecar(&manifest, &selected)?;
        if manifest.finished {
            return Err("指定的恢复运行已经完成，拒绝重复发送真实请求".to_owned());
        }
        let repaired = store.repair_uncommitted_fixtures(&manifest, &selected)?;
        if repaired > 0 {
            println!("已清理 {repaired} 个 Fixture 已同步但提交日志未完成的恢复孤儿");
        }
        store.write_resume_manifest(&manifest, &selected)?;
        return execute_run(store, manifest, "恢复真实", &options, selected).await;
    }

    let selected = providers_file.selected(&options.provider_filters)?;
    let (store, manifest, run_kind) = match &options.recovery {
        Some(recovery) => {
            let source = ReportStore::open_recovery_source(&recovery.source_dir)?;
            let source_manifest = source.load_recovery_source_manifest(
                &selected,
                options.allow_unauthenticated_legacy_base,
            )?;
            let (store, manifest) = source
                .create_recovery_copy(
                    &source_manifest,
                    &options.output_root,
                    &options,
                    &selected,
                    &recovery.expected_source_executable_sha256,
                    options.allow_unauthenticated_legacy_base,
                )
                .await?;
            println!(
                "已从只读来源建立隔离恢复副本：导入 {} 条已确认记录，后续不会重发这些请求",
                manifest
                    .run
                    .recovery_lineage
                    .as_ref()
                    .map_or(0, |lineage| lineage.imported_records)
            );
            (store, manifest, "隔离恢复真实")
        }
        None => {
            let run_id = new_run_id()?;
            let store = ReportStore::create(&options.output_root, &run_id)?;
            let run = RunMetadata::new(run_id, &options)?;
            let manifest = ResumeManifest::new(run, &options, &selected)?;
            store.write_resume_manifest(&manifest, &selected)?;
            (store, manifest, "真实")
        }
    };
    execute_run(store, manifest, run_kind, &options, selected).await
}

/// 只读核验指定的已完成运行目录，不发起请求或写入任何运行产物。
async fn verify_completed_run(options: &RuntimeOptions, run_dir: &Path) -> Result<(), String> {
    let providers_file = ProvidersFile::load(&options.config_path)?;
    let providers = providers_file.providers.iter().collect::<Vec<_>>();
    let store = ReportStore::open_recovery_source(run_dir)?;
    let verification = store.verify_completed_run(&providers).await?;
    println!(
        "已完成运行只读核验通过：Provider {}，记录 {}，Fixture {}，Journal 序号 {}，封印产物 {}",
        verification.provider_count,
        verification.record_count,
        verification.fixture_count,
        verification.journal_sequence,
        verification.seal_artifact_count,
    );
    Ok(())
}

/// 从已完成来源创建独立补测目录，并只执行固定选择中的精确 tuple。
async fn create_retry_run(
    mut options: RuntimeOptions,
    providers_file: &ProvidersFile,
    all_providers: &[&config::ProviderEntry],
    retry: RetryOptions,
) -> Result<(), String> {
    let source = ReportStore::open_recovery_source(&retry.source_dir)?;
    let source_manifest = source
        .load_retry_source_manifest(all_providers, options.allow_unauthenticated_legacy_base)?;
    let selection = source
        .create_retry_selection(
            &source_manifest,
            all_providers,
            &retry.provider_id,
            retry.through_sequence,
            &retry.expected_source_executable_sha256,
        )
        .await?;

    let (provider_id, capabilities) = selection.runtime_shape();
    options.apply_retry_runtime_shape(provider_id, capabilities)?;
    let selected = providers_file.selected(&options.provider_filters)?;
    let run_id = new_run_id()?;
    let store = source.create_retry_target(&options.output_root, &run_id)?;
    let mut run = RunMetadata::new(run_id, &options)?;
    run.retry_lineage = Some(selection.lineage.clone());
    let mut manifest = ResumeManifest::new_retry(run, &options, &selected, selection.clone())?;
    manifest.register_candidates(
        &selection.lineage.provider_id,
        selection.cases.iter().map(|case| case.model.clone()),
    )?;
    store.write_retry_selection(&selection, &selected)?;
    store.write_resume_manifest(&manifest, &selected)?;
    store.complete_retry_target_setup()?;
    drop(source);
    execute_run(store, manifest, "精确补测", &options, selected).await
}

/// 执行普通矩阵或恢复清单中已经冻结的精确补测选择，并生成最终产物。
async fn execute_run(
    store: ReportStore,
    manifest: ResumeManifest,
    run_kind: &str,
    options: &RuntimeOptions,
    selected: Vec<&config::ProviderEntry>,
) -> Result<(), String> {
    let reusable_records = store.reusable_records(&manifest, &selected).await?;
    let run_id = manifest.run.run_id.clone();
    let retry_selection = manifest.retry_selection().cloned();
    let manifest = RefCell::new(manifest);
    let mut report = RunReport::new(manifest.borrow().run.clone());

    for provider in &selected {
        report
            .providers
            .push(ProviderRecord::from_provider(provider)?);
    }

    println!(
        "开始{} Provider 测试：{} 个 Provider，运行标识 {}",
        run_kind,
        selected.len(),
        escape_untrusted_inline_text(&run_id)
    );
    if let Some(selection) = &retry_selection {
        let provider = selected
            .iter()
            .copied()
            .find(|provider| provider.redact_text(&provider.id) == selection.lineage.provider_id)
            .ok_or_else(|| "精确补测选择的 Provider 未进入当前恢复身份".to_owned())?;
        report.probes = probe_selected_retry_cases(
            provider,
            options,
            &run_id,
            &selection.cases,
            &reusable_records,
            |probe, reused| {
                if !reused {
                    manifest.borrow().validate_probe_scope(probe)?;
                    let sequence = store.append_probe(&run_id, probe, &selected)?;
                    let mut manifest = manifest.borrow_mut();
                    manifest.commit_probe(sequence, probe.clone())?;
                    store.write_resume_manifest(&manifest, &selected)?;
                }
                println!(
                    "  {} | {} | {} | {} | 尝试 {} => {}{}",
                    escape_untrusted_inline_text(&probe.model),
                    escape_untrusted_inline_text(&probe.protocol),
                    escape_untrusted_inline_text(&probe.response_mode),
                    escape_untrusted_inline_text(&probe.capability),
                    probe.attempts,
                    escape_untrusted_inline_text(&probe.status),
                    if reused { "（已恢复）" } else { "" }
                );
                Ok(())
            },
        )
        .await?;
    } else {
        for provider in &selected {
            println!(
                "正在测试 Provider：{} ({})",
                provider.redact_text(&provider.name),
                provider.redact_text(&provider.id)
            );
            let execution = probe_provider(
                provider,
                options,
                &run_id,
                &reusable_records,
                |_, candidate_ids| {
                    if candidate_ids
                        .iter()
                        .any(|model| provider.redact_text(model) != *model)
                    {
                        return Err(
                            "模型目录或配置中的模型标识包含认证凭据，拒绝持久化或发送".to_owned()
                        );
                    }
                    let candidates = candidate_ids
                        .iter()
                        .map(|model| provider.redact_text(model))
                        .collect::<Vec<_>>();
                    let mut manifest = manifest.borrow_mut();
                    let frozen = manifest
                        .register_candidates(&provider.redact_text(&provider.id), candidates)?;
                    store.write_resume_manifest(&manifest, &selected)?;
                    Ok(frozen)
                },
                |probe, reused| {
                    if !reused {
                        let sequence = store.append_probe(&run_id, probe, &selected)?;
                        let mut manifest = manifest.borrow_mut();
                        manifest.commit_probe(sequence, probe.clone())?;
                        store.write_resume_manifest(&manifest, &selected)?;
                    }
                    println!(
                        "  {} | {} | {} | {} | 尝试 {} => {}{}",
                        escape_untrusted_inline_text(&probe.model),
                        escape_untrusted_inline_text(&probe.protocol),
                        escape_untrusted_inline_text(&probe.response_mode),
                        escape_untrusted_inline_text(&probe.capability),
                        probe.attempts,
                        escape_untrusted_inline_text(&probe.status),
                        if reused { "（已恢复）" } else { "" }
                    );
                    Ok(())
                },
            )
            .await?;
            println!(
                "  模型目录：{}，实时 {} 个，候选 {} 个",
                execution.catalog.status,
                execution.catalog.discovered_models.len(),
                execution.catalog.candidates.len()
            );
            report.catalogs.push(execution.catalog);
            report.probes.extend(execution.probes);
        }
    }

    report.refresh_summary();
    let committed = manifest.borrow().clone();
    store
        .verify_committed_fixtures(&committed, &selected)
        .await?;
    if let Some(selection) = &retry_selection {
        if report.probes.len() != selection.cases.len()
            || manifest.borrow().record_count() != selection.cases.len()
        {
            return Err("精确补测没有为选择清单中的每个 tuple 形成唯一终态".to_owned());
        }
    }
    // 先结束只读借用，再进入可能更新恢复清单的失败分支，避免 RefCell 重入崩溃。
    let catalog_completion = {
        let manifest = manifest.borrow();
        validate_catalog_completion(&manifest, &report.catalogs, &report.probes, &selected)
    };
    let completion_state = match catalog_completion {
        Ok(state) => state,
        Err(error) => {
            persist_incomplete_run(&store, &report, &manifest, &selected)?;
            println!(
                "部分报告目录：{}",
                escape_untrusted_inline_text(&store.run_dir().display().to_string())
            );
            return Err(error);
        }
    };

    if matches!(
        completion_state,
        CatalogCompletionState::DiscoveryFailedWithCompleteFrozenMatrix
    ) {
        println!(
            "模型目录存在真实失败证据，但冻结候选矩阵已完整闭合；保留 failed 目录记录并封印完成态"
        );
    }

    report.run.finished_at = Some(timestamp()?);
    store.finalize_completed(&report, &manifest.borrow(), &selected)?;
    println!(
        "真实 Provider 测试完成：远端案例 {}，通过 {}，契约不符合 {}，失败 {}，跳过 {}，未验证 {}，远端请求尝试 {}",
        report.summary.provider_compatibility_probes,
        report.summary.passed,
        report.summary.contract_violations,
        report.summary.failed,
        report.summary.skipped,
        report.summary.unverified,
        report.summary.total_attempts
    );
    println!(
        "本地 Client/Adapter Conformance：案例 {}，通过 {}，契约不符合 {}，失败 {}，跳过 {}，未验证 {}，本地回环请求尝试 {}；不计入 Provider 或模型兼容率",
        report.summary.local_conformance.total,
        report.summary.local_conformance.passed,
        report.summary.local_conformance.contract_violations,
        report.summary.local_conformance.failed,
        report.summary.local_conformance.skipped,
        report.summary.local_conformance.unverified,
        report.summary.local_loopback_attempts
    );
    println!(
        "报告目录：{}",
        escape_untrusted_inline_text(&store.run_dir().display().to_string())
    );
    Ok(())
}

/// 在完成校验失败后保留可恢复的部分报告和未完成恢复清单。
fn persist_incomplete_run(
    store: &ReportStore,
    report: &RunReport,
    manifest: &RefCell<ResumeManifest>,
    selected: &[&config::ProviderEntry],
) -> Result<(), String> {
    store.finalize(report, selected)?;
    let mut manifest = manifest.borrow_mut();
    manifest.run = report.run.clone();
    manifest.finished = false;
    store.write_resume_manifest(&manifest, selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use report::{
        CandidateModelRecord, CatalogRecord, ErrorMessageEvidence, NormalizedError, ProviderRecord,
        validate_catalog_completion,
    };
    use std::collections::BTreeSet;
    use std::fs;
    use std::panic::AssertUnwindSafe;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 创建不发起网络请求的目录完成失败回归夹具。
    fn catalog_failure_fixture() -> (
        report::ReportStore,
        config::ProviderEntry,
        RefCell<ResumeManifest>,
        RunReport,
        PathBuf,
    ) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-partial-catalog-{}-{unique}",
            std::process::id()
        ));
        let store =
            ReportStore::create(&output_root, "run").expect("应能创建目录完成失败回归测试运行");
        let provider: config::ProviderEntry = serde_json::from_value(serde_json::json!({
            "id": "provider",
            "name": "测试 Provider",
            "baseUrl": "https://example.com/v1",
            "models": ["model"],
            "apiBackend": "responses",
            "apiKey": "fixture-secret-value"
        }))
        .expect("应能创建目录完成失败回归 Provider");
        let options = config::RuntimeOptions {
            user_data_directory: output_root.join("user-data"),
            config_path: output_root.join("providers.json"),
            verify_run_dir: None,
            output_root: output_root.clone(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 1,
            request_timeout_secs: 5,
            catalog_only: true,
            diagnostics_only: false,
            capabilities: BTreeSet::new(),
            full_matrix: false,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        };
        let run = RunMetadata::new("run".to_owned(), &options)
            .expect("应能创建目录完成失败回归运行元数据");
        let mut manifest = ResumeManifest::new(run, &options, &[&provider])
            .expect("应能创建目录完成失败回归恢复清单");
        manifest
            .register_candidates("provider", ["model".to_owned()])
            .expect("应能冻结目录完成失败回归候选集合");
        let mut report = RunReport::new(manifest.run.clone());
        report.providers = vec![
            ProviderRecord::from_provider(&provider)
                .expect("应能创建目录完成失败回归 Provider 快照"),
        ];
        report.catalogs = vec![CatalogRecord {
            provider_id: "provider".to_owned(),
            status: "failed".to_owned(),
            attempts: 1,
            latency_ms: 0,
            pages: 0,
            raw_count: 0,
            invalid_count: 0,
            discovered_models: Vec::new(),
            candidates: vec![CandidateModelRecord {
                model: "model".to_owned(),
                configured: true,
                discovered: false,
                explicit: false,
                frozen_from_resume: false,
            }],
            normalized_error: Some(NormalizedError {
                kind: "transport".to_owned(),
                message_evidence: ErrorMessageEvidence::from_text("synthetic catalog failure"),
                retryable: true,
                http_status: None,
            }),
        }];
        report.refresh_summary();
        (store, provider, RefCell::new(manifest), report, output_root)
    }

    /// 验证目录完成校验失败后可写出部分报告和未完成 Resume，且不会重入借用崩溃。
    #[test]
    fn catalog_completion_failure_persists_partial_report_and_resume_without_panic() {
        let (store, provider, manifest, report, output_root) = catalog_failure_fixture();
        let validation_error = {
            let manifest = manifest.borrow();
            validate_catalog_completion(&manifest, &report.catalogs, &report.probes, &[&provider])
                .expect_err("缺少冻结矩阵终态时目录完成校验必须失败")
        };

        let persisted = std::panic::catch_unwind(AssertUnwindSafe(|| {
            persist_incomplete_run(&store, &report, &manifest, &[&provider])
        }))
        .expect("目录完成校验失败后的部分报告写入不得触发 RefCell panic");
        persisted.expect("目录完成校验失败后仍应能写出部分报告和恢复清单");

        let result: serde_json::Value = serde_json::from_slice(
            &fs::read(store.run_dir().join("result.json")).expect("应能读取部分 result.json"),
        )
        .expect("部分 result.json 应为有效 JSON");
        let resume: serde_json::Value = serde_json::from_slice(
            &fs::read(store.run_dir().join("resume.json")).expect("应能读取未完成 resume.json"),
        )
        .expect("未完成 resume.json 应为有效 JSON");
        assert!(!validation_error.is_empty());
        assert_eq!(result["run"]["finishedAt"], serde_json::Value::Null);
        assert_eq!(resume["finished"], serde_json::json!(false));
        for artifact in [
            "result.json",
            "compatibility-matrix.md",
            "summary.md",
            "redaction-report.json",
            "resume.json",
        ] {
            assert!(
                store.run_dir().join(artifact).is_file(),
                "目录完成失败必须保留 {artifact}"
            );
        }

        drop(manifest);
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理目录完成失败回归测试目录");
    }
}
