//! MCP 初始化状态机与 Provider 中立调用入口。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock as StdRwLock};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::{RwLock as AsyncRwLock, broadcast};
use tokio_util::sync::CancellationToken;

use crate::config::{McpClientOptions, McpServerConfig};
use crate::error::McpError;
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, McpNotification, RequestId};
use crate::transport::{McpTransport, connect_transport};
use crate::types::{
    InitializeResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    McpResource, McpResourceContent, McpResourceTemplate, McpServerSession, McpTool, McpToolSet,
    ReadResourceResult, ToolCallResult,
};

/// 已完成 initialize/initialized 握手的 MCP 客户端。
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<ClientInner>,
}

impl fmt::Debug for McpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpClient")
            .field("closed", &self.inner.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// 连接指定 MCP 服务，完成 initialize 请求与 initialized 通知后返回。
    pub async fn connect(
        config: McpServerConfig,
        options: McpClientOptions,
    ) -> Result<Self, McpError> {
        options.validate()?;
        let transport = connect_transport(config, &options).await?;
        let initialize_id = RequestId::Number(1);
        let initialize_params = json!({
            "protocolVersion": options.protocol_version,
            "capabilities": options.capabilities,
            "clientInfo": options.client_info,
        });
        let initialize_value = match request_with_limits(
            &transport,
            &options,
            initialize_id,
            "initialize",
            initialize_params,
            &CancellationToken::new(),
            false,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                let _ = transport.close().await;
                return Err(error);
            }
        };
        let initialize = match validate_initialize(&options, initialize_value) {
            Ok(initialize) => initialize,
            Err(error) => {
                let _ = transport.close().await;
                return Err(error);
            }
        };
        if let Err(error) = send_initialized(&transport, options.request_timeout).await {
            let _ = transport.close().await;
            return Err(error);
        }

        let client = Self {
            inner: Arc::new(ClientInner {
                transport: Arc::clone(&transport),
                session: StdRwLock::new(initialize.into()),
                options,
                next_request_id: AtomicI64::new(2),
                session_generation: AtomicU64::new(0),
                lifecycle: AsyncRwLock::new(()),
                tool_catalog: StdRwLock::new(None),
                shutdown: CancellationToken::new(),
                closed: AtomicBool::new(false),
            }),
        };
        Ok(client)
    }

    /// 返回 initialize 握手得到的不可变服务端会话信息。
    pub fn session(&self) -> McpServerSession {
        self.inner
            .session
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 订阅服务端异步通知；接收速度过慢时广播通道会报告 lag。
    pub async fn subscribe_notifications(
        &self,
    ) -> Result<broadcast::Receiver<McpNotification>, McpError> {
        self.ensure_open()?;
        let receiver = self.inner.transport.subscribe();
        self.inner.transport.start_listening().await?;
        Ok(receiver)
    }

    /// 读取服务端公布的全部工具页。
    pub async fn list_tools(&self) -> Result<McpToolSet, McpError> {
        self.list_tools_with_cancellation(&CancellationToken::new())
            .await
    }

    /// 使用取消令牌读取服务端公布的全部工具页。
    pub async fn list_tools_with_cancellation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<McpToolSet, McpError> {
        self.ensure_open()?;
        if self.session().capabilities.tools.is_none() {
            return Err(McpError::NotReady("服务端没有声明 tools 能力".to_owned()));
        }
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        let mut total_bytes = 0;
        let mut total_cursor_bytes = 0;
        for _ in 0..self.inner.options.max_pages {
            let params = cursor_params(cursor.as_deref());
            let page: ListToolsResult = self
                .request_typed("tools/list", params, cancellation)
                .await?;
            extend_bounded(
                "tools/list",
                &mut tools,
                page.tools,
                &mut total_bytes,
                &self.inner.options,
            )?;
            match next_cursor(
                "tools/list",
                page.next_cursor,
                &mut seen,
                &mut total_cursor_bytes,
                &self.inner.options,
            )? {
                Some(next) => cursor = Some(next),
                None => {
                    let tools = cache_and_filter_tools(&self.inner.tool_catalog, tools)?;
                    return Ok(McpToolSet::new(tools));
                }
            }
        }
        Err(max_pages_error("tools/list", self.inner.options.max_pages))
    }

    /// 调用指定 MCP 工具；arguments 必须是 JSON 对象。
    pub async fn call_tool(
        &self,
        name: impl Into<String>,
        arguments: Value,
    ) -> Result<ToolCallResult, McpError> {
        self.call_tool_with_cancellation(name, arguments, &CancellationToken::new())
            .await
    }

    /// 使用取消令牌调用指定 MCP 工具。
    pub async fn call_tool_with_cancellation(
        &self,
        name: impl Into<String>,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> Result<ToolCallResult, McpError> {
        self.ensure_open()?;
        if self.session().capabilities.tools.is_none() {
            return Err(McpError::NotReady("服务端没有声明 tools 能力".to_owned()));
        }
        let name = name.into();
        if name.trim().is_empty() {
            return Err(McpError::Configuration("工具名称不得为空".to_owned()));
        }
        if !arguments.is_object() {
            return Err(McpError::Configuration(
                "工具 arguments 必须是 JSON 对象".to_owned(),
            ));
        }
        let catalog_missing = self
            .inner
            .tool_catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none();
        let list_may_change = self
            .session()
            .capabilities
            .tools
            .as_ref()
            .is_some_and(|capabilities| capabilities.list_changed);
        if catalog_missing || list_may_change {
            self.list_tools_with_cancellation(cancellation).await?;
        }
        let task_requirement = self
            .inner
            .tool_catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|catalog| catalog.get(&name).copied());
        match task_requirement {
            Some(true) => {
                return Err(McpError::NotReady(
                    "该工具要求 MCP Tasks 协议，当前客户端拒绝普通 tools/call".to_owned(),
                ));
            }
            Some(false) => {}
            None => {
                return Err(McpError::NotReady(
                    "工具不在服务端当前公布的工具列表中".to_owned(),
                ));
            }
        }
        self.request_typed(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
            cancellation,
        )
        .await
    }

    /// 读取服务端公布的全部具体资源页。
    pub async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        self.list_resources_with_cancellation(&CancellationToken::new())
            .await
    }

    /// 使用取消令牌读取服务端公布的全部具体资源页。
    pub async fn list_resources_with_cancellation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<McpResource>, McpError> {
        self.ensure_resources_capability()?;
        let mut resources = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        let mut total_bytes = 0;
        let mut total_cursor_bytes = 0;
        for _ in 0..self.inner.options.max_pages {
            let page: ListResourcesResult = self
                .request_typed(
                    "resources/list",
                    cursor_params(cursor.as_deref()),
                    cancellation,
                )
                .await?;
            extend_bounded(
                "resources/list",
                &mut resources,
                page.resources,
                &mut total_bytes,
                &self.inner.options,
            )?;
            match next_cursor(
                "resources/list",
                page.next_cursor,
                &mut seen,
                &mut total_cursor_bytes,
                &self.inner.options,
            )? {
                Some(next) => cursor = Some(next),
                None => return Ok(resources),
            }
        }
        Err(max_pages_error(
            "resources/list",
            self.inner.options.max_pages,
        ))
    }

    /// 读取服务端公布的全部参数化资源模板页。
    pub async fn list_resource_templates(&self) -> Result<Vec<McpResourceTemplate>, McpError> {
        self.list_resource_templates_with_cancellation(&CancellationToken::new())
            .await
    }

    /// 使用取消令牌读取服务端公布的全部参数化资源模板页。
    pub async fn list_resource_templates_with_cancellation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<McpResourceTemplate>, McpError> {
        self.ensure_resources_capability()?;
        let mut templates = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        let mut total_bytes = 0;
        let mut total_cursor_bytes = 0;
        for _ in 0..self.inner.options.max_pages {
            let page: ListResourceTemplatesResult = self
                .request_typed(
                    "resources/templates/list",
                    cursor_params(cursor.as_deref()),
                    cancellation,
                )
                .await?;
            extend_bounded(
                "resources/templates/list",
                &mut templates,
                page.resource_templates,
                &mut total_bytes,
                &self.inner.options,
            )?;
            match next_cursor(
                "resources/templates/list",
                page.next_cursor,
                &mut seen,
                &mut total_cursor_bytes,
                &self.inner.options,
            )? {
                Some(next) => cursor = Some(next),
                None => return Ok(templates),
            }
        }
        Err(max_pages_error(
            "resources/templates/list",
            self.inner.options.max_pages,
        ))
    }

    /// 读取指定 URI 的 MCP 资源内容。
    pub async fn read_resource(
        &self,
        uri: impl Into<String>,
    ) -> Result<Vec<McpResourceContent>, McpError> {
        self.read_resource_with_cancellation(uri, &CancellationToken::new())
            .await
    }

    /// 使用取消令牌读取指定 URI 的 MCP 资源内容。
    pub async fn read_resource_with_cancellation(
        &self,
        uri: impl Into<String>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<McpResourceContent>, McpError> {
        self.ensure_resources_capability()?;
        let uri = uri.into();
        if uri.trim().is_empty() {
            return Err(McpError::Configuration("资源 URI 不得为空".to_owned()));
        }
        let result: ReadResourceResult = self
            .request_typed("resources/read", json!({ "uri": uri }), cancellation)
            .await?;
        Ok(result.contents)
    }

    /// 关闭传输并回收 stdio 子进程或 Streamable HTTP 会话。
    pub async fn close(&self) -> Result<(), McpError> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner.shutdown.cancel();
        let timeout = self.inner.options.shutdown_timeout;
        let close = async {
            let _lifecycle = self.inner.lifecycle.write().await;
            self.inner.transport.close().await
        };
        match tokio::time::timeout(timeout, close).await {
            Ok(result) => result,
            Err(_) => {
                self.inner.transport.force_close();
                Err(McpError::Timeout {
                    method: "MCP client close".to_owned(),
                    duration: timeout,
                })
            }
        }
    }

    async fn request_typed<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
    ) -> Result<T, McpError> {
        self.ensure_open()?;
        if cancellation.is_cancelled() {
            return Err(McpError::Cancelled {
                method: method.to_owned(),
                reason: None,
            });
        }
        let id = self.next_request_id()?;
        let (first, generation) = {
            let _lifecycle = self.inner.lifecycle.read().await;
            let generation = self.inner.session_generation.load(Ordering::Acquire);
            (
                self.request_while_running(id, method, params.clone(), cancellation, true)
                    .await,
                generation,
            )
        };
        let value = match first {
            Err(McpError::SessionExpired) => {
                let _lifecycle = self.inner.lifecycle.write().await;
                if self.inner.session_generation.load(Ordering::Acquire) == generation {
                    self.reinitialize().await?;
                }
                if !can_replay_after_session_expired(method) {
                    return Err(McpError::SessionExpired);
                }
                let retry_id = self.next_request_id()?;
                self.request_while_running(retry_id, method, params, cancellation, true)
                    .await?
            }
            other => other?,
        };
        deserialize_result(method, value)
    }

    async fn reinitialize(&self) -> Result<(), McpError> {
        let id = self.next_request_id()?;
        let params = json!({
            "protocolVersion": self.inner.options.protocol_version,
            "capabilities": self.inner.options.capabilities,
            "clientInfo": self.inner.options.client_info,
        });
        let value = self
            .request_while_running(id, "initialize", params, &CancellationToken::new(), false)
            .await?;
        let initialize = validate_initialize(&self.inner.options, value)?;
        tokio::select! {
            result = send_initialized(
                &self.inner.transport,
                self.inner.options.request_timeout,
            ) => result?,
            _ = self.inner.shutdown.cancelled() => {
                return Err(McpError::NotReady("MCP 客户端正在关闭".to_owned()));
            }
        }
        *self
            .inner
            .session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = initialize.into();
        *self
            .inner
            .tool_catalog
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.inner.session_generation.fetch_add(1, Ordering::AcqRel);
        self.inner.transport.restart_listening().await
    }

    async fn request_while_running(
        &self,
        id: RequestId,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
        allow_cancellation: bool,
    ) -> Result<Value, McpError> {
        tokio::select! {
            result = request_with_limits(
                &self.inner.transport,
                &self.inner.options,
                id,
                method,
                params,
                cancellation,
                allow_cancellation,
            ) => result,
            _ = self.inner.shutdown.cancelled() => {
                Err(McpError::NotReady("MCP 客户端正在关闭".to_owned()))
            }
        }
    }

    fn next_request_id(&self) -> Result<RequestId, McpError> {
        self.inner
            .next_request_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map(RequestId::Number)
            .map_err(|_| McpError::Protocol("JSON-RPC 请求 ID 已耗尽".to_owned()))
    }

    fn ensure_open(&self) -> Result<(), McpError> {
        if self.inner.closed.load(Ordering::Acquire) {
            Err(McpError::NotReady("MCP 客户端已经关闭".to_owned()))
        } else {
            Ok(())
        }
    }

    fn ensure_resources_capability(&self) -> Result<(), McpError> {
        self.ensure_open()?;
        if self.session().capabilities.resources.is_none() {
            Err(McpError::NotReady(
                "服务端没有声明 resources 能力".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

/// 判断会话失效后是否可以安全重发请求；有副作用的工具调用绝不自动重放。
fn can_replay_after_session_expired(method: &str) -> bool {
    matches!(
        method,
        "tools/list" | "resources/list" | "resources/templates/list" | "resources/read"
    )
}

struct ClientInner {
    transport: Arc<dyn McpTransport>,
    session: StdRwLock<McpServerSession>,
    options: McpClientOptions,
    next_request_id: AtomicI64,
    session_generation: AtomicU64,
    lifecycle: AsyncRwLock<()>,
    tool_catalog: StdRwLock<Option<HashMap<String, bool>>>,
    shutdown: CancellationToken,
    closed: AtomicBool,
}

async fn request_with_limits(
    transport: &Arc<dyn McpTransport>,
    options: &McpClientOptions,
    id: RequestId,
    method: &str,
    params: Value,
    cancellation: &CancellationToken,
    allow_cancellation: bool,
) -> Result<Value, McpError> {
    let request = JsonRpcRequest::new(id.clone(), method, params);
    let request_future = transport.request(request);
    tokio::pin!(request_future);
    let timeout = tokio::time::sleep(options.request_timeout);
    tokio::pin!(timeout);
    tokio::select! {
        result = &mut request_future => result,
        _ = cancellation.cancelled(), if allow_cancellation => {
            send_cancellation(transport, &id, Some("调用方取消请求")).await;
            Err(McpError::Cancelled {
                method: method.to_owned(),
                reason: Some("调用方取消请求".to_owned()),
            })
        }
        _ = &mut timeout => {
            if allow_cancellation {
                send_cancellation(transport, &id, Some("请求超时")).await;
            }
            Err(McpError::Timeout {
                method: method.to_owned(),
                duration: options.request_timeout,
            })
        }
    }
}

fn validate_initialize(
    options: &McpClientOptions,
    value: Value,
) -> Result<InitializeResult, McpError> {
    let initialize: InitializeResult = deserialize_result("initialize", value)?;
    if initialize.protocol_version != options.protocol_version {
        return Err(McpError::Protocol(format!(
            "MCP 协议版本协商失败：客户端仅支持 {:?}，服务端选择 {:?}",
            options.protocol_version, initialize.protocol_version
        )));
    }
    if initialize.server_info.name.trim().is_empty()
        || initialize.server_info.version.trim().is_empty()
    {
        return Err(McpError::Protocol(
            "initialize 响应的 serverInfo.name 和 version 不得为空".to_owned(),
        ));
    }
    Ok(initialize)
}

async fn send_cancellation(
    transport: &Arc<dyn McpTransport>,
    id: &RequestId,
    reason: Option<&str>,
) {
    let notification = JsonRpcNotification::new(
        "notifications/cancelled",
        Some(json!({ "requestId": id, "reason": reason })),
    );
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        transport.notify(notification),
    )
    .await;
}

async fn send_initialized(
    transport: &Arc<dyn McpTransport>,
    timeout: std::time::Duration,
) -> Result<(), McpError> {
    tokio::time::timeout(
        timeout,
        transport.notify(JsonRpcNotification::new(
            "notifications/initialized",
            Some(json!({})),
        )),
    )
    .await
    .map_err(|_| McpError::Timeout {
        method: "notifications/initialized".to_owned(),
        duration: timeout,
    })?
}

fn deserialize_result<T: DeserializeOwned>(method: &str, value: Value) -> Result<T, McpError> {
    serde_json::from_value(value)
        .map_err(|error| McpError::Protocol(format!("{method} 响应结构无效：{error}")))
}

fn cursor_params(cursor: Option<&str>) -> Value {
    cursor.map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }))
}

fn next_cursor(
    method: &str,
    next_cursor: Option<String>,
    seen: &mut HashSet<String>,
    total_cursor_bytes: &mut usize,
    options: &McpClientOptions,
) -> Result<Option<String>, McpError> {
    match next_cursor {
        Some(cursor) => {
            if cursor.len() > options.max_cursor_bytes {
                return Err(McpError::Pagination {
                    method: method.to_owned(),
                    message: format!("单个游标超过 {} 字节上限", options.max_cursor_bytes),
                });
            }
            let next_total = total_cursor_bytes
                .checked_add(cursor.len())
                .ok_or_else(|| McpError::Pagination {
                    method: method.to_owned(),
                    message: "累计游标字节数溢出".to_owned(),
                })?;
            if next_total > options.max_total_cursor_bytes {
                return Err(McpError::Pagination {
                    method: method.to_owned(),
                    message: format!("累计游标超过 {} 字节上限", options.max_total_cursor_bytes),
                });
            }
            if !seen.insert(cursor.clone()) {
                return Err(McpError::Pagination {
                    method: method.to_owned(),
                    message: "服务端重复返回游标".to_owned(),
                });
            }
            *total_cursor_bytes = next_total;
            Ok(Some(cursor))
        }
        None => Ok(None),
    }
}

fn cache_and_filter_tools(
    cache: &StdRwLock<Option<HashMap<String, bool>>>,
    tools: Vec<McpTool>,
) -> Result<Vec<McpTool>, McpError> {
    let mut catalog = HashMap::with_capacity(tools.len());
    for tool in &tools {
        if catalog
            .insert(tool.name.clone(), tool.requires_task())
            .is_some()
        {
            return Err(McpError::Protocol(
                "tools/list 返回了重复工具名称".to_owned(),
            ));
        }
    }
    *cache
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(catalog);
    Ok(tools
        .into_iter()
        .filter(|tool| !tool.requires_task())
        .collect())
}

fn max_pages_error(method: &str, max_pages: usize) -> McpError {
    McpError::Pagination {
        method: method.to_owned(),
        message: format!("超过最大页数 {max_pages}"),
    }
}

fn extend_bounded<T: Serialize>(
    method: &str,
    destination: &mut Vec<T>,
    page: Vec<T>,
    total_bytes: &mut usize,
    options: &McpClientOptions,
) -> Result<(), McpError> {
    let next_items =
        destination
            .len()
            .checked_add(page.len())
            .ok_or_else(|| McpError::Pagination {
                method: method.to_owned(),
                message: "累计条目数溢出".to_owned(),
            })?;
    if next_items > options.max_total_items {
        return Err(McpError::Pagination {
            method: method.to_owned(),
            message: format!("累计条目数超过上限 {}", options.max_total_items),
        });
    }
    let page_bytes = serde_json::to_vec(&page)
        .map_err(|error| McpError::Protocol(format!("{method} 分页结果无法计量：{error}")))?
        .len();
    *total_bytes = total_bytes
        .checked_add(page_bytes)
        .ok_or_else(|| McpError::Pagination {
            method: method.to_owned(),
            message: "累计结果字节数溢出".to_owned(),
        })?;
    if *total_bytes > options.max_total_result_bytes {
        return Err(McpError::Pagination {
            method: method.to_owned(),
            message: format!(
                "累计序列化结果超过 {} 字节上限",
                options.max_total_result_bytes
            ),
        });
    }
    destination.extend(page);
    Ok(())
}
