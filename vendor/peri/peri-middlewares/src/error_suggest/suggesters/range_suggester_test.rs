use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::range_suggester::RangeSuggester;

struct CtxHolder {
    input: serde_json::Value,
    snap: ToolRegistrySnapshot,
}

impl CtxHolder {
    fn new(input: serde_json::Value) -> Self {
        Self {
            input,
            snap: ToolRegistrySnapshot::default(),
        }
    }

    fn ctx<'a>(
        &'a self,
        tool_name: &'a str,
        err: &'a str,
        cwd: &'a std::path::Path,
    ) -> ErrorContext<'a> {
        ErrorContext::new(tool_name, &self.input, err, cwd, &self.snap)
    }
}

#[test]
fn test_range_suggester_only_for_read() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx(
        "Edit",
        "Error: offset 100 exceeds file length (50 lines)",
        cwd,
    );
    assert!(RangeSuggester.suggest(&ctx).is_none());
}

#[test]
fn test_range_suggester_recognizes_offset_error() {
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "/tmp/foo.rs",
        "offset": 100,
        "limit": 10,
    }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx(
        "Read",
        "Error: offset 100 exceeds file length (50 lines)",
        cwd,
    );
    let result = RangeSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert_eq!(
        sug.summary,
        "请求的 offset 100 超出文件范围（文件共 50 行）。建议把 offset 改为 1（从头读）或小于 50 的值，配合 limit 控制读取范围。"
    );
}

#[test]
fn test_range_suggester_skips_non_range_errors() {
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "/tmp/foo.rs",
    }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx("Read", "Error: File not found", cwd);
    assert!(RangeSuggester.suggest(&ctx).is_none());
}
