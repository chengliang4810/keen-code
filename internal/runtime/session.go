package runtime

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"
)

// Session is one loaded conversation: journal persistence, event hub, and
// per-session turn scheduling. All state transitions take s.mu, which is
// also the ordering lock of the seamless replay contract: journal appends
// and hub dispatch happen under it, so a subscriber either sees an event in
// its history preload or receives it live, never both and never neither
// (docs/go-migration.md §5.5).
type Session struct {
	root string

	mu           sync.Mutex
	meta         SessionMeta
	jrn          *journal
	hub          *hub
	agentFactory AgentFactory
	modelFn      ModelFunc
	running      bool
	turnCancel   context.CancelFunc
	turnID       string
	deleted      bool
	lastSeq      int64
	// synthetic carries in-memory turn failure projections for turns that
	// were open when the process died; they never enter the journal
	// (docs/go-migration.md §5.5: 重放遇未闭合 Turn 内存补投影).
	synthetic []Event
}

// newSession assembles a session around an already replayed journal.
func newSession(root string, meta SessionMeta, jrn *journal, factory AgentFactory, modelFn ModelFunc) *Session {
	return &Session{
		root:         root,
		meta:         meta,
		jrn:          jrn,
		hub:          newHub(),
		agentFactory: factory,
		modelFn:      modelFn,
		lastSeq:      jrn.lastSequence(),
		synthetic:    syntheticRecoveryEvents(jrn.history()),
	}
}

// Meta returns a copy of the session metadata.
func (s *Session) Meta() SessionMeta {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.meta
}

// History returns every event of the session in journal order, including
// in-memory recovery projections for turns that never reached a terminal
// state before the last shutdown (their Seq is 0).
func (s *Session) History() []Event {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.historyLocked()
}

// historyLocked returns history with the synthetic tail. Called with s.mu
// held.
func (s *Session) historyLocked() []Event {
	events := s.jrn.history()
	events = append(events, cloneAll(s.synthetic)...)
	return events
}

// Running reports whether a turn goroutine is active.
func (s *Session) Running() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.running
}

// Subscribe returns the live event channel and an unsubscribe func. The
// channel first delivers the full history with Replay=true, then live
// events; the whole stream is ordered by Seq and gapless. Slow consumers
// never block dispatch — their events queue up unboundedly
// (docs/go-migration.md §5.5).
func (s *Session) Subscribe() (<-chan Event, func()) {
	s.mu.Lock()
	if s.deleted {
		s.mu.Unlock()
		ch := make(chan Event)
		close(ch)
		return ch, func() {}
	}
	sub := s.hub.subscribe(s.historyLocked())
	s.mu.Unlock()
	return sub.Channel(), func() { s.hub.cancel(sub) }
}

// Send appends the user message and starts one turn goroutine. It returns
// ErrBusy when a turn is already running; the running turn keeps its own
// context, and the passed ctx governs the new turn's lifetime.
func (s *Session) Send(ctx context.Context, text string) error {
	trimmed := strings.TrimSpace(text)
	if trimmed == "" {
		return errors.New("消息内容不能为空")
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.deleted {
		return errors.New("会话已删除")
	}
	if s.running {
		return ErrBusy
	}
	if s.agentFactory == nil {
		return ErrNoAgent
	}
	// Assemble the runner before anything is journaled: a misconfigured
	// runtime must not strand a user message without a turn.
	runner, err := s.agentFactory()
	if err != nil {
		return fmt.Errorf("装配 agent 失败: %w", err)
	}

	userEvent := Event{ID: newID(), Type: EventUserMessage, Text: trimmed, Time: time.Now()}
	if _, err := s.recordLocked(userEvent); err != nil {
		return fmt.Errorf("落盘用户消息: %w", err)
	}
	if s.meta.Title == "" {
		s.meta.Title = truncatedTitle(trimmed)
		s.syncMetaLocked()
	}

	turnID := newID()
	modelID := ""
	if s.modelFn != nil {
		modelID = s.modelFn()
	}
	req := TurnRequest{
		SessionID: s.meta.ID,
		TurnID:    turnID,
		Model:     modelID,
		History:   eventsToHistory(s.historyLocked()),
		WorkDir:   s.meta.ProjectDir,
	}
	turnCtx, cancel := context.WithCancel(ctx)
	s.running = true
	s.turnCancel = cancel
	s.turnID = turnID
	go s.runTurn(turnCtx, runner, req)
	return nil
}

// Stop cancels the running turn (idempotent, no-op when idle). The turn
// goroutine winds down asynchronously; Running() turns false once it
// returned and the terminal event is journalled.
func (s *Session) Stop() {
	s.mu.Lock()
	cancel := s.turnCancel
	s.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

// runTurn drives one TurnRunner to a terminal state, guaranteeing the
// journal ends up with exactly one terminal event for the turn even when
// the runner violates its contract or panics.
func (s *Session) runTurn(ctx context.Context, runner TurnRunner, req TurnRequest) {
	var runnerErr error
	func() {
		defer func() {
			if r := recover(); r != nil {
				runnerErr = fmt.Errorf("agent 内部错误: %v", r)
			}
		}()
		runnerErr = runner.RunTurn(ctx, req, s.journalAppend, s.journalAppend)
	}()

	s.mu.Lock()
	defer s.mu.Unlock()
	// Keep Running true until the terminal event is recorded. Otherwise
	// callers can start another turn or tear down the journal too early.
	turnID := s.turnID
	terminal := false
	for _, ev := range s.historyLocked() {
		if ev.TurnID == turnID && ev.Type.isTerminalTurnEvent() {
			terminal = true
			break
		}
	}
	switch {
	case terminal:
	case runnerErr != nil:
		_, _ = s.recordLocked(Event{ID: newID(), Type: EventTurnFailed, TurnID: turnID, Text: runnerErr.Error(), Time: time.Now()})
	case ctx.Err() != nil:
		_, _ = s.recordLocked(Event{ID: newID(), Type: EventTurnCancelled, TurnID: turnID, Text: "已停止", Time: time.Now()})
	default:
		_, _ = s.recordLocked(Event{ID: newID(), Type: EventTurnFailed, TurnID: turnID, Text: "回合未产生终态事件", Time: time.Now()})
	}
	s.running = false
	if s.turnCancel != nil {
		s.turnCancel()
	}
	s.turnCancel = nil
	s.turnID = ""
}

// journalAppend is both halves of the TurnRunner contract: append to the
// journal first, then deliver. It is passed as the journal and the emit
// callback — the second call is an idempotent no-op (AlreadyCommitted),
// while an emit with a reused ID but different content fails the turn, so
// the append-then-emit contract of the planned agent signature is enforced
// even against contract-violating runners.
func (s *Session) journalAppend(ev Event) error {
	if ev.ID == "" {
		ev.ID = newID()
	}
	_, err := s.record(ev)
	return err
}

// record appends and publishes under the session lock.
func (s *Session) record(ev Event) (Event, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.recordLocked(ev)
}

// recordLocked appends the event to the journal and, on a fresh commit,
// publishes it to subscribers. Called with s.mu held.
func (s *Session) recordLocked(ev Event) (Event, error) {
	if ev.Time.IsZero() {
		ev.Time = time.Now()
	}
	if ev.TurnID == "" && s.turnID != "" {
		ev.TurnID = s.turnID
	}
	stored, outcome, err := s.jrn.append(ev, s.lastSeq)
	if err != nil {
		return Event{}, err
	}
	if outcome == AppendAlreadyCommitted {
		// Idempotent retry: the event is already in the history (replayed
		// or published earlier); delivering it again would duplicate.
		return stored, nil
	}
	s.lastSeq = stored.Seq
	s.hub.publish(stored)
	if stored.Type.syncImmediately() {
		// meta.json only feeds the sidebar; the journal stays the source of
		// truth, so a failed metadata write is retried on the next anchor
		// event rather than failing the turn.
		s.meta.UpdatedAt = stored.Time
		s.syncMetaLocked()
	}
	return stored, nil
}

// syncMetaLocked persists metadata; errors are intentionally swallowed (see
// recordLocked). Called with s.mu held.
func (s *Session) syncMetaLocked() {
	_ = writeMeta(sessionFilePath(s.root, s.meta.ID, metaFileName), s.meta)
}

// rename changes the display title (user action; overrides the
// auto-derived one). Callers validate non-emptiness.
func (s *Session) rename(title string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.deleted {
		return errors.New("会话已删除")
	}
	s.meta.Title = title
	return writeMeta(sessionFilePath(s.root, s.meta.ID, metaFileName), s.meta)
}

func (s *Session) setPinned(pinned bool) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.deleted {
		return errors.New("会话已删除")
	}
	meta := s.meta
	meta.Pinned = pinned
	if err := writeMeta(sessionFilePath(s.root, meta.ID, metaFileName), meta); err != nil {
		return err
	}
	s.meta = meta
	return nil
}

// currentMeta returns the live metadata (manager list path).
func (s *Session) currentMeta() SessionMeta {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.meta
}

// close releases journal resources and detaches subscribers. The session
// must not be used afterwards.
func (s *Session) close() {
	s.mu.Lock()
	s.deleted = true
	hub := s.hub
	jrn := s.jrn
	s.mu.Unlock()
	hub.close()
	_ = jrn.close()
}

// syntheticRecoveryEvents projects an in-memory turn failure for every turn
// that has events but no terminal event — the "应用中断" marker of an
// interrupted application (docs/go-migration.md §5.5). Seq stays 0: these
// events never consume journal sequences.
func syntheticRecoveryEvents(events []Event) []Event {
	var order []string
	seen := make(map[string]bool)
	terminal := make(map[string]bool)
	for _, ev := range events {
		if ev.TurnID == "" {
			continue
		}
		if !seen[ev.TurnID] {
			seen[ev.TurnID] = true
			order = append(order, ev.TurnID)
		}
		if ev.Type.isTerminalTurnEvent() {
			terminal[ev.TurnID] = true
		}
	}
	var out []Event
	for _, turnID := range order {
		if terminal[turnID] {
			continue
		}
		out = append(out, Event{
			ID:     newID(),
			Type:   EventTurnFailed,
			TurnID: turnID,
			Text:   "应用中断",
			Time:   time.Now(),
		})
	}
	return out
}
