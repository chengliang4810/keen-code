//! 工作区搜索在 Rust 保存唯一有界索引；Renderer 只接收 top-K，不复制全目录到 Worker。
use super::native_file_watcher::NativeFileWatch;
use ignore::{WalkBuilder, gitignore::GitignoreBuilder};
use serde_json::{Value, json};
use std::{
    collections::{BinaryHeap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const MAX_ROOTS: usize = 2;
const MAX_INDEX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 250_000;
const MAX_RESULTS: usize = 1000;

struct Entry {
    relative: String,
    name_lower: String,
    relative_lower: String,
    directory: bool,
}

#[derive(Default)]
struct Snapshot {
    generation: u64,
    entries: Vec<Entry>,
}

struct Index {
    root: PathBuf,
    generation: Arc<AtomicU64>,
    snapshot: Mutex<Snapshot>,
    // watcher 的析构停止原生通知；不能将缓存淘汰变成常驻后台任务。
    _watch: NativeFileWatch,
}

struct Lease {
    index: Arc<Index>,
    owners: HashSet<String>,
}

#[derive(Default)]
struct Cache {
    leases: Vec<Lease>,
    closed: VecDeque<String>,
}
static INDEXES: OnceLock<Mutex<Cache>> = OnceLock::new();
fn indexes() -> &'static Mutex<Cache> {
    INDEXES.get_or_init(|| Mutex::new(Cache::default()))
}

pub(super) fn connection_closed(owner: &str) {
    let retired = {
        let Ok(mut cache) = indexes().lock() else {
            return;
        };
        // blocking 扫描可能晚于 RPC close 才开始，有限 tombstone 拒绝这类迟到的租约。
        if !cache.closed.iter().any(|value| value == owner) {
            cache.closed.push_back(owner.to_owned());
            if cache.closed.len() > 256 {
                cache.closed.pop_front();
            }
        }
        for lease in cache.leases.iter_mut() {
            lease.owners.remove(owner);
        }
        let mut retired = Vec::new();
        let mut i = 0;
        while i < cache.leases.len() {
            if cache.leases[i].owners.is_empty() {
                retired.push(cache.leases.remove(i));
            } else {
                i += 1;
            }
        }
        retired
    };
    drop(retired);
}

fn acquire(root: &Path, owner: &str) -> Result<Arc<Index>, String> {
    let mut cache = indexes().lock().map_err(|_| "文件索引锁失败")?;
    if cache.closed.iter().any(|value| value == owner) {
        return Err("搜索连接已关闭".to_owned());
    }
    if let Some(position) = cache
        .leases
        .iter()
        .position(|lease| lease.index.root == root)
    {
        let mut lease = cache.leases.remove(position);
        lease.owners.insert(owner.to_owned());
        let index = Arc::clone(&lease.index);
        cache.leases.push(lease);
        return Ok(index);
    }
    let generation = Arc::new(AtomicU64::new(1));
    let changed = Arc::clone(&generation);
    let watch = NativeFileWatch::start(root.to_owned(), true, move |_| {
        changed.fetch_add(1, Ordering::Release);
    })?;
    let index = Arc::new(Index {
        root: root.to_owned(),
        generation,
        snapshot: Mutex::new(Snapshot::default()),
        _watch: watch,
    });
    let retired = if cache.leases.len() >= MAX_ROOTS {
        Some(cache.leases.remove(0))
    } else {
        None
    };
    cache.leases.push(Lease {
        index: Arc::clone(&index),
        owners: HashSet::from([owner.to_owned()]),
    });
    drop(cache);
    drop(retired);
    Ok(index)
}

fn build(root: &Path) -> Result<Vec<Entry>, String> {
    let ignore_path = root.join(".zcodeignore");
    let rules = match std::fs::read_to_string(&ignore_path) {
        Ok(rules) => rules,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ".git\nnode_modules\ntarget\ndist\n".to_owned()
        }
        Err(error) => return Err(format!("读取搜索忽略规则失败：{error}")),
    };
    let mut builder = GitignoreBuilder::new(root);
    for line in rules.lines() {
        builder
            .add_line(Some(ignore_path.clone()), line)
            .map_err(|error| format!("搜索忽略规则无效：{error}"))?;
    }
    let matcher = builder
        .build()
        .map_err(|error| format!("搜索忽略规则无效：{error}"))?;
    let walker = WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(move |entry| {
            entry.depth() == 0
                || (!is_link(entry.path())
                    && !matcher
                        .matched(
                            entry.path(),
                            entry.file_type().is_some_and(|kind| kind.is_dir()),
                        )
                        .is_ignore())
        })
        .build();
    let mut entries = Vec::new();
    let mut bytes = 0;
    for entry in walker {
        let entry = entry.map_err(|error| format!("扫描工作区失败：{error}"))?;
        if entry.depth() == 0 {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| "索引路径越界")?
            .to_string_lossy()
            .replace('\\', "/");
        let name_lower = relative
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let relative_lower = relative.trim().to_lowercase();
        bytes += relative.capacity()
            + name_lower.capacity()
            + relative_lower.capacity()
            + std::mem::size_of::<Entry>();
        // 显式报错，不能悄悄裁掉目录尾部而让真实文件永远无法找到。
        if bytes > MAX_INDEX_BYTES || entries.len() >= MAX_ENTRIES {
            return Err(
                "工作区搜索索引超过 32 MiB / 250000 项限制，请配置 .zcodeignore".to_owned(),
            );
        }
        entries.push(Entry {
            relative,
            name_lower,
            relative_lower,
            directory: entry.file_type().is_some_and(|kind| kind.is_dir()),
        });
    }
    Ok(entries)
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
        }
        #[cfg(not(windows))]
        {
            metadata.file_type().is_symlink()
        }
    })
}

/// 与来源 TS 的 UTF-16 分数保持一致；索引稳定排序作为相同分数的次序。
fn score(text: &str, query: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let length = || {
        text.encode_utf16()
            .count()
            .saturating_sub(query.encode_utf16().count())
    };
    if text.starts_with(query) {
        return Some(length());
    }
    if let Some(offset) = text.find(query) {
        return Some(100 + text[..offset].encode_utf16().count());
    }
    if !text.is_ascii() || !query.is_ascii() {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut start = 0;
        let mut gaps = 0;
        for character in query.chars() {
            let mut buffer = [0; 2];
            let needle = character.encode_utf16(&mut buffer);
            let found = units[start..]
                .windows(needle.len())
                .position(|part| part == needle)?
                + start;
            gaps += found - start;
            // 来源 JS 的 indexOf 后只前进一个 UTF-16 单元，包含补充平面字符时仍保持分数一致。
            start = found + 1;
        }
        return Some(200 + gaps + length());
    }
    let mut start = 0;
    let mut gaps = 0;
    for character in query.chars() {
        let found = text[start..].find(character)? + start;
        gaps += text[start..found].encode_utf16().count();
        start = found + character.len_utf8();
    }
    Some(200 + gaps + length())
}

pub(super) fn search(root: PathBuf, owner: String, args: Value) -> Result<Value, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or("缺少参数 query")?
        .trim()
        .to_lowercase();
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_RESULTS as u64)
        .min(MAX_RESULTS as u64) as usize;
    let words = args.get("matchMode").and_then(Value::as_str) == Some("substring-words");
    if limit == 0
        || (query.is_empty() && args.get("requireQuery").and_then(Value::as_bool) == Some(true))
    {
        return Ok(json!([]));
    }
    let index = acquire(&root, &owner)?;
    let generation = index.generation.load(Ordering::Acquire);
    let mut snapshot = index.snapshot.lock().map_err(|_| "文件索引锁失败")?;
    if snapshot.generation != generation
        || args.get("refresh").and_then(Value::as_bool) == Some(true)
    {
        let entries = build(&root)?;
        snapshot.entries = entries;
        // 在扫描期间发生变动时不标为最新；下一次请求再扫描，绝不丢掉失效通知。
        snapshot.generation = generation;
    }
    let root_lower = crate::path_utils::path_to_frontend(&root)
        .trim()
        .to_lowercase();
    let mut best = BinaryHeap::with_capacity(limit + 1);
    for (position, entry) in snapshot.entries.iter().enumerate() {
        let rank = if words {
            if entry.directory
                || !query.split_whitespace().all(|word| {
                    entry.name_lower.contains(word) || entry.relative_lower.contains(word)
                })
            {
                continue;
            }
            0
        } else if query.is_empty() {
            usize::from(entry.directory)
        } else {
            let name = score(&entry.name_lower, &query);
            let relative = score(&entry.relative_lower, &query).map(|rank| rank + 25);
            // Windows 绝对路径仍用反斜线，relativePath 仍用斜线，兼容来源的两种匹配。
            let separator = if root_lower.contains('\\') { "\\" } else { "/" };
            let absolute = score(
                &format!(
                    "{root_lower}{separator}{}",
                    entry.relative_lower.replace('/', separator)
                ),
                &query,
            )
            .map(|rank| rank + 300);
            let Some(rank) = [name, relative, absolute].into_iter().flatten().min() else {
                continue;
            };
            rank
        };
        best.push((rank, position));
        if best.len() > limit {
            best.pop();
        }
    }
    let results: Vec<Value> = best.into_sorted_vec().into_iter().map(|(_, position)| {
        let entry = &snapshot.entries[position];
        json!({"name": entry.relative.rsplit('/').next().unwrap_or_default(), "path": crate::path_utils::path_to_frontend(&root.join(&entry.relative)), "relativePath": entry.relative, "type": if entry.directory { "directory" } else { "file" }})
    }).collect();
    Ok(Value::Array(results))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuzzy_scores_follow_utf16_and_subsequence_contract() {
        assert_eq!(score("module.rs", "mod"), Some(6));
        assert_eq!(score("源😀/模块.rs", "模块"), Some(104));
        assert_eq!(score("src/module.rs", "smrs"), Some(218));
        assert_eq!(score("😀x😀", "😀😀"), Some(203));
        assert_eq!(score("other.rs", "xyz"), None);
    }
    #[test]
    fn index_is_bounded_invalidates_and_disposes_with_connection() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("模块.rs"), "fixture").unwrap();
        std::fs::create_dir(root.path().join("target")).unwrap();
        std::fs::write(root.path().join("target/hidden.rs"), "fixture").unwrap();
        let query =
            |value| search(root.path().to_owned(), "test-search".to_owned(), value).unwrap();
        assert_eq!(
            query(json!({"query":"模块","limit":1}))[0]["relativePath"],
            "模块.rs"
        );
        assert_eq!(query(json!({"query":"hidden"})), json!([]));
        std::fs::write(root.path().join("new.rs"), "fixture").unwrap();
        assert_eq!(
            query(json!({"query":"new","refresh":true}))[0]["name"],
            "new.rs"
        );
        // 确认失效来自真实文件系统通知，而非靠 refresh 绕过缓存使测试通过。
        let index = acquire(root.path(), "test-search").unwrap();
        let previous = index.generation.load(Ordering::Acquire);
        std::fs::write(root.path().join("event.rs"), "fixture").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while index.generation.load(Ordering::Acquire) == previous
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(index.generation.load(Ordering::Acquire) > previous);
        assert_eq!(query(json!({"query":"event"}))[0]["name"], "event.rs");
        std::fs::write(root.path().join(".zcodeignore"), "*.rs\n!new.rs\n").unwrap();
        assert_eq!(query(json!({"query":"模块","refresh":true})), json!([]));
        connection_closed("test-search");
        assert!(acquire(root.path(), "test-search").is_err());
        assert!(
            !indexes()
                .lock()
                .unwrap()
                .leases
                .iter()
                .any(|lease| lease.index.root == root.path())
        );
    }
}
