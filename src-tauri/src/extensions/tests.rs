//! 扩展、插件市场、Agent、Skill 与 MCP 的完整回归测试。

use super::*;

/// 有 managed state 的正常生产路径也必须把插件 Hooks 放入会话快照。
#[test]
fn attaches_plugin_hooks_to_runtime_snapshot() {
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![crate::claude_plugins::RuntimePlugin {
            id: PluginId::from_components("demo", Some("local"))
                .expect("测试插件 ID 应通过统一校验入口"),
            root: PathBuf::from("/plugins/demo"),
            commands: Vec::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            hooks: Some(serde_json::json!({
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "echo checked"}]
                }]
            })),
            unsupported_hooks: Vec::new(),
            mcp_servers: BTreeMap::new(),
            lsp_servers: Vec::new(),
        }],
        plugin_hooks: Vec::new(),
    };

    let snapshot = attach_claude_hooks(snapshot);

    assert_eq!(snapshot.plugin_hooks.len(), 1);
    assert_eq!(snapshot.plugin_hooks[0].plugin_id, "demo@local");
    assert_eq!(snapshot.plugin_hooks[0].matcher.as_deref(), Some("Bash"));
}

/// Claude 命令命名空间必须保留嵌套目录，但不能把 `commands` 根目录当作名称。
#[test]
fn plugin_command_namespace_uses_command_relative_path() {
    assert_eq!(
        plugin_command_namespace("plugin:demo", Path::new("commands/foo.md")),
        "plugin:demo:foo"
    );
    assert_eq!(
        plugin_command_namespace("plugin:demo", Path::new("commands/admin/check.md")),
        "plugin:demo:admin:check"
    );
}

/// 自定义嵌套 marketplace.json 必须按记录保存的 manifestPath 重载，而不是回退到根目录默认路径。
#[test]
fn loads_nested_claude_marketplace_manifest_from_record() {
    let root = test_directory("nested-claude-marketplace");
    let manifest_path = root.join("catalog/.claude-plugin/marketplace.json");
    fs::create_dir_all(manifest_path.parent().expect("清单应有父目录"))
        .expect("应创建嵌套清单目录");
    fs::write(
        &manifest_path,
        br#"{"name":"nested","plugins":[{"name":"demo","source":"./plugin"}]}"#,
    )
    .expect("应写入嵌套 marketplace.json");
    let record = MarketplaceRecord {
        name: "nested".to_owned(),
        path: root.join("catalog").display().to_string(),
        manifest_path: manifest_path.display().to_string(),
    };
    let manifest = load_claude_marketplace_manifest_from_record(&record)
        .expect("应按 manifestPath 读取嵌套清单");
    assert_eq!(manifest.name, "nested");
    fs::remove_dir_all(root).expect("应清理嵌套市场测试目录");
}

/// 首次启动应发现 Claude Code 已下载的官方市场，而不是返回空市场列表。
#[test]
fn discovers_claude_known_marketplaces_from_install_location() {
    let root = test_directory("known-marketplaces");
    let marketplace_root = root.join("marketplaces/official");
    let manifest_path = marketplace_root.join(".claude-plugin/marketplace.json");
    fs::create_dir_all(manifest_path.parent().expect("清单应有父目录"))
        .expect("应创建 Claude 市场目录");
    fs::write(
        &manifest_path,
        br#"{"name":"claude-plugins-official","plugins":[]}"#,
    )
    .expect("应写入 Claude 市场清单");
    let known_path = root.join("known_marketplaces.json");
    fs::write(
        &known_path,
        serde_json::to_vec(&serde_json::json!({
            "claude-plugins-official": {
                "source": {"source": "github", "repo": "anthropics/claude-plugins-official"},
                "installLocation": marketplace_root,
                "lastUpdated": "2026-08-03T00:00:00Z"
            }
        }))
        .expect("应序列化 Claude 已知市场"),
    )
    .expect("应写入 Claude 已知市场登记");

    let discovered = discover_claude_known_marketplaces_from_path(&known_path);

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].name, "claude-plugins-official");
    assert_eq!(
        discovered[0].manifest_path,
        manifest_path.display().to_string()
    );
    fs::remove_dir_all(root).expect("应清理 Claude 已知市场测试目录");
}

/// Claude Code 已知市场的官方保留名称必须仍与 Anthropic 来源绑定。
#[test]
fn rejects_discovered_official_marketplace_from_non_anthropic_source() {
    let root = test_directory("known-marketplaces-spoof");
    let marketplace_root = root.join("marketplaces/spoof");
    let manifest_path = marketplace_root.join(".claude-plugin/marketplace.json");
    fs::create_dir_all(manifest_path.parent().expect("清单应有父目录"))
        .expect("应创建伪造市场目录");
    fs::write(
        &manifest_path,
        br#"{"name":"claude-plugins-official","plugins":[]}"#,
    )
    .expect("应写入伪造市场清单");
    let known_path = root.join("known_marketplaces.json");
    fs::write(
        &known_path,
        serde_json::to_vec(&serde_json::json!({
            "spoof": {
                "source": {"source": "github", "repo": "attacker/claude-plugins-official"},
                "installLocation": marketplace_root,
                "lastUpdated": "2026-08-03T00:00:00Z"
            }
        }))
        .expect("应序列化伪造 Claude 已知市场"),
    )
    .expect("应写入伪造 Claude 已知市场登记");

    let discovered = discover_claude_known_marketplaces_from_path(&known_path);

    assert!(
        discovered.is_empty(),
        "非 Anthropic 来源不得占用官方市场命名空间"
    );
    fs::remove_dir_all(root).expect("应清理伪造 Claude 已知市场测试目录");
}

/// 新用户默认来源必须指向 Anthropic 管理的 Claude Code 官方插件仓库。
#[test]
fn default_claude_marketplace_source_points_to_official_repository() {
    assert_eq!(
        DEFAULT_CLAUDE_MARKETPLACE_SOURCE,
        "github:anthropics/claude-plugins-official"
    );
    assert_eq!(DEFAULT_CLAUDE_MARKETPLACE_NAME, "claude-plugins-official");
    assert!(
        crate::claude_plugins::validate_marketplace_name_source(
            DEFAULT_CLAUDE_MARKETPLACE_NAME,
            DEFAULT_CLAUDE_MARKETPLACE_SOURCE,
        )
        .is_ok()
    );
}

/// 默认市场后台取得必须去重，并在失败后按退避时间允许下一次自动重试。
#[test]
fn marketplace_bootstrap_deduplicates_and_backs_off_failures() {
    let now = Instant::now();
    let mut state = MarketplaceBootstrapState::default();
    assert!(state.should_start(false, now));
    let generation = state.begin();
    assert!(state.is_current(generation));
    assert!(!state.should_start(false, now));
    state.fail("network unavailable".to_owned(), now);
    assert!(!state.should_start(false, now + Duration::from_secs(1)));
    assert!(state.should_start(false, now + MARKETPLACE_RETRY_BACKOFF));
    assert!(state.should_start(true, now + Duration::from_secs(1)));

    state.succeed();
    assert!(!state.should_start(false, now));
    assert!(state.should_start(true, now));

    let generation = state.begin();
    state.invalidate();
    assert!(!state.is_current(generation));
}

/// 顶部“刷新目录”必须能在默认源尚未登记时绕过失败退避；普通自定义源刷新不能抢回默认源。
#[test]
fn explicit_catalog_refresh_can_restore_the_missing_default_marketplace() {
    assert!(should_refresh_default_marketplace(None, false, true));
    assert!(!should_refresh_default_marketplace(None, false, false));
    assert!(should_refresh_default_marketplace(None, true, false));
    assert!(should_refresh_default_marketplace(
        Some("CLAUDE-PLUGINS-OFFICIAL"),
        false,
        false,
    ));
    assert!(!should_refresh_default_marketplace(
        Some("custom-market"),
        false,
        false,
    ));
}

/// 默认官方市场只有在清单含插件时才算已取得，避免合法但空的缓存永久阻止重试。
#[test]
fn empty_default_marketplace_manifest_is_not_materialized() {
    let root = test_directory("empty-default-marketplace");
    let manifest_path = root.join(".claude-plugin/marketplace.json");
    fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    fs::write(
        &manifest_path,
        br#"{"name":"claude-plugins-official","plugins":[]}"#,
    )
    .unwrap();
    let record = MarketplaceRecord {
        name: DEFAULT_CLAUDE_MARKETPLACE_NAME.to_owned(),
        path: root.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
    };
    assert!(!marketplace_record_is_materialized(&record));

    fs::write(
        &manifest_path,
        br#"{"name":"claude-plugins-official","plugins":[{"name":"demo","source":"./demo"}]}"#,
    )
    .unwrap();
    assert!(marketplace_record_is_materialized(&record));
    fs::remove_dir_all(root).unwrap();
}

/// 官方市场的隐藏清单必须转换成仓库根锚定 sparse pattern，避免被 Git 模糊匹配漏掉。
#[test]
fn sparse_checkout_patterns_anchor_marketplace_manifest() {
    assert_eq!(
        sparse_checkout_pattern(".claude-plugin/marketplace.json"),
        "/.claude-plugin/marketplace.json"
    );
    assert_eq!(sparse_checkout_pattern("./plugins"), "/plugins");
}

/// 自定义 Git 市场未声明 sparsePaths 时必须保留清单引用的相对插件目录。
#[test]
fn git_marketplace_without_sparse_paths_checks_out_relative_plugins() {
    let directory = tempfile::tempdir().expect("创建 Git 市场测试目录");
    let repository = directory.path().join("repository");
    let plugin = repository.join("plugins/demo/.claude-plugin");
    fs::create_dir_all(&plugin).expect("创建测试插件目录");
    fs::create_dir_all(repository.join(".claude-plugin")).expect("创建测试市场目录");
    fs::write(
        repository.join(".claude-plugin/marketplace.json"),
        br#"{"name":"custom","plugins":[{"name":"demo","source":"./plugins/demo"}]}"#,
    )
    .expect("写入测试市场清单");
    fs::write(plugin.join("plugin.json"), br#"{"name":"demo"}"#).expect("写入测试插件清单");

    let mut init = process::Command::new("git");
    init.current_dir(&repository).args(["init", "--quiet"]);
    run_external(init, "初始化 Git 市场测试仓库").expect("初始化测试仓库");
    let mut add = process::Command::new("git");
    add.current_dir(&repository).args(["add", "."]);
    run_external(add, "暂存 Git 市场测试仓库").expect("暂存测试仓库");
    let mut commit = process::Command::new("git");
    commit.current_dir(&repository).args([
        "-c",
        "user.name=KeenCode Test",
        "-c",
        "user.email=keencode-test@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    run_external(commit, "提交 Git 市场测试仓库").expect("提交测试仓库");

    let workspace = directory.path().join("workspace");
    let materialized = materialize_claude_marketplace_spec(
        MarketplaceSourceSpec::Git {
            url: repository.display().to_string(),
            reference: None,
            path: None,
            sparse_paths: Vec::new(),
        },
        &workspace,
    )
    .expect("取得自定义 Git 市场");
    assert!(
        materialized
            .root
            .join("plugins/demo/.claude-plugin/plugin.json")
            .is_file(),
        "未配置 sparsePaths 时应检出相对插件目录"
    );
    assert_eq!(materialized.catalog.name, "custom");
}

/// Git 插件子目录不能是指向克隆根外的符号链接；两条 Git 物化路径都必须拒绝。
#[cfg(unix)]
#[test]
fn rejects_git_plugin_subdir_symlink_escape() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("创建 Git 插件测试目录");
    let repository = directory.path().join("repository");
    let outside = directory.path().join("outside");
    fs::create_dir_all(&outside).expect("创建克隆根外目录");
    fs::create_dir_all(repository.join("plugins")).expect("创建 Git 插件目录");
    symlink(&outside, repository.join("plugins/demo")).expect("创建 Git 插件符号链接");

    let mut init = process::Command::new("git");
    init.current_dir(&repository).args(["init", "--quiet"]);
    run_external(init, "初始化 Git 插件测试仓库").expect("初始化测试仓库");
    let mut add = process::Command::new("git");
    add.current_dir(&repository).args(["add", "."]);
    run_external(add, "暂存 Git 插件测试仓库").expect("暂存测试仓库");
    let mut commit = process::Command::new("git");
    commit.current_dir(&repository).args([
        "-c",
        "user.name=KeenCode Test",
        "-c",
        "user.email=keencode-test@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    run_external(commit, "提交 Git 插件测试仓库").expect("提交测试仓库");

    let git_url = repository.display().to_string();
    let marketplace_error = materialize_marketplace_plugin_source(
        MarketplacePluginSourceSpec::Git {
            url: git_url.clone(),
            path: Some("plugins/demo".to_owned()),
            reference: None,
            sha: None,
            sparse_paths: Vec::new(),
        },
        directory.path(),
        &directory.path().join("marketplace-workspace"),
    )
    .expect_err("marketplace Git 插件不能跟随越界符号链接");
    assert!(marketplace_error.contains("符号链接"));

    let claude_error = materialize_claude_plugin_source(
        &PluginSource::GitSubdir {
            url: git_url,
            path: "plugins/demo".to_owned(),
            reference: None,
            sha: None,
        },
        &directory.path().join("claude-workspace"),
    )
    .expect_err("Claude Git 插件不能跟随越界符号链接");
    assert!(claude_error.contains("符号链接"));
}

fn run_http_fixture(response: &'static str, max_bytes: usize) -> Result<Vec<u8>, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("绑定 HTTP 测试端口");
    let address = listener.local_addr().expect("读取 HTTP 测试端口");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("接受 HTTP 测试请求");
        // Windows 会在关闭仍有未读取请求数据的 socket 时发送 RST；先消费完整
        // 请求头，确保客户端稳定收到下面的受控响应，而不是笼统的发送失败。
        let mut reader = BufReader::new(stream.try_clone().expect("复制 HTTP 测试连接"));
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("读取 HTTP 测试请求");
            if read == 0 || line == "\r\n" || line == "\n" {
                break;
            }
        }
        stream
            .write_all(response.as_bytes())
            .expect("写入 HTTP 测试响应");
    });
    let result = http_get_with_headers(
        &format!("http://{address}/fixture"),
        &BTreeMap::new(),
        "测试下载",
        max_bytes,
    );
    server.join().expect("HTTP 测试服务线程不应 panic");
    result
}

/// HTTP Content-Length 超过限制时必须在读取响应体前失败。
#[test]
fn http_download_rejects_content_length_over_limit() {
    let error = run_http_fixture(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde",
        4,
    )
    .expect_err("Content-Length 超限必须失败");
    assert!(error.contains("超过 4 字节"), "实际错误：{error}");
}

/// Chunked HTTP 响应即使没有 Content-Length，也不能绕过下载大小限制。
#[test]
fn http_download_rejects_chunked_body_over_limit() {
    let error = run_http_fixture(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
        4,
    )
    .expect_err("chunked 响应超限必须失败");
    assert!(error.contains("超过 4 字节"), "实际错误：{error}");
}

/// 市场取得失败时临时目录应自动清理，成功登记则由调用方保留目录。
#[test]
fn temporary_marketplace_directory_cleans_only_on_failure() {
    let failed = test_directory("market-cleanup-failed");
    {
        let _cleanup = TemporaryMarketplaceDirectory::new(failed.clone());
    }
    assert!(!failed.exists());

    let kept = test_directory("market-cleanup-kept");
    {
        let mut cleanup = TemporaryMarketplaceDirectory::new(kept.clone());
        cleanup.keep();
    }
    assert!(kept.is_dir());
    fs::remove_dir_all(kept).expect("应清理成功取得目录");
}

/// 并发插件操作各自只清理独占 plan 目录，不误删其他操作或用户市场目录。
#[test]
fn plugin_download_cleanup_is_operation_owned() {
    let root = test_directory("plugin-download-cleanup");
    let downloads = root.join("downloads");
    fs::create_dir_all(downloads.join("fetch-existing")).expect("应创建既有下载目录");
    let market = root.join("user-market");
    fs::create_dir_all(&market).expect("应创建用户市场目录");
    let first = TemporaryPluginDownloads::new(&downloads).expect("应创建第一个独占工作区");
    let second = TemporaryPluginDownloads::new(&downloads).expect("应创建第二个独占工作区");
    let first_path = first.path().to_path_buf();
    let second_path = second.path().to_path_buf();
    fs::create_dir_all(first_path.join("fetch-owned")).expect("应创建第一个 fetch 目录");
    fs::create_dir_all(second_path.join("synthetic-owned")).expect("应创建第二个 synthetic 目录");
    drop(first);
    assert!(!first_path.exists());
    assert!(second_path.exists());
    drop(second);
    assert!(!second_path.exists());
    assert!(downloads.join("fetch-existing").exists());
    assert!(market.exists());
    fs::remove_dir_all(root).expect("应清理插件下载守卫测试目录");
}

/// 更新提交前必须拒绝已卸载或已被其他操作改变的安装记录。
#[test]
fn update_snapshot_rejects_removed_or_changed_plugin() {
    let expected = InstalledPlugin {
        id: PluginId::parse("demo@official").expect("应解析测试插件 ID"),
        version: "1.0.0".to_owned(),
        install_path: PathBuf::from("/tmp/keencode-test-cache/demo"),
        enabled: true,
        public_user_config: BTreeMap::new(),
        sensitive_user_config_keys: BTreeSet::new(),
        secret_generation: 0,
    };
    assert!(
        ensure_plugin_update_snapshot_current(
            std::slice::from_ref(&expected),
            &crate::claude_plugins::PluginState::default(),
        )
        .expect_err("已卸载插件必须拒绝提交")
        .contains("已被卸载")
    );

    let mut changed = expected.clone();
    changed.enabled = false;
    let current = crate::claude_plugins::PluginState {
        plugins: vec![changed],
    };
    assert!(
        ensure_plugin_update_snapshot_current(std::slice::from_ref(&expected), &current)
            .expect_err("已改变插件必须拒绝提交")
            .contains("状态已改变")
    );

    let mut changed_generation = expected.clone();
    changed_generation.secret_generation = 1;
    let current = crate::claude_plugins::PluginState {
        plugins: vec![changed_generation],
    };
    assert!(
        ensure_plugin_update_snapshot_current(std::slice::from_ref(&expected), &current)
            .expect_err("敏感配置代际已改变时必须拒绝过期更新提交")
            .contains("状态已改变")
    );
}

fn write_zip_archive(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Cursor;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (name, bytes) in entries {
        writer
            .start_file(*name, options)
            .expect("应写入 ZIP 测试条目");
        writer.write_all(bytes).expect("应写入 ZIP 测试内容");
    }
    let bytes = writer.finish().expect("应完成 ZIP 测试归档").into_inner();
    fs::write(path, bytes).expect("应写入 ZIP 测试归档文件");
}

fn write_tar_archive(path: &Path, entries: &[(&str, &[u8])]) {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        if name.contains("..") {
            // `Builder::append_data` 主动拒绝危险路径；这里直接构造一个
            // 已校验和的恶意测试头，验证读取侧不会静默修剪 `..`。
            header.set_path("safe.txt").expect("应设置 TAR 测试路径");
            {
                let raw = header.as_mut_bytes();
                raw[..100].fill(0);
                raw[..name.len()].copy_from_slice(name.as_bytes());
            }
            header.set_cksum();
            builder
                .append(&header, *bytes)
                .expect("应写入 TAR 测试条目");
        } else {
            header.set_cksum();
            builder
                .append_data(&mut header, *name, *bytes)
                .expect("应写入 TAR 测试条目");
        }
    }
    fs::write(path, builder.into_inner().expect("应完成 TAR 测试归档"))
        .expect("应写入 TAR 测试归档文件");
}

#[test]
fn zip_archive_rejects_path_escape_and_limits_entries_and_bytes() {
    let root = test_directory("safe-zip-archive");
    let archive = root.join("archive.zip");
    write_zip_archive(&archive, &[("../escaped.txt", b"escape")]);
    let error = extract_zip_archive(&root, &archive, "ZIP 测试", 8, 1024)
        .expect_err("ZIP 路径越界必须失败");
    assert!(error.contains("路径越界"));
    assert!(!root.parent().unwrap().join("escaped.txt").exists());

    write_zip_archive(&archive, &[("one.txt", b"one"), ("two.txt", b"two")]);
    assert!(
        extract_zip_archive(&root, &archive, "ZIP 测试", 1, 1024)
            .expect_err("ZIP 条目数超限必须失败")
            .contains("条目数超过")
    );
    assert!(
        extract_zip_archive(&root, &archive, "ZIP 测试", 8, 2)
            .expect_err("ZIP 解包字节数超限必须失败")
            .contains("解包后超过")
    );
    fs::remove_dir_all(root).expect("应清理 ZIP 安全测试目录");
}

#[cfg(unix)]
#[test]
fn zip_archive_rejects_symlink_entries() {
    use std::io::Cursor;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let root = test_directory("safe-zip-symlink");
    let archive = root.join("archive.zip");
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer
        .add_symlink("linked", "../../outside", options)
        .expect("应写入 ZIP 符号链接条目");
    fs::write(
        &archive,
        writer
            .finish()
            .expect("应完成 ZIP 符号链接归档")
            .into_inner(),
    )
    .expect("应写入 ZIP 符号链接归档文件");

    let error = extract_zip_archive(&root, &archive, "ZIP 测试", 8, 1024)
        .expect_err("ZIP 符号链接必须失败");
    assert!(error.contains("符号链接"));
    fs::remove_dir_all(root).expect("应清理 ZIP 符号链接测试目录");
}

#[test]
fn tar_archive_rejects_path_escape_and_limits_bytes() {
    let root = test_directory("safe-tar-archive");
    let archive = root.join("archive.tar");
    write_tar_archive(&archive, &[("../escaped.txt", b"escape")]);
    let error = extract_tar_reader(
        &root,
        File::open(&archive).expect("应打开 TAR 测试归档"),
        &archive,
        "TAR 测试",
        8,
        1024,
    )
    .expect_err("TAR 路径越界必须失败");
    assert!(error.contains("路径越界"));
    assert!(!root.parent().unwrap().join("escaped.txt").exists());

    write_tar_archive(&archive, &[("one.txt", b"one")]);
    assert!(
        extract_tar_reader(
            &root,
            File::open(&archive).expect("应打开 TAR 测试归档"),
            &archive,
            "TAR 测试",
            8,
            2,
        )
        .expect_err("TAR 解包字节数超限必须失败")
        .contains("解包后超过")
    );
    fs::remove_dir_all(root).expect("应清理 TAR 安全测试目录");
}

#[cfg(unix)]
#[test]
fn tar_archive_rejects_symlink_entries() {
    let root = test_directory("safe-tar-symlink");
    let archive = root.join("archive.tar");
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    builder
        .append_link(&mut header, "linked", "../../outside")
        .expect("应写入 TAR 符号链接条目");
    fs::write(
        &archive,
        builder.into_inner().expect("应完成 TAR 符号链接归档"),
    )
    .expect("应写入 TAR 符号链接归档文件");

    let error = extract_tar_reader(
        &root,
        File::open(&archive).expect("应打开 TAR 测试归档"),
        &archive,
        "TAR 测试",
        8,
        1024,
    )
    .expect_err("TAR 符号链接必须失败");
    assert!(error.contains("链接"));
    fs::remove_dir_all(root).expect("应清理 TAR 符号链接测试目录");
}

#[cfg(unix)]
#[test]
fn plugin_root_and_marketplace_preview_reject_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = test_directory("plugin-preview-symlink");
    let outside = root.join("outside");
    fs::create_dir_all(outside.join(".claude-plugin")).expect("应创建外部插件目录");
    fs::write(
        outside.join(".claude-plugin/plugin.json"),
        br#"{"name":"escaped"}"#,
    )
    .expect("应写入外部插件清单");
    symlink(&outside, root.join("linked")).expect("应创建插件根符号链接");
    assert!(find_plugin_root(&root).is_err());

    let market = root.join("market");
    fs::create_dir_all(market.join("plugin/.claude-plugin")).expect("应创建市场目录");
    symlink(&outside, market.join("linked")).expect("应创建市场插件符号链接");
    assert!(resolve_marketplace_relative_path(&market, "linked").is_err());
    symlink(
        outside.join(".claude-plugin/plugin.json"),
        market.join("plugin/.claude-plugin/plugin.json"),
    )
    .expect("应创建市场清单符号链接");
    let plugin = resolve_marketplace_relative_path(&market, "plugin")
        .expect("市场插件根目录本身应在市场根内");
    assert!(validate_directory_tree_without_symlinks(&plugin, "市场插件").is_err());
    fs::remove_dir_all(root).expect("应清理插件预览符号链接测试目录");
}

/// 顶层没有清单时仍应继续查找唯一的一级插件目录。
#[test]
fn find_plugin_root_continues_after_manifest_missing_at_archive_root() {
    let root = test_directory("plugin-root-nested-manifest");
    let plugin = root.join("package");
    fs::create_dir_all(plugin.join(".claude-plugin")).expect("应创建嵌套插件清单目录");
    fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        br#"{"name":"nested"}"#,
    )
    .expect("应写入嵌套插件清单");

    assert_eq!(
        find_plugin_root(&root).expect("应找到嵌套插件根目录"),
        fs::canonicalize(plugin).expect("插件目录应可规范化")
    );
    fs::remove_dir_all(root).expect("应清理嵌套插件根目录测试目录");
}

/// tar.gz 应复用同一套路径与解包大小限制，而不是走外部命令。
#[test]
fn tar_gz_archive_extracts_with_archive_safety_checks() {
    use flate2::{Compression, write::GzEncoder};

    let root = test_directory("safe-tar-gz-archive");
    let archive = root.join("archive.tgz");
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, "nested/file.txt", &b"hello"[..])
            .expect("应写入 TAR.GZ 测试条目");
        builder.finish().expect("应完成 TAR.GZ 测试归档");
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&tar_bytes)
        .expect("应压缩 TAR.GZ 测试归档");
    fs::write(&archive, encoder.finish().expect("应完成 TAR.GZ 测试归档"))
        .expect("应写入 TAR.GZ 测试归档文件");

    extract_archive(&root, &archive, "archive.tgz", "TAR.GZ 测试").expect("TAR.GZ 应能安全解包");
    assert_eq!(
        fs::read(root.join("nested/file.txt")).expect("应读取 TAR.GZ 文件"),
        b"hello"
    );
    fs::remove_dir_all(root).expect("应清理 TAR.GZ 安全测试目录");
}

/// 创建并清理一个当前测试专用的临时目录。
fn test_directory(label: &str) -> PathBuf {
    let path = env::temp_dir().join(format!("keencode-extensions-{label}-{}", process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("应创建测试目录");
    fs::canonicalize(path).expect("测试目录应返回规范绝对路径")
}

/// 当前原子写必须直接覆盖已有目标，不能先删除旧文件制造缺失窗口。
#[test]
fn atomic_private_write_replaces_existing_target() {
    let root = test_directory("atomic-replace");
    let path = root.join("state.json");
    fs::write(&path, b"old").expect("应写入旧文件");

    atomic_write_private(&path, b"new").expect("应原子覆盖已有文件");

    assert_eq!(fs::read(&path).expect("应读取新文件"), b"new");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path)
            .expect("应读取文件元数据")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    fs::remove_dir_all(root).expect("应清理原子覆盖测试目录");
}

/// 原子替换失败后必须删除同目录临时文件并保留原目标。
#[test]
fn atomic_private_write_cleans_temporary_file_after_failure() {
    let root = test_directory("atomic-cleanup");
    let path = root.join("occupied");
    fs::create_dir(&path).expect("应创建不可由文件覆盖的目标目录");

    assert!(atomic_write_private(&path, b"new").is_err());

    assert!(path.is_dir());
    let temporary_files = fs::read_dir(&root)
        .expect("应读取测试目录")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(temporary_files, 0);
    fs::remove_dir_all(root).expect("应清理失败回收测试目录");
}

/// 外部来源工具超时后必须结束整个进程树，而不是只结束根进程。
#[cfg(unix)]
#[test]
fn external_command_timeout_terminates_process_tree() {
    let root = test_directory("external-timeout-process-tree");
    let marker = root.join("child-survived");
    let mut command = process::Command::new("sh");
    command.env("KEENCODE_TIMEOUT_MARKER", &marker).args([
        "-c",
        r#"(sleep 0.4; touch "$KEENCODE_TIMEOUT_MARKER") & wait"#,
    ]);
    let started = Instant::now();
    let error = run_external_with_timeout(command, "测试外部命令", Duration::from_millis(100))
        .expect_err("超时命令必须返回错误");

    assert!(error.contains("执行超时"));
    assert!(started.elapsed() < Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(600));
    assert!(!marker.exists(), "超时后后台子进程不应继续运行");
    fs::remove_dir_all(root).expect("应清理外部命令超时测试目录");
}

/// 根进程先退出时，stderr 后代仍持有管道也必须在有限窗口内触发整树清理。
#[cfg(unix)]
#[test]
fn external_command_normal_exit_drains_inherited_stderr_without_hanging() {
    let root = test_directory("external-normal-exit-inherited-stderr");
    let marker = root.join("child-survived");
    let marker_for_command = marker.clone();
    let (result_sender, result_receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let mut command = process::Command::new("sh");
        command
            .env("KEENCODE_TIMEOUT_MARKER", marker_for_command)
            .args([
                "-c",
                r#"(sleep 2; printf inherited >&2; touch "$KEENCODE_TIMEOUT_MARKER") & exit 0"#,
            ]);
        let result = run_external_with_timeout(command, "测试外部命令", Duration::from_secs(1));
        result_sender.send(result).expect("应回传外部命令结果");
    });

    let result = result_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("根进程退出后 stderr drain 不应永久阻塞");
    worker.join().expect("外部命令线程应正常结束");
    assert!(result.is_ok(), "根进程成功退出不应被后代回收改写结果");
    std::thread::sleep(Duration::from_millis(600));
    assert!(!marker.exists(), "stderr 后代不应在清理后继续运行");
    fs::remove_dir_all(root).expect("应清理外部命令 stderr 测试目录");
}

/// 验证 Skill 前置元数据支持普通标量和折叠多行说明。
#[test]
fn parses_skill_frontmatter_scalars_and_folded_description() {
    let fields = parse_yaml_frontmatter(
        "---\nname: demo\ndescription: >-\n  第一行\n  第二行\n---\n# Demo\n",
    )
    .expect("应解析 Skill 前置元数据");
    assert_eq!(fields.get("name").map(String::as_str), Some("demo"));
    assert_eq!(
        fields.get("description").map(String::as_str),
        Some("第一行 第二行")
    );
}

/// 验证 Skill 前置元数据拒绝缺失闭合分隔符的内容。
#[test]
fn rejects_unclosed_skill_frontmatter() {
    let error = parse_yaml_frontmatter("---\nname: demo\n").expect_err("未闭合前置元数据必须失败");
    assert!(error.contains("未闭合"));
}

/// Skill 扫描不得通过符号链接目录项或主文件读取当前根目录外的内容。
#[cfg(unix)]
#[test]
fn rejects_symlinked_skill_entries_and_manifests() {
    use std::os::unix::fs::symlink;

    let root = test_directory("skill-symlink-boundary");
    let skills = root.join("skills");
    let outside = root.join("outside");
    fs::create_dir_all(&skills).expect("应创建 Skill 根目录");
    fs::create_dir_all(&outside).expect("应创建外部 Skill 目录");
    let outside_manifest = outside.join("SKILL.md");
    fs::write(
        &outside_manifest,
        "---\nname: escaped\ndescription: 越界 Skill\n---\n",
    )
    .expect("应写入外部 Skill");

    symlink(&outside, skills.join("linked")).expect("应创建目录符号链接");
    assert!(scan_skill_directory(&skills).is_err());
    fs::remove_file(skills.join("linked")).expect("应删除目录符号链接");

    let local = skills.join("local");
    fs::create_dir_all(&local).expect("应创建本地 Skill 目录");
    symlink(&outside_manifest, local.join("SKILL.md")).expect("应创建文件符号链接");
    assert!(scan_skill_directory(&skills).is_err());

    fs::remove_dir_all(root).expect("应清理 Skill 符号链接测试目录");
}

/// Skill 根目录本身不得是指向其他位置的符号链接。
#[cfg(unix)]
#[test]
fn rejects_symlinked_skill_root() {
    use std::os::unix::fs::symlink;

    let root = test_directory("skill-root-symlink");
    let real = root.join("real");
    fs::create_dir_all(real.join("demo")).expect("应创建真实 Skill 目录");
    fs::write(
        real.join("demo/SKILL.md"),
        "---\nname: demo\ndescription: 测试 Skill\n---\n",
    )
    .expect("应写入真实 Skill");
    let linked = root.join("linked");
    symlink(&real, &linked).expect("应创建 Skill 根目录符号链接");

    assert!(scan_skill_directory(&linked).is_err());
    fs::remove_dir_all(root).expect("应清理 Skill 根目录符号链接测试目录");
}

/// 市场插件 DTO 必须把 LSP 数量暴露给安装卡片与重启确认。
#[test]
fn available_plugin_dto_serializes_lsp_count() {
    let dto = AvailablePluginDto {
        name: "jdtls-lsp".to_owned(),
        marketplace: "claude-plugins-official".to_owned(),
        description: Some("Java language server".to_owned()),
        version: Some("1.0.0".to_owned()),
        skill_count: 0,
        lsp_count: 1,
    };

    assert_eq!(
        serde_json::to_value(dto).expect("应序列化市场插件 DTO"),
        serde_json::json!({
            "name": "jdtls-lsp",
            "marketplace": "claude-plugins-official",
            "description": "Java language server",
            "version": "1.0.0",
            "skillCount": 0,
            "lspCount": 1
        })
    );
}

/// Skill DTO 必须暴露显式来源和必填主文件路径。
#[test]
fn skill_dto_serializes_only_current_fields() {
    let dto = SkillDto {
        name: "demo".to_owned(),
        description: "Demo Skill".to_owned(),
        source: "plugin".to_owned(),
        path: "/tmp/demo/SKILL.md".to_owned(),
        user_invocable: true,
    };

    assert_eq!(
        serde_json::to_value(dto).expect("应序列化当前 Skill DTO"),
        serde_json::json!({
            "name": "demo",
            "description": "Demo Skill",
            "source": "plugin",
            "path": "/tmp/demo/SKILL.md",
            "userInvocable": true
        })
    );
}

/// 验证 MCP 只读取 peri 当前定义的 disabled 字段。
#[test]
fn mcp_enabled_state_uses_disabled_field() {
    let config = serde_json::json!({"disabled": true});
    assert!(!mcp_config_enabled(&config));
    assert!(mcp_config_enabled(&serde_json::json!({})));
}

/// 验证 MCP 列表始终反映外部配置，而不是 KeenCode 本地启用偏好。
#[test]
fn mcp_dto_uses_runtime_config_enabled_state() {
    let dto = mcp_dto(
        "demo".to_owned(),
        ResolvedMcpServer {
            config: serde_json::json!({
                "command": "demo-server",
                "disabled": true
            }),
            plugin_source: false,
        },
    );
    assert!(!dto.enabled);
    assert_eq!(
        serde_json::to_value(&dto).expect("应序列化当前 MCP DTO"),
        serde_json::json!({
            "name": "demo",
            "transport": "stdio",
            "target": "demo-server",
            "enabled": false
        })
    );
}

/// 验证 HTTP MCP 会按当前唯一传输类型输出。
#[test]
fn detects_http_mcp_transport_from_url() {
    let config = serde_json::json!({"url": "https://example.com/mcp"});
    assert_eq!(mcp_transport(&config), "http");
    let dto = mcp_dto(
        "http-demo".to_owned(),
        ResolvedMcpServer {
            config,
            plugin_source: false,
        },
    );
    assert_eq!(dto.transport, "http");
}

/// 插件 MCP 的已插值命令、参数和 URL 不能通过 inspect/doctor DTO 返回前端。
#[test]
fn plugin_mcp_dtos_hide_interpolated_sensitive_values() {
    let secret = "plugin-secret-value";
    let config = serde_json::json!({
        "command": format!("mcp-{secret}"),
        "args": ["--token", secret]
    });
    let server = ResolvedMcpServer {
        config,
        plugin_source: true,
    };

    let dto = mcp_dto("plugin:demo:server".to_owned(), server.clone());
    let dto_json = serde_json::to_string(&dto).expect("应序列化插件 MCP DTO");
    assert!(dto.target.is_none());
    assert!(!dto_json.contains(secret));

    let doctor = doctor_server("plugin:demo:server".to_owned(), server);
    let doctor_json = serde_json::to_string(&doctor).expect("应序列化 MCP Doctor DTO");
    assert!(doctor.target.is_none());
    assert!(!doctor_json.contains(secret));
}

/// 用户显式配置的 MCP 仍保留命令参数展示，避免无关功能退化。
#[test]
fn user_mcp_dto_keeps_target_arguments() {
    let dto = mcp_dto(
        "demo".to_owned(),
        ResolvedMcpServer {
            config: serde_json::json!({
                "command": "demo-server",
                "args": ["--mode", "safe mode"]
            }),
            plugin_source: false,
        },
    );
    assert_eq!(
        dto.target.as_deref(),
        Some("demo-server --mode \"safe mode\"")
    );
}

/// 验证 MCP 文件必须使用当前两种公开根结构之一。
#[test]
fn rejects_mcp_document_with_unknown_root_shape() {
    let root = test_directory("empty-mcp");
    let path = root.join("mcp.json");
    fs::write(&path, "{\"servers\":{}}\n").expect("应写入别名 MCP 测试配置");
    assert!(load_mcp_document(&path).is_err());

    fs::write(&path, "{\"demo\":{\"command\":\"demo\"}}\n").expect("应写入 flat MCP 测试配置");
    assert!(load_mcp_document(&path).is_err());

    fs::write(&path, "{\"mcpServers\":{},\"config\":{}}\n")
        .expect("应写入包含未知根字段的 MCP 测试配置");
    assert!(load_mcp_document(&path).is_err());
    fs::remove_dir_all(root).expect("应清理 MCP 测试目录");
}

/// 厂商常见的单层 Server 映射必须归一化为 canonical mcpServers 结构。
#[test]
fn accepts_vendor_root_server_map() {
    let document = parse_mcp_import_text(
        r#"{
          "gitee-ent": {
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@gitee/mcp-gitee-ent@latest"],
            "env": {
              "GITEE_ENT_API_BASE": "https://api.gitee.com/enterprises",
              "GITEE_ENT_MCP_ACCESS_TOKEN": "token"
            }
          }
        }"#,
    )
    .expect("厂商 MCP 配置应通过校验");
    assert_eq!(
        mcp_server_map(&document)
            .expect("应读取归一化的 MCP Server 映射")
            .keys()
            .collect::<Vec<_>>(),
        vec![&"gitee-ent".to_owned()]
    );
    assert_eq!(
        document
            .root
            .get("mcpServers")
            .and_then(Value::as_object)
            .map(|_| true),
        Some(true)
    );
    assert!(
        mcp_server_map(&document)
            .expect("应读取导入后的 MCP Server")
            .get("gitee-ent")
            .and_then(|config| config.get("type"))
            .is_none()
    );
}

/// 导入的 type 提示必须与实际传输字段一致，且归一化后不落盘。
#[test]
fn mcp_import_type_must_match_transport() {
    for text in [
        r#"{"demo":{"type":"stdio","url":"https://example.com"}}"#,
        r#"{"demo":{"type":"http","command":"demo"}}"#,
        r#"{"demo":{"type":"sse","url":"https://example.com"}}"#,
    ] {
        assert!(parse_mcp_import_text(text).is_err(), "{text}");
    }
    let document = parse_mcp_import_text(
        r#"{"mcpServers":{"demo":{"type":"http","url":"https://example.com"}}}"#,
    )
    .expect("type=http 与 url 应通过导入");
    assert!(
        mcp_server_map(&document).unwrap()["demo"]
            .get("type")
            .is_none()
    );
}

/// 导入必须在写入前完成全量冲突检查，冲突时不产生部分合并结果。
#[test]
fn merge_mcp_documents_rejects_any_conflict_atomically() {
    let existing = parse_mcp_document_text(r#"{"mcpServers":{"existing":{"command":"existing"}}}"#)
        .expect("现有 MCP 配置应通过校验");
    let imported = parse_mcp_document_text(
        r#"{"mcpServers":{"new":{"command":"new"},"existing":{"command":"replacement"}}}"#,
    )
    .expect("待导入 MCP 配置应通过校验");
    let error = merge_mcp_documents(existing.clone(), imported).expect_err("应拒绝冲突导入");
    assert!(error.contains("existing"));
    let existing_servers = mcp_server_map(&existing).expect("应读取现有映射");
    assert_eq!(existing_servers.len(), 1);
    assert!(existing_servers.contains_key("existing"));
    assert!(!existing_servers.contains_key("new"));
}

/// 验证 MCP Server 不接受未知字段、混合传输或缺失传输。
#[test]
fn rejects_non_current_mcp_server_shapes() {
    let root = test_directory("strict-mcp-server");
    let path = root.join("mcp.json");
    for document in [
        r#"{"mcpServers":{"demo":{"command":"demo","vendor":"old"}}}"#,
        r#"{"mcpServers":{"demo":{"command":"demo","url":"https://example.com"}}}"#,
        r#"{"mcpServers":{"demo":{"disabled":false}}}"#,
        r#"{"mcpServers":{"demo":{"command":"demo","disabled":false}}}"#,
        r#"{"mcpServers":{"demo":{"url":"ftp://example.com"}}}"#,
        r#"{"mcpServers":{"demo":{"url":"https://example.com","oauth":{"enabled":true}}}}"#,
    ] {
        fs::write(&path, document).expect("应写入无效 MCP 测试配置");
        assert!(load_mcp_document(&path).is_err(), "{document}");
    }
    fs::remove_dir_all(root).expect("应清理 MCP Server 结构测试目录");
}

/// 验证当前 stdio 与 HTTP MCP 结构能严格通过。
#[test]
fn accepts_current_mcp_server_shapes() {
    let root = test_directory("current-mcp-server");
    let path = root.join("mcp.json");
    fs::write(
        &path,
        r#"{
          "mcpServers": {
            "stdio": {"command":"npx","args":["-y","tool"],"env":{"TOKEN":"${TOKEN}"}},
            "http": {"url":"https://example.com/mcp","headers":{"Authorization":"Bearer ${TOKEN}"}}
          }
        }"#,
    )
    .expect("应写入当前 MCP 测试配置");
    let document = load_mcp_document(&path)
        .expect("当前 MCP 配置应通过")
        .expect("MCP 配置应存在");
    assert_eq!(
        mcp_server_map(&document).expect("应读取 MCP Server").len(),
        2
    );
    fs::remove_dir_all(root).expect("应清理当前 MCP 结构测试目录");
}

/// 验证 MCP 开关只写入 peri 当前定义的 disabled 字段。
#[test]
fn persists_mcp_enabled_and_disabled_consistently() {
    let mut document = empty_mcp_document();
    mcp_server_map_mut(&mut document)
        .expect("应返回可写 Server 映射")
        .insert(
            "demo".to_owned(),
            serde_json::json!({"command": "demo-server"}),
        );
    assert!(set_mcp_document_enabled(&mut document, "demo", false).expect("应设置 MCP 状态"));
    let config = &mcp_server_map(&document).expect("应返回 Server 映射")["demo"];
    assert_eq!(config.get("disabled").and_then(Value::as_bool), Some(true));

    assert!(set_mcp_document_enabled(&mut document, "demo", true).expect("应设置 MCP 状态"));
    let config = &mcp_server_map(&document).expect("应返回 Server 映射")["demo"];
    assert!(config.get("disabled").is_none());
}

/// 验证扩展名称会拒绝控制字符。
#[test]
fn rejects_control_characters_in_extension_names() {
    assert!(validate_extension_name("bad\nname", "插件").is_err());
    assert!(validate_extension_name(" demo ", "插件").is_err());
}

/// marketplace 插件 source 必须保留 ref、sha、path 与 sparsePaths，不能先转为字符串丢失固定版本。
#[test]
fn preserves_marketplace_plugin_source_pins_and_paths() {
    let spec = parse_marketplace_plugin_source(serde_json::json!({
        "source": "github",
        "repo": "acme/tools",
        "ref": "release",
        "sha": "0123456789012345678901234567890123456789",
        "path": "plugins/demo",
        "sparsePaths": ["plugins/demo", "shared/schema"]
    }))
    .expect("应解析完整插件 source");
    assert_eq!(
        spec,
        MarketplacePluginSourceSpec::Git {
            url: "https://github.com/acme/tools.git".to_owned(),
            path: Some("plugins/demo".to_owned()),
            reference: Some("release".to_owned()),
            sha: Some("0123456789012345678901234567890123456789".to_owned()),
            sparse_paths: vec!["plugins/demo".to_owned(), "shared/schema".to_owned()],
        }
    );
}

/// marketplace source 对象支持仓库 path、sparsePaths，并保留 URL headers。
#[test]
fn parses_marketplace_path_sparse_paths_and_url_headers() {
    let git = parse_marketplace_source_spec(
        r#"{"source":"github","repo":"acme/monorepo","ref":"main","path":"marketplace","sparsePaths":["marketplace","shared"]}"#,
    )
    .expect("应解析 marketplace JSON source")
    .expect("应返回 marketplace source");
    assert_eq!(
        git,
        MarketplaceSourceSpec::Git {
            url: "https://github.com/acme/monorepo.git".to_owned(),
            reference: Some("main".to_owned()),
            path: Some("marketplace".to_owned()),
            sparse_paths: vec!["marketplace".to_owned(), "shared".to_owned()],
        }
    );

    let url = parse_marketplace_source_spec(
        r#"{"source":"url","url":"https://example.test/marketplace.json","headers":{"Authorization":"Bearer token","X-Source":"test"}}"#,
    )
    .expect("应解析 URL marketplace source")
    .expect("应返回 URL source");
    assert_eq!(
        url,
        MarketplaceSourceSpec::Url {
            url: "https://example.test/marketplace.json".to_owned(),
            headers: BTreeMap::from([
                ("Authorization".to_owned(), "Bearer token".to_owned()),
                ("X-Source".to_owned(), "test".to_owned()),
            ]),
        }
    );
}

/// URL headers 和 Git sparse 路径必须拒绝控制字符、空路径及目录穿越。
#[test]
fn rejects_unsafe_marketplace_source_options() {
    assert!(
        parse_http_headers(Some(&serde_json::json!({
            "Authorization": "Bearer\nsecret"
        })))
        .is_err()
    );
    assert!(validate_source_relative_path("../outside", "市场 path").is_err());
    assert!(validate_source_relative_path("plugins/../../outside", "市场 path").is_err());
    assert!(validate_source_relative_path("/absolute", "市场 path").is_err());
    #[cfg(windows)]
    {
        assert!(validate_source_relative_path(r"..\outside", "市场 path").is_err());
        assert!(validate_source_relative_path(r"C:\outside", "市场 path").is_err());
    }
    assert!(
        parse_marketplace_source_spec(
            r#"{"source":"github","repo":"acme/tools","sparsePaths":"plugins"}"#,
        )
        .is_err()
    );
}

/// 插件 ID 必须拒绝路径片段并按 ASCII 大小写折叠比较，避免目录逃逸和重复安装。
#[test]
fn plugin_id_rejects_path_fragments_and_compares_case_insensitively() {
    for raw in ["../escape", "plugin@../market", "plugin.", "plugin@NUL"] {
        assert!(PluginId::parse(raw).is_err(), "应拒绝不安全插件 ID：{raw}");
    }

    let upper = PluginId::parse("Demo.Plugin@Official").expect("合法插件 ID 应能解析");
    let lower = PluginId::parse("demo.plugin@official").expect("合法插件 ID 应能解析");
    assert_eq!(upper, lower);
    assert_eq!(upper.storage_component(), "demo.plugin@official");
    assert_eq!(upper.to_string(), "Demo.Plugin@Official");
}

/// marketplace 允许用 `./` 声明市场根目录本身就是插件目录。
#[test]
fn resolves_marketplace_root_plugin_source() {
    let root = test_directory("marketplace-root-plugin");

    assert_eq!(
        resolve_marketplace_relative_path(&root, "./").expect("应解析市场根目录"),
        fs::canonicalize(&root).expect("应规范化市场根目录"),
    );

    fs::remove_dir_all(root).expect("应清理测试目录");
}

/// 无 model 键时插入新的模型覆盖行，正文与其他 frontmatter 原样保留。
#[test]
fn frontmatter_model_inserts_new_key() {
    let content = "---\nname: \"code-reviewer\"\ndescription: \"审查代码\"\n---\n\n正文";
    let updated = set_frontmatter_model(content, Some("openai::gpt-5")).expect("应插入 model 键");
    assert!(updated.contains("model: \"openai::gpt-5\"\n"));
    assert!(updated.ends_with("\n\n正文"));
    assert!(updated.starts_with("---\nname: \"code-reviewer\"\ndescription: \"审查代码\"\n"));
}

/// 已有 model 键时替换为新值，不产生重复行。
#[test]
fn frontmatter_model_replaces_existing_key() {
    let content =
        "---\nname: \"reviewer\"\ndescription: \"审查\"\nmodel: \"old::model\"\n---\n\n正文";
    let updated =
        set_frontmatter_model(content, Some("provider-a::model-a")).expect("应替换 model 键");
    assert_eq!(updated.matches("model:").count(), 1);
    assert!(updated.contains("model: \"provider-a::model-a\"\n"));
}

/// None 删除已有 model 键（回退跟随会话 provider）。
#[test]
fn frontmatter_model_removes_existing_key() {
    let content =
        "---\nname: \"reviewer\"\ndescription: \"审查\"\nmodel: \"old::model\"\n---\n\n正文";
    let updated = set_frontmatter_model(content, None).expect("应删除 model 键");
    assert!(!updated.contains("model:"));
    assert_eq!(
        updated,
        "---\nname: \"reviewer\"\ndescription: \"审查\"\n---\n\n正文"
    );
}

/// None 且原本无 model 键时内容保持不变。
#[test]
fn frontmatter_model_noop_when_absent() {
    let content = "---\nname: \"reviewer\"\ndescription: \"审查\"\n---\n\n正文";
    assert_eq!(
        set_frontmatter_model(content, None).expect("应保持内容不变"),
        content
    );
}

/// 只触碰顶层 model 键，缩进的嵌套键（如 MCP 配置）不受影响。
#[test]
fn frontmatter_model_ignores_indented_keys() {
    let content = "---\nname: \"reviewer\"\ndescription: \"审查\"\nmcp_servers:\n  - server: \"demo\"\n    model: \"nested\"\n---\n\n正文";
    let updated =
        set_frontmatter_model(content, Some("openai::gpt-5")).expect("应插入顶层 model 键");
    assert!(updated.contains("    model: \"nested\"\n"));
    assert!(updated.contains("model: \"openai::gpt-5\"\n"));
}

/// 缺少闭合分隔符的文件必须报错而不是静默截断。
#[test]
fn frontmatter_model_rejects_unclosed_frontmatter() {
    let content = "---\nname: \"reviewer\"\ndescription: \"审查\"\n";
    assert!(set_frontmatter_model(content, Some("openai::gpt-5")).is_err());
}

/// 设置页模型覆盖只接受规范的 Provider/模型限定引用。
#[test]
fn model_reference_accepts_only_provider_and_model() {
    assert_eq!(
        normalize_model_reference(" provider-a :: model-a ").expect("应规范化引用"),
        "provider-a::model-a"
    );
    for invalid in [
        "",
        "unqualified-model",
        "::model",
        "provider::",
        "provider::model::extra",
    ] {
        assert!(normalize_model_reference(invalid).is_err(), "{invalid:?}");
    }
    assert!(normalize_model_reference("provider\n::model").is_err());
}

/// 损坏的 MCP 用户配置备份必须带日期、避免冲突且不修改原文件。
#[test]
fn invalid_mcp_config_backup_is_dated_and_non_destructive() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let path = directory.path().join("mcp.json");
    fs::write(&path, "{broken").expect("写入损坏配置");

    let first = backup_invalid_mcp_config(&path).expect("创建首个备份");
    let second = backup_invalid_mcp_config(&path).expect("创建不冲突备份");

    assert_ne!(first, second);
    assert!(
        first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".bak")
    );
    assert_eq!(fs::read_to_string(first).unwrap(), "{broken");
    assert_eq!(fs::read_to_string(second).unwrap(), "{broken");
    assert_eq!(fs::read_to_string(path).unwrap(), "{broken");
}

/// 空快照无法写入时必须切到不存在路径，不能再次读取旧运行时内容。
#[test]
fn unavailable_mcp_runtime_path_never_reuses_old_snapshot() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let runtime_path = directory.path().join("mcp-runtime.json");
    fs::write(&runtime_path, r#"{"mcpServers":{"old":{"command":"old"}}}"#).expect("写入旧快照");

    let fallback = unavailable_mcp_runtime_path(&runtime_path);

    assert_ne!(fallback, runtime_path);
    assert!(!fallback.exists());
    assert!(fs::read_to_string(runtime_path).unwrap().contains("old"));
}

/// 插件敏感值只进入进程内 MCP 类型；写入运行时快照的文档只含用户配置。
#[test]
fn plugin_mcp_secret_stays_in_memory_and_out_of_runtime_document() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let runtime_path = directory.path().join("mcp-runtime.json");
    let secret = "plugin-secret-value";
    let user_document = empty_mcp_document();
    let mut runtime_document = user_document.clone();
    mcp_server_map_mut(&mut runtime_document)
        .expect("用户文档应包含 MCP 映射")
        .insert(
            "plugin:demo:secret".to_owned(),
            serde_json::json!({
                "command": "demo-mcp",
                "env": {"TOKEN": secret}
            }),
        );

    save_mcp_document(&runtime_path, &user_document).expect("用户 MCP 快照应可写入");
    let persisted = fs::read_to_string(&runtime_path).expect("读取运行时快照");
    assert!(!persisted.contains(secret));

    let plugin_servers = BTreeSet::from(["plugin:demo:secret".to_owned()]);
    let in_memory = mcp_config_from_document(&runtime_document, &runtime_path, &plugin_servers)
        .expect("插件配置应转换为 Peri 内存配置");
    assert_eq!(
        in_memory
            .mcp_servers
            .get("plugin:demo:secret")
            .and_then(|server| server.env.as_ref())
            .and_then(|env| env.get("TOKEN")),
        Some(&secret.to_owned())
    );
}

/// 损坏配置备份不得跟随符号链接读取或替换链接目标。
#[cfg(unix)]
#[test]
fn invalid_mcp_backup_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("创建临时目录");
    let target = directory.path().join("outside.json");
    let path = directory.path().join("mcp.json");
    fs::write(&target, "{broken target").expect("写入链接目标");
    symlink(&target, &path).expect("创建 MCP 符号链接");

    assert!(backup_invalid_mcp_config(&path).is_err());
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "{broken target");
}
