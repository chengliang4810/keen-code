use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::json_schema_suggester::JsonSchemaSuggester;

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

    fn ctx<'a>(&'a self, tool_name: &'a str, err: &'a str) -> ErrorContext<'a> {
        ErrorContext::new(
            tool_name,
            &self.input,
            err,
            std::path::Path::new("."),
            &self.snap,
        )
    }
}

#[test]
fn test_json_schema_recognizes_missing_field() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let ctx = holder.ctx("Read", "The 'file_path' parameter is required.");
    let result = JsonSchemaSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert!(
        sug.summary.contains("file_path"),
        "缺少必需参数字段名，实际：{}",
        sug.summary
    );
}

#[test]
fn test_json_schema_recognizes_invalid_type() {
    let holder = CtxHolder::new(serde_json::json!({ "offset": "abc" }));
    let ctx = holder.ctx("Read", "Error: invalid type: string \"abc\", expected u64");
    let result = JsonSchemaSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    // 错误消息中不含字段名"offset"，regex 从错误消息提取提示词
    assert!(
        sug.summary.contains("u64"),
        "应该提示期望类型 u64，实际：{}",
        sug.summary
    );
}

#[test]
fn test_json_schema_recognizes_serde_missing_field() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let ctx = holder.ctx("Write", "Error: missing field `file_path`");
    let result = JsonSchemaSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert!(
        sug.summary.contains("file_path"),
        "应该提示缺失的 file_path，实际：{}",
        sug.summary
    );
}

#[test]
fn test_json_schema_skips_non_schema_errors() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let ctx = holder.ctx("Read", "Error: File not found at /tmp/foo");
    assert!(JsonSchemaSuggester.suggest(&ctx).is_none());
}
