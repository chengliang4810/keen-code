//! Tests for pool

use std::{collections::HashMap, sync::Arc, time::Duration};

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(unix)]
use std::time::Instant;

use peri_acp_types::ports::LspPoolPort;

use super::*;
use crate::config::{LspConfigFile, LspServerConfig};

fn make_config() -> LspConfigFile {
    let mut servers = HashMap::new();
    servers.insert(
        "rust-analyzer".to_string(),
        LspServerConfig {
            name: "rust-analyzer".to_string(),
            command: "rust-analyzer".to_string(),
            args: vec!["--stdio".to_string()],
            env: None,
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    servers.insert(
        "typescript".to_string(),
        LspServerConfig {
            name: "typescript-language-server".to_string(),
            command: "typescript-language-server".to_string(),
            args: vec!["--stdio".to_string()],
            env: None,
            extension_to_language: HashMap::from([
                (".ts".to_string(), "typescript".to_string()),
                (".tsx".to_string(), "typescriptreact".to_string()),
            ]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspConfigFile {
        lsp_servers: servers,
    }
}

#[test]
fn test_extension_routing() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.server_for_file("/test/main.rs").is_some());
    assert!(pool.server_for_file("/test/index.ts").is_some());
    assert!(pool.server_for_file("/test/App.tsx").is_some());
    assert!(pool.server_for_file("/test/readme.md").is_none());
    assert!(pool.server_for_file("/test/no_ext").is_none());
}

/// 类型不匹配的端口实现（downcast 失败路径用）
struct StubPool;

#[async_trait::async_trait]
impl LspPoolPort for StubPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) {}
}

/// 端口 downcast 往返：upcast 为 Arc<dyn LspPoolPort> 后经 downcast_arc
/// 还原为同一 LspServerPool 实例（装配面会话级复用前置条件，H1）。
#[test]
fn test_lsp_pool_port_downcast_roundtrip() {
    let pool: Arc<LspServerPool> = Arc::new(LspServerPool::new("/tmp", make_config()));
    let port: Arc<dyn LspPoolPort> = pool.clone();
    let restored = match port.downcast_arc::<LspServerPool>() {
        Ok(restored) => restored,
        Err(_) => panic!("类型匹配时应还原成功"),
    };
    assert!(
        Arc::ptr_eq(&pool, &restored),
        "downcast 应还原同一 pool 实例"
    );
    assert!(restored.has_servers());
}

/// 类型不匹配：downcast 失败返回原端口句柄（仍可调用 shutdown）。
#[tokio::test]
async fn test_lsp_pool_port_downcast_mismatch_returns_original() {
    let port: Arc<dyn LspPoolPort> = Arc::new(StubPool);
    let err = match port.downcast_arc::<LspServerPool>() {
        Ok(_) => panic!("类型不匹配时应还原失败"),
        Err(p) => p,
    };
    err.shutdown().await;
}

#[test]
fn test_case_insensitive_extension() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.server_for_file("/test/main.RS").is_some());
    assert!(pool.server_for_file("/test/main.TS").is_some());
}

#[test]
fn test_disabled_server() {
    let mut config = make_config();
    config
        .lsp_servers
        .get_mut("rust-analyzer")
        .unwrap()
        .disabled = Some(true);
    let pool = LspServerPool::new("/tmp", config);
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[test]
fn test_has_servers() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.has_servers());
}

#[test]
fn test_empty_config() {
    let pool = LspServerPool::new("/tmp", LspConfigFile::default());
    assert!(!pool.has_servers());
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[tokio::test]
async fn test_ensure_server_for_file_no_match() {
    let pool = LspServerPool::new("/tmp", make_config());
    // .md 文件没有匹配的 LSP 服务器
    let result = pool.ensure_server_for_file("/test/readme.md").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("readme.md"));
}

#[tokio::test]
async fn test_ensure_server_for_file_already_initialized() {
    let pool = LspServerPool::new("/tmp", make_config());
    // 手动标记为已初始化
    pool.initialized.write().insert("rust-analyzer".to_string());
    // 不应尝试启动
    let result = pool.ensure_server_for_file("/test/main.rs").await;
    assert!(result.is_ok());
    // typescript 仍然未初始化
    assert!(!pool.initialized.read().contains("typescript"));
}

/// perl 编写的极简 LSP 服务器（同 client_test.rs）：
/// 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 追加一行 "spawned"，对带 id 的请求回 result:null
const FAKE_LSP_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 构造以 perl fake server 为命令的 LspServerPool（仅 .rs 路由）
fn make_fake_pool(count_file: &std::path::Path) -> LspServerPool {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    let mut servers = HashMap::new();
    servers.insert(
        "fake-lsp".to_string(),
        LspServerConfig {
            name: "fake-lsp".to_string(),
            command: "perl".to_string(),
            args: vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
            env: Some(env),
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspServerPool::new(
        "/tmp",
        LspConfigFile {
            lsp_servers: servers,
        },
    )
}

/// 同 FAKE_LSP_SCRIPT，另将子进程的操作系统 PID 写入
/// `$ENV{PERI_LSP_TEST_PID}`（生命周期断言用）。
///
/// Git for Windows 自带的 MSYS Perl 使用独立的 POSIX PID 命名空间，
/// 因此 Windows 必须通过 Win32 API 记录可被 `tasklist` 查询的真实 PID。
const FAKE_LSP_SCRIPT_WITH_PID: &str = r#"open my $p, '>', $ENV{PERI_LSP_TEST_PID} or exit 1;
my $pid = $$;
if ($^O eq 'MSWin32' || $^O eq 'msys') {
    require Win32;
    $pid = Win32::GetCurrentProcessId();
}
print $p "$pid\n";
close $p;
open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 构造记录 PID 的 fake pool（仅 .rs 路由）
fn make_fake_pool_with_pid(
    count_file: &std::path::Path,
    pid_file: &std::path::Path,
) -> LspServerPool {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_PID".to_string(),
        pid_file.to_string_lossy().into_owned(),
    );
    let mut servers = HashMap::new();
    servers.insert(
        "fake-lsp".to_string(),
        LspServerConfig {
            name: "fake-lsp".to_string(),
            command: "perl".to_string(),
            args: vec!["-e".to_string(), FAKE_LSP_SCRIPT_WITH_PID.to_string()],
            env: Some(env),
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspServerPool::new(
        "/tmp",
        LspConfigFile {
            lsp_servers: servers,
        },
    )
}

/// Unix 通过 kill -0 查询进程状态，同时保留命令诊断信息。
#[cfg(unix)]
fn probe_process(pid: u32) -> Result<(bool, String), String> {
    let output = std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .map_err(|error| format!("执行 kill -0 失败: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Ok((
        output.status.success(),
        format!(
            "kill -0 status={:?}, stdout={stdout:?}, stderr={stderr:?}",
            output.status.code()
        ),
    ))
}

/// Windows 进程句柄守卫：在 shutdown 前打开并持有同一进程对象。
///
/// 句柄绑定的是内核进程对象而非 PID 数值；即使关闭后 PID 被复用，
/// `WaitForSingleObject` 仍然只观察原服务器进程，不会产生假阳性。
#[cfg(windows)]
struct WindowsProcessHandle {
    /// 句柄所属的服务器进程 PID，仅用于错误诊断。
    pid: u32,
    /// 独占持有的原生进程句柄，离开作用域时由 OwnedHandle 自动关闭。
    handle: OwnedHandle,
}

#[cfg(windows)]
impl WindowsProcessHandle {
    /// 按真实 Windows PID 打开可查询、可等待的进程句柄。
    fn open(pid: u32) -> Result<Self, String> {
        use windows_sys::Win32::Foundation::{
            GetLastError, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };

        // SAFETY: PID 来自刚启动的测试子进程；不继承句柄，访问权限仅限查询与等待。
        let raw_handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if raw_handle.is_null() {
            // SAFETY: OpenProcess 刚返回空句柄，中间没有其他 Win32 调用改写 last-error。
            let code = unsafe { GetLastError() };
            let reason = match code {
                ERROR_INVALID_PARAMETER => "进程不存在或 PID 无效",
                ERROR_ACCESS_DENIED => "打开进程句柄被拒绝",
                _ => "未知 Win32 错误",
            };
            return Err(format!(
                "OpenProcess(pid={pid}) 失败: {reason}, code={code}, error={}",
                std::io::Error::from_raw_os_error(code as i32)
            ));
        }

        // SAFETY: raw_handle 是 OpenProcess 成功返回的独占句柄，现在将其所有权转交给 OwnedHandle。
        let handle = unsafe { OwnedHandle::from_raw_handle(raw_handle) };
        Ok(Self { pid, handle })
    }

    /// 立即查询原进程对象是否仍未退出。
    fn is_alive(&self) -> Result<bool, String> {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};

        match self.wait_status(0)? {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            status => Err(format!(
                "WaitForSingleObject(pid={}) 返回未知状态 {status}",
                self.pid
            )),
        }
    }

    /// 在有界时间内等待原进程对象退出。
    fn wait_for_exit(&self, timeout: Duration) -> Result<(), String> {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};

        let timeout_ms = u32::try_from(timeout.as_millis())
            .map_err(|_| format!("进程 {} 等待时间超出 Win32 范围", self.pid))?;
        match self.wait_status(timeout_ms)? {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(format!("进程 {} 在 {timeout_ms}ms 内未退出", self.pid)),
            status => Err(format!(
                "WaitForSingleObject(pid={}) 返回未知状态 {status}",
                self.pid
            )),
        }
    }

    /// 调用 Win32 等待 API，并将 WAIT_FAILED 转为带错误码的诊断。
    fn wait_status(&self, timeout_ms: u32) -> Result<u32, String> {
        use windows_sys::Win32::Foundation::{GetLastError, WAIT_FAILED};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        // SAFETY: handle 由 OwnedHandle 持有且在本调用期间保持有效。
        let status = unsafe { WaitForSingleObject(self.handle.as_raw_handle(), timeout_ms) };
        if status == WAIT_FAILED {
            // SAFETY: WaitForSingleObject 刚返回 WAIT_FAILED，可立即读取 last-error。
            let code = unsafe { GetLastError() };
            return Err(format!(
                "WaitForSingleObject(pid={}) 失败: code={code}, error={}",
                self.pid,
                std::io::Error::from_raw_os_error(code as i32)
            ));
        }
        Ok(status)
    }
}

/// Unix 在有界时间内等待进程表达到预期状态。
#[cfg(unix)]
async fn wait_for_process_state(pid: u32, expected_alive: bool) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let diagnostic = match probe_process(pid) {
            Ok((alive, _)) if alive == expected_alive => return Ok(()),
            Ok((alive, diagnostic)) => format!("alive={alive}; {diagnostic}"),
            Err(diagnostic) => diagnostic,
        };
        if Instant::now() >= deadline {
            let expected = if expected_alive { "存活" } else { "退出" };
            return Err(format!(
                "等待进程 {pid} {expected} 超时；最后一次探测: {diagnostic}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 生命周期：pool.shutdown() 后 LSP 服务器子进程必须退出（H1 进程泄漏验证）。
#[tokio::test]
async fn test_shutdown_kills_child_process() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let pid_file = dir.path().join("server.pid");
    let pool = make_fake_pool_with_pid(&count_file, &pid_file);

    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .expect("fake server 应写出 PID 文件")
        .trim()
        .parse()
        .expect("PID 应为数字");
    #[cfg(windows)]
    let process = WindowsProcessHandle::open(pid)
        .unwrap_or_else(|error| panic!("启动后应能打开服务器进程句柄: {error}"));
    #[cfg(windows)]
    assert!(
        process
            .is_alive()
            .unwrap_or_else(|error| panic!("查询服务器进程失败: {error}")),
        "启动后服务器进程应存活（pid={pid}）"
    );
    #[cfg(unix)]
    wait_for_process_state(pid, true)
        .await
        .unwrap_or_else(|error| panic!("启动后服务器进程应存活: {error}"));

    pool.shutdown().await;

    #[cfg(windows)]
    process
        .wait_for_exit(Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("shutdown 后服务器子进程应退出: {error}"));
    #[cfg(unix)]
    wait_for_process_state(pid, false)
        .await
        .unwrap_or_else(|error| panic!("shutdown 后服务器子进程应退出: {error}"));
}

/// 生命周期：shutdown 清空 initialized；再次 ensure 重新 spawn（不残留旧进程复用）。
#[tokio::test]
async fn test_shutdown_then_ensure_respawns() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let pool = make_fake_pool(&count_file);

    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    assert_eq!(pool.initialized.read().len(), 1);

    pool.shutdown().await;
    assert!(
        pool.initialized.read().is_empty(),
        "shutdown 后 initialized 应清空"
    );

    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    let count = std::fs::read_to_string(&count_file)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert_eq!(count, 2, "shutdown 后 ensure 应重新 spawn 而非复用已死进程");

    pool.shutdown().await;
}

#[tokio::test]
async fn test_concurrent_ensure_server_for_file_spawns_once() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let pool = make_fake_pool(&count_file);

    let (r1, r2) = tokio::join!(
        pool.ensure_server_for_file("/test/main.rs"),
        pool.ensure_server_for_file("/test/lib.rs"),
    );

    assert!(r1.is_ok(), "第一个 ensure 失败: {:?}", r1.err());
    assert!(r2.is_ok(), "第二个 ensure 失败: {:?}", r2.err());
    let count = std::fs::read_to_string(&count_file)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert_eq!(
        count, 1,
        "并发 ensure_server_for_file 只应 spawn 一次子进程"
    );
    assert_eq!(pool.initialized.read().len(), 1, "initialized 只插入一次");

    pool.shutdown().await;
}
