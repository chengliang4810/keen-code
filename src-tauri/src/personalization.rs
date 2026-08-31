use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::AppHandle;

/// 全局自定义指令文件的最大字符数。
pub const MAX_CUSTOM_INSTRUCTIONS_CHARS: usize = 12_000;

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

fn validate(instructions: &str) -> Result<()> {
    if instructions.chars().count() > MAX_CUSTOM_INSTRUCTIONS_CHARS {
        anyhow::bail!("自定义指令不能超过 {MAX_CUSTOM_INSTRUCTIONS_CHARS} 个字符");
    }
    Ok(())
}

fn custom_instructions_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join("AGENTS.md"))
}

fn load_path(path: &Path) -> Result<String> {
    let instructions = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("读取自定义指令失败：{}", path.display()));
        }
    };
    validate(&instructions)?;
    Ok(instructions)
}

/// 通过统一私有原子写入口保存当前指令，避免只写入半段提示词。
fn save_path(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::storage::atomic_write_private(path, bytes)
        .with_context(|| format!("保存自定义指令失败：{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{MAX_CUSTOM_INSTRUCTIONS_CHARS, load_path, save_path, validate};
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
}
