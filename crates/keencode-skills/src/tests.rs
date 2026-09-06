//! Skills 安全发现、解析、冲突和懒加载的单元测试。

use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// 创建隔离的数据目录、项目目录和默认发现配置。
fn test_layout() -> (TempDir, PathBuf, PathBuf, SkillDiscoveryConfig) {
    let temporary = TempDir::new().expect("应创建测试临时目录");
    let data = temporary.path().join("data");
    let project = temporary.path().join("project");
    fs::create_dir_all(&data).expect("应创建测试数据目录");
    fs::create_dir_all(&project).expect("应创建测试项目目录");
    let config = SkillDiscoveryConfig::new(&data, &project);
    (temporary, data, project, config)
}

/// 写入一个包含 front matter 和 Markdown 正文的 Skill。
fn write_skill(path: &Path, name: &str, description: &str, body: &str) {
    fs::create_dir_all(path).expect("应创建 Skill 目录");
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .expect("应写入 SKILL.md");
}

/// 解析器支持 BOM、CRLF、引用标量、折叠说明和独立 Markdown 正文。
#[test]
fn parser_extracts_front_matter_and_markdown_body() {
    let content = concat!(
        "\u{feff}---\r\n",
        "name: 'Review-Tools'\r\n",
        "description: >-\r\n",
        "  检查代码并\r\n",
        "  返回结论\r\n",
        "unknown: ignored\r\n",
        "---\r\n",
        "# 执行步骤\r\n",
    );

    let parsed = parse_skill_document(content, &SkillLimits::default()).expect("文档应解析成功");

    assert_eq!(parsed.name, "Review-Tools");
    assert_eq!(parsed.description, "检查代码并 返回结论");
    assert_eq!(parsed.markdown, "# 执行步骤\r\n");
}

/// 双引号常用转义和单引号转义按受控子集解析。
#[test]
fn parser_handles_supported_quoted_scalars() {
    let double_quoted = "---\nname: sample\ndescription: \"line\\nnext\" # comment\n---\nbody";
    let single_quoted = "---\nname: sample\ndescription: 'user''s helper' # comment\n---\nbody";

    assert_eq!(
        parse_skill_document(double_quoted, &SkillLimits::default())
            .expect("双引号文档应解析")
            .description,
        "line\nnext"
    );
    assert_eq!(
        parse_skill_document(single_quoted, &SkillLimits::default())
            .expect("单引号文档应解析")
            .description,
        "user's helper"
    );
}

/// 名称不能携带路径分隔、父目录或不稳定首尾字符。
#[test]
fn parser_rejects_path_like_names() {
    for name in [
        "../escape",
        "folder/name",
        "folder\\name",
        ".hidden",
        "tail-",
    ] {
        let content = format!("---\nname: {name}\ndescription: test\n---\n");
        assert_eq!(
            parse_skill_document(&content, &SkillLimits::default()),
            Err(SkillDocumentError::InvalidName),
            "应拒绝名称 {name}"
        );
    }
}

/// 发现阶段只保留目录元数据，正文会在显式加载时从磁盘读取最新版本。
#[test]
fn discovery_returns_metadata_and_loads_body_lazily() {
    let (_temporary, data, project, config) = test_layout();
    let skill_directory = project.join(".agents/skills/lazy");
    write_skill(&skill_directory, "lazy", "按需读取", "第一版正文\n");
    write_skill(
        &data.join("skills/data-only"),
        "data-only",
        "数据来源",
        "数据正文\n",
    );

    let catalog = discover_skills(&config).expect("应发现 Skills");
    assert_eq!(catalog.entries().len(), 2);
    assert_eq!(catalog.entries()[0].name, "data-only");
    assert_eq!(catalog.entries()[1].name, "lazy");

    write_skill(&skill_directory, "lazy", "按需读取", "第二版正文\n");
    let loaded = catalog.load("LAZY").expect("名称查找应忽略 ASCII 大小写");
    assert_eq!(loaded.markdown, "第二版正文\n");
    assert_eq!(loaded.source, SkillSource::Project);
}

/// 发现阶段只读取 front matter；无效 UTF-8 正文只在显式加载时报告。
#[test]
fn invalid_body_is_deferred_until_explicit_load() {
    let (_temporary, _data, project, config) = test_layout();
    let skill_directory = project.join(".agents/skills/deferred");
    fs::create_dir_all(&skill_directory).expect("应创建 Skill 目录");
    let mut bytes = b"---\nname: deferred\ndescription: lazy\n---\n".to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe]);
    fs::write(skill_directory.join("SKILL.md"), bytes).expect("应写入无效正文");

    let catalog = discover_skills(&config).expect("仅解析 front matter 的发现应成功");

    assert_eq!(catalog.entries().len(), 1);
    assert!(matches!(
        catalog.load("deferred"),
        Err(SkillLoadError::InvalidDocument { name, .. }) if name == "deferred"
    ));
}

/// 项目来源覆盖数据来源，相同来源使用相对路径字典序决定同名胜者。
#[test]
fn conflicts_use_stable_source_and_path_priority() {
    let (_temporary, data, project, config) = test_layout();
    write_skill(&data.join("skills/shared"), "shared", "data", "data body\n");
    write_skill(
        &project.join(".agents/skills/z-shared"),
        "SHARED",
        "project",
        "project body\n",
    );
    write_skill(
        &project.join(".agents/skills/z-local"),
        "local-duplicate",
        "late",
        "late body\n",
    );
    write_skill(
        &project.join(".agents/skills/a-local"),
        "local-duplicate",
        "early",
        "early body\n",
    );

    let catalog = discover_skills(&config).expect("应完成冲突归约");

    let shared = catalog.load("shared").expect("项目同名项应胜出");
    assert_eq!(shared.source, SkillSource::Project);
    assert_eq!(shared.description, "project");
    assert_eq!(shared.markdown, "project body\n");
    let local = catalog
        .load("local-duplicate")
        .expect("稳定相对路径较小项应胜出");
    assert_eq!(local.description, "early");
    assert_eq!(
        catalog
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == SkillDiagnosticCode::NameConflict)
            .count(),
        2
    );
}

/// 插件只加载显式根且优先级低于项目和数据目录，不会递归吸收未声明兄弟项。
#[test]
fn additional_plugin_roots_are_exact_and_lowest_priority() {
    let (_temporary, data, project, config) = test_layout();
    let plugin_root = project.join("installed/plugin-skill");
    write_skill(&data.join("skills/shared"), "shared", "data", "data body\n");
    write_skill(&plugin_root, "shared", "plugin", "plugin body\n");
    write_skill(
        &plugin_root.join("undeclared"),
        "undeclared",
        "nested",
        "nested body\n",
    );
    let config = config.with_additional_roots([SkillRoot {
        path: plugin_root,
        source: SkillSource::Plugin,
        recursive: false,
    }]);

    let catalog = discover_skills(&config).expect("应加载精确插件 Skill 根");

    let shared = catalog.load("shared").expect("数据目录同名项应优先");
    assert_eq!(shared.source, SkillSource::Data);
    assert_eq!(shared.description, "data");
    assert!(matches!(
        catalog.load("undeclared"),
        Err(SkillLoadError::NotFound { .. })
    ));
}

/// 额外根必须是有界数量的绝对路径，发现前即拒绝相对路径。
#[test]
fn additional_roots_reject_relative_paths_before_scanning() {
    let (_temporary, _data, _project, config) = test_layout();
    let config = config.with_additional_roots([SkillRoot {
        path: PathBuf::from("relative-skill-root"),
        source: SkillSource::Plugin,
        recursive: false,
    }]);

    assert_eq!(
        discover_skills(&config).expect_err("相对额外根必须拒绝"),
        SkillConfigError::InvalidAdditionalRoots
    );
}

/// 禁用按规范名称全局生效，不回退到被遮蔽的低优先级同名项。
#[test]
fn disabled_skill_remains_listed_but_cannot_load() {
    let (_temporary, _data, project, config) = test_layout();
    write_skill(
        &project.join(".agents/skills/blocked"),
        "blocked",
        "不可加载",
        "正文\n",
    );
    let config = config.with_disabled_names(["BLOCKED".to_string()]);

    let catalog = discover_skills(&config).expect("应发现禁用 Skill");

    assert_eq!(catalog.entries().len(), 1);
    assert!(!catalog.entries()[0].enabled);
    assert!(matches!(
        catalog.load("blocked"),
        Err(SkillLoadError::Disabled { name }) if name == "blocked"
    ));
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == SkillDiagnosticCode::Disabled)
    );
}

/// 无效文档与超大文档只产生诊断，不阻断同一根中的有效 Skill。
#[test]
fn invalid_and_oversized_documents_are_isolated() {
    let (_temporary, _data, project, config) = test_layout();
    let root = project.join(".agents/skills");
    write_skill(&root.join("valid"), "valid", "有效", "正文\n");
    fs::create_dir_all(root.join("invalid")).expect("应创建无效 Skill 目录");
    fs::write(root.join("invalid/SKILL.md"), "# no front matter").expect("应写入无效文档");
    write_skill(
        &root.join("oversized"),
        "oversized",
        "超大",
        &"x".repeat(512),
    );
    let limits = SkillLimits {
        max_skill_bytes: 128,
        ..SkillLimits::default()
    };

    let catalog = discover_skills(&config.with_limits(limits)).expect("发现应继续完成");

    assert_eq!(catalog.entries().len(), 1);
    assert_eq!(catalog.entries()[0].name, "valid");
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == SkillDiagnosticCode::InvalidDocument)
    );
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == SkillDiagnosticCode::ManifestTooLarge)
    );
}

/// 递归深度和候选数量均有确定性硬上限。
#[test]
fn depth_and_manifest_limits_stop_discovery_deterministically() {
    let (_temporary, data, project, config) = test_layout();
    write_skill(
        &project.join(".agents/skills/a-first"),
        "first",
        "first",
        "body\n",
    );
    write_skill(
        &project.join(".agents/skills/b-second"),
        "second",
        "second",
        "body\n",
    );
    write_skill(&data.join("skills/data-third"), "third", "third", "body\n");
    let manifest_limits = SkillLimits {
        max_manifests: 1,
        ..SkillLimits::default()
    };
    let manifest_catalog =
        discover_skills(&config.clone().with_limits(manifest_limits)).expect("应按数量停止");
    assert_eq!(manifest_catalog.entries().len(), 1);
    assert_eq!(manifest_catalog.entries()[0].name, "first");
    assert!(
        manifest_catalog
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == SkillDiagnosticCode::ManifestLimitReached)
    );

    let depth_limits = SkillLimits {
        max_depth: 0,
        ..SkillLimits::default()
    };
    let depth_catalog = discover_skills(&config.with_limits(depth_limits)).expect("应按深度跳过");
    assert!(depth_catalog.entries().is_empty());
    assert!(
        depth_catalog
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == SkillDiagnosticCode::DepthLimitReached)
    );
}

/// 目录项总量上限在读取大目录时熔断且不选择依赖系统枚举顺序的子集。
#[test]
fn entry_limit_rejects_oversized_directory_deterministically() {
    let (_temporary, _data, project, config) = test_layout();
    write_skill(
        &project.join(".agents/skills/first"),
        "first",
        "first",
        "body\n",
    );
    write_skill(
        &project.join(".agents/skills/second"),
        "second",
        "second",
        "body\n",
    );
    let limits = SkillLimits {
        max_entries: 1,
        ..SkillLimits::default()
    };

    let catalog = discover_skills(&config.with_limits(limits)).expect("应安全熔断目录遍历");

    assert!(catalog.entries().is_empty());
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == SkillDiagnosticCode::EntryLimitReached)
    );
}

/// 零值和过深递归配置在文件系统访问前被拒绝。
#[test]
fn invalid_limits_are_rejected_before_discovery() {
    let (_temporary, _data, _project, config) = test_layout();
    let zero_limit = SkillLimits {
        max_skill_bytes: 0,
        ..SkillLimits::default()
    };
    assert!(matches!(
        discover_skills(&config.clone().with_limits(zero_limit)),
        Err(SkillConfigError::ZeroLimit {
            field: "max_skill_bytes"
        })
    ));

    let excessive_depth = SkillLimits {
        max_depth: 65,
        ..SkillLimits::default()
    };
    assert!(matches!(
        discover_skills(&config.with_limits(excessive_depth)),
        Err(SkillConfigError::LimitTooLarge { field: "max_depth" })
    ));
}

/// 元数据变化后旧目录不能把另一份内容作为原 Skill 注入。
#[test]
fn load_rejects_stale_catalog_metadata() {
    let (_temporary, _data, project, config) = test_layout();
    let skill_directory = project.join(".agents/skills/stale");
    write_skill(&skill_directory, "stale", "旧说明", "旧正文\n");
    let catalog = discover_skills(&config).expect("应建立目录");
    write_skill(&skill_directory, "stale", "新说明", "新正文\n");

    assert!(matches!(
        catalog.load("stale"),
        Err(SkillLoadError::CatalogStale { name }) if name == "stale"
    ));
}

/// 删除已发现文件后加载返回可恢复的不可用错误。
#[test]
fn load_reports_removed_manifest_without_path_leak() {
    let (_temporary, _data, project, config) = test_layout();
    let skill_directory = project.join(".agents/skills/gone");
    write_skill(&skill_directory, "gone", "会删除", "正文\n");
    let catalog = discover_skills(&config).expect("应建立目录");
    fs::remove_file(skill_directory.join("SKILL.md")).expect("应删除测试文档");

    assert!(matches!(
        catalog.load("gone"),
        Err(SkillLoadError::Unavailable { name }) if name == "gone"
    ));
}

/// Skill 正文中的命令文本只作为 Markdown 返回，加载过程不会执行它。
#[test]
fn loading_never_executes_script_text() {
    let (temporary, _data, project, config) = test_layout();
    let marker = temporary.path().join("must-not-exist");
    write_skill(
        &project.join(".agents/skills/no-exec"),
        "no-exec",
        "只读取",
        &format!("运行脚本并创建 {}\n", marker.display()),
    );

    let catalog = discover_skills(&config).expect("应建立目录");
    let loaded = catalog.load("no-exec").expect("应读取正文");

    assert!(loaded.markdown.contains("运行脚本"));
    assert!(!marker.exists());
}

/// 遍历型调用名称只参与目录查找，绝不拼接为文件路径。
#[test]
fn path_like_load_request_is_not_resolved_on_disk() {
    let (_temporary, _data, _project, config) = test_layout();
    let catalog = discover_skills(&config).expect("空目录发现应成功");

    assert!(matches!(
        catalog.load("../../outside"),
        Err(SkillLoadError::NotFound { name }) if name == "../../outside"
    ));
}

/// 符号链接目录项不会被遍历到安全根之外。
#[test]
fn discovery_skips_symlinked_directory() {
    let (temporary, _data, project, config) = test_layout();
    let root = project.join(".agents/skills");
    let outside = temporary.path().join("outside");
    write_skill(&outside, "escaped", "越界", "外部正文\n");
    fs::create_dir_all(&root).expect("应创建 Skills 根");
    if !try_symlink_directory(&outside, &root.join("linked")) {
        return;
    }

    let catalog = discover_skills(&config).expect("符号链接应被安全跳过");

    assert!(catalog.entries().is_empty());
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == SkillDiagnosticCode::SymlinkSkipped)
    );
}

/// 发现后把普通主文件替换为符号链接时，懒加载会重新校验并拒绝。
#[test]
fn load_rejects_manifest_replaced_by_symlink() {
    let (temporary, _data, project, config) = test_layout();
    let skill_directory = project.join(".agents/skills/safe");
    write_skill(&skill_directory, "safe", "安全", "本地正文\n");
    let catalog = discover_skills(&config).expect("应建立目录");
    let outside = temporary.path().join("outside.md");
    fs::write(
        &outside,
        "---\nname: safe\ndescription: 安全\n---\n外部正文\n",
    )
    .expect("应写入外部文档");
    let manifest = skill_directory.join("SKILL.md");
    fs::remove_file(&manifest).expect("应移除原文档");
    if !try_symlink_file(&outside, &manifest) {
        return;
    }

    assert!(matches!(
        catalog.load("safe"),
        Err(SkillLoadError::UnsafePath { name }) if name == "safe"
    ));
}

/// 在当前平台尝试创建目录符号链接；权限不足时由测试安全跳过。
fn try_symlink_directory(source: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(source, target).is_ok()
    }
}

/// 在当前平台尝试创建文件符号链接；权限不足时由测试安全跳过。
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
