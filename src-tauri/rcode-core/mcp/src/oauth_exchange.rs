//! MCP OAuth 令牌端点的有界 HTTP 交换实现。
//!
//! 本模块只负责把 [`OAuthTokenRequest`] 发送到已校验的令牌端点，并把安全边界内的
//! JSON 响应转换为 [`OAuthTokenSet`]。授权状态机仍然由 [`crate::oauth`] 负责。

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Url;
use serde_json::Value;
use url::form_urlencoded;

use crate::oauth::{OAuthError, OAuthTokenRequest, OAuthTokenSet};

// OAuth 访问令牌的最大字节数，与本地状态机保持一致。
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
// OAuth 刷新令牌的最大字节数，与本地状态机保持一致。
const MAX_REFRESH_TOKEN_BYTES: usize = 16 * 1024;
// OAuth scope 字符串的最大字节数，与本地状态机保持一致。
const MAX_SCOPE_BYTES: usize = 4 * 1024;
// 请求中普通 OAuth 文本字段的最大字节数。
const MAX_REQUEST_FIELD_BYTES: usize = 8 * 1024;
// 无论远端返回什么正文，令牌请求的端点配置错误都使用此固定信息。
const INVALID_ENDPOINT_ERROR: &str = "OAuth 令牌端点必须使用 HTTPS 或明确的回环 HTTP";
// 无论远端返回什么正文，令牌请求的构造错误都使用此固定信息。
const INVALID_REQUEST_ERROR: &str = "OAuth 令牌请求参数无效";
// 无论远端返回什么正文，令牌响应解析失败都使用此固定信息。
const INVALID_RESPONSE_ERROR: &str = "OAuth 令牌响应无效";
// 响应超过上限时使用固定信息，不暴露远端正文。
const RESPONSE_TOO_LARGE_ERROR: &str = "OAuth 令牌响应超过大小上限";
// 令牌请求超时时使用固定信息。
const REQUEST_TIMEOUT_ERROR: &str = "OAuth 令牌请求超时";
// 令牌请求传输失败时使用固定信息。
const REQUEST_TRANSPORT_ERROR: &str = "OAuth 令牌请求传输失败";
// 重定向被明确禁止时使用固定信息。
const REDIRECT_ERROR: &str = "OAuth 令牌端点禁止重定向";
// 客户端构造失败时使用固定信息。
const CLIENT_ERROR: &str = "创建 OAuth 令牌 HTTP 客户端失败";

/// 使用 Reqwest 执行有界、禁止重定向的 OAuth 令牌交换。
pub struct ReqwestOAuthTokenExchanger {
    // 复用已设置超时和重定向策略的 HTTP 客户端。
    client: reqwest::Client,
    // 单次交换的总超时时间。
    request_timeout: Duration,
    // 令牌端点响应正文的最大字节数。
    max_response_bytes: usize,
}

impl ReqwestOAuthTokenExchanger {
    /// 创建一个带总超时、响应上限且禁止自动重定向的令牌交换器。
    pub fn new(request_timeout: Duration, max_response_bytes: usize) -> Result<Self, OAuthError> {
        if request_timeout.is_zero() || max_response_bytes == 0 {
            return Err(OAuthError::InvalidConfiguration(
                "OAuth 令牌超时与响应上限必须大于零".to_owned(),
            ));
        }

        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| OAuthError::InvalidConfiguration(CLIENT_ERROR.to_owned()))?;

        Ok(Self {
            client,
            request_timeout,
            max_response_bytes,
        })
    }

    /// 执行一次授权码或刷新令牌交换，并返回规范化的令牌集合。
    ///
    /// `now_unix_seconds` 用于把非负的 `expires_in` 安全转换为绝对过期时间；
    /// 调用方取消这个 future 即可取消网络请求，不会留下独立后台任务。
    pub async fn exchange(
        &self,
        request: &OAuthTokenRequest,
        now_unix_seconds: u64,
    ) -> Result<OAuthTokenSet, OAuthError> {
        tokio::time::timeout(
            self.request_timeout,
            self.exchange_inner(request, now_unix_seconds),
        )
        .await
        .map_err(|_| OAuthError::DiscoveryTransport(REQUEST_TIMEOUT_ERROR.to_owned()))?
    }

    // 在总超时边界内完成端点校验、表单构造、请求发送和响应解析。
    async fn exchange_inner(
        &self,
        request: &OAuthTokenRequest,
        now_unix_seconds: u64,
    ) -> Result<OAuthTokenSet, OAuthError> {
        let endpoint = validate_token_endpoint(&request.token_endpoint)?;
        let form = build_form(request)?;
        let response = self
            .client
            .post(endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(encode_form(&form))
            .send()
            .await
            .map_err(map_request_error)?;

        let status = response.status();
        if status.is_redirection() {
            return Err(OAuthError::InvalidCallback(REDIRECT_ERROR.to_owned()));
        }

        let body = self.read_body(response).await?;
        if !status.is_success() {
            return Err(parse_error_response(&body));
        }

        parse_success_response(&body, now_unix_seconds)
    }

    // 逐块读取响应并在超过配置上限时立即停止。
    async fn read_body(&self, response: reqwest::Response) -> Result<Vec<u8>, OAuthError> {
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_request_error)?;
            let next_len = body
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| OAuthError::InvalidCallback(RESPONSE_TOO_LARGE_ERROR.to_owned()))?;
            if next_len > self.max_response_bytes {
                return Err(OAuthError::InvalidCallback(
                    RESPONSE_TOO_LARGE_ERROR.to_owned(),
                ));
            }
            body.extend_from_slice(&chunk);
        }

        Ok(body)
    }
}

// 将 Reqwest 错误映射为不包含 URL、远端正文或其他请求秘密的固定错误。
fn map_request_error(error: reqwest::Error) -> OAuthError {
    if error.is_timeout() {
        OAuthError::DiscoveryTransport(REQUEST_TIMEOUT_ERROR.to_owned())
    } else {
        OAuthError::DiscoveryTransport(REQUEST_TRANSPORT_ERROR.to_owned())
    }
}

// 校验令牌端点只使用 HTTPS，或明确指向 localhost/回环 IP 的 HTTP。
fn validate_token_endpoint(value: &str) -> Result<Url, OAuthError> {
    let url = Url::parse(value)
        .map_err(|_| OAuthError::InvalidConfiguration(INVALID_ENDPOINT_ERROR.to_owned()))?;
    let host = url
        .host_str()
        .ok_or_else(|| OAuthError::InvalidConfiguration(INVALID_ENDPOINT_ERROR.to_owned()))?;

    let allowed = match url.scheme() {
        "https" => true,
        "http" => {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }
        _ => false,
    };
    if !allowed
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(OAuthError::InvalidConfiguration(
            INVALID_ENDPOINT_ERROR.to_owned(),
        ));
    }

    Ok(url)
}

// 按 OAuth grant_type 构造严格的 form-urlencoded 字段，不把缺失字段编码为空值。
fn build_form(request: &OAuthTokenRequest) -> Result<Vec<(&'static str, &str)>, OAuthError> {
    let client_id = required_request_field(&request.client_id)?;
    let resource = required_request_field(&request.resource)?;
    let mut form = vec![("grant_type", request.grant_type.as_str())];

    match request.grant_type.as_str() {
        "authorization_code" => {
            if request.refresh_token.is_some() {
                return Err(OAuthError::InvalidCallback(
                    INVALID_REQUEST_ERROR.to_owned(),
                ));
            }
            let code = request
                .code
                .as_deref()
                .ok_or_else(|| OAuthError::InvalidCallback(INVALID_REQUEST_ERROR.to_owned()))?;
            let redirect_uri = request
                .redirect_uri
                .as_deref()
                .ok_or_else(|| OAuthError::InvalidCallback(INVALID_REQUEST_ERROR.to_owned()))?;
            let code_verifier = request
                .code_verifier
                .as_deref()
                .ok_or_else(|| OAuthError::InvalidCallback(INVALID_REQUEST_ERROR.to_owned()))?;
            validate_request_field(code, MAX_REQUEST_FIELD_BYTES)?;
            validate_request_field(redirect_uri, MAX_REQUEST_FIELD_BYTES)?;
            validate_request_field(code_verifier, MAX_REQUEST_FIELD_BYTES)?;
            form.push(("code", code));
            form.push(("redirect_uri", redirect_uri));
            form.push(("client_id", client_id));
            form.push(("resource", resource));
            form.push(("code_verifier", code_verifier));
        }
        "refresh_token" => {
            if request.code.is_some()
                || request.redirect_uri.is_some()
                || request.code_verifier.is_some()
            {
                return Err(OAuthError::InvalidCallback(
                    INVALID_REQUEST_ERROR.to_owned(),
                ));
            }
            let refresh_token = request
                .refresh_token
                .as_deref()
                .ok_or_else(|| OAuthError::InvalidCallback(INVALID_REQUEST_ERROR.to_owned()))?;
            validate_opaque_value(refresh_token, MAX_REFRESH_TOKEN_BYTES)?;
            form.push(("refresh_token", refresh_token));
            form.push(("client_id", client_id));
            form.push(("resource", resource));
        }
        _ => {
            return Err(OAuthError::InvalidCallback(
                INVALID_REQUEST_ERROR.to_owned(),
            ));
        }
    }

    Ok(form)
}

// 使用现有 url 依赖编码 form-urlencoded，避免把缺失字段转换成空字符串。
fn encode_form(form: &[(&str, &str)]) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (name, value) in form {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

// 校验请求中必须存在且不含控制字符的普通文本字段。
fn required_request_field(value: &str) -> Result<&str, OAuthError> {
    validate_request_field(value, MAX_REQUEST_FIELD_BYTES)?;
    Ok(value)
}

// 校验请求文本的非空、有界和单行约束。
fn validate_request_field(value: &str, limit: usize) -> Result<(), OAuthError> {
    if value.trim().is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(OAuthError::InvalidCallback(
            INVALID_REQUEST_ERROR.to_owned(),
        ));
    }
    Ok(())
}

// 校验 OAuth 不透明令牌的非空、有界和可安全放入 Authorization 头的 ASCII 约束。
fn validate_opaque_value(value: &str, limit: usize) -> Result<(), OAuthError> {
    if value.is_empty() || value.len() > limit || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(OAuthError::InvalidCallback(
            INVALID_RESPONSE_ERROR.to_owned(),
        ));
    }
    Ok(())
}

// 解析非成功响应，只识别标准 invalid_grant，不暴露其他错误码或远端正文。
fn parse_error_response(body: &[u8]) -> OAuthError {
    let value = serde_json::from_slice::<Value>(body).ok();
    if value
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get("error"))
        .and_then(Value::as_str)
        == Some("invalid_grant")
    {
        return OAuthError::AuthorizationDenied {
            code: "invalid_grant".to_owned(),
            description: None,
        };
    }
    OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned())
}

// 解析成功响应并将 expires_in 安全转换为绝对 Unix 秒时间戳。
fn parse_success_response(body: &[u8], now_unix_seconds: u64) -> Result<OAuthTokenSet, OAuthError> {
    let value = serde_json::from_slice::<Value>(body)
        .map_err(|_| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?;
    let object = value
        .as_object()
        .ok_or_else(|| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?;

    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?
        .to_owned();
    validate_opaque_value(&access_token, MAX_ACCESS_TOKEN_BYTES)?;

    let token_type = object
        .get("token_type")
        .and_then(Value::as_str)
        .ok_or_else(|| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?;
    if !token_type.eq_ignore_ascii_case("Bearer") {
        return Err(OAuthError::InvalidCallback(
            INVALID_RESPONSE_ERROR.to_owned(),
        ));
    }

    let expires_in = object
        .get("expires_in")
        .and_then(Value::as_number)
        .filter(|number| number.is_u64())
        .and_then(serde_json::Number::as_u64)
        .ok_or_else(|| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?;
    let expires_at = now_unix_seconds
        .checked_add(expires_in)
        .ok_or_else(|| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?;

    let refresh_token = parse_optional_opaque(object, "refresh_token", MAX_REFRESH_TOKEN_BYTES)?;
    let scope = parse_optional_scope(object)?;

    Ok(OAuthTokenSet {
        access_token,
        token_type: "Bearer".to_owned(),
        expires_at: Some(expires_at),
        refresh_token,
        scope,
    })
}

// 解析可选刷新令牌，字段存在时必须是合法的非空字符串。
fn parse_optional_opaque(
    object: &serde_json::Map<String, Value>,
    name: &str,
    limit: usize,
) -> Result<Option<String>, OAuthError> {
    let Some(value) = object.get(name) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?;
    validate_opaque_value(value, limit)?;
    Ok(Some(value.to_owned()))
}

// 解析可选 scope，并执行 OAuth scope ASCII、长度和空格分隔约束。
fn parse_optional_scope(
    object: &serde_json::Map<String, Value>,
) -> Result<Option<String>, OAuthError> {
    let Some(value) = object.get("scope") else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| OAuthError::InvalidCallback(INVALID_RESPONSE_ERROR.to_owned()))?;
    if value.is_empty()
        || value.len() > MAX_SCOPE_BYTES
        || value.split(' ').any(str::is_empty)
        || !value.bytes().all(|byte| {
            byte == b' '
                || byte == 0x21
                || (0x23..=0x5b).contains(&byte)
                || (0x5d..=0x7e).contains(&byte)
        })
    {
        return Err(OAuthError::InvalidCallback(
            INVALID_RESPONSE_ERROR.to_owned(),
        ));
    }
    Ok(Some(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    use super::*;

    // 测试 HTTP fixture 返回的状态、正文和可选延迟。
    struct FixtureResponse {
        status: u16,
        body: Vec<u8>,
        delay: Duration,
        location: Option<String>,
    }

    // 启动只接受一个请求的本地 HTTP fixture，并返回端点、请求体和任务句柄。
    async fn fixture(response: FixtureResponse) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("应能绑定本地 OAuth fixture");
        let address = listener.local_addr().expect("fixture 应能取得监听地址");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("fixture 应能接受令牌请求");
            let request = read_request(&mut stream).await;
            if !response.delay.is_zero() {
                tokio::time::sleep(response.delay).await;
            }
            let reason = match response.status {
                200 => "OK",
                302 => "Found",
                400 => "Bad Request",
                500 => "Internal Server Error",
                _ => "Test",
            };
            let mut headers = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                response.status,
                reason,
                response.body.len()
            );
            if let Some(location) = response.location {
                headers.push_str(&format!("Location: {location}\r\n"));
            }
            headers.push_str("\r\n");
            let _ = stream.write_all(headers.as_bytes()).await;
            let _ = stream.write_all(&response.body).await;
            request_body(&request)
        });
        (format!("http://{address}/token"), task)
    }

    // 读取 HTTP 请求头和 Content-Length 指定的请求体。
    async fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .expect("fixture 应能读取请求头");
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
            assert!(request.len() < 16 * 1024, "测试请求头不应无限增长");
        }
        let headers = String::from_utf8_lossy(&request);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let old_len = request.len();
        request.resize(old_len + content_length, 0);
        stream
            .read_exact(&mut request[old_len..])
            .await
            .expect("fixture 应能读取请求体");
        request
    }

    // 从完整 HTTP 请求中提取 form-urlencoded 请求体。
    fn request_body(request: &[u8]) -> String {
        let split = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("fixture 请求应包含头体分隔符");
        String::from_utf8(request[split + 4..].to_vec())
            .expect("请求体应为 UTF-8")
            .to_owned()
    }

    // 构造授权码交换请求测试数据。
    fn code_request(endpoint: String) -> OAuthTokenRequest {
        OAuthTokenRequest {
            token_endpoint: endpoint,
            grant_type: "authorization_code".to_owned(),
            code: Some("authorization-code".to_owned()),
            refresh_token: None,
            redirect_uri: Some("http://localhost/callback".to_owned()),
            client_id: "client-id".to_owned(),
            resource: "https://mcp.example.test/resource".to_owned(),
            code_verifier: Some("pkce-verifier".to_owned()),
        }
    }

    // 构造刷新令牌交换请求测试数据。
    fn refresh_request(endpoint: String) -> OAuthTokenRequest {
        OAuthTokenRequest {
            token_endpoint: endpoint,
            grant_type: "refresh_token".to_owned(),
            code: None,
            refresh_token: Some("refresh-token".to_owned()),
            redirect_uri: None,
            client_id: "client-id".to_owned(),
            resource: "https://mcp.example.test/resource".to_owned(),
            code_verifier: None,
        }
    }

    // 解析 form-urlencoded 的测试请求字段，避免额外引入 URL 编码依赖。
    fn form_fields(body: &str) -> HashMap<String, String> {
        body.split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn authorization_code_exchange_sends_exact_form_and_parses_tokens() {
        let (endpoint, task) = fixture(FixtureResponse {
            status: 200,
            body: br#"{"access_token":"access-token","token_type":"bearer","expires_in":60,"refresh_token":"refresh-token","scope":"read write"}"#.to_vec(),
            delay: Duration::ZERO,
            location: None,
        })
        .await;
        let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_secs(2), 4096)
            .expect("应能创建令牌交换器");
        let token_set = exchanger
            .exchange(&code_request(endpoint), 100)
            .await
            .expect("授权码交换应成功");
        assert_eq!(token_set.access_token, "access-token");
        assert_eq!(token_set.token_type, "Bearer");
        assert_eq!(token_set.expires_at, Some(160));
        assert_eq!(token_set.refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(token_set.scope.as_deref(), Some("read write"));

        let fields = form_fields(&task.await.expect("fixture 任务应成功"));
        assert_eq!(
            fields.get("grant_type").map(String::as_str),
            Some("authorization_code")
        );
        assert_eq!(
            fields.get("code").map(String::as_str),
            Some("authorization-code")
        );
        assert_eq!(
            fields.get("redirect_uri").map(String::as_str),
            Some("http%3A%2F%2Flocalhost%2Fcallback")
        );
        assert_eq!(
            fields.get("client_id").map(String::as_str),
            Some("client-id")
        );
        assert_eq!(
            fields.get("resource").map(String::as_str),
            Some("https%3A%2F%2Fmcp.example.test%2Fresource")
        );
        assert_eq!(
            fields.get("code_verifier").map(String::as_str),
            Some("pkce-verifier")
        );
        assert!(!fields.contains_key("refresh_token"));
    }

    #[tokio::test]
    async fn refresh_exchange_sends_only_refresh_fields() {
        let (endpoint, task) = fixture(FixtureResponse {
            status: 200,
            body: br#"{"access_token":"new-access","token_type":"Bearer","expires_in":0}"#.to_vec(),
            delay: Duration::ZERO,
            location: None,
        })
        .await;
        let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_secs(2), 4096)
            .expect("应能创建令牌交换器");
        let token_set = exchanger
            .exchange(&refresh_request(endpoint), 42)
            .await
            .expect("刷新交换应成功");
        assert_eq!(token_set.expires_at, Some(42));
        assert!(token_set.refresh_token.is_none());
        assert!(token_set.scope.is_none());

        let fields = form_fields(&task.await.expect("fixture 任务应成功"));
        assert_eq!(
            fields.get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(
            fields.get("refresh_token").map(String::as_str),
            Some("refresh-token")
        );
        assert_eq!(
            fields.get("client_id").map(String::as_str),
            Some("client-id")
        );
        assert_eq!(
            fields.get("resource").map(String::as_str),
            Some("https%3A%2F%2Fmcp.example.test%2Fresource")
        );
        assert_eq!(fields.len(), 4);
        assert!(!fields.contains_key("code"));
        assert!(!fields.contains_key("redirect_uri"));
        assert!(!fields.contains_key("code_verifier"));
    }

    #[tokio::test]
    async fn invalid_grant_is_distinguishable_without_remote_description() {
        let (endpoint, task) = fixture(FixtureResponse {
            status: 400,
            body: br#"{"error":"invalid_grant","error_description":"secret-code-and-token"}"#
                .to_vec(),
            delay: Duration::ZERO,
            location: None,
        })
        .await;
        let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_secs(2), 4096)
            .expect("应能创建令牌交换器");
        let error = exchanger
            .exchange(&code_request(endpoint), 100)
            .await
            .expect_err("invalid_grant 应失败");
        assert!(matches!(
            error,
            OAuthError::AuthorizationDenied { ref code, description: None }
                if code == "invalid_grant"
        ));
        assert!(!format!("{error:?}").contains("secret-code-and-token"));
        let _ = task.await.expect("fixture 任务应成功");
    }

    #[tokio::test]
    async fn malformed_success_responses_are_rejected() {
        let malformed = [
            br#"[]"#.to_vec(),
            br#"{"access_token":"","token_type":"Bearer","expires_in":60}"#.to_vec(),
            br#"{"access_token":"a","token_type":"Basic","expires_in":60}"#.to_vec(),
            br#"{"access_token":"a","token_type":"Bearer","expires_in":-1}"#.to_vec(),
            br#"{"access_token":"a","token_type":"Bearer","expires_in":1.5}"#.to_vec(),
            br#"{"access_token":"a","token_type":"Bearer","expires_in":60,"refresh_token":null}"#
                .to_vec(),
            br#"{"access_token":"a","token_type":"Bearer","expires_in":60,"scope":"read  write"}"#
                .to_vec(),
        ];
        for body in malformed {
            let (endpoint, task) = fixture(FixtureResponse {
                status: 200,
                body,
                delay: Duration::ZERO,
                location: None,
            })
            .await;
            let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_secs(2), 4096)
                .expect("应能创建令牌交换器");
            let error = exchanger
                .exchange(&code_request(endpoint), 100)
                .await
                .expect_err("畸形令牌响应应失败");
            assert!(matches!(error, OAuthError::InvalidCallback(_)));
            let _ = task.await.expect("fixture 任务应成功");
        }
    }

    #[tokio::test]
    async fn oversized_response_is_rejected_before_json_parse() {
        let (endpoint, task) = fixture(FixtureResponse {
            status: 200,
            body: vec![b'x'; 128],
            delay: Duration::ZERO,
            location: None,
        })
        .await;
        let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_secs(2), 64)
            .expect("应能创建令牌交换器");
        let error = exchanger
            .exchange(&code_request(endpoint), 100)
            .await
            .expect_err("超大响应应失败");
        assert!(
            matches!(error, OAuthError::InvalidCallback(message) if message == RESPONSE_TOO_LARGE_ERROR)
        );
        let _ = task.await.expect("fixture 任务应成功");
    }

    #[tokio::test]
    async fn timeout_and_redirect_are_bounded_and_not_followed() {
        let (endpoint, task) = fixture(FixtureResponse {
            status: 200,
            body: br#"{"access_token":"a","token_type":"Bearer","expires_in":60}"#.to_vec(),
            delay: Duration::from_millis(250),
            location: None,
        })
        .await;
        let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_millis(40), 4096)
            .expect("应能创建令牌交换器");
        let error = exchanger
            .exchange(&code_request(endpoint), 100)
            .await
            .expect_err("超时响应应失败");
        assert!(
            matches!(error, OAuthError::DiscoveryTransport(message) if message == REQUEST_TIMEOUT_ERROR)
        );
        let _ = task.await.expect("fixture 任务应成功");

        let (endpoint, task) = fixture(FixtureResponse {
            status: 302,
            body: br#"{"access_token":"should-not-be-read"}"#.to_vec(),
            delay: Duration::ZERO,
            location: Some("http://127.0.0.1:9/redirect".to_owned()),
        })
        .await;
        let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_secs(2), 4096)
            .expect("应能创建令牌交换器");
        let error = exchanger
            .exchange(&code_request(endpoint), 100)
            .await
            .expect_err("重定向应被拒绝");
        assert!(matches!(error, OAuthError::InvalidCallback(message) if message == REDIRECT_ERROR));
        let _ = task.await.expect("fixture 任务应成功");
    }

    #[tokio::test]
    async fn expiry_overflow_and_unsafe_http_endpoint_are_rejected() {
        let (endpoint, task) = fixture(FixtureResponse {
            status: 200,
            body: br#"{"access_token":"a","token_type":"Bearer","expires_in":1}"#.to_vec(),
            delay: Duration::ZERO,
            location: None,
        })
        .await;
        let exchanger = ReqwestOAuthTokenExchanger::new(Duration::from_secs(2), 4096)
            .expect("应能创建令牌交换器");
        let error = exchanger
            .exchange(&code_request(endpoint), u64::MAX)
            .await
            .expect_err("过期时间溢出应失败");
        assert!(
            matches!(error, OAuthError::InvalidCallback(message) if message == INVALID_RESPONSE_ERROR)
        );
        let _ = task.await.expect("fixture 任务应成功");

        let mut request = code_request("http://example.com/token".to_owned());
        request.code = Some("sensitive-code".to_owned());
        let error = exchanger
            .exchange(&request, 100)
            .await
            .expect_err("非回环 HTTP 端点应在发送前拒绝");
        assert!(
            matches!(error, OAuthError::InvalidConfiguration(message) if message == INVALID_ENDPOINT_ERROR)
        );
    }

    #[test]
    fn sensitive_values_are_redacted_by_existing_debug_contracts() {
        let request = OAuthTokenRequest {
            token_endpoint: "https://oauth.example.test/token?secret=query".to_owned(),
            grant_type: "authorization_code".to_owned(),
            code: Some("secret-code".to_owned()),
            refresh_token: None,
            redirect_uri: Some("https://app.example.test/callback".to_owned()),
            client_id: "client-id".to_owned(),
            resource: "https://mcp.example.test/resource?secret=query".to_owned(),
            code_verifier: Some("secret-verifier".to_owned()),
        };
        let token_set = OAuthTokenSet {
            access_token: "secret-access".to_owned(),
            token_type: "Bearer".to_owned(),
            expires_at: Some(100),
            refresh_token: Some("secret-refresh".to_owned()),
            scope: Some("read".to_owned()),
        };
        let request_debug = format!("{request:?}");
        let token_debug = format!("{token_set:?}");
        assert!(!request_debug.contains("secret-code"));
        assert!(!request_debug.contains("secret-verifier"));
        assert!(!request_debug.contains("secret=query"));
        assert!(!token_debug.contains("secret-access"));
        assert!(!token_debug.contains("secret-refresh"));
    }

    #[test]
    fn constructor_rejects_zero_limits() {
        assert!(matches!(
            ReqwestOAuthTokenExchanger::new(Duration::ZERO, 1),
            Err(OAuthError::InvalidConfiguration(_))
        ));
        assert!(matches!(
            ReqwestOAuthTokenExchanger::new(Duration::from_secs(1), 0),
            Err(OAuthError::InvalidConfiguration(_))
        ));
    }
}
