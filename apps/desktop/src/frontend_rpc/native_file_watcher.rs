//! 系统目录变化通知与有界防抖；空闲时阻塞等待，不扫描目录或定时唤醒。

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const QUIET_PERIOD: Duration = Duration::from_millis(150);
const MAX_BATCH_DELAY: Duration = Duration::from_millis(750);

#[derive(Default)]
struct PendingChange {
    stopped: bool,
    first_at: Option<Instant>,
    deadline: Option<Instant>,
    only_path: Option<PathBuf>,
    unknown_path: bool,
}

impl PendingChange {
    fn push(&mut self, path: Option<PathBuf>, now: Instant) {
        if self.stopped {
            return;
        }
        let first = *self.first_at.get_or_insert(now);
        self.deadline = Some((now + QUIET_PERIOD).min(first + MAX_BATCH_DELAY));
        // 最多保留一个路径；多路径、重命名或通知溢出只发根目录刷新，不能误报首个文件。
        if path.is_none()
            || self
                .only_path
                .as_ref()
                .is_some_and(|old| Some(old) != path.as_ref())
        {
            self.unknown_path = true;
            self.only_path = None;
        } else if !self.unknown_path {
            self.only_path = path;
        }
    }

    fn take(&mut self) -> Option<PathBuf> {
        self.first_at = None;
        self.deadline = None;
        self.unknown_path = false;
        self.only_path.take()
    }
}

type Signal = Arc<(Mutex<PendingChange>, Condvar)>;

/// 一个原生监听与防抖线程；析构后不再发出回调，允许立即重新订阅。
pub(super) struct NativeFileWatch {
    watcher: Option<RecommendedWatcher>,
    signal: Signal,
    worker: Option<JoinHandle<()>>,
}

impl NativeFileWatch {
    pub(super) fn start(
        root: PathBuf,
        recursive: bool,
        emit: impl Fn(Option<PathBuf>) + Send + 'static,
    ) -> Result<Self, String> {
        let signal: Signal = Arc::new((Mutex::new(PendingChange::default()), Condvar::new()));
        let handler_signal = Arc::clone(&signal);
        let handler_root = root.clone();
        // RecommendedWatcher 是当前平台的系统事件后端；失败明确返回，不退回轮询。
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<Event>| {
                enqueue_event(&handler_signal, &handler_root, event)
            },
            Config::default().with_follow_symlinks(false),
        )
        .map_err(|error| format!("无法创建原生文件监听器：{error}"))?;
        watcher
            .watch(
                &root,
                if recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                },
            )
            .map_err(|error| format!("无法监听目录：{error}"))?;
        let worker_signal = Arc::clone(&signal);
        let worker = thread::Builder::new()
            .name("keencode-file-events".to_owned())
            .spawn(move || dispatch_changes(worker_signal, emit))
            .map_err(|error| format!("无法启动文件事件分发：{error}"))?;
        Ok(Self {
            watcher: Some(watcher),
            signal,
            worker: Some(worker),
        })
    }
}

impl Drop for NativeFileWatch {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.signal.0.lock() {
            pending.stopped = true;
            self.signal.1.notify_all();
        }
        // 先撤销系统监听，再等待分发退出；不把未确认的 pending batch 发给后继订阅。
        drop(self.watcher.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn enqueue_event(signal: &Signal, root: &Path, event: notify::Result<Event>) {
    let mut pending = match signal.0.lock() {
        Ok(pending) => pending,
        Err(_) => return,
    };
    match event {
        Ok(event) if !event.need_rescan() && matches!(event.kind, EventKind::Access(_)) => return,
        Ok(event) if !event.need_rescan() && !event.paths.is_empty() => {
            let mut accepted = false;
            for path in event.paths {
                if event_path_is_owned(root, &path) {
                    pending.push(Some(path), Instant::now());
                    accepted = true;
                }
            }
            if !accepted {
                return;
            }
        }
        // 后端错误/溢出只触发一次有界根目录重同步，实际数据仍由原 RPC 读取。
        _ => pending.push(None, Instant::now()),
    }
    signal.1.notify_one();
}

fn event_path_is_owned(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return false;
    }
    let mut ancestor = root.to_path_buf();
    for part in relative.components() {
        ancestor.push(part.as_os_str());
        // 只验证实际事件的祖先，不枚举目录；删除事件允许路径已不存在。
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }
    true
}

fn dispatch_changes(signal: Signal, emit: impl Fn(Option<PathBuf>)) {
    let mut pending = match signal.0.lock() {
        Ok(pending) => pending,
        Err(_) => return,
    };
    loop {
        if pending.stopped {
            break;
        }
        match pending.deadline {
            None => match signal.1.wait(pending) {
                Ok(next) => pending = next,
                Err(_) => break,
            },
            Some(deadline) if deadline > Instant::now() => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match signal.1.wait_timeout(pending, remaining) {
                    Ok((next, _)) => pending = next,
                    Err(_) => break,
                }
            }
            Some(_) => {
                let path = pending.take();
                drop(pending);
                emit(path);
                pending = match signal.0.lock() {
                    Ok(next) => next,
                    Err(_) => break,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn batch_preserves_one_file_and_invalidates_ambiguous_paths() {
        let now = Instant::now();
        let mut batch = PendingChange::default();
        batch.push(Some("a.txt".into()), now);
        batch.push(Some("a.txt".into()), now + QUIET_PERIOD);
        assert_eq!(batch.take(), Some("a.txt".into()));
        batch.push(Some("a.txt".into()), now);
        batch.push(Some("b.txt".into()), now);
        batch.push(Some("a.txt".into()), now);
        assert_eq!(batch.take(), None);
        batch.push(Some("a.txt".into()), now);
        batch.push(None, now);
        assert_eq!(batch.take(), None);
    }

    #[test]
    fn continuous_changes_have_a_bounded_flush_deadline() {
        let now = Instant::now();
        let mut batch = PendingChange::default();
        for i in 0..10 {
            batch.push(Some("a.txt".into()), now + Duration::from_millis(i * 100));
        }
        assert_eq!(batch.deadline, Some(now + MAX_BATCH_DELAY));
    }

    #[test]
    fn access_notifications_are_ignored_and_backend_errors_request_resync() {
        let signal: Signal = Arc::new((Mutex::new(PendingChange::default()), Condvar::new()));
        let directory = tempfile::tempdir().unwrap();
        enqueue_event(
            &signal,
            directory.path(),
            Ok(Event::new(EventKind::Access(
                notify::event::AccessKind::Any,
            ))),
        );
        assert!(signal.0.lock().unwrap().deadline.is_none());
        enqueue_event(
            &signal,
            directory.path(),
            Err(notify::Error::generic("overflow")),
        );
        let mut pending = signal.0.lock().unwrap();
        assert!(pending.deadline.is_some());
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn missing_directory_fails_instead_of_starting_a_polling_fallback() {
        let directory = tempfile::tempdir().unwrap();
        assert!(NativeFileWatch::start(directory.path().join("missing"), true, |_| {}).is_err());
    }

    #[test]
    fn native_events_report_changes_and_drop_discards_pending_work() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let (sender, receiver) = mpsc::channel();
        let watch = NativeFileWatch::start(root.clone(), true, move |path| {
            let _ = sender.send(path);
        })
        .unwrap();
        assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let file = nested.join("a.txt");
        std::fs::write(&file, "first").unwrap();
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        while receiver.try_recv().is_ok() {}
        std::fs::rename(&file, nested.join("b.txt")).unwrap();
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        std::fs::remove_file(nested.join("b.txt")).unwrap();
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        std::fs::write(root.join("pending.txt"), "pending").unwrap();
        drop(watch);
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn native_watch_can_be_recreated_and_reports_same_size_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "aaaa").unwrap();
        for text in ["bbbb", "cccc"] {
            let (sender, receiver) = mpsc::channel();
            let watch = NativeFileWatch::start(root.clone(), false, move |path| {
                let _ = sender.send(path);
            })
            .unwrap();
            std::fs::write(&file, text).unwrap();
            assert_eq!(
                receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
                Some(file.clone())
            );
            drop(watch);
        }
    }

    #[test]
    fn event_paths_do_not_escape_the_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        assert!(!event_path_is_owned(root, &root.join("../outside.txt")));
        assert!(!event_path_is_owned(
            root,
            &root.with_extension("other").join("a.txt")
        ));
        assert!(event_path_is_owned(root, &root.join("deleted.txt")));
    }
}
