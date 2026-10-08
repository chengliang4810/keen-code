use std::io::Read;

/// 有界 HTTP 响应读取的统一失败类型。
#[derive(Debug)]
pub(crate) enum HttpResponseReadError {
    /// 响应声明或实际读取的正文超过调用方给定上限。
    TooLarge { max_bytes: usize },
    /// 流式读取响应正文时发生 I/O 错误。
    Read(std::io::Error),
}

/// 在分配完整正文前读取阻塞式 HTTP 响应，并严格限制最大字节数。
///
/// 已知 `Content-Length` 时先拒绝超限响应；未知长度或 chunked 响应只读取
/// `max_bytes + 1` 字节，以额外一个字节判断是否超限，避免先完整缓冲远端正文。
pub(crate) fn read_http_response_limited(
    response: reqwest::blocking::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, HttpResponseReadError> {
    let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let content_length = response.content_length();
    if content_length.is_some_and(|length| length > max_bytes_u64) {
        return Err(HttpResponseReadError::TooLarge { max_bytes });
    }

    // 不按远端声明的长度预分配，避免合法上限内的虚假大 Content-Length 先占满内存。
    let mut bytes = Vec::new();
    response
        .take(max_bytes_u64.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(HttpResponseReadError::Read)?;
    if bytes.len() > max_bytes {
        return Err(HttpResponseReadError::TooLarge { max_bytes });
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "http_response_test.rs"]
mod tests;
