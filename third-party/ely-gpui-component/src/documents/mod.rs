mod blocks;
mod counts;
mod epub;
mod find;
mod format;
mod history;
mod knowledge;
mod link;
mod markdown;
mod modes;
mod pages;
mod print;
mod reading;
mod render;
mod rich;
mod suggest;
mod templates;
#[cfg(all(test, feature = "test-support"))]
mod tests;
mod thumbs;
mod todo;
mod toolbar;
mod tree;
mod viewer;

use gpui::App;

pub(crate) use blocks::source;
pub use blocks::{Align, BlockData, BlockEditor, BlockEvent, BlockKind, Media};
pub use counts::{ReadingTime, WordCount};
pub use epub::{Chapter, EpubReader};
pub use format::{Format, format};
pub(crate) use history::Change;
pub use history::{PageHistoryDiff, Version, VersionHistory};
pub use knowledge::{Backlink, Backlinks, Favorite, Favorites, TrashBin, Trashed};
pub use link::LinkEditor;
pub use markdown::markdown_highlights;
pub use modes::{MarkdownEditor, MarkdownMode};
pub use pages::{PageCover, PageIcon};
pub use print::{Margins, Paper, PrintPreview, PrintSettings, sheet};
pub use reading::{Footnote, Glossary, ReadingProgress, TooltipTerm, ZenMode};
pub use render::{MarkdownRenderer, outline};
pub use rich::RichTextEditor;
pub use templates::{Template, TemplatePicker};
pub use thumbs::PageThumbnailList;
pub use todo::{Checklist, TodoItem};
pub use toolbar::FixedFormatToolbar;
pub(crate) use viewer::step as zoom_step;
pub use viewer::{DocPage, DocumentViewer, PageHit, PageNote};

/// Binds the documents keys. `init` calls it.
pub(crate) fn bind_keys(cx: &mut App) {
    rich::bind_keys(cx);
}
