package app

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"

	"keencode/internal/config"
	"keencode/internal/runtime"
)

// Bridge tests for the service layer: the plan-signature surface on top of
// the real runtime and config packages, exercised with temporary data
// roots and a scripted turn runner.

// scriptedRunner replays a fixed event list through both sink callbacks,
// mirroring the loop's append-then-emit contract. Event ids are made
// unique per position: a reused id with different content fails the turn
// at the journal (the idempotent identity contract).
type scriptedRunner struct {
	events []runtime.Event
}

// RunTurn emits the scripted events and returns.
func (s scriptedRunner) RunTurn(ctx context.Context, req runtime.TurnRequest,
	journal func(runtime.Event) error, emit func(runtime.Event) error) error {
	for i, ev := range s.events {
		ev.SessionID = req.SessionID
		ev.TurnID = req.TurnID
		if ev.ID == "" {
			ev.ID = fmt.Sprintf("scripted-%d", i)
		}
		if journal != nil {
			if err := journal(ev); err != nil {
				return err
			}
		}
		if emit != nil {
			if err := emit(ev); err != nil {
				return err
			}
		}
	}
	return nil
}

// newTestHarness builds the full bridge on temporary data roots. opts, when
// given, supplies the manager options (agent assembly); otherwise the
// manager has none and Sends fail with ErrNoAgent. seed, when given, runs
// before the state root is built, so config snapshots see it.
func newTestHarness(t *testing.T, opts func() runtime.ManagerOptions, seed func(*Services)) (*Services, *App) {
	t.Helper()
	store, err := config.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	svcs := NewServices(store)
	if seed != nil {
		seed(svcs)
	}
	// Headless assemblies cannot open native dialogs: the picker answers
	// with a fixed temporary directory, so the header's 选择目录 button
	// drives the real goroutine path in tests.
	pickerDir := t.TempDir()
	svcs.Project.setOpenDialog(func(mygo.OpenDialogOptions) ([]string, error) {
		return []string{pickerDir}, nil
	})
	var mgrOpts runtime.ManagerOptions
	if opts != nil {
		mgrOpts = opts()
	}
	mgr, err := runtime.OpenManager(t.TempDir(), mgrOpts)
	if err != nil {
		t.Fatalf("open manager: %v", err)
	}
	svcs.AttachManager(mgr)
	return svcs, Root(svcs)
}

// seedProvider persists one provider record with its first model selected
// (the validators require the active pair whenever providers exist).
func seedProvider(t *testing.T, svcs *Services, id string) {
	t.Helper()
	record, err := config.NewProviderRecord(id, "测试供应商", "https://api.test.local/v1", config.ProtocolMessages, []string{"模型一"}, nil)
	if err != nil {
		t.Fatalf("provider record: %v", err)
	}
	state := config.DefaultProvidersState()
	state.Providers = []config.ProviderRecord{record}
	activeProvider, activeModel := id, "模型一"
	state.ActiveProviderID = &activeProvider
	state.ActiveModelID = &activeModel
	if err := svcs.Settings.ReplaceProviders(state); err != nil {
		t.Fatalf("save provider: %v", err)
	}
}

// replayInto applies a session's full history to the app projection. The
// subscription channel has no end-of-replay marker, so the drain stops
// after an idle window instead of blocking on the next receive.
func replayInto(t *testing.T, svcs *Services, a *App, id string) {
	t.Helper()
	ch, cancel, err := svcs.Sessions.Events(id)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	defer cancel()
	var batch []runtime.Event
	for {
		select {
		case ev, ok := <-ch:
			if !ok {
				a.deliver(id, batch)
				return
			}
			batch = append(batch, ev)
		case <-time.After(200 * time.Millisecond):
			a.deliver(id, batch)
			return
		}
	}
}

// waitFor spins until cond holds or the deadline passes.
func waitFor(t *testing.T, cond func() bool, what string) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

// waitForPick spins frames until the fake directory picker's outcome has
// been drained into the state (drainPicks runs inside a frame).
func waitForPick(t *testing.T, tt *ui.Tester, a *App) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		tt.Frame()
		if a.draftDir != "" {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatal("timed out waiting for project directory pick")
}

// TestSessionServiceSendDraftConversion covers the draft→session
// conversion: Send("") creates the session from the passed directory, the
// user message lands in the journal, and a failed agent assembly removes
// the empty shell again.
func TestSessionServiceSendDraftConversion(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, nil) // no agent factory: the Send must fail

	if _, err := svcs.Sessions.send("", "", "hello"); err == nil {
		t.Fatal("send without a project directory must fail")
	}
	if metas, _ := svcs.Sessions.List(); len(metas) != 0 {
		t.Fatalf("a rejected send created %d sessions, want 0", len(metas))
	}

	svcs.Sessions.runner.setAsk(func(opts mygo.MessageOptions) (mygo.MessageResult, error) {
		t.Error("no permission dialog may open in this test")
		return mygo.MessageResult{}, nil
	})
	sess, err := svcs.Sessions.send("", "/nonexistent-project", "帮我看看")
	if err == nil {
		t.Fatal("send without an agent factory must fail")
	}
	if sess == nil {
		t.Fatal("the created session must be reported even on failure")
	}
	if meta := sess.Meta(); meta.ProjectDir != "/nonexistent-project" {
		t.Errorf("project dir = %q, want the passed directory", meta.ProjectDir)
	}
	// The app layer cleans the empty shell up; the service also exposes the
	// pieces it needs to do so.
	if err := svcs.Sessions.Delete(sess.Meta().ID); err != nil {
		t.Fatalf("cleanup delete: %v", err)
	}
	if metas, _ := svcs.Sessions.List(); len(metas) != 0 {
		t.Fatalf("cleanup left %d sessions, want 0", len(metas))
	}
}

// TestSessionServiceSendThroughScriptedTurn drives a full turn through the
// real runtime: send → turn goroutine → journal → subscription replay.
func TestSessionServiceSendThroughScriptedTurn(t *testing.T) {
	script := []runtime.Event{
		{Type: runtime.EventTextDelta, Text: "你好"},
		{Type: runtime.EventTextDelta, Text: "，需要什么？"},
		{Type: runtime.EventTurnCompleted, StopReason: "end_turn"},
	}
	svcs, _ := newTestHarness(t, func() runtime.ManagerOptions {
		return runtime.ManagerOptions{
			Agent: func() (runtime.TurnRunner, error) { return scriptedRunner{events: script}, nil },
			Model: func() string { return "测试模型" },
		}
	}, nil)

	sess, err := svcs.Sessions.send("", t.TempDir(), "打个招呼")
	if err != nil {
		t.Fatalf("send: %v", err)
	}
	id := sess.Meta().ID
	if meta := sess.Meta(); meta.Title != "打个招呼" {
		t.Errorf("auto title = %q, want the first message", meta.Title)
	}
	waitFor(t, func() bool { return !svcs.Sessions.Running(id) }, "turn completion")

	ch, cancel, err := svcs.Sessions.Events(id)
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}
	defer cancel()
	var types []runtime.EventType
	for {
		select {
		case ev, ok := <-ch:
			if !ok {
				types = nil
			} else {
				types = append(types, ev.Type)
				if ev.Type == runtime.EventTextDelta && ev.Seq <= 0 {
					t.Errorf("journal event Seq not set: %+v", ev)
				}
				continue
			}
		case <-time.After(200 * time.Millisecond):
		}
		break
	}
	assertReplayTypes(t, types)
	if err := svcs.Sessions.Rename(id, "自定义标题"); err != nil {
		t.Fatalf("rename: %v", err)
	}
	for _, meta := range mustList(t, svcs) {
		if meta.ID == id && meta.Title != "自定义标题" {
			t.Fatalf("title after rename = %q", meta.Title)
		}
	}
}

// assertReplayTypes checks the replayed order of the scripted turn.
func assertReplayTypes(t *testing.T, types []runtime.EventType) {
	t.Helper()
	want := []runtime.EventType{runtime.EventUserMessage,
		runtime.EventTextDelta, runtime.EventTextDelta, runtime.EventTurnCompleted}
	if len(types) != len(want) {
		t.Fatalf("replay types = %v, want %v", types, want)
	}
	for i := range want {
		if types[i] != want[i] {
			t.Fatalf("replay types = %v, want %v", types, want)
		}
	}
}

// mustList is List with fail-fast error handling.
func mustList(t *testing.T, svcs *Services) []runtime.SessionMeta {
	t.Helper()
	metas, err := svcs.Sessions.List()
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	return metas
}

// TestDraftPersistence covers the draft service face on both targets.
func TestDraftPersistence(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, nil)

	svcs.Sessions.SetDraftProjectDir("/tmp/proj")
	if err := svcs.Sessions.SaveDraft("", "未发送的草稿"); err != nil {
		t.Fatalf("save new draft: %v", err)
	}
	text, err := svcs.Sessions.Draft("")
	if err != nil || text != "未发送的草稿" {
		t.Fatalf("draft(\"\") = %q, %v", text, err)
	}
	dir, got, ok, err := svcs.Sessions.NewDraft()
	if err != nil || !ok || got != "未发送的草稿" || dir != "/tmp/proj" {
		t.Fatalf("NewDraft = %q %q %v %v", dir, got, ok, err)
	}
	// Clearing removes the file.
	svcs.Sessions.SetDraftProjectDir("")
	if err := svcs.Sessions.SaveDraft("", ""); err != nil {
		t.Fatalf("clear new draft: %v", err)
	}
	if _, _, ok, _ := svcs.Sessions.NewDraft(); ok {
		t.Error("cleared draft resurrected")
	}

	// Session drafts.
	meta, err := svcs.Sessions.Create(t.TempDir())
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	if err := svcs.Sessions.SaveDraft(meta.ID, "会话草稿"); err != nil {
		t.Fatalf("save session draft: %v", err)
	}
	if text, _ := svcs.Sessions.Draft(meta.ID); text != "会话草稿" {
		t.Fatalf("session draft = %q", text)
	}
}

// TestSettingsBridge covers the provider/model selection persistence.
func TestSettingsBridge(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })

	providerID, ok := svcs.Settings.ActiveProviderID()
	if !ok || providerID != "p1" {
		t.Fatalf("active provider = %q, %v", providerID, ok)
	}
	if modelID, ok := svcs.Settings.ActiveModelID(); !ok || modelID != "模型一" {
		t.Fatalf("active model = %q, %v", modelID, ok)
	}
	if err := svcs.Settings.SetDefaultModel("p1", "不存在"); err == nil {
		t.Error("selecting an unknown model must fail")
	}
	if err := svcs.Settings.SetTheme(config.ThemeLight); err != nil {
		t.Fatalf("set theme: %v", err)
	}
	if err := svcs.Settings.SetTheme("neon"); err == nil {
		t.Error("an unknown theme must fail")
	}
	if err := svcs.Settings.DeleteProvider("p1"); err != nil {
		t.Fatalf("delete provider: %v", err)
	}
	if _, ok := svcs.Settings.ActiveProviderID(); ok {
		t.Error("deleting the active provider must clear the selection")
	}
}

// TestProviderForMapping checks the endpoint→adapter mapping without any
// network activity (construction only).
func TestProviderForMapping(t *testing.T) {
	record, err := config.NewProviderRecord("p", "P", "https://api.test.local/v1", config.ProtocolMessages, []string{"m"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := providerFor(record); err != nil {
		t.Errorf("messages mapping failed: %v", err)
	}
	chat, err := config.NewProviderRecord("c", "C", "https://api.test.local/v1/chat/completions", config.ProtocolChatCompletions, []string{"m"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := providerFor(chat); err != nil {
		t.Errorf("chat mapping failed: %v", err)
	}
	responses, err := config.NewProviderRecord("r", "R", "https://api.test.local/v1/responses", config.ProtocolResponses, []string{"m"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := providerFor(responses); err == nil {
		t.Error("the deferred Responses protocol must fail construction")
	}
	bad, err := config.NewProviderRecord("b", "B", "not-a-url", config.ProtocolMessages, []string{"m"}, nil)
	if err == nil {
		t.Fatal("an invalid base URL must fail at record construction")
	}
	if _, err := providerFor(bad); err == nil {
		t.Error("an invalid record must fail endpoint resolution")
	}
}

// TestRuntimeOptionsWiring proves the two-step assembly: options built
// before AttachManager still route turns through the services' own agent
// assembly (late binding), here failing with the no-provider message
// because the fresh store has none.
func TestRuntimeOptionsWiring(t *testing.T) {
	store, err := config.OpenStore(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	svcs := NewServices(store)
	opts := svcs.RuntimeOptions() // taken before the manager exists
	mgr, err := runtime.OpenManager(t.TempDir(), opts)
	if err != nil {
		t.Fatal(err)
	}
	svcs.AttachManager(mgr)
	// The send goes through the service so the workDir handoff runs; the
	// fresh store has no provider, and the error must come from the app's
	// own agent assembly.
	_, err = svcs.Sessions.send("", t.TempDir(), "hi")
	if err == nil {
		t.Fatal("send without a configured provider must fail")
	}
	if !strings.Contains(err.Error(), "尚未选择模型供应商") {
		t.Fatalf("send error = %v, want the no-provider message from the app assembly", err)
	}
	// The failed send must not have left an empty session behind.
	if metas, _ := svcs.Sessions.List(); len(metas) != 0 {
		t.Fatalf("failed send left %d sessions, want 0", len(metas))
	}
}
