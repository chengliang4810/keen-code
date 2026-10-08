package config

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

// TestDefaultRoot checks the KEENCODE_GO_HOME override and the default
// go-v1 layout (docs/go-migration.md §5.6).
func TestDefaultRoot(t *testing.T) {
	t.Setenv("KEENCODE_GO_HOME", "/tmp/keencode-test-root")
	root, err := DefaultRoot()
	if err != nil {
		t.Fatalf("DefaultRoot: %v", err)
	}
	if root != "/tmp/keencode-test-root" {
		t.Errorf("DefaultRoot with override = %q, want /tmp/keencode-test-root", root)
	}

	t.Setenv("KEENCODE_GO_HOME", "")
	root, err = DefaultRoot()
	if err != nil {
		t.Fatalf("DefaultRoot: %v", err)
	}
	if filepath.Base(filepath.Dir(root)) != ".keencode" || filepath.Base(root) != "go-v1" {
		t.Errorf("DefaultRoot = %q, want $HOME/.keencode/go-v1", root)
	}
}

// TestOpenStoreRejectsEmptyRoot keeps the store contract strict.
func TestOpenStoreRejectsEmptyRoot(t *testing.T) {
	for _, root := range []string{"", "   "} {
		if _, err := OpenStore(root); err == nil {
			t.Errorf("OpenStore(%q) should fail", root)
		}
	}
	store, err := OpenStore("/tmp/whatever")
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	if store.Root() != "/tmp/whatever" {
		t.Errorf("Root() = %q", store.Root())
	}
	if store.ProvidersPath() != filepath.Join("/tmp/whatever", "providers.json") {
		t.Errorf("ProvidersPath() = %q", store.ProvidersPath())
	}
	if store.SettingsPath() != filepath.Join("/tmp/whatever", "settings.json") {
		t.Errorf("SettingsPath() = %q", store.SettingsPath())
	}
}

// TestStoreRoundtrip exercises both files through the Store API.
func TestStoreRoundtrip(t *testing.T) {
	dir := t.TempDir()
	store, err := OpenStore(dir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	providers, warnings, err := store.LoadProviders()
	if err != nil || len(warnings) != 0 || len(providers.Providers) != 0 {
		t.Fatalf("initial providers load = %+v / %v / %v", providers, warnings, err)
	}
	record, err := NewProviderRecord("gateway", "Gateway", "https://gw.example.com/v1",
		ProtocolChatCompletions, []string{"glm-5.3-flash"}, strPtr("test-key"))
	if err != nil {
		t.Fatalf("NewProviderRecord: %v", err)
	}
	state := ProvidersState{
		ActiveProviderID: strPtr("gateway"),
		ActiveModelID:    strPtr("glm-5.3-flash"),
		Providers:        []ProviderRecord{record},
	}
	if err := store.SaveProviders(state); err != nil {
		t.Fatalf("SaveProviders: %v", err)
	}
	loaded, warnings, err := store.LoadProviders()
	if err != nil {
		t.Fatalf("reload providers: %v", err)
	}
	if len(warnings) != 0 {
		t.Errorf("reload warnings = %v", warnings)
	}
	if len(loaded.Providers) != 1 || loaded.Providers[0].ID != "gateway" {
		t.Errorf("reloaded providers = %+v", loaded.Providers)
	}

	settings := DefaultSettings()
	settings.Theme = ThemeLight
	settings.ToolPermissionPolicy = ToolPermissionAllowAll
	if err := store.SaveSettings(settings); err != nil {
		t.Fatalf("SaveSettings: %v", err)
	}
	settingsLoad := store.LoadSettings()
	if settingsLoad.LoadError != "" {
		t.Fatalf("LoadSettings: %s", settingsLoad.LoadError)
	}
	if settingsLoad.Settings != settings {
		t.Errorf("reloaded settings = %+v, want %+v", settingsLoad.Settings, settings)
	}

	// OpenStore must not have created anything besides the saved files.
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("read dir: %v", err)
	}
	if len(entries) != 2 {
		t.Errorf("data root contains %d entries, want exactly providers.json and settings.json", len(entries))
	}
}

// TestAtomicWriteReplacesRepeatedly ports
// private_atomic_write_replaces_existing_target_repeatedly
// (apps/desktop/src/storage.rs:347-365): repeated saves replace the target,
// keep mode 0600, and leave no temporary files behind.
func TestAtomicWriteReplacesRepeatedly(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")
	for round, content := range []string{"first", "second", "third"} {
		if err := atomicWritePrivate(path, []byte(content)); err != nil {
			t.Fatalf("round %d: %v", round, err)
		}
	}
	data, err := os.ReadFile(path)
	if err != nil || string(data) != "third" {
		t.Fatalf("final content = %q / %v, want third", data, err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Errorf("mode = %v, want 0600", info.Mode().Perm())
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("read dir: %v", err)
	}
	if len(entries) != 1 {
		t.Errorf("directory holds %d entries, want only the target (temp files leaked)", len(entries))
	}
}

// TestAtomicWriteFailurePreservesTarget ports
// private_atomic_write_failure_preserves_target_and_cleans_temp
// (storage.rs:368-382): a target that cannot be replaced stays intact.
func TestAtomicWriteFailurePreservesTarget(t *testing.T) {
	dir := t.TempDir()
	target := filepath.Join(dir, "occupied")
	if err := os.Mkdir(target, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	marker := filepath.Join(target, "original.txt")
	if err := os.WriteFile(marker, []byte("original"), 0o600); err != nil {
		t.Fatalf("write marker: %v", err)
	}
	if err := atomicWritePrivate(target, []byte("new")); err == nil {
		t.Fatal("writing over a directory should fail")
	}
	data, err := os.ReadFile(marker)
	if err != nil || string(data) != "original" {
		t.Errorf("marker = %q / %v, want original", data, err)
	}
	entries, _ := os.ReadDir(dir)
	if len(entries) != 1 {
		t.Error("failed write must clean up its temporary file")
	}
}

// TestAtomicWriteRejectsSymlinkTarget keeps the save path from replacing a
// symlink (providers.rs:1233-1237).
func TestAtomicWriteRejectsSymlinkTarget(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink test requires unix")
	}
	dir := t.TempDir()
	victim := filepath.Join(dir, "victim.txt")
	if err := os.WriteFile(victim, []byte("victim"), 0o600); err != nil {
		t.Fatalf("write victim: %v", err)
	}
	link := filepath.Join(dir, "settings.json")
	if err := os.Symlink(victim, link); err != nil {
		t.Fatalf("symlink: %v", err)
	}
	if err := atomicWritePrivate(link, []byte("new")); err == nil {
		t.Fatal("atomic write over a symlink should fail")
	}
	data, err := os.ReadFile(victim)
	if err != nil || string(data) != "victim" {
		t.Errorf("victim changed: %q / %v", data, err)
	}
	info, err := os.Lstat(link)
	if err != nil || info.Mode()&os.ModeSymlink == 0 {
		t.Error("failed write must keep the symlink")
	}
}
