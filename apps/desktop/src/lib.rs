//! KeenCode 原生 GPUI 桌面组合根。
//!
//! 界面直接调用类型化 Rust 领域服务；没有浏览器、JavaScript 执行器或前端 RPC。

mod agent_prompt;
pub mod agent_runtime;
mod analytics;
mod app_exit;
mod app_settings;
mod app_updates;
mod client_request;
mod diagnostics;
mod elicitation;
mod http_response;
mod memories;
mod model_metadata;
mod native_agents;
mod native_attention;
mod native_controls;
mod native_drafts;
mod native_extension_contributor;
pub(crate) mod native_hooks;
mod native_host;
mod native_insights;
mod native_memory;
pub mod native_paths;
mod native_rewind;
mod native_services;
mod native_sidebar;
pub mod native_ui;
mod native_update_installer;
mod native_wire_trace;
mod network_proxy;
mod path_utils;
mod permissions;
mod personalization;
mod plugin_secrets;
mod plugins;
mod power_management;
mod providers;
mod session_commands;
mod shell_env;
mod storage;
mod task_notifications;
mod tray;
mod workflows;
mod workspace;

pub use native_host::NativeHost;

/// 创建单一 Rust Host 和 GPUI 原生窗口。
pub fn run() {
    match native_update_installer::run_update_helper_if_requested() {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            diagnostics::write_stderr_best_effort(format_args!(
                "KeenCode 更新助手失败：{}",
                keencode_model::redact_error_secrets_bounded(&format!("{error:#}"), 2048)
            ));
            std::process::exit(1);
        }
    }
    if let Err(error) = native_host::run() {
        diagnostics::write_stderr_best_effort(format_args!(
            "KeenCode 启动失败：{}",
            keencode_model::redact_error_secrets_bounded(&format!("{error:#}"), 2048)
        ));
        std::process::exit(1);
    }
}
