package runtime

import (
	"path/filepath"
)

// On-disk layout of the Go v1 data root (docs/go-migration.md §5.5, D4).
// In production the root is ~/.keencode/go-v1 (config.DefaultRoot); tests
// inject a temporary directory.
//
//	<root>/sessions/<id>/meta.json      session metadata (title, project)
//	<root>/sessions/<id>/journal.jsonl  one event envelope per line
//	<root>/sessions/<id>/draft.txt      unsent composer draft of the session
//	<root>/sessions/<id>/journal.truncated-tail-*.bin  evidence of a
//	                                                   truncated tail repair
//	<root>/draft-new.json               draft of a not-yet-created session
const (
	// sessionsDirName is the directory under the data root holding sessions.
	sessionsDirName = "sessions"
	// metaFileName stores SessionMeta as JSON.
	metaFileName = "meta.json"
	// journalFileName is the append-only JSONL event log.
	journalFileName = "journal.jsonl"
	// sessionDraftFileName stores the per-session unsent draft.
	sessionDraftFileName = "draft.txt"
	// newDraftFileName stores the draft of a session that does not exist yet.
	newDraftFileName = "draft-new.json"
	// truncatedTailEvidencePrefix names truncated-tail evidence files.
	truncatedTailEvidencePrefix = "journal.truncated-tail-"
)

// journalRecordSchema is the fixed schema name of a journal envelope record,
// mirroring core/resources/src/types.rs:13 in the fresh Go namespace.
const journalRecordSchema = "keencode/session-event"

// journalRecordVersion is the Go v1 envelope version. The data namespace is
// new (D4), so versioning restarts at 1; records written by a strictly newer
// version fail closed on replay (core/resources/src/journal.rs:2560-2566).
const journalRecordVersion = 1

// sessionDir returns the directory of one session under the root.
func sessionDir(root, id string) string {
	return filepath.Join(root, sessionsDirName, id)
}

// sessionFilePath returns the path of a per-session file.
func sessionFilePath(root, id, name string) string {
	return filepath.Join(sessionDir(root, id), name)
}

// newDraftPath returns the path of the new-session draft file.
func newDraftPath(root string) string {
	return filepath.Join(root, newDraftFileName)
}
