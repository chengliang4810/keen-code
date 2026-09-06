use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::future::{Future, poll_fn};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::thread::{self, JoinHandle};
use std::time::{Duration as StdDuration, Instant};

use keencode_model::{
    ContentBlock, ImageContent, Message, MessageRole, ModelError, ModelProvider, ModelRequest,
    ModelResponse, ModelStreamEvent, ProviderProtocol, ReasoningConfig, ReasoningEffort,
    StopReason, StructuredOutputConfig, StructuredOutputEnforcement, StructuredOutputFailureKind,
    ToolChoice, ToolDefinition, ToolResult, ToolResultContent,
};
use keencode_provider::{
    ApiKey, ModelCatalog, ProviderClient, ProviderConfig, WireExchange, WireResponseMode,
    WireTraceCollector, replay_wire_error_response, replay_wire_response,
};
use serde_json::{Value, json};
use tokio::time::{Duration, sleep};

#[cfg(test)]
use crate::config::contains_unsafe_inline_character;
use crate::config::{
    ProbeKind, ProviderEntry, RuntimeOptions, all_protocols, protocol_name, response_mode_name,
    validate_inline_value,
};
use crate::report::{
    ActualTextEvidence, CancellationEvidence, CandidateModelRecord, CatalogRecord,
    ErrorMessageEvidence, FixtureExchangeOutcome, FixtureReplayEvidence, NormalizedError,
    ProbeRecord, ResponseEvidence, RetryCase, SemanticAssertion, SkipEvidence,
    domain_separated_hex, first_turn_marker, marker_from_probe_stable_key, probe_stable_key,
};
use crate::wire_shape::inspect_wire_response_shape;

/// 工具调用探测要求模型选择的固定工具名称。
const TOOL_NAME: &str = "keencode_probe_echo";
/// 并行调用探测使用的左侧工具名称。
const PARALLEL_LEFT_TOOL: &str = "keencode_probe_left";
/// 并行调用探测使用的右侧工具名称。
const PARALLEL_RIGHT_TOOL: &str = "keencode_probe_right";
/// 工具调用和结构化输出探测要求返回的固定整数。
const EXPECTED_COUNT: i64 = 7;
/// 在途调用保留到本地取消计时器触发的窗口。
const CANCEL_AFTER_MS: u64 = 500;
/// 流式取消在判定失败前等待首个有效统一事件的最长时间。
const FIRST_EVENT_TIMEOUT_MS: u64 = 60_000;
/// 缓存探测稳定前缀的固定重复单元数，确保越过常见最小缓存阈值。
const PROMPT_CACHE_PREFIX_UNITS: usize = 4_096;
/// 上下文溢出探测生成的独立短 Token 单元数，覆盖常见百万 Token 窗口。
const CONTEXT_OVERFLOW_TOKEN_UNITS: usize = 1_100_000;
/// Provider 能力矩阵允许同时运行的模型、协议和响应模式 lane 数量。
const MAX_CONCURRENT_PROBE_LANES: usize = 4;
/// 图片工具结果探测使用的固定 1x1 合成 PNG，正文只存在于内存中的请求中。
const SYNTHETIC_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

/// 一个正在执行单个模型、协议和响应模式 lane 的异步任务。
type ProbeLaneFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<ProbeRecord>, String>> + 'a>>;

/// 一个 Provider 完成目录与所选能力矩阵后的结果。
pub(crate) struct ProviderExecution {
    /// 实时目录与候选模型集合。
    pub(crate) catalog: CatalogRecord,
    /// 每个模型、协议、响应模式和能力的独立结果。
    pub(crate) probes: Vec<ProbeRecord>,
}

/// 在不跨越异步等待持有回调借用的前提下，同步提交一条探测记录。
fn notify_probe<F>(
    callback: &Rc<RefCell<F>>,
    record: &mut ProbeRecord,
    reused: bool,
) -> Result<(), String>
where
    F: FnMut(&mut ProbeRecord, bool) -> Result<(), String>,
{
    let mut callback = callback.borrow_mut();
    callback(record, reused)
}

/// 依次执行一个模型、协议和响应模式 lane，并在每条记录完成后立即提交。
#[allow(clippy::too_many_arguments)]
async fn run_probe_lane<'a, F>(
    provider: &'a ProviderEntry,
    options: &'a RuntimeOptions,
    run_id: &'a str,
    reusable_records: &'a BTreeMap<String, ProbeRecord>,
    model: String,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    callback: Rc<RefCell<F>>,
) -> Result<Vec<ProbeRecord>, String>
where
    F: FnMut(&mut ProbeRecord, bool) -> Result<(), String> + 'a,
{
    let mut probes = Vec::new();
    let (mut gate, gate_reused) = match reusable_probe(
        reusable_records,
        run_id,
        provider,
        &model,
        protocol,
        response_mode,
        ProbeKind::Text.as_str(),
    ) {
        Some(record) => (record, true),
        None => (
            run_probe(
                provider,
                &model,
                protocol,
                response_mode,
                ProbeKind::Text,
                options,
                run_id,
            )
            .await,
            false,
        ),
    };
    let gate_passed = gate.status == "passed";
    notify_probe(&callback, &mut gate, gate_reused)?;
    probes.push(gate.clone());

    for capability in &options.capabilities {
        if *capability == ProbeKind::Text {
            continue;
        }
        let (mut record, reused) = match reusable_probe(
            reusable_records,
            run_id,
            provider,
            &model,
            protocol,
            response_mode,
            capability.as_str(),
        ) {
            Some(record) => (record, true),
            None if gate_passed || options.full_matrix => (
                run_probe(
                    provider,
                    &model,
                    protocol,
                    response_mode,
                    *capability,
                    options,
                    run_id,
                )
                .await,
                false,
            ),
            None => (
                skipped_by_base_gate(
                    provider,
                    &model,
                    protocol,
                    response_mode,
                    *capability,
                    run_id,
                    &gate,
                ),
                false,
            ),
        };
        notify_probe(&callback, &mut record, reused)?;
        probes.push(record);
    }

    Ok(probes)
}

/// 获取实时目录并按顺序执行当前 Provider 的所选能力探测。
pub(crate) async fn probe_provider<F>(
    provider: &ProviderEntry,
    options: &RuntimeOptions,
    run_id: &str,
    reusable_records: &BTreeMap<String, ProbeRecord>,
    mut on_candidates: impl FnMut(&CatalogRecord, &[String]) -> Result<Vec<String>, String>,
    mut on_probe: F,
) -> Result<ProviderExecution, String>
where
    F: FnMut(&mut ProbeRecord, bool) -> Result<(), String>,
{
    let (mut catalog, candidate_ids) = fetch_catalog(provider, options).await?;
    validate_candidate_model_ids(&candidate_ids, "目录与配置候选模型标识")?;
    let candidate_ids = on_candidates(&catalog, &candidate_ids)?;
    validate_candidate_model_ids(&candidate_ids, "恢复候选模型标识")?;
    merge_frozen_candidates(&mut catalog, provider, options, &candidate_ids);
    if options.catalog_only {
        return Ok(ProviderExecution {
            catalog,
            probes: Vec::new(),
        });
    }

    let mut probes = Vec::new();
    if options.full_matrix || options.diagnostics_only {
        let authentication_model = "keencode-authentication-probe";
        for protocol in all_protocols() {
            for response_mode in [WireResponseMode::Buffered, WireResponseMode::Streaming] {
                let missing_model = missing_model_id(provider, protocol, response_mode, run_id);
                for (mut record, reused) in [
                    match reusable_probe(
                        reusable_records,
                        run_id,
                        provider,
                        authentication_model,
                        protocol,
                        response_mode,
                        "diagnostic_invalid_authentication",
                    ) {
                        Some(record) => (record, true),
                        None => (
                            run_invalid_authentication_probe(
                                provider,
                                authentication_model,
                                protocol,
                                response_mode,
                                options,
                                run_id,
                            )
                            .await,
                            false,
                        ),
                    },
                    match reusable_probe(
                        reusable_records,
                        run_id,
                        provider,
                        &missing_model,
                        protocol,
                        response_mode,
                        "diagnostic_missing_model",
                    ) {
                        Some(record) => (record, true),
                        None => (
                            run_missing_model_probe(
                                provider,
                                &missing_model,
                                protocol,
                                response_mode,
                                options,
                                run_id,
                            )
                            .await,
                            false,
                        ),
                    },
                ] {
                    on_probe(&mut record, reused)?;
                    probes.push(record);
                }
            }
        }
    }
    if options.diagnostics_only {
        return Ok(ProviderExecution { catalog, probes });
    }
    let callback = Rc::new(RefCell::new(on_probe));
    let lane_specs = candidate_ids
        .into_iter()
        .flat_map(|model| {
            all_protocols().into_iter().flat_map(move |protocol| {
                let model = model.clone();
                [WireResponseMode::Buffered, WireResponseMode::Streaming]
                    .into_iter()
                    .map(move |response_mode| (model.clone(), protocol, response_mode))
            })
        })
        .collect::<Vec<_>>();
    let mut next_lane = lane_specs.into_iter();
    let mut active_lanes: Vec<ProbeLaneFuture<'_>> = Vec::with_capacity(MAX_CONCURRENT_PROBE_LANES);

    loop {
        while active_lanes.len() < MAX_CONCURRENT_PROBE_LANES {
            let Some((model, protocol, response_mode)) = next_lane.next() else {
                break;
            };
            active_lanes.push(Box::pin(run_probe_lane(
                provider,
                options,
                run_id,
                reusable_records,
                model,
                protocol,
                response_mode,
                Rc::clone(&callback),
            )));
        }
        if active_lanes.is_empty() {
            break;
        }

        let (finished_index, result) = poll_fn(|context| {
            for (index, lane) in active_lanes.iter_mut().enumerate() {
                if let Poll::Ready(result) = lane.as_mut().poll(context) {
                    return Poll::Ready((index, result));
                }
            }
            Poll::Pending
        })
        .await;
        drop(active_lanes.swap_remove(finished_index));
        match result {
            Ok(lane_probes) => probes.extend(lane_probes),
            Err(error) => return Err(error),
        }
    }
    Ok(ProviderExecution { catalog, probes })
}

/// 只执行已经由补测选择清单冻结的精确模型、协议、响应模式和能力 tuple。
pub(crate) async fn probe_selected_retry_cases<F>(
    provider: &ProviderEntry,
    options: &RuntimeOptions,
    run_id: &str,
    cases: &[RetryCase],
    reusable_records: &BTreeMap<String, ProbeRecord>,
    mut on_probe: F,
) -> Result<Vec<ProbeRecord>, String>
where
    F: FnMut(&mut ProbeRecord, bool) -> Result<(), String>,
{
    let provider_id = provider.redact_text(&provider.id);
    let mut probes = Vec::with_capacity(cases.len());
    for case in cases {
        validate_inline_value("精确补测模型标识", &case.model)?;
        if case.provider_id != provider_id {
            return Err("精确补测 tuple 引用了选择范围外的 Provider".to_owned());
        }
        let protocol = match case.protocol.as_str() {
            "anthropic_messages" => ProviderProtocol::Messages,
            "openai_chat_completions" => ProviderProtocol::ChatCompletions,
            "openai_responses" => ProviderProtocol::Responses,
            _ => return Err("精确补测 tuple 包含未知协议".to_owned()),
        };
        let response_mode = match case.response_mode.as_str() {
            "buffered" => WireResponseMode::Buffered,
            "streaming" => WireResponseMode::Streaming,
            _ => return Err("精确补测 tuple 包含未知响应模式".to_owned()),
        };
        let capability = ProbeKind::parse(&case.capability)?;
        if capability == ProbeKind::StreamInterruption {
            return Err("精确线上补测不能包含只访问本地回环服务的断流能力".to_owned());
        }
        let (mut record, reused) = match reusable_probe(
            reusable_records,
            run_id,
            provider,
            &case.model,
            protocol,
            response_mode,
            capability.as_str(),
        ) {
            Some(record) => (record, true),
            None => (
                run_probe(
                    provider,
                    &case.model,
                    protocol,
                    response_mode,
                    capability,
                    options,
                    run_id,
                )
                .await,
                false,
            ),
        };
        on_probe(&mut record, reused)?;
        probes.push(record);
    }
    Ok(probes)
}

/// 按稳定身份查找已经通过严格恢复门禁的确定性探测记录。
fn reusable_probe(
    records: &BTreeMap<String, ProbeRecord>,
    run_id: &str,
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: &str,
) -> Option<ProbeRecord> {
    let key = probe_stable_key(
        run_id,
        &provider.id,
        model,
        protocol_name(protocol),
        response_mode_name(response_mode),
        capability,
    );
    records.get(&key).cloned()
}

/// 为基础文本门禁阻止的能力生成零请求未验证记录。
fn skipped_by_base_gate(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: ProbeKind,
    run_id: &str,
    gate: &ProbeRecord,
) -> ProbeRecord {
    let normalized_error = gate.normalized_error.as_ref();
    let reason = if gate.status == "contract_violation" {
        "base_text_contract_violation"
    } else if normalized_error.is_some_and(|error| error.retryable) {
        "base_text_transient_failure"
    } else {
        "base_text_permanent_failure"
    };
    ProbeRecord {
        stable_key: probe_stable_key(
            run_id,
            &provider.id,
            model,
            protocol_name(protocol),
            response_mode_name(response_mode),
            capability.as_str(),
        ),
        provider_id: gate.provider_id.clone(),
        model: gate.model.clone(),
        protocol: gate.protocol.clone(),
        response_mode: gate.response_mode.clone(),
        capability: capability.as_str().to_owned(),
        endpoint_path: gate.endpoint_path.clone(),
        status: "skipped".to_owned(),
        attempts: 0,
        latency_ms: 0,
        expected_text: None,
        synthetic_marker: None,
        actual_text_evidence: None,
        response: None,
        assertions: Vec::new(),
        cancellation: None,
        skip_evidence: Some(SkipEvidence {
            verification: "unverified".to_owned(),
            reason: reason.to_owned(),
            blocked_by: gate.stable_key(),
            gate_status: gate.status.clone(),
            error_kind: normalized_error.map(|error| error.kind.clone()),
            retryable: normalized_error.map(|error| error.retryable),
            http_status: normalized_error.and_then(|error| error.http_status),
        }),
        fixture_paths: Vec::new(),
        recovered_from: None,
        fixture_replay: None,
        normalized_error: None,
        wire_response_shapes: Vec::new(),
        wire_exchanges: Vec::new(),
        wire_exchange_outcomes: Vec::new(),
    }
}

/// 使用明确无效且非秘密的凭据验证服务不会接受未认证调用。
async fn run_invalid_authentication_probe(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    options: &RuntimeOptions,
    run_id: &str,
) -> ProbeRecord {
    let capability = "diagnostic_invalid_authentication";
    let marker = diagnostic_marker(provider, model, protocol, response_mode, capability, run_id);
    let started = Instant::now();
    let config = match provider.provider_config_with_credential(
        protocol,
        response_mode,
        options.request_timeout_secs,
        format!("keencode-intentionally-invalid-{marker}"),
    ) {
        Ok(config) => config,
        Err(message) => {
            return failed_diagnostic_configuration(
                provider,
                model,
                protocol,
                response_mode,
                capability,
                run_id,
                started.elapsed().as_millis(),
                message,
            );
        }
    };
    let endpoint_path = diagnostic_endpoint_path(provider, &config, protocol);
    let max_event_bytes = config.max_event_bytes;
    let (client, trace) = match ProviderClient::new_traced(config) {
        Ok(client) => client,
        Err(error) => {
            return failed_diagnostic_configuration(
                provider,
                model,
                protocol,
                response_mode,
                capability,
                run_id,
                started.elapsed().as_millis(),
                error.to_string(),
            );
        }
    };
    let result = client.complete(text_request(model, &marker)).await;
    let mut record = expected_error_probe_record(
        provider,
        model,
        protocol,
        response_mode,
        capability,
        run_id,
        &marker,
        endpoint_path,
        started.elapsed().as_millis(),
        result,
        |error| {
            matches!(
                error,
                ModelError::Authentication { .. } | ModelError::Authorization { .. }
            )
        },
        "invalid_credential_rejected",
        "无效凭据被服务拒绝并归一化为认证或授权错误",
        "无效凭据没有被服务按认证边界拒绝",
    );
    attach_fixture_evidence(
        &mut record,
        &trace,
        protocol,
        max_event_bytes,
        FixtureReplayRequirement::ExpectedHttpError,
        provider,
    )
    .await;
    record
}

/// 使用不可碰撞的合成模型标识验证模型不存在错误的归一化。
async fn run_missing_model_probe(
    provider: &ProviderEntry,
    missing_model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    options: &RuntimeOptions,
    run_id: &str,
) -> ProbeRecord {
    let capability = "diagnostic_missing_model";
    let marker = diagnostic_marker(
        provider,
        missing_model,
        protocol,
        response_mode,
        capability,
        run_id,
    );
    let started = Instant::now();
    let config =
        match provider.provider_config(protocol, response_mode, options.request_timeout_secs) {
            Ok(config) => config,
            Err(message) => {
                return failed_diagnostic_configuration(
                    provider,
                    missing_model,
                    protocol,
                    response_mode,
                    capability,
                    run_id,
                    started.elapsed().as_millis(),
                    message,
                );
            }
        };
    let endpoint_path = diagnostic_endpoint_path(provider, &config, protocol);
    let max_event_bytes = config.max_event_bytes;
    let (client, trace) = match ProviderClient::new_traced(config) {
        Ok(client) => client,
        Err(error) => {
            return failed_diagnostic_configuration(
                provider,
                missing_model,
                protocol,
                response_mode,
                capability,
                run_id,
                started.elapsed().as_millis(),
                error.to_string(),
            );
        }
    };
    let result = client.complete(text_request(missing_model, &marker)).await;
    let mut record = expected_error_probe_record(
        provider,
        missing_model,
        protocol,
        response_mode,
        capability,
        run_id,
        &marker,
        endpoint_path,
        started.elapsed().as_millis(),
        result,
        |error| matches!(error, ModelError::ModelNotFound { .. }),
        "missing_model_classified",
        "不存在的模型被稳定归一化为 model_not_found",
        "不存在的模型未被稳定归一化为 model_not_found",
    );
    attach_fixture_evidence(
        &mut record,
        &trace,
        protocol,
        max_event_bytes,
        FixtureReplayRequirement::ExpectedHttpError,
        provider,
    )
    .await;
    record
}

/// 把期望远端拒绝的负向调用转换为可聚合的事实记录。
#[allow(clippy::too_many_arguments)]
fn expected_error_probe_record<F>(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: &str,
    run_id: &str,
    synthetic_marker: &str,
    endpoint_path: String,
    latency_ms: u128,
    result: Result<ModelResponse, ModelError>,
    expected: F,
    assertion_name: &str,
    passed_detail: &str,
    failed_detail: &str,
) -> ProbeRecord
where
    F: FnOnce(&ModelError) -> bool,
{
    match result {
        Err(error) => {
            let passed = expected(&error);
            let inconclusive = error.is_retryable()
                || matches!(
                    error,
                    ModelError::ProtocolUnsupported { .. }
                        | ModelError::QuotaExceeded { .. }
                        | ModelError::Cancelled { .. }
                );
            ProbeRecord {
                stable_key: probe_stable_key(
                    run_id,
                    &provider.id,
                    model,
                    protocol_name(protocol),
                    response_mode_name(response_mode),
                    capability,
                ),
                provider_id: provider.redact_text(&provider.id),
                model: provider.redact_text(model),
                protocol: protocol_name(protocol).to_owned(),
                response_mode: response_mode_name(response_mode).to_owned(),
                capability: capability.to_owned(),
                endpoint_path,
                status: if passed {
                    "passed".to_owned()
                } else if inconclusive {
                    "failed".to_owned()
                } else {
                    "contract_violation".to_owned()
                },
                attempts: 1,
                latency_ms,
                expected_text: None,
                synthetic_marker: Some(synthetic_marker.to_owned()),
                actual_text_evidence: None,
                response: None,
                assertions: vec![assertion(
                    assertion_name,
                    passed,
                    passed_detail,
                    failed_detail,
                )],
                cancellation: None,
                skip_evidence: None,
                fixture_paths: Vec::new(),
                recovered_from: None,
                fixture_replay: None,
                normalized_error: Some(normalize_error(provider, &error)),
                wire_response_shapes: Vec::new(),
                wire_exchanges: Vec::new(),
                wire_exchange_outcomes: Vec::new(),
            }
        }
        Ok(response) => {
            let stable_key = probe_stable_key(
                run_id,
                &provider.id,
                model,
                protocol_name(protocol),
                response_mode_name(response_mode),
                capability,
            );
            let actual_text = response_text(&response);
            ProbeRecord {
                actual_text_evidence: Some(ActualTextEvidence::from_text(
                    provider,
                    &stable_key,
                    &actual_text,
                )),
                stable_key,
                provider_id: provider.redact_text(&provider.id),
                model: provider.redact_text(model),
                protocol: protocol_name(protocol).to_owned(),
                response_mode: response_mode_name(response_mode).to_owned(),
                capability: capability.to_owned(),
                endpoint_path,
                status: "contract_violation".to_owned(),
                attempts: 1,
                latency_ms,
                expected_text: None,
                synthetic_marker: Some(synthetic_marker.to_owned()),
                response: Some(ResponseEvidence::from_response(&response, provider)),
                assertions: vec![assertion(
                    assertion_name,
                    false,
                    passed_detail,
                    failed_detail,
                )],
                cancellation: None,
                skip_evidence: None,
                fixture_paths: Vec::new(),
                recovered_from: None,
                fixture_replay: None,
                normalized_error: None,
                wire_response_shapes: Vec::new(),
                wire_exchanges: Vec::new(),
                wire_exchange_outcomes: Vec::new(),
            }
        }
    }
}

/// 把诊断配置失败转换为不含合成凭据的统一记录。
#[allow(clippy::too_many_arguments)]
fn failed_diagnostic_configuration(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: &str,
    run_id: &str,
    latency_ms: u128,
    message: String,
) -> ProbeRecord {
    ProbeRecord {
        stable_key: probe_stable_key(
            run_id,
            &provider.id,
            model,
            protocol_name(protocol),
            response_mode_name(response_mode),
            capability,
        ),
        provider_id: provider.redact_text(&provider.id),
        model: provider.redact_text(model),
        protocol: protocol_name(protocol).to_owned(),
        response_mode: response_mode_name(response_mode).to_owned(),
        capability: capability.to_owned(),
        endpoint_path: String::new(),
        status: "failed".to_owned(),
        attempts: 0,
        latency_ms,
        expected_text: None,
        synthetic_marker: None,
        actual_text_evidence: None,
        response: None,
        assertions: Vec::new(),
        cancellation: None,
        skip_evidence: None,
        fixture_paths: Vec::new(),
        recovered_from: None,
        fixture_replay: None,
        normalized_error: Some(NormalizedError {
            kind: "configuration".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text(
                &provider.redact_credentials(&message),
            ),
            retryable: false,
            http_status: None,
        }),
        wire_response_shapes: Vec::new(),
        wire_exchanges: Vec::new(),
        wire_exchange_outcomes: Vec::new(),
    }
}

/// 返回负向诊断使用且不包含凭据的唯一合成标记。
fn diagnostic_marker(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: &str,
    run_id: &str,
) -> String {
    let stable_key = probe_stable_key(
        run_id,
        &provider.id,
        model,
        protocol_name(protocol),
        response_mode_name(response_mode),
        capability,
    );
    marker_from_probe_stable_key(&stable_key, true)
}

/// 返回缺失模型诊断使用的确定性模型标识，使恢复查询与真实请求共享同一原始身份。
fn missing_model_id(
    provider: &ProviderEntry,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    run_id: &str,
) -> String {
    let digest = domain_separated_hex(
        b"keencode-provider-missing-model-v1",
        &[
            run_id.as_bytes(),
            provider.id.as_bytes(),
            protocol_name(protocol).as_bytes(),
            response_mode_name(response_mode).as_bytes(),
        ],
    );
    format!("keencode-missing-{}", &digest[..20])
}

/// 提取诊断请求实际使用且经过凭据清理的端点路径。
fn diagnostic_endpoint_path(
    provider: &ProviderEntry,
    config: &keencode_provider::ProviderConfig,
    protocol: ProviderProtocol,
) -> String {
    config
        .base_url()
        .join(config.endpoints.for_protocol(protocol))
        .map(|url| provider.redacted_endpoint_path(url.path()))
        .unwrap_or_else(|_| {
            provider.redacted_endpoint_path(config.endpoints.for_protocol(protocol))
        })
}

/// 只在回环地址接受一个请求并主动关闭缺少协议终态的 2xx SSE 流。
struct TruncatedSseServer {
    /// 可写入临时 Provider 配置的本地基础地址。
    base_url: String,
    /// 客户端未连接或提前失败时用于终止非阻塞接受循环。
    stop: Arc<AtomicBool>,
    /// 必须在离开探测函数前确定性回收并检查结果的服务线程。
    thread: Option<JoinHandle<Result<bool, String>>>,
}

impl TruncatedSseServer {
    /// 为目标协议启动一次性本地截断响应服务。
    fn start(protocol: ProviderProtocol) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("无法绑定本地截断探测端口：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("无法设置本地截断探测监听器：{error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("无法读取本地截断探测端口：{error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let body = truncated_sse_body(protocol).as_bytes().to_vec();
        let thread = thread::spawn(move || -> Result<bool, String> {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).map_err(|error| {
                            format!("无法将本地截断探测连接恢复为阻塞模式：{error}")
                        })?;
                        stream
                            .set_read_timeout(Some(StdDuration::from_secs(5)))
                            .map_err(|error| {
                                format!("无法设置本地截断探测连接读取超时：{error}")
                            })?;
                        read_local_probe_request(&mut stream)
                            .map_err(|error| format!("读取本地截断探测请求失败：{error}"))?;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        stream
                            .write_all(response.as_bytes())
                            .map_err(|error| format!("写入本地截断探测响应头失败：{error}"))?;
                        stream
                            .write_all(&body)
                            .map_err(|error| format!("写入本地截断探测响应正文失败：{error}"))?;
                        stream
                            .flush()
                            .map_err(|error| format!("刷新本地截断探测响应失败：{error}"))?;
                        return Ok(true);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(StdDuration::from_millis(5));
                    }
                    Err(error) => {
                        return Err(format!("接受本地截断探测连接失败：{error}"));
                    }
                }
            }
            Ok(false)
        });
        Ok(Self {
            base_url: format!("http://{address}/v1"),
            stop,
            thread: Some(thread),
        })
    }

    /// 停止接受循环、回收服务线程，并要求本轮确实完整处理过一个请求。
    fn finish(mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        match self.join_thread()? {
            true => Ok(()),
            false => Err("本地截断探测服务未接受客户端请求".to_owned()),
        }
    }

    /// 回收服务线程并返回它是否完整处理过目标请求。
    fn join_thread(&mut self) -> Result<bool, String> {
        let Some(thread) = self.thread.take() else {
            return Ok(false);
        };
        match thread.join() {
            Ok(result) => result,
            Err(_) => Err("本地截断探测服务线程异常终止".to_owned()),
        }
    }
}

impl Drop for TruncatedSseServer {
    /// 在提前返回或 panic 清理路径停止线程，并把无法返回的线程错误写入标准错误。
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Err(error) = self.join_thread() {
            eprintln!("本地截断探测服务清理失败：{error}");
        }
    }
}

/// 读取一次本地探测请求的完整 Header 与固定长度正文。
fn read_local_probe_request(stream: &mut TcpStream) -> std::io::Result<()> {
    /// 本地合成服务允许读取的单次完整请求上限。
    const MAX_LOCAL_REQUEST_BYTES: usize = 2 * 1024 * 1024;
    let mut request = Vec::new();
    let mut expected_size = None;
    let mut buffer = [0_u8; 4096];
    loop {
        let size = stream.read(&mut buffer)?;
        if size == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "本地截断探测请求在完整 Header 与正文前关闭",
            ));
        }
        request.extend_from_slice(&buffer[..size]);
        if request.len() > MAX_LOCAL_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "本地截断探测请求超过固定安全上限",
            ));
        }
        if expected_size.is_none() {
            if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let body_start = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                expected_size = Some(body_start + content_length);
            }
        }
        if expected_size.is_some_and(|size| request.len() >= size) {
            return Ok(());
        }
    }
}

/// 返回目标协议可开始解析但明确缺少最终终态事件的最小 SSE 正文。
fn truncated_sse_body(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::Messages => {
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-local\",\"model\":\"synthetic-model\",\"usage\":{\"input_tokens\":1}}}\n\n"
        }
        ProviderProtocol::ChatCompletions => {
            "data: {\"id\":\"chat-local\",\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"partial\"},\"finish_reason\":null}]}\n\n"
        }
        ProviderProtocol::Responses => {
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-local\",\"model\":\"synthetic-model\",\"status\":\"in_progress\"}}\n\n"
        }
    }
}

/// 区分本地回环基础设施失败与真正进入 Adapter 后形成的协议终态。
fn stream_interruption_infrastructure_message(
    result: &Result<ModelResponse, ModelError>,
    server_result: Result<(), String>,
) -> Option<String> {
    match server_result {
        Err(message) => Some(message),
        Ok(()) => match result {
            Err(ModelError::Transport { message, .. }) => {
                Some(format!("本地回环 HTTP 传输失败：{message}"))
            }
            Ok(_) | Err(_) => None,
        },
    }
}

/// 构造明确归因于本地回环服务且不会计入 Adapter 契约不符合的失败记录。
#[allow(clippy::too_many_arguments)]
fn failed_stream_interruption_infrastructure_probe(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    run_id: &str,
    marker: &str,
    latency_ms: u128,
    message: &str,
) -> ProbeRecord {
    ProbeRecord {
        stable_key: probe_stable_key(
            run_id,
            &provider.id,
            model,
            protocol_name(protocol),
            response_mode_name(response_mode),
            ProbeKind::StreamInterruption.as_str(),
        ),
        provider_id: provider.redact_text(&provider.id),
        model: provider.redact_text(model),
        protocol: protocol_name(protocol).to_owned(),
        response_mode: response_mode_name(response_mode).to_owned(),
        capability: ProbeKind::StreamInterruption.as_str().to_owned(),
        endpoint_path: "/local/stream-interruption".to_owned(),
        status: "failed".to_owned(),
        attempts: 1,
        latency_ms,
        expected_text: None,
        synthetic_marker: Some(marker.to_owned()),
        actual_text_evidence: None,
        response: None,
        assertions: vec![assertion(
            "loopback_harness_completed",
            false,
            "本地回环截断服务完整处理请求",
            "本地回环截断服务未完整处理请求，结果不归因于 Provider Adapter",
        )],
        cancellation: None,
        skip_evidence: None,
        fixture_paths: Vec::new(),
        recovered_from: None,
        fixture_replay: None,
        normalized_error: Some(NormalizedError {
            kind: "harness_infrastructure".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text(
                &provider.redact_credentials(message),
            ),
            retryable: false,
            http_status: None,
        }),
        wire_response_shapes: Vec::new(),
        wire_exchanges: Vec::new(),
        wire_exchange_outcomes: Vec::new(),
    }
}

/// 通过真实回环 HTTP 连接验证 2xx SSE 缺少终态时的主动断流归一化与重放。
#[allow(clippy::too_many_arguments)]
async fn run_stream_interruption_probe(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    options: &RuntimeOptions,
    run_id: &str,
    marker: String,
    started: Instant,
) -> ProbeRecord {
    let server = match TruncatedSseServer::start(protocol) {
        Ok(server) => server,
        Err(message) => {
            return failed_local_probe(
                provider,
                model,
                protocol,
                response_mode,
                ProbeKind::StreamInterruption,
                run_id,
                Some(marker),
                started.elapsed().as_millis(),
                message,
            );
        }
    };
    let mut config = match ProviderConfig::new(
        "keencode-local-stream-interruption",
        protocol,
        &server.base_url,
        ApiKey::new("keencode-local-synthetic-credential").expect("固定本地合成凭据必须有效"),
    ) {
        Ok(config) => config,
        Err(error) => {
            return failed_local_probe(
                provider,
                model,
                protocol,
                response_mode,
                ProbeKind::StreamInterruption,
                run_id,
                Some(marker),
                started.elapsed().as_millis(),
                error.to_string(),
            );
        }
    };
    config.response_mode = response_mode;
    config.request_timeout = Duration::from_secs(options.request_timeout_secs);
    let max_event_bytes = config.max_event_bytes;
    let (client, trace) = match ProviderClient::new_traced(config) {
        Ok(client) => client,
        Err(error) => {
            return failed_local_probe(
                provider,
                model,
                protocol,
                response_mode,
                ProbeKind::StreamInterruption,
                run_id,
                Some(marker),
                started.elapsed().as_millis(),
                error.to_string(),
            );
        }
    };
    let result = client.complete(text_request(model, &marker)).await;
    let server_result = server.finish();
    if let Some(message) = stream_interruption_infrastructure_message(&result, server_result) {
        let mut record = failed_stream_interruption_infrastructure_probe(
            provider,
            model,
            protocol,
            response_mode,
            run_id,
            &marker,
            started.elapsed().as_millis(),
            &message,
        );
        attach_fixture_evidence(
            &mut record,
            &trace,
            protocol,
            max_event_bytes,
            FixtureReplayRequirement::ExpectedAdapterError,
            provider,
        )
        .await;
        return record;
    }
    let mut record = expected_error_probe_record(
        provider,
        model,
        protocol,
        response_mode,
        ProbeKind::StreamInterruption.as_str(),
        run_id,
        &marker,
        "/local/stream-interruption".to_owned(),
        started.elapsed().as_millis(),
        result,
        |error| {
            matches!(
                error,
                ModelError::StreamInterrupted {
                    retryable: true,
                    ..
                }
            )
        },
        "truncated_stream_classified",
        "本地主动关闭的缺终态 2xx SSE 被归一化为可重试 stream_interrupted",
        "本地主动关闭的缺终态 2xx SSE 未被稳定归一化为可重试 stream_interrupted",
    );
    record.assertions.push(assertion(
        "loopback_harness_completed",
        true,
        "本地回环截断服务完整处理请求",
        "本地回环截断服务未完整处理请求，结果不归因于 Provider Adapter",
    ));
    attach_fixture_evidence(
        &mut record,
        &trace,
        protocol,
        max_event_bytes,
        FixtureReplayRequirement::ExpectedAdapterError,
        provider,
    )
    .await;
    record
}

/// 请求实时模型目录并与配置模型及显式模型合并。
async fn fetch_catalog(
    provider: &ProviderEntry,
    options: &RuntimeOptions,
) -> Result<(CatalogRecord, Vec<String>), String> {
    let started = Instant::now();
    let protocol = provider.configured_protocol()?;
    let config = provider.provider_config(
        protocol,
        WireResponseMode::Buffered,
        options.request_timeout_secs,
    )?;
    let client = ProviderClient::new(config).map_err(|error| error.to_string())?;
    let mut attempts = 0;
    let mut final_error = None;
    let mut discovered = Vec::new();
    let mut pages = 0;
    let mut raw_count = 0;
    let mut invalid_count = 0;
    let mut best_partial = None;
    let mut observed_models = BTreeSet::new();

    while attempts < options.max_attempts {
        attempts += 1;
        match client.list_models_with_partial().await {
            Ok(mut catalog) => {
                reject_unsafe_catalog_model_ids(&mut catalog);
                best_partial = None;
                pages = catalog.pages;
                raw_count = catalog.raw_count;
                invalid_count = catalog.invalid_count;
                observed_models.extend(catalog.models.into_iter().map(|entry| entry.id));
                final_error = (invalid_count > 0).then(|| ModelError::Protocol {
                    message: format!(
                        "模型目录包含 {invalid_count} 个缺少有效稳定 ID 的条目，无法证明目录完整"
                    ),
                });
                break;
            }
            Err(failure) => {
                let mut partial = failure.partial;
                reject_unsafe_catalog_model_ids(&mut partial);
                observed_models.extend(partial.models.iter().map(|entry| entry.id.clone()));
                retain_better_partial(&mut best_partial, partial);
                let error = failure.error;
                let retryable = error.is_retryable();
                let delay = retry_delay(attempts, &error);
                final_error = Some(error);
                if !retryable || attempts >= options.max_attempts {
                    break;
                }
                sleep(delay).await;
            }
        }
    }

    if final_error.is_some() {
        if let Some(partial) = best_partial {
            pages = partial.pages;
            raw_count = partial.raw_count;
            invalid_count = partial.invalid_count;
            discovered = partial.models.into_iter().map(|entry| entry.id).collect();
        }
    }
    discovered.extend(observed_models);
    discovered.sort();
    discovered.dedup();

    let candidates = merge_candidates(provider, &discovered, &options.model_filters);
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.model.clone())
        .collect::<Vec<_>>();
    let report_candidates = candidates
        .into_iter()
        .map(|mut candidate| {
            candidate.model = provider.redact_text(&candidate.model);
            candidate
        })
        .collect();
    if final_error.is_none() && candidate_ids.is_empty() {
        final_error = Some(ModelError::Protocol {
            message: "模型目录与用户配置没有产生任何可测试模型".to_owned(),
        });
    }
    let normalized_error = final_error
        .as_ref()
        .map(|error| normalize_error(provider, error));
    let record = CatalogRecord {
        provider_id: provider.redact_text(&provider.id),
        status: if normalized_error.is_none() {
            "success".to_owned()
        } else {
            "failed".to_owned()
        },
        attempts,
        latency_ms: started.elapsed().as_millis(),
        pages,
        raw_count,
        invalid_count,
        discovered_models: discovered
            .iter()
            .map(|model| provider.redact_text(model))
            .collect(),
        candidates: report_candidates,
        normalized_error,
    };
    Ok((record, candidate_ids))
}

/// 校验候选集合中的每个模型标识均可安全进入恢复清单、报告和实际请求。
fn validate_candidate_model_ids(candidate_ids: &[String], field: &str) -> Result<(), String> {
    for model in candidate_ids {
        validate_inline_value(field, model)?;
    }
    Ok(())
}

/// 从实时目录中移除会改变终端或双向文本显示的模型 ID，并累计为无效条目。
fn reject_unsafe_catalog_model_ids(catalog: &mut ModelCatalog) {
    let mut rejected_count = 0_usize;
    catalog.models.retain(|entry| {
        if validate_inline_value("实时目录模型标识", &entry.id).is_ok() {
            true
        } else {
            rejected_count = rejected_count.saturating_add(entry.source_count.max(1));
            false
        }
    });
    catalog.invalid_count = catalog.invalid_count.saturating_add(rejected_count);
}

/// 在多次目录尝试中保留成功页数最多且证据最完整的一次部分结果。
fn retain_better_partial(current: &mut Option<ModelCatalog>, candidate: ModelCatalog) {
    let candidate_score = (
        candidate.pages,
        candidate.raw_count,
        candidate.models.len(),
        candidate.wire_bytes,
    );
    let replace = current.as_ref().is_none_or(|existing| {
        candidate_score
            > (
                existing.pages,
                existing.raw_count,
                existing.models.len(),
                existing.wire_bytes,
            )
    });
    if replace {
        *current = Some(candidate);
    }
}

/// 合并配置、目录和显式模型，并保持可复现顺序。
fn merge_candidates(
    provider: &ProviderEntry,
    discovered: &[String],
    explicit: &BTreeSet<String>,
) -> Vec<CandidateModelRecord> {
    let mut merged = BTreeMap::<String, CandidateModelRecord>::new();
    let mut order = Vec::new();
    for model in &provider.models {
        insert_candidate(&mut merged, &mut order, model, true, false, false);
    }
    for model in discovered {
        insert_candidate(&mut merged, &mut order, model, false, true, false);
    }
    for model in explicit {
        insert_candidate(&mut merged, &mut order, model, false, false, true);
    }
    order
        .into_iter()
        .filter(|model| explicit.is_empty() || explicit.contains(model))
        .filter_map(|model| merged.remove(&model))
        .collect()
}

/// 把恢复清单中已冻结但本次目录未返回的模型追加到实际执行集合报告。
fn merge_frozen_candidates(
    catalog: &mut CatalogRecord,
    provider: &ProviderEntry,
    options: &RuntimeOptions,
    frozen_candidates: &[String],
) {
    let mut present = catalog
        .candidates
        .iter()
        .map(|candidate| candidate.model.clone())
        .collect::<BTreeSet<_>>();
    for model in frozen_candidates {
        let redacted = provider.redact_text(model);
        if !present.insert(redacted.clone()) {
            continue;
        }
        catalog.candidates.push(CandidateModelRecord {
            model: redacted.clone(),
            configured: provider
                .models
                .iter()
                .any(|configured| provider.redact_text(configured) == redacted),
            discovered: catalog
                .discovered_models
                .iter()
                .any(|discovered| discovered == &redacted),
            explicit: options
                .model_filters
                .iter()
                .any(|explicit| provider.redact_text(explicit) == redacted),
            frozen_from_resume: true,
        });
    }
}

/// 插入或更新一个候选模型的来源标记。
fn insert_candidate(
    merged: &mut BTreeMap<String, CandidateModelRecord>,
    order: &mut Vec<String>,
    model: &str,
    configured: bool,
    discovered: bool,
    explicit: bool,
) {
    if validate_inline_value("候选模型标识", model).is_err() {
        return;
    }
    let record = merged.entry(model.to_owned()).or_insert_with(|| {
        order.push(model.to_owned());
        CandidateModelRecord {
            model: model.to_owned(),
            configured: false,
            discovered: false,
            explicit: false,
            frozen_from_resume: false,
        }
    });
    record.configured |= configured;
    record.discovered |= discovered;
    record.explicit |= explicit;
}

/// 真实发送一个能力请求并根据响应执行语义断言。
#[allow(clippy::too_many_arguments)]
async fn run_probe(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: ProbeKind,
    options: &RuntimeOptions,
    run_id: &str,
) -> ProbeRecord {
    let marker = expected_marker(provider, model, protocol, response_mode, capability, run_id);
    let started = Instant::now();
    if capability == ProbeKind::StreamInterruption {
        return run_stream_interruption_probe(
            provider,
            model,
            protocol,
            response_mode,
            options,
            run_id,
            marker,
            started,
        )
        .await;
    }
    let config =
        match provider.provider_config(protocol, response_mode, options.request_timeout_secs) {
            Ok(config) => config,
            Err(message) => {
                return failed_local_probe(
                    provider,
                    model,
                    protocol,
                    response_mode,
                    capability,
                    run_id,
                    marker_for_report(capability, &marker),
                    started.elapsed().as_millis(),
                    message,
                );
            }
        };
    let endpoint_path = config
        .base_url()
        .join(config.endpoints.for_protocol(protocol))
        .map(|url| provider.redacted_endpoint_path(url.path()))
        .unwrap_or_else(|_| {
            provider.redacted_endpoint_path(config.endpoints.for_protocol(protocol))
        });
    let max_event_bytes = config.max_event_bytes;
    let (client, trace) = match ProviderClient::new_traced(config) {
        Ok(client) => client,
        Err(error) => {
            return failed_local_probe(
                provider,
                model,
                protocol,
                response_mode,
                capability,
                run_id,
                marker_for_report(capability, &marker),
                started.elapsed().as_millis(),
                error.to_string(),
            );
        }
    };

    if capability == ProbeKind::InvalidParameter {
        let result = client
            .complete(invalid_parameter_request(model, &marker))
            .await;
        let mut record = expected_error_probe_record(
            provider,
            model,
            protocol,
            response_mode,
            capability.as_str(),
            run_id,
            &marker,
            endpoint_path,
            started.elapsed().as_millis(),
            result,
            |error| matches!(error, ModelError::InvalidRequest { .. }),
            "invalid_parameter_rejected",
            "越界采样参数被远端拒绝并归一化为 invalid_request",
            "越界采样参数未被远端稳定拒绝为 invalid_request",
        );
        attach_fixture_evidence(
            &mut record,
            &trace,
            protocol,
            max_event_bytes,
            FixtureReplayRequirement::ExpectedHttpError,
            provider,
        )
        .await;
        return record;
    }

    if capability == ProbeKind::ContextOverflow {
        let result = client
            .complete(context_overflow_request(model, &marker))
            .await;
        let mut record = expected_error_probe_record(
            provider,
            model,
            protocol,
            response_mode,
            capability.as_str(),
            run_id,
            &marker,
            endpoint_path,
            started.elapsed().as_millis(),
            result,
            |error| matches!(error, ModelError::ContextLengthExceeded { .. }),
            "context_overflow_classified",
            "超大纯合成输入被归一化为 context_length",
            "超大纯合成输入未被稳定归一化为 context_length",
        );
        attach_fixture_evidence(
            &mut record,
            &trace,
            protocol,
            max_event_bytes,
            FixtureReplayRequirement::ExpectedHttpOrAdapterError,
            provider,
        )
        .await;
        return record;
    }

    if capability == ProbeKind::Cancellation {
        let mut record = run_cancellation_probe(
            provider,
            model,
            protocol,
            response_mode,
            options,
            run_id,
            endpoint_path,
            client,
            marker,
            started,
        )
        .await;
        attach_fixture_evidence(
            &mut record,
            &trace,
            protocol,
            max_event_bytes,
            FixtureReplayRequirement::Cancellation,
            provider,
        )
        .await;
        return record;
    }

    let mut attempts = 0;
    let mut final_error = None;
    while attempts < options.max_attempts {
        attempts += 1;
        match execute_probe_attempt(&client, model, protocol, capability, &marker, provider).await {
            Ok(success) => {
                let response = success.response;
                let evaluation = success.evaluation;
                let actual_text = response_text(&response);
                let passed = evaluation
                    .assertions
                    .iter()
                    .all(|assertion| assertion.passed);
                // 已归一化为不可重试的拒绝不能被语义断言重试路径重新发送。
                // 无错误的模型遵循性失败仍遵循原有尝试预算。
                let semantic_retry_allowed = evaluation
                    .normalized_error
                    .as_ref()
                    .is_none_or(|error| error.retryable);
                if !passed && semantic_retry_allowed && attempts < options.max_attempts {
                    sleep(retry_delay(
                        attempts,
                        &ModelError::ProviderUnavailable {
                            message: "模型响应暂未满足能力契约".to_owned(),
                            status_code: None,
                            retryable: true,
                        },
                    ))
                    .await;
                    continue;
                }
                let stable_key = probe_stable_key(
                    run_id,
                    &provider.id,
                    model,
                    protocol_name(protocol),
                    response_mode_name(response_mode),
                    capability.as_str(),
                );
                let mut record = ProbeRecord {
                    actual_text_evidence: Some(ActualTextEvidence::from_text(
                        provider,
                        &stable_key,
                        &actual_text,
                    )),
                    stable_key,
                    provider_id: provider.redact_text(&provider.id),
                    model: provider.redact_text(model),
                    protocol: protocol_name(protocol).to_owned(),
                    response_mode: response_mode_name(response_mode).to_owned(),
                    capability: capability.as_str().to_owned(),
                    endpoint_path,
                    status: if passed {
                        "passed".to_owned()
                    } else {
                        "contract_violation".to_owned()
                    },
                    attempts,
                    latency_ms: started.elapsed().as_millis(),
                    expected_text: marker_for_report(capability, &marker),
                    synthetic_marker: Some(marker.clone()),
                    response: Some(ResponseEvidence::from_response(&response, provider)),
                    assertions: evaluation.assertions,
                    cancellation: None,
                    skip_evidence: None,
                    fixture_paths: Vec::new(),
                    recovered_from: None,
                    fixture_replay: None,
                    normalized_error: evaluation.normalized_error,
                    wire_response_shapes: Vec::new(),
                    wire_exchanges: Vec::new(),
                    wire_exchange_outcomes: Vec::new(),
                };
                attach_fixture_evidence(
                    &mut record,
                    &trace,
                    protocol,
                    max_event_bytes,
                    FixtureReplayRequirement::SuccessfulResponse,
                    provider,
                )
                .await;
                return record;
            }
            Err(error) => {
                let retryable = error.is_retryable();
                let delay = retry_delay(attempts, &error);
                final_error = Some(error);
                if !retryable || attempts >= options.max_attempts {
                    break;
                }
                sleep(delay).await;
            }
        }
    }

    let mut record = failed_remote_probe(
        provider,
        model,
        protocol,
        response_mode,
        capability,
        run_id,
        &marker,
        endpoint_path,
        marker_for_report(capability, &marker),
        attempts,
        started.elapsed().as_millis(),
        final_error.as_ref(),
    );
    attach_fixture_evidence(
        &mut record,
        &trace,
        protocol,
        max_event_bytes,
        FixtureReplayRequirement::SuccessfulResponse,
        provider,
    )
    .await;
    record
}

/// 线级证据在当前进程完成 Adapter 复核时采用的能力策略。
#[derive(Clone, Copy)]
enum FixtureReplayRequirement {
    /// 普通能力必须捕获并成功重放至少一个完整的 2xx JSON 或 SSE 响应。
    SuccessfulResponse,
    /// 负向诊断必须捕获非 2xx 响应头，错误正文不通过成功响应 Adapter 重放。
    ExpectedHttpError,
    /// 2xx 响应必须由目标 Adapter 稳定重放为与在线相同的协议错误。
    ExpectedAdapterError,
    /// 同一预期错误既可来自非 2xx HTTP，也可来自 2xx 内嵌 Adapter 错误。
    ExpectedHttpOrAdapterError,
    /// 本地取消只要求证明请求已经开始，不声称能够重放不完整响应。
    Cancellation,
}

/// 严格匹配可由 HTTP 状态或成功响应内嵌错误表达的同一个预期错误。
fn expected_http_or_adapter_error_matches(
    exchange: &WireExchange,
    outcome: &FixtureExchangeOutcome,
    record: &ProbeRecord,
) -> bool {
    exchange.response_status.is_some()
        && matches!(outcome, FixtureExchangeOutcome::Error { .. })
        && fixture_outcome_matches_record(outcome, record)
}

/// 把真实 HTTP 交换、离线 Adapter 重放与在线归一化结果绑定到同一探测记录。
async fn attach_fixture_evidence(
    record: &mut ProbeRecord,
    collector: &WireTraceCollector,
    protocol: ProviderProtocol,
    max_event_bytes: usize,
    requirement: FixtureReplayRequirement,
    provider: &ProviderEntry,
) {
    let exchanges = collector.exchanges();
    let exchange_count = exchanges.len();
    record.wire_exchanges = exchanges.clone();
    record.wire_response_shapes = exchanges
        .iter()
        .map(|exchange| {
            inspect_wire_response_shape(
                protocol,
                exchange.response_status,
                exchange.response_content_type.as_deref(),
                &exchange.response_body,
                exchange.response_body_eof_observed,
                exchange.response_body_truncated,
            )
        })
        .collect();

    let first_tool_call_id = if record.capability == ProbeKind::ToolResultImageRoundTrip.as_str() {
        match exchanges.first() {
            Some(exchange) => replay_first_tool_call_id(exchange, protocol, max_event_bytes).await,
            None => None,
        }
    } else {
        None
    };
    if record.capability == ProbeKind::ToolResultImageRoundTrip.as_str() {
        record.assertions.extend(image_round_trip_wire_assertions(
            record,
            &exchanges,
            protocol,
            first_tool_call_id.as_deref(),
        ));
    }

    if exchange_count == 0 {
        record.wire_response_shapes.clear();
        record.wire_exchange_outcomes.clear();
        record.fixture_replay = Some(FixtureReplayEvidence {
            status: "unavailable".to_owned(),
            exchange_count: 0,
            replayed_exchanges: 0,
            reason: Some("没有捕获任何真实 HTTP 交换".to_owned()),
        });
        record.assertions.push(assertion(
            "wire_fixture_present",
            false,
            "已捕获真实 HTTP 交换",
            "没有捕获真实 HTTP 交换，结果不能作为线级契约证据",
        ));
        if record.status == "passed" {
            record.status = "unverified".to_owned();
        }
        return;
    }

    let mut outcomes = Vec::with_capacity(exchange_count);
    let request_only_final = matches!(requirement, FixtureReplayRequirement::Cancellation)
        && record.cancellation.as_ref().is_some_and(|evidence| {
            evidence.local_future_dropped
                && !evidence.completed_before_cancel
                && record.response.is_none()
                && record.normalized_error.is_none()
        });
    for (index, exchange) in exchanges.iter().enumerate() {
        if request_only_final && index + 1 == exchange_count {
            outcomes.push(FixtureExchangeOutcome::RequestOnly);
        } else {
            outcomes.push(
                replay_captured_exchange(
                    exchange,
                    protocol,
                    exchange.max_event_bytes,
                    provider,
                    &record.stable_key,
                )
                .await,
            );
        }
    }
    let capture_config_matches = exchanges
        .iter()
        .all(|exchange| exchange.max_event_bytes == max_event_bytes);
    let replayed_exchanges = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome,
                FixtureExchangeOutcome::Response { .. } | FixtureExchangeOutcome::Error { .. }
            )
        })
        .count();
    let unavailable_reason = outcomes.iter().find_map(|outcome| match outcome {
        FixtureExchangeOutcome::Unavailable { reason } => Some(reason.clone()),
        FixtureExchangeOutcome::ObservedTerminalError { .. } => {
            Some("线级交换只记录了在线传输终态，磁盘无法独立重放该外部失败".to_owned())
        }
        FixtureExchangeOutcome::Response { .. }
        | FixtureExchangeOutcome::Error { .. }
        | FixtureExchangeOutcome::RequestOnly => None,
    });
    let has_unavailable_outcome = unavailable_reason.is_some();
    let final_outcome = outcomes.last();
    if matches!(requirement, FixtureReplayRequirement::Cancellation)
        && record
            .cancellation
            .as_ref()
            .is_some_and(|evidence| evidence.completed_before_cancel)
        && record.normalized_error.is_none()
    {
        if let Some(FixtureExchangeOutcome::Response {
            response,
            actual_text_evidence,
        }) = final_outcome
        {
            record.response.get_or_insert_with(|| response.clone());
            record
                .actual_text_evidence
                .get_or_insert_with(|| actual_text_evidence.clone());
        }
    }
    let requirement_matches = match (requirement, exchanges.last(), final_outcome) {
        (
            FixtureReplayRequirement::SuccessfulResponse,
            _,
            Some(FixtureExchangeOutcome::Response { .. }),
        ) => true,
        (
            FixtureReplayRequirement::ExpectedHttpError,
            Some(exchange),
            Some(FixtureExchangeOutcome::Error { .. }),
        ) => exchange
            .response_status
            .is_some_and(|status| !(200..300).contains(&status)),
        (
            FixtureReplayRequirement::ExpectedAdapterError,
            Some(exchange),
            Some(FixtureExchangeOutcome::Error { .. }),
        ) => exchange
            .response_status
            .is_some_and(|status| (200..300).contains(&status)),
        (FixtureReplayRequirement::ExpectedHttpOrAdapterError, Some(exchange), Some(outcome)) => {
            expected_http_or_adapter_error_matches(exchange, outcome, record)
        }
        (FixtureReplayRequirement::Cancellation, _, Some(outcome)) => {
            fixture_outcome_matches_record(outcome, record)
        }
        (FixtureReplayRequirement::Cancellation, _, None)
        | (FixtureReplayRequirement::SuccessfulResponse, _, _)
        | (FixtureReplayRequirement::ExpectedHttpError, _, _)
        | (FixtureReplayRequirement::ExpectedAdapterError, _, _)
        | (FixtureReplayRequirement::ExpectedHttpOrAdapterError, _, _) => false,
    };
    let expected_matches =
        final_outcome.is_some_and(|outcome| fixture_outcome_matches_record(outcome, record));
    let request_only_exchanges = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, FixtureExchangeOutcome::RequestOnly))
        .count();
    let complete = unavailable_reason.is_none()
        && replayed_exchanges + request_only_exchanges == exchange_count
        && request_only_exchanges == usize::from(request_only_final)
        && capture_config_matches
        && requirement_matches
        && expected_matches;
    let reason = unavailable_reason
        .or_else(|| {
            (!capture_config_matches)
                .then(|| "Trace 捕获的 maxEventBytes 与实际 ProviderConfig 不一致".to_owned())
        })
        .or_else(|| {
            (!requirement_matches)
                .then(|| "最终线级交换没有复现当前能力要求的响应或错误类型".to_owned())
        })
        .or_else(|| {
            (!expected_matches).then(|| {
                "内存 Adapter 复核的最终 response、actual_text_evidence 或 normalized_error 与在线记录不一致"
                    .to_owned()
            })
        });
    record.wire_exchange_outcomes = outcomes;
    record.assertions.push(assertion(
        "wire_adapter_replay",
        complete,
        "当前进程捕获的全部 HTTP 错误、2xx JSON 或 SSE 已由同一 Adapter 内存重放，且最终响应、文本或错误与在线记录逐字段一致",
        reason
            .as_deref()
            .unwrap_or("线级捕获未通过完整 Adapter 内存重放"),
    ));
    let local_cancellation_reason = request_only_final.then(|| {
        "本地取消计时器获胜并丢弃最后一个在途 Future 或 Stream；此前交换仍已逐一重放".to_owned()
    });
    record.fixture_replay = Some(FixtureReplayEvidence {
        status: if complete && request_only_final {
            "not_applicable".to_owned()
        } else if complete {
            "passed".to_owned()
        } else if has_unavailable_outcome {
            "unavailable".to_owned()
        } else {
            "failed".to_owned()
        },
        exchange_count,
        replayed_exchanges,
        reason: reason.or(local_cancellation_reason),
    });
    if record.status == "passed" && !complete {
        record.status = if record
            .fixture_replay
            .as_ref()
            .is_some_and(|evidence| evidence.status == "unavailable")
        {
            "unverified".to_owned()
        } else {
            "contract_violation".to_owned()
        };
    }
    if record.capability == ProbeKind::ToolResultImageRoundTrip.as_str()
        && record.status == "passed"
        && record.assertions.iter().any(|assertion| !assertion.passed)
    {
        record.status = "contract_violation".to_owned();
    }
}

/// 通过同一 Adapter 重放首轮响应，取得首轮真实归一化工具调用标识。
async fn replay_first_tool_call_id(
    exchange: &WireExchange,
    protocol: ProviderProtocol,
    max_event_bytes: usize,
) -> Option<String> {
    let status = exchange.response_status?;
    if !(200..300).contains(&status)
        || exchange.response_body_truncated
        || !exchange.response_body_eof_observed
    {
        return None;
    }
    let content_type = exchange
        .response_content_type
        .as_deref()
        .unwrap_or("application/json");
    replay_wire_response(
        protocol,
        content_type,
        &exchange.response_body,
        max_event_bytes,
    )
    .await
    .ok()
    .and_then(|response| tool_calls(&response).first().map(|call| call.id.clone()))
}

/// 校验图片工具结果的 HTTP 交换数量、调用关联和协议线级图片编码。
fn image_round_trip_wire_assertions(
    record: &ProbeRecord,
    exchanges: &[WireExchange],
    protocol: ProviderProtocol,
    first_tool_call_id: Option<&str>,
) -> Vec<SemanticAssertion> {
    let chat_unsupported = protocol == ProviderProtocol::ChatCompletions
        && record
            .normalized_error
            .as_ref()
            .is_some_and(|error| error.kind == "unsupported_capability" && !error.retryable);
    let expected_exchange_count = if chat_unsupported { 1 } else { 2 };
    let exchange_count_matches = exchanges.len() == expected_exchange_count
        && exchanges.iter().all(|exchange| {
            exchange
                .response_status
                .is_some_and(|status| (200..300).contains(&status))
        });
    let second = exchanges.get(1);
    let semantic_tool_result = second.and_then(|exchange| {
        exchange
            .model_request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|block| match block {
                ContentBlock::ToolResult { tool_result } => Some(tool_result),
                ContentBlock::Text { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::ToolCall { .. } => None,
            })
    });
    let semantic_call_id = semantic_tool_result.map(|result| result.tool_call_id.as_str());
    let call_id_matches = if chat_unsupported {
        true
    } else {
        first_tool_call_id.is_some_and(|first_call_id| !first_call_id.trim().is_empty())
            && semantic_call_id.is_some_and(|call_id| {
                Some(call_id) == first_tool_call_id && !call_id.trim().is_empty()
            })
            && second.is_some_and(|exchange| {
                wire_tool_result_call_id(protocol, &exchange.request_body)
                    .is_some_and(|wire_id| Some(wire_id.as_str()) == first_tool_call_id)
            })
    };
    let first_request_excludes_final_marker =
        record.synthetic_marker.as_deref().is_some_and(|marker| {
            exchanges.first().is_some_and(|exchange| {
                !serde_json::to_string(&exchange.request_body)
                    .unwrap_or_default()
                    .contains(marker)
            })
        });
    let image_encoding_matches = if chat_unsupported {
        true
    } else {
        second.is_some_and(|exchange| {
            wire_tool_result_image_matches(
                protocol,
                &exchange.request_body,
                record.synthetic_marker.as_deref().unwrap_or_default(),
            )
        })
    };
    vec![
        assertion(
            "tool_result_image_http_exchange_count",
            exchange_count_matches,
            if chat_unsupported {
                "Chat Completions 在 Adapter 拒绝前只产生首轮 HTTP 交换"
            } else {
                "Messages/Responses 图片工具结果产生了两次成功 HTTP 交换"
            },
            "图片工具结果 HTTP 交换数量或成功状态不符合协议边界",
        ),
        assertion(
            "tool_result_image_call_id_preserved",
            call_id_matches,
            if chat_unsupported {
                "Chat Completions 不支持图片工具结果，无第二轮调用关联可伪造"
            } else {
                "第二轮线级请求保留了首轮工具调用标识"
            },
            "第二轮线级请求没有保留首轮工具调用标识",
        ),
        assertion(
            "tool_result_image_first_request_excludes_final_marker",
            first_request_excludes_final_marker,
            "首轮线级请求没有携带第二轮最终标记",
            "首轮线级请求意外携带了第二轮最终标记",
        ),
        assertion(
            "tool_result_image_wire_encoding",
            image_encoding_matches,
            if chat_unsupported {
                "Chat Completions 明确拒绝图片工具结果，未伪造图片线级请求"
            } else {
                "第二轮线级请求包含固定 PNG 与第二轮文本"
            },
            "第二轮线级请求没有按目标协议编码固定 PNG 或文本",
        ),
    ]
}

/// 从 Messages 或 Responses 线级请求提取函数调用结果的调用标识。
fn wire_tool_result_call_id(protocol: ProviderProtocol, body: &Value) -> Option<String> {
    match protocol {
        ProviderProtocol::Messages => {
            body.get("messages")
                .and_then(Value::as_array)
                .and_then(|messages| {
                    messages.iter().find_map(|message| {
                        message
                            .get("content")
                            .and_then(Value::as_array)
                            .and_then(|content| {
                                content.iter().find_map(|block| {
                                    (block.get("type").and_then(Value::as_str)
                                        == Some("tool_result"))
                                    .then(|| block.get("tool_use_id").and_then(Value::as_str))
                                    .flatten()
                                    .map(ToOwned::to_owned)
                                })
                            })
                    })
                })
        }
        ProviderProtocol::Responses => {
            body.get("input")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        (item.get("type").and_then(Value::as_str) == Some("function_call_output"))
                            .then(|| item.get("call_id").and_then(Value::as_str))
                            .flatten()
                            .map(ToOwned::to_owned)
                    })
                })
        }
        ProviderProtocol::ChatCompletions => None,
    }
}

/// 校验 Messages 或 Responses 第二轮请求中的文本、图片类型、顺序和完整 Base64。
fn wire_tool_result_image_matches(protocol: ProviderProtocol, body: &Value, marker: &str) -> bool {
    let expected_url = format!("data:image/png;base64,{SYNTHETIC_PNG_BASE64}");
    match protocol {
        ProviderProtocol::Messages => body
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| {
                messages.iter().find_map(|message| {
                    message
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|content| {
                            content.iter().find_map(|block| {
                                (block.get("type").and_then(Value::as_str) == Some("tool_result"))
                                    .then(|| block.get("content").and_then(Value::as_array))
                                    .flatten()
                                    .map(|content| {
                                        content.len() == 2
                                            && content[0].get("type").and_then(Value::as_str)
                                                == Some("text")
                                            && content[0]
                                                .get("text")
                                                .and_then(Value::as_str)
                                                .is_some_and(|text| text.contains(marker))
                                            && content[1].get("type").and_then(Value::as_str)
                                                == Some("image")
                                            && content[1]
                                                .get("source")
                                                .and_then(|source| source.get("type"))
                                                .and_then(Value::as_str)
                                                == Some("base64")
                                            && content[1]
                                                .get("source")
                                                .and_then(|source| source.get("media_type"))
                                                .and_then(Value::as_str)
                                                == Some("image/png")
                                            && content[1]
                                                .get("source")
                                                .and_then(|source| source.get("data"))
                                                .and_then(Value::as_str)
                                                == Some(SYNTHETIC_PNG_BASE64)
                                    })
                            })
                        })
                })
            })
            .unwrap_or(false),
        ProviderProtocol::Responses => body
            .get("input")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find_map(|item| {
                    (item.get("type").and_then(Value::as_str) == Some("function_call_output"))
                        .then(|| item.get("output").and_then(Value::as_array))
                        .flatten()
                        .map(|output| {
                            output.len() == 2
                                && output[0].get("type").and_then(Value::as_str)
                                    == Some("input_text")
                                && output[0]
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .is_some_and(|text| text.contains(marker))
                                && output[1].get("type").and_then(Value::as_str)
                                    == Some("input_image")
                                && output[1].get("image_url").and_then(Value::as_str)
                                    == Some(expected_url.as_str())
                        })
                })
            })
            .unwrap_or(false),
        ProviderProtocol::ChatCompletions => false,
    }
}

/// 使用线上阶段的原始捕获字节，按同一协议 Adapter 归一化一个交换。
async fn replay_captured_exchange(
    exchange: &WireExchange,
    protocol: ProviderProtocol,
    max_event_bytes: usize,
    provider: &ProviderEntry,
    stable_key: &str,
) -> FixtureExchangeOutcome {
    let Some(status) = exchange.response_status else {
        if let Some(error) = &exchange.terminal_error {
            return FixtureExchangeOutcome::ObservedTerminalError {
                error: normalize_error(provider, error),
            };
        }
        return FixtureExchangeOutcome::Unavailable {
            reason: "线级交换没有收到 HTTP 响应头".to_owned(),
        };
    };
    if matches!(
        exchange.terminal_error.as_ref(),
        Some(ModelError::Transport { .. })
    ) {
        return FixtureExchangeOutcome::ObservedTerminalError {
            error: normalize_error(
                provider,
                exchange
                    .terminal_error
                    .as_ref()
                    .expect("已经匹配的终态传输错误必须存在"),
            ),
        };
    }
    if exchange.response_body_truncated {
        return FixtureExchangeOutcome::Unavailable {
            reason: "线级响应超过 Fixture 捕获上限，正文已截断".to_owned(),
        };
    }
    if std::str::from_utf8(&exchange.response_body).is_err() {
        return FixtureExchangeOutcome::Unavailable {
            reason: "线级响应不是可持久化并重放的 UTF-8 JSON 或 SSE".to_owned(),
        };
    }
    if !(200..300).contains(&status) {
        let error = replay_wire_error_response(status, &exchange.response_body);
        return FixtureExchangeOutcome::Error {
            error: normalize_error(provider, &error),
        };
    }
    let content_type = exchange
        .response_content_type
        .as_deref()
        .unwrap_or("application/json");
    match replay_wire_response(
        protocol,
        content_type,
        &exchange.response_body,
        max_event_bytes,
    )
    .await
    {
        Ok(response) => FixtureExchangeOutcome::Response {
            response: ResponseEvidence::from_response(&response, provider),
            actual_text_evidence: ActualTextEvidence::from_text(
                provider,
                stable_key,
                &response_text(&response),
            ),
        },
        Err(error) => FixtureExchangeOutcome::Error {
            error: normalize_error(provider, &error),
        },
    }
}

/// 判断记录是否携带由已成功解码响应产生的原生结构化输出语义错误。
fn has_structured_output_semantic_error(record: &ProbeRecord) -> bool {
    record.capability == ProbeKind::StructuredOutput.as_str()
        && record.status == "contract_violation"
        && record.normalized_error.as_ref().is_some_and(|error| {
            matches!(
                error.kind.as_str(),
                "provider_contract_violation_missing_output"
                    | "provider_contract_violation_invalid_json"
                    | "provider_contract_violation_schema"
                    | "provider_contract_violation_unexpected_content"
                    | "provider_contract_violation_incomplete"
                    | "provider_contract_violation_emulation_protocol"
            ) && !error.retryable
                && error.http_status.is_none()
        })
}

/// 逐字段比较最后一个线级交换与探测记录保存的传输终态，同时保留能力层语义错误。
fn fixture_outcome_matches_record(outcome: &FixtureExchangeOutcome, record: &ProbeRecord) -> bool {
    match (
        &record.response,
        &record.actual_text_evidence,
        &record.normalized_error,
        outcome,
    ) {
        (
            Some(expected_response),
            Some(expected_text),
            None,
            FixtureExchangeOutcome::Response {
                response,
                actual_text_evidence,
            },
        ) => expected_response == response && expected_text == actual_text_evidence,
        (
            Some(expected_response),
            Some(expected_text),
            Some(_),
            FixtureExchangeOutcome::Response {
                response,
                actual_text_evidence,
            },
        ) if has_structured_output_semantic_error(record)
            || has_tool_result_image_unsupported_error(record) =>
        {
            expected_response == response && expected_text == actual_text_evidence
        }
        (None, None, Some(expected), FixtureExchangeOutcome::Error { error }) => expected == error,
        (None, None, Some(expected), FixtureExchangeOutcome::ObservedTerminalError { error }) => {
            expected == error
        }
        (None, None, None, FixtureExchangeOutcome::RequestOnly) => {
            record.cancellation.as_ref().is_some_and(|evidence| {
                evidence.local_future_dropped && !evidence.completed_before_cancel
            })
        }
        (Some(_), _, _, _) | (_, Some(_), _, _) | (_, _, Some(_), _) | (None, None, None, _) => {
            false
        }
    }
}

/// 识别 Chat Completions 图片工具结果在本地 Adapter 被明确拒绝的响应保留记录。
fn has_tool_result_image_unsupported_error(record: &ProbeRecord) -> bool {
    record.capability == ProbeKind::ToolResultImageRoundTrip.as_str()
        && record.status == "contract_violation"
        && record.normalized_error.as_ref().is_some_and(|error| {
            error.kind == "unsupported_capability"
                && !error.retryable
                && error.http_status.is_none()
        })
}

/// 单次能力尝试成功后用于报告的响应与完整语义断言。
struct SuccessfulProbe {
    /// 最后一轮统一响应。
    response: ModelResponse,
    /// 包含所有中间轮次的语义断言。
    evaluation: Evaluation,
}

/// 执行单请求或多轮能力场景的一次完整尝试。
async fn execute_probe_attempt(
    client: &ProviderClient,
    model: &str,
    protocol: ProviderProtocol,
    capability: ProbeKind,
    marker: &str,
    provider: &ProviderEntry,
) -> Result<SuccessfulProbe, ModelError> {
    match capability {
        ProbeKind::ToolResultRoundTrip => {
            execute_tool_result_round_trip(client, model, marker).await
        }
        ProbeKind::ToolResultImageRoundTrip => {
            execute_tool_result_image_round_trip(client, model, protocol, marker, provider).await
        }
        ProbeKind::MultiTurn => execute_multi_turn(client, model, marker).await,
        ProbeKind::PromptCaching => execute_prompt_cache(client, model, marker).await,
        ProbeKind::Text
        | ProbeKind::ToolCalling
        | ProbeKind::ParallelToolCalling
        | ProbeKind::Reasoning
        | ProbeKind::Usage
        | ProbeKind::StructuredOutput
        | ProbeKind::OutputLimit => {
            let request = probe_request(model, protocol, capability, marker)?;
            let response = client.complete(request).await?;
            let evaluation = evaluate_response(capability, &response, marker, provider);
            Ok(SuccessfulProbe {
                response,
                evaluation,
            })
        }
        ProbeKind::InvalidParameter
        | ProbeKind::ContextOverflow
        | ProbeKind::StreamInterruption
        | ProbeKind::Cancellation => Err(ModelError::InvalidRequest {
            message: format!("能力 {} 必须由专用探测路径执行", capability.as_str()),
        }),
    }
}

/// 构造不会携带用户数据的能力请求。
fn probe_request(
    model: &str,
    protocol: ProviderProtocol,
    capability: ProbeKind,
    marker: &str,
) -> Result<ModelRequest, ModelError> {
    let request = match capability {
        ProbeKind::Text => text_request(model, marker),
        ProbeKind::ToolCalling => tool_calling_request(model, marker),
        ProbeKind::ParallelToolCalling => parallel_tool_calling_request(model, marker),
        ProbeKind::Reasoning => reasoning_request(model, marker, protocol),
        ProbeKind::Usage => text_request(model, marker),
        ProbeKind::StructuredOutput => structured_output_request(model, marker),
        ProbeKind::OutputLimit => output_limit_request(model, marker),
        ProbeKind::Cancellation => cancellation_request(model, marker),
        ProbeKind::ToolResultRoundTrip
        | ProbeKind::ToolResultImageRoundTrip
        | ProbeKind::MultiTurn
        | ProbeKind::PromptCaching
        | ProbeKind::InvalidParameter
        | ProbeKind::ContextOverflow
        | ProbeKind::StreamInterruption => {
            return Err(ModelError::InvalidRequest {
                message: format!("能力 {} 必须由专用探测路径执行", capability.as_str()),
            });
        }
    };
    Ok(request)
}

/// 构造精确文本探测请求。
fn text_request(model: &str, marker: &str) -> ModelRequest {
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!("只输出下一行的精确标记，不要添加标点、Markdown、空格或解释：\n{marker}"),
        )],
    );
    request.max_output_tokens = Some(1024);
    request
}

/// 构造指定工具与精确参数的工具调用请求。
fn tool_calling_request(model: &str, marker: &str) -> ModelRequest {
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!(
                "只调用一次 {TOOL_NAME}，不要输出普通文本。参数 marker 必须是 {marker}，count 必须是 {EXPECTED_COUNT}。"
            ),
        )],
    );
    request.tools = vec![ToolDefinition::new(
        TOOL_NAME,
        "回传兼容性探测标记和固定整数，不执行任何外部操作。",
        json!({
            "type": "object",
            "properties": {
                "marker": { "type": "string", "const": marker },
                "count": { "type": "integer", "const": EXPECTED_COUNT }
            },
            "required": ["marker", "count"],
            "additionalProperties": false
        }),
    )];
    request.tool_choice = ToolChoice::Specific {
        name: TOOL_NAME.to_owned(),
    };
    request.parallel_tool_calls = Some(false);
    request.max_output_tokens = Some(1024);
    request
}

/// 构造必须在同一响应中请求两个独立工具的探测请求。
fn parallel_tool_calling_request(model: &str, marker: &str) -> ModelRequest {
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!(
                "必须在同一轮中各调用一次 {PARALLEL_LEFT_TOOL} 和 {PARALLEL_RIGHT_TOOL}，不要输出普通文本。两个调用的 marker 都必须是 {marker}。"
            ),
        )],
    );
    request.tools = vec![
        parallel_tool_definition(PARALLEL_LEFT_TOOL, marker, "left"),
        parallel_tool_definition(PARALLEL_RIGHT_TOOL, marker, "right"),
    ];
    request.tool_choice = ToolChoice::Required;
    request.parallel_tool_calls = Some(true);
    request.max_output_tokens = Some(2048);
    request
}

/// 构造并行探测中一个具有固定名称和参数的无副作用工具。
fn parallel_tool_definition(name: &str, marker: &str, side: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        "记录并行工具调用的固定方向与合成标记，不执行任何外部操作。",
        json!({
            "type": "object",
            "properties": {
                "marker": { "type": "string", "const": marker },
                "side": { "type": "string", "const": side }
            },
            "required": ["marker", "side"],
            "additionalProperties": false
        }),
    )
}

/// 构造低强度推理和精确最终文本请求。
fn reasoning_request(model: &str, marker: &str, protocol: ProviderProtocol) -> ModelRequest {
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!(
                "先在模型支持的推理通道中判断 2 + 2 是否等于 4，最终普通文本只输出精确标记：\n{marker}"
            ),
        )],
    );
    request.reasoning = Some(match protocol {
        ProviderProtocol::Messages => ReasoningConfig {
            effort: Some(ReasoningEffort::Low),
            max_tokens: Some(512),
            include_summary: false,
        },
        ProviderProtocol::ChatCompletions => ReasoningConfig {
            effort: Some(ReasoningEffort::Low),
            max_tokens: None,
            include_summary: false,
        },
        ProviderProtocol::Responses => ReasoningConfig {
            effort: Some(ReasoningEffort::Low),
            max_tokens: None,
            include_summary: true,
        },
    });
    request.max_output_tokens = Some(2048);
    request
}

/// 构造 Provider 原生 JSON Schema 输出请求。
fn structured_output_request(model: &str, marker: &str) -> ModelRequest {
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            "严格按提供的 JSON Schema 生成唯一结果，不要输出 Markdown 或额外文本。",
        )],
    );
    request.structured_output = Some(structured_output_config(marker));
    request.max_output_tokens = Some(1024);
    request
}

/// 构造两次完全一致且具有长稳定前缀的远端提示缓存请求。
fn prompt_cache_request(model: &str, marker: &str) -> ModelRequest {
    let prefix = "KC_CACHE_PREFIX_0123456789abcdef ".repeat(PROMPT_CACHE_PREFIX_UNITS);
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!(
                "以下全部内容都是 KeenCode 生成的无用户数据缓存前缀。\n{prefix}\n只输出下一行精确标记，不要添加其他内容：\n{marker}"
            ),
        )],
    );
    request.max_output_tokens = Some(1024);
    request
}

/// 构造通过统一层校验但明显越过厂商采样范围的远端无效参数请求。
fn invalid_parameter_request(model: &str, marker: &str) -> ModelRequest {
    let mut request = text_request(model, marker);
    request.temperature = Some(999.0);
    request.max_output_tokens = Some(16);
    request
}

/// 构造超过常见百万 Token 窗口且只包含合成数据的上下文溢出请求。
fn context_overflow_request(model: &str, marker: &str) -> ModelRequest {
    let synthetic_tokens = "x ".repeat(CONTEXT_OVERFLOW_TOKEN_UNITS);
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!(
                "KeenCode 上下文边界探测；以下内容全部为可丢弃合成 Token：\n{synthetic_tokens}\n若服务仍接受请求，只输出 {marker}"
            ),
        )],
    );
    request.max_output_tokens = Some(16);
    request
}

/// 构造足够长、可在本地计时器触发前保持在途状态的请求。
fn cancellation_request(model: &str, marker: &str) -> ModelRequest {
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!(
                "从 1 开始逐行输出连续整数，每行附加标记 {marker}，持续输出直到达到响应上限，不要提前总结或停止。"
            ),
        )],
    );
    request.max_output_tokens = Some(2048);
    request
}

/// 构造应由较小输出预算截断的长响应请求。
fn output_limit_request(model: &str, marker: &str) -> ModelRequest {
    let long_marker = marker.repeat(32);
    let mut request = ModelRequest::new(
        model,
        vec![Message::text(
            MessageRole::User,
            format!("只原样复制下一行，不要解释、拒绝、添加 Markdown 或提前停止：\n{long_marker}"),
        )],
    );
    request.max_output_tokens = Some(8);
    request
}

/// 执行工具调用、工具结果回传和最终文本三段式真实往返。
async fn execute_tool_result_round_trip(
    client: &ProviderClient,
    model: &str,
    marker: &str,
) -> Result<SuccessfulProbe, ModelError> {
    let first_request = tool_calling_request(model, marker);
    let mut messages = first_request.messages.clone();
    let tools = first_request.tools.clone();
    let first_response = client.complete(first_request).await?;
    let mut assertions = prefixed_assertions(
        evaluate_tool_calling(&first_response, marker).assertions,
        "first_",
    );
    let Some(call) = tool_calls(&first_response).first().copied() else {
        assertions.push(assertion(
            "tool_result_round_trip_completed",
            false,
            "工具结果已完成第二轮模型往返",
            "首轮没有可关联的工具调用，无法回传工具结果",
        ));
        return Ok(SuccessfulProbe {
            response: first_response,
            evaluation: Evaluation {
                assertions,
                normalized_error: None,
            },
        });
    };

    messages.push(Message::new(
        MessageRole::Assistant,
        first_response.content.clone(),
    ));
    messages.push(Message::new(
        MessageRole::Tool,
        vec![ContentBlock::ToolResult {
            tool_result: ToolResult::text(
                call.id.clone(),
                format!("工具已完成。最终只输出下一行的精确标记，不要添加任何其他内容：\n{marker}"),
                false,
            ),
        }],
    ));
    let mut second_request = ModelRequest::new(model, messages);
    second_request.tools = tools;
    second_request.tool_choice = ToolChoice::None;
    second_request.parallel_tool_calls = Some(false);
    second_request.max_output_tokens = Some(1024);
    let final_response = client.complete(second_request).await?;
    assertions.extend(prefixed_assertions(
        evaluate_text(&final_response, marker).assertions,
        "final_",
    ));
    assertions.push(assertion(
        "tool_result_round_trip_completed",
        true,
        "工具结果已完成第二轮模型往返",
        "工具结果未完成第二轮模型往返",
    ));
    Ok(SuccessfulProbe {
        response: final_response,
        evaluation: Evaluation {
            assertions,
            normalized_error: None,
        },
    })
}

/// 执行含固定合成 PNG 的工具结果往返；最终标记只允许出现在第二轮响应中。
async fn execute_tool_result_image_round_trip(
    client: &ProviderClient,
    model: &str,
    protocol: ProviderProtocol,
    marker: &str,
    provider: &ProviderEntry,
) -> Result<SuccessfulProbe, ModelError> {
    let first_marker = first_turn_marker(marker);
    let first_request = tool_calling_request(model, &first_marker);
    let mut messages = first_request.messages.clone();
    let tools = first_request.tools.clone();
    let first_response = client.complete(first_request).await?;
    let mut assertions = prefixed_assertions(
        evaluate_tool_calling(&first_response, &first_marker).assertions,
        "first_",
    );
    assertions.push(assertion(
        "image_round_trip_markers_distinct",
        first_marker != marker,
        "首轮工具参数标记与第二轮最终标记不同",
        "首轮工具参数标记意外复用了第二轮最终标记",
    ));
    assertions.push(assertion(
        "first_response_excludes_final_marker",
        !response_text(&first_response).contains(marker),
        "首轮可见响应没有泄露第二轮最终标记",
        "首轮可见响应已经包含第二轮最终标记",
    ));
    let Some(call) = tool_calls(&first_response).first().copied() else {
        assertions.push(assertion(
            "tool_result_image_round_trip_completed",
            false,
            "图片工具结果已完成第二轮模型往返",
            "首轮没有可关联的工具调用，无法回传图片工具结果",
        ));
        return Ok(SuccessfulProbe {
            response: first_response,
            evaluation: Evaluation {
                assertions,
                normalized_error: None,
            },
        });
    };

    messages.push(Message::new(
        MessageRole::Assistant,
        first_response.content.clone(),
    ));
    messages.push(Message::new(
        MessageRole::Tool,
        vec![ContentBlock::ToolResult {
            tool_result: ToolResult::new(
                call.id.clone(),
                vec![
                    ToolResultContent::Text {
                        text: format!(
                            "已完成合成图片读取。最终只输出下一行的精确标记，不要添加任何其他内容：\n{marker}"
                        ),
                    },
                    ToolResultContent::Image {
                        image: ImageContent::from_base64("image/png", SYNTHETIC_PNG_BASE64),
                    },
                ],
                false,
            ),
        }],
    ));
    let mut second_request = ModelRequest::new(model, messages);
    second_request.tools = tools;
    second_request.tool_choice = ToolChoice::None;
    second_request.parallel_tool_calls = Some(false);
    second_request.max_output_tokens = Some(1024);

    let final_response = match client.complete(second_request).await {
        Ok(response) => response,
        Err(error)
            if protocol == ProviderProtocol::ChatCompletions
                && matches!(
                    &error,
                    ModelError::UnsupportedCapability { capability, .. }
                        if capability == "tool_result_image"
                ) =>
        {
            assertions.push(assertion(
                "tool_result_image_supported",
                false,
                "当前协议支持图片工具结果",
                "Chat Completions Adapter 明确报告不支持图片工具结果",
            ));
            assertions.push(assertion(
                "tool_result_image_round_trip_completed",
                false,
                "图片工具结果已完成第二轮模型往返",
                "协议不支持图片工具结果，未伪造第二轮 HTTP 往返",
            ));
            return Ok(SuccessfulProbe {
                response: first_response,
                evaluation: Evaluation {
                    assertions,
                    normalized_error: Some(normalize_error(provider, &error)),
                },
            });
        }
        Err(error) => return Err(error),
    };

    assertions.extend(prefixed_assertions(
        evaluate_text(&final_response, marker).assertions,
        "final_",
    ));
    assertions.push(assertion(
        "tool_result_image_supported",
        true,
        "当前协议支持图片工具结果",
        "当前协议不支持图片工具结果",
    ));
    assertions.push(assertion(
        "tool_result_image_round_trip_completed",
        true,
        "图片工具结果已完成第二轮模型往返",
        "图片工具结果未完成第二轮模型往返",
    ));
    Ok(SuccessfulProbe {
        response: final_response,
        evaluation: Evaluation {
            assertions,
            normalized_error: None,
        },
    })
}

/// 执行两轮用户输入并验证第二轮能够接续首轮助手历史。
async fn execute_multi_turn(
    client: &ProviderClient,
    model: &str,
    marker: &str,
) -> Result<SuccessfulProbe, ModelError> {
    let first_marker = first_turn_marker(marker);
    let first_request = text_request(model, &first_marker);
    let mut messages = first_request.messages.clone();
    let first_response = client.complete(first_request).await?;
    let mut assertions = prefixed_assertions(
        evaluate_text(&first_response, &first_marker).assertions,
        "first_",
    );
    messages.push(Message::new(
        MessageRole::Assistant,
        first_response.content.clone(),
    ));
    messages.push(Message::text(
        MessageRole::User,
        format!("这是同一对话的第二轮。只输出下一行的精确标记，不要添加任何其他内容：\n{marker}"),
    ));
    let mut second_request = ModelRequest::new(model, messages);
    second_request.max_output_tokens = Some(1024);
    let final_response = client.complete(second_request).await?;
    assertions.extend(prefixed_assertions(
        evaluate_text(&final_response, marker).assertions,
        "second_",
    ));
    assertions.push(assertion(
        "multi_turn_completed",
        true,
        "两轮上下文已完成真实模型往返",
        "两轮上下文未完成真实模型往返",
    ));
    Ok(SuccessfulProbe {
        response: final_response,
        evaluation: Evaluation {
            assertions,
            normalized_error: None,
        },
    })
}

/// 使用完全相同的长合成前缀连续请求两次并验证第二次真实缓存读取用量。
async fn execute_prompt_cache(
    client: &ProviderClient,
    model: &str,
    marker: &str,
) -> Result<SuccessfulProbe, ModelError> {
    let request = prompt_cache_request(model, marker);
    let first_response = client.complete(request.clone()).await?;
    let second_response = client.complete(request).await?;
    let mut assertions =
        prefixed_assertions(evaluate_text(&first_response, marker).assertions, "first_");
    assertions.extend(prefixed_assertions(
        evaluate_text(&second_response, marker).assertions,
        "second_",
    ));
    let cache_read_tokens = second_response.usage.cache_read_tokens.unwrap_or(0);
    assertions.push(assertion(
        "second_cache_read_tokens_positive",
        cache_read_tokens > 0,
        "第二次相同长前缀请求报告了正数缓存读取 Token",
        "第二次相同长前缀请求没有报告正数缓存读取 Token",
    ));
    assertions.push(assertion(
        "stable_prefix_requested_twice",
        true,
        "两次请求使用完全相同的长合成前缀与最终标记",
        "两次请求未使用相同稳定前缀",
    ));
    Ok(SuccessfulProbe {
        response: second_response,
        evaluation: Evaluation {
            assertions,
            normalized_error: None,
        },
    })
}

/// 为多轮场景中的断言名称添加稳定阶段前缀。
fn prefixed_assertions(assertions: Vec<SemanticAssertion>, prefix: &str) -> Vec<SemanticAssertion> {
    assertions
        .into_iter()
        .map(|mut assertion| {
            assertion.name = format!("{prefix}{}", assertion.name);
            assertion
        })
        .collect()
}

/// 返回结构化输出探测复用的严格 Schema。
fn structured_output_config(marker: &str) -> StructuredOutputConfig {
    let mut config = StructuredOutputConfig::new(
        "keencode_probe_result",
        json!({
            "type": "object",
            "properties": {
                "marker": { "type": "string", "const": marker },
                "count": { "type": "integer", "const": EXPECTED_COUNT }
            },
            "required": ["marker", "count"],
            "additionalProperties": false
        }),
    );
    config.description = Some("KeenCode Provider 原生结构化输出兼容性探测。".to_owned());
    config
}

/// 对一个成功解析的模型响应执行当前能力的全部语义断言。
fn evaluate_response(
    capability: ProbeKind,
    response: &ModelResponse,
    marker: &str,
    provider: &ProviderEntry,
) -> Evaluation {
    match capability {
        ProbeKind::Text => evaluate_text(response, marker),
        ProbeKind::ToolCalling => evaluate_tool_calling(response, marker),
        ProbeKind::ParallelToolCalling => evaluate_parallel_tool_calling(response, marker),
        ProbeKind::Reasoning => evaluate_reasoning(response, marker),
        ProbeKind::Usage => evaluate_usage(response, marker),
        ProbeKind::StructuredOutput => evaluate_structured_output(response, marker, provider),
        ProbeKind::OutputLimit => evaluate_output_limit(response),
        ProbeKind::ToolResultRoundTrip
        | ProbeKind::ToolResultImageRoundTrip
        | ProbeKind::MultiTurn
        | ProbeKind::PromptCaching => Evaluation {
            assertions: vec![assertion(
                "multi_step_runner_used",
                false,
                "多请求能力已由专用执行路径处理",
                "多请求能力错误进入单响应评估路径",
            )],
            normalized_error: None,
        },
        ProbeKind::InvalidParameter
        | ProbeKind::ContextOverflow
        | ProbeKind::StreamInterruption
        | ProbeKind::Cancellation => Evaluation::default(),
    }
}

/// 一次成功响应的语义断言和可选契约错误。
#[derive(Default)]
struct Evaluation {
    /// 逐项语义断言。
    assertions: Vec<SemanticAssertion>,
    /// 结构化输出校验失败时的稳定错误分类。
    normalized_error: Option<NormalizedError>,
}

/// 验证普通文本、正常结束和无工具调用。
fn evaluate_text(response: &ModelResponse, marker: &str) -> Evaluation {
    let assertions = vec![
        assertion(
            "stop_reason_completed",
            response.stop_reason == StopReason::Completed,
            "结束原因为 completed",
            "结束原因不是 completed",
        ),
        assertion(
            "visible_text_exact",
            response_text(response) == marker,
            "可见文本与合成标记完全一致",
            "可见文本未与合成标记完全一致",
        ),
        assertion(
            "no_tool_call",
            tool_calls(response).is_empty(),
            "响应不包含工具调用",
            "响应意外包含工具调用",
        ),
    ];
    Evaluation {
        assertions,
        normalized_error: None,
    }
}

/// 验证指定工具、唯一调用和精确 JSON 参数。
fn evaluate_tool_calling(response: &ModelResponse, marker: &str) -> Evaluation {
    let calls = tool_calls(response);
    let call = calls.first().copied();
    let arguments = call.and_then(|call| call.arguments.as_object());
    let assertions = vec![
        assertion(
            "stop_reason_tool_use",
            response.stop_reason == StopReason::ToolUse,
            "结束原因为 tool_use",
            "结束原因不是 tool_use",
        ),
        assertion(
            "single_tool_call",
            calls.len() == 1,
            "响应恰好包含一个工具调用",
            "响应没有恰好包含一个工具调用",
        ),
        assertion(
            "tool_call_id_non_empty",
            call.is_some_and(|call| !call.id.trim().is_empty()),
            "工具调用标识非空",
            "工具调用标识为空或缺失",
        ),
        assertion(
            "tool_name_exact",
            call.is_some_and(|call| call.name == TOOL_NAME),
            "工具名称与指定名称完全一致",
            "工具名称与指定名称不一致",
        ),
        assertion(
            "tool_arguments_object",
            arguments.is_some(),
            "工具参数是 JSON 对象",
            "工具参数不是 JSON 对象或缺失",
        ),
        assertion(
            "tool_marker_exact",
            arguments
                .and_then(|value| value.get("marker"))
                .and_then(Value::as_str)
                == Some(marker),
            "工具 marker 参数完全一致",
            "工具 marker 参数缺失或不一致",
        ),
        assertion(
            "tool_count_exact",
            arguments
                .and_then(|value| value.get("count"))
                .and_then(Value::as_i64)
                == Some(EXPECTED_COUNT),
            "工具 count 参数完全一致",
            "工具 count 参数缺失或不一致",
        ),
        assertion(
            "tool_arguments_no_extra_fields",
            arguments.is_some_and(|value| value.len() == 2),
            "工具参数没有额外字段",
            "工具参数包含额外字段或缺失",
        ),
    ];
    Evaluation {
        assertions,
        normalized_error: None,
    }
}

/// 验证两个独立工具均在同一响应中以唯一调用标识出现。
fn evaluate_parallel_tool_calling(response: &ModelResponse, marker: &str) -> Evaluation {
    let calls = tool_calls(response);
    let by_name = calls
        .iter()
        .map(|call| (call.name.as_str(), *call))
        .collect::<BTreeMap<_, _>>();
    let left = by_name.get(PARALLEL_LEFT_TOOL).copied();
    let right = by_name.get(PARALLEL_RIGHT_TOOL).copied();
    let ids = calls
        .iter()
        .map(|call| call.id.as_str())
        .collect::<BTreeSet<_>>();
    let arguments_match = |call: Option<&keencode_model::ToolCall>, side: &str| {
        call.and_then(|call| call.arguments.as_object())
            .is_some_and(|arguments| {
                arguments.len() == 2
                    && arguments.get("marker").and_then(Value::as_str) == Some(marker)
                    && arguments.get("side").and_then(Value::as_str) == Some(side)
            })
    };
    let assertions = vec![
        assertion(
            "stop_reason_tool_use",
            response.stop_reason == StopReason::ToolUse,
            "结束原因为 tool_use",
            "结束原因不是 tool_use",
        ),
        assertion(
            "two_parallel_tool_calls",
            calls.len() == 2,
            "响应恰好包含两个工具调用",
            "响应没有恰好包含两个工具调用",
        ),
        assertion(
            "parallel_tool_names_exact",
            left.is_some() && right.is_some() && by_name.len() == 2,
            "两个工具名称均与定义完全一致",
            "两个工具名称缺失、重复或包含额外名称",
        ),
        assertion(
            "parallel_tool_ids_unique",
            calls.len() == 2 && ids.len() == 2 && ids.iter().all(|id| !id.trim().is_empty()),
            "两个工具调用标识非空且互不相同",
            "工具调用标识为空或重复",
        ),
        assertion(
            "parallel_left_arguments_exact",
            arguments_match(left, "left"),
            "左侧工具参数与 Schema 完全一致",
            "左侧工具参数缺失或与 Schema 不一致",
        ),
        assertion(
            "parallel_right_arguments_exact",
            arguments_match(right, "right"),
            "右侧工具参数与 Schema 完全一致",
            "右侧工具参数缺失或与 Schema 不一致",
        ),
        assertion(
            "parallel_no_visible_text",
            response_text(response).is_empty(),
            "响应没有混入普通文本",
            "响应在工具调用之外混入了普通文本",
        ),
    ];
    Evaluation {
        assertions,
        normalized_error: None,
    }
}

/// 验证正常结束、精确最终文本和至少一项真实推理证据。
fn evaluate_reasoning(response: &ModelResponse, marker: &str) -> Evaluation {
    let reasoning_evidence = response.content.iter().any(|block| match block {
        ContentBlock::Reasoning { reasoning } => {
            !reasoning.text.trim().is_empty()
                || reasoning
                    .summary
                    .as_deref()
                    .is_some_and(|summary| !summary.trim().is_empty())
                || reasoning.continuation.is_some()
        }
        ContentBlock::Text { .. }
        | ContentBlock::Image { .. }
        | ContentBlock::ToolCall { .. }
        | ContentBlock::ToolResult { .. } => false,
    }) || response
        .usage
        .reasoning_tokens
        .is_some_and(|tokens| tokens > 0);
    let assertions = vec![
        assertion(
            "stop_reason_completed",
            response.stop_reason == StopReason::Completed,
            "结束原因为 completed",
            "结束原因不是 completed",
        ),
        assertion(
            "visible_text_exact",
            response_text(response) == marker,
            "最终普通文本与合成标记完全一致",
            "最终普通文本未与合成标记完全一致",
        ),
        assertion(
            "reasoning_evidence_present",
            reasoning_evidence,
            "响应包含推理文本、摘要、续传状态或正数推理 Token",
            "响应没有任何可观测推理证据",
        ),
    ];
    Evaluation {
        assertions,
        normalized_error: None,
    }
}

/// 验证文本语义与 Provider 明确报告的核心 Token 用量。
fn evaluate_usage(response: &ModelResponse, marker: &str) -> Evaluation {
    let mut assertions = evaluate_text(response, marker).assertions;
    assertions.extend([
        assertion(
            "usage_reported",
            response.usage.is_reported(),
            "Provider 至少报告了一个 Token 用量字段",
            "Provider 没有报告任何 Token 用量字段",
        ),
        assertion(
            "input_tokens_positive",
            response.usage.input_tokens.is_some_and(|value| value > 0),
            "Provider 报告了正数输入 Token",
            "Provider 未报告正数输入 Token",
        ),
        assertion(
            "output_tokens_positive",
            response.usage.output_tokens.is_some_and(|value| value > 0),
            "Provider 报告了正数输出 Token",
            "Provider 未报告正数输出 Token",
        ),
        assertion(
            "total_tokens_consistent_if_reported",
            total_tokens_consistent(response),
            "总 Token 缺失或不小于已报告输入与输出 Token 之和",
            "Provider 报告的总 Token 小于输入与输出 Token 之和",
        ),
    ]);
    Evaluation {
        assertions,
        normalized_error: None,
    }
}

/// 判断可选总 Token 是否与已报告的输入、输出值保持基本一致。
fn total_tokens_consistent(response: &ModelResponse) -> bool {
    let Some(total) = response.usage.total_tokens else {
        return true;
    };
    let known = response
        .usage
        .input_tokens
        .unwrap_or(0)
        .saturating_add(response.usage.output_tokens.unwrap_or(0));
    total >= known
}

/// 验证唯一完整 JSON、Schema 和固定字段值。
fn evaluate_structured_output(
    response: &ModelResponse,
    marker: &str,
    provider: &ProviderEntry,
) -> Evaluation {
    let config = structured_output_config(marker);
    let parsed = config.parse_response(response, StructuredOutputEnforcement::Native);
    let mut assertions = vec![assertion(
        "stop_reason_completed",
        response.stop_reason == StopReason::Completed,
        "结束原因为 completed",
        "结束原因不是 completed",
    )];
    match parsed {
        Ok(value) => {
            assertions.push(assertion(
                "unique_json_schema_valid",
                true,
                "响应是唯一完整且满足 Schema 的 JSON 值",
                "响应不是唯一完整且满足 Schema 的 JSON 值",
            ));
            assertions.push(assertion(
                "structured_marker_exact",
                value.get("marker").and_then(Value::as_str) == Some(marker),
                "结构化 marker 字段完全一致",
                "结构化 marker 字段缺失或不一致",
            ));
            assertions.push(assertion(
                "structured_count_exact",
                value.get("count").and_then(Value::as_i64) == Some(EXPECTED_COUNT),
                "结构化 count 字段完全一致",
                "结构化 count 字段缺失或不一致",
            ));
            Evaluation {
                assertions,
                normalized_error: None,
            }
        }
        Err(error) => {
            assertions.push(assertion(
                "unique_json_schema_valid",
                false,
                "响应是唯一完整且满足 Schema 的 JSON 值",
                "响应不是唯一完整且满足 Schema 的 JSON 值",
            ));
            Evaluation {
                assertions,
                normalized_error: Some(normalize_error(provider, &error)),
            }
        }
    }
}

/// 验证输出预算被真实执行并归一化为长度结束原因。
fn evaluate_output_limit(response: &ModelResponse) -> Evaluation {
    let produced_output = !response.content.is_empty()
        || response
            .usage
            .output_tokens
            .is_some_and(|tokens| tokens > 0)
        || response
            .usage
            .reasoning_tokens
            .is_some_and(|tokens| tokens > 0);
    let assertions = vec![
        assertion(
            "stop_reason_max_output_tokens",
            response.stop_reason == StopReason::MaxOutputTokens,
            "结束原因已归一化为 max_output_tokens",
            "结束原因没有归一化为 max_output_tokens",
        ),
        assertion(
            "output_evidence_present",
            produced_output,
            "长度截断前产生了内容或 Provider 报告了输出用量",
            "长度截断响应既没有内容也没有输出用量证据",
        ),
        assertion(
            "no_tool_call",
            tool_calls(response).is_empty(),
            "响应不包含工具调用",
            "响应意外包含工具调用",
        ),
    ];
    Evaluation {
        assertions,
        normalized_error: None,
    }
}

/// 创建一项结果对应固定且不回显远端正文的语义断言。
fn assertion(
    name: &str,
    passed: bool,
    passed_detail: &str,
    failed_detail: &str,
) -> SemanticAssertion {
    SemanticAssertion::new(
        name,
        passed,
        if passed { passed_detail } else { failed_detail },
    )
}

/// 返回响应中的全部工具调用引用。
fn tool_calls(response: &ModelResponse) -> Vec<&keencode_model::ToolCall> {
    response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall { tool_call } => Some(tool_call),
            ContentBlock::Text { .. }
            | ContentBlock::Reasoning { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolResult { .. } => None,
        })
        .collect()
}

/// 从普通文本块按响应顺序归并最终可见文本。
pub(crate) fn response_text(response: &ModelResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Reasoning { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::ToolResult { .. } => None,
        })
        .collect()
}

/// 生成只与当前组合绑定且不包含秘密的短断言标记。
fn expected_marker(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: ProbeKind,
    run_id: &str,
) -> String {
    let stable_key = probe_stable_key(
        run_id,
        &provider.id,
        model,
        protocol_name(protocol),
        response_mode_name(response_mode),
        capability.as_str(),
    );
    marker_from_probe_stable_key(&stable_key, false)
}

/// 仅为需要最终值断言的能力保存合成标记。
fn marker_for_report(capability: ProbeKind, marker: &str) -> Option<String> {
    (capability != ProbeKind::Cancellation).then(|| marker.to_owned())
}

/// 对缓冲或流式调用执行计时器驱动的本地取消。
async fn run_cancellation_attempt(
    client: &ProviderClient,
    request: ModelRequest,
    response_mode: WireResponseMode,
) -> CancellationAttempt {
    let started = Instant::now();
    match response_mode {
        WireResponseMode::Buffered => {
            let mut timer = Box::pin(sleep(Duration::from_millis(CANCEL_AFTER_MS)));
            let mut completion = client.complete(request);
            tokio::select! {
                _ = &mut timer => CancellationAttempt::LocalCancelled {
                    first_event_received: false,
                    observed_latency_ms: started.elapsed().as_millis(),
                },
                result = &mut completion => match result {
                    Ok(response) => CancellationAttempt::CompletedBeforeCancel {
                        first_event_received: false,
                        observed_latency_ms: started.elapsed().as_millis(),
                        response: Some(response),
                    },
                    Err(error) => CancellationAttempt::Failed {
                        error,
                        observed_latency_ms: started.elapsed().as_millis(),
                    },
                }
            }
        }
        WireResponseMode::Streaming => {
            let mut first_event_deadline =
                Box::pin(sleep(Duration::from_millis(FIRST_EVENT_TIMEOUT_MS)));
            let mut opening = client.stream(request);
            let mut stream = tokio::select! {
                _ = &mut first_event_deadline => return CancellationAttempt::Failed {
                    error: ModelError::Transport {
                        message: "取消探测等待流式首事件超时".to_owned(),
                        retryable: true,
                    },
                    observed_latency_ms: started.elapsed().as_millis(),
                },
                result = &mut opening => match result {
                    Ok(stream) => stream,
                    Err(error) => return CancellationAttempt::Failed {
                        error,
                        observed_latency_ms: started.elapsed().as_millis(),
                    },
                }
            };

            {
                let next = poll_fn(|context| stream.as_mut().poll_next(context));
                tokio::select! {
                    _ = &mut first_event_deadline => return CancellationAttempt::Failed {
                        error: ModelError::Transport {
                            message: "取消探测等待流式首事件超时".to_owned(),
                            retryable: true,
                        },
                        observed_latency_ms: started.elapsed().as_millis(),
                    },
                    item = next => match item {
                        Some(Ok(ModelStreamEvent::MessageEnd { .. })) => {
                            return CancellationAttempt::CompletedBeforeCancel {
                                first_event_received: true,
                                observed_latency_ms: started.elapsed().as_millis(),
                                response: None,
                            };
                        }
                        Some(Ok(_)) => {},
                        Some(Err(error)) => return CancellationAttempt::Failed {
                            error,
                            observed_latency_ms: started.elapsed().as_millis(),
                        },
                        None => return CancellationAttempt::Failed {
                            error: ModelError::Protocol {
                                message: "取消探测的事件流在首事件之前关闭".to_owned(),
                            },
                            observed_latency_ms: started.elapsed().as_millis(),
                        },
                    }
                }
            }

            let mut timer = Box::pin(sleep(Duration::from_millis(CANCEL_AFTER_MS)));
            loop {
                let next = poll_fn(|context| stream.as_mut().poll_next(context));
                tokio::select! {
                    _ = &mut timer => return CancellationAttempt::LocalCancelled {
                        first_event_received: true,
                        observed_latency_ms: started.elapsed().as_millis(),
                    },
                    item = next => match item {
                        Some(Ok(ModelStreamEvent::MessageEnd { .. })) => {
                            return CancellationAttempt::CompletedBeforeCancel {
                                first_event_received: true,
                                observed_latency_ms: started.elapsed().as_millis(),
                                response: None,
                            };
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return CancellationAttempt::Failed {
                            error,
                            observed_latency_ms: started.elapsed().as_millis(),
                        },
                        None => return CancellationAttempt::Failed {
                            error: ModelError::Protocol {
                                message: "取消探测的事件流在结束事件之前关闭".to_owned(),
                            },
                            observed_latency_ms: started.elapsed().as_millis(),
                        },
                    }
                }
            }
        }
    }
}

/// 一次取消尝试的内部事实结果。
enum CancellationAttempt {
    /// 计时器先到并通过丢弃 Future 或 Stream 完成本地取消。
    LocalCancelled {
        /// 取消前是否收到过统一流事件。
        first_event_received: bool,
        /// 从调用开始到本地取消的耗时。
        observed_latency_ms: u128,
    },
    /// 远端在取消窗口之前已完成，无法验证在途取消。
    CompletedBeforeCancel {
        /// 流式响应是否至少产生一个事件。
        first_event_received: bool,
        /// 从调用开始到完整结束的耗时。
        observed_latency_ms: u128,
        /// 缓冲模式可获得的完整响应证据。
        response: Option<ModelResponse>,
    },
    /// 调用在取消窗口前失败。
    Failed {
        /// 统一模型错误。
        error: ModelError,
        /// 从调用开始到失败的耗时。
        observed_latency_ms: u128,
    },
}

/// 执行取消探测并记录本地边界而不声称远端停止计费。
#[allow(clippy::too_many_arguments)]
async fn run_cancellation_probe(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    options: &RuntimeOptions,
    run_id: &str,
    endpoint_path: String,
    client: ProviderClient,
    marker: String,
    started: Instant,
) -> ProbeRecord {
    let mut attempts = 0;
    let mut final_error = None;
    let mut final_latency = 0;
    while attempts < options.max_attempts {
        attempts += 1;
        match run_cancellation_attempt(&client, cancellation_request(model, &marker), response_mode)
            .await
        {
            CancellationAttempt::LocalCancelled {
                first_event_received,
                observed_latency_ms,
            } => {
                return ProbeRecord {
                    stable_key: probe_stable_key(
                        run_id,
                        &provider.id,
                        model,
                        protocol_name(protocol),
                        response_mode_name(response_mode),
                        ProbeKind::Cancellation.as_str(),
                    ),
                    provider_id: provider.redact_text(&provider.id),
                    model: provider.redact_text(model),
                    protocol: protocol_name(protocol).to_owned(),
                    response_mode: response_mode_name(response_mode).to_owned(),
                    capability: ProbeKind::Cancellation.as_str().to_owned(),
                    endpoint_path,
                    status: "passed".to_owned(),
                    attempts,
                    latency_ms: started.elapsed().as_millis(),
                    expected_text: None,
                    synthetic_marker: Some(marker.clone()),
                    actual_text_evidence: None,
                    response: None,
                    assertions: cancellation_assertions(
                        response_mode,
                        true,
                        first_event_received,
                        false,
                    ),
                    cancellation: Some(cancellation_evidence(
                        true,
                        first_event_received,
                        false,
                        observed_latency_ms,
                    )),
                    skip_evidence: None,
                    fixture_paths: Vec::new(),
                    recovered_from: None,
                    fixture_replay: None,
                    normalized_error: None,
                    wire_response_shapes: Vec::new(),
                    wire_exchanges: Vec::new(),
                    wire_exchange_outcomes: Vec::new(),
                };
            }
            CancellationAttempt::CompletedBeforeCancel {
                first_event_received,
                observed_latency_ms,
                response,
            } => {
                return ProbeRecord {
                    stable_key: probe_stable_key(
                        run_id,
                        &provider.id,
                        model,
                        protocol_name(protocol),
                        response_mode_name(response_mode),
                        ProbeKind::Cancellation.as_str(),
                    ),
                    provider_id: provider.redact_text(&provider.id),
                    model: provider.redact_text(model),
                    protocol: protocol_name(protocol).to_owned(),
                    response_mode: response_mode_name(response_mode).to_owned(),
                    capability: ProbeKind::Cancellation.as_str().to_owned(),
                    endpoint_path,
                    status: "unverified".to_owned(),
                    attempts,
                    latency_ms: started.elapsed().as_millis(),
                    expected_text: None,
                    synthetic_marker: Some(marker.clone()),
                    actual_text_evidence: None,
                    response: response
                        .as_ref()
                        .map(|response| ResponseEvidence::from_response(response, provider)),
                    assertions: cancellation_assertions(
                        response_mode,
                        false,
                        first_event_received,
                        true,
                    ),
                    cancellation: Some(cancellation_evidence(
                        false,
                        first_event_received,
                        true,
                        observed_latency_ms,
                    )),
                    skip_evidence: None,
                    fixture_paths: Vec::new(),
                    recovered_from: None,
                    fixture_replay: None,
                    normalized_error: None,
                    wire_response_shapes: Vec::new(),
                    wire_exchanges: Vec::new(),
                    wire_exchange_outcomes: Vec::new(),
                };
            }
            CancellationAttempt::Failed {
                error,
                observed_latency_ms,
            } => {
                let retryable = error.is_retryable();
                let delay = retry_delay(attempts, &error);
                final_latency = observed_latency_ms;
                final_error = Some(error);
                if !retryable || attempts >= options.max_attempts {
                    break;
                }
                sleep(delay).await;
            }
        }
    }
    let mut record = failed_remote_probe(
        provider,
        model,
        protocol,
        response_mode,
        ProbeKind::Cancellation,
        run_id,
        &marker,
        endpoint_path,
        None,
        attempts,
        started.elapsed().as_millis(),
        final_error.as_ref(),
    );
    record.cancellation = Some(cancellation_evidence(false, false, false, final_latency));
    record
}

/// 生成取消探测可直接证明的语义断言。
fn cancellation_assertions(
    response_mode: WireResponseMode,
    local_future_dropped: bool,
    first_event_received: bool,
    completed_before_cancel: bool,
) -> Vec<SemanticAssertion> {
    let mut assertions = vec![
        assertion(
            "local_cancel_timer_won",
            local_future_dropped,
            "取消计时器先于完整响应获胜",
            "完整响应或错误先于取消计时器结束",
        ),
        assertion(
            "in_flight_future_dropped",
            local_future_dropped && !completed_before_cancel,
            "在途 Future 或 Stream 已在本地丢弃",
            "未能在完整响应前丢弃在途调用",
        ),
        assertion(
            "remote_termination_not_claimed",
            true,
            "结果仅证明本地释放，不声称远端停止生成或计费",
            "结果错误声称远端终止",
        ),
    ];
    if response_mode == WireResponseMode::Streaming {
        assertions.insert(
            0,
            assertion(
                "stream_event_received_before_cancel",
                first_event_received,
                "流式调用在取消计时开始前已收到有效统一事件",
                "流式调用未收到有效统一事件，不能把等待超时视为取消成功",
            ),
        );
    }
    assertions
}

/// 构造明确限定为本地行为的取消证据。
fn cancellation_evidence(
    local_future_dropped: bool,
    first_event_received: bool,
    completed_before_cancel: bool,
    observed_latency_ms: u128,
) -> CancellationEvidence {
    CancellationEvidence {
        cancel_after_ms: CANCEL_AFTER_MS,
        local_future_dropped,
        first_event_received,
        completed_before_cancel,
        observed_latency_ms,
        remote_termination_proven: false,
    }
}

/// 把本地配置失败转换为与网络结果同结构的记录。
#[allow(clippy::too_many_arguments)]
fn failed_local_probe(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: ProbeKind,
    run_id: &str,
    expected_text: Option<String>,
    latency_ms: u128,
    message: String,
) -> ProbeRecord {
    ProbeRecord {
        stable_key: probe_stable_key(
            run_id,
            &provider.id,
            model,
            protocol_name(protocol),
            response_mode_name(response_mode),
            capability.as_str(),
        ),
        provider_id: provider.redact_text(&provider.id),
        model: provider.redact_text(model),
        protocol: protocol_name(protocol).to_owned(),
        response_mode: response_mode_name(response_mode).to_owned(),
        capability: capability.as_str().to_owned(),
        endpoint_path: String::new(),
        status: "failed".to_owned(),
        attempts: 0,
        latency_ms,
        expected_text,
        synthetic_marker: None,
        actual_text_evidence: None,
        response: None,
        assertions: Vec::new(),
        cancellation: None,
        skip_evidence: None,
        fixture_paths: Vec::new(),
        recovered_from: None,
        fixture_replay: None,
        normalized_error: Some(NormalizedError {
            kind: "configuration".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text(
                &provider.redact_credentials(&message),
            ),
            retryable: false,
            http_status: None,
        }),
        wire_response_shapes: Vec::new(),
        wire_exchanges: Vec::new(),
        wire_exchange_outcomes: Vec::new(),
    }
}

/// 把耗尽重试的统一错误转换为稳定探测记录。
#[allow(clippy::too_many_arguments)]
fn failed_remote_probe(
    provider: &ProviderEntry,
    model: &str,
    protocol: ProviderProtocol,
    response_mode: WireResponseMode,
    capability: ProbeKind,
    run_id: &str,
    synthetic_marker: &str,
    endpoint_path: String,
    expected_text: Option<String>,
    attempts: usize,
    latency_ms: u128,
    error: Option<&ModelError>,
) -> ProbeRecord {
    let normalized_error = error
        .map(|error| normalize_error(provider, error))
        .unwrap_or_else(|| NormalizedError {
            kind: "internal".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text("探测没有产生响应或错误"),
            retryable: false,
            http_status: None,
        });
    ProbeRecord {
        stable_key: probe_stable_key(
            run_id,
            &provider.id,
            model,
            protocol_name(protocol),
            response_mode_name(response_mode),
            capability.as_str(),
        ),
        provider_id: provider.redact_text(&provider.id),
        model: provider.redact_text(model),
        protocol: protocol_name(protocol).to_owned(),
        response_mode: response_mode_name(response_mode).to_owned(),
        capability: capability.as_str().to_owned(),
        endpoint_path,
        status: "failed".to_owned(),
        attempts,
        latency_ms,
        expected_text,
        synthetic_marker: Some(synthetic_marker.to_owned()),
        actual_text_evidence: None,
        response: None,
        assertions: Vec::new(),
        cancellation: None,
        skip_evidence: None,
        fixture_paths: Vec::new(),
        recovered_from: None,
        fixture_replay: None,
        normalized_error: Some(normalized_error),
        wire_response_shapes: Vec::new(),
        wire_exchanges: Vec::new(),
        wire_exchange_outcomes: Vec::new(),
    }
}

/// 把统一模型错误转换成稳定且经过 Provider 凭据清理的报告结构。
pub(crate) fn normalize_error(provider: &ProviderEntry, error: &ModelError) -> NormalizedError {
    let (kind, http_status) = match error {
        ModelError::Authentication { status_code, .. } => ("authentication", *status_code),
        ModelError::Authorization { status_code, .. } => ("authorization", *status_code),
        ModelError::QuotaExceeded { status_code, .. } => ("quota_exceeded", *status_code),
        ModelError::ModelNotFound { status_code, .. } => ("model_not_found", *status_code),
        ModelError::ProtocolUnsupported { status_code, .. } => {
            ("protocol_unsupported", *status_code)
        }
        ModelError::RateLimited { status_code, .. } => ("rate_limit", *status_code),
        ModelError::ContextLengthExceeded { .. } => ("context_length", None),
        ModelError::InvalidRequest { .. } => ("invalid_request", None),
        ModelError::UnsupportedCapability { .. } => ("unsupported_capability", None),
        ModelError::StructuredOutput {
            enforcement,
            failure,
            ..
        } => (structured_output_error_kind(*enforcement, *failure), None),
        ModelError::ProviderUnavailable { status_code, .. } => ("server_error", *status_code),
        ModelError::Transport { .. } => ("transport", None),
        ModelError::StreamInterrupted { .. } => ("stream_interrupted", None),
        ModelError::Protocol { .. } => ("decode", None),
        ModelError::Cancelled { .. } => ("cancelled", None),
    };
    let message = provider.redact_credentials(error.message());
    NormalizedError {
        kind: kind.to_owned(),
        message_evidence: ErrorMessageEvidence::from_text(&message),
        retryable: error.is_retryable(),
        http_status,
    }
}

/// 区分 Provider 原生契约失败与 Runtime 工具模拟失败。
const fn structured_output_error_kind(
    enforcement: StructuredOutputEnforcement,
    failure: StructuredOutputFailureKind,
) -> &'static str {
    match (enforcement, failure) {
        (StructuredOutputEnforcement::Native, StructuredOutputFailureKind::MissingOutput) => {
            "provider_contract_violation_missing_output"
        }
        (StructuredOutputEnforcement::Native, StructuredOutputFailureKind::InvalidJson) => {
            "provider_contract_violation_invalid_json"
        }
        (StructuredOutputEnforcement::Native, StructuredOutputFailureKind::SchemaViolation) => {
            "provider_contract_violation_schema"
        }
        (StructuredOutputEnforcement::Native, StructuredOutputFailureKind::UnexpectedContent) => {
            "provider_contract_violation_unexpected_content"
        }
        (StructuredOutputEnforcement::Native, StructuredOutputFailureKind::Incomplete) => {
            "provider_contract_violation_incomplete"
        }
        (StructuredOutputEnforcement::Native, StructuredOutputFailureKind::EmulationProtocol) => {
            "provider_contract_violation_emulation_protocol"
        }
        (StructuredOutputEnforcement::ToolEmulated, StructuredOutputFailureKind::MissingOutput) => {
            "runtime_emulation_missing_output"
        }
        (StructuredOutputEnforcement::ToolEmulated, StructuredOutputFailureKind::InvalidJson) => {
            "runtime_emulation_invalid_json"
        }
        (
            StructuredOutputEnforcement::ToolEmulated,
            StructuredOutputFailureKind::SchemaViolation,
        ) => "runtime_emulation_schema",
        (
            StructuredOutputEnforcement::ToolEmulated,
            StructuredOutputFailureKind::UnexpectedContent,
        ) => "runtime_emulation_unexpected_content",
        (StructuredOutputEnforcement::ToolEmulated, StructuredOutputFailureKind::Incomplete) => {
            "runtime_emulation_incomplete"
        }
        (
            StructuredOutputEnforcement::ToolEmulated,
            StructuredOutputFailureKind::EmulationProtocol,
        ) => "runtime_emulation_protocol",
    }
}

/// 返回规范要求的指数退避间隔。
fn retry_delay(completed_attempts: usize, error: &ModelError) -> Duration {
    if let ModelError::RateLimited {
        retry_after_ms: Some(milliseconds),
        ..
    } = error
    {
        return Duration::from_millis((*milliseconds).min(60_000));
    }
    if matches!(error, ModelError::RateLimited { .. }) {
        if let Some(seconds) = retry_seconds_from_message(error.message()) {
            return Duration::from_secs(seconds.min(60));
        }
    }
    match completed_attempts {
        0 | 1 => Duration::from_secs(1),
        _ => Duration::from_secs(2),
    }
}

/// 从常见“约 9s 后重试”或“9 秒后重试”文本提取等待秒数。
fn retry_seconds_from_message(message: &str) -> Option<u64> {
    let characters = message.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < characters.len() {
        if !characters[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < characters.len() && characters[index].is_ascii_digit() {
            index += 1;
        }
        let mut suffix_index = index;
        while characters
            .get(suffix_index)
            .is_some_and(|character| character.is_whitespace())
        {
            suffix_index += 1;
        }
        let suffix = characters.get(suffix_index).copied();
        if matches!(suffix, Some('s' | 'S' | '秒')) {
            return characters[start..index]
                .iter()
                .collect::<String>()
                .parse()
                .ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

    use keencode_model::{ReasoningContent, ResponseMetadata, TokenUsage, ToolCall};
    use keencode_provider::encode_wire_request;

    use super::*;
    use crate::report::{ReportStore, ResumeManifest, RunMetadata, retry_tuple_key};

    /// 创建不依赖网络的 Provider 测试配置。
    fn provider() -> ProviderEntry {
        serde_json::from_value(serde_json::json!({
            "id": "provider",
            "name": "测试",
            "baseUrl": "https://example.com/v1",
            "models": ["configured"],
            "apiBackend": "responses",
            "apiKey": "secret-value"
        }))
        .expect("测试 Provider 应可解析")
    }

    /// 创建指向本地请求计数服务的 Provider 测试配置。
    fn provider_with_base_url(base_url: &str) -> ProviderEntry {
        serde_json::from_value(serde_json::json!({
            "id": "provider",
            "name": "测试",
            "baseUrl": base_url,
            "models": ["configured"],
            "apiBackend": "responses",
            "apiKey": "secret-value"
        }))
        .expect("本地测试 Provider 应可解析")
    }

    /// 创建指向本地请求服务且选择指定协议的 Provider 测试配置。
    fn provider_with_protocol(base_url: &str, protocol: ProviderProtocol) -> ProviderEntry {
        let api_backend = match protocol {
            ProviderProtocol::Messages => "messages",
            ProviderProtocol::ChatCompletions => "chat_completions",
            ProviderProtocol::Responses => "responses",
        };
        serde_json::from_value(serde_json::json!({
            "id": "provider",
            "name": "测试",
            "baseUrl": base_url,
            "models": ["configured"],
            "apiBackend": api_backend,
            "apiKey": "secret-value"
        }))
        .expect("本地指定协议 Provider 应可解析")
    }

    /// 创建只用于本地 Provider 编排测试的运行参数。
    fn test_runtime_options(
        capabilities: BTreeSet<ProbeKind>,
        full_matrix: bool,
        max_attempts: usize,
    ) -> RuntimeOptions {
        RuntimeOptions {
            user_data_directory: PathBuf::new(),
            config_path: PathBuf::new(),
            verify_run_dir: None,
            output_root: PathBuf::new(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts,
            request_timeout_secs: 5,
            catalog_only: false,
            diagnostics_only: false,
            capabilities,
            full_matrix,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        }
    }

    /// 验证本地回环 transport 只形成 Harness 基础设施失败，不会伪装成 Adapter 契约违规。
    #[test]
    fn stream_interruption_transport_归为harness基础设施失败() {
        let result: Result<ModelResponse, ModelError> = Err(ModelError::Transport {
            message: "synthetic loopback failure".to_owned(),
            retryable: false,
        });
        let message = stream_interruption_infrastructure_message(&result, Ok(()))
            .expect("本地 transport 必须被识别为 Harness 基础设施失败");
        let record = failed_stream_interruption_infrastructure_probe(
            &provider(),
            "configured",
            ProviderProtocol::Responses,
            WireResponseMode::Buffered,
            "loopback-infrastructure-test",
            "KC_LOCAL_0123456789abcdef",
            1,
            &message,
        );

        assert_eq!(record.status, "failed");
        assert_ne!(record.status, "contract_violation");
        assert_eq!(record.attempts, 1);
        assert!(
            record.normalized_error.as_ref().is_some_and(|error| {
                error.kind == "harness_infrastructure" && !error.retryable
            })
        );
        assert!(record.assertions.iter().any(|assertion| {
            assertion.name == "loopback_harness_completed" && !assertion.passed
        }));
    }

    /// 验证一次性服务线程的请求读取错误能够由调用方确定性取得。
    #[test]
    fn truncated_sse_server_服务线程读取错误可观测() {
        let server = TruncatedSseServer::start(ProviderProtocol::Messages)
            .expect("应能启动本地截断探测服务");
        let address = server
            .base_url
            .strip_prefix("http://")
            .and_then(|value| value.strip_suffix("/v1"))
            .expect("本地截断探测地址必须采用固定格式")
            .to_owned();
        let mut client = TcpStream::connect(address).expect("应能连接本地截断探测服务");
        client
            .set_read_timeout(Some(StdDuration::from_secs(2)))
            .expect("应能设置测试客户端读取超时");
        client
            .write_all(b"POST /v1/messages HTTP/1.1\r\n")
            .expect("应能发送故意不完整的请求");
        client
            .shutdown(Shutdown::Write)
            .expect("应能关闭测试客户端写入方向");
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response);

        let error = server
            .finish()
            .expect_err("不完整请求必须形成可观测的服务线程错误");
        assert!(error.starts_with("读取本地截断探测请求失败："));
    }

    /// 高频覆盖三协议与两种响应模式，防止 Windows 非阻塞继承重新引入随机 transport。
    #[tokio::test]
    async fn stream_interruption_高频回环不产生基础设施transport假阳性() {
        /// 每个协议与响应模式组合的重复次数，用于稳定覆盖调度竞争窗口。
        const ITERATIONS_PER_CASE: usize = 64;
        let provider = provider();
        let options =
            test_runtime_options(BTreeSet::from([ProbeKind::StreamInterruption]), false, 1);

        for iteration in 0..ITERATIONS_PER_CASE {
            for protocol in all_protocols() {
                for response_mode in [WireResponseMode::Buffered, WireResponseMode::Streaming] {
                    let record = run_probe(
                        &provider,
                        "configured",
                        protocol,
                        response_mode,
                        ProbeKind::StreamInterruption,
                        &options,
                        &format!("loopback-stress-{iteration}"),
                    )
                    .await;
                    let case = format!(
                        "iteration={iteration}, protocol={}, mode={}",
                        protocol_name(protocol),
                        response_mode_name(response_mode)
                    );

                    assert_eq!(record.status, "passed", "{case}");
                    assert!(
                        record.normalized_error.as_ref().is_some_and(|error| {
                            error.kind == "stream_interrupted" && error.retryable
                        }),
                        "{case}"
                    );
                    assert!(
                        record.assertions.iter().any(|assertion| {
                            assertion.name == "truncated_stream_classified" && assertion.passed
                        }),
                        "{case}"
                    );
                    assert!(
                        record.assertions.iter().any(|assertion| {
                            assertion.name == "loopback_harness_completed" && assertion.passed
                        }),
                        "{case}"
                    );
                    assert_eq!(record.wire_exchanges.len(), 1, "{case}");
                    assert_eq!(
                        record.wire_exchanges[0].response_status,
                        Some(200),
                        "{case}"
                    );
                }
            }
        }
    }

    /// 验证上下文超限 Fixture 同时接受 HTTP 与 2xx Adapter 错误，并拒绝其他错误。
    #[tokio::test]
    async fn context_overflow_fixture_严格接受两种线级错误表达() {
        const EXPECTED_MESSAGE: &str = "synthetic context limit";
        const MARKER: &str = "KC_CONTEXT_0123456789abcdef";
        const MAX_EVENT_BYTES: usize = 64 * 1024;
        let provider = provider();
        let record = expected_error_probe_record(
            &provider,
            "configured",
            ProviderProtocol::Responses,
            WireResponseMode::Buffered,
            ProbeKind::ContextOverflow.as_str(),
            "context-overflow-fixture-test",
            MARKER,
            "/v1/responses".to_owned(),
            1,
            Err(ModelError::ContextLengthExceeded {
                message: EXPECTED_MESSAGE.to_owned(),
            }),
            |error| matches!(error, ModelError::ContextLengthExceeded { .. }),
            "context_overflow_classified",
            "超大纯合成输入被归一化为 context_length",
            "超大纯合成输入未被稳定归一化为 context_length",
        );
        let exchange = |protocol: ProviderProtocol,
                        streaming: bool,
                        status: u16,
                        content_type: &str,
                        body: &str| {
            let request = text_request("configured", MARKER);
            WireExchange {
                model_request: request.clone(),
                max_event_bytes: MAX_EVENT_BYTES,
                request_body: encode_wire_request(protocol, &request, streaming)
                    .expect("合成上下文超限请求必须可编码"),
                response_status: Some(status),
                response_content_type: Some(content_type.to_owned()),
                response_body: body.as_bytes().to_vec(),
                response_body_truncated: false,
                response_body_eof_observed: true,
                terminal_error: None,
            }
        };
        let accepted = [
            (
                ProviderProtocol::Responses,
                false,
                400,
                "application/json",
                r#"{"error":{"code":"context_length_exceeded","message":"synthetic context limit"}}"#,
            ),
            (
                ProviderProtocol::Messages,
                true,
                200,
                "text/event-stream",
                "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"context_length_exceeded\",\"message\":\"synthetic context limit\"}}\n\n",
            ),
            (
                ProviderProtocol::Responses,
                false,
                200,
                "application/json",
                r#"{"id":"resp-context","status":"failed","error":{"code":"context_length_exceeded","message":"synthetic context limit"}}"#,
            ),
            (
                ProviderProtocol::Responses,
                true,
                200,
                "text/event-stream",
                "event: error\ndata: {\"type\":\"error\",\"code\":\"context_length_exceeded\",\"message\":\"synthetic context limit\"}\n\n",
            ),
        ];

        for (protocol, streaming, status, content_type, body) in accepted {
            let exchange = exchange(protocol, streaming, status, content_type, body);
            let outcome = replay_captured_exchange(
                &exchange,
                protocol,
                MAX_EVENT_BYTES,
                &provider,
                &record.stable_key,
            )
            .await;

            assert!(matches!(
                &outcome,
                FixtureExchangeOutcome::Error { error } if error.kind == "context_length"
            ));
            assert!(expected_http_or_adapter_error_matches(
                &exchange, &outcome, &record
            ));
        }

        let rejected = [
            (
                400,
                "application/json",
                r#"{"error":{"code":"invalid_request_error","message":"synthetic ordinary rejection"}}"#,
            ),
            (
                200,
                "text/event-stream",
                "event: response.synthetic_unknown\ndata: {\"type\":\"response.synthetic_unknown\"}\n\n",
            ),
            (
                200,
                "text/event-stream",
                "event: error\ndata: {\"type\":\"error\",\"code\":\"invalid_request_error\",\"message\":\"synthetic ordinary rejection\"}\n\n",
            ),
        ];
        for (status, content_type, body) in rejected {
            let exchange = exchange(
                ProviderProtocol::Responses,
                true,
                status,
                content_type,
                body,
            );
            let outcome = replay_captured_exchange(
                &exchange,
                ProviderProtocol::Responses,
                MAX_EVENT_BYTES,
                &provider,
                &record.stable_key,
            )
            .await;

            assert!(!expected_http_or_adapter_error_matches(
                &exchange, &outcome, &record
            ));
        }
    }

    /// 构造已经通过恢复门禁的单个文本记录。
    fn restored_text_record(
        provider: &ProviderEntry,
        protocol: ProviderProtocol,
        response_mode: WireResponseMode,
        run_id: &str,
    ) -> ProbeRecord {
        let marker = "KC_OK_0123456789abcdef";
        let stable_key = probe_stable_key(
            run_id,
            &provider.id,
            "configured",
            protocol_name(protocol),
            response_mode_name(response_mode),
            ProbeKind::Text.as_str(),
        );
        ProbeRecord {
            actual_text_evidence: Some(ActualTextEvidence::from_text(
                provider,
                &stable_key,
                marker,
            )),
            stable_key,
            provider_id: provider.redact_text(&provider.id),
            model: "configured".to_owned(),
            protocol: protocol_name(protocol).to_owned(),
            response_mode: response_mode_name(response_mode).to_owned(),
            capability: ProbeKind::Text.as_str().to_owned(),
            endpoint_path: "/synthetic".to_owned(),
            status: "passed".to_owned(),
            attempts: 1,
            latency_ms: 1,
            expected_text: Some(marker.to_owned()),
            synthetic_marker: Some(marker.to_owned()),
            response: None,
            assertions: Vec::new(),
            cancellation: None,
            skip_evidence: None,
            fixture_paths: vec!["fixtures/synthetic.json".to_owned()],
            recovered_from: None,
            fixture_replay: Some(FixtureReplayEvidence {
                status: "passed".to_owned(),
                exchange_count: 1,
                replayed_exchanges: 1,
                reason: None,
            }),
            normalized_error: None,
            wire_response_shapes: Vec::new(),
            wire_exchanges: Vec::new(),
            wire_exchange_outcomes: Vec::new(),
        }
    }

    /// 构造一个已通过恢复目录完整性门禁的终态记录。
    #[allow(clippy::too_many_arguments)]
    fn restored_terminal_record(
        provider: &ProviderEntry,
        run_id: &str,
        model: &str,
        protocol: ProviderProtocol,
        response_mode: WireResponseMode,
        capability: &str,
        status: &str,
        marker: &str,
    ) -> ProbeRecord {
        let stable_key = probe_stable_key(
            run_id,
            &provider.id,
            model,
            protocol_name(protocol),
            response_mode_name(response_mode),
            capability,
        );
        let skipped = status == "skipped";
        let cancellation = capability == ProbeKind::Cancellation.as_str();
        ProbeRecord {
            actual_text_evidence: (status == "passed" && !cancellation)
                .then(|| ActualTextEvidence::from_text(provider, &stable_key, marker)),
            stable_key,
            provider_id: provider.redact_text(&provider.id),
            model: provider.redact_text(model),
            protocol: protocol_name(protocol).to_owned(),
            response_mode: response_mode_name(response_mode).to_owned(),
            capability: capability.to_owned(),
            endpoint_path: "/synthetic".to_owned(),
            status: status.to_owned(),
            attempts: usize::from(!skipped),
            latency_ms: 1,
            expected_text: (!skipped && !cancellation && !capability.starts_with("diagnostic_"))
                .then(|| marker.to_owned()),
            synthetic_marker: (!skipped).then(|| marker.to_owned()),
            response: None,
            assertions: Vec::new(),
            cancellation: cancellation.then(|| {
                cancellation_evidence(
                    status == "passed",
                    response_mode == WireResponseMode::Streaming,
                    status == "unverified",
                    1,
                )
            }),
            skip_evidence: skipped.then(|| SkipEvidence {
                verification: "unverified".to_owned(),
                reason: "base_text_permanent_failure".to_owned(),
                blocked_by: probe_stable_key(
                    run_id,
                    &provider.id,
                    model,
                    protocol_name(protocol),
                    response_mode_name(response_mode),
                    ProbeKind::Text.as_str(),
                ),
                gate_status: "failed".to_owned(),
                error_kind: Some("model_not_found".to_owned()),
                retryable: Some(false),
                http_status: Some(404),
            }),
            fixture_paths: Vec::new(),
            recovered_from: None,
            fixture_replay: None,
            normalized_error: (status == "failed").then(|| NormalizedError {
                kind: "model_not_found".to_owned(),
                message_evidence: ErrorMessageEvidence::from_text("合成失败"),
                retryable: false,
                http_status: Some(404),
            }),
            wire_response_shapes: Vec::new(),
            wire_exchanges: Vec::new(),
            wire_exchange_outcomes: Vec::new(),
        }
    }

    /// 一个仅记录 HTTP 请求数并返回固定目录与失败响应的本地服务。
    struct CountingServer {
        /// 可写入 Provider 配置的本地基础地址。
        base_url: String,
        /// 已接受的 HTTP 请求数。
        requests: Arc<AtomicUsize>,
        /// 已接受的模型目录校验请求数。
        catalog_requests: Arc<AtomicUsize>,
        /// 已接受的生成、诊断或取消模型请求数。
        model_requests: Arc<AtomicUsize>,
        /// 服务线程的停止标记。
        stop: Arc<AtomicBool>,
        /// 可在销毁时确定性回收的服务线程。
        thread: Option<JoinHandle<()>>,
    }

    /// 基础文本请求的固定测试响应类别。
    #[derive(Clone)]
    enum GateResponse {
        /// 返回不可重试的缺失资源错误。
        NotFound,
        /// 返回可立即重试的限流错误。
        RateLimited,
        /// 返回包含指定合成标记的 Responses JSON 成功响应。
        ResponsesSuccess(String),
    }

    /// 模型目录端点的固定测试响应类别。
    #[derive(Clone)]
    enum CatalogResponse {
        /// 返回成功的空模型目录。
        Empty,
        /// 返回包含指定稳定模型 ID 的成功目录。
        Models(Vec<String>),
        /// 返回一个缺少稳定模型 ID 的目录条目。
        InvalidEntry,
        /// 返回可立即重试的服务端错误。
        ServerError,
        /// 三次尝试分别返回一个首页模型，并在第二页返回服务端错误。
        PartialAttemptSequence,
    }

    /// 读取一个带固定 Content-Length 的完整 HTTP 请求，避免响应时客户端仍在上传请求体。
    fn read_http_request(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        const MAX_REQUEST_SIZE: usize = 1024 * 1024;
        let mut request = Vec::new();
        let mut expected_size = None;
        let mut buffer = [0_u8; 4096];
        loop {
            let size = stream.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..size]);
            if request.len() > MAX_REQUEST_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "测试 HTTP 请求超过本地服务上限",
                ));
            }
            if expected_size.is_none() {
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let body_start = header_end + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    expected_size = Some(body_start + content_length);
                }
            }
            if expected_size.is_some_and(|size| request.len() >= size) {
                break;
            }
        }
        Ok(request)
    }

    impl CountingServer {
        /// 启动使用成功空目录且只监听回环地址的请求计数服务。
        fn start(gate_response: GateResponse) -> Self {
            Self::start_with_catalog(gate_response, CatalogResponse::Empty)
        }

        /// 启动可独立控制目录与模型响应的回环请求计数服务。
        fn start_with_catalog(
            gate_response: GateResponse,
            catalog_response: CatalogResponse,
        ) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("应能绑定本地端口");
            listener.set_nonblocking(true).expect("应能设置非阻塞监听");
            let address = listener.local_addr().expect("应能读取本地端口");
            let requests = Arc::new(AtomicUsize::new(0));
            let catalog_requests = Arc::new(AtomicUsize::new(0));
            let model_requests = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let thread_requests = Arc::clone(&requests);
            let thread_catalog_requests = Arc::clone(&catalog_requests);
            let thread_model_requests = Arc::clone(&model_requests);
            let thread_stop = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_nonblocking(false)
                                .expect("已接受连接应切换为阻塞读取");
                            stream
                                .set_read_timeout(Some(StdDuration::from_secs(2)))
                                .expect("应能设置读取超时");
                            let request = match read_http_request(&mut stream) {
                                Ok(request) => request,
                                Err(_) => continue,
                            };
                            let first_line = String::from_utf8_lossy(&request)
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .to_owned();
                            if first_line.is_empty() {
                                continue;
                            }
                            thread_requests.fetch_add(1, Ordering::SeqCst);
                            let catalog_request = first_line.starts_with("GET /v1/models ")
                                || first_line.starts_with("GET /v1/models?");
                            let catalog_request_index = if catalog_request {
                                thread_catalog_requests.fetch_add(1, Ordering::SeqCst) + 1
                            } else {
                                thread_model_requests.fetch_add(1, Ordering::SeqCst);
                                0
                            };
                            let (status, body) = if catalog_request {
                                match &catalog_response {
                                    CatalogResponse::Empty => {
                                        ("200 OK".to_owned(), r#"{"data":[]}"#.to_owned())
                                    }
                                    CatalogResponse::Models(models) => (
                                        "200 OK".to_owned(),
                                        serde_json::json!({
                                            "data": models
                                                .iter()
                                                .map(|model| serde_json::json!({
                                                    "id": model,
                                                    "type": "model"
                                                }))
                                                .collect::<Vec<_>>()
                                        })
                                        .to_string(),
                                    ),
                                    CatalogResponse::InvalidEntry => (
                                        "200 OK".to_owned(),
                                        r#"{"data":[{"type":"model"}]}"#.to_owned(),
                                    ),
                                    CatalogResponse::ServerError => (
                                        "503 Service Unavailable".to_owned(),
                                        r#"{"error":{"message":"catalog unavailable","type":"server_error"}}"#
                                            .to_owned(),
                                    ),
                                    CatalogResponse::PartialAttemptSequence => {
                                        match catalog_request_index {
                                            1 | 3 | 5 => {
                                                let model = match catalog_request_index {
                                                    1 => "model-a",
                                                    3 => "model-b",
                                                    5 => "model-c",
                                                    _ => unreachable!("已由外层模式限定目录请求序号"),
                                                };
                                                (
                                                    "200 OK".to_owned(),
                                                    serde_json::json!({
                                                        "data": [{"id": model, "type": "model"}],
                                                        "next": "/v1/models?page=2"
                                                    })
                                                    .to_string(),
                                                )
                                            }
                                            _ => (
                                                "503 Service Unavailable".to_owned(),
                                                r#"{"error":{"message":"catalog page unavailable","type":"server_error"}}"#
                                                    .to_owned(),
                                            ),
                                        }
                                    }
                                }
                            } else {
                                match &gate_response {
                                    GateResponse::NotFound => (
                                        "404 Not Found".to_owned(),
                                        r#"{"error":{"message":"missing","type":"not_found"}}"#
                                            .to_owned(),
                                    ),
                                    GateResponse::RateLimited => (
                                        "429 Too Many Requests".to_owned(),
                                        r#"{"error":{"message":"limited","type":"rate_limit"}}"#
                                            .to_owned(),
                                    ),
                                    GateResponse::ResponsesSuccess(marker) => (
                                        "200 OK".to_owned(),
                                        serde_json::json!({
                                            "id": "resp-fixture",
                                            "object": "response",
                                            "model": "configured",
                                            "status": "completed",
                                            "output": [{
                                                "id": "message-fixture",
                                                "type": "message",
                                                "role": "assistant",
                                                "content": [{"type": "output_text", "text": marker}]
                                            }],
                                            "usage": {
                                                "input_tokens": 8,
                                                "output_tokens": 4,
                                                "total_tokens": 12
                                            }
                                        })
                                        .to_string(),
                                    ),
                                }
                            };
                            let retry_after =
                                if (matches!(&gate_response, GateResponse::RateLimited)
                                    && status.starts_with("429"))
                                    || status.starts_with("503")
                                {
                                    "Retry-After: 0\r\n"
                                } else {
                                    ""
                                };
                            let response = format!(
                                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{retry_after}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(StdDuration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                base_url: format!("http://{address}/v1"),
                requests,
                catalog_requests,
                model_requests,
                stop,
                thread: Some(thread),
            }
        }

        /// 返回截至当前已被服务接受的请求数。
        fn request_count(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }

        /// 返回模型目录身份校验请求数。
        fn catalog_request_count(&self) -> usize {
            self.catalog_requests.load(Ordering::SeqCst)
        }

        /// 返回生成、诊断与取消共享端点的请求数。
        fn model_request_count(&self) -> usize {
            self.model_requests.load(Ordering::SeqCst)
        }
    }

    impl Drop for CountingServer {
        /// 停止并回收本地服务线程。
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// 保存图片工具结果回环服务收到的一个线级请求。
    #[derive(Clone)]
    struct ImageRoundTripRequest {
        /// 请求目标路径，用于确认三种协议使用了各自的端点。
        path: String,
        /// 请求 JSON 正文，用于断言第二轮图片字段和调用标识。
        body: Value,
    }

    /// 只监听本地回环地址、按请求轮次返回工具调用和最终文本的图片闭环服务。
    struct ImageRoundTripServer {
        /// 可写入 Provider 配置的本地基础地址。
        base_url: String,
        /// 服务收到的请求，按到达顺序保存且不包含认证 Header。
        requests: Arc<Mutex<Vec<ImageRoundTripRequest>>>,
        /// 服务线程停止标记。
        stop: Arc<AtomicBool>,
        /// 可在测试结束时确定性回收的服务线程。
        thread: Option<JoinHandle<()>>,
    }

    impl ImageRoundTripServer {
        /// 启动指定协议的本地图片工具结果双轮回环服务。
        fn start(protocol: ProviderProtocol, first_marker: &str, final_marker: &str) -> Self {
            let listener =
                TcpListener::bind(("127.0.0.1", 0)).expect("应能绑定本地图片工具结果回环端口");
            listener
                .set_nonblocking(true)
                .expect("应能设置图片工具结果回环监听器为非阻塞");
            let address = listener
                .local_addr()
                .expect("应能读取本地图片工具结果回环端口");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread_requests = Arc::clone(&requests);
            let thread_stop = Arc::clone(&stop);
            let first_marker = first_marker.to_owned();
            let final_marker = final_marker.to_owned();
            let thread = thread::spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_nonblocking(false)
                                .expect("已接受图片工具结果连接应切换为阻塞读取");
                            stream
                                .set_read_timeout(Some(StdDuration::from_secs(2)))
                                .expect("应能设置图片工具结果请求读取超时");
                            let request = match read_http_request(&mut stream) {
                                Ok(request) => request,
                                Err(_) => continue,
                            };
                            let request_text = String::from_utf8_lossy(&request);
                            let first_line = request_text.lines().next().unwrap_or_default();
                            let path = first_line
                                .split_whitespace()
                                .nth(1)
                                .unwrap_or_default()
                                .to_owned();
                            let Some(header_end) =
                                request.windows(4).position(|part| part == b"\r\n\r\n")
                            else {
                                continue;
                            };
                            let body = serde_json::from_slice::<Value>(&request[header_end + 4..])
                                .unwrap_or(Value::Null);
                            let request_index = {
                                let mut requests =
                                    thread_requests.lock().expect("图片工具结果请求锁不应中毒");
                                let request_index = requests.len();
                                requests.push(ImageRoundTripRequest { path, body });
                                request_index
                            };
                            let response_body = image_round_trip_response(
                                protocol,
                                request_index == 0,
                                if request_index == 0 {
                                    &first_marker
                                } else {
                                    &final_marker
                                },
                            )
                            .to_string();
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                                response_body.len()
                            );
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(StdDuration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                base_url: format!("http://{address}/v1"),
                requests,
                stop,
                thread: Some(thread),
            }
        }

        /// 返回服务按到达顺序保存的请求快照。
        fn requests(&self) -> Vec<ImageRoundTripRequest> {
            self.requests
                .lock()
                .expect("图片工具结果请求锁不应中毒")
                .clone()
        }
    }

    impl Drop for ImageRoundTripServer {
        /// 停止并回收本地图片工具结果服务线程。
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// 为图片工具结果回环返回指定协议的首轮工具调用或第二轮最终文本。
    fn image_round_trip_response(protocol: ProviderProtocol, first: bool, marker: &str) -> Value {
        let arguments = json!({
            "marker": marker,
            "count": EXPECTED_COUNT,
        })
        .to_string();
        match (protocol, first) {
            (ProviderProtocol::Messages, true) => json!({
                "id": "msg-image-first",
                "type": "message",
                "role": "assistant",
                "model": "configured",
                "content": [{
                    "type": "tool_use",
                    "id": "call-image-1",
                    "name": TOOL_NAME,
                    "input": {"marker": marker, "count": EXPECTED_COUNT}
                }],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 8, "output_tokens": 4}
            }),
            (ProviderProtocol::Messages, false) => json!({
                "id": "msg-image-final",
                "type": "message",
                "role": "assistant",
                "model": "configured",
                "content": [{"type": "text", "text": marker}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 12, "output_tokens": 2}
            }),
            (ProviderProtocol::Responses, true) => json!({
                "id": "resp-image-first",
                "object": "response",
                "model": "configured",
                "status": "completed",
                "output": [{
                    "id": "fc-image-1",
                    "type": "function_call",
                    "call_id": "call-image-1",
                    "name": TOOL_NAME,
                    "arguments": arguments
                }],
                "usage": {"input_tokens": 8, "output_tokens": 4, "total_tokens": 12}
            }),
            (ProviderProtocol::Responses, false) => json!({
                "id": "resp-image-final",
                "object": "response",
                "model": "configured",
                "status": "completed",
                "output": [{
                    "id": "message-image-final",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": marker}]
                }],
                "usage": {"input_tokens": 12, "output_tokens": 2, "total_tokens": 14}
            }),
            (ProviderProtocol::ChatCompletions, true) => json!({
                "id": "chat-image-first",
                "object": "chat.completion",
                "model": "configured",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-image-1",
                            "type": "function",
                            "function": {
                                "name": TOOL_NAME,
                                "arguments": arguments
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12}
            }),
            (ProviderProtocol::ChatCompletions, false) => json!({
                "id": "chat-image-final",
                "object": "chat.completion",
                "model": "configured",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": marker},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 12, "completion_tokens": 2, "total_tokens": 14}
            }),
        }
    }

    /// 记录并发模型请求、在指定数量的 lane 到达后同时放行响应的本地服务状态。
    struct ConcurrentProbeState {
        /// 当前仍在等待或写回响应的模型请求数。
        active: AtomicUsize,
        /// 观察到的最大并发模型请求数。
        max_active: AtomicUsize,
        /// 已接受的模型请求数，不包含目录请求。
        model_requests: AtomicUsize,
        /// 用于触发并发响应放行的模型请求序号。
        started: AtomicUsize,
        /// 达到放行阈值后不再阻塞新的模型请求。
        released: AtomicBool,
        /// 控制尚未达到放行阈值的模型请求等待。
        gate: Mutex<()>,
        /// 唤醒等待响应的模型请求。
        gate_waker: Condvar,
        /// 按发生顺序保存请求和回调事件，供测试验证 lane 内顺序。
        events: Arc<Mutex<Vec<String>>>,
        /// 第几个模型请求到达后放行全部响应；零表示不等待。
        release_after: usize,
    }

    /// 一个支持多连接并发处理的本地 Provider 回环服务。
    struct ConcurrentProbeServer {
        /// 可写入 Provider 配置的本地基础地址。
        base_url: String,
        /// 用于唤醒阻塞 accept 的本地地址。
        wake_address: String,
        /// 服务线程与测试共享的计数和事件状态。
        state: Arc<ConcurrentProbeState>,
        /// 服务线程的停止标记。
        stop: Arc<AtomicBool>,
        /// 可在销毁时确定性回收的服务线程。
        thread: Option<JoinHandle<()>>,
    }

    impl ConcurrentProbeServer {
        /// 启动只监听回环地址的多连接测试服务。
        fn start(release_after: usize) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("应能绑定本地并发服务端口");
            let address = listener
                .local_addr()
                .expect("应能读取本地并发服务端口")
                .to_string();
            let state = Arc::new(ConcurrentProbeState {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                model_requests: AtomicUsize::new(0),
                started: AtomicUsize::new(0),
                released: AtomicBool::new(release_after == 0),
                gate: Mutex::new(()),
                gate_waker: Condvar::new(),
                events: Arc::new(Mutex::new(Vec::new())),
                release_after,
            });
            let stop = Arc::new(AtomicBool::new(false));
            let thread_state = Arc::clone(&state);
            let thread_stop = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                let mut workers = Vec::new();
                while !thread_stop.load(Ordering::SeqCst) {
                    let (stream, _) = match listener.accept() {
                        Ok(accepted) => accepted,
                        Err(_) => break,
                    };
                    if thread_stop.load(Ordering::SeqCst) {
                        drop(stream);
                        break;
                    }
                    let worker_state = Arc::clone(&thread_state);
                    workers.push(thread::spawn(move || {
                        handle_concurrent_probe_request(stream, worker_state);
                    }));
                }
                thread_state.released.store(true, Ordering::SeqCst);
                thread_state.gate_waker.notify_all();
                for worker in workers {
                    let _ = worker.join();
                }
            });
            Self {
                base_url: format!("http://{address}/v1"),
                wake_address: address,
                state,
                stop,
                thread: Some(thread),
            }
        }

        /// 返回观察到的最大模型请求并发数。
        fn max_active(&self) -> usize {
            self.state.max_active.load(Ordering::SeqCst)
        }

        /// 返回已接受的模型请求数。
        fn model_request_count(&self) -> usize {
            self.state.model_requests.load(Ordering::SeqCst)
        }

        /// 返回与服务共享的有序事件日志。
        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.state.events)
        }
    }

    impl Drop for ConcurrentProbeServer {
        /// 放行等待中的请求、唤醒 accept 并回收服务线程。
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.state.released.store(true, Ordering::SeqCst);
            self.state.gate_waker.notify_all();
            let _ = TcpStream::connect(&self.wake_address);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// 更新并发计数的最大值，使用无锁原子 CAS 避免测试服务串行化请求。
    fn record_max_active(state: &ConcurrentProbeState, active: usize) {
        let mut observed = state.max_active.load(Ordering::SeqCst);
        while observed < active {
            match state.max_active.compare_exchange(
                observed,
                active,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(next) => observed = next,
            }
        }
    }

    /// 从模型请求中提取可用于测试事件匹配的模型、路径、流模式和能力类别。
    fn concurrent_request_event(request: &[u8]) -> String {
        let text = String::from_utf8_lossy(request);
        let first_line = text.lines().next().unwrap_or_default();
        let path = first_line.split_whitespace().nth(1).unwrap_or("unknown");
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.as_bytes())
            .unwrap_or_default();
        let payload = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let streaming = payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let capability = if payload.get("tools").is_some() {
            "tool"
        } else {
            "text"
        };
        format!("request:{model}:{path}:{streaming}:{capability}")
    }

    /// 处理一个模型连接并返回固定 404，供并发调度和取消测试使用。
    fn handle_concurrent_probe_request(mut stream: TcpStream, state: Arc<ConcurrentProbeState>) {
        stream
            .set_read_timeout(Some(StdDuration::from_secs(2)))
            .expect("并发服务连接应能设置读取超时");
        let request = match read_http_request(&mut stream) {
            Ok(request) => request,
            Err(_) => return,
        };
        let first_line = String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        if first_line.is_empty() {
            return;
        }
        if first_line.starts_with("GET /v1/models ") || first_line.starts_with("GET /v1/models?") {
            let body = r#"{"data":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            return;
        }

        state.model_requests.fetch_add(1, Ordering::SeqCst);
        let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
        record_max_active(&state, active);
        let request_number = state.started.fetch_add(1, Ordering::SeqCst) + 1;
        state
            .events
            .lock()
            .expect("并发服务事件锁不应中毒")
            .push(concurrent_request_event(&request));
        if state.release_after > 0 && request_number >= state.release_after {
            state.released.store(true, Ordering::SeqCst);
            state.gate_waker.notify_all();
        }
        if state.release_after > 0 {
            let mut guard = state.gate.lock().expect("并发服务门锁不应中毒");
            while !state.released.load(Ordering::SeqCst) {
                guard = state.gate_waker.wait(guard).expect("并发服务等待不应中毒");
            }
        }
        let body = r#"{"error":{"message":"missing","type":"not_found"}}"#;
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        state.active.fetch_sub(1, Ordering::SeqCst);
    }

    /// 创建包含指定内容与结束原因的离线响应。
    fn response(content: Vec<ContentBlock>, stop_reason: StopReason) -> ModelResponse {
        ModelResponse::new(
            ResponseMetadata::default(),
            content,
            TokenUsage::unknown(),
            stop_reason,
        )
    }

    /// 验证 Provider 报告的模型名称经过统一脱敏后不再携带终端或方向控制字符。
    #[test]
    fn response_evidence_清理reported_model危险字符() {
        let mut response = response(Vec::new(), StopReason::Completed);
        response.metadata.model =
            Some("reported\r\n\u{001b}]0;owned\u{0007}\u{009b}31m\u{202e}\u{200b}".to_owned());

        let evidence = ResponseEvidence::from_response(&response, &provider());
        let reported = evidence
            .reported_model_redacted
            .expect("Provider 报告的模型名称应保留安全证据");
        assert!(!contains_unsafe_inline_character(&reported));
        assert!(reported.contains("\\u{001b}"));
        assert!(reported.contains("\\u{202e}"));
    }

    /// 验证真实 HTTP 响应只在当前进程重放，磁盘 Fixture 仅保留结构证据。
    #[tokio::test]
    async fn run_probe_捕获写盘并重新读取线级fixture() {
        let run_id = "fixture-e2e";
        let model = "configured";
        let marker = expected_marker(
            &provider(),
            model,
            ProviderProtocol::Responses,
            WireResponseMode::Buffered,
            ProbeKind::Text,
            run_id,
        );
        let server = CountingServer::start(GateResponse::ResponsesSuccess(marker.clone()));
        let provider = provider_with_base_url(&server.base_url);
        let options = RuntimeOptions {
            user_data_directory: PathBuf::new(),
            config_path: PathBuf::new(),
            verify_run_dir: None,
            output_root: PathBuf::new(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 1,
            request_timeout_secs: 5,
            catalog_only: false,
            diagnostics_only: false,
            capabilities: BTreeSet::from([ProbeKind::Text]),
            full_matrix: false,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        };

        let mut record = run_probe(
            &provider,
            model,
            ProviderProtocol::Responses,
            WireResponseMode::Buffered,
            ProbeKind::Text,
            &options,
            run_id,
        )
        .await;
        assert_eq!(record.status, "passed");
        assert_eq!(record.wire_exchanges.len(), 1);
        assert_eq!(record.wire_response_shapes.len(), 1);
        record.wire_response_shapes[0]
            .validate()
            .expect("线上响应结构证据必须满足固定布局");
        assert!(record.fixture_replay.as_ref().is_some_and(|replay| {
            replay.status == "passed"
                && replay.exchange_count == 1
                && replay.replayed_exchanges == 1
        }));
        assert_eq!(server.request_count(), 1);

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应有效")
            .as_nanos();
        let output_root = std::env::temp_dir().join(format!(
            "keencode-provider-fixture-e2e-{}-{unique}",
            std::process::id()
        ));
        let store = ReportStore::create(&output_root, "run").expect("应能创建 Fixture 测试目录");
        let manifest = ResumeManifest::new(
            RunMetadata::new(run_id.to_owned(), &options).expect("应能创建运行元数据"),
            &options,
            &[&provider],
        )
        .expect("应能创建恢复清单");
        store
            .write_resume_manifest(&manifest, &[&provider])
            .expect("追加 Fixture 前应能写入已认证恢复清单");
        store
            .append_probe(run_id, &mut record, &[&provider])
            .expect("真实线级记录应能写盘");
        assert!(record.wire_exchanges.is_empty());
        assert!(record.wire_exchange_outcomes.is_empty());
        assert_eq!(record.wire_response_shapes.len(), 1);
        let relative = record.fixture_paths.first().expect("应写入 Fixture 路径");
        let fixture_path = store.run_dir().join(relative);
        let fixture_text = fs::read_to_string(&fixture_path).expect("应能重新读取 Fixture");
        assert!(!fixture_text.contains("secret-value"));
        let fixture: Value = serde_json::from_str(&fixture_text).expect("Fixture 应是有效 JSON");
        assert_eq!(fixture["schemaVersion"], "6");
        assert_eq!(fixture["payload"]["runId"], run_id);
        assert_eq!(fixture["payload"]["stableKey"], record.stable_key);
        assert_eq!(fixture["payload"]["providerId"], record.provider_id);
        assert_eq!(fixture["payload"]["model"], model);
        assert_eq!(fixture["payload"]["protocol"], "openai_responses");
        assert_eq!(fixture["payload"]["responseMode"], "buffered");
        assert_eq!(fixture["payload"]["capability"], "text");
        assert_eq!(fixture["payload"]["syntheticMarker"], marker);
        assert_eq!(fixture["payload"]["syntheticOnly"], true);
        assert_eq!(fixture["payload"]["replay"]["status"], "unavailable");
        assert!(
            fixture["payload"]["exchanges"][0]
                .get("responseBodyUtf8")
                .is_none()
        );
        assert!(
            fixture["payload"]["exchanges"][0]
                .get("responseContentType")
                .is_none()
        );
        assert_eq!(
            fixture["payload"]["exchanges"][0]["responseShape"],
            serde_json::to_value(&record.wire_response_shapes[0])
                .expect("Probe 响应结构证据应可序列化")
        );
        let serialized_record = serde_json::to_value(&record).expect("ProbeRecord 应可序列化");
        assert_eq!(
            serialized_record["wireResponseShapes"][0],
            fixture["payload"]["exchanges"][0]["responseShape"]
        );
        let content_sha256 = fixture["contentSha256"]
            .as_str()
            .expect("Fixture 应包含 Payload 摘要");
        let stable_key_digest = domain_separated_hex(
            b"keencode-provider-fixture-stable-key-v2",
            &[record.stable_key.as_bytes()],
        );
        assert_eq!(
            fixture_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("Fixture 文件名应是有效 Unicode"),
            format!(
                "{stable_key_digest}-{}.json",
                content_sha256
                    .strip_prefix("sha256:")
                    .expect("Fixture 摘要应带 sha256 前缀")
            )
        );
        assert_eq!(
            fixture["payload"]["exchanges"][0]["request"]["kind"],
            "synthetic_first_request"
        );
        assert_eq!(
            fixture["payload"]["exchanges"][0]["request"]["semanticMessageCount"],
            1
        );
        assert_eq!(record.status, "passed");
        assert!(record.fixture_replay.as_ref().is_some_and(|replay| {
            replay.status == "unavailable"
                && replay.reason.as_deref() == Some("线级响应未通过安全持久化门禁，正文已省略")
        }));
        drop(store);
        fs::remove_dir_all(&output_root).expect("应能清理 Fixture 测试目录");
    }

    /// 验证图片工具结果闭环只访问本地三协议服务，并保留调用关联与线级图片字段。
    #[tokio::test]
    async fn tool_result_image_round_trip_本地三协议验证两轮与不支持边界() {
        let run_id = "tool-result-image-round-trip-loopback";
        let model = "configured";
        let options = test_runtime_options(
            BTreeSet::from([ProbeKind::ToolResultImageRoundTrip]),
            false,
            3,
        );

        for protocol in all_protocols() {
            let marker_provider = provider_with_protocol("http://127.0.0.1:1/v1", protocol);
            let marker = expected_marker(
                &marker_provider,
                model,
                protocol,
                WireResponseMode::Buffered,
                ProbeKind::ToolResultImageRoundTrip,
                run_id,
            );
            let first_marker = first_turn_marker(&marker);
            let server = ImageRoundTripServer::start(protocol, &first_marker, &marker);
            let provider = provider_with_protocol(&server.base_url, protocol);
            let record = run_probe(
                &provider,
                model,
                protocol,
                WireResponseMode::Buffered,
                ProbeKind::ToolResultImageRoundTrip,
                &options,
                run_id,
            )
            .await;
            let requests = server.requests();
            let expected_request_count = if protocol == ProviderProtocol::ChatCompletions {
                1
            } else {
                2
            };
            let case = format!("protocol={}", protocol_name(protocol));

            assert_eq!(requests.len(), expected_request_count, "{case}");
            assert_eq!(
                record.wire_exchanges.len(),
                expected_request_count,
                "{case}"
            );
            assert_eq!(
                requests.first().map(|request| request.path.as_str()),
                Some(match protocol {
                    ProviderProtocol::Messages => "/v1/messages",
                    ProviderProtocol::ChatCompletions => "/v1/chat/completions",
                    ProviderProtocol::Responses => "/v1/responses",
                }),
                "{case}"
            );
            assert!(
                record.assertions.iter().any(|assertion| {
                    assertion.name == "image_round_trip_markers_distinct" && assertion.passed
                }),
                "{case}"
            );
            assert!(
                record.assertions.iter().any(|assertion| {
                    assertion.name == "tool_result_image_first_request_excludes_final_marker"
                        && assertion.passed
                }),
                "{case}"
            );

            if protocol == ProviderProtocol::ChatCompletions {
                assert_eq!(record.attempts, 1, "不可重试的协议能力拒绝不能再次请求模型");
                assert_eq!(record.status, "contract_violation", "{case}");
                assert!(record.normalized_error.as_ref().is_some_and(|error| {
                    error.kind == "unsupported_capability"
                        && !error.retryable
                        && error.http_status.is_none()
                }));
                assert!(record.assertions.iter().any(|assertion| {
                    assertion.name == "tool_result_image_supported" && !assertion.passed
                }));
                assert!(record.assertions.iter().any(|assertion| {
                    assertion.name == "tool_result_image_http_exchange_count" && assertion.passed
                }));
                continue;
            }

            assert_eq!(record.status, "passed", "{case}");
            assert!(
                record.assertions.iter().all(|assertion| assertion.passed),
                "{case}"
            );
            let second = &requests[1].body;
            assert_eq!(
                wire_tool_result_call_id(protocol, second).as_deref(),
                Some("call-image-1"),
                "{case}"
            );
            assert!(
                wire_tool_result_image_matches(protocol, second, &marker),
                "{case}"
            );

            let mut tampered_exchanges = record.wire_exchanges.clone();
            let tampered_call_id = "call-image-tampered";
            let second = tampered_exchanges
                .get_mut(1)
                .expect("Messages/Responses 应有第二轮交换");
            for message in &mut second.model_request.messages {
                for block in &mut message.content {
                    if let ContentBlock::ToolResult { tool_result } = block {
                        tool_result.tool_call_id = tampered_call_id.to_owned();
                    }
                }
            }
            match protocol {
                ProviderProtocol::Messages => {
                    for message in second
                        .request_body
                        .get_mut("messages")
                        .and_then(Value::as_array_mut)
                        .into_iter()
                        .flatten()
                    {
                        for block in message
                            .get_mut("content")
                            .and_then(Value::as_array_mut)
                            .into_iter()
                            .flatten()
                        {
                            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                                block["tool_use_id"] = Value::String(tampered_call_id.to_owned());
                            }
                        }
                    }
                }
                ProviderProtocol::Responses => {
                    for item in second
                        .request_body
                        .get_mut("input")
                        .and_then(Value::as_array_mut)
                        .into_iter()
                        .flatten()
                    {
                        if item.get("type").and_then(Value::as_str) == Some("function_call_output")
                        {
                            item["call_id"] = Value::String(tampered_call_id.to_owned());
                        }
                    }
                }
                ProviderProtocol::ChatCompletions => unreachable!("已在 Chat 分支前跳过"),
            }
            let tampered_assertions = image_round_trip_wire_assertions(
                &record,
                &tampered_exchanges,
                protocol,
                Some("call-image-1"),
            );
            assert!(tampered_assertions.iter().any(|assertion| {
                assertion.name == "tool_result_image_call_id_preserved" && !assertion.passed
            }));
        }
    }

    /// 验证精确补测直接执行冻结的高级能力，不请求目录、不补发文本门禁或其他 tuple。
    #[tokio::test]
    async fn probe_selected_retry_cases_只发送选择中的精确tuple() {
        let run_id = "retry-exact-test";
        let source_run_id = "retry-source-test";
        let model = "configured";
        let marker = expected_marker(
            &provider(),
            model,
            ProviderProtocol::Responses,
            WireResponseMode::Buffered,
            ProbeKind::Reasoning,
            run_id,
        );
        let server = CountingServer::start(GateResponse::ResponsesSuccess(marker));
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Reasoning]), false, 1);
        let case = RetryCase {
            source_sequence: 7,
            source_stable_key: probe_stable_key(
                source_run_id,
                "provider",
                model,
                "openai_responses",
                "buffered",
                "reasoning",
            ),
            tuple_key: retry_tuple_key(
                "provider",
                model,
                "openai_responses",
                "buffered",
                "reasoning",
            ),
            provider_id: "provider".to_owned(),
            model: model.to_owned(),
            protocol: "openai_responses".to_owned(),
            response_mode: "buffered".to_owned(),
            capability: "reasoning".to_owned(),
        };
        let mut callbacks = Vec::new();

        let probes = probe_selected_retry_cases(
            &provider,
            &options,
            run_id,
            std::slice::from_ref(&case),
            &BTreeMap::new(),
            |record, reused| {
                callbacks.push((record.capability.clone(), reused));
                Ok(())
            },
        )
        .await
        .expect("精确补测应完成冻结 tuple");

        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].capability, "reasoning");
        assert_eq!(callbacks, vec![("reasoning".to_owned(), false)]);
        assert_eq!(server.catalog_request_count(), 0);
        assert_eq!(server.model_request_count(), 1);
        assert_eq!(server.request_count(), 1);

        let padded_model = " configured ";
        let padded_case = RetryCase {
            source_sequence: 8,
            source_stable_key: probe_stable_key(
                source_run_id,
                "provider",
                padded_model,
                "openai_responses",
                "buffered",
                "reasoning",
            ),
            tuple_key: retry_tuple_key(
                "provider",
                padded_model,
                "openai_responses",
                "buffered",
                "reasoning",
            ),
            provider_id: "provider".to_owned(),
            model: padded_model.to_owned(),
            protocol: "openai_responses".to_owned(),
            response_mode: "buffered".to_owned(),
            capability: "reasoning".to_owned(),
        };
        let result = probe_selected_retry_cases(
            &provider,
            &options,
            run_id,
            &[padded_case],
            &BTreeMap::new(),
            |_, _| panic!("首尾空白模型标识不得进入补测回调"),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("精确补测必须在请求前拒绝首尾空白模型标识"),
        };

        assert!(error.contains("精确补测模型标识不能包含首尾空白"));
        assert_eq!(server.catalog_request_count(), 0);
        assert_eq!(server.model_request_count(), 1);
        assert_eq!(server.request_count(), 1);
    }

    /// 验证配置、目录和显式来源能够按精确 ID 合并。
    #[test]
    fn merge_candidates_记录全部来源() {
        let explicit = BTreeSet::from(["configured".to_owned(), "explicit".to_owned()]);
        let candidates = merge_candidates(
            &provider(),
            &["configured".to_owned(), "discovered".to_owned()],
            &explicit,
        );
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].configured);
        assert!(candidates[0].discovered);
        assert!(candidates[0].explicit);
        assert_eq!(candidates[1].model, "explicit");
    }

    /// 验证模型目录全部失败时仍对配置候选执行三协议文本探测。
    #[tokio::test]
    async fn probe_provider_目录全部失败时仍探测配置候选() {
        let server = CountingServer::start_with_catalog(
            GateResponse::NotFound,
            CatalogResponse::ServerError,
        );
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 3);
        let mut candidate_callbacks = 0;
        let mut probe_callbacks = 0;

        let execution = probe_provider(
            &provider,
            &options,
            "catalog-failure-test",
            &BTreeMap::new(),
            |_, candidates| {
                candidate_callbacks += 1;
                assert_eq!(candidates, ["configured"]);
                Ok(candidates.to_vec())
            },
            |_, _| {
                probe_callbacks += 1;
                Ok(())
            },
        )
        .await
        .expect("目录失败应形成事实记录而不是中断测试器");

        assert_eq!(execution.catalog.status, "failed");
        assert_eq!(execution.catalog.attempts, 3);
        assert!(execution.catalog.normalized_error.is_some());
        assert_eq!(execution.probes.len(), 6);
        assert!(
            execution
                .probes
                .iter()
                .all(|probe| probe.model == "configured" && probe.status == "failed")
        );
        assert_eq!(candidate_callbacks, 1);
        assert_eq!(probe_callbacks, 6);
        assert_eq!(server.catalog_request_count(), 3);
        assert_eq!(server.model_request_count(), 6);
        assert_eq!(server.request_count(), 9);
    }

    /// 验证多次分页失败会单调保留并探测配置及每次已观察模型。
    #[tokio::test]
    async fn probe_provider_三次部分目录失败保留全部已观察模型() {
        let server = CountingServer::start_with_catalog(
            GateResponse::NotFound,
            CatalogResponse::PartialAttemptSequence,
        );
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 3);
        let mut candidate_callbacks = 0;
        let mut probe_callbacks = 0;

        let execution = probe_provider(
            &provider,
            &options,
            "partial-catalog-union-test",
            &BTreeMap::new(),
            |_, candidates| {
                candidate_callbacks += 1;
                assert_eq!(candidates, ["configured", "model-a", "model-b", "model-c"]);
                Ok(candidates.to_vec())
            },
            |_, _| {
                probe_callbacks += 1;
                Ok(())
            },
        )
        .await
        .expect("多次部分目录失败应形成可恢复事实记录");

        assert_eq!(execution.catalog.status, "failed");
        assert_eq!(execution.catalog.attempts, 3);
        assert_eq!(
            execution.catalog.discovered_models,
            ["model-a", "model-b", "model-c"]
        );
        assert_eq!(
            execution
                .catalog
                .candidates
                .iter()
                .map(|candidate| candidate.model.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["configured", "model-a", "model-b", "model-c"])
        );
        assert_eq!(execution.probes.len(), 24);
        assert!(
            execution
                .probes
                .iter()
                .all(|probe| probe.status == "failed")
        );
        assert_eq!(candidate_callbacks, 1);
        assert_eq!(probe_callbacks, 24);
        assert_eq!(server.catalog_request_count(), 6);
        assert_eq!(server.model_request_count(), 24);
        assert_eq!(server.request_count(), 30);
    }

    /// 验证实时目录新增更早排序模型后，认证诊断仍按固定探针身份恢复。
    #[tokio::test]
    async fn probe_provider_新增目录模型不改变认证诊断恢复键() {
        let server = CountingServer::start_with_catalog(
            GateResponse::NotFound,
            CatalogResponse::Models(vec!["a-model".to_owned()]),
        );
        let mut provider = provider_with_base_url(&server.base_url);
        provider.models = vec!["z-model".to_owned()];
        let mut options = test_runtime_options(BTreeSet::new(), false, 1);
        options.diagnostics_only = true;
        let run_id = "stable-authentication-diagnostic-test";
        let authentication_model = "keencode-authentication-probe";
        let mut reusable = BTreeMap::new();
        let mut expected_invalid_auth_keys = BTreeSet::new();
        for protocol in all_protocols() {
            for response_mode in [WireResponseMode::Buffered, WireResponseMode::Streaming] {
                let invalid_auth_marker = diagnostic_marker(
                    &provider,
                    authentication_model,
                    protocol,
                    response_mode,
                    "diagnostic_invalid_authentication",
                    run_id,
                );
                let invalid_auth = restored_terminal_record(
                    &provider,
                    run_id,
                    authentication_model,
                    protocol,
                    response_mode,
                    "diagnostic_invalid_authentication",
                    "failed",
                    &invalid_auth_marker,
                );
                let invalid_auth_key = invalid_auth.stable_key();
                expected_invalid_auth_keys.insert(invalid_auth_key.clone());
                reusable.insert(invalid_auth_key, invalid_auth);

                let missing_model = missing_model_id(&provider, protocol, response_mode, run_id);
                let missing_marker = diagnostic_marker(
                    &provider,
                    &missing_model,
                    protocol,
                    response_mode,
                    "diagnostic_missing_model",
                    run_id,
                );
                let missing = restored_terminal_record(
                    &provider,
                    run_id,
                    &missing_model,
                    protocol,
                    response_mode,
                    "diagnostic_missing_model",
                    "contract_violation",
                    &missing_marker,
                );
                reusable.insert(missing.stable_key(), missing);
            }
        }
        let mut candidate_callbacks = 0;
        let mut reused_callbacks = 0;
        let mut actual_invalid_auth_keys = BTreeSet::new();

        let execution = probe_provider(
            &provider,
            &options,
            run_id,
            &reusable,
            |_, candidates| {
                candidate_callbacks += 1;
                assert_eq!(
                    candidates
                        .iter()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>(),
                    BTreeSet::from(["a-model", "z-model"])
                );
                Ok(vec!["a-model".to_owned(), "z-model".to_owned()])
            },
            |probe, reused| {
                assert!(reused);
                reused_callbacks += 1;
                if probe.capability == "diagnostic_invalid_authentication" {
                    assert_eq!(probe.model, authentication_model);
                    assert!(expected_invalid_auth_keys.contains(&probe.stable_key));
                    actual_invalid_auth_keys.insert(probe.stable_key.clone());
                }
                Ok(())
            },
        )
        .await
        .expect("固定认证探针应能在目录变化后完整恢复");

        assert_eq!(candidate_callbacks, 1);
        assert_eq!(reused_callbacks, 12);
        assert_eq!(execution.probes.len(), 12);
        assert_eq!(actual_invalid_auth_keys, expected_invalid_auth_keys);
        assert_eq!(actual_invalid_auth_keys.len(), 6);
        assert!(execution.probes.iter().all(|probe| {
            probe.capability != "diagnostic_invalid_authentication"
                || probe.model == authentication_model
        }));
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 0);
        assert_eq!(server.request_count(), 1);
    }

    /// 验证目录含无效稳定 ID 时整体失败，但仍探测配置中的安全候选。
    #[tokio::test]
    async fn probe_provider_目录含无效条目时仍探测配置候选() {
        let server = CountingServer::start_with_catalog(
            GateResponse::NotFound,
            CatalogResponse::InvalidEntry,
        );
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        let mut candidate_callbacks = 0;
        let mut probe_callbacks = 0;

        let execution = probe_provider(
            &provider,
            &options,
            "invalid-catalog-entry-test",
            &BTreeMap::new(),
            |_, candidates| {
                candidate_callbacks += 1;
                assert_eq!(candidates, ["configured"]);
                Ok(candidates.to_vec())
            },
            |_, _| {
                probe_callbacks += 1;
                Ok(())
            },
        )
        .await
        .expect("无效目录条目应形成失败事实记录");

        assert_eq!(execution.catalog.status, "failed");
        assert_eq!(execution.catalog.raw_count, 1);
        assert_eq!(execution.catalog.invalid_count, 1);
        assert_eq!(execution.probes.len(), 6);
        assert!(
            execution
                .probes
                .iter()
                .all(|probe| probe.model == "configured" && probe.status == "failed")
        );
        assert_eq!(candidate_callbacks, 1);
        assert_eq!(probe_callbacks, 6);
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 6);
        assert_eq!(server.request_count(), 7);
    }

    /// 验证实时目录的控制序列、C1 与 Unicode 方向字符不会进入报告、清单或执行集合。
    #[tokio::test]
    async fn probe_provider_拒绝危险目录模型标识() {
        let server = CountingServer::start_with_catalog(
            GateResponse::NotFound,
            CatalogResponse::Models(vec![
                "safe-model".to_owned(),
                "rtl\u{202e}txt.exe".to_owned(),
                "zero\u{200b}width".to_owned(),
                "ansi\u{001b}]0;owned\u{0007}".to_owned(),
                "c1\u{009b}31mowned".to_owned(),
                " padded-model ".to_owned(),
            ]),
        );
        let provider = provider_with_base_url(&server.base_url);
        let mut options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        options.catalog_only = true;
        let mut candidate_callbacks = 0;

        let execution = probe_provider(
            &provider,
            &options,
            "unsafe-catalog-model-test",
            &BTreeMap::new(),
            |_, candidates| {
                candidate_callbacks += 1;
                assert_eq!(candidates, ["configured", "safe-model"]);
                assert!(
                    candidates
                        .iter()
                        .all(|model| !contains_unsafe_inline_character(model))
                );
                Ok(candidates.to_vec())
            },
            |_, _| panic!("仅目录模式不得执行模型请求"),
        )
        .await
        .expect("危险目录模型应形成失败事实而不是进入候选集合");

        assert_eq!(execution.catalog.status, "failed");
        assert_eq!(execution.catalog.raw_count, 6);
        assert_eq!(execution.catalog.invalid_count, 5);
        assert_eq!(execution.catalog.discovered_models, ["safe-model"]);
        assert!(execution.catalog.candidates.iter().all(|candidate| {
            !contains_unsafe_inline_character(&candidate.model)
                && !candidate.model.starts_with("rtl")
                && !candidate.model.starts_with("zero")
                && !candidate.model.starts_with("ansi")
                && !candidate.model.starts_with("c1")
        }));
        let serialized = serde_json::to_string(&execution.catalog).expect("目录记录应可安全序列化");
        for rejected_prefix in ["rtl", "zero", "ansi", "c1", "padded-model"] {
            assert!(!serialized.contains(rejected_prefix));
        }
        assert_eq!(candidate_callbacks, 1);
        assert!(execution.probes.is_empty());
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 0);
    }

    /// 验证恢复回调返回的恶意模型标识会在合并和实际执行前被拒绝。
    #[tokio::test]
    async fn probe_provider_拒绝危险恢复候选模型标识() {
        let server = CountingServer::start(GateResponse::NotFound);
        let provider = provider_with_base_url(&server.base_url);
        let mut options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        options.catalog_only = true;

        let result = probe_provider(
            &provider,
            &options,
            "unsafe-resume-candidate-test",
            &BTreeMap::new(),
            |_, _| {
                Ok(vec![
                    "configured".to_owned(),
                    "forged\u{2067}model".to_owned(),
                ])
            },
            |_, _| panic!("危险恢复候选不得执行模型请求"),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("危险恢复候选必须被拒绝"),
        };

        assert!(error.contains("恢复候选模型标识"));
        assert!(!error.contains("forged"));
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 0);
    }

    /// 验证恢复回调不能用首尾空白制造与配置模型近似但不精确相等的请求标识。
    #[tokio::test]
    async fn probe_provider_拒绝首尾空白恢复候选模型标识() {
        let server = CountingServer::start(GateResponse::NotFound);
        let provider = provider_with_base_url(&server.base_url);
        let mut options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        options.catalog_only = true;

        let result = probe_provider(
            &provider,
            &options,
            "padded-resume-candidate-test",
            &BTreeMap::new(),
            |_, _| Ok(vec!["configured".to_owned(), " configured ".to_owned()]),
            |_, _| panic!("首尾空白恢复候选不得执行模型请求"),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("首尾空白恢复候选必须被拒绝"),
        };

        assert!(error.contains("恢复候选模型标识不能包含首尾空白"));
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 0);
    }

    /// 验证目录与配置都没有候选模型时不能把空矩阵标记为完整成功。
    #[tokio::test]
    async fn probe_provider_空候选目录保持未完成且不发模型请求() {
        let server = CountingServer::start(GateResponse::NotFound);
        let mut provider = provider_with_base_url(&server.base_url);
        provider.models.clear();
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        let mut candidate_callbacks = 0;

        let execution = probe_provider(
            &provider,
            &options,
            "empty-catalog-test",
            &BTreeMap::new(),
            |_, candidates| {
                candidate_callbacks += 1;
                assert!(candidates.is_empty());
                Ok(Vec::new())
            },
            |_, _| panic!("空候选目录不得产生模型记录"),
        )
        .await
        .expect("空候选目录应形成可恢复失败事实");

        assert_eq!(execution.catalog.status, "failed");
        assert!(
            execution
                .catalog
                .normalized_error
                .as_ref()
                .is_some_and(|error| {
                    error.kind == "decode"
                        && error.message_evidence
                            == ErrorMessageEvidence::from_text(
                                "模型目录与用户配置没有产生任何可测试模型",
                            )
                })
        );
        assert!(execution.probes.is_empty());
        assert_eq!(candidate_callbacks, 1);
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 0);
    }

    /// 验证完整矩阵模式不会因文本 404 而跳过同键的 Reasoning 请求。
    #[tokio::test]
    async fn probe_provider_full模式在文本404后仍执行reasoning() {
        let server = CountingServer::start(GateResponse::NotFound);
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(
            BTreeSet::from([ProbeKind::Text, ProbeKind::Reasoning]),
            true,
            1,
        );

        let execution = probe_provider(
            &provider,
            &options,
            "full-matrix-gate-test",
            &BTreeMap::new(),
            |_, candidates| Ok(candidates.to_vec()),
            |_, _| Ok(()),
        )
        .await
        .expect("完整矩阵应继续执行高级能力");

        let reasoning = execution
            .probes
            .iter()
            .filter(|probe| probe.capability == ProbeKind::Reasoning.as_str())
            .collect::<Vec<_>>();
        assert_eq!(reasoning.len(), 6);
        assert!(reasoning.iter().all(|probe| {
            probe.status != "skipped" && probe.attempts == 1 && probe.skip_evidence.is_none()
        }));
        assert_eq!(execution.probes.len(), 24, "十二个诊断加十二个能力探测");
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 24);
        assert_eq!(server.request_count(), 25);
    }

    /// 验证恢复回调冻结的历史模型仍会执行，而已恢复当前模型不会重复请求。
    #[tokio::test]
    async fn probe_provider_执行冻结历史模型并标记恢复来源() {
        let server = CountingServer::start(GateResponse::NotFound);
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        let run_id = "frozen-candidate-test";
        let historical_model = "historical-missing";
        let mut reusable = BTreeMap::new();
        for protocol in all_protocols() {
            for response_mode in [WireResponseMode::Buffered, WireResponseMode::Streaming] {
                let record = restored_text_record(&provider, protocol, response_mode, run_id);
                reusable.insert(record.stable_key(), record);
            }
        }
        let mut reused_callbacks = 0;
        let mut fresh_callbacks = 0;

        let execution = probe_provider(
            &provider,
            &options,
            run_id,
            &reusable,
            |_, candidates| {
                assert_eq!(candidates, ["configured"]);
                Ok(vec!["configured".to_owned(), historical_model.to_owned()])
            },
            |probe, reused| {
                if reused {
                    reused_callbacks += 1;
                    assert_eq!(probe.model, "configured");
                } else {
                    fresh_callbacks += 1;
                    assert_eq!(probe.model, historical_model);
                    assert_eq!(probe.attempts, 1);
                }
                Ok(())
            },
        )
        .await
        .expect("冻结历史模型应进入实际执行集合");

        assert!(execution.catalog.candidates.iter().any(|candidate| {
            candidate.model == historical_model && candidate.frozen_from_resume
        }));
        assert!(
            execution.catalog.candidates.iter().any(|candidate| {
                candidate.model == "configured" && !candidate.frozen_from_resume
            })
        );
        let historical = execution
            .probes
            .iter()
            .filter(|probe| probe.model == historical_model)
            .collect::<Vec<_>>();
        assert_eq!(historical.len(), 6);
        assert!(historical.iter().all(|probe| {
            probe.status == "failed" && probe.attempts == 1 && probe.skip_evidence.is_none()
        }));
        assert_eq!(reused_callbacks, 6);
        assert_eq!(fresh_callbacks, 6);
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 6);
        assert_eq!(server.request_count(), 7);
    }

    /// 验证全部基础记录从冷恢复日志复用时，除目录冻结外不会新增模型请求。
    #[tokio::test]
    async fn probe_provider_恢复全部文本记录后零新增生成请求() {
        let server = CountingServer::start(GateResponse::NotFound);
        let provider = provider_with_base_url(&server.base_url);
        let options = RuntimeOptions {
            user_data_directory: PathBuf::new(),
            config_path: PathBuf::new(),
            verify_run_dir: None,
            output_root: PathBuf::new(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 1,
            request_timeout_secs: 5,
            catalog_only: false,
            diagnostics_only: false,
            capabilities: BTreeSet::from([ProbeKind::Text]),
            full_matrix: false,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        };
        let mut reusable = BTreeMap::new();
        for protocol in all_protocols() {
            for response_mode in [WireResponseMode::Buffered, WireResponseMode::Streaming] {
                let record =
                    restored_text_record(&provider, protocol, response_mode, "resume-test");
                reusable.insert(record.stable_key(), record);
            }
        }
        let mut candidate_callbacks = 0;
        let mut reused_callbacks = 0;
        let execution = probe_provider(
            &provider,
            &options,
            "resume-test",
            &reusable,
            |_, candidates| {
                candidate_callbacks += 1;
                assert_eq!(candidates, ["configured"]);
                Ok(candidates.to_vec())
            },
            |_, reused| {
                assert!(reused);
                reused_callbacks += 1;
                Ok(())
            },
        )
        .await
        .expect("全部恢复记录应跳过生成请求");

        assert_eq!(candidate_callbacks, 1);
        assert_eq!(reused_callbacks, 6);
        assert_eq!(execution.probes.len(), 6);
        assert_eq!(server.request_count(), 1, "只允许一次模型目录请求");
    }

    /// 验证目录可重查一次，但全部已提交终态、诊断与取消都不会再次调用模型端点。
    #[tokio::test]
    async fn probe_provider_恢复全部终态后模型请求为零且目录校验一次() {
        let server = CountingServer::start(GateResponse::NotFound);
        let provider = provider_with_base_url(&server.base_url);
        let run_id = "resume-terminal-test";
        let options = RuntimeOptions {
            user_data_directory: PathBuf::new(),
            config_path: PathBuf::new(),
            verify_run_dir: None,
            output_root: PathBuf::new(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 1,
            request_timeout_secs: 5,
            catalog_only: false,
            diagnostics_only: false,
            capabilities: BTreeSet::from([
                ProbeKind::Text,
                ProbeKind::Reasoning,
                ProbeKind::Cancellation,
            ]),
            full_matrix: true,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        };
        let text_statuses = [
            "passed",
            "contract_violation",
            "failed",
            "unverified",
            "passed",
            "failed",
        ];
        let mut reusable = BTreeMap::new();
        let mut mode_index = 0;
        for protocol in all_protocols() {
            for response_mode in [WireResponseMode::Buffered, WireResponseMode::Streaming] {
                let invalid_auth_marker = diagnostic_marker(
                    &provider,
                    "keencode-authentication-probe",
                    protocol,
                    response_mode,
                    "diagnostic_invalid_authentication",
                    run_id,
                );
                let invalid_auth = restored_terminal_record(
                    &provider,
                    run_id,
                    "keencode-authentication-probe",
                    protocol,
                    response_mode,
                    "diagnostic_invalid_authentication",
                    "failed",
                    &invalid_auth_marker,
                );
                reusable.insert(invalid_auth.stable_key(), invalid_auth);

                let missing_model = missing_model_id(&provider, protocol, response_mode, run_id);
                let missing_marker = diagnostic_marker(
                    &provider,
                    &missing_model,
                    protocol,
                    response_mode,
                    "diagnostic_missing_model",
                    run_id,
                );
                let missing = restored_terminal_record(
                    &provider,
                    run_id,
                    &missing_model,
                    protocol,
                    response_mode,
                    "diagnostic_missing_model",
                    "contract_violation",
                    &missing_marker,
                );
                reusable.insert(missing.stable_key(), missing);

                for (capability, status) in [
                    (ProbeKind::Text, text_statuses[mode_index]),
                    (ProbeKind::Reasoning, "skipped"),
                    (ProbeKind::Cancellation, "unverified"),
                ] {
                    let marker = expected_marker(
                        &provider,
                        "configured",
                        protocol,
                        response_mode,
                        capability,
                        run_id,
                    );
                    let record = restored_terminal_record(
                        &provider,
                        run_id,
                        "configured",
                        protocol,
                        response_mode,
                        capability.as_str(),
                        status,
                        &marker,
                    );
                    reusable.insert(record.stable_key(), record);
                }
                mode_index += 1;
            }
        }
        let mut reused_callbacks = 0;
        let execution = probe_provider(
            &provider,
            &options,
            run_id,
            &reusable,
            |_, candidates| {
                assert_eq!(candidates, ["configured"]);
                Ok(candidates.to_vec())
            },
            |_, reused| {
                assert!(reused);
                reused_callbacks += 1;
                Ok(())
            },
        )
        .await
        .expect("全部终态恢复后不应再次调用模型端点");

        assert_eq!(reused_callbacks, 30);
        assert_eq!(execution.probes.len(), 30);
        assert_eq!(server.catalog_request_count(), 1);
        assert_eq!(server.model_request_count(), 0);
        assert_eq!(server.request_count(), 1);
    }

    /// 验证六个模型协议模式键的基础失败不会继续发送高级能力请求。
    #[tokio::test]
    async fn probe_provider_基础门禁失败后仅发送文本请求() {
        let server = CountingServer::start(GateResponse::NotFound);
        let provider = provider_with_base_url(&server.base_url);
        let options = RuntimeOptions {
            user_data_directory: PathBuf::new(),
            config_path: PathBuf::new(),
            verify_run_dir: None,
            output_root: PathBuf::new(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 1,
            request_timeout_secs: 5,
            catalog_only: false,
            diagnostics_only: false,
            capabilities: BTreeSet::from([ProbeKind::ToolCalling, ProbeKind::Usage]),
            full_matrix: false,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        };

        let execution = probe_provider(
            &provider,
            &options,
            "gate-test",
            &BTreeMap::new(),
            |_, candidates| Ok(candidates.to_vec()),
            |_, _| Ok(()),
        )
        .await
        .expect("基础门禁运行应完成");

        assert_eq!(server.request_count(), 7, "目录一次加六个文本门禁");
        assert_eq!(execution.probes.len(), 18);
        let text = execution
            .probes
            .iter()
            .filter(|probe| probe.capability == "text")
            .collect::<Vec<_>>();
        assert_eq!(text.len(), 6);
        assert!(text.iter().all(|probe| probe.status == "failed"));
        let skipped = execution
            .probes
            .iter()
            .filter(|probe| probe.status == "skipped")
            .collect::<Vec<_>>();
        assert_eq!(skipped.len(), 12);
        assert!(skipped.iter().all(|probe| {
            probe.attempts == 0
                && probe
                    .skip_evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.verification == "unverified")
        }));
    }

    /// 验证能力矩阵最多保持四条在途 lane，且每条 lane 的门禁回调先于后续能力请求。
    #[tokio::test]
    async fn probe_provider_并发lane上限且逐lane即时回调() {
        let server = ConcurrentProbeServer::start(MAX_CONCURRENT_PROBE_LANES);
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        let events = server.events();
        let callback_events = Arc::clone(&events);

        let execution = probe_provider(
            &provider,
            &options,
            "lane-concurrency-test",
            &BTreeMap::new(),
            |_, candidates| Ok(candidates.to_vec()),
            move |probe, _| {
                callback_events
                    .lock()
                    .expect("并发测试回调事件锁不应中毒")
                    .push(format!(
                        "callback:{}:{}:{}:{}",
                        probe.model, probe.protocol, probe.response_mode, probe.capability
                    ));
                Ok(())
            },
        )
        .await
        .expect("并发能力矩阵应完成");

        assert_eq!(execution.probes.len(), 6, "六条 lane 各包含一个 text 门禁");
        assert!(server.max_active() > 1, "本地服务必须观察到实际并发");
        assert!(
            server.max_active() <= MAX_CONCURRENT_PROBE_LANES,
            "在途 lane 不得超过固定上限"
        );
        assert_eq!(server.model_request_count(), 6);
    }

    /// 验证 lane 内的 text 记录写入回调后才会发出后续能力请求。
    #[tokio::test]
    async fn probe_provider_回调在lane后续能力请求前完成() {
        let server = ConcurrentProbeServer::start(0);
        let provider = provider_with_base_url(&server.base_url);
        let run_id = "lane-callback-order-test";
        let options = test_runtime_options(
            BTreeSet::from([ProbeKind::Text, ProbeKind::ToolCalling]),
            false,
            1,
        );
        let restored = restored_text_record(
            &provider,
            ProviderProtocol::Messages,
            WireResponseMode::Buffered,
            run_id,
        );
        let reusable = BTreeMap::from([(restored.stable_key(), restored)]);
        let events = server.events();
        let callback_events = Arc::clone(&events);

        let execution = probe_provider(
            &provider,
            &options,
            run_id,
            &reusable,
            |_, candidates| Ok(candidates.to_vec()),
            move |probe, _| {
                callback_events
                    .lock()
                    .expect("回调顺序事件锁不应中毒")
                    .push(format!(
                        "callback:{}:{}:{}:{}",
                        probe.model, probe.protocol, probe.response_mode, probe.capability
                    ));
                Ok(())
            },
        )
        .await
        .expect("回调顺序测试应完成");

        assert_eq!(execution.probes.len(), 12);
        assert_eq!(
            server.model_request_count(),
            6,
            "复用的 text 门禁不得重复请求"
        );
        let events = events.lock().expect("回调顺序事件锁不应中毒");
        let text_callback = "callback:configured:anthropic_messages:buffered:text";
        let text_callback_index = events
            .iter()
            .position(|event| event == text_callback)
            .expect("复用的 text 门禁应先触发回调");
        let tool_request = "request:configured:/v1/messages:false:tool";
        assert!(
            events
                .iter()
                .enumerate()
                .any(|(index, event)| index > text_callback_index && event == tool_request),
            "text 回调必须先于同一 lane 的 tool 请求"
        );
    }

    /// 验证任一记录回调失败后不会启动第五条 lane，也会取消其余在途请求。
    #[tokio::test]
    async fn probe_provider_回调失败立即取消其它lane() {
        let server = ConcurrentProbeServer::start(MAX_CONCURRENT_PROBE_LANES);
        let provider = provider_with_base_url(&server.base_url);
        let options = test_runtime_options(BTreeSet::from([ProbeKind::Text]), false, 1);
        let mut callback_count = 0;

        let result = probe_provider(
            &provider,
            &options,
            "lane-callback-error-test",
            &BTreeMap::new(),
            |_, candidates| Ok(candidates.to_vec()),
            |_, _| {
                callback_count += 1;
                Err("synthetic callback failure".to_owned())
            },
        )
        .await;

        assert_eq!(callback_count, 1);
        match result {
            Err(error) => assert_eq!(error, "synthetic callback failure"),
            Ok(_) => panic!("回调失败必须终止能力矩阵"),
        }
        assert_eq!(
            server.model_request_count(),
            MAX_CONCURRENT_PROBE_LANES,
            "回调失败后不得启动第五条 lane"
        );
        assert!(server.max_active() <= MAX_CONCURRENT_PROBE_LANES);
    }

    /// 验证基础限流只按上限重试文本门禁而不放大高级能力请求。
    #[tokio::test]
    async fn probe_provider_基础限流耗尽后跳过高级能力() {
        let server = CountingServer::start(GateResponse::RateLimited);
        let provider = provider_with_base_url(&server.base_url);
        let options = RuntimeOptions {
            user_data_directory: PathBuf::new(),
            config_path: PathBuf::new(),
            verify_run_dir: None,
            output_root: PathBuf::new(),
            resume_dir: None,
            recovery: None,
            retry: None,
            consolidation: None,
            provider_filters: BTreeSet::new(),
            model_filters: BTreeSet::new(),
            max_attempts: 3,
            request_timeout_secs: 5,
            catalog_only: false,
            diagnostics_only: false,
            capabilities: BTreeSet::from([ProbeKind::Text, ProbeKind::ToolCalling]),
            full_matrix: false,
            retry_scope_explicit: false,
            allow_unauthenticated_legacy_base: false,
        };

        let execution = probe_provider(
            &provider,
            &options,
            "rate-limit-gate-test",
            &BTreeMap::new(),
            |_, candidates| Ok(candidates.to_vec()),
            |_, _| Ok(()),
        )
        .await
        .expect("限流门禁运行应完成");

        assert_eq!(execution.probes.len(), 12);
        let text = execution
            .probes
            .iter()
            .filter(|probe| probe.capability == "text")
            .collect::<Vec<_>>();
        assert_eq!(text.len(), 6);
        assert!(text.iter().all(|probe| {
            probe.attempts == 3
                && probe
                    .normalized_error
                    .as_ref()
                    .is_some_and(|error| error.kind == "rate_limit" && error.retryable)
        }));
        assert_eq!(server.request_count(), 19, "目录一次加六个门禁各三次");
        let skipped = execution
            .probes
            .iter()
            .filter(|probe| probe.status == "skipped")
            .collect::<Vec<_>>();
        assert_eq!(skipped.len(), 6);
        assert!(skipped.iter().all(|probe| {
            probe
                .skip_evidence
                .as_ref()
                .is_some_and(|evidence| evidence.reason == "base_text_transient_failure")
        }));
    }

    /// 验证文本探测同时要求正常结束、精确正文和无工具调用。
    #[test]
    fn evaluate_text_拒绝额外空格() {
        let exact = response(vec![ContentBlock::text("KC_OK")], StopReason::Completed);
        assert!(
            evaluate_text(&exact, "KC_OK")
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
        let extra_space = response(vec![ContentBlock::text("KC_OK ")], StopReason::Completed);
        assert!(
            evaluate_text(&extra_space, "KC_OK")
                .assertions
                .iter()
                .any(|assertion| assertion.name == "visible_text_exact" && !assertion.passed)
        );
    }

    /// 验证工具调用必须具有唯一调用、固定名称和精确参数。
    #[test]
    fn evaluate_tool_calling_验证完整契约() {
        let valid = response(
            vec![ContentBlock::ToolCall {
                tool_call: ToolCall::new(
                    "call-1",
                    TOOL_NAME,
                    json!({"marker": "KC_OK", "count": EXPECTED_COUNT}),
                ),
            }],
            StopReason::ToolUse,
        );
        assert!(
            evaluate_tool_calling(&valid, "KC_OK")
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
        let wrong = response(
            vec![ContentBlock::ToolCall {
                tool_call: ToolCall::new(
                    "call-1",
                    TOOL_NAME,
                    json!({"marker": "wrong", "count": EXPECTED_COUNT}),
                ),
            }],
            StopReason::ToolUse,
        );
        assert!(
            evaluate_tool_calling(&wrong, "KC_OK")
                .assertions
                .iter()
                .any(|assertion| assertion.name == "tool_marker_exact" && !assertion.passed)
        );
    }

    /// 验证并行工具调用要求两个名称、参数和调用标识均完整且唯一。
    #[test]
    fn evaluate_parallel_tool_calling_验证两个独立调用() {
        let valid = response(
            vec![
                ContentBlock::ToolCall {
                    tool_call: ToolCall::new(
                        "call-left",
                        PARALLEL_LEFT_TOOL,
                        json!({"marker": "KC_OK", "side": "left"}),
                    ),
                },
                ContentBlock::ToolCall {
                    tool_call: ToolCall::new(
                        "call-right",
                        PARALLEL_RIGHT_TOOL,
                        json!({"marker": "KC_OK", "side": "right"}),
                    ),
                },
            ],
            StopReason::ToolUse,
        );
        assert!(
            evaluate_parallel_tool_calling(&valid, "KC_OK")
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
        let duplicate = response(
            vec![
                ContentBlock::ToolCall {
                    tool_call: ToolCall::new(
                        "same",
                        PARALLEL_LEFT_TOOL,
                        json!({"marker": "KC_OK", "side": "left"}),
                    ),
                },
                ContentBlock::ToolCall {
                    tool_call: ToolCall::new(
                        "same",
                        PARALLEL_RIGHT_TOOL,
                        json!({"marker": "KC_OK", "side": "right"}),
                    ),
                },
            ],
            StopReason::ToolUse,
        );
        assert!(duplicate_tool_id_fails(&duplicate));
    }

    /// 判断离线响应是否被唯一调用标识断言拒绝。
    fn duplicate_tool_id_fails(response: &ModelResponse) -> bool {
        evaluate_parallel_tool_calling(response, "KC_OK")
            .assertions
            .iter()
            .any(|assertion| assertion.name == "parallel_tool_ids_unique" && !assertion.passed)
    }

    /// 验证仅接受推理参数而没有任何推理证据不会通过。
    #[test]
    fn evaluate_reasoning_要求可观测证据() {
        let missing = response(vec![ContentBlock::text("KC_OK")], StopReason::Completed);
        assert!(
            evaluate_reasoning(&missing, "KC_OK")
                .assertions
                .iter()
                .any(|assertion| {
                    assertion.name == "reasoning_evidence_present" && !assertion.passed
                })
        );
        let present = response(
            vec![
                ContentBlock::Reasoning {
                    reasoning: ReasoningContent::new("推理证据"),
                },
                ContentBlock::text("KC_OK"),
            ],
            StopReason::Completed,
        );
        assert!(
            evaluate_reasoning(&present, "KC_OK")
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
    }

    /// 验证推理探测不会把 Messages 专用预算错误发送到另外两种协议。
    #[test]
    fn reasoning_request_按协议生成可编码配置() {
        let messages = reasoning_request("model", "KC_OK", ProviderProtocol::Messages);
        let chat = reasoning_request("model", "KC_OK", ProviderProtocol::ChatCompletions);
        let responses = reasoning_request("model", "KC_OK", ProviderProtocol::Responses);

        assert_eq!(
            messages
                .reasoning
                .as_ref()
                .and_then(|value| value.max_tokens),
            Some(512)
        );
        assert_eq!(
            chat.reasoning.as_ref().and_then(|value| value.max_tokens),
            None
        );
        assert!(!chat.reasoning.as_ref().unwrap().include_summary);
        assert_eq!(
            responses
                .reasoning
                .as_ref()
                .and_then(|value| value.max_tokens),
            None
        );
        assert!(responses.reasoning.as_ref().unwrap().include_summary);
    }

    /// 验证 Usage 探测区分未上报字段与真实正数，并检查总量一致性。
    #[test]
    fn evaluate_usage_要求核心用量且不伪造缺失值() {
        let missing = response(vec![ContentBlock::text("KC_OK")], StopReason::Completed);
        assert!(
            evaluate_usage(&missing, "KC_OK")
                .assertions
                .iter()
                .any(|assertion| assertion.name == "usage_reported" && !assertion.passed)
        );
        let mut valid = response(vec![ContentBlock::text("KC_OK")], StopReason::Completed);
        valid.usage = TokenUsage {
            input_tokens: Some(8),
            output_tokens: Some(3),
            reasoning_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: Some(11),
        };
        assert!(
            evaluate_usage(&valid, "KC_OK")
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
        valid.usage.total_tokens = Some(10);
        assert!(
            evaluate_usage(&valid, "KC_OK")
                .assertions
                .iter()
                .any(
                    |assertion| assertion.name == "total_tokens_consistent_if_reported"
                        && !assertion.passed
                )
        );
    }

    /// 验证输出上限探测接受正文或用量证据，但必须报告长度结束原因。
    #[test]
    fn evaluate_output_limit_要求统一截断原因() {
        let valid = response(
            vec![ContentBlock::text("1 KC_OK")],
            StopReason::MaxOutputTokens,
        );
        assert!(
            evaluate_output_limit(&valid)
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
        let completed = response(vec![ContentBlock::text("1 KC_OK")], StopReason::Completed);
        assert!(evaluate_output_limit(&completed).assertions.iter().any(
            |assertion| assertion.name == "stop_reason_max_output_tokens" && !assertion.passed
        ));
        let mut usage_only = response(Vec::new(), StopReason::MaxOutputTokens);
        usage_only.usage.output_tokens = Some(8);
        assert!(
            evaluate_output_limit(&usage_only)
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
    }

    /// 验证结构化输出同时执行唯一 JSON、Schema 和固定字段检查。
    #[test]
    fn evaluate_structured_output_验证原生契约() {
        let valid = response(
            vec![ContentBlock::text(
                json!({"marker": "KC_OK", "count": EXPECTED_COUNT}).to_string(),
            )],
            StopReason::Completed,
        );
        assert!(
            evaluate_structured_output(&valid, "KC_OK", &provider())
                .assertions
                .iter()
                .all(|assertion| assertion.passed)
        );
        let invalid = response(
            vec![ContentBlock::text(
                json!({"marker": "wrong", "count": EXPECTED_COUNT}).to_string(),
            )],
            StopReason::Completed,
        );
        let evaluation = evaluate_structured_output(&invalid, "KC_OK", &provider());
        assert!(evaluation.assertions.iter().any(|assertion| {
            assertion.name == "unique_json_schema_valid" && !assertion.passed
        }));
        assert_eq!(
            evaluation
                .normalized_error
                .expect("Schema 错误应有稳定分类")
                .kind,
            "provider_contract_violation_schema"
        );
    }

    /// 验证成功 Wire 响应与结构化输出语义错误可以同时匹配，其他错误层级仍被拒绝。
    #[test]
    fn fixture_outcome_结构化语义错误不覆盖成功wire响应() {
        let provider = provider();
        let marker = "KC_OK";
        let response = response(
            vec![ContentBlock::text(
                json!({"marker": "wrong", "count": EXPECTED_COUNT}).to_string(),
            )],
            StopReason::Completed,
        );
        let response_evidence = ResponseEvidence::from_response(&response, &provider);
        let actual_text_evidence = ActualTextEvidence::from_text(
            &provider,
            "structured-output-fixture-test",
            &response_text(&response),
        );
        let evaluation = evaluate_structured_output(&response, marker, &provider);
        let record = ProbeRecord {
            stable_key: "structured-output-fixture-test".to_owned(),
            provider_id: "provider".to_owned(),
            model: "configured".to_owned(),
            protocol: "responses".to_owned(),
            response_mode: "buffered".to_owned(),
            capability: ProbeKind::StructuredOutput.as_str().to_owned(),
            endpoint_path: "/v1/responses".to_owned(),
            status: "contract_violation".to_owned(),
            attempts: 1,
            latency_ms: 1,
            expected_text: None,
            synthetic_marker: Some(marker.to_owned()),
            actual_text_evidence: Some(actual_text_evidence.clone()),
            response: Some(response_evidence.clone()),
            assertions: evaluation.assertions,
            cancellation: None,
            skip_evidence: None,
            fixture_paths: Vec::new(),
            recovered_from: None,
            fixture_replay: None,
            normalized_error: evaluation.normalized_error,
            wire_response_shapes: Vec::new(),
            wire_exchanges: Vec::new(),
            wire_exchange_outcomes: Vec::new(),
        };
        let outcome = FixtureExchangeOutcome::Response {
            response: response_evidence,
            actual_text_evidence,
        };

        assert!(fixture_outcome_matches_record(&outcome, &record));

        let mut adapter_error = record.clone();
        adapter_error.normalized_error = Some(NormalizedError {
            kind: "decode".to_owned(),
            message_evidence: ErrorMessageEvidence::from_text("synthetic adapter error"),
            retryable: false,
            http_status: None,
        });
        assert!(!fixture_outcome_matches_record(&outcome, &adapter_error));

        let mut wrong_capability = record;
        wrong_capability.capability = ProbeKind::Text.as_str().to_owned();
        assert!(!fixture_outcome_matches_record(&outcome, &wrong_capability));
    }

    /// 验证取消通过只表示本地 Future 被及时丢弃。
    #[test]
    fn cancellation_assertions_不声称远端终止() {
        let passed = cancellation_assertions(WireResponseMode::Streaming, true, true, false);
        assert!(passed.iter().all(|assertion| assertion.passed));
        let missing_event =
            cancellation_assertions(WireResponseMode::Streaming, true, false, false);
        assert!(missing_event.iter().any(|assertion| {
            assertion.name == "stream_event_received_before_cancel" && !assertion.passed
        }));
        let buffered = cancellation_assertions(WireResponseMode::Buffered, true, false, false);
        assert!(
            buffered
                .iter()
                .all(|assertion| assertion.name != "stream_event_received_before_cancel")
        );
        assert!(buffered.iter().all(|assertion| assertion.passed));
        let completed = cancellation_assertions(WireResponseMode::Streaming, false, true, true);
        assert!(completed.iter().any(|assertion| !assertion.passed));
        assert!(completed.iter().any(|assertion| {
            assertion.name == "remote_termination_not_claimed" && assertion.passed
        }));
    }

    /// 验证原生契约错误与 Runtime 模拟错误不会归入通用 decode。
    #[test]
    fn normalize_error_区分结构化输出执行层() {
        let native = ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::InvalidJson,
            message: "Authorization: Bearer secret-value".to_owned(),
        };
        let emulated = ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::ToolEmulated,
            failure: StructuredOutputFailureKind::EmulationProtocol,
            message: "协议错误".to_owned(),
        };
        let native = normalize_error(&provider(), &native);
        assert_eq!(native.kind, "provider_contract_violation_invalid_json");
        let serialized = serde_json::to_string(&native).expect("错误证据应能序列化");
        assert!(!serialized.contains("secret-value"));
        assert_ne!(
            native.message_evidence,
            ErrorMessageEvidence::from_text("Authorization: Bearer secret-value")
        );
        assert!(native.message_evidence.utf8_bytes > 0);
        assert!(!native.message_evidence.truncated);
        assert!(!serialized.contains("sha256"));
        assert_eq!(
            normalize_error(&provider(), &emulated).kind,
            "runtime_emulation_protocol"
        );
    }

    /// 验证负向诊断区分预期拒绝、契约错分与暂时性不可判定错误。
    #[test]
    fn expected_error_probe_record_区分三类结果() {
        let make = |error: ModelError| {
            expected_error_probe_record(
                &provider(),
                "model",
                ProviderProtocol::Responses,
                WireResponseMode::Buffered,
                "diagnostic",
                "run",
                "KC_DIAG_0123456789abcdef",
                "/v1/responses".to_owned(),
                1,
                Err(error),
                |error| matches!(error, ModelError::ModelNotFound { .. }),
                "expected",
                "预期错误",
                "非预期错误",
            )
        };
        assert_eq!(
            make(ModelError::ModelNotFound {
                message: "missing".to_owned(),
                status_code: Some(404),
            })
            .status,
            "passed"
        );
        assert_eq!(
            make(ModelError::InvalidRequest {
                message: "wrong category".to_owned(),
            })
            .status,
            "contract_violation"
        );
        assert_eq!(
            make(ModelError::ProviderUnavailable {
                message: "temporary".to_owned(),
                status_code: Some(503),
                retryable: true,
            })
            .status,
            "failed"
        );
    }

    /// 验证推理块不会混入普通文本断言。
    #[test]
    fn response_text_只归并文本块() {
        let response = response(
            vec![
                ContentBlock::Reasoning {
                    reasoning: ReasoningContent::new("内部"),
                },
                ContentBlock::text("KC_OK"),
            ],
            StopReason::Completed,
        );
        assert_eq!(response_text(&response), "KC_OK");
    }

    /// 验证服务正文中的重试秒数不会被 RPM 数字误识别。
    #[test]
    fn retry_seconds_from_message_提取秒数后缀() {
        assert_eq!(
            retry_seconds_from_message("已达 RPM 上限 (10/分钟)，请约 9s 后重试"),
            Some(9)
        );
        assert_eq!(retry_seconds_from_message("请在 12 秒后重试"), Some(12));
        assert_eq!(retry_seconds_from_message("套餐次数已用尽"), None);
    }
}
