//! 显式授权的单模型 Messages 验证，复用现有语义探针，不发现模型或探测其他协议。

use super::*;

/// 仅保存合成线级正文，不生成依赖凭据的恢复证明，允许网关提供的短测试密钥。
#[tokio::test]
#[ignore = "需要 KEENCODE_MESSAGES_TEST_CONFIG、MODEL、EVIDENCE 显式配置及联网授权"]
async fn live_messages_protocol_matrix() {
    let required = |name| std::env::var(name).expect("缺少显式真实测试参数");
    let config: Value =
        serde_json::from_slice(&std::fs::read(required("KEENCODE_MESSAGES_TEST_CONFIG")).unwrap())
            .unwrap();
    let model = required("KEENCODE_MESSAGES_TEST_MODEL");
    let evidence = std::path::PathBuf::from(required("KEENCODE_MESSAGES_TEST_EVIDENCE"));
    std::fs::create_dir_all(&evidence).unwrap();
    let entries = config["providers"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "使用隔离的单供应商测试配置");
    let mut entry = entries[0].clone();
    assert_eq!(entry["apiBackend"], "messages");
    assert!(
        entry["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == &model)
    );
    let secret = entry["apiKey"].as_str().unwrap().to_owned();
    let endpoint = entry["baseUrl"].as_str().unwrap().to_owned();
    entry["baseUrl"] = endpoint
        .trim_end_matches('/')
        .strip_suffix("/messages")
        .unwrap()
        .into();
    let provider: ProviderEntry = serde_json::from_value(entry).unwrap();
    let redact = |text: String| {
        text.replace(&secret, "[redacted]")
            .replace(&endpoint, "[endpoint]")
    };
    let filter = std::env::var("KEENCODE_MESSAGES_TEST_CASES").ok();
    let mut records = Vec::new();
    for mode in [WireResponseMode::Buffered, WireResponseMode::Streaming] {
        for kind in ProbeKind::all() {
            // 长窗口另做有实际 usage 的分级探针，流中断由确定性传输测试覆盖。
            if matches!(
                kind,
                ProbeKind::ContextOverflow | ProbeKind::StreamInterruption
            ) {
                continue;
            }
            if filter
                .as_ref()
                .is_some_and(|s| !s.split(',').any(|v| v == kind.as_str()))
            {
                continue;
            }
            let name = format!("{}-{}", response_mode_name(mode), kind.as_str());
            let config = provider
                .provider_config(ProviderProtocol::Messages, mode, 120)
                .unwrap();
            let (client, trace) = ProviderClient::new_traced(config).unwrap();
            let started = Instant::now();
            let mut record = match kind {
                ProbeKind::Cancellation => {
                    let result = run_cancellation_attempt(
                        &client,
                        cancellation_request(&model, "KC_OK"),
                        mode,
                    )
                    .await;
                    json!({"passed":matches!(result, CancellationAttempt::LocalCancelled { .. }),
                        "scope":"local future/stream release; no claim of remote billing cancellation"})
                }
                ProbeKind::InvalidParameter => {
                    let result = client
                        .complete(invalid_parameter_request(&model, "KC_OK"))
                        .await;
                    json!({"passed":matches!(result, Err(ModelError::InvalidRequest { .. })),
                        "error":result.err().map(|e| redact(e.to_string()))})
                }
                _ => match execute_probe_attempt(
                    &client,
                    &model,
                    ProviderProtocol::Messages,
                    kind,
                    "KC_MESSAGES_OK",
                    &provider,
                )
                .await
                {
                    Ok(result) => {
                        json!({"passed":result.evaluation.assertions.iter().all(|a| a.passed),
                        "assertions":result.evaluation.assertions,"response":result.response})
                    }
                    Err(error) => json!({"passed":false,"error":redact(error.to_string())}),
                },
            };
            record["scenario"] = name.clone().into();
            if kind == ProbeKind::Reasoning {
                let mut request =
                    reasoning_request(&model, "KC_MESSAGES_OK", ProviderProtocol::Messages);
                request.max_output_tokens = None;
                request.reasoning.as_mut().unwrap().max_tokens = None;
                request.reasoning.as_mut().unwrap().effort = Some(ReasoningEffort::Medium);
                match client.complete(request).await {
                    Ok(response) => {
                        let passed = response.stop_reason == StopReason::Completed
                            && response_text(&response) == "KC_MESSAGES_OK";
                        record["default_output_budget_passed"] = passed.into();
                        record["passed"] = (record["passed"] == true && passed).into();
                        record["default_output_response"] = json!(response);
                    }
                    Err(error) => {
                        record["passed"] = false.into();
                        record["default_output_error"] = redact(error.to_string()).into();
                    }
                }
            }
            record["elapsed_ms"] = json!(started.elapsed().as_millis());
            let exchanges = trace.exchanges();
            let wire: Vec<_> = exchanges.iter().map(|e| json!({
                "request":e.request_body,"http_status":e.response_status,
                "response":redact(String::from_utf8_lossy(&e.response_body).into_owned()),
                "truncated":e.response_body_truncated,"eof_observed":e.response_body_eof_observed,
            })).collect();
            std::fs::write(
                evidence.join(format!("{name}.wire.json")),
                redact(serde_json::to_string_pretty(&wire).unwrap()),
            )
            .unwrap();
            eprintln!("Messages {name}: {}", record["passed"]);
            records.push(record);
            std::fs::write(
                evidence.join("protocol-matrix.json"),
                redact(serde_json::to_string_pretty(&records).unwrap()),
            )
            .unwrap();
        }
    }
    assert!(!records.is_empty());
    assert!(
        records.iter().all(|r| r["passed"] == true),
        "部分远端契约未通过，见逐项报告；不得自动降低验收条件"
    );
}
