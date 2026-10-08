package config

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// TestSettingsSchemaRoundtrip ports the schema half of
// settings_schema_tolerates_drift_with_defaults_and_warnings
// (apps/desktop/src/app_settings.rs:723-744): the current envelope writes
// the fixed schema/version and reloads as the defaults.
func TestSettingsSchemaRoundtrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")
	if err := SaveSettingsToPath(path, DefaultSettings()); err != nil {
		t.Fatalf("save defaults: %v", err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read saved settings: %v", err)
	}
	for _, want := range []string{
		`"schema": "keencode/app-settings"`,
		`"version": 1`,
		`"theme": "dark"`,
		`"defaultModel": null`,
		`"workingDirectory": ""`,
		`"toolPermissionPolicy": "ask"`,
	} {
		if !strings.Contains(string(data), want) {
			t.Errorf("saved settings missing %s in:\n%s", want, data)
		}
	}
	loaded := LoadSettingsFromPath(path)
	if loaded.LoadError != "" {
		t.Fatalf("reload error: %s", loaded.LoadError)
	}
	if len(loaded.Warnings) != 0 {
		t.Errorf("warnings = %v, want empty", loaded.Warnings)
	}
	if loaded.Settings != DefaultSettings() {
		t.Errorf("settings = %+v, want defaults", loaded.Settings)
	}
}

// TestSettingsTolerantLoading ports
// settings_schema_tolerates_drift_with_defaults_and_warnings and
// invalid_settings_fall_back_to_defaults_without_replacement
// (app_settings.rs:746-817): missing fields fall back to defaults, unknown
// fields warn, broken files yield the defaults with a load error and never
// touch the original file.
func TestSettingsTolerantLoading(t *testing.T) {
	valid := `{"schema":"keencode/app-settings","version":1,"theme":"light","defaultModel":null,"workingDirectory":"","toolPermissionPolicy":"ask"}`
	cases := []struct {
		name          string
		content       string
		wantWarning   string
		wantLoadError string
		check         func(t *testing.T, settings Settings)
	}{
		{
			name:    "valid minimal",
			content: valid,
			check: func(t *testing.T, settings Settings) {
				if settings.Theme != ThemeLight {
					t.Errorf("theme = %q, want light", settings.Theme)
				}
			},
		},
		{
			name:    "missing fields fall back to defaults",
			content: `{"schema":"keencode/app-settings","version":1}`,
			check: func(t *testing.T, settings Settings) {
				if settings != DefaultSettings() {
					t.Errorf("settings = %+v, want defaults", settings)
				}
			},
		},
		{
			name:        "unknown field warns",
			content:     strings.Replace(valid, `"toolPermissionPolicy":"ask"`, `"toolPermissionPolicy":"ask","oldSetting":true`, 1),
			wantWarning: "oldSetting",
		},
		{
			name:          "missing schema fails",
			content:       `{"version":1}`,
			wantLoadError: "schema",
		},
		{
			name:          "empty object fails",
			content:       `{}`,
			wantLoadError: "schema",
		},
		{
			name:          "version drift fails",
			content:       strings.Replace(valid, `"version":1`, `"version":99`, 1),
			wantLoadError: "schema 或版本",
		},
		{
			name:          "broken json falls back",
			content:       "{ broken",
			wantLoadError: "JSON",
		},
		{
			name:          "invalid theme falls back",
			content:       strings.Replace(valid, `"theme":"light"`, `"theme":"sepia"`, 1),
			wantLoadError: "主题",
		},
		{
			name:          "invalid policy falls back",
			content:       strings.Replace(valid, `"toolPermissionPolicy":"ask"`, `"toolPermissionPolicy":"yolo"`, 1),
			wantLoadError: "工具权限策略",
		},
		{
			name:    "explicit default model parses",
			content: strings.Replace(valid, `"defaultModel":null`, `"defaultModel":{"providerId":"p1","modelId":"m1"}`, 1),
			check: func(t *testing.T, settings Settings) {
				if settings.DefaultModel == nil || settings.DefaultModel.ProviderID != "p1" || settings.DefaultModel.ModelID != "m1" {
					t.Errorf("default model = %+v, want p1/m1", settings.DefaultModel)
				}
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			path := writeTempFile(t, dir, "settings.json", tc.content)
			loaded := LoadSettingsFromPath(path)
			if tc.wantLoadError != "" {
				if loaded.LoadError == "" {
					t.Fatalf("load error = %q, want containing %q", loaded.LoadError, tc.wantLoadError)
				}
				if !strings.Contains(loaded.LoadError, tc.wantLoadError) {
					t.Errorf("load error = %q, want containing %q", loaded.LoadError, tc.wantLoadError)
				}
			} else if loaded.LoadError != "" {
				t.Fatalf("unexpected load error: %s", loaded.LoadError)
			}
			if tc.wantWarning != "" {
				found := false
				for _, warning := range loaded.Warnings {
					if strings.Contains(warning, tc.wantWarning) {
						found = true
						break
					}
				}
				if !found {
					t.Errorf("warnings = %v, want one naming %q", loaded.Warnings, tc.wantWarning)
				}
			}
			if tc.check != nil {
				tc.check(t, loaded.Settings)
			}
			if loaded.LoadError != "" && loaded.Settings != DefaultSettings() {
				t.Error("a failed load must return the default settings")
			}
			data, err := os.ReadFile(path)
			if err != nil {
				t.Fatalf("re-read settings: %v", err)
			}
			if string(data) != tc.content {
				t.Error("loading must not rewrite the original file")
			}
		})
	}
}

// TestMissingSettingsFileUsesDefaults covers the absent-file branch: no
// error, defaults, no file created.
func TestMissingSettingsFileUsesDefaults(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")
	loaded := LoadSettingsFromPath(path)
	if loaded.LoadError != "" {
		t.Fatalf("load error for missing file: %s", loaded.LoadError)
	}
	if loaded.Settings != DefaultSettings() {
		t.Errorf("settings = %+v, want defaults", loaded.Settings)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Error("load must not create the settings file")
	}
}

// TestSettingsNonRegularPaths ports non_regular_settings_path_falls_back and
// symlinked_settings_are_not_followed (app_settings.rs:820-855).
func TestSettingsNonRegularPaths(t *testing.T) {
	dir := t.TempDir()

	directory := filepath.Join(dir, "settings.json")
	if err := os.Mkdir(directory, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if loaded := LoadSettingsFromPath(directory); loaded.LoadError == "" {
		t.Error("directory settings path should fail with a load error")
	}

	if runtime.GOOS == "windows" {
		t.Skip("symlink test requires unix")
	}
	target := filepath.Join(dir, "outside.json")
	if err := os.WriteFile(target, []byte("{broken target"), 0o600); err != nil {
		t.Fatalf("write target: %v", err)
	}
	link := filepath.Join(dir, "link.json")
	if err := os.Symlink(target, link); err != nil {
		t.Fatalf("symlink: %v", err)
	}
	loaded := LoadSettingsFromPath(link)
	if loaded.LoadError == "" {
		t.Error("symlinked settings should not be followed")
	}
	data, err := os.ReadFile(target)
	if err != nil || string(data) != "{broken target" {
		t.Errorf("target changed: %q / %v", data, err)
	}
}

// TestSettingsValidate is the table for the constraints validation cannot
// infer from types (app_settings.rs:204-255 adapted to the Go fields).
func TestSettingsValidate(t *testing.T) {
	cases := []struct {
		name     string
		settings Settings
		wantErr  bool
	}{
		{"defaults", DefaultSettings(), false},
		{"light", Settings{Theme: ThemeLight, ToolPermissionPolicy: ToolPermissionAsk}, false},
		{"allow all", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAllowAll}, false},
		{"read only", Settings{Theme: ThemeSystem, ToolPermissionPolicy: ToolPermissionReadOnly}, false},
		{"unknown theme", Settings{Theme: "sepia", ToolPermissionPolicy: ToolPermissionAsk}, true},
		{"unknown policy", Settings{Theme: ThemeDark, ToolPermissionPolicy: "always"}, true},
		{"relative working dir", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAsk, WorkingDirectory: "relative/projects"}, true},
		{"dotted working dir", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAsk, WorkingDirectory: "/tmp/a/../b"}, true},
		{"padded working dir", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAsk, WorkingDirectory: " /tmp/a"}, true},
		{"absolute working dir", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAsk, WorkingDirectory: "/tmp/projects"}, false},
		{"default model bad provider", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAsk, DefaultModel: &ModelRef{ProviderID: "bad id!", ModelID: "m"}}, true},
		{"default model empty model id", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAsk, DefaultModel: &ModelRef{ProviderID: "p", ModelID: " "}}, true},
		{"default model valid", Settings{Theme: ThemeDark, ToolPermissionPolicy: ToolPermissionAsk, DefaultModel: &ModelRef{ProviderID: "p", ModelID: "m"}}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.settings.Validate()
			if (err != nil) != tc.wantErr {
				t.Errorf("Validate() = %v, wantErr %v", err, tc.wantErr)
			}
		})
	}
}

// TestSettingsSavePreservesUnknownFields applies the providers.json
// preservation contract to settings.json as well.
func TestSettingsSavePreservesUnknownFields(t *testing.T) {
	dir := t.TempDir()
	path := writeTempFile(t, dir, "settings.json",
		`{"schema":"keencode/app-settings","version":1,"theme":"light","futureFlag":{"a":1}}`)
	loaded := LoadSettingsFromPath(path)
	if loaded.LoadError != "" {
		t.Fatalf("load: %s", loaded.LoadError)
	}
	if err := SaveSettingsToPath(path, loaded.Settings); err != nil {
		t.Fatalf("save: %v", err)
	}
	data, _ := os.ReadFile(path)
	if !strings.Contains(string(data), `"futureFlag"`) {
		t.Errorf("saved settings lost unknown field:\n%s", data)
	}
	reloaded := LoadSettingsFromPath(path)
	if reloaded.LoadError != "" || reloaded.Settings.Theme != ThemeLight {
		t.Errorf("reload = %+v / %s", reloaded.Settings, reloaded.LoadError)
	}
}

// TestSettingsSaveRefusesUnparseableExisting mirrors app_settings set()
// refusing to save over an unparseable file (app_settings.rs:441-444).
func TestSettingsSaveRefusesUnparseableExisting(t *testing.T) {
	dir := t.TempDir()
	path := writeTempFile(t, dir, "settings.json", "{ broken")
	original, _ := os.ReadFile(path)
	if err := SaveSettingsToPath(path, DefaultSettings()); err == nil {
		t.Fatal("save over unparseable settings should fail")
	}
	data, _ := os.ReadFile(path)
	if string(data) != string(original) {
		t.Error("failed save must not touch the original file")
	}
}

// TestSettingsSaveRejectsInvalid ensures invalid settings are rejected
// before writing.
func TestSettingsSaveRejectsInvalid(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")
	invalid := DefaultSettings()
	invalid.Theme = "neon"
	if err := SaveSettingsToPath(path, invalid); err == nil {
		t.Fatal("invalid settings should be rejected")
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Error("rejected save must not create the file")
	}
}

// TestSettingsExplicitValuesRoundtrip verifies a fully populated settings
// file survives a save/load cycle unchanged.
func TestSettingsExplicitValuesRoundtrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")
	want := Settings{
		Theme:                ThemeLight,
		DefaultModel:         &ModelRef{ProviderID: "gateway", ModelID: "glm-5.3-flash"},
		WorkingDirectory:     "/Users/demo/Documents/KeenCode",
		ToolPermissionPolicy: ToolPermissionReadOnly,
	}
	if err := SaveSettingsToPath(path, want); err != nil {
		t.Fatalf("save: %v", err)
	}
	loaded := LoadSettingsFromPath(path)
	if loaded.LoadError != "" {
		t.Fatalf("reload: %s", loaded.LoadError)
	}
	got := loaded.Settings
	if got.Theme != want.Theme || got.WorkingDirectory != want.WorkingDirectory ||
		got.ToolPermissionPolicy != want.ToolPermissionPolicy {
		t.Errorf("roundtrip = %+v, want %+v", got, want)
	}
	switch {
	case got.DefaultModel == nil || want.DefaultModel == nil:
		if got.DefaultModel != want.DefaultModel {
			t.Errorf("roundtrip default model = %+v, want %+v", got.DefaultModel, want.DefaultModel)
		}
	case *got.DefaultModel != *want.DefaultModel:
		t.Errorf("roundtrip default model = %+v, want %+v", *got.DefaultModel, *want.DefaultModel)
	}
}
