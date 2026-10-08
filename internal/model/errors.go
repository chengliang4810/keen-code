package model

import "fmt"

// ErrorKind is the stable classification of a ModelError. The values match the
// snake_case serde tags of ModelError in core/model/src/error.rs (structured
// output kinds are omitted: structured output is out of the v1 scope).
type ErrorKind string

const (
	// ErrorAuthentication reports missing, invalid, or expired credentials.
	ErrorAuthentication ErrorKind = "authentication"
	// ErrorAuthorization reports valid credentials denied access to the
	// target resource or model.
	ErrorAuthorization ErrorKind = "authorization"
	// ErrorQuotaExceeded reports an exhausted balance, quota, or plan.
	ErrorQuotaExceeded ErrorKind = "quota_exceeded"
	// ErrorModelNotFound reports an unknown model or an unavailable endpoint.
	ErrorModelNotFound ErrorKind = "model_not_found"
	// ErrorProtocolUnsupported reports an endpoint that does not serve the
	// requested protocol.
	ErrorProtocolUnsupported ErrorKind = "protocol_unsupported"
	// ErrorRateLimited reports a rate or quota limited call. It is always
	// retryable.
	ErrorRateLimited ErrorKind = "rate_limited"
	// ErrorContextLengthExceeded reports input beyond the model window.
	ErrorContextLengthExceeded ErrorKind = "context_length_exceeded"
	// ErrorInvalidRequest reports a request that violates the unified model
	// layer invariants.
	ErrorInvalidRequest ErrorKind = "invalid_request"
	// ErrorOutputLimitRejected reports a max output tokens value rejected by
	// the remote endpoint; the agent loop uses it for a single de-limited
	// retry instead of string matching vendor text.
	ErrorOutputLimitRejected ErrorKind = "output_limit_rejected"
	// ErrorUnsupportedCapability reports a requested capability the endpoint
	// does not support. Capability carries the stable capability name.
	ErrorUnsupportedCapability ErrorKind = "unsupported_capability"
	// ErrorProviderUnavailable reports a temporarily unavailable model
	// service. Retryable marks whether callers should retry with backoff.
	ErrorProviderUnavailable ErrorKind = "provider_unavailable"
	// ErrorTransport reports a network, timeout, or connection failure.
	// Retryable marks whether callers should retry with backoff.
	ErrorTransport ErrorKind = "transport"
	// ErrorStreamInterrupted reports an HTTP-success stream cut before the
	// protocol terminal event. PartialText carries the already streamed text
	// so callers can persist it into history.
	ErrorStreamInterrupted ErrorKind = "stream_interrupted"
	// ErrorProtocol reports a remote response that cannot be converted into
	// unified events (event order, field, or content problems).
	ErrorProtocol ErrorKind = "protocol"
	// ErrorCancelled reports a call cancelled by the user or the runtime.
	ErrorCancelled ErrorKind = "cancelled"
)

// ModelError is the unified error the model layer reports upward. It mirrors
// the tagged enum ModelError in core/model/src/error.rs as a single struct
// with a kind discriminant, so it survives JSON round trips (journal, IPC)
// without losing its classification. Use errors.As with *ModelError.
//
// Message must be safe to show: it never contains credentials, full model
// output, or raw user data. Run adapter-provided upstream text through
// RedactErrorSecrets before storing it here.
type ModelError struct {
	Kind         ErrorKind `json:"kind"`
	Message      string    `json:"message"`
	Capability   string    `json:"capability,omitempty"`   // ErrorUnsupportedCapability
	StatusCode   int       `json:"statusCode,omitempty"`   // remote HTTP status; 0 = none
	RetryAfterMS int64     `json:"retryAfterMs,omitempty"` // ErrorRateLimited hint; 0 = absent
	Retryable    bool      `json:"retryable,omitempty"`    // backoff hint for transport-class kinds
	PartialText  string    `json:"partialText,omitempty"`  // ErrorStreamInterrupted already-streamed text
}

// compile-time assertion that *ModelError satisfies error via pointer.
var _ error = (*ModelError)(nil)

// Error implements the error interface with the same Chinese display strings
// as the Rust ModelError Display impl (core/model/src/error.rs:63-194).
func (e *ModelError) Error() string {
	switch e.Kind {
	case ErrorUnsupportedCapability:
		return fmt.Sprintf("模型能力不受支持（%s）：%s", e.Capability, e.Message)
	default:
		return e.displayPrefix() + e.Message
	}
}

// displayPrefix returns the kind-specific Chinese prefix of Error().
func (e *ModelError) displayPrefix() string {
	switch e.Kind {
	case ErrorAuthentication:
		return "模型服务认证失败："
	case ErrorAuthorization:
		return "模型服务拒绝授权："
	case ErrorQuotaExceeded:
		return "模型服务额度不足："
	case ErrorModelNotFound:
		return "模型不可用："
	case ErrorProtocolUnsupported:
		return "模型协议不受支持："
	case ErrorRateLimited:
		return "模型服务限制了请求："
	case ErrorContextLengthExceeded:
		return "模型上下文超过限制："
	case ErrorInvalidRequest:
		return "模型请求无效："
	case ErrorOutputLimitRejected:
		return "模型请求无效："
	case ErrorProviderUnavailable:
		return "模型服务不可用："
	case ErrorTransport:
		return "模型传输失败："
	case ErrorStreamInterrupted:
		return "模型响应流中断："
	case ErrorProtocol:
		return "模型响应协议错误："
	case ErrorCancelled:
		return "模型调用已取消："
	default:
		return ""
	}
}

// IsRetryable reports whether the error suits an automatic retry with
// backoff. Rate limits are always retryable; transport-class errors carry
// their own Retryable flag; everything else is final. Same table as
// ModelError::is_retryable (core/model/src/error.rs:199-218).
func (e *ModelError) IsRetryable() bool {
	switch e.Kind {
	case ErrorRateLimited:
		return true
	case ErrorProviderUnavailable, ErrorTransport, ErrorStreamInterrupted:
		return e.Retryable
	default:
		return false
	}
}

// WithPartialText attaches already-streamed text to a stream interruption.
// It only affects ErrorStreamInterrupted; other kinds are returned unchanged.
// Empty text keeps the field unset, mirroring
// ModelError::with_partial_text (core/model/src/error.rs:225-233).
func (e *ModelError) WithPartialText(partialText string) *ModelError {
	if e.Kind == ErrorStreamInterrupted && partialText != "" {
		e.PartialText = partialText
	}
	return e
}

// StreamPartialText returns the already-streamed text attached to a stream
// interruption, or "" when absent or for other kinds.
func (e *ModelError) StreamPartialText() string {
	if e.Kind == ErrorStreamInterrupted {
		return e.PartialText
	}
	return ""
}

// NewError returns a model error of the given kind with a safe display
// message.
func NewError(kind ErrorKind, message string) *ModelError {
	return &ModelError{Kind: kind, Message: message}
}

// InvalidRequest returns an ErrorInvalidRequest; the message is formatted
// like fmt.Sprintf.
func InvalidRequest(format string, args ...any) *ModelError {
	return &ModelError{Kind: ErrorInvalidRequest, Message: fmt.Sprintf(format, args...)}
}

// ProtocolError returns an ErrorProtocol; the message is formatted like
// fmt.Sprintf.
func ProtocolError(format string, args ...any) *ModelError {
	return &ModelError{Kind: ErrorProtocol, Message: fmt.Sprintf(format, args...)}
}

// CancelledError returns an ErrorCancelled carrying the cancellation reason.
func CancelledError(message string) *ModelError {
	return &ModelError{Kind: ErrorCancelled, Message: message}
}
