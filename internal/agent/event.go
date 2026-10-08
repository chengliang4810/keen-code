package agent

import (
	"time"

	"keencode/internal/model"
)

// EventType discriminates one event on the unified turn timeline
// (docs/go-migration.md §5.4).
//
// The migration plan originally placed this type in the model package so the
// runtime journal could persist it without importing the agent. The completed
// internal/model does not carry it, and sibling packages are implemented in
// parallel, so it lives here instead: the dependency direction runtime →
// agent stays acyclic, and the runtime can alias the type when it lands.
type EventType string

const (
	// EventUserMessage reports a user utterance. The runtime emits it when a
	// turn is accepted; the agent loop itself never produces it. It is
	// declared here so the journal and the UI share one vocabulary.
	EventUserMessage EventType = "user_message"
	// EventTextDelta carries an incremental assistant text chunk in Text.
	EventTextDelta EventType = "text_delta"
	// EventReasoningDelta carries an incremental reasoning chunk in Text.
	EventReasoningDelta EventType = "reasoning_delta"
	// EventReasoningContinuation carries the opaque reasoning signature of the
	// current reasoning block in Text. It must be journaled: replay merges it
	// into the persisted ReasoningBlock.Signature so later requests keep
	// provider-side reasoning continuity.
	EventReasoningContinuation EventType = "reasoning_continuation"
	// EventToolArgs reports that one model tool call finished streaming. The
	// ToolEvent carries Status=ToolStatusPending and Detail=raw input JSON;
	// it creates the tool card.
	EventToolArgs EventType = "tool_args"
	// EventToolStart reports that an authorized tool invocation began
	// executing (ToolEvent.Status=ToolStatusRunning).
	EventToolStart EventType = "tool_start"
	// EventToolEnd reports the terminal outcome of one tool execution in
	// ToolEvent (Status=ToolStatusCompleted or ToolStatusFailed, Summary and
	// a bounded Detail preview filled).
	EventToolEnd EventType = "tool_end"
	// EventToolResult reports the exact result content that was backfilled
	// into history for the model: Text carries the full content and
	// ToolEvent.CallID identifies the call. Replay rebuilds the
	// ToolResultBlock list from these events.
	EventToolResult EventType = "tool_result"
	// EventUsage reports the merged token usage of one model round in Usage.
	EventUsage EventType = "usage"
	// EventPermissionDenied reports that Dependencies.Authorize rejected a
	// side-effect tool call (ToolEvent.Status=ToolStatusDenied).
	EventPermissionDenied EventType = "permission_denied"
	// EventTurnCompleted closes a turn that ended normally
	// (StopReason=model.StopEndTurn).
	EventTurnCompleted EventType = "turn_completed"
	// EventTurnFailed closes a turn after an error; Text carries a
	// display-safe failure description.
	EventTurnFailed EventType = "turn_failed"
	// EventTurnCancelled closes a turn cancelled through its context
	// (StopReason=model.StopCancelled).
	EventTurnCancelled EventType = "turn_cancelled"
)

// Tool event card statuses (docs/go-migration.md §5.4; the same vocabulary as
// the ZCode tool-call summary states).
const (
	// ToolStatusPending marks a call that finished streaming but has not been
	// authorized or started.
	ToolStatusPending = "pending"
	// ToolStatusRunning marks a call whose execution is in flight.
	ToolStatusRunning = "running"
	// ToolStatusCompleted marks a call that produced a successful result.
	ToolStatusCompleted = "completed"
	// ToolStatusFailed marks a call whose execution failed or was rejected by
	// the tool layer.
	ToolStatusFailed = "failed"
	// ToolStatusDenied marks a call the user (or a failing authorization
	// bridge) refused to run.
	ToolStatusDenied = "denied"
	// ToolStatusStopped marks a call abandoned because the turn was cancelled.
	ToolStatusStopped = "stopped"
)

// ToolEvent is the tool-card projection carried by tool-related events
// (docs/go-migration.md §5.4). AddedLines/RemovedLines stay zero in v1: the
// loop does not compute diff statistics.
type ToolEvent struct {
	// CallID is the model tool call identifier this card belongs to.
	CallID string
	// Name is the invoked tool name.
	Name string
	// Summary is the short card summary produced by the tool output.
	Summary string
	// Status is one of the ToolStatus* constants.
	Status string
	// Detail carries card detail text: raw input JSON for EventToolArgs and
	// a bounded output preview for EventToolEnd.
	Detail string
	// AddedLines is the number of added lines (v1: always 0).
	AddedLines int
	// RemovedLines is the number of removed lines (v1: always 0).
	RemovedLines int
}

// Event is one unified timeline event of a session
// (docs/go-migration.md §5.4). The loop fills ID, SessionID, TurnID, Time,
// Type and the payload fields; Seq and Replay are owned by the runtime
// journal/subscription layer and stay zero on live loop events.
type Event struct {
	// ID is the idempotent identity of the event: stable across redelivery,
	// unique within the turn.
	ID string
	// SessionID is the session the event belongs to.
	SessionID string
	// TurnID is the turn the event belongs to.
	TurnID string
	// Seq is the journal sequence number; assigned by the runtime, never by
	// the loop.
	Seq int64
	// Replay marks events redelivered from journal history; live events are
	// always false.
	Replay bool
	// Time is the wall-clock moment the loop produced the event.
	Time time.Time
	// Type discriminates the payload.
	Type EventType
	// Text carries delta text, the reasoning continuation signature, the
	// full tool result content, or a failure message depending on Type.
	Text string
	// Tool carries the tool-card projection for tool-related events.
	Tool *ToolEvent
	// Usage carries the merged token usage for EventUsage.
	Usage *model.TokenUsage
	// StopReason carries the unified stop reason on turn terminal events.
	StopReason model.StopReason
}
