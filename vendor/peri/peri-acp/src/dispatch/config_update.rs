//! Shared ConfigOptionUpdate construction for ACP transport paths.
//!
//! Both the TUI notify layer and the request handler need to build
//! `ConfigOptionUpdate` values from the current `LlmProvider`.
//! This module centralises that construction to avoid duplication.

use crate::provider::LlmProvider;
use crate::session::state_builders::build_config_options;
use agent_client_protocol::schema::v1::{ConfigOptionUpdate, SessionConfigOption};

/// Build config options list from current config state.
pub fn make_config_options(provider: &LlmProvider) -> Vec<SessionConfigOption> {
    build_config_options(provider)
}

/// Build a [`ConfigOptionUpdate`] from current config state.
pub fn make_config_option_update(provider: &LlmProvider) -> ConfigOptionUpdate {
    let config_options = make_config_options(provider);
    ConfigOptionUpdate::new(config_options)
}
