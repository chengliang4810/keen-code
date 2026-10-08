package runtime

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// metaSchema and metaVersion tag the session metadata file. Same contract as
// the journal envelope: fresh namespace, version restarts at 1, unknown
// fields ignored on read, newer versions fail closed.
const (
	metaSchema    = "keencode/session-meta"
	metaVersion   = 1
	maxTitleRunes = 40
)

// SessionMeta is the persisted identity of one session
// (docs/go-migration.md §5.5).
type SessionMeta struct {
	// ID is the time ordered identifier and directory name.
	ID string `json:"id"`
	// Title is the auto derived (first user message, 40 runes) or user set
	// display name. Empty until the first user message.
	Title string `json:"title"`
	// ProjectDir is the working directory tool calls run in.
	ProjectDir string `json:"projectDir"`
	Pinned     bool   `json:"pinned"`
	// CreatedAt and UpdatedAt drive the sidebar ordering (UpdatedAt first).
	CreatedAt time.Time `json:"createdAt"`
	UpdatedAt time.Time `json:"updatedAt"`
}

// metaWire is the JSON shape of meta.json.
type metaWire struct {
	Schema      string `json:"schema"`
	Version     int    `json:"version"`
	ID          string `json:"id"`
	Title       string `json:"title"`
	ProjectDir  string `json:"projectDir"`
	Pinned      bool   `json:"pinned"`
	CreatedAtMS int64  `json:"createdAtUnixMs"`
	UpdatedAtMS int64  `json:"updatedAtUnixMs"`
}

// truncatedTitle derives the session title from the first user message: trim
// surrounding whitespace, cut at 40 runes (docs/go-migration.md §5.5).
func truncatedTitle(text string) string {
	trimmed := strings.TrimSpace(text)
	runes := []rune(trimmed)
	if len(runes) > maxTitleRunes {
		runes = runes[:maxTitleRunes]
	}
	return string(runes)
}

// writeMeta atomically persists the session metadata file: temp file plus
// rename in the session directory, mode 0600
// (core/resources/src/atomic.rs discipline).
func writeMeta(path string, meta SessionMeta) error {
	wire := metaWire{
		Schema:      metaSchema,
		Version:     metaVersion,
		ID:          meta.ID,
		Title:       meta.Title,
		ProjectDir:  meta.ProjectDir,
		Pinned:      meta.Pinned,
		CreatedAtMS: meta.CreatedAt.UnixMilli(),
		UpdatedAtMS: meta.UpdatedAt.UnixMilli(),
	}
	data, err := json.Marshal(wire)
	if err != nil {
		return fmt.Errorf("序列化会话元数据: %w", err)
	}
	return writeFileAtomic(path, data, 0o600)
}

// readMeta loads and validates meta.json. Unknown fields are ignored; a
// schema/version mismatch fails closed (providers.rs tolerant loading
// discipline transplanted to session metadata).
func readMeta(path string) (SessionMeta, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return SessionMeta{}, err
	}
	var wire metaWire
	if err := json.Unmarshal(data, &wire); err != nil {
		return SessionMeta{}, fmt.Errorf("会话元数据 %s 不是有效 JSON: %w", filepath.Base(filepath.Dir(path)), err)
	}
	if wire.Schema != metaSchema {
		return SessionMeta{}, fmt.Errorf("会话元数据 schema %q 不是 %q", wire.Schema, metaSchema)
	}
	if wire.Version > metaVersion {
		return SessionMeta{}, fmt.Errorf("会话元数据版本 %d 高于支持的 %d", wire.Version, metaVersion)
	}
	if wire.ID == "" {
		return SessionMeta{}, fmt.Errorf("会话元数据缺少 id")
	}
	return SessionMeta{
		ID:         wire.ID,
		Title:      wire.Title,
		ProjectDir: wire.ProjectDir,
		Pinned:     wire.Pinned,
		CreatedAt:  time.UnixMilli(wire.CreatedAtMS),
		UpdatedAt:  time.UnixMilli(wire.UpdatedAtMS),
	}, nil
}
