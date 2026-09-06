use keencode_model::{ProviderProtocol, TokenUsage};

/// `ModelRequest.metadata` 中由 Runtime 写入的 Session 标识键。
pub const REQUEST_METADATA_SESSION_ID: &str = "keencode.session_id";
/// `ModelRequest.metadata` 中由 Runtime 写入的 Turn 标识键。
pub const REQUEST_METADATA_TURN_ID: &str = "keencode.turn_id";
/// `ModelRequest.metadata` 中由 Runtime 写入的根或子 Agent 标识键。
pub const REQUEST_METADATA_AGENT_ID: &str = "keencode.agent_id";
/// `ModelRequest.metadata` 中由 Runtime 写入的调用用途键。
pub const REQUEST_METADATA_PURPOSE: &str = "keencode.purpose";

/// 一条模型请求观测属于完整逻辑请求还是一次实际 HTTP 尝试。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestObservationScope {
    /// 覆盖请求校验、编码和所有实际 HTTP 尝试的完整调用。
    Logical,
    /// 一次已经准备发往远端的实际 HTTP 请求。
    Attempt,
}

/// 模型请求在当前观测点的生命周期状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestObservationState {
    /// 请求已经开始且尚未形成终态。
    Started,
    /// 请求已经成功读到协议终态；调用方可在此后立即结束当前 Round。
    Completed,
    /// 请求 Future 或响应流被调用方提前丢弃。
    Cancelled,
    /// 请求校验、传输或协议处理失败。
    Failed,
}

/// 模型请求在线上采用的响应方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestMode {
    /// 请求并按顺序消费 SSE 增量事件。
    Stream,
    /// 请求完整 JSON 后再输出统一事件流。
    Buffered,
}

/// 模型请求失败的稳定、Provider 中立分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestErrorKind {
    /// 无法建立或维持网络连接。
    Connection,
    /// 请求超过明确超时。
    Timeout,
    /// TLS 握手或证书校验失败。
    Tls,
    /// 其他传输失败。
    Transport,
    /// 远端返回非成功 HTTP 状态。
    HttpStatus,
    /// 远端响应不能满足统一协议约束。
    Protocol,
    /// HTTP 成功后响应流在协议终态前中断。
    StreamInterrupted,
    /// 调用方主动取消请求。
    Cancelled,
    /// 所有显式重试都已失败。
    RetryExhausted,
    /// 不能归入以上稳定类型的失败。
    Other,
}

/// 不包含请求正文、响应正文、Header 或凭据的模型请求观测。
#[derive(Clone, Debug)]
pub struct RequestObservation {
    /// 当前观测所属层级。
    pub scope: RequestObservationScope,
    /// 当前生命周期状态。
    pub state: RequestObservationState,
    /// 一次逻辑请求内保持不变的稳定标识。
    pub logical_request_id: String,
    /// 实际 HTTP 尝试序号；逻辑层或发出请求前失败时为零。
    pub attempt: u32,
    /// 当前请求允许的最大 HTTP 尝试次数。
    pub max_attempts: u32,
    /// Provider 中立模型标识。
    pub model: String,
    /// 当前真实调用使用的三种协议之一。
    pub protocol: ProviderProtocol,
    /// 当前调用的响应方式。
    pub mode: RequestMode,
    /// 当前协议端点；消费者落盘前仍须投影为安全 origin。
    pub endpoint: String,
    /// 当前观测的 Unix 毫秒时间。
    pub at_ms: u64,
    /// 已结束观测从开始到当前的毫秒数。
    pub duration_ms: Option<u64>,
    /// 首次收到响应头的 Unix 毫秒时间。
    pub response_headers_at_ms: Option<u64>,
    /// 远端返回的 HTTP 状态。
    pub http_status: Option<u16>,
    /// Provider 返回的安全请求标识。
    pub provider_request_id: Option<String>,
    /// Provider 中立用量；全部字段为空表示远端未报告。
    pub usage: TokenUsage,
    /// 失败观测的稳定分类。
    pub error_kind: Option<RequestErrorKind>,
    /// 已脱敏且有界的失败说明。
    pub error_summary: Option<String>,
    /// 可选的 Session 标识。
    pub session_id: Option<String>,
    /// 可选的 Turn 标识。
    pub turn_id: Option<String>,
    /// 可选的根或子 Agent 标识。
    pub agent_id: Option<String>,
    /// 调用用途，例如 agent、memory 或 title。
    pub purpose: Option<String>,
}

/// 同步接收模型 HTTP 生命周期短元数据的观察者。
///
/// 实现不得阻塞、panic 或把失败反向注入模型请求。调用方不会向此接口提供正文、
/// Header、Cookie 或凭据。
pub trait RequestObserver: Send + Sync {
    /// 接收一条已经脱敏且不含正文的请求观测。
    fn on_request(&self, observation: RequestObservation);
}
