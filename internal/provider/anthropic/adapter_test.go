package anthropic

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"

	"keencode/internal/model"
)

// frame builds an SSE frame whose data is the compact JSON of the raw text.
func frame(t *testing.T, data string) sseFrame {
	t.Helper()
	if !json.Valid([]byte(data)) {
		t.Fatalf("test fixture is not valid JSON: %s", data)
	}
	return sseFrame{data: data}
}

// runFrames feeds every frame through a fresh adapter and returns the events.
func runFrames(t *testing.T, frames []sseFrame) ([]model.StreamEvent, error) {
	t.Helper()
	adapter := newStreamAdapter()
	var events []model.StreamEvent
	for _, f := range frames {
		if err := adapter.consumeSSE(f, &events); err != nil {
			return nil, err
		}
	}
	if err := adapter.finishStream(); err != nil {
		return nil, err
	}
	return events, nil
}

// TestConsumeSSEFullTextStream checks the canonical text event sequence.
func TestConsumeSSEFullTextStream(t *testing.T) {
	events, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{"id":"msg_1","model":"test-model","usage":{"input_tokens":3,"output_tokens":1}}}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"，世界"}}`),
		frame(t, `{"type":"content_block_stop","index":0}`),
		frame(t, `{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}`),
		frame(t, `{"type":"message_stop"}`),
	})
	if err != nil {
		t.Fatalf("runFrames: %v", err)
	}
	want := []model.StreamEvent{
		{Type: model.EventMessageStart},
		{Type: model.EventUsage, Usage: model.TokenUsage{InputTokens: 3, OutputTokens: 1, CachedTokens: -1}},
		{Type: model.EventTextDelta, Index: 0, Delta: "你好"},
		{Type: model.EventTextDelta, Index: 0, Delta: "，世界"},
		// message_delta carries its own usage snapshot that merges later.
		{Type: model.EventUsage, Usage: model.TokenUsage{InputTokens: -1, OutputTokens: 5, CachedTokens: -1}},
		{Type: model.EventMessageEnd, StopReason: model.StopEndTurn},
	}
	if len(events) != len(want) {
		t.Fatalf("events = %#v, want %d events", events, len(want))
	}
	for i := range want {
		if events[i] != want[i] {
			t.Fatalf("events[%d] = %#v, want %#v", i, events[i], want[i])
		}
	}
}

// TestConsumeSSEToolCallAggregation checks start/args/stop assembly including
// the inline input object of content_block_start.
func TestConsumeSSEToolCallAggregation(t *testing.T) {
	events, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":"weather","input":{"city":"北京"}}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\",\"unit\":\"C\"}"}}`),
		frame(t, `{"type":"content_block_stop","index":0}`),
		frame(t, `{"type":"message_delta","delta":{"stop_reason":"tool_use"}}`),
		frame(t, `{"type":"message_stop"}`),
	})
	if err != nil {
		t.Fatalf("runFrames: %v", err)
	}
	want := []model.StreamEvent{
		{Type: model.EventMessageStart},
		{Type: model.EventToolCallStart, Index: 0, CallID: "call-1", Name: "weather"},
		{Type: model.EventToolCallArgsDelta, Index: 0, CallID: "call-1", Delta: `{"city":"北京"}`},
		{Type: model.EventToolCallArgsDelta, Index: 0, CallID: "call-1", Delta: `","unit":"C"}`},
		{Type: model.EventToolCallEnd, Index: 0, CallID: "call-1"},
		{Type: model.EventMessageEnd, StopReason: model.StopToolUse},
	}
	if len(events) != len(want) {
		t.Fatalf("events = %#v, want %d events", events, len(want))
	}
	for i := range want {
		if events[i] != want[i] {
			t.Fatalf("events[%d] = %#v, want %#v", i, events[i], want[i])
		}
	}
}

// TestConsumeSSEThinkingSignatureAndRedacted checks reasoning delta, signature
// continuation commit at block stop, and the immediate redacted continuation.
func TestConsumeSSEThinkingSignatureAndRedacted(t *testing.T) {
	events, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"推理"}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-"}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"part"}}`),
		frame(t, `{"type":"content_block_stop","index":0}`),
		frame(t, `{"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":{"encrypted":"x"}}}`),
		frame(t, `{"type":"content_block_stop","index":1}`),
		frame(t, `{"type":"message_delta","delta":{"stop_reason":"end_turn"}}`),
		frame(t, `{"type":"message_stop"}`),
	})
	if err != nil {
		t.Fatalf("runFrames: %v", err)
	}
	// Expected: start, reasoning delta, signature continuation (accumulated),
	// redacted continuation, end.
	if len(events) != 5 {
		t.Fatalf("events = %#v, want 5 events", events)
	}
	if events[1].Type != model.EventReasoningDelta || events[1].Delta != "推理" {
		t.Fatalf("events[1] = %#v", events[1])
	}
	if events[2].Type != model.EventReasoningContinuation {
		t.Fatalf("events[2] = %#v", events[2])
	}
	kind, data, err := decodeContinuationEnvelope(events[2].Continuation)
	if err != nil || kind != signatureStateKind || string(data) != `"sig-part"` {
		t.Fatalf("continuation = (%s, %s, %v)", kind, data, err)
	}
	if events[3].Type != model.EventReasoningContinuation {
		t.Fatalf("events[3] = %#v", events[3])
	}
	kind, _, err = decodeContinuationEnvelope(events[3].Continuation)
	if err != nil || kind != redactedStateKind {
		t.Fatalf("redacted continuation = (%s, %v)", kind, err)
	}
	if events[4].Type != model.EventMessageEnd || events[4].StopReason != model.StopEndTurn {
		t.Fatalf("events[4] = %#v", events[4])
	}
}

// TestConsumeSSEEmptyMessageStartSynthesizesStart ports the Bedrock gateway
// test (core/provider/src/adapters/messages_tests.rs:17-62).
func TestConsumeSSEEmptyMessageStartSynthesizesStart(t *testing.T) {
	events, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start"}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}`),
		frame(t, `{"type":"content_block_stop","index":0}`),
		frame(t, `{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":3,"output_tokens":5}}`),
		frame(t, `{"type":"message_stop"}`),
	})
	if err != nil {
		t.Fatalf("runFrames: %v", err)
	}
	if events[0].Type != model.EventMessageStart {
		t.Fatalf("first event = %#v, want message start", events[0])
	}
	hasText, hasEnd := false, false
	for _, event := range events {
		if event.Type == model.EventTextDelta && event.Delta == "你好" {
			hasText = true
		}
		if event.Type == model.EventMessageEnd {
			hasEnd = true
		}
	}
	if !hasText || !hasEnd {
		t.Fatalf("events = %#v, want text and end", events)
	}
}

// TestConsumeSSEToleratesNoise covers ping frames, unknown event types,
// unknown content block types, and post-terminal drain mode
// (messages.rs:264-311 unknown-element policy).
func TestConsumeSSEToleratesNoise(t *testing.T) {
	events, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"ping"}`),
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv","name":"web_search"}}`),
		frame(t, `{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}`),
		frame(t, `{"type":"content_block_stop","index":0}`),
		frame(t, `{"type":"content_block_start","index":1,"content_block":{"type":"text","text":"正文"}}`),
		frame(t, `{"type":"content_block_stop","index":1}`),
		frame(t, `{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"孤儿增量"}}`),
		frame(t, `{"type":"custom_gateway_event","payload":{}}`),
		frame(t, `{"type":"message_delta","delta":{"stop_reason":"end_turn"}}`),
		frame(t, `{"type":"message_stop"}`),
		// Drain mode: everything after the terminal state is ignored.
		frame(t, `{"type":"ping"}`),
		frame(t, `{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"迟到"}}`),
	})
	if err != nil {
		t.Fatalf("runFrames: %v", err)
	}
	want := []model.StreamEvent{
		{Type: model.EventMessageStart},
		{Type: model.EventTextDelta, Index: 1, Delta: "正文"},
		{Type: model.EventMessageEnd, StopReason: model.StopEndTurn},
	}
	if len(events) != len(want) {
		t.Fatalf("events = %#v, want %d events", events, len(want))
	}
	for i := range want {
		if events[i] != want[i] {
			t.Fatalf("events[%d] = %#v, want %#v", i, events[i], want[i])
		}
	}
}

// TestConsumeSSEProtocolViolations covers the structural failures that must
// reject the stream (messages.rs 逐帧校验).
func TestConsumeSSEProtocolViolations(t *testing.T) {
	tests := []struct {
		name   string
		frames []string
	}{
		{
			name: "content before message_start",
			frames: []string{
				`{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"早到"}}`,
				`{"type":"message_start","message":{}}`,
				`{"type":"message_stop"}`,
			},
		},
		{
			name: "duplicate message_start",
			frames: []string{
				`{"type":"message_start","message":{}}`,
				`{"type":"message_start","message":{}}`,
				`{"type":"message_stop"}`,
			},
		},
		{
			name: "duplicate block index",
			frames: []string{
				`{"type":"message_start","message":{}}`,
				`{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`,
				`{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`,
				`{"type":"message_stop"}`,
			},
		},
		{
			name: "index type conflict",
			frames: []string{
				`{"type":"message_start","message":{}}`,
				`{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`,
				`{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}`,
				`{"type":"message_stop"}`,
			},
		},
		{
			name: "stop without start",
			frames: []string{
				`{"type":"message_start","message":{}}`,
				`{"type":"content_block_stop","index":0}`,
				`{"type":"message_stop"}`,
			},
		},
		{
			name: "message_stop with open blocks",
			frames: []string{
				`{"type":"message_start","message":{}}`,
				`{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`,
				`{"type":"message_stop"}`,
			},
		},
		{
			name: "event field disagrees with data.type",
			frames: []string{
				`{"type":"message_start","message":{}}`,
				`{"type":"message_stop"}`,
			},
		},
		{
			name: "invalid JSON data",
			frames: []string{
				`{"type":"message_start","message":{}}`,
				`{"type":"message_stop"}`,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			frames := make([]sseFrame, 0, len(tt.frames))
			for _, data := range tt.frames {
				frames = append(frames, frame(t, data))
			}
			// The mismatch and invalid-JSON cases are rewritten here because
			// their violations cannot survive frame() JSON validation.
			switch tt.name {
			case "event field disagrees with data.type":
				frames = []sseFrame{
					frame(t, `{"type":"message_start","message":{}}`),
					{data: `{"type":"message_stop"}`, event: strPtr("content_block_delta")},
				}
			case "invalid JSON data":
				frames = []sseFrame{
					frame(t, `{"type":"message_start","message":{}}`),
					{data: `{not-json`},
				}
			}
			if _, err := runFrames(t, frames); err == nil {
				t.Fatalf("protocol violation %q accepted", tt.name)
			} else {
				var modelErr *model.ModelError
				if !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorProtocol {
					t.Fatalf("violation %q: err = %v, want protocol", tt.name, err)
				}
			}
		})
	}
}

// TestConsumeSSEInBandErrorEvent covers the HTTP-200 error event and the
// classification upgrade for overloaded errors
// (http.rs test in_band_overloaded_error_is_retryable_without_status).
func TestConsumeSSEInBandErrorEvent(t *testing.T) {
	_, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}`),
	})
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) {
		t.Fatalf("err = %v, want ModelError", err)
	}
	if modelErr.Kind != model.ErrorProviderUnavailable || !modelErr.Retryable {
		t.Fatalf("kind = %s retryable = %v, want retryable provider unavailable", modelErr.Kind, modelErr.Retryable)
	}
	if modelErr.StatusCode != 0 {
		t.Fatalf("in-band status must not be fabricated, got %d", modelErr.StatusCode)
	}

	// A plain error event without a recognizable upgrade stays a protocol
	// error with the upstream message.
	_, err = runFrames(t, []sseFrame{
		frame(t, `{"type":"error","error":{"type":"api_error","message":"内部错误"}}`),
	})
	if !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorProtocol {
		t.Fatalf("plain in-band error = %v, want protocol", err)
	}
	if !strings.Contains(modelErr.Message, "内部错误") {
		t.Fatalf("message = %q, want upstream text", modelErr.Message)
	}
}

// TestConsumeSSEStopReasonNormalization covers the v1 mapping: recognized
// reasons map directly, and unrecognized reasons (refusal, pause_turn,
// missing) fail the stream instead of ending it (docs/go-migration.md §5.1).
func TestConsumeSSEStopReasonNormalization(t *testing.T) {
	tests := []struct {
		name      string
		reason    string // "" omits stop_reason entirely
		wantEnd   bool
		wantStop  model.StopReason
		wantError bool
	}{
		{name: "end_turn", reason: "end_turn", wantEnd: true, wantStop: model.StopEndTurn},
		{name: "stop_sequence", reason: "stop_sequence", wantEnd: true, wantStop: model.StopEndTurn},
		{name: "tool_use", reason: "tool_use", wantEnd: true, wantStop: model.StopToolUse},
		{name: "max_tokens", reason: "max_tokens", wantEnd: true, wantStop: model.StopMaxTokens},
		{name: "context window exceeded", reason: "model_context_window_exceeded", wantEnd: true, wantStop: model.StopMaxTokens},
		{name: "refusal is unrecognized in v1", reason: "refusal", wantError: true},
		{name: "missing stop reason", reason: "", wantError: true},
		{name: "unknown reason", reason: "server_error", wantError: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			deltaLine := ""
			if tt.reason != "" {
				deltaLine = `{"type":"message_delta","delta":{"stop_reason":"` + tt.reason + `"}}`
			}
			frames := []sseFrame{
				frame(t, `{"type":"message_start","message":{}}`),
				frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"内容"}}`),
				frame(t, `{"type":"content_block_stop","index":0}`),
			}
			if deltaLine != "" {
				frames = append(frames, frame(t, deltaLine))
			}
			frames = append(frames, frame(t, `{"type":"message_stop"}`))

			adapter := newStreamAdapter()
			var events []model.StreamEvent
			var stopErr error
			for _, f := range frames {
				if err := adapter.consumeSSE(f, &events); err != nil {
					stopErr = err
					break
				}
			}
			if stopErr == nil {
				stopErr = adapter.finishStream()
			}
			if tt.wantError {
				if stopErr == nil {
					t.Fatalf("reason %q accepted as end", tt.reason)
				}
				return
			}
			if stopErr != nil {
				t.Fatalf("reason %q: %v", tt.reason, stopErr)
			}
			last := events[len(events)-1]
			if last.Type != model.EventMessageEnd || last.StopReason != tt.wantStop {
				t.Fatalf("final event = %#v, want end %s", last, tt.wantStop)
			}
		})
	}
}

// TestConsumeSSEPauseTurnWithToolCall covers the server pause guard
// (messages.rs:996-1001).
func TestConsumeSSEPauseTurnWithToolCall(t *testing.T) {
	_, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":"bash","input":{}}}`),
		frame(t, `{"type":"content_block_stop","index":0}`),
		frame(t, `{"type":"message_delta","delta":{"stop_reason":"pause_turn"}}`),
		frame(t, `{"type":"message_stop"}`),
	})
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) {
		t.Fatalf("err = %v, want ModelError", err)
	}
	if !strings.Contains(modelErr.Message, "暂停") {
		t.Fatalf("message = %q, want pause semantics", modelErr.Message)
	}
}

// TestFinishStreamRequiresMessageStop covers the terminal-state check.
func TestFinishStreamRequiresMessageStop(t *testing.T) {
	adapter := newStreamAdapter()
	var events []model.StreamEvent
	if err := adapter.consumeSSE(frame(t, `{"type":"message_start","message":{}}`), &events); err != nil {
		t.Fatalf("consumeSSE: %v", err)
	}
	if err := adapter.finishStream(); err == nil {
		t.Fatalf("stream closed before message_stop accepted")
	}
}

// TestDecodeJSONResponse covers the buffered JSON decode path including the
// complete content conversion and the error object check.
func TestDecodeJSONResponse(t *testing.T) {
	body := `{
		"id": "msg_2",
		"model": "test-model",
		"content": [
			{"type": "thinking", "thinking": "想一下", "signature": "sig"},
			{"type": "text", "text": "答案"},
			{"type": "tool_use", "id": "call-9", "name": "read", "input": {"path": "a.go"}}
		],
		"stop_reason": "tool_use",
		"usage": {"input_tokens": 11, "output_tokens": 4, "cache_read_input_tokens": 6}
	}`
	adapter := newStreamAdapter()
	events, err := adapter.decodeJSON([]byte(body))
	if err != nil {
		t.Fatalf("decodeJSON: %v", err)
	}
	var continuation string
	var sawToolStart, sawToolArgs, sawToolEnd bool
	for _, event := range events {
		switch event.Type {
		case model.EventReasoningContinuation:
			continuation = event.Continuation
		case model.EventToolCallStart:
			sawToolStart = event.CallID == "call-9" && event.Name == "read"
		case model.EventToolCallArgsDelta:
			sawToolArgs = event.Delta == `{"path":"a.go"}`
		case model.EventToolCallEnd:
			sawToolEnd = event.CallID == "call-9"
		}
	}
	if continuation == "" {
		t.Fatalf("signature continuation missing: %#v", events)
	}
	kind, data, err := decodeContinuationEnvelope(continuation)
	if err != nil || kind != signatureStateKind || string(data) != `"sig"` {
		t.Fatalf("continuation = (%s, %s, %v)", kind, data, err)
	}
	if !sawToolStart || !sawToolArgs || !sawToolEnd {
		t.Fatalf("tool call events incomplete: %#v", events)
	}
	last := events[len(events)-1]
	if last.Type != model.EventMessageEnd || last.StopReason != model.StopToolUse {
		t.Fatalf("final event = %#v", last)
	}

	// Error-shaped responses classify instead of decoding.
	adapter = newStreamAdapter()
	if _, err := adapter.decodeJSON([]byte(`{"type":"error","error":{"type":"authentication_error","message":"bad key"}}`)); err == nil {
		t.Fatalf("error response decoded as content")
	}

	// Unknown complete content blocks fail closed.
	adapter = newStreamAdapter()
	if _, err := adapter.decodeJSON([]byte(`{"content":[{"type":"document","source":{}}],"stop_reason":"end_turn"}`)); err == nil {
		t.Fatalf("unknown block accepted")
	}
}

// TestConsumeSSERequiresStringFields covers the required-field protocol
// errors for malformed frames.
func TestConsumeSSERequiresStringFields(t *testing.T) {
	_, err := runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"text"}}`),
	})
	if err == nil {
		t.Fatalf("text block without text accepted")
	}
	_, err = runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"content_block_start","index":"zero","content_block":{"type":"text","text":"x"}}`),
	})
	if err == nil {
		t.Fatalf("non-integer index accepted")
	}
	_, err = runFrames(t, []sseFrame{
		frame(t, `{"type":"message_start","message":{}}`),
		frame(t, `{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":"bash"}}`),
	})
	if err == nil {
		t.Fatalf("tool_use without input accepted")
	}
}
