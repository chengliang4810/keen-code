package app

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	"keencode/internal/agent"
	"keencode/internal/config"
	"keencode/internal/runtime"
)

// Windowless view tests for the key flows of docs/go-migration.md §6
// (第一版产品范围) that the earlier files only cover in fragments: one
// continuous new-session flow ending in Stop, switching between two
// existing sessions, the settings form render, and the three permission
// choices landing in the visible timeline.

// gatedRunner is a TurnRunner for the full-flow scenario: it streams an
// opening sequence (two text deltas, then a Write tool call going
// pending → running) and parks until its context is cancelled, at which
// point it journals the turn_cancelled event itself.
type gatedRunner struct {
	started chan struct{}
}

// RunTurn implements runtime.TurnRunner.
func (g gatedRunner) RunTurn(ctx context.Context, req runtime.TurnRequest,
	journal func(runtime.Event) error, emit func(runtime.Event) error) error {
	if g.started != nil {
		select {
		case <-g.started:
		default:
			close(g.started)
		}
	}
	opening := []runtime.Event{
		{ID: "g1", Type: runtime.EventTextDelta, Text: "先分析需求，"},
		{ID: "g2", Type: runtime.EventTextDelta, Text: "然后写入文件"},
		{ID: "g3", Type: runtime.EventToolArgs, Tool: &runtime.ToolEvent{
			CallID: "c1", Name: "Write", Status: runtime.ToolStatusPending, Detail: `{"path":"main.go"}`}},
		{ID: "g4", Type: runtime.EventToolStart, Tool: &runtime.ToolEvent{
			CallID: "c1", Name: "Write", Status: runtime.ToolStatusRunning}},
	}
	for _, e := range opening {
		if err := journal(e); err != nil {
			return err
		}
		if err := emit(e); err != nil {
			return err
		}
	}
	<-ctx.Done()
	cancelled := runtime.Event{ID: "g5", Type: runtime.EventTurnCancelled}
	_ = journal(cancelled)
	_ = emit(cancelled)
	return nil
}

// collectEvents drains a live subscription channel until match fires
// (returning everything read including the matched event) or, with a nil
// match, until the channel stays idle. The idle budget is generous before
// the first event and short afterwards, so a late producer is still
// awaited but a finished stream ends promptly.
func collectEvents(t *testing.T, ch <-chan runtime.Event, match func(runtime.Event) bool) []runtime.Event {
	t.Helper()
	var batch []runtime.Event
	for {
		budget := 300 * time.Millisecond
		if len(batch) == 0 {
			budget = 2 * time.Second
		}
		select {
		case evt, ok := <-ch:
			if !ok {
				return batch
			}
			batch = append(batch, evt)
			if match != nil && match(evt) {
				return batch
			}
		case <-time.After(budget):
			return batch
		}
	}
}

// TestViewNewSessionSendStreamToolCardStop walks the §6.4/§6.5 flow as one
// continuous scenario: the empty draft state, a composer send that creates
// the session, live streaming deltas appending into one body, the tool
// card appearing while the turn runs, and Stop folding it all back.
func TestViewNewSessionSendStreamToolCardStop(t *testing.T) {
	started := make(chan struct{})
	svcs, a := harnessWithRunner(t, gatedRunner{started: started}, func(s *Services) { seedProvider(t, s, "p1") })
	tt := ui.NewTester(a.View, 1100, 720)

	// Empty draft state: the greeting, no sessions.
	if metas := mustList(t, svcs); len(metas) != 0 {
		t.Fatalf("fresh harness has %d sessions", len(metas))
	}
	if !tt.HasText(Greeting) {
		t.Fatalf("empty draft state missing the greeting: %q", tt.Texts())
	}

	// Send from the composer: pick the directory, type, submit.
	if err := tt.Click(ChooseProjectButton); err != nil {
		t.Fatalf("choose project: %v", err)
	}
	waitForPick(t, tt, a)
	if err := tt.Click("Draft"); err != nil {
		t.Fatalf("focus editor: %v", err)
	}
	tt.Type("帮我看下这个仓库")
	if err := tt.Click("Send"); err != nil {
		t.Fatalf("click send: %v", err)
	}
	metas := mustList(t, svcs)
	if len(metas) != 1 {
		t.Fatalf("send created %d sessions, want 1", len(metas))
	}
	id := metas[0].ID
	select {
	case <-started:
	case <-time.After(2 * time.Second):
		t.Fatal("the turn never started")
	}

	// The headless app has no pump goroutine: the test drains the live
	// channel and feeds deliver, playing what the frame pump does in
	// production (win.Update batches).
	ch, cancel, err := svcs.Sessions.Events(id)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	defer cancel()

	// Streaming deltas append into one assistant body (match the second
	// delta so both halves are in the batch).
	a.deliver(id, collectEvents(t, ch, func(evt runtime.Event) bool {
		return evt.Type == runtime.EventTextDelta && strings.Contains(evt.Text, "然后写入文件")
	}))
	tt.Frame()
	if !tt.HasText("帮我看下这个仓库") {
		t.Errorf("user bubble missing: %q", tt.Texts())
	}
	if !tt.HasText("先分析需求，然后写入文件") {
		t.Errorf("streaming deltas did not append into one body: %q", tt.Texts())
	}

	// The tool card appears and runs while the turn is live.
	a.deliver(id, collectEvents(t, ch, func(evt runtime.Event) bool {
		return evt.Type == runtime.EventToolStart
	}))
	tt.Frame()
	if !tt.HasText("Write") {
		t.Errorf("tool card missing: %q", tt.Texts())
	}
	if !tt.HasText("执行中") {
		t.Errorf("running status word missing: %q", tt.Texts())
	}

	// Stop folds the turn: the card shows 已跳过 and Send returns.
	if err := tt.Click("Stop"); err != nil {
		t.Fatalf("click stop: %v", err)
	}
	waitFor(t, func() bool { return !svcs.Sessions.Running(id) }, "turn cancellation")
	a.deliver(id, collectEvents(t, ch, nil))
	tt.Frame()
	if tt.HasText("执行中") {
		t.Errorf("running status survived the stop: %q", tt.Texts())
	}
	if !tt.HasText("已跳过") {
		t.Errorf("stopped status word missing: %q", tt.Texts())
	}
	if _, ok := tt.Find("Stop"); ok {
		t.Error("the Stop button must be gone after the turn ended")
	}
	if _, ok := tt.Find("Send"); !ok {
		t.Errorf("Send button missing after the turn ended: %q", tt.Texts())
	}
	if !tt.HasText("帮我看下这个仓库") {
		t.Error("the user bubble must survive the stop")
	}
}

// TestViewSwitchBetweenSessions covers §6.3 session switching: two
// sessions with distinct histories, per-session composer drafts that
// survive the switch (a switch flushes the visible draft before opening
// the next target), and timelines that never mix.
func TestViewSwitchBetweenSessions(t *testing.T) {
	svcs, a := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
	metaA, err := svcs.Sessions.Create(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	metaB, err := svcs.Sessions.Create(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := svcs.Sessions.Rename(metaA.ID, "会话甲"); err != nil {
		t.Fatal(err)
	}
	if err := svcs.Sessions.Rename(metaB.ID, "会话乙"); err != nil {
		t.Fatal(err)
	}
	a.refreshSessions()
	tt := ui.NewTester(a.View, 1100, 720)

	// Open 甲, give it one message and a composer draft.
	if err := tt.Click("会话甲"); err != nil {
		t.Fatalf("open A: %v", err)
	}
	if a.activeID != metaA.ID {
		t.Fatalf("active = %q, want %q", a.activeID, metaA.ID)
	}
	a.deliver(metaA.ID, []runtime.Event{{ID: "a1", Type: runtime.EventUserMessage, Text: "甲的问题"}})
	if err := tt.Click("Draft"); err != nil {
		t.Fatalf("focus editor: %v", err)
	}
	tt.Type("甲的草稿")

	// Switch to 乙: 甲's draft is flushed to disk, 乙 opens clean.
	if err := tt.Click("会话乙"); err != nil {
		t.Fatalf("open B: %v", err)
	}
	if a.activeID != metaB.ID {
		t.Fatalf("active = %q, want %q", a.activeID, metaB.ID)
	}
	if text, err := svcs.Sessions.Draft(metaA.ID); err != nil || text != "甲的草稿" {
		t.Errorf("A draft after switching = %q, %v; want 甲的草稿", text, err)
	}
	a.deliver(metaB.ID, []runtime.Event{{ID: "b1", Type: runtime.EventUserMessage, Text: "乙的问题"}})
	tt.Frame()
	if !tt.HasText("乙的问题") || tt.HasText("甲的问题") {
		t.Errorf("B timeline mixed: %q", tt.Texts())
	}
	if st := a.activeView(); st.draft != "" {
		t.Errorf("B opens with a leftover draft %q", st.draft)
	}
	if err := tt.Click("Draft"); err != nil {
		t.Fatalf("focus editor in B: %v", err)
	}
	tt.Type("乙的草稿")

	// Back to 甲: its draft and history come back untouched.
	if err := tt.Click("会话甲"); err != nil {
		t.Fatalf("reopen A: %v", err)
	}
	if text, err := svcs.Sessions.Draft(metaB.ID); err != nil || text != "乙的草稿" {
		t.Errorf("B draft after switching back = %q, %v; want 乙的草稿", text, err)
	}
	tt.Frame()
	if st := a.activeView(); st == nil || st.draft != "甲的草稿" {
		t.Errorf("A draft after switching back = %+v; want 甲的草稿", st)
	}
	if !tt.HasText("甲的问题") || tt.HasText("乙的问题") {
		t.Errorf("A timeline mixed: %q", tt.Texts())
	}
	if !tt.HasText("会话乙") {
		t.Error("乙 missing from the sidebar after switching back")
	}
}

// TestSettingsFormRender asserts the §6.7 settings form renders completely
// without mutating anything: the nav, the general rows with their current
// values (closed selects must not leak the unselected options), the
// provider rows with their actions, and every provider form field.
func TestSettingsFormRender(t *testing.T) {
	_, a := newTestHarness(t, nil, func(s *Services) { seedTwoModels(t, s) })
	tt := ui.NewTester(a.View, 1100, 720)
	openSettings(t, tt, a)

	// General panel: nav, row labels and descriptions, current values.
	for _, want := range []string{
		SettingsBack, SettingsTabGeneral, SettingsTabProviders, SettingsGeneralTitle,
		ThemeRowLabel, ThemeRowDesc, ThemeDark,
		DefaultModelRowLabel, DefaultModelRowDesc, "测试供应商 / 模型一",
		PolicyRowLabel, PolicyRowDesc, PolicyAsk,
		ProjectRowLabel, NoProjectHint, ChooseProjectButton,
	} {
		if !tt.HasText(want) {
			t.Errorf("general panel missing %q: %q", want, tt.Texts())
		}
	}
	for _, hidden := range []string{ThemeLight, ThemeSystem, PolicyAllowAll, PolicyReadOnly, "测试供应商 / 模型二"} {
		if tt.HasText(hidden) {
			t.Errorf("closed select leaks option %q", hidden)
		}
	}

	// Providers tab: the seeded record with its protocol, endpoint, model
	// count and row actions.
	if err := tt.Click(SettingsTabProviders); err != nil {
		t.Fatalf("open providers tab: %v", err)
	}
	tt.Frame()
	for _, want := range []string{
		SettingsProvidersTitle, AddProviderButton,
		"测试供应商", ProviderFormMessages, "https://api.test.local/v1",
		"2 个模型", ProviderEditLabel, ProviderDeleteLabel,
	} {
		if !tt.HasText(want) {
			t.Errorf("providers tab missing %q: %q", want, tt.Texts())
		}
	}
	if tt.HasText(ProviderEmptyHint) {
		t.Error("the empty hint must not show next to a provider row")
	}

	// The create form dialog: title, field labels, the protocol select's
	// current option, the save/cancel buttons, and the placeholder-labeled
	// inputs (input contents are not findable, labels are).
	if err := tt.Click(AddProviderButton); err != nil {
		t.Fatalf("open form: %v", err)
	}
	tt.Frame()
	if !tt.HasText(ProviderFormCreateTitle) {
		t.Errorf("form dialog title missing: %q", tt.Texts())
	}
	for _, want := range []string{
		ProviderFormName, ProviderFormProto, ProviderFormURL, ProviderFormKey, ProviderFormModels,
		ProviderFormMessages, ProviderFormSave, "取消",
	} {
		if !tt.HasText(want) {
			t.Errorf("provider form missing %q: %q", want, tt.Texts())
		}
	}
	for _, label := range []string{
		ProviderFormNamePlaceholder, ProviderFormURLPlaceholder,
		ProviderFormKeyPlaceholder, ProviderFormModelsPlaceholder,
	} {
		if _, ok := tt.Find(label); !ok {
			t.Errorf("provider form input %q missing", label)
		}
	}
	if tt.HasText(ProviderFormChat) {
		t.Errorf("closed protocol select leaks option %q", ProviderFormChat)
	}
	if err := tt.Click("取消"); err != nil {
		t.Fatalf("cancel form: %v", err)
	}
}

// permTurnRunner is a TurnRunner that drives two side-effect Bash calls
// through the app's own Authorize bridge, mirroring the loop's event
// sequence (agent/loop.go executeCall): tool_args, then the bridge, then
// permission_denied or tool_start + tool_end per call, and one terminal
// event at the end. Denials keep the turn going, exactly like the loop.
type permTurnRunner struct {
	authorize func(context.Context, agent.PermissionRequest) (bool, error)
}

// RunTurn implements runtime.TurnRunner.
func (r permTurnRunner) RunTurn(ctx context.Context, req runtime.TurnRequest,
	journal func(runtime.Event) error, emit func(runtime.Event) error) error {
	send := func(id string, typ runtime.EventType, tool *runtime.ToolEvent) error {
		ev := runtime.Event{ID: id, Type: typ, Tool: tool}
		if err := journal(ev); err != nil {
			return err
		}
		return emit(ev)
	}
	for i, callID := range []string{"c1", "c2"} {
		if err := send(fmt.Sprintf("p%d-args", i), runtime.EventToolArgs, &runtime.ToolEvent{
			CallID: callID, Name: "Bash", Status: runtime.ToolStatusPending,
			Detail: `{"command":"rm -rf build"}`,
		}); err != nil {
			return err
		}
		allowed, err := r.authorize(ctx, agent.PermissionRequest{
			SessionID: req.SessionID,
			TurnID:    req.TurnID,
			CallID:    callID,
			ToolName:  "Bash",
			Input:     []byte(`{"command":"rm -rf build"}`),
			Summary:   "工具 Bash 请求执行副作用操作：rm -rf build",
		})
		if err != nil {
			return err
		}
		if !allowed {
			if err := send(fmt.Sprintf("p%d-denied", i), runtime.EventPermissionDenied, &runtime.ToolEvent{
				CallID: callID, Name: "Bash", Status: runtime.ToolStatusDenied,
			}); err != nil {
				return err
			}
			continue
		}
		if err := send(fmt.Sprintf("p%d-start", i), runtime.EventToolStart, &runtime.ToolEvent{
			CallID: callID, Name: "Bash", Status: runtime.ToolStatusRunning,
		}); err != nil {
			return err
		}
		if err := send(fmt.Sprintf("p%d-end", i), runtime.EventToolEnd, &runtime.ToolEvent{
			CallID: callID, Name: "Bash", Status: runtime.ToolStatusCompleted, Summary: "build", Detail: "done",
		}); err != nil {
			return err
		}
	}
	return send("p-done", runtime.EventTurnCompleted, nil)
}

// TestViewPermissionChoicesInTurn ties the three dialog choices to the
// visible timeline. The turn routes both side-effect calls through the
// real Authorize bridge with the injected ask seam (the native dialog
// itself cannot render headless), and the frame shows each choice's
// consequence: 拒绝 marks both cards 已跳过 (skipped) while the turn still completes;
// 允许一次 re-prompts and both cards complete; 本会话允许 prompts once for
// the pair.
func TestViewPermissionChoicesInTurn(t *testing.T) {
	cases := []struct {
		name      string
		button    int
		wantAsks  int
		wantCard  string
		otherCard string
	}{
		{"拒绝", 2, 2, "已跳过", "已完成"},
		{"允许一次", 0, 2, "已完成", "已跳过"},
		{"本会话允许", 1, 1, "已完成", "已跳过"},
	}
	for _, tc := range cases {
		store, err := config.OpenStore(t.TempDir())
		if err != nil {
			t.Fatal(err)
		}
		svcs := NewServices(store)
		ask := &askStub{answer: tc.button}
		svcs.Sessions.runner.setAsk(ask.ask)
		mgr, err := runtime.OpenManager(t.TempDir(), runtime.ManagerOptions{
			Agent: func() (runtime.TurnRunner, error) {
				return permTurnRunner{authorize: svcs.Sessions.runner.authorize}, nil
			},
			Model: func() string { return "测试模型" },
		})
		if err != nil {
			t.Fatal(err)
		}
		svcs.AttachManager(mgr)
		a := Root(svcs)

		sess, err := svcs.Sessions.send("", t.TempDir(), "帮我清理构建目录")
		if err != nil {
			t.Fatalf("%s: send: %v", tc.name, err)
		}
		id := sess.Meta().ID
		waitFor(t, func() bool { return !svcs.Sessions.Running(id) }, tc.name+": turn completion")

		a.openSession(nil, id)
		tt := ui.NewTester(a.View, 1100, 720)
		if got := countText(tt.Texts(), "Bash"); got != 2 {
			t.Errorf("%s: %d Bash cards rendered, want 2: %q", tc.name, got, tt.Texts())
		}
		if !tt.HasText(tc.wantCard) {
			t.Errorf("%s: card status %q missing: %q", tc.name, tc.wantCard, tt.Texts())
		}
		if tt.HasText(tc.otherCard) {
			t.Errorf("%s: unexpected card status %q: %q", tc.name, tc.otherCard, tt.Texts())
		}
		if ask.calls != tc.wantAsks {
			t.Errorf("%s: dialogs = %d, want %d", tc.name, ask.calls, tc.wantAsks)
		}
		if svcs.Sessions.Running(id) {
			t.Errorf("%s: the turn must complete", tc.name)
		}
	}
}

// countText counts how many rendered labels are exactly s (the tool card
// renders both a "Name, status" group label and the bare name).
func countText(texts []string, s string) int {
	n := 0
	for _, text := range texts {
		if text == s {
			n++
		}
	}
	return n
}
