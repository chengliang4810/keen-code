//! 文件快照的原始字节、分块容量和损坏拒绝回归。

use keencode_resources::{
    ArtifactLimits, ArtifactStore, ArtifactValidator, FILE_SNAPSHOT_CHUNK_BYTES,
    MAX_FILE_SNAPSHOT_BYTES, ResourceError, SessionId,
};
use tempfile::TempDir;

/// 在测试专属目录中创建真实 ArtifactStore，不复用用户 Session 数据。
fn store(max_artifact_bytes: u64) -> (TempDir, ArtifactStore) {
    let directory = tempfile::tempdir().unwrap();
    let artifacts = ArtifactStore::open(
        directory.path(),
        SessionId::new("session-file-snapshots").unwrap(),
        ArtifactLimits {
            max_artifact_bytes,
            ..ArtifactLimits::default()
        },
    )
    .unwrap();
    (directory, artifacts)
}

#[test]
fn raw_snapshot_preserves_bom_crlf_nul_and_non_utf8_bytes() {
    let (_directory, artifacts) = store(16 * 1024 * 1024);
    let bytes = b"\xef\xbb\xbfhello\r\n\0\xff\x80";
    let snapshot = artifacts.plan_file_snapshot(bytes).unwrap();
    assert_eq!(artifacts.capacity().unwrap().committed_unique_artifacts, 0);
    artifacts.persist_file_snapshot(&snapshot, bytes).unwrap();
    artifacts.validate_file_snapshot(&snapshot).unwrap();
    assert_eq!(artifacts.read_file_snapshot(&snapshot).unwrap(), bytes);
}

#[test]
fn empty_file_is_a_present_snapshot_without_an_artifact() {
    let (_directory, artifacts) = store(8);
    let snapshot = artifacts.plan_file_snapshot(&[]).unwrap();
    assert_eq!(snapshot.size_bytes, 0);
    assert!(snapshot.chunks.is_empty());
    artifacts.persist_file_snapshot(&snapshot, &[]).unwrap();
    artifacts.validate_file_snapshot(&snapshot).unwrap();
    assert!(artifacts.read_file_snapshot(&snapshot).unwrap().is_empty());
    assert_eq!(artifacts.capacity().unwrap().committed_unique_artifacts, 0);
    let serialized = serde_json::to_value(Some(&snapshot)).unwrap();
    assert_ne!(serialized, serde_json::Value::Null);
}

#[test]
fn full_sixty_four_mib_file_fits_default_artifacts_and_small_manifest() {
    let (_directory, artifacts) = store(16 * 1024 * 1024);
    let bytes = vec![b'x'; MAX_FILE_SNAPSHOT_BYTES as usize];
    let snapshot = artifacts.plan_file_snapshot(&bytes).unwrap();
    assert_eq!(snapshot.size_bytes, MAX_FILE_SNAPSHOT_BYTES);
    assert_eq!(snapshot.chunks.len(), 64);
    assert!(
        snapshot
            .chunks
            .iter()
            .all(|chunk| chunk.size_bytes == 1024 * 1024)
    );
    artifacts.persist_file_snapshot(&snapshot, &bytes).unwrap();
    artifacts.validate_file_snapshot(&snapshot).unwrap();
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < 32 * 1024);
    // 顺序引用允许重复；内容寻址必须只占一个实体，而非每个块都复制同样正文。
    assert_eq!(artifacts.capacity().unwrap().committed_unique_artifacts, 1);
}

#[test]
fn oversize_file_and_too_many_tiny_chunks_fail_before_storage() {
    let (_directory, artifacts) = store(16 * 1024 * 1024);
    let oversized = vec![0; MAX_FILE_SNAPSHOT_BYTES as usize + 1];
    assert!(matches!(
        artifacts.plan_file_snapshot(&oversized),
        Err(ResourceError::ArtifactTooLarge { .. })
    ));
    assert_eq!(artifacts.capacity().unwrap().committed_unique_artifacts, 0);
    let (_tiny_directory, tiny_store) = store(8);
    assert!(
        tiny_store
            .plan_file_snapshot(&vec![1; 8 * 128 + 1])
            .is_err()
    );
    assert_eq!(tiny_store.capacity().unwrap().committed_unique_artifacts, 0);
}

#[test]
fn chunked_range_reads_cross_boundaries_without_text_conversion() {
    let (_directory, artifacts) = store(8);
    let bytes = b"\xef\xbb\xbf123\r\n\xe4\xb8\xad\0\xfflast";
    let snapshot = artifacts.plan_file_snapshot(bytes).unwrap();
    artifacts.persist_file_snapshot(&snapshot, bytes).unwrap();
    assert_eq!(
        artifacts.read_file_snapshot_range(&snapshot, 6, 8).unwrap(),
        bytes[6..14]
    );
    assert_eq!(
        artifacts
            .read_file_snapshot_range(&snapshot, bytes.len() as u64 - 2, 10)
            .unwrap(),
        b"st"
    );
    assert!(
        artifacts
            .read_file_snapshot_range(&snapshot, bytes.len() as u64, 1)
            .unwrap()
            .is_empty()
    );
    assert!(
        artifacts
            .read_file_snapshot_range(&snapshot, 0, FILE_SNAPSHOT_CHUNK_BYTES + 1)
            .is_err()
    );
    assert!(
        artifacts
            .read_file_snapshot_range(&snapshot, bytes.len() as u64 + 1, 1)
            .is_err()
    );
}

#[test]
fn matching_existing_content_reuses_its_frozen_media_type() {
    let (_directory, artifacts) = store(16 * 1024 * 1024);
    let bytes = b"already a model text artifact\r\n";
    let original = artifacts.put(bytes, Some("text/plain".to_owned())).unwrap();
    let snapshot = artifacts.plan_file_snapshot(bytes).unwrap();
    assert_eq!(snapshot.chunks, vec![original.as_event_use()]);
    artifacts.persist_file_snapshot(&snapshot, bytes).unwrap();
    assert_eq!(artifacts.capacity().unwrap().committed_unique_artifacts, 1);
    assert_eq!(artifacts.read_file_snapshot(&snapshot).unwrap(), bytes);
}

#[test]
fn invalid_frozen_chunk_order_is_rejected_before_any_put() {
    let (_directory, artifacts) = store(4);
    let bytes = b"aaaabbbb";
    let mut snapshot = artifacts.plan_file_snapshot(bytes).unwrap();
    snapshot.chunks.swap(0, 1);
    assert!(matches!(
        artifacts.persist_file_snapshot(&snapshot, bytes),
        Err(ResourceError::ArtifactHashMismatch)
    ));
    assert_eq!(artifacts.capacity().unwrap().committed_unique_artifacts, 0);
}

#[test]
fn reordered_valid_chunks_cannot_pass_whole_file_hash_validation() {
    let (_directory, artifacts) = store(4);
    let bytes = b"aaaabbbb";
    let mut snapshot = artifacts.plan_file_snapshot(bytes).unwrap();
    artifacts.persist_file_snapshot(&snapshot, bytes).unwrap();
    snapshot.chunks.swap(0, 1);
    assert!(matches!(
        artifacts.validate_file_snapshot(&snapshot),
        Err(ResourceError::ArtifactHashMismatch)
    ));
    assert!(matches!(
        artifacts.read_file_snapshot(&snapshot),
        Err(ResourceError::ArtifactHashMismatch)
    ));
}

#[test]
fn missing_and_tampered_chunks_fail_closed() {
    let (directory, artifacts) = store(4);
    let bytes = b"aaaabbbb";
    let snapshot = artifacts.plan_file_snapshot(bytes).unwrap();
    assert!(artifacts.validate_file_snapshot(&snapshot).is_err());
    artifacts.persist_file_snapshot(&snapshot, bytes).unwrap();
    let chunk = &snapshot.chunks[0];
    let path = directory
        .path()
        .join("sessions/session-file-snapshots/artifacts")
        .join(format!("{}.artifact", chunk.artifact_id.as_str()));
    std::fs::write(path, b"cccc").unwrap();
    assert!(matches!(
        artifacts.validate_file_snapshot(&snapshot),
        Err(ResourceError::ArtifactHashMismatch)
    ));
    assert!(artifacts.read_file_snapshot_range(&snapshot, 0, 1).is_err());
    // 局部读取只声明实际读取范围完整；不把未读块当作本次重新核验通过。
    assert_eq!(
        artifacts.read_file_snapshot_range(&snapshot, 4, 4).unwrap(),
        b"bbbb"
    );
}

#[test]
fn malformed_empty_hash_size_and_chunk_count_are_rejected() {
    let (_directory, artifacts) = store(4);
    let mut empty = artifacts.plan_file_snapshot(&[]).unwrap();
    empty.sha256 = "0".repeat(64);
    assert!(empty.validate_shape().is_err());
    let mut snapshot = artifacts.plan_file_snapshot(b"abcd").unwrap();
    snapshot.size_bytes = 5;
    assert!(snapshot.validate_shape().is_err());
    snapshot.size_bytes = 4;
    snapshot.chunks[0].size_bytes = 0;
    assert!(snapshot.validate_shape().is_err());
    let mut many = artifacts.plan_file_snapshot(b"abcd").unwrap();
    many.chunks = vec![many.chunks[0].clone(); 129];
    many.size_bytes = 4 * 129;
    assert!(many.validate_shape().is_err());
}

#[test]
fn snapshot_validator_rejects_a_different_session_scope() {
    let (_directory, artifacts) = store(4);
    let snapshot = artifacts.plan_file_snapshot(b"abcd").unwrap();
    artifacts.persist_file_snapshot(&snapshot, b"abcd").unwrap();
    assert!(matches!(
        ArtifactValidator::validate_file_snapshot(
            &artifacts,
            &SessionId::new("session-other").unwrap(),
            &snapshot,
        ),
        Err(ResourceError::ArtifactScopeMismatch)
    ));
}
