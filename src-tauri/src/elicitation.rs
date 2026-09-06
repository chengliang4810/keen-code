//! 桌面 Runtime 的标准 ACP 结构化问答桥。

use crate::agent_runtime::ClientRequestRouter;
use crate::client_request::{
    ClientRequestDisplayGate, ClientRequestDisplayPermit, ClientRequestSink,
};
use keencode_acp::schema::{
    ClientCapabilities, CreateElicitationRequest, CreateElicitationResponse, ElicitationAction,
    ElicitationContentValue, ElicitationFormMode, ElicitationSchema, ElicitationSessionScope,
    EnumOption, Meta, MultiSelectPropertySchema, RequestId, StringPropertySchema,
};
use keencode_acp::{
    AcpClientRequestEncoder, AcpClientRequestFrame, AcpResponseDecoder, ElicitationRouter,
};
use keencode_tools::{
    UserQuestion, UserQuestionAnswer, UserQuestionError, UserQuestionFuture, UserQuestionHandler,
    UserQuestionRequest, UserQuestionResponse,
};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

/// 同一进程中为 Elicitation JSON-RPC 标识分配的不复用序号。
static NEXT_ELICITATION_REQUEST_ID: AtomicU64 = AtomicU64::new(0);

/// 标准 ACP `_meta` 中承载 KeenCode 交互扩展的唯一命名空间。
const KEENCODE_META_KEY: &str = "_keencode";

/// Elicitation 注册、投递或响应违反桌面桥不变量时返回的稳定错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElicitationBridgeError {
    /// Client 尚未协商能力，或重复握手试图改变已固定的能力。
    CapabilitiesUnavailable,
    /// Runtime 已关闭，不再接受新问答。
    RuntimeClosed,
    /// Handler 收到了其他 Session 的请求。
    SessionMismatch,
    /// 同一 Session 已经存在一个未结束的可见问答。
    SessionBusy,
    /// 请求标识序号已经耗尽。
    RequestIdExhausted,
    /// 标准 ACP 请求无法构造或越过资源边界。
    RegistrationRejected,
    /// 当前 Session 的 Client Request 无法送达桌面。
    DeliveryUnavailable,
    /// Client 返回的完整 JSON-RPC 响应不符合 Elicitation DTO。
    InvalidResponse,
    /// 响应携带的请求标识不存在或已经结束。
    UnknownRequest,
    /// 响应到达时请求尚未被投递泵确认送达。
    RequestNotDelivered,
    /// 用户拒绝或取消了本次问答。
    Cancelled,
    /// 内部等待通道或状态不变量被破坏。
    InternalState,
}

impl std::fmt::Display for ElicitationBridgeError {
    /// 输出不包含问题正文、选项或回答的安全错误说明。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapabilitiesUnavailable => {
                formatter.write_str("ACP 问答能力尚未协商或与既有连接不一致")
            }
            Self::RuntimeClosed => formatter.write_str("问答 Runtime 已关闭"),
            Self::SessionMismatch => formatter.write_str("问答请求与当前 Session 不匹配"),
            Self::SessionBusy => formatter.write_str("当前 Session 已有待回答问题"),
            Self::RequestIdExhausted => formatter.write_str("问答请求标识已经耗尽"),
            Self::RegistrationRejected => formatter.write_str("问答请求无法安全登记"),
            Self::DeliveryUnavailable => formatter.write_str("问答请求无法送达桌面"),
            Self::InvalidResponse => formatter.write_str("ACP 问答响应无效"),
            Self::UnknownRequest => formatter.write_str("ACP 待决问答不存在或已经结束"),
            Self::RequestNotDelivered => formatter.write_str("ACP 待决问答尚未送达桌面"),
            Self::Cancelled => formatter.write_str("用户取消了本次问答"),
            Self::InternalState => formatter.write_str("问答 Runtime 内部状态不一致"),
        }
    }
}

impl std::error::Error for ElicitationBridgeError {}

impl ElicitationBridgeError {
    /// 转成不会泄露问答内容的工具端错误。
    fn into_user_question_error(self) -> UserQuestionError {
        UserQuestionError::new(self.to_string())
    }
}

/// 一个待决问答从登记到桌面确认送达之间的阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElicitationDeliveryStage {
    /// 请求已经登记，正在等待共享展示许可。
    Dispatching,
    /// 请求已经向桌面投递泵提交，正在等待同步投递回执。
    Emitting,
    /// 串行投递泵已经确认请求送达桌面。
    Delivered,
}

/// 单个待决标准 Elicitation 的不可变绑定和一次性等待者。
struct PendingElicitation {
    /// 请求绑定的唯一 Session。
    session_id: String,
    /// 用于把结构化响应还原为工具答案的原始问题定义。
    questions: Vec<UserQuestion>,
    /// 当前投递阶段。
    delivery_stage: ElicitationDeliveryStage,
    /// 投递未确认时可取消的异步任务句柄。
    dispatch_abort: Option<AbortHandle>,
    /// 从实际投递前一直持有到响应或取消收口的跨路由展示许可。
    display_permit: Option<ClientRequestDisplayPermit>,
    /// AskUser 工具等待的一次性结果通道。
    waiter: oneshot::Sender<Result<UserQuestionResponse, UserQuestionError>>,
}

/// Elicitation 协调器中不随克隆复制的共享状态。
struct ElicitationCoordinatorInner {
    /// 保护待决映射、Session 占用和关闭状态的短临界区。
    state: Mutex<ElicitationState>,
}

/// 一个 Runtime 内全部待决结构化问答。
struct ElicitationState {
    /// 按字符串 JSON-RPC 标识保存的待决请求。
    pending: HashMap<String, PendingElicitation>,
    /// 每个 Session 当前唯一待决请求，防止前端被并发弹窗覆盖。
    pending_by_session: HashMap<String, String>,
    /// Runtime 是否已经永久停止接受问答。
    closed: bool,
}

/// 跨 Session 共享、只在当前进程保存待决问答的协调器。
#[derive(Clone)]
pub struct ElicitationCoordinator {
    /// 唯一共享的待决状态。
    inner: Arc<ElicitationCoordinatorInner>,
    /// 标准 ACP Client Request 编码器。
    request_encoder: AcpClientRequestEncoder,
    /// 标准 ACP Client Response 严格解码器。
    response_decoder: AcpResponseDecoder,
    /// initialize 协商后固定的共享能力快照；握手前不发送问答。
    router: Arc<OnceLock<ElicitationRouter>>,
    /// 持有到响应终态的每 Session 展示串行门。
    client_request_gate: Arc<ClientRequestDisplayGate>,
}

impl ElicitationCoordinator {
    /// 创建不恢复旧问答、等待 Client 协商能力的协调器。
    pub fn new() -> Self {
        Self::with_gate(Arc::new(ClientRequestDisplayGate::new()))
    }

    /// 使用 Runtime 共享展示门创建尚未协商能力的协调器。
    pub(crate) fn with_gate(client_request_gate: Arc<ClientRequestDisplayGate>) -> Self {
        Self {
            inner: Arc::new(ElicitationCoordinatorInner {
                state: Mutex::new(ElicitationState {
                    pending: HashMap::new(),
                    pending_by_session: HashMap::new(),
                    closed: false,
                }),
            }),
            request_encoder: AcpClientRequestEncoder::new(),
            response_decoder: AcpResponseDecoder::new(),
            router: Arc::new(OnceLock::new()),
            client_request_gate,
        }
    }

    /// 首次握手固定实际能力；同能力刷新幂等，改变能力必须建立新连接。
    pub fn negotiate_client_capabilities(
        &self,
        capabilities: &ClientCapabilities,
    ) -> Result<(), ElicitationBridgeError> {
        let negotiated = ElicitationRouter::from_client_capabilities(capabilities);
        let current = self.router.get_or_init(|| negotiated);
        if current == &negotiated {
            Ok(())
        } else {
            Err(ElicitationBridgeError::CapabilitiesUnavailable)
        }
    }

    /// 仅在 Client 明确协商支持表单时向 Agent 暴露 AskUser 工具。
    pub fn supports_form(&self) -> bool {
        self.router
            .get()
            .is_some_and(ElicitationRouter::supports_form)
    }

    /// 为一个已经建立投递泵的 Session 创建 AskUser Handler。
    pub fn handler(
        self: &Arc<Self>,
        session_id: keencode_agent::SessionId,
        sink: Arc<dyn ClientRequestSink>,
    ) -> DesktopQuestionHandler {
        DesktopQuestionHandler {
            session_id,
            coordinator: Arc::clone(self),
            sink,
        }
    }

    /// 返回当前进程尚未收口的问答数量。
    pub fn pending_len(&self) -> usize {
        self.inner.state.lock().pending.len()
    }

    /// 判断字符串 JSON-RPC 标识是否属于一个待决问答。
    pub fn contains_pending(&self, request_id: &str) -> bool {
        self.inner.state.lock().pending.contains_key(request_id)
    }

    /// 严格解析并 exactly-once 收口一个完整 ACP Elicitation 响应。
    pub fn respond(&self, response_json: &str) -> Result<(), ElicitationBridgeError> {
        if response_json.len() > self.response_decoder.limits().max_payload_bytes() {
            return Err(ElicitationBridgeError::InvalidResponse);
        }
        let routed_request_id = response_request_id(response_json)?;
        if !self.contains_pending(&routed_request_id) {
            return Err(ElicitationBridgeError::UnknownRequest);
        }
        let decoded = match self
            .response_decoder
            .decode_result::<CreateElicitationResponse>(response_json.as_bytes())
        {
            Ok(decoded) => decoded,
            Err(_) => {
                self.cancel_request(&routed_request_id, ElicitationBridgeError::InvalidResponse);
                return Err(ElicitationBridgeError::InvalidResponse);
            }
        };
        let (response_id, response) = decoded.into_parts();
        let RequestId::Str(request_id) = response_id else {
            self.cancel_request(&routed_request_id, ElicitationBridgeError::InvalidResponse);
            return Err(ElicitationBridgeError::InvalidResponse);
        };
        if request_id != routed_request_id {
            self.cancel_request(&routed_request_id, ElicitationBridgeError::InvalidResponse);
            return Err(ElicitationBridgeError::InvalidResponse);
        }

        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get(&request_id) else {
            return Err(ElicitationBridgeError::UnknownRequest);
        };
        if !matches!(
            pending.delivery_stage,
            ElicitationDeliveryStage::Emitting | ElicitationDeliveryStage::Delivered
        ) {
            return Err(ElicitationBridgeError::RequestNotDelivered);
        }
        let result = response_to_answers(&pending.questions, response);
        let Some(pending) = state.pending.remove(&request_id) else {
            return Err(ElicitationBridgeError::InternalState);
        };
        state.pending_by_session.remove(&pending.session_id);
        drop(state);
        match result {
            Ok(response) => pending
                .waiter
                .send(Ok(response))
                .map_err(|_| ElicitationBridgeError::InternalState),
            Err(error) => {
                pending
                    .waiter
                    .send(Err(error.into_user_question_error()))
                    .map_err(|_| ElicitationBridgeError::InternalState)?;
                if error == ElicitationBridgeError::Cancelled {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    /// 关闭 Runtime，取消全部内存问答且不接受迟到响应。
    pub fn shutdown(&self) {
        let mut state = self.inner.state.lock();
        if state.closed && state.pending.is_empty() {
            return;
        }
        state.closed = true;
        let pending = state
            .pending
            .drain()
            .map(|(_, pending)| pending)
            .collect::<Vec<_>>();
        state.pending_by_session.clear();
        drop(state);
        for mut pending in pending {
            if let Some(abort) = pending.dispatch_abort.take() {
                abort.abort();
            }
            let _ = pending.waiter.send(Err(
                ElicitationBridgeError::RuntimeClosed.into_user_question_error()
            ));
        }
    }

    /// 同步登记一次问答并启动当前 Session 的唯一异步投递。
    fn register(
        &self,
        handler_session_id: &keencode_agent::SessionId,
        request: UserQuestionRequest,
        sink: Arc<dyn ClientRequestSink>,
    ) -> Result<RegisteredElicitation, ElicitationBridgeError> {
        if &request.session_id != handler_session_id {
            return Err(ElicitationBridgeError::SessionMismatch);
        }
        let request_id = next_request_id()?;
        let frame = self
            .request_encoder
            .elicitation_request_frame(
                RequestId::Str(request_id.clone()),
                self.router
                    .get()
                    .ok_or(ElicitationBridgeError::CapabilitiesUnavailable)?,
                create_request(&request),
            )
            .map_err(|_| ElicitationBridgeError::RegistrationRejected)?;
        let (waiter, receiver) = oneshot::channel();
        let session_id = request.session_id.as_str().to_owned();
        {
            let mut state = self.inner.state.lock();
            if state.closed {
                return Err(ElicitationBridgeError::RuntimeClosed);
            }
            if state.pending_by_session.contains_key(&session_id) {
                return Err(ElicitationBridgeError::SessionBusy);
            }
            state
                .pending_by_session
                .insert(session_id.clone(), request_id.clone());
            state.pending.insert(
                request_id.clone(),
                PendingElicitation {
                    session_id: session_id.clone(),
                    questions: request.questions,
                    delivery_stage: ElicitationDeliveryStage::Dispatching,
                    dispatch_abort: None,
                    display_permit: None,
                    waiter,
                },
            );
        }
        self.schedule_dispatch(request_id.clone(), session_id, sink, frame)?;
        Ok(RegisteredElicitation {
            guard: PendingElicitationGuard {
                coordinator: self.clone(),
                request_id,
                armed: true,
            },
            receiver,
        })
    }

    /// 创建投递任务，并在请求仍处于 Dispatching 时才允许其开始。
    fn schedule_dispatch(
        &self,
        request_id: String,
        session_id: String,
        sink: Arc<dyn ClientRequestSink>,
        frame: AcpClientRequestFrame,
    ) -> Result<(), ElicitationBridgeError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| ElicitationBridgeError::DeliveryUnavailable)?;
        let coordinator = self.clone();
        let client_request_gate = Arc::clone(&self.client_request_gate);
        let task_request_id = request_id.clone();
        let (start, started) = oneshot::channel();
        let task = runtime.spawn(async move {
            if started.await.is_err() {
                return;
            }
            let permit = match client_request_gate.acquire(&session_id).await {
                Some(permit) => permit,
                None => {
                    coordinator.finish_delivery(
                        &task_request_id,
                        Err(ElicitationBridgeError::InternalState),
                    );
                    return;
                }
            };
            if !coordinator.begin_emit_with_display_permit(&task_request_id, permit) {
                return;
            }
            let result = sink
                .send_client_request(frame)
                .await
                .map_err(|_| ElicitationBridgeError::DeliveryUnavailable);
            coordinator.finish_delivery(&task_request_id, result);
        });
        let abort = task.abort_handle();
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get_mut(&request_id) else {
            abort.abort();
            return Err(ElicitationBridgeError::InternalState);
        };
        if pending.delivery_stage != ElicitationDeliveryStage::Dispatching {
            abort.abort();
            return Err(ElicitationBridgeError::InternalState);
        }
        pending.dispatch_abort = Some(abort);
        drop(state);
        let _ = start.send(());
        Ok(())
    }

    /// 保存跨路由许可并在调用投递泵前原子进入可响应的发送阶段。
    fn begin_emit_with_display_permit(
        &self,
        request_id: &str,
        permit: ClientRequestDisplayPermit,
    ) -> bool {
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get_mut(request_id) else {
            return false;
        };
        if pending.delivery_stage != ElicitationDeliveryStage::Dispatching
            || pending.display_permit.is_some()
        {
            return false;
        }
        pending.display_permit = Some(permit);
        pending.delivery_stage = ElicitationDeliveryStage::Emitting;
        true
    }

    /// 记录桌面投递结果；失败时移除占用并唤醒 AskUser Future。
    fn finish_delivery(&self, request_id: &str, result: Result<(), ElicitationBridgeError>) {
        let mut state = self.inner.state.lock();
        let Some(pending) = state.pending.get_mut(request_id) else {
            return;
        };
        if pending.delivery_stage != ElicitationDeliveryStage::Emitting {
            return;
        }
        pending.dispatch_abort = None;
        if result.is_ok() {
            pending.delivery_stage = ElicitationDeliveryStage::Delivered;
            return;
        }
        let Some(pending) = state.pending.remove(request_id) else {
            return;
        };
        state.pending_by_session.remove(&pending.session_id);
        drop(state);
        let _ = pending.waiter.send(Err(
            ElicitationBridgeError::DeliveryUnavailable.into_user_question_error()
        ));
    }

    /// Future 被取消或丢弃时 exactly-once 清理待决问答。
    fn cancel_request(&self, request_id: &str, error: ElicitationBridgeError) {
        let mut state = self.inner.state.lock();
        let Some(mut pending) = state.pending.remove(request_id) else {
            return;
        };
        state.pending_by_session.remove(&pending.session_id);
        drop(state);
        if let Some(abort) = pending.dispatch_abort.take() {
            abort.abort();
        }
        let _ = pending.waiter.send(Err(error.into_user_question_error()));
    }
}

impl Default for ElicitationCoordinator {
    /// 创建默认的空结构化问答协调器。
    fn default() -> Self {
        Self::new()
    }
}

impl ClientRequestRouter for ElicitationCoordinator {
    /// 让 AgentRuntime 只把本协调器登记的请求交给 Elicitation DTO。
    fn contains_pending(&self, request_id: &str) -> bool {
        ElicitationCoordinator::contains_pending(self, request_id)
    }

    /// 使用严格 ACP Response 解码器处理完整响应。
    fn respond(&self, response_json: &str) -> Result<(), String> {
        ElicitationCoordinator::respond(self, response_json).map_err(|error| error.to_string())
    }
}

/// 绑定一个 Session 和其唯一投递泵的 AskUser 实现。
pub struct DesktopQuestionHandler {
    /// 该 Handler 唯一允许接收的 Session。
    session_id: keencode_agent::SessionId,
    /// 进程内共享的问答协调器。
    coordinator: Arc<ElicitationCoordinator>,
    /// 当前 Session 的 ACP 投递泵。
    sink: Arc<dyn ClientRequestSink>,
}

impl UserQuestionHandler for DesktopQuestionHandler {
    /// 同步登记问题并等待一次严格 Client 响应。
    fn ask(&self, request: UserQuestionRequest) -> UserQuestionFuture<'_> {
        let registration =
            self.coordinator
                .register(&self.session_id, request, Arc::clone(&self.sink));
        Box::pin(async move {
            let RegisteredElicitation {
                mut guard,
                receiver,
            } = registration.map_err(ElicitationBridgeError::into_user_question_error)?;
            let result = receiver.await.unwrap_or_else(|_| {
                Err(ElicitationBridgeError::InternalState.into_user_question_error())
            });
            guard.armed = false;
            result
        })
    }
}

/// Handler 登记成功后持有的取消守卫和一次性接收端。
struct RegisteredElicitation {
    /// Future 被丢弃时负责清理待决映射的守卫。
    guard: PendingElicitationGuard,
    /// 等待桌面回答的一次性接收端。
    receiver: oneshot::Receiver<Result<UserQuestionResponse, UserQuestionError>>,
}

/// AskUser Future 未正常完成时执行 exactly-once 清理。
struct PendingElicitationGuard {
    /// 负责移除待决请求的共享协调器。
    coordinator: ElicitationCoordinator,
    /// 当前 Future 对应的 JSON-RPC 标识。
    request_id: String,
    /// 正常完成后关闭自动清理。
    armed: bool,
}

impl Drop for PendingElicitationGuard {
    /// 取消尚未完成的桌面问答并拒绝迟到响应。
    fn drop(&mut self) {
        if self.armed {
            self.coordinator
                .cancel_request(&self.request_id, ElicitationBridgeError::Cancelled);
        }
    }
}

/// 为当前进程分配一个非零、带类型前缀的字符串请求标识。
fn next_request_id() -> Result<String, ElicitationBridgeError> {
    NEXT_ELICITATION_REQUEST_ID
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map(|previous| format!("elicitation-{}", previous + 1))
        .map_err(|_| ElicitationBridgeError::RequestIdExhausted)
}

/// 把 Provider 中立问题集合转换为标准 ACP form schema。
fn create_request(request: &UserQuestionRequest) -> CreateElicitationRequest {
    let mut schema = ElicitationSchema::new().title("KeenCode AskUser");
    let mut allow_custom_by_question = serde_json::Map::new();
    let question_order = request
        .questions
        .iter()
        .map(|question| question.id.clone())
        .collect::<Vec<_>>();
    for question in &request.questions {
        allow_custom_by_question.insert(question.id.clone(), Value::Bool(question.allow_custom));
        let options = question
            .options
            .iter()
            .map(|option| EnumOption::new(option.label.clone(), option.label.clone()))
            .collect::<Vec<_>>();
        let description = question_description(question);
        if question.multi_select {
            schema = schema.property(
                question.id.clone(),
                MultiSelectPropertySchema::titled(options)
                    .description(description)
                    .min_items(0_u64)
                    .max_items(question.options.len() as u64),
                false,
            );
        } else {
            let mut property = StringPropertySchema::new()
                .description(description)
                .max_length(4_000_u32);
            if !options.is_empty() {
                property = property.one_of(options);
            }
            schema = schema.property(question.id.clone(), property, false);
        }
    }
    let scope = ElicitationSessionScope::new(request.session_id.as_str().to_owned())
        .tool_call_id(request.tool_call_id.as_str());
    let mut meta = Meta::new();
    meta.insert(
        KEENCODE_META_KEY.to_owned(),
        serde_json::json!({
            "askUser": {
                "allowCustomByQuestion": allow_custom_by_question,
                "questionOrder": question_order
            }
        }),
    );
    CreateElicitationRequest::new(
        ElicitationFormMode::new(scope, schema),
        "请回答编码 Agent 继续执行所需的问题",
    )
    .meta(meta)
}

/// 把选项说明附加到问题正文，使标准 Schema 不丢失决策取舍信息。
fn question_description(question: &UserQuestion) -> String {
    let described = question
        .options
        .iter()
        .filter_map(|option| {
            option
                .description
                .as_deref()
                .map(|description| format!("{}：{description}", option.label))
        })
        .collect::<Vec<_>>();
    if described.is_empty() {
        question.prompt.clone()
    } else {
        format!(
            "{}\n\n选项说明：\n{}",
            question.prompt,
            described.join("\n")
        )
    }
}

/// 将严格 ACP Elicitation 响应还原为 AskUser 的有序答案。
fn response_to_answers(
    questions: &[UserQuestion],
    response: CreateElicitationResponse,
) -> Result<UserQuestionResponse, ElicitationBridgeError> {
    let ElicitationAction::Accept(accepted) = response.action else {
        return Err(ElicitationBridgeError::Cancelled);
    };
    let mut content = accepted.content.unwrap_or_default();
    let mut answers = Vec::with_capacity(questions.len());
    for question in questions {
        let values = match content.remove(&question.id) {
            None => Vec::new(),
            Some(ElicitationContentValue::String(value)) if !question.multi_select => vec![value],
            Some(ElicitationContentValue::StringArray(values)) if question.multi_select => values,
            Some(_) => return Err(ElicitationBridgeError::InvalidResponse),
        };
        answers.push(UserQuestionAnswer {
            id: question.id.clone(),
            values,
        });
    }
    if !content.is_empty() {
        return Err(ElicitationBridgeError::InvalidResponse);
    }
    Ok(UserQuestionResponse { answers })
}

/// 从一个有界 JSON 视图读取字符串响应 ID；严格 DTO 校验仍由对应路由完成。
fn response_request_id(response_json: &str) -> Result<String, ElicitationBridgeError> {
    let value = serde_json::from_str::<Value>(response_json)
        .map_err(|_| ElicitationBridgeError::InvalidResponse)?;
    value
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or(ElicitationBridgeError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_request::ClientRequestBridgeError;
    use keencode_acp::schema::{ElicitationCapabilities, ElicitationFormCapabilities};
    use keencode_agent::{AgentId, SessionId, ToolCallId, TurnId};
    use keencode_tools::UserQuestionOption;
    use serde_json::json;
    use std::future::Future;
    use std::pin::Pin;
    use tokio::sync::{Notify, mpsc};
    use tokio::time::{Duration, timeout};

    /// 模拟 Client 通过 initialize 声明表单能力，测试不依赖生产默认值。
    fn negotiated_coordinator() -> ElicitationCoordinator {
        let coordinator = ElicitationCoordinator::new();
        coordinator
            .negotiate_client_capabilities(&form_capabilities())
            .unwrap();
        coordinator
    }

    /// 构造测试 Client 显式声明的表单能力。
    fn form_capabilities() -> ClientCapabilities {
        ClientCapabilities::new().elicitation(Some(
            ElicitationCapabilities::new().form(Some(ElicitationFormCapabilities::new())),
        ))
    }

    /// 未协商时不支持表单；缺少表单能力的 Client 不能触发任何投递。
    #[test]
    fn form_elicitation_requires_actual_client_capability() {
        let coordinator = ElicitationCoordinator::new();
        assert!(!coordinator.supports_form());
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = Arc::new(RecordingSink { sender });
        let session = SessionId::new("session-capability").unwrap();
        assert!(matches!(
            coordinator.register(&session, request(session.as_str()), sink.clone()),
            Err(ElicitationBridgeError::CapabilitiesUnavailable)
        ));
        coordinator
            .negotiate_client_capabilities(&ClientCapabilities::new())
            .unwrap();
        assert!(matches!(
            coordinator.register(&session, request(session.as_str()), sink),
            Err(ElicitationBridgeError::RegistrationRejected)
        ));
        assert_eq!(coordinator.pending_len(), 0);
        assert!(receiver.try_recv().is_err());
    }

    /// WebView 同能力刷新允许重握手，但不能改变正在使用的能力快照。
    #[test]
    fn repeated_capability_negotiation_is_idempotent_and_shared() {
        let coordinator = ElicitationCoordinator::new();
        let cloned = coordinator.clone();
        coordinator
            .negotiate_client_capabilities(&form_capabilities())
            .unwrap();
        cloned
            .negotiate_client_capabilities(&form_capabilities())
            .unwrap();
        assert!(cloned.supports_form());
        assert_eq!(
            cloned.negotiate_client_capabilities(&ClientCapabilities::new()),
            Err(ElicitationBridgeError::CapabilitiesUnavailable)
        );
        assert!(coordinator.supports_form());
    }

    /// 把标准 Client Request 交给测试接收端。
    struct RecordingSink {
        /// 每次投递保存完整类型化 frame。
        sender: mpsc::Sender<AcpClientRequestFrame>,
    }

    impl ClientRequestSink for RecordingSink {
        /// 异步发送一帧并把关闭接收端映射为稳定投递错误。
        fn send_client_request(
            &self,
            request: AcpClientRequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<(), ClientRequestBridgeError>> + Send + '_>>
        {
            Box::pin(async move {
                self.sender
                    .send(request)
                    .await
                    .map_err(|_| ClientRequestBridgeError::DeliveryUnavailable)
            })
        }
    }

    /// 先公开请求、再等待测试显式回执的问答投递泵。
    struct BlockingAcknowledgementSink {
        /// 已经对桌面可见但尚未返回投递回执的请求。
        sender: mpsc::Sender<AcpClientRequestFrame>,
        /// 测试在验证响应后释放投递 Future。
        release: Arc<Notify>,
    }

    impl ClientRequestSink for BlockingAcknowledgementSink {
        /// 发送请求后阻塞回执，模拟 Tauri emit 与 oneshot 回执之间的调度窗口。
        fn send_client_request(
            &self,
            request: AcpClientRequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<(), ClientRequestBridgeError>> + Send + '_>>
        {
            let sender = self.sender.clone();
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                sender
                    .send(request)
                    .await
                    .map_err(|_| ClientRequestBridgeError::DeliveryUnavailable)?;
                release.notified().await;
                Ok(())
            })
        }
    }

    /// 构造包含单选、多选和说明的完整测试问答。
    fn request(session_id: &str) -> UserQuestionRequest {
        UserQuestionRequest {
            session_id: SessionId::new(session_id).expect("测试 Session 标识有效"),
            turn_id: TurnId::new("turn-a").expect("测试 Turn 标识有效"),
            source_agent_id: AgentId::new("agent-root").expect("测试 Agent 标识有效"),
            tool_call_id: ToolCallId::new("tool-ask-a").expect("测试 ToolCall 标识有效"),
            questions: vec![
                UserQuestion {
                    id: "strategy".to_owned(),
                    prompt: "选择实现策略".to_owned(),
                    options: vec![UserQuestionOption {
                        label: "直接实现".to_owned(),
                        description: Some("立即修改当前模块".to_owned()),
                    }],
                    multi_select: false,
                    allow_custom: true,
                },
                UserQuestion {
                    id: "checks".to_owned(),
                    prompt: "选择验证项".to_owned(),
                    options: vec![
                        UserQuestionOption {
                            label: "测试".to_owned(),
                            description: None,
                        },
                        UserQuestionOption {
                            label: "Clippy".to_owned(),
                            description: None,
                        },
                    ],
                    multi_select: true,
                    allow_custom: false,
                },
            ],
        }
    }

    /// 构造字典序与用户输入顺序不同的三题问答。
    fn non_dictionary_order_request(session_id: &str) -> UserQuestionRequest {
        let mut request = request(session_id);
        request.questions = vec![
            UserQuestion {
                id: "target".to_owned(),
                prompt: "选择部署目标".to_owned(),
                options: vec![UserQuestionOption {
                    label: "服务器".to_owned(),
                    description: None,
                }],
                multi_select: false,
                allow_custom: true,
            },
            UserQuestion {
                id: "checks".to_owned(),
                prompt: "选择检查项".to_owned(),
                options: vec![
                    UserQuestionOption {
                        label: "类型检查".to_owned(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "测试".to_owned(),
                        description: None,
                    },
                ],
                multi_select: true,
                allow_custom: false,
            },
            UserQuestion {
                id: "note".to_owned(),
                prompt: "补充说明".to_owned(),
                options: Vec::new(),
                multi_select: false,
                allow_custom: true,
            },
        ];
        request
    }

    /// 从任意标准 Client Request frame 读取字符串请求标识。
    fn frame_request_id(frame: &AcpClientRequestFrame) -> String {
        serde_json::to_value(frame).expect("Client Request 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned()
    }

    /// 标准请求必须携带 Session、ToolCall、问题 Schema，并按答案类型完成 round-trip。
    #[tokio::test]
    async fn question_handler_round_trips_standard_form_elicitation() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(request("session-a"));
        let frame = receiver.recv().await.expect("应收到标准问答请求");
        let value = serde_json::to_value(&frame).expect("标准问答请求应可序列化");
        assert_eq!(value["method"], json!("elicitation/create"));
        assert_eq!(value["params"]["sessionId"], json!("session-a"));
        assert_eq!(value["params"]["toolCallId"], json!("tool-ask-a"));
        assert_eq!(
            value["params"]["_meta"][KEENCODE_META_KEY]["askUser"]["allowCustomByQuestion"],
            json!({ "strategy": true, "checks": false })
        );
        assert_eq!(
            value["params"]["requestedSchema"]["properties"]["checks"]["type"],
            json!("array")
        );
        let request_id = value["id"].as_str().expect("请求 ID 应为字符串");
        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "action": "accept",
                        "content": {
                            "strategy": "直接实现",
                            "checks": ["测试", "Clippy"]
                        }
                    }
                })
                .to_string(),
            )
            .expect("严格问答响应应完成");
        let response = answer.await.expect("AskUser 应收到答案");
        assert_eq!(response.answers[0].values, ["直接实现"]);
        assert_eq!(response.answers[1].values, ["测试", "Clippy"]);
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// Schema 属性按字典序序列化时，questionOrder 仍保留用户问题和答案顺序。
    #[tokio::test]
    async fn question_order_meta_preserves_input_order_when_schema_properties_are_sorted() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(1);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );
        let answer = handler.ask(non_dictionary_order_request("session-a"));
        let frame = receiver.recv().await.expect("三题问答请求应送达");
        let value = serde_json::to_value(&frame).expect("标准问答请求应可序列化");
        assert_eq!(
            value["params"]["_meta"][KEENCODE_META_KEY]["askUser"]["allowCustomByQuestion"],
            json!({ "target": true, "checks": false, "note": true })
        );
        assert_eq!(
            value["params"]["_meta"][KEENCODE_META_KEY]["askUser"]["questionOrder"],
            json!(["target", "checks", "note"])
        );

        let serialized = serde_json::to_string(&frame).expect("标准问答请求应可序列化");
        let properties_start = serialized
            .find("\"properties\":")
            .expect("序列化请求应包含 properties");
        let serialized_properties = &serialized[properties_start..];
        let checks_position = serialized_properties
            .find("\"checks\":")
            .expect("properties 应包含 checks");
        let note_position = serialized_properties
            .find("\"note\":")
            .expect("properties 应包含 note");
        let target_position = serialized_properties
            .find("\"target\":")
            .expect("properties 应包含 target");
        assert!(
            checks_position < note_position && note_position < target_position,
            "properties 应按字典序序列化: {serialized}"
        );

        let request_id = value["id"].as_str().expect("请求 ID 应为字符串");
        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "action": "accept",
                        "content": {
                            "target": "服务器",
                            "checks": ["测试"],
                            "note": "保留默认配置"
                        }
                    }
                })
                .to_string(),
            )
            .expect("三题合法答案应完成");
        let response = answer.await.expect("AskUser 应收到三题答案");
        assert_eq!(
            response
                .answers
                .iter()
                .map(|answer| answer.id.as_str())
                .collect::<Vec<_>>(),
            vec!["target", "checks", "note"]
        );
        assert_eq!(response.answers[0].values, ["服务器"]);
        assert_eq!(response.answers[1].values, ["测试"]);
        assert_eq!(response.answers[2].values, ["保留默认配置"]);
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// 错 Session、并发弹窗、迟到响应和 Future 丢弃都必须失败关闭。
    #[tokio::test]
    async fn question_handler_rejects_wrong_session_concurrency_and_late_response() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(2);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );
        assert!(handler.ask(request("session-b")).await.is_err());
        let pending = handler.ask(request("session-a"));
        let frame = receiver.recv().await.expect("首个请求应送达");
        assert!(handler.ask(request("session-a")).await.is_err());
        let request_id = serde_json::to_value(frame).expect("frame 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned();
        drop(pending);
        tokio::task::yield_now().await;
        assert_eq!(coordinator.pending_len(), 0);
        assert_eq!(
            coordinator.respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": { "action": "cancel" }
                })
                .to_string()
            ),
            Err(ElicitationBridgeError::UnknownRequest)
        );
    }

    /// 用户取消应被当作已处理响应，而已知请求的畸形结果必须失败关闭并唤醒工具。
    #[tokio::test]
    async fn question_handler_finishes_cancel_and_malformed_response_exactly_once() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(2);
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(RecordingSink { sender }),
        );

        let cancelled = handler.ask(request("session-a"));
        let cancelled_frame = receiver.recv().await.expect("取消请求应先送达");
        let cancelled_id = serde_json::to_value(cancelled_frame).expect("frame 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned();
        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": cancelled_id,
                    "result": { "action": "cancel" }
                })
                .to_string(),
            )
            .expect("标准取消响应应被成功收口");
        assert!(cancelled.await.is_err());
        assert_eq!(coordinator.pending_len(), 0);

        let malformed = handler.ask(request("session-a"));
        let malformed_frame = receiver.recv().await.expect("畸形响应请求应先送达");
        let malformed_id = serde_json::to_value(malformed_frame).expect("frame 应可序列化")["id"]
            .as_str()
            .expect("请求 ID 应为字符串")
            .to_owned();
        assert_eq!(
            coordinator.respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": malformed_id,
                    "result": { "action": "unsupported" }
                })
                .to_string()
            ),
            Err(ElicitationBridgeError::InvalidResponse)
        );
        assert!(malformed.await.is_err());
        assert_eq!(coordinator.pending_len(), 0);
    }

    /// Client 在 emit 后、投递回执前立即回答时不能丢失真实答案。
    #[tokio::test]
    async fn response_visible_before_delivery_ack_is_accepted_exactly_once() {
        let coordinator = Arc::new(negotiated_coordinator());
        let (sender, mut receiver) = mpsc::channel(1);
        let release = Arc::new(Notify::new());
        let handler = coordinator.handler(
            SessionId::new("session-a").expect("测试 Session 标识有效"),
            Arc::new(BlockingAcknowledgementSink {
                sender,
                release: Arc::clone(&release),
            }),
        );
        let answer = handler.ask(request("session-a"));
        let frame = receiver.recv().await.expect("问答请求应先对 Client 可见");
        let request_id = frame_request_id(&frame);

        coordinator
            .respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "action": "accept",
                        "content": {
                            "strategy": "直接实现",
                            "checks": ["测试"]
                        }
                    }
                })
                .to_string(),
            )
            .expect("已经对 Client 可见的问答应接受立即响应");
        let response = timeout(Duration::from_secs(2), answer)
            .await
            .expect("立即响应应在投递回执前唤醒 AskUser")
            .expect("合法答案应返回工具");
        assert_eq!(response.answers[0].values, ["直接实现"]);
        assert_eq!(response.answers[1].values, ["测试"]);
        assert_eq!(coordinator.pending_len(), 0);
        assert_eq!(
            coordinator.respond(
                &json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": { "action": "cancel" }
                })
                .to_string()
            ),
            Err(ElicitationBridgeError::UnknownRequest)
        );
        release.notify_one();
    }
}
