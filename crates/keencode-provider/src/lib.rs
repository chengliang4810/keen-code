//! KeenCode 的 Provider 中立 HTTP Adapter 层。
//!
//! 本 crate 只在协议边界处理三种厂商线格式，并向 Agent Runtime 暴露
//! [`keencode_model::ModelProvider`]。Agent、工具、Session 和 ACP 不得依赖本 crate
//! 内部的请求字段或响应事件名称。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod adapters;
mod catalog;
mod client;
mod config;
mod http;
mod observation;
mod registry;
mod sse;
#[cfg(feature = "live-test-trace")]
mod trace;

pub use catalog::{ModelCatalog, ModelCatalogEntry, ModelCatalogFailure};
pub use client::ProviderClient;
pub use config::{
    ApiKey, ProviderConfig, ProviderConfigError, ProviderEndpoints, WireResponseMode,
};
pub use observation::{
    REQUEST_METADATA_AGENT_ID, REQUEST_METADATA_PURPOSE, REQUEST_METADATA_SESSION_ID,
    REQUEST_METADATA_TURN_ID, RequestErrorKind, RequestMode, RequestObservation,
    RequestObservationScope, RequestObservationState, RequestObserver,
};
pub use registry::{
    ProviderModelPolicy, ProviderRegistration, ProviderRegistrationSummary, ProviderRegistry,
    ProviderRegistryError, ProviderRegistrySnapshot, ResolvedProvider,
};
#[cfg(feature = "live-test-trace")]
pub use trace::{
    WireExchange, WireTraceCollector, encode_wire_request, replay_wire_error_response,
    replay_wire_response,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
