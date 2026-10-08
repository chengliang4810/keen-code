package openai

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/url"
	"strings"
	"unicode"
	"unicode/utf8"

	"keencode/internal/model"
)

// Limits of raw error text before classification and display
// (core/provider/src/http.rs:21-23).
const (
	// maxErrorInputBytes bounds the raw UTF-8 prefix entering classification.
	maxErrorInputBytes = 64 * 1024
	// maxErrorMessageChars bounds the final display message in runes.
	maxErrorMessageChars = 1000
	// maxAPIKeyBytes bounds the configured credential size
	// (core/provider/src/config.rs:16).
	maxAPIKeyBytes = 16 * 1024
)

// emptyErrorMessage replaces error bodies that sanitize to whitespace.
const emptyErrorMessage = "模型服务返回空错误"

// providerErrorFields extracts the message and error code from a common
// error JSON body or falls back to the raw text
// (core/provider/src/http.rs:641-669).
func providerErrorFields(body []byte) (message, code string, codeOK bool) {
	var value any
	if err := json.Unmarshal(body, &value); err == nil {
		if object, ok := value.(map[string]any); ok {
			errorObject, hasErrorObject := asObject(object["error"])
			messageSource := object
			codeSource := object
			if hasErrorObject {
				// When an error member exists, the message chain consults it
				// first and the code only comes from it; non-object error
				// values contribute neither.
				messageSource = errorObject
				codeSource = errorObject
			}
			message = firstString(messageSource, "message", "msg")
			if message == "" {
				message = firstString(object, "message", "msg")
			}
			if message == "" {
				message = "模型服务返回未说明错误"
			}
			if codeValue, present := codeSource["code"]; present {
				if text, ok := asString(codeValue); ok {
					return message, text, true
				}
				if encoded, err := json.Marshal(codeValue); err == nil {
					return message, string(encoded), true
				}
			}
			return message, "", false
		}
	}
	// Not JSON: keep a lossy UTF-8 rendering of the body without a code.
	return strings.ToValidUTF8(string(body), "\uFFFD"), "", false
}

// firstString returns the first present string field among names.
func firstString(object map[string]any, names ...string) string {
	for _, name := range names {
		if text, ok := asString(object[name]); ok {
			return text
		}
	}
	return ""
}

// classifyHTTPError normalizes a non-success HTTP response by status, public
// error code, and bounded raw text (core/provider/src/http.rs:109-266).
// retryAfterMS is 0 when absent and code empty when the endpoint reports
// none. The message is sanitized before storage so credentials never reach
// the unified error.
func classifyHTTPError(status int, retryAfterMS int64, message, code, apiKey string) *model.ModelError {
	classifierMessage := boundedUTF8Prefix(message, maxErrorInputBytes)
	classifierCode := boundedUTF8Prefix(code, maxErrorInputBytes)
	classifier := strings.ToLower(classifierCode + " " + classifierMessage)
	message = safeErrorMessage(apiKey, message)

	switch {
	case containsAny(classifier,
		"context_length", "context length", "context window", "maximum context",
		"max context", "prompt is too long", "input is too long", "too many tokens") ||
		(strings.Contains(classifier, "input") && strings.Contains(classifier, "token") &&
			(strings.Contains(classifier, "exceed") || strings.Contains(classifier, "maximum"))) ||
		(strings.Contains(classifier, "input") && strings.Contains(classifier, "context") &&
			(strings.Contains(classifier, "exceed") || strings.Contains(classifier, "maximum") ||
				strings.Contains(classifier, "too_long") || strings.Contains(classifier, "too long"))) ||
		(strings.Contains(classifier, "上下文") && strings.Contains(classifier, "超")):
		return &model.ModelError{Kind: model.ErrorContextLengthExceeded, Message: message}
	case containsAny(classifier, "quota_exhausted", "insufficient_balance", "insufficient_quota",
		"余额不足", "套餐次数已用尽") || status == 402:
		return &model.ModelError{Kind: model.ErrorQuotaExceeded, Message: message, StatusCode: status}
	case containsAny(classifier, "invalid_api_key", "authentication_error", "unauthorized",
		"authentication failed", "认证失败"):
		return &model.ModelError{Kind: model.ErrorAuthentication, Message: message, StatusCode: status}
	case containsAny(classifier, "permission_denied", "forbidden", "not authorized",
		"authorization failed", "无权", "未授权"):
		return &model.ModelError{Kind: model.ErrorAuthorization, Message: message, StatusCode: status}
	case containsAny(classifier, "rate_limit", "rate limited", "too many requests", "throttled",
		"请求过于频繁"):
		return &model.ModelError{Kind: model.ErrorRateLimited, Message: message, StatusCode: status, RetryAfterMS: retryAfterMS}
	case containsAny(classifier, "model_not_found", "unsupported_model") ||
		(strings.Contains(classifier, "model") && strings.Contains(classifier, "not supported")) ||
		(strings.Contains(classifier, "模型") && strings.Contains(classifier, "不支持")):
		return &model.ModelError{Kind: model.ErrorModelNotFound, Message: message, StatusCode: status}
	case containsAny(classifier, "service_unavailable", "server_error", "temporarily unavailable",
		"overloaded", "服务不可用", "过载"):
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: message, StatusCode: status, Retryable: true}
	}

	switch status {
	case 401:
		return &model.ModelError{Kind: model.ErrorAuthentication, Message: message, StatusCode: status}
	case 403:
		return &model.ModelError{Kind: model.ErrorAuthorization, Message: message, StatusCode: status}
	case 404, 405:
		return &model.ModelError{Kind: model.ErrorProtocolUnsupported, Message: message, StatusCode: status}
	case 425, 429:
		return &model.ModelError{Kind: model.ErrorRateLimited, Message: message, StatusCode: status, RetryAfterMS: retryAfterMS}
	case 408:
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: message, StatusCode: status, Retryable: true}
	case 400, 409, 422:
		// A rejected output limit is a de-escalatable retry signal: mark it
		// structurally so the agent loop can retry once without the limit
		// instead of string matching vendor text (http.rs:237-248).
		if containsAny(classifier, "max_tokens", "max_output_tokens", "max_completion_tokens") {
			return &model.ModelError{Kind: model.ErrorOutputLimitRejected, Message: message}
		}
		return &model.ModelError{Kind: model.ErrorInvalidRequest, Message: message}
	case 500, 502, 503, 504, 520, 521, 522, 523, 524, 527, 529:
		// Origin or gateway outages where retrying pays off; 525/526 describe
		// TLS handshake and certificate failures and stay non-retryable
		// (http.rs:249-259).
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: message, StatusCode: status, Retryable: true}
	default:
		return &model.ModelError{Kind: model.ErrorProviderUnavailable, Message: message, StatusCode: status}
	}
}

// classifyInBandProviderError normalizes an error object carried inside an
// HTTP 200 body (core/provider/src/http.rs:272-288; wire.rs:59-92). Only
// errors with clear structural evidence upgrade beyond protocol class, and
// the fictional status of the synthetic classification is stripped.
func classifyInBandProviderError(value map[string]any, fallback string) *model.ModelError {
	message := fallback
	code := ""
	if errorObject, ok := asObject(value["error"]); ok {
		if text, ok := asString(errorObject["message"]); ok {
			message = text
		}
		codeField, present := errorObject["code"]
		if !present {
			codeField, present = errorObject["type"]
		}
		if present {
			if text, ok := asString(codeField); ok {
				code = text
			} else if encoded, err := json.Marshal(codeField); err == nil {
				code = string(encoded)
			}
		}
	} else {
		if text, ok := asString(value["message"]); ok {
			message = text
		}
		if text, ok := asString(value["code"]); ok {
			code = text
		}
	}

	classified := classifyHTTPError(400, 0, message, code, "")
	switch classified.Kind {
	case model.ErrorContextLengthExceeded,
		model.ErrorAuthentication,
		model.ErrorAuthorization,
		model.ErrorQuotaExceeded,
		model.ErrorModelNotFound,
		model.ErrorProtocolUnsupported,
		model.ErrorRateLimited,
		model.ErrorProviderUnavailable:
		classified.StatusCode = 0
		if classified.Kind == model.ErrorRateLimited {
			classified.RetryAfterMS = 0
		}
		return classified
	default:
		// Unknown or ordinary invalid_request codes stay protocol errors;
		// without an HTTP status the layer must not guess at credentials,
		// quota, or transience (http.rs:268-272).
		return &model.ModelError{Kind: model.ErrorProtocol, Message: classified.Message}
	}
}

// transportError converts a Go transport failure into a retryable unified
// error without credential or URL exposure
// (core/provider/src/http.rs:337-376). Cancellation keeps its own kind.
func transportError(err error) *model.ModelError {
	if errors.Is(err, context.Canceled) {
		return model.CancelledError("模型调用已取消")
	}
	category := "request"
	var netErr net.Error
	if errors.As(err, &netErr) && netErr.Timeout() {
		category = "timeout"
	}
	// Strip the URL: reqwest calls without_url for the same reason — the
	// endpoint may embed credentials in exotic setups and never belongs in
	// user-facing diagnostics.
	var urlErr *url.Error
	if errors.As(err, &urlErr) && urlErr.Err != nil {
		err = urlErr.Err
	}
	return &model.ModelError{
		Kind:      model.ErrorTransport,
		Message:   fmt.Sprintf("[%s] %v", category, err),
		Retryable: true,
	}
}

// safeErrorMessage removes credentials, control characters, and excess
// length from provider-provided error text
// (core/provider/src/http.rs:688-711). The exact credential is replaced
// first so a key containing separators survives the structural pass.
func safeErrorMessage(apiKey, message string) string {
	if apiKey != "" {
		message = strings.ReplaceAll(message, apiKey, model.RedactedSecret)
	}
	message = model.RedactErrorSecretsBounded(message, maxErrorInputBytes)
	var bounded strings.Builder
	runes := 0
	for _, r := range message {
		if runes >= maxErrorMessageChars {
			break
		}
		if isControlRune(r) {
			r = ' '
		}
		bounded.WriteRune(r)
		runes++
	}
	safe := bounded.String()
	if strings.TrimSpace(safe) == "" {
		return emptyErrorMessage
	}
	return safe
}

// boundedUTF8Prefix limits text to maximumBytes without splitting a rune
// (core/provider/src/http.rs:764-773).
func boundedUTF8Prefix(value string, maximumBytes int) string {
	if len(value) <= maximumBytes {
		return value
	}
	end := maximumBytes
	for end > 0 && !utf8.RuneStart(value[end]) {
		end--
	}
	return value[:end]
}

// containsAny reports whether the haystack contains any needle.
func containsAny(haystack string, needles ...string) bool {
	for _, needle := range needles {
		if strings.Contains(haystack, needle) {
			return true
		}
	}
	return false
}

// isControlRune reports Unicode control characters (the sanitizer's
// replacement class).
func isControlRune(r rune) bool {
	return unicode.IsControl(r)
}
