//! peri-sessions — 会话持久化子模块（自 peri-agent/src/thread 迁入）。
//!
//! 直操 sqlite：`SqliteThreadStore` 为生产实现；`FilesystemThreadStore` 为纯测试用途。
//! 契约类型（`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags`）位于
//! peri-acp-types（接口契约归 peri-acp-types），本模块仅实现，不解释业务语义。

use peri_acp_types::messages::{BaseMessage, ContentBlock, MessageContent};

mod filesystem;
mod sqlite_store;

pub use filesystem::FilesystemThreadStore;
pub use sqlite_store::SqliteThreadStore;

/// 从消息列表中提取第一条可用 Human 标题，最多保留 50 个 Unicode 字符。
pub(crate) fn extract_title(msgs: &[BaseMessage]) -> Option<String> {
    for msg in msgs {
        if let BaseMessage::Human { content, .. } = msg {
            let text = match content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| {
                        if let ContentBlock::Text { text } = block {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                MessageContent::Raw(_) => continue,
            };
            let title: String = text.chars().take(50).collect();
            if !title.trim().is_empty() {
                return Some(title);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::extract_title;
    use peri_acp_types::messages::{BaseMessage, ContentBlock, MessageContent};

    /// 验证纯文本 Human 消息可以直接生成标题。
    #[test]
    fn 从_text内容提取标题() {
        let messages = vec![BaseMessage::human(MessageContent::text("Hello world"))];

        assert_eq!(extract_title(&messages), Some("Hello world".to_string()));
    }

    /// 验证 Blocks 只拼接文本块，并用空格分隔不连续文本。
    #[test]
    fn 从_blocks内容提取标题() {
        let messages = vec![BaseMessage::human(MessageContent::blocks(vec![
            ContentBlock::text("第一段"),
            ContentBlock::image_url("https://example.com/image.png"),
            ContentBlock::text("第二段"),
        ]))];

        assert_eq!(extract_title(&messages), Some("第一段 第二段".to_string()));
    }

    /// 验证 Raw 内容不猜测 provider 原生结构，并继续查找后续 Human 消息。
    #[test]
    fn 跳过_raw内容并查找后续标题() {
        let messages = vec![
            BaseMessage::human(MessageContent::raw(vec![serde_json::json!({
                "type": "text",
                "text": "raw title",
            })])),
            BaseMessage::human("后续标题"),
        ];

        assert_eq!(extract_title(&messages), Some("后续标题".to_string()));
    }

    /// 验证空白 Human、空 Blocks 与空消息列表都不会生成标题。
    #[test]
    fn 空白消息不生成标题() {
        let empty_text = vec![BaseMessage::human(" \n\t")];
        let empty_blocks = vec![BaseMessage::human(MessageContent::blocks(vec![]))];

        assert_eq!(extract_title(&empty_text), None);
        assert_eq!(extract_title(&empty_blocks), None);
        assert_eq!(extract_title(&[]), None);
    }

    /// 验证标题按 Unicode 字符而非字节截断到 50 个字符。
    #[test]
    fn 标题按_unicode字符截断() {
        let messages = vec![BaseMessage::human("你好".repeat(30))];

        let title = extract_title(&messages).expect("非空消息应生成标题");
        assert_eq!(title.chars().count(), 50);
        assert_eq!(title, "你好".repeat(25));
    }
}
