use std::path::PathBuf;

use peri_agent::agent::async_tasks::new_std_command;
use tempfile::{tempdir, TempDir};

use super::*;
use crate::plugin::PluginOrigin;

/// 创建带可安装文件的本地 Git 插件 fixture；没有 Git 的环境跳过相关测试。
fn create_git_plugin_fixture() -> Option<(TempDir, PathBuf)> {
    let version = new_std_command("git").arg("--version").output().ok()?;
    if !version.status.success() {
        return None;
    }

    let repository = tempdir().expect("创建 Git 插件 fixture 目录");
    let repository_path = repository.path().to_string_lossy().into_owned();
    let init = new_std_command("git")
        .args(["init", "--quiet", "--", &repository_path])
        .output()
        .expect("执行 git init");
    assert!(init.status.success(), "git init 失败: {:?}", init.stderr);
    std::fs::write(repository.path().join("README.md"), "external plugin")
        .expect("写入插件 fixture");
    let add = new_std_command("git")
        .args(["-C", &repository_path, "add", "--", "README.md"])
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

/// 创建跨平台目录符号链接；Windows 无开发者模式时由调用方跳过测试。
#[cfg(unix)]
fn create_directory_symlink(
    target: &std::path::Path,
    link: &std::path::Path,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 创建跨平台目录符号链接；Windows 无开发者模式时由调用方跳过测试。
#[cfg(windows)]
fn create_directory_symlink(
    target: &std::path::Path,
    link: &std::path::Path,
) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

/// 创建跨平台文件符号链接；Windows 无开发者模式时由调用方跳过测试。
#[cfg(unix)]
fn create_file_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 创建跨平台文件符号链接；Windows 无开发者模式时由调用方跳过测试。
#[cfg(windows)]
fn create_file_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// 按当前安装布局构造指定插件版本的缓存目录。
fn plugin_cache_version_dir(
    claude_dir: &std::path::Path,
    plugin_id: &str,
    version: &str,
) -> PathBuf {
    let plugin_id = PluginId::parse(plugin_id).expect("测试插件 ID 必须有效");
    claude_dir
        .join("plugins")
        .join("cache")
        .join(plugin_storage_component(&plugin_id))
        .join(version)
}

fn setup_marketplace_cache(cache_dir: &Path) {
    let mkt_dir = cache_dir.join("test-mkt");
    std::fs::create_dir_all(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join(".claude-plugin"),
    )
    .unwrap();
    let marketplace_json = r#"{
            "name": "test-marketplace",
            "plugins": [
                {
                    "name": "test-plugin",
                    "description": "A test plugin",
                    "source": "plugins/test-plugin",
                    "version": "1.0.0",
                    "sha": "abc1234567890"
                }
            ]
        }"#;
    std::fs::write(mkt_dir.join("marketplace.json"), marketplace_json).unwrap();
    let plugin_json = r#"{"name":"test-plugin","version":"1.0.0","description":"Test"}"#;
    std::fs::write(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join(".claude-plugin")
            .join("plugin.json"),
        plugin_json,
    )
    .unwrap();
    // Add a skill file
    std::fs::create_dir_all(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join("skills")
            .join("test-skill"),
    )
    .unwrap();
    std::fs::write(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join("skills")
            .join("test-skill")
            .join("SKILL.md"),
        "---\nname: test-skill\ndescription: test\n---\nTest content",
    )
    .unwrap();
}

/// 外部 Git 插件 marketplace 清单使用 URL 对象，验证安装入口复用统一 checkout 提升。
fn setup_external_git_marketplace_cache(cache_dir: &Path, repository: &Path) {
    let marketplace_dir = cache_dir.join("external-mkt");
    std::fs::create_dir_all(&marketplace_dir).unwrap();
    let manifest = serde_json::json!({
        "name": "external-marketplace",
        "plugins": [{
            "name": "external-plugin",
            "description": "External Git plugin",
            "source": {"source": "url", "url": repository.to_string_lossy()},
            "version": "1.0.0"
        }]
    });
    std::fs::write(
        marketplace_dir.join("marketplace.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn test_install_plugin_success() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    let result = install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.id, "test-plugin@test-mkt");
    assert_eq!(result.version, "abc1234");
    assert_eq!(result.marketplace, "test-mkt");

    // Verify installed_plugins.json
    let installed = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert_eq!(installed.plugins.len(), 1);
    assert_eq!(installed.plugins[0].id, "test-plugin@test-mkt");

    // Verify cache directory has plugin files
    let plugin_cache = claude_dir
        .path()
        .join("plugins")
        .join("cache")
        .join(plugin_storage_component(
            &crate::plugin::PluginId::parse("test-plugin@test-mkt").unwrap(),
        ))
        .join("abc1234");
    assert!(plugin_cache
        .join(".claude-plugin")
        .join("plugin.json")
        .exists());

    // Verify settings.json enabledPlugins (对象格式)
    let settings_path = claude_dir.path().join("settings.json");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(
        enabled
            .get("test-plugin@test-mkt")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

/// marketplace 根目录本身可以通过 `.` 或 `./` 作为插件 source 安装。
#[tokio::test]
async fn test_install_plugin_accepts_marketplace_root_source_forms() {
    for source in [".", "./"] {
        let claude_dir = tempdir().unwrap();
        let cache_dir = tempdir().unwrap();
        let marketplace_dir = cache_dir.path().join("root-mkt");
        std::fs::create_dir_all(marketplace_dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            marketplace_dir.join("marketplace.json"),
            serde_json::json!({
                "name": "root-marketplace",
                "plugins": [{
                    "name": "root-plugin",
                    "description": "Root plugin",
                    "source": source,
                    "version": "1.0.0"
                }]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            marketplace_dir.join(".claude-plugin/plugin.json"),
            r#"{"name":"root-plugin","version":"1.0.0","description":"Root"}"#,
        )
        .unwrap();

        let installed = install_plugin(
            "root-plugin",
            "root-mkt",
            InstallScope::User,
            cache_dir.path(),
            claude_dir.path(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(installed.id, "root-plugin@root-mkt");
        assert!(installed
            .install_path
            .join(".claude-plugin/plugin.json")
            .is_file());
    }
}

/// marketplace source 指向市场根外部的目录符号链接时必须在安装前拒绝。
#[tokio::test]
async fn test_install_plugin_rejects_marketplace_source_symlink_escape() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let outside_dir = tempdir().unwrap();
    let marketplace_dir = cache_dir.path().join("symlink-mkt");
    std::fs::create_dir_all(&marketplace_dir).unwrap();
    std::fs::write(
        marketplace_dir.join("marketplace.json"),
        serde_json::json!({
            "name": "symlink-marketplace",
            "plugins": [{
                "name": "escaped-plugin",
                "description": "Escaped plugin",
                "source": "linked",
                "version": "1.0.0"
            }]
        })
        .to_string(),
    )
    .unwrap();
    if create_directory_symlink(outside_dir.path(), &marketplace_dir.join("linked")).is_err() {
        return;
    }

    let result = install_plugin(
        "escaped-plugin",
        "symlink-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert!(matches!(
        result,
        Err(InstallerError::SettingsError(message)) if message.contains("符号链接")
    ));
}

/// 插件目录内部的符号链接不得在复制时被跟随或写入目标目录。
#[tokio::test]
async fn test_install_plugin_rejects_symlink_inside_marketplace_source() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let outside_dir = tempdir().unwrap();
    let marketplace_dir = cache_dir.path().join("inner-symlink-mkt");
    let plugin_dir = marketplace_dir.join("plugins/inner-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        marketplace_dir.join("marketplace.json"),
        serde_json::json!({
            "name": "inner-symlink-marketplace",
            "plugins": [{
                "name": "inner-plugin",
                "description": "Inner symlink plugin",
                "source": "plugins/inner-plugin",
                "version": "1.0.0"
            }]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin/plugin.json"),
        r#"{"name":"inner-plugin","version":"1.0.0","description":"Inner"}"#,
    )
    .unwrap();
    let outside_file = outside_dir.path().join("outside.txt");
    std::fs::write(&outside_file, "outside").unwrap();
    if create_file_symlink(&outside_file, &plugin_dir.join("linked.txt")).is_err() {
        return;
    }

    let result = install_plugin(
        "inner-plugin",
        "inner-symlink-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert!(matches!(
        result,
        Err(InstallerError::CopyFailed { source, .. })
            if source.kind() == std::io::ErrorKind::InvalidData
    ));
    let target_dir = claude_dir
        .path()
        .join("plugins/cache")
        .join(plugin_storage_component(
            &PluginId::parse("inner-plugin@inner-symlink-mkt").unwrap(),
        ))
        .join("1.0.0");
    assert!(!target_dir.join("linked.txt").exists());
    assert_eq!(std::fs::read_to_string(outside_file).unwrap(), "outside");
}

/// 外部 Git 插件已有无效缓存时应自动恢复，并且只提升完整 checkout。
#[tokio::test]
async fn test_install_external_git_plugin_recovers_invalid_cache() {
    let Some((_repository, repository_path)) = create_git_plugin_fixture() else {
        return;
    };
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_external_git_marketplace_cache(cache_dir.path(), &repository_path);
    let plugin_id = crate::plugin::PluginId::parse("external-plugin@external-mkt").unwrap();
    let external_cache = external_plugin_cache_dir(claude_dir.path(), &plugin_id);
    std::fs::create_dir_all(&external_cache).unwrap();
    std::fs::write(external_cache.join("partial.txt"), "partial").unwrap();

    let installed = install_plugin(
        "external-plugin",
        "external-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(installed.id, "external-plugin@external-mkt");
    assert!(external_cache.join(".git").is_dir());
    assert!(external_cache.join("README.md").is_file());
    assert!(!external_cache.join("partial.txt").exists());
}

/// 外部 Git clone 失败时不能把正式插件缓存路径误留为可安装的空目录。
#[tokio::test]
async fn test_install_external_git_plugin_failure_cleans_cache() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let missing_repository = tempdir().unwrap().path().join("missing");
    setup_external_git_marketplace_cache(cache_dir.path(), &missing_repository);
    let plugin_id = crate::plugin::PluginId::parse("external-plugin@external-mkt").unwrap();
    let external_cache = external_plugin_cache_dir(claude_dir.path(), &plugin_id);
    std::fs::create_dir_all(&external_cache).unwrap();

    let result = install_plugin(
        "external-plugin",
        "external-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert!(result.is_err());
    assert!(!external_cache.exists());
}

#[tokio::test]
async fn test_install_plugin_not_found() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    let result = install_plugin(
        "nonexistent",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        InstallerError::PluginNotFound { name, .. } => assert_eq!(name, "nonexistent"),
        _ => panic!("expected PluginNotFound"),
    }
}

#[tokio::test]
async fn test_install_plugin_invalid_manifest() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let mkt_dir = cache_dir.path().join("test-mkt");
    std::fs::create_dir_all(mkt_dir.join("bad-plugin").join(".claude-plugin")).unwrap();
    let marketplace_json = r#"{
            "name": "test",
            "plugins": [{"name": "bad-plugin", "description": "", "source": "bad-plugin", "version": "1.0.0"}]
        }"#;
    std::fs::write(mkt_dir.join("marketplace.json"), marketplace_json).unwrap();
    std::fs::write(
        mkt_dir
            .join("bad-plugin")
            .join(".claude-plugin")
            .join("plugin.json"),
        "invalid json{{{",
    )
    .unwrap();

    let result = install_plugin(
        "bad-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_install_plugin_reinstall() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    let installed = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert_eq!(installed.plugins.len(), 1);
}

/// 安装去重使用 PluginId 的大小写无关语义，不误删除其他 scope 或项目记录。
#[tokio::test]
async fn test_install_plugin_matches_persisted_id_case_insensitively() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let project_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    let plugins_path = claude_dir
        .path()
        .join("plugins")
        .join("installed_plugins.json");
    let mut installed = load_installed_plugins(Some(&plugins_path)).unwrap();
    installed.plugins[0].id = "TEST-PLUGIN@TEST-MKT".into();
    save_installed_plugins(&installed, Some(&plugins_path)).unwrap();

    // 不同 scope/project_path 的同 ID 记录必须继续保留。
    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::Project,
        cache_dir.path(),
        claude_dir.path(),
        Some(project_dir.path()),
    )
    .await
    .unwrap();

    let installed = load_installed_plugins(Some(&plugins_path)).unwrap();
    assert_eq!(installed.plugins.len(), 2);
    assert!(installed
        .plugins
        .iter()
        .any(|plugin| plugin.scope == InstallScope::User));
    assert!(installed.plugins.iter().any(|plugin| {
        plugin.scope == InstallScope::Project
            && match_project_path(&plugin.project_path, Some(project_dir.path()))
    }));

    // 同 scope/project_path 的大小写变体必须被替换为一条记录。
    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    let installed = load_installed_plugins(Some(&plugins_path)).unwrap();
    assert_eq!(installed.plugins.len(), 2);
    assert_eq!(
        installed
            .plugins
            .iter()
            .filter(|plugin| plugin.scope == InstallScope::User)
            .count(),
        1
    );
}

#[tokio::test]
async fn test_uninstall_plugin() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    let data_dir = claude_dir
        .path()
        .join("plugins")
        .join("data")
        .join(plugin_storage_component(
            &crate::plugin::PluginId::parse("test-plugin@test-mkt").unwrap(),
        ));
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("state.json"), "{}").unwrap();

    uninstall_plugin("test-plugin@test-mkt", claude_dir.path(), None)
        .await
        .unwrap();

    let installed = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert!(installed.plugins.is_empty());

    // Verify settings.json enabledPlugins removed
    let settings_path = claude_dir.path().join("settings.json");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert!(!enabled.contains_key("test-plugin@test-mkt"));
    assert!(
        !data_dir.exists(),
        "卸载必须清理共享 storage_component 目录"
    );
}

/// 卸载、更新均应按 PluginId 忽略持久化 ID 的 ASCII 大小写差异。
#[tokio::test]
async fn test_uninstall_plugin_matches_case_insensitive_persisted_id_and_scope() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let project_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();
    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::Project,
        cache_dir.path(),
        claude_dir.path(),
        Some(project_dir.path()),
    )
    .await
    .unwrap();

    let plugins_path = claude_dir
        .path()
        .join("plugins")
        .join("installed_plugins.json");
    let mut installed = load_installed_plugins(Some(&plugins_path)).unwrap();
    for plugin in &mut installed.plugins {
        plugin.id = "TEST-PLUGIN@TEST-MKT".into();
    }
    save_installed_plugins(&installed, Some(&plugins_path)).unwrap();

    let settings_path = claude_dir.path().join("settings.json");
    let mut settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    settings["pluginConfigs"] = serde_json::json!({
        "TEST-PLUGIN@TEST-MKT": {"enabled": true}
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    // 传入小写 ID 时只卸载指定项目 scope，用户 scope 必须保留。
    uninstall_plugin(
        "test-plugin@test-mkt",
        claude_dir.path(),
        Some(project_dir.path()),
    )
    .await
    .unwrap();
    let installed = load_installed_plugins(Some(&plugins_path)).unwrap();
    assert_eq!(installed.plugins.len(), 1);
    assert_eq!(installed.plugins[0].scope, InstallScope::User);

    // 传入大写 ID 仍能匹配剩余用户 scope。
    uninstall_plugin("TEST-PLUGIN@TEST-MKT", claude_dir.path(), None)
        .await
        .unwrap();
    assert!(load_installed_plugins(Some(&plugins_path))
        .unwrap()
        .plugins
        .is_empty());

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(settings["pluginConfigs"].as_object().unwrap().is_empty());
}

#[tokio::test]
async fn test_uninstall_plugin_not_found() {
    let claude_dir = tempdir().unwrap();
    let result = uninstall_plugin("nonexistent@test", claude_dir.path(), None).await;
    assert!(result.is_err());
}

#[tokio::test]
/// 卸载入口必须原样保留共享契约提供的字段错误标签。
async fn test_uninstall_plugin_preserves_shared_validation_label() {
    let claude_dir = tempdir().unwrap();
    let error = uninstall_plugin("bad/name@market", claude_dir.path(), None)
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "插件名称 无效：bad/name");
}

#[tokio::test]
async fn test_update_plugin_same_version() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    let installed = install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    let result = update_plugin(
        "test-plugin@test-mkt",
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.id, installed.id);
    assert_eq!(result.version, installed.version);
}

/// 更新查找当前记录时必须忽略请求 ID 与持久化 ID 的大小写差异。
#[tokio::test]
async fn test_update_plugin_matches_case_variants() {
    for (stored_id, requested_id) in [
        ("TEST-PLUGIN@TEST-MKT", "test-plugin@test-mkt"),
        ("test-plugin@test-mkt", "TEST-PLUGIN@TEST-MKT"),
    ] {
        let claude_dir = tempdir().unwrap();
        let cache_dir = tempdir().unwrap();
        setup_marketplace_cache(cache_dir.path());

        install_plugin(
            "test-plugin",
            "test-mkt",
            InstallScope::User,
            cache_dir.path(),
            claude_dir.path(),
            None,
        )
        .await
        .unwrap();

        let plugins_path = claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json");
        let mut installed = load_installed_plugins(Some(&plugins_path)).unwrap();
        installed.plugins[0].id = stored_id.into();
        save_installed_plugins(&installed, Some(&plugins_path)).unwrap();

        let result = update_plugin(requested_id, cache_dir.path(), claude_dir.path(), None)
            .await
            .unwrap();
        assert_eq!(result.id, stored_id, "请求 ID={requested_id}");
        assert_eq!(result.version, "abc1234");
    }
}

#[tokio::test]
async fn test_check_updates() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    // Install plugin with old version
    let mut installed = InstalledPlugins::default();
    installed.plugins.push(InstalledPlugin {
        id: "test-plugin@test-mkt".into(),
        name: "test-plugin".into(),
        version: "old-version".into(),
        marketplace: "test-mkt".into(),
        install_path: claude_dir.path().join("fake").into(),
        scope: InstallScope::User,
        project_path: None,
        origin: PluginOrigin::PeriInstalled,
    });
    // Add a plugin with no update
    installed.plugins.push(InstalledPlugin {
        id: "other@test-mkt".into(),
        name: "other".into(),
        version: "abc1234".into(),
        marketplace: "test-mkt".into(),
        install_path: claude_dir.path().join("fake2").into(),
        scope: InstallScope::User,
        project_path: None,
        origin: PluginOrigin::PeriInstalled,
    });

    let updates = check_updates(&installed, cache_dir.path()).await;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].plugin_id, "test-plugin@test-mkt");
    assert_eq!(updates[0].latest_version, "abc1234");
    assert_eq!(updates[0].current_version, "old-version");
}

#[test]
fn test_copy_dir_recursive() {
    let src = tempdir().unwrap();
    let dst = tempdir().unwrap();

    // Create nested structure
    std::fs::create_dir_all(src.path().join("sub").join("deep")).unwrap();
    std::fs::write(src.path().join("file1.txt"), "content1").unwrap();
    std::fs::write(src.path().join("sub").join("file2.txt"), "content2").unwrap();
    std::fs::write(
        src.path().join("sub").join("deep").join("file3.txt"),
        "content3",
    )
    .unwrap();

    // Create .git dir (should be skipped)
    std::fs::create_dir_all(src.path().join(".git").join("objects")).unwrap();
    std::fs::write(src.path().join(".git").join("config"), "gitconfig").unwrap();

    copy_dir_recursive(src.path(), &dst.path().join("copy")).unwrap();

    assert!(dst.path().join("copy").join("file1.txt").exists());
    assert!(dst
        .path()
        .join("copy")
        .join("sub")
        .join("file2.txt")
        .exists());
    assert!(dst
        .path()
        .join("copy")
        .join("sub")
        .join("deep")
        .join("file3.txt")
        .exists());
    assert!(!dst.path().join("copy").join(".git").exists());

    // Verify content
    let content = std::fs::read_to_string(dst.path().join("copy").join("file1.txt")).unwrap();
    assert_eq!(content, "content1");
}

#[test]
fn test_update_enabled_plugins_append() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    update_enabled_plugins(
        &PluginId::parse("plugin-a").unwrap(),
        InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    // 现在写入对象格式
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(
        enabled.get("plugin-a").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn test_update_enabled_plugins_dedup() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    // 写入数组格式的现有文件
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":["plugin-a","plugin-b"]}"#,
    )
    .unwrap();

    update_enabled_plugins(
        &PluginId::parse("plugin-a").unwrap(),
        InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    // 应该转换为对象格式
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 2);
    assert!(enabled.contains_key("plugin-a"));
    assert!(enabled.contains_key("plugin-b"));
}

#[test]
fn test_update_enabled_plugins_matches_case_insensitive_key() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":{"PLUGIN-A@MARKET":true}}"#,
    )
    .unwrap();

    update_enabled_plugins(
        &PluginId::parse("plugin-a@market").unwrap(),
        InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled["PLUGIN-A@MARKET"], true);
}

#[test]
fn test_update_enabled_plugins_object_format() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    // 写入对象格式的现有文件（Claude Code 格式）
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":{"plugin-a":true,"plugin-b":true}}"#,
    )
    .unwrap();

    update_enabled_plugins(
        &PluginId::parse("plugin-c").unwrap(),
        InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 3);
    assert_eq!(
        enabled.get("plugin-c").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn test_remove_from_enabled_plugins_array_format() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":["plugin-a","plugin-b"]}"#,
    )
    .unwrap();

    remove_from_enabled_plugins(
        &PluginId::parse("plugin-a").unwrap(),
        &InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_array().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].as_str(), Some("plugin-b"));
}

#[test]
fn test_remove_from_enabled_plugins_object_format() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":{"plugin-a":true,"plugin-b":true}}"#,
    )
    .unwrap();

    remove_from_enabled_plugins(
        &PluginId::parse("plugin-a").unwrap(),
        &InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(
        enabled.get("plugin-b").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn test_remove_from_enabled_plugins_matches_case_insensitive_keys() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":{"PLUGIN-A@MARKET":true,"plugin-a@market":true,"plugin-b@market":true}}"#,
    )
    .unwrap();

    remove_from_enabled_plugins(
        &PluginId::parse("plugin-a@market").unwrap(),
        &InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 1);
    assert!(!enabled.contains_key("PLUGIN-A@MARKET"));
    assert!(!enabled.contains_key("plugin-a@market"));
    assert_eq!(enabled["plugin-b@market"], true);

    // 数组格式同样按 PluginId 语义清理大小写变体。
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":["PLUGIN-A@MARKET","plugin-b@market"]}"#,
    )
    .unwrap();
    remove_from_enabled_plugins(
        &PluginId::parse("plugin-a@market").unwrap(),
        &InstallScope::User,
        claude_dir,
        None,
    )
    .unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_array().unwrap();
    assert_eq!(enabled, &[serde_json::json!("plugin-b@market")]);
}

#[test]
fn test_get_marketplace_manifest_uses_safe_cache_key() {
    let cache_dir = tempdir().unwrap();
    let marketplace = "owner/repository";
    let marketplace_dir =
        crate::plugin::marketplace::marketplace_cache_dir(cache_dir.path(), marketplace).unwrap();
    std::fs::create_dir_all(&marketplace_dir).unwrap();
    std::fs::write(
        marketplace_dir.join("marketplace.json"),
        r#"{"name":"nested","plugins":[]}"#,
    )
    .unwrap();

    let manifest = get_marketplace_manifest(marketplace, cache_dir.path()).unwrap();

    assert_eq!(manifest.name, "nested");
    assert!(manifest.plugins.is_empty());
}

#[test]
fn test_get_marketplace_manifest_rejects_cache_path_escape() {
    let cache_dir = tempdir().unwrap();
    let outside_dir = tempdir().unwrap();
    std::fs::write(
        outside_dir.path().join("marketplace.json"),
        r#"{"name":"outside","plugins":[]}"#,
    )
    .unwrap();
    let outside_name = outside_dir
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap();
    let escaped_name = format!("../{outside_name}");

    let result = get_marketplace_manifest(&escaped_name, cache_dir.path());

    assert!(matches!(result, Err(InstallerError::SettingsError(_))));
}

// ── match_project_path tests ──

#[test]
fn test_match_project_path_both_none() {
    assert!(match_project_path(&None, None));
}

#[test]
fn test_match_project_path_stored_none_given_some() {
    assert!(!match_project_path(&None, Some(Path::new("/project"))));
}

#[test]
fn test_match_project_path_given_none_stored_some() {
    assert!(!match_project_path(&Some("/project".into()), None));
}

#[test]
fn test_match_project_path_exact_match() {
    assert!(match_project_path(
        &Some("/home/user/project".into()),
        Some(Path::new("/home/user/project"))
    ));
}

#[test]
fn test_match_project_path_suffix_match() {
    assert!(match_project_path(
        &Some("/home/user/project".into()),
        Some(Path::new("project"))
    ));
    assert!(match_project_path(
        &Some("project".into()),
        Some(Path::new("/home/user/project"))
    ));
}

#[test]
fn test_match_project_path_no_match() {
    assert!(!match_project_path(
        &Some("/home/user/project-a".into()),
        Some(Path::new("/home/user/project-b"))
    ));
}

// ── cleanup_orphaned_plugins tests ──

#[tokio::test]
async fn test_cleanup_no_cache_dir() {
    let dir = tempdir().unwrap();
    let result = cleanup_orphaned_plugins(dir.path()).await.unwrap();
    assert_eq!(result, 0, "no cache dir should return 0");
}

#[tokio::test]
/// 当前缓存布局下，超过保留期且带孤儿标记的版本应被删除。
async fn test_cleanup_removes_old_orphaned() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    // 创建当前缓存结构：cache/<完整 PluginId 安全键>/<version>/。
    let version_dir = plugin_cache_version_dir(claude_dir, "my-plugin@mkt", "v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    // Write .orphaned_at with a timestamp 8 days ago (> 7 day threshold)
    let eight_days_ago = chrono::Utc::now() - chrono::Duration::try_days(8).unwrap();
    std::fs::write(
        version_dir.join(".orphaned_at"),
        eight_days_ago.to_rfc3339(),
    )
    .unwrap();
    // Set file modified time to 8 days ago
    let eight_days_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(eight_days_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(eight_days_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    // No installed plugins → empty installed_plugins.json
    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(deleted, 1, "should delete 1 old orphaned version");
    assert!(!version_dir.exists(), "old orphaned dir should be removed");
}

#[tokio::test]
/// 当前缓存布局下，未达到保留期的孤儿版本应继续保留。
async fn test_cleanup_preserves_recent_orphaned() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    let version_dir = plugin_cache_version_dir(claude_dir, "my-plugin@mkt", "v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    // .orphaned_at 1 day ago (< 7 day threshold)
    let one_day_ago = chrono::Utc::now() - chrono::Duration::try_days(1).unwrap();
    std::fs::write(version_dir.join(".orphaned_at"), one_day_ago.to_rfc3339()).unwrap();
    let one_day_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(one_day_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(one_day_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(deleted, 0, "recent orphaned should not be deleted");
    assert!(
        version_dir.exists(),
        "recent orphaned dir should still exist"
    );
}

#[tokio::test]
/// 已安装版本即使带有旧孤儿标记也必须保留，并清除该标记。
async fn test_cleanup_preserves_installed_version() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    let version_dir = plugin_cache_version_dir(claude_dir, "my-plugin@mkt", "v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    // Mark as old orphaned
    let eight_days_ago = chrono::Utc::now() - chrono::Duration::try_days(8).unwrap();
    std::fs::write(
        version_dir.join(".orphaned_at"),
        eight_days_ago.to_rfc3339(),
    )
    .unwrap();
    let eight_days_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(eight_days_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(eight_days_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    // Register as installed → should be preserved
    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![InstalledPlugin {
                id: "my-plugin@mkt".into(),
                name: "my-plugin".into(),
                version: "v1".into(),
                marketplace: "mkt".into(),
                install_path: version_dir.clone(),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            }],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(deleted, 0, "installed version should not be deleted");
    assert!(
        version_dir.exists(),
        "installed version dir should still exist"
    );
    assert!(
        !version_dir.join(".orphaned_at").exists(),
        ".orphaned_at marker should be removed for installed version"
    );
}

#[tokio::test]
/// 删除孤儿版本后应清理空的插件安全键目录，但保留 cache 根目录。
async fn test_cleanup_removes_empty_parent_dirs() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    // 当前结构：cache/<完整 PluginId 安全键>/<version>/。
    let version_dir = plugin_cache_version_dir(claude_dir, "my-plugin@mkt", "v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    let eight_days_ago = chrono::Utc::now() - chrono::Duration::try_days(8).unwrap();
    std::fs::write(
        version_dir.join(".orphaned_at"),
        eight_days_ago.to_rfc3339(),
    )
    .unwrap();
    let eight_days_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(eight_days_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(eight_days_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let _deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();

    let plugin_dir = version_dir.parent().unwrap();
    let cache_dir = plugin_dir.parent().unwrap();
    assert!(
        !plugin_dir.exists(),
        "empty plugin cache dir should be removed"
    );
    assert!(cache_dir.exists(), "cache root should remain available");
}

#[tokio::test]
/// 没有孤儿标记的版本目录不得被自动清理。
async fn test_cleanup_orphaned_no_marker_not_deleted() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    // 没有 .orphaned_at 标记的版本目录不应被清理。
    let version_dir = plugin_cache_version_dir(claude_dir, "my-plugin@mkt", "v1");
    std::fs::create_dir_all(&version_dir).unwrap();
    // Write a dummy file so dir is not empty
    std::fs::write(version_dir.join("plugin.json"), "{}").unwrap();

    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(
        deleted, 0,
        "version without orphaned marker should not be deleted"
    );
    assert!(
        version_dir.exists(),
        "version dir without marker should still exist"
    );
}

/// 自动发现插件时应沿用 PluginId 的大小写无关名称语义。
#[test]
fn test_plugin_name_matching_is_case_insensitive() {
    assert!(crate::plugin::marketplace::plugin_names_equal(
        "Demo-Plugin",
        "DEMO-PLUGIN"
    ));
    assert!(!crate::plugin::marketplace::plugin_names_equal(
        "Demo-Plugin",
        "other-plugin"
    ));
}

#[test]
fn test_generate_synthetic_manifest_lsp() {
    let dir = tempdir().unwrap();
    let plugin = crate::plugin::types::MarketplacePlugin {
        name: "rust-analyzer-lsp".into(),
        description: "Rust language server".into(),
        source: serde_json::json!("./plugins/rust-analyzer-lsp"),
        version: "1.0.0".into(),
        sha: None,
        author: None,
        category: None,
        homepage: None,
        tags: None,
        extra: serde_json::json!({
            "lspServers": {
                "rust-analyzer": {
                    "command": "rust-analyzer",
                    "extensionToLanguage": { ".rs": "rust" }
                }
            }
        }),
    };

    generate_synthetic_manifest(dir.path(), &plugin).unwrap();

    let manifest_path = dir.path().join(".claude-plugin").join("plugin.json");
    assert!(manifest_path.exists());

    let content = std::fs::read_to_string(&manifest_path).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(manifest["name"], "rust-analyzer-lsp");
    assert_eq!(manifest["version"], "1.0.0");
    assert_eq!(manifest["description"], "Rust language server");

    let lsp_servers = manifest["lspServers"].as_array().unwrap();
    assert_eq!(lsp_servers.len(), 1);
    assert_eq!(lsp_servers[0]["name"], "rust-analyzer");
    assert_eq!(lsp_servers[0]["command"], "rust-analyzer");
    assert_eq!(lsp_servers[0]["extensionToLanguage"][".rs"], "rust");
}

#[test]
fn test_generate_synthetic_manifest_with_author() {
    let dir = tempdir().unwrap();
    let plugin = crate::plugin::types::MarketplacePlugin {
        name: "test-plugin".into(),
        description: String::new(),
        source: serde_json::json!("."),
        version: "2.0.0".into(),
        sha: None,
        author: Some(crate::plugin::types::PluginAuthor {
            name: "Test".into(),
            url: None,
        }),
        category: None,
        homepage: None,
        tags: None,
        extra: serde_json::Value::Object(Default::default()),
    };

    generate_synthetic_manifest(dir.path(), &plugin).unwrap();

    let content =
        std::fs::read_to_string(dir.path().join(".claude-plugin").join("plugin.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(manifest["author"]["name"], "Test");
    assert!(manifest.get("lspServers").is_none());
}

#[test]
fn test_generate_synthetic_manifest_no_version() {
    let dir = tempdir().unwrap();
    let plugin = crate::plugin::types::MarketplacePlugin {
        name: "minimal".into(),
        description: "desc".into(),
        source: serde_json::json!("."),
        version: String::new(),
        sha: None,
        author: None,
        category: None,
        homepage: None,
        tags: None,
        extra: serde_json::Value::Object(Default::default()),
    };

    generate_synthetic_manifest(dir.path(), &plugin).unwrap();

    let content =
        std::fs::read_to_string(dir.path().join(".claude-plugin").join("plugin.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(manifest["name"], "minimal");
    assert!(manifest.get("version").is_none());
}
