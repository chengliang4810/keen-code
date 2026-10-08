use std::path::Path;
use std::time::UNIX_EPOCH;
use std::{fs, io::Write};

use rcode_tools::file_transaction::{
    check_file_version, file_version, lock_file, read_file_snapshot, read_file_version_snapshot,
};
use serde::Serialize;
use tauri::Emitter;
use tempfile::NamedTempFile;

use crate::modules::workspace::{resolve_path, WorkspaceEnv};

const MAX_READ_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
/// Ceiling for explicit "open anyway"; mirrored as FORCE_READ_LIMIT in useDocument.ts.
const FORCE_MAX_READ_BYTES: u64 = 50 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReadResult {
    Text {
        content: String,
        size: u64,
        mtime: u64,
        version: String,
    },
    Binary {
        size: u64,
        version: String,
    },
    /// File exceeds MAX_READ_BYTES. UI decides whether to offer "open anyway".
    TooLarge {
        size: u64,
        limit: u64,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatKind {
    File,
    Dir,
    Symlink,
}

#[derive(Serialize)]
pub struct FileStat {
    pub size: u64,
    pub mtime: u64,
    pub kind: StatKind,
}

fn mtime_millis(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tauri::command]
pub async fn fs_read_file(
    path: String,
    workspace: Option<WorkspaceEnv>,
    force: Option<bool>,
) -> Result<ReadResult, String> {
    let workspace = WorkspaceEnv::from_option(workspace);
    read_file_sync(&resolve_path(&path, &workspace), force.unwrap_or(false))
}

pub(super) fn read_file_sync(p: &Path, force: bool) -> Result<ReadResult, String> {
    let _transaction = lock_file(p).map_err(|error| error.to_string())?;
    let meta = std::fs::metadata(p).map_err(|e| {
        log::debug!("fs_read_file stat({}) failed: {e}", p.display());
        e.to_string()
    })?;

    let size = meta.len();
    let limit = if force {
        FORCE_MAX_READ_BYTES
    } else {
        MAX_READ_BYTES
    };
    if size > limit {
        return Ok(ReadResult::TooLarge { size, limit });
    }

    let (bytes, meta, version) = read_file_snapshot(p, limit).map_err(|e| {
        log::debug!("fs_read_file read({}) failed: {e}", p.display());
        e.to_string()
    })?;
    let size = meta.len();

    // Null-byte sniff on the first chunk. Not perfect (misses UTF-16 BOM
    // cases) but catches the common "this is a PNG" mistake cheaply.
    let sniff_len = bytes.len().min(BINARY_SNIFF_BYTES);
    if bytes[..sniff_len].contains(&0) {
        return Ok(ReadResult::Binary { size, version });
    }

    match String::from_utf8(bytes) {
        Ok(content) => Ok(ReadResult::Text {
            content,
            size,
            mtime: mtime_millis(&meta),
            version,
        }),
        Err(_) => Ok(ReadResult::Binary { size, version }),
    }
}

#[derive(Serialize, Debug)]
pub struct BinaryFileSnapshot {
    pub size: u64,
    pub version: String,
}

pub(super) fn binary_file_snapshot_sync(p: &Path) -> Result<Option<BinaryFileSnapshot>, String> {
    let _transaction = lock_file(p).map_err(|error| error.to_string())?;
    let snapshot = read_file_version_snapshot(p, u64::MAX).map_err(|error| error.to_string())?;
    Ok(snapshot.binary.then_some(BinaryFileSnapshot {
        size: snapshot.metadata.len(),
        version: snapshot.version,
    }))
}

#[derive(Serialize, Clone)]
struct FileWrittenEvent {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

/// Atomic write via O_EXCL tempfile in the target's parent, then rename.
/// The random suffix is what blocks pre-staged symlink attacks.
fn write_atomic(
    target: &Path,
    content: &[u8],
    expected_version: Option<&str>,
    overwrite: bool,
) -> std::io::Result<()> {
    let parent = target.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = fs::metadata(target) {
        tmp.as_file().set_permissions(metadata.permissions())?;
    }
    tmp.as_file_mut().write_all(content)?;
    tmp.as_file_mut().sync_all()?;
    if !overwrite {
        check_file_version(target, expected_version, u64::MAX)?;
    }
    if !overwrite && expected_version.is_none() {
        tmp.persist_noclobber(target).map_err(|error| error.error)?;
    } else {
        tmp.persist(target).map_err(|error| error.error)?;
    }
    Ok(())
}

/// Returns the new mtime so the editor can track disk state for conflict
/// detection without a follow-up stat.
#[tauri::command]
pub async fn fs_write_file(
    path: String,
    content: String,
    workspace: Option<WorkspaceEnv>,
    source: Option<String>,
    expected_version: Option<String>,
    overwrite: Option<bool>,
    app: tauri::AppHandle,
) -> Result<FileWriteResult, String> {
    let workspace = WorkspaceEnv::from_option(workspace);
    let target = resolve_path(&path, &workspace);
    let result = write_file_sync(
        &target,
        &content,
        expected_version.as_deref(),
        overwrite.unwrap_or(false),
    )?;
    emit_file_written(&app, path, source);
    Ok(result)
}

#[derive(Serialize, Debug)]
pub struct FileWriteResult {
    pub mtime: u64,
    pub version: String,
}

pub(super) fn write_file_sync(
    target: &Path,
    content: &str,
    expected_version: Option<&str>,
    overwrite: bool,
) -> Result<FileWriteResult, String> {
    let _transaction = lock_file(target).map_err(|error| error.to_string())?;
    write_atomic(target, content.as_bytes(), expected_version, overwrite)
        .map_err(|error| error.to_string())?;
    let metadata = fs::metadata(target).map_err(|error| error.to_string())?;
    Ok(FileWriteResult {
        mtime: mtime_millis(&metadata),
        version: file_version(&metadata, content.as_bytes()),
    })
}

pub(super) fn emit_file_written(app: &tauri::AppHandle, path: String, source: Option<String>) {
    let _ = app.emit("fs:file-written", FileWrittenEvent { path, source });
}

#[tauri::command]
pub async fn fs_canonicalize(
    path: String,
    workspace: Option<WorkspaceEnv>,
) -> Result<String, String> {
    let workspace = WorkspaceEnv::from_option(workspace);
    let p = resolve_path(&path, &workspace);
    let canon = std::fs::canonicalize(&p).map_err(|e| e.to_string())?;
    Ok(super::to_canon(&canon))
}

#[tauri::command]
pub async fn fs_stat(path: String, workspace: Option<WorkspaceEnv>) -> Result<FileStat, String> {
    let workspace = WorkspaceEnv::from_option(workspace);
    let p = resolve_path(&path, &workspace);
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    // fs::metadata follows symlinks, so the link check needs symlink_metadata.
    let kind = if std::fs::symlink_metadata(&p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        StatKind::Symlink
    } else if meta.is_dir() {
        StatKind::Dir
    } else {
        StatKind::File
    };
    Ok(FileStat {
        size: meta.len(),
        mtime: mtime_millis(&meta),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_classifies_utf8_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"hello world").unwrap();
        match read_file_sync(&f, false).unwrap() {
            ReadResult::Text {
                content,
                size,
                mtime,
                version,
            } => {
                assert_eq!(content, "hello world");
                assert_eq!(size, 11);
                assert!(mtime > 0);
                assert_eq!(version.len(), 64);
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn read_file_detects_binary_via_null_byte() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.bin");
        std::fs::write(&f, b"PNG\0\x89image").unwrap();
        assert!(matches!(
            read_file_sync(&f, false).unwrap(),
            ReadResult::Binary { .. }
        ));
    }

    #[test]
    fn read_file_detects_binary_via_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.bin");
        // Invalid UTF-8 with no null byte: must still classify as binary.
        std::fs::write(&f, [0xff, 0xfe, 0xfd, 0xfc]).unwrap();
        assert!(matches!(
            read_file_sync(&f, false).unwrap(),
            ReadResult::Binary { .. }
        ));
    }

    #[test]
    fn binary_versions_allow_full_replacement_without_returning_binary_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("binary.txt");
        for bytes in [&[0xff, 0xfe, 0x41, 0x00][..], &[0xff, 0xfe, 0xfd][..]] {
            fs::write(&target, bytes).unwrap();
            let result = read_file_sync(&target, false).unwrap();
            let serialized = serde_json::to_value(&result).unwrap();
            assert!(serialized.get("content").is_none());
            let ReadResult::Binary { version, .. } = result else {
                panic!("expected binary snapshot")
            };
            write_file_sync(&target, "UTF-8 replacement", Some(&version), false).unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"UTF-8 replacement");
        }
    }

    #[test]
    fn binary_snapshot_cannot_overwrite_a_concurrent_change() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("binary.txt");
        fs::write(&target, [0xff, 0xfe]).unwrap();
        let ReadResult::Binary { version, .. } = read_file_sync(&target, false).unwrap() else {
            panic!("binary")
        };
        fs::write(&target, [0xff, 0x81]).unwrap();
        let error =
            write_file_sync(&target, "UTF-8 replacement", Some(&version), false).unwrap_err();
        assert!(error.starts_with("FILE_CONFLICT:"));
        assert_eq!(fs::read(&target).unwrap(), [0xff, 0x81]);
    }

    #[test]
    fn large_binary_replacement_retains_read_limits_and_opaque_versions() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("binary.dat");
        for size in [MAX_READ_BYTES + 1, FORCE_MAX_READ_BYTES + 1] {
            fs::File::create(&target).unwrap().set_len(size).unwrap();
            assert!(matches!(
                read_file_sync(&target, false).unwrap(),
                ReadResult::TooLarge {
                    limit: MAX_READ_BYTES,
                    ..
                }
            ));
            if size > FORCE_MAX_READ_BYTES {
                assert!(matches!(
                    read_file_sync(&target, true).unwrap(),
                    ReadResult::TooLarge {
                        limit: FORCE_MAX_READ_BYTES,
                        ..
                    }
                ));
            }
            let snapshot = binary_file_snapshot_sync(&target).unwrap().unwrap();
            assert_eq!(snapshot.size, size);
            assert!(serde_json::to_value(&snapshot)
                .unwrap()
                .get("content")
                .is_none());
            write_file_sync(&target, "replacement", Some(&snapshot.version), false).unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"replacement");
        }
    }

    #[test]
    fn large_binary_same_length_external_changes_still_conflict() {
        use std::io::{Seek, SeekFrom};
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("binary.dat");
        fs::File::create(&target)
            .unwrap()
            .set_len(FORCE_MAX_READ_BYTES + 1)
            .unwrap();
        let snapshot = binary_file_snapshot_sync(&target).unwrap().unwrap();
        let mut writer = fs::OpenOptions::new().write(true).open(&target).unwrap();
        writer.seek(SeekFrom::End(-1)).unwrap();
        writer.write_all(&[1]).unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        assert!(
            write_file_sync(&target, "replacement", Some(&snapshot.version), false)
                .unwrap_err()
                .starts_with("FILE_CONFLICT:")
        );
        assert_eq!(
            fs::metadata(&target).unwrap().len(),
            FORCE_MAX_READ_BYTES + 1
        );
    }

    #[test]
    fn large_text_cannot_obtain_an_implicit_write_version() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("large.txt");
        let mut writer = fs::File::create(&target).unwrap();
        let block = [b'a'; 64 * 1024];
        for _ in 0..161 {
            writer.write_all(&block).unwrap();
        }
        drop(writer);
        assert!(binary_file_snapshot_sync(&target).unwrap().is_none());
        assert!(write_file_sync(&target, "replacement", None, false)
            .unwrap_err()
            .starts_with("FILE_CONFLICT:"));
    }

    #[test]
    fn creating_large_text_retains_the_existing_write_capability() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("large.txt");
        let content = "a".repeat((FORCE_MAX_READ_BYTES + 1) as usize);
        let result = write_file_sync(&target, &content, None, false).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().len(), content.len() as u64);
        assert_eq!(result.version.len(), 64);
        assert!(matches!(
            read_file_sync(&target, true).unwrap(),
            ReadResult::TooLarge {
                limit: FORCE_MAX_READ_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn force_lifts_the_default_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big.txt");
        std::fs::write(&f, vec![b'a'; (MAX_READ_BYTES + 1) as usize]).unwrap();
        assert!(matches!(
            read_file_sync(&f, false).unwrap(),
            ReadResult::TooLarge { .. }
        ));
        assert!(matches!(
            read_file_sync(&f, true).unwrap(),
            ReadResult::Text { .. }
        ));
    }

    #[test]
    fn overwrites_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("note.txt");
        std::fs::write(&target, b"old").unwrap();
        write_atomic(&target, b"new", None, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn save_rejects_deleted_or_changed_snapshot_and_preserves_explicit_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("note.txt");
        std::fs::write(&target, "first").unwrap();
        let ReadResult::Text { version, .. } = read_file_sync(&target, false).unwrap() else {
            panic!("text")
        };
        std::fs::write(&target, "other").unwrap();
        assert!(write_file_sync(&target, "saved", Some(&version), false)
            .unwrap_err()
            .starts_with("FILE_CONFLICT:"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "other");
        std::fs::remove_file(&target).unwrap();
        assert!(write_file_sync(&target, "saved", Some(&version), false).is_err());
        assert!(!target.exists());
        let result = write_file_sync(&target, "saved", Some(&version), true).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "saved");
        assert_eq!(result.version.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_legacy_staging_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"untouched").unwrap();

        let target = dir.path().join("note.txt");
        // Pre-stage a symlink at the legacy deterministic staging path.
        let legacy = dir.path().join(".note.txt.rcode.tmp");
        symlink(&outside, &legacy).unwrap();

        write_atomic(&target, b"payload", None, true).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
        // The pre-staged symlink target must not have been written through.
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
    }
}
