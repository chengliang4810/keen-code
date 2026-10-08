package agent

import (
	"context"
	"encoding/json"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"keencode/internal/model"
)

// mustWithin fails the test when the channel does not fire in time.
func mustWithin(t *testing.T, ch <-chan struct{}, what string) {
	t.Helper()
	select {
	case <-ch:
	case <-time.After(5 * time.Second):
		t.Fatalf("等待 %s 超时", what)
	}
}

// TestRunTurnCancelledBetweenCalls: cancelling after one call finished stops
// the in-flight call with a synthesized stopped result, and every remaining
// call receives one too without executing — so journal replay can always pair
// tool calls with results (core/agent/src/runner.rs:5946-5959).
func TestRunTurnCancelledBetweenCalls(t *testing.T) {
	round1 := toolRound(
		append(
			append(
				toolCallEvents(1, "call-1", "get", `{}`),
				toolCallEvents(2, "call-2", "put", `{}`)...,
			),
			toolCallEvents(3, "call-3", "list", `{}`)...,
		),
		model.StopToolUse,
	)
	provider := newFakeProvider(round1, textRound("不应到达", model.StopEndTurn))

	putStarted := make(chan struct{})
	get := &fakeTool{def: toolDef("get")}
	put := &fakeTool{def: toolDef("put"), effect: func(input json.RawMessage) Effect {
		return EffectSideEffect
	}, exec: func(ctx context.Context, inv Invocation) (ToolOutput, error) {
		close(putStarted)
		<-ctx.Done()
		return ToolOutput{}, ctx.Err()
	}}
	list := &fakeTool{def: toolDef("list")}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{get, put, list}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
		Authorize: func(ctx context.Context, req PermissionRequest) (bool, error) {
			return true, nil
		},
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	rec := newRecorder()
	errCh := make(chan error, 1)
	go func() {
		errCh <- agent.RunTurn(ctx, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec.persist, rec.emit)
	}()

	// Wait for the first call's terminal result, then cancel while the second
	// call is in flight and the third has not started.
	rec.waitFor(t, EventToolResult)
	mustWithin(t, putStarted, "第二个工具开始执行")
	cancel()

	if err := <-errCh; err != nil {
		t.Fatalf("取消后 RunTurn 应返回 nil，得到 %v", err)
	}
	assertTerminal(t, rec, EventTurnCancelled, nil)

	starts := rec.all(EventToolStart)
	if len(starts) != 2 || starts[0].Tool.CallID != "call-1" || starts[1].Tool.CallID != "call-2" {
		t.Fatalf("tool_start 事件错误：%v", starts)
	}
	if put.invocationCount() != 1 || list.invocationCount() != 0 {
		t.Fatalf("执行计数错误：put=%d list=%d", put.invocationCount(), list.invocationCount())
	}
	ends := rec.all(EventToolEnd)
	if len(ends) != 3 ||
		ends[0].Tool.Status != ToolStatusCompleted ||
		ends[1].Tool.CallID != "call-2" || ends[1].Tool.Status != ToolStatusStopped ||
		ends[2].Tool.CallID != "call-3" || ends[2].Tool.Status != ToolStatusStopped {
		t.Fatalf("tool_end 状态错误：%v", ends)
	}
	results := rec.all(EventToolResult)
	if len(results) != 3 || results[1].Tool.Status != ToolStatusStopped || results[1].Text == "" ||
		results[2].Tool.Status != ToolStatusStopped || results[2].Text == "" {
		t.Fatalf("综合取消结果错误：%v", results)
	}
}

// TestRunTurnProtocolViolationsFailTurn covers stream-level malformations the
// consumer must reject: a duplicated start event and a channel closed before
// the terminal event.
func TestRunTurnProtocolViolationsFailTurn(t *testing.T) {
	tests := []struct {
		name       string
		events     []model.StreamEvent
		wantSubstr string
	}{
		{
			name: "重复开始事件",
			events: []model.StreamEvent{
				{Type: model.EventMessageStart},
				{Type: model.EventMessageStart},
			},
			wantSubstr: "开始事件",
		},
		{
			name: "流提前关闭",
			events: []model.StreamEvent{
				{Type: model.EventMessageStart},
				{Type: model.EventTextDelta, Index: 0, Delta: "部分"},
			},
			wantSubstr: "响应结束事件之前",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			provider := newFakeProvider(fakeRound{events: tt.events})
			agent := New(Dependencies{
				Provider: provider,
				System:   model.TextMessage(model.RoleSystem, "系统提示词"),
			})
			rec := newRecorder()
			err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
			assertTerminal(t, rec, EventTurnFailed, err)
			if text := rec.last().Text; !strings.Contains(text, tt.wantSubstr) {
				t.Fatalf("失败说明 %q 未包含 %q", text, tt.wantSubstr)
			}
		})
	}
}

// TestRunTurnInvalidToolCallFailsTurn: a tool call the unified layer cannot
// validate (non-portable name) fails the round instead of reaching a tool.
func TestRunTurnInvalidToolCallFailsTurn(t *testing.T) {
	provider := newFakeProvider(toolRound(toolCallEvents(1, "call-1", "坏名字", `{}`), model.StopToolUse))
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnFailed, err)
	if args := rec.all(EventToolArgs); len(args) != 0 {
		t.Fatalf("非法调用不应产生工具卡片：%v", args)
	}
}

// TestRunTurnAdvertisesRegistryDefinitions: the registry's frozen definition
// order travels on every request.
func TestRunTurnAdvertisesRegistryDefinitions(t *testing.T) {
	provider := newFakeProvider(textRound("好", model.StopEndTurn))
	get := &fakeTool{def: toolDef("get")}
	put := &fakeTool{def: toolDef("put")}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{get, put}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)

	defs := provider.snapshot()[0].Tools
	if len(defs) != 2 || defs[0].Name != "get" || defs[1].Name != "put" {
		t.Fatalf("请求工具定义错误：%v", defs)
	}
}

// TestRunTurnRejectsNilToolFromRegistry: a registry handing back a nil tool
// with ok=true must not crash the turn; it degrades to an unknown-tool result.
func TestRunTurnRejectsNilToolFromRegistry(t *testing.T) {
	round1 := toolRound(toolCallEvents(1, "call-1", "ghost", `{}`), model.StopToolUse)
	provider := newFakeProvider(round1, textRound("好", model.StopEndTurn))
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &nilToolRegistry{},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)
	results := rec.all(EventToolResult)
	if len(results) != 1 || !strings.Contains(results[0].Text, "未知工具") {
		t.Fatalf("nil 工具应按未知工具处理：%v", results)
	}
}

// nilToolRegistry violates the registry contract by returning a nil tool with
// ok=true; the loop must fail soft.
type nilToolRegistry struct{}

// Definitions implements ToolRegistry.
func (r *nilToolRegistry) Definitions() []model.ToolDefinition {
	return []model.ToolDefinition{toolDef("ghost")}
}

// Get implements ToolRegistry with the contract violation.
func (r *nilToolRegistry) Get(name string) (Tool, bool) {
	return nil, true
}

// TestRunTurnPanickingToolNormalized: a tool implementation that panics
// degrades into a failed result and the turn keeps going
// (core/agent/src/runner.rs:5930-5942 的归一语义).
func TestRunTurnPanickingToolNormalized(t *testing.T) {
	round1 := toolRound(
		append(
			toolCallEvents(1, "call-1", "boom", `{}`),
			toolCallEvents(2, "call-2", "get", `{}`)...,
		),
		model.StopToolUse,
	)
	provider := newFakeProvider(round1, textRound("继续", model.StopEndTurn))
	boom := &fakeTool{def: toolDef("boom"), exec: func(ctx context.Context, inv Invocation) (ToolOutput, error) {
		panic("工具内部崩溃")
	}}
	get := &fakeTool{def: toolDef("get")}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{boom, get}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)

	ends := rec.all(EventToolEnd)
	if len(ends) != 2 || ends[0].Tool.Status != ToolStatusFailed || ends[1].Tool.Status != ToolStatusCompleted {
		t.Fatalf("panic 未归一为失败卡片：%v", ends)
	}
	backfilled := resultsOf(t, provider.snapshot()[1].Messages[3])
	if len(backfilled) != 2 || !backfilled[0].IsError || !strings.Contains(backfilled[0].Content, "异常退出") {
		t.Fatalf("panic 结果回填错误：%+v", backfilled)
	}
}

// TestRunTurnToolErrorResultPassThrough: a tool returning an error-flagged
// output (rather than an error) backfills IsError content and completes.
func TestRunTurnToolErrorResultPassThrough(t *testing.T) {
	round1 := toolRound(toolCallEvents(1, "call-1", "get", `{}`), model.StopToolUse)
	provider := newFakeProvider(round1, textRound("好", model.StopEndTurn))
	get := &fakeTool{def: toolDef("get"), exec: func(ctx context.Context, inv Invocation) (ToolOutput, error) {
		return ToolOutput{Content: "文件不存在", IsError: true}, nil
	}}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{get}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)

	ends := rec.all(EventToolEnd)
	if len(ends) != 1 || ends[0].Tool.Status != ToolStatusFailed {
		t.Fatalf("错误输出应产生 failed 卡片：%v", ends)
	}
	backfilled := resultsOf(t, provider.snapshot()[1].Messages[3])
	if len(backfilled) != 1 || !backfilled[0].IsError || backfilled[0].Content != "文件不存在" {
		t.Fatalf("错误输出回填错误：%+v", backfilled)
	}
}

// TestRunTurnResultsSentToNextRound: arguments streamed across several deltas
// are concatenated before execution; the card carries the joined raw input.
func TestRunTurnResultsSentToNextRound(t *testing.T) {
	round1 := toolRound(splitToolCallEvents(1, "call-1", "get", `{"path":"a.txt"}`), model.StopToolUse)
	provider := newFakeProvider(round1, textRound("完成", model.StopEndTurn))

	var seenArgs atomic.Pointer[string]
	get := &fakeTool{def: toolDef("get"), exec: func(ctx context.Context, inv Invocation) (ToolOutput, error) {
		text := string(inv.Input)
		seenArgs.Store(&text)
		return ToolOutput{Content: "内容 A"}, nil
	}}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{get}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)

	if seenArgs.Load() == nil || *seenArgs.Load() != `{"path":"a.txt"}` {
		t.Fatalf("工具收到的拼接参数错误：%v", seenArgs.Load())
	}
	args := rec.all(EventToolArgs)
	if len(args) != 1 || args[0].Tool.Detail != `{"path":"a.txt"}` {
		t.Fatalf("tool_args 详情错误：%v", args)
	}
}

// splitToolCallEvents streams one tool call's arguments in two deltas.
func splitToolCallEvents(index uint32, id, name, args string) []model.StreamEvent {
	half := len(args) / 2
	return []model.StreamEvent{
		{Type: model.EventToolCallStart, Index: index, CallID: id, Name: name},
		{Type: model.EventToolCallArgsDelta, Index: index, Delta: args[:half]},
		{Type: model.EventToolCallArgsDelta, Index: index, Delta: args[half:]},
		{Type: model.EventToolCallEnd, Index: index},
	}
}
