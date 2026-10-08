package model

import (
	"encoding/json"
	"errors"
)

// StopReason is the unified reason a model ended its response
// (docs/go-migration.md §5.1; Rust StopReason with the Other variant
// removed: adapters must convert unrecognized endpoint reasons into an
// EventError carrying sanitized reason text instead of a MessageEnd).
type StopReason string

const (
	// StopEndTurn reports a normally completed response.
	StopEndTurn StopReason = "end_turn"
	// StopToolUse reports the model requesting one or more tool runs.
	StopToolUse StopReason = "tool_use"
	// StopMaxTokens reports the response hitting the output limit.
	StopMaxTokens StopReason = "max_tokens"
	// StopContentFilter reports the response cut by a content policy.
	StopContentFilter StopReason = "content_filter"
	// StopCancelled reports the response cancelled by the caller.
	StopCancelled StopReason = "cancelled"
)

// IsValid reports whether the reason is one of the five unified values.
func (s StopReason) IsValid() bool {
	switch s {
	case StopEndTurn, StopToolUse, StopMaxTokens, StopContentFilter, StopCancelled:
		return true
	default:
		return false
	}
}

// permitsTruncatedTail reports whether the reason tolerates incomplete
// (truncated) tool call blocks: output-limit cuts, content filters, and
// cancellations may sever argument transport mid-way, so such blocks are
// stripped instead of failing the whole response
// (core/model/src/stream.rs:419-424).
func (s StopReason) permitsTruncatedTail() bool {
	return s == StopMaxTokens || s == StopContentFilter || s == StopCancelled
}

// TokenUsage is the normalized token usage of one model call
// (docs/go-migration.md §5.1). Every field is -1 when the remote endpoint
// did not report it; 0 means the endpoint explicitly reported zero
// (Rust TokenUsage with Option<u64> fields, reduced to the three v1
// counters).
type TokenUsage struct {
	// InputTokens is the total input token count as normalized by the
	// adapter (including cache reads).
	InputTokens int64 `json:"inputTokens"`
	// OutputTokens is the output token count.
	OutputTokens int64 `json:"outputTokens"`
	// CachedTokens is the cache-related token count reported by the
	// endpoint, normalized by the adapter.
	CachedTokens int64 `json:"cachedTokens"`
}

// UnknownUsage returns a usage snapshot with every field not reported.
func UnknownUsage() TokenUsage {
	return TokenUsage{InputTokens: -1, OutputTokens: -1, CachedTokens: -1}
}

// IsReported reports whether at least one field was explicitly reported by
// the remote endpoint (Rust TokenUsage::is_reported).
func (u TokenUsage) IsReported() bool {
	return u.InputTokens != -1 || u.OutputTokens != -1 || u.CachedTokens != -1
}

// UpdateFrom merges a newer snapshot into u: fields reported by the newer
// snapshot override, fields the newer snapshot leaves unknown keep their old
// value (Rust TokenUsage::update_from).
func (u *TokenUsage) UpdateFrom(newer TokenUsage) {
	if newer.InputTokens != -1 {
		u.InputTokens = newer.InputTokens
	}
	if newer.OutputTokens != -1 {
		u.OutputTokens = newer.OutputTokens
	}
	if newer.CachedTokens != -1 {
		u.CachedTokens = newer.CachedTokens
	}
}

// MarshalJSON omits unknown (-1) fields so absent and explicit zero stay
// distinct on the wire, mirroring the Option<u64> Rust serialization
// (core/model/src/tests.rs:78-97).
func (u TokenUsage) MarshalJSON() ([]byte, error) {
	wire := tokenUsageWire{}
	if u.InputTokens != -1 {
		v := u.InputTokens
		wire.InputTokens = &v
	}
	if u.OutputTokens != -1 {
		v := u.OutputTokens
		wire.OutputTokens = &v
	}
	if u.CachedTokens != -1 {
		v := u.CachedTokens
		wire.CachedTokens = &v
	}
	return json.Marshal(wire)
}

// UnmarshalJSON decodes usage; absent fields become unknown (-1). Values
// below -1 are rejected.
func (u *TokenUsage) UnmarshalJSON(data []byte) error {
	var wire tokenUsageWire
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	*u = TokenUsage{InputTokens: -1, OutputTokens: -1, CachedTokens: -1}
	for _, field := range []struct {
		value *int64
		slot  *int64
	}{
		{wire.InputTokens, &u.InputTokens},
		{wire.OutputTokens, &u.OutputTokens},
		{wire.CachedTokens, &u.CachedTokens},
	} {
		if field.value == nil {
			continue
		}
		if *field.value < -1 {
			return InvalidRequest("Token 用量字段不能小于 -1")
		}
		*field.slot = *field.value
	}
	return nil
}

// tokenUsageWire is the JSON shape of TokenUsage with unknown fields absent.
type tokenUsageWire struct {
	InputTokens  *int64 `json:"inputTokens,omitempty"`
	OutputTokens *int64 `json:"outputTokens,omitempty"`
	CachedTokens *int64 `json:"cachedTokens,omitempty"`
}

// StreamEventType discriminates the unified stream events an adapter emits
// (docs/go-migration.md §5.1; Rust ModelStreamEvent reduced: no decode
// timing, no event payload metadata).
type StreamEventType string

const (
	// EventMessageStart marks the beginning of one model response. It must
	// be the first event and appear exactly once.
	EventMessageStart StreamEventType = "message_start"
	// EventTextDelta carries an incremental UTF-8 text chunk.
	EventTextDelta StreamEventType = "text_delta"
	// EventReasoningDelta carries an incremental reasoning chunk.
	EventReasoningDelta StreamEventType = "reasoning_delta"
	// EventReasoningSummaryDelta carries an incremental reasoning summary
	// chunk. Summaries are transient UI state: the v1 ReasoningBlock has no
	// summary field and the agent loop does not journal summary deltas.
	EventReasoningSummaryDelta StreamEventType = "reasoning_summary_delta"
	// EventReasoningContinuation carries the final opaque signature state of
	// one reasoning block for request continuity in later turns. Adapters
	// without a signature mechanism never emit it.
	EventReasoningContinuation StreamEventType = "reasoning_continuation"
	// EventToolCallStart marks the beginning of one tool call.
	EventToolCallStart StreamEventType = "tool_call_start"
	// EventToolCallArgsDelta carries an incremental JSON argument chunk of
	// the tool call started for the same Index.
	EventToolCallArgsDelta StreamEventType = "tool_call_args_delta"
	// EventToolCallEnd marks the end of one tool call's argument transport.
	EventToolCallEnd StreamEventType = "tool_call_end"
	// EventUsage carries a token usage snapshot; later snapshots merge.
	EventUsage StreamEventType = "usage"
	// EventMessageEnd terminates a successful response with a StopReason.
	// Exactly one per response, never preceded by EventError.
	EventMessageEnd StreamEventType = "message_end"
	// EventError reports a stream failure (HTTP, parsing, transport). After
	// it the event channel closes; no further events may follow.
	EventError StreamEventType = "error"
)

// StreamEvent is the flat unified stream event one protocol adapter emits
// toward the agent runtime (docs/go-migration.md §5.1). Only the fields
// relevant to Type carry values; Index is the stable content block index
// within the response.
type StreamEvent struct {
	Type StreamEventType `json:"type"`
	// Index is the stable content block index this event belongs to.
	Index uint32 `json:"index,omitempty"`
	// Delta is the appended text for text/reasoning/summary/argument deltas.
	Delta string `json:"delta,omitempty"`
	// Continuation is the opaque reasoning signature carried by
	// EventReasoningContinuation; persisted and echoed back verbatim.
	Continuation string `json:"continuation,omitempty"`
	// CallID is the response-unique tool call identifier.
	CallID string `json:"callId,omitempty"`
	// Name is the tool name on EventToolCallStart.
	Name string `json:"name,omitempty"`
	// Usage is the usage snapshot on EventUsage.
	Usage TokenUsage `json:"usage,omitempty"`
	// StopReason terminates the response on EventMessageEnd.
	StopReason StopReason `json:"stopReason,omitempty"`
	// Err carries the failure on EventError. The channel closes right after
	// the event.
	Err error `json:"error,omitempty"`
}

// MarshalJSON encodes only the payload fields the event actually carries.
// Err degrades to its message string: stream events cross process boundaries
// in memory (the journal persists model events, not stream events), so typed
// errors do not survive a round trip by design.
func (e StreamEvent) MarshalJSON() ([]byte, error) {
	wire := streamEventWire{Type: e.Type}
	if e.Index != 0 {
		v := e.Index
		wire.Index = &v
	}
	if e.Delta != "" {
		v := e.Delta
		wire.Delta = &v
	}
	if e.Continuation != "" {
		v := e.Continuation
		wire.Continuation = &v
	}
	if e.CallID != "" {
		v := e.CallID
		wire.CallID = &v
	}
	if e.Name != "" {
		v := e.Name
		wire.Name = &v
	}
	if e.Type == EventUsage && e.Usage.IsReported() {
		v := e.Usage
		wire.Usage = &v
	}
	if e.StopReason != "" {
		v := e.StopReason
		wire.StopReason = &v
	}
	if e.Err != nil {
		message := e.Err.Error()
		wire.Err = &message
	}
	return json.Marshal(wire)
}

// UnmarshalJSON decodes an event; an encoded error re-materializes as a plain
// error carrying the original message.
func (e *StreamEvent) UnmarshalJSON(data []byte) error {
	var wire streamEventWire
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	*e = StreamEvent{Type: wire.Type}
	if wire.Index != nil {
		e.Index = *wire.Index
	}
	if wire.Delta != nil {
		e.Delta = *wire.Delta
	}
	if wire.Continuation != nil {
		e.Continuation = *wire.Continuation
	}
	if wire.CallID != nil {
		e.CallID = *wire.CallID
	}
	if wire.Name != nil {
		e.Name = *wire.Name
	}
	if wire.Usage != nil {
		e.Usage = *wire.Usage
	}
	if wire.StopReason != nil {
		e.StopReason = *wire.StopReason
	}
	if wire.Err != nil {
		e.Err = errors.New(*wire.Err)
	}
	return nil
}

// streamEventWire is the JSON shape of StreamEvent with pointer fields so
// irrelevant payloads stay absent instead of collapsing into zero values.
type streamEventWire struct {
	Type         StreamEventType `json:"type"`
	Index        *uint32         `json:"index,omitempty"`
	Delta        *string         `json:"delta,omitempty"`
	Continuation *string         `json:"continuation,omitempty"`
	CallID       *string         `json:"callId,omitempty"`
	Name         *string         `json:"name,omitempty"`
	Usage        *TokenUsage     `json:"usage,omitempty"`
	StopReason   *StopReason     `json:"stopReason,omitempty"`
	Err          *string         `json:"error,omitempty"`
}
