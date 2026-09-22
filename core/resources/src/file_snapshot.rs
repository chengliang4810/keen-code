//! 工具文件变更使用的有界分块快照；正文只保存到现有 Session ArtifactStore。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ArtifactStore, ArtifactUse, ResourceError};

/// 文件工具支持的单份原始快照大小，包含 BOM 和原始换行字节。
pub const MAX_FILE_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// 单个快照允许引用的最多块数，防止较小 Artifact 配置放大 Journal。
pub const MAX_FILE_SNAPSHOT_CHUNKS: usize = 128;
/// 默认分块及单次按需读取的最大字节数。
pub const FILE_SNAPSHOT_CHUNK_BYTES: usize = 1024 * 1024;

/// 独立于工作区当前状态的文件原始字节快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FileSnapshot {
    /// 完整文件原始字节数；零字节文件使用空块列表。
    pub size_bytes: u64,
    /// 按块顺序拼接后完整原始文件的小写 SHA-256。
    pub sha256: String,
    /// 按原始字节顺序排列的块；相同内容可以重复引用同一 Artifact。
    pub chunks: Vec<ArtifactUse>,
}

impl FileSnapshot {
    /// 在读取或归约前验证固定数量、字节、Hash 与空文件约束，不执行 IO。
    pub fn validate_shape(&self) -> Result<(), ResourceError> {
        if self.size_bytes > MAX_FILE_SNAPSHOT_BYTES
            || self.chunks.len() > MAX_FILE_SNAPSHOT_CHUNKS
            || !valid_hash(&self.sha256)
        {
            return Err(invalid_snapshot());
        }
        let mut total = 0_u64;
        for chunk in &self.chunks {
            if chunk.size_bytes == 0
                || chunk.size_bytes > FILE_SNAPSHOT_CHUNK_BYTES as u64
                || chunk.artifact_id.as_str() != chunk.sha256
                || !valid_hash(&chunk.sha256)
            {
                return Err(invalid_snapshot());
            }
            total = total
                .checked_add(chunk.size_bytes)
                .ok_or_else(invalid_snapshot)?;
        }
        if total != self.size_bytes || (self.size_bytes == 0 && self.sha256 != content_hash(&[])) {
            return Err(invalid_snapshot());
        }
        Ok(())
    }
}

impl ArtifactStore {
    /// 在文件副作用之前计算快照引用，不写入 Artifact 或用户文件。
    ///
    /// 已存在块保留首次冻结的媒体类型，避免原始字节恰好与模型文本或图片相同时
    /// 产生无意义的媒体类型冲突。读取快照始终使用原始字节，不据该类型解码。
    pub fn plan_file_snapshot(&self, bytes: &[u8]) -> Result<FileSnapshot, ResourceError> {
        let size_bytes = bytes.len() as u64;
        if size_bytes > MAX_FILE_SNAPSHOT_BYTES {
            return Err(ResourceError::ArtifactTooLarge {
                actual: size_bytes,
                limit: MAX_FILE_SNAPSHOT_BYTES,
            });
        }
        let chunk_bytes = self
            .limits()
            .max_artifact_bytes
            .min(FILE_SNAPSHOT_CHUNK_BYTES as u64) as usize;
        if bytes.len().div_ceil(chunk_bytes) > MAX_FILE_SNAPSHOT_CHUNKS {
            return Err(invalid_snapshot());
        }
        let chunks = bytes
            .chunks(chunk_bytes)
            .map(|chunk| self.file_snapshot_chunk_use(chunk))
            .collect::<Result<_, _>>()?;
        Ok(FileSnapshot {
            size_bytes,
            sha256: content_hash(bytes),
            chunks,
        })
    }

    /// 按预检时冻结的引用持久化每一块，返回前完成原始内容和规范元数据核验。
    ///
    /// 本方法不代替 Runtime reservation 或 Journal 提交。调用方必须先保留容量，
    /// 再持久化块并提交 Prepared 事件，最后才允许改变工作区文件。
    pub fn persist_file_snapshot(
        &self,
        snapshot: &FileSnapshot,
        bytes: &[u8],
    ) -> Result<(), ResourceError> {
        snapshot.validate_shape()?;
        if snapshot.size_bytes != bytes.len() as u64 || snapshot.sha256 != content_hash(bytes) {
            return Err(ResourceError::ArtifactHashMismatch);
        }
        let mut offset = 0_usize;
        // 先检查全部块与冻结内容一致，禁止直到写入中途才发现错误的块边界或顺序。
        for chunk in &snapshot.chunks {
            let end = offset + chunk.size_bytes as usize;
            if content_hash(&bytes[offset..end]) != chunk.sha256 {
                return Err(ResourceError::ArtifactHashMismatch);
            }
            offset = end;
        }
        offset = 0;
        for chunk in &snapshot.chunks {
            let end = offset + chunk.size_bytes as usize;
            let actual = self
                .put(&bytes[offset..end], chunk.media_type.clone())?
                .as_event_use();
            if actual != *chunk {
                return Err(ResourceError::ArtifactHashMismatch);
            }
            offset = end;
        }
        Ok(())
    }

    /// 逐块检查存储实体并核对完整文件摘要，峰值读取缓冲不超过一个块。
    pub fn validate_file_snapshot(&self, snapshot: &FileSnapshot) -> Result<(), ResourceError> {
        snapshot.validate_shape()?;
        let mut hash = Sha256::new();
        for chunk in &snapshot.chunks {
            hash.update(self.read_use(chunk)?);
        }
        if format!("{:x}", hash.finalize()) != snapshot.sha256 {
            return Err(ResourceError::ArtifactHashMismatch);
        }
        Ok(())
    }

    /// 读取完整原始快照并同时验证块内容与整份摘要，不读取工作区当前文件。
    pub fn read_file_snapshot(&self, snapshot: &FileSnapshot) -> Result<Vec<u8>, ResourceError> {
        snapshot.validate_shape()?;
        let mut bytes = Vec::with_capacity(snapshot.size_bytes as usize);
        for chunk in &snapshot.chunks {
            bytes.extend_from_slice(&self.read_use(chunk)?);
        }
        if content_hash(&bytes) != snapshot.sha256 {
            return Err(ResourceError::ArtifactHashMismatch);
        }
        Ok(bytes)
    }

    /// 按原始字节区间读取已提交快照，单次不超过一 MiB，不要求区间落在 UTF-8 边界。
    ///
    /// 每次复核实际读取块的完整摘要；未读取块由 Journal 接受/冷恢复时的整份快照
    /// 校验负责。该局部读取成功不表示此次重新扫描了所有未读取块。
    pub fn read_file_snapshot_range(
        &self,
        snapshot: &FileSnapshot,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, ResourceError> {
        snapshot.validate_shape()?;
        if length > FILE_SNAPSHOT_CHUNK_BYTES || offset > snapshot.size_bytes {
            return Err(invalid_snapshot());
        }
        let end = offset
            .saturating_add(length as u64)
            .min(snapshot.size_bytes);
        if end == offset {
            return Ok(Vec::new());
        }
        let mut output = Vec::with_capacity((end - offset) as usize);
        let mut chunk_start = 0_u64;
        for chunk in &snapshot.chunks {
            let chunk_end = chunk_start + chunk.size_bytes;
            if chunk_start < end && chunk_end > offset {
                let bytes = self.read_use(chunk)?;
                let from = offset.saturating_sub(chunk_start) as usize;
                let to = (end.min(chunk_end) - chunk_start) as usize;
                output.extend_from_slice(&bytes[from..to]);
            }
            chunk_start = chunk_end;
            if chunk_start >= end {
                break;
            }
        }
        Ok(output)
    }
}

/// 判断引用中的摘要是否使用唯一的小写 SHA-256 编码。
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 生成内容寻址使用的小写 SHA-256。
fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 返回不包含文件正文的稳定快照结构错误。
fn invalid_snapshot() -> ResourceError {
    ResourceError::Reduction("文件快照的块数、字节范围或摘要结构无效".to_owned())
}
