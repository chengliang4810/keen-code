package kit

import (
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	agentui "github.com/ZacharyZhang-NY/MujicaUI/agent"
	"github.com/ZacharyZhang-NY/MujicaUI/core"
)

// --- ReasoningBlock (chat.ThinkingBlock adapter) ---

func reasoningView(c *ui.Context, st *ReasoningState) {
	core.Use(c, core.Settings{Mode: core.Dark, Locale: core.ZhCN})
	ui.Column(c).Fill().Children(func() {
		ReasoningBlock(c, st)
	})
}

func newReasoningTester(t *testing.T, st *ReasoningState) *ui.Tester {
	t.Helper()
	tt := ui.NewTester(func(c *ui.Context) { reasoningView(c, st) }, 480, 400)
	tt.SetDark(true)
	tt.SetPreferences(ui.Preferences{ReduceMotion: true})
	return tt
}

func TestReasoningDefaultCollapsed(t *testing.T) {
	st := &ReasoningState{Elapsed: 12 * time.Second, Text: "first thought\nlatest thought"}
	tt := newReasoningTester(t, st)
	// Default state: the done label shows, the body does not.
	if !tt.HasText("思考了 12 秒") {
		t.Error("done label missing")
	}
	if tt.HasText("first thought") {
		t.Error("collapsed body leaked")
	}
}

func TestReasoningExpandCollapse(t *testing.T) {
	st := &ReasoningState{Elapsed: 3 * time.Second, Text: "first thought\nlatest thought"}
	tt := newReasoningTester(t, st)
	if st.Open {
		t.Fatal("must start collapsed")
	}
	if err := tt.Click("思考了 3 秒"); err != nil {
		t.Fatalf("click trigger: %v", err)
	}
	if !st.Open {
		t.Error("expand not registered")
	}
	if !tt.HasText("first thought") {
		t.Error("expanded body missing")
	}
	// Collapse again.
	if err := tt.Click("思考了 3 秒"); err != nil {
		t.Fatalf("click trigger: %v", err)
	}
	if st.Open {
		t.Error("collapse not registered")
	}
	if tt.HasText("first thought") {
		t.Error("body still visible after collapse")
	}
}

func TestReasoningRunningLabel(t *testing.T) {
	st := &ReasoningState{Running: true, Started: time.Now().Add(-3 * time.Second), Text: "thinking…"}
	tt := newReasoningTester(t, st)
	if !tt.HasText("思考中") {
		t.Error("running label missing")
	}
	if tt.HasText("思考了") {
		t.Error("done label shown while running")
	}
}

func TestReasoningEmptyNotRendered(t *testing.T) {
	rendered := false
	tt := ui.NewTester(func(c *ui.Context) {
		reasoningView(c, &ReasoningState{Running: true})
		rendered = true
	}, 480, 400)
	_ = tt
	if !rendered {
		t.Error("view did not render")
	}
	if e := ReasoningBlock(nil, &ReasoningState{Running: true}); e != nil {
		t.Error("empty reasoning returned an element")
	}
}

// --- ToolCallCard (via ChatList) ---

func toolEntry() Entry {
	return Entry{
		ID:   "t1",
		Kind: EntryTool,
		Time: time.Now(),
		Tool: &agentui.ToolCall{
			ID:       "c1",
			Name:     "Bash",
			Args:     `{"command":"go test ./..."}`,
			Result:   "PASS\nok\tkeencode\t0.2s",
			State:    agentui.AgentDone,
			Duration: 420 * time.Millisecond,
		},
	}
}

func TestToolCardCollapsedSummary(t *testing.T) {
	st := &ChatListState{}
	tt := newMujicaDark(t, 560, 400, func(c *ui.Context) {
		ChatList(c, st, []Entry{toolEntry()})
	})
	for _, s := range []string{"Bash", "已完成"} {
		if !tt.HasText(s) {
			t.Errorf("summary missing %q", s)
		}
	}
	if tt.HasText("PASS") {
		t.Error("result leaked while collapsed")
	}
}

func TestToolCardExpandArgsAndResult(t *testing.T) {
	st := &ChatListState{}
	tt := newMujicaDark(t, 560, 400, func(c *ui.Context) {
		ChatList(c, st, []Entry{toolEntry()})
	})
	// The 参数 and 结果 sections fold independently.
	if err := tt.Click("参数"); err != nil {
		t.Fatalf("click args toggle: %v", err)
	}
	if !tt.HasText(`"command"`) {
		t.Error("args JSON missing after expanding 参数")
	}
	if err := tt.Click("结果"); err != nil {
		t.Fatalf("click result toggle: %v", err)
	}
	if !tt.HasText("PASS") {
		t.Error("result missing after expanding 结果")
	}
	// Collapse the result again.
	if err := tt.Click("结果"); err != nil {
		t.Fatalf("click result toggle: %v", err)
	}
	if tt.HasText("PASS") {
		t.Error("result still visible after collapse")
	}
}

func TestToolCardRunningAndFailedStates(t *testing.T) {
	running := toolEntry()
	running.Tool = &agentui.ToolCall{ID: "c1", Name: "Bash", State: agentui.AgentRunning}
	tt := newMujicaDark(t, 560, 300, func(c *ui.Context) {
		ChatList(c, &ChatListState{}, []Entry{running})
	})
	if !tt.HasText("执行中") {
		t.Error("running status word missing")
	}

	failed := toolEntry()
	failed.Tool = &agentui.ToolCall{ID: "c2", Name: "Bash", State: agentui.AgentFailed, Error: "exit 1"}
	tt2 := newMujicaDark(t, 560, 300, func(c *ui.Context) {
		ChatList(c, &ChatListState{}, []Entry{failed})
	})
	if !tt2.HasText("失败") {
		t.Error("failed status word missing")
	}
	if !tt2.HasText("错误") {
		t.Error("error section title missing")
	}
}
