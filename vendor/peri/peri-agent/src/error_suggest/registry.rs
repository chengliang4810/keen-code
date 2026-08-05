use crate::error_suggest::context::ErrorContext;

/// 单个建议器接口。返回 None 表示"本建议器不处理这种错误"
pub trait ErrorSuggester: Send + Sync {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion>;
}

/// 建议文本
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub summary: String,
    pub details: Option<String>,
}

impl Suggestion {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
}

/// 建议器注册表，按注册顺序短路
#[derive(Default)]
pub struct ErrorSuggestRegistry {
    suggesters: Vec<Box<dyn ErrorSuggester>>,
}

impl ErrorSuggestRegistry {
    pub fn new(suggesters: Vec<Box<dyn ErrorSuggester>>) -> Self {
        Self { suggesters }
    }

    pub fn empty() -> Self {
        Self {
            suggesters: Vec::new(),
        }
    }

    /// 第一个返回 Some 的 suggester 胜出（短路）
    pub fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        for s in &self.suggesters {
            if let Some(sug) = s.suggest(ctx) {
                return Some(sug);
            }
        }
        None
    }
}
