use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::atomic::{
    BoundedJson, BoundedRead, ExclusiveFileLock, atomic_write, ensure_regular_file_or_absent,
    exclusive_lock, prepare_root, read_file_bounded, secure_child_dir, serialize_json_bounded,
};
use crate::canonical::canonical_json_sha256;
use crate::{AgentId, ArtifactRef, ArtifactStore, ArtifactUse, ResourceError, ScopeId, SessionId};

/// 当前本地 Memory 文档 schema。
const MEMORY_SCHEMA: &str = "keencode/memory";
/// 当前本地 Goal 文档 schema。
const GOAL_SCHEMA: &str = "keencode/goal";
/// 当前本地 Plan 沙箱文档 schema。
const PLAN_SCHEMA: &str = "keencode/plan";
/// 当前本地 Memory 文档格式版本。
const MEMORY_DOCUMENT_VERSION: u32 = 1;
/// 当前本地 Goal 文档格式版本。
const GOAL_DOCUMENT_VERSION: u32 = 2;
/// 当前本地 Plan 文档格式版本。
const PLAN_DOCUMENT_VERSION: u32 = 2;
/// 完成态 Goal 验收证据允许的最大 Unicode 字符数。
const MAX_GOAL_COMPLETION_EVIDENCE_CHARS: usize = 20_000;
/// 阻塞态 Goal 原因允许的最大 Unicode 字符数。
const MAX_GOAL_BLOCKED_REASON_CHARS: usize = 4_000;
/// Goal 标题允许的最大 Unicode 字符数，与 ACP 标题边界保持一致。
const MAX_GOAL_TITLE_CHARS: usize = 512;
/// Goal 目标和说明允许的最大 Unicode 字符数。
const MAX_GOAL_TEXT_CHARS: usize = 64 * 1024;
/// Goal 标识允许的最大 UTF-8 字节数。
const MAX_GOAL_IDENTIFIER_BYTES: usize = 256;
/// 项目级 Goal 记录中固定的作用域值。
const GOAL_SCOPE: &str = "project";
/// 单个 Goal 或 Plan 文档保留的最近幂等操作收据上限。
pub const MAX_DOCUMENT_OPERATION_RECEIPTS: usize = 256;

/// 一个已经原子应用到 Goal 或 Plan 文档的有界幂等操作收据。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DocumentOperationReceipt {
    /// 调用方从可信请求上下文派生且重试时保持不变的操作标识。
    pub operation_id: String,
    /// 操作种类与规范化参数的递归键排序 JSON SHA-256。
    pub payload_sha256: String,
    /// 该操作完成后的业务状态 revision；纯幂等 no-op 可以保持原值。
    pub result_revision: u64,
}

/// 一次带持久幂等收据的文档 CAS 结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentOperationOutcome<T> {
    /// 当前调用首次原子应用，并已连同收据持久化。
    Applied(T),
    /// 相同操作标识与载荷此前已经提交，本次只返回当前文档。
    Deduplicated(T),
}

impl<T> DocumentOperationOutcome<T> {
    /// 返回本次操作是否命中了已经持久化的去重收据。
    pub const fn deduplicated(&self) -> bool {
        matches!(self, Self::Deduplicated(_))
    }

    /// 返回结果中当前持久文档的共享引用。
    pub const fn document(&self) -> &T {
        match self {
            Self::Applied(document) | Self::Deduplicated(document) => document,
        }
    }

    /// 消费结果并返回当前持久文档。
    pub fn into_document(self) -> T {
        match self {
            Self::Applied(document) | Self::Deduplicated(document) => document,
        }
    }
}

/// Memory、Goal 与 Plan 单文件的可配置字节限制。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DocumentLimits {
    /// 单个 JSON 文档允许的最大落盘字节数。
    pub max_document_bytes: u64,
}

impl Default for DocumentLimits {
    /// 返回适合本地项目级文档的保守默认限制。
    fn default() -> Self {
        Self {
            max_document_bytes: 4 * 1024 * 1024,
        }
    }
}

impl DocumentLimits {
    /// 拒绝不能保存任何文档的零字节限制。
    fn validate(self) -> Result<Self, ResourceError> {
        if self.max_document_bytes == 0 {
            return Err(ResourceError::UnsafePath(
                "文档大小限制必须大于零".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// 一条已经由上层抽取并决定持久化的本地 Memory。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MemoryEntry {
    /// Memory 稳定标识。
    pub memory_id: String,
    /// Memory 正文。
    pub content: String,
    /// 创建或更新时间的 Unix Epoch 毫秒值。
    pub updated_at_unix_ms: u64,
    /// 供上层检索的非秘密标签。
    pub tags: Vec<String>,
}

/// 一个作用域的完整本地 Memory 文档。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MemoryDocument {
    /// 固定 schema 名称。
    pub schema: String,
    /// 固定格式版本。
    pub version: u32,
    /// 安全路径作用域。
    pub scope: ScopeId,
    /// 每次成功比较交换后单调递增的版本号。
    pub revision: u64,
    /// 上层已经抽取的完整 Memory 列表。
    pub entries: Vec<MemoryEntry>,
}

impl MemoryDocument {
    /// 创建尚未持久化、revision 为零的当前格式 Memory 文档。
    pub fn new(scope: ScopeId, entries: Vec<MemoryEntry>) -> Self {
        Self {
            schema: MEMORY_SCHEMA.to_owned(),
            version: MEMORY_DOCUMENT_VERSION,
            scope,
            revision: 0,
            entries,
        }
    }
}

/// 项目级持久 Goal 的生命周期状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum GoalStatus {
    /// Agent 仍应继续推进目标。
    Active,
    /// 目标已经完成，不允许再次迁移。
    Completed,
    /// 目标因无法自行解决的原因阻塞，不允许再次迁移。
    Blocked,
}

impl GoalStatus {
    /// 返回当前状态是否已经不可逆地结束。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Blocked)
    }
}

/// 项目当前唯一 Goal 的完整持久字段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct GoalRecord {
    /// 跨进程唯一且按创建时间排序的 Goal 标识。
    pub id: String,
    /// 输入框上方展示的简短标题。
    pub title: String,
    /// 固定项目级作用域。
    pub scope: String,
    /// 当前生命周期状态。
    pub status: GoalStatus,
    /// 可选补充说明。
    pub description: Option<String>,
    /// 可选人工进度百分比。
    pub progress_percent: Option<u8>,
    /// 可验证且完整的目标描述。
    pub objective: String,
    /// 可选 Token 预算。
    pub token_budget: Option<u64>,
    /// Provider 明确报告并累计的 Token 数。
    pub tokens_used: u64,
    /// 累计实际运行秒数。
    pub time_used_seconds: u64,
    /// 仅在阻塞状态存在的原因。
    pub blocked_reason: Option<String>,
    /// 仅在完成状态存在的非空验收证据。
    pub completion_evidence: Option<String>,
    /// 创建时间的 Unix 毫秒值。
    pub created_at_unix_ms: u64,
    /// 最后变化时间的 Unix 毫秒值。
    pub updated_at_unix_ms: u64,
}

impl GoalRecord {
    /// 返回明确预算存在且大于零时的使用比例。
    pub fn usage_ratio(&self) -> Option<f64> {
        self.token_budget
            .filter(|budget| *budget > 0)
            .map(|budget| self.tokens_used as f64 / budget as f64)
    }
}

/// 项目 Goal 当前版本和可选记录。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct GoalSnapshot {
    /// 每次实际变化递增的版本号。
    pub revision: u64,
    /// 当前唯一 Goal；清除后为 `None`。
    pub goal: Option<GoalRecord>,
    /// 已清除终态 Goal 的标识墓碑。
    pub retired_goal_ids: Vec<String>,
}

/// 一个作用域的完整本地 Goal 文档。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GoalDocument {
    /// 固定 schema 名称。
    pub schema: String,
    /// 固定格式版本。
    pub version: u32,
    /// 安全路径作用域。
    pub scope: ScopeId,
    /// 每次成功比较交换后单调递增的 Goal 版本号。
    pub revision: u64,
    /// 当前唯一 Goal；明确清除时为 `None`。
    pub goal: Option<GoalRecord>,
    /// 已进入终态并清除的 Goal 标识墓碑，防止相同 Goal 被重新激活。
    pub retired_goal_ids: Vec<String>,
    /// 最近成功接受的有界幂等操作收据，按提交顺序保存。
    pub operation_receipts: Vec<DocumentOperationReceipt>,
}

impl GoalDocument {
    /// 创建尚未持久化、revision 为零的当前格式 Goal 文档。
    pub fn new(scope: ScopeId, goal: Option<GoalRecord>) -> Self {
        Self {
            schema: GOAL_SCHEMA.to_owned(),
            version: GOAL_DOCUMENT_VERSION,
            scope,
            revision: 0,
            goal,
            retired_goal_ids: Vec::new(),
            operation_receipts: Vec::new(),
        }
    }

    /// 从运行时 GoalSnapshot 无损构造持久文档。
    pub fn from_snapshot(scope: ScopeId, snapshot: GoalSnapshot) -> Self {
        Self {
            schema: GOAL_SCHEMA.to_owned(),
            version: GOAL_DOCUMENT_VERSION,
            scope,
            revision: snapshot.revision,
            goal: snapshot.goal,
            retired_goal_ids: snapshot.retired_goal_ids,
            operation_receipts: Vec::new(),
        }
    }

    /// 返回可由 Agent 运行时直接复用的无损 GoalSnapshot。
    pub fn snapshot(&self) -> GoalSnapshot {
        GoalSnapshot {
            revision: self.revision,
            goal: self.goal.clone(),
            retired_goal_ids: self.retired_goal_ids.clone(),
        }
    }

    /// 查询操作标识与规范化载荷是否已经提交，载荷冲突时明确报错。
    pub fn applied_operation_revision<P: Serialize + ?Sized>(
        &self,
        operation_id: &str,
        operation: &P,
    ) -> Result<Option<u64>, ResourceError> {
        operation_receipt_revision(&self.operation_receipts, operation_id, operation)
    }
}

/// 按项目、Session 与 Agent 隔离的计划沙箱文档。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanDocument {
    /// 固定 schema 名称。
    pub schema: String,
    /// 固定格式版本。
    pub version: u32,
    /// 由规范项目根目录派生的安全项目作用域。
    pub project_scope: ScopeId,
    /// 计划所属 Session。
    pub session_id: SessionId,
    /// 计划所属根 Agent 或单层子 Agent。
    pub agent_id: AgentId,
    /// 每次实际正文变化后单调递增的版本号。
    pub revision: u64,
    /// 当前计划或报告正文；清除后为 `None`。
    pub content: Option<String>,
    /// 当前正文对应的 Session Artifact；清除或尚未生成最终产物时为 `None`。
    pub plan_artifact: Option<ArtifactUse>,
    /// 最后一次实际正文变化的 Unix Epoch 毫秒时间。
    pub updated_at_unix_ms: Option<u64>,
    /// 最近成功接受的有界幂等操作收据，按提交顺序保存。
    pub operation_receipts: Vec<DocumentOperationReceipt>,
}

impl PlanDocument {
    /// 创建尚未持久化、revision 为零的当前格式计划文档。
    pub fn new(project_scope: ScopeId, session_id: SessionId, agent_id: AgentId) -> Self {
        Self {
            schema: PLAN_SCHEMA.to_owned(),
            version: PLAN_DOCUMENT_VERSION,
            project_scope,
            session_id,
            agent_id,
            revision: 0,
            content: None,
            plan_artifact: None,
            updated_at_unix_ms: None,
            operation_receipts: Vec::new(),
        }
    }

    /// 查询操作标识与规范化载荷是否已经提交，载荷冲突时明确报错。
    pub fn applied_operation_revision<P: Serialize + ?Sized>(
        &self,
        operation_id: &str,
        operation: &P,
    ) -> Result<Option<u64>, ResourceError> {
        operation_receipt_revision(&self.operation_receipts, operation_id, operation)
    }
}

/// 本地 Memory 文件的原子比较交换存储边界。
pub struct MemoryFileStore {
    /// 通用文档目录和锁边界。
    store: DocumentStore,
}

impl MemoryFileStore {
    /// 打开或创建全新 Memory 文档目录。
    ///
    /// 路径隔离仅为尽力检查，不承诺抵御具有本机目录写权限的并发攻击者。
    pub fn open(storage_root: impl AsRef<Path>) -> Result<Self, ResourceError> {
        Self::open_with_limits(storage_root, DocumentLimits::default())
    }

    /// 使用明确字节限制打开或创建 Memory 文档目录。
    pub fn open_with_limits(
        storage_root: impl AsRef<Path>,
        limits: DocumentLimits,
    ) -> Result<Self, ResourceError> {
        Ok(Self {
            store: DocumentStore::open(storage_root.as_ref(), "memories", limits.validate()?)?,
        })
    }

    /// 在同一文件锁内核验 revision 并原子替换完整 Memory 文档。
    ///
    /// 不存在的文档其 revision 视为零；提交成功后返回 revision 加一的实际文档。
    pub fn compare_and_swap(
        &self,
        expected_revision: u64,
        mut document: MemoryDocument,
    ) -> Result<MemoryDocument, ResourceError> {
        validate_memory(&document, false)?;
        let _lock = self.store.lock()?;
        let current: Option<MemoryDocument> = self.store.read_unlocked(&document.scope)?;
        if let Some(current) = &current {
            validate_memory(current, true)?;
            ensure_scope_matches(&current.scope, &document.scope, "Memory")?;
        }
        let actual_revision = current.as_ref().map_or(0, |value| value.revision);
        ensure_revision(expected_revision, actual_revision)?;
        document.revision = next_revision(actual_revision)?;
        validate_memory(&document, true)?;
        self.store.write_unlocked(&document.scope, &document)?;
        Ok(document)
    }

    /// 在文档文件锁内读取当前 Memory 文档；不存在时返回 `None`。
    pub fn read(&self, scope: &ScopeId) -> Result<Option<MemoryDocument>, ResourceError> {
        let _lock = self.store.lock()?;
        let document = self.store.read_unlocked(scope)?;
        if let Some(document) = &document {
            validate_memory(document, true)?;
            ensure_scope_matches(&document.scope, scope, "Memory")?;
        }
        Ok(document)
    }
}

/// 本地 Goal 文件的原子比较交换存储边界。
pub struct GoalFileStore {
    /// 通用文档目录和锁边界。
    store: DocumentStore,
}

impl GoalFileStore {
    /// 打开或创建全新 Goal 文档目录。
    ///
    /// 这是项目 Goal 的唯一权威持久化边界；Session 事件和状态不保存 Goal 副本。
    /// 路径隔离仅为尽力检查，不承诺抵御具有本机目录写权限的并发攻击者。
    pub fn open(storage_root: impl AsRef<Path>) -> Result<Self, ResourceError> {
        Self::open_with_limits(storage_root, DocumentLimits::default())
    }

    /// 使用明确字节限制打开或创建 Goal 文档目录。
    pub fn open_with_limits(
        storage_root: impl AsRef<Path>,
        limits: DocumentLimits,
    ) -> Result<Self, ResourceError> {
        Ok(Self {
            store: DocumentStore::open(storage_root.as_ref(), "goals", limits.validate()?)?,
        })
    }

    /// 在同一文件锁内核验幂等收据与 revision，并原子替换完整 Goal 文档。
    ///
    /// 不存在的文档其 revision 视为零；只有 Goal 业务字段实际变化时 revision 才加一。
    pub fn compare_and_swap<P: Serialize + ?Sized>(
        &self,
        operation_id: &str,
        operation: &P,
        expected_revision: u64,
        mut document: GoalDocument,
    ) -> Result<DocumentOperationOutcome<GoalDocument>, ResourceError> {
        validate_operation_id(operation_id)?;
        let payload_sha256 = canonical_json_sha256(operation)?;
        let _lock = self.store.lock()?;
        let current: Option<GoalDocument> = self.store.read_unlocked(&document.scope)?;
        if let Some(current) = &current {
            validate_goal(current, true)?;
            ensure_scope_matches(&current.scope, &document.scope, "Goal")?;
            if matching_operation_receipt(
                &current.operation_receipts,
                operation_id,
                &payload_sha256,
            )?
            .is_some()
            {
                return Ok(DocumentOperationOutcome::Deduplicated(current.clone()));
            }
        }
        let actual_revision = current.as_ref().map_or(0, |value| value.revision);
        ensure_revision(expected_revision, actual_revision)?;
        if document.revision != expected_revision {
            return Err(ResourceError::RevisionConflict {
                expected: expected_revision,
                actual: document.revision,
            });
        }
        document.retired_goal_ids = current
            .as_ref()
            .map_or_else(Vec::new, |current| current.retired_goal_ids.clone());
        document.operation_receipts = current
            .as_ref()
            .map_or_else(Vec::new, |current| current.operation_receipts.clone());
        validate_goal(&document, false)?;
        validate_goal_transition(current.as_ref(), &document)?;
        let state_changed =
            current.as_ref().and_then(|value| value.goal.as_ref()) != document.goal.as_ref();
        if let Some(retired_id) = current
            .as_ref()
            .and_then(|value| value.goal.as_ref())
            .filter(|goal| goal.status.is_terminal() && document.goal.is_none())
            .map(|goal| goal.id.clone())
        {
            document.retired_goal_ids.push(retired_id);
        }
        document.revision = if state_changed {
            next_revision(actual_revision)?
        } else {
            actual_revision
        };
        append_operation_receipt(
            &mut document.operation_receipts,
            operation_id,
            payload_sha256,
            document.revision,
        );
        validate_goal(&document, true)?;
        self.store.write_unlocked(&document.scope, &document)?;
        Ok(DocumentOperationOutcome::Applied(document))
    }

    /// 在文档文件锁内读取当前 Goal 文档；不存在时返回 `None`。
    pub fn read(&self, scope: &ScopeId) -> Result<Option<GoalDocument>, ResourceError> {
        let _lock = self.store.lock()?;
        let document = self.store.read_unlocked(scope)?;
        if let Some(document) = &document {
            validate_goal(document, true)?;
            ensure_scope_matches(&document.scope, scope, "Goal")?;
        }
        Ok(document)
    }
}

/// 应用数据根下按项目、Session 与 Agent 隔离的 Plan 文件存储边界。
pub struct PlanFileStore {
    /// 已验证且只能位于应用数据根下的计划目录。
    directory: PathBuf,
    /// 协调全部计划文档比较交换的跨实例文件锁。
    lock_path: PathBuf,
    /// 单个计划 JSON 文档允许的最大字节数。
    max_document_bytes: u64,
}

impl PlanFileStore {
    /// 打开或创建全新的计划沙箱目录。
    ///
    /// 调用方必须传入 KeenCode 应用数据根，不能传入用户项目目录。
    /// 路径隔离仅为尽力检查，不承诺抵御具有本机目录写权限的并发攻击者。
    pub fn open(storage_root: impl AsRef<Path>) -> Result<Self, ResourceError> {
        Self::open_with_limits(storage_root, DocumentLimits::default())
    }

    /// 使用明确单文档字节限制打开或创建计划沙箱目录。
    pub fn open_with_limits(
        storage_root: impl AsRef<Path>,
        limits: DocumentLimits,
    ) -> Result<Self, ResourceError> {
        let limits = limits.validate()?;
        let root = prepare_root(storage_root.as_ref())?;
        let directory = secure_child_dir(&root, "plans")?;
        let lock_path = directory.join("documents.lock");
        ensure_regular_file_or_absent(&lock_path)?;
        Ok(Self {
            directory,
            lock_path,
            max_document_bytes: limits.max_document_bytes,
        })
    }

    /// 在同一文件锁内核验幂等收据与 revision，并原子替换完整计划文档。
    ///
    /// 不存在的文档其 revision 视为零；只有计划正文实际变化时 revision 才加一。
    pub fn compare_and_swap<P: Serialize + ?Sized>(
        &self,
        operation_id: &str,
        operation: &P,
        expected_revision: u64,
        mut document: PlanDocument,
    ) -> Result<DocumentOperationOutcome<PlanDocument>, ResourceError> {
        validate_operation_id(operation_id)?;
        let payload_sha256 = canonical_json_sha256(operation)?;
        let _lock = exclusive_lock(&self.lock_path)?;
        let current = self.read_unlocked(
            &document.project_scope,
            &document.session_id,
            &document.agent_id,
        )?;
        if let Some(current) = &current {
            validate_plan(current, true)?;
            ensure_plan_identity(
                current,
                &document.project_scope,
                &document.session_id,
                &document.agent_id,
            )?;
            if matching_operation_receipt(
                &current.operation_receipts,
                operation_id,
                &payload_sha256,
            )?
            .is_some()
            {
                return Ok(DocumentOperationOutcome::Deduplicated(current.clone()));
            }
        }
        let actual_revision = current.as_ref().map_or(0, |value| value.revision);
        ensure_revision(expected_revision, actual_revision)?;
        if document.revision != expected_revision {
            return Err(ResourceError::RevisionConflict {
                expected: expected_revision,
                actual: document.revision,
            });
        }
        document.operation_receipts = current
            .as_ref()
            .map_or_else(Vec::new, |current| current.operation_receipts.clone());
        validate_plan(&document, false)?;
        validate_plan_transition(current.as_ref(), &document)?;
        // 空计划首次保存只建立操作收据，不应凭空制造正文 revision；有正文或
        // Artifact 的首次保存才算一次实际状态变化。
        let state_changed = match current.as_ref() {
            None => document.content.is_some() || document.plan_artifact.is_some(),
            Some(value) => {
                value.content != document.content || value.plan_artifact != document.plan_artifact
            }
        };
        document.revision = if state_changed {
            next_revision(actual_revision)?
        } else {
            actual_revision
        };
        append_operation_receipt(
            &mut document.operation_receipts,
            operation_id,
            payload_sha256,
            document.revision,
        );
        validate_plan(&document, true)?;
        self.write_unlocked(&document)?;
        Ok(DocumentOperationOutcome::Applied(document))
    }

    /// 以已形成的 Markdown Artifact 作为计划最终产物执行一次 CAS。
    ///
    /// 调用方先以内容寻址方式形成完整 pair，再把同一个精简引用写入
    /// `plan.json`。如果计划 CAS 因 revision 冲突或校验失败，调用方不会看到
    /// 新计划文档；未被权威 `PlanChanged` 引用的 Artifact 会在 Session 冷恢复时
    /// 按 ArtifactStore 的孤儿回收规则清理。该顺序使失败不会返回半完成的计划状态。
    pub fn compare_and_swap_with_artifact_ref(
        &self,
        artifact: &ArtifactRef,
        operation_id: &str,
        expected_revision: u64,
        mut document: PlanDocument,
    ) -> Result<DocumentOperationOutcome<PlanDocument>, ResourceError> {
        if document.content.is_none() {
            return Err(ResourceError::InvalidPlanTransition(
                "最终计划必须包含正文才能创建 Artifact".to_owned(),
            ));
        }
        validate_plan(&document, false)?;
        let content = document.content.clone().expect("已检查计划正文存在");
        if !artifact_matches_plan_content(artifact, &content) {
            return Err(ResourceError::ArtifactHashMismatch);
        }
        document.plan_artifact = Some(artifact.as_event_use());
        let operation = ("plan_replace_artifact_v1", &content);
        self.compare_and_swap(operation_id, &operation, expected_revision, document)
    }

    /// 在同一 Session ArtifactStore 中创建最终计划 Artifact 并执行一次 CAS。
    ///
    /// 该便捷入口先校验计划身份，再原子写入 ArtifactStore，最后把返回引用交给
    /// [`Self::compare_and_swap_with_artifact_ref`]；跨 Session 引用因此会在创建前拒绝。
    /// 若文档 CAS 失败，计划文档保持原状，未被权威 `PlanChanged` 引用的 Artifact
    /// 由 Session 冷恢复按孤儿规则回收。
    pub fn compare_and_swap_with_artifact(
        &self,
        artifacts: &ArtifactStore,
        operation_id: &str,
        expected_revision: u64,
        mut document: PlanDocument,
    ) -> Result<DocumentOperationOutcome<PlanDocument>, ResourceError> {
        if document.content.is_none() {
            return Err(ResourceError::InvalidPlanTransition(
                "最终计划必须包含正文才能创建 Artifact".to_owned(),
            ));
        }
        if artifacts.session_id() != &document.session_id {
            return Err(ResourceError::ArtifactScopeMismatch);
        }
        validate_plan(&document, false)?;
        let content = document.content.clone().expect("已检查计划正文存在");
        let artifact = artifacts.put(content.as_bytes(), Some("text/markdown".to_owned()))?;
        document.plan_artifact = Some(artifact.as_event_use());
        self.compare_and_swap_with_artifact_ref(
            &artifact,
            operation_id,
            expected_revision,
            document,
        )
    }

    /// 在文件锁内读取一个隔离计划文档；不存在时返回 `None`。
    pub fn read(
        &self,
        project_scope: &ScopeId,
        session_id: &SessionId,
        agent_id: &AgentId,
    ) -> Result<Option<PlanDocument>, ResourceError> {
        let _lock = exclusive_lock(&self.lock_path)?;
        let document = self.read_unlocked(project_scope, session_id, agent_id)?;
        if let Some(document) = &document {
            validate_plan(document, true)?;
            ensure_plan_identity(document, project_scope, session_id, agent_id)?;
        }
        Ok(document)
    }

    /// 在调用方持有全局计划锁时读取一个有界 JSON 文档。
    fn read_unlocked(
        &self,
        project_scope: &ScopeId,
        session_id: &SessionId,
        agent_id: &AgentId,
    ) -> Result<Option<PlanDocument>, ResourceError> {
        let destination = self.document_path(project_scope, session_id, agent_id)?;
        ensure_regular_file_or_absent(&destination)?;
        if !destination.exists() {
            return Ok(None);
        }
        let bytes = match read_file_bounded(&destination, self.max_document_bytes) {
            Ok(BoundedRead::Bytes(bytes)) => bytes,
            Ok(BoundedRead::TooLarge { actual }) => {
                return Err(ResourceError::DocumentTooLarge {
                    actual,
                    limit: self.max_document_bytes,
                });
            }
            Err(error) => return Err(ResourceError::io("read_plan_document", error)),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| ResourceError::Json(error.to_string()))
    }

    /// 在调用方持有全局计划锁时原子写入一个有界 JSON 文档。
    fn write_unlocked(&self, document: &PlanDocument) -> Result<(), ResourceError> {
        let destination = self.document_path(
            &document.project_scope,
            &document.session_id,
            &document.agent_id,
        )?;
        ensure_regular_file_or_absent(&destination)?;
        let mut bytes = match serialize_json_bounded(document, self.max_document_bytes, true)? {
            BoundedJson::Bytes(bytes) => bytes,
            BoundedJson::TooLarge { actual } => {
                return Err(ResourceError::DocumentTooLarge {
                    actual,
                    limit: self.max_document_bytes,
                });
            }
        };
        bytes.push(b'\n');
        let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual > self.max_document_bytes {
            return Err(ResourceError::DocumentTooLarge {
                actual,
                limit: self.max_document_bytes,
            });
        }
        atomic_write(&destination, &bytes, true)
    }

    /// 逐层复核安全路径段并构造固定 `plan.json` 目标。
    fn document_path(
        &self,
        project_scope: &ScopeId,
        session_id: &SessionId,
        agent_id: &AgentId,
    ) -> Result<PathBuf, ResourceError> {
        let project_directory = secure_child_dir(&self.directory, project_scope.as_str())?;
        let session_directory = secure_child_dir(&project_directory, session_id.as_str())?;
        let agent_directory = secure_child_dir(&session_directory, agent_id.as_str())?;
        Ok(agent_directory.join("plan.json"))
    }
}

/// 单一固定文档种类的目录与锁。
struct DocumentStore {
    /// 已验证的文档目录。
    directory: PathBuf,
    /// 跨实例原子替换协调锁。
    lock_path: PathBuf,
    /// 单个 JSON 文档允许的最大字节数。
    max_document_bytes: u64,
}

impl DocumentStore {
    /// 打开一个固定名称文档目录。
    fn open(
        storage_root: &Path,
        directory_name: &str,
        limits: DocumentLimits,
    ) -> Result<Self, ResourceError> {
        let root = prepare_root(storage_root)?;
        let directory = secure_child_dir(&root, directory_name)?;
        let lock_path = directory.join("documents.lock");
        ensure_regular_file_or_absent(&lock_path)?;
        Ok(Self {
            directory,
            lock_path,
            max_document_bytes: limits.max_document_bytes,
        })
    }

    /// 获取跨实例文档锁。
    fn lock(&self) -> Result<ExclusiveFileLock, ResourceError> {
        exclusive_lock(&self.lock_path)
    }

    /// 在调用方已持锁时序列化并原子替换作用域文档。
    fn write_unlocked<T: Serialize>(
        &self,
        scope: &ScopeId,
        value: &T,
    ) -> Result<(), ResourceError> {
        let destination = self.path(scope);
        ensure_regular_file_or_absent(&destination)?;
        let mut bytes = match serialize_json_bounded(value, self.max_document_bytes, true)? {
            BoundedJson::Bytes(bytes) => bytes,
            BoundedJson::TooLarge { actual } => {
                return Err(ResourceError::DocumentTooLarge {
                    actual,
                    limit: self.max_document_bytes,
                });
            }
        };
        bytes.push(b'\n');
        let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual > self.max_document_bytes {
            return Err(ResourceError::DocumentTooLarge {
                actual,
                limit: self.max_document_bytes,
            });
        }
        atomic_write(&destination, &bytes, true)
    }

    /// 在调用方已持锁时读取并反序列化作用域文档。
    fn read_unlocked<T: for<'de> Deserialize<'de>>(
        &self,
        scope: &ScopeId,
    ) -> Result<Option<T>, ResourceError> {
        let destination = self.path(scope);
        ensure_regular_file_or_absent(&destination)?;
        if !destination.exists() {
            return Ok(None);
        }
        let bytes = match read_file_bounded(&destination, self.max_document_bytes) {
            Ok(BoundedRead::Bytes(bytes)) => bytes,
            Ok(BoundedRead::TooLarge { actual }) => {
                return Err(ResourceError::DocumentTooLarge {
                    actual,
                    limit: self.max_document_bytes,
                });
            }
            Err(error) => return Err(ResourceError::io("read_atomic_document", error)),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| ResourceError::Json(error.to_string()))
    }

    /// 从已验证 ScopeId 构造固定 JSON 文件名。
    fn path(&self, scope: &ScopeId) -> PathBuf {
        self.directory.join(format!("{}.json", scope.as_str()))
    }
}

/// 校验 Memory 文档属于当前唯一 schema 且标识不重复。
fn validate_memory(document: &MemoryDocument, persisted: bool) -> Result<(), ResourceError> {
    if document.schema != MEMORY_SCHEMA
        || document.version != MEMORY_DOCUMENT_VERSION
        || persisted && document.revision == 0
    {
        return Err(ResourceError::Json(
            "Memory schema/version/revision 不受支持".to_owned(),
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    if document.entries.iter().any(|entry| {
        entry.memory_id.trim().is_empty()
            || entry.content.trim().is_empty()
            || !ids.insert(entry.memory_id.as_str())
    }) {
        return Err(ResourceError::Json("Memory 标识重复或正文为空".to_owned()));
    }
    Ok(())
}

/// 校验 Goal 文档与完整 GoalRecord 的结构不变量。
fn validate_goal(document: &GoalDocument, persisted: bool) -> Result<(), ResourceError> {
    if document.schema != GOAL_SCHEMA
        || document.version != GOAL_DOCUMENT_VERSION
        || persisted && document.revision == 0
        || persisted && document.operation_receipts.is_empty()
        || persisted && document.goal.is_none() && document.retired_goal_ids.is_empty()
    {
        return Err(ResourceError::Json(
            "Goal schema/version/revision 不受支持".to_owned(),
        ));
    }
    if document.goal.as_ref().is_some_and(|goal| {
        !valid_goal_identifier(&goal.id)
            || !valid_goal_text(&goal.title, MAX_GOAL_TITLE_CHARS)
            || goal.scope != GOAL_SCOPE
            || !valid_goal_text(&goal.objective, MAX_GOAL_TEXT_CHARS)
            || goal
                .description
                .as_deref()
                .is_some_and(|description| !valid_goal_text(description, MAX_GOAL_TEXT_CHARS))
            || goal.token_budget == Some(0)
            || goal.progress_percent.is_some_and(|progress| progress > 100)
            || goal.created_at_unix_ms == 0
            || goal.updated_at_unix_ms < goal.created_at_unix_ms
            || match goal.status {
                GoalStatus::Active => {
                    goal.blocked_reason.is_some() || goal.completion_evidence.is_some()
                }
                GoalStatus::Blocked => {
                    goal.completion_evidence.is_some()
                        || goal.blocked_reason.as_deref().is_none_or(|reason| {
                            !valid_goal_text(reason, MAX_GOAL_BLOCKED_REASON_CHARS)
                        })
                }
                GoalStatus::Completed => {
                    goal.blocked_reason.is_some()
                        || goal.completion_evidence.as_deref().is_none_or(|evidence| {
                            !valid_goal_text(evidence, MAX_GOAL_COMPLETION_EVIDENCE_CHARS)
                        })
                }
            }
    }) {
        return Err(ResourceError::Json("Goal 字段或状态不变量无效".to_owned()));
    }
    let mut retired = std::collections::BTreeSet::new();
    if document
        .retired_goal_ids
        .iter()
        .any(|id| !valid_goal_identifier(id) || !retired.insert(id.as_str()))
        || persisted
            && document
                .goal
                .as_ref()
                .is_some_and(|goal| retired.contains(goal.id.as_str()))
    {
        return Err(ResourceError::Json(
            "Goal 墓碑重复、为空或与当前 Goal 冲突".to_owned(),
        ));
    }
    validate_operation_receipts(&document.operation_receipts, document.revision)?;
    Ok(())
}

/// 校验 Goal CAS 不会绕过 Agent 的单例、活跃更新和不可逆终态语义。
fn validate_goal_transition(
    current: Option<&GoalDocument>,
    next: &GoalDocument,
) -> Result<(), ResourceError> {
    if current.is_none() && !next.retired_goal_ids.is_empty() {
        return Err(ResourceError::InvalidGoalTransition(
            "首次创建 Goal 不能伪造历史墓碑".to_owned(),
        ));
    }
    let current_goal = current.and_then(|document| document.goal.as_ref());
    match (current_goal, next.goal.as_ref()) {
        (None, Some(goal)) => {
            if goal.status != GoalStatus::Active
                || goal.tokens_used != 0
                || goal.time_used_seconds != 0
                || current.is_some_and(|document| {
                    document
                        .retired_goal_ids
                        .iter()
                        .any(|retired| retired == &goal.id)
                })
            {
                return Err(ResourceError::InvalidGoalTransition(
                    "新 Goal 必须从 active、零用量和未退役标识开始".to_owned(),
                ));
            }
        }
        (None, None) => {
            return Err(ResourceError::InvalidGoalTransition(
                "不存在 Goal 时不能重复清除".to_owned(),
            ));
        }
        (Some(previous), None) => {
            if !previous.status.is_terminal() {
                return Err(ResourceError::InvalidGoalTransition(
                    "active Goal 必须先进入 completed 或 blocked 才能清除".to_owned(),
                ));
            }
        }
        (Some(previous), Some(candidate)) => {
            if previous.status.is_terminal() {
                return Err(ResourceError::InvalidGoalTransition(
                    "终态 Goal 只能清除，不能更新或重新激活".to_owned(),
                ));
            }
            if previous.id != candidate.id
                || previous.scope != candidate.scope
                || previous.created_at_unix_ms != candidate.created_at_unix_ms
            {
                return Err(ResourceError::InvalidGoalTransition(
                    "现有 Goal 的 id、scope 和创建时间不可替换".to_owned(),
                ));
            }
            if candidate.tokens_used < previous.tokens_used
                || candidate.time_used_seconds < previous.time_used_seconds
                || candidate.updated_at_unix_ms < previous.updated_at_unix_ms
            {
                return Err(ResourceError::InvalidGoalTransition(
                    "Goal 用量和更新时间不能倒退".to_owned(),
                ));
            }
            if candidate.status.is_terminal()
                && (previous.title != candidate.title
                    || previous.description != candidate.description
                    || previous.progress_percent != candidate.progress_percent
                    || previous.objective != candidate.objective
                    || previous.token_budget != candidate.token_budget
                    || previous.tokens_used != candidate.tokens_used
                    || previous.time_used_seconds != candidate.time_used_seconds)
            {
                return Err(ResourceError::InvalidGoalTransition(
                    "终态迁移只能改变状态、阻塞原因、完成证据和更新时间".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// 校验计划文档 schema、正文上限、时间戳与清除状态不变量。
fn validate_plan(document: &PlanDocument, persisted: bool) -> Result<(), ResourceError> {
    if document.schema != PLAN_SCHEMA
        || document.version != PLAN_DOCUMENT_VERSION
        || persisted && document.operation_receipts.is_empty()
        || document.content.is_some() && document.updated_at_unix_ms.is_none()
        || document.content.is_none() && document.plan_artifact.is_some()
        || document
            .plan_artifact
            .as_ref()
            .is_some_and(|artifact| !valid_artifact_use(artifact))
    {
        return Err(ResourceError::Json(
            "Plan schema/version/revision/timestamp 不受支持".to_owned(),
        ));
    }
    if document.content.as_ref().is_some_and(|content| {
        // Markdown 正文允许文件惯例中的首尾换行；校验只拒绝纯空白正文，
        // 同时保留原始正文用于 Artifact 哈希和持久化，不能静默 trim。
        content.trim().is_empty() || content.chars().count() > 200_000
    }) {
        return Err(ResourceError::Json(
            "Plan 正文为空或超过字符上限".to_owned(),
        ));
    }
    validate_operation_receipts(&document.operation_receipts, document.revision)?;
    Ok(())
}

/// 校验计划 CAS 的正文变化与最后更新时间保持一致且不会倒退。
fn validate_plan_transition(
    current: Option<&PlanDocument>,
    next: &PlanDocument,
) -> Result<(), ResourceError> {
    match current {
        None if next.content.is_none() && next.updated_at_unix_ms.is_some() => Err(
            ResourceError::InvalidPlanTransition("空计划不能伪造正文更新时间".to_owned()),
        ),
        Some(current)
            if current.content == next.content
                && current.updated_at_unix_ms != next.updated_at_unix_ms =>
        {
            Err(ResourceError::InvalidPlanTransition(
                "计划正文未变化时不能修改更新时间".to_owned(),
            ))
        }
        Some(current)
            if next.updated_at_unix_ms.unwrap_or_default()
                < current.updated_at_unix_ms.unwrap_or_default() =>
        {
            Err(ResourceError::InvalidPlanTransition(
                "计划更新时间不能倒退".to_owned(),
            ))
        }
        Some(current)
            if current.content != next.content
                && current.plan_artifact.is_some()
                && current.plan_artifact == next.plan_artifact =>
        {
            Err(ResourceError::InvalidPlanTransition(
                "计划正文变化时必须同时更新 Artifact 引用".to_owned(),
            ))
        }
        None | Some(_) => Ok(()),
    }
}

/// 校验计划文档中的精简 Artifact 引用具有内容寻址身份和可审计媒体类型。
fn valid_artifact_use(artifact: &ArtifactUse) -> bool {
    artifact.artifact_id.as_str() == artifact.sha256
        && artifact.media_type.as_deref().is_none_or(|media_type| {
            !media_type.trim().is_empty()
                && !media_type.contains('\r')
                && !media_type.contains('\n')
        })
}

/// 验证计划正文与即将关联的 ArtifactRef 具有相同内容寻址身份。
fn artifact_matches_plan_content(artifact: &ArtifactRef, content: &str) -> bool {
    let bytes = content.as_bytes();
    if artifact.size_bytes != bytes.len() as u64 || !valid_artifact_use(&artifact.as_event_use()) {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut expected = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut expected, "{byte:02x}").expect("写入 String 不会失败");
    }
    artifact.sha256 == expected
}

/// 查询同一操作标识的收据，并拒绝把它复用于不同规范化载荷。
fn operation_receipt_revision<P: Serialize + ?Sized>(
    receipts: &[DocumentOperationReceipt],
    operation_id: &str,
    operation: &P,
) -> Result<Option<u64>, ResourceError> {
    validate_operation_id(operation_id)?;
    let payload_sha256 = canonical_json_sha256(operation)?;
    matching_operation_receipt(receipts, operation_id, &payload_sha256)
        .map(|receipt| receipt.map(|receipt| receipt.result_revision))
}

/// 返回匹配收据；相同标识绑定不同载荷时返回稳定冲突。
fn matching_operation_receipt<'a>(
    receipts: &'a [DocumentOperationReceipt],
    operation_id: &str,
    payload_sha256: &str,
) -> Result<Option<&'a DocumentOperationReceipt>, ResourceError> {
    match receipts
        .iter()
        .find(|receipt| receipt.operation_id == operation_id)
    {
        Some(receipt) if receipt.payload_sha256 == payload_sha256 => Ok(Some(receipt)),
        Some(_) => Err(ResourceError::OperationConflict),
        None => Ok(None),
    }
}

/// 把一次新操作收据追加到文档尾部，并在达到上限时淘汰最旧收据。
fn append_operation_receipt(
    receipts: &mut Vec<DocumentOperationReceipt>,
    operation_id: &str,
    payload_sha256: String,
    result_revision: u64,
) {
    if receipts.len() == MAX_DOCUMENT_OPERATION_RECEIPTS {
        receipts.remove(0);
    }
    receipts.push(DocumentOperationReceipt {
        operation_id: operation_id.to_owned(),
        payload_sha256,
        result_revision,
    });
}

/// 校验持久收据数量、标识、摘要、revision 和唯一性。
fn validate_operation_receipts(
    receipts: &[DocumentOperationReceipt],
    document_revision: u64,
) -> Result<(), ResourceError> {
    if receipts.len() > MAX_DOCUMENT_OPERATION_RECEIPTS {
        return Err(ResourceError::Json("资源操作收据超过上限".to_owned()));
    }
    let mut operation_ids = std::collections::BTreeSet::new();
    for receipt in receipts {
        validate_operation_id(&receipt.operation_id)?;
        if !valid_sha256(&receipt.payload_sha256)
            || receipt.result_revision > document_revision
            || !operation_ids.insert(receipt.operation_id.as_str())
        {
            return Err(ResourceError::Json(
                "资源操作收据摘要、revision 或唯一性无效".to_owned(),
            ));
        }
    }
    Ok(())
}

/// 校验可信操作标识具有固定上限且不含隐式空白或控制字符。
fn validate_operation_id(operation_id: &str) -> Result<(), ResourceError> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || operation_id.trim() != operation_id
        || operation_id.chars().any(char::is_control)
    {
        return Err(ResourceError::InvalidId(
            "操作标识长度必须为 1..=128 字节且不能包含首尾空白或控制字符".to_owned(),
        ));
    }
    Ok(())
}

/// 判断字符串是否为固定 64 位小写十六进制 SHA-256。
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// 校验 Goal 标识不含隐式空白或控制字符，并限制其落盘大小。
fn valid_goal_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_GOAL_IDENTIFIER_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// 校验 Goal 用户文本非空、有界且不包含不可展示控制字符。
fn valid_goal_text(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.trim() == value
        && value.chars().count() <= max_chars
        && !value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

/// 确保磁盘计划文档声明的三层身份与目标路径完全一致。
fn ensure_plan_identity(
    document: &PlanDocument,
    project_scope: &ScopeId,
    session_id: &SessionId,
    agent_id: &AgentId,
) -> Result<(), ResourceError> {
    if &document.project_scope == project_scope
        && &document.session_id == session_id
        && &document.agent_id == agent_id
    {
        Ok(())
    } else {
        Err(ResourceError::Json(
            "Plan 文档项目、Session 或 Agent 身份与路径不匹配".to_owned(),
        ))
    }
}

/// 确保磁盘文档声明的作用域与目标文件名相同。
fn ensure_scope_matches(
    actual: &ScopeId,
    expected: &ScopeId,
    kind: &str,
) -> Result<(), ResourceError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ResourceError::Json(format!(
            "{kind} 文档作用域与文件名不匹配"
        )))
    }
}

/// 比较调用方期望 revision 与锁内实际 revision。
fn ensure_revision(expected: u64, actual: u64) -> Result<(), ResourceError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ResourceError::RevisionConflict { expected, actual })
    }
}

/// 返回下一个 revision，并拒绝整数回绕。
fn next_revision(current: u64) -> Result<u64, ResourceError> {
    current
        .checked_add(1)
        .ok_or_else(|| ResourceError::Json("文档 revision 已达到上限".to_owned()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    /// 创建满足持久层不变量的当前版本活跃 Goal 夹具。
    fn version_fixture_goal() -> GoalRecord {
        GoalRecord {
            id: "019d0000-0000-7000-8000-000000000001".to_owned(),
            title: "严格 Goal 版本".to_owned(),
            scope: "project".to_owned(),
            status: GoalStatus::Active,
            description: Some("验证旧版本和缺失字段均被拒绝".to_owned()),
            progress_percent: Some(10),
            objective: "仅接受当前完整 Goal schema".to_owned(),
            token_budget: Some(10_000),
            tokens_used: 100,
            time_used_seconds: 5,
            blocked_reason: None,
            completion_evidence: None,
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
        }
    }

    /// 旧版本或缺失当前必需字段的 Goal 文档必须严格拒绝且不执行迁移。
    #[test]
    fn old_or_incomplete_goal_document_is_rejected() {
        let storage = TempDir::new().expect("测试存储目录应创建");
        let scope = ScopeId::new("strict-goal").expect("测试 Scope 应有效");
        let store = GoalFileStore::open(storage.path()).expect("Goal Store 应打开");
        let mut current = serde_json::to_value(GoalDocument::new(
            scope.clone(),
            Some(version_fixture_goal()),
        ))
        .expect("Goal 版本夹具应编码");
        let object = current.as_object_mut().expect("Goal 文档应为对象");
        object.insert("revision".to_owned(), json!(1));
        object.insert(
            "operationReceipts".to_owned(),
            json!([{
                "operationId": "strict-goal-create",
                "payloadSha256": "0000000000000000000000000000000000000000000000000000000000000000",
                "resultRevision": 1
            }]),
        );

        let mut old = current.clone();
        let object = old.as_object_mut().expect("Goal 文档应为对象");
        object.insert("version".to_owned(), json!(1));
        fs::write(
            storage.path().join("goals").join("strict-goal.json"),
            serde_json::to_vec(&old).expect("旧 Goal 文档应序列化"),
        )
        .expect("旧 Goal 文档应写入测试目录");
        assert!(matches!(store.read(&scope), Err(ResourceError::Json(_))));

        let mut incomplete = current.clone();
        let object = incomplete.as_object_mut().expect("Goal 文档应为对象");
        object.remove("operationReceipts");
        fs::write(
            storage.path().join("goals").join("strict-goal.json"),
            serde_json::to_vec(&incomplete).expect("缺字段 Goal 文档应序列化"),
        )
        .expect("缺字段 Goal 文档应写入测试目录");
        assert!(matches!(store.read(&scope), Err(ResourceError::Json(_))));

        let mut legacy_field = current;
        legacy_field
            .get_mut("goal")
            .and_then(Value::as_object_mut)
            .expect("Goal 记录应为对象")
            .insert("legacy_field".to_owned(), json!(true));
        fs::write(
            storage.path().join("goals").join("strict-goal.json"),
            serde_json::to_vec(&legacy_field).expect("旧字段 Goal 文档应序列化"),
        )
        .expect("旧字段 Goal 文档应写入测试目录");
        assert!(matches!(store.read(&scope), Err(ResourceError::Json(_))));
    }

    /// Goal 终态原因与完成证据必须保持规范化且受字符上限约束。
    #[test]
    fn noncanonical_goal_terminal_details_are_rejected() {
        let scope = ScopeId::new("strict-terminal-details").expect("测试 Scope 应有效");
        let mut blocked = version_fixture_goal();
        blocked.status = GoalStatus::Blocked;
        blocked.blocked_reason = Some(" 等待外部服务 ".to_owned());
        assert!(matches!(
            validate_goal(&GoalDocument::new(scope.clone(), Some(blocked)), false),
            Err(ResourceError::Json(_))
        ));

        let mut oversized_reason = version_fixture_goal();
        oversized_reason.status = GoalStatus::Blocked;
        oversized_reason.blocked_reason = Some("原".repeat(MAX_GOAL_BLOCKED_REASON_CHARS + 1));
        assert!(matches!(
            validate_goal(
                &GoalDocument::new(scope.clone(), Some(oversized_reason)),
                false
            ),
            Err(ResourceError::Json(_))
        ));

        let mut completed = version_fixture_goal();
        completed.status = GoalStatus::Completed;
        completed.completion_evidence = Some(" 验收通过 ".to_owned());
        assert!(matches!(
            validate_goal(&GoalDocument::new(scope, Some(completed)), false),
            Err(ResourceError::Json(_))
        ));
    }

    /// Goal 文档必须固定项目作用域、有效时间和完整用户文本，不能留下孤立空状态。
    #[test]
    fn goal_document_rejects_invalid_identity_and_orphaned_state() {
        let scope = ScopeId::new("strict-goal-identity").expect("测试 Scope 应有效");

        let mut wrong_scope = version_fixture_goal();
        wrong_scope.scope = "session".to_owned();
        assert!(matches!(
            validate_goal(&GoalDocument::new(scope.clone(), Some(wrong_scope)), false),
            Err(ResourceError::Json(_))
        ));

        let mut missing_created_at = version_fixture_goal();
        missing_created_at.created_at_unix_ms = 0;
        assert!(matches!(
            validate_goal(
                &GoalDocument::new(scope.clone(), Some(missing_created_at)),
                false
            ),
            Err(ResourceError::Json(_))
        ));

        let mut blank_description = version_fixture_goal();
        blank_description.description = Some("   ".to_owned());
        assert!(matches!(
            validate_goal(
                &GoalDocument::new(scope.clone(), Some(blank_description)),
                false
            ),
            Err(ResourceError::Json(_))
        ));

        let mut forged_history = GoalDocument::new(scope.clone(), Some(version_fixture_goal()));
        forged_history.retired_goal_ids = vec!["retired-before-first-write".to_owned()];
        let forged_root = TempDir::new().expect("测试存储目录应创建");
        let store = GoalFileStore::open(forged_root.path()).expect("Goal Store 应打开");
        assert!(matches!(
            store.compare_and_swap("strict-goal-create", &"goal_create_v1", 0, forged_history,),
            Err(ResourceError::InvalidGoalTransition(_))
        ));

        let orphaned = GoalDocument {
            schema: GOAL_SCHEMA.to_owned(),
            version: GOAL_DOCUMENT_VERSION,
            scope: scope.clone(),
            revision: 1,
            goal: None,
            retired_goal_ids: Vec::new(),
            operation_receipts: vec![DocumentOperationReceipt {
                operation_id: "orphaned-goal".to_owned(),
                payload_sha256: "0".repeat(64),
                result_revision: 1,
            }],
        };
        let orphan_root = TempDir::new().expect("孤立状态测试目录应创建");
        let orphan_store = GoalFileStore::open(orphan_root.path()).expect("Goal Store 应打开");
        fs::write(
            orphan_root
                .path()
                .join("goals")
                .join("strict-goal-identity.json"),
            serde_json::to_vec(&orphaned).expect("孤立 Goal 文档应序列化"),
        )
        .expect("孤立 Goal 文档应写入");
        assert!(matches!(
            orphan_store.read(&scope),
            Err(ResourceError::Json(_))
        ));
    }
}
