use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};
use regex::Regex;
use std::sync::OnceLock;

/// B2：Read 工具 offset/limit 越界建议
pub struct RangeSuggester;

impl ErrorSuggester for RangeSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        if ctx.tool_name != "Read" {
            return None;
        }

        // 识别 "offset X exceeds file length (Y lines)" 错误
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(r"offset\s+(\d+)\s+exceeds file length\s+\((\d+)\s+lines\)").unwrap()
        });
        let caps = re.captures(ctx.error_message)?;

        let requested: u64 = caps[1].parse().ok()?;
        let total: u64 = caps[2].parse().ok()?;

        Some(Suggestion::new(format!(
            "请求的 offset {requested} 超出文件范围（文件共 {total} 行）。建议把 offset 改为 1（从头读）或小于 {total} 的值，配合 limit 控制读取范围。"
        )))
    }
}
