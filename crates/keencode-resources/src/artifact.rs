use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::atomic::{
    ATOMIC_TEMP_PREFIX, BoundedRead, atomic_write, ensure_regular_file_or_absent, exclusive_lock,
    prepare_root, read_file_bounded, secure_child_dir, sync_directory,
};
use crate::{
    ArtifactId, ArtifactMaterialization, ArtifactUse, MessageImageSource, MessagePart,
    ResourceError, SessionId, SessionLease, SessionState, ToolResultPart, TranscriptRecord,
};

/// Artifact 规范元数据的固定 schema。
const ARTIFACT_METADATA_SCHEMA: &str = "keencode/artifact-metadata";
/// Artifact 规范元数据的固定版本。
const ARTIFACT_METADATA_VERSION: u32 = 1;
/// 单个 Artifact 元数据文件允许的最大字节数。
const ARTIFACT_METADATA_MAX_BYTES: u64 = 16 * 1024;

/// Session 事件提交前对 Artifact 引用执行实际存储核验的可注入边界。
pub trait ArtifactValidator: Send + Sync {
    /// 核验引用属于目标 Session，且实际文件存在、大小与 SHA-256 完全一致。
    fn validate(&self, session_id: &SessionId, artifact: &ArtifactUse)
    -> Result<(), ResourceError>;

    /// 核验引用的实际字节能够按声明类型安全恢复。
    fn validate_materialization(
        &self,
        session_id: &SessionId,
        artifact: &ArtifactUse,
        materialization: ArtifactMaterialization,
    ) -> Result<(), ResourceError>;

    /// 验证文件快照全部块和整份字节摘要；不具备读取能力的实现默认拒绝。
    fn validate_file_snapshot(
        &self,
        _session_id: &SessionId,
        _snapshot: &crate::FileSnapshot,
    ) -> Result<(), ResourceError> {
        Err(ResourceError::Reduction(
            "Artifact 校验器不支持文件快照完整性核验".to_owned(),
        ))
    }
}

/// 读取 Artifact 后得到的明确内容类型，Binary 不会伪装成模型消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactMaterialized {
    /// 已完整验证的 UTF-8 文本。
    Utf8Text(String),
    /// 已绑定规范媒体类型的图片字节。
    Image {
        /// 图片原始字节。
        bytes: Vec<u8>,
        /// 首次写入时冻结的规范图片媒体类型。
        media_type: String,
    },
    /// 只用于审计、保存或下载的通用二进制内容。
    Binary {
        /// 二进制原始字节。
        bytes: Vec<u8>,
        /// 首次写入时冻结的可选规范媒体类型。
        media_type: Option<String>,
    },
}

/// 与内容文件一同持久化且冻结首次媒体类型的规范元数据。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ArtifactMetadata {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// 与内容 SHA-256 完全相同的 Artifact 标识。
    artifact_id: ArtifactId,
    /// 内容 SHA-256。
    sha256: String,
    /// 内容原始字节数。
    size_bytes: u64,
    /// 首次写入时冻结的规范媒体类型。
    media_type: Option<String>,
}

/// 单个 Session 的 Artifact 数量、大小和预览限制。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactLimits {
    /// 单个 Artifact 最大字节数。
    pub max_artifact_bytes: u64,
    /// 单个 Session 最多保存的不同内容数量。
    pub max_artifacts_per_session: usize,
    /// 事件与界面可直接展示的最大 UTF-8 预览字节数。
    pub max_preview_bytes: usize,
}

/// 当前 Session 已提交 Artifact 数量与配置上限的只读结构容量快照。
///
/// 此值不是 reservation，不能单独用于跨进程保留槽位。只有调用方同时持有
/// `SessionLease`，并由 Runtime 单写入口和内存 reservation 账本协调时，才能将
/// 此快照用于副作用发生前的容量保证。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactCapacity {
    /// pair 结构完整，且元数据身份、规范 MIME 与文件长度校验通过的唯一 Artifact 数量。
    pub committed_unique_artifacts: usize,
    /// 当前 ArtifactStore 配置允许保存的最大唯一 Artifact 数量。
    pub maximum_unique_artifacts: usize,
}

impl ArtifactCapacity {
    /// 返回当前仍可提交的唯一 Artifact 槽位；已有数量超出新配置时稳定返回零。
    pub const fn remaining(&self) -> usize {
        self.maximum_unique_artifacts
            .saturating_sub(self.committed_unique_artifacts)
    }
}

impl Default for ArtifactLimits {
    /// 返回适合本地编码会话的保守默认限制。
    fn default() -> Self {
        Self {
            max_artifact_bytes: 16 * 1024 * 1024,
            max_artifacts_per_session: 1_024,
            max_preview_bytes: 4 * 1024,
        }
    }
}

impl ArtifactLimits {
    /// 拒绝无法保存任何有效内容的零限制。
    fn validate(self) -> Result<Self, ResourceError> {
        if self.max_artifact_bytes == 0
            || self.max_artifacts_per_session == 0
            || self.max_preview_bytes == 0
        {
            return Err(ResourceError::UnsafePath(
                "Artifact 大小、数量和预览限制必须大于零".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// 不会截断 UTF-8 码点的 Artifact 文本预览。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactPreview {
    /// 最大限制内的有效 UTF-8 前缀。
    pub text: String,
    /// 是否没有展示全部原始字节。
    pub truncated: bool,
    /// 整个 Artifact 是否都是有效 UTF-8。
    pub source_is_utf8: bool,
}

/// 一个按内容寻址且可复核的 Artifact 引用。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    /// 与 SHA-256 十六进制相同的安全 Artifact 标识。
    pub artifact_id: ArtifactId,
    /// 小写十六进制 SHA-256。
    pub sha256: String,
    /// 原始字节数。
    pub size_bytes: u64,
    /// 可选标准媒体类型。
    pub media_type: Option<String>,
    /// 有界 UTF-8 预览。
    pub preview: ArtifactPreview,
}

impl ArtifactRef {
    /// 转换为可嵌入 Session 事件的精简引用。
    pub fn as_event_use(&self) -> ArtifactUse {
        ArtifactUse {
            artifact_id: self.artifact_id.clone(),
            sha256: self.sha256.clone(),
            size_bytes: self.size_bytes,
            media_type: self.media_type.clone(),
        }
    }
}

/// 按 Session 隔离、原子写入且内容寻址的大工具结果存储。
pub struct ArtifactStore {
    /// Session 标识。
    session_id: SessionId,
    /// 已验证且必须与 Runtime lease 完全匹配的 Session 目录。
    session_dir: PathBuf,
    /// 已验证的 Artifact 目录。
    artifacts_dir: PathBuf,
    /// 数量限制的跨实例协调锁。
    lock_path: PathBuf,
    /// 当前限制。
    limits: ArtifactLimits,
}

impl ArtifactStore {
    /// 打开或创建一个 Session 隔离 ArtifactStore。
    ///
    /// 路径隔离仅为尽力检查，不承诺抵御具有本机目录写权限的并发攻击者。
    pub fn open(
        storage_root: impl AsRef<Path>,
        session_id: SessionId,
        limits: ArtifactLimits,
    ) -> Result<Self, ResourceError> {
        let limits = limits.validate()?;
        let root = prepare_root(storage_root.as_ref())?;
        let sessions = secure_child_dir(&root, "sessions")?;
        let session_dir = secure_child_dir(&sessions, session_id.as_str())?;
        let artifacts_dir = secure_child_dir(&session_dir, "artifacts")?;
        let lock_path = session_dir.join("artifacts.lock");
        ensure_regular_file_or_absent(&lock_path)?;
        let lock = exclusive_lock(&lock_path)?;
        recover_artifact_directory(&artifacts_dir)?;
        drop(lock);
        Ok(Self {
            session_id,
            session_dir,
            artifacts_dir,
            lock_path,
            limits,
        })
    }

    /// 返回当前隔离的 Session 标识。
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 返回当前 ArtifactStore 使用的不可变大小、数量和预览限制。
    pub const fn limits(&self) -> &ArtifactLimits {
        &self.limits
    }

    /// 为原始快照块构造引用，复用已有内容首次冻结的媒体类型，不创建文件。
    pub(crate) fn file_snapshot_chunk_use(
        &self,
        bytes: &[u8],
    ) -> Result<ArtifactUse, ResourceError> {
        let sha256 = sha256_hex(bytes);
        let artifact_id = ArtifactId::new(sha256.clone())?;
        let mut reference = ArtifactUse {
            artifact_id,
            sha256,
            size_bytes: bytes.len() as u64,
            media_type: None,
        };
        let _lock = exclusive_lock(&self.lock_path)?;
        let path = self.artifact_path(&reference.artifact_id);
        let metadata_path = self.metadata_path(&reference.artifact_id);
        ensure_regular_file_or_absent(&path)?;
        ensure_regular_file_or_absent(&metadata_path)?;
        if path.exists() || metadata_path.exists() {
            reference.media_type = read_metadata(&metadata_path)?.media_type;
            self.read_verified_use_locked(&reference)?;
        }
        Ok(reference)
    }

    /// 在 Artifact 跨实例锁内恢复半提交文件并返回经过结构校验的容量快照。
    ///
    /// 此方法不会创建 reservation 或跨进程保留槽位；只有调用方同时持有
    /// `SessionLease`，并由 Runtime 单写入口和内存 reservation 账本协调时，才能将
    /// 快照用于副作用发生前的容量保证。恢复阶段可能删除没有形成完整提交的孤立内容、
    /// 元数据和 KeenCode 原子临时文件。容量热路径只读取有界元数据和内容文件长度，
    /// 不读取内容正文或重新计算 Hash；同尺寸内容篡改会保守占用槽位，并由 `read` 或
    /// `validate_use` 的完整校验拒绝。
    pub fn capacity(&self) -> Result<ArtifactCapacity, ResourceError> {
        let _lock = exclusive_lock(&self.lock_path)?;
        recover_artifact_directory(&self.artifacts_dir)?;
        let committed_unique_artifacts =
            validate_committed_artifact_pairs(&self.artifacts_dir, self.limits.max_artifact_bytes)?;
        Ok(ArtifactCapacity {
            committed_unique_artifacts,
            maximum_unique_artifacts: self.limits.max_artifacts_per_session,
        })
    }

    /// 在冷打开期间按健康权威状态回收完整但未提交引用的 Artifact pair。
    ///
    /// 调用方必须持有与当前 Store 的 Session 标识和规范目录都完全一致的独占
    /// [`SessionLease`]，并传入已经由健康 Journal 完整归约的 [`SessionState`]。
    /// 方法会先完整验证状态仍引用的每个实体，再删除其余完整 pair；任何校验、删除或
    /// 目录同步失败都会原样返回错误，调用方不得把该 Session 恢复为可运行状态。
    pub fn recover_for_state(
        &self,
        lease: &SessionLease,
        state: &SessionState,
    ) -> Result<ArtifactCapacity, ResourceError> {
        if lease.session_id() != &self.session_id
            || state.session_id != self.session_id
            || lease.session_dir() != self.session_dir
        {
            return Err(ResourceError::ArtifactScopeMismatch);
        }

        let _lock = exclusive_lock(&self.lock_path)?;
        recover_artifact_directory(&self.artifacts_dir)?;
        let committed = committed_artifact_identities(&self.artifacts_dir)?;
        // 在删除任何完整 pair 前校验全部快照的整体顺序和摘要；单块有效不足以
        // 证明一个未篡改的文件快照。这里已持有 Artifact 锁，禁止递归获取同一锁。
        for change in state
            .tools
            .values()
            .filter_map(|tool| tool.file_change.as_ref())
        {
            for snapshot in change.before.iter().chain(std::iter::once(&change.after)) {
                snapshot.validate_shape()?;
                let mut hash = Sha256::new();
                for chunk in &snapshot.chunks {
                    hash.update(self.read_verified_use_locked(chunk)?);
                }
                if format!("{:x}", hash.finalize()) != snapshot.sha256 {
                    return Err(ResourceError::ArtifactHashMismatch);
                }
            }
        }
        let references = state_artifact_references(state);
        let mut references_by_identity = BTreeMap::<&str, Vec<StateArtifactReference<'_>>>::new();
        for reference in references {
            references_by_identity
                .entry(reference.artifact.artifact_id.as_str())
                .or_default()
                .push(reference);
        }
        for references in references_by_identity.values() {
            let Some(first) = references.first() else {
                continue;
            };
            let bytes = self.read_verified_use_locked(first.artifact)?;
            for reference in references {
                if reference.artifact != first.artifact {
                    self.read_verified_use_locked(reference.artifact)?;
                }
                if let Some(materialization) = reference.materialization {
                    validate_materialized_bytes(reference.artifact, materialization, &bytes)?;
                }
            }
        }

        let mut removed = false;
        for identity in committed
            .iter()
            .map(String::as_str)
            .filter(|identity| !references_by_identity.contains_key(identity))
        {
            #[cfg(test)]
            fail_artifact_recovery_if_requested()?;
            removed |= remove_recoverable_artifact_file(
                &self.artifacts_dir.join(format!("{identity}.artifact")),
            )?;
            removed |= remove_recoverable_artifact_file(
                &self.artifacts_dir.join(format!("{identity}.metadata.json")),
            )?;
        }
        if removed {
            sync_directory(&self.artifacts_dir, true)?;
        }
        Ok(ArtifactCapacity {
            committed_unique_artifacts: references_by_identity.len(),
            maximum_unique_artifacts: self.limits.max_artifacts_per_session,
        })
    }

    /// 原子保存字节并返回带 Hash、大小和 UTF-8 预览的引用。
    pub fn put(
        &self,
        bytes: &[u8],
        media_type: Option<String>,
    ) -> Result<ArtifactRef, ResourceError> {
        let size_bytes = bytes.len() as u64;
        if size_bytes > self.limits.max_artifact_bytes {
            return Err(ResourceError::ArtifactTooLarge {
                actual: size_bytes,
                limit: self.limits.max_artifact_bytes,
            });
        }
        let media_type = canonical_media_type(media_type)?;
        let sha256 = sha256_hex(bytes);
        let artifact_id = ArtifactId::new(sha256.clone())?;
        let destination = self.artifact_path(&artifact_id);
        let metadata_path = self.metadata_path(&artifact_id);
        let metadata = ArtifactMetadata {
            schema: ARTIFACT_METADATA_SCHEMA.to_owned(),
            version: ARTIFACT_METADATA_VERSION,
            artifact_id: artifact_id.clone(),
            sha256: sha256.clone(),
            size_bytes,
            media_type: media_type.clone(),
        };
        let _lock = exclusive_lock(&self.lock_path)?;
        let committed_artifact_count = recover_artifact_directory(&self.artifacts_dir)?;
        ensure_regular_file_or_absent(&destination)?;
        ensure_regular_file_or_absent(&metadata_path)?;
        let content_exists = destination.exists();
        let metadata_exists = metadata_path.exists();
        if content_exists && metadata_exists {
            verify_existing(
                &destination,
                &sha256,
                size_bytes,
                self.limits.max_artifact_bytes,
            )?;
            let stored = read_metadata(&metadata_path)?;
            validate_metadata(&stored, &artifact_id, &sha256, size_bytes, &media_type)?;
        } else {
            if metadata_exists {
                remove_orphan_metadata(&metadata_path, &self.artifacts_dir)?;
            }
            if content_exists {
                verify_existing(
                    &destination,
                    &sha256,
                    size_bytes,
                    self.limits.max_artifact_bytes,
                )?;
            }
            if committed_artifact_count >= self.limits.max_artifacts_per_session {
                return Err(ResourceError::ArtifactCountLimit {
                    limit: self.limits.max_artifacts_per_session,
                });
            }
            if !content_exists {
                #[cfg(test)]
                fail_artifact_put_if(ArtifactPutFault::BeforeContent)?;
                atomic_write(&destination, bytes, true)?;
                #[cfg(test)]
                fail_artifact_put_if(ArtifactPutFault::ContentCommitted)?;
            }
            #[cfg(test)]
            fail_artifact_put_if(ArtifactPutFault::BeforeMetadata)?;
            write_metadata(&metadata_path, &metadata)?;
            #[cfg(test)]
            fail_artifact_put_if(ArtifactPutFault::MetadataCommitted)?;
        }
        Ok(ArtifactRef {
            artifact_id,
            sha256,
            size_bytes,
            media_type,
            preview: utf8_preview(bytes, self.limits.max_preview_bytes),
        })
    }

    /// 读取并重新验证一个属于当前 Session 的 Artifact。
    pub fn read(&self, reference: &ArtifactRef) -> Result<Vec<u8>, ResourceError> {
        self.read_verified_use(&reference.as_event_use())
    }

    /// 核验一个事件精简引用确实指向当前 Session 内的现有内容。
    pub fn validate_use(&self, reference: &ArtifactUse) -> Result<(), ResourceError> {
        <Self as ArtifactValidator>::validate(self, &self.session_id, reference)
    }

    /// 读取并重新校验一个事件中的精简 Artifact 引用。
    pub fn read_use(&self, reference: &ArtifactUse) -> Result<Vec<u8>, ResourceError> {
        self.read_verified_use(reference)
    }

    /// 读取引用并返回经过 UTF-8 或媒体类型约束验证的明确内容类型。
    pub fn materialize_use(
        &self,
        reference: &ArtifactUse,
        materialization: ArtifactMaterialization,
    ) -> Result<ArtifactMaterialized, ResourceError> {
        let bytes = self.read_verified_use(reference)?;
        match materialization {
            ArtifactMaterialization::Utf8Text => {
                if !utf8_materialization_matches(reference.media_type.as_deref(), &bytes) {
                    return Err(ResourceError::ArtifactMaterializationMismatch {
                        materialization: "utf8_text",
                    });
                }
                String::from_utf8(bytes)
                    .map(ArtifactMaterialized::Utf8Text)
                    .map_err(|_| ResourceError::ArtifactMaterializationMismatch {
                        materialization: "utf8_text",
                    })
            }
            ArtifactMaterialization::Image => {
                let media_type = reference.media_type.as_deref().ok_or(
                    ResourceError::ArtifactMaterializationMismatch {
                        materialization: "image",
                    },
                )?;
                if !image_materialization_matches(media_type) {
                    return Err(ResourceError::ArtifactMaterializationMismatch {
                        materialization: "image",
                    });
                }
                Ok(ArtifactMaterialized::Image {
                    bytes,
                    media_type: media_type.to_owned(),
                })
            }
            ArtifactMaterialization::Binary => Ok(ArtifactMaterialized::Binary {
                bytes,
                media_type: reference.media_type.clone(),
            }),
        }
    }

    /// 在同一 Artifact 锁和同一次文件读取内返回经过大小与摘要校验的字节。
    fn read_verified_use(&self, reference: &ArtifactUse) -> Result<Vec<u8>, ResourceError> {
        let _lock = exclusive_lock(&self.lock_path)?;
        self.read_verified_use_locked(reference)
    }

    /// 在调用方已持有 Artifact 锁时返回经过大小、摘要和元数据校验的字节。
    fn read_verified_use_locked(&self, reference: &ArtifactUse) -> Result<Vec<u8>, ResourceError> {
        if reference.artifact_id.as_str() != reference.sha256 {
            return Err(ResourceError::ArtifactHashMismatch);
        }
        let canonical_reference = canonical_media_type(reference.media_type.clone())
            .map_err(|_| ResourceError::ArtifactMediaTypeMismatch)?;
        if canonical_reference != reference.media_type {
            return Err(ResourceError::ArtifactMediaTypeMismatch);
        }
        if reference.size_bytes > self.limits.max_artifact_bytes {
            return Err(ResourceError::ArtifactTooLarge {
                actual: reference.size_bytes,
                limit: self.limits.max_artifact_bytes,
            });
        }
        let path = self.artifact_path(&reference.artifact_id);
        let metadata_path = self.metadata_path(&reference.artifact_id);
        ensure_regular_file_or_absent(&path)?;
        ensure_regular_file_or_absent(&metadata_path)?;
        let bytes = read_artifact_bounded(&path, self.limits.max_artifact_bytes)?;
        let actual_size = bytes.len() as u64;
        if actual_size != reference.size_bytes {
            return Err(ResourceError::ArtifactSizeMismatch {
                expected: reference.size_bytes,
                actual: actual_size,
            });
        }
        if sha256_hex(&bytes) != reference.sha256 {
            return Err(ResourceError::ArtifactHashMismatch);
        }
        let metadata = read_metadata(&metadata_path)?;
        validate_metadata(
            &metadata,
            &reference.artifact_id,
            &reference.sha256,
            reference.size_bytes,
            &reference.media_type,
        )?;
        Ok(bytes)
    }

    /// 读取精简引用并按当前有界策略生成 UTF-8 预览。
    pub fn preview_use(&self, reference: &ArtifactUse) -> Result<ArtifactPreview, ResourceError> {
        let bytes = self.read_use(reference)?;
        Ok(utf8_preview(&bytes, self.limits.max_preview_bytes))
    }

    /// 从经过校验的 ArtifactId 构造固定扩展名路径。
    fn artifact_path(&self, artifact_id: &ArtifactId) -> PathBuf {
        self.artifacts_dir
            .join(format!("{}.artifact", artifact_id.as_str()))
    }

    /// 从经过校验的 ArtifactId 构造固定元数据扩展名路径。
    fn metadata_path(&self, artifact_id: &ArtifactId) -> PathBuf {
        self.artifacts_dir
            .join(format!("{}.metadata.json", artifact_id.as_str()))
    }
}

/// 权威状态中的一个 Artifact 引用及其可选模型物化约束。
struct StateArtifactReference<'a> {
    /// 当前状态保存的完整内容寻址引用。
    artifact: &'a ArtifactUse,
    /// 消息或工具结果声明的恢复类型；仅审计引用为 `None`。
    materialization: Option<ArtifactMaterialization>,
}

/// 收集当前权威状态仍直接引用的全部 Artifact，保留重复引用以逐一校验元数据声明。
fn state_artifact_references(state: &SessionState) -> Vec<StateArtifactReference<'_>> {
    let mut references = Vec::new();
    for record in &state.transcript {
        match record {
            TranscriptRecord::MessageAdded(message) => {
                collect_message_artifacts(&message.content, &mut references);
            }
            TranscriptRecord::SegmentCommitted(segment) => {
                for message in &segment.messages {
                    collect_message_artifacts(&message.content, &mut references);
                }
            }
            TranscriptRecord::CompactionApplied(_) => {}
        }
    }
    for lifecycle in state.tools.values() {
        if let Some(outcome) = &lifecycle.outcome {
            collect_tool_result_artifacts(&outcome.result.content, &mut references);
        }
        if let Some(change) = &lifecycle.file_change {
            // Prepared 与 Applied 同样保留，崩溃或取消不能让写前证据被当孤儿回收。
            for snapshot in change.before.iter().chain(std::iter::once(&change.after)) {
                references.extend(
                    snapshot
                        .chunks
                        .iter()
                        .map(|artifact| StateArtifactReference {
                            artifact,
                            materialization: None,
                        }),
                );
            }
        }
    }
    for terminal in state.terminals.values() {
        references.extend(terminal.output_artifacts.iter().map(|artifact| {
            StateArtifactReference {
                artifact,
                materialization: None,
            }
        }));
    }
    if let Some(artifact) = &state.plan.plan_artifact {
        references.push(StateArtifactReference {
            artifact,
            materialization: None,
        });
    }
    for message in state.mailbox.values() {
        if let Some(artifact) = &message.artifact {
            references.push(StateArtifactReference {
                artifact,
                materialization: None,
            });
        }
    }
    references
}

/// 递归收集消息图片、大内容和内嵌工具结果中的 Artifact 引用。
fn collect_message_artifacts<'a>(
    parts: &'a [MessagePart],
    references: &mut Vec<StateArtifactReference<'a>>,
) {
    for part in parts {
        match part {
            MessagePart::Image {
                source: MessageImageSource::Artifact { artifact },
            } => references.push(StateArtifactReference {
                artifact,
                materialization: Some(ArtifactMaterialization::Image),
            }),
            MessagePart::Artifact {
                artifact,
                materialization,
            } => references.push(StateArtifactReference {
                artifact,
                materialization: Some(*materialization),
            }),
            MessagePart::ToolResult { content, .. } => {
                collect_tool_result_artifacts(content, references);
            }
            MessagePart::Text { .. }
            | MessagePart::Reasoning { .. }
            | MessagePart::ToolCall { .. }
            | MessagePart::Image {
                source: MessageImageSource::Url { .. },
            } => {}
        }
    }
}

/// 收集工具结果图片和大内容中的 Artifact 引用。
fn collect_tool_result_artifacts<'a>(
    parts: &'a [ToolResultPart],
    references: &mut Vec<StateArtifactReference<'a>>,
) {
    for part in parts {
        match part {
            ToolResultPart::Image {
                source: MessageImageSource::Artifact { artifact },
            } => references.push(StateArtifactReference {
                artifact,
                materialization: Some(ArtifactMaterialization::Image),
            }),
            ToolResultPart::Artifact {
                artifact,
                materialization,
            } => references.push(StateArtifactReference {
                artifact,
                materialization: Some(*materialization),
            }),
            ToolResultPart::Text { .. }
            | ToolResultPart::Image {
                source: MessageImageSource::Url { .. },
            } => {}
        }
    }
}

impl ArtifactValidator for ArtifactStore {
    /// 在同一 Session 内逐块复核并验证完整文件摘要，不将块媒体类型误当文件格式。
    fn validate_file_snapshot(
        &self,
        session_id: &SessionId,
        snapshot: &crate::FileSnapshot,
    ) -> Result<(), ResourceError> {
        if session_id != &self.session_id {
            return Err(ResourceError::ArtifactScopeMismatch);
        }
        self.validate_file_snapshot(snapshot)
    }

    /// 在 Artifact 锁内核验 Session 作用域、文件存在性、大小和内容摘要。
    fn validate(
        &self,
        session_id: &SessionId,
        artifact: &ArtifactUse,
    ) -> Result<(), ResourceError> {
        if session_id != &self.session_id {
            return Err(ResourceError::ArtifactScopeMismatch);
        }
        self.read_verified_use(artifact).map(drop)
    }

    /// 在实际读取、Hash 与媒体类型校验后验证声明的物化方式。
    fn validate_materialization(
        &self,
        session_id: &SessionId,
        artifact: &ArtifactUse,
        materialization: ArtifactMaterialization,
    ) -> Result<(), ResourceError> {
        if session_id != &self.session_id {
            return Err(ResourceError::ArtifactScopeMismatch);
        }
        self.materialize_use(artifact, materialization).map(drop)
    }
}

/// 在 Artifact 锁内回收半提交内容、孤立 marker 和原子临时文件，并返回完整 pair 数量。
fn recover_artifact_directory(directory: &Path) -> Result<usize, ResourceError> {
    let mut contents = BTreeSet::new();
    let mut metadata = BTreeSet::new();
    let mut temporary_files = Vec::new();
    for entry in
        fs::read_dir(directory).map_err(|error| ResourceError::io("list_artifacts", error))?
    {
        let entry = entry.map_err(|error| ResourceError::io("read_artifact_entry", error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| ResourceError::io("inspect_artifact_entry", error))?;
        if file_type.is_symlink() {
            return Err(ResourceError::SymlinkRejected(
                entry.file_name().to_string_lossy().into_owned(),
            ));
        }
        if !file_type.is_file() {
            return Err(ResourceError::UnsafePath(
                "Artifact 目录包含非普通文件条目".to_owned(),
            ));
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(ResourceError::UnsafePath(
                "Artifact 目录包含非 UTF-8 文件名".to_owned(),
            ));
        };
        if name.starts_with(ATOMIC_TEMP_PREFIX) {
            temporary_files.push(entry.path());
        } else if let Some(identity) = valid_artifact_file_identity(&name, ".artifact") {
            contents.insert(identity.to_owned());
        } else if let Some(identity) = valid_artifact_file_identity(&name, ".metadata.json") {
            metadata.insert(identity.to_owned());
        } else {
            return Err(ResourceError::UnsafePath(
                "Artifact 目录包含无法识别的普通文件".to_owned(),
            ));
        }
    }
    let committed_count = contents.intersection(&metadata).count();
    let mut removed = false;
    for identity in contents.difference(&metadata) {
        removed |=
            remove_recoverable_artifact_file(&directory.join(format!("{identity}.artifact")))?;
    }
    for identity in metadata.difference(&contents) {
        removed |=
            remove_recoverable_artifact_file(&directory.join(format!("{identity}.metadata.json")))?;
    }
    for path in temporary_files {
        removed |= remove_recoverable_artifact_file(&path)?;
    }
    if removed {
        sync_directory(directory, true)?;
    }
    Ok(committed_count)
}

/// 重新枚举恢复后的目录，并用有界元数据与文件长度校验每个已提交 pair。
fn validate_committed_artifact_pairs(
    directory: &Path,
    max_artifact_bytes: u64,
) -> Result<usize, ResourceError> {
    let contents = committed_artifact_identities(directory)?;
    for identity in &contents {
        let artifact_id = ArtifactId::new(identity.clone())?;
        let content_path = directory.join(format!("{identity}.artifact"));
        let metadata_path = directory.join(format!("{identity}.metadata.json"));
        ensure_regular_file_or_absent(&content_path)?;
        ensure_regular_file_or_absent(&metadata_path)?;
        let stored = read_metadata(&metadata_path)?;
        validate_capacity_metadata(&stored, &artifact_id, identity)?;
        let content_metadata = fs::metadata(&content_path)
            .map_err(|error| ResourceError::io("inspect_artifact_content", error))?;
        let actual_size = content_metadata.len();
        if stored.size_bytes > max_artifact_bytes {
            return Err(ResourceError::ArtifactTooLarge {
                actual: stored.size_bytes,
                limit: max_artifact_bytes,
            });
        }
        if actual_size > max_artifact_bytes {
            return Err(ResourceError::ArtifactTooLarge {
                actual: actual_size,
                limit: max_artifact_bytes,
            });
        }
        if actual_size != stored.size_bytes {
            return Err(ResourceError::ArtifactSizeMismatch {
                expected: stored.size_bytes,
                actual: actual_size,
            });
        }
    }
    Ok(contents.len())
}

/// 枚举恢复后目录中的完整 Artifact pair，并拒绝所有不安全或不成对条目。
fn committed_artifact_identities(directory: &Path) -> Result<BTreeSet<String>, ResourceError> {
    let mut contents = BTreeSet::new();
    let mut metadata = BTreeSet::new();
    for entry in
        fs::read_dir(directory).map_err(|error| ResourceError::io("list_artifacts", error))?
    {
        let entry = entry.map_err(|error| ResourceError::io("read_artifact_entry", error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| ResourceError::io("inspect_artifact_entry", error))?;
        if file_type.is_symlink() {
            return Err(ResourceError::SymlinkRejected(
                entry.file_name().to_string_lossy().into_owned(),
            ));
        }
        if !file_type.is_file() {
            return Err(ResourceError::UnsafePath(
                "Artifact 目录包含非普通文件条目".to_owned(),
            ));
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(ResourceError::UnsafePath(
                "Artifact 目录包含非 UTF-8 文件名".to_owned(),
            ));
        };
        if let Some(identity) = valid_artifact_file_identity(&name, ".artifact") {
            contents.insert(identity.to_owned());
        } else if let Some(identity) = valid_artifact_file_identity(&name, ".metadata.json") {
            metadata.insert(identity.to_owned());
        } else {
            return Err(ResourceError::UnsafePath(
                "Artifact 恢复后目录包含无法识别的普通文件".to_owned(),
            ));
        }
    }
    if contents != metadata {
        return Err(ResourceError::ArtifactMetadataMismatch);
    }
    Ok(contents)
}

/// 校验容量快照所需的元数据身份与规范 MIME 结构，不读取内容或推断首次写入 MIME。
fn validate_capacity_metadata(
    metadata: &ArtifactMetadata,
    artifact_id: &ArtifactId,
    file_identity: &str,
) -> Result<(), ResourceError> {
    let canonical_stored = canonical_media_type(metadata.media_type.clone())
        .map_err(|_| ResourceError::ArtifactMetadataMismatch)?;
    if metadata.schema != ARTIFACT_METADATA_SCHEMA
        || metadata.version != ARTIFACT_METADATA_VERSION
        || metadata.artifact_id != *artifact_id
        || metadata.sha256 != file_identity
        || canonical_stored != metadata.media_type
    {
        return Err(ResourceError::ArtifactMetadataMismatch);
    }
    Ok(())
}

/// 从受支持文件名中提取并重新校验小写 SHA-256 Artifact 标识。
fn valid_artifact_file_identity<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let identity = name.strip_suffix(suffix)?;
    ArtifactId::new(identity.to_owned()).ok()?;
    Some(identity)
}

/// 在删除前再次拒绝符号链接或非普通文件，并容忍并发消失的恢复目标。
fn remove_recoverable_artifact_file(path: &Path) -> Result<bool, ResourceError> {
    ensure_regular_file_or_absent(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ResourceError::io("remove_artifact_orphan", error)),
    }
}

/// 删除没有内容文件支撑的旧 metadata marker，使未完成提交不会冻结媒体类型。
fn remove_orphan_metadata(path: &Path, directory: &Path) -> Result<(), ResourceError> {
    match fs::remove_file(path) {
        Ok(()) => sync_directory(directory, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ResourceError::io("remove_orphan_artifact_metadata", error)),
    }
}

/// 校验去重命中的已有文件确实包含相同内容。
fn verify_existing(
    path: &Path,
    expected_hash: &str,
    expected_size: u64,
    max_artifact_bytes: u64,
) -> Result<(), ResourceError> {
    let bytes = read_artifact_bounded(path, max_artifact_bytes)?;
    if bytes.len() as u64 != expected_size || sha256_hex(&bytes) != expected_hash {
        return Err(ResourceError::ArtifactHashMismatch);
    }
    Ok(())
}

/// 一个已经完整解析的媒体类型及其无重复参数。
struct ParsedMediaType {
    /// 规范化为小写的顶级类型。
    kind: String,
    /// 规范化为小写的子类型。
    subtype: String,
    /// 参数名小写且按名称稳定排序的已解码值。
    parameters: BTreeMap<String, String>,
}

impl ParsedMediaType {
    /// 按 RFC token、quoted-string 与 RFC 6838 restricted-name 约束完整解析输入。
    fn parse(input: &str) -> Result<Self, ResourceError> {
        if input.is_empty()
            || input.len() > 1_024
            || !input.is_ascii()
            || input
                .bytes()
                .any(|byte| byte == 0x7f || byte < b' ' && byte != b'\t')
        {
            return Err(invalid_media_type());
        }
        let mut parser = MediaTypeParser {
            bytes: input.as_bytes(),
            cursor: 0,
        };
        parser.skip_ows();
        let kind = parser.parse_restricted_name()?.to_ascii_lowercase();
        parser.expect(b'/')?;
        let subtype = parser.parse_restricted_name()?.to_ascii_lowercase();
        parser.skip_ows();

        let mut parameters = BTreeMap::new();
        while !parser.is_finished() {
            parser.expect(b';')?;
            parser.skip_ows();
            if parser.is_finished() {
                return Err(invalid_media_type());
            }
            let name = parser.parse_token()?.to_ascii_lowercase();
            parser.expect(b'=')?;
            let mut value = parser.parse_parameter_value()?;
            if value.is_empty() {
                return Err(invalid_media_type());
            }
            if name == "charset" {
                value.make_ascii_lowercase();
            }
            if parameters.insert(name, value).is_some() {
                return Err(invalid_media_type());
            }
            parser.skip_ows();
            if !parser.is_finished() && parser.peek() != Some(b';') {
                return Err(invalid_media_type());
            }
        }
        Ok(Self {
            kind,
            subtype,
            parameters,
        })
    }

    /// 输出能够稳定比较等价 MIME 的规范字符串。
    fn canonical(&self) -> String {
        let mut output = format!("{}/{}", self.kind, self.subtype);
        for (name, value) in &self.parameters {
            output.push_str("; ");
            output.push_str(name);
            output.push('=');
            append_canonical_parameter_value(&mut output, value);
        }
        output
    }
}

/// 媒体类型的有界 ASCII 游标解析器。
struct MediaTypeParser<'a> {
    /// 待解析的完整 ASCII 字节。
    bytes: &'a [u8],
    /// 下一待消费字节下标。
    cursor: usize,
}

impl<'a> MediaTypeParser<'a> {
    /// 判断输入是否已经全部消费。
    fn is_finished(&self) -> bool {
        self.cursor == self.bytes.len()
    }

    /// 查看当前字节但不推进游标。
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    /// 跳过 RFC OWS 允许的空格或水平制表符。
    fn skip_ows(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.cursor += 1;
        }
    }

    /// 消费一个精确分隔符，否则拒绝整个媒体类型。
    fn expect(&mut self, expected: u8) -> Result<(), ResourceError> {
        if self.peek() != Some(expected) {
            return Err(invalid_media_type());
        }
        self.cursor += 1;
        Ok(())
    }

    /// 读取长度不超过 127 且首字符为字母数字的 RFC 6838 restricted-name。
    fn parse_restricted_name(&mut self) -> Result<&'a str, ResourceError> {
        let start = self.cursor;
        while self.peek().is_some_and(valid_restricted_name_byte) {
            self.cursor += 1;
        }
        let value = &self.bytes[start..self.cursor];
        if value.is_empty() || value.len() > 127 || !value[0].is_ascii_alphanumeric() {
            return Err(invalid_media_type());
        }
        std::str::from_utf8(value).map_err(|_| invalid_media_type())
    }

    /// 读取至少一个 RFC tchar 组成的参数 token。
    fn parse_token(&mut self) -> Result<&'a str, ResourceError> {
        let start = self.cursor;
        while self.peek().is_some_and(valid_token_byte) {
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(invalid_media_type());
        }
        std::str::from_utf8(&self.bytes[start..self.cursor]).map_err(|_| invalid_media_type())
    }

    /// 读取一个非空 token 或完整 quoted-string 参数值。
    fn parse_parameter_value(&mut self) -> Result<String, ResourceError> {
        if self.peek() == Some(b'"') {
            self.parse_quoted_string()
        } else {
            self.parse_token().map(str::to_owned)
        }
    }

    /// 解码 quoted-string，支持引号内分号及反斜杠 quoted-pair。
    fn parse_quoted_string(&mut self) -> Result<String, ResourceError> {
        self.expect(b'"')?;
        let mut output = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(invalid_media_type());
            };
            match byte {
                b'"' => {
                    self.cursor += 1;
                    return Ok(output);
                }
                b'\\' => {
                    self.cursor += 1;
                    let escaped = self.peek().ok_or_else(invalid_media_type)?;
                    if !valid_quoted_pair_byte(escaped) {
                        return Err(invalid_media_type());
                    }
                    output.push(char::from(escaped));
                    self.cursor += 1;
                }
                byte if valid_quoted_text_byte(byte) => {
                    output.push(char::from(byte));
                    self.cursor += 1;
                }
                _ => return Err(invalid_media_type()),
            }
        }
    }
}

/// 把媒体类型完整解析并输出稳定 type/subtype、参数顺序、引号和转义。
fn canonical_media_type(media_type: Option<String>) -> Result<Option<String>, ResourceError> {
    media_type
        .as_deref()
        .map(ParsedMediaType::parse)
        .transpose()
        .map(|parsed| parsed.map(|media_type| media_type.canonical()))
}

/// 判断 type/subtype restricted-name 后续字符是否合法。
fn valid_restricted_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
        )
}

/// 判断参数名和值的未引用形式是否为 RFC tchar。
fn valid_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// 判断 quoted-string 内未转义字节是否属于 qdtext。
fn valid_quoted_text_byte(byte: u8) -> bool {
    matches!(byte, b'\t' | b' ' | b'!' | 0x23..=0x5b | 0x5d..=0x7e)
}

/// 判断反斜杠后的 quoted-pair 字节是否可安全解码。
fn valid_quoted_pair_byte(byte: u8) -> bool {
    matches!(byte, b'\t' | b' '..=b'~')
}

/// 以 token 或带必要转义的 quoted-string 追加规范参数值。
fn append_canonical_parameter_value(output: &mut String, value: &str) {
    if !value.is_empty() && value.bytes().all(valid_token_byte) {
        output.push_str(value);
        return;
    }
    output.push('"');
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            output.push('\\');
        }
        output.push(character);
    }
    output.push('"');
}

/// 判断字节与冻结媒体类型是否可安全恢复成 UTF-8 模型文本。
fn utf8_materialization_matches(media_type: Option<&str>, bytes: &[u8]) -> bool {
    if std::str::from_utf8(bytes).is_err() {
        return false;
    }
    if !text_materialization_media_type_matches(media_type) {
        return false;
    }
    let Some(media_type) = media_type else {
        return true;
    };
    let parsed = ParsedMediaType::parse(media_type).expect("媒体类型已经由共享文本判定完整解析");
    match parsed.parameters.get("charset").map(String::as_str) {
        None | Some("utf-8") => true,
        Some("us-ascii") => bytes.is_ascii(),
        Some(_) => false,
    }
}

/// 验证已完成实体与元数据校验的字节符合状态声明的模型物化方式。
fn validate_materialized_bytes(
    reference: &ArtifactUse,
    materialization: ArtifactMaterialization,
    bytes: &[u8],
) -> Result<(), ResourceError> {
    let matches = match materialization {
        ArtifactMaterialization::Utf8Text => {
            utf8_materialization_matches(reference.media_type.as_deref(), bytes)
        }
        ArtifactMaterialization::Image => reference
            .media_type
            .as_deref()
            .is_some_and(image_materialization_matches),
        ArtifactMaterialization::Binary => true,
    };
    if matches {
        Ok(())
    } else {
        Err(ResourceError::ArtifactMaterializationMismatch {
            materialization: match materialization {
                ArtifactMaterialization::Utf8Text => "utf8_text",
                ArtifactMaterialization::Image => "image",
                ArtifactMaterialization::Binary => "binary",
            },
        })
    }
}

/// 判断声明的媒体类型是否允许作为模型 UTF-8 文本；实际字节约束由 ArtifactStore 补充。
pub(crate) fn text_materialization_media_type_matches(media_type: Option<&str>) -> bool {
    let Some(media_type) = media_type else {
        return true;
    };
    let Ok(parsed) = ParsedMediaType::parse(media_type) else {
        return false;
    };
    let is_textual = parsed.kind == "text"
        || parsed.kind == "application"
            && (parsed.subtype == "json"
                || parsed.subtype.ends_with("+json")
                || parsed.subtype == "xml"
                || parsed.subtype.ends_with("+xml"));
    is_textual
        && matches!(
            parsed.parameters.get("charset").map(String::as_str),
            None | Some("utf-8") | Some("us-ascii")
        )
}

/// 判断冻结媒体类型是否是完整有效的 image/* 类型。
pub(crate) fn image_materialization_matches(media_type: &str) -> bool {
    ParsedMediaType::parse(media_type).is_ok_and(|parsed| parsed.kind == "image")
}

/// 构造不回显用户媒体类型正文的稳定解析错误。
fn invalid_media_type() -> ResourceError {
    ResourceError::UnsafePath("Artifact media type 无效".to_owned())
}

/// 原子写入冻结媒体类型与内容身份的 Artifact 元数据。
fn write_metadata(path: &Path, metadata: &ArtifactMetadata) -> Result<(), ResourceError> {
    let mut bytes =
        serde_json::to_vec(metadata).map_err(|error| ResourceError::Json(error.to_string()))?;
    bytes.push(b'\n');
    atomic_write(path, &bytes, true)
}

/// 有界读取并解析 Artifact 规范元数据。
fn read_metadata(path: &Path) -> Result<ArtifactMetadata, ResourceError> {
    let bytes = match read_file_bounded(path, ARTIFACT_METADATA_MAX_BYTES) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::TooLarge { .. }) | Err(_) => {
            return Err(ResourceError::ArtifactMetadataMismatch);
        }
    };
    serde_json::from_slice(&bytes).map_err(|_| ResourceError::ArtifactMetadataMismatch)
}

/// 校验元数据身份、大小和首次写入媒体类型没有被重新标注。
fn validate_metadata(
    metadata: &ArtifactMetadata,
    artifact_id: &ArtifactId,
    sha256: &str,
    size_bytes: u64,
    media_type: &Option<String>,
) -> Result<(), ResourceError> {
    let canonical_stored = canonical_media_type(metadata.media_type.clone())
        .map_err(|_| ResourceError::ArtifactMetadataMismatch)?;
    if metadata.schema != ARTIFACT_METADATA_SCHEMA
        || metadata.version != ARTIFACT_METADATA_VERSION
        || metadata.artifact_id != *artifact_id
        || metadata.sha256 != sha256
        || metadata.size_bytes != size_bytes
        || canonical_stored != metadata.media_type
    {
        return Err(ResourceError::ArtifactMetadataMismatch);
    }
    if metadata.media_type != *media_type {
        return Err(ResourceError::ArtifactMediaTypeMismatch);
    }
    Ok(())
}

/// 使用同一文件句柄执行大小检查，并拒绝超过 Artifact 上限的内容。
fn read_artifact_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ResourceError> {
    match read_file_bounded(path, limit) {
        Ok(BoundedRead::Bytes(bytes)) => Ok(bytes),
        Ok(BoundedRead::TooLarge { actual }) => {
            Err(ResourceError::ArtifactTooLarge { actual, limit })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(ResourceError::ArtifactNotFound)
        }
        Err(error) => Err(ResourceError::io("read_artifact", error)),
    }
}

/// Artifact 双文件提交测试使用的一次性故障位置。
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactPutFault {
    /// 内容临时文件写入前失败。
    BeforeContent,
    /// 内容已原子落盘但 metadata 尚未提交时失败。
    ContentCommitted,
    /// 即将写入 metadata commit marker 时失败。
    BeforeMetadata,
    /// metadata commit marker 已落盘但调用尚未返回时失败。
    MetadataCommitted,
}

#[cfg(test)]
thread_local! {
    /// 当前测试线程下一次要触发的 Artifact 提交故障。
    static ARTIFACT_PUT_FAULT: std::cell::RefCell<Option<ArtifactPutFault>> = const { std::cell::RefCell::new(None) };
    /// 当前测试线程下一次冷恢复删除是否必须确定性失败。
    static ARTIFACT_RECOVERY_DELETE_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 为当前测试线程设置一个必须被消费的一次性 Artifact 提交故障。
#[cfg(test)]
fn set_artifact_put_fault(fault: ArtifactPutFault) {
    ARTIFACT_PUT_FAULT.with(|current| {
        let previous = current.replace(Some(fault));
        assert!(
            previous.is_none(),
            "Artifact 故障必须在设置下一故障前被消费"
        );
    });
}

/// 在匹配位置消费测试故障并返回不依赖平台的 IO 错误。
#[cfg(test)]
fn fail_artifact_put_if(fault: ArtifactPutFault) -> Result<(), ResourceError> {
    let matched = ARTIFACT_PUT_FAULT.with(|current| {
        if *current.borrow() == Some(fault) {
            current.replace(None);
            true
        } else {
            false
        }
    });
    if matched {
        Err(ResourceError::io(
            "artifact_put_fault",
            std::io::Error::other("keencode-resources Artifact 测试注入故障"),
        ))
    } else {
        Ok(())
    }
}

/// 为当前测试线程设置一次必须由冷恢复删除消费的确定性故障。
#[cfg(test)]
fn set_artifact_recovery_delete_fault() {
    ARTIFACT_RECOVERY_DELETE_FAULT.with(|current| {
        assert!(!current.replace(true), "冷恢复删除故障不得重复设置");
    });
}

/// 消费一次冷恢复删除故障，并返回不依赖操作系统权限语义的 IO 错误。
#[cfg(test)]
fn fail_artifact_recovery_if_requested() -> Result<(), ResourceError> {
    let matched = ARTIFACT_RECOVERY_DELETE_FAULT.with(|current| current.replace(false));
    if matched {
        Err(ResourceError::io(
            "remove_unreferenced_artifact",
            std::io::Error::other("keencode-resources Artifact 冷恢复测试注入故障"),
        ))
    } else {
        Ok(())
    }
}

/// 生成不切断 UTF-8 码点且明确标记二进制来源的预览。
fn utf8_preview(bytes: &[u8], max_bytes: usize) -> ArtifactPreview {
    let source = std::str::from_utf8(bytes);
    let source_is_utf8 = source.is_ok();
    let valid_prefix_len = match &source {
        Ok(_) => bytes.len(),
        Err(error) => error.valid_up_to(),
    };
    let mut end = valid_prefix_len.min(max_bytes);
    if let Ok(text) = source {
        while !text.is_char_boundary(end) {
            end -= 1;
        }
    }
    let text = std::str::from_utf8(&bytes[..end])
        .unwrap_or_default()
        .to_owned();
    ArtifactPreview {
        text,
        truncated: end < bytes.len(),
        source_is_utf8,
    }
}

/// 计算小写十六进制 SHA-256。
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentId, MailboxMessage, MailboxMessageId, MailboxState, MessageRole, PersistedToolResult,
        PlanState, RequestId, SessionLeaseAcquire, SessionMessage, TerminalId, TerminalRecord,
        ToolCompletionStatus, ToolEffect, ToolLifecycle, ToolOutcome, ToolRequest, TurnId,
    };

    /// 创建一个限制明确且 Session 隔离的测试 ArtifactStore。
    fn test_store(root: &Path, session: &str, max_artifacts: usize) -> ArtifactStore {
        ArtifactStore::open(
            root,
            SessionId::new(session).expect("Session ID 应有效"),
            ArtifactLimits {
                max_artifact_bytes: 1024 * 1024,
                max_artifacts_per_session: max_artifacts,
                max_preview_bytes: 1024,
            },
        )
        .expect("ArtifactStore 应打开")
    }

    /// 为测试取得一个必须保持到冷恢复结束的独占 Session lease。
    fn test_lease(root: &Path, session_id: &SessionId) -> SessionLease {
        match SessionLease::try_acquire(root, session_id.clone()).expect("Session lease 应获取")
        {
            SessionLeaseAcquire::Acquired(lease) => lease,
            SessionLeaseAcquire::Busy { .. } => panic!("测试 Session 不应被其他 Runtime 占用"),
        }
    }

    /// 创建合法 Artifact 后篡改单一元数据字段，并断言容量查询失败关闭。
    fn assert_capacity_metadata_mismatch(
        session: &str,
        mutate: impl FnOnce(&mut ArtifactMetadata),
    ) {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let store = test_store(root.path(), session, 2);
        let reference = store
            .put(b"capacity metadata", Some("text/plain".to_owned()))
            .expect("基准 Artifact 应提交");
        let metadata_path = store.metadata_path(&reference.artifact_id);
        let mut metadata = read_metadata(&metadata_path).expect("基准元数据应读取");
        mutate(&mut metadata);
        write_metadata(&metadata_path, &metadata).expect("篡改元数据夹具应写入");
        assert!(matches!(
            store.capacity(),
            Err(ResourceError::ArtifactMetadataMismatch)
        ));
    }

    /// 验证冷恢复只保留健康状态在全部持久位置中引用的 Artifact，并释放其余容量。
    #[test]
    fn artifact冷恢复按权威状态回收完整孤儿() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("cold-state-gc").expect("Session ID 应有效");
        let store = test_store(root.path(), session_id.as_str(), 8);
        let lease = test_lease(root.path(), &session_id);
        let transcript = store
            .put(b"transcript", Some("text/plain".to_owned()))
            .expect("Transcript Artifact 应提交");
        let tool = store
            .put(b"tool", Some("text/plain".to_owned()))
            .expect("工具 Artifact 应提交");
        let terminal = store.put(b"terminal", None).expect("终端 Artifact 应提交");
        let plan = store.put(b"plan", None).expect("Plan Artifact 应提交");
        let mailbox = store.put(b"mailbox", None).expect("邮箱 Artifact 应提交");
        let orphan = store
            .put(b"unreferenced", None)
            .expect("孤儿 Artifact 应提交");

        let turn_id = TurnId::new("turn-cold-gc").expect("Turn ID 应有效");
        let root_agent = AgentId::new("root").expect("Agent ID 应有效");
        let request_id = RequestId::derive_model_tool_call(
            &session_id,
            &turn_id,
            &root_agent,
            1,
            "call-cold-gc",
        )
        .expect("Request ID 应派生");
        let terminal_id = TerminalId::new("terminal-cold-gc").expect("Terminal ID 应有效");
        let mailbox_id = MailboxMessageId::new("mail-cold-gc").expect("邮箱 ID 应有效");
        let mut state = SessionState::empty(session_id.clone());
        state
            .transcript
            .push(TranscriptRecord::MessageAdded(SessionMessage {
                message_id: "message-cold-gc".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::System,
                content: vec![
                    MessagePart::Text {
                        text: "已保存 Transcript Artifact".to_owned(),
                    },
                    MessagePart::Artifact {
                        artifact: transcript.as_event_use(),
                        materialization: ArtifactMaterialization::Utf8Text,
                    },
                ],
            }));
        state.tools.insert(
            request_id.clone(),
            ToolLifecycle {
                request: ToolRequest {
                    request_id: request_id.clone(),
                    turn_id: turn_id.clone(),
                    agent_id: root_agent.clone(),
                    model_round: 1,
                    request_index: 0,
                    model_tool_call_id: "call-cold-gc".to_owned(),
                    tool_name: "read".to_owned(),
                    arguments: serde_json::json!({}),
                    effect: ToolEffect::ReadOnly,
                },
                requested_at_unix_ms: 1,
                execution_started: true,
                execution_started_at_unix_ms: Some(2),
                file_change: None,
                outcome: Some(ToolOutcome {
                    status: ToolCompletionStatus::Succeeded,
                    result: PersistedToolResult {
                        tool_call_id: "call-cold-gc".to_owned(),
                        content: vec![ToolResultPart::Artifact {
                            artifact: tool.as_event_use(),
                            materialization: ArtifactMaterialization::Utf8Text,
                        }],
                        is_error: false,
                    },
                }),
                completed_at_unix_ms: Some(3),
                transcript_segment: None,
            },
        );
        state.terminals.insert(
            terminal_id.clone(),
            TerminalRecord {
                terminal_id,
                request_id,
                command_display: "test".to_owned(),
                working_directory: "D:/workspace".to_owned(),
                output_artifacts: vec![terminal.as_event_use()],
                exit_code: Some(0),
                cancelled: false,
                exited: true,
            },
        );
        state.plan = PlanState {
            enabled: true,
            plan_artifact: Some(plan.as_event_use()),
        };
        state.mailbox.insert(
            mailbox_id.clone(),
            MailboxMessage {
                message_id: mailbox_id,
                from: root_agent.clone(),
                to: root_agent,
                related_turn_id: turn_id,
                body: "Artifact 报告".to_owned(),
                artifact: Some(mailbox.as_event_use()),
                state: MailboxState::Delivered,
            },
        );

        let recovered = store
            .recover_for_state(&lease, &state)
            .expect("冷恢复应删除完整孤儿");
        assert_eq!(recovered.committed_unique_artifacts, 5);
        assert_eq!(recovered.remaining(), 3);
        for reference in [&transcript, &tool, &terminal, &plan, &mailbox] {
            store.read(reference).expect("权威状态引用必须保留");
        }
        assert!(matches!(
            store.read(&orphan),
            Err(ResourceError::ArtifactNotFound)
        ));
        assert_eq!(
            store
                .recover_for_state(&lease, &state)
                .expect("重复冷恢复必须幂等"),
            recovered
        );
    }

    /// 验证 Session、存储根或状态身份不匹配时不能借用其他 Runtime lease 执行 GC。
    #[test]
    fn artifact冷恢复要求精确匹配的session_lease() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let other_root = tempfile::tempdir().expect("另一临时目录应创建");
        let session_id = SessionId::new("cold-lease-scope").expect("Session ID 应有效");
        let store = test_store(root.path(), session_id.as_str(), 2);
        store.put(b"orphan", None).expect("孤儿 Artifact 应提交");
        let foreign_root_lease = test_lease(other_root.path(), &session_id);
        let state = SessionState::empty(session_id.clone());
        assert!(matches!(
            store.recover_for_state(&foreign_root_lease, &state),
            Err(ResourceError::ArtifactScopeMismatch)
        ));
        assert_eq!(
            store
                .capacity()
                .expect("拒绝错误 lease 后内容必须保留")
                .committed_unique_artifacts,
            1
        );

        let matching_lease = test_lease(root.path(), &session_id);
        let wrong_state = SessionState::empty(
            SessionId::new("cold-lease-other-session").expect("替代 Session ID 应有效"),
        );
        assert!(matches!(
            store.recover_for_state(&matching_lease, &wrong_state),
            Err(ResourceError::ArtifactScopeMismatch)
        ));
    }

    /// 验证冷恢复删除失败会向上返回错误且不会把 Session 伪装成已经恢复健康。
    #[test]
    fn artifact冷恢复删除失败必须失败关闭() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("cold-delete-failure").expect("Session ID 应有效");
        let store = test_store(root.path(), session_id.as_str(), 2);
        let lease = test_lease(root.path(), &session_id);
        store.put(b"orphan", None).expect("孤儿 Artifact 应提交");
        let state = SessionState::empty(session_id);

        set_artifact_recovery_delete_fault();
        assert!(matches!(
            store.recover_for_state(&lease, &state),
            Err(ResourceError::Io {
                operation: "remove_unreferenced_artifact",
                ..
            })
        ));
        assert_eq!(
            store
                .capacity()
                .expect("注入发生在删除前，完整 pair 必须仍存在")
                .committed_unique_artifacts,
            1
        );
        assert_eq!(
            store
                .recover_for_state(&lease, &state)
                .expect("故障消费后冷恢复应成功")
                .committed_unique_artifacts,
            0
        );
    }

    /// 验证预览不会从多字节 UTF-8 字符中间截断。
    #[test]
    fn utf8_preview_保持字符边界() {
        let preview = utf8_preview("A中B".as_bytes(), 2);
        assert_eq!(preview.text, "A");
        assert!(preview.truncated);
        assert!(preview.source_is_utf8);
    }

    /// 验证二进制预览只保留首个无损 UTF-8 前缀。
    #[test]
    fn utf8_preview_二进制不插入替换字符() {
        let preview = utf8_preview(&[b'a', b'b', 0xff, b'c'], 16);
        assert_eq!(preview.text, "ab");
        assert!(preview.truncated);
        assert!(!preview.source_is_utf8);
    }

    /// 验证 quoted-string、参数排序和已知 charset 能生成稳定规范 MIME。
    #[test]
    fn media_type_严格解析并稳定规范化() {
        let quoted = canonical_media_type(Some(
            r#" Text/Plain; Note="a;b\"c\\d"; Charset="UTF-8" "#.to_owned(),
        ))
        .expect("合法 quoted-string 应解析")
        .expect("媒体类型应存在");
        assert_eq!(quoted, r#"text/plain; charset=utf-8; note="a;b\"c\\d""#);
        assert_eq!(
            canonical_media_type(Some(r#"text/plain;charset=utf-8"#.to_owned()))
                .expect("token charset 应解析"),
            canonical_media_type(Some(r#"Text/Plain; Charset="UTF-8""#.to_owned()))
                .expect("quoted charset 应解析")
        );
        assert_eq!(
            canonical_media_type(Some("application/problem+json; profile=v1".to_owned()))
                .expect("结构化 vendor MIME 应解析")
                .as_deref(),
            Some("application/problem+json; profile=v1")
        );
    }

    /// 验证缺值、重复参数、残缺引号和非法 restricted-name 都被完整拒绝。
    #[test]
    fn media_type_拒绝所有残缺或歧义语法() {
        let oversized_kind = format!("{}/plain", "a".repeat(128));
        let invalid = [
            "text/plain; charset".to_owned(),
            "text/plain; charset=".to_owned(),
            "text/plain; charset=\"\"".to_owned(),
            "text/plain; =utf-8".to_owned(),
            "text/plain; charset=utf-8; CHARSET=us-ascii".to_owned(),
            "text/plain; note=\"unterminated".to_owned(),
            "text/plain; note=\"dangling\\".to_owned(),
            "text/plain; note=has space".to_owned(),
            "text/plain; note=a/b".to_owned(),
            "text/plain;".to_owned(),
            "text /plain".to_owned(),
            "text/plain; charset =utf-8".to_owned(),
            "!text/plain".to_owned(),
            "text/!plain".to_owned(),
            oversized_kind,
        ];
        for media_type in invalid {
            assert!(
                canonical_media_type(Some(media_type)).is_err(),
                "非法 MIME 必须被拒绝"
            );
        }
    }

    /// 验证内容先写、metadata 后提交的每个故障窗口都可确定性重试。
    #[test]
    fn artifact双文件提交故障可恢复且未提交mime不冻结() {
        for (index, fault) in [
            ArtifactPutFault::BeforeContent,
            ArtifactPutFault::ContentCommitted,
            ArtifactPutFault::BeforeMetadata,
            ArtifactPutFault::MetadataCommitted,
        ]
        .into_iter()
        .enumerate()
        {
            let root = tempfile::tempdir().expect("临时目录应创建");
            let store = test_store(root.path(), &format!("fault-{index}"), 1);
            let bytes = b"recoverable artifact";
            let artifact_id = ArtifactId::new(sha256_hex(bytes)).expect("Artifact ID 应有效");
            let content_path = store.artifact_path(&artifact_id);
            let metadata_path = store.metadata_path(&artifact_id);

            set_artifact_put_fault(fault);
            assert!(
                store.put(bytes, Some("image/png".to_owned())).is_err(),
                "注入故障必须返回错误"
            );
            match fault {
                ArtifactPutFault::BeforeContent => {
                    assert!(!content_path.exists());
                    assert!(!metadata_path.exists());
                }
                ArtifactPutFault::ContentCommitted | ArtifactPutFault::BeforeMetadata => {
                    assert!(content_path.exists());
                    assert!(!metadata_path.exists());
                }
                ArtifactPutFault::MetadataCommitted => {
                    assert!(content_path.exists());
                    assert!(metadata_path.exists());
                }
            }

            let retry_media_type = if fault == ArtifactPutFault::MetadataCommitted {
                "image/png"
            } else {
                "text/plain"
            };
            let recovered = store
                .put(bytes, Some(retry_media_type.to_owned()))
                .expect("相同字节应恢复或幂等确认");
            assert_eq!(recovered.media_type.as_deref(), Some(retry_media_type));
            assert_eq!(store.read(&recovered).expect("恢复后内容应读取"), bytes);
            if fault == ArtifactPutFault::MetadataCommitted {
                assert!(matches!(
                    store.put(bytes, Some("text/plain".to_owned())),
                    Err(ResourceError::ArtifactMediaTypeMismatch)
                ));
            }
        }
    }

    /// 验证 data-only、metadata-only 和完整 pair 三种磁盘夹具遵守 commit marker 语义。
    #[test]
    fn artifact磁盘夹具只把完整pair计入配额() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let data_only_store = test_store(root.path(), "data-only", 1);
        let orphan_bytes = b"data orphan";
        let orphan_id = ArtifactId::new(sha256_hex(orphan_bytes)).expect("Artifact ID 应有效");
        atomic_write(
            &data_only_store.artifact_path(&orphan_id),
            orphan_bytes,
            true,
        )
        .expect("data-only 夹具应写入");
        let committed = data_only_store
            .put(b"committed", Some("text/plain".to_owned()))
            .expect("data-only orphan 不应占用配额");
        assert_eq!(
            data_only_store.read(&committed).expect("完整 pair 应读取"),
            b"committed"
        );

        let metadata_only_store = test_store(root.path(), "metadata-only", 1);
        let bytes = b"metadata orphan";
        let sha256 = sha256_hex(bytes);
        let artifact_id = ArtifactId::new(sha256.clone()).expect("Artifact ID 应有效");
        write_metadata(
            &metadata_only_store.metadata_path(&artifact_id),
            &ArtifactMetadata {
                schema: ARTIFACT_METADATA_SCHEMA.to_owned(),
                version: ARTIFACT_METADATA_VERSION,
                artifact_id: artifact_id.clone(),
                sha256,
                size_bytes: bytes.len() as u64,
                media_type: Some("image/png".to_owned()),
            },
        )
        .expect("metadata-only 夹具应写入");
        let recovered = metadata_only_store
            .put(bytes, Some("text/plain".to_owned()))
            .expect("metadata-only marker 不得冻结 MIME");
        assert_eq!(recovered.media_type.as_deref(), Some("text/plain"));
        assert_eq!(
            recover_artifact_directory(&metadata_only_store.artifacts_dir)
                .expect("完整 pair 应统计"),
            1
        );
        assert!(matches!(
            metadata_only_store.put(bytes, Some("image/png".to_owned())),
            Err(ResourceError::ArtifactMediaTypeMismatch)
        ));
    }

    /// 验证不同 Digest 连续半提交时，下一次持锁写入会先回收旧 orphan。
    #[test]
    fn artifact不同digest连续半提交保持有界() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let store = test_store(root.path(), "repeated-orphans", 1);
        for index in 0..4 {
            let bytes = format!("orphan-{index}");
            set_artifact_put_fault(ArtifactPutFault::ContentCommitted);
            assert!(
                store
                    .put(bytes.as_bytes(), Some("text/plain".to_owned()))
                    .is_err()
            );
            let entries = fs::read_dir(&store.artifacts_dir)
                .expect("Artifact 目录应读取")
                .collect::<Result<Vec<_>, _>>()
                .expect("Artifact 条目应读取");
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "artifact"))
                    .count(),
                1
            );
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".metadata.json")
                    })
                    .count(),
                0
            );
        }
        let committed = store
            .put(b"final artifact", Some("text/plain".to_owned()))
            .expect("最终写入应先回收旧 orphan");
        assert_eq!(
            store.read(&committed).expect("完整 pair 应读取"),
            b"final artifact"
        );
        assert_eq!(
            recover_artifact_directory(&store.artifacts_dir).expect("最终目录应恢复"),
            1
        );
    }

    /// 验证重开会回收多个 16 MiB data orphan、metadata orphan 与 KeenCode 原子临时文件，但保留完整 pair。
    #[test]
    fn artifact重开回收边界大小半成品且保留合法提交() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("reopen-orphan-recovery").expect("Session ID 应有效");
        let store = ArtifactStore::open(root.path(), session_id.clone(), ArtifactLimits::default())
            .expect("ArtifactStore 应打开");
        let committed = store
            .put(b"protected committed pair", Some("text/plain".to_owned()))
            .expect("合法 pair 应提交");
        let artifacts_dir = store.artifacts_dir.clone();
        drop(store);

        let boundary_size = ArtifactLimits::default().max_artifact_bytes;
        let mut recoverable_paths = Vec::new();
        for digit in ['0', '1', '2'] {
            let path = artifacts_dir.join(format!("{}.artifact", digit.to_string().repeat(64)));
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("data-only 边界夹具应创建");
            file.set_len(boundary_size)
                .expect("data-only 夹具应达到 16 MiB 边界");
            recoverable_paths.push(path);
        }
        let metadata_id = ArtifactId::new("f".repeat(64)).expect("metadata ID 应有效");
        let metadata_path = artifacts_dir.join(format!("{}.metadata.json", metadata_id.as_str()));
        write_metadata(
            &metadata_path,
            &ArtifactMetadata {
                schema: ARTIFACT_METADATA_SCHEMA.to_owned(),
                version: ARTIFACT_METADATA_VERSION,
                artifact_id: metadata_id,
                sha256: "f".repeat(64),
                size_bytes: boundary_size,
                media_type: Some("image/png".to_owned()),
            },
        )
        .expect("metadata-only 夹具应创建");
        recoverable_paths.push(metadata_path);
        let temporary_path = artifacts_dir.join(format!("{ATOMIC_TEMP_PREFIX}crash-leftover"));
        let temporary_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .expect("原子临时夹具应创建");
        temporary_file
            .set_len(boundary_size)
            .expect("临时夹具应达到 16 MiB 边界");
        recoverable_paths.push(temporary_path);

        let reopened = ArtifactStore::open(
            root.path(),
            session_id,
            ArtifactLimits {
                max_artifacts_per_session: 1,
                ..ArtifactLimits::default()
            },
        )
        .expect("带半成品的 ArtifactStore 应恢复打开");
        assert_eq!(
            reopened.read(&committed).expect("合法完整 pair 不得被删除"),
            b"protected committed pair"
        );
        assert!(recoverable_paths.iter().all(|path| !path.exists()));
        assert_eq!(
            recover_artifact_directory(&artifacts_dir).expect("恢复后目录应健康"),
            1
        );
        assert!(matches!(
            reopened.put(b"quota blocked", Some("text/plain".to_owned())),
            Err(ResourceError::ArtifactCountLimit { limit: 1 })
        ));
    }

    /// 验证容量边界只统计唯一内容，重复 Hash 不占用新槽位且降低配置时不下溢。
    #[test]
    fn artifact容量报告唯一提交边界且重复hash不重复计数() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("capacity-boundary").expect("Session ID 应有效");
        let limits = ArtifactLimits {
            max_artifact_bytes: 1024,
            max_artifacts_per_session: 2,
            max_preview_bytes: 128,
        };
        let store = ArtifactStore::open(root.path(), session_id.clone(), limits)
            .expect("ArtifactStore 应打开");
        assert_eq!(store.limits(), &limits);
        assert_eq!(
            store.capacity().expect("空 Store 容量应读取"),
            ArtifactCapacity {
                committed_unique_artifacts: 0,
                maximum_unique_artifacts: 2,
            }
        );
        let first = store
            .put(b"same bytes", Some("text/plain".to_owned()))
            .expect("首个 Artifact 应提交");
        let duplicate = store
            .put(b"same bytes", Some("text/plain".to_owned()))
            .expect("相同 Hash 应幂等命中");
        assert_eq!(first.artifact_id, duplicate.artifact_id);
        let one = store.capacity().expect("单项容量应读取");
        assert_eq!(one.committed_unique_artifacts, 1);
        assert_eq!(one.remaining(), 1);
        store
            .put(b"different bytes", Some("text/plain".to_owned()))
            .expect("第二个唯一 Artifact 应提交");
        let full = store.capacity().expect("满容量应读取");
        assert_eq!(full.committed_unique_artifacts, 2);
        assert_eq!(full.remaining(), 0);
        drop(store);

        let reopened = ArtifactStore::open(
            root.path(),
            session_id,
            ArtifactLimits {
                max_artifacts_per_session: 1,
                ..limits
            },
        )
        .expect("降低数量上限后仍应只读重开");
        let over_limit = reopened.capacity().expect("超配置容量应安全读取");
        assert_eq!(over_limit.committed_unique_artifacts, 2);
        assert_eq!(over_limit.maximum_unique_artifacts, 1);
        assert_eq!(over_limit.remaining(), 0);
    }

    /// 验证容量查询先回收 data-only、metadata-only 与临时文件，再统计完整 pair。
    #[test]
    fn artifact容量查询先恢复不完整pair且只保留有效提交() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let store = test_store(root.path(), "capacity-orphan-recovery", 4);
        store
            .put(b"committed", Some("text/plain".to_owned()))
            .expect("基准 Artifact 应提交");

        let data_bytes = b"data-only";
        let data_id = ArtifactId::new(sha256_hex(data_bytes)).expect("Artifact ID 应有效");
        let data_path = store.artifact_path(&data_id);
        atomic_write(&data_path, data_bytes, true).expect("data-only 夹具应写入");

        let metadata_bytes = b"metadata-only";
        let metadata_sha256 = sha256_hex(metadata_bytes);
        let metadata_id = ArtifactId::new(metadata_sha256.clone()).expect("Artifact ID 应有效");
        let metadata_path = store.metadata_path(&metadata_id);
        write_metadata(
            &metadata_path,
            &ArtifactMetadata {
                schema: ARTIFACT_METADATA_SCHEMA.to_owned(),
                version: ARTIFACT_METADATA_VERSION,
                artifact_id: metadata_id,
                sha256: metadata_sha256,
                size_bytes: metadata_bytes.len() as u64,
                media_type: Some("text/plain".to_owned()),
            },
        )
        .expect("metadata-only 夹具应写入");
        let temporary_path = store
            .artifacts_dir
            .join(format!("{ATOMIC_TEMP_PREFIX}capacity-leftover"));
        atomic_write(&temporary_path, b"temporary", true).expect("临时夹具应写入");

        let capacity = store.capacity().expect("容量查询应完成 orphan 恢复");
        assert_eq!(capacity.committed_unique_artifacts, 1);
        assert_eq!(capacity.maximum_unique_artifacts, 4);
        assert_eq!(capacity.remaining(), 3);
        assert!(!data_path.exists());
        assert!(!metadata_path.exists());
        assert!(!temporary_path.exists());
    }

    /// 验证同尺寸内容篡改仍保守占用容量，但完整读取会因摘要不匹配失败。
    #[test]
    fn artifact容量不读取同尺寸篡改内容且完整读取拒绝摘要不匹配() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let store = test_store(root.path(), "capacity-corrupt-pair", 2);
        let reference = store
            .put(b"original", Some("text/plain".to_owned()))
            .expect("Artifact 应提交");
        atomic_write(
            &store.artifact_path(&reference.artifact_id),
            b"tampered",
            true,
        )
        .expect("损坏夹具应写入");
        let capacity = store.capacity().expect("同尺寸篡改仍应保守占用槽位");
        assert_eq!(capacity.committed_unique_artifacts, 1);
        assert_eq!(capacity.remaining(), 1);
        assert!(matches!(
            store.read(&reference),
            Err(ResourceError::ArtifactHashMismatch)
        ));
    }

    /// 验证容量查询拒绝错误 schema、版本、文件身份和非规范 MIME 元数据。
    #[test]
    fn artifact容量拒绝错误身份版本与非规范mime元数据() {
        assert_capacity_metadata_mismatch("capacity-schema", |metadata| {
            metadata.schema = "keencode/unknown-artifact-metadata".to_owned();
        });
        assert_capacity_metadata_mismatch("capacity-version", |metadata| {
            metadata.version = ARTIFACT_METADATA_VERSION + 1;
        });
        assert_capacity_metadata_mismatch("capacity-artifact-id", |metadata| {
            metadata.artifact_id =
                ArtifactId::new("0".repeat(64)).expect("替代 Artifact ID 应有效");
        });
        assert_capacity_metadata_mismatch("capacity-sha256", |metadata| {
            metadata.sha256 = "1".repeat(64);
        });
        assert_capacity_metadata_mismatch("capacity-media-type", |metadata| {
            metadata.media_type = Some("Text/Plain".to_owned());
        });
    }

    /// 验证容量查询按上限和文件长度校验元数据大小，不读取内容正文。
    #[test]
    fn artifact容量校验元数据与内容长度边界() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let limits = ArtifactLimits {
            max_artifact_bytes: 8,
            max_artifacts_per_session: 2,
            max_preview_bytes: 8,
        };
        let store = ArtifactStore::open(
            root.path(),
            SessionId::new("capacity-size-boundary").expect("Session ID 应有效"),
            limits,
        )
        .expect("ArtifactStore 应打开");
        let reference = store
            .put(b"12345678", Some("text/plain".to_owned()))
            .expect("恰好达到大小上限的 Artifact 应提交");
        assert_eq!(
            store
                .capacity()
                .expect("边界大小 pair 应计入容量")
                .committed_unique_artifacts,
            1
        );

        let metadata_path = store.metadata_path(&reference.artifact_id);
        let mut metadata = read_metadata(&metadata_path).expect("基准元数据应读取");
        metadata.size_bytes = 7;
        write_metadata(&metadata_path, &metadata).expect("大小不匹配元数据应写入");
        assert!(matches!(
            store.capacity(),
            Err(ResourceError::ArtifactSizeMismatch {
                expected: 7,
                actual: 8
            })
        ));

        metadata.size_bytes = 9;
        write_metadata(&metadata_path, &metadata).expect("超限元数据应写入");
        assert!(matches!(
            store.capacity(),
            Err(ResourceError::ArtifactTooLarge {
                actual: 9,
                limit: 8
            })
        ));

        metadata.size_bytes = 8;
        write_metadata(&metadata_path, &metadata).expect("边界元数据应恢复");
        let content_path = store.artifact_path(&reference.artifact_id);
        fs::OpenOptions::new()
            .write(true)
            .open(&content_path)
            .expect("内容文件应打开")
            .set_len(9)
            .expect("内容长度应扩展到上限之外");
        assert!(matches!(
            store.capacity(),
            Err(ResourceError::ArtifactTooLarge {
                actual: 9,
                limit: 8
            })
        ));
    }

    /// 验证普通 `.tmp` 用户数据既不会被恢复逻辑删除，也会让所有入口失败关闭。
    #[test]
    fn artifact普通tmp用户数据不会被删除且所有入口失败关闭() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("capacity-user-tmp").expect("Session ID 应有效");
        let limits = ArtifactLimits {
            max_artifact_bytes: 1024,
            max_artifacts_per_session: 2,
            max_preview_bytes: 128,
        };
        let store = ArtifactStore::open(root.path(), session_id.clone(), limits)
            .expect("ArtifactStore 应打开");
        let user_data_path = store.artifacts_dir.join(".tmp-user-data");
        atomic_write(&user_data_path, b"must remain", true).expect("用户数据夹具应写入");

        assert!(matches!(
            store.capacity(),
            Err(ResourceError::UnsafePath(_))
        ));
        assert!(user_data_path.exists());
        assert!(matches!(
            store.put(b"blocked", Some("text/plain".to_owned())),
            Err(ResourceError::UnsafePath(_))
        ));
        assert!(user_data_path.exists());
        drop(store);

        assert!(matches!(
            ArtifactStore::open(root.path(), session_id, limits),
            Err(ResourceError::UnsafePath(_))
        ));
        assert!(user_data_path.exists());
    }

    /// 验证未知普通文件或目录不会被容量查询忽略或误算为空闲容量。
    #[test]
    fn artifact容量拒绝不安全目录条目() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let unknown_store = test_store(root.path(), "capacity-unknown-entry", 2);
        atomic_write(
            &unknown_store.artifacts_dir.join("unexpected.txt"),
            b"unexpected",
            true,
        )
        .expect("未知文件夹具应写入");
        assert!(matches!(
            unknown_store.capacity(),
            Err(ResourceError::UnsafePath(_))
        ));

        let directory_store = test_store(root.path(), "capacity-directory-entry", 2);
        fs::create_dir(directory_store.artifacts_dir.join("unexpected-directory"))
            .expect("未知目录夹具应创建");
        assert!(matches!(
            directory_store.capacity(),
            Err(ResourceError::UnsafePath(_))
        ));
    }

    /// 验证容量查询遵守 artifacts.lock，并在竞争解除后可由另一实例和重开实例读取。
    #[test]
    fn artifact容量锁竞争解除后可跨实例与重开读取() {
        let root = tempfile::tempdir().expect("临时目录应创建");
        let session_id = SessionId::new("capacity-lock-reopen").expect("Session ID 应有效");
        let limits = ArtifactLimits {
            max_artifact_bytes: 1024,
            max_artifacts_per_session: 2,
            max_preview_bytes: 128,
        };
        let first = ArtifactStore::open(root.path(), session_id.clone(), limits)
            .expect("首个 ArtifactStore 应打开");
        first
            .put(b"locked artifact", Some("text/plain".to_owned()))
            .expect("Artifact 应提交");
        let second = ArtifactStore::open(root.path(), session_id.clone(), limits)
            .expect("第二个 ArtifactStore 应打开");
        let held_lock = exclusive_lock(&first.lock_path).expect("测试应持有 artifacts.lock");
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_sender.send(()).expect("测试启动信号应发送");
            let result = second.capacity();
            result_sender.send(result).expect("容量结果应发送");
        });
        started_receiver.recv().expect("容量线程应启动");
        assert!(
            result_receiver
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "持锁期间第二实例不得越过容量查询"
        );
        drop(held_lock);
        let capacity = result_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("解锁后容量查询应完成")
            .expect("容量查询应成功");
        assert_eq!(capacity.committed_unique_artifacts, 1);
        assert_eq!(capacity.remaining(), 1);
        worker.join().expect("容量线程不应异常");
        drop(first);

        let reopened =
            ArtifactStore::open(root.path(), session_id, limits).expect("ArtifactStore 应重开");
        assert_eq!(
            reopened
                .capacity()
                .expect("重开后容量应读取")
                .committed_unique_artifacts,
            1
        );
    }
}
