package config

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// DefaultRoot returns the KeenCode Go data root: $KEENCODE_GO_HOME when set
// (test isolation), otherwise $HOME/.keencode/go-v1. The go-v1 subtree keeps
// the Go application's data isolated from the Rust desktop build's
// ~/.keencode root (docs/go-migration.md §5.5, decision D4).
func DefaultRoot() (string, error) {
	if override := strings.TrimSpace(os.Getenv("KEENCODE_GO_HOME")); override != "" {
		return override, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", errors.New("无法确定当前用户目录")
	}
	return filepath.Join(home, ".keencode", "go-v1"), nil
}

// Store serializes access to the two JSON configuration files (providers.json
// and settings.json) under one data root. All methods are safe for
// concurrent use; callers that interleave loads and saves for the same root
// should share one Store so the mutations are serialized.
type Store struct {
	mu   sync.Mutex
	root string
}

// OpenStore returns a Store rooted at root. It performs no filesystem
// access: files are created lazily by the first save.
func OpenStore(root string) (*Store, error) {
	if strings.TrimSpace(root) == "" {
		return nil, errors.New("配置根目录不能为空")
	}
	return &Store{root: root}, nil
}

// Root returns the data root the store was opened with.
func (s *Store) Root() string {
	return s.root
}

// ProvidersPath is the providers.json location under the root.
func (s *Store) ProvidersPath() string {
	return filepath.Join(s.root, providersFileName)
}

// SettingsPath is the settings.json location under the root.
func (s *Store) SettingsPath() string {
	return filepath.Join(s.root, settingsFileName)
}

// LoadProviders reads providers.json through this store.
func (s *Store) LoadProviders() (ProvidersState, []string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return LoadProvidersFromPath(s.ProvidersPath())
}

// SaveProviders validates and atomically writes providers.json through this
// store, preserving unknown fields of the existing file.
func (s *Store) SaveProviders(state ProvidersState) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return SaveProvidersToPath(s.ProvidersPath(), state)
}

// LoadSettings reads settings.json through this store. Loading never fails;
// see SettingsLoad.
func (s *Store) LoadSettings() SettingsLoad {
	s.mu.Lock()
	defer s.mu.Unlock()
	return LoadSettingsFromPath(s.SettingsPath())
}

// SaveSettings validates and atomically writes settings.json through this
// store, preserving unknown fields of the existing file.
func (s *Store) SaveSettings(settings Settings) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return SaveSettingsToPath(s.SettingsPath(), settings)
}
