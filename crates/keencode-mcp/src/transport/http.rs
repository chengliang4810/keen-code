//! MCP Streamable HTTP POST、JSON 与可恢复 SSE 传输。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, WWW_AUTHENTICATE,
};
use reqwest::{Client, Response, StatusCode, Url};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::McpTransport;
use crate::config::{McpClientOptions, StreamableHttpConfig};
use crate::error::McpError;
use crate::protocol::{
    IncomingMessage, JsonRpcNotification, JsonRpcRequest, McpNotification, RequestId,
    parse_incoming, server_request_response,
};

const MAX_SSE_RECONNECTS: usize = 3;
const DEFAULT_SSE_RETRY: Duration = Duration::from_millis(250);
const MAX_SSE_RETRY: Duration = Duration::from_secs(5);
const MAX_SESSION_ID_BYTES: usize = 1024;

pub(super) struct StreamableHttpTransport {
    runtime: HttpRuntime,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    terminate_session_on_close: bool,
    listener_task: Mutex<Option<ListenerHandle>>,
    closed: AtomicBool,
}

#[derive(Clone)]
struct HttpRuntime {
    client: Client,
    endpoint: Url,
    headers: HeaderMap,
    auth_provider: Option<Arc<dyn crate::auth::McpAuthProvider>>,
    protocol_version: String,
    session: Arc<StdMutex<HttpSessionState>>,
    notifications: broadcast::Sender<McpNotification>,
    max_response_bytes: usize,
}

#[derive(Default)]
struct HttpSessionState {
    id: Option<String>,
    generation: u64,
    expired: bool,
    /// 建立 MCP 会话时绑定的认证状态；动态认证未授权时也必须与有令牌状态区分。
    auth_binding: AuthBinding,
}

#[derive(Clone)]
struct HttpSessionSnapshot {
    id: Option<String>,
    generation: u64,
    expired: bool,
    /// 请求快照对应的认证状态；用于阻止旧会话携带新令牌或空令牌。
    auth_binding: AuthBinding,
}

/// MCP 会话建立时冻结的动态认证状态。
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum AuthBinding {
    /// 未配置动态认证，令牌由静态请求头或服务端策略决定。
    #[default]
    Static,
    /// 已配置动态认证；内部 None 表示提供方当前没有访问令牌。
    Dynamic {
        /// 当前访问令牌代次；无令牌时为空。
        generation: Option<u64>,
    },
}

/// 一次 HTTP 请求使用的认证代次。
#[derive(Clone, Copy)]
struct AuthRequestContext {
    /// 本次请求实际构造 Authorization 头时的认证状态。
    binding: AuthBinding,
    /// 当前逻辑请求是否仍允许触发一次认证刷新。
    allow_refresh: bool,
}

/// HTTP 响应处理中的内部认证控制流。
enum HttpResponseError {
    /// 初次 initialize 无旧 MCP Session 时允许重新取令牌后重试一次。
    RetryInitialAuth,
    /// 已归一化的对外 MCP 错误。
    Mcp(McpError),
}

impl From<McpError> for HttpResponseError {
    fn from(error: McpError) -> Self {
        Self::Mcp(error)
    }
}

struct ListenerHandle {
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

impl StreamableHttpTransport {
    pub(super) fn connect(
        config: StreamableHttpConfig,
        options: &McpClientOptions,
    ) -> Result<Self, McpError> {
        let endpoint = config.validate()?;
        let headers = parse_headers(&config.headers)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                McpError::Transport(format!("创建 MCP HTTP 客户端失败：{}", error.without_url()))
            })?;
        let (notifications, _) = broadcast::channel(options.notification_capacity);
        Ok(Self {
            runtime: HttpRuntime {
                client,
                endpoint,
                headers,
                auth_provider: config.auth_provider,
                protocol_version: options.protocol_version.clone(),
                session: Arc::new(StdMutex::new(HttpSessionState::default())),
                notifications,
                max_response_bytes: options.max_response_bytes,
            },
            request_timeout: options.request_timeout,
            shutdown_timeout: options.shutdown_timeout,
            terminate_session_on_close: config.terminate_session_on_close,
            listener_task: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    async fn send<T: Serialize>(
        &self,
        message: &T,
        expected_id: Option<&RequestId>,
        session: HttpSessionSnapshot,
        is_initialize: bool,
    ) -> Result<Option<Value>, McpError> {
        // 只有真正的首次 initialize 才能在无会话时因 401 重新取令牌；
        // 不能把“有请求 ID 且当前无 Session”误当成 initialize。
        let allow_initial_auth_retry = is_initialize && session.id.is_none();
        let mut retried_initial_auth = false;
        loop {
            let (builder, mut auth) = self
                .runtime
                .request_builder(
                    reqwest::Method::POST,
                    &session,
                    None,
                    is_initialize && session.id.is_none(),
                )
                .await?;
            auth.allow_refresh = !retried_initial_auth;
            match self
                .send_once(message, expected_id, session.clone(), builder, auth)
                .await
            {
                Ok(result) => return Ok(result),
                Err(HttpResponseError::RetryInitialAuth)
                    if allow_initial_auth_retry && !retried_initial_auth =>
                {
                    retried_initial_auth = true;
                }
                Err(HttpResponseError::RetryInitialAuth) => {
                    return Err(McpError::Transport(
                        "MCP HTTP 身份认证失败，初次 initialize 重试次数已用尽".to_owned(),
                    ));
                }
                Err(HttpResponseError::Mcp(error)) => return Err(error),
            }
        }
    }

    /// 发送一次不可重试的 HTTP POST，并把响应交给协议解析器。
    async fn send_once<T: Serialize>(
        &self,
        message: &T,
        expected_id: Option<&RequestId>,
        session: HttpSessionSnapshot,
        builder: reqwest::RequestBuilder,
        auth: AuthRequestContext,
    ) -> Result<Option<Value>, HttpResponseError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(McpError::NotReady("Streamable HTTP 传输已经关闭".to_owned()).into());
        }
        let body = serde_json::to_vec(message)
            .map_err(|error| McpError::Protocol(format!("JSON-RPC 序列化失败：{error}")))?;
        if body.len() > self.runtime.max_response_bytes {
            return Err(McpError::ResponseTooLarge {
                limit: self.runtime.max_response_bytes,
            }
            .into());
        }
        let response = builder
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(body)
            .send()
            .await
            .map_err(|error| {
                McpError::Transport(format!("MCP HTTP POST 失败：{}", error.without_url()))
            })?;
        self.parse_response(response, expected_id, session, auth)
            .await
    }

    async fn parse_response(
        &self,
        response: Response,
        expected_id: Option<&RequestId>,
        sent_session: HttpSessionSnapshot,
        sent_auth: AuthRequestContext,
    ) -> Result<Option<Value>, HttpResponseError> {
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            if !sent_auth.allow_refresh {
                let _ = read_bounded(response, self.runtime.max_response_bytes).await?;
                return Err(McpError::Transport(
                    "MCP HTTP 返回 401，认证刷新重试次数已用尽".to_owned(),
                )
                .into());
            }
            let outcome = self
                .runtime
                .handle_unauthorized(&sent_session, sent_auth.binding, response)
                .await?;
            return Err(outcome);
        }
        if status == StatusCode::NOT_FOUND && sent_session.id.is_some() {
            self.runtime.expire_if_current(&sent_session);
            return Err(McpError::SessionExpired.into());
        }
        if !status.is_success() {
            let _ = read_bounded(response, self.runtime.max_response_bytes).await?;
            return Err(McpError::Transport(format!(
                "MCP HTTP 返回 {status}；响应正文已省略以避免敏感信息进入日志"
            ))
            .into());
        }
        self.runtime
            .capture_session_id(&response, &sent_session, sent_auth.binding)
            .map_err(HttpResponseError::from)?;
        if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
            return if expected_id.is_some() {
                Err(McpError::Protocol("MCP 请求没有返回 JSON-RPC 响应".to_owned()).into())
            } else {
                Ok(None)
            };
        }

        match response_content_type(&response)?.as_str() {
            "application/json" => {
                let body = read_bounded(response, self.runtime.max_response_bytes).await?;
                if body.is_empty() {
                    return if expected_id.is_some() {
                        Err(McpError::Protocol("MCP HTTP 请求响应体为空".to_owned()).into())
                    } else {
                        Ok(None)
                    };
                }
                let result = self.runtime.process_message(&body, expected_id).await?;
                if expected_id.is_some() && result.is_none() {
                    Err(
                        McpError::Protocol("MCP HTTP JSON 响应没有匹配的 JSON-RPC 响应".to_owned())
                            .into(),
                    )
                } else {
                    Ok(result)
                }
            }
            "text/event-stream" => {
                self.parse_sse_stream(response, expected_id, sent_auth)
                    .await
            }
            content_type => Err(McpError::Protocol(format!(
                "MCP HTTP 返回不支持的 Content-Type：{content_type}"
            ))
            .into()),
        }
    }

    async fn parse_sse_stream(
        &self,
        response: Response,
        expected_id: Option<&RequestId>,
        sent_auth: AuthRequestContext,
    ) -> Result<Option<Value>, HttpResponseError> {
        let mut response = response;
        let mut last_event_id = None;
        let mut retry_delay = DEFAULT_SSE_RETRY;
        let mut reconnects = 0;
        loop {
            let mut stream = response.bytes_stream();
            let mut decoder = SseDecoder::new(self.runtime.max_response_bytes);
            let mut stream_error = None;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        stream_error = Some(McpError::Transport(format!(
                            "读取 MCP SSE 响应失败：{}",
                            error.without_url()
                        )));
                        break;
                    }
                };
                for event in decoder.push(&chunk)? {
                    if let Some(result) = self
                        .runtime
                        .process_sse_event(event, expected_id, &mut last_event_id, &mut retry_delay)
                        .await?
                    {
                        return Ok(Some(result));
                    }
                }
            }
            if stream_error.is_none() {
                for event in decoder.finish()? {
                    if let Some(result) = self
                        .runtime
                        .process_sse_event(event, expected_id, &mut last_event_id, &mut retry_delay)
                        .await?
                    {
                        return Ok(Some(result));
                    }
                }
            }
            if expected_id.is_none() {
                return stream_error.map_or(Ok(None), |error| Err(error.into()));
            }
            let event_id = match last_event_id.as_deref() {
                Some(event_id) => event_id,
                None => {
                    return Err(stream_error
                        .unwrap_or_else(|| {
                            McpError::Protocol(
                                "MCP SSE 在响应完成前断开，且没有可用于恢复的事件 ID".to_owned(),
                            )
                        })
                        .into());
                }
            };
            if reconnects >= MAX_SSE_RECONNECTS {
                return Err(McpError::Protocol(format!(
                    "MCP SSE 连续恢复超过 {MAX_SSE_RECONNECTS} 次"
                ))
                .into());
            }
            reconnects += 1;
            tokio::time::sleep(retry_delay).await;
            let session = self.runtime.session_snapshot();
            if session.expired {
                return Err(McpError::SessionExpired.into());
            }
            let (builder, auth) = self
                .runtime
                .request_builder(reqwest::Method::GET, &session, Some(event_id), false)
                .await?;
            let resumed = builder
                .header(ACCEPT, "text/event-stream")
                .send()
                .await
                .map_err(|error| {
                    McpError::Transport(format!("恢复 MCP SSE 失败：{}", error.without_url()))
                })?;
            response = self
                .runtime
                .validate_sse_response(
                    resumed,
                    &session,
                    AuthRequestContext {
                        allow_refresh: sent_auth.allow_refresh,
                        ..auth
                    },
                )
                .await?;
        }
    }

    async fn start_listening_internal(&self, force_restart: bool) -> Result<(), McpError> {
        let (ready, cancellation) = {
            let mut listener = self.listener_task.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err(McpError::NotReady(
                    "Streamable HTTP 传输已经关闭".to_owned(),
                ));
            }
            if !force_restart
                && listener.as_ref().is_some_and(|handle| {
                    !handle.task.is_finished() && !handle.cancellation.is_cancelled()
                })
            {
                return Ok(());
            }
            if let Some(previous) = listener.take() {
                previous.cancellation.cancel();
                previous.task.abort();
            }
            let cancellation = CancellationToken::new();
            let (ready_sender, ready) = oneshot::channel();
            let task = spawn_get_listener(self.runtime.clone(), cancellation.clone(), ready_sender);
            *listener = Some(ListenerHandle {
                cancellation: cancellation.clone(),
                task,
            });
            (ready, cancellation)
        };
        match tokio::time::timeout(self.request_timeout, ready).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) if self.closed.load(Ordering::Acquire) => Err(McpError::NotReady(
                "Streamable HTTP 传输已经关闭".to_owned(),
            )),
            Ok(Err(_)) => Err(McpError::Transport(
                "MCP GET SSE 监听任务在就绪前退出".to_owned(),
            )),
            Err(_) => {
                cancellation.cancel();
                // 超时后的监听句柄不能继续伪装成健康监听；否则紧随其后的
                // subscribe_notifications 会看到未完成任务并直接返回成功。
                let mut listener = self.listener_task.lock().await;
                if listener
                    .as_ref()
                    .is_some_and(|handle| handle.cancellation.is_cancelled())
                {
                    if let Some(listener) = listener.take() {
                        listener.task.abort();
                    }
                }
                Err(McpError::Timeout {
                    method: "HTTP GET SSE listener".to_owned(),
                    duration: self.request_timeout,
                })
            }
        }
    }

    fn force_close_resources(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut listener) = self.listener_task.try_lock() {
            if let Some(listener) = listener.take() {
                listener.cancellation.cancel();
                listener.task.abort();
            }
        }
    }
}

#[async_trait]
impl McpTransport for StreamableHttpTransport {
    async fn request(&self, request: JsonRpcRequest) -> Result<Value, McpError> {
        let is_initialize = request.method == "initialize";
        let session = self.runtime.session_snapshot();
        if session.expired && !is_initialize {
            return Err(McpError::SessionExpired);
        }
        let expected_id = request.id.clone();
        let result = self
            .send(&request, Some(&expected_id), session.clone(), is_initialize)
            .await?
            .ok_or_else(|| McpError::Protocol("MCP HTTP 请求没有响应结果".to_owned()))?;
        if is_initialize {
            self.runtime.mark_initialized(&session);
        }
        Ok(result)
    }

    async fn notify(&self, notification: JsonRpcNotification) -> Result<(), McpError> {
        let session = self.runtime.session_snapshot();
        if session.expired {
            return Err(McpError::SessionExpired);
        }
        self.send(&notification, None, session, false)
            .await
            .map(|_| ())
    }

    fn subscribe(&self) -> broadcast::Receiver<McpNotification> {
        self.runtime.notifications.subscribe()
    }

    async fn start_listening(&self) -> Result<(), McpError> {
        self.start_listening_internal(false).await
    }

    async fn restart_listening(&self) -> Result<(), McpError> {
        let was_requested = self.listener_task.lock().await.is_some();
        if was_requested {
            self.start_listening_internal(true).await
        } else {
            Ok(())
        }
    }

    async fn close(&self) -> Result<(), McpError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let close = async {
            if let Some(listener) = self.listener_task.lock().await.take() {
                listener.cancellation.cancel();
                let _ = listener.task.await;
            }

            let session = self.runtime.session_snapshot();
            if !self.terminate_session_on_close || session.id.is_none() {
                return Ok(());
            }
            let (builder, auth) = self
                .runtime
                .request_builder(reqwest::Method::DELETE, &session, None, false)
                .await?;
            let response = builder.send().await.map_err(|error| {
                McpError::Transport(format!("终止 MCP HTTP 会话失败：{}", error.without_url()))
            })?;
            if response.status() == StatusCode::UNAUTHORIZED {
                let outcome = self
                    .runtime
                    .handle_unauthorized(&session, auth.binding, response)
                    .await
                    .map_err(|error| match error {
                        HttpResponseError::Mcp(error) => error,
                        HttpResponseError::RetryInitialAuth => McpError::Transport(
                            "MCP HTTP DELETE 身份认证失败，不能重试会话终止".to_owned(),
                        ),
                    })?;
                return match outcome {
                    HttpResponseError::Mcp(error) => Err(error),
                    HttpResponseError::RetryInitialAuth => Err(McpError::Transport(
                        "MCP HTTP DELETE 身份认证失败，不能重试会话终止".to_owned(),
                    )),
                };
            }
            if response.status().is_success()
                || matches!(
                    response.status(),
                    StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
                )
            {
                Ok(())
            } else {
                Err(McpError::Transport(format!(
                    "终止 MCP HTTP 会话时返回 {}",
                    response.status()
                )))
            }
        };
        match tokio::time::timeout(self.shutdown_timeout, close).await {
            Ok(result) => result,
            Err(_) => {
                self.force_close_resources();
                Err(McpError::Timeout {
                    method: "Streamable HTTP close".to_owned(),
                    duration: self.shutdown_timeout,
                })
            }
        }
    }

    fn force_close(&self) {
        self.force_close_resources();
    }
}

impl Drop for StreamableHttpTransport {
    fn drop(&mut self) {
        self.force_close_resources();
    }
}

impl HttpRuntime {
    fn session_snapshot(&self) -> HttpSessionSnapshot {
        let session = lock_unpoisoned(&self.session);
        HttpSessionSnapshot {
            id: session.id.clone(),
            generation: session.generation,
            expired: session.expired,
            auth_binding: session.auth_binding,
        }
    }

    async fn request_builder(
        &self,
        method: reqwest::Method,
        session: &HttpSessionSnapshot,
        last_event_id: Option<&str>,
        allow_expired_session: bool,
    ) -> Result<(reqwest::RequestBuilder, AuthRequestContext), McpError> {
        if session.expired && !allow_expired_session {
            return Err(McpError::SessionExpired);
        }
        let auth_token = match &self.auth_provider {
            Some(provider) => provider.access_token().await?,
            None => None,
        };
        let auth = AuthRequestContext {
            binding: match self.auth_provider {
                Some(_) => AuthBinding::Dynamic {
                    generation: auth_token.as_ref().map(|token| token.generation),
                },
                None => AuthBinding::Static,
            },
            allow_refresh: true,
        };
        if session.id.is_some() && session.auth_binding != auth.binding {
            // 会话与令牌代次不再绑定时，先使当前会话失效，再返回给上层重新初始化；
            // 本次请求不会继续带着旧 MCP-Session-Id 发出。
            self.expire_if_current(session);
            return Err(McpError::SessionExpired);
        }
        let mut builder = self
            .client
            .request(method, self.endpoint.clone())
            .headers(self.headers.clone())
            .header("MCP-Protocol-Version", &self.protocol_version);
        if let Some(token) = auth_token {
            if token.token.is_empty() {
                return Err(McpError::Configuration("动态访问令牌不得为空".to_owned()));
            }
            let value =
                HeaderValue::from_str(&format!("Bearer {}", token.token)).map_err(|_| {
                    McpError::Configuration(
                        "动态访问令牌不能安全写入 Authorization 请求头".to_owned(),
                    )
                })?;
            builder = builder.header(AUTHORIZATION, value);
        }
        if let Some(session_id) = &session.id {
            builder = builder.header("MCP-Session-Id", session_id);
        }
        if let Some(last_event_id) = last_event_id {
            let value = HeaderValue::from_str(last_event_id).map_err(|_| {
                McpError::Protocol("MCP SSE 事件 ID 不能安全写入 Last-Event-ID".to_owned())
            })?;
            builder = builder.header(HeaderName::from_static("last-event-id"), value);
        }
        Ok((builder, auth))
    }

    /// 处理一次 401，并在认证代次变化时使仍绑定旧令牌的 MCP Session 失效。
    async fn handle_unauthorized(
        &self,
        sent_session: &HttpSessionSnapshot,
        sent_binding: AuthBinding,
        response: Response,
    ) -> Result<HttpResponseError, HttpResponseError> {
        let challenge = response
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let Some(provider) = &self.auth_provider else {
            let _ = read_bounded(response, self.max_response_bytes).await?;
            return Ok(HttpResponseError::Mcp(McpError::Transport(
                "MCP HTTP 返回 401，未配置动态认证提供方".to_owned(),
            )));
        };
        provider
            .on_unauthorized(
                match sent_binding {
                    AuthBinding::Dynamic { generation } => generation.unwrap_or(0),
                    AuthBinding::Static => 0,
                },
                challenge.as_deref(),
            )
            .await
            .map_err(HttpResponseError::from)?;
        let current_binding = AuthBinding::Dynamic {
            generation: provider
                .access_token()
                .await
                .map_err(HttpResponseError::from)?
                .map(|token| token.generation),
        };
        let _ = read_bounded(response, self.max_response_bytes).await?;
        if current_binding != sent_binding {
            if sent_session.id.is_some() {
                self.expire_if_current(sent_session);
                return Ok(HttpResponseError::Mcp(McpError::SessionExpired));
            }
            return Ok(HttpResponseError::RetryInitialAuth);
        }
        Ok(HttpResponseError::Mcp(McpError::Transport(
            "MCP HTTP 返回 401，认证代次未变化".to_owned(),
        )))
    }

    fn capture_session_id(
        &self,
        response: &Response,
        sent_session: &HttpSessionSnapshot,
        sent_auth_binding: AuthBinding,
    ) -> Result<(), McpError> {
        let Some(value) = response.headers().get("MCP-Session-Id") else {
            return Ok(());
        };
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > MAX_SESSION_ID_BYTES
            || !bytes.iter().all(|byte| matches!(byte, 0x21..=0x7e))
        {
            return Err(McpError::Protocol(
                "MCP-Session-Id 必须是有界的可见 ASCII".to_owned(),
            ));
        }
        let value = value
            .to_str()
            .map_err(|_| McpError::Protocol("MCP-Session-Id 不是有效可见 ASCII".to_owned()))?;
        let mut current = lock_unpoisoned(&self.session);
        if current.generation != sent_session.generation {
            return Ok(());
        }
        if current
            .id
            .as_deref()
            .is_some_and(|current| current != value)
        {
            return Err(McpError::Protocol(
                "服务端在同一会话中更换 MCP-Session-Id".to_owned(),
            ));
        }
        if current.id.is_none() {
            current.id = Some(value.to_owned());
            current.expired = false;
            current.auth_binding = sent_auth_binding;
            current.generation = current.generation.wrapping_add(1);
        }
        Ok(())
    }

    fn expire_if_current(&self, sent_session: &HttpSessionSnapshot) {
        if sent_session.id.is_none() {
            return;
        }
        let mut current = lock_unpoisoned(&self.session);
        if current.generation == sent_session.generation && current.id == sent_session.id {
            current.id = None;
            current.expired = true;
            current.auth_binding = AuthBinding::default();
            current.generation = current.generation.wrapping_add(1);
        }
    }

    fn mark_initialized(&self, sent_session: &HttpSessionSnapshot) {
        let mut current = lock_unpoisoned(&self.session);
        if current.expired
            && (current.generation == sent_session.generation || current.id.is_some())
        {
            current.expired = false;
            current.generation = current.generation.wrapping_add(1);
        }
    }

    async fn process_message(
        &self,
        message: &[u8],
        expected_id: Option<&RequestId>,
    ) -> Result<Option<Value>, McpError> {
        match parse_incoming(message)? {
            IncomingMessage::Notification(notification) => {
                let _ = self.notifications.send(notification);
                Ok(None)
            }
            IncomingMessage::Response(response) => {
                let expected_id = expected_id.ok_or_else(|| {
                    McpError::Protocol("MCP 通知流收到意外 JSON-RPC 响应".to_owned())
                })?;
                response.into_result(expected_id).map(Some)
            }
            IncomingMessage::ServerRequest { id, method } => {
                self.post_server_response(server_request_response(id, &method))
                    .await?;
                Ok(None)
            }
        }
    }

    async fn process_sse_event(
        &self,
        event: SseEvent,
        expected_id: Option<&RequestId>,
        last_event_id: &mut Option<String>,
        retry_delay: &mut Duration,
    ) -> Result<Option<Value>, McpError> {
        if let Some(id) = event.id {
            *last_event_id = (!id.is_empty()).then_some(id);
        }
        if let Some(retry) = event.retry {
            *retry_delay = Duration::from_millis(retry).min(MAX_SSE_RETRY);
        }
        match event.data {
            Some(data) if !data.is_empty() => self.process_message(&data, expected_id).await,
            _ => Ok(None),
        }
    }

    async fn post_server_response(&self, message: Value) -> Result<(), McpError> {
        let body = serde_json::to_vec(&message)
            .map_err(|error| McpError::Protocol(format!("JSON-RPC 序列化失败：{error}")))?;
        if body.len() > self.max_response_bytes {
            return Err(McpError::ResponseTooLarge {
                limit: self.max_response_bytes,
            });
        }
        let session = self.session_snapshot();
        if session.expired {
            return Err(McpError::SessionExpired);
        }
        let (builder, auth) = self
            .request_builder(reqwest::Method::POST, &session, None, false)
            .await?;
        let response = builder
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(body)
            .send()
            .await
            .map_err(|error| {
                McpError::Transport(format!(
                    "发送 MCP 服务端请求响应失败：{}",
                    error.without_url()
                ))
            })?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let outcome = self
                .handle_unauthorized(&session, auth.binding, response)
                .await
                .map_err(http_response_error)?;
            return match outcome {
                HttpResponseError::Mcp(error) => Err(error),
                HttpResponseError::RetryInitialAuth => Err(McpError::Transport(
                    "MCP 服务端请求响应认证失败，不能重试有副作用 POST".to_owned(),
                )),
            };
        }
        if response.status() == StatusCode::NOT_FOUND && session.id.is_some() {
            self.expire_if_current(&session);
            return Err(McpError::SessionExpired);
        }
        if response.status() != StatusCode::ACCEPTED && response.status() != StatusCode::NO_CONTENT
        {
            let status = response.status();
            let _ = read_bounded(response, self.max_response_bytes).await?;
            return Err(McpError::Protocol(format!(
                "MCP 服务端请求响应 POST 返回 {status}，预期 202 或 204"
            )));
        }
        self.capture_session_id(&response, &session, auth.binding)
    }

    async fn validate_sse_response(
        &self,
        response: Response,
        sent_session: &HttpSessionSnapshot,
        sent_auth: AuthRequestContext,
    ) -> Result<Response, McpError> {
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            if !sent_auth.allow_refresh {
                let _ = read_bounded(response, self.max_response_bytes).await?;
                return Err(McpError::Transport(
                    "MCP GET SSE 返回 401，认证刷新重试次数已用尽".to_owned(),
                ));
            }
            let outcome = self
                .handle_unauthorized(sent_session, sent_auth.binding, response)
                .await
                .map_err(http_response_error)?;
            return match outcome {
                HttpResponseError::Mcp(error) => Err(error),
                HttpResponseError::RetryInitialAuth => {
                    Err(McpError::Transport("MCP GET SSE 身份认证失败".to_owned()))
                }
            };
        }
        if status == StatusCode::NOT_FOUND && sent_session.id.is_some() {
            self.expire_if_current(sent_session);
            return Err(McpError::SessionExpired);
        }
        if !status.is_success() {
            let _ = read_bounded(response, self.max_response_bytes).await?;
            return Err(McpError::Transport(format!("MCP GET SSE 返回 {status}")));
        }
        self.capture_session_id(&response, sent_session, sent_auth.binding)?;
        if response_content_type(&response)? != "text/event-stream" {
            return Err(McpError::Protocol(
                "MCP GET SSE 响应必须使用 text/event-stream".to_owned(),
            ));
        }
        Ok(response)
    }
}

fn spawn_get_listener(
    runtime: HttpRuntime,
    cancellation: CancellationToken,
    ready: oneshot::Sender<Result<(), McpError>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_get_listener(runtime, cancellation, ready).await;
    })
}

async fn run_get_listener(
    runtime: HttpRuntime,
    cancellation: CancellationToken,
    ready: oneshot::Sender<Result<(), McpError>>,
) {
    let mut ready = Some(ready);
    let mut last_event_id = None;
    let mut retry_delay = DEFAULT_SSE_RETRY;
    for reconnect in 0..=MAX_SSE_RECONNECTS {
        let session = runtime.session_snapshot();
        if session.expired {
            send_listener_ready(&mut ready, Err(McpError::SessionExpired));
            return;
        }
        let (request, auth) = match runtime
            .request_builder(
                reqwest::Method::GET,
                &session,
                last_event_id.as_deref(),
                false,
            )
            .await
        {
            Ok((request, auth)) => (request.header(ACCEPT, "text/event-stream"), auth),
            Err(error) => {
                send_listener_ready(&mut ready, Err(error));
                return;
            }
        };
        let response = tokio::select! {
            _ = cancellation.cancelled() => return,
            response = request.send() => match response {
                Ok(response) => response,
                Err(error) => {
                    send_listener_ready(
                        &mut ready,
                        Err(McpError::Transport(format!(
                            "MCP GET SSE 失败：{}",
                            error.without_url()
                        ))),
                    );
                    return;
                }
            }
        };
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            send_listener_ready(&mut ready, Ok(()));
            return;
        }
        let response = match runtime
            .validate_sse_response(response, &session, auth)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                send_listener_ready(&mut ready, Err(error));
                return;
            }
        };
        send_listener_ready(&mut ready, Ok(()));
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::new(runtime.max_response_bytes);
        let mut stream_failed = false;
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => return,
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else {
                break;
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => {
                    stream_failed = true;
                    break;
                }
            };
            let events = match decoder.push(&chunk) {
                Ok(events) => events,
                Err(_) => return,
            };
            for event in events {
                if runtime
                    .process_sse_event(event, None, &mut last_event_id, &mut retry_delay)
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        if !stream_failed {
            let events = match decoder.finish() {
                Ok(events) => events,
                Err(_) => return,
            };
            for event in events {
                if runtime
                    .process_sse_event(event, None, &mut last_event_id, &mut retry_delay)
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        if reconnect == MAX_SSE_RECONNECTS || last_event_id.is_none() {
            return;
        }
        tokio::select! {
            _ = cancellation.cancelled() => return,
            _ = tokio::time::sleep(retry_delay) => {}
        }
    }
}

fn send_listener_ready(
    ready: &mut Option<oneshot::Sender<Result<(), McpError>>>,
    result: Result<(), McpError>,
) {
    if let Some(ready) = ready.take() {
        let _ = ready.send(result);
    }
}

fn response_content_type(response: &Response) -> Result<String, McpError> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .ok_or_else(|| McpError::Protocol("MCP HTTP 响应缺少 Content-Type".to_owned()))
}

fn parse_headers(
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<HeaderMap, McpError> {
    let mut parsed = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            McpError::Configuration(format!("MCP HTTP 请求头名称无效：{error}"))
        })?;
        if matches!(
            name.as_str(),
            "content-type"
                | "accept"
                | "last-event-id"
                | "mcp-protocol-version"
                | "mcp-session-id"
                | "host"
        ) {
            return Err(McpError::Configuration(format!(
                "MCP HTTP 请求头 {} 由传输层管理，不能覆盖",
                name.as_str()
            )));
        }
        let value = HeaderValue::from_str(value).map_err(|error| {
            McpError::Configuration(format!(
                "MCP HTTP 请求头 {} 的值无效：{error}",
                name.as_str()
            ))
        })?;
        parsed.insert(name, value);
    }
    Ok(parsed)
}

async fn read_bounded(response: Response, limit: usize) -> Result<Vec<u8>, McpError> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > limit as u64)
    {
        return Err(McpError::ResponseTooLarge { limit });
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            McpError::Transport(format!("读取 MCP HTTP 响应失败：{}", error.without_url()))
        })?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(McpError::ResponseTooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn lock_unpoisoned<T>(mutex: &StdMutex<T>) -> StdMutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 将无法在当前请求上下文重试的内部认证控制流归一化为安全错误。
fn http_response_error(error: HttpResponseError) -> McpError {
    match error {
        HttpResponseError::Mcp(error) => error,
        HttpResponseError::RetryInitialAuth => {
            McpError::Transport("MCP HTTP 身份认证失败".to_owned())
        }
    }
}

struct SseEvent {
    data: Option<Vec<u8>>,
    id: Option<String>,
    retry: Option<u64>,
}

struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<Vec<u8>>,
    id: Option<String>,
    retry: Option<u64>,
    event_bytes: usize,
    limit: usize,
}

impl SseDecoder {
    fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            data_lines: Vec::new(),
            id: None,
            retry: None,
            event_bytes: 0,
            limit,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, McpError> {
        let mut events = Vec::new();
        let mut start = 0;
        for (index, byte) in chunk.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            self.extend_buffer(&chunk[start..index])?;
            let mut line = std::mem::take(&mut self.buffer);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut events)?;
            start = index + 1;
        }
        self.extend_buffer(&chunk[start..])?;
        Ok(events)
    }

    fn finish(mut self) -> Result<Vec<SseEvent>, McpError> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let mut line = std::mem::take(&mut self.buffer);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut events)?;
        }
        self.finish_event(&mut events);
        Ok(events)
    }

    fn extend_buffer(&mut self, bytes: &[u8]) -> Result<(), McpError> {
        if self.buffer.len().saturating_add(bytes.len()) > self.limit {
            return Err(McpError::ResponseTooLarge { limit: self.limit });
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn process_line(&mut self, line: &[u8], events: &mut Vec<SseEvent>) -> Result<(), McpError> {
        self.event_bytes = self
            .event_bytes
            .checked_add(line.len().saturating_add(1))
            .ok_or(McpError::ResponseTooLarge { limit: self.limit })?;
        if self.event_bytes > self.limit {
            return Err(McpError::ResponseTooLarge { limit: self.limit });
        }
        if line.is_empty() {
            self.finish_event(events);
            return Ok(());
        }
        if line.starts_with(b":") {
            return Ok(());
        }
        let line = std::str::from_utf8(line)
            .map_err(|error| McpError::Protocol(format!("MCP SSE 行不是有效 UTF-8：{error}")))?;
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => self.data_lines.push(value.as_bytes().to_vec()),
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                self.retry = value.parse().ok();
            }
            _ => {}
        }
        Ok(())
    }

    fn finish_event(&mut self, events: &mut Vec<SseEvent>) {
        let data = if self.data_lines.is_empty() {
            None
        } else {
            let capacity = self.data_lines.iter().map(Vec::len).sum::<usize>()
                + self.data_lines.len().saturating_sub(1);
            let mut message = Vec::with_capacity(capacity);
            for (index, line) in self.data_lines.drain(..).enumerate() {
                if index > 0 {
                    message.push(b'\n');
                }
                message.extend_from_slice(&line);
            }
            Some(message)
        };
        let id = self.id.take();
        let retry = self.retry.take();
        if data.is_some() || id.is_some() || retry.is_some() {
            events.push(SseEvent { data, id, retry });
        }
        self.event_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use reqwest::{Client, Url};
    use tokio::sync::broadcast;

    use super::{AuthBinding, HttpRuntime, HttpSessionSnapshot, HttpSessionState, SseDecoder};

    #[test]
    fn sse_decoder_preserves_priming_id_and_ignores_empty_data() {
        let events = SseDecoder::new(128)
            .push(b"id: event-1\nretry: 125\ndata:\n\n")
            .expect("priming 事件应可解析");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("event-1"));
        assert_eq!(events[0].retry, Some(125));
        assert_eq!(events[0].data.as_deref(), Some([].as_slice()));
    }

    #[test]
    fn sse_decoder_limits_each_event_instead_of_whole_connection() {
        let mut decoder = SseDecoder::new(20);
        assert_eq!(
            decoder
                .push(b"data: 123456\n\ndata: abcdef\n\n")
                .expect("两个独立小事件不应按连接累计")
                .len(),
            2
        );
        assert!(decoder.push(b"data: 12345678901234567890").is_err());
    }

    #[test]
    fn stale_get_expiry_cannot_clear_new_session_generation() {
        let (notifications, _) = broadcast::channel(1);
        let runtime = HttpRuntime {
            client: Client::new(),
            endpoint: Url::parse("http://127.0.0.1/mcp").expect("测试 URL 应有效"),
            headers: reqwest::header::HeaderMap::new(),
            auth_provider: None,
            protocol_version: crate::DEFAULT_PROTOCOL_VERSION.to_owned(),
            session: Arc::new(StdMutex::new(HttpSessionState {
                id: Some("new-session".to_owned()),
                generation: 3,
                expired: false,
                auth_binding: AuthBinding::Static,
            })),
            notifications,
            max_response_bytes: 1024,
        };
        runtime.expire_if_current(&HttpSessionSnapshot {
            id: Some("old-session".to_owned()),
            generation: 1,
            expired: false,
            auth_binding: AuthBinding::Static,
        });
        let current = runtime.session_snapshot();
        assert_eq!(current.id.as_deref(), Some("new-session"));
        assert_eq!(current.generation, 3);
        assert!(!current.expired);
    }
}
