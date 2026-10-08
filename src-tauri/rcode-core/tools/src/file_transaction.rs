//! Shared file mutation locks and content versions for desktop and Agent writes.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

const LOCK_COUNT: usize = 64;
static FILE_LOCKS: OnceLock<[Mutex<()>; LOCK_COUNT]> = OnceLock::new();

/// Serializes mutations across all host entry points without retaining file paths.
pub fn lock_file(path: &Path) -> io::Result<MutexGuard<'static, ()>> {
    let canonical = fs::canonicalize(path)
        .or_else(|_| {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("missing parent"))?;
            Ok::<_, io::Error>(
                fs::canonicalize(parent)?.join(
                    path.file_name()
                        .ok_or_else(|| io::Error::other("missing filename"))?,
                ),
            )
        })
        .unwrap_or_else(|_| path.to_path_buf());
    let key = canonical.to_string_lossy();
    let mut hash = DefaultHasher::new();
    if cfg!(windows) {
        key.replace('/', "\\").to_ascii_lowercase().hash(&mut hash);
    } else {
        canonical.hash(&mut hash);
    }
    FILE_LOCKS.get_or_init(|| std::array::from_fn(|_| Mutex::new(())))
        [hash.finish() as usize % LOCK_COUNT]
        .lock()
        .map_err(|_| io::Error::other("file transaction lock poisoned"))
}

/// Identifies the exact bytes and metadata of one open file snapshot.
pub fn file_version(metadata: &fs::Metadata, bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    finish_file_version(hash, metadata)
}

fn finish_file_version(mut hash: Sha256, metadata: &fs::Metadata) -> String {
    hash.update(metadata.len().to_le_bytes());
    for time in [metadata.modified(), metadata.created()] {
        hash.update(
            time.ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|time| time.as_nanos())
                .unwrap_or_default()
                .to_le_bytes(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hash.update(metadata.dev().to_le_bytes());
        hash.update(metadata.ino().to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn unchanged_metadata(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && before.created().ok() == after.created().ok()
}

/// A content-free snapshot used for conditional replacement of existing files.
pub struct FileVersionSnapshot {
    /// Metadata captured from the same handle before the validated read.
    pub metadata: fs::Metadata,
    /// The same byte-and-metadata version returned by bounded text reads.
    pub version: String,
    /// Whether the file fails UTF-8 validation or contains NUL in its first 8 KiB.
    pub binary: bool,
}

/// Hashes one handle without retaining file contents or exposing binary bytes.
pub fn read_file_version_snapshot(path: &Path, maximum: u64) -> io::Result<FileVersionSnapshot> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(io::Error::other(
            "file exceeds snapshot size limit or is not a regular file",
        ));
    }
    let mut reader = file.take(metadata.len().saturating_add(1));
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024 + 3];
    let mut carried = 0;
    let mut total = 0_u64;
    let mut binary = false;
    loop {
        let count = reader.read(&mut buffer[carried..])?;
        if count == 0 {
            break;
        }
        let end = carried + count;
        let bytes = &buffer[carried..end];
        hash.update(bytes);
        let sniff = (8192_u64.saturating_sub(total)).min(count as u64) as usize;
        binary |= bytes[..sniff].contains(&0);
        total += count as u64;
        if !binary {
            match std::str::from_utf8(&buffer[..end]) {
                Ok(_) => carried = 0,
                Err(error) if error.error_len().is_none() => {
                    let valid = error.valid_up_to();
                    carried = end - valid;
                    buffer.copy_within(valid..end, 0);
                }
                Err(_) => {
                    binary = true;
                    carried = 0;
                }
            }
        } else {
            carried = 0;
        }
    }
    let after = reader.get_ref().metadata()?;
    if total != metadata.len() || !unchanged_metadata(&metadata, &after) {
        return Err(io::Error::other(
            "FILE_CONFLICT: file changed while it was being read",
        ));
    }
    let version = finish_file_version(hash, &metadata);
    Ok(FileVersionSnapshot {
        metadata,
        version,
        binary: binary || carried != 0,
    })
}

/// Reads a bounded snapshot from a single handle so the version matches its bytes.
pub fn read_file_snapshot(
    path: &Path,
    maximum: u64,
) -> io::Result<(Vec<u8>, fs::Metadata, String)> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(io::Error::other(
            "file exceeds snapshot size limit or is not a regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(io::Error::other("file exceeds snapshot size limit"));
    }
    let after = file.metadata()?;
    if bytes.len() as u64 != metadata.len() || !unchanged_metadata(&metadata, &after) {
        return Err(io::Error::other(
            "FILE_CONFLICT: file changed while it was being read",
        ));
    }
    let version = file_version(&metadata, &bytes);
    Ok((bytes, metadata, version))
}

/// Compares the expected snapshot with disk; absent files have no version.
pub fn check_file_version(path: &Path, expected: Option<&str>, maximum: u64) -> io::Result<()> {
    let current = match read_file_version_snapshot(path, maximum) {
        Ok(snapshot) => Some(snapshot.version),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if current.as_deref() != expected {
        return Err(io::Error::other(
            "FILE_CONFLICT: file changed or was deleted since it was read",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_detect_equal_length_changes_and_deletions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        fs::write(&path, "A0 B0").unwrap();
        let (_, _, version) = read_file_snapshot(&path, 100).unwrap();
        fs::write(&path, "A1 B0").unwrap();
        assert!(check_file_version(&path, Some(&version), 100).is_err());
        fs::remove_file(&path).unwrap();
        assert!(check_file_version(&path, Some(&version), 100).is_err());
        assert!(check_file_version(&path, None, 100).is_ok());
    }

    #[test]
    fn streamed_versions_match_byte_snapshots_and_preserve_binary_classification() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot");
        for bytes in [
            vec![b'a'; 65_538],
            [vec![b'a'; 65_538], "\u{10000}".as_bytes().to_vec()].concat(),
            [vec![b'a'; 65_538], vec![0xff]].concat(),
            [vec![b'a'; 8192], vec![0]].concat(),
            [vec![b'a'; 8191], vec![0]].concat(),
            [vec![b'a'; 65_538], vec![0xf0, 0x9f]].concat(),
        ] {
            fs::write(&path, &bytes).unwrap();
            let (_, metadata, version) = read_file_snapshot(&path, 100_000).unwrap();
            let streamed = read_file_version_snapshot(&path, 100_000).unwrap();
            assert_eq!(streamed.version, version);
            assert_eq!(streamed.metadata.len(), metadata.len());
            assert_eq!(
                streamed.binary,
                bytes[..bytes.len().min(8192)].contains(&0) || std::str::from_utf8(&bytes).is_err()
            );
        }
    }

    #[test]
    fn compare_and_write_allows_only_one_writer_for_one_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        fs::write(&path, "initial").unwrap();
        let (_, _, version) = read_file_snapshot(&path, 100).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writers: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|content| {
                let (path, version, barrier) = (path.clone(), version.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let _lock = lock_file(&path).unwrap();
                    if check_file_version(&path, Some(&version), 100).is_err() {
                        return false;
                    }
                    fs::write(path, content).unwrap();
                    true
                })
            })
            .collect();
        assert_eq!(
            writers
                .into_iter()
                .map(|writer| writer.join().unwrap())
                .filter(|written| *written)
                .count(),
            1
        );
    }
}
