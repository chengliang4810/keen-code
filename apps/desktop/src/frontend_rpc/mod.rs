//! ZCode RPC transport adapter for the Tauri desktop host.

mod agent_views;
pub(crate) mod attachments;
mod codec;
pub(crate) mod command_receipts;
pub(crate) mod controller;
pub(crate) mod desktop_controls;
pub(crate) mod dispatch;
pub(crate) mod host;
pub(crate) mod media_protocol;
pub(crate) mod native_actions;
mod native_file_watcher;
pub(crate) mod providers;
pub(crate) mod services;
pub(crate) mod session;
mod terminal;
pub(crate) mod workflows;
pub(crate) mod workspace_hook_review;
mod workspace_search;

pub(crate) use dispatch::RpcGateway;
pub(crate) use host::DesktopHandler;
