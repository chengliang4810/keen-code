//! 跨 Provider、工具、持久化与桌面投影共享的错误秘密脱敏。

use url::{Url, form_urlencoded};

/// 错误文本中秘密值使用的唯一固定占位符。
pub const REDACTED_SECRET: &str = "[REDACTED]";
/// 交给 URL 解析器的单个候选字节上限；超限候选整段保守脱敏。
const MAX_URL_CANDIDATE_BYTES: usize = 64 * 1024;
/// URL 组件允许递归检查的最大层数；超限嵌套 URL 整段保守脱敏。
const MAX_NESTED_URL_DEPTH: usize = 8;
/// 重建转义 URL 时保留的反斜杠层数上限，避免畸形前缀放大输出。
const MAX_URL_SLASH_ESCAPE_BACKSLASHES: usize = 8;

/// 从错误文本中移除常见认证 Header、敏感字段和 URL 凭据。
///
/// 本函数只处理带明确秘密语义的上下文，不猜测普通自由文本中的随机字符串。
/// 调用方仍负责按自身边界清理控制字符和限制最终长度。
pub fn redact_error_secrets(input: &str) -> String {
    redact_error_secrets_at_depth(input, 0)
}

/// 在固定深度预算内扫描错误正文；URL 组件递归调用仍受同一候选字节上限约束。
fn redact_error_secrets_at_depth(input: &str, url_depth: usize) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        if let Some(redaction) = redact_url_at(input, cursor, url_depth)
            .or_else(|| redact_sensitive_assignment_at(input, cursor))
            .or_else(|| redact_bearer_at(input, cursor))
        {
            output.push_str(&redaction.replacement);
            cursor = redaction.end;
            continue;
        }
        let character = input[cursor..]
            .chars()
            .next()
            .expect("cursor 始终位于非空 UTF-8 后缀");
        output.push(character);
        cursor += character.len_utf8();
    }
    output
}

/// 一次局部脱敏及其消费的原文本终点。
struct Redaction {
    end: usize,
    replacement: String,
}

/// 在当前字符处识别 HTTP(S) URL，并一次性消费整个候选，避免从候选内部重复扫描。
fn redact_url_at(input: &str, start: usize, url_depth: usize) -> Option<Redaction> {
    if start > 0 && input.as_bytes()[start - 1].is_ascii_alphanumeric() {
        return None;
    }
    let slash_style = url_slash_style(&input[start..])?;

    let mut end = start;
    while end < input.len() {
        if input.as_bytes()[end] == b'\\' {
            if let Some((slash_end, _)) = encoded_slash_at(input, end) {
                end = slash_end;
                continue;
            }
            break;
        }
        let character = input[end..]
            .chars()
            .next()
            .expect("URL 候选游标始终位于非空 UTF-8 后缀");
        if character.is_whitespace()
            || character.is_control()
            || matches!(character, '"' | '\'' | '<' | '>' | '\\')
        {
            break;
        }
        end += character.len_utf8();
    }
    if end == start {
        return None;
    }

    let mut parsed_end = end;
    while parsed_end > start
        && !input[start..parsed_end].ends_with(REDACTED_SECRET)
        && input[..parsed_end]
            .chars()
            .next_back()
            .is_some_and(|character| {
                matches!(
                    character,
                    ',' | ';' | ':' | '!' | '?' | '.' | ')' | ']' | '}'
                )
            })
    {
        let character = input[..parsed_end]
            .chars()
            .next_back()
            .expect("非空 URL 候选应有末字符");
        parsed_end -= character.len_utf8();
    }
    let trailing = &input[parsed_end..end];
    if parsed_end.saturating_sub(start) > MAX_URL_CANDIDATE_BYTES {
        return Some(Redaction {
            end,
            replacement: format!("{REDACTED_SECRET}{trailing}"),
        });
    }

    if url_depth >= MAX_NESTED_URL_DEPTH {
        return Some(Redaction {
            end,
            replacement: format!("{REDACTED_SECRET}{trailing}"),
        });
    }

    let candidate = match slash_style {
        UrlSlashStyle::Plain => input[start..parsed_end].to_owned(),
        UrlSlashStyle::JsonEscaped { .. } => normalize_url_slashes(&input[start..parsed_end]),
    };
    let Ok(mut url) = Url::parse(&candidate) else {
        // 已有明确 URL scheme 但语法畸形时无法安全区分路径与凭据，整段删除。
        return Some(Redaction {
            end,
            replacement: format!("{REDACTED_SECRET}{trailing}"),
        });
    };
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }

    let mut changed = false;
    if !url.username().is_empty() || url.password().is_some() {
        url.set_password(None).ok()?;
        url.set_username("").ok()?;
        changed = true;
    }

    let path = url.path().to_owned();
    let redacted_path = redact_error_secrets_at_depth(&path, url_depth + 1);
    if redacted_path != path {
        url.set_path(&redacted_path);
        changed = true;
    }

    if url.query().is_some() {
        let mut pairs = Vec::new();
        let mut query_changed = false;
        for (name, value) in url.query_pairs() {
            if is_sensitive_query_name(&name) {
                pairs.push((name.into_owned(), REDACTED_SECRET.to_owned()));
                query_changed = true;
            } else {
                let name = name.into_owned();
                let value = value.into_owned();
                let redacted_value = redact_error_secrets_at_depth(&value, url_depth + 1);
                query_changed |= redacted_value != value;
                pairs.push((name, redacted_value));
            }
        }
        if query_changed {
            url.set_query(None);
            let mut query = url.query_pairs_mut();
            for (name, value) in pairs {
                query.append_pair(&name, &value);
            }
            changed = true;
        }
    }

    if let Some(fragment) = url.fragment().map(str::to_owned)
        && fragment.contains('=')
    {
        let mut pairs = Vec::new();
        let mut fragment_changed = false;
        for (name, value) in form_urlencoded::parse(fragment.as_bytes()) {
            if is_sensitive_query_name(&name) {
                pairs.push((name.into_owned(), REDACTED_SECRET.to_owned()));
                fragment_changed = true;
            } else {
                let name = name.into_owned();
                let value = value.into_owned();
                let redacted_value = redact_error_secrets_at_depth(&value, url_depth + 1);
                fragment_changed |= redacted_value != value;
                pairs.push((name, redacted_value));
            }
        }
        if fragment_changed {
            let encoded = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(pairs)
                .finish();
            url.set_fragment(Some(&encoded));
            changed = true;
        }
    } else if let Some(fragment) = url.fragment().map(str::to_owned) {
        let redacted_fragment = redact_error_secrets_at_depth(&fragment, url_depth + 1);
        if redacted_fragment != fragment {
            url.set_fragment(Some(&redacted_fragment));
            changed = true;
        }
    }

    if !changed {
        return Some(Redaction {
            end,
            replacement: input[start..end].to_owned(),
        });
    }
    // `url` 会把占位符方括号编码；错误展示统一恢复为同一个可见占位符。
    let mut replacement = url.to_string().replace("%5BREDACTED%5D", REDACTED_SECRET);
    if let UrlSlashStyle::JsonEscaped { backslashes } = slash_style {
        let escaped_slash = format!("{}/", "\\".repeat(backslashes));
        replacement = replacement.replace('/', &escaped_slash);
    }
    replacement.push_str(trailing);
    Some(Redaction { end, replacement })
}

/// URL 在原错误文本中的斜杠表示形式。
#[derive(Clone, Copy, Eq, PartialEq)]
enum UrlSlashStyle {
    Plain,
    JsonEscaped {
        /// 用于重建安全 URL 的规范转义层数。
        backslashes: usize,
    },
}

/// 同时识别普通 URL 与一层或多层 JSON 字符串中的斜杠转义。
fn url_slash_style(value: &str) -> Option<UrlSlashStyle> {
    let scheme_end = ["http:", "https:"]
        .into_iter()
        .find_map(|scheme| starts_ascii_case_insensitive(value, scheme).then_some(scheme.len()))?;
    let (cursor, first_backslashes) = encoded_slash_at(value, scheme_end)?;
    let (_, second_backslashes) = encoded_slash_at(value, cursor)?;
    let backslashes = first_backslashes.max(second_backslashes);
    if backslashes == 0 {
        Some(UrlSlashStyle::Plain)
    } else {
        Some(UrlSlashStyle::JsonEscaped {
            backslashes: backslashes.min(MAX_URL_SLASH_ESCAPE_BACKSLASHES),
        })
    }
}

/// 识别当前位置的 `/` 或任意正层数 `\\.../`，并返回斜杠后的游标与反斜杠数。
fn encoded_slash_at(value: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = value.as_bytes();
    let mut cursor = start;
    while bytes.get(cursor) == Some(&b'\\') {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'/')).then_some((cursor + 1, cursor - start))
}

/// 把多层 `\\.../` 规范为交给 URL 解析器的普通 `/`。
fn normalize_url_slashes(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        if value.as_bytes()[cursor] == b'\\'
            && let Some((slash_end, _)) = encoded_slash_at(value, cursor)
        {
            normalized.push('/');
            cursor = slash_end;
            continue;
        }
        let character = value[cursor..]
            .chars()
            .next()
            .expect("URL 规范化游标始终位于非空 UTF-8 后缀");
        normalized.push(character);
        cursor += character.len_utf8();
    }
    normalized
}

/// 在当前字符处识别 `Bearer <credential>`，不误删普通 `bearer` 单词。
fn redact_bearer_at(input: &str, start: usize) -> Option<Redaction> {
    if start > 0 && is_identifier_byte(input.as_bytes()[start - 1]) {
        return None;
    }
    let rest = &input[start..];
    if !starts_ascii_case_insensitive(rest, "bearer") {
        return None;
    }
    let marker_end = start + "bearer".len();
    if marker_end >= input.len() || !is_horizontal_whitespace(input.as_bytes()[marker_end]) {
        return None;
    }
    let mut value_start = marker_end;
    while value_start < input.len() && is_horizontal_whitespace(input.as_bytes()[value_start]) {
        value_start += 1;
    }
    if value_start == input.len() {
        return None;
    }
    let (end, replacement_value) = redact_value(input, value_start, false)?;
    Some(Redaction {
        end,
        replacement: format!(
            "{}{}{}",
            &input[start..marker_end],
            &input[marker_end..value_start],
            replacement_value
        ),
    })
}

/// 在当前字符处识别 JSON、Header 或普通键值形式的敏感字段。
fn redact_sensitive_assignment_at(input: &str, start: usize) -> Option<Redaction> {
    let bytes = input.as_bytes();
    if start > 0 && matches!(bytes[start], b'\\' | b'"' | b'\'') && bytes[start - 1] == b'\\' {
        return None;
    }
    let quoted = opening_quote_at(input, start);
    let (key_start, mut cursor, key_end) = if let Some((content_start, delimiter)) = quoted {
        let maximum_key_end = content_start.saturating_add(80).min(input.len());
        let mut cursor = content_start;
        let closing_start = loop {
            if let Some(end) = closing_quote_end_at(input, cursor, delimiter) {
                break (cursor, end);
            }
            if cursor >= maximum_key_end
                || !bytes
                    .get(cursor)
                    .copied()
                    .is_some_and(is_quoted_field_name_byte)
            {
                return None;
            }
            cursor += 1;
        };
        (content_start, closing_start.1, closing_start.0)
    } else {
        if !bytes
            .get(start)
            .copied()
            .is_some_and(is_unquoted_field_name_byte)
            || start > 0 && is_unquoted_field_name_byte(bytes[start - 1])
        {
            return None;
        }
        let key_start = start;
        let maximum_key_end = key_start.saturating_add(80).min(input.len());
        let mut cursor = key_start;
        while cursor < maximum_key_end
            && bytes
                .get(cursor)
                .copied()
                .is_some_and(is_quoted_field_name_byte)
        {
            cursor += 1;
        }
        (key_start, cursor, cursor)
    };

    let mut key_end = key_end;
    while key_end > key_start && is_horizontal_whitespace(bytes[key_end - 1]) {
        key_end -= 1;
    }
    if key_end == key_start || !is_sensitive_field_name(&input[key_start..key_end]) {
        return None;
    }

    while cursor < input.len() && is_horizontal_whitespace(bytes[cursor]) {
        cursor += 1;
    }
    let separator = *bytes.get(cursor)?;
    match separator {
        b':' => cursor += 1,
        b'=' => {
            cursor += 1;
            if bytes.get(cursor) == Some(&b'>') {
                cursor += 1;
            }
        }
        _ => return None,
    }
    while cursor < input.len() && is_horizontal_whitespace(bytes[cursor]) {
        cursor += 1;
    }
    if cursor == input.len() {
        return None;
    }

    let normalized_name = normalized_field_name(&input[key_start..key_end])?;
    let authorization = matches!(
        normalized_name.as_str(),
        "authorization" | "proxyauthorization"
    );
    let cookie_header = matches!(normalized_name.as_str(), "cookie" | "setcookie");
    let structured_header = (authorization || cookie_header)
        && opening_quote_at(input, cursor).is_none()
        && !matches!(bytes.get(cursor), Some(b'{' | b'[' | b'('));
    let (end, replacement_value) = if structured_header {
        let end = structured_header_value_end(input, cursor);
        let value = &input[cursor..end];
        let replacement = if authorization {
            redact_auth_scheme(value, true)
        } else {
            REDACTED_SECRET.to_owned()
        };
        (end, replacement)
    } else {
        redact_value(input, cursor, authorization)?
    };
    Some(Redaction {
        end,
        replacement: format!("{}{}", &input[start..cursor], replacement_value),
    })
}

/// 找到未加引号的认证或 Cookie Header 终点，完整覆盖结构参数并保留独立诊断字段。
fn structured_header_value_end(input: &str, start: usize) -> usize {
    let bytes = input.as_bytes();
    let mut quote = None;
    let mut cursor = start;
    while cursor < input.len() {
        let byte = bytes[cursor];
        if let Some(active_quote) = quote {
            match byte {
                b'\\' => cursor = (cursor + 2).min(input.len()),
                byte if byte == active_quote => {
                    quote = None;
                    cursor += 1;
                }
                b'\n' | b'\r' => return cursor,
                _ => cursor += 1,
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'\n' | b'\r' => return cursor,
            b'\\' if matches!(bytes.get(cursor + 1), Some(b'n' | b'r')) => return cursor,
            byte if is_horizontal_whitespace(byte) => {
                let mut diagnostic_start = cursor;
                while diagnostic_start < input.len()
                    && is_horizontal_whitespace(bytes[diagnostic_start])
                {
                    diagnostic_start += 1;
                }
                if is_independent_diagnostic_at(input, diagnostic_start) {
                    return cursor;
                }
                cursor = diagnostic_start;
            }
            // 逗号与分号属于 Digest 参数和 Cookie 属性的结构，后续字段仍是秘密。
            b',' | b';' => {
                cursor += 1;
                while cursor < input.len() && is_horizontal_whitespace(bytes[cursor]) {
                    cursor += 1;
                }
            }
            b'|' => {
                let mut diagnostic_start = cursor + 1;
                while diagnostic_start < input.len()
                    && is_horizontal_whitespace(bytes[diagnostic_start])
                {
                    diagnostic_start += 1;
                }
                if is_independent_diagnostic_at(input, diagnostic_start) {
                    return cursor;
                }
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    input.len()
}

/// 识别 Header 后可安全保留并继续单独脱敏的顶层诊断字段。
fn is_independent_diagnostic_at(input: &str, start: usize) -> bool {
    let bytes = input.as_bytes();
    let maximum_key_end = start.saturating_add(80).min(input.len());
    let mut cursor = start;
    while cursor < maximum_key_end
        && bytes
            .get(cursor)
            .copied()
            .is_some_and(is_unquoted_field_name_byte)
    {
        cursor += 1;
    }
    if cursor == start {
        return false;
    }
    let Some(name) = normalized_field_name(&input[start..cursor]) else {
        return false;
    };
    while cursor < input.len() && is_horizontal_whitespace(bytes[cursor]) {
        cursor += 1;
    }
    if !matches!(bytes.get(cursor), Some(b':' | b'=')) {
        return false;
    }
    matches!(
        name.as_str(),
        "requestid"
            | "traceid"
            | "correlationid"
            | "status"
            | "statuscode"
            | "httpstatus"
            | "error"
            | "errorcode"
            | "code"
            | "retryafter"
            | "retryafterms"
            | "detail"
            | "details"
            | "reason"
            | "type"
            | "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
    )
}

/// 替换一个字段值，同时保留引号、认证 scheme 和后续非敏感上下文。
fn redact_value(input: &str, start: usize, preserve_auth_scheme: bool) -> Option<(usize, String)> {
    let bytes = input.as_bytes();
    if redaction_placeholder_length(&input[start..]).is_some() {
        let end = unquoted_secret_end(input, start);
        return Some((end, REDACTED_SECRET.to_owned()));
    }

    if let Some((content_start, delimiter)) = opening_quote_at(input, start) {
        let (content_end, end) = quoted_value_end(input, content_start, delimiter);
        let content = &input[content_start..content_end];
        let redacted = redact_auth_scheme(content, preserve_auth_scheme);
        let mut replacement = input[start..content_start].to_owned();
        replacement.push_str(&redacted);
        if end != content_end {
            replacement.push_str(&input[content_end..end]);
        }
        return Some((end, replacement));
    }

    if matches!(bytes.get(start), Some(b'{' | b'[' | b'(')) {
        let end = balanced_value_end(input, start).unwrap_or_else(|| line_end(input, start));
        return Some((end, REDACTED_SECRET.to_owned()));
    }
    if input[start..].starts_with("Some(") {
        let end = balanced_value_end(input, start + "Some".len())
            .unwrap_or_else(|| line_end(input, start));
        return Some((end, REDACTED_SECRET.to_owned()));
    }

    if preserve_auth_scheme {
        for scheme in ["bearer", "basic"] {
            if starts_ascii_case_insensitive(&input[start..], scheme) {
                let scheme_end = start + scheme.len();
                if scheme_end < input.len() && is_horizontal_whitespace(bytes[scheme_end]) {
                    let mut secret_start = scheme_end;
                    while secret_start < input.len()
                        && is_horizontal_whitespace(bytes[secret_start])
                    {
                        secret_start += 1;
                    }
                    let end = unquoted_secret_end(input, secret_start);
                    if end > secret_start {
                        return Some((
                            end,
                            format!(
                                "{}{}{}",
                                &input[start..scheme_end],
                                &input[scheme_end..secret_start],
                                REDACTED_SECRET
                            ),
                        ));
                    }
                }
            }
        }
    }

    let end = unquoted_value_end(input, start);
    (end > start).then(|| (end, REDACTED_SECRET.to_owned()))
}

/// 返回未加引号秘密的完整终点；占位符只有在明确终止时才算完整安全值。
fn unquoted_secret_end(input: &str, start: usize) -> usize {
    let Some(placeholder_length) = redaction_placeholder_length(&input[start..]) else {
        return unquoted_value_end(input, start);
    };
    let placeholder_end = start + placeholder_length;
    if redaction_placeholder_has_safe_terminator(&input[placeholder_end..]) {
        placeholder_end
    } else {
        structured_header_value_end(input, placeholder_end).max(placeholder_end)
    }
}

/// 普通引号或多层 JSON 字符串中的转义引号边界。
#[derive(Clone, Copy)]
struct QuoteDelimiter {
    quote: u8,
    leading_backslashes: usize,
}

/// 识别值或字段名前的普通引号，以及 `\"`、`\\\"` 等嵌套转义引号。
fn opening_quote_at(input: &str, start: usize) -> Option<(usize, QuoteDelimiter)> {
    let bytes = input.as_bytes();
    let first = *bytes.get(start)?;
    if matches!(first, b'"' | b'\'') {
        return Some((
            start + 1,
            QuoteDelimiter {
                quote: first,
                leading_backslashes: 0,
            },
        ));
    }
    if first != b'\\' {
        return None;
    }
    let mut cursor = start;
    while bytes.get(cursor) == Some(&b'\\') {
        cursor += 1;
    }
    let quote = *bytes.get(cursor)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    Some((
        cursor + 1,
        QuoteDelimiter {
            quote,
            leading_backslashes: cursor - start,
        },
    ))
}

/// 若当前位置是与开引号同层级的闭引号，则返回闭引号后的字节位置。
fn closing_quote_end_at(input: &str, start: usize, delimiter: QuoteDelimiter) -> Option<usize> {
    let bytes = input.as_bytes();
    if delimiter.leading_backslashes == 0 {
        return (bytes.get(start) == Some(&delimiter.quote)).then_some(start + 1);
    }
    if start > 0 && bytes[start - 1] == b'\\' {
        return None;
    }
    let quote_at = start.checked_add(delimiter.leading_backslashes)?;
    if quote_at >= input.len()
        || !bytes[start..quote_at].iter().all(|byte| *byte == b'\\')
        || bytes[quote_at] != delimiter.quote
    {
        return None;
    }
    Some(quote_at + 1)
}

/// 找到引号包裹值的正文和整体终点；未闭合时不把末尾秘密误当闭引号。
fn quoted_value_end(input: &str, start: usize, delimiter: QuoteDelimiter) -> (usize, usize) {
    let bytes = input.as_bytes();
    let mut cursor = start;
    while cursor < input.len() {
        if let Some(end) = closing_quote_end_at(input, cursor, delimiter) {
            return (cursor, end);
        }
        match (delimiter.leading_backslashes, bytes[cursor]) {
            (0, b'\\') => cursor = (cursor + 2).min(input.len()),
            (_, b'\n' | b'\r') => return (cursor, cursor),
            _ => cursor += 1,
        }
    }
    (input.len(), input.len())
}

/// 找到 JSON/调试容器的配对结束位置，避免嵌套 credential 对象只删掉首个词。
fn balanced_value_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let opening = *bytes.get(start)?;
    let closing = match opening {
        b'{' => b'}',
        b'[' => b']',
        b'(' => b')',
        _ => return None,
    };
    let mut depth = 0usize;
    let mut quote = None;
    let mut cursor = start;
    while cursor < input.len() {
        let byte = bytes[cursor];
        if let Some(active_quote) = quote {
            match byte {
                b'\\' => cursor = (cursor + 2).min(input.len()),
                byte if byte == active_quote => {
                    quote = None;
                    cursor += 1;
                }
                _ => cursor += 1,
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                cursor += 1;
            }
            byte if byte == opening => {
                depth += 1;
                cursor += 1;
            }
            byte if byte == closing => {
                depth = depth.checked_sub(1)?;
                cursor += 1;
                if depth == 0 {
                    return Some(cursor);
                }
            }
            b'\n' | b'\r' => return None,
            _ => cursor += 1,
        }
    }
    None
}

/// 保留 Authorization 的任意合法 scheme token，但移除 scheme 后的完整秘密。
fn redact_auth_scheme(value: &str, preserve: bool) -> String {
    if preserve {
        let scheme_end = value
            .bytes()
            .take_while(|byte| is_auth_scheme_byte(*byte))
            .count();
        if scheme_end > 0
            && value
                .as_bytes()
                .get(scheme_end)
                .copied()
                .is_some_and(is_horizontal_whitespace)
        {
            let mut secret_start = scheme_end;
            while secret_start < value.len()
                && is_horizontal_whitespace(value.as_bytes()[secret_start])
            {
                secret_start += 1;
            }
            return format!(
                "{}{}{}",
                &value[..scheme_end],
                &value[scheme_end..secret_start],
                REDACTED_SECRET
            );
        }
    }
    REDACTED_SECRET.to_owned()
}

/// RFC 9110 `auth-scheme` 使用的 token 字符集合。
fn is_auth_scheme_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// 未加引号的秘密只消费当前值，不吞掉后续错误码、请求 ID 或其他上下文。
fn unquoted_value_end(input: &str, start: usize) -> usize {
    let bytes = input.as_bytes();
    let mut cursor = start;
    while cursor < input.len() {
        let byte = bytes[cursor];
        if byte.is_ascii_whitespace()
            || matches!(byte, b',' | b';' | b'&' | b'}' | b']' | b')' | b'"' | b'\'')
            || (byte == b'\\'
                && matches!(
                    bytes.get(cursor + 1),
                    Some(b'n' | b'r' | b't' | b'"' | b'\'')
                ))
        {
            break;
        }
        cursor += 1;
    }
    cursor
}

/// 未闭合容器只删除当前行，避免吞掉后续独立诊断记录。
fn line_end(input: &str, start: usize) -> usize {
    input[start..]
        .find(['\n', '\r'])
        .map_or(input.len(), |offset| start + offset)
}

/// 判断 Header/JSON/查询参数字段是否明确承载秘密。
fn is_sensitive_field_name(name: &str) -> bool {
    let allow_suffix = !name.bytes().any(is_horizontal_whitespace);
    let Some(name) = normalized_field_name(name) else {
        return false;
    };
    matches!(
        name.as_str(),
        "auth"
            | "authorization"
            | "proxyauthorization"
            | "apikey"
            | "xapikey"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "authtoken"
            | "bearertoken"
            | "idtoken"
            | "sessiontoken"
            | "password"
            | "passwd"
            | "pwd"
            | "secret"
            | "clientsecret"
            | "apisecret"
            | "credential"
            | "credentials"
            | "cookie"
            | "setcookie"
            | "signature"
            | "sig"
    ) || allow_suffix
        && (name.ends_with("apikey")
            || name.ends_with("password")
            || name.ends_with("passwd")
            || name.ends_with("secret")
            || name.ends_with("privatekey")
            || name.ends_with("accesskey")
            || name.ends_with("secretkey")
            || name.ends_with("subscriptionkey")
            || name.ends_with("signature")
            || (name.ends_with("token") && !is_token_metric_name(&name)))
}

/// URL 查询和片段额外覆盖常见会话签名字段。
fn is_sensitive_query_name(name: &str) -> bool {
    if is_sensitive_field_name(name) {
        return true;
    }
    normalized_field_name(name).is_some_and(|name| {
        matches!(
            name.as_str(),
            "auth" | "key" | "session" | "sessionid" | "csrf" | "nonce" | "jwt"
        )
    })
}

/// `max_tokens` 等计量字段是定位上下文/预算问题所必需的普通错误上下文。
fn is_token_metric_name(name: &str) -> bool {
    matches!(
        name,
        "tokens"
            | "maxtoken"
            | "maxtokens"
            | "maxinputtokens"
            | "maxoutputtokens"
            | "inputtoken"
            | "inputtokens"
            | "outputtoken"
            | "outputtokens"
            | "totaltoken"
            | "totaltokens"
            | "completiontoken"
            | "completiontokens"
            | "prompttoken"
            | "prompttokens"
            | "cachedtoken"
            | "cachedtokens"
            | "reasoningtoken"
            | "reasoningtokens"
            | "tokencount"
            | "tokenlimit"
            | "tokenbudget"
            | "tokenusage"
    )
}

/// 把常见 snake/kebab/dotted/camel 字段名收敛为可比较的 ASCII 紧凑形式。
fn normalized_field_name(name: &str) -> Option<String> {
    if name.is_empty() || !name.is_ascii() {
        return None;
    }
    let normalized = name
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| char::from(byte.to_ascii_lowercase()))
        .collect::<String>();
    (!normalized.is_empty()).then_some(normalized)
}

/// 未加引号字段名允许的有限字符集；长度另有硬上限。
fn is_unquoted_field_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

/// 引号内及 `API key` 这类人类可读字段名额外允许横向空白，但绝不跨行。
fn is_quoted_field_name_byte(byte: u8) -> bool {
    is_unquoted_field_name_byte(byte) || is_horizontal_whitespace(byte)
}

/// 字段名和分隔符之间只跳过横向空白，防止一次匹配吞掉下一条日志记录。
fn is_horizontal_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

/// 独立认证 scheme 前后的单词边界。
fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// 返回固定占位符前缀的长度；调用方仍必须校验其后的值终止边界。
fn redaction_placeholder_length(value: &str) -> Option<usize> {
    [REDACTED_SECRET, "<redacted>"]
        .into_iter()
        .find_map(|placeholder| {
            starts_ascii_case_insensitive(value, placeholder).then_some(placeholder.len())
        })
}

/// 占位符后只允许字段结束，或横向空白后的下一个独立键值字段。
fn redaction_placeholder_has_safe_terminator(remainder: &str) -> bool {
    if remainder.is_empty() {
        return true;
    }
    let mut tail = remainder;
    if let Some(stripped) = tail
        .strip_prefix("\\\"")
        .or_else(|| tail.strip_prefix("\\'"))
    {
        tail = stripped;
    } else if tail.starts_with('"') || tail.starts_with('\'') {
        tail = &tail[1..];
    }
    let before_whitespace = tail.len();
    tail = tail.trim_start_matches([' ', '\t']);
    if tail.is_empty()
        || tail.as_bytes().first().is_some_and(|byte| {
            matches!(
                *byte,
                b',' | b';' | b'&' | b'}' | b']' | b')' | b'\r' | b'\n'
            )
        })
    {
        return true;
    }
    tail.len() < before_whitespace && assignment_at(tail)
}

/// 识别横向空白后的独立键值字段，避免重复脱敏吞掉普通诊断上下文。
fn assignment_at(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    while bytes
        .get(cursor)
        .copied()
        .is_some_and(is_unquoted_field_name_byte)
    {
        cursor += 1;
    }
    if cursor == 0 {
        return false;
    }
    while bytes
        .get(cursor)
        .copied()
        .is_some_and(is_horizontal_whitespace)
    {
        cursor += 1;
    }
    matches!(bytes.get(cursor), Some(b':' | b'='))
}

/// 不分 ASCII 大小写比较固定协议标记，不为整段错误创建小写副本。
fn starts_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::{REDACTED_SECRET, redact_error_secrets};

    #[test]
    fn redacts_case_variants_separators_and_header_dump() {
        let raw = concat!(
            "HTTP 401 request_id=req-42\n",
            "AUTHORIZATION: Bearer header-secret\n",
            "Proxy-Authorization: Basic cHJveHk6c2VjcmV0\n",
            "Authorization: Basic basic-secret ignored-segment request_id=req-basic\n",
            "Authorization: Digest username=alice, response=digest-response, nonce=digest-nonce request_id=req-digest\n",
            "Authorization: Digest username=bob, request_id=digest-param-secret\n",
            "x-api-key = api-secret, Retry-After: 30\n",
            "Client_Secret=>\"client-secret\"; PASSWORD: 'two words'\n",
            "Cookie: sid=cookie-secret; refresh=refresh-cookie-secret request_id=req-cookie\n",
            "Cookie: sid=second-cookie-secret; request_id=cookie-field-secret\n",
            "Set-Cookie: sid=set-cookie-secret; HttpOnly\n",
            "headers={\\\"Refresh-Token\\\":\\\"refresh-secret\\\"}"
        );
        let safe = redact_error_secrets(raw);
        for secret in [
            "header-secret",
            "cHJveHk6c2VjcmV0",
            "basic-secret",
            "ignored-segment",
            "username=alice",
            "digest-response",
            "digest-nonce",
            "digest-param-secret",
            "api-secret",
            "client-secret",
            "two words",
            "refresh-secret",
            "cookie-secret",
            "refresh-cookie-secret",
            "second-cookie-secret",
            "cookie-field-secret",
            "set-cookie-secret",
            "HttpOnly",
        ] {
            assert!(!safe.contains(secret), "仍包含秘密 {secret}: {safe}");
        }
        assert!(safe.contains("HTTP 401 request_id=req-42"));
        assert!(safe.contains("Retry-After: 30"));
        assert!(safe.contains("AUTHORIZATION: Bearer [REDACTED]"));
        assert!(safe.contains("Proxy-Authorization: Basic [REDACTED]"));
        assert!(safe.contains("Authorization: Basic [REDACTED] request_id=req-basic"));
        assert!(safe.contains("Authorization: Digest [REDACTED] request_id=req-digest"));
        assert!(safe.contains("Cookie: [REDACTED] request_id=req-cookie"));
        assert!(safe.matches(REDACTED_SECRET).count() >= 8);
    }

    #[test]
    fn redacts_url_userinfo_query_and_fragment_without_losing_safe_context() {
        let raw = "connect https://用户:口令@example.invalid/v1?api_key=query-secret&request_id=req-9&token_count=88#access_token=fragment-secret failed E_CONN";
        let safe = redact_error_secrets(raw);
        assert!(!safe.contains("用户") && !safe.contains("口令"));
        assert!(!safe.contains("query-secret") && !safe.contains("fragment-secret"));
        assert!(safe.contains("example.invalid/v1"));
        assert!(safe.contains("api_key=[REDACTED]"));
        assert!(safe.contains("access_token=[REDACTED]"));
        assert!(safe.contains("request_id=req-9"));
        assert!(safe.contains("token_count=88"));
        assert!(safe.contains("failed E_CONN"));
    }

    #[test]
    fn redacts_json_escaped_url_and_preserves_trailing_punctuation() {
        let raw = r#"failed \"https:\/\/user:password@example.invalid\/v1?api_key=query-secret&request_id=req-json#access_token=fragment-secret\"), status=401"#;
        let safe = redact_error_secrets(raw);
        for secret in ["user", "password", "query-secret", "fragment-secret"] {
            assert!(!safe.contains(secret), "仍包含 URL 秘密 {secret}: {safe}");
        }
        assert!(safe.contains(r"https:\/\/example.invalid\/v1"));
        assert!(safe.contains("api_key=[REDACTED]"));
        assert!(safe.contains("request_id=req-json"));
        assert!(safe.contains("access_token=[REDACTED]"));
        assert!(safe.ends_with(r#"\"), status=401"#));

        let punctuated = r#"failed https:\/\/user:password@example.invalid\/v1?api_key=query-secret#access_token=fragment-secret). request_id=req-punctuation"#;
        let safe = redact_error_secrets(punctuated);
        assert!(safe.contains("access_token=[REDACTED]). request_id=req-punctuation"));
        assert!(!safe.contains("query-secret") && !safe.contains("fragment-secret"));
    }

    #[test]
    fn redacts_urls_with_multiple_json_escape_layers() {
        for backslashes in [2, 4, 8, 32] {
            let slash = format!("{}/", "\\".repeat(backslashes));
            let raw = format!(
                "payload=https:{slash}{slash}user:password@example.invalid{slash}v1?api_key=query-secret#access_token=fragment-secret request_id=req-nested-url"
            );
            let safe = redact_error_secrets(&raw);
            for secret in ["user", "password", "query-secret", "fragment-secret"] {
                assert!(
                    !safe.contains(secret),
                    "{backslashes} 层斜杠转义仍包含 URL 秘密 {secret}: {safe}"
                );
            }
            assert!(safe.contains("example.invalid"));
            assert!(safe.contains("request_id=req-nested-url"));
            assert!(safe.contains(REDACTED_SECRET));
        }
    }

    #[test]
    fn redacts_url_path_matrix_and_nested_values_before_consuming_candidate() {
        let cases = [
            (
                "https://example.invalid/token=path-secret request_id=req-path",
                "path-secret",
                "req-path",
            ),
            (
                "https://example.invalid/v1;api_key=matrix-secret request_id=req-matrix",
                "matrix-secret",
                "req-matrix",
            ),
            (
                "https://outer.invalid/callback?redirect=https://inner-user:inner-password@inner.invalid/v1 request_id=req-nested-userinfo",
                "inner-password",
                "inner.invalid",
            ),
            (
                "https://outer.invalid/callback?detail=token=nested-secret request_id=req-nested-assignment",
                "nested-secret",
                "req-nested-assignment",
            ),
        ];
        for (raw, secret, safe_context) in cases {
            let safe = redact_error_secrets(raw);
            assert!(
                !safe.contains(secret),
                "URL 内层秘密 {secret} 未删除: {safe}"
            );
            assert!(safe.contains(safe_context), "URL 安全上下文丢失: {safe}");
            assert_ne!(safe, raw, "URL 内层秘密未触发任何替换");
        }
        let nested_userinfo = redact_error_secrets(cases[2].0);
        assert!(!nested_userinfo.contains("inner-user"));
    }

    #[test]
    fn redacts_nested_json_strings_and_non_ascii_values() {
        let raw = r#"provider payload={\"error\":\"上游失败\",\"details\":\"{\\\"apiKey\\\":\\\"密钥值-東京\\\",\\\"password\\\":\\\"口令 值\\\"}\"} code=E401"#;
        let safe = redact_error_secrets(raw);
        assert!(!safe.contains("密钥值-東京"));
        assert!(!safe.contains("口令 值"));
        assert!(safe.contains("上游失败"));
        assert!(safe.contains("code=E401"));
    }

    #[test]
    fn redacts_human_readable_keys_and_unclosed_quoted_values() {
        let raw = concat!(
            "API key: readable-secret; status=401\n",
            "password=\"未闭合秘密末字\n",
            "request_id=req-after error_code=E_AUTH",
        );
        let safe = redact_error_secrets(raw);
        assert!(!safe.contains("readable-secret"));
        assert!(!safe.contains("未闭合秘密末字"));
        assert!(safe.contains("API key: [REDACTED]"));
        assert!(safe.contains("password=\"[REDACTED]\n"));
        assert!(safe.ends_with("request_id=req-after error_code=E_AUTH"));
    }

    #[test]
    fn placeholder_prefix_cannot_hide_secret_suffixes() {
        let raw = concat!(
            "token=[REDACTED]opaque-secret\n",
            "Authorization: [REDACTED] auth-secret\n",
            "Cookie: <redacted>; sid=cookie-secret\n",
            "Bearer [REDACTED]bearer-secret\n",
            "request_id=req-placeholder",
        );
        let safe = redact_error_secrets(raw);
        for secret in [
            "opaque-secret",
            "auth-secret",
            "cookie-secret",
            "bearer-secret",
        ] {
            assert!(
                !safe.contains(secret),
                "占位符后仍包含秘密 {secret}: {safe}"
            );
        }
        assert!(safe.contains("request_id=req-placeholder"));
        assert_eq!(redact_error_secrets(&safe), safe);
    }

    #[test]
    fn preserves_token_metrics_and_ordinary_error_context() {
        let raw = "maximum context length: max_tokens=4096 input_tokens=5000 token_count=5000; Authorization failed; password policy rejected; secret service unavailable; https://example.invalid/v1?request_id=req-7&token_count=9";
        assert_eq!(redact_error_secrets(raw), raw);
        assert_eq!(
            redact_error_secrets("Bearer\nrequest_id=req-next"),
            "Bearer\nrequest_id=req-next"
        );
    }

    #[test]
    fn long_unicode_secret_is_removed_linearly_and_suffix_survives() {
        let raw = format!(
            "远端失败 access_token={} request_id=req-long error_code=E429",
            "密".repeat(100_000)
        );
        let safe = redact_error_secrets(&raw);
        assert!(!safe.contains('密'));
        assert!(safe.contains("access_token=[REDACTED]"));
        assert!(safe.ends_with("request_id=req-long error_code=E429"));
        assert!(safe.len() < 256);
    }

    #[test]
    fn long_non_secret_escape_run_remains_unchanged() {
        let raw = format!("{} ordinary diagnostic", "\\".repeat(100_000));
        assert_eq!(redact_error_secrets(&raw), raw);
    }

    #[test]
    fn oversized_url_candidate_is_consumed_once_and_conservatively_redacted() {
        let raw = format!("{}), request_id=req-url-limit", "https://".repeat(9 * 1024));
        assert!(raw.len() > 72 * 1024);
        assert_eq!(
            redact_error_secrets(&raw),
            "[REDACTED]), request_id=req-url-limit"
        );
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact_error_secrets(
            "Authorization: Bearer secret token=[REDACTED] url=https://u:p@host.invalid/?sig=x",
        );
        assert!(once.contains("Authorization: Bearer [REDACTED]"));
        assert_eq!(redact_error_secrets(&once), once);
    }
}
