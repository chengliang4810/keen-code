package app

import (
	"context"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	"keencode/internal/runtime"
)

// Headless view tests (mygo ui.NewTester): the ask's five scenarios —
// empty state, message order, streaming append, tool-card expand, the
// input disabled state — plus the send flow, the rename/delete dialogs and
// the debounced draft persistence. All of them drive the real App over a
// real runtime manager and config store on temporary roots; the turn
// runner is scripted, and no window is bound (the test goroutine plays the
// pump by calling deliver).

// TestViewDraftEmptyState shows the new-session draft: the greeting, the
// chooser header, the sidebar new-task row, and a focused-able editor.
func TestViewDraftEmptyState(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	tt := ui.NewTester(a.View, 1100, 720)
	_ = svcs

	if !tt.HasText(Greeting) {
		t.Errorf("greeting missing: %q", tt.Texts())
	}
	if !tt.HasText(NewSessionButton) {
		t.Error("sidebar new-task row missing")
	}
	if !tt.HasText(ChooseProjectButton) {
		t.Error("header chooser missing")
	}
	if !tt.HasText(NoProjectHint) {
		t.Error("no-project hint missing")
	}
	if _, ok := tt.Find("Draft"); !ok {
		t.Error("composer editor (label Draft) missing")
	}
	if _, ok := tt.Find("Send"); !ok {
		t.Error("send button missing")
	}
}

// TestViewEmptySessionListKeepsDraftState verifies the sidebar stays empty
// and the draft placeholder branch is the new-task one (the placeholder
// text itself is not findable, so the state is asserted via the sidebar
// and the model hint).
func TestViewEmptySessionListKeepsDraftState(t *testing.T) {
	svcs, a := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
	tt := ui.NewTester(a.View, 1100, 720)

	if metas, _ := svcs.Sessions.List(); len(metas) != 0 {
		t.Fatalf("fresh harness has %d sessions", len(metas))
	}
	if tt.HasText(UntitledSession) {
		t.Error("no session row may render before any session exists")
	}
	if !tt.HasText("测试供应商 / 模型一") {
		t.Errorf("model selector does not show the seeded model: %q", tt.Texts())
	}
}

// fullTurnEvents scripts one representative turn: reasoning, streamed
// text, a tool call, and the terminal event.
func fullTurnEvents() []runtime.Event {
	return []runtime.Event{
		{ID: "e1", Type: runtime.EventUserMessage, Text: "帮我看下构建"},
		{ID: "e2", Type: runtime.EventReasoningDelta, Text: "先看看依赖版本"},
		{ID: "e3", Type: runtime.EventTextDelta, Text: "回复正文"},
		{ID: "e4", Type: runtime.EventToolArgs, Tool: &runtime.ToolEvent{CallID: "c1", Name: "Write", Status: runtime.ToolStatusPending, Detail: `{"path":"main.go"}`}},
		{ID: "e5", Type: runtime.EventToolEnd, Tool: &runtime.ToolEvent{CallID: "c1", Name: "Write", Status: runtime.ToolStatusCompleted, Summary: "main.go", Detail: "ok"}},
		{ID: "e6", Type: runtime.EventTurnCompleted},
	}
}

// openScriptedSession creates a session and pre-plays the scripted events
// into the projection (the headless stand-in for the replay+pump paths),
// then opens it in the shell.
func openScriptedSession(t *testing.T, svcs *Services, a *App, events []runtime.Event) string {
	t.Helper()
	meta, err := svcs.Sessions.Create(t.TempDir())
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	a.openSession(nil, meta.ID)
	a.deliver(meta.ID, events)
	return meta.ID
}

// TestViewMessageOrder checks the timeline order: header, user bubble,
// reasoning, assistant text, tool card.
func TestViewMessageOrder(t *testing.T) {
	svcs, a := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
	id := openScriptedSession(t, svcs, a, fullTurnEvents())
	_ = id
	tt := ui.NewTester(a.View, 1100, 720)

	texts := tt.Texts()
	index := func(s string) int { return slices.Index(texts, s) }
	// The reasoning body is folded inside the ThinkingBlock, so the block's
	// title anchors its position.
	indexContaining := func(s string) int {
		for i, text := range texts {
			if strings.Contains(text, s) {
				return i
			}
		}
		return -1
	}
	user, reasoning, assistant, tool := index("帮我看下构建"), indexContaining("思考了"), index("回复正文"), index("Write")
	for name, at := range map[string]int{"user": user, "reasoning": reasoning, "assistant": assistant, "tool": tool} {
		if at < 0 {
			t.Fatalf("%s text missing from %q", name, texts)
		}
	}
	if !(user < reasoning && reasoning < assistant && assistant < tool) {
		t.Errorf("timeline order wrong: user=%d reasoning=%d assistant=%d tool=%d", user, reasoning, assistant, tool)
	}
	if !tt.HasText("已完成") {
		t.Errorf("tool status word missing: %q", texts)
	}
}

// TestViewStreamingAppend grows the assistant body across two delivered
// deltas with a frame between them.
func TestViewStreamingAppend(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	id, err := svcs.Sessions.Create(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	a.openSession(nil, id.ID)
	tt := ui.NewTester(a.View, 1100, 720)

	a.deliver(id.ID, []runtime.Event{{ID: "u1", Type: runtime.EventUserMessage, Text: "问题"}, {ID: "d1", Type: runtime.EventTextDelta, Text: "第一段"}})
	tt.Frame()
	if !tt.HasText("第一段") {
		t.Fatalf("first delta missing: %q", tt.Texts())
	}
	a.deliver(id.ID, []runtime.Event{{ID: "d2", Type: runtime.EventTextDelta, Text: "第二段"}})
	tt.Frame()
	if !tt.HasText("第一段第二段") {
		t.Errorf("second delta did not append into one body: %q", tt.Texts())
	}
	if tt.HasText("第一段\n第二段") {
		t.Error("deltas must land in one paragraph, not separate rows")
	}
}

// TestViewToolCardExpand toggles the tool card's 参数 section and finds
// the raw argument JSON.
func TestViewToolCardExpand(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	id := openScriptedSession(t, svcs, a, []runtime.Event{
		{ID: "u1", Type: runtime.EventUserMessage, Text: "建个文件"},
		{ID: "t1", Type: runtime.EventToolArgs, Tool: &runtime.ToolEvent{CallID: "c1", Name: "Write", Status: runtime.ToolStatusPending, Detail: `{"path":"notes/a.md"}`}},
	})
	_ = id
	tt := ui.NewTester(a.View, 1100, 720)
	// Reduced motion lands the fold at its target in one frame, so the
	// collapsed assertion needs no settle wait.
	tt.SetPreferences(ui.Preferences{ReduceMotion: true})

	if !tt.HasText("待开始") {
		t.Fatalf("pending status word missing: %q", tt.Texts())
	}
	// The raw input JSON stays folded under 参数 while collapsed.
	if tt.HasText(`"notes/a.md"`) {
		t.Error("args leaked while collapsed")
	}
	if err := tt.Click("参数"); err != nil {
		t.Fatalf("click args toggle: %v", err)
	}
	tt.Frame()
	if !tt.HasText(`"notes/a.md"`) {
		t.Errorf("expanded args missing: %q", tt.Texts())
	}
	// Collapse again.
	if err := tt.Click("参数"); err != nil {
		t.Fatalf("click args toggle again: %v", err)
	}
	tt.Frame()
	if tt.HasText(`"notes/a.md"`) {
		t.Error("args still visible after collapsing")
	}
}

// blockingRunner emits one delta and then parks until its context is
// cancelled, simulating a long turn.
type blockingRunner struct{ started chan struct{} }

// RunTurn implements runtime.TurnRunner.
func (b blockingRunner) RunTurn(ctx context.Context, req runtime.TurnRequest,
	journal func(runtime.Event) error, emit func(runtime.Event) error) error {
	if b.started != nil {
		select {
		case <-b.started:
		default:
			close(b.started)
		}
	}
	delta := runtime.Event{ID: "block-delta", Type: runtime.EventTextDelta, Text: "漫长的回答"}
	_ = journal(delta)
	_ = emit(delta)
	<-ctx.Done()
	return nil
}

// TestViewSendDisabledAndRunning covers the composer state machine: no
// send on an empty draft, no send without a model, and the running state
// that swaps Send for Stop and accepts Esc.
func TestViewSendDisabledAndRunning(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil) // no providers configured
	tt := ui.NewTester(a.View, 1100, 720)

	// 1) Empty draft: the Send button exists but a click does nothing.
	if err := tt.Click("Send"); err != nil {
		t.Fatalf("click send: %v", err)
	}
	if metas, _ := svcs.Sessions.List(); len(metas) != 0 {
		t.Fatalf("empty draft created %d sessions", len(metas))
	}

	// 2) Text without a configured model: pressing Enter submits the
	// editor, and the send is refused with the model hint toast (the send
	// button is disabled in this state, so the keyboard path exercises the
	// guard).
	if err := tt.Click(ChooseProjectButton); err != nil {
		t.Fatalf("choose project: %v", err)
	}
	waitForPick(t, tt, a)
	tt.Frame()
	if err := tt.Click("Draft"); err != nil {
		t.Fatalf("focus editor: %v", err)
	}
	tt.Type("帮我写个脚本")
	tt.Key(0, ui.KeyEnter)
	if metas, _ := svcs.Sessions.List(); len(metas) != 0 {
		t.Fatalf("model-less send created %d sessions", len(metas))
	}
	if !slices.Contains(tt.Announcements(), SendNoModelHint) {
		t.Errorf("model hint toast missing: %q", tt.Announcements())
	}
	if a.draftText != "帮我写个脚本" {
		t.Errorf("draft must be retained after a refused send: %q", a.draftText)
	}

	// 3) A running turn swaps the button to Stop and blocks a second send;
	// Esc stops the turn.
	started := make(chan struct{})
	svcs2, a2 := harnessWithRunner(t, blockingRunner{started: started}, func(s *Services) { seedProvider(t, s, "p1") })
	tt2 := ui.NewTester(a2.View, 1100, 720)
	if err := tt2.Click(ChooseProjectButton); err != nil {
		t.Fatalf("choose project: %v", err)
	}
	waitForPick(t, tt2, a2)
	if err := tt2.Click("Draft"); err != nil {
		t.Fatalf("focus editor: %v", err)
	}
	tt2.Type("长任务")
	if err := tt2.Click("Send"); err != nil {
		t.Fatalf("click send: %v", err)
	}
	select {
	case <-started:
	case <-time.After(2 * time.Second):
		t.Fatal("the turn never started")
	}
	waitFor(t, func() bool { return a2.activeView() != nil }, "session view")
	st := a2.activeView()
	waitFor(t, func() bool { return st.running && len(st.entries) > 1 }, "running projection")
	tt2.Frame()
	if _, ok := tt2.Find("Stop"); !ok {
		t.Errorf("running state must show the Stop button: %q", tt2.Texts())
	}
	if _, ok := tt2.Find("Send"); ok {
		t.Error("the Send button must be gone while running (差异 8)")
	}
	// Esc stops the turn (window shortcut, no modal open).
	tt2.Key(0, ui.KeyEscape)
	waitFor(t, func() bool { return !svcs2.Sessions.Running(st.id) }, "turn cancellation")
	replayInto(t, svcs2, a2, st.id)
	tt2.Frame()
	if st.running {
		t.Error("projection stays running after cancellation")
	}
	// A cancelled turn has no text body; the visible signal is the send
	// button returning (the draft is empty, so it shows disabled Send).
	if _, ok := tt2.Find("Stop"); ok {
		t.Errorf("Stop button survived cancellation: %q", tt2.Texts())
	}
	if _, ok := tt2.Find("Send"); !ok {
		t.Errorf("Send button missing after cancellation: %q", tt2.Texts())
	}
	if a2.draftText != "" {
		t.Errorf("draft must be cleared after send, got %q", a2.draftText)
	}
}

// harnessWithRunner is newTestHarness with a fixed agent factory.
func harnessWithRunner(t *testing.T, runner runtime.TurnRunner, seed func(*Services)) (*Services, *App) {
	return newTestHarness(t, func() runtime.ManagerOptions {
		return runtime.ManagerOptions{
			Agent: func() (runtime.TurnRunner, error) { return runner, nil },
			Model: func() string { return "测试模型" },
		}
	}, seed)
}

// TestViewSendFlowEndToEnd types a draft, sends with Enter, and expects
// the session to appear with the user bubble and the scripted reply.
func TestViewSendFlowEndToEnd(t *testing.T) {
	script := []runtime.Event{
		{ID: "r1", Type: runtime.EventTextDelta, Text: "脚本写好了"},
		{ID: "r2", Type: runtime.EventTurnCompleted},
	}
	svcs, a := newTestHarness(t, func() runtime.ManagerOptions {
		return runtime.ManagerOptions{
			Agent: func() (runtime.TurnRunner, error) { return scriptedRunner{events: script}, nil },
			Model: func() string { return "测试模型" },
		}
	}, func(s *Services) { seedProvider(t, s, "p1") })
	tt := ui.NewTester(a.View, 1100, 720)

	if err := tt.Click(ChooseProjectButton); err != nil {
		t.Fatalf("choose project: %v", err)
	}
	waitForPick(t, tt, a)
	if err := tt.Click("Draft"); err != nil {
		t.Fatalf("focus editor: %v", err)
	}
	tt.Type("帮我写个脚本")
	tt.Key(0, ui.KeyEnter)

	metas := mustList(t, svcs)
	if len(metas) != 1 {
		t.Fatalf("sessions after send = %d, want 1", len(metas))
	}
	id := metas[0].ID
	if metas[0].ProjectDir == "" {
		t.Error("the created session lost its project directory")
	}
	waitFor(t, func() bool { return !svcs.Sessions.Running(id) }, "turn completion")
	replayInto(t, svcs, a, id)
	tt.Frame()

	if !tt.HasText("帮我写个脚本") {
		t.Errorf("user bubble missing: %q", tt.Texts())
	}
	if !tt.HasText("脚本写好了") {
		t.Errorf("assistant reply missing: %q", tt.Texts())
	}
	if a.draftText != "" {
		t.Errorf("draft must be cleared after send, got %q", a.draftText)
	}
	if a.activeID != id {
		t.Errorf("active session = %q, want %q", a.activeID, id)
	}
}

// TestViewSendWithEnterKeyViaEditor covers the Enter-submits contract of
// the kit editor inside the real composer (Shift+Enter must not send).
func TestViewSendWithEnterKeyViaEditor(t *testing.T) {
	svcs, a := newTestHarness(t, func() runtime.ManagerOptions {
		return runtime.ManagerOptions{
			Agent: func() (runtime.TurnRunner, error) { return scriptedRunner{}, nil },
			Model: func() string { return "测试模型" },
		}
	}, func(s *Services) { seedProvider(t, s, "p1") })
	tt := ui.NewTester(a.View, 1100, 720)

	if err := tt.Click(ChooseProjectButton); err != nil {
		t.Fatal(err)
	}
	waitForPick(t, tt, a)
	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Type("第一行")
	tt.Key(ui.Shift, ui.KeyEnter) // newline, never send
	if metas := mustList(t, svcs); len(metas) != 0 {
		t.Fatalf("Shift+Enter sent early: %d sessions", len(metas))
	}
	if a.draftText != "第一行\n" {
		t.Fatalf("Shift+Enter must keep the draft with a newline: %q", a.draftText)
	}
	tt.Type("第二行")
	tt.Key(0, ui.KeyEnter) // send
	waitFor(t, func() bool { return len(mustList(t, svcs)) == 1 }, "session creation")
	if a.draftText != "" {
		t.Fatalf("draft must be cleared after send, got %q", a.draftText)
	}
}

// TestViewSessionSwitchingAndRenameDelete exercises the sidebar rows: open
// a session, rename it through the context menu + prompt, delete it
// through the context menu + destructive confirm.
func TestViewSessionSwitchingAndRenameDelete(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	meta, err := svcs.Sessions.Create(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	a.refreshSessions()
	tt := ui.NewTester(a.View, 1100, 720)

	if err := tt.Click(UntitledSession); err != nil {
		t.Fatalf("open session: %v", err)
	}
	if a.activeID != meta.ID {
		t.Fatalf("active = %q, want %q", a.activeID, meta.ID)
	}

	// Rename through the context menu and the prompt dialog: the prompt
	// field is auto-focused with the current title, so select-all + type
	// replaces it, and Enter confirms.
	if err := tt.RightClick(UntitledSession); err != nil {
		t.Fatalf("right click: %v", err)
	}
	if err := tt.ChooseMenuItem("重命名"); err != nil {
		t.Fatalf("choose rename: %v", err)
	}
	if !tt.HasText(RenameTitle) {
		t.Fatalf("rename dialog missing: %q", tt.Texts())
	}
	tt.Key(ui.Cmd, ui.KeyA)
	tt.Type("新标题")
	tt.Key(0, ui.KeyEnter) // Enter confirms the prompt
	waitFor(t, func() bool {
		for _, m := range mustList(t, svcs) {
			if m.ID == meta.ID {
				return m.Title == "新标题"
			}
		}
		return false
	}, "rename to 新标题")
	tt.Frame()
	if !tt.HasText("新标题") {
		t.Errorf("sidebar does not show the new title: %q", tt.Texts())
	}

	// Delete through the context menu and the destructive confirm.
	if err := tt.RightClick("新标题"); err != nil {
		t.Fatalf("right click: %v", err)
	}
	if err := tt.ChooseMenuItem("删除"); err != nil {
		t.Fatalf("choose delete: %v", err)
	}
	if !tt.HasText(DeleteTitle) {
		t.Fatalf("delete dialog missing: %q", tt.Texts())
	}
	if err := tt.Click("删除"); err != nil {
		t.Fatalf("confirm delete: %v", err)
	}
	waitFor(t, func() bool {
		metas, _ := svcs.Sessions.List()
		return len(metas) == 0
	}, "session deletion")
	tt.Frame()
	if tt.HasText("新标题") {
		t.Errorf("deleted session still listed: %q", tt.Texts())
	}
	if a.activeID != "" {
		t.Errorf("after deleting the open session the app must return to the draft state, got %q", a.activeID)
	}
}

// TestViewDraftPersistenceDebounce types into the draft editor and expects
// the runtime draft file to appear only after the 500ms debounce window.
func TestViewDraftPersistenceDebounce(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	tt := ui.NewTester(a.View, 1100, 720)

	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Type("跨重启的草稿")
	tt.Frame() // the frame that observes the change schedules the wake
	if text, _ := svcs.Sessions.Draft(""); text != "" {
		t.Fatalf("draft saved before the debounce elapsed: %q", text)
	}
	time.Sleep(650 * time.Millisecond)
	tt.Frame() // past the deadline: the save runs
	text, err := svcs.Sessions.Draft("")
	if err != nil || text != "跨重启的草稿" {
		t.Fatalf("draft after debounce = %q, %v; want the typed text", text, err)
	}

	// The stored draft must come back in a fresh state root assembly.
	a2 := Root(svcs)
	if a2.draftText != "跨重启的草稿" {
		t.Errorf("restored draft = %q, want the persisted text", a2.draftText)
	}
}

// TestViewErrorBannerAndDetail plays a failed turn and drives the banner's
// detail dialog.
func TestViewErrorBannerAndDetail(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	id := openScriptedSession(t, svcs, a, []runtime.Event{
		{ID: "u1", Type: runtime.EventUserMessage, Text: "坏了"},
		{ID: "f1", Type: runtime.EventTurnFailed, Text: "模型连接失败：connection refused"},
	})
	_ = id
	tt := ui.NewTester(a.View, 1100, 720)

	if !tt.HasText("模型连接失败：connection refused") {
		t.Fatalf("error banner summary missing: %q", tt.Texts())
	}
	if err := tt.Click("查看详情"); err != nil {
		t.Fatalf("open detail: %v", err)
	}
	if !tt.HasText(ErrorDetailTitle) {
		t.Errorf("detail dialog missing: %q", tt.Texts())
	}
	// Sending a new message clears the banner (the projection rule).
	st := a.activeView()
	a.deliver(st.id, []runtime.Event{{ID: "u2", Type: runtime.EventUserMessage, Text: "再试一次"}})
	tt.Frame()
	if tt.HasText("模型连接失败：connection refused") {
		t.Error("banner survived a new user message")
	}
}
