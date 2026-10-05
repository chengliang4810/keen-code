use super::*;
use keencode_resources::{AgentId, MessagePart, SessionEventId, SessionId, SessionMessage, TurnId};

fn now() -> DateTime<Utc> {
    "2026-10-02T00:30:00Z".parse().unwrap()
}
fn archive() -> ActivityArchive {
    ActivityArchive {
        schema: ARCHIVE_SCHEMA.into(),
        version: 1,
        ..Default::default()
    }
}
fn project() -> keencode_resources::ProjectStorage {
    keencode_resources::ProjectStorage {
        id: "project-proof".into(),
        name: "Proof".into(),
        path: "D:/proof".into(),
    }
}
fn root_batch() -> SessionEvent {
    let turn_id = TurnId::new("turn-proof").unwrap();
    SessionEvent::AtomicBatch {
        events: vec![
            SessionEvent::TurnStarted {
                turn_id: turn_id.clone(),
                source_agent_id: AgentId::new("root").unwrap(),
                root_turn_id: turn_id.clone(),
                parent_turn_id: None,
                prompt_summary: "private prompt".into(),
            },
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    is_meta: false,
                    references: vec![keencode_model::InputReference {
                        name: "plugin-proof".into(),
                        path: "plugin://plugin-proof@market".into(),
                    }],
                    message_id: "message-proof".into(),
                    turn_id: Some(turn_id),
                    agent_id: None,
                    role: keencode_resources::MessageRole::User,
                    content: vec![MessagePart::Text {
                        text: "private prompt".into(),
                    }],
                },
            },
        ],
    }
}
fn request(
    id: &str,
    time_ms: u64,
    reported: bool,
    status: &str,
) -> crate::analytics::RequestRecord {
    crate::analytics::RequestRecord {
        id: id.into(),
        logical_request_id: id.into(),
        attempt: 1,
        max_attempts: 3,
        session_id: Some("session-proof".into()),
        turn_id: Some("turn-proof".into()),
        agent_id: Some("root".into()),
        purpose: "agent".into(),
        model: "model-proof".into(),
        provider: "endpoint.invalid".into(),
        protocol: "openai-chat-completions".into(),
        endpoint: None,
        request_mode: "streaming".into(),
        status: status.into(),
        http_status: Some(200),
        error_kind: None,
        error: None,
        requested_at_ms: time_ms,
        first_response_at_ms: None,
        completed_at_ms: None,
        duration_ms: 0,
        usage_reported: reported,
        input_tokens: 20,
        output_tokens: 10,
        reasoning_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        estimated: false,
        provider_request_id: None,
    }
}

#[test]
fn empty_profile_preserves_unknown_tokens_and_native_contract_shape() {
    let stats = profile(&archive(), 480, now(), "test-user").unwrap();
    assert_eq!(stats["activity"]["totalPromptsSent"], 0);
    assert_eq!(stats["activity"]["heatmap"].as_array().unwrap().len(), 274);
    assert_eq!(stats["quota"]["status"], "unavailable");
    assert_eq!(stats["identity"]["initials"], "TU");
    assert_eq!(stats["identity"]["defaultHandle"], "@testuser");
    let token_stats = tokens(&[], &archive(), 480, now()).unwrap();
    assert_eq!(token_stats["available"], false);
    assert!(token_stats["lifetimeTotalTokens"].is_null());
}

#[test]
fn native_prompts_dedupe_fork_history_and_exclude_meta_child_inputs() {
    let mut state = archive();
    for session in ["source", "fork"] {
        let mut projection = SessionProjection {
            archive: &mut state,
            root_turns: BTreeSet::new(),
            session_id: session.into(),
            project: project(),
            skill_requests: BTreeMap::new(),
        };
        projection.observe(
            &root_batch(),
            "event-proof",
            now().timestamp_millis() as u64,
        );
        let child = SessionEvent::TurnStarted {
            turn_id: TurnId::new("child-turn").unwrap(),
            source_agent_id: AgentId::new("child").unwrap(),
            root_turn_id: TurnId::new("turn-proof").unwrap(),
            parent_turn_id: Some(TurnId::new("turn-proof").unwrap()),
            prompt_summary: "delegation".into(),
        };
        projection.observe(&child, "child-event", 1);
        let mut message = match &root_batch() {
            SessionEvent::AtomicBatch { events } => match &events[1] {
                SessionEvent::MessageAdded { message } => message.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        message.message_id = "child-message".into();
        message.turn_id = Some(TurnId::new("child-turn").unwrap());
        projection.observe(
            &SessionEvent::MessageAdded {
                message: message.clone(),
            },
            "child-message-event",
            1,
        );
        message.message_id = "meta-message".into();
        message.is_meta = true;
        message.turn_id = Some(TurnId::new("turn-proof").unwrap());
        projection.observe(
            &SessionEvent::MessageAdded { message },
            "meta-message-event",
            1,
        );
        projection.observe(
            &SessionEvent::DynamicInputReceiptCommitted {
                turn_id: TurnId::new("turn-proof").unwrap(),
                source_agent_id: AgentId::new("root").unwrap(),
                model_round: 2,
                segment_index: 0,
                kind: keencode_resources::DynamicInputKind::UserSteer,
                through_sequence: 1,
                user_inputs: vec![keencode_resources::DynamicUserInput {
                    sequence: 1,
                    text: "private steer".into(),
                    references: vec![],
                }],
            },
            "steer-event",
            now().timestamp_millis() as u64,
        );
    }
    assert_eq!(state.prompts.len(), 2);
    assert_eq!(state.skills.len(), 1);
    let bytes = serde_json::to_string(&state).unwrap();
    assert!(!bytes.contains("private prompt"));
    assert!(!bytes.contains("private steer"));
    assert_eq!(state.prompts["message:message-proof"].session_id, "source");
}

#[test]
fn timezone_streak_and_heatmap_follow_local_calendar_and_rank_ties() {
    let mut state = archive();
    for (id, time) in [("a", "2026-09-29T16:10:00Z"), ("b", "2026-09-30T16:10:00Z")] {
        state.prompts.insert(
            id.into(),
            PromptFact {
                time_ms: time.parse::<DateTime<Utc>>().unwrap().timestamp_millis() as u64,
                turn_id: id.into(),
                session_id: "session".into(),
                project_id: "project-proof".into(),
                title: "Proof".into(),
                root: "D:/proof".into(),
            },
        );
    }
    let stats = profile(&state, 480, now(), "proof").unwrap();
    assert_eq!(stats["activity"]["currentStreakDays"], 2);
    assert_eq!(stats["activity"]["longestStreakDays"], 2);
    assert_eq!(stats["activity"]["promptsToday"], 0);
    assert_eq!(stats["activeHours"]["startHour"], 0);
    let active: Vec<_> = stats["activity"]["heatmap"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["count"] == 1)
        .collect();
    assert_eq!(active.len(), 2);
    assert!(active.iter().all(|cell| cell["intensity"] == 4));
    let west = profile(&state, -480, now(), "proof").unwrap();
    assert_eq!(west["timezone"]["today"], "2026-10-01");
    assert_eq!(west["activity"]["promptsToday"], 0);
    let west_days: Vec<_> = west["activity"]["heatmap"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["count"] == 1)
        .map(|cell| cell["day"].as_str().unwrap())
        .collect();
    assert_eq!(west_days, vec!["2026-09-29", "2026-09-30"]);
    assert!(validate_offset(1441).is_err());
    assert!(local_time(u64::MAX, 0).is_err());
}

#[test]
fn model_tokens_use_actual_turn_binding_reported_usage_and_local_day() {
    let mut state = archive();
    state.turns.insert(
        "turn-proof".into(),
        TurnFact {
            model: "provider-proof::model-proof".into(),
            reasoning: Some("high".into()),
        },
    );
    let time = "2026-10-01T16:30:00Z"
        .parse::<DateTime<Utc>>()
        .unwrap()
        .timestamp_millis() as u64;
    let mut records = vec![
        request("success", time, true, "success"),
        request("retry", time, true, "error"),
        request("unknown", time, false, "success"),
    ];
    let mut writing = request("writing", time, true, "success");
    writing.session_id = None;
    records.push(writing);
    let stats = tokens(&records, &state, 480, now()).unwrap();
    assert_eq!(stats["lifetimeTotalTokens"], 30);
    assert_eq!(stats["peakDay"], "2026-10-02");
    assert_eq!(stats["models"][0]["model"], "model-proof");
    assert_eq!(stats["unavailableProviders"], json!(["keencode"]));
    assert_eq!(
        tokens(&records, &state, -480, now()).unwrap()["peakDay"],
        "2026-10-01"
    );
    // 明确报告零 usage 与完全未报告分开，不能用真值判断丢弃零。
    records[0].input_tokens = 0;
    records[0].output_tokens = 0;
    let zero = tokens(&records, &state, 480, now()).unwrap();
    assert_eq!(zero["available"], true);
    assert_eq!(zero["lifetimeTotalTokens"], 0);
    assert!(zero["topProvider"].is_null());
    assert!(zero["models"].as_array().unwrap().is_empty());
    records[0].input_tokens = 20;
    records[0].output_tokens = 10;
    state.turns.get_mut("turn-proof").unwrap().model = "provider-proof::different-model".into();
    assert_eq!(
        tokens(&records, &state, 480, now()).unwrap()["models"][0]["model"],
        "model-proof"
    );
}

#[test]
fn projection_survives_native_session_deletion_and_cold_reload() {
    let temp = tempfile::tempdir().unwrap();
    let directory = keencode_resources::register_project_storage(temp.path(), &project()).unwrap();
    let id = SessionId::new("session-proof").unwrap();
    let journal =
        match SessionJournal::open(&directory, id.clone(), JournalConfig::default()).unwrap() {
            SessionOpen::Ready(journal) => journal,
            _ => panic!("新日志应健康"),
        };
    journal
        .append_idempotent(
            SessionEventId::new("created").unwrap(),
            0,
            SessionEvent::SessionCreated {
                title: "Proof".into(),
                project_root: "D:/proof".into(),
            },
        )
        .unwrap();
    journal
        .append_idempotent(SessionEventId::new("prompt").unwrap(), 1, root_batch())
        .unwrap();
    journal
        .append_idempotent(
            SessionEventId::new("completed").unwrap(),
            2,
            SessionEvent::TurnCompleted {
                turn_id: TurnId::new("turn-proof").unwrap(),
            },
        )
        .unwrap();
    journal.flush().unwrap();
    drop(journal);
    preserve_before_delete(temp.path()).unwrap();
    assert!(keencode_resources::delete_session_storage(&directory, &id).unwrap());
    let state = refresh(temp.path()).unwrap();
    assert_eq!(state.threads.len(), 1);
    assert_eq!(state.prompts.len(), 1);
    assert_eq!(load(temp.path()).unwrap().prompts.len(), 1);
    assert_eq!(
        profile(&state, 480, now(), "proof").unwrap()["mostWorkedProject"]["promptCount"],
        1
    );
    // 损坏投影不能静默清零，防止一次查询覆盖累计记录。
    std::fs::write(temp.path().join(ARCHIVE_FILE), b"invalid").unwrap();
    assert!(refresh(temp.path()).is_err());
}

#[test]
fn mutation_archives_are_not_counted_as_new_threads() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("session-mutations/records");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("edit.json"), serde_json::to_vec(&json!({"schema": "keencode/session-mutation", "version": 6, "kind": {"type": "edit_user"}, "targetSessionId": "archive", "sourceSessionId": "source", "targetLastSequence": 3})).unwrap()).unwrap();
    std::fs::write(directory.join("fork.json"), serde_json::to_vec(&json!({"schema": "keencode/session-mutation", "version": 6, "kind": {"type": "fork"}, "targetSessionId": "fork", "sourceSessionId": "source", "targetLastSequence": 3})).unwrap()).unwrap();
    let origins = mutation_origins(temp.path()).unwrap();
    assert_eq!(origins.archives, BTreeSet::from(["archive".into()]));
    assert_eq!(origins.owner("fork", 2).unwrap(), "source");
    assert_eq!(origins.owner("fork", 4).unwrap(), "fork");
}

#[test]
fn actual_fork_and_edit_mutations_preserve_lifetime_without_inventing_work() {
    let temp = tempfile::tempdir().unwrap();
    let directory = keencode_resources::register_project_storage(temp.path(), &project()).unwrap();
    let id = SessionId::new("source-proof").unwrap();
    let journal =
        match SessionJournal::open(&directory, id.clone(), JournalConfig::default()).unwrap() {
            SessionOpen::Ready(journal) => journal,
            _ => panic!("健康日志"),
        };
    journal
        .append_idempotent(
            SessionEventId::new("created").unwrap(),
            0,
            SessionEvent::SessionCreated {
                title: "Proof".into(),
                project_root: "D:/proof".into(),
            },
        )
        .unwrap();
    journal
        .append_idempotent(SessionEventId::new("prompt").unwrap(), 1, root_batch())
        .unwrap();
    journal
        .append_idempotent(
            SessionEventId::new("completed").unwrap(),
            2,
            SessionEvent::TurnCompleted {
                turn_id: TurnId::new("turn-proof").unwrap(),
            },
        )
        .unwrap();
    journal.flush().unwrap();
    drop(journal);
    let fork = keencode_resources::fork_session(
        &directory,
        JournalConfig::default(),
        keencode_resources::ArtifactLimits::default(),
        keencode_resources::SessionForkRequest {
            source_session_id: id.clone(),
            operation_id: "fork-proof".into(),
            title: None,
            through_turn_id: None,
        },
    )
    .unwrap();
    let edited = keencode_resources::prepare_edit_user(
        &directory,
        JournalConfig::default(),
        keencode_resources::ArtifactLimits::default(),
        keencode_resources::SessionEditUserRequest {
            source_session_id: id.clone(),
            target_message_id: "message-proof".into(),
            expected_text: "private prompt".into(),
            operation_id: "edit-proof".into(),
        },
    )
    .unwrap();
    let state = refresh(temp.path()).unwrap();
    assert_eq!(
        state.threads,
        BTreeSet::from([id.as_str().into(), fork.session_id.as_str().into()])
    );
    assert_eq!(state.prompts.len(), 1);
    assert_eq!(
        state.prompts["message:message-proof"].session_id,
        "source-proof"
    );
    assert!(!state.threads.contains(edited.archived_session_id.as_str()));
    assert_eq!(state.skills.len(), 1);
}

#[test]
fn model_usage_merges_snapshot_and_record_names_without_exposing_provider_ids() {
    let mut state = archive();
    for (turn, provider) in [
        ("turn-proof", "provider-private-one"),
        ("second-turn", "provider-private-two"),
    ] {
        state.turns.insert(
            turn.into(),
            TurnFact {
                model: format!("{provider}::model-proof"),
                reasoning: None,
            },
        );
        state.prompts.insert(
            format!("message:{turn}"),
            PromptFact {
                time_ms: now().timestamp_millis() as u64,
                turn_id: turn.into(),
                session_id: "session-proof".into(),
                project_id: "project-proof".into(),
                title: "Proof".into(),
                root: "D:/proof".into(),
            },
        );
    }
    let time = now().timestamp_millis() as u64;
    let first = request("first", time, true, "success");
    let mut second = request("second", time, true, "success");
    second.turn_id = Some("second-turn".into());
    let mut unbound = request("unbound", time, true, "success");
    unbound.turn_id = None;
    let mut different = request("different", time, true, "success");
    different.model = "a-low-model".into();
    let usage = tokens(&[first, second, unbound, different], &state, 480, now()).unwrap();
    assert_eq!(usage["models"].as_array().unwrap().len(), 2);
    let model = usage["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model"] == "model-proof")
        .unwrap();
    assert_eq!(usage["models"][0]["model"], "model-proof");
    assert_eq!(model["tokens"], 90);
    assert_eq!(model["percent"].as_f64(), Some(75.0));
    assert_eq!(usage["lifetimeTotalTokens"], 120);
    let core = profile(&state, 480, now(), "proof").unwrap();
    assert_eq!(core["providerModels"].as_array().unwrap().len(), 1);
    assert_eq!(core["providerModels"][0]["model"], "model-proof");
    assert_eq!(core["providerModels"][0]["percent"].as_f64(), Some(100.0));
    assert!(
        !serde_json::to_string(&usage)
            .unwrap()
            .contains("provider-private")
    );
    assert!(
        !serde_json::to_string(&core)
            .unwrap()
            .contains("provider-private")
    );
}

#[test]
fn native_model_usage_ranks_top_eight_by_tokens_and_keeps_full_total() {
    let time = now().timestamp_millis() as u64;
    let records: Vec<_> = (0..10)
        .map(|index| {
            let mut value = request(&format!("request-{index}"), time, true, "success");
            value.model = format!("model-{index:02}");
            value.input_tokens = index;
            value.output_tokens = 0;
            value
        })
        .collect();
    let stats = tokens(&records, &archive(), 480, now()).unwrap();
    let models = stats["models"].as_array().unwrap();
    assert_eq!(models.len(), 8);
    assert_eq!(models[0]["model"], "model-09");
    assert_eq!(models[7]["model"], "model-02");
    assert_eq!(stats["lifetimeTotalTokens"], 45);
    assert_eq!(models[0]["percent"].as_f64(), Some(20.0));
    assert!(models.iter().all(|row| row["tokens"].as_u64().unwrap() > 0));
}
