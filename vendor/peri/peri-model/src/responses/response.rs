use crate::ModelError;

pub(super) fn provider_protocol_error() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Provider)
}
