package model

import (
	"encoding/json"
	"errors"
	"reflect"
	"testing"
)

func TestStopReasonIsValid(t *testing.T) {
	tests := []struct {
		reason StopReason
		valid  bool
	}{
		{StopEndTurn, true},
		{StopToolUse, true},
		{StopMaxTokens, true},
		{StopContentFilter, true},
		{StopCancelled, true},
		{"", false},
		{"refusal", false},
		{"server_error", false},
	}
	for _, tt := range tests {
		t.Run(string(tt.reason), func(t *testing.T) {
			if got := tt.reason.IsValid(); got != tt.valid {
				t.Fatalf("IsValid(%q) = %v, want %v", tt.reason, got, tt.valid)
			}
		})
	}
}

func TestStopReasonPermitsTruncatedTail(t *testing.T) {
	tests := []struct {
		reason  StopReason
		permits bool
	}{
		{StopEndTurn, false},
		{StopToolUse, false},
		{StopMaxTokens, true},
		{StopContentFilter, true},
		{StopCancelled, true},
	}
	for _, tt := range tests {
		t.Run(string(tt.reason), func(t *testing.T) {
			if got := tt.reason.permitsTruncatedTail(); got != tt.permits {
				t.Fatalf("permitsTruncatedTail(%q) = %v, want %v", tt.reason, got, tt.permits)
			}
		})
	}
}

func TestUnknownUsageIsNotReported(t *testing.T) {
	usage := UnknownUsage()
	if usage.IsReported() {
		t.Fatalf("unknown usage reported as reported: %+v", usage)
	}
	if usage != (TokenUsage{InputTokens: -1, OutputTokens: -1, CachedTokens: -1}) {
		t.Fatalf("unknown usage = %+v, want all -1", usage)
	}
	if (TokenUsage{}).IsReported() != true {
		t.Fatalf("zero usage should count as explicitly reported zeros")
	}
}

func TestTokenUsageJSONRoundTrip(t *testing.T) {
	tests := []struct {
		name     string
		usage    TokenUsage
		wantWire string
	}{
		{
			name:     "unknown fields stay absent",
			usage:    UnknownUsage(),
			wantWire: `{}`,
		},
		{
			name:     "explicit zero distinct from unknown",
			usage:    TokenUsage{},
			wantWire: `{"inputTokens":0,"outputTokens":0,"cachedTokens":0}`,
		},
		{
			name:     "partial snapshot",
			usage:    TokenUsage{InputTokens: 120, OutputTokens: -1, CachedTokens: 30},
			wantWire: `{"inputTokens":120,"cachedTokens":30}`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data, err := json.Marshal(tt.usage)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			if string(data) != tt.wantWire {
				t.Fatalf("wire = %s, want %s", data, tt.wantWire)
			}
			var decoded TokenUsage
			if err := json.Unmarshal(data, &decoded); err != nil {
				t.Fatalf("unmarshal: %v", err)
			}
			if decoded != tt.usage {
				t.Fatalf("round trip = %+v, want %+v", decoded, tt.usage)
			}
		})
	}
}

func TestTokenUsageUnmarshalRejectsBelowMinusOne(t *testing.T) {
	var usage TokenUsage
	if err := json.Unmarshal([]byte(`{"inputTokens":-2}`), &usage); err == nil {
		t.Fatalf("expected error for -2 input tokens")
	}
}

func TestTokenUsageUpdateFrom(t *testing.T) {
	tests := []struct {
		name  string
		base  TokenUsage
		newer TokenUsage
		want  TokenUsage
	}{
		{
			name:  "newer fields override",
			base:  TokenUsage{InputTokens: 10, OutputTokens: -1, CachedTokens: -1},
			newer: TokenUsage{InputTokens: 50, OutputTokens: 7, CachedTokens: -1},
			want:  TokenUsage{InputTokens: 50, OutputTokens: 7, CachedTokens: -1},
		},
		{
			name:  "newer unknown keeps old",
			base:  TokenUsage{InputTokens: 10, OutputTokens: 5, CachedTokens: 2},
			newer: UnknownUsage(),
			want:  TokenUsage{InputTokens: 10, OutputTokens: 5, CachedTokens: 2},
		},
		{
			name:  "unknown base takes reported newer",
			base:  UnknownUsage(),
			newer: TokenUsage{InputTokens: -1, OutputTokens: 3, CachedTokens: -1},
			want:  TokenUsage{InputTokens: -1, OutputTokens: 3, CachedTokens: -1},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			base := tt.base
			base.UpdateFrom(tt.newer)
			if base != tt.want {
				t.Fatalf("UpdateFrom = %+v, want %+v", base, tt.want)
			}
		})
	}
}

func TestStreamEventJSONRoundTrip(t *testing.T) {
	tests := []struct {
		name  string
		event StreamEvent
	}{
		{
			name:  "message start",
			event: StreamEvent{Type: EventMessageStart},
		},
		{
			name:  "text delta",
			event: StreamEvent{Type: EventTextDelta, Index: 0, Delta: "你好"},
		},
		{
			name:  "reasoning delta",
			event: StreamEvent{Type: EventReasoningDelta, Index: 1, Delta: "思考"},
		},
		{
			name:  "reasoning continuation",
			event: StreamEvent{Type: EventReasoningContinuation, Index: 1, Continuation: "opaque-sig"},
		},
		{
			name:  "tool call start",
			event: StreamEvent{Type: EventToolCallStart, Index: 2, CallID: "call-1", Name: "read_file"},
		},
		{
			name:  "tool call args delta",
			event: StreamEvent{Type: EventToolCallArgsDelta, Index: 2, CallID: "call-1", Delta: `{"path":`},
		},
		{
			name:  "tool call end",
			event: StreamEvent{Type: EventToolCallEnd, Index: 2, CallID: "call-1"},
		},
		{
			name:  "usage",
			event: StreamEvent{Type: EventUsage, Usage: TokenUsage{InputTokens: 12, OutputTokens: -1, CachedTokens: -1}},
		},
		{
			name:  "message end",
			event: StreamEvent{Type: EventMessageEnd, StopReason: StopToolUse},
		},
		{
			name:  "error",
			event: StreamEvent{Type: EventError, Err: errors.New("HTTP 429")},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data, err := json.Marshal(tt.event)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			var decoded StreamEvent
			if err := json.Unmarshal(data, &decoded); err != nil {
				t.Fatalf("unmarshal %s: %v", data, err)
			}
			if decoded.Type != tt.event.Type {
				t.Fatalf("type = %q, want %q", decoded.Type, tt.event.Type)
			}
			if decoded.Index != tt.event.Index || decoded.Delta != tt.event.Delta ||
				decoded.Continuation != tt.event.Continuation || decoded.CallID != tt.event.CallID ||
				decoded.Name != tt.event.Name || decoded.StopReason != tt.event.StopReason {
				t.Fatalf("payload mismatch:\n want %+v\n got  %+v", tt.event, decoded)
			}
			if tt.event.Usage.IsReported() && decoded.Usage != tt.event.Usage {
				t.Fatalf("usage = %+v, want %+v", decoded.Usage, tt.event.Usage)
			}
			if tt.event.Err != nil {
				if decoded.Err == nil || decoded.Err.Error() != tt.event.Err.Error() {
					t.Fatalf("error = %v, want message %q", decoded.Err, tt.event.Err.Error())
				}
			}
		})
	}
}

func TestStreamEventWireOmitsIrrelevantPayloads(t *testing.T) {
	data, err := json.Marshal(StreamEvent{Type: EventTextDelta, Index: 0, Delta: "文"})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var wire map[string]json.RawMessage
	if err := json.Unmarshal(data, &wire); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	for _, key := range []string{"callId", "name", "usage", "stopReason", "error", "continuation"} {
		if _, ok := wire[key]; ok {
			t.Fatalf("text delta wire should omit %q: %s", key, data)
		}
	}
	if _, ok := wire["type"]; !ok {
		t.Fatalf("wire missing type: %s", data)
	}
}

func TestModelResponseJSONRoundTrip(t *testing.T) {
	response := ModelResponse{
		Content: []ContentBlock{
			ReasoningBlock{Text: "推理", Signature: "sig"},
			TextBlock{Text: "回答"},
			ToolCallBlock{Call: ToolCall{ID: "c1", Name: "bash", Arguments: `{"command":"ls"}`}},
		},
		StopReason: StopToolUse,
		Usage:      TokenUsage{InputTokens: 100, OutputTokens: 20, CachedTokens: -1},
		Model:      "test-model",
	}
	data, err := json.Marshal(response)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var decoded ModelResponse
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if !reflect.DeepEqual(decoded, response) {
		t.Fatalf("round trip mismatch:\n want %+v\n got  %+v", response, decoded)
	}
}
