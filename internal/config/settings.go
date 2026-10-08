package config

import (
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"unicode"
)

const (
	// settingsSchemaName is the fixed schema identifier of settings.json.
	settingsSchemaName = "keencode/app-settings"
	// settingsFormatVersion is the only supported format version.
	settingsFormatVersion = 1
	// settingsFileName is the file name under the data root.
	settingsFileName = "settings.json"
)

// Theme selects the appearance used by the native window. The string values
// match internal/ui/theme's Setting constants; config cannot import that
// package (the dependency direction points the other way), so the app layer
// maps between them.
type Theme string

const (
	// ThemeSystem follows the OS appearance.
	ThemeSystem Theme = "system"
	// ThemeLight pins the zai-light palette.
	ThemeLight Theme = "light"
	// ThemeDark pins the zai-dark palette, the product default.
	ThemeDark Theme = "dark"
)

// valid reports whether t is a persisted-able theme value.
func (t Theme) valid() bool {
	return t == ThemeSystem || t == ThemeLight || t == ThemeDark
}

// ToolPermissionPolicy is the app-level default for how side-effect tool
// calls are authorized. The values map onto the app layer's Authorize
// bridge ("ask") and the agent PlanGuard semantics ("read-only", mirroring
// PlanGuard::read_only in core/agent/src/plan_guard.rs).
type ToolPermissionPolicy string

const (
	// ToolPermissionAsk confirms every side-effect invocation through a
	// native dialog; it is the default.
	ToolPermissionAsk ToolPermissionPolicy = "ask"
	// ToolPermissionAllowAll auto-approves side-effect invocations.
	ToolPermissionAllowAll ToolPermissionPolicy = "allow-all"
	// ToolPermissionReadOnly rejects every state-changing invocation.
	ToolPermissionReadOnly ToolPermissionPolicy = "read-only"
)

// valid reports whether p is a persisted-able policy value.
func (p ToolPermissionPolicy) valid() bool {
	return p == ToolPermissionAsk || p == ToolPermissionAllowAll || p == ToolPermissionReadOnly
}

// ModelRef points at one model of one configured provider.
type ModelRef struct {
	// ProviderID is the provider identifier the model belongs to.
	ProviderID string `json:"providerId"`
	// ModelID is the model identifier within that provider.
	ModelID string `json:"modelId"`
}

// Settings is the app-level settings.json content. Missing fields fall back
// to DefaultSettings on load; unknown fields are ignored with a warning.
type Settings struct {
	// Theme is the appearance preference.
	Theme Theme `json:"theme"`
	// DefaultModel is the model preselected for new conversations; nil means
	// none (the user picks per conversation).
	DefaultModel *ModelRef `json:"defaultModel"`
	// WorkingDirectory is the default parent directory offered for new
	// conversations; empty means unset. When set it must be a canonical
	// absolute path.
	WorkingDirectory string `json:"workingDirectory"`
	// ToolPermissionPolicy is the default authorization policy for
	// side-effect tools.
	ToolPermissionPolicy ToolPermissionPolicy `json:"toolPermissionPolicy"`
}

// DefaultSettings returns the first-launch settings.
func DefaultSettings() Settings {
	return Settings{
		Theme:                ThemeDark,
		DefaultModel:         nil,
		WorkingDirectory:     "",
		ToolPermissionPolicy: ToolPermissionAsk,
	}
}

// SettingsLoad is the outcome of reading settings.json. Loading never fails:
// any problem yields DefaultSettings, an explanatory LoadError, and leaves
// the original file untouched (app_settings.rs:530-622).
type SettingsLoad struct {
	// Settings holds the parsed settings, or the defaults after any load
	// error.
	Settings Settings
	// Warnings lists non-fatal notes such as ignored unknown fields.
	Warnings []string
	// LoadError explains why the on-disk file could not be used; empty when
	// the settings were loaded (or the file was absent).
	LoadError string
}

// known settings envelope keys; anything else is ignored with a warning.
var settingsFileKeys = []string{
	"schema",
	"version",
	"theme",
	"defaultModel",
	"workingDirectory",
	"toolPermissionPolicy",
}

// LoadSettingsFromPath reads settings.json. Unlike providers.json this load
// is fail-soft: a corrupt file, unsupported schema, or invalid value falls
// back to the defaults and records the reason, because settings must never
// block startup (app_settings.rs:585-622).
func LoadSettingsFromPath(path string) SettingsLoad {
	data, exists, err := readRegularFileBounded(path, maxConfigFileBytes, "应用设置")
	if err != nil {
		return SettingsLoad{Settings: DefaultSettings(), LoadError: err.Error()}
	}
	if !exists {
		return SettingsLoad{Settings: DefaultSettings()}
	}
	loaded := parseSettingsFile(data)
	if loaded.LoadError != "" {
		loaded.LoadError = fmt.Sprintf("%s：%s", loaded.LoadError, path)
	}
	return loaded
}

// parseSettingsFile parses settings.json content into a SettingsLoad.
func parseSettingsFile(data []byte) SettingsLoad {
	failure := func(format string, args ...any) SettingsLoad {
		return SettingsLoad{Settings: DefaultSettings(), LoadError: fmt.Sprintf(format, args...)}
	}
	var top map[string]json.RawMessage
	if err := json.Unmarshal(data, &top); err != nil {
		return failure("设置不是有效 JSON：%v", err)
	}

	var warnings []string
	var unknown []string
	for _, key := range sortedKeys(top) {
		if !containsString(settingsFileKeys, key) {
			unknown = append(unknown, key)
		}
	}
	if len(unknown) > 0 {
		warnings = append(warnings, fmt.Sprintf(
			"设置文件包含未知或已移除字段，已忽略：%s", strings.Join(unknown, ", ")))
	}

	schema, ok := top["schema"]
	if !ok {
		return failure("设置文件结构无效：缺少必填字段 schema")
	}
	schemaValue, err := decodeJSONString(schema)
	if err != nil {
		return failure("设置文件结构无效：字段 schema 必须是字符串：%v", err)
	}
	version, ok := top["version"]
	if !ok {
		return failure("设置文件结构无效：缺少必填字段 version")
	}
	var versionValue uint32
	if err := json.Unmarshal(version, &versionValue); err != nil {
		return failure("设置文件结构无效：字段 version 必须是无符号整数：%v", err)
	}
	if schemaValue != settingsSchemaName || versionValue != settingsFormatVersion {
		return failure("应用设置 schema 或版本不受支持")
	}

	settings := DefaultSettings()
	if raw, ok := top["theme"]; ok {
		value, err := decodeJSONString(raw)
		if err != nil {
			return failure("设置文件结构无效：字段 theme 必须是字符串：%v", err)
		}
		settings.Theme = Theme(value)
	}
	if raw, ok := top["defaultModel"]; ok && !isJSONNull(raw) {
		var fields map[string]json.RawMessage
		if err := json.Unmarshal(raw, &fields); err != nil {
			return failure("设置文件结构无效：字段 defaultModel 必须是对象或 null：%v", err)
		}
		providerID, err := requiredString(fields, "providerId")
		if err != nil {
			return failure("设置文件结构无效：defaultModel.%v", err)
		}
		modelID, err := requiredString(fields, "modelId")
		if err != nil {
			return failure("设置文件结构无效：defaultModel.%v", err)
		}
		settings.DefaultModel = &ModelRef{ProviderID: providerID, ModelID: modelID}
	}
	if raw, ok := top["workingDirectory"]; ok {
		value, err := decodeJSONString(raw)
		if err != nil {
			return failure("设置文件结构无效：字段 workingDirectory 必须是字符串：%v", err)
		}
		settings.WorkingDirectory = value
	}
	if raw, ok := top["toolPermissionPolicy"]; ok {
		value, err := decodeJSONString(raw)
		if err != nil {
			return failure("设置文件结构无效：字段 toolPermissionPolicy 必须是字符串：%v", err)
		}
		settings.ToolPermissionPolicy = ToolPermissionPolicy(value)
	}
	if err := settings.Validate(); err != nil {
		return failure("%v", err)
	}
	return SettingsLoad{Settings: settings, Warnings: warnings}
}

// Validate checks the constraints that the type system cannot express.
// Loaded settings run it after parsing (a failure turns into LoadError);
// saving runs it before writing.
func (s Settings) Validate() error {
	if !s.Theme.valid() {
		return fmt.Errorf("主题设置不受支持：%s", s.Theme)
	}
	if !s.ToolPermissionPolicy.valid() {
		return fmt.Errorf("工具权限策略不受支持：%s", s.ToolPermissionPolicy)
	}
	if s.WorkingDirectory != "" {
		dir := s.WorkingDirectory
		if strings.TrimSpace(dir) != dir || containsControlChar(dir) ||
			!filepath.IsAbs(dir) || containsDotComponent(dir) {
			return errors.New("默认工作目录必须是规范的绝对路径")
		}
	}
	if s.DefaultModel != nil {
		if _, err := ValidateProviderID(s.DefaultModel.ProviderID); err != nil {
			return fmt.Errorf("默认模型的供应商标识无效：%w", err)
		}
		if strings.TrimSpace(s.DefaultModel.ModelID) == "" {
			return errors.New("默认模型标识不能为空")
		}
	}
	return nil
}

// containsControlChar reports whether s contains control characters.
func containsControlChar(s string) bool {
	for _, r := range s {
		if unicode.IsControl(r) {
			return true
		}
	}
	return false
}

// containsDotComponent reports whether any path component is "." or "..",
// mirroring the Component::CurDir | Component::ParentDir rejection of
// app_settings.rs:204-216.
func containsDotComponent(path string) bool {
	for _, part := range strings.FieldsFunc(path, func(r rune) bool {
		return r == '/' || r == '\\'
	}) {
		if part == "." || part == ".." {
			return true
		}
	}
	return false
}

// SaveSettingsToPath atomically replaces settings.json after strict
// validation, preserving unknown fields from the existing file. An existing
// file that is not valid JSON is never overwritten, mirroring app_settings
// set() which refuses to save over an unparseable file
// (app_settings.rs:438-444).
func SaveSettingsToPath(path string, settings Settings) error {
	if err := settings.Validate(); err != nil {
		return err
	}
	extras := map[string]json.RawMessage{}
	if data, exists, err := readRegularFileBounded(path, maxConfigFileBytes, "应用设置"); err != nil {
		return err
	} else if exists {
		var top map[string]json.RawMessage
		if err := json.Unmarshal(data, &top); err != nil {
			return fmt.Errorf("应用设置已存在但不是有效 JSON，拒绝覆盖以保护未知字段：%s", path)
		}
		for key, raw := range top {
			if !containsString(settingsFileKeys, key) {
				extras[key] = raw
			}
		}
	}
	data, err := marshalSettingsFile(settings, extras)
	if err != nil {
		return err
	}
	if int64(len(data)) > maxConfigFileBytes {
		return fmt.Errorf("应用设置超过 %d 字节", maxConfigFileBytes)
	}
	return atomicWritePrivate(path, data)
}

// marshalSettingsFile renders the envelope: known keys first, then preserved
// unknown keys sorted by name.
func marshalSettingsFile(settings Settings, extras map[string]json.RawMessage) ([]byte, error) {
	envelope := newOrderedObject()
	if err := envelope.set("schema", settingsSchemaName); err != nil {
		return nil, err
	}
	if err := envelope.set("version", settingsFormatVersion); err != nil {
		return nil, err
	}
	if err := envelope.set("theme", string(settings.Theme)); err != nil {
		return nil, err
	}
	if settings.DefaultModel == nil {
		envelope.setRaw("defaultModel", json.RawMessage("null"))
	} else {
		model := newOrderedObject()
		if err := model.set("providerId", settings.DefaultModel.ProviderID); err != nil {
			return nil, err
		}
		if err := model.set("modelId", settings.DefaultModel.ModelID); err != nil {
			return nil, err
		}
		raw, err := model.bytes()
		if err != nil {
			return nil, err
		}
		envelope.setRaw("defaultModel", json.RawMessage(raw))
	}
	if err := envelope.set("workingDirectory", settings.WorkingDirectory); err != nil {
		return nil, err
	}
	if err := envelope.set("toolPermissionPolicy", string(settings.ToolPermissionPolicy)); err != nil {
		return nil, err
	}
	for _, key := range sortedKeys(extras) {
		envelope.setRaw(key, extras[key])
	}
	return envelope.bytes()
}
