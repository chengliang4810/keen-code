package model

import (
	"context"
	"errors"
	"reflect"
	"testing"
	"time"
)

// fakeProvider replays a scripted event list and records nothing; it stands
// in for a protocol adapter in collector tests (same idea as the Rust
// ScriptedProvider, core/model/src/scripted.rs).
type fakeProvider struct {
	caps      Capabilities
	streamErr error
	events    []StreamEvent
}

func (f *fakeProvider) Capabilities(string) Capabilities { return f.caps }

func (f *fakeProvider) Stream(context.Context, ModelRequest) (<-chan StreamEvent, error) {
	if f.streamErr != nil {
		return nil, f.streamErr
	}
	events := make(chan StreamEvent, len(f.events))
	for _, event := range f.events {
		events <- event
	}
	close(events)
	return events, nil
}

// blockingProvider never emits events; used for cancellation tests.
type blockingProvider struct {
	events chan StreamEvent
}

func (b blockingProvider) Capabilities(string) Capabilities { return Capabilities{} }

func (b blockingProvider) Stream(context.Context, ModelRequest) (<-chan StreamEvent, error) {
	return b.events, nil
}

func testRequest() ModelRequest {
	return ModelRequest{
		Model:    "test-model",
		Messages: []Message{TextMessage(RoleUser, "问题")},
	}
}

// event builders keep the scripted sequences readable.
func msgStart() StreamEvent { return StreamEvent{Type: EventMessageStart} }
func text(index uint32, s string) StreamEvent {
	return StreamEvent{Type: EventTextDelta, Index: index, Delta: s}
}
func reasoning(index uint32, s string) StreamEvent {
	return StreamEvent{Type: EventReasoningDelta, Index: index, Delta: s}
}
func continuation(index uint32, sig string) StreamEvent {
	return StreamEvent{Type: EventReasoningContinuation, Index: index, Continuation: sig}
}
func toolStart(index uint32, id, name string) StreamEvent {
	return StreamEvent{Type: EventToolCallStart, Index: index, CallID: id, Name: name}
}
func toolArgs(index uint32, id, delta string) StreamEvent {
	return StreamEvent{Type: EventToolCallArgsDelta, Index: index, CallID: id, Delta: delta}
}
func toolEnd(index uint32, id string) StreamEvent {
	return StreamEvent{Type: EventToolCallEnd, Index: index, CallID: id}
}
func usageEvent(u TokenUsage) StreamEvent { return StreamEvent{Type: EventUsage, Usage: u} }
func msgEnd(reason StopReason) StreamEvent {
	return StreamEvent{Type: EventMessageEnd, StopReason: reason}
}

func TestCompleteAssemblesEventSequence(t *testing.T) {
	provider := &fakeProvider{
		events: []StreamEvent{
			msgStart(),
			text(0, "你好"),
			text(0, "，世界"),
			reasoning(1, "想一想"),
			continuation(1, "opaque-signature"),
			toolStart(2, "call-1", "read_file"),
			toolArgs(2, "call-1", `{"path":"ma`),
			toolArgs(2, "call-1", `in.go"}`),
			toolEnd(2, "call-1"),
			usageEvent(TokenUsage{InputTokens: 100, OutputTokens: -1, CachedTokens: -1}),
			usageEvent(TokenUsage{InputTokens: -1, OutputTokens: 21, CachedTokens: 5}),
			msgEnd(StopToolUse),
		},
	}
	response, err := Complete(context.Background(), provider, testRequest())
	if err != nil {
		t.Fatalf("Complete: %v", err)
	}
	want := ModelResponse{
		Content: []ContentBlock{
			TextBlock{Text: "你好，世界"},
			ReasoningBlock{Text: "想一想", Signature: "opaque-signature"},
			ToolCallBlock{Call: ToolCall{ID: "call-1", Name: "read_file", Arguments: `{"path":"main.go"}`}},
		},
		StopReason: StopToolUse,
		Usage:      TokenUsage{InputTokens: 100, OutputTokens: 21, CachedTokens: 5},
		Model:      "test-model",
	}
	if !reflect.DeepEqual(response, want) {
		t.Fatalf("response mismatch:\n want %+v\n got  %+v", want, response)
	}
}

func TestCompleteKeepsBlocksInIndexOrder(t *testing.T) {
	provider := &fakeProvider{
		events: []StreamEvent{
			msgStart(),
			toolStart(2, "call-b", "bash"),
			toolArgs(2, "call-b", "{}"),
			toolEnd(2, "call-b"),
			text(0, "正文"),
			reasoning(1, "推理"),
			msgEnd(StopToolUse),
		},
	}
	response, err := Complete(context.Background(), provider, testRequest())
	if err != nil {
		t.Fatalf("Complete: %v", err)
	}
	wantTypes := []string{BlockTypeText, BlockTypeReasoning, BlockTypeToolCall}
	if len(response.Content) != len(wantTypes) {
		t.Fatalf("content length = %d, want %d", len(response.Content), len(wantTypes))
	}
	for i, block := range response.Content {
		if block.BlockType() != wantTypes[i] {
			t.Fatalf("content[%d] = %s, want %s", i, block.BlockType(), wantTypes[i])
		}
	}
}

func TestCompleteRejectsEventSequenceViolations(t *testing.T) {
	tests := []struct {
		name   string
		events []StreamEvent
	}{
		{
			name:   "content before start",
			events: []StreamEvent{text(0, "早到")},
		},
		{
			name:   "second start",
			events: []StreamEvent{msgStart(), msgStart()},
		},
		{
			name:   "events after end",
			events: []StreamEvent{msgStart(), msgEnd(StopEndTurn), text(0, "迟到")},
		},
		{
			name:   "index reused for another content type",
			events: []StreamEvent{msgStart(), text(0, "文本"), reasoning(0, "推理")},
		},
		{
			name:   "duplicate tool call id",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), toolStart(1, "call-1", "bash")},
		},
		{
			name:   "same index started twice",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), toolStart(0, "call-2", "bash")},
		},
		{
			name:   "args before start of the call",
			events: []StreamEvent{msgStart(), toolArgs(0, "call-1", "{}")},
		},
		{
			name:   "args id mismatch",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), toolArgs(0, "call-2", "{}")},
		},
		{
			name:   "args after end",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), toolEnd(0, "call-1"), toolArgs(0, "call-1", "{}")},
		},
		{
			name:   "double tool end",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), toolEnd(0, "call-1"), toolEnd(0, "call-1")},
		},
		{
			name:   "empty tool id",
			events: []StreamEvent{msgStart(), toolStart(0, " ", "bash")},
		},
		{
			name:   "duplicate reasoning continuation",
			events: []StreamEvent{msgStart(), reasoning(0, "推理"), continuation(0, "sig"), continuation(0, "sig2")},
		},
		{
			name:   "empty continuation",
			events: []StreamEvent{msgStart(), continuation(0, " ")},
		},
		{
			name:   "empty text block at end",
			events: []StreamEvent{msgStart(), text(0, ""), msgEnd(StopEndTurn)},
		},
		{
			name:   "reasoning payload without text or signature",
			events: []StreamEvent{msgStart(), StreamEvent{Type: EventReasoningSummaryDelta, Index: 0, Delta: "摘要"}, msgEnd(StopEndTurn)},
		},
		{
			name:   "unknown stop reason must not end a response",
			events: []StreamEvent{msgStart(), msgEnd("refusal")},
		},
		{
			name:   "unknown event type",
			events: []StreamEvent{msgStart(), StreamEvent{Type: "decode_timing"}},
		},
		{
			name:   "unterminated tool call without truncation reason",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), msgEnd(StopEndTurn)},
		},
		{
			name:   "invalid tool json without truncation reason",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), toolArgs(0, "call-1", `{"p`), toolEnd(0, "call-1"), msgEnd(StopEndTurn)},
		},
		{
			name:   "non object tool json",
			events: []StreamEvent{msgStart(), toolStart(0, "call-1", "bash"), toolArgs(0, "call-1", `[1,2]`), toolEnd(0, "call-1"), msgEnd(StopToolUse)},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			provider := &fakeProvider{events: tt.events}
			_, err := Complete(context.Background(), provider, testRequest())
			var modelErr *ModelError
			if !errors.As(err, &modelErr) {
				t.Fatalf("expected *ModelError, got %v", err)
			}
			if modelErr.Kind != ErrorProtocol {
				t.Fatalf("kind = %s (%s), want protocol", modelErr.Kind, modelErr.Message)
			}
		})
	}
}

func TestCompleteToleratesTruncatedToolTail(t *testing.T) {
	for _, reason := range []StopReason{StopMaxTokens, StopContentFilter, StopCancelled} {
		t.Run(string(reason), func(t *testing.T) {
			provider := &fakeProvider{
				events: []StreamEvent{
					msgStart(),
					text(0, "部分回答"),
					toolStart(1, "call-1", "bash"),
					toolArgs(1, "call-1", `{"command":"cu`),
					msgEnd(reason),
				},
			}
			response, err := Complete(context.Background(), provider, testRequest())
			if err != nil {
				t.Fatalf("Complete: %v", err)
			}
			want := []ContentBlock{TextBlock{Text: "部分回答"}}
			if !reflect.DeepEqual(response.Content, want) {
				t.Fatalf("content = %+v, want %+v", response.Content, want)
			}
			if response.StopReason != reason {
				t.Fatalf("stop reason = %s, want %s", response.StopReason, reason)
			}
		})
	}
}

func TestCompleteNormalizesEmptyToolArguments(t *testing.T) {
	provider := &fakeProvider{
		events: []StreamEvent{
			msgStart(),
			toolStart(0, "call-1", "list_dir"),
			toolEnd(0, "call-1"),
			msgEnd(StopToolUse),
		},
	}
	response, err := Complete(context.Background(), provider, testRequest())
	if err != nil {
		t.Fatalf("Complete: %v", err)
	}
	call := response.Content[0].(ToolCallBlock).Call
	if call.Arguments != "{}" {
		t.Fatalf("arguments = %q, want {}", call.Arguments)
	}
}

func TestCompleteStreamInterruptionCarriesPartialText(t *testing.T) {
	provider := &fakeProvider{
		events: []StreamEvent{
			msgStart(),
			reasoning(0, "推理文本"),
			text(1, "正文文本"),
		},
	}
	_, err := Complete(context.Background(), provider, testRequest())
	var modelErr *ModelError
	if !errors.As(err, &modelErr) {
		t.Fatalf("expected *ModelError, got %v", err)
	}
	if modelErr.Kind != ErrorStreamInterrupted || !modelErr.IsRetryable() {
		t.Fatalf("expected retryable stream interruption, got %+v", modelErr)
	}
	if got := modelErr.StreamPartialText(); got != "推理文本正文文本" {
		t.Fatalf("partial text = %q, want 推理文本正文文本", got)
	}
}

func TestCompleteCloseBeforeStart(t *testing.T) {
	provider := &fakeProvider{events: nil}
	_, err := Complete(context.Background(), provider, testRequest())
	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Kind != ErrorStreamInterrupted {
		t.Fatalf("expected stream interruption, got %v", err)
	}
	if modelErr.StreamPartialText() != "" {
		t.Fatalf("no partial text expected before start, got %q", modelErr.StreamPartialText())
	}
}

func TestCompletePropagatesStreamError(t *testing.T) {
	want := &ModelError{Kind: ErrorRateLimited, Message: "请求受限", RetryAfterMS: 800}
	provider := &fakeProvider{
		events: []StreamEvent{
			msgStart(),
			text(0, "开头"),
			{Type: EventError, Err: want},
		},
	}
	_, err := Complete(context.Background(), provider, testRequest())
	if !errors.Is(err, want) && !errors.As(err, &want) {
		t.Fatalf("expected the propagated error, got %v", err)
	}
}

func TestCompleteStreamValidationFailureReturnsImmediately(t *testing.T) {
	want := InvalidRequest("模型标识不能为空")
	provider := &fakeProvider{streamErr: want}
	_, err := Complete(context.Background(), provider, testRequest())
	if !errors.Is(err, want) {
		t.Fatalf("expected the validation error, got %v", err)
	}
}

func TestCompleteCancelReturnsCancelledError(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	provider := blockingProvider{events: make(chan StreamEvent)}
	done := make(chan error, 1)
	go func() {
		_, err := Complete(ctx, provider, testRequest())
		done <- err
	}()
	cancel()
	select {
	case err := <-done:
		var modelErr *ModelError
		if !errors.As(err, &modelErr) || modelErr.Kind != ErrorCancelled {
			t.Fatalf("expected cancelled error, got %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("Complete did not return after cancellation")
	}
}

func TestCompleteForwardsCapabilitiesIndependently(t *testing.T) {
	provider := &fakeProvider{caps: Capabilities{
		Reasoning:        true,
		ReasoningEfforts: []string{ReasoningEffortLow, ReasoningEffortHigh},
		ContextWindow:    128000,
		MaxOutputTokens:  8192,
	}}
	caps := provider.Capabilities("test-model")
	if !caps.Reasoning || caps.ContextWindow != 128000 || caps.MaxOutputTokens != 8192 || len(caps.ReasoningEfforts) != 2 {
		t.Fatalf("unexpected capabilities: %+v", caps)
	}
}
