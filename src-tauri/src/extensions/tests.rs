//! 扩展、插件市场、Agent、Skill 与 MCP 的完整回归测试。

use super::*;

/// 全局 Agent 模板目录列出固定支持工具，但不暴露根 Agent 专用或动态 MCP 工具。
#[test]
fn agents_tool_catalog_lists_template_support_tools() {
    let catalog = agents_tool_catalog().expect("Agent 模板工具目录应可读取");
    assert_eq!(
        catalog.tools,
        vec![
            "Bash",
            "PowerShell",
            "Git",
            "Write",
            "Edit",
            "Read",
            "Glob",
            "Grep",
            "TaskOutput",
            "TaskStop",
            "WebFetch",
            "WebSearch",
            "Skill",
            "PluginCommand",
            "ToolSearch",
            "ExecuteExtraTool",
            "LSP",
            "send_message",
            "followup_task",
            "interrupt_agent",
            "retry_agent",
            "list_agents",
            "wait_agent",
        ]
    );
    for excluded in ["spawn_agent", "AskUser", "TodoWrite", "Goal", "Plan"] {
        assert!(
            !catalog.tools.iter().any(|name| name == excluded),
            "目录不应包含根 Agent 专用工具 {excluded}"
        );
    }
}

/// 扩展查询的空项目路径只能进入全局视图，非空路径必须交给授权解析器。
#[test]
fn extension_query_project_path_requires_registered_root_for_non_empty_value() {
    let mut resolved = Vec::new();
    let root = resolve_extension_project_root_with(Some("  D:/projects/active  "), |path| {
        resolved.push(path.to_owned());
        Ok(PathBuf::from("D:/projects/active"))
    })
    .expect("非空项目路径应完成授权解析")
    .expect("非空项目路径应返回项目根");
    assert_eq!(root, PathBuf::from("D:/projects/active"));
    assert_eq!(resolved, vec!["D:/projects/active"]);

    assert_eq!(
        resolve_extension_project_root_with(None, |_| {
            Err("全局视图不应解析项目路径".to_owned())
        })
        .expect("空项目路径应进入全局视图"),
        None
    );
    assert_eq!(
        resolve_extension_project_root_with(Some("   "), |_| {
            Err("空白项目路径不应解析".to_owned())
        })
        .expect("空白项目路径应进入全局视图"),
        None
    );
}

/// 全局扩展视图的占位根必须脱离进程 current_dir，且不模拟项目授权。
#[test]
fn extension_global_view_root_is_derived_from_data_root() {
    let data_root = Path::new("D:/keencode-data");
    assert_eq!(
        extension_global_view_root(data_root),
        data_root.join(".keencode-global-view")
    );
}

/// 缺失插件市场状态只能返回当前空结构，保存后必须带严格 schema/version。
#[test]
fn marketplace_store_missing_file_roundtrips_current_schema() {
    let directory = tempfile::tempdir().expect("创建插件市场状态临时目录");
    let path = directory.path().join("marketplaces.json");

    let store = load_marketplace_store_from_path(&path).expect("缺失状态应返回当前空结构");
    assert_eq!(store.schema, MARKETPLACE_STORE_SCHEMA);
    assert_eq!(store.version, MARKETPLACE_STORE_VERSION);
    assert!(store.sources.is_empty());
    assert!(!path.exists());

    save_marketplace_store_to_path(&path, &store).expect("当前空状态应可保存");
    let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["schema"], MARKETPLACE_STORE_SCHEMA);
    assert_eq!(persisted["version"], MARKETPLACE_STORE_VERSION);
    assert_eq!(persisted["sources"], serde_json::json!([]));
    assert!(
        load_marketplace_store_from_path(&path)
            .unwrap()
            .sources
            .is_empty()
    );
}

/// 旧结构、损坏 JSON、未知字段和错误版本必须失败关闭且保留原文件。
#[test]
fn invalid_marketplace_store_is_rejected_without_replacement() {
    let directory = tempfile::tempdir().expect("创建插件市场状态临时目录");
    let path = directory.path().join("marketplaces.json");
    let cases = [
        b"not-json".as_slice(),
        br#"{"sources":[]}"#,
        br#"{"schema":"keencode/marketplace-store","version":0,"sources":[]}"#,
        br#"{"schema":"keencode/marketplace-store","version":1,"sources":[],"unexpected":true}"#,
    ];

    for (index, original) in cases.into_iter().enumerate() {
        fs::write(&path, original).expect("写入非法插件市场状态");
        assert!(
            load_marketplace_store_from_path(&path).is_err(),
            "非法插件市场状态 {index} 不应被接受"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

/// KeenCode 命令命名空间必须保留嵌套目录，但不能把 `commands` 根目录当作名称。
#[test]
fn plugin_command_namespace_uses_command_relative_path() {
    assert_eq!(
        plugin_command_namespace("plugin:market:demo", Path::new("commands/foo.md")),
        "plugin:market:demo:foo"
    );
    assert_eq!(
        plugin_command_namespace("plugin:market:demo", Path::new("commands/admin/check.md")),
        "plugin:market:demo:admin:check"
    );
}

/// 自定义嵌套 marketplace.json 必须按记录保存的 manifestPath 重载，而不是回退到根目录默认路径。
#[test]
fn loads_nested_keencode_marketplace_manifest_from_record() {
    let root = test_directory("nested-keencode-marketplace");
    let manifest_path = root.join("catalog/.keencode-plugin/marketplace.json");
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
    let manifest =
        load_marketplace_manifest_from_record(&record).expect("应按 manifestPath 读取嵌套清单");
    assert_eq!(manifest.name, "nested");
    fs::remove_dir_all(root).expect("应清理嵌套市场测试目录");
}

/// 市场插件必须自带唯一标准 plugin.json，不能从目录或市场字段合成清单。
#[test]
fn rejects_marketplace_plugin_without_plugin_manifest() {
    let root = test_directory("manifestless-marketplace-plugin");
    let plugin_root = root.join("plugin");
    fs::create_dir_all(plugin_root.join("skills/demo")).expect("应创建无清单插件目录");
    fs::write(plugin_root.join("skills/demo/SKILL.md"), "# Demo").expect("应写入无清单组件");
    let marketplace = crate::plugins::parse_marketplace_manifest(
        br#"{"name":"market","plugins":[{"name":"demo","source":"./plugin","skills":["./skills/demo"]}]}"#,
    )
    .expect("市场清单本身应有效");

    let error = materialize_marketplace_plugin_entry(
        &marketplace.plugins[0],
        &root.join(".keencode-plugin/marketplace.json"),
        &root,
        None,
        &root.join("downloads"),
    )
    .expect_err("缺少 plugin.json 的插件必须拒绝");

    assert!(error.contains(crate::plugins::PLUGIN_MANIFEST));
    assert!(!plugin_root.join(crate::plugins::PLUGIN_MANIFEST).exists());
    fs::remove_dir_all(root).expect("应清理无清单插件测试目录");
}

/// 官方市场的隐藏清单必须转换成仓库根锚定 sparse pattern，避免被 Git 模糊匹配漏掉。
#[test]
fn sparse_checkout_patterns_anchor_marketplace_manifest() {
    assert_eq!(
        sparse_checkout_pattern(".keencode-plugin/marketplace.json"),
        "/.keencode-plugin/marketplace.json"
    );
    assert_eq!(sparse_checkout_pattern("./plugins"), "/plugins");
}

/// 自定义 Git 市场未声明 sparsePaths 时必须保留清单引用的相对插件目录。
#[test]
fn git_marketplace_without_sparse_paths_checks_out_relative_plugins() {
    let directory = tempfile::tempdir().expect("创建 Git 市场测试目录");
    let repository = directory.path().join("repository");
    let plugin = repository.join("plugins/demo/.keencode-plugin");
    fs::create_dir_all(&plugin).expect("创建测试插件目录");
    fs::create_dir_all(repository.join(".keencode-plugin")).expect("创建测试市场目录");
    fs::write(
        repository.join(".keencode-plugin/marketplace.json"),
        br#"{"name":"custom","plugins":[{"name":"demo","source":"./plugins/demo"}]}"#,
    )
    .expect("写入测试市场清单");
    fs::write(plugin.join("plugin.json"), br#"{"name":"demo"}"#).expect("写入测试插件清单");

    let mut init = process::Command::new("git");
    init.current_dir(&repository).args(["init", "--quiet"]);
    run_external(&mut init, "初始化 Git 市场测试仓库").expect("初始化测试仓库");
    let mut add = process::Command::new("git");
    add.current_dir(&repository).args(["add", "."]);
    run_external(&mut add, "暂存 Git 市场测试仓库").expect("暂存测试仓库");
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
    run_external(&mut commit, "提交 Git 市场测试仓库").expect("提交测试仓库");

    let workspace = directory.path().join("workspace");
    let materialized = materialize_marketplace_spec(
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
            .join("plugins/demo/.keencode-plugin/plugin.json")
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
    run_external(&mut init, "初始化 Git 插件测试仓库").expect("初始化测试仓库");
    let mut add = process::Command::new("git");
    add.current_dir(&repository).args(["add", "."]);
    run_external(&mut add, "暂存 Git 插件测试仓库").expect("暂存测试仓库");
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
    run_external(&mut commit, "提交 Git 插件测试仓库").expect("提交测试仓库");

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

    let keencode_error = materialize_plugin_source(
        &PluginSource::GitSubdir {
            url: git_url,
            path: "plugins/demo".to_owned(),
            reference: None,
            sha: None,
        },
        &directory.path().join("keencode-workspace"),
    )
    .expect_err("KeenCode Git 插件不能跟随越界符号链接");
    assert!(keencode_error.contains("符号链接"));
}

/// 启动一次性本地 HTTP Server，并通过生产下载路径读取受控响应。
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
    fs::create_dir_all(second_path.join("second-owned")).expect("应创建第二个操作目录");
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
            &crate::plugins::PluginState::default(),
        )
        .expect_err("已卸载插件必须拒绝提交")
        .contains("已被卸载")
    );

    let mut changed = expected.clone();
    changed.enabled = false;
    let current = crate::plugins::PluginState {
        plugins: vec![changed],
    };
    assert!(
        ensure_plugin_update_snapshot_current(std::slice::from_ref(&expected), &current)
            .expect_err("已改变插件必须拒绝提交")
            .contains("状态已改变")
    );

    let mut changed_generation = expected.clone();
    changed_generation.secret_generation = 1;
    let current = crate::plugins::PluginState {
        plugins: vec![changed_generation],
    };
    assert!(
        ensure_plugin_update_snapshot_current(std::slice::from_ref(&expected), &current)
            .expect_err("敏感配置代际已改变时必须拒绝过期更新提交")
            .contains("状态已改变")
    );
}

/// 写入包含指定普通文件条目的 ZIP 测试归档。
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

/// 写入包含指定普通文件或危险路径条目的 TAR 测试归档。
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
/// ZIP 解包必须同时拒绝目录越界、条目数超限和解包体积超限。
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
/// ZIP 解包必须拒绝符号链接条目。
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
/// TAR 解包必须同时拒绝目录越界和解包体积超限。
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
/// TAR 解包必须拒绝符号链接条目。
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
/// 插件根发现和市场预览都不得通过符号链接离开受控目录。
fn plugin_root_and_marketplace_preview_reject_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = test_directory("plugin-preview-symlink");
    let outside = root.join("outside");
    fs::create_dir_all(outside.join(".keencode-plugin")).expect("应创建外部插件目录");
    fs::write(
        outside.join(".keencode-plugin/plugin.json"),
        br#"{"name":"escaped"}"#,
    )
    .expect("应写入外部插件清单");
    symlink(&outside, root.join("linked")).expect("应创建插件根符号链接");
    assert!(find_plugin_root(&root).is_err());

    let market = root.join("market");
    fs::create_dir_all(market.join("plugin/.keencode-plugin")).expect("应创建市场目录");
    symlink(&outside, market.join("linked")).expect("应创建市场插件符号链接");
    assert!(resolve_marketplace_relative_path(&market, "linked").is_err());
    symlink(
        outside.join(".keencode-plugin/plugin.json"),
        market.join("plugin/.keencode-plugin/plugin.json"),
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
    fs::create_dir_all(plugin.join(".keencode-plugin")).expect("应创建嵌套插件清单目录");
    fs::write(
        plugin.join(".keencode-plugin/plugin.json"),
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

/// 外部来源工具超时后必须主动结束，而不是让插件安装永久等待。
#[cfg(unix)]
#[test]
fn external_command_timeout_terminates_child() {
    let mut command = process::Command::new("sleep");
    command.arg("2");
    let started = Instant::now();
    let error = run_external_with_timeout(&mut command, "测试外部命令", Duration::from_millis(100))
        .expect_err("超时命令必须返回错误");

    assert!(error.contains("执行超时"));
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// 扩展路径映射必须复用运行时 Skill 解析器处理折叠说明。
#[test]
fn parses_skill_metadata_with_runtime_parser() {
    let root = test_directory("skill-runtime-parser");
    let path = root.join("SKILL.md");
    fs::write(
        &path,
        "---\nname: demo\ndescription: >-\n  第一行\n  第二行\n---\n# Demo\n",
    )
    .expect("应写入 Skill");

    let parsed = parse_skill_file(&path).expect("应使用运行时解析器读取 Skill");

    assert_eq!(parsed, ("demo".to_owned(), "第一行 第二行".to_owned()));
    fs::remove_dir_all(root).expect("应清理 Skill 解析测试目录");
}

/// 扩展路径映射必须复用运行时 Skill 解析器拒绝未闭合元数据。
#[test]
fn rejects_unclosed_skill_metadata_with_runtime_parser() {
    let root = test_directory("invalid-skill-runtime-parser");
    let path = root.join("SKILL.md");
    fs::write(&path, "---\nname: demo\n").expect("应写入无效 Skill");

    let error = parse_skill_file(&path).expect_err("未闭合前置元数据必须失败");

    assert!(error.contains(&keencode_skills::SkillDocumentError::UnclosedFrontMatter.to_string()));
    fs::remove_dir_all(root).expect("应清理无效 Skill 解析测试目录");
}

/// Skill 路径扫描必须递归发现嵌套清单，并按稳定相对路径选择同名首项。
#[test]
fn scans_nested_skills_with_stable_duplicate_priority() {
    let root = test_directory("nested-skill-paths");
    let skills = root.join("skills");
    let first = skills.join("a/deep/SKILL.md");
    let second = skills.join("z/SKILL.md");
    fs::create_dir_all(first.parent().expect("首个 Skill 应有父目录"))
        .expect("应创建嵌套 Skill 目录");
    fs::create_dir_all(second.parent().expect("第二个 Skill 应有父目录"))
        .expect("应创建第二个 Skill 目录");
    let document = "---\nname: duplicate\ndescription: 嵌套 Skill\n---\n";
    fs::write(&first, document).expect("应写入嵌套 Skill");
    fs::write(&second, document).expect("应写入第二个 Skill");

    let scanned = scan_skill_directory(&skills);

    assert_eq!(scanned.len(), 2);
    assert_eq!(
        scanned[0].path,
        first.canonicalize().expect("应规范化首个 Skill")
    );
    assert_eq!(
        scanned[1].path,
        second.canonicalize().expect("应规范化第二个 Skill")
    );
    fs::remove_dir_all(root).expect("应清理嵌套 Skill 测试目录");
}

/// Skill 路径归约必须安全跳过符号链接目录项和主文件。
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
    assert!(scan_skill_directory(&skills).is_empty());
    fs::remove_file(skills.join("linked")).expect("应删除目录符号链接");

    let local = skills.join("local");
    fs::create_dir_all(&local).expect("应创建本地 Skill 目录");
    symlink(&outside_manifest, local.join("SKILL.md")).expect("应创建文件符号链接");
    assert!(scan_skill_directory(&skills).is_empty());

    fs::remove_dir_all(root).expect("应清理 Skill 符号链接测试目录");
}

/// Skill 根目录本身是符号链接时必须按空来源处理。
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

    assert!(scan_skill_directory(&linked).is_empty());
    fs::remove_dir_all(root).expect("应清理 Skill 根目录符号链接测试目录");
}

/// 市场插件 DTO 必须把 LSP 数量暴露给安装卡片与重启确认。
#[test]
fn available_plugin_dto_serializes_lsp_count() {
    let dto = AvailablePluginDto {
        name: "jdtls-lsp".to_owned(),
        marketplace: "plugins-official".to_owned(),
        description: Some("Java language server".to_owned()),
        version: Some("1.0.0".to_owned()),
        skill_count: 0,
        lsp_count: 1,
    };

    assert_eq!(
        serde_json::to_value(dto).expect("应序列化市场插件 DTO"),
        serde_json::json!({
            "name": "jdtls-lsp",
            "marketplace": "plugins-official",
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

/// 验证 MCP 只读取 KeenCode 唯一 Schema 的 disabled 字段。
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
            "source": "user",
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

    let dto = mcp_dto("plugin:market:demo:server".to_owned(), server.clone());
    let dto_json = serde_json::to_string(&dto).expect("应序列化插件 MCP DTO");
    assert!(dto.target.is_none());
    assert_eq!(dto.source, "plugin");
    assert!(!dto_json.contains(secret));

    let doctor = doctor_server("plugin:market:demo:server".to_owned(), server);
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

/// HTTP OAuth 的公开绑定应原样保留，省略 scopes 时使用空集合。
#[test]
fn mcp_oauth_config_preserves_only_public_client_binding() {
    let value = serde_json::json!({
        "url": "https://mcp.example.test/api",
        "headers": {"X-Client": "keencode"},
        "oauth": {
            "clientId": "keencode-desktop",
            "resource": "https://mcp.example.test/api",
            "scopes": ["tools:read", "tools:write"]
        }
    });
    let server = runtime_mcp_server_from_value("demo", &value, Path::new("."))
        .expect("HTTP MCP 应接受非秘密公共客户端配置");
    let oauth = server.oauth.expect("运行时应保留 OAuth 绑定");
    assert_eq!(oauth.client_id, "keencode-desktop");
    assert_eq!(oauth.resource, "https://mcp.example.test/api");
    assert_eq!(oauth.scopes, ["tools:read", "tools:write"]);
    assert_eq!(serde_json::to_value(oauth).unwrap(), value["oauth"]);

    let without_scopes = serde_json::json!({
        "url": "https://mcp.example.test/api",
        "oauth": {"clientId": "keencode-desktop", "resource": "https://mcp.example.test/api"}
    });
    let server = runtime_mcp_server_from_value("demo", &without_scopes, Path::new("."))
        .expect("未声明 scopes 时应使用空集合");
    assert!(server.oauth.unwrap().scopes.is_empty());
}

/// OAuth 只用于 HTTP，且禁止静态 Authorization、令牌或旧配置字段混入。
#[test]
fn mcp_oauth_config_rejects_transport_conflicts_and_secret_fields() {
    let oauth = serde_json::json!({
        "clientId": "keencode-desktop",
        "resource": "https://mcp.example.test/api"
    });
    let stdio = serde_json::json!({"command": "demo-server", "oauth": oauth.clone()});
    assert!(validate_mcp_server_config("demo", &stdio).is_err());
    for header in ["Authorization", "authorization", "aUtHoRiZaTiOn"] {
        let mut headers = Map::new();
        headers.insert(
            header.to_owned(),
            Value::String("test-placeholder".to_owned()),
        );
        let value = serde_json::json!({
            "url": "https://mcp.example.test/api", "headers": headers, "oauth": oauth.clone()
        });
        assert!(validate_mcp_server_config("demo", &value).is_err());
    }
    for field in [
        "enabled",
        "accessToken",
        "refreshToken",
        "clientSecret",
        "unknown",
    ] {
        let mut invalid = oauth.clone();
        invalid[field] = Value::String("sensitive-test-marker".to_owned());
        let value = serde_json::json!({"url": "https://mcp.example.test/api", "oauth": invalid});
        let error = validate_mcp_server_config("demo", &value).unwrap_err();
        assert!(!error.contains("sensitive-test-marker"));
    }
}

/// OAuth 绑定损坏时必须在保存前失败，错误不得复述不可信配置内容。
#[test]
fn mcp_oauth_config_rejects_invalid_public_binding() {
    for oauth in [
        Value::Null,
        serde_json::json!({}),
        serde_json::json!({"clientId": "", "resource": "https://mcp.example.test/api"}),
        serde_json::json!({"clientId": "bad\nclient", "resource": "https://mcp.example.test/api"}),
        serde_json::json!({"clientId": "demo", "resource": "http://mcp.example.test/api"}),
        serde_json::json!({"clientId": "demo", "resource": "https://user:secret@mcp.example.test/api"}),
        serde_json::json!({"clientId": "demo", "resource": "https://mcp.example.test/api#fragment"}),
        serde_json::json!({"clientId": "demo", "resource": "https://mcp.example.test/api", "scopes": ["bad\nscope"]}),
    ] {
        let value = serde_json::json!({"url": "https://mcp.example.test/api", "oauth": oauth});
        assert!(
            validate_mcp_server_config("demo", &value).is_err(),
            "{value}"
        );
    }
}

/// 验证 MCP 开关只写入 KeenCode 唯一 Schema 的 disabled 字段。
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
    assert!(validate_source_relative_path("/absolute", "市场 path").is_err());
    assert!(
        parse_marketplace_source_spec(
            r#"{"source":"github","repo":"acme/tools","sparsePaths":"plugins"}"#,
        )
        .is_err()
    );
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

/// 新 MCP 桥只返回启用项，并分别使用项目根与插件根作为 stdio 工作目录。
#[test]
fn runtime_mcp_servers_merge_enabled_sources_with_scoped_working_directories() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let project_root = directory.path().join("project");
    let plugin_root = directory.path().join("plugin");
    fs::create_dir_all(&project_root).expect("创建项目目录");
    fs::create_dir_all(&plugin_root).expect("创建插件目录");
    let document = parse_mcp_document_text(
        r#"{
            "mcpServers": {
                "project-server": {
                    "command": "project-mcp",
                    "args": ["--stdio"],
                    "env": {"PROJECT_TOKEN": "project-secret"}
                },
                "disabled-project": {
                    "command": "disabled-mcp",
                    "disabled": true
                }
            }
        }"#,
    )
    .expect("用户 MCP 文档有效");
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![crate::plugins::RuntimePlugin {
            id: PluginId {
                plugin: "demo".to_owned(),
                marketplace: Some("local".to_owned()),
            },
            root: plugin_root.clone(),
            commands: Vec::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            hooks: None,
            unsupported_hooks: Vec::new(),
            mcp_servers: BTreeMap::from([
                (
                    "plugin-server".to_owned(),
                    serde_json::json!({
                        "command": "plugin-mcp",
                        "env": {"PLUGIN_TOKEN": "plugin-secret"}
                    }),
                ),
                (
                    "disabled-plugin".to_owned(),
                    serde_json::json!({"command": "disabled-mcp", "disabled": true}),
                ),
            ]),
            lsp_servers: Vec::new(),
        }],
    };

    let (servers, diagnostics) =
        runtime_mcp_servers_from_sources(&document, snapshot, &project_root)
            .expect("应构造新 MCP 运行时配置");

    assert!(diagnostics.is_empty());
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0].id, "plugin:local:demo:plugin-server");
    let keencode_mcp::McpServerConfig::Stdio(plugin) = &servers[0].config else {
        panic!("插件 Server 应使用 stdio");
    };
    assert_eq!(plugin.current_dir.as_deref(), Some(plugin_root.as_path()));
    assert_eq!(
        plugin.environment.get("PLUGIN_TOKEN").map(String::as_str),
        Some("plugin-secret")
    );
    assert!(plugin.inherit_environment);

    assert_eq!(servers[1].id, "project-server");
    let keencode_mcp::McpServerConfig::Stdio(project) = &servers[1].config else {
        panic!("用户 Server 应使用 stdio");
    };
    assert_eq!(project.current_dir.as_deref(), Some(project_root.as_path()));
    assert_eq!(project.args, ["--stdio"]);
    assert_eq!(
        project.environment.get("PROJECT_TOKEN").map(String::as_str),
        Some("project-secret")
    );
    assert!(project.inherit_environment);
}

/// 两个市场中的同名插件和 Server 必须保留各自独立的 MCP 运行时身份。
#[test]
fn runtime_mcp_server_namespace_includes_marketplace() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let project_root = directory.path().join("project");
    let plugin_root = directory.path().join("plugin");
    fs::create_dir_all(&project_root).expect("创建项目目录");
    fs::create_dir_all(&plugin_root).expect("创建插件目录");
    let plugin = |marketplace: &str| crate::plugins::RuntimePlugin {
        id: PluginId {
            plugin: "demo".to_owned(),
            marketplace: Some(marketplace.to_owned()),
        },
        root: plugin_root.clone(),
        commands: Vec::new(),
        skills: Vec::new(),
        agents: Vec::new(),
        hooks: None,
        unsupported_hooks: Vec::new(),
        mcp_servers: BTreeMap::from([(
            "server".to_owned(),
            serde_json::json!({"command": "plugin-mcp"}),
        )]),
        lsp_servers: Vec::new(),
    };
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![plugin("alpha"), plugin("beta")],
    };

    let (servers, diagnostics) =
        runtime_mcp_servers_from_sources(&empty_mcp_document(), snapshot, &project_root)
            .expect("同名插件 MCP 应完成命名空间归约");

    assert!(diagnostics.is_empty());
    assert_eq!(
        servers
            .iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["plugin:alpha:demo:server", "plugin:beta:demo:server"]
    );
}

/// 单个插件 MCP 配置无效时只跳过该 Server，并把原因留给 Runtime 首个根 Turn 通知客户端。
#[test]
fn runtime_mcp_servers_skip_invalid_plugin_with_diagnostic() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let project_root = directory.path().join("project");
    let plugin_root = directory.path().join("plugin");
    fs::create_dir_all(&project_root).expect("创建项目目录");
    fs::create_dir_all(&plugin_root).expect("创建插件目录");
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![crate::plugins::RuntimePlugin {
            id: PluginId {
                plugin: "demo".to_owned(),
                marketplace: Some("local".to_owned()),
            },
            root: plugin_root,
            commands: Vec::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            hooks: None,
            unsupported_hooks: Vec::new(),
            mcp_servers: BTreeMap::from([
                (
                    "invalid".to_owned(),
                    serde_json::json!({"command": "bad", "url": "https://also-bad"}),
                ),
                ("valid".to_owned(), serde_json::json!({"command": "good"})),
            ]),
            lsp_servers: Vec::new(),
        }],
    };

    let (servers, diagnostics) =
        runtime_mcp_servers_from_sources(&empty_mcp_document(), snapshot, &project_root)
            .expect("插件 MCP 配置故障不应阻断候选构建");

    assert_eq!(
        servers
            .iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["plugin:local:demo:valid"]
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].source, "mcp");
    assert_eq!(diagnostics[0].server, "plugin:local:demo:invalid");
    assert_eq!(diagnostics[0].code, "mcp_config_invalid");
    assert!(
        diagnostics[0]
            .message
            .contains("只能声明 command 或 url 之一")
    );
}

/// HTTP MCP 桥必须保留端点和内存请求头，并在关闭时终止服务端会话。
#[test]
fn runtime_mcp_server_maps_streamable_http_fields() {
    let server = runtime_mcp_server_from_value(
        "remote",
        &serde_json::json!({
            "url": "https://mcp.example.test/rpc",
            "headers": {"Authorization": "Bearer in-memory-secret"}
        }),
        Path::new("ignored-for-http"),
    )
    .expect("HTTP MCP 配置应完成转换");

    let keencode_mcp::McpServerConfig::StreamableHttp(http) = server.config else {
        panic!("远程 Server 应使用 Streamable HTTP");
    };
    assert_eq!(http.endpoint, "https://mcp.example.test/rpc");
    assert_eq!(
        http.headers.get("Authorization").map(String::as_str),
        Some("Bearer in-memory-secret")
    );
    assert!(http.terminate_session_on_close);
}

/// 插件只暴露清单精确声明的 SKILL.md 父目录，且不得递归加载相邻内容。
#[test]
fn runtime_skill_config_uses_exact_non_recursive_plugin_roots() {
    let directory = tempfile::tempdir().expect("创建临时目录");
    let data_root = directory.path().join("data");
    let project_root = directory.path().join("project");
    let plugin_root = directory.path().join("plugin");
    let first_root = plugin_root.join("skills/first");
    let second_root = plugin_root.join("bundles/second");
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![crate::plugins::RuntimePlugin {
            id: PluginId {
                plugin: "demo".to_owned(),
                marketplace: Some("local".to_owned()),
            },
            root: plugin_root,
            commands: Vec::new(),
            skills: vec![
                crate::plugins::ComponentFile {
                    path: first_root.join("SKILL.md"),
                    relative_path: PathBuf::from("skills/first/SKILL.md"),
                },
                crate::plugins::ComponentFile {
                    path: second_root.join("SKILL.md"),
                    relative_path: PathBuf::from("bundles/second/SKILL.md"),
                },
                crate::plugins::ComponentFile {
                    path: second_root.join("README.md"),
                    relative_path: PathBuf::from("bundles/second/README.md"),
                },
            ],
            agents: Vec::new(),
            hooks: None,
            unsupported_hooks: Vec::new(),
            mcp_servers: BTreeMap::new(),
            lsp_servers: Vec::new(),
        }],
    };

    let config =
        runtime_skill_config_from_snapshot(data_root.clone(), project_root.clone(), snapshot);

    assert_eq!(config.directories.data_directory, data_root);
    assert_eq!(config.directories.project_directory, project_root);
    assert_eq!(config.additional_roots.len(), 2);
    assert_eq!(config.additional_roots[0].path, second_root);
    assert_eq!(config.additional_roots[1].path, first_root);
    assert!(
        config
            .additional_roots
            .iter()
            .all(|root| { root.source == keencode_skills::SkillSource::Plugin && !root.recursive })
    );
}
