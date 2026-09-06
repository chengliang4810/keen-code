mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use keencode_resources::{
    JournalConfig, ResourceError, SessionEvent, SessionId, SessionJournal, SessionLease,
    SessionLeaseAcquire, SessionOpen,
};
use tempfile::TempDir;

use support::TestJournalAppend;

/// 子进程测试传递存储根目录的环境变量。
const HELPER_ROOT_ENV: &str = "KEENCODE_SESSION_LEASE_HELPER_ROOT";
/// 子进程测试传递 Session 标识的环境变量。
const HELPER_SESSION_ENV: &str = "KEENCODE_SESSION_LEASE_HELPER_SESSION";
/// 子进程确认已经持有 lease 的标准输出标记。
const HELPER_ACQUIRED_MARKER: &str = "KEENCODE_SESSION_LEASE_ACQUIRED";

/// 获取目标 Session lease，并把意外 Busy 视为测试失败。
fn acquire(root: &Path, session: &str) -> SessionLease {
    match SessionLease::try_acquire(root, SessionId::new(session).expect("Session ID 应有效"))
        .expect("Session lease 获取不应发生 IO 错误")
    {
        SessionLeaseAcquire::Acquired(lease) => lease,
        SessionLeaseAcquire::Busy { .. } => panic!("Session lease 不应处于 Busy"),
    }
}

/// 断言目标 Session 当前被另一个 lease 占用。
fn assert_busy(root: &Path, session: &str) {
    let result =
        SessionLease::try_acquire(root, SessionId::new(session).expect("Session ID 应有效"))
            .expect("竞争 lease 应返回可分类结果");
    assert!(matches!(
        result,
        SessionLeaseAcquire::Busy { session_id } if session_id.as_str() == session
    ));
}

/// 返回固定 Runtime lock 文件路径。
fn runtime_lock_path(root: &Path, session: &str) -> PathBuf {
    root.join("sessions").join(session).join("runtime.lock")
}

/// 在支持的系统上创建文件符号链接。
fn try_symlink_file(source: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(source, target).is_ok()
    }
}

/// 启动只持有 lease 并等待标准输入关闭的当前测试二进制子进程。
fn spawn_lease_helper(root: &Path, session: &str) -> (Child, BufReader<ChildStdout>) {
    let mut child = Command::new(std::env::current_exe().expect("测试可执行文件路径应读取"))
        .args([
            "--exact",
            "session_lease_process_helper",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ROOT_ENV, root)
        .env(HELPER_SESSION_ENV, session)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("lease helper 子进程应启动");
    let stdout = child.stdout.take().expect("helper 标准输出应存在");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).expect("helper 输出应读取");
        assert!(read > 0, "helper 未持有 lease 就提前退出");
        if line.contains(HELPER_ACQUIRED_MARKER) {
            break;
        }
    }
    (child, reader)
}

/// 仅由父测试作为子进程运行，持有 lease 直到标准输入关闭或进程被终止。
#[test]
#[ignore = "只作为 Session lease 跨进程测试 helper 运行"]
fn session_lease_process_helper() {
    let Some(root) = std::env::var_os(HELPER_ROOT_ENV) else {
        return;
    };
    let Some(session) = std::env::var_os(HELPER_SESSION_ENV) else {
        return;
    };
    let session = session.to_string_lossy();
    let _lease = acquire(Path::new(&root), &session);
    println!("{HELPER_ACQUIRED_MARKER}");
    std::io::stdout().flush().expect("helper 标记应刷新");
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("helper 标准输入应读取到关闭");
}

/// 验证同一进程的第二个句柄也只能得到非阻塞 Busy。
#[test]
fn same_process_competition_is_busy() {
    let root = TempDir::new().expect("临时目录应创建");
    let _lease = acquire(root.path(), "lease-same-process");
    assert_busy(root.path(), "lease-same-process");
}

/// 验证不同 Session 的 lease 彼此隔离。
#[test]
fn different_sessions_can_be_leased_concurrently() {
    let root = TempDir::new().expect("临时目录应创建");
    let _first = acquire(root.path(), "lease-session-a");
    let _second = acquire(root.path(), "lease-session-b");
}

/// 验证 Drop 会释放操作系统锁，但保留永久空锁文件。
#[test]
fn drop_releases_lease_and_preserves_empty_lock_file() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "lease-drop";
    let path = runtime_lock_path(root.path(), session);
    let lease = acquire(root.path(), session);
    assert_eq!(lease.session_id().as_str(), session);
    assert_eq!(
        lease.session_dir(),
        fs::canonicalize(path.parent().expect("锁文件应有父目录")).expect("Session 目录应规范化")
    );
    assert!(path.is_file());
    assert_eq!(fs::metadata(&path).expect("锁文件元数据应读取").len(), 0);
    assert_busy(root.path(), session);
    drop(lease);

    assert!(path.is_file());
    assert_eq!(fs::metadata(&path).expect("锁文件元数据应读取").len(), 0);
    let _reacquired = acquire(root.path(), session);
}

/// 验证线程 panic 展开并丢弃凭证后不会遗留进程内 lease。
#[test]
fn panic_unwind_releases_lease() {
    let root = TempDir::new().expect("临时目录应创建");
    let root_path = root.path().to_owned();
    let panicked = thread::spawn(move || {
        let _lease = acquire(&root_path, "lease-panic");
        panic!("触发测试用 panic");
    })
    .join();
    assert!(panicked.is_err());
    let _reacquired = acquire(root.path(), "lease-panic");
}

/// 验证首次并发创建目录和锁文件时只有一个租约成功，其余稳定返回 Busy。
#[test]
fn concurrent_first_acquire_revalidates_already_created_directories() {
    let root = TempDir::new().expect("临时目录应创建");
    let root_path = root.path().to_owned();
    let barrier = Arc::new(Barrier::new(32));
    let handles = (0..32)
        .map(|_| {
            let root_path = root_path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                SessionLease::try_acquire(
                    root_path,
                    SessionId::new("lease-concurrent-first").expect("Session ID 应有效"),
                )
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("并发线程不应 panic")
                .expect("并发获取不应发生 IO 错误")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, SessionLeaseAcquire::Acquired(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, SessionLeaseAcquire::Busy { .. }))
            .count(),
        31
    );
    assert_eq!(
        fs::metadata(runtime_lock_path(root.path(), "lease-concurrent-first"))
            .expect("并发锁文件应存在")
            .len(),
        0
    );
    drop(outcomes);
    let _reacquired = acquire(root.path(), "lease-concurrent-first");
}

/// 验证目录、非空文件和符号链接均不能伪装成 runtime.lock。
#[test]
fn invalid_runtime_lock_targets_fail_closed() {
    let root = TempDir::new().expect("临时目录应创建");

    let directory_path = runtime_lock_path(root.path(), "lease-lock-directory");
    fs::create_dir_all(&directory_path).expect("非法锁目录应创建");
    assert!(matches!(
        SessionLease::try_acquire(
            root.path(),
            SessionId::new("lease-lock-directory").expect("Session ID 应有效")
        ),
        Err(ResourceError::UnsafePath(_))
    ));

    let nonempty_path = runtime_lock_path(root.path(), "lease-lock-nonempty");
    fs::create_dir_all(nonempty_path.parent().expect("锁文件应有父目录"))
        .expect("Session 目录应创建");
    fs::write(&nonempty_path, b"not-empty").expect("非空锁文件应创建");
    assert!(matches!(
        SessionLease::try_acquire(
            root.path(),
            SessionId::new("lease-lock-nonempty").expect("Session ID 应有效")
        ),
        Err(ResourceError::UnsafePath(_))
    ));
    assert_eq!(
        fs::read(&nonempty_path).expect("非空锁文件应保持原样"),
        b"not-empty"
    );

    let symlink_path = runtime_lock_path(root.path(), "lease-lock-symlink");
    fs::create_dir_all(symlink_path.parent().expect("锁文件应有父目录"))
        .expect("Session 目录应创建");
    let outside = root.path().join("outside-lock");
    fs::write(&outside, b"").expect("外部文件应创建");
    if try_symlink_file(&outside, &symlink_path) {
        assert!(matches!(
            SessionLease::try_acquire(
                root.path(),
                SessionId::new("lease-lock-symlink").expect("Session ID 应有效")
            ),
            Err(ResourceError::SymlinkRejected(_))
        ));
    }
}

/// 验证持有 Runtime lease 时仍可按固定顺序获取 append.lock 并正常写 Journal。
#[test]
fn journal_operations_work_while_runtime_lease_is_held() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = SessionId::new("lease-journal-order").expect("Session ID 应有效");
    let _lease =
        match SessionLease::try_acquire(root.path(), session_id.clone()).expect("lease 应获取") {
            SessionLeaseAcquire::Acquired(lease) => lease,
            SessionLeaseAcquire::Busy { .. } => panic!("新 Session 不应 Busy"),
        };
    let journal = match SessionJournal::open(root.path(), session_id, JournalConfig::default())
        .expect("Journal 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("Journal 不应损坏：{:?}", report.issues),
    };
    journal
        .append(SessionEvent::SessionCreated {
            title: "Lease Journal".to_owned(),
            project_root: "D:/workspace".to_owned(),
        })
        .expect("持有 lease 时 Journal 应追加");
    journal
        .write_snapshot()
        .expect("持有 lease 时应写 Snapshot");
    assert_eq!(
        journal.state().expect("Journal 状态应读取").last_sequence,
        1
    );
}

/// 验证独立子进程持有 lease 时父进程得到 Busy，正常退出后立即可重取。
#[test]
fn cross_process_contention_and_graceful_release() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "lease-cross-process";
    let (mut child, _stdout) = spawn_lease_helper(root.path(), session);
    assert_busy(root.path(), session);
    drop(child.stdin.take());
    assert!(child.wait().expect("helper 应退出").success());
    let _reacquired = acquire(root.path(), session);
}

/// 验证强制终止 lease 持有进程后，操作系统会释放锁且不删除空锁文件。
#[test]
fn killed_process_releases_lease() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = "lease-killed-process";
    let path = runtime_lock_path(root.path(), session);
    let (mut child, _stdout) = spawn_lease_helper(root.path(), session);
    assert_busy(root.path(), session);
    child.kill().expect("helper 应可终止");
    child.wait().expect("helper 终止状态应回收");

    let mut reacquired = None;
    for _ in 0..100 {
        match SessionLease::try_acquire(
            root.path(),
            SessionId::new(session).expect("Session ID 应有效"),
        )
        .expect("终止后获取不应发生 IO 错误")
        {
            SessionLeaseAcquire::Acquired(lease) => {
                reacquired = Some(lease);
                break;
            }
            SessionLeaseAcquire::Busy { .. } => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(reacquired.is_some(), "终止进程后 lease 必须释放");
    assert!(path.is_file());
    assert_eq!(fs::metadata(path).expect("锁文件元数据应读取").len(), 0);
}
