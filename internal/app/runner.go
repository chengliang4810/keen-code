package app

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"
	"sync/atomic"

	"github.com/egoist/mygo"

	"keencode/internal/agent"
	"keencode/internal/config"
	"keencode/internal/model"
	"keencode/internal/runtime"
	"keencode/internal/tools"
)

// runnerSource assembles one TurnRunner per Send (docs/go-migration.md
// §5.7: deps func() agent.Dependencies wired through the runtime manager's
// AgentFactory). Configuration that only exists at Send time — the active
// provider endpoint, the project directory — is captured here; the
// per-session permission allowances live for the session's lifetime.
//
// Threading: prepareTurn runs on the UI thread immediately before
// Session.Send; the factory runs under the session lock inside that same
// Send call, so the workDir handoff cannot interleave. The Authorize
// callback runs on the turn goroutine and blocks on the native permission
// dialog, which mygo forwards to the main thread and waits on
// (mygo.go:44-49).
type runnerSource struct {
	settings *SettingsService

	// workDir is the project directory of the Send currently in flight.
	// UI-thread only (see prepareTurn).
	workDir string
	// newDraftDir mirrors the project directory of the new-session draft so
	// SaveDraft("") persists the pair. UI-thread only.
	newDraftDir string

	// win holds the main window for dialog parenting; nil until BindWindow.
	win atomic.Pointer[mygo.Window]

	// ask is the permission dialog seam; tests replace it. The default
	// calls mygo.Dialog.Message, which blocks until the user answers.
	ask askFunc

	// allowedMu guards allowed.
	allowedMu sync.Mutex
	// allowed maps session id → tool name → the user granted it for the
	// rest of the session ("本会话允许"). Entries die with the session
	// (forgetSession).
	allowed map[string]map[string]bool
}

// askFunc is the signature of the permission dialog seam.
type askFunc = func(mygo.MessageOptions) (mygo.MessageResult, error)

// defaultAsk is the production dialog: a blocking native message box.
func defaultAsk(opts mygo.MessageOptions) (mygo.MessageResult, error) {
	return mygo.Dialog.Message(opts)
}

// newRunnerSource builds the source around the settings service.
func newRunnerSource(settings *SettingsService) *runnerSource {
	return &runnerSource{
		settings: settings,
		ask:      defaultAsk,
		allowed:  map[string]map[string]bool{},
	}
}

// bindWindow stores the main window; native dialogs become sheets of it.
func (r *runnerSource) bindWindow(win *mygo.Window) {
	r.win.Store(win)
}

// setAsk replaces the dialog seam (test injection).
func (r *runnerSource) setAsk(fn askFunc) {
	r.ask = fn
}

// prepareTurn hands the project directory of the imminent Send to the
// factory. UI-thread only, immediately before Session.Send.
func (r *runnerSource) prepareTurn(_, workDir string) {
	r.workDir = workDir
}

// agentFactory returns the runtime.AgentFactory for ManagerOptions.
func (r *runnerSource) agentFactory() runtime.AgentFactory {
	return func() (runtime.TurnRunner, error) {
		runner, err := r.buildRunner()
		if err != nil {
			return nil, err
		}
		return turnRunnerAdapter{inner: runner}, nil
	}
}

// buildRunner assembles the loop agent for one turn: provider from the
// active configuration, the six-tool registry bound to the pending work
// directory, the fixed system prompt, and the permission bridge. A missing
// provider or a bad endpoint fails the Send with a display-safe error.
func (r *runnerSource) buildRunner() (*agent.Agent, error) {
	workDir := r.workDir
	if workDir == "" {
		return nil, fmt.Errorf("缺少项目目录，无法装配回合")
	}
	record, err := r.activeProviderRecord()
	if err != nil {
		return nil, err
	}
	provider, err := providerFor(record)
	if err != nil {
		return nil, err
	}
	registry, err := toolRegistryFor(workDir)
	if err != nil {
		return nil, err
	}
	deps := agent.Dependencies{
		Provider:  provider,
		Tools:     agentRegistryView{registry},
		System:    systemPromptFor(workDir),
		Authorize: r.authorize,
	}
	return agent.New(deps), nil
}

// activeProviderRecord resolves the currently selected provider record.
func (r *runnerSource) activeProviderRecord() (config.ProviderRecord, error) {
	state := r.settings.Providers()
	id, ok := state.ActiveProvider()
	if !ok {
		return config.ProviderRecord{}, fmt.Errorf("尚未选择模型供应商，请先配置并选择模型")
	}
	record, ok := state.Provider(id)
	if !ok {
		return config.ProviderRecord{}, fmt.Errorf("供应商 %s 不存在", id)
	}
	return record, nil
}

// agentRegistryView adapts *tools.Registry to the loop's ToolRegistry.
// The two Tool interfaces are distinct named types (agent.Invocation vs
// tools.Invocation), so a field-by-field bridge is required.
type agentRegistryView struct{ inner *tools.Registry }

// Definitions forwards the frozen tool definitions.
func (v agentRegistryView) Definitions() []model.ToolDefinition {
	return v.inner.Definitions()
}

// Get resolves one tool wrapped in the agent-side interface.
func (v agentRegistryView) Get(name string) (agent.Tool, bool) {
	tool, ok := v.inner.Get(name)
	if !ok || tool == nil {
		return nil, false
	}
	return bridgedTool{inner: tool}, true
}

// bridgedTool converts one tools.Tool into the agent loop's contract.
type bridgedTool struct{ inner tools.Tool }

// Definition forwards the schema.
func (b bridgedTool) Definition() model.ToolDefinition { return b.inner.Definition() }

// Effect maps the classification; both vocabularies share the same
// constant strings, so the mapping is exhaustive.
func (b bridgedTool) Effect(input json.RawMessage) agent.Effect {
	if b.inner.Effect(input) == tools.EffectSideEffect {
		return agent.EffectSideEffect
	}
	return agent.EffectReadOnly
}

// Execute converts the invocation and the output between the packages.
func (b bridgedTool) Execute(ctx context.Context, inv agent.Invocation) (agent.ToolOutput, error) {
	out, err := b.inner.Execute(ctx, tools.Invocation{
		CallID:  inv.CallID,
		Name:    inv.Name,
		Input:   inv.Input,
		WorkDir: inv.WorkDir,
		Emit:    inv.Emit,
	})
	return agent.ToolOutput{Content: out.Content, IsError: out.IsError, Summary: out.Summary}, err
}

// turnRunnerAdapter bridges the loop agent onto runtime.TurnRunner: the
// two TurnRequest/Event vocabularies are field-identical but nominal
// types, so the conversion is spelled out once here.
type turnRunnerAdapter struct{ inner *agent.Agent }

// RunTurn converts the request, forwards the journal/emit callbacks
// through the event mapping, and returns the loop's outcome unchanged.
func (t turnRunnerAdapter) RunTurn(ctx context.Context, req runtime.TurnRequest,
	journal func(runtime.Event) error, emit func(runtime.Event) error) error {
	areq := agent.TurnRequest{
		SessionID: req.SessionID,
		TurnID:    req.TurnID,
		Model:     req.Model,
		History:   req.History,
		WorkDir:   req.WorkDir,
	}
	var toJournal func(agent.Event) error
	if journal != nil {
		toJournal = func(ev agent.Event) error { return journal(eventToRuntime(ev)) }
	}
	var toEmit func(agent.Event) error
	if emit != nil {
		toEmit = func(ev agent.Event) error { return emit(eventToRuntime(ev)) }
	}
	return t.inner.RunTurn(ctx, areq, toJournal, toEmit)
}

// eventToRuntime maps one loop event onto the runtime envelope. The event
// kinds carry the same string values in both packages (agent/event.go and
// runtime/event.go share the vocabulary of docs/go-migration.md §5.4); the
// conversion fails loudly at compile time when a field drifts.
func eventToRuntime(ev agent.Event) runtime.Event {
	out := runtime.Event{
		ID:         ev.ID,
		SessionID:  ev.SessionID,
		TurnID:     ev.TurnID,
		Seq:        ev.Seq,
		Replay:     ev.Replay,
		Time:       ev.Time,
		Type:       runtime.EventType(ev.Type),
		Text:       ev.Text,
		StopReason: ev.StopReason,
		Usage:      ev.Usage,
	}
	if ev.Tool != nil {
		out.Tool = &runtime.ToolEvent{
			CallID:       ev.Tool.CallID,
			Name:         ev.Tool.Name,
			Summary:      ev.Tool.Summary,
			Status:       ev.Tool.Status,
			Detail:       ev.Tool.Detail,
			AddedLines:   ev.Tool.AddedLines,
			RemovedLines: ev.Tool.RemovedLines,
		}
	}
	return out
}

// forgetSession drops the per-session permission allowances.
func (r *runnerSource) forgetSession(sessionID string) {
	r.allowedMu.Lock()
	defer r.allowedMu.Unlock()
	delete(r.allowed, sessionID)
}

// sessionAllows reports whether the user granted a tool for the rest of
// the session.
func (r *runnerSource) sessionAllows(sessionID, toolName string) bool {
	r.allowedMu.Lock()
	defer r.allowedMu.Unlock()
	return r.allowed[sessionID][toolName]
}

// allowForSession records a session-scoped allowance.
func (r *runnerSource) allowForSession(sessionID, toolName string) {
	r.allowedMu.Lock()
	defer r.allowedMu.Unlock()
	if r.allowed[sessionID] == nil {
		r.allowed[sessionID] = map[string]bool{}
	}
	r.allowed[sessionID][toolName] = true
}
