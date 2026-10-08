package runtime

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"keencode/internal/model"
)

// funcRunner adapts a function to TurnRunner.
type funcRunner func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error

// RunTurn implements TurnRunner.
func (f funcRunner) RunTurn(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
	return f(ctx, req, journal, emit)
}

// waitFor polls cond until it holds or the deadline passes.
func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(2 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

// sessionFixture creates a manager over a temp root with the given factory.
func sessionFixture(t *testing.T, factory AgentFactory, modelFn ModelFunc) (*Manager, *Session) {
	t.Helper()
	mgr, err := OpenManager(t.TempDir(), ManagerOptions{Agent: factory, Model: modelFn})
	if err != nil {
		t.Fatalf("OpenManager: %v", err)
	}
	s, err := mgr.Create("/tmp/project")
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	return mgr, s
}

// completedTurnScript emits two text deltas then a terminal event.
func completedTurnScript(t *testing.T, capture *TurnRequest) funcRunner {
	return func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
		*capture = req
		for _, text := range []string{"第一", "第二"} {
			// The same event goes to journal and emit; the emit leg is an
			// idempotent no-op against the runtime.
			ev := Event{ID: newID(), Type: EventTextDelta, TurnID: req.TurnID, Text: text}
			if err := journal(ev); err != nil {
				return err
			}
			if err := emit(ev); err != nil {
				return err
			}
		}
		return journal(Event{ID: newID(), Type: EventTurnCompleted, TurnID: req.TurnID, StopReason: "end_turn"})
	}
}

func TestSessionSendRunsTurnAndDeliversEvents(t *testing.T) {
	var captured TurnRequest
	var calls int
	_, s := sessionFixture(t, func() (TurnRunner, error) {
		calls++
		return completedTurnScript(t, &captured), nil
	}, func() string { return "test-model" })

	ch, cancel := s.Subscribe()
	defer cancel()

	if err := s.Send(context.Background(), "  你好世界  "); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "turn completion", func() bool { return !s.Running() })

	events := collectEvents(t, ch, 4)
	want := []struct {
		typ  EventType
		text string
	}{
		{EventUserMessage, "你好世界"},
		{EventTextDelta, "第一"},
		{EventTextDelta, "第二"},
		{EventTurnCompleted, ""},
	}
	for i, w := range want {
		if events[i].Type != w.typ || events[i].Text != w.text {
			t.Fatalf("event %d = (%s, %q), want (%s, %q)", i, events[i].Type, events[i].Text, w.typ, w.text)
		}
		if events[i].Seq != int64(i+1) {
			t.Fatalf("event %d seq = %d, want %d", i, events[i].Seq, i+1)
		}
		if events[i].Replay {
			t.Fatalf("event %d must be live, not replay", i)
		}
	}

	// TurnRequest assembly per docs/go-migration.md §5.4.
	if captured.SessionID != s.Meta().ID || captured.TurnID == "" {
		t.Fatalf("request ids = %q/%q", captured.SessionID, captured.TurnID)
	}
	if captured.Model != "test-model" {
		t.Fatalf("model = %q, want test-model", captured.Model)
	}
	if captured.WorkDir != "/tmp/project" {
		t.Fatalf("workDir = %q", captured.WorkDir)
	}
	if len(captured.History) != 1 || captured.History[0].Role != model.RoleUser {
		t.Fatalf("history = %+v, want a single user message", captured.History)
	}

	// Title auto-derivation from the first user message (40 runes rule).
	if got := s.Meta().Title; got != "你好世界" {
		t.Fatalf("title = %q, want 你好世界", got)
	}
	if calls != 1 {
		t.Fatalf("factory calls = %d, want 1", calls)
	}
}

func TestSessionSendValidationAndBusy(t *testing.T) {
	release := make(chan struct{})
	_, s := sessionFixture(t, func() (TurnRunner, error) {
		return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
			<-release
			return journal(Event{ID: newID(), Type: EventTurnCompleted, TurnID: req.TurnID, StopReason: "end_turn"})
		}), nil
	}, nil)

	if err := s.Send(context.Background(), "   "); err == nil {
		t.Fatal("whitespace-only send must fail")
	}
	if err := s.Send(context.Background(), "第一问"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "turn start", func() bool { return s.Running() })
	if err := s.Send(context.Background(), "第二问"); !errors.Is(err, ErrBusy) {
		t.Fatalf("second send err = %v, want ErrBusy", err)
	}
	close(release)
	waitFor(t, "turn completion", func() bool { return !s.Running() })
}

func TestSessionStopCancelsTurn(t *testing.T) {
	_, s := sessionFixture(t, func() (TurnRunner, error) {
		return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
			<-ctx.Done() // the runner honors cancellation
			return journal(Event{ID: newID(), Type: EventTurnCancelled, TurnID: req.TurnID, Text: "已停止"})
		}), nil
	}, nil)
	if err := s.Send(context.Background(), "长任务"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	s.Stop() // idempotent: call twice
	s.Stop()
	waitFor(t, "turn completion", func() bool { return !s.Running() })
	events := s.History()
	last := events[len(events)-1]
	if last.Type != EventTurnCancelled {
		t.Fatalf("last event = %s, want turn_cancelled", last.Type)
	}
}

func TestSessionGuaranteesTerminalEvent(t *testing.T) {
	t.Run("runner returns error without terminal", func(t *testing.T) {
		_, s := sessionFixture(t, func() (TurnRunner, error) {
			return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
				return errors.New("上游断开")
			}), nil
		}, nil)
		if err := s.Send(context.Background(), "问"); err != nil {
			t.Fatalf("Send: %v", err)
		}
		waitFor(t, "turn completion", func() bool { return !s.Running() })
		last := s.History()[1]
		if last.Type != EventTurnFailed || !strings.Contains(last.Text, "上游断开") {
			t.Fatalf("terminal = (%s, %q), want turn_failed with the cause", last.Type, last.Text)
		}
	})
	t.Run("runner panics", func(t *testing.T) {
		_, s := sessionFixture(t, func() (TurnRunner, error) {
			return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
				panic("boom")
			}), nil
		}, nil)
		if err := s.Send(context.Background(), "问"); err != nil {
			t.Fatalf("Send: %v", err)
		}
		waitFor(t, "turn completion", func() bool { return !s.Running() })
		last := s.History()[1]
		if last.Type != EventTurnFailed || !strings.Contains(last.Text, "boom") {
			t.Fatalf("terminal = (%s, %q), want turn_failed with panic", last.Type, last.Text)
		}
		if s.Running() {
			t.Fatal("session still running after panic")
		}
	})
	t.Run("runner forgets terminal", func(t *testing.T) {
		_, s := sessionFixture(t, func() (TurnRunner, error) {
			return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
				return nil
			}), nil
		}, nil)
		if err := s.Send(context.Background(), "问"); err != nil {
			t.Fatalf("Send: %v", err)
		}
		waitFor(t, "turn completion", func() bool { return !s.Running() })
		if last := s.History()[1]; last.Type != EventTurnFailed {
			t.Fatalf("terminal = %s, want synthesized turn_failed", last.Type)
		}
	})
}

func TestSessionJournalFailureAbortsTurn(t *testing.T) {
	_, s := sessionFixture(t, func() (TurnRunner, error) {
		return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
			// First commit of the id succeeds, the retry with different
			// content conflicts and must fail the turn.
			if err := journal(Event{ID: "dup-1", Type: EventTextDelta, Text: "一次"}); err != nil {
				return err
			}
			return journal(Event{ID: "dup-1", Type: EventTextDelta, Text: "两次"})
		}), nil
	}, nil)
	if err := s.Send(context.Background(), "问"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "turn completion", func() bool { return !s.Running() })
	// The conflict fails the turn via the journal callback and the runtime
	// synthesizes turn_failed; the conflicting record stays unpersisted.
	history := s.History()
	found := 0
	for _, ev := range history {
		if ev.ID == "dup-1" {
			found++
		}
	}
	if found != 1 {
		t.Fatalf("dup-1 persisted %d times, want exactly 1", found)
	}
	if last := history[len(history)-1]; last.Type != EventTurnFailed {
		t.Fatalf("terminal = %s, want turn_failed after conflict", last.Type)
	}
}

func TestSessionSubscribeReplaysHistory(t *testing.T) {
	_, s := sessionFixture(t, func() (TurnRunner, error) {
		return completedTurnScript(t, &TurnRequest{}), nil
	}, nil)
	if err := s.Send(context.Background(), "第一问"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "turn completion", func() bool { return !s.Running() })

	ch, cancel := s.Subscribe()
	defer cancel()
	events := collectEvents(t, ch, 4)
	for i, ev := range events {
		if !ev.Replay {
			t.Fatalf("event %d (%s) must be flagged Replay", i, ev.Type)
		}
		if ev.Seq != int64(i+1) {
			t.Fatalf("event %d seq = %d", i, ev.Seq)
		}
	}
}

// TestSessionRecoveryProjectsInterruptedTurn covers the restart path: an
// unterminated turn in the journal gets an in-memory turn_failed projection
// ("应用中断") and the journal file itself stays untouched
// (docs/go-migration.md §5.5).
func TestSessionRecoveryProjectsInterruptedTurn(t *testing.T) {
	root := t.TempDir()
	mgr, err := OpenManager(root, ManagerOptions{})
	if err != nil {
		t.Fatalf("OpenManager: %v", err)
	}
	s, err := mgr.Create("/tmp/project")
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	id := s.Meta().ID
	// Append events of a turn that never terminates, then release the
	// journal without a terminal event.
	jrn := s.jrn
	if _, _, err := jrn.append(Event{ID: newID(), Type: EventUserMessage, Text: "断电前"}, 0); err != nil {
		t.Fatal(err)
	}
	if _, _, err := jrn.append(Event{ID: newID(), Type: EventTextDelta, Text: "半个回答", TurnID: "turn-dead"}, 1); err != nil {
		t.Fatal(err)
	}
	_ = jrn.close()
	mgr.mu.Lock()
	delete(mgr.sessions, id)
	mgr.mu.Unlock()

	reopened, err := OpenManager(root, ManagerOptions{})
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	s2, err := reopened.Get(id)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	history := s2.History()
	if len(history) != 3 {
		t.Fatalf("history = %d events, want 3 (journal + projection)", len(history))
	}
	last := history[2]
	if last.Type != EventTurnFailed || last.Text != "应用中断" || last.TurnID != "turn-dead" {
		t.Fatalf("projection = (%s, %q, %s), want turn_failed/应用中断/turn-dead", last.Type, last.Text, last.TurnID)
	}
	if last.Seq != 0 {
		t.Fatalf("projection seq = %d, want 0 (must not consume journal sequences)", last.Seq)
	}
	// The journal on disk is untouched.
	data, err := os.ReadFile(filepath.Join(sessionDir(root, id), journalFileName))
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(data), "应用中断") {
		t.Fatal("projection leaked into the journal file")
	}
	// A subsequent send builds history that closes the dangling turn buffer
	// instead of poisoning the request.
	reopenedWithAgent, err := OpenManager(root, ManagerOptions{Agent: func() (TurnRunner, error) { return noopRunner(), nil }})
	if err != nil {
		t.Fatal(err)
	}
	s2, err = reopenedWithAgent.Get(id)
	if err != nil {
		t.Fatalf("Get with agent: %v", err)
	}
	if err := s2.Send(context.Background(), "新的一问"); err != nil {
		t.Fatalf("Send after recovery: %v", err)
	}
	waitFor(t, "second turn completion", func() bool { return !s2.Running() })
}

func TestSessionHistoryAccumulatesAcrossTurns(t *testing.T) {
	turn := 0
	var seen []int
	_, s := sessionFixture(t, func() (TurnRunner, error) {
		return funcRunner(func(ctx context.Context, req TurnRequest, journal func(Event) error, emit func(Event) error) error {
			seen = append(seen, len(req.History))
			if err := journal(Event{ID: newID(), Type: EventTextDelta, TurnID: req.TurnID, Text: "答"}); err != nil {
				return err
			}
			turn++
			return journal(Event{ID: newID(), Type: EventTurnCompleted, TurnID: req.TurnID, StopReason: "end_turn"})
		}), nil
	}, nil)
	if err := s.Send(context.Background(), "第一问"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "first turn", func() bool { return !s.Running() })
	if err := s.Send(context.Background(), "第二问"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "second turn", func() bool { return !s.Running() })
	// First turn: just the user message. Second turn: user, assistant
	// (text), user (second question).
	if len(seen) != 2 || seen[0] != 1 || seen[1] != 3 {
		t.Fatalf("history sizes = %v, want [1 3]", seen)
	}
}

// An idle session is safe to start again or close: its preceding turn has
// already reached a terminal journal state, including synthesized errors.
func TestSessionIdleIncludesTerminalBeforeNextSend(t *testing.T) {
	_, s := sessionFixture(t, func() (TurnRunner, error) {
		return funcRunner(func(context.Context, TurnRequest, func(Event) error, func(Event) error) error {
			return errors.New("failed turn")
		}), nil
	}, nil)
	for i := 0; i < 30; i++ {
		if err := s.Send(context.Background(), "next turn"); err != nil {
			t.Fatal(err)
		}
		waitFor(t, "idle", func() bool { return !s.Running() })
		history := s.History()
		if len(history) != (i+1)*2 || history[len(history)-1].Type != EventTurnFailed {
			t.Fatalf("idle before terminal persistence: turn=%d history=%v", i, history)
		}
	}
}
