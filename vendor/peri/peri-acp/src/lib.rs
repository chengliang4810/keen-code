//! peri-acp — ACP Agent Service Layer
//!
//! Provides session management, agent construction, middleware chain assembly,
//! transport abstraction (mpsc), event mapping, and AskUser broker.
//! The embedded desktop host uses the in-memory MPSC transport.

// 过渡 re-export：telemetry 当前驻留 peri-agent；L5 从 peri-acp 移除
// peri-agent 依赖时须先给 telemetry 独立归宿（届时处理）。
pub use peri_agent::telemetry;

pub mod agent;
pub mod broker;
pub mod dispatch;
pub mod event;
pub mod host;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod transport;
