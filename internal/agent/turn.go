package agent

import (
	"encoding/json"

	"keencode/internal/model"
)

// TurnRequest identifies and seeds one agent turn
// (docs/go-migration.md §5.4; the reduced counterpart of TurnRequest in
// core/agent/src/runner.rs:280-293 — the plan guard and cancellation token of
// the Rust shape collapse into Dependencies.Authorize and the ctx passed to
// RunTurn).
type TurnRequest struct {
	// SessionID is the owning session identifier; echoed onto every event.
	SessionID string
	// TurnID is the unique identifier of this turn; echoed onto every event
	// and used to build event IDs.
	TurnID string
	// Model is the provider-neutral model identifier used for every round.
	Model string
	// History is the persisted conversation so far. It may already start with
	// the system message; see Dependencies.System for the assembly rule.
	History []model.Message
	// WorkDir is the project root handed to every tool invocation for
	// relative path resolution.
	WorkDir string
}

// PermissionRequest is handed to Dependencies.Authorize before a side-effect
// tool call executes (docs/go-migration.md §5.4). The callback may block on a
// native dialog; the loop invokes it from the turn goroutine.
type PermissionRequest struct {
	// SessionID is the owning session identifier.
	SessionID string
	// TurnID is the current turn identifier.
	TurnID string
	// CallID is the model tool call identifier awaiting authorization.
	CallID string
	// ToolName is the requested tool name.
	ToolName string
	// Input is the raw JSON object text of the call arguments.
	Input json.RawMessage
	// Summary is a display-safe one-line description of what the call wants
	// to do, rendered as the dialog body.
	Summary string
}
