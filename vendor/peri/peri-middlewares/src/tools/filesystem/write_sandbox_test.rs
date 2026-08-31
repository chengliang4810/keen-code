use super::WriteSandboxTool;
use peri_agent::tools::BaseTool;

/// 从错误消息中提取 draft_id。
fn extract_draft_id(err: &str) -> String {
    let re = regex::Regex::new(r"draft_[0-9a-f-]+").unwrap();
    re.find(err).unwrap().as_str().to_string()
}

/// 将目录权限改为只读(0o444),用于注入 tmp 写入失败
#[cfg(unix)]
fn make_readonly(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
}

/// 将目录权限还原为可写(0o755)
#[cfg(unix)]
fn make_writable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn make_tool(dir: &tempfile::TempDir, allowed: Vec<&str>) -> WriteSandboxTool {
    let cwd = dir.path().to_str().unwrap().to_string();
    // 先创建沙箱目录
    for d in &allowed {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    WriteSandboxTool::new(cwd, allowed.iter().map(|s| s.to_string()).collect()).unwrap()
}

#[tokio::test]
async fn test_write_sandbox_normal_create() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/hello.md", "content": "# Plan"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 1 line"));
    let content = std::fs::read_to_string(dir.path().join("sandbox/hello.md")).unwrap();
    assert_eq!(content, "# Plan");
}

#[tokio::test]
async fn test_write_sandbox_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    // 先写一次
    std::fs::write(dir.path().join("sandbox/v2.md"), "v1").unwrap();
    // 再覆盖写
    tool.invoke(
        serde_json::json!({"file_path": "sandbox/v2.md", "content": "v2"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("sandbox/v2.md")).unwrap();
    assert_eq!(content, "v2");
}

#[tokio::test]
async fn test_write_sandbox_dotdot_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/../outside.txt", "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), ".. 穿越应被拒绝");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox/../outside.txt"),
        "错误消息应包含完整路径: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_absolute_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let abs = dir.path().join("outside.txt");
    let result = tool
        .invoke(
            serde_json::json!({"file_path": abs.to_str().unwrap(), "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "绝对路径应被拒绝");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("绝对"), "错误消息应说明拒绝原因: {}", err);
}

#[tokio::test]
async fn test_write_sandbox_outside_dir_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "other/outside.txt", "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "沙箱外路径应被拒绝");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox") || err.contains("沙箱"),
        "错误消息应提示沙箱限制: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // 在沙箱外写入恶意文件
    std::fs::write(dir.path().join("outside.txt"), "evil").unwrap();
    #[cfg(unix)]
    {
        let tool = make_tool(&dir, vec!["sandbox"]);
        std::os::unix::fs::symlink(
            dir.path().join("outside.txt"),
            dir.path().join("sandbox/escape_link.txt"),
        )
        .unwrap();
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "sandbox/escape_link.txt", "content": "bypass"}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        assert!(result.is_err(), "symlink 逃逸应被拒绝");
    }
}

#[tokio::test]
async fn test_write_sandbox_parent_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // sandbox/sub 是外部目录的 symlink
    std::fs::create_dir_all(dir.path().join("outside_dir")).unwrap();
    #[cfg(unix)]
    {
        let tool = make_tool(&dir, vec!["sandbox"]);
        std::os::unix::fs::symlink(
            dir.path().join("outside_dir"),
            dir.path().join("sandbox/sub"),
        )
        .unwrap();
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "sandbox/sub/evil.txt", "content": "bypass"}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        assert!(result.is_err(), "父目录 symlink 逃逸应被拒绝");
    }
}

#[test]
fn test_write_sandbox_description_contains_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox", "output"]);
    let desc = tool.description();
    assert!(desc.contains("sandbox"));
    assert!(desc.contains("output"));
    assert!(desc.contains("Write a file ONLY into your sandbox directories"));
}

#[test]
fn test_write_sandbox_empty_allowed_dirs_ok() {
    let cwd = tempfile::tempdir().unwrap();
    let result = WriteSandboxTool::new(cwd.path().to_str().unwrap().to_string(), vec![]);
    // 空白名单应可构造（不注入时不报错）
    assert!(result.is_ok());
}

/// 构造阶段拒绝空目录、根目录、绝对目录和任何父目录跳转。
#[test]
fn test_write_sandbox_rejects_unsafe_allowed_directories() {
    let cwd = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let cwd = cwd.path().to_str().unwrap().to_string();
    let absolute = outside.path().to_str().unwrap().to_string();
    let unsafe_directories = [
        "".to_string(),
        ".".to_string(),
        "..".to_string(),
        "../outside".to_string(),
        "nested/../outside".to_string(),
        absolute,
    ];

    for directory in unsafe_directories {
        let result = WriteSandboxTool::new(cwd.clone(), vec![directory.clone()]);
        assert!(result.is_err(), "不安全的沙箱根目录应被拒绝: {directory}");
    }
}

/// 内置 Agent 的历史尾斜杠目录仍应保持可用。
#[test]
fn test_write_sandbox_accepts_relative_directory_with_trailing_separator() {
    let cwd = tempfile::tempdir().unwrap();
    let result = WriteSandboxTool::new(
        cwd.path().to_str().unwrap().to_string(),
        vec![".peri/plans/".to_string()],
    );

    assert!(result.is_ok(), "尾斜杠兼容路径应可构造: {:?}", result.err());
    assert!(cwd.path().join(".peri").join("plans").is_dir());
}

/// 创建跨平台目录符号链接，供沙箱根逃逸回归测试使用。
#[cfg(unix)]
fn create_directory_symlink(
    target: &std::path::Path,
    link: &std::path::Path,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 创建 Windows 目录符号链接，权限不足时调用方会跳过该平台能力测试。
#[cfg(windows)]
fn create_directory_symlink(
    target: &std::path::Path,
    link: &std::path::Path,
) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

/// 沙箱根本身解析到项目外部时，构造必须失败。
#[test]
fn test_write_sandbox_rejects_external_symlink_root() {
    let cwd = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let linked_root = cwd.path().join("linked-root");
    if create_directory_symlink(outside.path(), &linked_root).is_err() {
        return;
    }

    let result = WriteSandboxTool::new(
        cwd.path().to_str().unwrap().to_string(),
        vec!["linked-root".to_string()],
    );
    assert!(result.is_err(), "项目外部符号链接不能成为沙箱根");
}

/// 构造器不得先沿外部符号链接创建缺失子目录，再在事后校验时报错。
#[test]
fn test_write_sandbox_rejects_external_symlink_before_creating_child() {
    let cwd = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let linked_root = cwd.path().join("linked-root");
    if create_directory_symlink(outside.path(), &linked_root).is_err() {
        return;
    }

    let result = WriteSandboxTool::new(
        cwd.path().to_str().unwrap().to_string(),
        vec!["linked-root/new".to_string()],
    );

    assert!(result.is_err(), "项目外部符号链接子目录必须在创建前被拒绝");
    assert!(
        !outside.path().join("new").exists(),
        "拒绝构造时不得在项目外产生目录副作用"
    );
}

#[tokio::test]
async fn test_write_sandbox_multi_dir() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans", "output"]);
    tool.invoke(
        serde_json::json!({"file_path": "plans/design.md", "content": "# Design"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    tool.invoke(
        serde_json::json!({"file_path": "output/result.json", "content": "{\"ok\": true}"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert!(dir.path().join("plans/design.md").exists());
    assert!(dir.path().join("output/result.json").exists());
}

/// [回归测试] 沙箱目录不存在时构造应自动创建，而非失败。
/// 对应 spec/issues/2026-07-20-plan-agent-writesandbox-not-found.md
#[test]
fn test_write_sandbox_auto_create_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    // 不预创建沙箱目录——WriteSandboxTool::new 应自动创建
    assert!(!dir.path().join("plans").exists(), "开始前沙箱目录不应存在");
    let result = WriteSandboxTool::new(cwd, vec!["plans".into()]);
    assert!(result.is_ok(), "目录不存在时构造应成功: {:?}", result.err());
    // 验证目录确实被创建
    assert!(
        dir.path().join("plans").is_dir(),
        "构造后沙箱目录应被自动创建"
    );
    // 验证可正常写入
    let tool = result.unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(tool.invoke(
        serde_json::json!({"file_path": "plans/test.md", "content": "# Auto created"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    ))
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("plans/test.md")).unwrap();
    assert_eq!(content, "# Auto created");
}

/// [回归测试] 错误消息中的允许目录应使用原始相对路径，而非 canonicalized 绝对路径。
/// 对应 spec/issues/2026-07-20-writesandbox-still-confused-with-write.md 修复 #2
#[tokio::test]
async fn test_write_sandbox_error_displays_relative_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans"]);
    // 裸文件名——已有祖先（cwd）不在沙箱目录内
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "bare.md", "content": "test"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    // 错误消息应包含相对路径 "plans"，而非 canonicalized 绝对路径
    assert!(
        err.contains("\"plans\""),
        "错误消息应展示相对路径 'plans'，而非绝对路径: {}",
        err
    );
    // 不应包含 tempdir 的绝对路径
    let abs_path = dir.path().display().to_string();
    assert!(
        !err.contains(&abs_path),
        "错误消息不应包含绝对路径 '{}': {}",
        abs_path,
        err
    );
}

// ===== 失败草稿恢复机制测试(决策 6) =====

/// 替换失败必须保存可恢复草稿，并在所有平台清理临时文件。
#[tokio::test]
async fn test_write_sandbox_replace_failure_saves_draft_and_cleans_tmp() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    let target = directory.path().join("sandbox/d");
    std::fs::create_dir(&target).unwrap();

    let error = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/d", "content": "x\ny"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("rename 临时文件失败"),
        "应保留替换阶段错误文案: {error}"
    );
    assert!(error.contains("draft_"), "替换失败应保存草稿: {error}");
    assert_eq!(
        std::fs::read_dir(directory.path().join("sandbox"))
            .unwrap()
            .count(),
        1,
        "替换失败后只能保留原目标目录"
    );

    std::fs::remove_dir(&target).unwrap();
    let draft_id = extract_draft_id(&error);
    tool.invoke(
        serde_json::json!({"file_path": "sandbox/d", "from_draft": draft_id}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "x\ny");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_tmp_failure_saves_draft() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    // 沙箱目录只读 → tmp 写入失败
    make_readonly(&dir.path().join("sandbox"));
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "content": "hello\nworld"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("内容草稿已保存"), "应含中文草稿提示: {err}");
    assert!(err.contains("draft_"), "应含 draft_id: {err}");
    assert!(err.contains("2 行"), "应含行数: {err}");
    assert!(err.contains("11 字节"), "应含字节数: {err}");
    assert!(!err.contains("hello"), "错误消息不应展示正文: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_from_draft_restores() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    make_readonly(&dir.path().join("sandbox"));
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "content": "hello\nworld"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let draft_id = extract_draft_id(&err);
    // 还原权限后 from_draft 恢复
    make_writable(&dir.path().join("sandbox"));
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 2 lines"), "恢复消息: {result}");
    let content = std::fs::read_to_string(dir.path().join("sandbox/f.txt")).unwrap();
    assert_eq!(content, "hello\nworld");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_from_draft_runs_full_validation() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    make_readonly(&dir.path().join("sandbox"));
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/a.txt", "content": "payload"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let draft_id = extract_draft_id(&err);
    // from_draft 恢复必须走完整校验链:穿越路径被拒,而非草稿错误
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/../outside.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("拒绝"), "应命中校验链: {err}");
    assert!(!err.contains("草稿"), "不应是草稿错误: {err}");
    // 草稿未被消费,原路径仍可恢复
    make_writable(&dir.path().join("sandbox"));
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/a.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "原路径应仍可恢复: {:?}", result.err());
    let content = std::fs::read_to_string(dir.path().join("sandbox/a.txt")).unwrap();
    assert_eq!(content, "payload");
}

#[tokio::test]
async fn test_write_sandbox_requires_content_or_from_draft() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("必须提供"), "缺参数文案: {err}");
}

/// 回归（互斥劫持）：同时携带 content 与 from_draft 时 content 优先写入成功
/// （原「互斥报错」导致文件无法落盘、模型被迫重输出一遍内容）。
#[tokio::test]
async fn test_write_sandbox_content_and_from_draft_mutually_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    let result = tool
        .invoke(
            serde_json::json!({
                "file_path": "sandbox/f.txt",
                "content": "x",
                "from_draft": "draft_00000000-0000-7000-0000-000000000000"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("x"), "应以 content 优先写入: {result}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sandbox/f.txt")).unwrap(),
        "x",
        "文件应落盘 content 内容"
    );
}

/// 回归（占位符）：from_draft 填占位符等同未提供，content 生效
#[tokio::test]
async fn test_write_sandbox_from_draft_placeholder_uses_content() {
    for placeholder in ["", "__omit__"] {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
        let result = tool
            .invoke(
                serde_json::json!({
                    "file_path": "sandbox/f.txt",
                    "content": "real",
                    "from_draft": placeholder,
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert!(
            result.contains("Wrote"),
            "占位符 from_draft {:?} 应等同未提供并写入成功: {}",
            placeholder,
            result
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sandbox/f.txt")).unwrap(),
            "real",
            "占位符 from_draft {:?} 时文件应落盘 content 内容",
            placeholder
        );
    }
}

#[tokio::test]
async fn test_write_sandbox_from_draft_unknown_degrades() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    let err = tool
        .invoke(
            serde_json::json!({
                "file_path": "sandbox/f.txt",
                "from_draft": "draft_00000000-0000-7000-0000-000000000000"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("不存在或已失效"), "未知草稿降级文案: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_draft_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], false).unwrap();
    make_readonly(&dir.path().join("sandbox"));
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "content": "hello"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("draft_"), "禁用时不应存草稿: {err}");
}

// ===== 外部沙箱模式测试（桌面 PERI_SANDBOX_WRITE_BASE 模式）=====

/// 外部沙箱模式测试辅助：构造工具时传入显式 `external_base`（传 `Some(base.path())`
/// 以模拟桌面设置 `PERI_SANDBOX_WRITE_BASE`）。`allowed_dirs` 含 `.peri/plans/` 证明被忽略。
fn make_external_tool(project: &tempfile::TempDir, base: &tempfile::TempDir) -> WriteSandboxTool {
    let cwd = project.path().to_str().unwrap().to_string();
    // 项目模式的 allowed_dirs 在外部模式下被忽略
    WriteSandboxTool::with_draft_and_base(
        cwd,
        vec![".peri/plans/".into()],
        false,
        Some(base.path().to_path_buf()),
    )
    .unwrap()
}

#[tokio::test]
async fn test_external_sandbox_writes_outside_project() {
    let project = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let tool = make_external_tool(&project, &base);

    tool.invoke(
        serde_json::json!({"file_path": "plan.md", "content": "# External Plan"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    // base 下有且仅有 1 个项目子目录
    let entries: Vec<_> = std::fs::read_dir(base.path())
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(entries.len(), 1, "base 下应只有一个项目子目录");
    // 项目目录内没有 .peri/ 或任何其他内容
    assert_eq!(
        std::fs::read_dir(project.path())
            .unwrap()
            .map(|e| e.unwrap())
            .count(),
        0,
        "项目目录应保持干净（外部沙箱模式不在项目内写入）"
    );
    // description 含外部路径且不含 .peri
    let desc = tool.description();
    assert!(
        desc.contains("sandbox directory"),
        "外部模式 description 应含单数 sandbox directory: {}",
        desc
    );
    assert!(
        !desc.contains(".peri"),
        "外部模式 description 不应含项目内路径 .peri: {}",
        desc
    );
}

#[tokio::test]
async fn test_external_sandbox_rejects_absolute_and_dotdot() {
    let project = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let tool = make_external_tool(&project, &base);

    // 外部模式同样拒绝绝对路径；测试输入必须符合当前平台的绝对路径语法。
    let absolute_path = if cfg!(windows) {
        r"C:\tmp\evil.txt"
    } else {
        "/tmp/evil.txt"
    };
    let abs_err = tool
        .invoke(
            serde_json::json!({"file_path": absolute_path, "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(abs_err.contains("绝对路径"), "拒绝绝对: {abs_err}");

    // 外部模式同样拒绝路径穿越
    let dotdot_err = tool
        .invoke(
            serde_json::json!({"file_path": "../escape.txt", "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        dotdot_err.contains("路径穿越"),
        "拒绝路径穿越: {dotdot_err}"
    );

    // 构造器创建了一个项目子目录，但拒绝写入后该目录内应无文件
    let base_entries: Vec<_> = std::fs::read_dir(base.path())
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(base_entries.len(), 1, "base 下仅一个项目子目录");
    let key_dir = &base_entries[0].path();
    assert_eq!(
        std::fs::read_dir(key_dir).unwrap().count(),
        0,
        "拒绝写入后项目子目录应为空"
    );
}

#[test]
fn test_external_sandbox_key_is_stable_and_project_scoped() {
    use super::project_sandbox_key;
    let project_a = "/Users/demo/project-a";
    let project_b = "/Users/demo/project-b";
    let key_a1 = project_sandbox_key(project_a);
    let key_a2 = project_sandbox_key(project_a);
    let key_b = project_sandbox_key(project_b);

    // 同项目同键
    assert_eq!(
        key_a1, key_a2,
        "同项目应生成相同键（稳定性保证持久沙箱目录一致）"
    );
    // 异项目异键（哈希碰撞概率低，单测覆盖常见场景）
    assert_ne!(
        key_a1, key_b,
        "异项目应生成不同键（隔离保证多项目沙箱互不干扰）"
    );
    // 格式：<name>-<hash8>
    assert!(key_a1.contains('-'), "键格式应含分隔符");
    assert_eq!(key_a1.split('-').last().unwrap().len(), 8, "哈希后缀 8 位");
}

#[test]
fn test_project_sandbox_key_sanitizes_names() {
    use super::project_sandbox_key;
    // 非安全字符被过滤
    let key = project_sandbox_key("/Users/demo/我的 项目!/");
    let name_part = key.split('-').next().unwrap();
    assert!(
        name_part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "目录名应只含安全字符: {}",
        name_part
    );
    // 归一化保证 "/x/" 与 "/x" 同键（避免尾斜杠敏感）
    assert_eq!(
        project_sandbox_key("/x/"),
        project_sandbox_key("/x"),
        "归一化后尾斜杠不应影响键"
    );
}
