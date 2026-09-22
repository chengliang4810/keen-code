use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::AppHandle;

/// 全局自定义指令文件的最大字符数。
pub const MAX_CUSTOM_INSTRUCTIONS_CHARS: usize = 12_000;

/// 自动注入单个项目指令文件的最大字节数，避免启动时读取任意大的文件。
const MAX_PROJECT_INSTRUCTIONS_BYTES: usize = 128 * 1024;

/// 串行化全局自定义指令读写，避免并发保存互相覆盖。
static CUSTOM_INSTRUCTIONS_IO_LOCK: Mutex<()> = Mutex::new(());

/// 读取当前设备唯一的全局用户自定义指令；首次使用时为空。
pub fn get(app: &AppHandle) -> Result<String> {
    let _guard = CUSTOM_INSTRUCTIONS_IO_LOCK
        .lock()
        .expect("自定义指令读写锁已损坏");
    load_path(&custom_instructions_path(app)?)
}

/// 校验并保存当前设备唯一的全局用户自定义指令。
pub fn set(app: &AppHandle, instructions: String) -> Result<String> {
    let _guard = CUSTOM_INSTRUCTIONS_IO_LOCK
        .lock()
        .expect("自定义指令读写锁已损坏");
    validate(&instructions)?;
    save_path(&custom_instructions_path(app)?, instructions.as_bytes())?;
    Ok(instructions)
}

/// 读取当前设备唯一的全局用户自定义指令（IPC 入口）。
#[tauri::command]
pub fn custom_instructions_get(app: AppHandle) -> Result<String, String> {
    get(&app).map_err(|error| error.to_string())
}

/// 校验并保存当前设备唯一的全局用户自定义指令（IPC 入口）。
#[tauri::command]
pub fn custom_instructions_set(app: AppHandle, instructions: String) -> Result<String, String> {
    set(&app, instructions).map_err(|error| error.to_string())
}

/// 校验全局自定义指令的 Unicode 字符预算。
fn validate(instructions: &str) -> Result<()> {
    if instructions.chars().count() > MAX_CUSTOM_INSTRUCTIONS_CHARS {
        anyhow::bail!("自定义指令不能超过 {MAX_CUSTOM_INSTRUCTIONS_CHARS} 个字符");
    }
    Ok(())
}

/// 返回当前隔离数据根中的唯一全局指令路径。
fn custom_instructions_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join("AGENTS.md"))
}

/// 先按 UTF-8 最大字节数有界读取，再校验全局指令的实际字符数。
fn load_path(path: &Path) -> Result<String> {
    let instructions = read_instruction_file(path, MAX_CUSTOM_INSTRUCTIONS_CHARS * 4)?;
    validate(&instructions)?;
    Ok(instructions)
}

/// 为主 Agent 与子 Agent 装配同一份当前指令；正文只进入请求上下文，不写 Transcript。
///
/// 按优先级选择一个项目主文件，再追加 CLAUDE.local.md；保留原文，不添加包装。
pub(crate) fn prompt_context(
    data_root: &Path,
    project_root: &Path,
) -> Result<(String, Option<String>)> {
    let _guard = CUSTOM_INSTRUCTIONS_IO_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("自定义指令读写锁不可用"))?;
    let global = load_path(&data_root.join("AGENTS.md"))?;
    let mut project = String::new();
    for name in ["AGENTS.md", "CLAUDE.md", ".claude/AGENTS.md"] {
        let path = project_root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                project = read_instruction_file(&path, MAX_PROJECT_INSTRUCTIONS_BYTES)?;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("读取项目指令文件元数据失败"),
        }
    }
    let local = read_instruction_file(
        &project_root.join("CLAUDE.local.md"),
        MAX_PROJECT_INSTRUCTIONS_BYTES,
    )?;
    if !local.trim().is_empty() {
        if project.trim().is_empty() {
            project = local;
        } else {
            project.push_str("\n\n");
            project.push_str(&local);
        }
    }
    Ok((global, (!project.trim().is_empty()).then_some(project)))
}

/// 读取可选的 UTF-8 普通指令文件；缺失为空，损坏或超过预算明确报错而不静默裁剪。
fn read_instruction_file(path: &Path, maximum_bytes: usize) -> Result<String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error).context("读取指令文件元数据失败"),
    };
    anyhow::ensure!(metadata.file_type().is_file(), "指令路径必须是普通文件");
    anyhow::ensure!(
        metadata.len() <= maximum_bytes as u64,
        "指令文件超过读取预算"
    );
    let file = crate::storage::open_readonly_regular_file(path).context("打开指令文件失败")?;
    anyhow::ensure!(file.metadata()?.is_file(), "指令路径必须是普通文件");
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= maximum_bytes, "指令文件超过读取预算");
    String::from_utf8(bytes).context("指令文件必须使用 UTF-8 编码")
}

/// 通过统一私有原子写入口保存当前指令，避免只写入半段提示词。
fn save_path(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::storage::atomic_write_private(path, bytes)
        .with_context(|| format!("保存自定义指令失败：{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_CUSTOM_INSTRUCTIONS_CHARS, MAX_PROJECT_INSTRUCTIONS_BYTES, load_path, prompt_context,
        read_instruction_file, save_path, validate,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_directory(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应有效")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "keencode-personalization-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn missing_file_is_empty_and_saved_content_roundtrips() {
        let directory = test_directory("roundtrip");
        let path = directory.join("AGENTS.md");

        assert_eq!(load_path(&path).expect("缺失文件应为空"), "");
        save_path(&path, "使用中文回答".as_bytes()).expect("应保存自定义指令");
        assert_eq!(load_path(&path).expect("应读取自定义指令"), "使用中文回答");
        save_path(&path, "保持简洁".as_bytes()).expect("应覆盖已有自定义指令");
        assert_eq!(load_path(&path).expect("应读取新指令"), "保持简洁");

        fs::remove_file(&path).expect("清理测试文件");
        fs::remove_dir(&directory).expect("清理测试目录");
    }

    #[test]
    fn oversized_instructions_are_rejected() {
        assert!(validate(&"指".repeat(MAX_CUSTOM_INSTRUCTIONS_CHARS)).is_ok());
        assert!(validate(&"指".repeat(MAX_CUSTOM_INSTRUCTIONS_CHARS + 1)).is_err());
    }

    /// 全局和项目规则同时生效，项目规则排在全局偏好之后，后续读取采用当前正文。
    #[test]
    fn runtime_prompt_context_loads_current_global_and_project_instructions() {
        let data = tempfile::tempdir().expect("应创建隔离数据根");
        let project = tempfile::tempdir().expect("应创建测试项目");
        assert_eq!(
            prompt_context(data.path(), project.path()).unwrap(),
            (String::new(), None)
        );
        save_path(&data.path().join("AGENTS.md"), "全局规则甲".as_bytes()).unwrap();
        fs::write(project.path().join("AGENTS.md"), "项目规则乙").unwrap();
        let (global, first) = prompt_context(data.path(), project.path()).unwrap();
        assert_eq!(global, "全局规则甲");
        let first = first.unwrap();
        assert!(first.contains("项目规则乙"));
        assert!(!first.contains("全局规则甲"));
        assert_eq!(first, "项目规则乙");
        save_path(&data.path().join("AGENTS.md"), "全局规则丙".as_bytes()).unwrap();
        let (global, _) = prompt_context(data.path(), project.path()).unwrap();
        assert_eq!(global, "全局规则丙");
    }

    #[test]
    fn project_instructions_select_one_main_and_append_local_verbatim() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        fs::create_dir(project.path().join(".claude")).unwrap();
        for (name, text) in [
            ("AGENTS.md", "  agents\n"),
            ("CLAUDE.md", "claude"),
            (".claude/AGENTS.md", "nested"),
            ("CLAUDE.local.md", "local\n"),
        ] {
            fs::write(project.path().join(name), text).unwrap();
        }
        let read = || prompt_context(data.path(), project.path()).unwrap().1;
        assert_eq!(read().as_deref(), Some("  agents\n\n\nlocal\n"));
        fs::write(project.path().join("AGENTS.md"), "").unwrap();
        assert_eq!(read().as_deref(), Some("local\n"));
        fs::remove_file(project.path().join("AGENTS.md")).unwrap();
        assert_eq!(read().as_deref(), Some("claude\n\nlocal\n"));
        fs::remove_file(project.path().join("CLAUDE.md")).unwrap();
        assert_eq!(read().as_deref(), Some("nested\n\nlocal\n"));
        fs::remove_file(project.path().join(".claude/AGENTS.md")).unwrap();
        assert_eq!(read().as_deref(), Some("local\n"));
        fs::write(project.path().join("CLAUDE.local.md"), [0xff]).unwrap();
        assert!(prompt_context(data.path(), project.path()).is_err());
    }

    /// 超大、非 UTF-8 和目录路径必须拒绝，不能当作缺失指令静默运行。
    #[test]
    fn runtime_instruction_files_are_bounded_and_fail_explicitly() {
        let directory = tempfile::tempdir().expect("应创建指令测试目录");
        let path = directory.path().join("AGENTS.md");
        fs::write(&path, vec![b'a'; MAX_PROJECT_INSTRUCTIONS_BYTES + 1]).unwrap();
        assert!(read_instruction_file(&path, MAX_PROJECT_INSTRUCTIONS_BYTES).is_err());
        fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(read_instruction_file(&path, MAX_PROJECT_INSTRUCTIONS_BYTES).is_err());
        assert!(read_instruction_file(directory.path(), MAX_PROJECT_INSTRUCTIONS_BYTES).is_err());
    }
}
