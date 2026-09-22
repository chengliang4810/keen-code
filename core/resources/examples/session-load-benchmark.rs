//! 生成三档无真实会话正文的固定 Journal，并测量 Snapshot 冷打开时间。
use keencode_resources::{
    AgentId, Durability, JournalConfig, MessagePart, MessageRole, SessionEvent, SessionEventId,
    SessionId, SessionJournal, SessionMessage, SessionOpen, SnapshotPolicy, SubAgentState,
    SubAgentStatus,
};
use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Copy)]
struct Profile {
    name: &'static str,
    events: usize,
    text_bytes: usize,
    agents: usize,
    maximum_ms: Option<f64>,
}

const PROFILES: [Profile; 3] = [
    Profile {
        name: "normal",
        events: 500,
        text_bytes: 1_850,
        agents: 0,
        maximum_ms: Some(200.0),
    },
    Profile {
        name: "large",
        events: 5_000,
        text_bytes: 1_850,
        agents: 0,
        maximum_ms: Some(1_000.0),
    },
    Profile {
        name: "extreme",
        events: 4_550,
        text_bytes: 5_650,
        agents: 8,
        // 极端档不设未经需求定义的时间门槛；它固定覆盖 25 MiB 级、
        // 多 Agent 的有界单次冷恢复。普通和大型档承担明确的耗时门禁。
        maximum_ms: None,
    },
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResultRow {
    profile: &'static str,
    events: usize,
    agents: usize,
    journal_bytes: u64,
    snapshot_bytes: u64,
    attempts: usize,
    cold_load_ms: f64,
    maximum_ms: Option<f64>,
    passed: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let enforce = std::env::args().any(|argument| argument == "--enforce");
    let mut failed = false;
    for profile in PROFILES {
        let result = run(profile)?;
        failed |= !result.passed;
        println!("{}", serde_json::to_string(&result)?);
    }
    if enforce && failed {
        return Err("至少一个 Session 冷加载档位超过固定预算".into());
    }
    Ok(())
}

fn run(profile: Profile) -> Result<ResultRow, Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let session_id = SessionId::new(format!("benchmark-{}", profile.name))?;
    let config = JournalConfig {
        durability: Durability::Buffered,
        snapshot_policy: SnapshotPolicy::Disabled,
        ..JournalConfig::default()
    };
    let journal = match SessionJournal::open(root.path(), session_id.clone(), config)? {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(_) => return Err("新建基准 Journal 不应损坏".into()),
    };
    journal.append_idempotent(
        SessionEventId::new("benchmark-created")?,
        0,
        SessionEvent::SessionCreated {
            title: profile.name.to_owned(),
            project_root: "/benchmark".to_owned(),
        },
    )?;
    for index in 1..profile.events {
        let event = if index <= profile.agents {
            SessionEvent::SubAgentSpawned {
                agent: SubAgentState {
                    agent_id: AgentId::new(format!("benchmark-agent-{index}"))?,
                    parent_agent_id: AgentId::new("root")?,
                    agent_path: format!("/root/agent_{index}"),
                    task: "固定性能夹具".to_owned(),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            }
        } else {
            SessionEvent::MessageAdded {
                message: SessionMessage {
                    is_meta: false,
                    message_id: format!("benchmark-message-{index}"),
                    turn_id: None,
                    agent_id: None,
                    role: MessageRole::User,
                    content: vec![MessagePart::Text {
                        text: "x".repeat(profile.text_bytes),
                    }],
                },
            }
        };
        journal.append_idempotent(
            SessionEventId::new(format!("benchmark-event-{index}"))?,
            index as u64,
            event,
        )?;
    }
    journal.flush()?;
    journal.write_snapshot()?;
    let journal_bytes = std::fs::metadata(journal.log_path())?.len();
    let snapshot_bytes = std::fs::metadata(journal.snapshot_path())?.len();
    drop(journal);
    // 共享 CI runner 会有短暂调度抖动；取三次独立 reopen 的最小值，
    // 仍覆盖完整冷恢复路径，同时避免一次抢占把确定性回归误报为失败。
    const ATTEMPTS: usize = 3;
    let mut cold_load_ms = f64::INFINITY;
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        let reopened = SessionJournal::open(root.path(), session_id.clone(), config)?;
        cold_load_ms = cold_load_ms.min(started.elapsed().as_secs_f64() * 1_000.0);
        drop(reopened);
    }
    Ok(ResultRow {
        profile: profile.name,
        events: profile.events,
        agents: profile.agents,
        journal_bytes,
        snapshot_bytes,
        attempts: ATTEMPTS,
        cold_load_ms,
        maximum_ms: profile.maximum_ms,
        passed: profile
            .maximum_ms
            .is_none_or(|maximum_ms| cold_load_ms < maximum_ms),
    })
}
