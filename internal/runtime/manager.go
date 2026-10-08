package runtime

import (
	"errors"
	"fmt"
	"os"
	"sort"
	"strings"
	"sync"
	"time"
)

// Manager owns the session lifecycle over one injectable data root: create,
// list, rename, delete, and lazy open with journal replay. Production root
// is ~/.keencode/go-v1 (docs/go-migration.md D4); tests inject a temporary
// directory.
type Manager struct {
	root         string
	agentFactory AgentFactory
	modelFn      ModelFunc

	// mu guards the loaded session registry.
	mu sync.Mutex
	// sessions holds every loaded session by id.
	sessions map[string]*Session
	// loadMu serializes lazy opens so two concurrent Get calls never build
	// two Session objects over one journal.
	loadMu sync.Mutex
}

// ManagerOptions wires the agent assembly (docs/go-migration.md §5.7: deps
// func() agent.Dependencies). Both fields are optional; Send without a
// factory fails with ErrNoAgent.
type ManagerOptions struct {
	// Agent builds a TurnRunner per turn; called under the session lock at
	// Send time.
	Agent AgentFactory
	// Model resolves the model id recorded into TurnRequest at Send time.
	Model ModelFunc
}

// OpenManager prepares the data root (creating the sessions directory) and
// returns an empty registry. Existing sessions load lazily via Get.
func OpenManager(root string, opts ManagerOptions) (*Manager, error) {
	trimmed := strings.TrimSpace(root)
	if trimmed == "" {
		return nil, errors.New("数据根目录不能为空")
	}
	if err := os.MkdirAll(sessionsRoot(trimmed), 0o700); err != nil {
		return nil, fmt.Errorf("创建会话目录: %w", err)
	}
	return &Manager{
		root:         trimmed,
		agentFactory: opts.Agent,
		modelFn:      opts.Model,
		sessions:     make(map[string]*Session),
	}, nil
}

// sessionsRoot returns the directory that holds per-session directories.
func sessionsRoot(root string) string {
	return root + string(os.PathSeparator) + sessionsDirName
}

// Create starts a new session for the project directory: it allocates the
// id, writes meta.json and opens an empty journal. No user message is
// journaled here — the first Send does that (draft→session conversion,
// docs/go-migration.md §5.5).
func (m *Manager) Create(projectDir string) (*Session, error) {
	dir := strings.TrimSpace(projectDir)
	if dir == "" {
		return nil, errors.New("项目目录不能为空")
	}
	id := newID()
	now := time.Now()
	meta := SessionMeta{ID: id, ProjectDir: dir, CreatedAt: now, UpdatedAt: now}
	sdir := sessionDir(m.root, id)
	if err := os.MkdirAll(sdir, 0o700); err != nil {
		return nil, fmt.Errorf("创建会话目录: %w", err)
	}
	if err := writeMeta(sessionFilePath(m.root, id, metaFileName), meta); err != nil {
		os.RemoveAll(sdir)
		return nil, err
	}
	jrn, err := openJournal(id, sdir)
	if err != nil {
		os.RemoveAll(sdir)
		return nil, err
	}
	s := newSession(m.root, meta, jrn, m.agentFactory, m.modelFn)
	m.mu.Lock()
	m.sessions[id] = s
	m.mu.Unlock()
	return s, nil
}

// Get returns the loaded session or lazily opens it: meta.json is read, the
// journal is replayed (with truncated-tail repair), and interrupted turns
// get their in-memory failure projection. A corrupt journal fails closed
// with the JournalCorruptError detail.
func (m *Manager) Get(id string) (*Session, error) {
	if !validID(id) {
		return nil, fmt.Errorf("%w: %q", errBadID, id)
	}
	m.mu.Lock()
	s := m.sessions[id]
	m.mu.Unlock()
	if s != nil {
		return s, nil
	}
	m.loadMu.Lock()
	defer m.loadMu.Unlock()
	m.mu.Lock()
	s = m.sessions[id]
	m.mu.Unlock()
	if s != nil {
		return s, nil
	}

	meta, err := readMeta(sessionFilePath(m.root, id, metaFileName))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, fmt.Errorf("会话 %s 不存在", id)
		}
		return nil, err
	}
	jrn, err := openJournal(id, sessionDir(m.root, id))
	if err != nil {
		return nil, err
	}
	s = newSession(m.root, meta, jrn, m.agentFactory, m.modelFn)
	m.mu.Lock()
	m.sessions[id] = s
	m.mu.Unlock()
	return s, nil
}

// List returns every session ordered by UpdatedAt descending (newest
// first, docs/go-migration.md §5.5). Entries with an unreadable or corrupt
// meta.json are skipped; the corruption surfaces when the session is opened
// via Get.
func (m *Manager) List() ([]SessionMeta, error) {
	entries, err := os.ReadDir(sessionsRoot(m.root))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("读取会话目录: %w", err)
	}
	metas := make([]SessionMeta, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() || !validID(entry.Name()) {
			continue
		}
		m.mu.Lock()
		loaded := m.sessions[entry.Name()]
		m.mu.Unlock()
		if loaded != nil {
			metas = append(metas, loaded.currentMeta())
			continue
		}
		meta, err := readMeta(sessionFilePath(m.root, entry.Name(), metaFileName))
		if err != nil {
			continue
		}
		metas = append(metas, meta)
	}
	sort.Slice(metas, func(i, j int) bool {
		if !metas[i].UpdatedAt.Equal(metas[j].UpdatedAt) {
			return metas[i].UpdatedAt.After(metas[j].UpdatedAt)
		}
		return metas[i].ID > metas[j].ID
	})
	return metas, nil
}

// Rename sets the display title of a session, overriding the auto-derived
// one (docs/go-migration.md §5.5).
func (m *Manager) Rename(id, title string) error {
	if !validID(id) {
		return fmt.Errorf("%w: %q", errBadID, id)
	}
	trimmed := strings.TrimSpace(title)
	if trimmed == "" {
		return errors.New("会话标题不能为空")
	}
	m.mu.Lock()
	loaded := m.sessions[id]
	m.mu.Unlock()
	if loaded != nil {
		return loaded.rename(trimmed)
	}
	meta, err := readMeta(sessionFilePath(m.root, id, metaFileName))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("会话 %s 不存在", id)
		}
		return err
	}
	meta.Title = trimmed
	return writeMeta(sessionFilePath(m.root, id, metaFileName), meta)
}

// SetPinned updates the authoritative session metadata, including unloaded
// sessions. The session lock serializes pinning with turn metadata updates.
func (m *Manager) SetPinned(id string, pinned bool) error {
	session, err := m.Get(id)
	if err != nil {
		return err
	}
	return session.setPinned(pinned)
}

// Delete removes a session and its directory. A running session is refused
// (docs/go-migration.md §5.5); subscribers are detached and their channels
// drain and close.
func (m *Manager) Delete(id string) error {
	if !validID(id) {
		return fmt.Errorf("%w: %q", errBadID, id)
	}
	m.mu.Lock()
	loaded := m.sessions[id]
	if loaded != nil {
		loaded.mu.Lock()
		if loaded.running {
			loaded.mu.Unlock()
			m.mu.Unlock()
			return ErrBusy
		}
		loaded.deleted = true
		loaded.mu.Unlock()
		delete(m.sessions, id)
	}
	m.mu.Unlock()
	if loaded != nil {
		loaded.hub.close()
		_ = loaded.jrn.close()
	}
	if err := os.RemoveAll(sessionDir(m.root, id)); err != nil {
		return fmt.Errorf("删除会话目录: %w", err)
	}
	return nil
}
