mod support;

use std::fs;
use std::sync::Arc;

use keencode_resources::{
    ArtifactMaterialization, ArtifactUse, ArtifactValidator, JournalConfig, ResourceError,
    SESSION_EVENT_SCHEMA, SESSION_EVENT_VERSION, SessionEvent, SessionEventId, SessionEventRecord,
    SessionId, SessionJournal, SessionOpen, SessionState, SessionStatus, reduce_record,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::TestJournalAppend;

/// 构造一个可直接交给公开归约器的指定时间事件记录。
fn record(
    state: &SessionState,
    event_id: &str,
    time_unix_ms: u64,
    event: SessionEvent,
) -> SessionEventRecord {
    SessionEventRecord {
        schema: SESSION_EVENT_SCHEMA.to_owned(),
        version: SESSION_EVENT_VERSION,
        event_id: SessionEventId::new(event_id).expect("事件 ID 应有效"),
        session: state.session_id.clone(),
        sequence: state
            .last_sequence
            .checked_add(1)
            .expect("测试 sequence 不应溢出"),
        time_unix_ms,
        event,
    }
}

/// 向指定状态成功应用一条带固定时间的测试事件。
fn apply(state: &mut SessionState, event_id: &str, time_unix_ms: u64, event: SessionEvent) {
    let record = record(state, event_id, time_unix_ms, event);
    reduce_record(state, record).expect("测试事件应成功归约");
}

/// 构造一个固定标题和项目目录的 Session 创建事件。
fn created_event() -> SessionEvent {
    SessionEvent::SessionCreated {
        title: "初始标题".to_owned(),
        project_root: "D:/workspace".to_owned(),
    }
}

/// 对不含 Artifact 的元数据事件拒绝任何实际 Artifact 校验调用。
struct PanicArtifactValidator;

impl ArtifactValidator for PanicArtifactValidator {
    /// 元数据事件不得请求普通 Artifact 实体验证。
    fn validate(
        &self,
        _session_id: &SessionId,
        _artifact: &ArtifactUse,
    ) -> Result<(), ResourceError> {
        panic!("Session 元数据事件不应遍历 Artifact")
    }

    /// 元数据事件不得请求 Artifact 物化验证。
    fn validate_materialization(
        &self,
        _session_id: &SessionId,
        _artifact: &ArtifactUse,
        _materialization: ArtifactMaterialization,
    ) -> Result<(), ResourceError> {
        panic!("Session 元数据事件不应遍历 Artifact 物化")
    }
}

/// 验证创建、普通事件和成功批次只提交顶层物理记录携带的时间与 sequence。
#[test]
fn physical_record_time_drives_session_metadata() {
    let session_id = SessionId::new("session-time").expect("Session ID 应有效");
    let mut state = SessionState::empty(session_id);
    assert_eq!(state.created_at_unix_ms, 0);
    assert_eq!(state.updated_at_unix_ms, 0);

    apply(&mut state, "event-created", 100, created_event());
    assert_eq!(state.created_at_unix_ms, 100);
    assert_eq!(state.updated_at_unix_ms, 100);
    assert_eq!(state.last_sequence, 1);

    apply(
        &mut state,
        "event-waiting",
        250,
        SessionEvent::SessionStatusChanged {
            status: SessionStatus::Waiting,
        },
    );
    assert_eq!(state.created_at_unix_ms, 100);
    assert_eq!(state.updated_at_unix_ms, 250);
    assert_eq!(state.last_sequence, 2);

    apply(
        &mut state,
        "event-batch",
        500,
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::SessionRenamed {
                    title: "批次标题".to_owned(),
                },
                SessionEvent::SessionStatusChanged {
                    status: SessionStatus::Idle,
                },
            ],
        },
    );
    assert_eq!(state.title, "批次标题");
    assert_eq!(state.created_at_unix_ms, 100);
    assert_eq!(state.updated_at_unix_ms, 500);
    assert_eq!(state.last_sequence, 3);
}

/// 验证批次后段失败不会提交先前标题修改、sequence 或任一时间字段。
#[test]
fn failed_atomic_batch_preserves_all_session_metadata() {
    let session_id = SessionId::new("session-batch-rollback").expect("Session ID 应有效");
    let mut state = SessionState::empty(session_id);
    apply(&mut state, "event-created", 100, created_event());
    let baseline = state.clone();
    let failed = record(
        &state,
        "event-failed-batch",
        900,
        SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::SessionRenamed {
                    title: "不得泄漏的标题".to_owned(),
                },
                SessionEvent::SessionRenamed {
                    title: " \t\r\n ".to_owned(),
                },
            ],
        },
    );

    assert!(reduce_record(&mut state, failed).is_err());
    assert_eq!(state, baseline);
    assert_eq!(state.created_at_unix_ms, 100);
    assert_eq!(state.updated_at_unix_ms, 100);
}

/// 验证重命名标题规范化、空白拒绝以及创建前和关闭后生命周期守卫。
#[test]
fn rename_enforces_title_and_session_lifecycle() {
    let session_id = SessionId::new("session-rename-guards").expect("Session ID 应有效");
    let mut state = SessionState::empty(session_id);
    let empty = state.clone();
    let before_created = record(
        &state,
        "event-before-created",
        10,
        SessionEvent::SessionRenamed {
            title: "过早标题".to_owned(),
        },
    );
    assert!(reduce_record(&mut state, before_created).is_err());
    assert_eq!(state, empty);

    apply(&mut state, "event-created", 20, created_event());
    let before_blank = state.clone();
    let blank = record(
        &state,
        "event-blank",
        30,
        SessionEvent::SessionRenamed {
            title: " \n\t ".to_owned(),
        },
    );
    assert!(reduce_record(&mut state, blank).is_err());
    assert_eq!(state, before_blank);

    apply(
        &mut state,
        "event-renamed",
        40,
        SessionEvent::SessionRenamed {
            title: "  规范标题  \n".to_owned(),
        },
    );
    assert_eq!(state.title, "规范标题");
    assert_eq!(state.created_at_unix_ms, 20);
    assert_eq!(state.updated_at_unix_ms, 40);

    apply(
        &mut state,
        "event-closed",
        50,
        SessionEvent::SessionClosed {},
    );
    let closed = state.clone();
    let after_closed = record(
        &state,
        "event-after-closed",
        60,
        SessionEvent::SessionRenamed {
            title: "关闭后标题".to_owned(),
        },
    );
    assert!(reduce_record(&mut state, after_closed).is_err());
    assert_eq!(state, closed);

    assert_eq!(
        serde_json::to_value(SessionEvent::SessionRenamed {
            title: "原始标题".to_owned(),
        })
        .expect("重命名事件应可编码"),
        json!({"type": "session_renamed", "payload": {"title": "原始标题"}})
    );
}

/// 验证 Journal 重放和 Snapshot 往返保留标题及物理事件时间。
#[test]
fn journal_and_snapshot_round_trip_session_metadata() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("session-metadata-round-trip").expect("Session ID 应有效");
    let validator = Arc::new(PanicArtifactValidator);
    let journal = match SessionJournal::open_with_artifact_validator(
        root.path(),
        session_id.clone(),
        JournalConfig::default(),
        validator.clone(),
    )
    .expect("Session Journal 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("新 Session 不应损坏：{:?}", report.issues),
    };
    let created = journal
        .append(created_event())
        .expect("Session 创建事件应写入");
    let renamed = journal
        .append(SessionEvent::SessionRenamed {
            title: "  持久标题  ".to_owned(),
        })
        .expect("Session 重命名事件应写入");
    let expected = journal.state().expect("Session 状态应读取");
    assert_eq!(expected.title, "持久标题");
    assert_eq!(expected.created_at_unix_ms, created.record.time_unix_ms);
    assert_eq!(expected.updated_at_unix_ms, renamed.record.time_unix_ms);
    journal.write_snapshot().expect("Snapshot 应写入");
    let snapshot_path = journal.snapshot_path().to_owned();
    drop(journal);

    let replayed = match SessionJournal::open_with_artifact_validator(
        root.path(),
        session_id.clone(),
        JournalConfig::default(),
        validator.clone(),
    )
    .expect("Session 应从 Snapshot 与日志重放")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("有效 Session 不应损坏：{:?}", report.issues),
    };
    assert_eq!(replayed.state().expect("重放状态应读取"), expected);
    drop(replayed);

    let mut incomplete_snapshot: Value =
        serde_json::from_slice(&fs::read(&snapshot_path).expect("Snapshot 应读取"))
            .expect("Snapshot JSON 应有效");
    let incomplete_state = incomplete_snapshot["state"]
        .as_object_mut()
        .expect("Snapshot 状态应为对象");
    incomplete_state.remove("createdAtUnixMs");
    incomplete_state.remove("updatedAtUnixMs");
    assert!(serde_json::from_value::<SessionState>(incomplete_snapshot["state"].clone()).is_err());
    fs::write(
        &snapshot_path,
        serde_json::to_vec_pretty(&incomplete_snapshot).expect("不完整 Snapshot 应编码"),
    )
    .expect("不完整 Snapshot 夹具应写入");

    let rebuilt = match SessionJournal::open_with_artifact_validator(
        root.path(),
        session_id,
        JournalConfig::default(),
        validator,
    )
    .expect("缺少时间字段的不完整 Snapshot 应由日志重建")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("健康日志不应损坏：{:?}", report.issues),
    };
    assert_eq!(rebuilt.state().expect("重建状态应读取"), expected);
    let rebuilt_snapshot: Value =
        serde_json::from_slice(&fs::read(snapshot_path).expect("重建 Snapshot 应读取"))
            .expect("重建 Snapshot JSON 应有效");
    assert_eq!(
        rebuilt_snapshot["state"]["createdAtUnixMs"],
        json!(created.record.time_unix_ms)
    );
    assert_eq!(
        rebuilt_snapshot["state"]["updatedAtUnixMs"],
        json!(renamed.record.time_unix_ms)
    );
    assert_eq!(rebuilt_snapshot["state"]["title"], json!("持久标题"));
}
