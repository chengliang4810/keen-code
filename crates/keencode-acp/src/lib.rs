//! KeenCode 进程内 Agent Client Protocol 边界。
//!
//! 本 crate 复用官方 ACP Schema 描述标准方法，并只为标准无法表达的
//! Session 控制面和 KeenCode 生命周期事件提供类型化扩展。它不包含模型、
//! 工具、存储或桌面框架实现。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod capability;
mod delivery;
mod elicitation;
mod error;
mod event;
mod file_change;
mod json;
mod protocol;
mod sequence;

/// 官方 ACP Schema，供桌面 Host 与 Runtime 共享同一套标准类型。
pub use agent_client_protocol_schema as schema;
pub use agent_client_protocol_schema::{
    AuthenticateResponse, ForkSessionResponse, ListSessionsResponse, LoadSessionResponse,
    NewSessionResponse, PromptResponse, SetSessionConfigOptionResponse, SetSessionModeResponse,
};
pub use capability::{
    InitializeAgentCapabilitiesDto, InitializeResponseDto, InitializeSessionCapabilitiesDto,
    SessionDeleteCapabilities, validate_set_session_mode_request,
};
pub use delivery::{
    SESSION_UPDATE_DELIVERY_SCHEMA_VERSION, SessionUpdateDeliveryEnvelope,
    SessionUpdateDeliveryLimits,
};
pub use elicitation::ElicitationRouter;
pub use error::AcpBoundaryError;
pub use event::{
    AgentLifecycleStatus, BackgroundTaskKind, BackgroundTaskTerminalStatus, CompactionFailureKind,
    KeenCodeEvent, KeenCodeEventEnvelope, KeenCodeEventEnvelopeParams, KeenCodeEventLimits,
    McpOAuthEvent, RecoveryState, SystemNotificationLevel, TurnFailureKind,
};
pub use file_change::{
    FILE_CHANGE_META_KEY, FileChangeReference, FileChangeSide, FileSnapshotInfo,
    MAX_FILE_CHANGE_BYTES, MAX_FILE_CHANGE_READ_BYTES, ReadFileChangeRequest,
    ReadFileChangeResponse,
};
pub use protocol::{
    AcpClientRequestEncoder, AcpClientRequestFrame, AcpIncomingFrame, AcpNotification, AcpRequest,
    AcpRequestDecoder, AcpRequestFrame, AcpRequestLimits, AcpResponseDecoder, AcpResponseEncoder,
    AcpResponseLimits, AcpResponsePayload, AcpResultFrame, BackgroundTaskInfo,
    CancelBackgroundTaskRequest, CancelBackgroundTaskResponse, DeleteSessionRequest,
    DeleteSessionResponse, GenerateSessionTitleRequest, GenerateSessionTitleResponse,
    GoalClearRequest, GoalClearResponse, GoalGetRequest, GoalGetResponse, GoalInput,
    GoalMutationResponse, GoalRecord, GoalScope, GoalStatus, GoalTransitionRequest,
    GoalTransitionResponse, GoalTransitionStatus, GoalUpsertRequest, GoalUpsertResponse,
    ListBackgroundTasksRequest, ListBackgroundTasksResponse, MAX_REPLAY_EVENTS,
    McpConnectionStatus, McpListRequest, McpListResponse, McpOAuthCallbackRequest,
    McpOAuthCallbackResponse, McpOAuthCallbackStatus, McpOAuthCancelResponse, McpOAuthNotification,
    McpOAuthServerRequest, McpOAuthStartResponse, McpOAuthStartStatus, McpOAuthStatus,
    McpRuntimePhase, McpServerStatus, McpTransportKind, RenameSessionRequest,
    RenameSessionResponse, ReplaySessionRequest, ReplaySessionResponse, RewindCandidate,
    RewindCandidatesRequest, RewindCandidatesResponse, RewindSessionRequest, RewindSessionResponse,
    SessionConfigUpdateNotification, SteerSessionRequest, SteerSessionResponse, ValidateAcpParams,
};
pub use sequence::SessionSequence;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod background_list_tests;

#[cfg(test)]
mod file_change_tests;
