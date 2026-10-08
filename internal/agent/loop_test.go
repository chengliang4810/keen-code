package agent

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"keencode/internal/model"
)

var errSentinelEmit = errors.New("事件通道已关闭")
var errSentinelPersist = errors.New("日志写入失败")

// resultsOf extracts the tool results of a backfilled RoleUser message.
func resultsOf(t *testing.T, msg model.Message) []model.ToolResult {
	t.Helper()
	if msg.Role != model.RoleUser {
		t.Fatalf("回填消息角色为 %q，期望 user", msg.Role)
	}
	results := make([]model.ToolResult, 0, len(msg.Content))
	for _, block := range msg.Content {
		result, ok := block.(model.ToolResultBlock)
		if !ok {
			t.Fatalf("回填消息包含非工具结果内容块 %T", block)
		}
		results = append(results, result.Result)
	}
	return results
}

// textOf returns the single text payload of a message.
func textOf(t *testing.T, msg model.Message) string {
	t.Helper()
	text, ok := model.LastNonEmptyText(msg.Content)
	if !ok {
		t.Fatalf("消息 %q 不含文本内容块", msg.Role)
	}
	return text
}

// TestRunTurnTextOnlyCompletes covers the happy path: context assembly, live
// delta forwarding, persist-before-emit ordering, usage and the completed
// terminal event.
func TestRunTurnTextOnlyCompletes(t *testing.T) {
	provider := newFakeProvider(textRound("你好", model.StopEndTurn))
	rec := newRecorder()
	agent := New(Dependencies{
		Provider: provider,
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	req := TurnRequest{
		SessionID: "s1",
		TurnID:    "t1",
		Model:     "test-model",
		History:   []model.Message{userMessage("hi")},
		WorkDir:   "/tmp/demo",
	}
	if err := runTurn(t, agent, req, rec); err != nil {
		t.Fatalf("RunTurn: %v", err)
	}

	assertTerminal(t, rec, EventTurnCompleted, nil)
	if got := rec.last().StopReason; got != model.StopEndTurn {
		t.Fatalf("完成事件停止原因为 %q，期望 end_turn", got)
	}

	// Delivery order: journal leg strictly before the live leg per event.
	wantOps := []string{
		"persist:text_delta", "emit:text_delta",
		"persist:usage", "emit:usage",
		"persist:turn_completed", "emit:turn_completed",
	}
	gotOps := rec.opsSnapshot()
	if len(gotOps) != len(wantOps) {
		t.Fatalf("投递日志为 %v，期望 %v", gotOps, wantOps)
	}
	for i, op := range wantOps {
		if gotOps[i] != op {
			t.Fatalf("投递日志第 %d 项为 %s，期望 %s（全部：%v）", i, gotOps[i], op, gotOps)
		}
	}

	// Event identity: unique within the turn and rooted at the turn id.
	seen := make(map[string]bool)
	for _, event := range rec.snapshot() {
		if !strings.HasPrefix(event.ID, "t1-") {
			t.Fatalf("事件 ID %q 未以轮次 ID 为前缀", event.ID)
		}
		if seen[event.ID] {
			t.Fatalf("事件 ID %q 重复", event.ID)
		}
		seen[event.ID] = true
		if event.SessionID != "s1" || event.TurnID != "t1" {
			t.Fatalf("事件 %q 身份字段错误：%q/%q", event.ID, event.SessionID, event.TurnID)
		}
		if event.Time.IsZero() {
			t.Fatalf("事件 %q 缺少时间戳", event.ID)
		}
	}

	// Context assembly: system prompt first, then the replayed history.
	requests := provider.snapshot()
	if len(requests) != 1 {
		t.Fatalf("模型调用次数为 %d，期望 1", len(requests))
	}
	mreq := requests[0]
	if mreq.Model != "test-model" {
		t.Fatalf("请求模型为 %q，期望 test-model", mreq.Model)
	}
	if len(mreq.Messages) != 2 ||
		textOf(t, mreq.Messages[0]) != "系统提示词" ||
		mreq.Messages[0].Role != model.RoleSystem ||
		textOf(t, mreq.Messages[1]) != "hi" {
		t.Fatalf("请求消息组装错误：%d 条", len(mreq.Messages))
	}
	if len(mreq.Tools) != 0 {
		t.Fatalf("无注册表时不应携带工具定义：%v", mreq.Tools)
	}

	// Live payload: exactly one forwarded delta and one merged usage.
	deltas := rec.all(EventTextDelta)
	if len(deltas) != 1 || deltas[0].Text != "你好" {
		t.Fatalf("文本增量为 %v，期望一条 %q", deltas, "你好")
	}
	usages := rec.all(EventUsage)
	if len(usages) != 1 || usages[0].Usage == nil || usages[0].Usage.InputTokens != 10 {
		t.Fatalf("用量事件错误：%v", usages)
	}
}

// TestRunTurnToolRoundBackfillsAndContinues covers collect → authorize →
// execute → backfill → next round, with call-order results and the
// read-only bypass of the permission gate.
func TestRunTurnToolRoundBackfillsAndContinues(t *testing.T) {
	round1 := toolRound(
		append(
			append(
				[]model.StreamEvent{{Type: model.EventTextDelta, Index: 0, Delta: "先分析"}},
				toolCallEvents(1, "call-1", "get", `{"path":"a.txt"}`)...,
			),
			toolCallEvents(2, "call-2", "put", `{"path":"b.txt","content":"new"}`)...,
		),
		model.StopToolUse,
	)
	provider := newFakeProvider(round1, textRound("完成", model.StopEndTurn))

	get := &fakeTool{def: toolDef("get"), exec: func(ctx context.Context, inv Invocation) (ToolOutput, error) {
		return ToolOutput{Content: "a 的内容", Summary: "读取 a.txt"}, nil
	}}
	put := &fakeTool{def: toolDef("put"), effect: func(input json.RawMessage) Effect {
		return EffectSideEffect
	}}
	var approvals atomic.Int32
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{get, put}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
		Authorize: func(ctx context.Context, req PermissionRequest) (bool, error) {
			approvals.Add(1)
			return true, nil
		},
	})

	rec := newRecorder()
	req := TurnRequest{
		SessionID: "s1",
		TurnID:    "t1",
		Model:     "test-model",
		History:   []model.Message{userMessage("hi")},
		WorkDir:   "/tmp/demo",
	}
	if err := runTurn(t, agent, req, rec); err != nil {
		t.Fatalf("RunTurn: %v", err)
	}

	assertTerminal(t, rec, EventTurnCompleted, nil)
	if got := approvals.Load(); got != 1 {
		t.Fatalf("授权回调被调用 %d 次，期望 1（只读工具必须绕过授权）", got)
	}

	requests := provider.snapshot()
	if len(requests) != 2 {
		t.Fatalf("模型调用次数为 %d，期望 2", len(requests))
	}
	second := requests[1]
	if len(second.Messages) != 4 {
		t.Fatalf("第二轮请求消息数为 %d，期望 4（system+user+assistant+tool 结果）", len(second.Messages))
	}
	assistant := second.Messages[2]
	if assistant.Role != model.RoleAssistant {
		t.Fatalf("第三条消息角色为 %q，期望 assistant", assistant.Role)
	}
	if len(assistant.Content) != 3 {
		t.Fatalf("assistant 消息内容块数为 %d，期望 3", len(assistant.Content))
	}
	if text, ok := assistant.Content[0].(model.TextBlock); !ok || text.Text != "先分析" {
		t.Fatalf("assistant 首块错误：%T %v", assistant.Content[0], assistant.Content[0])
	}
	if call, ok := assistant.Content[1].(model.ToolCallBlock); !ok || call.Call.ID != "call-1" || call.Call.Name != "get" {
		t.Fatalf("assistant 第二块错误：%T %v", assistant.Content[1], assistant.Content[1])
	}
	if call, ok := assistant.Content[2].(model.ToolCallBlock); !ok || call.Call.ID != "call-2" {
		t.Fatalf("assistant 第三块错误：%T %v", assistant.Content[2], assistant.Content[2])
	}
	results := resultsOf(t, second.Messages[3])
	if len(results) != 2 ||
		results[0].CallID != "call-1" || results[0].IsError || results[0].Content != "a 的内容" ||
		results[1].CallID != "call-2" || results[1].IsError {
		t.Fatalf("回填结果错误：%+v", results)
	}

	// Tool cards: pending → running → completed in model order.
	argsEvents := rec.all(EventToolArgs)
	if len(argsEvents) != 2 ||
		argsEvents[0].Tool.Name != "get" || argsEvents[0].Tool.Status != ToolStatusPending || argsEvents[0].Tool.Detail != `{"path":"a.txt"}` ||
		argsEvents[1].Tool.Name != "put" || argsEvents[1].Tool.Status != ToolStatusPending {
		t.Fatalf("tool_args 事件错误：%v", argsEvents)
	}
	ends := rec.all(EventToolEnd)
	if len(ends) != 2 ||
		ends[0].Tool.Status != ToolStatusCompleted || ends[0].Tool.Summary != "读取 a.txt" ||
		ends[1].Tool.Status != ToolStatusCompleted {
		t.Fatalf("tool_end 事件错误：%v", ends)
	}
	results_ := rec.all(EventToolResult)
	if len(results_) != 2 || results_[0].Text != "a 的内容" || results_[1].Tool.Status != ToolStatusCompleted {
		t.Fatalf("tool_result 事件错误：%v", results_)
	}
	if denied := rec.all(EventPermissionDenied); len(denied) != 0 {
		t.Fatalf("不应出现 permission_denied：%v", denied)
	}

	// Tool invocations received the execution context.
	if get.invocationCount() != 1 || get.calls[0].CallID != "call-1" || get.calls[0].WorkDir != "/tmp/demo" {
		t.Fatalf("get 调用上下文错误：%+v", get.calls)
	}
	if put.invocationCount() != 1 || put.calls[0].Input == nil {
		t.Fatalf("put 调用上下文错误：%+v", put.calls)
	}
}

// TestRunTurnToolFailureContinues: a failing tool becomes an error result and
// the turn keeps going.
func TestRunTurnToolFailureContinues(t *testing.T) {
	round1 := toolRound(
		append(
			toolCallEvents(1, "call-1", "flaky", `{}`),
			toolCallEvents(2, "call-2", "get", `{}`)...,
		),
		model.StopToolUse,
	)
	provider := newFakeProvider(round1, textRound("已处理失败", model.StopEndTurn))
	flaky := &fakeTool{def: toolDef("flaky"), exec: func(ctx context.Context, inv Invocation) (ToolOutput, error) {
		return ToolOutput{}, errors.New("磁盘已满")
	}}
	get := &fakeTool{def: toolDef("get")}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{flaky, get}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)

	ends := rec.all(EventToolEnd)
	if len(ends) != 2 || ends[0].Tool.Status != ToolStatusFailed || ends[1].Tool.Status != ToolStatusCompleted {
		t.Fatalf("tool_end 状态错误：%v", ends)
	}
	failedResults := rec.all(EventToolResult)
	if len(failedResults) != 2 || !strings.Contains(failedResults[0].Text, "磁盘已满") {
		t.Fatalf("失败结果未回传给模型：%v", failedResults)
	}

	second := provider.snapshot()[1]
	results := resultsOf(t, second.Messages[3])
	if len(results) != 2 || !results[0].IsError || results[1].IsError {
		t.Fatalf("回填结果错误标志错误：%+v", results)
	}
}

// TestRunTurnSideEffectDeniedFailsClosed covers every denial path: explicit
// rejection, a failing authorization bridge, and a missing callback. Each
// denies the call, reports permission_denied, backfills an error result and
// keeps the turn alive.
func TestRunTurnSideEffectDeniedFailsClosed(t *testing.T) {
	tests := []struct {
		name      string
		authorize func(ctx context.Context, req PermissionRequest) (bool, error)
	}{
		{"用户拒绝", func(ctx context.Context, req PermissionRequest) (bool, error) { return false, nil }},
		{"授权回调出错", func(ctx context.Context, req PermissionRequest) (bool, error) {
			return true, errors.New("对话框崩溃")
		}},
		{"未配置授权回调", nil},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			round1 := toolRound(toolCallEvents(1, "call-1", "put", `{"path":"b.txt"}`), model.StopToolUse)
			provider := newFakeProvider(round1, textRound("好", model.StopEndTurn))
			put := &fakeTool{def: toolDef("put"), effect: func(input json.RawMessage) Effect {
				return EffectSideEffect
			}}
			agent := New(Dependencies{
				Provider:  provider,
				Tools:     &fakeRegistry{tools: []Tool{put}},
				System:    model.TextMessage(model.RoleSystem, "系统提示词"),
				Authorize: tt.authorize,
			})

			rec := newRecorder()
			err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
			assertTerminal(t, rec, EventTurnCompleted, err)

			denied := rec.all(EventPermissionDenied)
			if len(denied) != 1 || denied[0].Tool.Status != ToolStatusDenied || denied[0].Tool.CallID != "call-1" {
				t.Fatalf("permission_denied 事件错误：%v", denied)
			}
			if put.invocationCount() != 0 {
				t.Fatalf("被拒绝的工具不应执行")
			}
			results := resultsOf(t, provider.snapshot()[1].Messages[3])
			if len(results) != 1 || !results[0].IsError {
				t.Fatalf("拒绝结果未回填错误：%+v", results)
			}
		})
	}
}

// TestRunTurnDeniedPermissionSummary checks the dialog summary derivation.
func TestRunTurnDeniedPermissionSummary(t *testing.T) {
	round1 := toolRound(toolCallEvents(1, "call-1", "bash", `{"command":"rm -rf /"}`), model.StopToolUse)
	provider := newFakeProvider(round1, textRound("好", model.StopEndTurn))
	bash := &fakeTool{def: toolDef("bash"), effect: func(input json.RawMessage) Effect {
		return EffectSideEffect
	}}
	var captured atomic.Pointer[PermissionRequest]
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{bash}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
		Authorize: func(ctx context.Context, req PermissionRequest) (bool, error) {
			captured.Store(&req)
			return false, nil
		},
	})
	rec := newRecorder()
	if err := runTurn(t, agent, TurnRequest{SessionID: "s1", TurnID: "t1", Model: "m", History: []model.Message{userMessage("hi")}}, rec); err != nil {
		t.Fatalf("RunTurn: %v", err)
	}
	req := captured.Load()
	if req == nil {
		t.Fatal("授权回调未被调用")
	}
	if req.SessionID != "s1" || req.TurnID != "t1" || req.CallID != "call-1" || req.ToolName != "bash" {
		t.Fatalf("授权请求身份错误：%+v", req)
	}
	if !strings.Contains(req.Summary, "bash") || !strings.Contains(req.Summary, "rm -rf /") {
		t.Fatalf("授权摘要错误：%q", req.Summary)
	}
}

// TestRunTurnCancelledDuringStream: cancelling ctx while the provider hangs
// ends the turn with turn_cancelled and a nil error.
func TestRunTurnCancelledDuringStream(t *testing.T) {
	provider := newFakeProvider(fakeRound{
		events: []model.StreamEvent{
			{Type: model.EventMessageStart},
			{Type: model.EventTextDelta, Index: 0, Delta: "部分"},
		},
		hangTail: true,
	})
	agent := New(Dependencies{
		Provider: provider,
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	rec := newRecorder()
	errCh := make(chan error, 1)
	go func() {
		errCh <- agent.RunTurn(ctx, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec.persist, rec.emit)
	}()

	if event := rec.waitFor(t, EventTextDelta); event.Text != "部分" {
		t.Fatalf("流式增量为 %q，期望 %q", event.Text, "部分")
	}
	cancel()

	if err := <-errCh; err != nil {
		t.Fatalf("取消后 RunTurn 应返回 nil，得到 %v", err)
	}
	assertTerminal(t, rec, EventTurnCancelled, nil)
	if got := rec.last().StopReason; got != model.StopCancelled {
		t.Fatalf("取消事件停止原因为 %q，期望 cancelled", got)
	}
	if failed := rec.all(EventTurnFailed); len(failed) != 0 {
		t.Fatalf("取消不应产生 turn_failed：%v", failed)
	}
}

// TestRunTurnCancelledDuringToolExecution: a tool honoring ctx produces a
// stopped card plus a synthesized cancelled result, then the turn closes as
// cancelled.
func TestRunTurnCancelledDuringToolExecution(t *testing.T) {
	started := make(chan struct{})
	round1 := toolRound(toolCallEvents(1, "call-1", "get", `{}`), model.StopToolUse)
	provider := newFakeProvider(round1, textRound("不应到达", model.StopEndTurn))
	get := &fakeTool{def: toolDef("get"), exec: func(ctx context.Context, inv Invocation) (ToolOutput, error) {
		close(started)
		<-ctx.Done()
		return ToolOutput{}, ctx.Err()
	}}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{get}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	rec := newRecorder()
	errCh := make(chan error, 1)
	go func() {
		errCh <- agent.RunTurn(ctx, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec.persist, rec.emit)
	}()

	rec.waitFor(t, EventToolStart)
	<-started
	cancel()

	if err := <-errCh; err != nil {
		t.Fatalf("取消后 RunTurn 应返回 nil，得到 %v", err)
	}
	assertTerminal(t, rec, EventTurnCancelled, nil)

	ends := rec.all(EventToolEnd)
	if len(ends) != 1 || ends[0].Tool.Status != ToolStatusStopped {
		t.Fatalf("tool_end 应为 stopped：%v", ends)
	}
	results := rec.all(EventToolResult)
	if len(results) != 1 || results[0].Tool.Status != ToolStatusStopped || results[0].Text == "" {
		t.Fatalf("取消的工具结果错误：%v", results)
	}
}

// TestRunTurnMaxRoundsReached: the round budget fails the turn after exactly
// MaxRounds model calls.
func TestRunTurnMaxRoundsReached(t *testing.T) {
	provider := newFakeProvider(toolRound(toolCallEvents(1, "call-1", "get", `{}`), model.StopToolUse))
	get := &fakeTool{def: toolDef("get")}
	agent := New(Dependencies{
		Provider:  provider,
		Tools:     &fakeRegistry{tools: []Tool{get}},
		System:    model.TextMessage(model.RoleSystem, "系统提示词"),
		MaxRounds: 2,
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnFailed, err)
	if got := len(provider.snapshot()); got != 2 {
		t.Fatalf("模型调用次数为 %d，期望 2", got)
	}
	if text := rec.last().Text; !strings.Contains(text, "2") {
		t.Fatalf("失败说明未包含轮数上限 %q", text)
	}
}

// TestRunTurnStreamErrorFailsTurn: a provider failure becomes turn_failed with
// the unified safe message.
func TestRunTurnStreamErrorFailsTurn(t *testing.T) {
	provider := newFakeProvider(fakeRound{events: []model.StreamEvent{
		{Type: model.EventMessageStart},
		{Type: model.EventTextDelta, Index: 0, Delta: "部分"},
		{Type: model.EventError, Err: model.NewError(model.ErrorTransport, "连接中断")},
	}})
	agent := New(Dependencies{
		Provider: provider,
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnFailed, err)
	if text := rec.last().Text; !strings.Contains(text, "连接中断") {
		t.Fatalf("失败说明未携带安全消息：%q", text)
	}
}

// TestRunTurnEmitFailureAborts: a failing live callback aborts the turn and is
// returned as-is, without a terminal event.
func TestRunTurnEmitFailureAborts(t *testing.T) {
	provider := newFakeProvider(textRound("你好", model.StopEndTurn))
	agent := New(Dependencies{
		Provider: provider,
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})
	rec := newRecorder()
	rec.emitFail[EventUsage] = errSentinelEmit

	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	if !errors.Is(err, errSentinelEmit) {
		t.Fatalf("RunTurn 返回 %v，期望原样返回 emit 错误", err)
	}
	for _, eventType := range rec.types() {
		if eventType == EventTurnCompleted || eventType == EventTurnFailed {
			t.Fatalf("中止后不应再有终态事件：%v", rec.types())
		}
	}
}

// TestRunTurnPersistFailureAborts: a failing journal leg aborts before the
// live leg sees the event.
func TestRunTurnPersistFailureAborts(t *testing.T) {
	provider := newFakeProvider(textRound("你好", model.StopEndTurn))
	agent := New(Dependencies{
		Provider: provider,
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})
	rec := newRecorder()
	rec.persistFail[EventTextDelta] = errSentinelPersist

	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	if !errors.Is(err, errSentinelPersist) {
		t.Fatalf("RunTurn 返回 %v，期望原样返回 append 错误", err)
	}
	got := rec.opsSnapshot()
	if len(got) != 1 || got[0] != "persist:text_delta" {
		t.Fatalf("投递日志为 %v，期望仅一条 persist", got)
	}
}

// TestRunTurnReasoningContinuationRoundTrips: the opaque signature is
// forwarded, journaled and merged into the assistant history for the next
// request; reasoning summary deltas are never forwarded.
func TestRunTurnReasoningContinuationRoundTrips(t *testing.T) {
	round1 := toolRound(
		append([]model.StreamEvent{
			{Type: model.EventReasoningDelta, Index: 0, Delta: "思考中"},
			{Type: model.EventReasoningSummaryDelta, Index: 0, Delta: "摘要不落盘"},
			{Type: model.EventReasoningContinuation, Index: 0, Continuation: "SIG-42"},
		}, toolCallEvents(1, "call-1", "get", `{}`)...),
		model.StopToolUse,
	)
	provider := newFakeProvider(round1, textRound("完成", model.StopEndTurn))
	get := &fakeTool{def: toolDef("get")}
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{tools: []Tool{get}},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)

	continuations := rec.all(EventReasoningContinuation)
	if len(continuations) != 1 || continuations[0].Text != "SIG-42" {
		t.Fatalf("reasoning_continuation 事件错误：%v", continuations)
	}
	deltas := rec.all(EventReasoningDelta)
	if len(deltas) != 1 || deltas[0].Text != "思考中" {
		t.Fatalf("reasoning_delta 事件错误：%v", deltas)
	}
	for _, event := range rec.snapshot() {
		if strings.Contains(event.Text, "摘要不落盘") {
			t.Fatalf("摘要增量不应出现在时间线中：%+v", event)
		}
	}

	second := provider.snapshot()[1]
	assistant := second.Messages[2]
	reasoning, ok := assistant.Content[0].(model.ReasoningBlock)
	if !ok || reasoning.Text != "思考中" || reasoning.Signature != "SIG-42" {
		t.Fatalf("assistant 推理块未合并签名：%T %v", assistant.Content[0], assistant.Content[0])
	}
}

// TestRunTurnUnknownToolFailsCallAndContinues: an unregistered tool becomes a
// failed result and the turn keeps going.
func TestRunTurnUnknownToolFailsCallAndContinues(t *testing.T) {
	round1 := toolRound(toolCallEvents(1, "call-1", "nope", `{}`), model.StopToolUse)
	provider := newFakeProvider(round1, textRound("改用其它方式", model.StopEndTurn))
	agent := New(Dependencies{
		Provider: provider,
		Tools:    &fakeRegistry{},
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnCompleted, err)

	ends := rec.all(EventToolEnd)
	if len(ends) != 1 || ends[0].Tool.Status != ToolStatusFailed {
		t.Fatalf("未知工具卡片状态错误：%v", ends)
	}
	results := rec.all(EventToolResult)
	if len(results) != 1 || !strings.Contains(results[0].Text, "未知工具") {
		t.Fatalf("未知工具结果错误：%v", results)
	}
	resultsBack := resultsOf(t, provider.snapshot()[1].Messages[3])
	if len(resultsBack) != 1 || !resultsBack[0].IsError {
		t.Fatalf("未知工具回填错误：%+v", resultsBack)
	}
}

// TestRunTurnTruncatedToolCallUnderMaxTokens: an argument transport cut by the
// output limit is stripped instead of failing the assembly; the round ends as
// a failure because no executable call remains.
func TestRunTurnTruncatedToolCallUnderMaxTokens(t *testing.T) {
	provider := newFakeProvider(fakeRound{events: []model.StreamEvent{
		{Type: model.EventMessageStart},
		{Type: model.EventToolCallStart, Index: 0, CallID: "call-1", Name: "get"},
		{Type: model.EventToolCallArgsDelta, Index: 0, Delta: `{"path":"a`},
		{Type: model.EventMessageEnd, StopReason: model.StopMaxTokens},
	}})
	agent := New(Dependencies{
		Provider: provider,
		System:   model.TextMessage(model.RoleSystem, "系统提示词"),
	})

	rec := newRecorder()
	err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
	assertTerminal(t, rec, EventTurnFailed, err)
	if args := rec.all(EventToolArgs); len(args) != 0 {
		t.Fatalf("被截断的调用不应产生工具卡片：%v", args)
	}
	if text := rec.last().Text; !strings.Contains(text, "长度上限") {
		t.Fatalf("失败说明错误：%q", text)
	}
}

// TestRunTurnTerminalReasonMapping covers the no-call stop reasons against the
// terminal events they produce.
func TestRunTurnTerminalReasonMapping(t *testing.T) {
	tests := []struct {
		name       string
		stop       model.StopReason
		want       EventType
		wantSubstr string
	}{
		{"end_turn", model.StopEndTurn, EventTurnCompleted, ""},
		{"max_tokens", model.StopMaxTokens, EventTurnFailed, "长度上限"},
		{"content_filter", model.StopContentFilter, EventTurnFailed, "内容策略"},
		{"tool_use 无调用", model.StopToolUse, EventTurnFailed, "tool_use"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			provider := newFakeProvider(fakeRound{events: []model.StreamEvent{
				{Type: model.EventMessageStart},
				{Type: model.EventMessageEnd, StopReason: tt.stop},
			}})
			agent := New(Dependencies{
				Provider: provider,
				System:   model.TextMessage(model.RoleSystem, "系统提示词"),
			})
			rec := newRecorder()
			err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: []model.Message{userMessage("hi")}}, rec)
			assertTerminal(t, rec, tt.want, err)
			if tt.wantSubstr != "" && !strings.Contains(rec.last().Text, tt.wantSubstr) {
				t.Fatalf("失败说明 %q 未包含 %q", rec.last().Text, tt.wantSubstr)
			}
		})
	}
}

// TestRunTurnContextAssembly covers the system prompt precedence rules and the
// empty-context failure.
func TestRunTurnContextAssembly(t *testing.T) {
	defaultSystem := model.TextMessage(model.RoleSystem, "默认系统提示词")
	historySystem := model.TextMessage(model.RoleSystem, "历史系统提示词")

	tests := []struct {
		name      string
		system    model.Message
		history   []model.Message
		wantFirst string // expected first message text; "" expects zero messages
		wantLen   int
	}{
		{"历史自带系统提示词优先", defaultSystem, []model.Message{historySystem, userMessage("hi")}, "历史系统提示词", 2},
		{"默认系统提示词前置", defaultSystem, []model.Message{userMessage("hi")}, "默认系统提示词", 2},
		{"无系统提示词", model.Message{}, []model.Message{userMessage("hi")}, "hi", 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			provider := newFakeProvider(textRound("好", model.StopEndTurn))
			agent := New(Dependencies{Provider: provider, System: tt.system})
			rec := newRecorder()
			err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m", History: tt.history}, rec)
			assertTerminal(t, rec, EventTurnCompleted, err)

			mreq := provider.snapshot()[0]
			if len(mreq.Messages) != tt.wantLen {
				t.Fatalf("消息数为 %d，期望 %d", len(mreq.Messages), tt.wantLen)
			}
			if textOf(t, mreq.Messages[0]) != tt.wantFirst {
				t.Fatalf("首条消息为 %q，期望 %q", textOf(t, mreq.Messages[0]), tt.wantFirst)
			}
		})
	}

	t.Run("空上下文失败", func(t *testing.T) {
		provider := newFakeProvider(textRound("好", model.StopEndTurn))
		agent := New(Dependencies{Provider: provider})
		rec := newRecorder()
		err := runTurn(t, agent, TurnRequest{SessionID: "s", TurnID: "t", Model: "m"}, rec)
		assertTerminal(t, rec, EventTurnFailed, err)
		if got := len(provider.snapshot()); got != 0 {
			t.Fatalf("非法请求不应发起模型调用，实际 %d 次", got)
		}
		if text := rec.last().Text; !strings.Contains(text, "至少需要一条消息") {
			t.Fatalf("失败说明错误：%q", text)
		}
	})
}

// TestRunTurnInputValidation: configuration errors return an error without any
// event.
func TestRunTurnInputValidation(t *testing.T) {
	tests := []struct {
		name string
		deps Dependencies
		req  TurnRequest
		want string
	}{
		{"缺少 Provider", Dependencies{}, TurnRequest{SessionID: "s", TurnID: "t", Model: "m"}, "Provider"},
		{"缺少 SessionID", Dependencies{Provider: newFakeProvider(textRound("x", model.StopEndTurn))}, TurnRequest{TurnID: "t", Model: "m"}, "SessionID"},
		{"缺少 TurnID", Dependencies{Provider: newFakeProvider(textRound("x", model.StopEndTurn))}, TurnRequest{SessionID: "s", Model: "m"}, "TurnID"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := newRecorder()
			err := New(tt.deps).RunTurn(context.Background(), tt.req, rec.persist, rec.emit)
			if err == nil || !strings.Contains(err.Error(), tt.want) {
				t.Fatalf("RunTurn 返回 %v，期望包含 %q", err, tt.want)
			}
			if events := rec.snapshot(); len(events) != 0 {
				t.Fatalf("校验失败不应产生事件：%v", events)
			}
		})
	}
}

// TestBuildSystemPrompt checks the fixed v1 template.
func TestBuildSystemPrompt(t *testing.T) {
	msg := BuildSystemPrompt("/tmp/demo", "darwin")
	if msg.Role != model.RoleSystem {
		t.Fatalf("角色为 %q，期望 system", msg.Role)
	}
	if len(msg.Content) != 1 {
		t.Fatalf("内容块数为 %d，期望 1", len(msg.Content))
	}
	text := msg.Content[0].(model.TextBlock).Text
	for _, want := range []string{"/tmp/demo", "darwin", "工具使用规则", time.Now().Format("2006-01-02")} {
		if !strings.Contains(text, want) {
			t.Fatalf("系统提示词缺少 %q：%q", want, text)
		}
	}
	if err := msg.Validate(); err != nil {
		t.Fatalf("系统提示词校验失败：%v", err)
	}
}
