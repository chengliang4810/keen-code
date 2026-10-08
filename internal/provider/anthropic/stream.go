package anthropic

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"

	"keencode/internal/model"
)

// Default byte ceilings mirroring the Rust provider config defaults
// (core/provider/src/config.rs:325-326).
const (
	// DefaultMaxEventBytes caps one SSE event at 16 MiB.
	DefaultMaxEventBytes = 16 * 1024 * 1024
	// DefaultMaxResponseBytes caps one HTTP response body at 64 MiB.
	DefaultMaxResponseBytes = 64 * 1024 * 1024
)

// DefaultCapabilities is the capability snapshot used when Options does not
// override one: Anthropic thinking is requested through the thinking budget,
// with the four neutral effort levels mapped onto it. Context and output
// windows stay unknown (0) until a model catalog exists.
func DefaultCapabilities() model.Capabilities {
	return model.Capabilities{
		Reasoning:        true,
		ReasoningEfforts: []string{model.ReasoningEffortMinimal, model.ReasoningEffortLow, model.ReasoningEffortMedium, model.ReasoningEffortHigh},
	}
}

// Options configures one Anthropic Messages adapter.
type Options struct {
	// BaseURL is the provider base address ending in /v1 (or an exact
	// endpoint path). When the path already ends with "/messages" it is used
	// verbatim; otherwise the "messages" resource is joined like the Rust
	// base_url.join("messages") (core/provider/src/config.rs:508-531).
	BaseURL string
	// APIKey is the credential sent as the x-api-key header; empty sends an
	// anonymous request (client.rs:580-582).
	APIKey string
	// PromptCaching attaches Anthropic ephemeral cache breakpoints to the
	// request ladder (messages.rs:79-81). Off by default; the session-level
	// assembly enables it once capability snapshots declare it.
	PromptCaching bool
	// Capabilities overrides DefaultCapabilities.
	Capabilities *model.Capabilities
	// HTTPClient overrides the default client; tests inject httptest
	// clients here. The client must not follow redirects.
	HTTPClient *http.Client
	// MaxEventBytes overrides DefaultMaxEventBytes.
	MaxEventBytes int
	// MaxResponseBytes overrides DefaultMaxResponseBytes.
	MaxResponseBytes int
}

// Adapter implements the provider-neutral model.Provider interface for the
// Anthropic Messages protocol (HTTP plus SSE). It holds only immutable
// configuration; all per-request state lives in streamAdapter.
type Adapter struct {
	baseURL          string
	apiKey           string
	promptCaching    bool
	capabilities     model.Capabilities
	client           *http.Client
	maxEventBytes    int
	maxResponseBytes int
}

// compile-time shape assertion against the neutral provider interface: the
// internal/provider package (constructed via provider.New) hands adapters to
// callers as model.Provider.
var _ interface {
	Capabilities(modelName string) model.Capabilities
	Stream(ctx context.Context, req model.ModelRequest) (<-chan model.StreamEvent, error)
} = (*Adapter)(nil)

// New validates the options and returns an adapter.
func New(opts Options) (*Adapter, error) {
	baseURL, err := normalizeBaseURL(opts.BaseURL)
	if err != nil {
		return nil, err
	}
	if opts.APIKey != "" && !isHeaderValueSafe(opts.APIKey) {
		return nil, invalidRequest("API Key 包含不能用于 HTTP Header 的字符")
	}
	client := opts.HTTPClient
	if client == nil {
		transport := http.DefaultTransport.(*http.Transport).Clone()
		transport.Proxy = http.ProxyFromEnvironment
		client = &http.Client{
			Transport: transport,
			// Redirects are never followed: a provider redirect is surfaced
			// as a classified error response instead of silently retargeting
			// credentials (client.rs:498 Policy::none).
			CheckRedirect: func(*http.Request, []*http.Request) error {
				return http.ErrUseLastResponse
			},
		}
	}
	maxEventBytes := opts.MaxEventBytes
	if maxEventBytes <= 0 {
		maxEventBytes = DefaultMaxEventBytes
	}
	maxResponseBytes := opts.MaxResponseBytes
	if maxResponseBytes <= 0 {
		maxResponseBytes = DefaultMaxResponseBytes
	}
	capabilities := DefaultCapabilities()
	if opts.Capabilities != nil {
		capabilities = *opts.Capabilities
	}
	return &Adapter{
		baseURL:          baseURL,
		apiKey:           opts.APIKey,
		promptCaching:    opts.PromptCaching,
		capabilities:     capabilities,
		client:           client,
		maxEventBytes:    maxEventBytes,
		maxResponseBytes: maxResponseBytes,
	}, nil
}

// Capabilities implements the neutral provider boundary.
func (a *Adapter) Capabilities(modelName string) model.Capabilities {
	_ = modelName
	return a.capabilities
}

// Stream validates the request and starts one streaming Messages call,
// implementing the neutral provider boundary. A failed validation returns an
// error immediately; on success the event channel closes after
// EventMessageEnd or EventError. Cancelling ctx terminates the HTTP
// connection, emits one EventError, and closes the channel.
func (a *Adapter) Stream(ctx context.Context, req model.ModelRequest) (<-chan model.StreamEvent, error) {
	body, err := a.encodeRequest(&req, true)
	if err != nil {
		return nil, err
	}
	httpRequest, err := a.newRequest(ctx, body)
	if err != nil {
		return nil, err
	}
	response, err := a.client.Do(httpRequest)
	if err != nil {
		return nil, transportError(a.apiKey, err)
	}
	if response.StatusCode < 200 || response.StatusCode > 299 {
		defer response.Body.Close()
		return nil, a.classifyErrorResponse(response)
	}
	events := make(chan model.StreamEvent)
	go a.deliver(ctx, response, events)
	return events, nil
}

// normalizeBaseURL validates the base address and keeps an exact /messages
// endpoint verbatim.
func normalizeBaseURL(baseURL string) (string, error) {
	trimmed := strings.TrimSpace(baseURL)
	if trimmed == "" {
		return "", invalidRequest("Messages Provider 地址不能为空")
	}
	parsed, err := url.Parse(trimmed)
	if err != nil || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.Host == "" {
		return "", invalidRequest("Messages Provider 地址必须是合法的 http/https 地址")
	}
	return trimmed, nil
}

// isHeaderValueSafe reports whether the credential can travel in an HTTP
// header: visible ASCII only (client.rs:583-591).
func isHeaderValueSafe(value string) bool {
	for i := 0; i < len(value); i++ {
		b := value[i]
		if b <= 0x20 || b >= 0x7F {
			return false
		}
	}
	return true
}

// endpointURL joins the messages resource with the base address.
func (a *Adapter) endpointURL() string {
	if strings.HasSuffix(a.baseURL, "/messages") {
		return a.baseURL
	}
	if !strings.HasSuffix(a.baseURL, "/") {
		return a.baseURL + "/messages"
	}
	return a.baseURL + "messages"
}

// newRequest builds the authenticated POST request: JSON body, the dual
// accept media types, the required anthropic-version header, and the
// x-api-key credential when configured (client.rs:566-599).
func (a *Adapter) newRequest(ctx context.Context, body json.RawMessage) (*http.Request, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, a.endpointURL(), bytes.NewReader(body))
	if err != nil {
		return nil, invalidRequest("Messages 请求地址不合法：%v", err)
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Accept", "application/json, text/event-stream")
	request.Header.Set("anthropic-version", anthropicVersion)
	if a.apiKey != "" {
		request.Header.Set("x-api-key", a.apiKey)
	}
	return request, nil
}

// classifyErrorResponse reads a non-2xx body within limits and normalizes it
// by status, code, and text (core/provider/src/http.rs:70-96).
func (a *Adapter) classifyErrorResponse(response *http.Response) error {
	retryAfterMS, hasRetryAfter := int64(0), false
	if raw := response.Header.Get("Retry-After"); raw != "" {
		if seconds, err := strconv.ParseUint(strings.TrimSpace(raw), 10, 63); err == nil {
			retryAfterMS = int64(seconds) * 1000
			hasRetryAfter = true
		}
	}
	body, err := readLimited(response.Body, a.maxResponseBytes)
	if err != nil {
		return err
	}
	message, code, _ := providerErrorFields(body)
	return classifyHTTPError(a.apiKey, response.StatusCode, retryAfterMS, hasRetryAfter, message, code)
}

// deliver pumps the success response into the event channel and closes it.
// It is the Go counterpart of stream_sse / decode_success_response
// (core/provider/src/http.rs:26-67, 497-572).
func (a *Adapter) deliver(ctx context.Context, response *http.Response, events chan<- model.StreamEvent) {
	defer close(events)
	defer response.Body.Close()

	send := func(event model.StreamEvent) bool {
		// A blocking send is safe under the provider contract: consumers
		// drain the channel after cancelling so a well-behaved adapter is
		// never blocked on send (internal/model Complete does exactly that).
		events <- event
		return true
	}
	emitError := func(err error) {
		send(model.StreamEvent{Type: model.EventError, Err: err})
	}

	contentType := strings.ToLower(response.Header.Get("Content-Type"))
	if !strings.Contains(contentType, "text/event-stream") {
		a.deliverBuffered(response.Body, emitError, send)
		return
	}

	adapter := newStreamAdapter()
	decoder := newSSEDecoder(a.maxEventBytes)
	consumeFrames := func(frames []sseFrame) error {
		for _, frame := range frames {
			var batch []model.StreamEvent
			if err := adapter.consumeSSE(frame, &batch); err != nil {
				return err
			}
			for _, event := range batch {
				send(event)
			}
		}
		return nil
	}

	reader := bufio.NewReader(response.Body)
	buffer := make([]byte, 32*1024)
	var wireBytes int
	for {
		chunk, readErr := reader.Read(buffer)
		if chunk > 0 {
			wireBytes += chunk
			if wireBytes > a.maxResponseBytes {
				emitError(protocolError("模型流式响应超过 %d 字节安全上限", a.maxResponseBytes))
				return
			}
			frames, err := decoder.push(buffer[:chunk])
			if err != nil {
				emitError(err)
				return
			}
			if err := consumeFrames(frames); err != nil {
				emitError(err)
				return
			}
		}
		if readErr != nil {
			if errors.Is(readErr, io.EOF) {
				frames, err := decoder.finish()
				if err != nil {
					emitError(err)
					return
				}
				if err := consumeFrames(frames); err != nil {
					emitError(err)
					return
				}
				if err := adapter.finishStream(); err != nil {
					emitError(err)
					return
				}
				return
			}
			// Any read failure (including ctx cancellation closing the
			// connection) becomes a transport or cancelled error.
			emitError(transportError(a.apiKey, readErr))
			return
		}
	}
}

// deliverBuffered handles success responses whose media type is not SSE: the
// body may still be an SSE document (compat gateways) or a complete JSON
// response (core/provider/src/http.rs:50-67).
func (a *Adapter) deliverBuffered(body io.Reader, emitError func(error), send func(model.StreamEvent) bool) {
	payload, err := readLimited(body, a.maxResponseBytes)
	if err != nil {
		emitError(err)
		return
	}
	if looksLikeSSE(payload) {
		adapter := newStreamAdapter()
		decoder := newSSEDecoder(a.maxEventBytes)
		frames, err := decoder.push(payload)
		if err != nil {
			emitError(err)
			return
		}
		final, err := decoder.finish()
		if err != nil {
			emitError(err)
			return
		}
		frames = append(frames, final...)
		for _, frame := range frames {
			var batch []model.StreamEvent
			if err := adapter.consumeSSE(frame, &batch); err != nil {
				emitError(err)
				return
			}
			for _, event := range batch {
				send(event)
			}
		}
		if err := adapter.finishStream(); err != nil {
			emitError(err)
			return
		}
		return
	}
	adapter := newStreamAdapter()
	batch, err := adapter.decodeJSON(payload)
	if err != nil {
		emitError(err)
		return
	}
	for _, event := range batch {
		send(event)
	}
}

// looksLikeSSE reports whether a buffered body shows SSE field boundaries
// (core/provider/src/http.rs:655-658).
func looksLikeSSE(body []byte) bool {
	body = bytes.TrimPrefix(body, utf8BOM)
	return bytes.HasPrefix(body, []byte("data:")) ||
		bytes.HasPrefix(body, []byte("event:")) ||
		bytes.HasPrefix(body, []byte(":"))
}

// readLimited reads a complete HTTP body within the memory limit
// (core/provider/src/http.rs:619-652). Read failures become transport errors;
// overflow becomes a stable protocol error without server payload.
func readLimited(body io.Reader, maxBytes int) ([]byte, error) {
	var payload []byte
	buffer := make([]byte, 32*1024)
	for {
		chunk, err := body.Read(buffer)
		if chunk > 0 {
			if len(payload)+chunk > maxBytes {
				return nil, protocolError("模型 HTTP 响应超过 %d 字节安全上限", maxBytes)
			}
			payload = append(payload, buffer[:chunk]...)
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return payload, nil
			}
			return nil, transportError("", err)
		}
	}
}
