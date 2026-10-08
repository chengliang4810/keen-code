package runtime

import (
	"context"
	"errors"

	"keencode/internal/model"
)

// ErrBusy reports that the session is already running a turn and cannot
// accept another Send (docs/go-migration.md §6: v1 has no send queue).
var ErrBusy = errors.New("会话正在运行回合，请先停止")

// ErrNoAgent reports that the manager was assembled without an agent
// factory, so turns cannot start.
var ErrNoAgent = errors.New("运行时未装配 agent 工厂")

// TurnRequest is the input of one agent turn. The shape mirrors the planned
// agent.TurnRequest (docs/go-migration.md §5.4) so the cmd/app adapter maps
// it field by field.
type TurnRequest struct {
	// SessionID is the owning session.
	SessionID string
	// TurnID identifies this turn in every event it produces.
	TurnID string
	// Model is the model id resolved at Send time from configuration.
	Model string
	// History is the rebuilt conversation: every persisted message
	// reconstructed from the journal. It does not include the system
	// prompt — the TurnRunner's own dependencies carry it (see the
	// TurnRunner comment for the split).
	History []model.Message
	// WorkDir is the project directory tools execute in.
	WorkDir string
}

// TurnRunner executes one complete agent turn. It mirrors the planned
// agent.Agent surface (docs/go-migration.md §5.4: RunTurn(ctx, req, append,
// emit) with model.Event); internal/runtime depends on this interface, not
// on the concrete agent package, so the runtime and the agent loop can be
// implemented and tested independently. cmd/app assembles the real
// implementation and adapts model.Event to runtime.Event (fields align 1:1).
//
// Contract:
//   - The runner streams from its provider, executes tools, and reports
//     progress by calling journal and emit with every event.
//   - journal persists the event before it is delivered; an error aborts
//     the turn. Events must carry a non-empty ID (the idempotent journal
//     identity) and their Type.
//   - emit receives the same events after persistence; an error aborts the
//     turn.
//   - ctx cancellation stops all work; the runner (or the runtime, as a
//     last resort) records a terminal turn event.
//   - The runner must journal exactly one terminal event
//     (EventTurnCompleted, EventTurnFailed or EventTurnCancelled) carrying
//     req.TurnID; the runtime synthesizes a failure when it does not.
type TurnRunner interface {
	// RunTurn executes the turn described by req and returns when the turn
	// reaches a terminal state or ctx is cancelled. A returned error is a
	// turn failure, not a transport detail: the runtime journals it as an
	// EventTurnFailed when the runner has not already done so.
	RunTurn(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error
}

// AgentFactory builds the TurnRunner for one turn. It is called under the
// session lock at Send time and mirrors the plan's deps func()
// agent.Dependencies wiring (docs/go-migration.md §5.7): configuration
// dependent state (provider endpoint, tool registry, system prompt,
// authorize bridge, model id) is captured by the closure, session state is
// passed via TurnRequest.
type AgentFactory func() (TurnRunner, error)

// ModelFunc resolves the model id recorded into TurnRequest at Send time.
type ModelFunc func() string
