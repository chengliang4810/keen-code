//! 原生 Composer 草稿持久域。
//!
//! 草稿是会话级 UI 事实，不属于 Session Journal，也不能通过复制整个
//! `NativeUiState` 来保存。这里使用有界的 typed DTO 和每会话文件：进程内缓存
//! 只用于减少重复读取，冷恢复始终从文件重新建立 `DraftFact`。

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::native_paths::NativePaths;
use crate::native_ui::model::{
    AttachmentFact, DraftFact, MentionCandidate, MentionKind, MentionQuery,
};

const DRAFT_SCHEMA: &str = "keencode/native-draft";
const DRAFT_VERSION: u32 = 1;
const DRAFT_DIRECTORY: &str = "native-drafts";
const MAX_DRAFT_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DRAFT_TEXT_BYTES: usize = 1024 * 1024;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_ATTACHMENTS: usize = 32;
const MAX_ATTACHMENT_ID_BYTES: usize = 256;
const MAX_ATTACHMENT_PATH_BYTES: usize = 4096;
const MAX_ATTACHMENT_NAME_BYTES: usize = 512;
const MAX_MEDIA_TYPE_BYTES: usize = 128;
const MAX_MENTION_QUERY_BYTES: usize = 4096;
const MAX_MENTION_CANDIDATES: usize = 256;
const MAX_MENTION_ID_BYTES: usize = 256;
const MAX_MENTION_LABEL_BYTES: usize = 512;
const MAX_CACHE_ENTRIES: usize = 64;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

static DRAFT_STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 只缓存最近使用的 typed 草稿；缓存容量和估算字节数都受硬上限约束。
#[derive(Default)]
struct DraftCache {
    entries: HashMap<String, CacheEntry>,
    order: VecDeque<String>,
    bytes: usize,
}

struct CacheEntry {
    draft: DraftFact,
    bytes: usize,
}

/// NativeHost 使用的会话级草稿服务。`NativePaths` 在构造时复制，避免后台任务
/// 重新发现用户目录后把同一窗口的草稿写到不同数据根。
#[derive(Clone)]
pub(crate) struct NativeDraftStore {
    paths: NativePaths,
    cache: Arc<Mutex<DraftCache>>,
}

impl NativeDraftStore {
    /// 创建草稿服务；目录延迟到第一次写入，空工作区启动不会产生文件。
    pub(crate) fn new(paths: &NativePaths) -> Result<Self> {
        if paths.data_root.as_os_str().is_empty() {
            bail!("草稿数据根不能为空");
        }
        Ok(Self {
            paths: paths.clone(),
            cache: Arc::new(Mutex::new(DraftCache::default())),
        })
    }

    /// 读取当前草稿；没有持久文件时返回 typed 空草稿。
    pub(crate) fn load(&self, session_id: &str) -> Result<DraftFact> {
        validate_session_id(session_id)?;
        if let Some(draft) = self.cache_get(session_id)? {
            return Ok(draft);
        }
        let draft = self.load_uncached(session_id)?;
        self.cache_put(session_id, draft.clone())?;
        Ok(draft)
    }

    /// 保存一个会话草稿，并在成功替换文件后更新缓存。
    pub(crate) fn set(&self, session_id: &str, draft: &DraftFact) -> Result<()> {
        validate_session_id(session_id)?;
        validate_draft(draft)?;
        let path = self.draft_path(session_id)?;
        let bytes = encode_draft(session_id, draft)?;
        let _guard = draft_store_guard()?;
        let _file_lock = lock_file(&path)?;
        crate::storage::atomic_write_private_bytes(&path, &bytes)
            .with_context(|| format!("保存会话草稿失败：{session_id}"))?;
        drop(_file_lock);
        drop(_guard);
        self.cache_put(session_id, draft.clone())?;
        Ok(())
    }

    /// 清空草稿文件。删除是目录级原子操作，旧文件不会先被截断成半个 JSON。
    pub(crate) fn clear(&self, session_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        let path = self.draft_path(session_id)?;
        let _guard = draft_store_guard()?;
        let _file_lock = lock_file(&path)?;
        remove_private_file_if_present(&path)?;
        drop(_file_lock);
        drop(_guard);
        self.cache_remove(session_id)?;
        Ok(())
    }

    /// 冷恢复绕过进程内缓存，验证并读取磁盘上的最新 typed 草稿。
    pub(crate) fn cold_restore(&self, session_id: &str) -> Result<DraftFact> {
        validate_session_id(session_id)?;
        self.cache_remove(session_id)?;
        let draft = self.load_uncached(session_id)?;
        self.cache_put(session_id, draft.clone())?;
        Ok(draft)
    }

    /// 测试和 Host 关闭路径使用；只丢弃易失缓存，不触碰持久文件。
    pub(crate) fn clear_cache(&self) -> Result<()> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("草稿缓存锁已损坏"))?;
        *cache = DraftCache::default();
        Ok(())
    }

    fn load_uncached(&self, session_id: &str) -> Result<DraftFact> {
        let path = self.draft_path(session_id)?;
        let Some(bytes) =
            crate::storage::read_private_bytes_bounded(&path, MAX_DRAFT_FILE_BYTES, "会话草稿")?
        else {
            return Ok(DraftFact::default());
        };
        let stored: StoredDraft = serde_json::from_slice(&bytes)
            .with_context(|| format!("会话草稿格式无效：{session_id}"))?;
        stored.into_draft(session_id)
    }

    fn draft_path(&self, session_id: &str) -> Result<PathBuf> {
        let root = crate::storage::root_dir(&self.paths)?;
        Ok(root
            .join(DRAFT_DIRECTORY)
            .join(format!("{}.json", session_digest(session_id))))
    }

    fn cache_get(&self, session_id: &str) -> Result<Option<DraftFact>> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("草稿缓存锁已损坏"))?;
        let Some(entry) = cache.entries.remove(session_id) else {
            return Ok(None);
        };
        cache.order.retain(|key| key != session_id);
        cache.order.push_back(session_id.to_owned());
        let draft = entry.draft.clone();
        cache.entries.insert(session_id.to_owned(), entry);
        Ok(Some(draft))
    }

    fn cache_put(&self, session_id: &str, draft: DraftFact) -> Result<()> {
        let bytes = encoded_size(&draft)?;
        if bytes > MAX_CACHE_BYTES {
            return Ok(());
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("草稿缓存锁已损坏"))?;
        if let Some(old) = cache.entries.remove(session_id) {
            cache.bytes = cache.bytes.saturating_sub(old.bytes);
        }
        cache.order.retain(|key| key != session_id);
        while cache.entries.len() >= MAX_CACHE_ENTRIES
            || cache.bytes.saturating_add(bytes) > MAX_CACHE_BYTES
        {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            if let Some(old) = cache.entries.remove(&oldest) {
                cache.bytes = cache.bytes.saturating_sub(old.bytes);
            }
        }
        cache.order.push_back(session_id.to_owned());
        cache.bytes = cache.bytes.saturating_add(bytes);
        cache
            .entries
            .insert(session_id.to_owned(), CacheEntry { draft, bytes });
        Ok(())
    }

    fn cache_remove(&self, session_id: &str) -> Result<()> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("草稿缓存锁已损坏"))?;
        if let Some(entry) = cache.entries.remove(session_id) {
            cache.bytes = cache.bytes.saturating_sub(entry.bytes);
        }
        cache.order.retain(|key| key != session_id);
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredDraft {
    schema: String,
    version: u32,
    session_id: String,
    text: String,
    attachments: Vec<StoredAttachment>,
    mention_query: Option<StoredMentionQuery>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredAttachment {
    attachment_id: String,
    path: String,
    file_name: String,
    media_type: String,
    bytes: u64,
    image: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredMentionQuery {
    trigger: String,
    query: String,
    candidates: Vec<StoredMentionCandidate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StoredMentionKind {
    File,
    Session,
    Agent,
    Symbol,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredMentionCandidate {
    id: String,
    label: String,
    kind: StoredMentionKind,
}

impl StoredDraft {
    fn from_draft(session_id: &str, draft: &DraftFact) -> Self {
        Self {
            schema: DRAFT_SCHEMA.to_owned(),
            version: DRAFT_VERSION,
            session_id: session_id.to_owned(),
            text: draft.text.clone(),
            attachments: draft
                .attachments
                .iter()
                .map(|attachment| StoredAttachment {
                    attachment_id: attachment.attachment_id.clone(),
                    path: attachment.path.clone(),
                    file_name: attachment.file_name.clone(),
                    media_type: attachment.media_type.clone(),
                    bytes: attachment.bytes,
                    image: attachment.image,
                })
                .collect(),
            mention_query: draft
                .mention_query
                .as_ref()
                .map(|query| StoredMentionQuery {
                    trigger: query.trigger.to_string(),
                    query: query.query.clone(),
                    candidates: query
                        .candidates
                        .iter()
                        .map(|candidate| StoredMentionCandidate {
                            id: candidate.id.clone(),
                            label: candidate.label.clone(),
                            kind: match candidate.kind {
                                MentionKind::File => StoredMentionKind::File,
                                MentionKind::Session => StoredMentionKind::Session,
                                MentionKind::Agent => StoredMentionKind::Agent,
                                MentionKind::Symbol => StoredMentionKind::Symbol,
                            },
                        })
                        .collect(),
                }),
        }
    }

    fn into_draft(self, requested_session_id: &str) -> Result<DraftFact> {
        if self.schema != DRAFT_SCHEMA || self.version != DRAFT_VERSION {
            bail!("会话草稿 schema 或版本不受支持");
        }
        if self.session_id != requested_session_id {
            bail!("会话草稿与请求的 session 不匹配");
        }
        let draft = DraftFact {
            text: self.text,
            attachments: self
                .attachments
                .into_iter()
                .map(|attachment| {
                    Ok(AttachmentFact {
                        attachment_id: attachment.attachment_id,
                        path: attachment.path,
                        file_name: attachment.file_name,
                        media_type: attachment.media_type,
                        bytes: attachment.bytes,
                        image: attachment.image,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            mention_query: self
                .mention_query
                .map(|query| {
                    let trigger = query.trigger.chars().collect::<Vec<_>>();
                    if trigger.len() != 1 {
                        bail!("草稿 mention trigger 必须是单个字符");
                    }
                    Ok(MentionQuery {
                        trigger: trigger[0],
                        query: query.query,
                        candidates: query
                            .candidates
                            .into_iter()
                            .map(|candidate| MentionCandidate {
                                id: candidate.id,
                                label: candidate.label,
                                kind: match candidate.kind {
                                    StoredMentionKind::File => MentionKind::File,
                                    StoredMentionKind::Session => MentionKind::Session,
                                    StoredMentionKind::Agent => MentionKind::Agent,
                                    StoredMentionKind::Symbol => MentionKind::Symbol,
                                },
                            })
                            .collect(),
                    })
                })
                .transpose()?,
        };
        validate_draft(&draft)?;
        Ok(draft)
    }
}

fn encode_draft(session_id: &str, draft: &DraftFact) -> Result<Vec<u8>> {
    let stored = StoredDraft::from_draft(session_id, draft);
    let bytes = serde_json::to_vec(&stored).context("序列化会话草稿失败")?;
    if bytes.len() as u64 > MAX_DRAFT_FILE_BYTES {
        bail!("会话草稿超过 {MAX_DRAFT_FILE_BYTES} 字节");
    }
    Ok(bytes)
}

fn encoded_size(draft: &DraftFact) -> Result<usize> {
    Ok(serde_json::to_vec(&StoredDraft::from_draft("cache", draft))?.len())
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

fn validate_draft(draft: &DraftFact) -> Result<()> {
    if draft.text.len() > MAX_DRAFT_TEXT_BYTES || draft.text.chars().any(forbidden_text_control) {
        bail!("草稿文本超过限制或包含非法控制字符");
    }
    if draft.attachments.len() > MAX_ATTACHMENTS {
        bail!("草稿附件数量超过 {MAX_ATTACHMENTS}");
    }
    for attachment in &draft.attachments {
        bounded_text(
            &attachment.attachment_id,
            MAX_ATTACHMENT_ID_BYTES,
            "attachmentId",
        )?;
        bounded_text(&attachment.path, MAX_ATTACHMENT_PATH_BYTES, "附件路径")?;
        bounded_text(
            &attachment.file_name,
            MAX_ATTACHMENT_NAME_BYTES,
            "附件文件名",
        )?;
        bounded_text(&attachment.media_type, MAX_MEDIA_TYPE_BYTES, "附件媒体类型")?;
    }
    if let Some(query) = &draft.mention_query {
        if query.trigger.is_control() {
            bail!("草稿 mention trigger 不能是控制字符");
        }
        bounded_text(&query.query, MAX_MENTION_QUERY_BYTES, "mention 查询")?;
        if query.candidates.len() > MAX_MENTION_CANDIDATES {
            bail!("草稿 mention 候选超过 {MAX_MENTION_CANDIDATES}");
        }
        for candidate in &query.candidates {
            bounded_text(&candidate.id, MAX_MENTION_ID_BYTES, "mention 候选 ID")?;
            bounded_text(
                &candidate.label,
                MAX_MENTION_LABEL_BYTES,
                "mention 候选名称",
            )?;
        }
    }
    Ok(())
}

fn bounded_text(value: &str, max_bytes: usize, field: &str) -> Result<()> {
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        bail!("{field} 超过 {max_bytes} 字节或包含控制字符");
    }
    Ok(())
}

fn forbidden_text_control(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
}

fn session_digest(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn draft_store_guard() -> Result<std::sync::MutexGuard<'static, ()>> {
    DRAFT_STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("草稿持久化锁已损坏"))
}

/// 每会话锁文件避免两个桌面进程同时清理或覆盖同一个草稿时产生未定义顺序。
fn lock_file(path: &Path) -> Result<Option<fs::File>> {
    use fs2::FileExt;
    let Some(parent) = path.parent() else {
        bail!("草稿路径缺少父目录");
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("创建草稿目录失败：{}", parent.display()))?;
    let lock_path = path.with_extension("json.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        // 跨进程锁文件只负责互斥，不作为草稿正文写入或截断。
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("打开草稿锁失败：{}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("锁定草稿失败：{}", lock_path.display()))?;
    Ok(Some(file))
}

fn remove_private_file_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("草稿路径不是普通文件：{}", path.display());
            }
            fs::remove_file(path).with_context(|| format!("清理草稿失败：{}", path.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("检查草稿失败：{}", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn paths(root: &Path) -> NativePaths {
        NativePaths::new(root.to_owned(), root.to_owned(), root.join("Documents"))
    }

    #[test]
    fn draft_round_trips_through_typed_store_and_cold_restore() {
        let root = tempfile::tempdir().expect("临时目录");
        let store = NativeDraftStore::new(&paths(root.path())).expect("草稿服务");
        let session_id = "session-round-trip";
        let draft = DraftFact {
            text: "hello".into(),
            attachments: vec![AttachmentFact {
                attachment_id: "a".into(),
                path: "C:/tmp/a.txt".into(),
                file_name: "a.txt".into(),
                media_type: "text/plain".into(),
                bytes: 5,
                image: false,
            }],
            mention_query: Some(MentionQuery {
                trigger: '@',
                query: "ke".into(),
                candidates: vec![MentionCandidate {
                    id: "agent".into(),
                    label: "Keen".into(),
                    kind: MentionKind::Agent,
                }],
            }),
        };
        store.set(session_id, &draft).expect("写入草稿");
        assert_eq!(store.load(session_id).expect("缓存读取"), draft);
        assert_eq!(store.cold_restore(session_id).expect("冷恢复"), draft);
        store.clear(session_id).expect("清理草稿");
        assert_eq!(
            store.cold_restore(session_id).expect("空草稿恢复"),
            DraftFact::default()
        );
    }

    #[test]
    fn session_id_is_hashed_and_cannot_escape_draft_directory() {
        let root = tempfile::tempdir().expect("临时目录");
        let store = NativeDraftStore::new(&paths(root.path())).expect("草稿服务");
        let path = store.draft_path("../../outside").expect("哈希路径");
        let draft_directory = root.path().join(DRAFT_DIRECTORY);
        assert_eq!(path.parent(), Some(draft_directory.as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".json")
        );
    }

    #[test]
    fn oversized_draft_is_rejected_before_persisting() {
        let root = tempfile::tempdir().expect("临时目录");
        let store = NativeDraftStore::new(&paths(root.path())).expect("草稿服务");
        let draft = DraftFact {
            text: "x".repeat(MAX_DRAFT_TEXT_BYTES + 1),
            ..DraftFact::default()
        };
        assert!(store.set("session", &draft).is_err());
        assert!(!root.path().join(DRAFT_DIRECTORY).exists());
    }

    #[test]
    fn invalid_persisted_schema_is_not_silently_treated_as_empty() {
        let root = tempfile::tempdir().expect("临时目录");
        let store = NativeDraftStore::new(&paths(root.path())).expect("草稿服务");
        let path = store.draft_path("session").expect("草稿路径");
        fs::create_dir_all(path.parent().unwrap()).expect("目录");
        crate::storage::atomic_write_private_bytes(&path, br#"{}"#).expect("损坏文件");
        assert!(store.cold_restore("session").is_err());
    }

    #[test]
    fn digest_is_stable() {
        assert_eq!(session_digest("session"), session_digest("session"));
        assert_ne!(session_digest("session"), session_digest("other"));
        assert_eq!(PathBuf::from(session_digest("x")).components().count(), 1);
    }
}
