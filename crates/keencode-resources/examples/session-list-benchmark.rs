//! 对隔离的存储副本比较完整历史恢复与列表索引；不测 UI 或操作系统冷缓存。
use keencode_resources::{JournalConfig, SessionJournal, list_session_ids};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::args().nth(1).ok_or("需要隔离存储副本路径")?);
    let ids = list_session_ids(&root)?;
    let config = JournalConfig::default();
    // 先测没有派生索引时的构建成本；调用者必须传入可丢弃副本。
    for id in &ids {
        let path = root.join(id.as_str()).join("metadata.json");
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let started = Instant::now();
    for id in &ids {
        SessionJournal::read_metadata(&root, id.clone(), config)?;
    }
    let index_build_ms = started.elapsed().as_secs_f64() * 1000.;
    let started = Instant::now();
    for id in &ids {
        SessionJournal::open(&root, id.clone(), config)?;
    }
    let full_restore_ms = started.elapsed().as_secs_f64() * 1000.;
    let started = Instant::now();
    for _ in 0..10 {
        for id in &ids {
            SessionJournal::read_metadata(&root, id.clone(), config)?;
        }
    }
    let index_list_mean_ms = started.elapsed().as_secs_f64() * 100.;
    println!(
        "{}",
        serde_json::json!({"sessions": ids.len(), "indexBuildMs": index_build_ms,
        "fullRestoreMs": full_restore_ms, "indexListMeanMs": index_list_mean_ms, "indexRuns": 10})
    );
    Ok(())
}
