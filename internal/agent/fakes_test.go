package agent

import (
	"context"
	"encoding/json"
	"sync"
	"testing"
	"time"

	"keencode/internal/model"
)

// fakeRound scripts one provider streaming call.
type fakeRound struct {
	// events are delivered in order after the stream opens.
	events []model.StreamEvent
	// hangTail, when true, blocks after delivering every scripted event until
	// ctx is done, then emits one EventError and closes — the model.Provider
	// cancellation contract (internal/model/provider.go:26-28).
	hangTail bool
}

// fakeProvider replays scripted rounds; the last round repeats once the script
// is exhausted. It records every request it received.
type fakeProvider struct {
	mu      sync.Mutex
	rounds  []fakeRound
	request []model.ModelRequest
	failOn  int // zero-based Stream call that fails immediately; -1 = never
	failErr error
}

// newFakeProvider scripts the given rounds with no immediate failures.
func newFakeProvider(rounds ...fakeRound) *fakeProvider {
	return &fakeProvider{rounds: rounds, failOn: -1}
}

// Capabilities implements model.Provider.
func (p *fakeProvider) Capabilities(string) model.Capabilities { return model.Capabilities{} }

// Stream implements model.Provider.
func (p *fakeProvider) Stream(ctx context.Context, req model.ModelRequest) (<-chan model.StreamEvent, error) {
	p.mu.Lock()
	call := len(p.request)
	p.request = append(p.request, req)
	round := p.rounds[min(call, len(p.rounds)-1)]
	failOn, failErr := p.failOn, p.failErr
	p.mu.Unlock()

	if failOn == call {
		return nil, failErr
	}

	events := make(chan model.StreamEvent, 64)
	go func() {
		defer close(events)
		for _, event := range round.events {
			select {
			case events <- event:
			case <-ctx.Done():
				select {
				case events <- model.StreamEvent{Type: model.EventError, Err: model.CancelledError(ctx.Err().Error())}:
				case <-ctx.Done():
				}
				return
			}
		}
		if round.hangTail {
			<-ctx.Done()
			select {
			case events <- model.StreamEvent{Type: model.EventError, Err: model.CancelledError(ctx.Err().Error())}:
			case <-time.After(time.Second):
			}
		}
	}()
	return events, nil
}

// snapshot returns a copy of the recorded requests.
func (p *fakeProvider) snapshot() []model.ModelRequest {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]model.ModelRequest(nil), p.request...)
}

// fakeTool is a scriptable Tool implementation.
type fakeTool struct {
	def    model.ToolDefinition
	effect func(input json.RawMessage) Effect
	exec   func(ctx context.Context, inv Invocation) (ToolOutput, error)

	mu    sync.Mutex
	calls []Invocation
}

// Definition implements Tool.
func (f *fakeTool) Definition() model.ToolDefinition { return f.def }

// Effect implements Tool, falling back to read-only when no classifier is set.
func (f *fakeTool) Effect(input json.RawMessage) Effect {
	if f.effect == nil {
		return EffectReadOnly
	}
	return f.effect(input)
}

// Execute implements Tool and records the invocation.
func (f *fakeTool) Execute(ctx context.Context, inv Invocation) (ToolOutput, error) {
	f.mu.Lock()
	f.calls = append(f.calls, inv)
	f.mu.Unlock()
	if f.exec == nil {
		return ToolOutput{Content: "ok"}, nil
	}
	return f.exec(ctx, inv)
}

// invocationCount reports how often the tool ran.
func (f *fakeTool) invocationCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.calls)
}

// fakeRegistry is a minimal ToolRegistry over a fixed slice.
type fakeRegistry struct{ tools []Tool }

// Definitions implements ToolRegistry.
func (r *fakeRegistry) Definitions() []model.ToolDefinition {
	defs := make([]model.ToolDefinition, 0, len(r.tools))
	for _, tool := range r.tools {
		defs = append(defs, tool.Definition())
	}
	return defs
}

// Get implements ToolRegistry.
func (r *fakeRegistry) Get(name string) (Tool, bool) {
	for _, tool := range r.tools {
		if tool.Definition().Name == name {
			return tool, true
		}
	}
	return nil, false
}

// recorder captures every event on both delivery legs and can be scripted to
// fail on a given event type.
type recorder struct {
	mu          sync.Mutex
	events      []Event
	ops         []string // "persist:<type>" / "emit:<type>" in call order
	persistFail map[EventType]error
	emitFail    map[EventType]error
	live        chan Event
}

// newRecorder returns a recorder with a large live buffer.
func newRecorder() *recorder {
	return &recorder{
		persistFail: make(map[EventType]error),
		emitFail:    make(map[EventType]error),
		live:        make(chan Event, 256),
	}
}

// persist is the journal leg: it logs the operation (and may fail) without
// touching the recorded timeline, which belongs to the live leg.
func (r *recorder) persist(event Event) error {
	r.mu.Lock()
	r.ops = append(r.ops, "persist:"+string(event.Type))
	r.mu.Unlock()
	if err := r.persistFail[event.Type]; err != nil {
		return err
	}
	return nil
}

// emit is the live leg: it records, notifies waiters, then may fail.
func (r *recorder) emit(event Event) error {
	r.mu.Lock()
	r.events = append(r.events, event)
	r.ops = append(r.ops, "emit:"+string(event.Type))
	r.mu.Unlock()
	r.live <- event
	if err := r.emitFail[event.Type]; err != nil {
		return err
	}
	return nil
}

// snapshot returns a copy of all recorded events.
func (r *recorder) snapshot() []Event {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]Event(nil), r.events...)
}

// opsSnapshot returns a copy of the delivery-leg operation log.
func (r *recorder) opsSnapshot() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]string(nil), r.ops...)
}

// waitFor blocks until an event of the given type arrives on the live leg.
func (r *recorder) waitFor(t *testing.T, eventType EventType) Event {
	t.Helper()
	for {
		select {
		case event := <-r.live:
			if event.Type == eventType {
				return event
			}
		case <-time.After(5 * time.Second):
			t.Fatalf("等待事件 %s 超时；已见事件：%v", eventType, r.types())
		}
	}
}

// types lists the recorded event types in order.
func (r *recorder) types() []EventType {
	r.mu.Lock()
	defer r.mu.Unlock()
	types := make([]EventType, 0, len(r.events))
	for _, event := range r.events {
		types = append(types, event.Type)
	}
	return types
}

// all returns every recorded event of the given type.
func (r *recorder) all(eventType EventType) []Event {
	r.mu.Lock()
	defer r.mu.Unlock()
	var found []Event
	for _, event := range r.events {
		if event.Type == eventType {
			found = append(found, event)
		}
	}
	return found
}

// last returns the most recently recorded event.
func (r *recorder) last() Event {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.events[len(r.events)-1]
}

// scripted round builders.

// textRound scripts one text-only response.
func textRound(text string, stop model.StopReason) fakeRound {
	return fakeRound{events: []model.StreamEvent{
		{Type: model.EventMessageStart},
		{Type: model.EventTextDelta, Index: 0, Delta: text},
		{Type: model.EventUsage, Usage: model.TokenUsage{InputTokens: 10, OutputTokens: 5}},
		{Type: model.EventMessageEnd, StopReason: stop},
	}}
}

// toolCallEvents scripts the streaming of one complete tool call block.
func toolCallEvents(index uint32, id, name, args string) []model.StreamEvent {
	return []model.StreamEvent{
		{Type: model.EventToolCallStart, Index: index, CallID: id, Name: name},
		{Type: model.EventToolCallArgsDelta, Index: index, Delta: args},
		{Type: model.EventToolCallEnd, Index: index},
	}
}

// toolRound scripts one response that requests the given tool calls.
func toolRound(calls []model.StreamEvent, stop model.StopReason) fakeRound {
	events := append([]model.StreamEvent{{Type: model.EventMessageStart}}, calls...)
	events = append(events,
		model.StreamEvent{Type: model.EventUsage, Usage: model.TokenUsage{InputTokens: 20, OutputTokens: 8}},
		model.StreamEvent{Type: model.EventMessageEnd, StopReason: stop},
	)
	return fakeRound{events: events}
}

// fixtures.

func userMessage(text string) model.Message {
	return model.TextMessage(model.RoleUser, text)
}

func toolDef(name string) model.ToolDefinition {
	return model.ToolDefinition{
		Name:        name,
		Description: "测试工具 " + name,
		InputSchema: json.RawMessage(`{"type":"object"}`),
	}
}

func runTurn(t *testing.T, agent *Agent, req TurnRequest, rec *recorder) error {
	t.Helper()
	return agent.RunTurn(context.Background(), req, rec.persist, rec.emit)
}

// assertTerminal fails the test unless RunTurn returned nil and the last event
// closes the turn with the wanted type.
func assertTerminal(t *testing.T, rec *recorder, want EventType, err error) {
	t.Helper()
	if err != nil {
		t.Fatalf("RunTurn 返回错误 %v，期望 nil", err)
	}
	last := rec.last()
	if last.Type != want {
		t.Fatalf("最后事件为 %s，期望 %s；全部事件：%v", last.Type, want, rec.types())
	}
}
