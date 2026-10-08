package agent

import (
	"context"
	"encoding/json"

	"keencode/internal/model"
)

// Effect classifies one tool invocation for permission decisions
// (docs/go-migration.md §5.3; the reduced counterpart of ToolEffect in
// core/agent/src/tool.rs).
type Effect string

const (
	// EffectReadOnly marks an invocation that cannot change external state.
	// The loop executes it without asking for permission.
	EffectReadOnly Effect = "read_only"
	// EffectSideEffect marks an invocation that may change external state.
	// The loop asks Dependencies.Authorize before executing it.
	EffectSideEffect Effect = "side_effect"
)

// Invocation is the execution context handed to Tool.Execute for one call
// (docs/go-migration.md §5.3; the reduced counterpart of ToolContext in
// core/agent/src/tool.rs:142-153). Input is the raw argument JSON exactly as
// the model streamed it.
type Invocation struct {
	// CallID is the model tool call identifier.
	CallID string
	// Name is the invoked tool name.
	Name string
	// Input is the raw JSON object text of the call arguments.
	Input json.RawMessage
	// WorkDir is the project root used to resolve relative paths.
	WorkDir string
	// Emit reports long-running progress (such as command output previews).
	// It is called from the execution goroutine and must not block. The v1
	// loop passes nil because the event vocabulary has no progress kind yet;
	// implementations must tolerate nil.
	Emit func(progress string)
}

// ToolOutput is the text result a tool returns to the loop
// (docs/go-migration.md §5.3).
type ToolOutput struct {
	// Content is the model-facing text. The tool owns budget truncation; the
	// loop forwards it verbatim in the tool_result event.
	Content string
	// IsError marks a failed execution whose Content explains the failure to
	// the model.
	IsError bool
	// Summary is the short card summary shown while collapsed.
	Summary string
}

// Tool is the provider-neutral tool contract the loop executes
// (docs/go-migration.md §5.3; the reduced counterpart of the AgentTool trait
// in core/agent/src/tool.rs:1062-1106 — concurrency, timeout and artifact
// sink knobs are deferred with v1 sequential execution).
//
// Implementations must fail closed: Effect reports EffectSideEffect whenever
// the input cannot be classified, and Execute must return promptly once ctx
// is cancelled. A non-nil error return is normalized into a failed tool
// result; the turn continues.
type Tool interface {
	// Definition returns the name, description and JSON Schema advertised to
	// the model.
	Definition() model.ToolDefinition
	// Effect classifies the call for the given raw input.
	Effect(input json.RawMessage) Effect
	// Execute validates and runs one call. Input is the raw argument JSON.
	Execute(ctx context.Context, inv Invocation) (ToolOutput, error)
}

// ToolRegistry is the read-only view of a tool registry the loop consumes
// (docs/go-migration.md §5.3).
//
// It is an interface rather than *tools.Registry because the concrete tools
// package is implemented in parallel and the loop must not import it. The
// runtime bridges a *tools.Registry with a tiny adapter: Go interfaces are
// nominal, so the structurally identical concrete types do not satisfy this
// interface directly.
type ToolRegistry interface {
	// Definitions returns the tool definitions advertised to the model, in
	// the registry's frozen order.
	Definitions() []model.ToolDefinition
	// Get returns the tool registered under the exact name.
	Get(name string) (Tool, bool)
}
