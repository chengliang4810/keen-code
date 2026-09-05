use serde::de::DeserializeOwned;
use std::sync::OnceLock;
use std::time::Duration;

/// 网络来源可信度警告（附在 WebFetch/WebSearch 输出前）
pub(crate) const WEB_CREDIBILITY_WARNING: &str = "⚠ Web content may be inaccurate or outdated. Verify critical information before relying on it.\n\n";

/// Tavily 兼容后端的唯一基础地址。
const TAVILY_BASE_URL: &str = "https://tavily.claude-code-best.win";
/// 单次 Tavily 成功响应允许读取的最大字节数。
const MAX_TAVILY_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Tavily 错误响应最多保留的诊断字节数。
const MAX_TAVILY_ERROR_BYTES: usize = 64 * 1024;
/// Tavily 请求的完整超时时间。
const TAVILY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 一类 Tavily 请求需要保留的业务错误前缀。
#[derive(Clone, Copy)]
pub(crate) struct TavilyRequestLabels {
    /// 网络请求发送失败时使用的前缀。
    pub(crate) request_failed: &'static str,
    /// 后端返回非成功 HTTP 状态时使用的前缀。
    pub(crate) http_failed: &'static str,
    /// 成功响应无法解析为目标结构时使用的前缀。
    pub(crate) parse_failed: &'static str,
}

/// 返回进程内复用的 Tavily HTTP 客户端。
fn tavily_client() -> Result<reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(TAVILY_REQUEST_TIMEOUT)
                .build()
                .map_err(|error| format!("Failed to build HTTP client: {error}"))
        })
        .clone()
}

/// 按字节上限流式读取 HTTP 响应，避免错误正文或异常响应无限占用内存。
async fn read_response_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("response exceeds {limit} bytes"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read response body: {error}"))?
    {
        extend_response_limited(&mut bytes, &chunk, limit)?;
    }
    Ok(bytes)
}

/// 向响应缓冲区追加一个分块，并在扩容前执行上限检查。
fn extend_response_limited(bytes: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Result<(), String> {
    if bytes.len().saturating_add(chunk.len()) > limit {
        return Err(format!("response exceeds {limit} bytes"));
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

/// 发送 Tavily JSON 请求，并统一客户端复用、响应限额和解析错误处理。
pub(crate) async fn tavily_post<T: DeserializeOwned>(
    endpoint: &str,
    body: &serde_json::Value,
    labels: TavilyRequestLabels,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    let response = tavily_client()?
        .post(format!("{TAVILY_BASE_URL}/{endpoint}"))
        .json(body)
        .send()
        .await
        .map_err(|error| format!("{}: {error}", labels.request_failed))?;
    let status = response.status();
    let limit = if status.is_success() {
        MAX_TAVILY_RESPONSE_BYTES
    } else {
        MAX_TAVILY_ERROR_BYTES
    };
    let bytes = read_response_limited(response, limit)
        .await
        .map_err(|error| format!("{} {status}: {error}", labels.http_failed))?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(format!("{} {status}: {text}", labels.http_failed).into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: {error}", labels.parse_failed).into())
}

#[cfg(test)]
mod tests {
    use super::extend_response_limited;

    /// 分块累计恰好达到上限时仍应完整保留内容。
    #[test]
    fn limited_response_accepts_exact_limit() {
        let mut bytes = b"abc".to_vec();
        extend_response_limited(&mut bytes, b"def", 6).expect("恰好达到上限应成功");
        assert_eq!(bytes, b"abcdef");
    }

    /// 超限分块必须在修改既有缓冲区前失败。
    #[test]
    fn limited_response_rejects_chunk_without_partial_append() {
        let mut bytes = b"abc".to_vec();
        let error = extend_response_limited(&mut bytes, b"defg", 6).expect_err("超过上限应失败");
        assert_eq!(error, "response exceeds 6 bytes");
        assert_eq!(bytes, b"abc");
    }
}
