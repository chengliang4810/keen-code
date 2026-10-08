//! 原生侧栏的持久分组域。
//!
//! Session/Journal 只负责会话事实；本模块只保存项目范围内的分组成员投影。
//! 变更通过项目级 revision CAS 提交，operation ID 会随文件一起保存，因此重试
//! 不依赖进程内集合，也不会在冷启动后重复推进 revision。

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::native_paths::NativePaths;

const SIDEBAR_SCHEMA: &str = "keencode/native-sidebar";
const SIDEBAR_VERSION: u32 = 1;
const SIDEBAR_FILE: &str = "native-sidebar.json";
const SIDEBAR_LOCK_FILE: &str = "native-sidebar.json.lock";
const MAX_SIDEBAR_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROJECTS: usize = 256;
const MAX_GROUPS_PER_PROJECT: usize = 256;
const MAX_SESSION_IDS_PER_GROUP: usize = 2_000;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_GROUP_ID_BYTES: usize = 256;
const MAX_GROUP_TITLE_BYTES: usize = 512;
const MAX_OPERATION_ID_BYTES: usize = 256;
const MAX_OPERATION_RECORDS: usize = 256;

static SIDEBAR_STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// UI 侧栏所需的分组事实；Session 正文、标题和状态不在此复制。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SidebarGroupFact {
    pub group_id: String,
    pub project_key: String,
    pub root_path: String,
    pub title: String,
    pub session_ids: Vec<String>,
}

/// 供宿主映射到 `WorkspacePage.groups` 的项目分组快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SidebarSnapshot {
    pub project_key: String,
    pub root_path: String,
    pub revision: u64,
    pub groups: Vec<SidebarGroupFact>,
}

/// 分组写入回执。重试同一 operation ID 时 `replayed` 为 true，revision 不再推进。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SidebarMutation {
    pub snapshot: SidebarSnapshot,
    pub replayed: bool,
}

/// 侧栏分组持久服务；路径在构造时固定，后台任务不重新发现数据根。
#[derive(Clone)]
pub(crate) struct NativeSidebarStore {
    paths: NativePaths,
}

impl NativeSidebarStore {
    /// 创建分组服务；目录和文件延迟到首次读取/提交。
    pub(crate) fn new(paths: &NativePaths) -> Result<Self> {
        if paths.data_root.as_os_str().is_empty() {
            bail!("侧栏数据根不能为空");
        }
        Ok(Self {
            paths: paths.clone(),
        })
    }

    /// 读取一个已授权项目的分组快照；不存在时返回 revision 0 的空快照。
    pub(crate) fn load(&self, project_key: &str) -> Result<SidebarSnapshot> {
        let project = self.authorize_project(project_key)?;
        self.with_locked_document(|document| snapshot_for(document, &project))
    }

    /// 提交一个项目范围的分组成员替换。
    ///
    /// `project_key` 可以是已登记项目 ID 或项目根路径；两者均先通过现有
    /// `authorize_stored_root` 解析为规范根。空 `group_id` 表示把给定会话移出
    /// 所有分组。revision 校验和 operation 去重在同一跨进程文件锁内完成。
    pub(crate) fn group_sessions(
        &self,
        project_key: &str,
        group_id: &str,
        session_ids: &[String],
        expected_revision: u64,
        operation_id: &str,
    ) -> Result<SidebarMutation> {
        let project = self.authorize_project(project_key)?;
        validate_group_id(group_id, true)?;
        validate_session_ids(session_ids)?;
        validate_operation_id(operation_id)?;
        let request_digest = request_digest(
            &project.project_key,
            group_id,
            session_ids,
            expected_revision,
        )?;

        self.with_locked_document_mut(|document| {
            let index = document
                .projects
                .iter()
                .position(|record| record.project_key == project.project_key);
            if let Some(index) = index {
                let record = &mut document.projects[index];
                if record.root_path != project.root_path {
                    bail!("侧栏项目根目录与当前授权目录不一致");
                }
                validate_project_record(record)?;
                if let Some(operation) = record
                    .operations
                    .iter()
                    .find(|operation| operation.operation_id == operation_id)
                {
                    if operation.request_digest != request_digest {
                        bail!("operationId 已用于另一条侧栏分组请求");
                    }
                    return Ok(SidebarMutation {
                        snapshot: snapshot_from_operation(&project, operation),
                        replayed: true,
                    });
                }
                if record.revision != expected_revision {
                    bail!(
                        "侧栏分组 revision 冲突：expected={}, current={}",
                        expected_revision,
                        record.revision
                    );
                }
                let changed = apply_group_membership(record, group_id, session_ids)?;
                if changed {
                    record.revision = record
                        .revision
                        .checked_add(1)
                        .context("侧栏分组 revision 溢出")?;
                }
                record.operations.push(SidebarOperationRecord {
                    operation_id: operation_id.to_owned(),
                    request_digest,
                    revision: record.revision,
                    groups: record.groups.clone(),
                });
                trim_operations(&mut record.operations);
                let snapshot = snapshot_for_record(record, &project);
                return Ok(SidebarMutation {
                    snapshot,
                    replayed: false,
                });
            }

            if expected_revision != 0 {
                bail!(
                    "侧栏分组 revision 冲突：expected={}, current=0",
                    expected_revision
                );
            }
            if document.projects.len() >= MAX_PROJECTS {
                bail!("侧栏项目数量超过 {MAX_PROJECTS}");
            }
            let mut record = SidebarProjectRecord {
                project_key: project.project_key.clone(),
                root_path: project.root_path.clone(),
                revision: 0,
                groups: Vec::new(),
                operations: Vec::new(),
            };
            let changed = apply_group_membership(&mut record, group_id, session_ids)?;
            if changed {
                record.revision = 1;
            }
            record.operations.push(SidebarOperationRecord {
                operation_id: operation_id.to_owned(),
                request_digest,
                revision: record.revision,
                groups: record.groups.clone(),
            });
            trim_operations(&mut record.operations);
            let snapshot = snapshot_for_record(&record, &project);
            document.projects.push(record);
            Ok(SidebarMutation {
                snapshot,
                replayed: false,
            })
        })
    }

    fn authorize_project(&self, project_key: &str) -> Result<AuthorizedProject> {
        validate_project_key(project_key)?;
        let root = match crate::session_commands::authorize_stored_root(&self.paths, project_key) {
            Ok(root) => root,
            Err(path_error) => {
                let records = crate::workspace::project_records(&self.paths)
                    .map_err(|error| anyhow::anyhow!(error))?;
                let record = records
                    .into_iter()
                    .find(|record| record.id == project_key)
                    .ok_or_else(|| anyhow::anyhow!(path_error))?;
                crate::session_commands::authorize_stored_root(&self.paths, &record.path)
                    .map_err(|error| anyhow::anyhow!(error))?
            }
        };
        let canonical = crate::path_utils::path_to_frontend(&root);
        Ok(AuthorizedProject {
            project_key: canonical.clone(),
            root_path: canonical,
        })
    }

    fn sidebar_path(&self) -> Result<PathBuf> {
        Ok(crate::storage::root_dir(&self.paths)?.join(SIDEBAR_FILE))
    }

    fn with_locked_document<T>(
        &self,
        operation: impl FnOnce(&SidebarDocument) -> Result<T>,
    ) -> Result<T> {
        let path = self.sidebar_path()?;
        let _process_guard = sidebar_store_guard()?;
        let _file_lock = open_sidebar_lock(&path)?;
        let document = read_document(&path)?;
        operation(&document)
    }

    fn with_locked_document_mut<T>(
        &self,
        operation: impl FnOnce(&mut SidebarDocument) -> Result<T>,
    ) -> Result<T> {
        let path = self.sidebar_path()?;
        let _process_guard = sidebar_store_guard()?;
        let _file_lock = open_sidebar_lock(&path)?;
        let mut document = read_document(&path)?;
        let result = operation(&mut document)?;
        validate_document(&document)?;
        write_document(&path, &document)?;
        Ok(result)
    }
}

#[derive(Clone)]
struct AuthorizedProject {
    project_key: String,
    root_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SidebarDocument {
    schema: String,
    version: u32,
    projects: Vec<SidebarProjectRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SidebarProjectRecord {
    project_key: String,
    root_path: String,
    revision: u64,
    groups: Vec<SidebarGroupRecord>,
    operations: Vec<SidebarOperationRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SidebarGroupRecord {
    group_id: String,
    title: String,
    session_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SidebarOperationRecord {
    operation_id: String,
    request_digest: String,
    revision: u64,
    groups: Vec<SidebarGroupRecord>,
}

impl Default for SidebarDocument {
    fn default() -> Self {
        Self {
            schema: SIDEBAR_SCHEMA.to_owned(),
            version: SIDEBAR_VERSION,
            projects: Vec::new(),
        }
    }
}

fn read_document(path: &Path) -> Result<SidebarDocument> {
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(path, MAX_SIDEBAR_FILE_BYTES, "原生侧栏分组")?
    else {
        return Ok(SidebarDocument::default());
    };
    let document: SidebarDocument = serde_json::from_slice(&bytes)
        .with_context(|| format!("原生侧栏分组格式无效：{}", path.display()))?;
    validate_document(&document)?;
    Ok(document)
}

fn write_document(path: &Path, document: &SidebarDocument) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(document).context("序列化原生侧栏分组失败")?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_SIDEBAR_FILE_BYTES {
        bail!("原生侧栏分组超过 {MAX_SIDEBAR_FILE_BYTES} 字节");
    }
    crate::storage::atomic_write_private_bytes(path, &bytes)
        .with_context(|| format!("写入原生侧栏分组失败：{}", path.display()))
}

fn validate_document(document: &SidebarDocument) -> Result<()> {
    if document.schema != SIDEBAR_SCHEMA || document.version != SIDEBAR_VERSION {
        bail!("原生侧栏分组 schema 或版本不受支持");
    }
    if document.projects.len() > MAX_PROJECTS {
        bail!("侧栏项目数量超过 {MAX_PROJECTS}");
    }
    let mut projects = HashSet::new();
    for project in &document.projects {
        validate_project_record(project)?;
        if !projects.insert(project.project_key.as_str()) {
            bail!("原生侧栏分组包含重复项目");
        }
    }
    Ok(())
}

fn validate_project_record(record: &SidebarProjectRecord) -> Result<()> {
    validate_project_key(&record.project_key)?;
    validate_project_key(&record.root_path)?;
    if record.groups.len() > MAX_GROUPS_PER_PROJECT {
        bail!("项目分组数量超过 {MAX_GROUPS_PER_PROJECT}");
    }
    let mut group_ids = HashSet::new();
    let mut session_ids = HashSet::new();
    for group in &record.groups {
        validate_group_id(&group.group_id, false)?;
        bounded_text(&group.title, MAX_GROUP_TITLE_BYTES, "分组标题")?;
        if !group_ids.insert(group.group_id.as_str()) {
            bail!("项目分组 ID 重复");
        }
        if group.session_ids.len() > MAX_SESSION_IDS_PER_GROUP {
            bail!("分组会话数量超过 {MAX_SESSION_IDS_PER_GROUP}");
        }
        let mut local_sessions = HashSet::new();
        for session_id in &group.session_ids {
            validate_session_id(session_id)?;
            if !local_sessions.insert(session_id.as_str())
                || !session_ids.insert(session_id.as_str())
            {
                bail!("一个会话不能重复出现在侧栏分组中");
            }
        }
    }
    if record.operations.len() > MAX_OPERATION_RECORDS {
        bail!("侧栏 operation 去重记录超过 {MAX_OPERATION_RECORDS}");
    }
    let mut operation_ids = HashSet::new();
    for operation in &record.operations {
        validate_operation_id(&operation.operation_id)?;
        if operation.request_digest.len() != 64
            || !operation
                .request_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("侧栏 operation digest 格式无效");
        }
        if !operation_ids.insert(operation.operation_id.as_str()) {
            bail!("侧栏 operation ID 重复");
        }
        if operation.groups.len() > MAX_GROUPS_PER_PROJECT {
            bail!("侧栏 operation 快照分组数量超过限制");
        }
        validate_group_records(&operation.groups)?;
    }
    Ok(())
}

fn validate_group_records(groups: &[SidebarGroupRecord]) -> Result<()> {
    let mut group_ids = HashSet::new();
    let mut session_ids = HashSet::new();
    for group in groups {
        validate_group_id(&group.group_id, false)?;
        bounded_text(&group.title, MAX_GROUP_TITLE_BYTES, "分组标题")?;
        if !group_ids.insert(group.group_id.as_str()) {
            bail!("operation 快照包含重复分组");
        }
        if group.session_ids.len() > MAX_SESSION_IDS_PER_GROUP {
            bail!("operation 快照会话数量超过限制");
        }
        for session_id in &group.session_ids {
            validate_session_id(session_id)?;
            if !session_ids.insert(session_id.as_str()) {
                bail!("operation 快照重复包含会话");
            }
        }
    }
    Ok(())
}

fn snapshot_for(
    document: &SidebarDocument,
    project: &AuthorizedProject,
) -> Result<SidebarSnapshot> {
    document
        .projects
        .iter()
        .find(|record| record.project_key == project.project_key)
        .map(|record| {
            if record.root_path != project.root_path {
                bail!("侧栏项目根目录与当前授权目录不一致");
            }
            Ok(snapshot_for_record(record, project))
        })
        .unwrap_or_else(|| {
            Ok(SidebarSnapshot {
                project_key: project.project_key.clone(),
                root_path: project.root_path.clone(),
                revision: 0,
                groups: Vec::new(),
            })
        })
}

fn snapshot_for_record(
    record: &SidebarProjectRecord,
    project: &AuthorizedProject,
) -> SidebarSnapshot {
    SidebarSnapshot {
        project_key: project.project_key.clone(),
        root_path: project.root_path.clone(),
        revision: record.revision,
        groups: record
            .groups
            .iter()
            .map(|group| SidebarGroupFact {
                group_id: group.group_id.clone(),
                project_key: project.project_key.clone(),
                root_path: project.root_path.clone(),
                title: group.title.clone(),
                session_ids: group.session_ids.clone(),
            })
            .collect(),
    }
}

fn snapshot_from_operation(
    project: &AuthorizedProject,
    operation: &SidebarOperationRecord,
) -> SidebarSnapshot {
    SidebarSnapshot {
        project_key: project.project_key.clone(),
        root_path: project.root_path.clone(),
        revision: operation.revision,
        groups: operation
            .groups
            .iter()
            .map(|group| SidebarGroupFact {
                group_id: group.group_id.clone(),
                project_key: project.project_key.clone(),
                root_path: project.root_path.clone(),
                title: group.title.clone(),
                session_ids: group.session_ids.clone(),
            })
            .collect(),
    }
}

fn apply_group_membership(
    record: &mut SidebarProjectRecord,
    group_id: &str,
    session_ids: &[String],
) -> Result<bool> {
    let before = record.groups.clone();
    let selected: HashSet<&str> = session_ids.iter().map(String::as_str).collect();
    for group in &mut record.groups {
        group
            .session_ids
            .retain(|id| !selected.contains(id.as_str()));
    }
    record.groups.retain(|group| !group.session_ids.is_empty());
    if !group_id.is_empty() && !session_ids.is_empty() {
        record.groups.push(SidebarGroupRecord {
            group_id: group_id.to_owned(),
            title: group_id.to_owned(),
            session_ids: session_ids.to_vec(),
        });
        // 同一 group_id 可能原本有成员；合并而非产生两个同名记录。
        let mut merged = Vec::with_capacity(record.groups.len());
        for group in record.groups.drain(..) {
            if let Some(existing) = merged
                .iter_mut()
                .find(|item: &&mut SidebarGroupRecord| item.group_id == group.group_id)
            {
                existing.session_ids.extend(group.session_ids);
            } else {
                merged.push(group);
            }
        }
        record.groups = merged;
    }
    Ok(record.groups != before)
}

fn trim_operations(operations: &mut Vec<SidebarOperationRecord>) {
    if operations.len() > MAX_OPERATION_RECORDS {
        let remove = operations.len() - MAX_OPERATION_RECORDS;
        operations.drain(..remove);
    }
}

fn request_digest(
    project_key: &str,
    group_id: &str,
    session_ids: &[String],
    expected_revision: u64,
) -> Result<String> {
    #[derive(Serialize)]
    struct Request<'a> {
        project_key: &'a str,
        group_id: &'a str,
        session_ids: &'a [String],
        expected_revision: u64,
    }
    let bytes = serde_json::to_vec(&Request {
        project_key,
        group_id,
        session_ids,
        expected_revision,
    })?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_project_key(project_key: &str) -> Result<()> {
    if project_key.is_empty() || project_key.trim() != project_key {
        bail!("项目标识不能为空或包含首尾空白");
    }
    if project_key.len() > 4096 || project_key.chars().any(char::is_control) {
        bail!("项目标识超过长度限制或包含控制字符");
    }
    Ok(())
}

fn validate_group_id(group_id: &str, allow_empty: bool) -> Result<()> {
    if group_id.is_empty() && allow_empty {
        return Ok(());
    }
    bounded_text(group_id, MAX_GROUP_ID_BYTES, "分组 ID")?;
    if group_id.is_empty() {
        bail!("分组 ID 不能为空");
    }
    Ok(())
}

fn validate_session_ids(session_ids: &[String]) -> Result<()> {
    if session_ids.len() > MAX_SESSION_IDS_PER_GROUP {
        bail!("分组会话数量超过 {MAX_SESSION_IDS_PER_GROUP}");
    }
    let mut unique = HashSet::new();
    for session_id in session_ids {
        validate_session_id(session_id)?;
        if !unique.insert(session_id.as_str()) {
            bail!("分组请求重复包含同一会话");
        }
    }
    Ok(())
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

fn validate_operation_id(operation_id: &str) -> Result<()> {
    if operation_id.is_empty()
        || operation_id.trim() != operation_id
        || operation_id.len() > MAX_OPERATION_ID_BYTES
        || operation_id.chars().any(char::is_control)
    {
        bail!("operationId 不能为空、不能含首尾空白或控制字符");
    }
    Ok(())
}

fn bounded_text(value: &str, max_bytes: usize, field: &str) -> Result<()> {
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        bail!("{field} 超过 {max_bytes} 字节或包含控制字符");
    }
    Ok(())
}

fn sidebar_store_guard() -> Result<std::sync::MutexGuard<'static, ()>> {
    SIDEBAR_STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("侧栏持久化锁已损坏"))
}

fn open_sidebar_lock(path: &Path) -> Result<fs::File> {
    let parent = path.parent().context("侧栏文件路径缺少父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建侧栏数据目录失败：{}", parent.display()))?;
    let lock_path = parent.join(SIDEBAR_LOCK_FILE);
    let file = fs::OpenOptions::new()
        .create(true)
        // 保留锁文件，跨进程读写共享同一个锁对象。
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("打开侧栏锁失败：{}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("锁定侧栏文件失败：{}", lock_path.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_paths::NativePaths;
    use std::path::Path;

    fn paths(data_root: &Path) -> NativePaths {
        NativePaths::new(
            data_root.to_owned(),
            data_root.to_owned(),
            data_root.join("Documents"),
        )
    }

    fn authorized_store() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        NativeSidebarStore,
        String,
    ) {
        let data = tempfile::tempdir().expect("数据根");
        let project_root = tempfile::tempdir().expect("项目根");
        let native_paths = paths(data.path());
        let project = crate::workspace::project_create(
            &native_paths,
            Some(project_root.path().to_string_lossy().into_owned()),
            "Test Project".into(),
            false,
        )
        .expect("登记项目");
        let store = NativeSidebarStore::new(&native_paths).expect("侧栏服务");
        (data, project_root, store, project.path)
    }

    #[test]
    fn grouping_uses_revision_cas_and_persisted_operation_deduplication() {
        let (_data, _project_root, store, project_key) = authorized_store();
        let session_ids = vec!["session-a".to_owned(), "session-b".to_owned()];
        let first = store
            .group_sessions(&project_key, "group-a", &session_ids, 0, "op-a")
            .expect("首次分组");
        assert_eq!(first.snapshot.revision, 1);
        assert!(!first.replayed);
        assert_eq!(first.snapshot.groups[0].session_ids, session_ids);

        let replay = store
            .group_sessions(&project_key, "group-a", &session_ids, 0, "op-a")
            .expect("重试去重");
        assert_eq!(replay.snapshot.revision, 1);
        assert!(replay.replayed);

        assert!(
            store
                .group_sessions(&project_key, "group-b", &["session-c".into()], 0, "op-b")
                .is_err()
        );
        let fresh = store.load(&project_key).expect("读取分组");
        assert_eq!(fresh.revision, 1);
        assert_eq!(fresh.groups.len(), 1);
    }

    #[test]
    fn grouping_can_remove_members_without_leaving_empty_groups() {
        let (_data, _project_root, store, project_key) = authorized_store();
        store
            .group_sessions(&project_key, "group-a", &["session-a".into()], 0, "op-a")
            .expect("首次分组");
        let result = store
            .group_sessions(&project_key, "", &["session-a".into()], 1, "op-b")
            .expect("移出分组");
        assert_eq!(result.snapshot.revision, 2);
        assert!(result.snapshot.groups.is_empty());
    }

    #[test]
    fn unauthorized_project_is_rejected_before_store_access() {
        let data = tempfile::tempdir().expect("数据根");
        let project_root = tempfile::tempdir().expect("未登记根");
        let store = NativeSidebarStore::new(&paths(data.path())).expect("侧栏服务");
        assert!(store.load(&project_root.path().to_string_lossy()).is_err());
    }

    #[test]
    fn malformed_sidebar_file_is_rejected() {
        let (data, _project_root, store, project_key) = authorized_store();
        let path = data.path().join(SIDEBAR_FILE);
        crate::storage::atomic_write_private_bytes(&path, br#"{}"#).expect("损坏文件");
        assert!(store.load(&project_key).is_err());
    }

    #[test]
    fn operation_digest_is_stable_for_same_request() {
        let ids = vec!["a".to_owned()];
        assert_eq!(
            request_digest("project", "group", &ids, 4).expect("digest"),
            request_digest("project", "group", &ids, 4).expect("digest")
        );
        assert_ne!(
            request_digest("project", "group", &ids, 4).expect("digest"),
            request_digest("project", "group", &ids, 5).expect("digest")
        );
    }
}
