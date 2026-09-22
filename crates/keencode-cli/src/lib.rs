//! KeenCode 的本地 ACP Client、NDJSON Transport 与非交互命令模型。
//!
//! 本 crate 不创建 Runtime，也不依赖 Tauri。它只负责通过同数据根下的本机
//! Host 访问现有 ACP `session/*` 和 `keencode/*` 方法；Desktop、headless Host、
//! Web 和 CLI 共享同一 ACP 请求/事件语义。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod client;
mod command;
mod discovery;
mod exit_code;
mod server;
mod transport;

mod executor;
mod headless;

pub use client::{ClientError, HostClient, HostClientConfig, HostEvent, RequestResult};
pub use command::{
    CliCommand, CliOptions, CliParseError, HeadlessOptions, RunCommand, SessionCommand, WebCommand,
    parse_args, usage,
};
pub use discovery::{
    ENDPOINT_FILE_NAME, EndpointRecord, EndpointRecordError, HostOwnerKind, IpcTransport,
    data_root_fingerprint, default_data_root, endpoint_path,
};
pub use executor::{
    CliExecutionError, CliExecutionResult, DETACHED_PROMPT_METHOD, OPERATION_STATUS_METHOD,
    WEB_START_METHOD, WEB_STATUS_METHOD, WEB_STOP_METHOD, execute_command, execute_with_client,
};
pub use exit_code::ExitCode;
pub use headless::{HeadlessHost, HeadlessHostError};
pub use server::{
    FixedIdlePolicy, HostActivity, HostDispatch, HostDispatchError, HostDispatchFuture,
    HostIdlePolicy, LocalHostServer, LocalHostServerConfig, LocalHostServerError,
    LocalHostServerHandle, NoopHostActivity,
};
pub use transport::{
    IpcConnection, IpcListener, NdjsonError, NdjsonFrame, NdjsonReader, NdjsonWriter,
};
