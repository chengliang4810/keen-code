package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
	"unicode/utf8"

	"keencode/internal/model"
)

// DefaultMaxRounds is the model-round budget used when Dependencies.MaxRounds
// is not set (docs/go-migration.md §5.4: 兜底轮数，v1 = 32，防失控).
const DefaultMaxRounds = 32

// Display-size bounds applied before payloads leave the agent layer.
const (
	// maxFailureMessageRunes bounds the Text of turn_failed events.
	maxFailureMessageRunes = 800
	// maxToolErrorRunes bounds tool error text sent to the model when the
	// tool layer itself did not produce content.
	maxToolErrorRunes = 2000
	// maxToolDetailRunes bounds the tool_end card preview; the full content
	// always travels on the following tool_result event.
	maxToolDetailRunes = 2000
	// maxPermissionSummaryRunes bounds the authorization dialog summary.
	maxPermissionSummaryRunes = 200
)

// Fixed model-facing texts of synthesized tool results. The cancellation text
// mirrors core/agent/src/runner.rs:5949-5953.
const (
	deniedToolResultText    = "用户拒绝了本次工具执行"
	cancelledToolResultText = "工具调用因 Turn 取消而中止；请重新检查可能的外部副作用"
)

// sinkFailure wraps an append/emit callback failure so the loop can abort the
// turn without emitting a terminal event (docs/go-migration.md §5.4: "emit
// 返回 error 时中止").
type sinkFailure struct{ err error }

// Error implements the error interface.
func (f *sinkFailure) Error() string { return f.err.Error() }

// Unwrap exposes the callback error for errors.Is / errors.As.
func (f *sinkFailure) Unwrap() error { return f.err }

// Dependencies is the full set of external capabilities the loop needs
// (docs/go-migration.md §5.4). It is UI-agnostic and provider-neutral.
type Dependencies struct {
	// Provider is the model gateway used for every round. Required.
	Provider model.Provider
	// Tools is the tool registry; nil means a text-only agent whose requests
	// advertise no tools.
	Tools ToolRegistry
	// System is the default system prompt. It is used when History does not
	// already start with a system message; a History-provided leading system
	// message wins so the caller can vary the prompt per turn.
	System model.Message
	// Authorize approves side-effect tool executions. false denies the call
	// (a permission_denied event plus a failed tool result; the turn keeps
	// going). A nil callback or a callback error denies as well: the gate
	// fails closed. Implementations may block on native dialogs; the loop
	// invokes the callback from the turn goroutine.
	Authorize func(ctx context.Context, req PermissionRequest) (bool, error)
	// MaxRounds bounds the model rounds of one turn; values <= 0 select
	// DefaultMaxRounds. Reaching the bound fails the turn.
	MaxRounds int
}

// Agent runs provider-neutral turns: it streams from the provider, executes
// tool calls, feeds results back and reports the unified timeline events
// until the turn reaches a terminal state (docs/go-migration.md §5.4; the
// reduced Go counterpart of the turn runner in core/agent/src/runner.rs).
type Agent struct{ deps Dependencies }

// New returns an Agent for the given dependencies. Dependencies are validated
// per turn by RunTurn, so construction never fails.
func New(deps Dependencies) *Agent {
	if deps.MaxRounds <= 0 {
		deps.MaxRounds = DefaultMaxRounds
	}
	return &Agent{deps: deps}
}

// RunTurn executes one complete turn: model stream → event forwarding (with
// reasoning continuations merged into the assistant history so the next round
// echoes the opaque signature) → tool call collection → per-call
// authorization and sequential execution → results backfilled as the RoleUser
// tool-result message that immediately follows the assistant message → next
// round; the turn ends on end_turn, a failure, or cancellation
// (docs/go-migration.md §5.4).
//
// Every event first goes through appendEvent (the journal hook; may be nil)
// and then emitEvent (live delivery; may be nil). An error from either
// callback aborts the turn and is returned as-is without a terminal event.
// Cancelling ctx stops the model stream and pending tools, emits
// EventTurnCancelled and returns a nil error: cancellation is a normal turn
// outcome, not a failure. Model and round-budget failures emit EventTurnFailed
// and also return nil — the turn outcome travels on the event stream.
//
// req.History is never mutated; the assembled per-round history lives only
// inside the call. Callers rebuild cross-turn history from the journal.
func (a *Agent) RunTurn(ctx context.Context, req TurnRequest, appendEvent func(Event) error, emitEvent func(Event) error) error {
	if a.deps.Provider == nil {
		return errors.New("agent: Dependencies.Provider 不能为空")
	}
	if strings.TrimSpace(req.SessionID) == "" {
		return errors.New("agent: TurnRequest.SessionID 不能为空")
	}
	if strings.TrimSpace(req.TurnID) == "" {
		return errors.New("agent: TurnRequest.TurnID 不能为空")
	}

	run := &turnRun{
		agent: a,
		req:   req,
		sink: &eventSink{
			sessionID: req.SessionID,
			turnID:    req.TurnID,
			persist:   appendEvent,
			emit:      emitEvent,
		},
	}
	run.system, run.history = a.resolveContext(req.History)

	if ctx.Err() != nil {
		return run.cancelTurn()
	}

	for round := 1; ; round++ {
		if round > a.deps.MaxRounds {
			return run.failTurn(fmt.Sprintf("已达到单次任务的最大模型调用轮数（%d），任务中止", a.deps.MaxRounds))
		}
		if ctx.Err() != nil {
			return run.cancelTurn()
		}
		done, err := run.round(ctx)
		var sinkErr *sinkFailure
		if errors.As(err, &sinkErr) {
			return sinkErr.err
		}
		if err != nil {
			return err
		}
		if done {
			return nil
		}
	}
}

// round executes one model round. done=true reports that the turn reached a
// terminal state (the terminal event has been emitted). The returned error is
// either a wrapped sink failure or a plain error from failTurn delivering its
// own terminal event.
func (run *turnRun) round(ctx context.Context) (done bool, err error) {
	mreq := run.agent.buildModelRequest(run.req, run.system, run.history)
	if err := mreq.Validate(); err != nil {
		return true, run.failTurn(truncateRunes(err.Error(), maxFailureMessageRunes))
	}

	events, err := run.agent.deps.Provider.Stream(ctx, mreq)
	if err != nil {
		return true, run.failTurn(truncateRunes(safeModelMessage(err), maxFailureMessageRunes))
	}

	outcome := run.consumeStream(ctx, events)
	if outcome.err != nil {
		var sinkErr *sinkFailure
		if errors.As(outcome.err, &sinkErr) {
			return false, sinkErr.err
		}
		return true, run.failTurn(truncateRunes(safeModelMessage(outcome.err), maxFailureMessageRunes))
	}
	if outcome.cancelled {
		return true, run.cancelTurn()
	}

	assistant, calls, err := outcome.assembleAssistant()
	if err != nil {
		return true, run.failTurn(truncateRunes(err.Error(), maxFailureMessageRunes))
	}
	if err := run.sink.send(EventUsage, func(e *Event) { e.Usage = &outcome.usage }); err != nil {
		return false, err
	}

	if len(calls) == 0 {
		switch outcome.stop {
		case model.StopEndTurn:
			return true, run.completeTurn()
		case model.StopMaxTokens:
			return true, run.failTurn("模型输出达到长度上限，任务中止")
		case model.StopContentFilter:
			return true, run.failTurn("模型响应被内容策略截断，任务中止")
		default:
			return true, run.failTurn(fmt.Sprintf("模型以原因 %q 结束响应但未请求工具，任务中止", string(outcome.stop)))
		}
	}

	for _, call := range calls {
		if err := run.sink.send(EventToolArgs, func(e *Event) {
			e.Tool = &ToolEvent{CallID: call.ID, Name: call.Name, Status: ToolStatusPending, Detail: call.Arguments}
		}); err != nil {
			return false, err
		}
	}

	results, stopped, err := run.executeCalls(ctx, calls)
	if err != nil {
		return false, err
	}
	run.history = append(run.history, assistant, model.ToolResultMessage(results...))
	if stopped {
		return true, run.cancelTurn()
	}
	return false, nil
}

// resolveContext picks the effective system prompt and copies the replay
// history: a leading system message in History wins over Dependencies.System,
// which is otherwise prepended (docs/go-migration.md §5.4: History 含系统提示
// 词与已持久化全部消息，Dependencies.System 系统提示词).
func (a *Agent) resolveContext(history []model.Message) (system model.Message, rest []model.Message) {
	if len(history) > 0 && history[0].Role == model.RoleSystem {
		return history[0], history[1:]
	}
	if len(a.deps.System.Content) > 0 {
		return a.deps.System, history
	}
	return model.Message{}, history
}

// buildModelRequest assembles one round's provider-neutral request: the chosen
// system prompt plus the accumulated history, the registry's tool definitions
// and the default tool choice (docs/go-migration.md §5.4).
func (a *Agent) buildModelRequest(req TurnRequest, system model.Message, history []model.Message) model.ModelRequest {
	messages := make([]model.Message, 0, len(history)+1)
	if len(system.Content) > 0 {
		messages = append(messages, system)
	}
	messages = append(messages, history...)
	mreq := model.ModelRequest{Model: req.Model, Messages: messages}
	if a.deps.Tools != nil {
		mreq.Tools = a.deps.Tools.Definitions()
	}
	return mreq
}

// executeCalls authorizes and runs the collected calls sequentially in model
// order (docs/go-migration.md §5.4: 逐个授权执行; the parallel read-only
// batches of core/agent/src/runner.rs:3829-3897 are deferred with v1). It
// returns the backfill results in call order. stopped reports that
// cancellation cut the batch short; every remaining call then received a
// synthesized stopped result so journal replay can always pair calls with
// results (core/agent/src/runner.rs:5946-5959). The returned error is only a
// sink failure.
func (run *turnRun) executeCalls(ctx context.Context, calls []model.ToolCall) (results []model.ToolResult, stopped bool, err error) {
	results = make([]model.ToolResult, 0, len(calls))
	for _, call := range calls {
		if stopped || ctx.Err() != nil {
			stopped = true
			result := model.ToolResult{CallID: call.ID, Content: cancelledToolResultText, IsError: true}
			results = append(results, result)
			if err := run.sendToolEnd(call, ToolStatusStopped, "", ""); err != nil {
				return nil, false, err
			}
			if err := run.sendToolResult(call, result, ToolStatusStopped); err != nil {
				return nil, false, err
			}
			continue
		}

		result, callStopped, err := run.executeCall(ctx, call)
		if err != nil {
			return nil, false, err
		}
		results = append(results, result)
		stopped = stopped || callStopped
	}
	return results, stopped, nil
}

// executeCall runs one tool call through the guard chain: registry lookup →
// effect classification → authorization for side effects → execution. The
// returned error is only a sink failure; every tool-layer outcome is encoded
// in the result, status and stopped flag. A panicking tool implementation is
// normalized into a failed result so the journal can never strand a running
// card (the runner's tool panic boundary, core/agent/src/runner.rs:5930-5942).
func (run *turnRun) executeCall(ctx context.Context, call model.ToolCall) (result model.ToolResult, stopped bool, err error) {
	result = model.ToolResult{CallID: call.ID}

	tool, found := lookupTool(run.agent.deps.Tools, call.Name)
	if !found || tool == nil {
		result.Content = fmt.Sprintf("调用失败：未知工具 %s", call.Name)
		result.IsError = true
		if err := run.sendToolEnd(call, ToolStatusFailed, "", result.Content); err != nil {
			return result, false, err
		}
		if err := run.sendToolResult(call, result, ToolStatusFailed); err != nil {
			return result, false, err
		}
		return result, false, nil
	}

	input := json.RawMessage(call.Arguments)
	if tool.Effect(input) == EffectSideEffect {
		if !run.authorizeTool(ctx, call, input) {
			result.Content = deniedToolResultText
			result.IsError = true
			if err := run.sink.send(EventPermissionDenied, func(e *Event) {
				e.Tool = &ToolEvent{CallID: call.ID, Name: call.Name, Status: ToolStatusDenied}
			}); err != nil {
				return result, false, err
			}
			if err := run.sendToolResult(call, result, ToolStatusDenied); err != nil {
				return result, false, err
			}
			return result, false, nil
		}
	}

	if err := run.sink.send(EventToolStart, func(e *Event) {
		e.Tool = &ToolEvent{CallID: call.ID, Name: call.Name, Status: ToolStatusRunning}
	}); err != nil {
		return result, false, err
	}

	output, execErr := invokeTool(ctx, tool, Invocation{
		CallID:  call.ID,
		Name:    call.Name,
		Input:   input,
		WorkDir: run.req.WorkDir,
	})
	if ctx.Err() != nil {
		// The tool honored cancellation; normalize the outcome the way the
		// runner does for lost races (core/agent/src/runner.rs:5946-5959).
		result.Content = cancelledToolResultText
		result.IsError = true
		if err := run.sendToolEnd(call, ToolStatusStopped, "", ""); err != nil {
			return result, true, err
		}
		if err := run.sendToolResult(call, result, ToolStatusStopped); err != nil {
			return result, true, err
		}
		return result, true, nil
	}

	status := ToolStatusCompleted
	switch {
	case execErr != nil:
		result.Content = truncateRunes(execErr.Error(), maxToolErrorRunes)
		result.IsError = true
		status = ToolStatusFailed
	case output.IsError:
		result.Content = output.Content
		result.IsError = true
		status = ToolStatusFailed
	default:
		result.Content = output.Content
	}

	if err := run.sendToolEnd(call, status, output.Summary, truncateRunes(result.Content, maxToolDetailRunes)); err != nil {
		return result, false, err
	}
	if err := run.sendToolResult(call, result, status); err != nil {
		return result, false, err
	}
	return result, false, nil
}

// lookupTool resolves a tool by exact name, tolerating a nil registry.
func lookupTool(registry ToolRegistry, name string) (Tool, bool) {
	if registry == nil {
		return nil, false
	}
	return registry.Get(name)
}

// invokeTool executes one tool call with the panic boundary: an implementation
// that panics degrades into an ordinary error result, mirroring the runner's
// catch_unwind at the execution edge (core/agent/src/runner.rs:5930-5942). A
// contract-bound tool returns instead of panicking; this is defense in depth.
func invokeTool(ctx context.Context, tool Tool, inv Invocation) (output ToolOutput, err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			output = ToolOutput{}
			err = fmt.Errorf("工具实现异常退出: %v", recovered)
		}
	}()
	return tool.Execute(ctx, inv)
}

// authorizeTool asks Dependencies.Authorize to approve a side-effect call. A
// nil callback or a callback error denies: the permission gate fails closed
// (docs/go-migration.md D7).
func (run *turnRun) authorizeTool(ctx context.Context, call model.ToolCall, input json.RawMessage) bool {
	if run.agent.deps.Authorize == nil {
		return false
	}
	approved, err := run.agent.deps.Authorize(ctx, PermissionRequest{
		SessionID: run.req.SessionID,
		TurnID:    run.req.TurnID,
		CallID:    call.ID,
		ToolName:  call.Name,
		Input:     input,
		Summary:   permissionSummary(call.Name, input),
	})
	if err != nil {
		return false
	}
	return approved
}

// permissionSummary renders the dialog body of an authorization request: the
// first string-valued command/path field of the input when present (the
// primaryText/secondaryText contract of docs/go-migration/zcode-chat-specs.md
// §1.10), otherwise the raw arguments.
func permissionSummary(toolName string, input json.RawMessage) string {
	detail := strings.TrimSpace(string(input))
	var fields map[string]any
	if err := json.Unmarshal(input, &fields); err == nil {
		for _, key := range []string{"command", "path", "file_path", "filePath", "prompt"} {
			if value, ok := fields[key].(string); ok && strings.TrimSpace(value) != "" {
				detail = value
				break
			}
		}
	}
	return truncateRunes(fmt.Sprintf("工具 %s 请求执行副作用操作：%s", toolName, detail), maxPermissionSummaryRunes)
}

// sendToolEnd emits the terminal card update of one tool execution.
func (run *turnRun) sendToolEnd(call model.ToolCall, status, summary, detail string) error {
	return run.sink.send(EventToolEnd, func(e *Event) {
		e.Tool = &ToolEvent{CallID: call.ID, Name: call.Name, Status: status, Summary: summary, Detail: detail}
	})
}

// sendToolResult emits the exact backfilled content for one call.
func (run *turnRun) sendToolResult(call model.ToolCall, result model.ToolResult, status string) error {
	return run.sink.send(EventToolResult, func(e *Event) {
		e.Text = result.Content
		e.Tool = &ToolEvent{CallID: call.ID, Name: call.Name, Status: status}
	})
}

// completeTurn emits the normal terminal event.
func (run *turnRun) completeTurn() error {
	return run.sink.send(EventTurnCompleted, func(e *Event) { e.StopReason = model.StopEndTurn })
}

// failTurn emits the failure terminal event with a display-safe message.
func (run *turnRun) failTurn(message string) error {
	return run.sink.send(EventTurnFailed, func(e *Event) { e.Text = message })
}

// cancelTurn emits the cancellation terminal event (docs/go-migration.md
// §5.4: cancellation is reported, then RunTurn returns nil).
func (run *turnRun) cancelTurn() error {
	return run.sink.send(EventTurnCancelled, func(e *Event) { e.StopReason = model.StopCancelled })
}

// turnRun carries the per-turn state of one RunTurn call.
type turnRun struct {
	agent   *Agent
	req     TurnRequest
	sink    *eventSink
	system  model.Message
	history []model.Message
}

// eventSink assigns identities and forwards events persist-first, then live
// (docs/go-migration.md §5.4: 每个 Event 先经 append（journal，可 nil）再
// emit).
type eventSink struct {
	sessionID string
	turnID    string
	seq       int
	persist   func(Event) error
	emit      func(Event) error
}

// send builds one event, applies the payload, and delivers it through the
// journal hook and the live channel. A callback failure is wrapped in
// *sinkFailure so the loop can abort without emitting further events.
func (s *eventSink) send(eventType EventType, apply func(*Event)) error {
	s.seq++
	event := Event{
		ID:        fmt.Sprintf("%s-%06d", s.turnID, s.seq),
		SessionID: s.sessionID,
		TurnID:    s.turnID,
		Time:      time.Now(),
		Type:      eventType,
	}
	if apply != nil {
		apply(&event)
	}
	if s.persist != nil {
		if err := s.persist(event); err != nil {
			return &sinkFailure{err: err}
		}
	}
	if s.emit != nil {
		if err := s.emit(event); err != nil {
			return &sinkFailure{err: err}
		}
	}
	return nil
}

// safeModelMessage renders an error for display: ModelError values already
// carry the unified display-safe message; anything else degrades to its Error
// text and the caller applies the display bound.
func safeModelMessage(err error) string {
	var modelErr *model.ModelError
	if errors.As(err, &modelErr) {
		return modelErr.Error()
	}
	return err.Error()
}

// truncateRunes bounds a string to maxRunes visible runes, appending an
// ellipsis marker when content was dropped.
func truncateRunes(text string, maxRunes int) string {
	if utf8.RuneCountInString(text) <= maxRunes {
		return text
	}
	runes := []rune(text)
	return string(runes[:maxRunes]) + "…"
}
