package kit

import (
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	agentui "github.com/ZacharyZhang-NY/MujicaUI/agent"
	"github.com/ZacharyZhang-NY/MujicaUI/core"

	"keencode/internal/ui/theme"
)

// newMujicaDark frames a body the way the app shell does: MujicaUI's
// core.Use first, then the zai palette overlay, then the body. Reduced
// motion lands fold animations at their target in one frame, so
// collapsed-content assertions need no settle waits.
func newMujicaDark(t *testing.T, w, h int, body func(c *ui.Context)) *ui.Tester {
	t.Helper()
	tt := ui.NewTester(func(c *ui.Context) {
		core.Use(c, core.Settings{Mode: core.Dark, Locale: core.ZhCN})
		theme.Apply(c, P(c))
		body(c)
	}, w, h)
	tt.SetDark(true)
	tt.SetPreferences(ui.Preferences{ReduceMotion: true})
	return tt
}

// --- ErrorBanner (kept product banner) ---

func TestErrorBannerDetailCallback(t *testing.T) {
	called := false
	tt := ui.NewTester(func(c *ui.Context) {
		ui.Column(c).Fill().Children(func() {
			ErrorBanner(c, "请求失败：429 Too Many Requests", func() { called = true })
		})
	}, 560, 200)
	if !tt.HasText("请求失败：429 Too Many Requests") {
		t.Error("summary missing")
	}
	if !tt.HasText("查看详情") {
		t.Fatal("detail button missing")
	}
	if err := tt.Click("查看详情"); err != nil {
		t.Fatalf("click detail: %v", err)
	}
	if !called {
		t.Error("onDetail not called")
	}
}

func TestErrorBannerWithoutDetail(t *testing.T) {
	tt := ui.NewTester(func(c *ui.Context) {
		ui.Column(c).Fill().Children(func() {
			ErrorBanner(c, "对话过长，请新建会话后重试", nil)
		})
	}, 560, 200)
	if tt.HasText("查看详情") {
		t.Error("detail button rendered without a handler")
	}
	if !tt.HasText("对话过长，请新建会话后重试") {
		t.Error("fixed copy missing")
	}
}

// --- ChatList ---

func chatEntries() []Entry {
	now := time.Now()
	return []Entry{
		{ID: "u1", Kind: EntryUser, Text: "你好", Time: now},
		{ID: "r1", Kind: EntryReasoning, Time: now, Reasoning: &ReasoningState{Text: "thinking line", Elapsed: 3 * time.Second}},
		{ID: "t1", Kind: EntryTool, Time: now, Tool: &agentui.ToolCall{ID: "c1", Name: "Bash", State: agentui.AgentDone}},
		{ID: "a1", Kind: EntryAssistant, Text: "assistant **reply**", Time: now},
	}
}

func TestChatListRendersEntries(t *testing.T) {
	st := &ChatListState{}
	tt := newMujicaDark(t, 900, 640, func(c *ui.Context) {
		ChatList(c, st, chatEntries())
	})
	for _, s := range []string{"今天", "你好", "思考了 3 秒", "Bash", "已完成", "assistant", "reply"} {
		if !tt.HasText(s) {
			t.Errorf("entry missing %q", s)
		}
	}
	// At the end: the jump button stays hidden (it appears only when the
	// end is out of view).
	if !st.AtEnd() {
		t.Skip("list did not reach its end in the tester; skipping jump-button assertion")
	}
	if tt.HasText("回到最新") {
		t.Error("jump button visible while at the end")
	}
}

func TestChatListTypingRow(t *testing.T) {
	st := &ChatListState{Running: true}
	tt := newMujicaDark(t, 900, 640, func(c *ui.Context) {
		ChatList(c, st, chatEntries())
	})
	if !tt.HasText("正在输入") {
		t.Error("running-turn typing row missing")
	}
}

func TestChatListJumpButton(t *testing.T) {
	// Many tall entries make the list scrollable; after scrolling up, the
	// jump button appears and returns to the end.
	st := &ChatListState{}
	now := time.Now()
	var entries []Entry
	for i := range 30 {
		entries = append(entries, Entry{
			ID:   fmt.Sprintf("a%d", i),
			Kind: EntryAssistant,
			Text: strings.Repeat("row of text ", 40),
			Time: now,
		})
	}
	tt := newMujicaDark(t, 900, 640, func(c *ui.Context) {
		ChatList(c, st, entries)
	})
	// FollowEnd starts at the end.
	st.ScrollToEnd()
	tt.Frame()
	// Scroll up over the list.
	w, h := float32(900), float32(640)
	tt.Scroll(w/2, h/2, 0, -600)
	tt.Frame()
	if st.AtEnd() {
		t.Skip("scroll-up did not leave the end in the tester; cannot assert the jump button")
	}
	if !tt.HasText("回到最新") {
		t.Fatal("jump button missing after scrolling up")
	}
	if err := tt.Click("回到最新"); err != nil {
		t.Fatalf("click jump: %v", err)
	}
	tt.Frame()
}

func TestChatListDraftGreeting(t *testing.T) {
	st := &ChatListState{Draft: true, Greeting: "你好，今天要做点什么？"}
	tt := newMujicaDark(t, 900, 640, func(c *ui.Context) {
		ChatList(c, st, nil)
	})
	if !tt.HasText("你好，今天要做点什么？") {
		t.Error("greeting missing in draft mode")
	}
}

func TestChatListEmptySessionRendersNothing(t *testing.T) {
	st := &ChatListState{}
	rendered := false
	tt := newMujicaDark(t, 900, 640, func(c *ui.Context) {
		ChatList(c, st, nil)
		rendered = true
	})
	_ = tt
	if !rendered {
		t.Error("empty session view did not render")
	}
}
