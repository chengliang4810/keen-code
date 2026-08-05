// 基础设施（trait / registry / context / matcher / format）从 peri-agent re-export
// 避免循环依赖：peri-agent 不依赖 peri-middlewares，所以核心类型定义在底层
pub use peri_agent::error_suggest::{
    context, format, matcher, registry, ErrorContext, ErrorSuggestRegistry, ErrorSuggester,
    Suggestion, ToolRegistrySnapshot,
};

pub mod default_registry;
pub mod suggesters;

pub use default_registry::{build_default_registry, build_tool_registry_snapshot};

// Suggester 测试入口（测试文件留在 peri-middlewares 因为它们测试 peri-middlewares 的 suggesters）
#[cfg(test)]
#[path = "suggesters/path_suggester_test.rs"]
mod path_suggester_test;

#[cfg(test)]
#[path = "suggesters/range_suggester_test.rs"]
mod range_suggester_test;

#[cfg(test)]
#[path = "suggesters/glob_pattern_suggester_test.rs"]
mod glob_pattern_suggester_test;

#[cfg(test)]
#[path = "suggesters/regex_suggester_test.rs"]
mod regex_suggester_test;

#[cfg(test)]
#[path = "suggesters/json_schema_suggester_test.rs"]
mod json_schema_suggester_test;

#[cfg(test)]
#[path = "suggesters/bash_command_suggester_test.rs"]
mod bash_command_suggester_test;

#[cfg(test)]
#[path = "suggesters/subagent_suggester_test.rs"]
mod subagent_suggester_test;
