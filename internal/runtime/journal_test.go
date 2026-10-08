package runtime

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"keencode/internal/model"
)

// journalFixture builds a journal in a fresh temporary session directory.
func journalFixture(t *testing.T) (*journal, string) {
	t.Helper()
	root := t.TempDir()
	dir := filepath.Join(root, sessionsDirName, "testsession0000000000000000")
	j, err := openJournal("testsession0000000000000000", dir)
	if err != nil {
		t.Fatalf("openJournal: %v", err)
	}
	t.Cleanup(func() { _ = j.close() })
	return j, dir
}

// mustAppend appends an event or fails the test.
func mustAppend(t *testing.T, j *journal, ev Event) Event {
	t.Helper()
	stored, outcome, err := j.append(ev, j.lastSequence())
	if err != nil {
		t.Fatalf("append %s: %v", ev.Type, err)
	}
	if outcome != AppendAppended {
		t.Fatalf("append %s: outcome = %v, want AppendAppended", ev.Type, outcome)
	}
	return stored
}

// userEvent returns a user message event with a unique id.
func userEvent(text string) Event {
	return Event{ID: newID(), Type: EventUserMessage, Text: text}
}

func TestJournalAppendAssignsSequenceAndTime(t *testing.T) {
	j, dir := journalFixture(t)
	first := mustAppend(t, j, userEvent("你好"))
	second := mustAppend(t, j, Event{ID: newID(), Type: EventTextDelta, Text: "世"})

	if first.Seq != 1 || second.Seq != 2 {
		t.Fatalf("sequences = %d, %d; want 1, 2", first.Seq, second.Seq)
	}
	if !second.Time.After(first.Time) && !second.Time.Equal(first.Time) {
		t.Fatalf("time went backwards: %v -> %v", first.Time, second.Time)
	}
	if first.SessionID != j.sessionID {
		t.Fatalf("SessionID = %q, want %q", first.SessionID, j.sessionID)
	}
	// Envelope shape on disk: one line per record with the fixed schema.
	data, err := os.ReadFile(filepath.Join(dir, journalFileName))
	if err != nil {
		t.Fatalf("read journal: %v", err)
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	if len(lines) != 2 {
		t.Fatalf("journal has %d lines, want 2", len(lines))
	}
	var record journalRecord
	if err := json.Unmarshal([]byte(lines[0]), &record); err != nil {
		t.Fatalf("decode record: %v", err)
	}
	if record.Schema != journalRecordSchema || record.Version != journalRecordVersion {
		t.Fatalf("envelope = %s/%d, want %s/%d", record.Schema, record.Version, journalRecordSchema, journalRecordVersion)
	}
	if record.EventID != first.ID || record.Sequence != 1 || record.Type != string(EventUserMessage) {
		t.Fatalf("record = %+v", record)
	}
	if record.Session != j.sessionID {
		t.Fatalf("record session = %q, want %q", record.Session, j.sessionID)
	}
}

func TestJournalReplayRoundTrip(t *testing.T) {
	j, dir := journalFixture(t)
	sent := []Event{
		mustAppend(t, j, userEvent("第一问")),
		mustAppend(t, j, Event{ID: newID(), Type: EventTextDelta, Text: "回", TurnID: "t1"}),
		mustAppend(t, j, Event{ID: newID(), Type: EventTextDelta, Text: "答", TurnID: "t1"}),
		mustAppend(t, j, Event{ID: newID(), Type: EventToolStart, TurnID: "t1", Tool: &ToolEvent{CallID: "c1", Name: "read_file"}}),
		mustAppend(t, j, Event{ID: newID(), Type: EventToolArgs, TurnID: "t1", Text: `{"path":`, Tool: &ToolEvent{CallID: "c1"}}),
		mustAppend(t, j, Event{ID: newID(), Type: EventToolArgs, TurnID: "t1", Text: `"a.txt"}`, Tool: &ToolEvent{CallID: "c1"}}),
		mustAppend(t, j, Event{ID: newID(), Type: EventToolEnd, TurnID: "t1", Tool: &ToolEvent{CallID: "c1"}}),
		mustAppend(t, j, Event{ID: newID(), Type: EventToolResult, TurnID: "t1", Text: "内容", Tool: &ToolEvent{CallID: "c1", Status: ToolStatusCompleted}}),
		mustAppend(t, j, Event{ID: newID(), Type: EventReasoningContinuation, TurnID: "t1", Text: "sig=="}),
		mustAppend(t, j, Event{ID: newID(), Type: EventUsage, TurnID: "t1", Usage: usageFixture(11, 7)}),
		mustAppend(t, j, Event{ID: newID(), Type: EventTurnCompleted, TurnID: "t1", StopReason: "end_turn"}),
	}
	if err := j.close(); err != nil {
		t.Fatalf("close: %v", err)
	}

	reopened, err := openJournal(j.sessionID, dir)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	t.Cleanup(func() { _ = reopened.close() })
	got := reopened.history()
	if len(got) != len(sent) {
		t.Fatalf("replayed %d events, want %d", len(got), len(sent))
	}
	for i, want := range sent {
		got[i].Replay = false
		if got[i].ID != want.ID || got[i].Type != want.Type || got[i].Text != want.Text || got[i].Seq != want.Seq || got[i].TurnID != want.TurnID {
			t.Fatalf("event %d = %+v, want %+v", i, got[i], want)
		}
		switch {
		case (got[i].Tool == nil) != (want.Tool == nil):
			t.Fatalf("event %d tool presence mismatch: %+v vs %+v", i, got[i], want)
		case got[i].Tool != nil && (*got[i].Tool != *want.Tool):
			t.Fatalf("event %d tool = %+v, want %+v", i, got[i].Tool, want.Tool)
		}
		if (got[i].Usage == nil) != (want.Usage == nil) {
			t.Fatalf("event %d usage presence mismatch", i)
		}
		if got[i].Usage != nil && *got[i].Usage != *want.Usage {
			t.Fatalf("event %d usage = %+v, want %+v", i, got[i].Usage, want.Usage)
		}
		if got[i].StopReason != want.StopReason {
			t.Fatalf("event %d stopReason = %q, want %q", i, got[i].StopReason, want.StopReason)
		}
	}
	if reopened.lastSequence() != int64(len(sent)) {
		t.Fatalf("lastSequence = %d, want %d", reopened.lastSequence(), len(sent))
	}
}

// usageFixture returns a reported token usage snapshot.
func usageFixture(in, out int64) *model.TokenUsage {
	return &model.TokenUsage{InputTokens: in, OutputTokens: out, CachedTokens: -1}
}

func TestJournalIdempotentAppend(t *testing.T) {
	j, dir := journalFixture(t)
	ev := userEvent("重试我")
	first := mustAppend(t, j, ev)

	// Same id, same content: AlreadyCommitted, no second record.
	retry, outcome, err := j.append(ev, j.lastSequence())
	if err != nil {
		t.Fatalf("idempotent append: %v", err)
	}
	if outcome != AppendAlreadyCommitted {
		t.Fatalf("outcome = %v, want AppendAlreadyCommitted", outcome)
	}
	if retry.Seq != first.Seq {
		t.Fatalf("retry seq = %d, want %d", retry.Seq, first.Seq)
	}

	// Same id, different content: rejected, nothing written.
	_, _, err = j.append(Event{ID: ev.ID, Type: EventUserMessage, Text: "不同内容"}, j.lastSequence())
	var conflict *EventIDConflictError
	if err == nil || !errors.As(err, &conflict) {
		t.Fatalf("conflict err = %v, want *EventIDConflictError", err)
	}

	// CAS: a stale expected sequence is rejected without writing.
	_, _, err = j.append(Event{ID: newID(), Type: EventUserMessage, Text: "乱序"}, 0)
	var cas *SequenceConflictError
	if err == nil || !errors.As(err, &cas) {
		t.Fatalf("cas err = %v, want *SequenceConflictError", err)
	}
	if cas.Expected != 0 || cas.Actual != 1 {
		t.Fatalf("cas = %d/%d, want 0/1", cas.Expected, cas.Actual)
	}

	if err := j.close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	reopened, err := openJournal(j.sessionID, dir)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	defer reopened.close()
	if got := reopened.lastSequence(); got != 1 {
		t.Fatalf("replayed sequence = %d, want 1 (retries and rejects must not persist)", got)
	}
	if got := len(reopened.history()); got != 1 {
		t.Fatalf("replayed %d events, want 1", got)
	}
}

func TestJournalConcurrentAppendCommitsOnce(t *testing.T) {
	j, _ := journalFixture(t)
	const workers = 8
	var wg sync.WaitGroup
	accepted := make([]bool, workers)
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func(w int) {
			defer wg.Done()
			ev := Event{ID: newID(), Type: EventTextDelta, Text: fmt.Sprintf("w%d", w)}
			for {
				_, _, err := j.append(ev, j.lastSequence())
				if err == nil {
					accepted[w] = true
					return
				}
				var cas *SequenceConflictError
				if errors.As(err, &cas) {
					continue // lost the race, retry with fresh watermark
				}
				t.Errorf("worker %d: %v", w, err)
				return
			}
		}(w)
	}
	wg.Wait()
	if j.lastSequence() != workers {
		t.Fatalf("lastSequence = %d, want %d", j.lastSequence(), workers)
	}
	committed := 0
	for _, ok := range accepted {
		if ok {
			committed++
		}
	}
	if committed != workers {
		t.Fatalf("committed workers = %d, want %d", committed, workers)
	}
	seqs := map[int64]bool{}
	for _, ev := range j.history() {
		if seqs[ev.Seq] {
			t.Fatalf("duplicate sequence %d in history", ev.Seq)
		}
		seqs[ev.Seq] = true
	}
}

// TestJournalCorruptionVariants ports the read-time classification of
// core/resources/src/journal.rs:2556-2620: every defect except a truncated
// tail fails closed with the defect class of the first bad line.
func TestJournalCorruptionVariants(t *testing.T) {
	validLine := func(seq int64, id string) string {
		payload, _ := json.Marshal(eventPayload{Text: "ok"})
		record, _ := json.Marshal(journalRecord{
			Schema:     journalRecordSchema,
			Version:    journalRecordVersion,
			EventID:    id,
			Session:    "testsession0000000000000000",
			Sequence:   seq,
			TimeUnixMs: 1700000000000,
			Type:       string(EventUserMessage),
			Payload:    payload,
		})
		return string(record)
	}
	cases := []struct {
		name     string
		lines    []string
		wantKind CorruptKind
		wantLine int
	}{
		{
			name:     "invalid json",
			lines:    []string{validLine(1, "a"), "{not json"},
			wantKind: CorruptInvalidJSON, wantLine: 2,
		},
		{
			name:     "empty interior line",
			lines:    []string{validLine(1, "a"), "", validLine(2, "b")},
			wantKind: CorruptInvalidJSON, wantLine: 2,
		},
		{
			name:     "envelope schema mismatch",
			lines:    []string{strings.Replace(validLine(1, "a"), journalRecordSchema, "other/schema", 1)},
			wantKind: CorruptEnvelopeMismatch, wantLine: 1,
		},
		{
			name:     "envelope future version",
			lines:    []string{strings.Replace(validLine(1, "a"), `"version":1`, `"version":99`, 1)},
			wantKind: CorruptEnvelopeMismatch, wantLine: 1,
		},
		{
			name:     "envelope session mismatch",
			lines:    []string{strings.Replace(validLine(1, "a"), "testsession0000000000000000", "otherxxxxx0000000000000000", 1)},
			wantKind: CorruptEnvelopeMismatch, wantLine: 1,
		},
		{
			name:     "sequence gap",
			lines:    []string{validLine(1, "a"), validLine(3, "c")},
			wantKind: CorruptSequenceGap, wantLine: 2,
		},
		{
			name:     "out of order sequence",
			lines:    []string{validLine(1, "a"), validLine(2, "b"), validLine(1, "c")},
			wantKind: CorruptOutOfOrderSequence, wantLine: 3,
		},
		{
			name:     "duplicate sequence",
			lines:    []string{validLine(1, "a"), validLine(1, "b")},
			wantKind: CorruptDuplicateSequence, wantLine: 2,
		},
		{
			name:     "duplicate event id on disk",
			lines:    []string{validLine(1, "a"), validLine(2, "a")},
			wantKind: CorruptDuplicateEventID, wantLine: 2,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			root := t.TempDir()
			dir := filepath.Join(root, sessionsDirName, "testsession0000000000000000")
			if err := os.MkdirAll(dir, 0o700); err != nil {
				t.Fatal(err)
			}
			content := strings.Join(tc.lines, "\n") + "\n"
			if err := os.WriteFile(filepath.Join(dir, journalFileName), []byte(content), 0o600); err != nil {
				t.Fatal(err)
			}
			_, err := openJournal("testsession0000000000000000", dir)
			var corrupt *JournalCorruptError
			if err == nil || !errors.As(err, &corrupt) {
				t.Fatalf("err = %v, want *JournalCorruptError", err)
			}
			if corrupt.Kind != tc.wantKind || corrupt.Line != tc.wantLine {
				t.Fatalf("corrupt = %s line %d, want %s line %d", corrupt.Kind, corrupt.Line, tc.wantKind, tc.wantLine)
			}
		})
	}
}

// TestJournalTruncatedTailRecovery ports the truncated-tail semantics: a
// partial final line is repaired on open, the tail bytes are preserved as
// evidence, and appends continue from the last complete sequence
// (core/resources/src/journal.rs:861-994).
func TestJournalTruncatedTailRecovery(t *testing.T) {
	j, dir := journalFixture(t)
	first := mustAppend(t, j, userEvent("完成的一行"))
	if err := j.close(); err != nil {
		t.Fatalf("close: %v", err)
	}

	logPath := filepath.Join(dir, journalFileName)
	// Simulate a crash mid-write: a torn JSON line in the page cache.
	partial := []byte(`{"schema":"keencode/session-event","version":1,"event`)
	f, err := os.OpenFile(logPath, os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.Write(partial); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := openJournal(j.sessionID, dir)
	if err != nil {
		t.Fatalf("reopen after truncation: %v", err)
	}
	defer reopened.close()
	if got := reopened.lastSequence(); got != 1 {
		t.Fatalf("recovered sequence = %d, want 1", got)
	}
	if got := len(reopened.history()); got != 1 || reopened.history()[0].ID != first.ID {
		t.Fatalf("recovered history = %d events, want the single original", got)
	}

	// Evidence preserved with the exact tail bytes.
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	evidenceFound := false
	for _, entry := range entries {
		if strings.HasPrefix(entry.Name(), truncatedTailEvidencePrefix) {
			evidenceFound = true
			data, err := os.ReadFile(filepath.Join(dir, entry.Name()))
			if err != nil {
				t.Fatal(err)
			}
			if string(data) != string(partial) {
				t.Fatalf("evidence = %q, want %q", data, partial)
			}
		}
	}
	if !evidenceFound {
		t.Fatalf("no %s* evidence file in %v", truncatedTailEvidencePrefix, entries)
	}

	// The repaired journal keeps appending with gapless sequences.
	second := mustAppend(t, reopened, userEvent("恢复后追加"))
	if second.Seq != 2 {
		t.Fatalf("post-recovery seq = %d, want 2", second.Seq)
	}
	if err := reopened.close(); err != nil {
		t.Fatal(err)
	}
	third, err := openJournal(j.sessionID, dir)
	if err != nil {
		t.Fatalf("reopen after recovery append: %v", err)
	}
	defer third.close()
	if got := third.lastSequence(); got != 2 {
		t.Fatalf("final sequence = %d, want 2", got)
	}
}

// TestJournalAppendLockIsCrossProcess validates co-existence with a real
// second process: while this process keeps the journal open, a child
// process appends under the same append.lock; the parent then observes the
// child's record after reopening. The child re-enters this test binary via
// an environment switch.
func TestJournalAppendLockIsCrossProcess(t *testing.T) {
	if os.Getenv("KEENCODE_RUNTIME_LOCK_CHILD") == "1" {
		// Child: open the journal (takes the flock during load) and append
		// one event, then exit. Success = no error.
		dir := os.Getenv("KEENCODE_RUNTIME_LOCK_DIR")
		j, err := openJournal("testsession0000000000000000", dir)
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		if _, _, err := j.append(userEvent("来自子进程"), j.lastSequence()); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		if err := j.close(); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		os.Exit(0)
	}
	j, dir := journalFixture(t)
	mustAppend(t, j, userEvent("父进程一行"))

	cmd := exec.Command(os.Args[0], "-test.run", "^TestJournalAppendLockIsCrossProcess$")
	cmd.Env = append(os.Environ(),
		"KEENCODE_RUNTIME_LOCK_CHILD=1",
		"KEENCODE_RUNTIME_LOCK_DIR="+dir,
	)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("child process: %v\n%s", err, out)
	}
	if err := j.close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	reopened, err := openJournal(j.sessionID, dir)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	defer reopened.close()
	got := reopened.history()
	if len(got) != 2 || got[1].Text != "来自子进程" {
		t.Fatalf("history = %+v, want parent + child events", got)
	}
}
