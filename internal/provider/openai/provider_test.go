package openai

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"keencode/internal/model"
)

// newTextProvider builds a provider against the test server endpoint.
func newTextProvider(t *testing.T, endpoint string) *Provider {
	t.Helper()
	provider, err := New(Options{Endpoint: endpoint, APIKey: "sk-test-key-123"})
	if err != nil {
		t.Fatalf("构造 Provider 失败：%v", err)
	}
	return provider
}

// simpleRequest is a minimal valid request.
func simpleRequest() model.ModelRequest {
	return model.ModelRequest{
		Model:    "deepseek-test",
		Messages: []model.Message{model.TextMessage(model.RoleUser, "你好")},
	}
}

// sseDataLine renders one data frame of an SSE body.
func sseDataLine(payload string) string {
	return "data: " + payload + "\n\n"
}

// dataChunk builds an SSE line carrying one streamed chunk fixture.
func dataChunk(t *testing.T, value any) string {
	t.Helper()
	return sseDataLine(mustJSONString(t, value))
}

// collectChannel drains the event channel with a timeout guard.
func collectChannel(t *testing.T, events <-chan model.StreamEvent) []model.StreamEvent {
	t.Helper()
	var collected []model.StreamEvent
	for {
		select {
		case event, ok := <-events:
			if !ok {
				return collected
			}
			collected = append(collected, event)
		case <-time.After(5 * time.Second):
			t.Fatal("等待流事件超时")
			return nil
		}
	}
}

// requireTerminalEvent asserts the event list ends with MessageEnd or
// EventError and returns the last event.
func requireTerminalEvent(t *testing.T, events []model.StreamEvent) model.StreamEvent {
	t.Helper()
	if len(events) == 0 {
		t.Fatal("事件流为空")
	}
	last := events[len(events)-1]
	if last.Type != model.EventMessageEnd && last.Type != model.EventError {
		t.Fatalf("事件流应以终态事件结束，实际 %v", eventKinds(events))
	}
	return last
}

// requireModelError asserts err is a *model.ModelError of the given kind.
func requireModelError(t *testing.T, err error, kind model.ErrorKind) *model.ModelError {
	t.Helper()
	if err == nil {
		t.Fatalf("应返回 %s 错误", kind)
	}
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) {
		t.Fatalf("应返回 *model.ModelError，实际 %T：%v", err, err)
	}
	if modelErr.Kind != kind {
		t.Fatalf("错误类别应为 %s，实际 %s（%s）", kind, modelErr.Kind, modelErr.Message)
	}
	return modelErr
}

// TestStreamSSEHappyPath runs one full streaming turn against an
// httptest server and verifies request headers, the wire body, and the
// unified event sequence (including collection via model.Complete).
func TestStreamSSEHappyPath(t *testing.T) {
	var seenPath, seenAuth, seenAccept, seenContentType string
	var bodyFields map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seenPath = r.URL.Path
		seenAuth = r.Header.Get("Authorization")
		seenAccept = r.Header.Get("Accept")
		seenContentType = r.Header.Get("Content-Type")
		var raw map[string]any
		if err := json.NewDecoder(r.Body).Decode(&raw); err != nil {
			t.Errorf("请求体不是 JSON：%v", err)
			http.Error(w, "bad request", http.StatusBadRequest)
			return
		}
		bodyFields = raw
		w.Header().Set("Content-Type", "text/event-stream")
		flusher := w.(http.Flusher)
		fmt.Fprint(w, dataChunk(t, map[string]any{"id": "s1", "model": "deepseek-test",
			"choices": []any{map[string]any{"index": 0, "delta": map[string]any{"role": "assistant", "content": "你好"}}}}))
		flusher.Flush()
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{"tool_calls": []any{map[string]any{
				"index": 0, "id": "call-1", "function": map[string]any{"name": "lookup", "arguments": "{\"city\":\"杭州\"}"},
			}}}}}}))
		flusher.Flush()
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0, "delta": map[string]any{}, "finish_reason": "tool_calls"}}}))
		fmt.Fprint(w, dataChunk(t, map[string]any{"usage": map[string]any{
			"prompt_tokens": 11, "completion_tokens": 4,
			"prompt_tokens_details": map[string]any{"cached_tokens": 6},
		}}))
		fmt.Fprint(w, sseDataLine("[DONE]"))
	}))
	defer server.Close()

	// The endpoint is used verbatim, including its path prefix.
	provider := newTextProvider(t, server.URL+"/v1/chat/completions")
	req := baseToolRequest()
	req.Messages = []model.Message{model.TextMessage(model.RoleUser, "你好")}
	resp, err := model.Complete(context.Background(), provider, req)
	if err != nil {
		t.Fatalf("Complete 失败：%v", err)
	}

	if seenPath != "/v1/chat/completions" {
		t.Fatalf("请求路径不符：%q", seenPath)
	}
	if seenAuth != "Bearer sk-test-key-123" {
		t.Fatalf("认证头不符：%q", seenAuth)
	}
	if !strings.Contains(seenAccept, "text/event-stream") {
		t.Fatalf("Accept 头应接受 SSE：%q", seenAccept)
	}
	if !strings.Contains(seenContentType, "application/json") {
		t.Fatalf("Content-Type 不符：%q", seenContentType)
	}
	if bodyFields["stream"] != true {
		t.Fatalf("stream 应为 true：%v", bodyFields["stream"])
	}
	if _, ok := bodyFields["stream_options"]; !ok {
		t.Fatal("流式请求应携带 stream_options")
	}

	if len(resp.Content) != 2 {
		t.Fatalf("应有文本与工具调用两个内容块：%+v", resp.Content)
	}
	if text, ok := resp.Content[0].(model.TextBlock); !ok || text.Text != "你好" {
		t.Fatalf("文本块不符：%+v", resp.Content[0])
	}
	if call, ok := resp.Content[1].(model.ToolCallBlock); !ok ||
		call.Call.ID != "call-1" || call.Call.Name != "lookup" || call.Call.Arguments != `{"city":"杭州"}` {
		t.Fatalf("工具调用块不符：%+v", resp.Content[1])
	}
	if resp.StopReason != model.StopToolUse {
		t.Fatalf("结束原因应为 tool_use：%v", resp.StopReason)
	}
	if resp.Usage.InputTokens != 11 || resp.Usage.OutputTokens != 4 || resp.Usage.CachedTokens != 6 {
		t.Fatalf("usage 归一不符：%+v", resp.Usage)
	}
}

// TestStreamRawEventSequence verifies the raw channel contract: MessageStart
// first, terminal event last, channel closes afterwards.
func TestStreamRawEventSequence(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{"content": "好"}}}}))
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{}, "finish_reason": "stop"}}}))
		fmt.Fprint(w, sseDataLine("[DONE]"))
	}))
	defer server.Close()

	events, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
	if err != nil {
		t.Fatalf("Stream 失败：%v", err)
	}
	collected := collectChannel(t, events)
	first := collected[0]
	if first.Type != model.EventMessageStart {
		t.Fatalf("首个事件应为 message_start：%v", first.Type)
	}
	last := requireTerminalEvent(t, collected)
	if last.Type != model.EventMessageEnd || last.StopReason != model.StopEndTurn {
		t.Fatalf("终态不符：%+v", last)
	}
}

// TestStreamJSONFallback verifies a gateway that answers with a complete
// JSON body despite stream:true is decoded through decode_json
// (core/provider/src/http.rs:25-67).
func TestStreamJSONFallback(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprint(w, `{"id":"r1","model":"deepseek-test","choices":[{"index":0,"message":{
			"role":"assistant","content":"完整回答","reasoning_content":"推理中"},
			"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":2}}`)
	}))
	defer server.Close()

	events, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
	if err != nil {
		t.Fatalf("Stream 失败：%v", err)
	}
	collected := collectChannel(t, events)
	requireTerminalEvent(t, collected)
	deltas := eventsOfType(collected, model.EventTextDelta)
	if len(deltas) != 1 || deltas[0].Delta != "完整回答" {
		t.Fatalf("文本增量不符：%v", deltas)
	}
	reasoning := eventsOfType(collected, model.EventReasoningDelta)
	if len(reasoning) != 1 || reasoning[0].Delta != "推理中" {
		t.Fatalf("推理增量不符：%v", reasoning)
	}
	usages := eventsOfType(collected, model.EventUsage)
	if len(usages) != 1 || usages[0].Usage.InputTokens != 7 {
		t.Fatalf("usage 不符：%v", usages)
	}
}

// TestStreamBufferedSSEWithWrongContentType verifies an SSE body delivered
// under a wrong media type is still decoded (looks_like_sse fallback).
func TestStreamBufferedSSEWithWrongContentType(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{"content": "缓冲"}}}}))
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{}, "finish_reason": "stop"}}}))
		fmt.Fprint(w, sseDataLine("[DONE]"))
	}))
	defer server.Close()

	events, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
	if err != nil {
		t.Fatalf("Stream 失败：%v", err)
	}
	collected := collectChannel(t, events)
	last := requireTerminalEvent(t, collected)
	if last.StopReason != model.StopEndTurn {
		t.Fatalf("结束原因不符：%v", last.StopReason)
	}
}

// TestHTTPErrorClassificationTable ports the classification matrix of
// core/provider/src/tests.rs:1407-1456 against real HTTP responses.
func TestHTTPErrorClassificationTable(t *testing.T) {
	tests := []struct {
		name         string
		status       int
		retryAfter   string
		body         string
		wantKind     model.ErrorKind
		wantStatus   int
		wantRetryMS  int64
		wantRetryabl bool
	}{
		{
			name: "上下文超限", status: 400,
			body:     `{"error":{"message":"maximum context length is 200000 tokens","code":"invalid_request_error"}}`,
			wantKind: model.ErrorContextLengthExceeded,
		},
		{
			name: "额度耗尽非限流", status: 429,
			body:     `{"error":{"message":"套餐次数已用尽","code":"QUOTA_EXHAUSTED"}}`,
			wantKind: model.ErrorQuotaExceeded, wantStatus: 429,
		},
		{
			name: "限流携带 Retry-After", status: 429, retryAfter: "9",
			body:     `{"error":{"message":"已达本套餐 RPM 上限","code":"rate_limit_error"}}`,
			wantKind: model.ErrorRateLimited, wantStatus: 429, wantRetryMS: 9000, wantRetryabl: true,
		},
		{
			name: "认证失败", status: 401,
			body:     `{"error":{"message":"invalid api key","code":"invalid_api_key"}}`,
			wantKind: model.ErrorAuthentication, wantStatus: 401,
		},
		{
			name: "拒绝授权", status: 403,
			body:     `{"error":{"message":"forbidden"}}`,
			wantKind: model.ErrorAuthorization, wantStatus: 403,
		},
		{
			name: "输出上限被拒", status: 400,
			body:     `{"error":{"message":"max_tokens must be between 1 and 4096","code":"invalid_parameter"}}`,
			wantKind: model.ErrorOutputLimitRejected,
		},
		{
			name: "服务过载", status: 503,
			body:     `{"error":{"message":"server_error"}}`,
			wantKind: model.ErrorProviderUnavailable, wantStatus: 503, wantRetryabl: true,
		},
		{
			name: "模型不存在", status: 404,
			body:     `{"error":{"message":"model not found"}}`,
			wantKind: model.ErrorProtocolUnsupported, wantStatus: 404,
		},
		{
			// The unified InvalidRequest kind carries no status code, mirroring
			// core/model/src/error.rs.
			name: "普通无效请求", status: 422,
			body:     `{"error":{"message":"missing field"}}`,
			wantKind: model.ErrorInvalidRequest,
		},
		{
			name: "未知状态", status: 418,
			body:     "teapot",
			wantKind: model.ErrorProviderUnavailable, wantStatus: 418,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if tc.retryAfter != "" {
					w.Header().Set("Retry-After", tc.retryAfter)
				}
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(tc.status)
				fmt.Fprint(w, tc.body)
			}))
			defer server.Close()

			_, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
			modelErr := requireModelError(t, err, tc.wantKind)
			if modelErr.StatusCode != tc.wantStatus {
				t.Fatalf("状态码不符：got %d want %d", modelErr.StatusCode, tc.wantStatus)
			}
			if modelErr.RetryAfterMS != tc.wantRetryMS {
				t.Fatalf("Retry-After 不符：got %d want %d", modelErr.RetryAfterMS, tc.wantRetryMS)
			}
			if modelErr.IsRetryable() != tc.wantRetryabl {
				t.Fatalf("可重试性不符：got %v", modelErr.IsRetryable())
			}
		})
	}
}

// TestHTTPErrorRedactsAPIKey verifies the exact credential never reaches the
// unified error message (core/provider/src/http.rs:688-711).
func TestHTTPErrorRedactsAPIKey(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
		fmt.Fprintf(w, `{"error":{"message":"key sk-test-key-123 is invalid, see Authorization: Bearer sk-test-key-123","code":"invalid_api_key"}}`)
	}))
	defer server.Close()

	_, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
	modelErr := requireModelError(t, err, model.ErrorAuthentication)
	if strings.Contains(modelErr.Message, "sk-test-key-123") {
		t.Fatalf("错误信息泄露凭据：%q", modelErr.Message)
	}
	if !strings.Contains(modelErr.Message, model.RedactedSecret) {
		t.Fatalf("错误信息应包含脱敏占位符：%q", modelErr.Message)
	}
}

// TestInBandErrorMidStream verifies an error object inside a 200 SSE stream
// terminates the stream with a classified EventError.
func TestInBandErrorMidStream(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{"content": "部分"}}}}))
		fmt.Fprint(w, sseDataLine(`{"error":{"message":"authentication failed for this key","code":"authentication_error"}}`))
	}))
	defer server.Close()

	events, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
	if err != nil {
		t.Fatalf("Stream 不应在连接阶段失败：%v", err)
	}
	collected := collectChannel(t, events)
	last := requireTerminalEvent(t, collected)
	if last.Type != model.EventError {
		t.Fatalf("终态应为 error 事件：%v", last.Type)
	}
	var modelErr *model.ModelError
	if !errors.As(last.Err, &modelErr) || modelErr.Kind != model.ErrorAuthentication {
		t.Fatalf("带内错误应归一为 authentication：%v", last.Err)
	}
	if strings.Contains(modelErr.Message, "sk-test-key-123") {
		t.Fatal("错误信息不应包含凭据")
	}
}

// TestProtocolErrorMidStream verifies malformed SSE JSON terminates the
// stream with a protocol EventError.
func TestProtocolErrorMidStream(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, sseDataLine("{not json}"))
	}))
	defer server.Close()

	events, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
	if err != nil {
		t.Fatalf("Stream 失败：%v", err)
	}
	collected := collectChannel(t, events)
	last := requireTerminalEvent(t, collected)
	var modelErr *model.ModelError
	if !errors.As(last.Err, &modelErr) || modelErr.Kind != model.ErrorProtocol {
		t.Fatalf("坏 JSON 应产出协议错误：%v", last.Err)
	}
}

// TestStreamCutWithoutFinishReason verifies EOF before finish_reason fails
// with a protocol EventError (chat_completions.rs:292-297).
func TestStreamCutWithoutFinishReason(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{"content": "截断"}}}}))
		// Body ends without [DONE] and without finish_reason.
	}))
	defer server.Close()

	events, err := newTextProvider(t, server.URL).Stream(context.Background(), simpleRequest())
	if err != nil {
		t.Fatalf("Stream 失败：%v", err)
	}
	collected := collectChannel(t, events)
	last := requireTerminalEvent(t, collected)
	var modelErr *model.ModelError
	if !errors.As(last.Err, &modelErr) || modelErr.Kind != model.ErrorProtocol ||
		!strings.Contains(modelErr.Message, "finish_reason 之前关闭") {
		t.Fatalf("流截断应报协议错误：%v", last.Err)
	}
}

// TestContextCancelMidStream verifies cancelling the context terminates the
// HTTP connection and closes the event channel.
func TestContextCancelMidStream(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{"content": "开始"}}}}))
		w.(http.Flusher).Flush()
		<-r.Context().Done() // Hang until the client goes away.
	}))
	defer server.Close()

	ctx, cancel := context.WithCancel(context.Background())
	events, err := newTextProvider(t, server.URL).Stream(ctx, simpleRequest())
	if err != nil {
		t.Fatalf("Stream 失败：%v", err)
	}
	// Wait for the first event, then cancel.
	select {
	case <-events:
	case <-time.After(5 * time.Second):
		t.Fatal("未收到首个事件")
	}
	cancel()

	for {
		select {
		case event, ok := <-events:
			if !ok {
				return
			}
			if event.Type == model.EventError {
				var modelErr *model.ModelError
				if !errors.As(event.Err, &modelErr) || modelErr.Kind != model.ErrorCancelled {
					t.Fatalf("取消应产出 cancelled 错误：%v", event.Err)
				}
			}
		case <-time.After(5 * time.Second):
			t.Fatal("取消后通道未关闭")
			return
		}
	}
}

// TestStreamValidationFailsBeforeHTTP verifies invalid requests return an
// error without touching the network.
func TestStreamValidationFailsBeforeHTTP(t *testing.T) {
	hit := false
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hit = true
	}))
	defer server.Close()

	provider := newTextProvider(t, server.URL)
	_, err := provider.Stream(context.Background(), model.ModelRequest{})
	if err == nil {
		t.Fatal("空请求应被校验拒绝")
	}
	if hit {
		t.Fatal("校验失败不应发起网络请求")
	}
}

// TestTransportErrorIsRetryable verifies connection failures produce
// retryable transport errors without URL or credential exposure.
func TestTransportErrorIsRetryable(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {}))
	endpoint := server.URL
	server.Close() // Guarantee a connection failure.

	_, err := newTextProvider(t, endpoint).Stream(context.Background(), simpleRequest())
	modelErr := requireModelError(t, err, model.ErrorTransport)
	if !modelErr.IsRetryable() {
		t.Fatal("传输失败应可重试")
	}
	if strings.Contains(modelErr.Message, endpoint) {
		t.Fatalf("传输错误不应携带端点地址：%q", modelErr.Message)
	}
}

// TestOptionsValidation verifies constructor validation
// (core/provider/src/config.rs:460-480 reduced).
func TestOptionsValidation(t *testing.T) {
	tests := []struct {
		name    string
		options Options
	}{
		{"端点缺失", Options{}},
		{"端点非 http", Options{Endpoint: "ftp://example.invalid/v1/chat/completions"}},
		{"端点无主机", Options{Endpoint: "http:///path"}},
		{"API Key 控制字符", Options{Endpoint: "http://example.invalid/v1/chat/completions", APIKey: "sk-\n-key"}},
		{"未知预算字段", Options{Endpoint: "http://example.invalid/v1/chat/completions", OutputTokenField: "budget"}},
		{"响应上限小于事件上限", Options{Endpoint: "http://example.invalid/v1/chat/completions", MaxEventBytes: 2 << 20, MaxResponseBytes: 1 << 20}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := New(tc.options); err == nil {
				t.Fatalf("%s 应被拒绝", tc.name)
			}
		})
	}
}

// TestCapabilitiesSnapshot verifies the reported capability snapshot and its
// override.
func TestCapabilitiesSnapshot(t *testing.T) {
	provider, err := New(Options{Endpoint: "http://example.invalid/v1/chat/completions"})
	if err != nil {
		t.Fatalf("构造失败：%v", err)
	}
	defaults := provider.Capabilities("any-model")
	if !defaults.Reasoning || len(defaults.ReasoningEfforts) != 4 {
		t.Fatalf("默认能力快照不符：%+v", defaults)
	}

	override := model.Capabilities{Reasoning: false, ContextWindow: 128000}
	provider, err = New(Options{
		Endpoint:     "http://example.invalid/v1/chat/completions",
		Capabilities: override,
	})
	if err != nil {
		t.Fatalf("构造失败：%v", err)
	}
	if got := provider.Capabilities("any-model"); got.Reasoning || got.ContextWindow != 128000 {
		t.Fatalf("能力覆盖不符：%+v", got)
	}
}

// TestAnonymousEndpointOmitsAuthorization verifies empty API keys send no
// credential header (explicitly anonymous local gateways).
func TestAnonymousEndpointOmitsAuthorization(t *testing.T) {
	var authHeader string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		authHeader = r.Header.Get("Authorization")
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{"content": "匿名"}}}}))
		fmt.Fprint(w, dataChunk(t, map[string]any{"choices": []any{map[string]any{"index": 0,
			"delta": map[string]any{}, "finish_reason": "stop"}}}))
		fmt.Fprint(w, sseDataLine("[DONE]"))
	}))
	defer server.Close()

	provider, err := New(Options{Endpoint: server.URL})
	if err != nil {
		t.Fatalf("构造失败：%v", err)
	}
	events, err := provider.Stream(context.Background(), simpleRequest())
	if err != nil {
		t.Fatalf("Stream 失败：%v", err)
	}
	collectChannel(t, events)
	if authHeader != "" {
		t.Fatalf("匿名端点不应携带认证头：%q", authHeader)
	}
}
