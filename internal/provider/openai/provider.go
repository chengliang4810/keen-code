package openai

import (
	"bytes"
	"context"
	"errors"
	"io"
	"math"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	"keencode/internal/model"
)

// Byte limits and defaults mirroring core/provider/src/config.rs:323-326.
const (
	// DefaultMaxEventBytes bounds one SSE event or JSON error body.
	DefaultMaxEventBytes = 16 << 20
	// DefaultMaxResponseBytes bounds one whole buffered response.
	DefaultMaxResponseBytes = 64 << 20
	// defaultResponseHeaderTimeout bounds waiting for response headers; the
	// body itself streams without a global deadline because long generations
	// are legitimate (config.rs DEFAULT_STREAM_IDLE_TIMEOUT_MS magnitude).
	defaultResponseHeaderTimeout = 90 * time.Second
	// eventChannelBuffer smooths delivery between the network read loop and
	// the consumer while preserving backpressure.
	eventChannelBuffer = 16
	// readChunkSize is the HTTP body read buffer of the streaming loop.
	readChunkSize = 32 * 1024
)

// Output budget field names of Chat-compatible gateways
// (core/provider/src/config.rs:194-216). The choice is explicit and never
// guessed from vendor names; both fields are never sent together.
const (
	// OutputTokenFieldMaxCompletionTokens is the standard Chat budget
	// including provider-accounted reasoning tokens (the default).
	OutputTokenFieldMaxCompletionTokens = "max_completion_tokens"
	// OutputTokenFieldMaxTokens is the legacy budget field some compatible
	// gateways only recognize.
	OutputTokenFieldMaxTokens = "max_tokens"
)

// Options configures one OpenAI Chat Completions adapter instance.
type Options struct {
	// Endpoint is the complete Chat Completions request URL, already
	// resolved by configuration (the `#` full-path marker semantics of
	// docs/go-migration.md §5.6 are resolved before construction).
	Endpoint string
	// APIKey is the bearer credential; empty means an explicitly anonymous
	// endpoint such as a local gateway.
	APIKey string
	// OutputTokenField selects the output budget wire field; empty uses
	// OutputTokenFieldMaxCompletionTokens.
	OutputTokenField string
	// HTTPClient replaces the default client; nil installs a client with a
	// 90s response-header timeout and no global body deadline.
	HTTPClient *http.Client
	// MaxEventBytes bounds one SSE event (and one HTTP error body); 0 uses
	// DefaultMaxEventBytes.
	MaxEventBytes int
	// MaxResponseBytes bounds one buffered response body; 0 uses
	// DefaultMaxResponseBytes and must not be smaller than MaxEventBytes.
	MaxResponseBytes int
	// Capabilities replaces the reported capability snapshot when non-zero.
	// The v1 adapter has no model catalog, so it reports the four effort
	// levels the unified layer accepts and unknown context windows.
	Capabilities model.Capabilities
}

// Provider is the OpenAI Chat Completions protocol adapter implementing
// model.Provider (docs/go-migration.md §5.2). One instance serves any number
// of sequential or concurrent streams; per-response state lives in the
// stream adapters.
type Provider struct {
	endpoint         string
	apiKey           string
	outputTokenField string
	httpClient       *http.Client
	maxEventBytes    int
	maxResponseBytes int
	capabilities     model.Capabilities
}

// defaultCapabilities is the capability snapshot reported for every model:
// reasoning pass-through with the unified effort levels and unknown windows.
func defaultCapabilities() model.Capabilities {
	return model.Capabilities{
		Reasoning:        true,
		ReasoningEfforts: []string{model.ReasoningEffortMinimal, model.ReasoningEffortLow, model.ReasoningEffortMedium, model.ReasoningEffortHigh},
	}
}

// New validates the options and constructs the adapter
// (core/provider/src/config.rs validate rules reduced to the v1 fields).
func New(options Options) (*Provider, error) {
	parsed, err := url.Parse(options.Endpoint)
	if err != nil || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.Host == "" {
		return nil, model.InvalidRequest("OpenAI 端点地址必须是有效的 http(s) URL")
	}
	if options.APIKey != "" {
		if len(options.APIKey) > maxAPIKeyBytes {
			return nil, model.InvalidRequest("API Key 长度超过 %d 字节上限", maxAPIKeyBytes)
		}
		for _, r := range options.APIKey {
			if isControlRune(r) {
				return nil, model.InvalidRequest("API Key 不能包含控制字符")
			}
		}
	}
	outputTokenField := OutputTokenFieldMaxCompletionTokens
	if options.OutputTokenField != "" {
		switch options.OutputTokenField {
		case OutputTokenFieldMaxCompletionTokens, OutputTokenFieldMaxTokens:
			outputTokenField = options.OutputTokenField
		default:
			return nil, model.InvalidRequest("输出预算字段 %q 不受支持", options.OutputTokenField)
		}
	}
	maxEventBytes := options.MaxEventBytes
	if maxEventBytes == 0 {
		maxEventBytes = DefaultMaxEventBytes
	}
	maxResponseBytes := options.MaxResponseBytes
	if maxResponseBytes == 0 {
		maxResponseBytes = DefaultMaxResponseBytes
	}
	if maxResponseBytes < maxEventBytes {
		return nil, model.InvalidRequest("累计响应上限不能小于单事件上限")
	}
	httpClient := options.HTTPClient
	if httpClient == nil {
		transport := http.DefaultTransport.(*http.Transport).Clone()
		transport.ResponseHeaderTimeout = defaultResponseHeaderTimeout
		httpClient = &http.Client{Transport: transport}
	}
	capabilities := defaultCapabilities()
	if options.Capabilities.Reasoning ||
		len(options.Capabilities.ReasoningEfforts) > 0 ||
		options.Capabilities.ContextWindow != 0 || options.Capabilities.MaxOutputTokens != 0 {
		capabilities = options.Capabilities
	}
	return &Provider{
		endpoint:         options.Endpoint,
		apiKey:           options.APIKey,
		outputTokenField: outputTokenField,
		httpClient:       httpClient,
		maxEventBytes:    maxEventBytes,
		maxResponseBytes: maxResponseBytes,
		capabilities:     capabilities,
	}, nil
}

// Capabilities implements model.Provider. The v1 adapter has no catalog, so
// every model receives the instance-wide snapshot.
func (p *Provider) Capabilities(modelName string) model.Capabilities {
	return p.capabilities
}

// Stream implements model.Provider. Failures before a success response
// (validation, encoding, transport, HTTP error status) return a *model.ModelError
// immediately; after a 2xx response the event channel is returned and every
// later failure — protocol, in-band, cancellation, or a cut stream — is
// delivered as one EventError followed by channel close.
func (p *Provider) Stream(ctx context.Context, req model.ModelRequest) (<-chan model.StreamEvent, error) {
	if err := req.Validate(); err != nil {
		return nil, err
	}
	body, err := encodeRequest(req, true, p.outputTokenField)
	if err != nil {
		return nil, err
	}
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, p.endpoint, bytes.NewReader(body))
	if err != nil {
		return nil, model.InvalidRequest("OpenAI 请求构造失败：%v", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "application/json, text/event-stream")
	if p.apiKey != "" {
		// Control characters were rejected at construction, so the header
		// value cannot panic on write (client.rs authenticated_request).
		httpReq.Header.Set("Authorization", "Bearer "+p.apiKey)
	}
	resp, err := p.httpClient.Do(httpReq)
	if err != nil {
		return nil, p.sanitizeError(transportError(err))
	}
	// A non-success status never reaches the event channel: like the Rust
	// attempt boundary (client.rs perform_attempt), the response is read and
	// classified here and Stream fails immediately.
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		defer resp.Body.Close()
		retryAfterMS := parseRetryAfter(resp.Header.Get("Retry-After"))
		errorBody, err := readLimited(resp.Body, p.maxEventBytes)
		if err != nil {
			return nil, p.sanitizeError(err)
		}
		message, code, _ := providerErrorFields(errorBody)
		return nil, p.sanitizeError(classifyHTTPError(resp.StatusCode, retryAfterMS, message, code, p.apiKey))
	}
	events := make(chan model.StreamEvent, eventChannelBuffer)
	go p.deliver(ctx, resp, events)
	return events, nil
}

// deliver owns resp: it normalizes a 2xx response into events on the
// channel, closes the channel after the terminal event or an error, and
// always closes the body.
func (p *Provider) deliver(ctx context.Context, resp *http.Response, events chan<- model.StreamEvent) {
	defer close(events)
	defer resp.Body.Close()

	contentType := strings.ToLower(resp.Header.Get("Content-Type"))
	if strings.Contains(contentType, "text/event-stream") {
		p.deliverSSE(ctx, resp.Body, events)
		return
	}
	p.deliverBuffered(ctx, resp.Body, events)
}

// deliverSSE streams the body through the incremental decoder and adapter,
// emitting events as they arrive (core/provider/src/http.rs:497-572).
func (p *Provider) deliverSSE(ctx context.Context, body io.Reader, events chan<- model.StreamEvent) {
	decoder := newSSEDecoder(p.maxEventBytes)
	adapter := newStreamAdapter()
	buffer := make([]byte, readChunkSize)
	var wireBytes int
	var pending []model.StreamEvent

	for {
		select {
		case <-ctx.Done():
			p.emitError(ctx, events, model.CancelledError(ctx.Err().Error()))
			return
		default:
		}
		n, readErr := body.Read(buffer)
		if n > 0 {
			wireBytes += n
			if wireBytes > p.maxResponseBytes {
				p.emitError(ctx, events, protocolError("模型流式响应超过 %d 字节安全上限", p.maxResponseBytes))
				return
			}
			frames, err := decoder.push(buffer[:n])
			if err == nil {
				for _, frame := range frames {
					pending = pending[:0]
					if err = adapter.consumeSSE(frame, &pending); err != nil {
						break
					}
					if !p.emitAll(ctx, events, pending) {
						return
					}
				}
			}
			if err != nil {
				p.emitError(ctx, events, err)
				return
			}
		}
		if readErr == nil {
			continue
		}
		if errors.Is(readErr, io.EOF) {
			if !p.finishSSE(ctx, decoder, adapter, events) {
				return
			}
			return
		}
		if ctxErr := ctx.Err(); ctxErr != nil {
			// Cancellation and deadlines surface as wrapped read errors;
			// report them with the context's own reason.
			p.emitError(ctx, events, model.CancelledError(ctxErr.Error()))
			return
		}
		p.emitError(ctx, events, p.sanitizeError(transportError(readErr)))
		return
	}
}

// finishSSE flushes the decoder tail and the adapter terminal state at EOF.
// It reports whether delivery continued (false means the consumer cancelled).
func (p *Provider) finishSSE(ctx context.Context, decoder *sseDecoder, adapter *streamAdapter, events chan<- model.StreamEvent) bool {
	frames, err := decoder.finish()
	if err != nil {
		p.emitError(ctx, events, err)
		return false
	}
	var pending []model.StreamEvent
	for _, frame := range frames {
		pending = pending[:0]
		if err = adapter.consumeSSE(frame, &pending); err != nil {
			p.emitError(ctx, events, err)
			return false
		}
		if !p.emitAll(ctx, events, pending) {
			return false
		}
	}
	pending = pending[:0]
	if err = adapter.finishStream(&pending); err != nil {
		p.emitError(ctx, events, err)
		return false
	}
	p.emitAll(ctx, events, pending)
	return true
}

// deliverBuffered handles success responses that are not declared SSE: a
// body that still looks like SSE is decoded frame by frame, anything else is
// one JSON response (core/provider/src/http.rs:25-67, 574-590).
func (p *Provider) deliverBuffered(ctx context.Context, body io.Reader, events chan<- model.StreamEvent) {
	data, err := readLimited(body, p.maxResponseBytes)
	if err != nil {
		p.emitError(ctx, events, err)
		return
	}
	adapter := newStreamAdapter()
	var pending []model.StreamEvent
	if looksLikeSSE(data) {
		decoder := newSSEDecoder(p.maxEventBytes)
		frames, err := decoder.push(data)
		if err != nil {
			p.emitError(ctx, events, err)
			return
		}
		for _, frame := range frames {
			pending = pending[:0]
			if err = adapter.consumeSSE(frame, &pending); err != nil {
				p.emitError(ctx, events, err)
				return
			}
			if !p.emitAll(ctx, events, pending) {
				return
			}
		}
		if !p.finishSSE(ctx, decoder, adapter, events) {
			return
		}
		return
	}
	if err := adapter.decodeJSON(data, &pending); err != nil {
		// Deliver the events produced before the failure, then the failure,
		// mirroring the streaming path where earlier events already flowed.
		p.emitAll(ctx, events, pending)
		p.emitError(ctx, events, err)
		return
	}
	p.emitAll(ctx, events, pending)
}

// looksLikeSSE reports whether a buffered body opens with an SSE field
// boundary (core/provider/src/http.rs:601-604).
func looksLikeSSE(body []byte) bool {
	if len(body) >= len(utf8BOM) && string(body[:len(utf8BOM)]) == string(utf8BOM) {
		body = body[len(utf8BOM):]
	}
	return bytes.HasPrefix(body, []byte("data:")) ||
		bytes.HasPrefix(body, []byte("event:")) ||
		bytes.HasPrefix(body, []byte(":"))
}

// readLimited reads the whole body within the byte budget
// (core/provider/src/http.rs:619-660). Transport failures keep their class;
// exceeding the budget is a protocol failure because the endpoint ignored
// the size contract.
func readLimited(body io.Reader, maxBytes int) ([]byte, *model.ModelError) {
	var buffer bytes.Buffer
	if _, err := io.Copy(&buffer, io.LimitReader(body, int64(maxBytes)+1)); err != nil {
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			return nil, model.CancelledError(context.Canceled.Error())
		}
		return nil, transportError(err)
	}
	if buffer.Len() > maxBytes {
		return nil, protocolError("模型 HTTP 响应超过 %d 字节安全上限", maxBytes)
	}
	return buffer.Bytes(), nil
}

// parseRetryAfter converts an integer-seconds Retry-After header into
// milliseconds; HTTP-date forms are ignored like the Rust reference
// (core/provider/src/http.rs:77-82).
func parseRetryAfter(value string) int64 {
	seconds, err := strconv.ParseInt(strings.TrimSpace(value), 10, 64)
	if err != nil || seconds < 0 || seconds > math.MaxInt64/1000 {
		return 0
	}
	return seconds * 1000
}

// emitAll forwards events to the channel; false means the consumer context
// ended and delivery stops without further events.
func (p *Provider) emitAll(ctx context.Context, events chan<- model.StreamEvent, list []model.StreamEvent) bool {
	for _, event := range list {
		select {
		case events <- event:
		case <-ctx.Done():
			return false
		}
	}
	return true
}

// emitError sanitizes and forwards one terminal error event.
func (p *Provider) emitError(ctx context.Context, events chan<- model.StreamEvent, err error) {
	modelErr, ok := err.(*model.ModelError)
	if !ok {
		modelErr = &model.ModelError{Kind: model.ErrorProtocol, Message: err.Error()}
	}
	p.emitOne(ctx, events, model.StreamEvent{Type: model.EventError, Err: p.sanitizeError(modelErr)})
}

// emitOne forwards one event, preferring delivery over cancellation but
// never blocking a cancelled consumer indefinitely.
func (p *Provider) emitOne(ctx context.Context, events chan<- model.StreamEvent, event model.StreamEvent) bool {
	select {
	case events <- event:
		return true
	case <-ctx.Done():
		// Best effort: still try once to hand over the event so a consumer
		// draining after cancellation sees the terminal error.
		select {
		case events <- event:
			return true
		default:
			return false
		}
	}
}

// sanitizeError rewrites the message of a freshly built model error with the
// credential-aware sanitizer (core/provider/src/http.rs redact_model_error).
func (p *Provider) sanitizeError(err *model.ModelError) *model.ModelError {
	if err == nil {
		return nil
	}
	err.Message = safeErrorMessage(p.apiKey, err.Message)
	return err
}
