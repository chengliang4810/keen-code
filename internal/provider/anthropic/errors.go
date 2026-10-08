package anthropic

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/url"
	"strings"
	"unicode"

	"keencode/internal/model"
)

const (
	// maxErrorInputBytes bounds the raw UTF-8 input kept for error
	// classification and structured redaction (http.rs:21).
	maxErrorInputBytes = 64 * 1024
	// maxErrorMessageChars bounds the final error display text in Unicode
	// characters (http.rs:23).
	maxErrorMessageChars = 1000
	// unspecifiedErrorMessage is the fallback message when a provider error
	// payload explains nothing (http.rs:670).
	unspecifiedErrorMessage = "模型服务返回未说明错误"
	// emptyServerErrorMessage replaces blank sanitized error bodies
	// (http.rs:704).
	emptyServerErrorMessage = "模型服务返回空错误"
)

// providerErrorFields extracts the display message and public error code from
// a common error JSON body or plain text body (http.rs:661-680).
func providerErrorFields(body []byte) (message string, code string, hasCode bool) {
	var value map[string]json.RawMessage
	if err := json.Unmarshal(body, &value); err == nil {
		source := value
		if raw, ok := value["error"]; ok {
			var nested map[string]json.RawMessage
			if json.Unmarshal(raw, &nested) == nil && nested != nil {
				source = nested
			}
		}
		if text, ok := jsonStringField(source, "message"); ok {
			message = text
		} else if text, ok := jsonStringField(source, "msg"); ok {
			message = text
		} else if text, ok := jsonStringField(value, "message"); ok {
			message = text
		} else if text, ok := jsonStringField(value, "msg"); ok {
			message = text
		} else {
			message = unspecifiedErrorMessage
		}
		if raw, ok := source["code"]; ok {
			var text string
			if json.Unmarshal(raw, &text) == nil {
				return message, text, true
			}
			// Non-string codes degrade to their JSON rendering.
			return message, string(raw), true
		}
		return message, "", false
	}
	return string(body), "", false
}

// jsonStringField reads a string-typed JSON object field.
func jsonStringField(object map[string]json.RawMessage, field string) (string, bool) {
	raw, ok := object[field]
	if !ok {
		return "", false
	}
	var text string
	if err := json.Unmarshal(raw, &text); err != nil {
		return "", false
	}
	return text, true
}

// classifyHTTPError normalizes a non-2xx HTTP response by status, public
// error code, and bounded raw text, mirroring classify_http_error_with_api_key
// (core/provider/src/http.rs:109-266). retryAfterMS and hasRetryAfter carry
// the parsed Retry-After header (seconds scaled to milliseconds); apiKey is
// redacted from the display text before classification output is built.
func classifyHTTPError(apiKey string, status int, retryAfterMS int64, hasRetryAfter bool, message, code string) *model.ModelError {
	classifier := strings.ToLower(boundedUTF8Prefix(code, maxErrorInputBytes) + " " + boundedUTF8Prefix(message, maxErrorInputBytes))
	safe := safeErrorMessage(apiKey, message)

	contains := func(fragments ...string) bool {
		for _, fragment := range fragments {
			if !strings.Contains(classifier, fragment) {
				return false
			}
		}
		return true
	}
	switch {
	case contains("context_length") || contains("context length") || contains("context window") ||
		contains("maximum context") || contains("max context") || contains("prompt is too long") ||
		contains("input is too long") || contains("too many tokens") ||
		contains("input", "token") && containsAny(classifier, "exceed", "maximum") ||
		contains("input", "context") && containsAny(classifier, "exceed", "maximum", "too_long", "too long") ||
		contains("上下文", "超"):
		return &model.ModelError{Kind: model.ErrorContextLengthExceeded, Message: safe}
	case contains("quota_exhausted") || contains("insufficient_balance") || contains("insufficient_quota") ||
		contains("余额不足") || contains("套餐次数已用尽") || status == 402:
		return &model.ModelError{Kind: model.ErrorQuotaExceeded, Message: safe, StatusCode: status}
	case contains("invalid_api_key") || contains("authentication_error") || contains("unauthorized") ||
		contains("authentication failed") || contains("认证失败"):
		return &model.ModelError{Kind: model.ErrorAuthentication, Message: safe, StatusCode: status}
	case contains("permission_denied") || contains("forbidden") || contains("not authorized") ||
		contains("authorization failed") || contains("无权") || contains("未授权"):
		return &model.ModelError{Kind: model.ErrorAuthorization, Message: safe, StatusCode: status}
	case contains("rate_limit") || contains("rate limited") || contains("too many requests") ||
		contains("throttled") || contains("请求过于频繁"):
		err := &model.ModelError{Kind: model.ErrorRateLimited, Message: safe, StatusCode: status}
		if hasRetryAfter {
			err.RetryAfterMS = retryAfterMS
		}
		return err
	case contains("model_not_found") || contains("unsupported_model") ||
		contains("model") && contains(classifier, "not supported") ||
		contains("模型") && contains(classifier, "不支持"):
		return &model.ModelError{Kind: model.ErrorModelNotFound, Message: safe, StatusCode: status}
	case contains("service_unavailable") || contains("server_error") || contains("temporarily unavailable") ||
		contains("overloaded") || contains("服务不可用") || contains("过载"):
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: safe, StatusCode: status, Retryable: true}
	}

	switch status {
	case 401:
		return &model.ModelError{Kind: model.ErrorAuthentication, Message: safe, StatusCode: status}
	case 403:
		return &model.ModelError{Kind: model.ErrorAuthorization, Message: safe, StatusCode: status}
	case 404, 405:
		return &model.ModelError{Kind: model.ErrorProtocolUnsupported, Message: safe, StatusCode: status}
	case 425, 429:
		err := &model.ModelError{Kind: model.ErrorRateLimited, Message: safe, StatusCode: status}
		if hasRetryAfter {
			err.RetryAfterMS = retryAfterMS
		}
		return err
	case 408:
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: safe, StatusCode: status, Retryable: true}
	case 400, 409, 422:
		// A 400 rejecting the output limit is a downgradeable retry signal:
		// the structured variant lets the agent loop retry once without the
		// limit instead of string-matching vendor text in the neutral layer.
		if contains("max_tokens") || contains("max_output_tokens") || contains("max_completion_tokens") {
			return &model.ModelError{Kind: model.ErrorOutputLimitRejected, Message: safe}
		}
		return &model.ModelError{Kind: model.ErrorInvalidRequest, Message: safe}
	// 500/502/503/504, the Anthropic overload 529, and the Cloudflare origin
	// blips 520/521/522/523/524/527 are transient; 525/526 describe TLS
	// handshake and certificate failures and stay non-retryable.
	case 500, 502, 503, 504, 520, 521, 522, 523, 524, 527, 529:
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: safe, StatusCode: status, Retryable: true}
	default:
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: safe, StatusCode: status}
	}
}

// containsAny reports whether the classifier text contains any fragment.
func containsAny(classifier string, fragments ...string) bool {
	for _, fragment := range fragments {
		if strings.Contains(classifier, fragment) {
			return true
		}
	}
	return false
}

// classifyInBandProviderError normalizes a provider error carried in an HTTP
// 200 body: only error kinds with explicit structure or context-length
// semantics keep their classification (minus the fabricated status); unknown
// or ordinary invalid_request errors stay protocol errors
// (core/provider/src/http.rs:272-288). Like the Rust in-band path it runs
// without the API key: only the HTTP-status classification redacts the exact
// credential.
func classifyInBandProviderError(message, code string) *model.ModelError {
	classified := classifyHTTPError("", 400, 0, false, message, code)
	switch classified.Kind {
	case model.ErrorContextLengthExceeded, model.ErrorAuthentication, model.ErrorAuthorization,
		model.ErrorQuotaExceeded, model.ErrorModelNotFound, model.ErrorProtocolUnsupported,
		model.ErrorRateLimited, model.ErrorProviderUnavailable:
		classified.StatusCode = 0
		classified.RetryAfterMS = 0
		return classified
	default:
		return model.ProtocolError("%s", classified.Message)
	}
}

// classifyProviderErrorPayload normalizes an in-band error JSON value using
// error.message → message → fallback and the error.code/error.type code
// (core/provider/src/adapters/wire.rs:83-92).
func classifyProviderErrorPayload(value map[string]json.RawMessage, fallback string) *model.ModelError {
	message := providerErrorMessage(value, fallback)
	code := ""
	hasCode := false
	if raw, ok := value["error"]; ok {
		var nested map[string]json.RawMessage
		if json.Unmarshal(raw, &nested) == nil && nested != nil {
			if text, ok := jsonStringField(nested, "code"); ok {
				code, hasCode = text, true
			} else if text, ok := jsonStringField(nested, "type"); ok {
				code, hasCode = text, true
			}
		}
	}
	if !hasCode {
		if text, ok := jsonStringField(value, "code"); ok {
			code, hasCode = text, true
		}
	}
	if !hasCode {
		return classifyInBandProviderError(message, "")
	}
	return classifyInBandProviderError(message, code)
}

// providerErrorMessage extracts the safe text summary from a provider error
// object: error.message → message → fallback
// (core/provider/src/adapters/wire.rs:60-79).
func providerErrorMessage(value map[string]json.RawMessage, fallback string) string {
	if raw, ok := value["error"]; ok {
		var nested map[string]json.RawMessage
		if json.Unmarshal(raw, &nested) == nil && nested != nil {
			if text, ok := jsonStringField(nested, "message"); ok {
				return text
			}
		}
	}
	if text, ok := jsonStringField(value, "message"); ok {
		return text
	}
	return fallback
}

// hasExplicitProviderError reports whether the top-level value carries the
// Anthropic error-event nested error object (messages.rs:1020-1022).
func hasExplicitProviderError(value map[string]json.RawMessage) bool {
	raw, ok := value["error"]
	if !ok {
		return false
	}
	var nested map[string]json.RawMessage
	return json.Unmarshal(raw, &nested) == nil && nested != nil
}

// transportError converts a transport-layer failure into a neutral transport
// error without echoing authenticated URLs, mirroring transport_error
// (core/provider/src/http.rs:337-376): timeouts, connection failures, and
// request-stage failures are retryable, cancellation is preserved as such.
func transportError(apiKey string, err error) *model.ModelError {
	if errors.Is(err, context.Canceled) {
		return model.CancelledError("模型调用已取消")
	}
	category := "request"
	var urlError *url.Error
	if errors.As(err, &urlError) {
		err = urlError.Err
	}
	var netError net.Error
	if errors.As(err, &netError) && netError.Timeout() {
		category = "timeout"
	} else {
		var opError *net.OpError
		if errors.As(err, &opError) {
			category = "connect"
		}
	}
	message := safeErrorMessage(apiKey, "["+category+"] "+err.Error())
	return &model.ModelError{Kind: model.ErrorTransport, Message: message, Retryable: true}
}

// safeErrorMessage removes credentials, control characters, and length excess
// from error display text, mirroring safe_error_message
// (core/provider/src/http.rs:683-708). The caller's API key, when non-empty,
// is replaced by exact match first: the generic field-boundary redaction can
// otherwise split credentials containing separators such as "secret,foo" and
// leave an unredacted remainder behind.
func safeErrorMessage(apiKey, message string) string {
	redacted := message
	if apiKey != "" {
		redacted = redactAPIKeyBounded(apiKey, redacted, maxErrorInputBytes)
	}
	redacted = model.RedactErrorSecretsBounded(redacted, maxErrorInputBytes)
	var builder strings.Builder
	characters := 0
	for _, character := range redacted {
		if characters >= maxErrorMessageChars {
			break
		}
		if unicode.IsControl(character) {
			character = ' '
		}
		builder.WriteRune(character)
		characters++
	}
	safe := builder.String()
	if strings.TrimSpace(safe) == "" {
		return emptyServerErrorMessage
	}
	return safe
}

// redactAPIKeyBounded removes every exact occurrence of the provider key
// inside the bounded input window and independently bounds the rebuilt
// output, mirroring redact_api_key_bounded
// (core/provider/src/http.rs:715-749). The search window reads one key length
// past the retained boundary so a match starting before and ending after the
// boundary is still recognized; replacements never move later scan cursors,
// so earlier long keys cannot leave an unrecognized key prefix at the tail.
func redactAPIKeyBounded(apiKey, input string, maximumBytes int) string {
	if maximumBytes == 0 || input == "" || apiKey == "" {
		return ""
	}
	retainedEnd := len(boundedUTF8Prefix(input, maximumBytes))
	searchEnd := len(boundedUTF8Prefix(input, retainedEnd+len(apiKey)))
	var output strings.Builder
	cursor := 0
	for matchStart := strings.Index(input[:searchEnd], apiKey); matchStart >= 0; {
		if matchStart >= retainedEnd {
			break
		}
		unmatched := input[cursor:matchStart]
		remaining := maximumBytes - output.Len()
		bounded := boundedUTF8Prefix(unmatched, remaining)
		output.WriteString(bounded)
		if len(bounded) != len(unmatched) || len(model.RedactedSecret) > remaining-len(bounded) {
			return output.String()
		}
		output.WriteString(model.RedactedSecret)
		cursor = matchStart + len(apiKey)
		if cursor >= retainedEnd {
			return output.String()
		}
		next := strings.Index(input[cursor:searchEnd], apiKey)
		if next < 0 {
			break
		}
		matchStart = cursor + next
	}
	remaining := maximumBytes - output.Len()
	output.WriteString(boundedUTF8Prefix(input[cursor:retainedEnd], remaining))
	return output.String()
}

// boundedUTF8Prefix borrows the prefix of value limited to maximumBytes
// without cutting inside a rune (http.rs:752-761).
func boundedUTF8Prefix(value string, maximumBytes int) string {
	if len(value) <= maximumBytes {
		return value
	}
	end := maximumBytes
	for end > 0 && !isRuneStart(value[end]) {
		end--
	}
	return value[:end]
}

// isRuneStart reports whether the byte at value[index] starts a UTF-8 rune.
func isRuneStart(b byte) bool { return b&0xC0 != 0x80 }
