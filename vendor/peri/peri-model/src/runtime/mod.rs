mod error;
mod observe;
mod request;
#[allow(dead_code)]
mod retry;
#[allow(dead_code)]
pub(crate) mod stream;

pub use error::{
    ModelError, ModelResult, ProtocolError, ProtocolErrorKind, RetryErrorKind, TransportErrorKind,
};
pub(crate) use observe::{start_logical_request, RequestLifecycle, RequestObservationContext};
pub use observe::{
    RequestErrorKind, RequestObservation, RequestObservationScope, RequestObservationState,
    RequestObserver,
};
pub use request::{ModelRuntimeConfig, ObservedProviderBody, PreparedModelRequest};
pub use retry::{RetryConfig, RetryObservation, RetryObserver, RetryableErrorClasses};
