//! KeenCode 后端诊断日志。
//!
//! 诊断日志的目标是让启动、IPC、ACP 传输和供应商配置问题可以通过运行记录定位，
//! 而不是依赖重新猜测代码路径。敏感值和完整请求正文不会写入日志。

use std::fs::{OpenOptions, create_dir_all};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tauri::AppHandle;

/// 后端诊断日志句柄。
pub struct Diagnostics {
    /// 当前日志文件的绝对路径。
    path: PathBuf,
    /// 进程内串行写入锁。
    file: Mutex<std::fs::File>,
    /// 与本进程所有启动阶段共享的单调时钟起点。
    startup_started_at: Instant,
}

impl Diagnostics {
    /// 根据当前用户的 KeenCode 统一目录创建诊断日志。
    pub fn init(app: &AppHandle, startup_started_at: Instant) -> Arc<Self> {
        let data_dir = crate::storage::root_dir(app).unwrap_or_else(|error| {
            eprintln!("[keencode] 无法获取用户持久化目录，诊断日志回退临时目录: {error}");
            std::env::temp_dir().join("keencode-desktop-data")
        });
        let log_dir = data_dir.join("logs");
        let path = log_dir.join("keencode-desktop.log");
        let file = match open_log_file(&log_dir, &path) {
            Ok(file) => file,
            Err(error) => {
                eprintln!("[keencode] 无法打开诊断日志 {}: {error}", path.display());
                let fallback_dir = std::env::temp_dir().join("keencode-desktop-logs");
                let fallback_path = fallback_dir.join("keencode-desktop.log");
                match open_log_file(&fallback_dir, &fallback_path) {
                    Ok(file) => {
                        return Arc::new(Self {
                            path: fallback_path,
                            file: Mutex::new(file),
                            startup_started_at,
                        });
                    }
                    Err(fallback_error) => {
                        eprintln!("[keencode] 临时诊断日志也无法打开: {fallback_error}");
                        let fallback_path = std::env::temp_dir().join("keencode-desktop.log");
                        return Arc::new(Self {
                            path: fallback_path.clone(),
                            file: Mutex::new(
                                OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(fallback_path)
                                    .expect("无法创建任何诊断日志文件"),
                            ),
                            startup_started_at,
                        });
                    }
                }
            }
        };
        Arc::new(Self {
            path,
            file: Mutex::new(file),
            startup_started_at,
        })
    }

    /// 记录可由本地基准脚本稳定解析的启动阶段。
    pub fn startup_phase(&self, phase: &str) {
        let elapsed_ms = self.startup_started_at.elapsed().as_millis();
        self.log(
            "info",
            "startup.metric",
            format!("phase={phase} elapsed_ms={elapsed_ms}"),
        );
        if std::env::var_os("KEENCODE_BENCHMARK").as_deref() == Some(std::ffi::OsStr::new("1")) {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": phase,
                    "elapsedMs": elapsed_ms,
                })
            );
        }
    }

    /// 返回日志文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 写入一条结构化文本日志。
    pub fn log(&self, level: &str, component: &str, message: impl AsRef<str>) {
        let timestamp = unix_timestamp_millis();
        let line = format!(
            "{timestamp} level={level} component={component} message={}\n",
            sanitize_text(message.as_ref())
        );
        let Ok(mut file) = self.file.lock() else {
            eprintln!("[keencode] 诊断日志锁已损坏: {}", self.path.display());
            return;
        };
        if let Err(error) = file.write_all(line.as_bytes()).and_then(|_| file.flush()) {
            eprintln!(
                "[keencode] 写入诊断日志失败 {}: {error}",
                self.path.display()
            );
        }
    }

    /// 写入 JSON-RPC 方法摘要，不记录完整参数值。
    pub fn rpc(&self, direction: &str, method: &str, params: &Value) {
        let event_summary = summarize_acp_event_for_log(method, params);
        self.log(
            "info",
            "acp.rpc",
            format!(
                "direction={} method={} params={}{}",
                direction,
                method,
                summarize_value_for_log(params),
                event_summary
                    .map(|summary| format!(" {summary}"))
                    .unwrap_or_default()
            ),
        );
    }

    /// 写入异常摘要并对常见密钥格式做脱敏。
    pub fn error(&self, component: &str, error: impl AsRef<str>) {
        self.log("error", component, error);
    }
}

/// 创建日志目录并打开追加写入文件。
fn open_log_file(log_dir: &Path, path: &Path) -> std::io::Result<std::fs::File> {
    create_dir_all(log_dir)?;
    OpenOptions::new().create(true).append(true).open(path)
}

/// 返回 Unix 毫秒时间戳，避免额外依赖并保证日志在启动早期可用。
fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// 只保留可定位问题的文本，避免换行伪造日志记录。
fn sanitize_text(value: &str) -> String {
    let mut text = value.replace('\n', "\\n").replace('\r', "\\r");
    text = redact_bearer(&text);
    for marker in [
        "api_key",
        "apiKey",
        "authorization",
        "Authorization",
        "token",
        "password",
        "secret",
    ] {
        text = redact_after_marker(&text, marker);
    }
    if text.len() > 4_000 {
        let mut end = 4_000;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...(truncated)");
    }
    text
}

/// 将敏感字段的值替换为固定占位符。
fn redact_after_marker(input: &str, marker: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find(marker) {
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        let after_marker = start + marker.len();
        output.push_str(marker);
        let Some(separator_relative) = input[after_marker..].find([':', '=']) else {
            cursor = after_marker;
            continue;
        };
        let separator = after_marker + separator_relative;
        output.push_str(&input[after_marker..=separator]);
        let mut value_start = separator + 1;
        while value_start < input.len() && input.as_bytes()[value_start].is_ascii_whitespace() {
            value_start += 1;
        }
        output.push_str(&input[separator + 1..value_start]);
        output.push_str("<redacted>");
        let mut end = value_start;
        if end < input.len() && matches!(input.as_bytes()[end], b'\'' | b'"') {
            let quote = input.as_bytes()[end];
            end += 1;
            while end < input.len() {
                match input.as_bytes()[end] {
                    b'\\' => end = (end + 2).min(input.len()),
                    byte if byte == quote => {
                        end += 1;
                        break;
                    }
                    _ => end += 1,
                }
            }
        } else {
            while end < input.len() && !matches!(input.as_bytes()[end], b' ' | b',' | b'}' | b']') {
                end += 1;
            }
        }
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

/// 脱敏 HTTP Bearer 认证值。
fn redact_bearer(input: &str) -> String {
    let marker = "Bearer ";
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find(marker) {
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        output.push_str("Bearer <redacted>");
        let mut end = start + marker.len();
        while end < input.len() && !matches!(input.as_bytes()[end], b' ' | b',' | b'}' | b']') {
            end += 1;
        }
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

/// 递归生成 JSON 结构摘要，只输出键名、类型和长度。
pub(crate) fn summarize_value_for_log(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(text) => format!("string(len={})", text.len()),
        Value::Array(items) => format!(
            "array(len={}, items={})",
            items.len(),
            items
                .first()
                .map(summarize_value_for_log)
                .unwrap_or_else(|| "none".to_string())
        ),
        Value::Object(object) => {
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            format!("object(keys=[{}])", keys.join(","))
        }
    }
}

/// 为 ACP 会话事件补充可诊断但不包含正文的结构摘要。
fn summarize_acp_event_for_log(method: &str, params: &Value) -> Option<String> {
    if method != "session/update" {
        return None;
    }
    let update = params.get("update")?;
    let update_tag = update.get("sessionUpdate")?.as_str()?;
    let mut parts = vec![format!("update_tag={update_tag}")];
    let content = update.get("content");
    if let Some(content) = content {
        if let Some(chunk_type) = content.get("type").and_then(Value::as_str) {
            parts.push(format!("chunk_type={chunk_type}"));
        }
        if let Some(text) = content.get("text").and_then(Value::as_str) {
            parts.push(format!("text_len={}", text.len()));
        }
    }
    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::{sanitize_text, summarize_acp_event_for_log, summarize_value_for_log};
    use serde_json::json;

    #[test]
    fn redacts_secret_like_values() {
        let text = sanitize_text("api_key=sk-test Authorization: Bearer secret-value");
        assert!(!text.contains("sk-test"));
        assert!(!text.contains("secret-value"));
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn redacts_whitespace_and_quoted_secret_values() {
        let text = sanitize_text(
            r#"{"apiKey": "sk-json-secret", "password": 'two words'} token = plain-secret"#,
        );
        assert!(!text.contains("sk-json-secret"));
        assert!(!text.contains("two words"));
        assert!(!text.contains("plain-secret"));
        assert_eq!(text.matches("<redacted>").count(), 3);
    }

    #[test]
    fn truncates_unicode_diagnostics_on_a_character_boundary() {
        let text = sanitize_text(&"界".repeat(2_000));
        assert!(text.ends_with("...(truncated)"));
        assert!(text.len() <= 4_000 + "...(truncated)".len());
    }

    #[test]
    fn summarizes_json_without_values() {
        let summary = summarize_value_for_log(&json!({ "apiKey": "hidden", "prompt": "private" }));
        assert!(summary.contains("apiKey"));
        assert!(!summary.contains("hidden"));
        assert!(!summary.contains("private"));
    }

    #[test]
    fn summarizes_acp_text_event_without_content() {
        let params = json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "private reply" }
            }
        });
        let summary = summarize_acp_event_for_log("session/update", &params).unwrap();
        assert_eq!(
            summary,
            "update_tag=agent_message_chunk chunk_type=text text_len=13"
        );
        assert!(!summary.contains("private reply"));
    }
}
