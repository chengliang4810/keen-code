package anthropic

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"keencode/internal/model"
)

// sseBody builds an SSE document from event data payloads.
func sseBody(datas ...string) string {
	var builder strings.Builder
	for _, data := range datas {
		builder.WriteString("event: ")
		var probe map[string]json.RawMessage
		if json.Unmarshal([]byte(data), &probe) == nil {
			if raw, ok := probe["type"]; ok {
				var name string
				if json.Unmarshal(raw, &name) == nil {
					builder.WriteString(name)
				}
			}
		}
		builder.WriteString("\ndata: ")
		builder.WriteString(data)
		builder.WriteString("\n\n")
	}
	return builder.String()
}

// newStreamTestAdapter returns an adapter pointed at the test server.
func newStreamTestAdapter(server *httptest.Server, mutate func(*Options)) *Adapter {
	opts := Options{
		BaseURL:       server.URL + "/v1",
		APIKey:        "sk-test-key",
		PromptCaching: false,
	}
	if mutate != nil {
		mutate(&opts)
	}
	adapter, err := New(opts)
	if err != nil {
		panic(err)
	}
	return adapter
}

// collect drains the event channel into a slice.
func collect(t *testing.T, events <-chan model.StreamEvent) []model.StreamEvent {
	t.Helper()
	var received []model.StreamEvent
	for event := range events {
		received = append(received, event)
	}
	return received
}

// minimalRequest is the smallest valid neutral request.
func minimalRequest() model.ModelRequest {
	return model.ModelRequest{Model: "test-model", Messages: []model.Message{model.TextMessage(model.RoleUser, "问题")}}
}

// TestStreamSendsAuthenticatedMessagesRequest pins the request shape on the
// wire: path, method, auth and protocol headers, and the streaming body
// (client.rs:566-599).
func TestStreamSendsAuthenticatedMessagesRequest(t *testing.T) {
	var gotPath, gotMethod string
	var gotHeaders http.Header
	var gotBody map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath, gotMethod = r.URL.Path, r.Method
		gotHeaders = r.Header.Clone()
		payload, _ := io.ReadAll(r.Body)
		_ = json.Unmarshal(payload, &gotBody)
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, sseBody(
			`{"type":"message_start","message":{}}`,
			`{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"好"}}`,
			`{"type":"content_block_stop","index":0}`,
			`{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":2,"output_tokens":1}}`,
			`{"type":"message_stop"}`,
		))
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, nil)
	events, err := adapter.Stream(context.Background(), minimalRequest())
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	received := collect(t, events)

	if gotMethod != http.MethodPost {
		t.Fatalf("method = %s, want POST", gotMethod)
	}
	if gotPath != "/v1/messages" {
		t.Fatalf("path = %s, want /v1/messages", gotPath)
	}
	if gotHeaders.Get("x-api-key") != "sk-test-key" {
		t.Fatalf("x-api-key = %q", gotHeaders.Get("x-api-key"))
	}
	if gotHeaders.Get("anthropic-version") != anthropicVersion {
		t.Fatalf("anthropic-version = %q", gotHeaders.Get("anthropic-version"))
	}
	if !strings.Contains(gotHeaders.Get("Accept"), "text/event-stream") {
		t.Fatalf("accept = %q", gotHeaders.Get("Accept"))
	}
	if gotHeaders.Get("Content-Type") != "application/json" {
		t.Fatalf("content-type = %q", gotHeaders.Get("Content-Type"))
	}
	if gotBody["stream"] != true || gotBody["model"] != "test-model" {
		t.Fatalf("body = %#v", gotBody)
	}

	if len(received) != 4 ||
		received[0].Type != model.EventMessageStart ||
		received[1].Type != model.EventTextDelta || received[1].Delta != "好" ||
		received[2].Type != model.EventUsage || received[2].Usage != (model.TokenUsage{InputTokens: 2, OutputTokens: 1, CachedTokens: -1}) ||
		received[3].Type != model.EventMessageEnd || received[3].StopReason != model.StopEndTurn {
		t.Fatalf("events = %#v", received)
	}
}

// TestStreamFeedsNeutralCollector runs a full streaming call through
// model.Complete to prove the emitted sequence satisfies the neutral layer.
func TestStreamFeedsNeutralCollector(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, sseBody(
			`{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":10,"cache_read_input_tokens":4}}}`,
			`{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}`,
			`{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"推理"}}`,
			`{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}`,
			`{"type":"content_block_stop","index":0}`,
			`{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call-1","name":"bash","input":{}}}`,
			`{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}`,
			`{"type":"content_block_stop","index":1}`,
			`{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}`,
			`{"type":"message_stop"}`,
		))
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, nil)
	response, err := model.Complete(context.Background(), adapter, minimalRequest())
	if err != nil {
		t.Fatalf("Complete: %v", err)
	}
	if response.StopReason != model.StopToolUse {
		t.Fatalf("stop reason = %s, want tool_use", response.StopReason)
	}
	if len(response.Content) != 2 {
		t.Fatalf("content = %#v, want reasoning + tool call", response.Content)
	}
	reasoning, ok := response.Content[0].(model.ReasoningBlock)
	if !ok || reasoning.Text != "推理" {
		t.Fatalf("content[0] = %#v", response.Content[0])
	}
	kind, data, err := decodeContinuationEnvelope(reasoning.Signature)
	if err != nil || kind != signatureStateKind || string(data) != `"sig"` {
		t.Fatalf("signature = (%s, %s, %v)", kind, data, err)
	}
	call, ok := response.Content[1].(model.ToolCallBlock)
	if !ok || call.Call.ID != "call-1" || call.Call.Arguments != `{"command":"ls"}` {
		t.Fatalf("content[1] = %#v", response.Content[1])
	}
	// Input tokens include the cache read; output comes from message_delta.
	if response.Usage != (model.TokenUsage{InputTokens: 14, OutputTokens: 7, CachedTokens: 4}) {
		t.Fatalf("usage = %+v", response.Usage)
	}
}

// TestStreamClassifiesHTTPErrors ports the HTTP error classification table
// (tests.rs:1407+, http.rs:214-266).
func TestStreamClassifiesHTTPErrors(t *testing.T) {
	tests := []struct {
		name           string
		status         int
		retryAfter     string
		body           string
		wantKind       model.ErrorKind
		wantRetryable  bool
		wantStatusCode int
		wantRetryAfter int64
	}{
		{
			name:           "authentication error body",
			status:         401,
			body:           `{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}`,
			wantKind:       model.ErrorAuthentication,
			wantStatusCode: 401,
		},
		{
			name:           "rate limit with retry-after seconds",
			status:         429,
			retryAfter:     "7",
			body:           `{"error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}`,
			wantKind:       model.ErrorRateLimited,
			wantRetryable:  true,
			wantStatusCode: 429,
			wantRetryAfter: 7000,
		},
		{
			name:           "anthropic overload 529 is retryable",
			status:         529,
			body:           `{"error":{"type":"overloaded_error","message":"Overloaded"}}`,
			wantKind:       model.ErrorProviderUnavailable,
			wantRetryable:  true,
			wantStatusCode: 529,
		},
		{
			name:     "output limit rejection is structured",
			status:   400,
			body:     `{"error":{"type":"invalid_request_error","message":"max_tokens: Field required"}}`,
			wantKind: model.ErrorOutputLimitRejected,
		},
		{
			name:           "context length exceeded",
			status:         400,
			body:           `{"error":{"type":"invalid_request_error","message":"prompt is too long: 200000 tokens > 180000 maximum"}}`,
			wantKind:       model.ErrorContextLengthExceeded,
			wantStatusCode: 0, // the Rust variant carries no status field
		},
		{
			name:           "payment required maps to quota",
			status:         402,
			body:           `{"error":{"message":"insufficient balance"}}`,
			wantKind:       model.ErrorQuotaExceeded,
			wantStatusCode: 402,
		},
		{
			name:           "cloudflare blip 524 is retryable",
			status:         524,
			body:           "a timeout occurred",
			wantKind:       model.ErrorProviderUnavailable,
			wantRetryable:  true,
			wantStatusCode: 524,
		},
		{
			name:           "plain invalid request stays final",
			status:         400,
			body:           `{"error":{"type":"invalid_request_error","message":"temperature must be between 0 and 1"}}`,
			wantKind:       model.ErrorInvalidRequest,
			wantStatusCode: 0, // the Rust variant carries no status field
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if tt.retryAfter != "" {
					w.Header().Set("Retry-After", tt.retryAfter)
				}
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(tt.status)
				fmt.Fprint(w, tt.body)
			}))
			defer server.Close()

			adapter := newStreamTestAdapter(server, nil)
			_, err := adapter.Stream(context.Background(), minimalRequest())
			var modelErr *model.ModelError
			if !errors.As(err, &modelErr) {
				t.Fatalf("err = %v, want ModelError", err)
			}
			if modelErr.Kind != tt.wantKind {
				t.Fatalf("kind = %s (%s), want %s", modelErr.Kind, modelErr.Message, tt.wantKind)
			}
			if modelErr.StatusCode != tt.wantStatusCode {
				t.Fatalf("status code = %d, want %d", modelErr.StatusCode, tt.wantStatusCode)
			}
			if modelErr.IsRetryable() != tt.wantRetryable {
				t.Fatalf("retryable = %v, want %v", modelErr.IsRetryable(), tt.wantRetryable)
			}
			if modelErr.RetryAfterMS != tt.wantRetryAfter {
				t.Fatalf("retry after = %d, want %d", modelErr.RetryAfterMS, tt.wantRetryAfter)
			}
		})
	}
}

// TestStreamRedactsAPIKeyInErrorBodies pins the exact-credential redaction
// (http.rs safe_error_message tests).
func TestStreamRedactsAPIKeyInErrorBodies(t *testing.T) {
	key := "sk-secret,foo"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(401)
		fmt.Fprintf(w, `{"error":{"message":"api_key=%s request_id=req-1"}}`, key)
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, func(opts *Options) { opts.APIKey = key })
	_, err := adapter.Stream(context.Background(), minimalRequest())
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) {
		t.Fatalf("err = %v, want ModelError", err)
	}
	if strings.Contains(modelErr.Message, key) {
		t.Fatalf("message leaked the key: %q", modelErr.Message)
	}
	if !strings.Contains(modelErr.Message, model.RedactedSecret) || !strings.Contains(modelErr.Message, "request_id=req-1") {
		t.Fatalf("message = %q, want redacted key and preserved diagnostics", modelErr.Message)
	}
}

// TestStreamDoesNotFollowRedirects pins the Policy::none behavior: a 3xx is
// classified instead of silently retargeting the credential.
func TestStreamDoesNotFollowRedirects(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
		http.Redirect(w, r, "/elsewhere", http.StatusFound)
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, nil)
	_, err := adapter.Stream(context.Background(), minimalRequest())
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) {
		t.Fatalf("err = %v, want ModelError", err)
	}
	if modelErr.StatusCode != http.StatusFound {
		t.Fatalf("status = %d, want 302", modelErr.StatusCode)
	}
	if requests != 1 {
		t.Fatalf("requests = %d, want 1 (no redirect follow)", requests)
	}
}

// TestStreamSurfacesInBandErrorEvent keeps a mid-stream provider error inside
// the event channel with its classification.
func TestStreamSurfacesInBandErrorEvent(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, sseBody(
			`{"type":"message_start","message":{}}`,
			`{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}`,
		))
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, nil)
	events, err := adapter.Stream(context.Background(), minimalRequest())
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	received := collect(t, events)
	if len(received) != 2 {
		t.Fatalf("events = %#v, want start + error", received)
	}
	if received[1].Type != model.EventError || received[1].Err == nil {
		t.Fatalf("final event = %#v", received[1])
	}
	var modelErr *model.ModelError
	if !errors.As(received[1].Err, &modelErr) || modelErr.Kind != model.ErrorProviderUnavailable || !modelErr.Retryable {
		t.Fatalf("error = %v, want retryable provider unavailable", received[1].Err)
	}
}

// TestStreamAcceptsBufferedBodies covers the JSON success document and the
// SSE document delivered under a JSON media type
// (core/provider/src/http.rs:50-67).
func TestStreamAcceptsBufferedBodies(t *testing.T) {
	t.Run("json response", func(t *testing.T) {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			fmt.Fprint(w, `{"id":"msg_1","content":[{"type":"text","text":" buffered"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2}}`)
		}))
		defer server.Close()

		adapter := newStreamTestAdapter(server, nil)
		events, err := adapter.Stream(context.Background(), minimalRequest())
		if err != nil {
			t.Fatalf("Stream: %v", err)
		}
		received := collect(t, events)
		if len(received) != 4 ||
			received[0].Type != model.EventMessageStart ||
			received[1].Type != model.EventTextDelta || received[1].Delta != " buffered" ||
			received[2].Type != model.EventUsage || received[2].Usage != (model.TokenUsage{InputTokens: 1, OutputTokens: 2, CachedTokens: -1}) ||
			received[3].Type != model.EventMessageEnd || received[3].StopReason != model.StopEndTurn {
			t.Fatalf("events = %#v", received)
		}
	})

	t.Run("sse under json media type", func(t *testing.T) {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			fmt.Fprint(w, sseBody(
				`{"type":"message_start","message":{}}`,
				`{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"兼容"}}`,
				`{"type":"content_block_stop","index":0}`,
				`{"type":"message_delta","delta":{"stop_reason":"end_turn"}}`,
				`{"type":"message_stop"}`,
			))
		}))
		defer server.Close()

		adapter := newStreamTestAdapter(server, nil)
		events, err := adapter.Stream(context.Background(), minimalRequest())
		if err != nil {
			t.Fatalf("Stream: %v", err)
		}
		received := collect(t, events)
		if len(received) != 3 || received[2].Type != model.EventMessageEnd {
			t.Fatalf("events = %#v", received)
		}
	})
}

// TestStreamCancelBeforeResponse covers synchronous cancellation: ctx done
// before the response arrives returns a cancelled error.
func TestStreamCancelBeforeResponse(t *testing.T) {
	release := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		<-release
	}))
	defer server.Close()
	defer close(release)

	ctx, cancel := context.WithCancel(context.Background())
	adapter := newStreamTestAdapter(server, nil)
	go func() {
		time.Sleep(20 * time.Millisecond)
		cancel()
	}()
	_, err := adapter.Stream(ctx, minimalRequest())
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorCancelled {
		t.Fatalf("err = %v, want cancelled", err)
	}
}

// TestStreamCancelMidStreamEmitsEventError covers the channel path: the
// connection dies with ctx, one cancelled EventError is emitted, and the
// channel closes.
func TestStreamCancelMidStreamEmitsEventError(t *testing.T) {
	block := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, sseBody(`{"type":"message_start","message":{}}`))
		if flusher := w.(http.Flusher); flusher != nil {
			flusher.Flush()
		}
		<-block
	}))
	defer server.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	adapter := newStreamTestAdapter(server, nil)
	events, err := adapter.Stream(ctx, minimalRequest())
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	first, ok := <-events
	if !ok || first.Type != model.EventMessageStart {
		t.Fatalf("first event = %#v", first)
	}
	cancel()
	// The provider contract requires one EventError followed by channel
	// close; Complete drains, so read to the end here as well.
	sawError, sawClose := false, false
	for event := range events {
		if event.Type == model.EventError {
			sawError = true
			var modelErr *model.ModelError
			if !errors.As(event.Err, &modelErr) {
				t.Fatalf("error = %v, want ModelError", event.Err)
			}
		}
	}
	sawClose = true
	if !sawError || !sawClose {
		t.Fatalf("stream ended without error event (sawError=%v)", sawError)
	}
	close(block)
}

// TestStreamRejectsValidationBeforeNetwork pins the immediate validation
// error path.
func TestStreamRejectsValidationBeforeNetwork(t *testing.T) {
	var requests int
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, nil)
	invalid := minimalRequest()
	invalid.Messages = nil
	if _, err := adapter.Stream(context.Background(), invalid); err == nil {
		t.Fatalf("invalid request accepted")
	}
	if requests != 0 {
		t.Fatalf("requests = %d, want 0", requests)
	}
}

// TestStreamTransportFailureIsRetryable covers the connection-level failure.
func TestStreamTransportFailureIsRetryable(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		panic(http.ErrAbortHandler)
	}))
	server.Config.ErrorLog = discardLogger()
	defer server.Close()

	adapter := newStreamTestAdapter(server, nil)
	_, err := adapter.Stream(context.Background(), minimalRequest())
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorTransport || !modelErr.IsRetryable() {
		t.Fatalf("err = %v, want retryable transport", err)
	}
}

// TestExactEndpointBaseURLVerbatim pins the full-path endpoint mode: a base
// URL already ending in /messages is used verbatim.
func TestExactEndpointBaseURLVerbatim(t *testing.T) {
	var gotPath string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, sseBody(
			`{"type":"message_start","message":{}}`,
			`{"type":"message_delta","delta":{"stop_reason":"end_turn"}}`,
			`{"type":"message_stop"}`,
		))
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, func(opts *Options) {
		opts.BaseURL = server.URL + "/custom/gateway/messages"
	})
	events, err := adapter.Stream(context.Background(), minimalRequest())
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	collect(t, events)
	if gotPath != "/custom/gateway/messages" {
		t.Fatalf("path = %s, want verbatim endpoint", gotPath)
	}
}

// TestNewValidatesOptions covers constructor failures.
func TestNewValidatesOptions(t *testing.T) {
	tests := []struct {
		name string
		opts Options
	}{
		{name: "empty base url", opts: Options{}},
		{name: "non-http scheme", opts: Options{BaseURL: "ftp://example.com/v1"}},
		{name: "missing host", opts: Options{BaseURL: "https:///v1"}},
		{name: "unsafe api key", opts: Options{BaseURL: "https://api.anthropic.com/v1", APIKey: "bad\nkey"}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if _, err := New(tt.opts); err == nil {
				t.Fatalf("options accepted")
			}
		})
	}
	t.Run("default capabilities", func(t *testing.T) {
		adapter, err := New(Options{BaseURL: "https://api.anthropic.com/v1"})
		if err != nil {
			t.Fatalf("New: %v", err)
		}
		caps := adapter.Capabilities("claude-sonnet-4-5")
		if !caps.Reasoning || len(caps.ReasoningEfforts) != 4 {
			t.Fatalf("capabilities = %#v", caps)
		}
	})
}

// TestStreamFinishWithoutMessageStopIsError covers a stream the server cut
// before the terminal event.
func TestStreamFinishWithoutMessageStopIsError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, sseBody(
			`{"type":"message_start","message":{}}`,
			`{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"截断"}}`,
		))
		if flusher := w.(http.Flusher); flusher != nil {
			flusher.Flush()
		}
	}))
	defer server.Close()

	adapter := newStreamTestAdapter(server, nil)
	events, err := adapter.Stream(context.Background(), minimalRequest())
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	received := collect(t, events)
	if len(received) == 0 {
		t.Fatalf("no events")
	}
	last := received[len(received)-1]
	var modelErr *model.ModelError
	if last.Type != model.EventError || !errors.As(last.Err, &modelErr) {
		t.Fatalf("final event = %#v, want error", last)
	}
	if !strings.Contains(modelErr.Message, "message_stop") {
		t.Fatalf("error = %q, want missing-terminal diagnostics", modelErr.Message)
	}
}

// discardLogger returns a logger that swallows server-side panics noise.
func discardLogger() *log.Logger {
	return log.New(io.Discard, "", 0)
}
