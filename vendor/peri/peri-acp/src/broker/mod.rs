//! Interaction broker — bridges AskUser questions to ACP RPC.
//!
//! Implements [`UserInteractionBroker`](peri_acp_types::interaction::UserInteractionBroker) trait,
//! translating user questions into `elicitation/create` RPC via an
//! [`AcpTransport`](crate::transport::AcpTransport).

pub mod transport_broker;
pub use transport_broker::*;
