package runtime

import (
	"time"

	"keencode/internal/model"
)

// EventType discriminates the unified timeline events of one session
// (docs/go-migration.md §5.4). The set mirrors the planned model.Event
// kinds: the runtime owns persistence and delivery, the agent loop owns
// production, and cmd/app adapters map model.Event onto these values 1:1.
type EventType string

const (
	// EventUserMessage marks one user utterance submitted via Send.
	EventUserMessage EventType = "user_message"
	// EventTextDelta carries an incremental assistant text chunk.
	EventTextDelta EventType = "text_delta"
	// EventReasoningDelta carries an incremental reasoning chunk.
	EventReasoningDelta EventType = "reasoning_delta"
	// EventReasoningContinuation carries the opaque reasoning signature. The
	// text is persisted verbatim and folded back into the ReasoningBlock
	// signature when history is rebuilt; without it, providers that require
	// signed thinking blocks reject follow-up turns after a restart.
	EventReasoningContinuation EventType = "reasoning_continuation"
	// EventToolStart marks the beginning of one tool call.
	EventToolStart EventType = "tool_start"
	// EventToolArgs carries an incremental tool argument JSON chunk for the
	// ToolEvent.CallID of the same event.
	EventToolArgs EventType = "tool_args"
	// EventToolEnd closes argument transport of one tool call.
	EventToolEnd EventType = "tool_end"
	// EventToolResult carries a completed tool execution. Event.Text holds
	// the full model-facing result content; the ToolEvent projection holds
	// the card summary and preview.
	EventToolResult EventType = "tool_result"
	// EventUsage carries a token usage snapshot for the turn.
	EventUsage EventType = "usage"
	// EventTurnCompleted terminates a turn successfully.
	EventTurnCompleted EventType = "turn_completed"
	// EventTurnFailed terminates a turn with a failure; Event.Text explains.
	EventTurnFailed EventType = "turn_failed"
	// EventTurnCancelled terminates a turn cancelled by the user.
	EventTurnCancelled EventType = "turn_cancelled"
	// EventPermissionDenied reports a rejected side-effect tool run.
	EventPermissionDenied EventType = "permission_denied"
)

// Tool call card states carried by ToolEvent.Status
// (docs/go-migration.md §5.4).
const (
	ToolStatusPending   = "pending"
	ToolStatusRunning   = "running"
	ToolStatusCompleted = "completed"
	ToolStatusFailed    = "failed"
	ToolStatusDenied    = "denied"
	ToolStatusStopped   = "stopped"
)

// ToolEvent is the UI card projection of one tool call
// (docs/go-migration.md §5.4).
type ToolEvent struct {
	// CallID corresponds to model.ToolCall.ID.
	CallID string `json:"callId,omitempty"`
	// Name is the invoked tool name.
	Name string `json:"name,omitempty"`
	// Summary is the one-line card summary.
	Summary string `json:"summary,omitempty"`
	// Status is one of the ToolStatus constants.
	Status string `json:"status,omitempty"`
	// Detail is the expanded detail (command, path, output preview).
	Detail string `json:"detail,omitempty"`
	// AddedLines and RemovedLines count Edit/Write changes.
	AddedLines   int `json:"addedLines,omitempty"`
	RemovedLines int `json:"removedLines,omitempty"`
}

// Event is one unified timeline event of a session. The field set mirrors
// the planned model.Event (docs/go-migration.md §5.4); the runtime envelope
// persists the identity fields and the payload fields separately
// (internal/runtime/journal.go).
type Event struct {
	// ID is the idempotent journal identity: re-appending the same ID with
	// the same payload is a no-op, on disk a duplicate ID is corruption.
	ID string `json:"id,omitempty"`
	// SessionID identifies the owning session; assigned by the runtime.
	SessionID string `json:"session,omitempty"`
	// TurnID groups the events of one agent turn.
	TurnID string `json:"turnId,omitempty"`
	// Seq is the journal sequence: strictly increasing from 1 across the
	// whole session stream, assigned by the journal on append. Replay and
	// live delivery share one continuous sequence space, so consumers can
	// deduplicate on it.
	Seq int64 `json:"sequence,omitempty"`
	// Replay marks history events delivered by Subscribe; live events are
	// never flagged. The flag is transient and never persisted.
	Replay bool `json:"-"`
	// Time is the event wall-clock time, monotonic non-decreasing per
	// session (core/resources/src/journal.rs:1326-1327).
	Time time.Time `json:"-"`
	// Type discriminates the event kind.
	Type EventType `json:"type,omitempty"`
	// Text carries delta text, error and failure messages, tool result
	// content, and the opaque reasoning signature.
	Text string `json:"text,omitempty"`
	// Tool is the tool card projection on tool events.
	Tool *ToolEvent `json:"tool,omitempty"`
	// Usage is the token usage snapshot on EventUsage.
	Usage *model.TokenUsage `json:"usage,omitempty"`
	// StopReason terminates a turn on EventTurnCompleted.
	StopReason model.StopReason `json:"stopReason,omitempty"`
}

// clone returns a deep copy so journal history and concurrent subscribers
// never share mutable pointer payloads.
func (e Event) clone() Event {
	out := e
	if e.Tool != nil {
		tool := *e.Tool
		out.Tool = &tool
	}
	if e.Usage != nil {
		usage := *e.Usage
		out.Usage = &usage
	}
	return out
}

// cloneAll deep copies an event slice.
func cloneAll(events []Event) []Event {
	out := make([]Event, len(events))
	for i, e := range events {
		out[i] = e.clone()
	}
	return out
}

// isTerminalTurnEvent reports whether the type ends a turn.
func (t EventType) isTerminalTurnEvent() bool {
	return t == EventTurnCompleted || t == EventTurnFailed || t == EventTurnCancelled
}

// syncImmediately reports whether an event of this type must reach stable
// storage before Append returns: user messages and turn terminal events are
// the durable anchors, high-frequency deltas ride the 100ms batch window
// (docs/go-migration.md §5.5).
func (t EventType) syncImmediately() bool {
	return t == EventUserMessage || t.isTerminalTurnEvent()
}
