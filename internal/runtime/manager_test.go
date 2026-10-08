package runtime

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// noopRunner returns a runner that immediately completes the turn.
func noopRunner() TurnRunner {
	return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
		return journal(Event{ID: newID(), Type: EventTurnCompleted, TurnID: req.TurnID, StopReason: "end_turn"})
	})
}

func TestManagerCreateListOrderAndRename(t *testing.T) {
	root := t.TempDir()
	mgr, err := OpenManager(root, ManagerOptions{Agent: func() (TurnRunner, error) { return noopRunner(), nil }})
	if err != nil {
		t.Fatalf("OpenManager: %v", err)
	}
	first, err := mgr.Create("/tmp/a")
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	time.Sleep(3 * time.Millisecond)
	second, err := mgr.Create("/tmp/b")
	if err != nil {
		t.Fatalf("Create: %v", err)
	}

	metas, err := mgr.List()
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(metas) != 2 {
		t.Fatalf("List = %d entries, want 2", len(metas))
	}
	// UpdatedAt descending: newest first.
	if metas[0].ID != second.Meta().ID || metas[1].ID != first.Meta().ID {
		t.Fatalf("order = [%s %s], want newest first", metas[0].ID, metas[1].ID)
	}

	if err := mgr.Rename(first.Meta().ID, "自定义标题"); err != nil {
		t.Fatalf("Rename: %v", err)
	}
	if got := first.Meta().Title; got != "自定义标题" {
		t.Fatalf("loaded title = %q", got)
	}
	// Persisted: a fresh manager sees it too.
	fresh, err := OpenManager(root, ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	got, err := fresh.Get(first.Meta().ID)
	if err != nil {
		t.Fatalf("fresh Get: %v", err)
	}
	if got.Meta().Title != "自定义标题" || got.Meta().ProjectDir != "/tmp/a" {
		t.Fatalf("fresh meta = %+v", got.Meta())
	}

	// Validation.
	if err := mgr.Rename(first.Meta().ID, "   "); err == nil {
		t.Fatal("empty rename must fail")
	}
	if err := mgr.Rename("NOTANID", "x"); err == nil {
		t.Fatal("invalid id rename must fail")
	}
}

func TestManagerGetLazyReplayAcrossManagers(t *testing.T) {
	root := t.TempDir()
	mgr, err := OpenManager(root, ManagerOptions{Agent: func() (TurnRunner, error) {
		return completedTurnScript(t, &TurnRequest{}), nil
	}})
	if err != nil {
		t.Fatal(err)
	}
	s, err := mgr.Create("/tmp/project")
	if err != nil {
		t.Fatal(err)
	}
	id := s.Meta().ID
	if err := s.Send(context.Background(), "跨进程恢复"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "turn completion", func() bool { return !s.Running() })
	want := len(s.History())

	// A second manager over the same root lazily replays the journal.
	fresh, err := OpenManager(root, ManagerOptions{Agent: func() (TurnRunner, error) {
		return completedTurnScript(t, &TurnRequest{}), nil
	}})
	if err != nil {
		t.Fatal(err)
	}
	got, err := fresh.Get(id)
	if err != nil {
		t.Fatalf("fresh Get: %v", err)
	}
	events := got.History()
	if len(events) != want {
		t.Fatalf("replayed %d events, want %d", len(events), want)
	}
	if events[0].Type != EventUserMessage || events[0].Text != "跨进程恢复" {
		t.Fatalf("first replayed event = (%s, %q)", events[0].Type, events[0].Text)
	}
	if last := events[len(events)-1]; last.Type != EventTurnCompleted {
		t.Fatalf("last replayed event = %s, want turn_completed", last.Type)
	}
	// The replayed session can continue where it left off.
	if err := got.Send(context.Background(), "继续问"); err != nil {
		t.Fatalf("Send after replay: %v", err)
	}
	waitFor(t, "continuation turn", func() bool { return !got.Running() })
	seqs := map[int64]bool{}
	for _, ev := range got.History() {
		if seqs[ev.Seq] && ev.Seq != 0 {
			t.Fatalf("duplicate seq %d after continuation", ev.Seq)
		}
		seqs[ev.Seq] = true
	}
}

func TestManagerDeleteRules(t *testing.T) {
	release := make(chan struct{})
	mgr, err := OpenManager(t.TempDir(), ManagerOptions{Agent: func() (TurnRunner, error) {
		return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
			<-release
			return journal(Event{ID: newID(), Type: EventTurnCompleted, TurnID: req.TurnID, StopReason: "end_turn"})
		}), nil
	}})
	if err != nil {
		t.Fatal(err)
	}
	idle, err := mgr.Create("/tmp/idle")
	if err != nil {
		t.Fatal(err)
	}
	idleID := idle.Meta().ID

	// A running session refuses deletion.
	running, err := mgr.Create("/tmp/running")
	if err != nil {
		t.Fatal(err)
	}
	runningID := running.Meta().ID
	if err := running.Send(context.Background(), "运行中"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "turn start", func() bool { return running.Running() })
	if err := mgr.Delete(runningID); !errors.Is(err, ErrBusy) {
		t.Fatalf("delete running err = %v, want ErrBusy", err)
	}
	close(release)
	waitFor(t, "turn completion", func() bool { return !running.Running() })

	// An idle session deletes cleanly; the stale handle is inert.
	if err := mgr.Delete(idleID); err != nil {
		t.Fatalf("delete idle: %v", err)
	}
	if _, err := os.Stat(sessionDir(mgr.root, idleID)); !os.IsNotExist(err) {
		t.Fatalf("session dir still present: %v", err)
	}
	if _, err := mgr.Get(idleID); err == nil {
		t.Fatal("Get after delete must fail")
	}
	if err := mgr.Delete(idleID); err != nil {
		t.Fatalf("delete of an already deleted session must be idempotent: %v", err)
	}
	if err := idle.Send(context.Background(), "删除后再发"); err == nil {
		t.Fatal("send on deleted session must fail")
	}
	// Let the idle turn goroutine bookkeeping settle before temp cleanup.
	waitFor(t, "settle", func() bool { return !running.Running() })
}

func TestManagerCorruptJournalFailsClosedButListSurvives(t *testing.T) {
	root := t.TempDir()
	mgr, err := OpenManager(root, ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	s, err := mgr.Create("/tmp/x")
	if err != nil {
		t.Fatal(err)
	}
	id := s.Meta().ID
	// A complete (newline-terminated) invalid JSON line is corruption, not a
	// truncated tail.
	corrupt := "{\"schema\":\"keencode/session-event\",\"version\":1,\"eventI\n"
	if err := os.WriteFile(filepath.Join(sessionDir(root, id), journalFileName), []byte(corrupt), 0o600); err != nil {
		t.Fatal(err)
	}
	fresh, err := OpenManager(root, ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := fresh.Get(id); err == nil {
		t.Fatal("Get on corrupt journal must fail")
	} else {
		var corruptErr *JournalCorruptError
		if !errors.As(err, &corruptErr) {
			t.Fatalf("err = %v, want *JournalCorruptError", err)
		}
	}
	// List reads meta.json only, so the sidebar still shows the session;
	// the journal corruption surfaces when the session is opened.
	metas, err := fresh.List()
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(metas) != 1 || metas[0].ID != id {
		t.Fatalf("List = %v, want the session entry", metas)
	}
}

func TestManagerDrafts(t *testing.T) {
	mgr, err := OpenManager(t.TempDir(), ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	// New-session draft round trip.
	if _, _, ok, err := mgr.NewDraft(); err != nil || ok {
		t.Fatalf("NewDraft on empty root: ok=%v err=%v", ok, err)
	}
	if err := mgr.SaveNewDraft("/tmp/pick", "还没发送的想法"); err != nil {
		t.Fatalf("SaveNewDraft: %v", err)
	}
	dir, text, ok, err := mgr.NewDraft()
	if err != nil || !ok || dir != "/tmp/pick" || text != "还没发送的想法" {
		t.Fatalf("NewDraft = (%q, %q, %v, %v)", dir, text, ok, err)
	}
	// Removing both fields deletes the file.
	if err := mgr.SaveNewDraft("", ""); err != nil {
		t.Fatalf("SaveNewDraft clear: %v", err)
	}
	if _, _, ok, _ := mgr.NewDraft(); ok {
		t.Fatal("cleared new draft must be gone")
	}

	// Session draft round trip.
	s, err := mgr.Create("/tmp/session")
	if err != nil {
		t.Fatal(err)
	}
	id := s.Meta().ID
	if _, ok, err := mgr.SessionDraft(id); err != nil || ok {
		t.Fatalf("SessionDraft on fresh session: ok=%v err=%v", ok, err)
	}
	if err := mgr.SaveSessionDraft(id, "草稿内容"); err != nil {
		t.Fatalf("SaveSessionDraft: %v", err)
	}
	if text, ok, err := mgr.SessionDraft(id); err != nil || !ok || text != "草稿内容" {
		t.Fatalf("SessionDraft = (%q, %v, %v)", text, ok, err)
	}
	if err := mgr.SaveSessionDraft(id, ""); err != nil {
		t.Fatalf("SaveSessionDraft clear: %v", err)
	}
	if _, ok, _ := mgr.SessionDraft(id); ok {
		t.Fatal("cleared session draft must be gone")
	}
	if err := mgr.SaveSessionDraft("NOTANID", "x"); err == nil {
		t.Fatal("invalid id draft save must fail")
	}
}

func TestManagerCreateValidation(t *testing.T) {
	mgr, err := OpenManager(t.TempDir(), ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := mgr.Create("   "); err == nil {
		t.Fatal("empty project dir must fail")
	}
	if _, err := OpenManager("  ", ManagerOptions{}); err == nil {
		t.Fatal("empty root must fail")
	}
}

func TestTruncatedTitle(t *testing.T) {
	cases := []struct {
		in   string
		want string
	}{
		{"  你好，世界  ", "你好，世界"},
		{"", ""},
		{"   \n\t ", ""},
		{strings.Repeat("汉", 50), strings.Repeat("汉", 40)},
		{" ascii trim ", "ascii trim"},
	}
	for _, tc := range cases {
		if got := truncatedTitle(tc.in); got != tc.want {
			t.Fatalf("truncatedTitle(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}

func TestNewIDShape(t *testing.T) {
	seen := make(map[string]bool, 100)
	for i := 0; i < 100; i++ {
		id := newID()
		if !validID(id) {
			t.Fatalf("id %q is not a valid Crockford string", id)
		}
		if seen[id] {
			t.Fatalf("duplicate id %q", id)
		}
		seen[id] = true
	}
	if validID("not-26-characters-long") {
		t.Fatal("invalid shape accepted")
	}
	if validID("0000000000000000000000000I") {
		t.Fatal("Crockford-excluded letter I accepted")
	}
}
