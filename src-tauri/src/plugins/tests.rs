//! KeenCode 原生插件模型的完整回归测试。

use super::*;

#[derive(Debug, Default)]
/// 可在指定调用点注入失败的测试密钥存储。
struct FaultInjectingSecretStore {
    /// 当前模拟保存的密钥值。
    values: BTreeMap<String, Value>,
    /// 第几次写入时返回故障。
    fail_set_call: Option<usize>,
    /// 第几次删除时返回故障。
    fail_delete_call: Option<usize>,
    /// 已执行的写入次数。
    set_calls: usize,
    /// 已执行的删除次数。
    delete_calls: usize,
    /// 用于在状态保存阶段注入目标路径故障的源与目标。
    state_save_failure: Option<(PathBuf, PathBuf)>,
    /// 仅 Unix 符号链接故障注入测试需要在读取密钥时破坏状态目标。
    #[cfg(unix)]
    state_save_failure_on_get: bool,
}

impl SecretStore for FaultInjectingSecretStore {
    /// 写入模拟密钥，并按配置在指定调用点失败。
    fn set_json(&mut self, key: &str, value: &Value) -> Result<()> {
        self.set_calls += 1;
        if self.fail_set_call == Some(self.set_calls) {
            return Err(PluginError::Invalid("injected secret failure".to_owned()));
        }
        self.values.insert(key.to_owned(), value.clone());
        self.maybe_fail_state_save();
        Ok(())
    }

    /// 读取模拟密钥，并在 Unix 专用测试中按需破坏状态目标。
    fn get_json(&self, key: &str) -> Result<Option<Value>> {
        #[cfg(unix)]
        if self.state_save_failure_on_get
            && let Some((state_path, target)) = &self.state_save_failure
            && let Ok(metadata) = fs::symlink_metadata(state_path)
            && !metadata.file_type().is_symlink()
            && !target.exists()
        {
            fs::rename(state_path, target.as_path()).unwrap();
            std::os::unix::fs::symlink(target, state_path).unwrap();
        }
        Ok(self.values.get(key).cloned())
    }

    /// 删除模拟密钥，并按配置在指定调用点失败。
    fn delete(&mut self, key: &str) -> Result<()> {
        self.delete_calls += 1;
        if self.fail_delete_call == Some(self.delete_calls) {
            return Err(PluginError::Invalid("injected secret failure".to_owned()));
        }
        self.values.remove(key);
        self.maybe_fail_state_save();
        Ok(())
    }
}

impl FaultInjectingSecretStore {
    /// 在状态提交即将发生时注入符号链接替换故障。
    fn maybe_fail_state_save(&mut self) {
        #[cfg(unix)]
        if let Some((state_path, target)) = self.state_save_failure.take()
            && let Ok(metadata) = fs::symlink_metadata(&state_path)
            && !metadata.file_type().is_symlink()
            && !target.exists()
        {
            fs::rename(&state_path, target.as_path()).unwrap();
            std::os::unix::fs::symlink(target, state_path).unwrap();
        }
        #[cfg(not(unix))]
        {
            self.state_save_failure = None;
        }
    }
}

/// 构造包含公开配置和两项敏感配置的已安装插件事务夹具。
fn transactional_plugin_fixture() -> (tempfile::TempDir, PluginManager, PluginId) {
    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(directory.path());
    let id = PluginId::parse("demo@official").unwrap();
    let install_path = manager
        .storage
        .versioned_path(
            &id,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
    fs::create_dir_all(install_path.join(".keencode-plugin")).unwrap();
    fs::write(
        install_path.join(PLUGIN_MANIFEST),
        br#"{
                "name":"demo",
                "version":"1",
                "userConfig":{
                    "first":{"type":"string","sensitive":true},
                    "second":{"type":"string","sensitive":true},
                    "endpoint":{"type":"string"}
                }
            }"#,
    )
    .unwrap();
    manager
        .save_state(&PluginState {
            plugins: vec![InstalledPlugin {
                id: id.clone(),
                version: "1".to_owned(),
                install_path,
                enabled: true,
                public_user_config: BTreeMap::from([(
                    "endpoint".to_owned(),
                    Value::String("old-endpoint".to_owned()),
                )]),
                sensitive_user_config_keys: BTreeSet::from([
                    "first".to_owned(),
                    "second".to_owned(),
                ]),
                secret_generation: 0,
            }],
        })
        .unwrap();
    (directory, manager, id)
}

/// 为事务夹具填充上一代敏感配置值。
fn seeded_transaction_store(manager: &PluginManager, id: &PluginId) -> FaultInjectingSecretStore {
    FaultInjectingSecretStore {
        values: BTreeMap::from([
            (
                manager.storage.secret_key(id, "first").unwrap(),
                Value::String("old-first".to_owned()),
            ),
            (
                manager.storage.secret_key(id, "second").unwrap(),
                Value::String("old-second".to_owned()),
            ),
        ]),
        ..Default::default()
    }
}

#[cfg(unix)]
/// 返回 Unix 状态保存故障注入使用的替代目标路径。
fn make_state_save_fail(manager: &PluginManager) -> PathBuf {
    manager
        .storage
        .state_path
        .with_file_name("state-target.json")
}

/// 写入一个包含指定文件条目的 MCPB 测试归档。
fn write_test_mcpb(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Write as _;

    let file = fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    for (name, bytes) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

/// 验证 `plugin@marketplace` ID 与无命名空间 ID 均可解析。
#[test]
fn parses_plugin_id() {
    assert_eq!(
        PluginId::parse("demo@official").unwrap().to_string(),
        "demo@official"
    );
    assert_eq!(PluginId::parse("demo").unwrap().to_string(), "demo");
    let object: PluginId = serde_json::from_str(r#"{"plugin":"demo","marketplace":"official"}"#)
        .expect("应读取当前状态中的插件 ID 对象");
    assert_eq!(object.to_string(), "demo@official");
    let string: PluginId =
        serde_json::from_str(r#""demo@official""#).expect("应读取插件 ID 字符串简写");
    assert_eq!(string, object);
    assert!(
        serde_json::from_str::<PluginId>(
            r#"{"plugin":"demo","marketplace":"official","legacy":true}"#
        )
        .is_err(),
        "插件 ID 对象不得接受当前 Schema 之外的字段"
    );
    assert_eq!(
        object
            .runtime_namespace()
            .expect("已绑定市场应有运行时命名空间"),
        "plugin:official:demo"
    );
    assert!(
        PluginId::parse("demo")
            .unwrap()
            .runtime_namespace()
            .is_err()
    );
}

/// 当前插件状态的唯一 Schema 不得把缺失配置字段静默默认成空值或零代际。
#[test]
fn installed_plugin_requires_all_persisted_configuration_fields() {
    let base = serde_json::json!({
        "id": "demo@official",
        "version": "1",
        "installPath": "C:/keencode/plugins/official/demo/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "enabled": true,
        "publicUserConfig": {},
        "sensitiveUserConfigKeys": [],
        "secretGeneration": 0
    });
    for field in [
        "publicUserConfig",
        "sensitiveUserConfigKeys",
        "secretGeneration",
    ] {
        let mut value = base.clone();
        value
            .as_object_mut()
            .expect("插件状态夹具必须是对象")
            .remove(field);
        assert!(
            serde_json::from_value::<InstalledPlugin>(value).is_err(),
            "缺少 {field} 时必须拒绝插件状态"
        );
    }
}

/// 缓存目录和密钥键按 ASCII 小写折叠，但插件 ID 展示仍保留原始大小写。
#[test]
fn plugin_storage_keys_are_case_stable() {
    let storage = PluginStorage::under("/tmp/keencode-plugin-plugin-keys");
    let upper = PluginId::parse("Demo@Official").unwrap();
    let lower = PluginId::parse("demo@official").unwrap();

    assert_eq!(upper.to_string(), "Demo@Official");
    assert_eq!(
        storage.versioned_path(&upper, "1.0.0").unwrap(),
        storage.versioned_path(&lower, "1.0.0").unwrap()
    );
    assert_eq!(
        storage.secret_key(&upper, "apiKey").unwrap(),
        storage.secret_key(&lower, "apiKey").unwrap()
    );
    assert!(
        storage
            .versioned_path(&upper, "1.0.0")
            .unwrap()
            .ends_with("official/demo/1.0.0")
    );
}

/// 密钥键名不能因市场、插件或字段中的点号而发生分隔符碰撞。
#[test]
fn secret_keys_encode_dotted_components_without_collision() {
    let storage = PluginStorage::under("/tmp/keencode-plugin-plugin-keys");
    let left = PluginId::parse("plugin.part@market").unwrap();
    let right = PluginId::parse("part@market.plugin").unwrap();
    let field_left = PluginId::parse("plugin@market").unwrap();
    let field_right = PluginId::parse("plugin.part@market").unwrap();

    assert_ne!(
        storage.secret_key(&left, "token").unwrap(),
        storage.secret_key(&right, "token").unwrap()
    );
    assert_ne!(
        storage.secret_key(&field_left, "part.token").unwrap(),
        storage.secret_key(&field_right, "token").unwrap()
    );
    // account 采用固定长度摘要，不随三个合法标识的最长长度线性膨胀。
    let key = storage.secret_key(&left, "token").unwrap();
    assert!(key.len() <= 128);
    assert!(key.starts_with("keencode.plugin.v1."));
}

/// 验证市场 source 的 GitHub 形式被转换为可审查 Git 计划。
#[test]
fn parses_marketplace_source() {
    let source: MarketplaceSource =
            serde_json::from_str(r#"{"source":"github","repo":"acme/plugins","ref":"v1","path":"repo/.keencode-plugin/marketplace.json","sparsePaths":["repo/.keencode-plugin","repo/plugins"]}"#)
                .unwrap();
    assert!(matches!(
        source,
        MarketplaceSource::Github {
            path: Some(_),
            sparse_paths,
            ..
        } if sparse_paths.len() == 2
    ));
    let source: MarketplaceSource =
        serde_json::from_str(r#"{"source":"github","repo":"acme/plugins","ref":"v1"}"#).unwrap();
    assert_eq!(
        source.fetch_plan(&EmptyMarketplaceSettings).unwrap(),
        SourceFetchPlan::Git {
            url: "https://github.com/acme/plugins.git".to_owned(),
            reference: Some("v1".to_owned()),
            sha: None,
            subdir: None,
        }
    );
}

/// KeenCode marketplace schema 允许暂时没有插件条目的市场清单。
#[test]
fn accepts_empty_marketplace_plugin_list() {
    let manifest = parse_marketplace_manifest(br#"{"name":"empty-market","plugins":[]}"#)
        .expect("空插件市场应符合 KeenCode 清单结构");
    assert!(manifest.plugins.is_empty());
}

/// 验证 MCPB/DXT 归档能安全解包并转换为 stdio MCP 配置。
#[test]
fn loads_mcpb_bundle() {
    use std::io::Write as _;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("keencode-mcpb-test-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let bundle = root.join("server.mcpb");
    let file = fs::File::create(&bundle).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    writer
        .start_file("manifest.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(br#"{"name":"demo","server":{"type":"node","entry_point":"server.js"}}"#)
        .unwrap();
    writer
        .start_file("server.js", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"console.log('ok')").unwrap();
    writer.finish().unwrap();

    let (extracted, manifest) = materialize_mcp_bundle(&root, "./server.mcpb").unwrap();
    let content_hash = mcp_bundle_content_cache_name(&fs::read(&bundle).unwrap());
    assert_eq!(
        fs::read(extracted.join(MCPB_COMPLETION_MARKER)).unwrap(),
        content_hash.as_bytes()
    );
    let servers = mcp_bundle_servers(&extracted, manifest).unwrap();
    let config = servers.get("demo").unwrap();
    assert_eq!(config.get("command").and_then(Value::as_str), Some("node"));
    assert!(
        config.get("args").and_then(Value::as_array).unwrap()[0]
            .as_str()
            .unwrap()
            .ends_with("server.js")
    );
    let _ = fs::remove_dir_all(root);
}

/// manifest 之后出现非法条目时，两次读取都必须失败，不能复用第一次的部分解包。
#[test]
fn failed_mcpb_extraction_never_becomes_completed_cache() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("server.mcpb");
    write_test_mcpb(
        &bundle,
        &[
            (
                "manifest.json",
                br#"{"name":"demo","server":{"type":"node","entry_point":"server.js"}}"#,
            ),
            ("../escape", b"blocked"),
        ],
    );
    let content_hash = mcp_bundle_content_cache_name(&fs::read(&bundle).unwrap());
    let extracted = directory
        .path()
        .join(".mcpb-cache/extracted")
        .join(content_hash);

    for _ in 0..2 {
        assert!(materialize_mcp_bundle(directory.path(), "./server.mcpb").is_err());
        assert!(!extracted.exists());
    }
    let extracted_root = directory.path().join(".mcpb-cache/extracted");
    assert!(fs::read_dir(extracted_root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".extracting-")
    }));
}

/// 插件提供的缓存根符号链接不能把下载或解包写到插件根目录外。
#[cfg(unix)]
#[test]
fn mcpb_cache_root_rejects_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("plugin");
    let outside = directory.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    write_test_mcpb(
        &root.join("server.mcpb"),
        &[(
            "manifest.json",
            br#"{"name":"demo","server":{"type":"node","entry_point":"server.js"}}"#,
        )],
    );
    symlink(&outside, root.join(".mcpb-cache")).unwrap();

    let error = materialize_mcp_bundle(&root, "./server.mcpb").unwrap_err();
    assert!(error.to_string().contains("符号链接"));
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

/// 远程 MCPB/DXT 首次下载后按 URL SHA-256 缓存原始归档，后续读取不再访问网络。
#[test]
fn caches_remote_mcpb_by_url_sha256() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-mcpb-remote-cache-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();

    let mut archive_bytes = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut archive_bytes);
        writer
            .start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(br#"{"name":"demo","server":{"type":"node","entry_point":"server.js"}}"#)
            .unwrap();
        writer
            .start_file("server.js", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"console.log('ok')").unwrap();
        writer.finish().unwrap();
    }
    let archive_bytes = archive_bytes.into_inner();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let response_body = archive_bytes.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        )
        .unwrap();
        stream.write_all(&response_body).unwrap();
    });
    let url = format!("http://{address}/server.mcpb");

    let (extracted, _) = materialize_mcp_bundle(&root, &url).unwrap();
    server.join().unwrap();
    let cache_path = mcp_bundle_url_cache_path(&root, &url);
    assert!(cache_path.is_file());
    assert_eq!(fs::read(&cache_path).unwrap(), archive_bytes);

    // 删除解包结果，强制第二次调用从 URL 缓存重新读取；此时端口已关闭，
    // 若实现重新联网则测试会失败。
    fs::remove_dir_all(extracted).unwrap();
    let (reextracted, _) = materialize_mcp_bundle(&root, &url).unwrap();
    assert!(reextracted.join("manifest.json").is_file());

    fs::remove_dir_all(root).unwrap();
}

/// MCPB/DXT 的 user_config 应并入插件配置模型，敏感字段仍由同一 SecretStore 管道处理。
#[test]
fn merges_mcpb_user_config_into_plugin_manifest() {
    use std::io::Write as _;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-mcpb-config-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(root.join(".keencode-plugin")).unwrap();
    fs::write(
        root.join(".keencode-plugin/plugin.json"),
        br#"{"name":"bundle-plugin","mcpServers":["server.mcpb"]}"#,
    )
    .unwrap();
    let bundle = root.join("server.mcpb");
    let file = fs::File::create(&bundle).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    writer
        .start_file("manifest.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer
            .write_all(
                br#"{"name":"demo","user_config":{"token":{"type":"string","sensitive":true,"required":true},"port":{"type":"number","min":1,"max":65535}},"server":{"type":"node","entry_point":"server.js"}}"#,
            )
            .unwrap();
    writer
        .start_file("server.js", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"console.log('ok')").unwrap();
    writer.finish().unwrap();

    let manifest = load_plugin_manifest(&root).unwrap();
    assert!(manifest.user_config.get("token").unwrap().sensitive);
    assert_eq!(manifest.user_config.get("port").unwrap().min, Some(1.0));
    let _ = fs::remove_dir_all(root);
}

/// pip 来源生成结构化计划，版本使用 pip 的 `==` 语法而非 npm 的 `@`。
#[test]
fn pip_source_has_a_parameterized_fetch_plan() {
    let source: PluginSource =
        serde_json::from_str(r#"{"source":"pip","package":"acme-plugin","version":"1.2.3"}"#)
            .unwrap();
    assert_eq!(
        source.fetch_plan(Path::new("/tmp")).unwrap(),
        SourceFetchPlan::Pip {
            package_spec: "acme-plugin==1.2.3".to_owned(),
            registry: None,
        }
    );
}

/// npm/pip source 的私有 registry 必须进入结构化取得计划，不能在解析时丢失。
#[test]
fn preserves_package_registry_in_fetch_plans() {
    let npm: PluginSource = serde_json::from_str(
            r#"{"source":"npm","package":"@acme/plugin","version":"1.0.0","registry":"https://npm.acme.test/"}"#,
        )
        .unwrap();
    assert_eq!(
        npm.fetch_plan(Path::new("/tmp")).unwrap(),
        SourceFetchPlan::Npm {
            package_spec: "@acme/plugin@1.0.0".to_owned(),
            registry: Some("https://npm.acme.test/".to_owned()),
        }
    );
    let pip: PluginSource = serde_json::from_str(
        r#"{"source":"pip","package":"acme-plugin","registry":"https://pypi.acme.test/simple"}"#,
    )
    .unwrap();
    assert_eq!(
        pip.fetch_plan(Path::new("/tmp")).unwrap(),
        SourceFetchPlan::Pip {
            package_spec: "acme-plugin".to_owned(),
            registry: Some("https://pypi.acme.test/simple".to_owned()),
        }
    );
}

/// 变量只能使用字母、数字和下划线，且缺失值为硬错误。
#[test]
fn interpolates_variables() {
    let variables = BTreeMap::from([("NAME".to_owned(), "KeenCode".to_owned())]);
    assert_eq!(
        interpolate_variables("hello ${NAME}", &variables).unwrap(),
        "hello KeenCode"
    );
    assert!(matches!(
        interpolate_variables("${MISSING}", &variables),
        Err(PluginError::MissingVariable(_))
    ));
}

/// 缺省 version、mcpServers 文件/数组形式和 directory 多选配置均可解析。
#[test]
fn parses_current_keencode_manifest_variants() {
    let manifest = parse_plugin_manifest(
            br#"{
                "name":"demo",
                "mcpServers":["./mcp.json", {"name":"inline","command":"echo"}],
                "userConfig":{"paths":{"type":"directory","title":"Paths","multiple":true,"min":1,"max":20}}
            }"#,
        )
        .unwrap();
    assert_eq!(manifest.version, None);
    assert_eq!(manifest.mcp_servers.files, vec!["./mcp.json"]);
    assert!(manifest.mcp_servers.inline.contains_key("inline"));
    let definition = manifest.user_config.get("paths").unwrap();
    validate_user_config_value(
        "paths",
        definition,
        &Value::Array(vec![Value::String("/tmp/project".to_owned())]),
    )
    .unwrap();
}

/// lspServers 接受 KeenCode 当前运行时字段，并保留未知公开元数据。
#[test]
fn parses_complete_lsp_server_contract() {
    let manifest = parse_plugin_manifest(
        br#"{
                "name":"demo",
                "lspServers":[{
                    "name":"rust-analyzer",
                    "command":"rust-analyzer",
                    "args":["--stdio"],
                    "env":{"RUST_LOG":"info"},
                    "extensionToLanguage":{".rs":"rust"},
                    "initializationOptions":{"cargo":{"allFeatures":true}},
                    "disabled":false,
                    "maxRestarts":5,
                    "startupTimeout":120000,
                    "futureField":{"mode":"auto"}
                }]
            }"#,
    )
    .unwrap();
    assert_eq!(manifest.lsp_servers.len(), 1);
    let server = &manifest.lsp_servers[0];
    assert_eq!(server.name, "rust-analyzer");
    assert_eq!(server.env.get("RUST_LOG").map(String::as_str), Some("info"));
    assert_eq!(server.disabled, Some(false));
    assert_eq!(server.max_restarts, Some(5));
    assert_eq!(server.startup_timeout, Some(120_000));
    assert_eq!(
        server.extra.get("futureField"),
        Some(&serde_json::json!({"mode":"auto"}))
    );

    assert!(
        parse_plugin_manifest(
            br#"{"name":"demo","lspServers":{"rust":{"command":"rust-analyzer"}}}"#
        )
        .is_err()
    );
    assert!(parse_plugin_manifest(
            br#"{"name":"demo","lspServers":[{"name":"rust","command":"one"},{"name":"rust","command":"two"}]}"#
        )
        .is_err());
}

/// 运行时的 `KEENCODE_PLUGIN_DATA` 必须指向插件根目录下的 `data`。
#[test]
fn uses_plugin_data_directory_without_hidden_dot_prefix() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-plugin-data-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(root.join(".keencode-plugin")).unwrap();
    fs::write(
        root.join(".keencode-plugin/plugin.json"),
        br#"{
                "name":"demo",
                "lspServers":[{
                    "name":"server",
                    "command":"${KEENCODE_PLUGIN_DATA}/bin/server"
                }]
            }"#,
    )
    .unwrap();
    let manifest = load_plugin_manifest(&root).unwrap();
    let runtime = extract_components(
        PluginId::parse("demo@official").unwrap(),
        &root,
        &manifest,
        Path::new("."),
        &BTreeMap::new(),
        &ResolvedUserConfig::default(),
    )
    .unwrap();
    assert_eq!(
        runtime.lsp_servers[0].command,
        format!(
            "{}/data/bin/server",
            path_to_frontend(&root.canonicalize().unwrap())
        )
    );
    assert!(!runtime.lsp_servers[0].command.contains("/.data/"));
    fs::remove_dir_all(root).unwrap();
}

/// 插件 LSP 在加载期展开规范项目根，不采纳调用环境中的陈旧项目路径。
#[test]
fn expands_project_scoped_plugin_lsp_variables_when_loaded() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-lsp-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(root.join(".keencode-plugin")).unwrap();
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        root.join(".keencode-plugin/plugin.json"),
        br#"{
                "name":"demo",
                "lspServers":[{
                    "name":"rust",
                    "command":"${KEENCODE_PLUGIN_ROOT}/bin/server",
                    "args":["--project","${KEENCODE_PROJECT_DIR}","${user_config.channel}"],
                    "env":{
                        "PLUGIN_CACHE":"${KEENCODE_PLUGIN_ROOT}/cache",
                        "PROJECT_CACHE":"${KEENCODE_PROJECT_DIR}/cache"
                    },
                    "extensionToLanguage":{"rs":"rust"},
                    "initializationOptions":{
                        "project":"${KEENCODE_PROJECT_DIR}"
                    },
                    "disabled":false,
                    "maxRestarts":7,
                    "startupTimeout":120000
                }]
            }"#,
    )
    .unwrap();
    let manifest = load_plugin_manifest(&root).unwrap();
    let runtime = extract_components(
        PluginId::parse("demo@local").unwrap(),
        &root,
        &manifest,
        &project,
        &BTreeMap::from([
            (
                "KEENCODE_PROJECT_DIR".to_owned(),
                "/stale/project".to_owned(),
            ),
            ("KEENCODE_SESSION_ID".to_owned(), "stale-session".to_owned()),
        ]),
        &ResolvedUserConfig {
            values: BTreeMap::from([("channel".to_owned(), Value::String("stable".to_owned()))]),
            missing_sensitive: BTreeSet::new(),
        },
    )
    .unwrap();

    let canonical_root = root.canonicalize().unwrap();
    let canonical_frontend = path_to_frontend(&canonical_root);
    let canonical_project = path_to_frontend(&project.canonicalize().unwrap());
    let server = runtime.lsp_servers.first().unwrap();
    assert_eq!(server.name, "plugin:local:demo:rust");
    assert_eq!(server.command, format!("{canonical_frontend}/bin/server"));
    assert_eq!(server.args[1], canonical_project);
    assert_eq!(server.args[2], "stable");
    assert_eq!(
        server
            .environment
            .get("KEENCODE_PLUGIN_ROOT")
            .map(String::as_str),
        Some(canonical_frontend.as_str())
    );
    assert_eq!(
        server.environment.get("PLUGIN_CACHE").map(String::as_str),
        Some(format!("{canonical_frontend}/cache").as_str())
    );
    assert_eq!(
        server.environment.get("PROJECT_CACHE").map(String::as_str),
        Some(format!("{canonical_project}/cache").as_str())
    );
    assert!(!server.environment.contains_key("KEENCODE_SESSION_ID"));
    assert_eq!(
        server.extension_to_language.get("rs"),
        Some(&"rust".to_owned())
    );
    assert_eq!(
        server.initialization_options,
        Some(serde_json::json!({
            "project": canonical_project
        }))
    );
    assert!(!server.disabled);
    assert_eq!(server.max_restarts, 7);
    assert_eq!(server.startup_timeout_ms, 120_000);
    fs::remove_dir_all(root).unwrap();
}

/// 项目级共享 LSP 必须拒绝命令、参数、环境和初始化选项中的 Session 绑定。
#[test]
fn rejects_session_scoped_plugin_lsp_configuration() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-lsp-session-test-{}-{nonce}",
        std::process::id()
    ));
    let project = root.join("project");
    fs::create_dir_all(root.join(".keencode-plugin")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let cases = [
        (
            "命令",
            serde_json::json!({
                "name": "demo",
                "lspServers": [{"name": "rust", "command": "${KEENCODE_SESSION_ID}"}]
            }),
        ),
        (
            "参数",
            serde_json::json!({
                "name": "demo",
                "lspServers": [{
                    "name": "rust",
                    "command": "server",
                    "args": ["${KEENCODE_SESSION_ID}"]
                }]
            }),
        ),
        (
            "环境值",
            serde_json::json!({
                "name": "demo",
                "lspServers": [{
                    "name": "rust",
                    "command": "server",
                    "env": {"CACHE": "${KEENCODE_SESSION_ID}"}
                }]
            }),
        ),
        (
            "初始化选项",
            serde_json::json!({
                "name": "demo",
                "lspServers": [{
                    "name": "rust",
                    "command": "server",
                    "initializationOptions": {"session": "${KEENCODE_SESSION_ID}"}
                }]
            }),
        ),
        (
            "环境键",
            serde_json::json!({
                "name": "demo",
                "lspServers": [{
                    "name": "rust",
                    "command": "server",
                    "env": {"KEENCODE_SESSION_ID": "forged-session"}
                }]
            }),
        ),
        (
            "环境键变量",
            serde_json::json!({
                "name": "demo",
                "lspServers": [{
                    "name": "rust",
                    "command": "server",
                    "env": {"CACHE_${KEENCODE_SESSION_ID}": "cache"}
                }]
            }),
        ),
    ];
    for (location, document) in cases {
        fs::write(
            root.join(".keencode-plugin/plugin.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let manifest = load_plugin_manifest(&root).unwrap();
        let error = extract_components(
            PluginId::parse("demo@local").unwrap(),
            &root,
            &manifest,
            &project,
            &BTreeMap::from([(
                "KEENCODE_SESSION_ID".to_owned(),
                "ambient-session".to_owned(),
            )]),
            &ResolvedUserConfig::default(),
        )
        .expect_err("项目级 LSP 不得接受 Session 变量");
        let PluginError::Invalid(message) = error else {
            panic!("{location} 应返回明确的配置错误，实际为：{error}");
        };
        assert!(message.contains("按项目共享"), "{location}: {message}");
        assert!(
            message.contains("KEENCODE_SESSION_ID"),
            "{location}: {message}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

/// 未声明 inline hooks 时，默认加载插件根目录内的 hooks/hooks.json。
#[test]
fn loads_default_hooks_file() {
    let root = std::env::temp_dir().join(format!("keencode-plugin-hooks-{}", std::process::id()));
    fs::create_dir_all(root.join("hooks")).unwrap();
    fs::write(
        root.join("hooks/hooks.json"),
        br#"{"PostToolUse":[{"command":"echo ${KEENCODE_PLUGIN_ROOT}"}]}"#,
    )
    .unwrap();
    let variables =
        BTreeMap::from([("KEENCODE_PLUGIN_ROOT".to_owned(), "/safe/plugin".to_owned())]);
    let hooks = load_hooks(&root, None, &variables).unwrap().unwrap();
    assert!(hooks.to_string().contains("/safe/plugin"));
    fs::remove_dir_all(root).unwrap();
}

/// 配置名称将被归一为合法的 `KEENCODE_PLUGIN_*` 变量命名空间。
#[test]
fn normalizes_plugin_variable_namespace() {
    assert_eq!(normalize_variable_name("api.key-name"), "API_KEY_NAME");
    assert_eq!(
        config_value_as_variable(&Value::Array(vec![
            Value::String("one".to_owned()),
            Value::String("two".to_owned()),
        ])),
        Some("one,two".to_owned())
    );
}

/// 公开状态连续保存必须替换既有文件；Unix 上权限必须为 0600。
#[test]
fn saves_state_with_private_permissions() {
    let root = std::env::temp_dir().join(format!("keencode-plugin-state-{}", std::process::id()));
    let manager = PluginManager::new(&root);
    manager.save_state(&PluginState::default()).unwrap();
    manager.save_state(&PluginState::default()).unwrap();
    let persisted: Value = serde_json::from_slice(&fs::read(&manager.storage.state_path).unwrap())
        .expect("插件状态应使用严格版本外壳");
    assert_eq!(persisted["schema"], PLUGIN_STATE_SCHEMA);
    assert_eq!(persisted["version"], PLUGIN_STATE_VERSION);
    assert_eq!(persisted["state"]["plugins"], Value::Array(Vec::new()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&manager.storage.state_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    fs::remove_dir_all(root).unwrap();
}

/// 插件状态只接受当前版本外壳；旧平面格式、未知字段和错误版本都必须失败。
#[test]
fn plugin_state_rejects_non_current_schema() {
    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(directory.path());
    manager.storage.ensure_directories().unwrap();
    let cases = [
        br#"{"plugins":[]}"#.as_slice(),
        br#"{"schema":"keencode/plugin-state","version":1,"state":{"plugins":[]},"extra":true}"#,
        br#"{"schema":"keencode/plugin-state","version":1,"state":{"plugins":[],"extra":true}}"#,
        br#"{"schema":"keencode/plugin-state","version":2,"state":{"plugins":[]}}"#,
        br#"{"schema":"keencode/plugin-state","version":1}"#,
        br#"not-json"#,
    ];
    for (index, bytes) in cases.into_iter().enumerate() {
        fs::write(&manager.storage.state_path, bytes).unwrap();
        assert!(
            manager.load_state().is_err(),
            "非当前插件状态格式 {index} 不应被静默接受"
        );
    }
}

/// 状态替换失败时必须保留目标并清理独占创建的临时文件。
#[test]
fn failed_state_save_cleans_temporary_file() {
    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(directory.path());
    fs::create_dir_all(&manager.storage.state_path).unwrap();

    assert!(manager.save_state(&PluginState::default()).is_err());
    assert!(manager.storage.state_path.is_dir());
    let entries = fs::read_dir(manager.storage.state_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<BTreeSet<_>>();
    assert_eq!(entries.len(), 2, "应仅保留 cache 目录和原状态目标目录");
    assert!(entries.contains(std::ffi::OsStr::new("cache")));
    assert!(entries.contains(std::ffi::OsStr::new("state.json")));
}

/// 依赖闭包应该把依赖排在目标插件之前。
#[test]
fn resolves_dependency_closure() {
    let marketplace = parse_marketplace_manifest(
            br#"{"name":"official","plugins":[{"name":"a","source":"./a","dependencies":{"b":"^1"}},{"name":"b","source":"./b"}]}"#,
        )
        .unwrap();
    let manifests = BTreeMap::from([
        (
            "a".to_owned(),
            parse_plugin_manifest(br#"{"name":"a","version":"1"}"#).unwrap(),
        ),
        (
            "b".to_owned(),
            parse_plugin_manifest(br#"{"name":"b","version":"1"}"#).unwrap(),
        ),
    ]);
    assert_eq!(
        dependency_closure(
            &PluginId::parse("a@official").unwrap(),
            &marketplace,
            &manifests
        )
        .unwrap()
        .into_iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>(),
        vec!["b@official", "a@official"]
    );
}

/// 市场插件名称按 ASCII 折叠去重，避免大小写差异生成两个逻辑插件。
#[test]
fn rejects_case_insensitive_marketplace_plugin_duplicates() {
    let error = parse_marketplace_manifest(
        br#"{
                "name":"official",
                "plugins":[
                    {"name":"Demo","source":"./demo"},
                    {"name":"demo","source":"./demo-lower"}
                ]
            }"#,
    )
    .expect_err("大小写不同的重复插件必须失败");
    assert!(error.to_string().contains("忽略大小写"));
}

/// 依赖解析按折叠键查找，并把结果投影回市场清单中的 canonical 名称。
#[test]
fn canonicalizes_case_insensitive_dependency_ids() {
    let marketplace = parse_marketplace_manifest(
        br#"{
                "name":"official",
                "plugins":[
                    {"name":"Demo","source":"./demo","dependencies":{"UTIL":"*"}},
                    {"name":"Util","source":"./util"}
                ]
            }"#,
    )
    .unwrap();
    let manifests = BTreeMap::from([
        (
            "Demo".to_owned(),
            parse_plugin_manifest(br#"{"name":"Demo","version":"1"}"#).unwrap(),
        ),
        (
            "Util".to_owned(),
            parse_plugin_manifest(br#"{"name":"Util","version":"1"}"#).unwrap(),
        ),
    ]);
    let order = dependency_closure(
        &PluginId::parse("demo@OFFICIAL").unwrap(),
        &marketplace,
        &manifests,
    )
    .unwrap();
    assert_eq!(
        order
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
        vec!["Util@official", "Demo@official"]
    );
}

/// 依赖闭包必须递归遍历多级依赖，并把最深层依赖排在最前面。
#[test]
fn resolves_multilevel_dependency_closure() {
    let marketplace = parse_marketplace_manifest(
        br#"{
                "name":"official",
                "plugins":[
                    {"name":"a","source":"./a","dependencies":{"b":"^2"}},
                    {"name":"b","source":"./b","dependencies":{"c":"~3"}},
                    {"name":"c","source":"./c","dependencies":{"d":">=4"}},
                    {"name":"d","source":"./d"}
                ]
            }"#,
    )
    .unwrap();
    let manifests = BTreeMap::from([
        (
            "a".to_owned(),
            parse_plugin_manifest(br#"{"name":"a","version":"1"}"#).unwrap(),
        ),
        (
            "b".to_owned(),
            parse_plugin_manifest(br#"{"name":"b","version":"1"}"#).unwrap(),
        ),
        (
            "c".to_owned(),
            parse_plugin_manifest(br#"{"name":"c","version":"1"}"#).unwrap(),
        ),
        (
            "d".to_owned(),
            parse_plugin_manifest(br#"{"name":"d","version":"1"}"#).unwrap(),
        ),
    ]);

    let order = dependency_closure(
        &PluginId::parse("a@official").unwrap(),
        &marketplace,
        &manifests,
    )
    .unwrap();
    assert_eq!(
        order
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
        vec!["d@official", "c@official", "b@official", "a@official"]
    );
}

/// 市场条目和 plugin.json 的依赖声明必须合并后共同参与拓扑解析。
#[test]
fn merges_marketplace_and_plugin_manifest_dependencies() {
    let marketplace = parse_marketplace_manifest(
        br#"{
                "name":"official",
                "plugins":[
                    {"name":"a","source":"./a","dependencies":{"b":"^1"}},
                    {"name":"b","source":"./b"},
                    {"name":"c","source":"./c"}
                ]
            }"#,
    )
    .unwrap();
    let manifests = BTreeMap::from([
        (
            "a".to_owned(),
            parse_plugin_manifest(br#"{"name":"a","version":"1","dependencies":{"c":"~2"}}"#)
                .unwrap(),
        ),
        (
            "b".to_owned(),
            parse_plugin_manifest(br#"{"name":"b","version":"1"}"#).unwrap(),
        ),
        (
            "c".to_owned(),
            parse_plugin_manifest(br#"{"name":"c","version":"1"}"#).unwrap(),
        ),
    ]);

    let order = dependency_closure(
        &PluginId::parse("a@official").unwrap(),
        &marketplace,
        &manifests,
    )
    .unwrap();
    assert_eq!(
        order
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
        vec!["b@official", "c@official", "a@official"]
    );
}

/// 同一依赖同时在两层声明时只安装一次，版本要求保留原始字符串而不做求解。
#[test]
fn deduplicates_dependencies_without_solving_versions() {
    let marketplace = parse_marketplace_manifest(
        br#"{
                "name":"official",
                "plugins":[
                    {"name":"a","source":"./a","dependencies":{"b":"^1.0"}},
                    {"name":"b","source":"./b"}
                ]
            }"#,
    )
    .unwrap();
    let a_manifest =
        parse_plugin_manifest(br#"{"name":"a","version":"1","dependencies":{"b":"~2.0"}}"#)
            .unwrap();
    assert_eq!(marketplace.plugins[0].dependencies["b"].0, "^1.0");
    assert_eq!(a_manifest.dependencies["b"].0, "~2.0");
    let manifests = BTreeMap::from([
        ("a".to_owned(), a_manifest),
        (
            "b".to_owned(),
            parse_plugin_manifest(br#"{"name":"b","version":"1"}"#).unwrap(),
        ),
    ]);

    let order = dependency_closure(
        &PluginId::parse("a@official").unwrap(),
        &marketplace,
        &manifests,
    )
    .unwrap();
    assert_eq!(
        order
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
        vec!["b@official", "a@official"]
    );
}

/// 依赖显式指向另一个市场时必须拒绝当前市场的解析请求。
#[test]
fn rejects_cross_market_dependency() {
    let marketplace = parse_marketplace_manifest(
        br#"{
                "name":"official",
                "plugins":[
                    {"name":"a","source":"./a","dependencies":{"dep@other-market":"^1"}}
                ]
            }"#,
    )
    .unwrap();
    let manifests = BTreeMap::from([(
        "a".to_owned(),
        parse_plugin_manifest(br#"{"name":"a","version":"1"}"#).unwrap(),
    )]);
    let error = dependency_closure(
        &PluginId::parse("a@official").unwrap(),
        &marketplace,
        &manifests,
    )
    .expect_err("跨市场依赖必须失败");
    assert!(error.to_string().contains("跨市场依赖"));
}

/// 依赖指向市场不存在的插件时必须在安装前返回明确错误。
#[test]
fn rejects_missing_dependency() {
    let marketplace = parse_marketplace_manifest(
            br#"{"name":"official","plugins":[{"name":"a","source":"./a","dependencies":{"missing":"*"}}]}"#,
        )
        .unwrap();
    let manifests = BTreeMap::from([(
        "a".to_owned(),
        parse_plugin_manifest(br#"{"name":"a","version":"1"}"#).unwrap(),
    )]);
    let error = dependency_closure(
        &PluginId::parse("a@official").unwrap(),
        &marketplace,
        &manifests,
    )
    .expect_err("缺失依赖必须失败");
    assert!(error.to_string().contains("找不到依赖 missing@official"));
}

/// 循环依赖不能导致栈溢出，必须返回完整循环路径。
#[test]
fn detects_dependency_cycle() {
    let marketplace = parse_marketplace_manifest(
            br#"{"name":"official","plugins":[{"name":"a","source":"./a","dependencies":{"b":"*"}},{"name":"b","source":"./b","dependencies":{"a":"*"}}]}"#,
        )
        .unwrap();
    let manifests = BTreeMap::from([
        (
            "a".to_owned(),
            parse_plugin_manifest(br#"{"name":"a","version":"1"}"#).unwrap(),
        ),
        (
            "b".to_owned(),
            parse_plugin_manifest(br#"{"name":"b","version":"1"}"#).unwrap(),
        ),
    ]);
    assert!(matches!(
        dependency_closure(
            &PluginId::parse("a@official").unwrap(),
            &marketplace,
            &manifests
        ),
        Err(PluginError::DependencyCycle(_))
    ));
}

/// 批量安装必须先校验整组清单，后续清单失败时不能留下前一个插件的状态或缓存。
#[test]
fn batch_install_prevalidates_all_manifests_before_writing() {
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-batch-install-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let first = root.join("first");
    let second = root.join("second");
    fs::create_dir_all(first.join(".keencode-plugin")).unwrap();
    fs::create_dir_all(second.join(".keencode-plugin")).unwrap();
    fs::write(
        first.join(".keencode-plugin/plugin.json"),
        br#"{"name":"first","version":"1"}"#,
    )
    .unwrap();
    fs::write(
        second.join(".keencode-plugin/plugin.json"),
        br#"{"name":"different","version":"1"}"#,
    )
    .unwrap();

    let manager = PluginManager::new(&root);
    let mut secrets = InMemorySecretStore::default();
    let error = manager
        .install_from_directories(
            vec![
                MaterializedPlugin {
                    id: PluginId::parse("first@official").unwrap(),
                    source_root: first,
                },
                MaterializedPlugin {
                    id: PluginId::parse("second@official").unwrap(),
                    source_root: second,
                },
            ],
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .expect_err("清单名称不一致时整批安装必须失败");
    assert!(error.to_string().contains("plugin.json name"));
    assert!(manager.load_state().unwrap().plugins.is_empty());
    assert!(!manager.storage.state_path.exists());
    assert!(!manager.storage.cache_root.exists());
    fs::remove_dir_all(root).unwrap();
}

/// 批量安装成功后状态按稳定 ID 排序，重复安装不增加记录也不改变缓存路径。
#[test]
fn batch_install_is_sorted_and_idempotent() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-batch-idempotent-{}-{nonce}",
        std::process::id()
    ));
    let alpha = root.join("sources/alpha");
    let zeta = root.join("sources/zeta");
    fs::create_dir_all(alpha.join(".keencode-plugin")).unwrap();
    fs::create_dir_all(zeta.join(".keencode-plugin")).unwrap();
    fs::write(
        alpha.join(".keencode-plugin/plugin.json"),
        br#"{"name":"alpha","version":"2.0.0"}"#,
    )
    .unwrap();
    fs::write(
        zeta.join(".keencode-plugin/plugin.json"),
        br#"{"name":"zeta","version":"1.0.0"}"#,
    )
    .unwrap();

    let manager = PluginManager::new(&root);
    let materials = || {
        vec![
            MaterializedPlugin {
                id: PluginId::parse("zeta@official").unwrap(),
                source_root: zeta.clone(),
            },
            MaterializedPlugin {
                id: PluginId::parse("alpha@official").unwrap(),
                source_root: alpha.clone(),
            },
        ]
    };
    let mut secrets = InMemorySecretStore::default();
    manager
        .install_from_directories(materials(), UserConfigUpdate::default(), &mut secrets)
        .unwrap();
    let first = manager.load_state().unwrap();
    assert_eq!(
        first
            .plugins
            .iter()
            .map(|plugin| plugin.id.to_string())
            .collect::<Vec<_>>(),
        vec!["alpha@official", "zeta@official"]
    );
    let first_paths = first
        .plugins
        .iter()
        .map(|plugin| (plugin.id.to_string(), plugin.install_path.clone()))
        .collect::<Vec<_>>();

    manager
        .install_from_directories(materials(), UserConfigUpdate::default(), &mut secrets)
        .unwrap();
    let second = manager.load_state().unwrap();
    assert_eq!(second.plugins.len(), 2);
    let second_paths = second
        .plugins
        .iter()
        .map(|plugin| (plugin.id.to_string(), plugin.install_path.clone()))
        .collect::<Vec<_>>();
    assert_eq!(second_paths, first_paths);
    fs::remove_dir_all(root).unwrap();
}

/// 同一 manifest.version 的来源内容变化时必须生成新缓存并回收旧缓存。
#[test]
fn same_version_content_change_refreshes_cache() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-cache-fingerprint-{}-{nonce}",
        std::process::id()
    ));
    let source = root.join("source");
    fs::create_dir_all(source.join(".keencode-plugin")).unwrap();
    fs::write(
        source.join(".keencode-plugin/plugin.json"),
        br#"{"name":"demo","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(source.join("payload.txt"), b"before").unwrap();

    let manager = PluginManager::new(&root);
    let materialized = || MaterializedPlugin {
        id: PluginId::parse("demo@official").unwrap(),
        source_root: source.clone(),
    };
    let mut secrets = InMemorySecretStore::default();
    manager
        .install_from_directory(materialized(), UserConfigUpdate::default(), &mut secrets)
        .unwrap();
    let old_path = manager.load_state().unwrap().plugins[0]
        .install_path
        .clone();
    assert_eq!(fs::read(old_path.join("payload.txt")).unwrap(), b"before");

    fs::write(source.join("payload.txt"), b"after").unwrap();
    manager
        .install_from_directory(materialized(), UserConfigUpdate::default(), &mut secrets)
        .unwrap();
    let new_path = manager.load_state().unwrap().plugins[0]
        .install_path
        .clone();
    assert_ne!(new_path, old_path);
    assert!(!old_path.exists());
    assert_eq!(fs::read(new_path.join("payload.txt")).unwrap(), b"after");
    fs::remove_dir_all(root).unwrap();
}

/// 更新插件清单时必须保留非零密钥代际，并清理已删除、拒绝预填新增的敏感字段。
#[test]
fn reinstall_preserves_secret_generation_and_reconciles_sensitive_fields() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-secret-generation-install-{}-{nonce}",
        std::process::id()
    ));
    let source = root.join("source");
    fs::create_dir_all(source.join(".keencode-plugin")).unwrap();
    fs::write(
        source.join(".keencode-plugin/plugin.json"),
        br#"{
                "name":"demo",
                "version":"1",
                "userConfig":{
                    "first":{"type":"string","sensitive":true},
                    "second":{"type":"string","sensitive":true}
                }
            }"#,
    )
    .unwrap();

    let manager = PluginManager::new(&root);
    let id = PluginId::parse("demo@official").unwrap();
    let mut secrets = InMemorySecretStore::default();
    manager
        .install_from_directory(
            MaterializedPlugin {
                id: id.clone(),
                source_root: source.clone(),
            },
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .unwrap();
    manager
        .update_user_config(
            &id,
            UserConfigUpdate {
                values: BTreeMap::from([
                    ("first".to_owned(), Value::String("first-secret".to_owned())),
                    (
                        "second".to_owned(),
                        Value::String("second-secret".to_owned()),
                    ),
                ]),
                replace: false,
            },
            &mut secrets,
        )
        .unwrap();
    assert_eq!(
        manager.load_state().unwrap().plugins[0].secret_generation,
        1
    );

    // 新版清单删除 second 并新增 required sensitive 字段；安装更新不能把
    // second 继续写入公开状态，也不能凭空把 new 标记为已配置。
    fs::write(
        source.join(".keencode-plugin/plugin.json"),
        br#"{
                "name":"demo",
                "version":"2",
                "userConfig":{
                    "first":{"type":"string","sensitive":true},
                    "new":{"type":"string","sensitive":true,"required":true}
                }
            }"#,
    )
    .unwrap();
    manager
        .install_from_directory(
            MaterializedPlugin {
                id: id.clone(),
                source_root: source,
            },
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .unwrap();

    let state = manager.load_state().unwrap();
    assert_eq!(state.plugins[0].secret_generation, 1);
    assert_eq!(
        state.plugins[0].sensitive_user_config_keys,
        BTreeSet::from(["first".to_owned()])
    );
    assert!(!state.plugins[0].enabled);
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "first", 1).unwrap())
            .unwrap(),
        Some(Value::String("first-secret".to_owned()))
    );
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "second", 1).unwrap())
            .unwrap(),
        None
    );
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "new", 1).unwrap())
            .unwrap(),
        None
    );
    fs::remove_dir_all(root).unwrap();
}

/// 批量安装失败时必须保留此前成功安装的状态和缓存。
#[test]
fn failed_batch_install_preserves_existing_state_and_cache() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "keencode-plugin-batch-preserve-{}-{nonce}",
        std::process::id()
    ));
    let existing = root.join("sources/existing");
    let invalid = root.join("sources/invalid");
    fs::create_dir_all(existing.join(".keencode-plugin")).unwrap();
    fs::create_dir_all(invalid.join(".keencode-plugin")).unwrap();
    fs::write(
        existing.join(".keencode-plugin/plugin.json"),
        br#"{"name":"existing","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        invalid.join(".keencode-plugin/plugin.json"),
        br#"{"name":"different","version":"1.0.0"}"#,
    )
    .unwrap();

    let manager = PluginManager::new(&root);
    let existing_id = PluginId::parse("existing@official").unwrap();
    let mut secrets = InMemorySecretStore::default();
    manager
        .install_from_directory(
            MaterializedPlugin {
                id: existing_id.clone(),
                source_root: existing,
            },
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .unwrap();
    let state_before = manager.load_state().unwrap();
    let state_bytes_before = fs::read(&manager.storage.state_path).unwrap();
    let cache_before = state_before.plugins[0].install_path.clone();
    assert!(cache_before.is_dir());

    let error = manager
        .install_from_directories(
            vec![
                MaterializedPlugin {
                    id: existing_id.clone(),
                    source_root: root.join("sources/existing"),
                },
                MaterializedPlugin {
                    id: PluginId::parse("invalid@official").unwrap(),
                    source_root: invalid,
                },
            ],
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .expect_err("失败批量安装不应覆盖既有状态");
    assert!(error.to_string().contains("plugin.json name"));
    assert_eq!(
        fs::read(&manager.storage.state_path).unwrap(),
        state_bytes_before
    );
    assert!(cache_before.is_dir());
    let state_after = manager.load_state().unwrap();
    assert_eq!(state_after.plugins.len(), 1);
    assert_eq!(state_after.plugins[0].id, existing_id);
    assert_eq!(state_after.plugins[0].install_path, cache_before);
    fs::remove_dir_all(root).unwrap();
}

/// 插件目录中的符号链接即使指向根内也必须拒绝，避免目录环导致无限递归。
#[cfg(unix)]
#[test]
fn install_rejects_internal_directory_symlink_cycle() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir_all(source.join(".keencode-plugin")).unwrap();
    fs::write(
        source.join(PLUGIN_MANIFEST),
        br#"{"name":"demo","version":"1"}"#,
    )
    .unwrap();
    symlink(".", source.join("loop")).unwrap();
    let manager = PluginManager::new(directory.path().join("data"));
    let mut secrets = InMemorySecretStore::default();

    let error = manager
        .install_from_directory(
            MaterializedPlugin {
                id: PluginId::parse("demo@official").unwrap(),
                source_root: source,
            },
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .unwrap_err();
    assert!(error.to_string().contains("符号链接目标必须是普通文件"));
    assert!(manager.load_state().unwrap().plugins.is_empty());
}

/// 插件可用根内文件链接共享 AGENTS.md；安装后应解引用为普通文件。
#[cfg(unix)]
#[test]
fn install_materializes_internal_file_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir_all(source.join(".keencode-plugin")).unwrap();
    fs::write(
        source.join(PLUGIN_MANIFEST),
        br#"{"name":"demo","version":"1"}"#,
    )
    .unwrap();
    fs::write(source.join("SHARED.md"), "shared instructions").unwrap();
    symlink("SHARED.md", source.join("AGENTS.md")).unwrap();
    let manager = PluginManager::new(directory.path().join("data"));
    let mut secrets = InMemorySecretStore::default();

    manager
        .install_from_directory(
            MaterializedPlugin {
                id: PluginId::parse("demo@official").unwrap(),
                source_root: source,
            },
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .unwrap();
    let installed = manager
        .load_state()
        .unwrap()
        .plugins
        .into_iter()
        .next()
        .unwrap();
    let agents = installed.install_path.join("AGENTS.md");
    assert_eq!(fs::read_to_string(&agents).unwrap(), "shared instructions");
    assert!(
        !fs::symlink_metadata(agents)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// 状态文件本身不能通过符号链接读取其他文件。
#[cfg(unix)]
#[test]
fn load_state_rejects_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(directory.path());
    manager.storage.ensure_directories().unwrap();
    let target = directory.path().join("external-state.json");
    fs::write(&target, br#"{"plugins":[]}"#).unwrap();
    symlink(&target, &manager.storage.state_path).unwrap();

    let error = manager.load_state().unwrap_err();
    assert!(error.to_string().contains("符号链接插件状态文件"));
}

/// 空状态也必须检查受控插件根目录，不能因为 state/cache 尚不存在就跟随根符号链接。
#[cfg(unix)]
#[test]
fn empty_state_rejects_plugin_root_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let data_root = directory.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    let external = directory.path().join("external");
    fs::create_dir(&external).unwrap();
    let plugin_root = data_root.join("plugins");
    symlink(&external, &plugin_root).unwrap();
    let manager = PluginManager::new(&data_root);

    assert!(manager.load_state().is_err());
    assert!(manager.save_state(&PluginState::default()).is_err());
    assert!(!external.join("cache").exists());
}

/// 受控数据根的中间父级符号链接同样必须拒绝，不能只检查直接父目录。
#[cfg(unix)]
#[test]
fn intermediate_data_parent_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base");
    let external = directory.path().join("external");
    fs::create_dir_all(&base).unwrap();
    fs::create_dir(&external).unwrap();
    symlink(&external, base.join("link")).unwrap();
    let data_root = base.join("link/data");
    let manager = PluginManager::new(&data_root);

    assert!(manager.storage.ensure_directories().is_err());
    assert!(!external.join("data/plugins").exists());
}

/// 即使状态为空，缓存根目录符号链接也不能被创建目录或状态校验接受。
#[cfg(unix)]
#[test]
fn empty_state_rejects_cache_root_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(directory.path());
    let root = manager.storage.cache_root.parent().unwrap();
    fs::create_dir_all(root).unwrap();
    let external = directory.path().join("external-cache");
    fs::create_dir(&external).unwrap();
    symlink(&external, &manager.storage.cache_root).unwrap();

    assert!(manager.load_state().is_err());
    assert!(manager.save_state(&PluginState::default()).is_err());
    assert!(external.read_dir().unwrap().next().is_none());
}

/// 缓存内的市场层/插件层符号链接也不能把复制或清理操作导向受控根外部。
#[cfg(unix)]
#[test]
fn cache_parent_symlink_is_rejected_before_copy() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir_all(source.join(".keencode-plugin")).unwrap();
    fs::write(
        source.join(PLUGIN_MANIFEST),
        br#"{"name":"demo","version":"1"}"#,
    )
    .unwrap();
    let manager = PluginManager::new(directory.path().join("data"));
    manager.storage.ensure_directories().unwrap();
    let external = directory.path().join("external-market");
    fs::create_dir(&external).unwrap();
    symlink(&external, manager.storage.cache_root.join("official")).unwrap();
    let mut secrets = InMemorySecretStore::default();

    let error = manager
        .install_from_directory(
            MaterializedPlugin {
                id: PluginId::parse("demo@official").unwrap(),
                source_root: source,
            },
            UserConfigUpdate::default(),
            &mut secrets,
        )
        .unwrap_err();
    assert!(error.to_string().contains("符号链接"));
    assert!(external.read_dir().unwrap().next().is_none());
}

/// 越界安装记录必须失败关闭，不能被静默降级为空状态。
#[test]
fn load_state_rejects_install_path_outside_cache_without_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(directory.path());
    manager.storage.ensure_directories().unwrap();
    let fingerprint = "a".repeat(64);
    let outside = directory.path().join("outside").join(&fingerprint);
    fs::create_dir_all(&outside).unwrap();
    let state = PluginState {
        plugins: vec![InstalledPlugin {
            id: PluginId::parse("demo@official").unwrap(),
            version: "1".to_owned(),
            install_path: outside,
            enabled: true,
            public_user_config: BTreeMap::new(),
            sensitive_user_config_keys: BTreeSet::new(),
            secret_generation: 0,
        }],
    };
    let original = serde_json::to_vec(&PluginStateFile::from_state(&state)).unwrap();
    fs::write(&manager.storage.state_path, &original).unwrap();

    let error = manager.load_state().unwrap_err();
    assert!(error.to_string().contains("安装路径必须是当前缓存根目录"));
    assert_eq!(fs::read(&manager.storage.state_path).unwrap(), original);
}

/// userConfig 校验失败时不得读取、写入或删除任何密钥。
#[test]
fn user_config_validation_failure_does_not_touch_secret_store() {
    let (_directory, manager, id) = transactional_plugin_fixture();
    let mut secrets = seeded_transaction_store(&manager, &id);
    let values_before = secrets.values.clone();
    let state_before = fs::read(&manager.storage.state_path).unwrap();

    let error = manager
        .update_user_config(
            &id,
            UserConfigUpdate {
                values: BTreeMap::from([("endpoint".to_owned(), Value::Bool(true))]),
                replace: true,
            },
            &mut secrets,
        )
        .expect_err("无效 userConfig 必须在写密钥前失败");

    assert!(error.to_string().contains("userConfig endpoint"));
    assert_eq!(secrets.values, values_before);
    assert_eq!(secrets.set_calls, 0);
    assert_eq!(secrets.delete_calls, 0);
    assert_eq!(fs::read(&manager.storage.state_path).unwrap(), state_before);
}

/// SecretStore 中途失败时，已经应用和失败项都必须恢复为旧值。
#[test]
fn partial_secret_failure_rolls_back_all_applied_changes() {
    let (_directory, manager, id) = transactional_plugin_fixture();
    let mut secrets = seeded_transaction_store(&manager, &id);
    secrets.fail_set_call = Some(2);
    let values_before = secrets.values.clone();
    let state_before = fs::read(&manager.storage.state_path).unwrap();

    let error = manager
        .update_user_config(
            &id,
            UserConfigUpdate {
                values: BTreeMap::from([
                    (
                        "first".to_owned(),
                        Value::String("new-first-secret".to_owned()),
                    ),
                    (
                        "second".to_owned(),
                        Value::String("new-second-secret".to_owned()),
                    ),
                ]),
                replace: false,
            },
            &mut secrets,
        )
        .expect_err("第二个密钥操作失败时事务必须失败");

    assert!(error.to_string().contains("已回滚密钥"));
    assert!(!error.to_string().contains("new-first-secret"));
    assert!(!error.to_string().contains("new-second-secret"));
    assert_eq!(secrets.values, values_before);
    assert_eq!(fs::read(&manager.storage.state_path).unwrap(), state_before);
}

/// 公开状态保存失败时，配置密钥必须全部回滚。
#[cfg(unix)]
#[test]
fn state_save_failure_rolls_back_secret_changes() {
    let (_directory, manager, id) = transactional_plugin_fixture();
    let mut secrets = seeded_transaction_store(&manager, &id);
    let values_before = secrets.values.clone();
    let state_before = fs::read(&manager.storage.state_path).unwrap();
    let state_target = make_state_save_fail(&manager);
    secrets.state_save_failure = Some((manager.storage.state_path.clone(), state_target.clone()));

    let error = manager
        .update_user_config(
            &id,
            UserConfigUpdate {
                values: BTreeMap::from([(
                    "first".to_owned(),
                    Value::String("new-first-secret".to_owned()),
                )]),
                replace: false,
            },
            &mut secrets,
        )
        .expect_err("状态目标为符号链接时保存必须失败");

    assert!(error.to_string().contains("公开状态保存失败"));
    assert!(error.to_string().contains("已回滚新代际密钥"));
    assert_eq!(secrets.values, values_before);
    assert_eq!(fs::read(&state_target).unwrap(), state_before);
}

/// 新代际完整写入后再切换 state 指针；旧代际只在提交后清理。
#[test]
fn successful_secret_update_switches_generation_atomically() {
    let (_directory, manager, id) = transactional_plugin_fixture();
    let mut secrets = seeded_transaction_store(&manager, &id);

    manager
        .update_user_config(
            &id,
            UserConfigUpdate {
                values: BTreeMap::from([(
                    "first".to_owned(),
                    Value::String("new-first-secret".to_owned()),
                )]),
                replace: false,
            },
            &mut secrets,
        )
        .unwrap();

    let state = manager.load_state().unwrap();
    assert_eq!(state.plugins[0].secret_generation, 1);
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "first", 1).unwrap())
            .unwrap(),
        Some(Value::String("new-first-secret".to_owned()))
    );
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "second", 1).unwrap())
            .unwrap(),
        Some(Value::String("old-second".to_owned()))
    );
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "first", 0).unwrap())
            .unwrap(),
        None
    );
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "second", 0).unwrap())
            .unwrap(),
        None
    );
}

/// 旧代际清理失败不能回滚已经提交的新 state；失败只允许留下安全孤儿。
#[test]
fn old_secret_generation_cleanup_failure_keeps_new_state() {
    let (_directory, manager, id) = transactional_plugin_fixture();
    let mut secrets = seeded_transaction_store(&manager, &id);
    secrets.fail_delete_call = Some(1);

    manager
        .update_user_config(
            &id,
            UserConfigUpdate {
                values: BTreeMap::from([(
                    "first".to_owned(),
                    Value::String("new-first-secret".to_owned()),
                )]),
                replace: false,
            },
            &mut secrets,
        )
        .unwrap();

    assert_eq!(
        manager.load_state().unwrap().plugins[0].secret_generation,
        1
    );
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "first", 1).unwrap())
            .unwrap(),
        Some(Value::String("new-first-secret".to_owned()))
    );
    // 第一个旧键清理失败，作为安全孤儿保留；state 已不再引用它。
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "first", 0).unwrap())
            .unwrap(),
        Some(Value::String("old-first".to_owned()))
    );
}

/// 卸载保存失败时，已删除的密钥必须恢复且公开状态保持不变。
#[cfg(unix)]
#[test]
fn uninstall_state_save_failure_restores_secret_changes() {
    let (_directory, manager, id) = transactional_plugin_fixture();
    let mut secrets = seeded_transaction_store(&manager, &id);
    let values_before = secrets.values.clone();
    let state_before = fs::read(&manager.storage.state_path).unwrap();
    let state_target = make_state_save_fail(&manager);
    secrets.state_save_failure = Some((manager.storage.state_path.clone(), state_target.clone()));
    secrets.state_save_failure_on_get = true;

    let error = manager
        .uninstall(&id, &mut secrets)
        .expect_err("卸载状态保存失败时必须返回错误");

    assert!(error.to_string().contains("插件状态"));
    assert_eq!(secrets.values, values_before);
    assert_eq!(fs::read(&state_target).unwrap(), state_before);
    let unchanged =
        PluginStateFile::into_state(serde_json::from_slice(&state_before).unwrap()).unwrap();
    assert_eq!(unchanged.plugins.len(), 1);
}

/// 卸载状态提交成功后，即使密钥清理失败也不能回滚状态或删除仍被引用的数据。
#[test]
fn uninstall_commits_state_before_secret_cleanup() {
    let (_directory, manager, id) = transactional_plugin_fixture();
    let mut secrets = seeded_transaction_store(&manager, &id);
    secrets.fail_delete_call = Some(1);

    manager.uninstall(&id, &mut secrets).unwrap();

    assert!(manager.load_state().unwrap().plugins.is_empty());
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "first", 0).unwrap())
            .unwrap(),
        Some(Value::String("old-first".to_owned()))
    );
    assert_eq!(
        secrets
            .get_json(&manager.storage.secret_key_at(&id, "second", 0).unwrap())
            .unwrap(),
        None
    );
}

/// 敏感 userConfig 只能出现在 SecretStore，不能被写入公开状态。
#[test]
fn splits_sensitive_user_config() {
    let definition: UserConfigDefinition =
        serde_json::from_str(r#"{"type":"string","sensitive":true,"required":true}"#).unwrap();
    let manifest = PluginManifest {
        name: "demo".to_owned(),
        version: Some("1".to_owned()),
        description: None,
        author: None,
        homepage: None,
        repository: None,
        license: None,
        keywords: Vec::new(),
        commands: ComponentDeclaration::default(),
        skills: ComponentDeclaration::default(),
        agents: ComponentDeclaration::default(),
        hooks: None,
        mcp_servers: McpServersDeclaration::default(),
        lsp_servers: Vec::new(),
        user_config: BTreeMap::from([("token".to_owned(), definition)]),
        dependencies: BTreeMap::new(),
        extra: BTreeMap::new(),
    };
    let manager = PluginManager::new("/tmp/keencode-plugin-plugin-test");
    let id = PluginId::parse("demo@official").unwrap();
    let mut store = InMemorySecretStore::default();
    let (public, sensitive) = manager
        .apply_user_config(
            &id,
            &manifest,
            None,
            UserConfigUpdate {
                values: BTreeMap::from([("token".to_owned(), Value::String("secret".to_owned()))]),
                replace: false,
            },
            &mut store,
            true,
        )
        .unwrap();
    assert!(public.is_empty());
    assert!(sensitive.contains("token"));
    assert_eq!(
        store
            .get_json(&manager.storage.secret_key(&id, "token").unwrap())
            .unwrap(),
        Some(Value::String("secret".to_owned()))
    );
}
