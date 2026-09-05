use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use peri_agent::agent::async_tasks::{new_std_command, new_tokio_command};
use tempfile::{tempdir, TempDir};
use tokio::sync::mpsc;

use super::{fetch::*, *};
use crate::plugin::types::{MarketplacePlugin, PluginId};

/// 创建本地 Git marketplace fixture；未安装 Git 的环境跳过依赖 Git 的测试。
fn create_git_marketplace_fixture() -> Option<(TempDir, PathBuf)> {
    let version = new_std_command("git").arg("--version").output().ok()?;
    if !version.status.success() {
        return None;
    }

    let repository = tempdir().expect("创建 Git fixture 目录");
    let repository_path = repository.path().to_string_lossy().into_owned();
    let init = new_std_command("git")
        .args(["init", "--quiet", "--", &repository_path])
        .output()
        .expect("执行 git init");
    assert!(init.status.success(), "git init 失败: {:?}", init.stderr);

    std::fs::write(
        repository.path().join("marketplace.json"),
        r#"{"name":"local-marketplace","plugins":[]}"#,
    )
    .expect("写入 marketplace fixture");
    let add = new_std_command("git")
        .args(["-C", &repository_path, "add", "--", "marketplace.json"])
        .output()
        .expect("执行 git add");
    assert!(add.status.success(), "git add 失败: {:?}", add.stderr);
    let commit = new_std_command("git")
        .args([
            "-c",
            "user.email=peri-tests@example.invalid",
            "-c",
            "user.name=peri-tests",
            "-C",
            &repository_path,
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ])
        .output()
        .expect("执行 git commit");
    assert!(
        commit.status.success(),
        "git commit 失败: {:?}",
        commit.stderr
    );

    Some((repository, PathBuf::from(repository_path)))
}

/// 断言缓存根目录中没有 Git 临时 checkout 遗留目录。
fn assert_no_git_temporary_directories(cache_base: &std::path::Path) {
    let leftovers: Vec<_> = std::fs::read_dir(cache_base)
        .expect("读取缓存根目录")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("peri-git-clone-")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "Git 临时 checkout 未清理: {leftovers:?}"
    );
}

#[test]
fn test_find_marketplace_json_root() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("marketplace.json"), "{}").unwrap();
    let result = find_marketplace_json(dir.path());
    assert!(result.is_some());
    assert_eq!(result.unwrap().file_name().unwrap(), "marketplace.json");
}

#[test]
fn test_find_marketplace_json_subdir() {
    let dir = tempdir().unwrap();
    let subdir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(subdir.join("marketplace.json"), "{}").unwrap();
    let result = find_marketplace_json(dir.path());
    assert!(result.is_some());
}

#[test]
fn test_find_marketplace_json_not_found() {
    let dir = tempdir().unwrap();
    let result = find_marketplace_json(dir.path());
    assert!(result.is_none());
}

#[test]
fn test_find_marketplace_json_priority() {
    let dir = tempdir().unwrap();
    let subdir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(dir.path().join("marketplace.json"), "root").unwrap();
    std::fs::write(subdir.join("marketplace.json"), "sub").unwrap();
    let result = find_marketplace_json(dir.path()).unwrap();
    let content = std::fs::read_to_string(result).unwrap();
    assert_eq!(content, "root");
}

#[test]
fn test_read_manifest_from_path_success() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("marketplace.json");
    let json = r#"{"name":"test","plugins":[]}"#;
    std::fs::write(&path, json).unwrap();
    let manifest = read_manifest_from_path(&path).unwrap();
    assert_eq!(manifest.name, "test");
    assert!(manifest.plugins.is_empty());
}

#[test]
fn test_read_manifest_from_path_invalid_json() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("marketplace.json");
    std::fs::write(&path, "not json").unwrap();
    let result = read_manifest_from_path(&path);
    assert!(result.is_err());
    match result.unwrap_err() {
        MarketplaceError::ParseFailed(_) => {}
        _ => panic!("expected ParseFailed"),
    }
}

#[test]
fn test_read_manifest_from_path_not_found() {
    let result = read_manifest_from_path(Path::new("/nonexistent/path.json"));
    assert!(result.is_err());
}

#[test]
fn test_fetch_github_cache_hit() {
    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    let cache_dir = cache_base.join("test-repo");
    let plugin_dir = cache_dir.join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"name":"cached-marketplace","plugins":[{"name":"p1","description":"d","source":"s","version":"1.0.0"}]}"#;
    std::fs::write(plugin_dir.join("marketplace.json"), json).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let manifest = rt
        .block_on(fetch_github("test-repo", "some/repo", &cache_base, false))
        .unwrap();
    assert_eq!(manifest.name, "cached-marketplace");
    assert_eq!(manifest.plugins.len(), 1);
}

/// 首次 Git clone 应先完成临时 checkout，再原子提升，并能修复已有空缓存。
#[tokio::test]
async fn test_fetch_git_recovers_invalid_cache_with_atomic_promotion() {
    let Some((_repository, repository_path)) = create_git_marketplace_fixture() else {
        return;
    };
    let directory = tempdir().unwrap();
    let cache_base = directory.path().join("marketplaces");
    let cache_dir = marketplace_cache_dir(&cache_base, "local").unwrap();
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("partial.txt"), "partial").unwrap();

    let manifest = fetch_git(
        "local",
        repository_path.to_str().unwrap(),
        &cache_base,
        false,
    )
    .await
    .unwrap();

    assert_eq!(manifest.name, "local-marketplace");
    assert!(cache_dir.join(".git").is_dir());
    assert!(cache_dir.join("marketplace.json").is_file());
    assert!(!cache_dir.join("partial.txt").exists());
    assert_no_git_temporary_directories(&cache_base);
}

/// Git clone 失败时不得把正式缓存路径留成空目录或半成品。
#[tokio::test]
async fn test_fetch_git_failure_cleans_invalid_cache() {
    let directory = tempdir().unwrap();
    let cache_base = directory.path().join("marketplaces");
    let cache_dir = marketplace_cache_dir(&cache_base, "failed").unwrap();
    std::fs::create_dir_all(&cache_dir).unwrap();

    let result = fetch_git(
        "failed",
        directory
            .path()
            .join("missing-repository")
            .to_str()
            .unwrap(),
        &cache_base,
        false,
    )
    .await;

    assert!(result.is_err());
    assert!(!cache_dir.exists());
    assert_no_git_temporary_directories(&cache_base);
}

/// 同一 marketplace 的并发首次 clone 必须只有一个正式提升，其余调用复用完整缓存。
#[tokio::test]
async fn test_fetch_git_concurrent_initial_clones_share_promotion() {
    let Some((_repository, repository_path)) = create_git_marketplace_fixture() else {
        return;
    };
    let directory = tempdir().unwrap();
    let cache_base = directory.path().join("marketplaces");
    let repository_url = repository_path.to_string_lossy().into_owned();

    let results = futures::future::join_all(
        (0..4).map(|_| fetch_git("concurrent", repository_url.as_str(), &cache_base, false)),
    )
    .await;

    assert!(results.iter().all(Result::is_ok));
    let cache_dir = marketplace_cache_dir(&cache_base, "concurrent").unwrap();
    assert!(cache_dir.join(".git").is_dir());
    assert_no_git_temporary_directories(&cache_base);
}

/// 在 Unix 测试中构造一个会派生子 shell 并最终写入 marker 的命令。
#[cfg(unix)]
fn shell_quote_test_path(path: &Path) -> String {
    // 临时目录通常不含单引号；这里仍按 POSIX 规则转义，避免路径改变命令语义。
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// 构造等待子 shell 的脚本，用于超时和 future drop 的进程树断言。
#[cfg(unix)]
fn descendant_marker_script(started: &Path, finished: &Path, delay_secs: u64) -> String {
    format!(
        "touch -- {}; sh -c \"sleep {delay_secs}; touch -- {}\" & wait",
        shell_quote_test_path(started),
        shell_quote_test_path(finished)
    )
}

/// 构造根 shell 立即成功退出、子 shell 继续继承输出管道的脚本。
#[cfg(unix)]
fn inherited_pipe_marker_script(started: &Path, finished: &Path, delay_secs: u64) -> String {
    format!(
        "touch -- {}; sh -c \"sleep {delay_secs}; touch -- {}\" & exit 0",
        shell_quote_test_path(started),
        shell_quote_test_path(finished)
    )
}

/// Windows 测试使用 PowerShell 派生子进程，覆盖 Job Object 的整树语义。
#[cfg(windows)]
fn shell_quote_test_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// Windows 下创建会等待子 PowerShell 的进程树脚本。
#[cfg(windows)]
fn descendant_marker_script(started: &Path, finished: &Path, delay_secs: u64) -> String {
    let child_script = format!(
        "Start-Sleep -Seconds {delay_secs}; Set-Content -LiteralPath {} -Value done",
        shell_quote_test_path(finished)
    );
    format!(
        "Set-Content -LiteralPath {} -Value started; $child = Start-Process -FilePath powershell.exe -NoNewWindow -ArgumentList @('-NoProfile','-NonInteractive','-Command',{}) -PassThru; Wait-Process -Id $child.Id",
        shell_quote_test_path(started),
        powershell_quote(&child_script)
    )
}

/// Windows 下创建根 PowerShell 立即成功、子 PowerShell 继续持有输出管道的脚本。
#[cfg(windows)]
fn inherited_pipe_marker_script(started: &Path, finished: &Path, delay_secs: u64) -> String {
    let child_script = format!(
        "Start-Sleep -Seconds {delay_secs}; Set-Content -LiteralPath {} -Value done",
        shell_quote_test_path(finished)
    );
    format!(
        "Set-Content -LiteralPath {} -Value started; Start-Process -FilePath powershell.exe -NoNewWindow -ArgumentList @('-NoProfile','-NonInteractive','-Command',{}); exit 0",
        shell_quote_test_path(started),
        powershell_quote(&child_script)
    )
}

/// 将字符串作为 PowerShell 单引号字符串传递，避免路径或命令改变参数边界。
#[cfg(windows)]
fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// 用平台 shell 构造 marketplace 进程树回归测试命令。
#[cfg(unix)]
fn new_test_shell_command(script: &str) -> tokio::process::Command {
    let mut command = new_tokio_command("sh");
    command.args(["-c", script]);
    command
}

/// 用 PowerShell 构造 Windows Job Object 回归测试命令。
#[cfg(windows)]
fn new_test_shell_command(script: &str) -> tokio::process::Command {
    let mut command = new_tokio_command("powershell");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    command
}

/// 等待测试命令写入启动 marker，避免 abort 测试在 spawn 前误通过。
#[cfg(any(unix, windows))]
async fn wait_for_test_marker(path: &Path) {
    for _ in 0..200 {
        if path.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("测试命令未在限定时间内写入 marker: {}", path.display());
}

/// Git/npm 共用的外部命令 helper 超时后必须杀死整个 Unix 进程组。
#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_external_command_timeout_kills_process_tree() {
    let directory = tempdir().expect("创建进程树测试目录");
    let started = directory.path().join("started");
    let finished = directory.path().join("finished");
    // Windows PowerShell 冷启动可能超过 100ms；留出启动窗口后再由更长的
    // 子进程延迟触发超时，避免测试把“未启动”误判为整树清理成功。
    let delay_secs = if cfg!(windows) { 4 } else { 1 };
    let timeout = if cfg!(windows) {
        Duration::from_secs(2)
    } else {
        Duration::from_millis(100)
    };
    let script = descendant_marker_script(&started, &finished, delay_secs);
    let command = new_test_shell_command(&script);

    let result = run_external_command(command, timeout).await;
    assert!(matches!(result, Err(ExternalCommandError::Timeout)));
    wait_for_test_marker(&started).await;
    let settle = if cfg!(windows) {
        Duration::from_secs(delay_secs + 1)
    } else {
        Duration::from_millis(1_300)
    };
    tokio::time::sleep(settle).await;
    assert!(!finished.exists(), "超时后进程树仍存活并写入 marker");
}

/// 外部命令 helper 所在 future 被取消时，Drop 守卫也必须清理整个进程树。
#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_external_command_drop_kills_process_tree() {
    let directory = tempdir().expect("创建进程树测试目录");
    let started = directory.path().join("started");
    let finished = directory.path().join("finished");
    let script = descendant_marker_script(&started, &finished, 1);
    let command = new_test_shell_command(&script);

    let task = tokio::spawn(run_external_command(command, Duration::from_secs(10)));
    wait_for_test_marker(&started).await;
    task.abort();
    assert!(task
        .await
        .expect_err("取消命令 future 应返回 JoinError")
        .is_cancelled());
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert!(
        !finished.exists(),
        "future drop 后进程树仍存活并写入 marker"
    );
}

/// 根命令成功退出但后代继承输出管道时，成功路径不能无界等待 drain。
#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_external_command_success_does_not_hang_on_inherited_pipe() {
    let directory = tempdir().expect("创建输出管道测试目录");
    let started = directory.path().join("started");
    let finished = directory.path().join("finished");
    let script = inherited_pipe_marker_script(&started, &finished, 2);
    let command = new_test_shell_command(&script);

    let begin = std::time::Instant::now();
    let output = run_external_command(command, Duration::from_secs(5))
        .await
        .expect("根命令应成功退出");
    assert!(output.status.success());
    assert!(
        begin.elapsed() < Duration::from_secs(3),
        "继承管道不应让成功路径等待后代自然退出"
    );
    wait_for_test_marker(&started).await;
    tokio::time::sleep(Duration::from_millis(2_300)).await;
    assert!(!finished.exists(), "成功路径应清理持有输出管道的后代");
}

#[test]
fn test_read_file_success() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("marketplace.json");
    let json = r#"{"name":"file-test","plugins":[]}"#;
    std::fs::write(&path, json).unwrap();
    let manifest = read_file(&path).unwrap();
    assert_eq!(manifest.name, "file-test");
}

#[test]
fn test_read_file_not_found() {
    let result = read_file(Path::new("/nonexistent/file.json"));
    assert!(result.is_err());
}

#[test]
fn test_read_directory_root() {
    let dir = tempdir().unwrap();
    let json = r#"{"name":"dir-test","plugins":[]}"#;
    std::fs::write(dir.path().join("marketplace.json"), json).unwrap();
    let manifest = read_directory(dir.path()).unwrap();
    assert_eq!(manifest.name, "dir-test");
}

#[test]
fn test_read_directory_subdir() {
    let dir = tempdir().unwrap();
    let subdir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&subdir).unwrap();
    let json = r#"{"name":"subdir-test","plugins":[]}"#;
    std::fs::write(subdir.join("marketplace.json"), json).unwrap();
    let manifest = read_directory(dir.path()).unwrap();
    assert_eq!(manifest.name, "subdir-test");
}

#[test]
fn test_read_directory_not_found() {
    let dir = tempdir().unwrap();
    let result = read_directory(dir.path());
    assert!(result.is_err());
    match result.unwrap_err() {
        MarketplaceError::ManifestNotFound { .. } => {}
        _ => panic!("expected ManifestNotFound"),
    }
}

#[tokio::test]
#[ignore] // 需要 network，CI 环境手动启用
async fn test_fetch_url_cache_fallback() {
    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    let json = r#"{"name":"cached-url","plugins":[]}"#;
    let cache_file = marketplace_cache_file(&cache_base, "test").unwrap();
    std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
    std::fs::write(cache_file, json).unwrap();
    let manifest = fetch_url("test", "http://127.0.0.1:1/nonexistent.json", &cache_base)
        .await
        .unwrap();
    assert_eq!(manifest.name, "cached-url");
}

#[tokio::test]
#[ignore] // 需要 network，CI 环境手动启用
async fn test_fetch_url_no_cache_no_server() {
    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    std::fs::create_dir_all(&cache_base).unwrap();
    let result = fetch_url("test", "http://127.0.0.1:1/nonexistent.json", &cache_base).await;
    assert!(result.is_err());
}

/// URL marketplace 的 200 响应必须原子覆盖缓存，并且不留下临时文件。
#[tokio::test]
async fn test_fetch_url_writes_cache_atomically() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    let cache_file = marketplace_cache_file(&cache_base, "test").unwrap();
    std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
    std::fs::write(&cache_file, r#"{"name":"old-url","plugins":[]}"#).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let body = r#"{"name":"fresh-url","plugins":[]}"#.to_string();
    let response_body = body.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let result = fetch_url(
        "test",
        &format!("http://{address}/marketplace.json"),
        &cache_base,
    )
    .await;
    server.join().unwrap();
    let manifest = result.unwrap();

    assert_eq!(manifest.name, "fresh-url");
    assert_eq!(std::fs::read_to_string(cache_file).unwrap(), body);
    assert_eq!(
        std::fs::read_dir(cache_base.join("url")).unwrap().count(),
        1
    );
}

/// URL marketplace 缓存替换失败时必须保留 MarketplaceError::Io 语义和旧目标。
#[tokio::test]
async fn test_fetch_url_cache_replace_error_is_io_error() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    let cache_file = marketplace_cache_file(&cache_base, "test").unwrap();
    std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
    std::fs::create_dir(&cache_file).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let body = r#"{"name":"fresh-url","plugins":[]}"#;
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let result = fetch_url(
        "test",
        &format!("http://{address}/marketplace.json"),
        &cache_base,
    )
    .await;
    server.join().unwrap();

    assert!(matches!(result, Err(MarketplaceError::Io(_))));
    assert!(cache_file.is_dir());
    assert_eq!(
        std::fs::read_dir(cache_base.join("url")).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn test_manager_auto_register_official() {
    let dir = tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(16);
    let mut manager = MarketplaceManager::new(Some(dir.path().to_path_buf()));
    let handles = manager.init(tx).await;

    // Check that official marketplace was registered
    let km_path = dir.path().join("known_marketplaces.json");
    assert!(km_path.exists());
    let known = crate::plugin::config::load_known_marketplaces(Some(&km_path)).unwrap();
    assert!(known.iter().any(|km| match &km.source {
        MarketplaceSource::GitHub { repo } => repo == "anthropics/claude-plugins-official",
        _ => false,
    }));

    for h in handles {
        h.abort();
    }
}

/// Manager 合并已知来源时，官方 marketplace 的大小写变体不得生成重复条目。
#[tokio::test]
async fn test_manager_official_detection_is_case_insensitive() {
    let dir = tempdir().unwrap();
    let known_path = dir.path().join("known_marketplaces.json");
    let known = r#"{"official": {"source":{"source":"github","repo":"Anthropics/Claude-Plugins-Official"},"installLocation":"","lastUpdated":""}}"#;
    std::fs::write(&known_path, known).unwrap();

    let settings_path = dir.path().join("settings.json");
    let settings = r#"{
            "extraKnownMarketplaces": [
                {"source": {"source":"github","repo":"anthropics/claude-plugins-official"}}
            ]
        }"#;
    std::fs::write(&settings_path, settings).unwrap();

    let (tx, _rx) = mpsc::channel(16);
    let mut manager = MarketplaceManager::new(Some(dir.path().to_path_buf()));
    let handles = manager.init(tx).await;

    assert_eq!(
        manager
            .entries()
            .iter()
            .filter(|entry| marketplace_names_equal(&entry.name, "claude-plugins-official"))
            .count(),
        1
    );

    for h in handles {
        h.abort();
    }
}

#[tokio::test]
async fn test_manager_merge_extra_known_marketplaces() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let settings = r#"{
            "extraKnownMarketplaces": [
                {"source": {"source":"file","path":"/test/marketplace.json"}}
            ]
        }"#;
    std::fs::write(&settings_path, settings).unwrap();

    let (tx, _rx) = mpsc::channel(16);
    let mut manager = MarketplaceManager::new(Some(dir.path().to_path_buf()));
    let handles = manager.init(tx).await;

    assert!(manager.entries().iter().any(|e| match &e.source {
        MarketplaceSource::File { path } => path == "/test/marketplace.json",
        _ => false,
    }));

    for h in handles {
        h.abort();
    }
}

#[tokio::test]
async fn test_manager_cache_loading() {
    let dir = tempdir().unwrap();
    let marketplaces_dir = dir.path().join("marketplaces");
    let json = r#"{"name":"cached-test","plugins":[{"name":"p1","description":"Plugin 1","source":"s","version":"1.0.0"}]}"#;
    let cache_file = marketplace_cache_file(&marketplaces_dir, "test").unwrap();
    std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
    std::fs::write(cache_file, json).unwrap();

    let km_path = dir.path().join("known_marketplaces.json");
    // 使用对象格式，包含必需的 installLocation 和 lastUpdated 字段
    let known = r#"{"test": {"source":{"source":"url","url":"https://example.com/test.json"},"installLocation":"","lastUpdated":"2025-01-01T00:00:00Z"}}"#;
    std::fs::write(&km_path, known).unwrap();

    let (tx, _rx) = mpsc::channel(16);
    let mut manager = MarketplaceManager::new(Some(dir.path().to_path_buf()));
    let handles = manager.init(tx).await;

    let cached_entry = manager.entries().iter().find(|e| e.name == "test");
    assert!(cached_entry.is_some());
    let entry = cached_entry.unwrap();
    assert_eq!(entry.status, MarketplaceStatus::Cached);
    assert!(entry.manifest.is_some());

    for h in handles {
        h.abort();
    }
}

#[test]
fn test_manager_find_plugin() {
    let mut manager = MarketplaceManager::new(None);
    let manifest = MarketplaceManifest {
        name: "test-mkt".into(),
        plugins: vec![MarketplacePlugin {
            name: "target-plugin".into(),
            description: "desc".into(),
            source: serde_json::json!("src"),
            version: "1.0.0".into(),
            sha: None,
            author: None,
            category: None,
            homepage: None,
            tags: None,
            extra: serde_json::Value::Object(Default::default()),
        }],
        allow_cross_marketplace: None,
    };
    manager.entries.push(MarketplaceEntry {
        name: "test-mkt".into(),
        source: MarketplaceSource::Directory {
            path: "/tmp/test".into(),
        },
        manifest: Some(manifest),
        status: MarketplaceStatus::Cached,
        last_updated: None,
        auto_update: false,
    });
    let result = manager.find_plugin("TARGET-PLUGIN");
    assert!(result.is_some());
    assert_eq!(result.unwrap().0.name, "target-plugin");
}

#[test]
fn test_manager_find_plugin_not_found() {
    let mut manager = MarketplaceManager::new(None);
    let manifest = MarketplaceManifest {
        name: "test-mkt".into(),
        plugins: vec![],
        allow_cross_marketplace: None,
    };
    manager.entries.push(MarketplaceEntry {
        name: "test-mkt".into(),
        source: MarketplaceSource::Directory {
            path: "/tmp/test".into(),
        },
        manifest: Some(manifest),
        status: MarketplaceStatus::Cached,
        last_updated: None,
        auto_update: false,
    });
    assert!(manager.find_plugin("nonexistent").is_none());
}

#[test]
fn test_manager_available_plugins() {
    let mut manager = MarketplaceManager::new(None);
    let manifest1 = MarketplaceManifest {
        name: "mkt1".into(),
        plugins: vec![
            MarketplacePlugin {
                name: "p1".into(),
                description: "d1".into(),
                source: serde_json::json!("s1"),
                version: "1.0.0".into(),
                sha: None,
                author: None,
                category: None,
                homepage: None,
                tags: None,
                extra: serde_json::Value::Object(Default::default()),
            },
            MarketplacePlugin {
                name: "p2".into(),
                description: "d2".into(),
                source: serde_json::json!("s2"),
                version: "2.0.0".into(),
                sha: None,
                author: None,
                category: None,
                homepage: None,
                tags: None,
                extra: serde_json::Value::Object(Default::default()),
            },
        ],
        allow_cross_marketplace: None,
    };
    manager.entries.push(MarketplaceEntry {
        name: "mkt1".into(),
        source: MarketplaceSource::Directory { path: "/t".into() },
        manifest: Some(manifest1),
        status: MarketplaceStatus::Fresh,
        last_updated: None,
        auto_update: false,
    });
    // NotFetched entry should be skipped
    manager.entries.push(MarketplaceEntry {
        name: "mkt2".into(),
        source: MarketplaceSource::Directory { path: "/t2".into() },
        manifest: None,
        status: MarketplaceStatus::NotFetched,
        last_updated: None,
        auto_update: false,
    });

    let available = manager.available_plugins();
    assert_eq!(available.len(), 2);
    assert_eq!(available[0].name, "p1");
    assert_eq!(available[1].name, "p2");
}

#[test]
fn test_manager_update_entry() {
    let mut manager = MarketplaceManager::new(None);
    manager.entries.push(MarketplaceEntry {
        name: "test".into(),
        source: MarketplaceSource::Directory { path: "/t".into() },
        manifest: None,
        status: MarketplaceStatus::NotFetched,
        last_updated: None,
        auto_update: false,
    });
    let manifest = MarketplaceManifest {
        name: "updated".into(),
        plugins: vec![],
        allow_cross_marketplace: None,
    };
    manager.update_entry(0, manifest, MarketplaceStatus::Fresh);
    assert_eq!(manager.entries[0].status, MarketplaceStatus::Fresh);
    assert!(manager.entries[0].manifest.is_some());
    assert!(manager.entries[0].last_updated.is_some());
}

/// scoped NPM marketplace 必须映射为稳定、无斜杠且可用于 PluginId 的命名空间。
#[test]
fn test_manager_scoped_npm_namespace_is_stable_and_collision_free() {
    for (package, expected) in [
        ("@scope/my-plugin", "npm-4073636f70652f6d792d706c7567696e"),
        ("@scope/my_plugin", "npm-4073636f70652f6d795f706c7567696e"),
        ("plain-plugin", "npm-706c61696e2d706c7567696e"),
    ] {
        let source = MarketplaceSource::Npm {
            package: package.to_owned(),
        };
        let name = MarketplaceManager::extract_name(&source);
        assert_eq!(name, expected, "package={package}");
        assert!(!name.contains('/'), "命名空间不能包含斜杠：{name}");
        assert!(
            PluginId::from_components("plugin", Some(&name)).is_ok(),
            "命名空间必须可作为 PluginId marketplace：{name}"
        );
    }
}

/// marketplace 展示名称保留来源大小写，但比较、缓存和 PluginId identity 忽略 ASCII 大小写。
#[test]
fn test_marketplace_identity_is_ascii_case_insensitive() {
    let upper = MarketplaceSource::GitHub {
        repo: "owner/Official".into(),
    };
    let lower = MarketplaceSource::GitHub {
        repo: "owner/official".into(),
    };
    let upper_name = MarketplaceManager::extract_name(&upper);
    let lower_name = MarketplaceManager::extract_name(&lower);

    assert_eq!(upper_name, "Official");
    assert_eq!(lower_name, "official");
    assert_ne!(upper_name, lower_name);
    assert!(marketplace_names_equal(&upper_name, "OFFICIAL"));
    assert_eq!(
        marketplace_cache_key(&upper_name).unwrap(),
        marketplace_cache_key(&lower_name).unwrap()
    );
    let upper_id = PluginId::from_components("plugin", Some(&upper_name)).unwrap();
    let lower_id = PluginId::from_components("plugin", Some(&lower_name)).unwrap();
    assert_eq!(upper_id, lower_id);
}

/// 大小写折叠必须先于路径/NPM 编码，且两种编码域不能互相覆盖。
#[test]
fn test_marketplace_cache_identity_is_case_stable_and_domain_separated() {
    assert_eq!(
        marketplace_cache_key("Safe/Path").unwrap(),
        marketplace_cache_key("safe/path").unwrap()
    );

    let path_key = marketplace_cache_key("safe/path").unwrap();
    let path_encoded_name = path_key.to_ascii_uppercase();
    assert_ne!(
        path_key,
        marketplace_cache_key(&path_encoded_name).unwrap(),
        "路径编码结果作为普通名称时必须进入独立 raw 域"
    );

    let npm_namespace = npm_marketplace_namespace("plain-plugin");
    assert_eq!(
        marketplace_cache_dir_for_namespace(Path::new("cache"), &npm_namespace).unwrap(),
        marketplace_cache_dir_for_namespace(
            Path::new("cache"),
            &npm_namespace.to_ascii_uppercase()
        )
        .unwrap()
    );
    let invalid_npm_namespace = npm_marketplace_namespace("invalid package");
    assert!(invalid_npm_namespace.starts_with("npm-sha256-invalid-"));
    assert_eq!(
        marketplace_cache_dir_for_namespace(Path::new("cache"), &invalid_npm_namespace).unwrap(),
        marketplace_cache_dir_for_namespace(
            Path::new("cache"),
            &invalid_npm_namespace.to_ascii_uppercase()
        )
        .unwrap()
    );
    let npm_ordinary_key = marketplace_cache_key(&npm_namespace).unwrap();
    let npm_cache_key = marketplace_cache_dir_for_namespace(Path::new("cache"), &npm_namespace)
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_ne!(
        npm_ordinary_key, npm_cache_key,
        "NPM namespace 作为普通 marketplace 名称时必须进入独立 raw 域"
    );
}

/// marketplace 缓存键必须拒绝路径段、Windows 保留设备名和尾随点。
#[test]
fn test_marketplace_cache_key_rejects_windows_unsafe_components() {
    for (name, expected) in [
        ("official", Some("official")),
        ("safe/path", Some("marketplace-736166652f70617468")),
        (".", None),
        ("..", None),
        ("official.", None),
        ("CON", None),
        ("con.txt", None),
        ("safe/CON", None),
    ] {
        assert_eq!(
            super::marketplace_cache_key(name).ok().as_deref(),
            expected,
            "marketplace={name}"
        );
    }
}

// ─── parse_marketplace_input tests ───────────────────────────────────

#[test]
fn test_parse_input_empty() {
    assert!(parse_marketplace_input("").is_err());
    assert!(parse_marketplace_input("  ").is_err());
}

#[test]
fn test_parse_input_github_shorthand() {
    let result = parse_marketplace_input("owner/repo").unwrap();
    assert!(matches!(result, MarketplaceSource::GitHub { ref repo } if repo == "owner/repo"));
}

#[test]
fn test_parse_input_github_url() {
    let result = parse_marketplace_input("https://github.com/owner/repo").unwrap();
    assert!(matches!(result, MarketplaceSource::GitHub { ref repo } if repo == "owner/repo"));

    let result2 = parse_marketplace_input("https://github.com/owner/repo.git").unwrap();
    assert!(matches!(result2, MarketplaceSource::GitHub { ref repo } if repo == "owner/repo"));
}

#[test]
fn test_parse_input_ssh_url() {
    let result = parse_marketplace_input("git@github.com:owner/repo.git").unwrap();
    assert!(matches!(result, MarketplaceSource::GitHub { .. }));
}

#[test]
fn test_parse_input_http_url() {
    let result = parse_marketplace_input("https://example.com/marketplace.json").unwrap();
    assert!(
        matches!(result, MarketplaceSource::Url { ref url } if url == "https://example.com/marketplace.json")
    );
}

#[test]
fn test_parse_input_local_directory() {
    let result = parse_marketplace_input("./path/to/marketplace").unwrap();
    assert!(matches!(result, MarketplaceSource::Directory { .. }));
}

#[test]
fn test_parse_input_local_file() {
    let result = parse_marketplace_input("./path/to/marketplace.json").unwrap();
    assert!(matches!(result, MarketplaceSource::File { .. }));
}

/// 普通来源占用 NPM namespace 的同形名称时，必须落在独立缓存域，且仍能作为 PluginId。
#[test]
fn test_marketplace_and_npm_cache_namespaces_are_disjoint() {
    let cache_base = tempdir().unwrap();
    let npm_namespace = npm_marketplace_namespace("abc");
    let git_name = MarketplaceManager::extract_name(&MarketplaceSource::GitHub {
        repo: format!("owner/{npm_namespace}"),
    });

    assert_ne!(git_name, npm_namespace);
    assert!(PluginId::from_components("plugin", Some(&git_name)).is_ok());

    let git_dir = marketplace_cache_dir_for_namespace(cache_base.path(), &git_name).unwrap();
    let npm_dir = npm_cache_dir(cache_base.path(), "abc").unwrap();
    assert_ne!(git_dir, npm_dir);

    // 长 NPM namespace 使用 SHA-256 形式时，也不能与普通来源同形名称冲突。
    let long_package = "a".repeat(100);
    let long_namespace = npm_marketplace_namespace(&long_package);
    assert!(long_namespace.starts_with("npm-sha256-"));
    let ordinary_key = marketplace_cache_key(&long_namespace).unwrap();
    assert_ne!(ordinary_key, long_namespace);
    assert_ne!(
        marketplace_cache_dir(cache_base.path(), &long_namespace).unwrap(),
        npm_cache_dir(cache_base.path(), &long_package).unwrap()
    );
}

/// NPM namespace 的长名和非法输入回退都必须经过 PluginId 校验，不能返回超长值。
#[test]
fn test_npm_namespace_fallback_always_respects_plugin_id_contract() {
    let packages = [
        "a".repeat(100),
        "a".repeat(MAX_NPM_PACKAGE_BYTES),
        "invalid package".to_owned(),
    ];

    for package in packages {
        let namespace = npm_marketplace_namespace(&package);
        assert!(
            PluginId::from_components("npm", Some(&namespace)).is_ok(),
            "NPM namespace 必须始终满足 PluginId 契约：package={package}, namespace={namespace}"
        );
    }
}

/// 直接调用 fetch_npm 时，非法包名必须在创建缓存目录或 npm 临时目录前失败。
#[tokio::test]
async fn test_fetch_npm_rejects_unsafe_package_before_creating_cache_or_temp_dir() {
    let directory = tempdir().expect("创建 NPM 非法包名测试目录");
    let cache_base = directory.path().join("marketplaces");
    let invalid_packages = [
        ("父级路径", "../escape"),
        ("反斜杠路径", r"..\escape"),
        ("Unix 绝对路径", "/tmp/escape"),
        ("Windows 绝对路径", r"C:\escape"),
        ("控制字符", "bad\nname"),
        ("选项前缀", "--help"),
    ];

    for (label, package) in invalid_packages {
        let result = fetch_npm(package, &cache_base).await;
        let Err(MarketplaceError::NpmFailed(error)) = result else {
            panic!("{label}必须在 NPM 执行前返回 NpmFailed: {result:?}");
        };
        assert!(
            error.contains("NPM 包名无效"),
            "{label}应返回统一的包名校验错误: {error}"
        );
    }

    // 校验失败发生在 create_dir_all(cache_base) 之前，缓存根和其临时目录都不应出现。
    assert!(
        !cache_base.exists(),
        "非法包名不应创建缓存根: {}",
        cache_base.display()
    );
    let leaked_temp_dir = std::fs::read_dir(directory.path())
        .expect("读取 NPM 非法包名测试目录")
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("peri-npm-pack-")
        });
    assert!(
        !leaked_temp_dir,
        "非法包名不应创建 peri-npm-pack-* 临时目录"
    );
}

/// Git 目录与 URL 文件必须使用不同的缓存域，不能因 `.json` 后缀形成同路径。
#[test]
fn test_git_directory_and_url_file_cache_domains_are_disjoint() {
    let cache_base = tempdir().unwrap();
    let git_dir = marketplace_cache_dir(cache_base.path(), "foo.json").unwrap();
    let url_file = marketplace_cache_file(cache_base.path(), "foo").unwrap();

    assert_ne!(git_dir, url_file);
    assert_ne!(git_dir.parent(), url_file.parent());
}

/// 由外部来源提取的 marketplace namespace 必须遵循 PluginId 的统一长度契约。
#[test]
fn test_non_npm_marketplace_namespace_uses_plugin_id_length_contract() {
    let long_name = "a".repeat(255);
    let source = MarketplaceSource::Git {
        url: format!("https://example.com/{long_name}.git"),
    };
    let name = MarketplaceManager::extract_name(&source);

    assert!(name.len() < long_name.len());
    assert!(PluginId::from_components("plugin", Some(&name)).is_ok());
}

/// URL marketplace 名称达到 255 字节时，.json 扩展名仍必须计入总组件上限。
#[test]
fn test_marketplace_cache_file_component_includes_extension_limit() {
    let cache_base = tempdir().unwrap();
    let name = "a".repeat(250);
    let path = marketplace_cache_file(cache_base.path(), &name).unwrap();
    let component = path.file_name().unwrap().to_str().unwrap();

    assert!(component.ends_with(".json"));
    assert_eq!(component.len(), 255);

    // 255 字节的 URL 名称本身仍合法，但扩展名会触发受控短键，不能返回 260 字节组件。
    let maximum_name = "a".repeat(255);
    let maximum_path = marketplace_cache_file(cache_base.path(), &maximum_name).unwrap();
    let maximum_component = maximum_path.file_name().unwrap().to_str().unwrap();
    assert!(maximum_component.ends_with(".json"));
    assert!(maximum_component.len() <= 255);
    assert!(marketplace_cache_file(cache_base.path(), &format!("{maximum_name}b")).is_err());
}

#[test]
fn test_parse_input_npm_scoped() {
    let result = parse_marketplace_input("@scope/my-plugin").unwrap();
    assert!(
        matches!(result, MarketplaceSource::Npm { ref package } if package == "@scope/my-plugin")
    );
}

#[test]
fn test_parse_input_npm_unscoped() {
    let result = parse_marketplace_input("my-plugin").unwrap();
    assert!(matches!(result, MarketplaceSource::Npm { ref package } if package == "my-plugin"));
}

#[test]
fn test_parse_input_absolute_path() {
    let result = parse_marketplace_input("/absolute/path/to/dir").unwrap();
    assert!(matches!(result, MarketplaceSource::Directory { .. }));
}
