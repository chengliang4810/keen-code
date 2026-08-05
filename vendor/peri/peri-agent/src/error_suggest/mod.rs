pub mod context;
pub mod format;
pub mod matcher;
pub mod registry;

pub use context::{ErrorContext, ToolRegistrySnapshot};
pub use registry::{ErrorSuggestRegistry, ErrorSuggester, Suggestion};

#[cfg(test)]
mod registry_test;

#[cfg(test)]
mod matcher_test;

#[cfg(test)]
mod format_test;
