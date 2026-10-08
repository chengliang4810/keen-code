package config

import (
	"encoding/json"
	"errors"
	"path/filepath"
	"slices"
)

// Workspaces keeps the registered project directories. Conversation facts
// (including pins) remain in the session store, not in this navigation file.
type Workspaces struct {
	Projects []string `json:"projects"`
}

func (s *Store) LoadWorkspaces() (Workspaces, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	data, exists, err := readRegularFileBounded(filepath.Join(s.root, "workspaces.json"), maxConfigFileBytes, "项目列表")
	if err != nil || !exists {
		return Workspaces{}, err
	}
	var wire struct {
		Schema  string `json:"schema"`
		Version int    `json:"version"`
		Workspaces
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return Workspaces{}, err
	}
	if wire.Schema != "keencode/workspaces" || wire.Version != 1 {
		return Workspaces{}, errors.New("项目列表格式不受支持")
	}
	for _, dir := range wire.Projects {
		if !filepath.IsAbs(dir) || filepath.Clean(dir) != dir {
			return Workspaces{}, errors.New("项目目录必须是规范绝对路径")
		}
	}
	return Workspaces{Projects: slices.Clone(wire.Projects)}, nil
}

func (s *Store) SaveWorkspaces(state Workspaces) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, dir := range state.Projects {
		if !filepath.IsAbs(dir) || filepath.Clean(dir) != dir {
			return errors.New("项目目录必须是规范绝对路径")
		}
	}
	data, err := json.Marshal(struct {
		Schema  string `json:"schema"`
		Version int    `json:"version"`
		Workspaces
	}{"keencode/workspaces", 1, state})
	if err != nil {
		return err
	}
	return atomicWritePrivate(filepath.Join(s.root, "workspaces.json"), data)
}
