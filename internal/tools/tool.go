package tools

import (
	"context"
	"encoding/json"
	"fmt"

	"keencode/internal/model"
)

// Effect classifies what one tool invocation can do to the outside world
// (docs/go-migration.md §5.3; Rust ToolEffect). Permission decisions are
// fail-closed: only tools whose whole class is provably read-only are
// classified as EffectReadOnly.
type Effect string

const (
	// EffectReadOnly marks tools that never mutate external state; the host
	// may execute them without asking the user (Read/Glob/Grep in v1).
	EffectReadOnly Effect = "read_only"
	// EffectSideEffect marks tools that may mutate files, processes or
	// remote state; the host must confirm every call before Execute
	// (Write/Edit/Bash in v1).
	EffectSideEffect Effect = "side_effect"
)

// Invocation describes one tool call handed to Tool.Execute
// (docs/go-migration.md §5.3).
type Invocation struct {
	// CallID is the provider-assigned identifier of the tool call this
	// invocation answers.
	CallID string
	// Name is the registered tool name (diagnostics only; dispatch already
	// happened by the time a concrete Tool sees the input).
	Name string
	// Input is the raw JSON argument object. Registry.ValidateInput has
	// checked it against Definition().InputSchema; every tool still
	// re-parses strictly and rejects unknown fields on its own.
	Input json.RawMessage
	// WorkDir is the project root the host binds this call to. Empty means
	// "the work directory the tool was constructed with". A non-empty value
	// that disagrees with it is rejected (fail closed).
	WorkDir string
	// Emit optionally receives progress notes during long executions (a
	// Bash output preview). It may be nil and v1 built-in tools do not
	// emit; the field exists to keep the plan's invocation shape stable.
	Emit func(progress string)
}

// ToolOutput is the model-facing result of one tool call
// (docs/go-migration.md §5.3).
type ToolOutput struct {
	// Content is the text handed back to the model; producers keep it
	// within the agent layer's output budgets by truncating explicitly.
	Content string
	// IsError marks the result as a failed tool outcome. Model-facing
	// failures travel as results (not Go errors) so the loop can hand the
	// reason back to the model, mirroring the old stack where ToolError
	// becomes an error tool result.
	IsError bool
	// Summary is a short single-line projection for the session tool card
	// (the primary/secondary text contract in docs/go-migration.md §5.4).
	Summary string
}

// Tool is one provider-neutral built-in tool. It is the v1 Go reduction of
// the Rust AgentTool trait (core/agent/src/tool.rs:1062-1105): the per-call
// concurrency class, the artifact-sink port and the per-tool wall-clock
// declaration are not part of the v1 interface; read-only tools enforce
// their own wall clock and every tool keeps failure inside ToolOutput.
type Tool interface {
	// Definition returns the stable name, description and JSON Schema
	// handed to the model.
	Definition() model.ToolDefinition
	// Effect declares the class of this call kind. Implementations must be
	// fail-closed; v1 built-ins return a constant per tool type and do not
	// classify individual inputs.
	Effect(input json.RawMessage) Effect
	// Execute validates and runs one call.
	//
	// Contract: model-facing failures — invalid input, paths outside the
	// workspace sandbox, non-zero exit, timeout, cancellation — are
	// returned as (ToolOutput{IsError: true}, nil) so the agent loop can
	// deliver the reason to the model. A non-nil error is reserved for
	// failures that prevent producing any result at all (a misconfigured
	// tool environment, a host contract violation in Invocation). ctx
	// cancellation is observed at safe points and yields a stable
	// cancellation result; it never aborts without a result.
	Execute(ctx context.Context, inv Invocation) (ToolOutput, error)
}

// Model-facing text shared by all tools (ported from the old stack so the
// model sees one stable vocabulary across tools).
const (
	// emptyReadBody is the stable body for an empty file or an empty
	// selected range (filesystem.rs:36).
	emptyReadBody = "<空文件或所选范围没有内容>"
)

// errResultContent is an internal error whose message is already the final
// model-facing content. Internal helpers return it so Execute can convert
// the whole pipeline into a ToolOutput in one place.
type errResultContent struct{ content string }

func (e *errResultContent) Error() string { return e.content }

// fail builds an internal error carrying final model-facing content.
func fail(content string) error { return &errResultContent{content: content} }

// textOutput builds a successful result.
func textOutput(content, summary string) ToolOutput {
	return ToolOutput{Content: content, Summary: summary}
}

// errorResult builds a failed model-facing result.
func errorResult(content, summary string) ToolOutput {
	return ToolOutput{Content: content, IsError: true, Summary: summary}
}

// ctxResult maps the ctx state to a stable model-facing result. The second
// return is false while ctx is still alive (filesystem.rs:1327-1333 port).
func ctxResult(ctx context.Context, summary string) (ToolOutput, bool) {
	switch ctx.Err() {
	case nil:
		return ToolOutput{}, false
	case context.Canceled:
		return errorResult("工具调用已取消", summary), true
	default: // context.DeadlineExceeded
		return errorResult("工具调用已超时", summary), true
	}
}

// objectSchema builds the strict JSON Schema object every built-in tool
// uses: typed properties, required keys, and additionalProperties closed so
// the registry rejects unknown fields at the boundary.
func objectSchema(properties map[string]any, required ...string) json.RawMessage {
	schema := map[string]any{
		"type":                 "object",
		"properties":           properties,
		"required":             required,
		"additionalProperties": false,
	}
	raw, err := json.Marshal(schema)
	if err != nil {
		// Every schema is a static literal; a marshal failure is a
		// programming error and must surface at the first test run.
		panic(fmt.Sprintf("tools: 工具 Schema 序列化失败：%v", err))
	}
	return raw
}
