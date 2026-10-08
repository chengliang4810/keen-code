package config

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// TestValidateProviderID mirrors validate_provider_id
// (apps/desktop/src/providers.rs:1285-1301).
func TestValidateProviderID(t *testing.T) {
	cases := []struct {
		name    string
		raw     string
		want    string
		wantErr bool
	}{
		{"plain", "openai", "openai", false},
		{"trimmed", "  openai  ", "openai", false},
		{"allowed punctuation", "a1._-", "a1._-", false},
		{"leading digit", "1provider", "1provider", false},
		{"empty", "   ", "", true},
		{"too long", strings.Repeat("a", 65), "", true},
		{"max length ok", strings.Repeat("a", 64), strings.Repeat("a", 64), false},
		{"leading dot", ".provider", "", true},
		{"leading dash", "-provider", "", true},
		{"space inside", "a b", "", true},
		{"unicode", "供应商", "", true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := ValidateProviderID(tc.raw)
			if tc.wantErr {
				if err == nil {
					t.Fatalf("ValidateProviderID(%q) = %q, want error", tc.raw, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("ValidateProviderID(%q) unexpected error: %v", tc.raw, err)
			}
			if got != tc.want {
				t.Errorf("ValidateProviderID(%q) = %q, want %q", tc.raw, got, tc.want)
			}
		})
	}
}

// TestValidateSecret mirrors validate_secret (providers.rs:1243-1257).
func TestValidateSecret(t *testing.T) {
	cases := []struct {
		name    string
		secret  string
		wantErr bool
	}{
		{"normal", "sk-abc123", false},
		{"empty", "", true},
		{"leading space", " sk", true},
		{"trailing newline", "sk\n", true},
		{"control char", "sk\x7f", true},
		{"oversized", strings.Repeat("a", maxProviderAPIKeyBytes+1), true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := ValidateSecret(tc.secret)
			if (err != nil) != tc.wantErr {
				t.Errorf("ValidateSecret(%q) error = %v, wantErr %v", tc.secret, err, tc.wantErr)
			}
		})
	}
	// nil optional key explicitly means no authentication.
	if err := ValidateAPIKey(nil); err != nil {
		t.Errorf("ValidateAPIKey(nil) unexpected error: %v", err)
	}
	key := "sk-abc"
	if err := ValidateAPIKey(&key); err != nil {
		t.Errorf("ValidateAPIKey(valid) unexpected error: %v", err)
	}
}

// TestNormalizeModels mirrors normalize_models (providers.rs:1201-1217).
func TestNormalizeModels(t *testing.T) {
	cases := []struct {
		name    string
		input   []string
		want    []string
		wantErr bool
	}{
		{"order kept", []string{"b", "a"}, []string{"b", "a"}, false},
		{"trim and dedup", []string{" a ", "a", "b"}, []string{"a", "b"}, false},
		{"empty entries skipped", []string{"", "  ", "a"}, []string{"a"}, false},
		{"all empty", []string{"", " "}, nil, true},
		{"nil", nil, nil, true},
		{"control char", []string{"a\x01b"}, nil, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := NormalizeModels(tc.input)
			if tc.wantErr {
				if err == nil {
					t.Fatalf("NormalizeModels(%v) = %v, want error", tc.input, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("NormalizeModels(%v) unexpected error: %v", tc.input, err)
			}
			if strings.Join(got, "|") != strings.Join(tc.want, "|") {
				t.Errorf("NormalizeModels(%v) = %v, want %v", tc.input, got, tc.want)
			}
		})
	}
}

// TestValidateBaseURL ports the Rust base URL tests
// (providers.rs:1447-1496).
func TestValidateBaseURL(t *testing.T) {
	cases := []struct {
		name    string
		raw     string
		want    string
		wantErr string
	}{
		{"bare domain gains /v1", "https://api.example.com", "https://api.example.com/v1", ""},
		{"custom path kept", "https://api.example.com/custom", "https://api.example.com/custom", ""},
		{"trailing slash trimmed", "https://api.example.com/v1/", "https://api.example.com/v1", ""},
		{"exact marker preserved", "https://api.example.com/v2/chat/completions#", "https://api.example.com/v2/chat/completions#", ""},
		{"marker on bare domain", "https://api.example.com#", "", "完整的请求路径"},
		{"named fragment", "https://api.example.com/v1#section", "", "# 片段"},
		{"not a url", "not-a-url", "", "无效"},
		{"ftp scheme", "ftp://api.example.com", "", "http 或 https"},
		{"empty", "   ", "", "无效"},
		{"scheme lowercased", "HTTPS://API.example.com", "https://api.example.com/v1", ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := ValidateBaseURL(tc.raw)
			if tc.wantErr != "" {
				if err == nil {
					t.Fatalf("ValidateBaseURL(%q) = %q, want error containing %q", tc.raw, got, tc.wantErr)
				}
				if !strings.Contains(err.Error(), tc.wantErr) {
					t.Errorf("ValidateBaseURL(%q) error = %v, want containing %q", tc.raw, err, tc.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("ValidateBaseURL(%q) unexpected error: %v", tc.raw, err)
			}
			if got != tc.want {
				t.Errorf("ValidateBaseURL(%q) = %q, want %q", tc.raw, got, tc.want)
			}
		})
	}
}

// TestValidateExactEndpoint ports providers.rs:1519-1540: a '#' exact-path
// URL must end with the chosen protocol's generation endpoint.
func TestValidateExactEndpoint(t *testing.T) {
	cases := []struct {
		name    string
		baseURL string
		backend string
		wantErr bool
	}{
		{"matching chat marker", "https://api.example/v2/chat/completions#", "chat_completions", false},
		{"no marker always ok", "https://api.example/v2/chat/completions", "chat_completions", false},
		{"wrong suffix marker", "https://api.example/v2#", "chat_completions", true},
		{"protocol mismatch marker", "https://api.example/v2/messages#", "chat_completions", true},
		{"messages marker", "https://api.example/v2/messages#", "messages", false},
		{"responses marker", "https://api.example/v2/responses#", "responses", false},
		{"unknown backend marker", "https://api.example/v2/messages#", "bogus", true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := ValidateExactEndpoint(tc.baseURL, tc.backend)
			if (err != nil) != tc.wantErr {
				t.Errorf("ValidateExactEndpoint(%q, %q) error = %v, wantErr %v",
					tc.baseURL, tc.backend, err, tc.wantErr)
			}
		})
	}
}

// TestReasoningEffortsInOrder ports providers.rs:1409-1435.
func TestReasoningEffortsInOrder(t *testing.T) {
	cases := []struct {
		name   string
		values []string
		want   bool
	}{
		{"ascending", []string{"low", "high", "max"}, true},
		{"empty", []string{}, true},
		{"unknown level", []string{"unknown"}, false},
		{"duplicate", []string{"low", "low"}, false},
		{"descending", []string{"high", "low"}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := reasoningEffortsInOrder(tc.values); got != tc.want {
				t.Errorf("reasoningEffortsInOrder(%v) = %v, want %v", tc.values, got, tc.want)
			}
		})
	}
}

// TestProviderEndpointResolution ports the runtime mapping checks of
// providers.rs:2128-2202: the '#' marker and protocol endpoint suffix are
// stripped exactly once and authentication follows the explicit key.
func TestProviderEndpointResolution(t *testing.T) {
	key := "test-key"
	cases := []struct {
		name        string
		record      ProviderRecord
		wantBase    string
		wantProto   Protocol
		wantAuthKey string
		wantAuth    bool
	}{
		{
			name: "messages marker",
			record: ProviderRecord{
				ID: "p", Name: "p", BaseURL: "https://models.example/v2/messages#",
				APIBackend: ProtocolMessages, Models: []string{"m"}, APIKey: &key,
				SupportsVision: map[string]bool{"m": false},
			},
			wantBase: "https://models.example/v2", wantProto: ProtocolMessages,
			wantAuthKey: "test-key", wantAuth: true,
		},
		{
			name: "chat completions marker",
			record: ProviderRecord{
				ID: "p", Name: "p", BaseURL: "https://models.example/v2/chat/completions#",
				APIBackend: ProtocolChatCompletions, Models: []string{"m"}, APIKey: &key,
				SupportsVision: map[string]bool{"m": false},
			},
			wantBase: "https://models.example/v2", wantProto: ProtocolChatCompletions,
			wantAuthKey: "test-key", wantAuth: true,
		},
		{
			name: "responses marker",
			record: ProviderRecord{
				ID: "p", Name: "p", BaseURL: "https://models.example/v2/responses#",
				APIBackend: ProtocolResponses, Models: []string{"m"}, APIKey: &key,
				SupportsVision: map[string]bool{"m": false},
			},
			wantBase: "https://models.example/v2", wantProto: ProtocolResponses,
			wantAuthKey: "test-key", wantAuth: true,
		},
		{
			name: "unauthenticated keeps explicit no-auth",
			record: ProviderRecord{
				ID: "local", Name: "local", BaseURL: "http://127.0.0.1:11434/v1/responses",
				APIBackend: ProtocolResponses, Models: []string{"m"},
				SupportsVision: map[string]bool{"m": false},
			},
			wantBase: "http://127.0.0.1:11434/v1", wantProto: ProtocolResponses,
			wantAuthKey: "", wantAuth: false,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			endpoint, err := tc.record.Endpoint()
			if err != nil {
				t.Fatalf("Endpoint() unexpected error: %v", err)
			}
			if endpoint.BaseURL != tc.wantBase {
				t.Errorf("BaseURL = %q, want %q", endpoint.BaseURL, tc.wantBase)
			}
			if endpoint.Protocol != tc.wantProto {
				t.Errorf("Protocol = %q, want %q", endpoint.Protocol, tc.wantProto)
			}
			if endpoint.APIKey != tc.wantAuthKey {
				t.Errorf("APIKey = %q, want %q", endpoint.APIKey, tc.wantAuthKey)
			}
			if endpoint.Authenticated != tc.wantAuth {
				t.Errorf("Authenticated = %v, want %v", endpoint.Authenticated, tc.wantAuth)
			}
		})
	}

	bad := ProviderRecord{
		ID: "p", Name: "p", BaseURL: "https://api.example/v2#",
		APIBackend: ProtocolChatCompletions, Models: []string{"m"},
		SupportsVision: map[string]bool{"m": false},
	}
	if _, err := bad.Endpoint(); err == nil {
		t.Error("marker URL without protocol suffix should fail endpoint resolution")
	}
}

// validProvidersFile is a minimal valid providers.json used as the base for
// tolerant-loading tests.
const validProvidersFile = `{
  "schema": "keencode/providers",
  "version": 1,
  "activeProviderId": "provider",
  "activeModelId": "test-model",
  "providers": [
    {
      "id": "provider",
      "name": "Provider",
      "baseUrl": "https://api.example.com/v1",
      "models": ["test-model"],
      "apiBackend": "responses",
      "apiKey": null,
      "contextWindows": {},
      "supportsVision": {"test-model": false}
    }
  ]
}`

// writeTempFile writes content to a fresh file inside dir and returns its
// path.
func writeTempFile(t *testing.T, dir, name, content string) string {
	t.Helper()
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatalf("write %s: %v", name, err)
	}
	return path
}

// TestMissingProvidersFileReturnsEmptyState ports the first half of
// providers.rs:2441-2458: a missing config yields the empty state and
// creates nothing on disk.
func TestMissingProvidersFileReturnsEmptyState(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "providers.json")
	state, warnings, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("LoadProvidersFromPath(missing) unexpected error: %v", err)
	}
	if len(warnings) != 0 {
		t.Errorf("warnings = %v, want empty", warnings)
	}
	if len(state.Providers) != 0 {
		t.Errorf("providers = %v, want empty", state.Providers)
	}
	if _, ok := state.ActiveProvider(); ok {
		t.Error("active provider should be unset")
	}
	if _, ok := state.ActiveModel(); ok {
		t.Error("active model should be unset")
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Errorf("load must not create %s (stat err = %v)", path, err)
	}
}

// TestProvidersSchemaRoundtrip ports the second half of providers.rs:2441:
// the first save must write the strict envelope and reload losslessly.
func TestProvidersSchemaRoundtrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "providers.json")
	if err := SaveProvidersToPath(path, DefaultProvidersState()); err != nil {
		t.Fatalf("SaveProvidersToPath(empty) unexpected error: %v", err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read saved config: %v", err)
	}
	for _, want := range []string{
		`"schema": "keencode/providers"`,
		`"version": 1`,
		`"activeProviderId": null`,
		`"activeModelId": null`,
		`"providers": []`,
	} {
		if !strings.Contains(string(data), want) {
			t.Errorf("saved envelope missing %s in:\n%s", want, data)
		}
	}
	state, warnings, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("reload unexpected error: %v", err)
	}
	if len(warnings) != 0 || len(state.Providers) != 0 {
		t.Errorf("reload = %v / %v, want empty state without warnings", warnings, state.Providers)
	}
}

// TestLoadProvidersIgnoresUnknownFields ports
// provider_config_ignores_unknown_fields_with_warnings
// (providers.rs:1628-1662) and
// provider_config_with_removed_fields_loads_and_is_usable
// (providers.rs:2483-2501): unknown or removed fields warn but never block,
// and loading must not rewrite the file.
func TestLoadProvidersIgnoresUnknownFields(t *testing.T) {
	dir := t.TempDir()
	original := `{
  "schema": "keencode/providers",
  "version": 1,
  "activeProviderId": "provider",
  "activeModelId": "test-model",
  "expiredTopLevelField": true,
  "providers": [
    {
      "id": "provider",
      "name": "Provider",
      "baseUrl": "https://api.example.com/v1",
      "models": ["test-model"],
      "apiBackend": "responses",
      "apiKey": "secret",
      "contextWindows": {},
      "removedProviderField": true,
      "supportsVision": {"test-model": false}
    }
  ]
}`
	path := writeTempFile(t, dir, "providers.json", original)

	state, warnings, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("tolerant load failed: %v", err)
	}
	if len(warnings) != 2 {
		t.Fatalf("warnings = %v, want exactly top-level and record warnings", warnings)
	}
	if !strings.Contains(warnings[0], "expiredTopLevelField") {
		t.Errorf("warnings[0] = %q, want top-level field name", warnings[0])
	}
	if !strings.Contains(warnings[1], "removedProviderField") {
		t.Errorf("warnings[1] = %q, want record field name", warnings[1])
	}
	if len(state.Providers) != 1 || state.Providers[0].Models[0] != "test-model" {
		t.Errorf("state = %+v, want the single provider intact", state.Providers)
	}
	if key, ok := state.Providers[0].AuthKey(); !ok || key != "secret" {
		t.Errorf("api key = %q/%v, want persisted secret", key, ok)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("re-read config: %v", err)
	}
	if string(data) != original {
		t.Error("loading must not rewrite the original file")
	}
}

// TestLoadProvidersRejectsCaseOnlyTypo ports
// provider_config_rejects_case_only_typo_fields (providers.rs:1667-1687):
// a case-only variant of a known field blocks loading.
func TestLoadProvidersRejectsCaseOnlyTypo(t *testing.T) {
	dir := t.TempDir()
	original := strings.Replace(validProvidersFile, `"apiKey": null`, `"apikey": "real-secret"`, 1)
	path := writeTempFile(t, dir, "providers.json", original)

	_, _, err := LoadProvidersFromPath(path)
	if err == nil {
		t.Fatal("case-only typo field should block loading")
	}
	if !strings.Contains(err.Error(), "apikey") {
		t.Errorf("error = %v, want it to name the offending field", err)
	}
	data, _ := os.ReadFile(path)
	if string(data) != original {
		t.Error("failed load must not rewrite the original file")
	}
}

// TestLoadProvidersRejectsCaseOnlyTypoTopLevel checks the envelope-level
// variant (providers.rs:840-844).
func TestLoadProvidersRejectsCaseOnlyTypoTopLevel(t *testing.T) {
	dir := t.TempDir()
	original := strings.Replace(validProvidersFile, `"activeProviderId"`, `"activeproviderid"`, 1)
	path := writeTempFile(t, dir, "providers.json", original)
	_, _, err := LoadProvidersFromPath(path)
	if err == nil || !strings.Contains(err.Error(), "activeproviderid") {
		t.Fatalf("err = %v, want a case-typo error naming activeproviderid", err)
	}
}

// TestLoadProvidersNormalizesStaleEntries ports
// provider_config_normalizes_stale_model_entries (providers.rs:1691-1731):
// stale per-model entries are dropped non-fatally and the active selection
// falls back.
func TestLoadProvidersNormalizesStaleEntries(t *testing.T) {
	dir := t.TempDir()
	path := writeTempFile(t, dir, "providers.json", `{
  "schema": "keencode/providers",
  "version": 1,
  "activeProviderId": "provider",
  "activeModelId": "removed-model",
  "providers": [
    {
      "id": "provider",
      "name": "Provider",
      "baseUrl": "https://api.example.com/v1",
      "models": ["test-model"],
      "apiBackend": "responses",
      "apiKey": null,
      "contextWindows": {"removed-model": 128000, "test-model": 99},
      "maxOutputTokens": {"removed-model": 128000, "test-model": 0},
      "supportsVision": {"removed-model": false}
    }
  ]
}`)

	state, warnings, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("tolerant load failed: %v", err)
	}
	record := state.Providers[0]
	if len(record.ContextWindows) != 0 {
		t.Errorf("context windows = %v, want empty (stale and out-of-range dropped)", record.ContextWindows)
	}
	if len(record.MaxOutputTokens) != 0 {
		t.Errorf("max output tokens = %v, want empty (zero and stale dropped)", record.MaxOutputTokens)
	}
	if vision, ok := record.SupportsVision["test-model"]; len(record.SupportsVision) != 1 || !ok || vision {
		t.Errorf("supports vision = %v, want {test-model: false}", record.SupportsVision)
	}
	if model, ok := state.ActiveModel(); !ok || model != "test-model" {
		t.Errorf("active model = %q/%v, want test-model", model, ok)
	}
	if len(warnings) == 0 {
		t.Error("normalization must leave recordable warnings")
	}
	for _, marker := range []string{"上下文窗口", "输出预算", "视觉能力", "当前模型"} {
		found := false
		for _, warning := range warnings {
			if strings.Contains(warning, marker) {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("warnings %v missing a note about %s", warnings, marker)
		}
	}
	if err := ValidateProvidersState(state); err != nil {
		t.Errorf("normalized state must pass validation: %v", err)
	}
}

// TestLoadProvidersEmptyStateSelection covers the empty-provider cleanup
// and the explicit null shape (providers.rs:1010-1020, 1788-1802).
func TestLoadProvidersEmptyStateSelection(t *testing.T) {
	dir := t.TempDir()

	// Explicit null selections with no providers are the canonical empty
	// shape and load cleanly.
	path := writeTempFile(t, dir, "empty.json", `{
  "schema": "keencode/providers",
  "version": 1,
  "activeProviderId": null,
  "activeModelId": null,
  "providers": []
}`)
	state, warnings, err := LoadProvidersFromPath(path)
	if err != nil || len(warnings) != 0 {
		t.Fatalf("explicit empty shape failed: %v / %v", err, warnings)
	}
	if len(state.Providers) != 0 {
		t.Errorf("providers = %v, want empty", state.Providers)
	}

	// Stale selections without providers are cleared with a warning.
	stale := writeTempFile(t, dir, "stale.json", `{
  "schema": "keencode/providers",
  "version": 1,
  "activeProviderId": "gone",
  "activeModelId": "gone-model",
  "providers": []
}`)
	state, warnings, err = LoadProvidersFromPath(stale)
	if err != nil {
		t.Fatalf("stale selection load failed: %v", err)
	}
	if _, ok := state.ActiveProvider(); ok {
		t.Error("active provider should be cleared")
	}
	if len(warnings) == 0 || !strings.Contains(warnings[0], "清除当前供应商") {
		t.Errorf("warnings = %v, want a cleanup note", warnings)
	}
}

// TestLoadProvidersRejectsStructuralErrors ports
// provider_config_still_rejects_structural_errors,
// provider_state_rejects_missing_active_model, and
// invalid_provider_config_is_rejected_without_replacement
// (providers.rs:1735-1762, 1765-1784, 2462-2479): structural errors fail
// closed and leave the file untouched.
func TestLoadProvidersRejectsStructuralErrors(t *testing.T) {
	cases := []struct {
		name    string
		content string
	}{
		{"not json", "not-json"},
		{"empty file", ""},
		{"version 0", `{"schema":"keencode/providers","version":0,"activeProviderId":null,"activeModelId":null,"providers":[]}`},
		{"other schema", `{"schema":"other/schema","version":1,"activeProviderId":null,"activeModelId":null,"providers":[]}`},
		{"missing schema", `{"version":1,"activeProviderId":null,"activeModelId":null,"providers":[]}`},
		{"missing activeModelId", `{"schema":"keencode/providers","version":1,"activeProviderId":"provider","providers":[{"id":"provider","name":"Provider","baseUrl":"https://api.example.com/v1","models":["test-model"],"apiBackend":"responses","apiKey":null,"contextWindows":{},"supportsVision":{"test-model":false}}]}`},
		{"providers null", `{"schema":"keencode/providers","version":1,"activeProviderId":null,"activeModelId":null,"providers":null}`},
		{"record not object", `{"schema":"keencode/providers","version":1,"activeProviderId":null,"activeModelId":null,"providers":[1]}`},
		{"missing api key key", `{"schema":"keencode/providers","version":1,"activeProviderId":"provider","activeModelId":"test-model","providers":[{"id":"provider","name":"Provider","baseUrl":"https://api.example.com/v1","models":["test-model"],"apiBackend":"responses","contextWindows":{},"supportsVision":{"test-model":false}}]}`},
		{"missing supports vision", `{"schema":"keencode/providers","version":1,"activeProviderId":"provider","activeModelId":"test-model","providers":[{"id":"provider","name":"Provider","baseUrl":"https://api.example.com/v1","models":["test-model"],"apiBackend":"responses","apiKey":null,"contextWindows":{}}]}`},
		{"bad url", `{"schema":"keencode/providers","version":1,"activeProviderId":"provider","activeModelId":"test-model","providers":[{"id":"provider","name":"Provider","baseUrl":"not-a-url","models":["test-model"],"apiBackend":"responses","apiKey":null,"contextWindows":{},"supportsVision":{"test-model":false}}]}`},
		{"empty models", `{"schema":"keencode/providers","version":1,"activeProviderId":"provider","activeModelId":"test-model","providers":[{"id":"provider","name":"Provider","baseUrl":"https://api.example.com/v1","models":[],"apiBackend":"responses","apiKey":null,"contextWindows":{},"supportsVision":{}}]}`},
		{"duplicate ids", `{"schema":"keencode/providers","version":1,"activeProviderId":"provider","activeModelId":"test-model","providers":[{"id":"provider","name":"Provider","baseUrl":"https://api.example.com/v1","models":["test-model"],"apiBackend":"responses","apiKey":null,"contextWindows":{},"supportsVision":{"test-model":false}},{"id":"provider","name":"Provider2","baseUrl":"https://api.example.com/v1","models":["test-model"],"apiBackend":"responses","apiKey":null,"contextWindows":{},"supportsVision":{"test-model":false}}]}`},
		{"unknown backend", strings.Replace(validProvidersFile, `"apiBackend": "responses"`, `"apiBackend": "graphql"`, 1)},
		{"padded api key", strings.Replace(validProvidersFile, `"apiKey": null`, `"apiKey": " secret"`, 1)},
		{"context window wrong type", strings.Replace(validProvidersFile, `"contextWindows": {}`, `"contextWindows": {"test-model": "big"}`, 1)},
		{"chat output field unknown", strings.Replace(validProvidersFile, `"contextWindows": {},`, `"contextWindows": {}, "chatOutputTokenField": "bogus",`, 1)},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			path := writeTempFile(t, dir, "providers.json", tc.content)
			_, _, err := LoadProvidersFromPath(path)
			if err == nil {
				t.Fatalf("invalid config accepted:\n%s", tc.content)
			}
			data, readErr := os.ReadFile(path)
			if readErr != nil {
				t.Fatalf("re-read config: %v", readErr)
			}
			if string(data) != tc.content {
				t.Error("failed load must not rewrite the original file")
			}
		})
	}
}

// TestSaveProvidersPreservesUnknownFields covers the ask requirement that
// saving must not destroy unrecognized fields, both at the top level and
// per record.
func TestSaveProvidersPreservesUnknownFields(t *testing.T) {
	dir := t.TempDir()
	path := writeTempFile(t, dir, "providers.json", `{
  "schema": "keencode/providers",
  "version": 1,
  "activeProviderId": "provider",
  "activeModelId": "test-model",
  "readTimeoutSeconds": 500,
  "legacyRootField": {"nested": true},
  "providers": [
    {
      "id": "provider",
      "name": "Provider",
      "baseUrl": "https://api.example.com/v1",
      "models": ["test-model"],
      "apiBackend": "responses",
      "apiKey": null,
      "contextWindows": {},
      "context1m": {"test-model": true},
      "supportsVision": {"test-model": false}
    }
  ]
}`)

	state, _, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("load failed: %v", err)
	}
	if err := SaveProvidersToPath(path, state); err != nil {
		t.Fatalf("save failed: %v", err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("re-read: %v", err)
	}
	for _, marker := range []string{`"readTimeoutSeconds"`, `"legacyRootField"`, `"context1m"`} {
		if !strings.Contains(string(data), marker) {
			t.Errorf("saved file lost unknown field %s:\n%s", marker, data)
		}
	}
	reloaded, warnings, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("reload failed: %v", err)
	}
	if len(reloaded.Providers) != 1 {
		t.Errorf("reloaded providers = %d, want 1", len(reloaded.Providers))
	}
	if len(warnings) == 0 {
		t.Error("reloaded file still carries unknown fields and must warn about them")
	}
	// A second save keeps the preservation stable.
	if err := SaveProvidersToPath(path, reloaded); err != nil {
		t.Fatalf("second save failed: %v", err)
	}
	data, _ = os.ReadFile(path)
	if !strings.Contains(string(data), `"context1m"`) {
		t.Error("second save lost the preserved unknown field")
	}
}

// TestSaveProvidersPreservesOtherRecordExtras verifies the ID-keyed
// attachment of record extras: editing a record keeps its unknown fields, a
// deleted record's extras go away with it, and untouched records are
// unaffected.
func TestSaveProvidersPreservesOtherRecordExtras(t *testing.T) {
	dir := t.TempDir()
	path := writeTempFile(t, dir, "providers.json", `{
  "schema": "keencode/providers",
  "version": 1,
  "activeProviderId": "a",
  "activeModelId": "m",
  "providers": [
    {
      "id": "a",
      "name": "A",
      "baseUrl": "https://a.example.com/v1",
      "models": ["m"],
      "apiBackend": "responses",
      "apiKey": null,
      "contextWindows": {},
      "legacyA": true,
      "supportsVision": {"m": false}
    },
    {
      "id": "b",
      "name": "B",
      "baseUrl": "https://b.example.com/v1",
      "models": ["m"],
      "apiBackend": "responses",
      "apiKey": null,
      "contextWindows": {},
      "legacyB": true,
      "supportsVision": {"m": false}
    }
  ]
}`)
	state, _, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("load failed: %v", err)
	}

	// Edit record "a" in place: its unknown field survives the edit.
	state.Providers[0].Name = "A renamed"
	if err := SaveProvidersToPath(path, state); err != nil {
		t.Fatalf("save after edit: %v", err)
	}
	data, _ := os.ReadFile(path)
	if !strings.Contains(string(data), `"legacyA"`) || !strings.Contains(string(data), `"legacyB"`) {
		t.Errorf("edited save lost unknown fields:\n%s", data)
	}

	// Delete record "b": its unknown field goes away with the record.
	state.Providers = state.Providers[:1]
	if err := SaveProvidersToPath(path, state); err != nil {
		t.Fatalf("save after delete: %v", err)
	}
	data, _ = os.ReadFile(path)
	if strings.Contains(string(data), `"legacyB"`) {
		t.Errorf("deleted record's unknown field survived:\n%s", data)
	}
	if !strings.Contains(string(data), `"legacyA"`) {
		t.Error("kept record lost its unknown field after the delete save")
	}
}

// TestSaveProvidersRefusesUnparseableExisting ensures the preservation
// guarantee never silently destroys an unreadable file.
func TestSaveProvidersRefusesUnparseableExisting(t *testing.T) {
	dir := t.TempDir()
	path := writeTempFile(t, dir, "providers.json", "{ broken")
	original, _ := os.ReadFile(path)
	err := SaveProvidersToPath(path, DefaultProvidersState())
	if err == nil {
		t.Fatal("save over unparseable file should fail")
	}
	data, _ := os.ReadFile(path)
	if string(data) != string(original) {
		t.Error("failed save must not touch the original file")
	}
}

// TestSaveProvidersRejectsInvalidStates is a table over states that must be
// rejected before any bytes are written.
func TestSaveProvidersRejectsInvalidStates(t *testing.T) {
	valid, err := NewProviderRecord("p", "P", "https://p.example.com/v1", ProtocolMessages, []string{"m"}, nil)
	if err != nil {
		t.Fatalf("build valid record: %v", err)
	}
	validProvider := valid
	activeProvider := "p"
	activeModel := "m"

	cases := []struct {
		name  string
		state ProvidersState
	}{
		{"bad id", ProvidersState{Providers: []ProviderRecord{{ID: "bad id!", Name: "P", BaseURL: "https://p.example.com/v1", APIBackend: ProtocolMessages, Models: []string{"m"}, SupportsVision: map[string]bool{"m": false}}}}},
		{"empty models", ProvidersState{Providers: []ProviderRecord{{ID: "p", Name: "P", BaseURL: "https://p.example.com/v1", APIBackend: ProtocolMessages, Models: []string{}, SupportsVision: map[string]bool{}}}}},
		{"non canonical url", ProvidersState{Providers: []ProviderRecord{{ID: "p", Name: "P", BaseURL: "https://p.example.com", APIBackend: ProtocolMessages, Models: []string{"m"}, SupportsVision: map[string]bool{"m": false}}}}},
		{"missing vision entry", ProvidersState{Providers: []ProviderRecord{{ID: "p", Name: "P", BaseURL: "https://p.example.com/v1", APIBackend: ProtocolMessages, Models: []string{"m"}, SupportsVision: map[string]bool{}}}}},
		{"bad chat field", ProvidersState{Providers: []ProviderRecord{{ID: "p", Name: "P", BaseURL: "https://p.example.com/v1", APIBackend: ProtocolMessages, Models: []string{"m"}, ChatOutputTokenField: "bogus", SupportsVision: map[string]bool{"m": false}}}}},
		{"active without model", ProvidersState{ActiveProviderID: &activeProvider, Providers: []ProviderRecord{valid}}},
		{"active model foreign", ProvidersState{ActiveProviderID: &activeProvider, ActiveModelID: strPtr("other"), Providers: []ProviderRecord{validProvider}}},
		{"selection without providers", ProvidersState{ActiveProviderID: &activeProvider, ActiveModelID: &activeModel}},
		{"nil active with providers", ProvidersState{Providers: []ProviderRecord{validProvider}}},
		{"nil active provider field", ProvidersState{ActiveModelID: &activeModel, Providers: []ProviderRecord{validProvider}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "providers.json")
			if err := SaveProvidersToPath(path, tc.state); err == nil {
				t.Fatalf("invalid state accepted: %+v", tc.state)
			}
			if _, err := os.Stat(path); !os.IsNotExist(err) {
				t.Error("rejected save must not create the file")
			}
		})
	}
}

// strPtr is a small helper for building states in tests.
func strPtr(value string) *string { return &value }

// TestProvidersOversizedAndNonRegularTargets ports
// oversized_and_non_file_provider_configs_are_rejected
// (providers.rs:2545-2558).
func TestProvidersOversizedAndNonRegularTargets(t *testing.T) {
	dir := t.TempDir()

	oversized := filepath.Join(dir, "oversized.json")
	original := strings.Repeat("x", maxConfigFileBytes+1)
	if err := os.WriteFile(oversized, []byte(original), 0o600); err != nil {
		t.Fatalf("write oversized: %v", err)
	}
	if _, _, err := LoadProvidersFromPath(oversized); err == nil {
		t.Error("oversized config should be rejected")
	}
	data, _ := os.ReadFile(oversized)
	if len(data) != len(original) {
		t.Error("oversized file must stay untouched")
	}

	nonFile := filepath.Join(dir, "directory.json")
	if err := os.Mkdir(nonFile, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if _, _, err := LoadProvidersFromPath(nonFile); err == nil {
		t.Error("directory target should be rejected on load")
	}
	if err := SaveProvidersToPath(nonFile, DefaultProvidersState()); err == nil {
		t.Error("directory target should be rejected on save")
	}
	if !nonFileIsDir(t, nonFile) {
		t.Error("rejected save must not replace the directory")
	}

	if runtime.GOOS == "windows" {
		t.Skip("symlink test requires unix")
	}
	link := filepath.Join(dir, "link.json")
	if err := os.Symlink(filepath.Join(dir, "missing-target"), link); err != nil {
		t.Fatalf("symlink: %v", err)
	}
	if _, _, err := LoadProvidersFromPath(link); err == nil {
		t.Error("symlink target should be rejected on load")
	}
	if err := SaveProvidersToPath(link, DefaultProvidersState()); err == nil {
		t.Error("symlink target should be rejected on save")
	}
	if linkType := linkMode(t, link); linkType&os.ModeSymlink == 0 {
		t.Error("rejected save must not replace the symlink")
	}
}

func nonFileIsDir(t *testing.T, path string) bool {
	t.Helper()
	info, err := os.Lstat(path)
	return err == nil && info.IsDir()
}

func linkMode(t *testing.T, path string) os.FileMode {
	t.Helper()
	info, err := os.Lstat(path)
	if err != nil {
		t.Fatalf("lstat %s: %v", path, err)
	}
	return info.Mode()
}

// TestLoadProvidersIncidentShape ports
// provider_config_with_removed_context1m_maps_to_runtime_registry
// (providers.rs:2506-2541): the real-world config that once broke startup
// (removed context1m/readTimeoutSeconds fields plus optional per-model
// maps) must load, and the active provider/model must resolve to a runtime
// endpoint — the precondition of the send-message path.
func TestLoadProvidersIncidentShape(t *testing.T) {
	dir := t.TempDir()
	path := writeTempFile(t, dir, "providers.json", `{"schema":"keencode/providers","version":1,
  "activeProviderId":"zcode","activeModelId":"glm-5.3-flash",
  "providers":[
    {"id":"zcode","name":"ZCode","baseUrl":"https://api.example.com/v1",
     "models":["glm-5.3-flash"],"apiBackend":"chat_completions",
     "apiKey":null,"contextWindows":{},
     "maxOutputTokens":{"glm-5.3-flash":131000},
     "chatOutputTokenField":"max_completion_tokens",
     "readTimeoutSeconds":500,
     "context1m":{},
     "supportsVision":{"glm-5.3-flash":false}},
    {"id":"router","name":"OpenRouter","baseUrl":"https://api.example.org/v1",
     "models":["union-alpha"],"apiBackend":"responses",
     "apiKey":"router-key","contextWindows":{"union-alpha":262144},
     "chatOutputTokenField":"max_tokens",
     "context1m":{"union-alpha":true},
     "supportsVision":{"union-alpha":true}}]}`)

	state, warnings, err := LoadProvidersFromPath(path)
	if err != nil {
		t.Fatalf("incident shape config must load: %v", err)
	}
	if len(warnings) == 0 {
		t.Error("removed fields must produce warnings")
	}
	if len(state.Providers) != 2 {
		t.Fatalf("providers = %d, want 2", len(state.Providers))
	}
	if model, _ := state.ActiveModel(); model != "glm-5.3-flash" {
		t.Errorf("active model = %q, want glm-5.3-flash", model)
	}
	record, _ := state.Provider("zcode")
	if budget := record.MaxOutputTokens["glm-5.3-flash"]; budget != 131000 {
		t.Errorf("maxOutputTokens = %d, want 131000", budget)
	}
	if record.ChatOutputTokenField != ChatOutputFieldMaxCompletionTokens {
		t.Errorf("chat field = %q, want max_completion_tokens", record.ChatOutputTokenField)
	}
	endpoint, err := record.Endpoint()
	if err != nil {
		t.Fatalf("active record must resolve to an endpoint: %v", err)
	}
	if endpoint.Protocol != ProtocolChatCompletions || endpoint.BaseURL != "https://api.example.com/v1" {
		t.Errorf("endpoint = %+v, want chat_completions at https://api.example.com/v1", endpoint)
	}
	router, _ := state.Provider("router")
	if router.ChatOutputTokenField != ChatOutputFieldMaxTokens {
		t.Errorf("router chat field = %q, want max_tokens", router.ChatOutputTokenField)
	}
	if window := router.ContextWindows["union-alpha"]; window != 262144 {
		t.Errorf("router window = %d, want 262144", window)
	}
}

// TestProvidersStateAccessors covers the explicit-null accessors used by
// callers.
func TestProvidersStateAccessors(t *testing.T) {
	state := DefaultProvidersState()
	if _, ok := state.ActiveProvider(); ok {
		t.Error("default state has no active provider")
	}
	state.ActiveProviderID = strPtr("p")
	state.ActiveModelID = strPtr("m")
	record, err := NewProviderRecord("p", "P", "https://p.example.com/v1", ProtocolChatCompletions, []string{"m"}, nil)
	if err != nil {
		t.Fatalf("NewProviderRecord: %v", err)
	}
	state.Providers = []ProviderRecord{record}
	got, ok := state.Provider("p")
	if !ok || got.ID != "p" {
		t.Fatalf("Provider(p) = %+v/%v", got, ok)
	}
	if key, ok := got.AuthKey(); ok {
		t.Errorf("AuthKey() = %q/%v, want explicit no-auth", key, ok)
	}
}
