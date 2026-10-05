//! 工作流事实复用父会话日志和产物存储，覆盖乱序拒绝、幂等和跨重启归属。

use keencode_resources::WorkflowJournalEvent;
use keencode_runtime::{CreateSessionRequest, OpenSessionResult, RuntimeConfig, RuntimeSession};
use serde_json::json;

fn record(sequence: u64) -> WorkflowJournalEvent {
    WorkflowJournalEvent {
        run_id: "run-1".into(),
        tool_call_id: "launch-1".into(),
        sequence,
        event_type: "node-completed".into(),
        payload: json!({"nodeId":"report", "result":{"ok":true}}),
        artifacts: Vec::new(),
        actor_session_id: None,
        launch_input_id: None,
    }
}

#[test]
fn parallel_append_allocates_sequence_atomically_and_retries_use_the_workflow_domain() {
    let root = tempfile::tempdir().unwrap();
    let config = RuntimeConfig::new(root.path());
    let session = RuntimeSession::create_session(
        config.clone(),
        CreateSessionRequest {
            session_id: "parallel-workflow".into(),
            title: "并发日志".into(),
            project_root: root.path().display().to_string(),
        },
    )
    .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
    let joins = (0..16)
        .map(|node| {
            let (session, barrier) = (session.clone(), barrier.clone());
            std::thread::spawn(move || {
                let mut event = record(0);
                event.payload = json!({"nodeId": format!("node-{node}"), "result": node});
                barrier.wait();
                let committed = session
                    .append_workflow_event(&format!("node-{node}"), event.clone())
                    .unwrap();
                assert_eq!(
                    session
                        .append_workflow_event(&format!("node-{node}"), event)
                        .unwrap(),
                    committed
                );
                committed
            })
        })
        .collect::<Vec<_>>();
    let committed = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    let state = session.snapshot().unwrap().state;
    assert_eq!(state.workflow_events["run-1"].len(), 16);
    assert_eq!(
        state.workflow_events["run-1"]
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (1..=16).collect::<Vec<_>>()
    );
    let original = committed[9].clone();
    let mut changed = original.clone();
    changed.payload = json!({"nodeId":"node-9", "result":"different"});
    assert!(session.append_workflow_event("node-9", changed).is_err());
    assert_eq!(
        session.snapshot().unwrap().state.last_sequence,
        state.last_sequence
    );
    drop(session);
    let OpenSessionResult::Ready(recovered) =
        RuntimeSession::open_session(config, "parallel-workflow").unwrap()
    else {
        panic!("应健康恢复");
    };
    assert_eq!(
        recovered
            .append_workflow_event("node-9", original.clone())
            .unwrap(),
        original
    );
    assert!(
        recovered
            .committed_control_event("node-9")
            .unwrap()
            .is_none()
    );
    assert!(
        recovered
            .committed_control_event_in_domain("keencode/workflow/event", "node-9")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        recovered.snapshot().unwrap().state.workflow_events["run-1"].len(),
        16
    );
}

#[test]
fn journal_retries_and_artifact_ownership_survive_cold_recovery() {
    let root = tempfile::tempdir().unwrap();
    let config = RuntimeConfig::new(root.path());
    let session = RuntimeSession::create_session(
        config.clone(),
        CreateSessionRequest {
            session_id: "workflow-parent".into(),
            title: "工作流日志测试".into(),
            project_root: root.path().display().to_string(),
        },
    )
    .unwrap();
    let artifact = session
        .put_artifact(b"confirmed result", Some("text/plain".into()))
        .unwrap();
    let mut first = record(1);
    first.artifacts.push(artifact.as_event_use());
    session
        .commit_workflow_event("run-1-event-1", first.clone())
        .unwrap();
    let before = session.snapshot().unwrap().state.last_sequence;
    session
        .commit_workflow_event("run-1-event-1", first.clone())
        .unwrap();
    assert_eq!(session.snapshot().unwrap().state.last_sequence, before);
    assert!(
        session
            .commit_workflow_event("run-1-event-3", record(3))
            .is_err()
    );
    assert_eq!(
        session.snapshot().unwrap().state.workflow_events["run-1"].len(),
        1
    );
    assert!(
        session
            .read_workflow_artifact("other-run", &artifact.artifact_id)
            .is_err()
    );
    drop(session);

    let OpenSessionResult::Ready(recovered) =
        RuntimeSession::open_session(config, "workflow-parent").unwrap()
    else {
        panic!("工作流日志应该健康恢复")
    };
    assert_eq!(
        recovered.snapshot().unwrap().state.workflow_events["run-1"],
        vec![first.clone()]
    );
    assert_eq!(
        recovered
            .read_workflow_artifact("run-1", &artifact.artifact_id)
            .unwrap(),
        b"confirmed result"
    );
    recovered
        .commit_workflow_event("run-1-event-1", first)
        .unwrap();
    recovered
        .commit_workflow_event("run-1-event-2", record(2))
        .unwrap();
    assert_eq!(
        recovered.snapshot().unwrap().state.workflow_events["run-1"].len(),
        2
    );
}

#[test]
fn conflicting_launch_identity_does_not_change_confirmed_history() {
    let root = tempfile::tempdir().unwrap();
    let session = RuntimeSession::create_session(
        RuntimeConfig::new(root.path()),
        CreateSessionRequest {
            session_id: "workflow-parent".into(),
            title: "关联身份测试".into(),
            project_root: root.path().display().to_string(),
        },
    )
    .unwrap();
    session.commit_workflow_event("one", record(1)).unwrap();
    let mut changed = record(2);
    changed.tool_call_id = "different-launch".into();
    assert!(session.commit_workflow_event("two", changed).is_err());
    assert_eq!(
        session.snapshot().unwrap().state.workflow_events["run-1"].len(),
        1
    );
}

#[test]
fn actor_binding_is_unique_and_survives_cold_recovery() {
    let root = tempfile::tempdir().unwrap();
    let config = RuntimeConfig::new(root.path());
    let actor = RuntimeSession::create_session(
        config.clone(),
        CreateSessionRequest {
            session_id: "workflow-actor".into(),
            title: "单层工作流 actor".into(),
            project_root: root.path().display().to_string(),
        },
    )
    .unwrap();
    assert!(!actor.is_workflow_actor().unwrap());
    let mut binding = record(1);
    binding.event_type = "actor-bound".into();
    binding.payload =
        json!({"parentSessionId":"workflow-parent","nodeId":"research","planEnabled":true});
    actor
        .commit_workflow_event("bind", binding.clone())
        .unwrap();
    assert!(actor.is_workflow_actor().unwrap());
    let mut another = binding.clone();
    another.run_id = "another-run".into();
    assert!(actor.commit_workflow_event("rebind", another).is_err());
    drop(actor);
    let OpenSessionResult::Ready(actor) =
        RuntimeSession::open_session(config, "workflow-actor").unwrap()
    else {
        panic!("actor 绑定应恢复为权威事实")
    };
    assert!(actor.is_workflow_actor().unwrap());
    assert_eq!(
        actor.snapshot().unwrap().state.workflow_events["run-1"],
        vec![binding]
    );
}
