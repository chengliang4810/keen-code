use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::{ErrorSuggestRegistry, ErrorSuggester, Suggestion};
use std::collections::HashSet;

// 一个总是返回 Some 的测试 suggester
struct AlwaysSuggest {
    label: &'static str,
}

impl ErrorSuggester for AlwaysSuggest {
    fn suggest(&self, _ctx: &ErrorContext) -> Option<Suggestion> {
        Some(Suggestion {
            summary: format!("来自 {}", self.label),
            details: None,
        })
    }
}

// 一个总是返回 None 的测试 suggester
struct NeverSuggest;

impl ErrorSuggester for NeverSuggest {
    fn suggest(&self, _ctx: &ErrorContext) -> Option<Suggestion> {
        None
    }
}

#[test]
fn test_registry_short_circuits_on_first_hit() {
    let registry = ErrorSuggestRegistry::new(vec![
        Box::new(AlwaysSuggest { label: "first" }),
        Box::new(AlwaysSuggest { label: "second" }),
    ]);

    let snap = ToolRegistrySnapshot {
        all_tool_names: HashSet::new(),
        subagent_types: HashSet::new(),
    };
    let tool_name: &'static str = "Read";
    let input = serde_json::json!({});
    let err: &'static str = "Error: File not found";
    let cwd = std::path::Path::new(".");
    let ctx = ErrorContext::new(tool_name, &input, err, cwd, &snap);

    let result = registry.suggest(&ctx);
    assert!(result.is_some());
    assert_eq!(result.unwrap().summary, "来自 first");
}

#[test]
fn test_registry_returns_none_when_all_miss() {
    let registry = ErrorSuggestRegistry::new(vec![Box::new(NeverSuggest), Box::new(NeverSuggest)]);

    let snap = ToolRegistrySnapshot {
        all_tool_names: HashSet::new(),
        subagent_types: HashSet::new(),
    };
    let input = serde_json::json!({});
    let err: &'static str = "Error: unknown";
    let cwd = std::path::Path::new(".");
    let ctx = ErrorContext::new("Read", &input, err, cwd, &snap);

    let result = registry.suggest(&ctx);
    assert!(result.is_none());
}

#[test]
fn test_registry_falls_through_to_next_when_first_misses() {
    let registry = ErrorSuggestRegistry::new(vec![
        Box::new(NeverSuggest),
        Box::new(AlwaysSuggest { label: "fallback" }),
    ]);

    let snap = ToolRegistrySnapshot {
        all_tool_names: HashSet::new(),
        subagent_types: HashSet::new(),
    };
    let input = serde_json::json!({});
    let err: &'static str = "Error: unknown";
    let cwd = std::path::Path::new(".");
    let ctx = ErrorContext::new("Read", &input, err, cwd, &snap);

    let result = registry.suggest(&ctx);
    assert!(result.is_some());
    assert_eq!(result.unwrap().summary, "来自 fallback");
}
