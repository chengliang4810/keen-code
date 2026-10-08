package runtime

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"sync"
	"time"

	"keencode/internal/model"
)

// Journal durability bounds. Values follow the Rust defaults
// (core/resources/src/journal.rs:135-146): a batched sync fires after 64
// records or 100ms, whichever comes first.
const (
	// journalBatchMaxRecords flushes a pending batch once it holds this many
	// records.
	journalBatchMaxRecords = 64
	// journalBatchMaxDelay flushes a pending batch this long after its first
	// record was written.
	journalBatchMaxDelay = 100 * time.Millisecond
	// journalFlusherIdleExit retires the background flusher after this much
	// idleness; the next batched append restarts it.
	journalFlusherIdleExit = 250 * time.Millisecond
	// journalMaxEventBytes bounds one serialized JSONL line.
	journalMaxEventBytes = 8 << 20
	// journalMaxLogBytes bounds the whole journal file.
	journalMaxLogBytes = 512 << 20
	// journalMaxRecords bounds the record count of one journal.
	journalMaxRecords = 10_000_000
)

// journalRecord is one JSONL line of journal.jsonl. The envelope mirrors
// SessionEventRecord (core/resources/src/types.rs:1074-1090) in the fresh Go
// namespace: fixed schema and version, idempotent event identity, owning
// session, gapless sequence, millisecond time, event type, and the typed
// payload.
type journalRecord struct {
	Schema     string          `json:"schema"`
	Version    int             `json:"version"`
	EventID    string          `json:"eventId"`
	Session    string          `json:"session"`
	Sequence   int64           `json:"sequence"`
	TimeUnixMs int64           `json:"timeUnixMs"`
	Type       string          `json:"type"`
	Payload    json.RawMessage `json:"payload"`
}

// eventPayload is the payload half of a journal record: everything the
// envelope does not already carry of a runtime Event.
type eventPayload struct {
	TurnID     string            `json:"turnId,omitempty"`
	Text       string            `json:"text,omitempty"`
	Tool       *ToolEvent        `json:"tool,omitempty"`
	Usage      *model.TokenUsage `json:"usage,omitempty"`
	StopReason model.StopReason  `json:"stopReason,omitempty"`
}

// CorruptKind classifies an unrecoverable journal defect
// (core/resources/src/journal.rs:2524-2620 CorruptionKind).
type CorruptKind string

// Journal corruption classifications. A truncated tail is repaired
// automatically with evidence; everything else fails closed.
const (
	CorruptInvalidJSON        CorruptKind = "invalid_json"
	CorruptEnvelopeMismatch   CorruptKind = "envelope_mismatch"
	CorruptDuplicateSequence  CorruptKind = "duplicate_sequence"
	CorruptOutOfOrderSequence CorruptKind = "out_of_order_sequence"
	CorruptSequenceGap        CorruptKind = "sequence_gap"
	CorruptDuplicateEventID   CorruptKind = "duplicate_event_id"
	CorruptEventTooLarge      CorruptKind = "event_too_large"
	CorruptPayloadInvalid     CorruptKind = "payload_invalid"
)

// JournalCorruptError reports a journal defect that is not a truncated
// tail. The session stays closed for appends until the file is repaired
// (fail closed, mirroring the Rust read-only corrupt report).
type JournalCorruptError struct {
	// SessionID owning the damaged journal.
	SessionID string
	// Kind classifies the first defect encountered.
	Kind CorruptKind
	// Line is the 1-based line number of the defect.
	Line int
	// Detail explains the defect.
	Detail string
}

// Error implements error.
func (e *JournalCorruptError) Error() string {
	return fmt.Sprintf("会话 %s 事件日志损坏（第 %d 行，%s）: %s", e.SessionID, e.Line, e.Kind, e.Detail)
}

// SequenceConflictError reports a compare-and-swap miss: the journal
// sequence moved past the expected watermark, nothing was written
// (core/resources/src/journal.rs:1290-1296).
type SequenceConflictError struct {
	Expected, Actual int64
}

// Error implements error.
func (e *SequenceConflictError) Error() string {
	return fmt.Sprintf("追加序号冲突：期望 %d，实际 %d", e.Expected, e.Actual)
}

// EventIDConflictError reports an event ID reuse with a different payload:
// nothing was written (core/resources/src/journal.rs:1279-1283).
type EventIDConflictError struct {
	EventID     string
	ExistingSeq int64
}

// Error implements error.
func (e *EventIDConflictError) Error() string {
	return fmt.Sprintf("事件标识 %s 已在序号 %d 以不同内容提交", e.EventID, e.ExistingSeq)
}

// AppendOutcome distinguishes the results of an idempotent append.
type AppendOutcome int

const (
	// AppendAppended reports a freshly persisted record.
	AppendAppended AppendOutcome = iota
	// AppendAlreadyCommitted reports the event ID was previously committed
	// with identical content; the journal is unchanged.
	AppendAlreadyCommitted
)

// journal is the append-only JSONL event log of one session: idempotent
// append under a sequence CAS, O_APPEND line writes, batched background
// fsync (64 records / 100ms), and startup replay with truncated-tail
// repair. It is a port of SessionJournal
// (core/resources/src/journal.rs) reduced to the Go v1 shape.
type journal struct {
	sessionID string
	dir       string
	logPath   string
	lock      *fileLock

	// mu guards every field below and all file operations.
	mu sync.Mutex
	// file is the O_APPEND write handle, opened lazily on first append.
	file *os.File
	// lastSeq is the highest persisted sequence (0 when empty).
	lastSeq int64
	// lastTimeMS keeps event times monotonic non-decreasing
	// (core/resources/src/journal.rs:1326-1327).
	lastTimeMS int64
	// index maps event ID to its sequence and payload digest.
	index map[string]journalIndexEntry
	// events is the replayed in-order history with sequence assigned.
	events []Event
	// logLen is the current byte size of the log file.
	logLen int64
	// pendingRecords counts records written but not yet synced.
	pendingRecords int
	// pendingSince is the write time of the first unsynced record.
	pendingSince time.Time
	// flusherRunning reports whether the background flusher goroutine is
	// alive.
	flusherRunning bool
	// flusherArmed reports whether a batch deadline is scheduled.
	flusherArmed bool
	// closed marks a closed journal; further appends fail.
	closed bool

	flushWake chan struct{}
	flushStop chan struct{}
}

// journalIndexEntry is the per-event-id dedupe index entry.
type journalIndexEntry struct {
	sequence int64
	timeMS   int64
	digest   string
}

// openJournal replays the journal of sessionID from dir, repairing a
// truncated tail with evidence, and returns a journal ready for appends.
// Open and append share one cross-process lock so a concurrent writer's
// half written line is never misread as corruption
// (core/resources/src/journal.rs:777-779).
func openJournal(sessionID, dir string) (*journal, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("创建会话目录: %w", err)
	}
	logPath := dir + string(os.PathSeparator) + journalFileName
	lock, err := openFileLock(dir + string(os.PathSeparator) + "append.lock")
	if err != nil {
		return nil, err
	}
	j := &journal{
		sessionID: sessionID,
		dir:       dir,
		logPath:   logPath,
		lock:      lock,
		index:     make(map[string]journalIndexEntry),
		flushWake: make(chan struct{}, 1),
		flushStop: make(chan struct{}),
	}
	if err := j.load(); err != nil {
		lock.close()
		return nil, err
	}
	return j, nil
}

// load replays the log under the append lock, classifying defects. Only a
// truncated tail is repaired (evidence file + truncate); every other defect
// fails closed with a JournalCorruptError.
func (j *journal) load() error {
	if err := j.lock.lock(); err != nil {
		return err
	}
	defer j.lock.unlock()

	data, err := os.ReadFile(j.logPath)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("读取事件日志: %w", err)
	}
	if len(data) > journalMaxLogBytes {
		return fmt.Errorf("事件日志 %d 字节超过上限 %d", len(data), journalMaxLogBytes)
	}

	// A missing trailing newline marks a truncated tail: the offset after
	// the last complete line starts the unpersisted tail
	// (core/resources/src/journal.rs:2510-2522).
	complete := len(data)
	if complete > 0 && data[complete-1] != '\n' {
		offset := 0
		if idx := bytes.LastIndexByte(data, '\n'); idx >= 0 {
			offset = idx + 1
		}
		if err := j.repairTruncatedTail(data[offset:], int64(offset)); err != nil {
			return err
		}
		data = data[:offset]
		complete = offset
	}

	var (
		previous int64
		line     = 1
		consumed int64
	)
	// Drop exactly the one allowed trailing newline; interior empty lines
	// remain and are reported as invalid JSON
	// (core/resources/src/journal.rs:2552-2555).
	body := data
	if complete > 0 {
		body = data[:complete-1]
	}
	for {
		var raw []byte
		if idx := bytes.IndexByte(body, '\n'); idx >= 0 {
			raw, body = body[:idx], body[idx+1:]
		} else if len(body) > 0 {
			raw, body = body, nil
		} else {
			break
		}
		if line > journalMaxRecords {
			return fmt.Errorf("事件日志记录数超过上限 %d", journalMaxRecords)
		}
		if len(raw)+1 > journalMaxEventBytes {
			return &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptEventTooLarge, Line: line,
				Detail: fmt.Sprintf("第 %d 行 %d 字节超过单条上限 %d", line, len(raw)+1, journalMaxEventBytes)}
		}
		event, entry, issue := j.decodeRecord(raw, previous, line)
		if issue != nil {
			return issue
		}
		previous = entry.sequence
		j.lastSeq = entry.sequence
		if entry.timeMS > j.lastTimeMS {
			j.lastTimeMS = entry.timeMS
		}
		j.index[event.ID] = *entry
		j.events = append(j.events, event)
		consumed += int64(len(raw) + 1)
		line++
	}
	j.logLen = consumed
	return nil
}

// decodeRecord parses and validates one JSONL line against the envelope
// contract and the sequence/event-id invariants
// (core/resources/src/journal.rs:2556-2620).
func (j *journal) decodeRecord(raw []byte, previous int64, line int) (Event, *journalIndexEntry, error) {
	if len(bytes.TrimSpace(raw)) == 0 {
		return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptInvalidJSON, Line: line,
			Detail: fmt.Sprintf("第 %d 行是空记录", line)}
	}
	var record journalRecord
	if err := json.Unmarshal(raw, &record); err != nil {
		return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptInvalidJSON, Line: line,
			Detail: fmt.Sprintf("第 %d 行不是有效事件 JSON: %v", line, err)}
	}
	if record.Schema != journalRecordSchema || record.Version > journalRecordVersion || record.Session != j.sessionID {
		return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptEnvelopeMismatch, Line: line,
			Detail: fmt.Sprintf("第 %d 行 envelope 不匹配（schema=%q version=%d session=%q）", line, record.Schema, record.Version, record.Session)}
	}
	if record.Sequence != previous+1 {
		switch {
		case previous != 0 && record.Sequence == previous:
			return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptDuplicateSequence, Line: line,
				Detail: fmt.Sprintf("第 %d 行重复序号 %d", line, record.Sequence)}
		case record.Sequence < previous+1:
			return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptOutOfOrderSequence, Line: line,
				Detail: fmt.Sprintf("第 %d 行序号 %d 小于期望 %d", line, record.Sequence, previous+1)}
		default:
			return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptSequenceGap, Line: line,
				Detail: fmt.Sprintf("第 %d 行序号 %d 出现缺口，期望 %d", line, record.Sequence, previous+1)}
		}
	}
	if _, ok := j.index[record.EventID]; ok {
		return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptDuplicateEventID, Line: line,
			Detail: fmt.Sprintf("第 %d 行事件标识 %s 重复", line, record.EventID)}
	}
	var payload eventPayload
	if err := json.Unmarshal(record.Payload, &payload); err != nil {
		return Event{}, nil, &JournalCorruptError{SessionID: j.sessionID, Kind: CorruptPayloadInvalid, Line: line,
			Detail: fmt.Sprintf("第 %d 行 payload 无法解码: %v", line, err)}
	}
	event := Event{
		ID:         record.EventID,
		SessionID:  record.Session,
		Seq:        record.Sequence,
		Time:       time.UnixMilli(record.TimeUnixMs),
		Type:       EventType(record.Type),
		TurnID:     payload.TurnID,
		Text:       payload.Text,
		StopReason: payload.StopReason,
	}
	if payload.Tool != nil {
		event.Tool = payload.Tool
	}
	if payload.Usage != nil {
		event.Usage = payload.Usage
	}
	entry := &journalIndexEntry{
		sequence: record.Sequence,
		timeMS:   record.TimeUnixMs,
		digest:   payloadDigest(record.Payload),
	}
	return event, entry, nil
}

// repairTruncatedTail preserves the unpersisted tail bytes as an evidence
// file and truncates the log back to offset, then syncs both
// (core/resources/src/journal.rs:861-994 evidence discipline).
func (j *journal) repairTruncatedTail(tail []byte, offset int64) error {
	if len(tail) == 0 {
		return nil
	}
	evidencePath, err := truncatedTailEvidencePath(j.dir)
	if err != nil {
		return err
	}
	if err := os.WriteFile(evidencePath, tail, 0o600); err != nil {
		return fmt.Errorf("写入截断尾证据: %w", err)
	}
	file, err := os.OpenFile(j.logPath, os.O_WRONLY, 0o600)
	if err != nil {
		return fmt.Errorf("打开事件日志以便截断: %w", err)
	}
	defer file.Close()
	if err := file.Truncate(offset); err != nil {
		return fmt.Errorf("截断事件日志尾部: %w", err)
	}
	if err := file.Sync(); err != nil {
		return fmt.Errorf("刷盘截断后的事件日志: %w", err)
	}
	syncDirectory(j.dir)
	return nil
}

// truncatedTailEvidencePath allocates a unique evidence file name, retrying
// on collision like the Rust attempt counter
// (core/resources/src/journal.rs:2871-2880).
func truncatedTailEvidencePath(dir string) (string, error) {
	base := time.Now().UnixNano()
	for attempt := 0; attempt < 1000; attempt++ {
		name := fmt.Sprintf("%s%d-%d.bin", truncatedTailEvidencePrefix, base, attempt)
		path := dir + string(os.PathSeparator) + name
		if _, err := os.Stat(path); errors.Is(err, os.ErrNotExist) {
			return path, nil
		} else if err != nil {
			return "", fmt.Errorf("检查截断尾证据文件: %w", err)
		}
	}
	return "", fmt.Errorf("分配截断尾证据文件名失败")
}

// lastSequence returns the highest persisted sequence (0 when empty).
func (j *journal) lastSequence() int64 {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.lastSeq
}

// history returns a deep copy of the replayed events.
func (j *journal) history() []Event {
	j.mu.Lock()
	defer j.mu.Unlock()
	return cloneAll(j.events)
}

// append persists one event idempotently: a known event ID with identical
// payload is a no-op (AlreadyCommitted), a known ID with different payload
// or a stale expectedSeq fails without writing anything. The sync policy is
// derived from the event type: user messages and turn terminal events sync
// inline, everything else rides the 64-record/100ms batch window
// (core/resources/src/journal.rs:1247-1420 append_idempotent).
func (j *journal) append(ev Event, expectedSeq int64) (Event, AppendOutcome, error) {
	if ev.ID == "" {
		return Event{}, AppendAppended, fmt.Errorf("事件缺少幂等标识 eventId")
	}
	if ev.Type == "" {
		return Event{}, AppendAppended, fmt.Errorf("事件缺少类型 type")
	}
	payloadWire := eventPayload{
		TurnID:     ev.TurnID,
		Text:       ev.Text,
		StopReason: ev.StopReason,
	}
	if ev.Tool != nil {
		tool := *ev.Tool
		payloadWire.Tool = &tool
	}
	if ev.Usage != nil {
		usage := *ev.Usage
		payloadWire.Usage = &usage
	}
	payload, err := json.Marshal(payloadWire)
	if err != nil {
		return Event{}, AppendAppended, fmt.Errorf("序列化事件 payload: %w", err)
	}
	digest := payloadDigest(payload)

	j.mu.Lock()
	defer j.mu.Unlock()
	if j.closed {
		return Event{}, AppendAppended, fmt.Errorf("事件日志已关闭")
	}
	if err := j.lock.lock(); err != nil {
		return Event{}, AppendAppended, err
	}
	defer j.lock.unlock()

	// Idempotency: identical content replays the committed outcome, reused
	// content under the same ID is rejected without writing.
	if existing, ok := j.index[ev.ID]; ok {
		if existing.digest != digest {
			return Event{}, AppendAppended, &EventIDConflictError{EventID: ev.ID, ExistingSeq: existing.sequence}
		}
		if err := j.syncPendingIfCommitted(); err != nil {
			return Event{}, AppendAppended, err
		}
		return j.replayEventByID(ev.ID, existing), AppendAlreadyCommitted, nil
	}
	// Sequence CAS: the watermark must match the caller's expectation or
	// nothing is written (core/resources/src/journal.rs:1290-1296).
	if j.lastSeq != expectedSeq {
		return Event{}, AppendAppended, &SequenceConflictError{Expected: expectedSeq, Actual: j.lastSeq}
	}
	if j.lastSeq+1 > journalMaxRecords {
		return Event{}, AppendAppended, fmt.Errorf("事件日志记录数达到上限 %d", journalMaxRecords)
	}
	sequence := j.lastSeq + 1
	nowMS := time.Now().UnixMilli()
	if nowMS < j.lastTimeMS {
		nowMS = j.lastTimeMS
	}
	record := journalRecord{
		Schema:     journalRecordSchema,
		Version:    journalRecordVersion,
		EventID:    ev.ID,
		Session:    j.sessionID,
		Sequence:   sequence,
		TimeUnixMs: nowMS,
		Type:       string(ev.Type),
		Payload:    payload,
	}
	line, err := json.Marshal(record)
	if err != nil {
		return Event{}, AppendAppended, fmt.Errorf("序列化事件记录: %w", err)
	}
	line = append(line, '\n')
	if len(line) > journalMaxEventBytes {
		return Event{}, AppendAppended, fmt.Errorf("事件记录 %d 字节超过单条上限 %d", len(line), journalMaxEventBytes)
	}
	if j.logLen+int64(len(line)) > journalMaxLogBytes {
		return Event{}, AppendAppended, fmt.Errorf("事件日志达到大小上限 %d", journalMaxLogBytes)
	}

	file, err := j.appendFile()
	if err != nil {
		return Event{}, AppendAppended, err
	}
	written, err := file.Write(line)
	if err != nil {
		// The write may have landed partially; the next open repairs the
		// truncated tail with evidence. In-memory state stays unadvanced.
		return Event{}, AppendAppended, fmt.Errorf("追加事件: %w", err)
	}
	_ = written
	created := j.logLen == 0 && sequence == 1

	stored := ev.clone()
	stored.ID = ev.ID
	stored.SessionID = j.sessionID
	stored.Seq = sequence
	stored.Time = time.UnixMilli(nowMS)
	stored.Replay = false
	j.lastSeq = sequence
	if nowMS > j.lastTimeMS {
		j.lastTimeMS = nowMS
	}
	j.index[ev.ID] = journalIndexEntry{sequence: sequence, timeMS: nowMS, digest: digest}
	j.events = append(j.events, stored)
	j.logLen += int64(len(line))
	if created {
		syncDirectory(j.dir)
	}

	if ev.Type.syncImmediately() {
		if err := j.syncLocked(); err != nil {
			return Event{}, AppendAppended, err
		}
	} else {
		j.scheduleBatchLocked()
	}
	return stored, AppendAppended, nil
}

// appendFile returns the lazily opened O_APPEND write handle.
func (j *journal) appendFile() (*os.File, error) {
	if j.file != nil {
		return j.file, nil
	}
	file, err := os.OpenFile(j.logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return nil, fmt.Errorf("打开事件日志: %w", err)
	}
	j.file = file
	return file, nil
}

// syncPendingIfCommitted backfills durability on the AlreadyCommitted fast
// path so a committed-but-unsynced record becomes durable before the caller
// relies on it (core/resources/src/journal.rs:1284-1288).
func (j *journal) syncPendingIfCommitted() error {
	if j.pendingRecords == 0 {
		return nil
	}
	return j.syncLocked()
}

// replayEventByID reassembles the committed event for an idempotent retry.
func (j *journal) replayEventByID(id string, entry journalIndexEntry) Event {
	for i := len(j.events) - 1; i >= 0; i-- {
		if j.events[i].ID == id {
			return j.events[i].clone()
		}
	}
	// Unreachable when the index and history are consistent; defensive.
	return Event{ID: id, SessionID: j.sessionID, Seq: entry.sequence, Time: time.UnixMilli(entry.timeMS)}
}

// scheduleBatchLocked arms the batch deadline and starts the flusher if
// needed. Called with j.mu held.
func (j *journal) scheduleBatchLocked() {
	if j.pendingRecords == 1 {
		j.pendingSince = time.Now()
	}
	if !j.flusherRunning {
		j.flusherRunning = true
		go j.flusherLoop()
	}
	if !j.flusherArmed {
		j.flusherArmed = true
		select {
		case j.flushWake <- struct{}{}:
		default:
		}
	}
}

// flusherLoop performs the batched fsync: 100ms after the first unsynced
// record, or as soon as an immediate sync left nothing pending. It exits
// after journalFlusherIdleExit without work and is restarted on demand.
func (j *journal) flusherLoop() {
	batch := time.NewTimer(0)
	if !batch.Stop() {
		select {
		case <-batch.C:
		default:
		}
	}
	idle := time.NewTimer(journalFlusherIdleExit)
	defer idle.Stop()
	for {
		select {
		case <-j.flushWake:
			j.mu.Lock()
			j.flusherArmed = false
			delay := journalBatchMaxDelay - time.Since(j.pendingSince)
			j.mu.Unlock()
			if delay < 0 {
				delay = 0
			}
			batch.Reset(delay)
			idle.Reset(journalFlusherIdleExit)
		case <-batch.C:
			j.mu.Lock()
			err := j.syncLocked()
			j.mu.Unlock()
			if err != nil {
				// A failed background sync keeps the records in the file
				// (write already succeeded); the next immediate sync or the
				// AlreadyCommitted path reconciles. Stop retrying on a timer.
				idle.Reset(journalFlusherIdleExit)
			}
		case <-idle.C:
			j.mu.Lock()
			done := j.pendingRecords == 0 && !j.flusherArmed && !j.closed
			if done {
				j.flusherRunning = false
			}
			j.mu.Unlock()
			if done {
				return
			}
			idle.Reset(journalFlusherIdleExit)
		case <-j.flushStop:
			j.mu.Lock()
			_ = j.syncLocked()
			j.mu.Unlock()
			return
		}
	}
}

// syncLocked flushes and fsyncs the write handle, resetting the pending
// batch. Called with j.mu held.
func (j *journal) syncLocked() error {
	j.pendingRecords = 0
	j.pendingSince = time.Time{}
	if j.file == nil {
		return nil
	}
	if err := j.file.Sync(); err != nil {
		return fmt.Errorf("刷盘事件日志: %w", err)
	}
	return nil
}

// close stops the flusher (flushing any pending batch), closes the write
// handle and releases the append lock.
func (j *journal) close() error {
	j.mu.Lock()
	if !j.closed {
		j.closed = true
		if j.flusherRunning {
			close(j.flushStop)
			j.flusherRunning = false
		}
	}
	err := j.syncLocked()
	if j.file != nil {
		if cerr := j.file.Close(); err == nil {
			err = cerr
		}
		j.file = nil
	}
	j.mu.Unlock()
	return j.lock.close()
}

// payloadDigest fingerprints the payload bytes for idempotency comparison
// (canonical payload hash, core/resources/src/journal.rs:1272-1277).
func payloadDigest(payload []byte) string {
	sum := sha256.Sum256(payload)
	return string(sum[:])
}
