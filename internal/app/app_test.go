package app

import (
	"testing"
	"time"

	agentui "github.com/ZacharyZhang-NY/MujicaUI/agent"

	"keencode/internal/runtime"
	"keencode/internal/ui/kit"
)

// Projection tests: runtime events → MujicaUI chat entries, without a
// window.

// ev builds one journal event with a distinct identity.
func ev(seq int64, typ runtime.EventType, mutate func(*runtime.Event)) runtime.Event {
	e := runtime.Event{
		ID:   "id-" + string(typ) + "-" + time.Now().Format("150405.000000000"),
		Type: typ,
		Seq:  seq,
		Time: time.Now(),
	}
	if mutate != nil {
		mutate(&e)
	}
	return e
}

func textDelta(seq int64, text string) runtime.Event {
	return ev(seq, runtime.EventTextDelta, func(e *runtime.Event) { e.Text = text })
}

func toolEvent(seq int64, typ runtime.EventType, callID, name, summary, status, detail string) runtime.Event {
	return ev(seq, typ, func(e *runtime.Event) {
		e.Tool = &runtime.ToolEvent{CallID: callID, Name: name, Summary: summary, Status: status, Detail: detail}
	})
}

// TestProjectionOrderAndStreaming folds a full turn into one projection and
// checks entry order, streaming accumulation, and the closed state.
func TestProjectionOrderAndStreaming(t *testing.T) {
	meta := runtime.SessionMeta{ID: "s1", CreatedAt: time.Now()}
	v := newSessionView(meta)

	v.apply(ev(1, runtime.EventUserMessage, func(e *runtime.Event) { e.Text = "帮我看下构建" }))
	v.apply(ev(2, runtime.EventReasoningDelta, func(e *runtime.Event) { e.Text = "先看错误日志" }))
	v.apply(textDelta(3, "你好"))
	v.apply(textDelta(4, "，世界"))
	v.apply(toolEvent(5, runtime.EventToolArgs, "c1", "Write", "", runtime.ToolStatusPending, `{"path":"a.go"}`))
	v.apply(toolEvent(6, runtime.EventToolStart, "c1", "Write", "", runtime.ToolStatusRunning, ""))
	v.apply(toolEvent(7, runtime.EventToolEnd, "c1", "Write", "写入 a.go", runtime.ToolStatusCompleted, "ok"))
	v.apply(ev(8, runtime.EventTurnCompleted, nil))

	wantKinds := []kit.EntryKind{kit.EntryUser, kit.EntryReasoning, kit.EntryAssistant, kit.EntryTool}
	if len(v.entries) != len(wantKinds) {
		t.Fatalf("entries = %d kinds, want %d", len(v.entries), len(wantKinds))
	}
	for i, kind := range wantKinds {
		if v.entries[i].Kind != kind {
			t.Errorf("entries[%d] kind = %v, want %v", i, v.entries[i].Kind, kind)
		}
	}
	if got := v.entries[2].Text; got != "你好，世界" {
		t.Errorf("assistant source did not accumulate the deltas: %q", got)
	}
	if v.running {
		t.Error("projection stays running after the terminal event")
	}
	call := v.tools["c1"]
	if call == nil || call.State != agentui.AgentDone {
		t.Errorf("tool call = %+v, want done", call)
	}
	if call.Args != `{"path":"a.go"}` || call.Result != "ok" {
		t.Errorf("tool call args/result = %q / %q, want the raw input JSON and the end preview", call.Args, call.Result)
	}
}

// TestProjectionDeduplicatesSeq replays a duplicate and expects no second
// entry.
func TestProjectionDeduplicatesSeq(t *testing.T) {
	v := newSessionView(runtime.SessionMeta{ID: "s2", CreatedAt: time.Now()})
	v.apply(ev(1, runtime.EventUserMessage, func(e *runtime.Event) { e.Text = "第一条" }))
	v.apply(ev(1, runtime.EventUserMessage, func(e *runtime.Event) { e.Text = "第一条" }))
	count := 0
	for _, e := range v.entries {
		if e.Kind == kit.EntryUser {
			count++
		}
	}
	if count != 1 {
		t.Errorf("user entries = %d, want 1 (seq dedup failed)", count)
	}
}

// TestProjectionTurnFailedBanner checks the error banner fields.
func TestProjectionTurnFailedBanner(t *testing.T) {
	v := newSessionView(runtime.SessionMeta{ID: "s3", CreatedAt: time.Now()})
	v.apply(ev(1, runtime.EventUserMessage, func(e *runtime.Event) { e.Text = "hi" }))
	v.apply(ev(2, runtime.EventTurnFailed, func(e *runtime.Event) { e.Text = "模型连接失败" }))
	if v.errSummary != "模型连接失败" || v.errDetail != "模型连接失败" {
		t.Errorf("banner = %q / %q", v.errSummary, v.errDetail)
	}
	v.apply(ev(3, runtime.EventUserMessage, func(e *runtime.Event) { e.Text = "再试" }))
	if v.errSummary != "" {
		t.Errorf("a new user message must clear the banner, got %q", v.errSummary)
	}
}

// TestProjectionToolStates maps every runtime tool status onto the agent
// card state, including the skipped landing of denied and stopped calls.
func TestProjectionToolStates(t *testing.T) {
	v := newSessionView(runtime.SessionMeta{ID: "s4", CreatedAt: time.Now()})
	v.apply(ev(1, runtime.EventUserMessage, func(e *runtime.Event) { e.Text = "go" }))
	v.apply(toolEvent(2, runtime.EventToolArgs, "d1", "Bash", "", runtime.ToolStatusPending, `{"command":"ls"}`))
	if v.tools["d1"].State != agentui.AgentPending {
		t.Errorf("after tool_args: state = %v, want pending", v.tools["d1"].State)
	}
	v.apply(toolEvent(3, runtime.EventToolStart, "d1", "Bash", "", runtime.ToolStatusRunning, ""))
	if v.tools["d1"].State != agentui.AgentRunning {
		t.Errorf("after tool_start: state = %v, want running", v.tools["d1"].State)
	}
	v.apply(ev(4, runtime.EventPermissionDenied, func(e *runtime.Event) {
		e.Tool = &runtime.ToolEvent{CallID: "d1", Name: "Bash", Status: runtime.ToolStatusDenied}
	}))
	if v.tools["d1"].State != agentui.AgentSkipped {
		t.Errorf("after permission_denied: state = %v, want skipped", v.tools["d1"].State)
	}

	v.apply(toolEvent(5, runtime.EventToolArgs, "d2", "Bash", "", runtime.ToolStatusPending, `{"command":"build"}`))
	v.apply(toolEvent(6, runtime.EventToolStart, "d2", "Bash", "", runtime.ToolStatusRunning, ""))
	v.apply(toolEvent(7, runtime.EventToolEnd, "d2", "Bash", "", runtime.ToolStatusFailed, "exit 1"))
	call := v.tools["d2"]
	if call.State != agentui.AgentFailed || call.Error != "exit 1" {
		t.Errorf("failed call = %+v, want failed with the error preview", call)
	}

	v.apply(toolEvent(8, runtime.EventToolArgs, "d3", "Bash", "", runtime.ToolStatusPending, ""))
	v.apply(ev(9, runtime.EventTurnCancelled, nil))
	if v.tools["d3"].State != agentui.AgentSkipped {
		t.Errorf("stranded call after cancel: state = %v, want skipped", v.tools["d3"].State)
	}
}
