use super::*;

#[tokio::test]
async fn exact_archive_receipt_survives_pin_but_not_undo_rearchive() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let runtime =
        crate::agent_runtime::AgentRuntime::new_for_control_test(temp.path().join("data")).unwrap();
    let session = runtime
        .open_or_create_session(&project, None, "archive-test")
        .unwrap();
    let id = session.session_id().as_str().to_owned();
    let archived = session
        .set_preference("archive-a", None, Some(true))
        .unwrap();
    assert!(validate_archive(&session, "archive-a", archived.last_sequence).is_ok());
    assert!(validate_archive(&session, "archive-a", archived.last_sequence + 1).is_err());
    assert!(validate_archive(&session, "unknown", archived.last_sequence).is_err());
    session.set_preference("pin-a", Some(true), None).unwrap();
    assert!(validate_archive(&session, "archive-a", archived.last_sequence).is_ok());
    session.set_preference("undo-a", None, Some(false)).unwrap();
    assert!(validate_archive(&session, "archive-a", archived.last_sequence).is_err());
    let next = session
        .set_preference("archive-b", None, Some(true))
        .unwrap();
    assert!(
        validate_archive(&session, "archive-a", archived.last_sequence)
            .unwrap_err()
            .contains("替代")
    );
    assert!(validate_archive(&session, "archive-b", next.last_sequence).is_ok());
    drop(session);
    runtime.close_session(&id).await.unwrap();
}

#[tokio::test]
async fn deleted_session_tombstone_does_not_block_archive_reference_check() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let project = std::fs::canonicalize(project).unwrap();
    let runtime =
        crate::agent_runtime::AgentRuntime::new_for_control_test(temp.path().join("data")).unwrap();

    let deleted_session = runtime
        .open_or_create_session(&project, None, "archive-deleted")
        .unwrap();
    let deleted_id = deleted_session.session_id().as_str().to_owned();
    drop(deleted_session);
    runtime.close_session(&deleted_id).await.unwrap();
    let deleted_id = keencode_resources::SessionId::new(deleted_id).unwrap();
    keencode_resources::record_deleted_session(
        runtime.storage_root(),
        &deleted_id,
        &project.to_string_lossy(),
    )
    .unwrap();

    let live_session = runtime
        .open_or_create_session(&project, None, "archive-live")
        .unwrap();
    let live_id = live_session.session_id().as_str().to_owned();
    drop(live_session);
    runtime.close_session(&live_id).await.unwrap();

    let deleted = keencode_resources::list_deleted_session_ids(
        runtime.storage_root(),
        &project.to_string_lossy(),
    )
    .unwrap()
    .into_iter()
    .collect();
    let sessions = runtime.stored_sessions().unwrap();
    let deleted_metadata = sessions
        .iter()
        .find(|session| session.session_id == deleted_id)
        .unwrap();
    let live_metadata = sessions
        .iter()
        .find(|session| session.session_id.as_str() == live_id)
        .unwrap();

    assert!(!is_archive_reference_candidate(
        deleted_metadata,
        "archive-owner",
        &deleted
    ));
    assert!(is_archive_reference_candidate(
        live_metadata,
        "archive-owner",
        &deleted
    ));
}
