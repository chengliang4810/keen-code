//! Host `session/list` 的共享投影与偏移分页。
//!
//! 持久 Session 的权威事实在 [`StoredSessionMetadata`]（Journal 归约）；本模块
//! 把它投影成 ACP `SessionInfo` 线格式，并统一 Desktop 与 Headless 两侧的
//! 偏移游标分页。置顶/归档/标题来源等偏好状态经 `_meta` 暴露，前端与 CLI
//! 只做投影，不再各自持久化。

use keencode_acp::schema;
use keencode_acp::{
    META_LAST_USER_MESSAGE_AT, META_SESSION_ARCHIVED, META_SESSION_PINNED,
    META_SESSION_TITLE_SOURCE,
};
use keencode_resources::{StoredSessionMetadata, TitleSource};
use serde_json::{Map, Value};

/// `session/list` 每页固定返回的 Session 数量。
pub const SESSION_LIST_PAGE_SIZE: usize = 100;

/// 无效的 `session/list` 偏移游标：空值、带首尾空白、非数字或超出总数。
#[derive(Debug, Eq, PartialEq)]
pub struct InvalidListCursor;

/// 把持久毫秒时间转换为稳定 UTC RFC 3339 文本。
pub fn rfc3339_from_ms(value: u64) -> Result<String, String> {
    use chrono::{SecondsFormat, TimeZone, Utc};
    let value = i64::try_from(value).map_err(|_| "Session 更新时间超出支持范围".to_owned())?;
    Utc.timestamp_millis_opt(value)
        .single()
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| "Session 更新时间无效".to_owned())
}

/// 权威标题来源的线格式值；与 ACP `SessionTitleSource` 的 snake_case 一致。
pub fn title_source_wire(source: TitleSource) -> &'static str {
    match source {
        TitleSource::Unspecified => "unspecified",
        TitleSource::Manual => "manual",
        TitleSource::Automatic => "automatic",
        TitleSource::MessagePrefix => "message_prefix",
    }
}

/// 把一条持久 Session 元数据投影为 ACP `SessionInfo`。
///
/// `cwd` 必须是调用方授权语义下的项目目录：Desktop 传授权后的规范根，
/// Headless 传存储的项目根。损坏（corrupt）记录返回 `Ok(None)` 由调用方
/// 跳过；时间戳无法表示时返回 `Err`，属于内部数据错误而不是协议错误。
pub fn session_info_from_metadata(
    metadata: StoredSessionMetadata,
    cwd: String,
) -> Result<Option<schema::SessionInfo>, String> {
    if metadata.corrupt {
        return Ok(None);
    }
    let updated_at = rfc3339_from_ms(metadata.updated_at_unix_ms)?;
    // 会话偏好经 _meta 暴露：置顶/归档是权威 Journal 状态，客户端只做投影。
    let mut meta = Map::new();
    meta.insert(META_SESSION_PINNED.to_owned(), Value::Bool(metadata.pinned));
    meta.insert(
        META_SESSION_ARCHIVED.to_owned(),
        Value::Bool(metadata.archived),
    );
    meta.insert(
        META_SESSION_TITLE_SOURCE.to_owned(),
        Value::String(title_source_wire(metadata.title_source).to_owned()),
    );
    let mut info = schema::SessionInfo::new(
        schema::SessionId::new(metadata.session_id.as_str().to_owned()),
        cwd,
    )
    .title(Some(metadata.title))
    .updated_at(Some(updated_at))
    .meta(Some(meta));
    // 从未发送消息的 Session 不伪造用户消息时间，由客户端回退到更新时间。
    if metadata.last_user_message_at_unix_ms > 0 {
        let last_user_message_at = rfc3339_from_ms(metadata.last_user_message_at_unix_ms)?;
        if let Some(meta) = info.meta.as_mut() {
            meta.insert(
                META_LAST_USER_MESSAGE_AT.to_owned(),
                Value::String(last_user_message_at),
            );
        }
    }
    Ok(Some(info))
}

/// 解析偏移游标并切出固定大小的一页；返回本页与下一页游标。
///
/// 游标是上一页返回的 `nextCursor`（整数偏移文本）；没有更多数据时不返回
/// 游标，调用方据此结束翻页。
pub fn paginate_sessions(
    sessions: Vec<schema::SessionInfo>,
    cursor: Option<&str>,
) -> Result<(Vec<schema::SessionInfo>, Option<String>), InvalidListCursor> {
    let start = match cursor {
        Some(cursor) => {
            if cursor.is_empty() || cursor.trim() != cursor {
                return Err(InvalidListCursor);
            }
            cursor.parse::<usize>().map_err(|_| InvalidListCursor)?
        }
        None => 0,
    };
    if start > sessions.len() {
        return Err(InvalidListCursor);
    }
    let end = start
        .saturating_add(SESSION_LIST_PAGE_SIZE)
        .min(sessions.len());
    let next_cursor = (end < sessions.len()).then(|| end.to_string());
    Ok((sessions[start..end].to_vec(), next_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> StoredSessionMetadata {
        StoredSessionMetadata {
            session_id: keencode_resources::SessionId::new("session-list-projection".to_owned())
                .expect("测试 session id 合法"),
            title: "列表投影测试".to_owned(),
            project_root: "D:/projects/keen-code".to_owned(),
            pinned: true,
            archived: false,
            title_source: TitleSource::Automatic,
            status: keencode_resources::SessionStatus::default(),
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_012_345,
            last_user_message_at_unix_ms: 1_700_000_010_000,
            last_sequence: 7,
            corrupt: false,
        }
    }

    fn project() -> Result<Option<schema::SessionInfo>, String> {
        session_info_from_metadata(metadata(), "D:/projects/keen-code".to_owned())
    }

    #[test]
    fn projection_exposes_journal_preferences_via_meta() {
        let info = project()
            .expect("健康元数据必须可投影")
            .expect("corrupt 记录才返回 None");
        assert_eq!(info.title.as_deref(), Some("列表投影测试"));
        assert_eq!(info.updated_at.as_deref(), Some("2023-11-14T22:13:32.345Z"));
        let meta = info.meta.expect("偏好状态必须经 _meta 暴露");
        assert_eq!(meta.get(META_SESSION_PINNED), Some(&Value::Bool(true)));
        assert_eq!(meta.get(META_SESSION_ARCHIVED), Some(&Value::Bool(false)));
        assert_eq!(
            meta.get(META_SESSION_TITLE_SOURCE),
            Some(&Value::String("automatic".to_owned()))
        );
        assert_eq!(
            meta.get(META_LAST_USER_MESSAGE_AT),
            Some(&Value::String("2023-11-14T22:13:30.000Z".to_owned()))
        );
    }

    #[test]
    fn projection_skips_corrupt_and_never_fakes_user_message_time() {
        let mut corrupt = metadata();
        corrupt.corrupt = true;
        assert!(
            session_info_from_metadata(corrupt, "D:/projects/keen-code".to_owned())
                .expect("损坏是可跳过状态，不是内部错误")
                .is_none()
        );

        let mut silent = metadata();
        silent.last_user_message_at_unix_ms = 0;
        let info = session_info_from_metadata(silent, "D:/projects/keen-code".to_owned())
            .expect("健康元数据必须可投影")
            .expect("corrupt 记录才返回 None");
        let meta = info.meta.expect("偏好 meta 不依赖用户消息时间");
        assert_eq!(meta.get(META_LAST_USER_MESSAGE_AT), None);
    }

    #[test]
    fn pagination_returns_fixed_pages_and_offset_cursors() {
        let items = (0..105)
            .filter_map(|index| {
                let mut item = metadata();
                item.title = format!("session {index}");
                session_info_from_metadata(item, "D:/projects/keen-code".to_owned())
                    .expect("健康元数据必须可投影")
            })
            .collect::<Vec<_>>();
        assert_eq!(items.len(), 105);

        let (first, next) = paginate_sessions(items.clone(), None).expect("首页游标有效");
        assert_eq!(first.len(), SESSION_LIST_PAGE_SIZE);
        assert_eq!(next.as_deref(), Some("100"));

        let (second, end) = paginate_sessions(items, Some("100")).expect("偏移游标有效");
        assert_eq!(second.len(), 5);
        assert_eq!(end, None);
    }

    #[test]
    fn pagination_rejects_malformed_and_overflowed_cursors() {
        let items = vec![
            project()
                .expect("健康元数据必须可投影")
                .expect("corrupt 记录才返回 None"),
        ];
        for cursor in ["", " 1", "1 ", "abc", "-1", "2"] {
            assert_eq!(
                paginate_sessions(items.clone(), Some(cursor)),
                Err(InvalidListCursor),
                "游标 {cursor:?} 必须被拒绝"
            );
        }
    }
}
