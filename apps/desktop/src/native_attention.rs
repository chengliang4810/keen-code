//! 原生会话的已读水位持久域。
//!
//! 这里只保存每个 Session 最后确认的 Journal sequence，不复制消息正文或
//! Runtime 状态。水位单调推进，UI 可以用它与当前最新 sequence 比较得到未读状态。

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::native_paths::NativePaths;

const ATTENTION_SCHEMA: &str = "keencode/native-attention";
const ATTENTION_VERSION: u32 = 1;
const ATTENTION_FILE: &str = "native-attention.json";
const ATTENTION_LOCK_FILE: &str = "native-attention.json.lock";
const MAX_ATTENTION_FILE_BYTES: u64 = 512 * 1024;
const MAX_ATTENTION_ENTRIES: usize = 1024;
const MAX_SESSION_ID_BYTES: usize = 256;

static ATTENTION_STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 进程内共享的已读水位服务；路径在创建时固定，避免后台任务漂移到另一数据根。
#[derive(Clone)]
pub(crate) struct NativeAttentionStore {
    paths: NativePaths,
}

impl NativeAttentionStore {
    /// 创建已读水位服务；目录和文件延迟到第一次成功写入。
    pub(crate) fn new(paths: &NativePaths) -> Result<Self> {
        if paths.data_root.as_os_str().is_empty() {
            bail!("已读水位数据根不能为空");
        }
        Ok(Self {
            paths: paths.clone(),
        })
    }

    /// 比较当前最新 Journal sequence；缺少水位时按未读处理以避免漏报。
    pub(crate) fn is_unread(&self, session_id: &str, last_seq: u64) -> Result<bool> {
        validate_session_id(session_id)?;
        if last_seq == 0 {
            return Ok(false);
        }
        let _guard = attention_store_guard()?;
        let document = self.load_document()?;
        Ok(document
            .entries
            .iter()
            .find(|entry| entry.session_id == session_id)
            .is_none_or(|entry| last_seq > entry.read_seq))
    }

    /// 一次读取全部已读水位，供工作区列表批量计算未读状态。
    pub(crate) fn read_watermarks(&self) -> Result<HashMap<String, u64>> {
        let _guard = attention_store_guard()?;
        let document = self.load_document()?;
        Ok(document
            .entries
            .into_iter()
            .map(|entry| (entry.session_id, entry.read_seq))
            .collect())
    }

    /// 单调推进一个会话的已读水位；同 seq 或更旧 seq 不触发文件写入。
    pub(crate) fn mark_read(&self, session_id: &str, seq: u64) -> Result<()> {
        validate_session_id(session_id)?;
        if seq == 0 {
            return Ok(());
        }

        let path = self.attention_path()?;
        let _guard = attention_store_guard()?;
        let _file_lock = open_attention_lock(&path)?;
        let mut document = self.load_document()?;
        let next_last_used = next_last_used(&document)?;
        if let Some(entry) = document
            .entries
            .iter_mut()
            .find(|entry| entry.session_id == session_id)
        {
            if seq <= entry.read_seq {
                return Ok(());
            }
            entry.read_seq = seq;
            entry.last_used = next_last_used;
        } else {
            if document.entries.len() >= MAX_ATTENTION_ENTRIES {
                let Some((oldest_index, _)) = document
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.last_used)
                else {
                    bail!("已读水位记录为空，无法执行容量淘汰");
                };
                document.entries.remove(oldest_index);
            }
            document.entries.push(AttentionEntry {
                session_id: session_id.to_owned(),
                read_seq: seq,
                last_used: next_last_used,
            });
        }
        validate_document(&document)?;
        write_document(&path, &document)
    }

    fn attention_path(&self) -> Result<PathBuf> {
        Ok(crate::storage::root_dir(&self.paths)?.join(ATTENTION_FILE))
    }

    fn load_document(&self) -> Result<AttentionDocument> {
        read_document(&self.attention_path()?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AttentionDocument {
    schema: String,
    version: u32,
    entries: Vec<AttentionEntry>,
}

impl Default for AttentionDocument {
    fn default() -> Self {
        Self {
            schema: ATTENTION_SCHEMA.to_owned(),
            version: ATTENTION_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AttentionEntry {
    session_id: String,
    read_seq: u64,
    last_used: u64,
}

fn read_document(path: &Path) -> Result<AttentionDocument> {
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(path, MAX_ATTENTION_FILE_BYTES, "原生已读水位")?
    else {
        return Ok(AttentionDocument::default());
    };
    let document: AttentionDocument = serde_json::from_slice(&bytes)
        .with_context(|| format!("原生已读水位格式无效：{}", path.display()))?;
    validate_document(&document)?;
    Ok(document)
}

fn write_document(path: &Path, document: &AttentionDocument) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(document).context("序列化原生已读水位失败")?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_ATTENTION_FILE_BYTES {
        bail!("原生已读水位超过 {MAX_ATTENTION_FILE_BYTES} 字节");
    }
    crate::storage::atomic_write_private_bytes(path, &bytes)
        .with_context(|| format!("写入原生已读水位失败：{}", path.display()))
}

fn validate_document(document: &AttentionDocument) -> Result<()> {
    if document.schema != ATTENTION_SCHEMA || document.version != ATTENTION_VERSION {
        bail!("原生已读水位 schema 或版本不受支持");
    }
    if document.entries.len() > MAX_ATTENTION_ENTRIES {
        bail!("已读水位会话数量超过 {MAX_ATTENTION_ENTRIES}");
    }
    let mut session_ids = HashSet::with_capacity(document.entries.len());
    for entry in &document.entries {
        validate_session_id(&entry.session_id)?;
        if entry.last_used == 0 {
            bail!("原生已读水位 LRU 序号必须为正数");
        }
        if !session_ids.insert(entry.session_id.as_str()) {
            bail!("原生已读水位包含重复 sessionId");
        }
    }
    Ok(())
}

fn next_last_used(document: &AttentionDocument) -> Result<u64> {
    document
        .entries
        .iter()
        .map(|entry| entry.last_used)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .context("已读水位 LRU 序号溢出")
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty()
        || session_id.trim() != session_id
        || session_id.len() > MAX_SESSION_ID_BYTES
        || session_id.chars().any(char::is_control)
    {
        bail!("sessionId 不能为空、不能含首尾空白或控制字符");
    }
    Ok(())
}

fn attention_store_guard() -> Result<std::sync::MutexGuard<'static, ()>> {
    ATTENTION_STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("已读水位持久化锁已损坏"))
}

fn open_attention_lock(path: &Path) -> Result<fs::File> {
    let parent = path.parent().context("已读水位路径缺少父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建已读水位目录失败：{}", parent.display()))?;
    let lock_path = parent.join(ATTENTION_LOCK_FILE);
    let file = fs::OpenOptions::new()
        .create(true)
        // 锁文件复用同一 inode；截断会改变其他进程正在持有的锁对象语义。
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("打开已读水位锁失败：{}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("锁定已读水位失败：{}", lock_path.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn paths(data_root: &Path) -> NativePaths {
        NativePaths::new(
            data_root.to_owned(),
            data_root.to_owned(),
            data_root.join("Documents"),
        )
    }

    #[test]
    fn watermark_is_monotonic_and_reports_only_newer_sequences() {
        let data = tempfile::tempdir().expect("数据根");
        let store = NativeAttentionStore::new(&paths(data.path())).expect("已读水位服务");

        assert!(store.is_unread("session-a", 1).expect("缺少水位时未读"));
        store.mark_read("session-a", 4).expect("写入水位");
        assert!(!store.is_unread("session-a", 4).expect("相同水位"));
        assert!(!store.is_unread("session-a", 3).expect("更旧水位"));
        assert!(store.is_unread("session-a", 5).expect("新消息"));

        store.mark_read("session-a", 2).expect("忽略旧水位");
        assert!(store.is_unread("session-a", 5).expect("旧写入不应推进"));
        store.mark_read("session-a", 5).expect("推进水位");
        assert!(!store.is_unread("session-a", 5).expect("推进后已读"));
    }

    #[test]
    fn same_or_lower_sequence_does_not_change_file() {
        let data = tempfile::tempdir().expect("数据根");
        let store = NativeAttentionStore::new(&paths(data.path())).expect("已读水位服务");
        store.mark_read("session-a", 7).expect("首次写入");
        let path = store.attention_path().expect("路径");
        let before = fs::read(&path).expect("已读水位文件");

        store.mark_read("session-a", 7).expect("相同水位");
        store.mark_read("session-a", 6).expect("更旧水位");
        assert_eq!(fs::read(&path).expect("已读水位文件"), before);
        let document = read_document(&path).expect("读取已读水位");
        assert_eq!(document.entries.len(), 1);
        assert_eq!(document.entries[0].read_seq, 7);
    }

    #[test]
    fn malformed_or_unknown_schema_is_rejected() {
        let data = tempfile::tempdir().expect("数据根");
        let store = NativeAttentionStore::new(&paths(data.path())).expect("已读水位服务");
        let path = store.attention_path().expect("路径");
        let malformed =
            br#"{"schema":"keencode/native-attention","version":1,"entries":[],"extra":true}"#;
        crate::storage::atomic_write_private_bytes(&path, malformed).expect("写入损坏文件");
        assert!(store.is_unread("session-a", 1).is_err());
    }

    #[test]
    fn oversized_document_is_rejected_before_use() {
        let data = tempfile::tempdir().expect("数据根");
        let store = NativeAttentionStore::new(&paths(data.path())).expect("已读水位服务");
        let path = store.attention_path().expect("路径");
        let document = AttentionDocument {
            schema: ATTENTION_SCHEMA.to_owned(),
            version: ATTENTION_VERSION,
            entries: (0..=MAX_ATTENTION_ENTRIES)
                .map(|index| AttentionEntry {
                    session_id: format!("session-{index}"),
                    read_seq: 1,
                    last_used: index as u64 + 1,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&document).expect("序列化超限文件");
        crate::storage::atomic_write_private_bytes(&path, &bytes).expect("写入超限文件");
        assert!(store.is_unread("session-a", 1).is_err());
    }

    #[test]
    fn invalid_session_ids_are_rejected() {
        let data = tempfile::tempdir().expect("数据根");
        let store = NativeAttentionStore::new(&paths(data.path())).expect("已读水位服务");
        assert!(store.mark_read(" session", 1).is_err());
        assert!(store.mark_read("session\n", 1).is_err());
        assert!(
            store
                .mark_read(&"x".repeat(MAX_SESSION_ID_BYTES + 1), 1)
                .is_err()
        );
    }

    #[test]
    fn full_store_evicts_the_least_recently_marked_watermark() {
        let data = tempfile::tempdir().expect("数据根");
        let store = NativeAttentionStore::new(&paths(data.path())).expect("已读水位服务");
        let path = store.attention_path().expect("路径");
        let document = AttentionDocument {
            schema: ATTENTION_SCHEMA.to_owned(),
            version: ATTENTION_VERSION,
            entries: (0..MAX_ATTENTION_ENTRIES)
                .map(|index| AttentionEntry {
                    session_id: format!("session-{index}"),
                    read_seq: 1,
                    last_used: index as u64 + 1,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&document).expect("序列化满容量文件");
        crate::storage::atomic_write_private_bytes(&path, &bytes).expect("写入满容量文件");

        store.mark_read("session-0", 2).expect("更新旧记录");
        store.mark_read("session-new", 1).expect("淘汰后写入新记录");
        let watermarks = store.read_watermarks().expect("批量读取水位");
        assert_eq!(watermarks.get("session-0"), Some(&2));
        assert_eq!(watermarks.get("session-new"), Some(&1));
        assert!(!watermarks.contains_key("session-1"));
    }
}
