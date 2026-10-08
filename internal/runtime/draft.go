package runtime

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
)

// Draft persistence (docs/go-migration.md §5.5): unsent composer text
// survives restarts. Two stores exist — the per-session draft for sessions
// that already exist (draft.txt) and the new-session draft for the
// not-yet-created draft state (draft-new.json at the data root). Writes go
// through immediately; the 500ms input debounce lives in the app layer, so
// no timer or flush buffer is kept here.
//
// Draft files are mode 0600; an empty text removes the file so an emptied
// composer does not resurrect old text after a restart.

// newDraftSchema tags draft-new.json; it is separate from the journal
// envelope schema.
const newDraftSchema = "keencode/new-draft"

// newDraftWire is the JSON shape of draft-new.json.
type newDraftWire struct {
	Schema     string `json:"schema"`
	Version    int    `json:"version"`
	ProjectDir string `json:"projectDir"`
	Text       string `json:"text"`
}

// SessionDraft returns the unsent draft of a session. ok is false when no
// draft is stored.
func (m *Manager) SessionDraft(id string) (text string, ok bool, err error) {
	if !validID(id) {
		return "", false, fmt.Errorf("%w: %q", errBadID, id)
	}
	data, err := os.ReadFile(sessionFilePath(m.root, id, sessionDraftFileName))
	if errors.Is(err, os.ErrNotExist) {
		return "", false, nil
	}
	if err != nil {
		return "", false, fmt.Errorf("读取会话草稿: %w", err)
	}
	return string(data), true, nil
}

// SaveSessionDraft stores the unsent draft of a session; empty text deletes
// the stored draft.
func (m *Manager) SaveSessionDraft(id, text string) error {
	if !validID(id) {
		return fmt.Errorf("%w: %q", errBadID, id)
	}
	path := sessionFilePath(m.root, id, sessionDraftFileName)
	if text == "" {
		if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("删除会话草稿: %w", err)
		}
		return nil
	}
	return writeFileAtomic(path, []byte(text), 0o600)
}

// NewDraft returns the draft of a session that does not exist yet. ok is
// false when nothing is stored.
func (m *Manager) NewDraft() (projectDir, text string, ok bool, err error) {
	data, err := os.ReadFile(newDraftPath(m.root))
	if errors.Is(err, os.ErrNotExist) {
		return "", "", false, nil
	}
	if err != nil {
		return "", "", false, fmt.Errorf("读取新会话草稿: %w", err)
	}
	var wire newDraftWire
	if err := json.Unmarshal(data, &wire); err != nil {
		return "", "", false, fmt.Errorf("新会话草稿不是有效 JSON: %w", err)
	}
	if wire.Schema != "" && wire.Schema != newDraftSchema {
		// draft-new.json carries its own schema tag; an unexpected one means
		// the file was not written by this package.
		return "", "", false, fmt.Errorf("新会话草稿 schema %q 不受支持", wire.Schema)
	}
	return wire.ProjectDir, wire.Text, true, nil
}

// SaveNewDraft stores the draft of a not-yet-created session; empty text
// and project dir delete the stored draft.
func (m *Manager) SaveNewDraft(projectDir, text string) error {
	if projectDir == "" && text == "" {
		if err := os.Remove(newDraftPath(m.root)); err != nil && !errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("删除新会话草稿: %w", err)
		}
		return nil
	}
	data, err := json.Marshal(newDraftWire{
		Schema:     newDraftSchema,
		Version:    1,
		ProjectDir: projectDir,
		Text:       text,
	})
	if err != nil {
		return fmt.Errorf("序列化新会话草稿: %w", err)
	}
	return writeFileAtomic(newDraftPath(m.root), data, 0o600)
}
