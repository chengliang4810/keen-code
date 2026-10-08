package config

import (
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
)

const (
	// providersSchemaName is the fixed schema identifier of providers.json.
	providersSchemaName = "keencode/providers"
	// providersFormatVersion is the only supported format version.
	providersFormatVersion = 1
	// providersFileName is the file name under the data root.
	providersFileName = "providers.json"
)

// Known keys of the providers.json envelope (providers.rs:808-814). Any
// other top-level key is a removed or future field and is ignored with a
// warning; a key differing from a known one only by case is treated as a
// typo and blocks loading.
var providerFileKeys = []string{
	"schema",
	"version",
	"activeProviderId",
	"activeModelId",
	"providers",
}

// Known keys of one provider record (providers.rs:817-828) plus
// "reasoningEfforts". The Rust key list omits reasoningEfforts even though
// the struct persists it, so loading a config carrying it logs a spurious
// warning; this port deliberately treats the field as known.
var providerRecordKeys = []string{
	"id",
	"name",
	"baseUrl",
	"models",
	"apiBackend",
	"apiKey",
	"contextWindows",
	"maxOutputTokens",
	"chatOutputTokenField",
	"supportsVision",
	"reasoningEfforts",
}

// requiredRecordKeys must be explicitly present in every persisted record
// (serde has no default for them); apiKey may be null but the key itself
// must exist.
var requiredRecordKeys = []string{
	"id",
	"name",
	"baseUrl",
	"models",
	"apiBackend",
	"apiKey",
	"contextWindows",
	"supportsVision",
}

// LoadProvidersFromPath reads providers.json. A missing file yields the
// default empty state without creating anything on disk.
//
// Tolerant loading contract (providers.rs:773-1051):
//   - unknown or removed top-level and record fields are ignored and
//     reported as warnings;
//   - a key differing from a known field only by ASCII case is a suspected
//     typo (e.g. "apikey" would silently drop authentication) and blocks
//     loading;
//   - stale per-model entries (context windows, output budgets, vision
//     flags, reasoning levels pointing at removed models, out-of-range or
//     zero values) and stale active selections are dropped with warnings;
//   - structural errors — invalid JSON, schema/version mismatch, wrong field
//     types, missing required keys, duplicate IDs, non-canonical values —
//     fail closed.
func LoadProvidersFromPath(path string) (ProvidersState, []string, error) {
	data, exists, err := readRegularFileBounded(path, maxConfigFileBytes, "供应商配置")
	if err != nil {
		return ProvidersState{}, nil, err
	}
	if !exists {
		return DefaultProvidersState(), nil, nil
	}
	state, warnings, err := parseProvidersFile(data)
	if err != nil {
		return ProvidersState{}, nil, fmt.Errorf("供应商配置格式无效：%s：%w", path, err)
	}
	warnings = append(warnings, normalizeLoadedState(&state)...)
	if err := ValidateProvidersState(state); err != nil {
		return ProvidersState{}, nil, fmt.Errorf("供应商配置无效：%s：%w", path, err)
	}
	return state, warnings, nil
}

// SaveProvidersToPath atomically replaces providers.json after strict
// validation. Unknown fields found in the existing file are carried over
// verbatim: top-level extras are re-emitted next to the known envelope keys
// and per-record extras stay attached to the record ID — editing a record's
// known fields keeps its unknown fields, while deleting the record (or
// renaming its ID) drops them with it. An existing file that is not valid
// JSON is never overwritten — all save paths are gated on a readable file
// so no salvageable bytes are destroyed silently.
func SaveProvidersToPath(path string, state ProvidersState) error {
	state = canonicalProvidersState(state)
	if err := ValidateProvidersState(state); err != nil {
		return err
	}
	extras, err := readProviderExtras(path)
	if err != nil {
		return err
	}
	data, err := marshalProvidersFile(state, extras)
	if err != nil {
		return err
	}
	if int64(len(data)) > maxConfigFileBytes {
		return fmt.Errorf("供应商配置超过 %d 字节", maxConfigFileBytes)
	}
	return atomicWritePrivate(path, data)
}

// canonicalProvidersState returns a copy safe to marshal: nil slices/maps
// become empty JSON containers and an unset output token field becomes the
// default, matching what serde serializes for the Rust structs.
func canonicalProvidersState(state ProvidersState) ProvidersState {
	canonical := ProvidersState{
		ActiveProviderID: state.ActiveProviderID,
		ActiveModelID:    state.ActiveModelID,
		Providers:        make([]ProviderRecord, 0, len(state.Providers)),
	}
	for _, record := range state.Providers {
		record.ContextWindows = orEmptyMap(record.ContextWindows)
		record.MaxOutputTokens = orEmptyMap(record.MaxOutputTokens)
		record.ReasoningEfforts = orEmptyNestedMap(record.ReasoningEfforts)
		if record.SupportsVision == nil {
			record.SupportsVision = map[string]bool{}
		}
		if record.Models == nil {
			record.Models = []string{}
		}
		if record.ChatOutputTokenField == "" {
			record.ChatOutputTokenField = ChatOutputFieldMaxCompletionTokens
		}
		canonical.Providers = append(canonical.Providers, record)
	}
	return canonical
}

func orEmptyMap[V comparable](m map[string]V) map[string]V {
	if m == nil {
		return map[string]V{}
	}
	return m
}

func orEmptyNestedMap(m map[string][]string) map[string][]string {
	if m == nil {
		return map[string][]string{}
	}
	return m
}

// providerExtras holds the unrecognized fields of an existing file so a save
// can re-emit them instead of destroying them.
type providerExtras struct {
	topLevel map[string]json.RawMessage
	byID     map[string]map[string]json.RawMessage
}

// readProviderExtras extracts unknown fields from the existing providers
// file. A missing file yields no extras; a present-but-unparseable file is
// an error so the caller refuses to overwrite it.
func readProviderExtras(path string) (*providerExtras, error) {
	data, exists, err := readRegularFileBounded(path, maxConfigFileBytes, "供应商配置")
	if err != nil {
		return nil, err
	}
	if !exists {
		return nil, nil
	}
	var top map[string]json.RawMessage
	if err := json.Unmarshal(data, &top); err != nil {
		return nil, fmt.Errorf("供应商配置已存在但不是有效 JSON，拒绝覆盖以保护未知字段：%s", path)
	}
	extras := &providerExtras{
		topLevel: map[string]json.RawMessage{},
		byID:     map[string]map[string]json.RawMessage{},
	}
	for key, raw := range top {
		if containsString(providerFileKeys, key) {
			continue
		}
		extras.topLevel[key] = raw
	}
	var records []json.RawMessage
	if raw, ok := top["providers"]; ok {
		// A malformed providers value cannot be attributed per record; the
		// top-level extras are still preserved.
		_ = json.Unmarshal(raw, &records)
	}
	for _, recordRaw := range records {
		var record map[string]json.RawMessage
		if err := json.Unmarshal(recordRaw, &record); err != nil {
			continue
		}
		var id string
		if raw, ok := record["id"]; ok {
			_ = json.Unmarshal(raw, &id)
		}
		if id == "" {
			continue
		}
		fields := map[string]json.RawMessage{}
		for key, raw := range record {
			if containsString(providerRecordKeys, key) {
				continue
			}
			fields[key] = raw
		}
		if _, exists := extras.byID[id]; !exists {
			extras.byID[id] = fields
		}
	}
	return extras, nil
}

// marshalProvidersFile renders the full envelope: known keys in Rust
// declaration order, then preserved unknown keys sorted by name.
func marshalProvidersFile(state ProvidersState, extras *providerExtras) ([]byte, error) {
	envelope := newOrderedObject()
	if err := envelope.set("schema", providersSchemaName); err != nil {
		return nil, err
	}
	if err := envelope.set("version", providersFormatVersion); err != nil {
		return nil, err
	}
	if err := envelope.set("activeProviderId", state.ActiveProviderID); err != nil {
		return nil, err
	}
	if err := envelope.set("activeModelId", state.ActiveModelID); err != nil {
		return nil, err
	}
	records := make([]json.RawMessage, 0, len(state.Providers))
	for _, record := range state.Providers {
		raw, err := marshalProviderRecord(record, extras)
		if err != nil {
			return nil, err
		}
		records = append(records, raw)
	}
	if err := envelope.set("providers", records); err != nil {
		return nil, err
	}
	if extras != nil {
		for _, key := range sortedKeys(extras.topLevel) {
			envelope.setRaw(key, extras.topLevel[key])
		}
	}
	return envelope.bytes()
}

// marshalProviderRecord renders one record: known keys in Rust declaration
// order (providers.rs:48-74), then its preserved unknown keys sorted by name.
func marshalProviderRecord(record ProviderRecord, extras *providerExtras) (json.RawMessage, error) {
	object := newOrderedObject()
	var recordExtras map[string]json.RawMessage
	if extras != nil {
		recordExtras = extras.byID[record.ID]
	}
	set := func(key string, value any) error {
		return object.set(key, value)
	}
	if err := set("id", record.ID); err != nil {
		return nil, err
	}
	if err := set("name", record.Name); err != nil {
		return nil, err
	}
	if err := set("baseUrl", record.BaseURL); err != nil {
		return nil, err
	}
	if err := set("models", record.Models); err != nil {
		return nil, err
	}
	if err := set("apiBackend", string(record.APIBackend)); err != nil {
		return nil, err
	}
	if record.APIKey == nil {
		object.setRaw("apiKey", json.RawMessage("null"))
	} else if err := set("apiKey", *record.APIKey); err != nil {
		return nil, err
	}
	if err := set("contextWindows", record.ContextWindows); err != nil {
		return nil, err
	}
	if err := set("maxOutputTokens", record.MaxOutputTokens); err != nil {
		return nil, err
	}
	if err := set("chatOutputTokenField", string(record.ChatOutputTokenField)); err != nil {
		return nil, err
	}
	if err := set("supportsVision", record.SupportsVision); err != nil {
		return nil, err
	}
	if err := set("reasoningEfforts", record.ReasoningEfforts); err != nil {
		return nil, err
	}
	for _, key := range sortedKeys(recordExtras) {
		object.setRaw(key, recordExtras[key])
	}
	raw, err := object.bytes()
	if err != nil {
		return nil, err
	}
	return json.RawMessage(raw), nil
}

// parseProvidersFile parses the raw providers.json bytes into a validated
// state plus tolerant-loading warnings. It never touches the filesystem.
func parseProvidersFile(data []byte) (ProvidersState, []string, error) {
	var top map[string]json.RawMessage
	if err := json.Unmarshal(data, &top); err != nil {
		return ProvidersState{}, nil, err
	}
	var warnings []string
	typo, unknown := partitionUnknownKeys(sortedKeys(top), providerFileKeys)
	if len(typo) > 0 {
		return ProvidersState{}, nil, fmt.Errorf(
			"供应商配置字段 `%s` 与已知字段仅大小写不同，疑似拼写错误；请修正字段名后重试", typo[0])
	}
	if len(unknown) > 0 {
		warnings = append(warnings, fmt.Sprintf(
			"供应商配置包含未知或已移除字段，已忽略：%s", strings.Join(unknown, ", ")))
	}

	schema, err := requiredString(top, "schema")
	if err != nil {
		return ProvidersState{}, nil, err
	}
	version, err := requiredUint32(top, "version")
	if err != nil {
		return ProvidersState{}, nil, err
	}
	if schema != providersSchemaName || version != providersFormatVersion {
		return ProvidersState{}, nil, errors.New("供应商配置 schema 或版本不受支持")
	}

	state := ProvidersState{}
	if state.ActiveProviderID, err = requiredOptionalString(top, "activeProviderId"); err != nil {
		return ProvidersState{}, nil, err
	}
	if state.ActiveModelID, err = requiredOptionalString(top, "activeModelId"); err != nil {
		return ProvidersState{}, nil, err
	}
	recordsRaw, ok := top["providers"]
	if !ok {
		return ProvidersState{}, nil, errors.New("供应商配置缺少必填字段 providers")
	}
	if isJSONNull(recordsRaw) {
		return ProvidersState{}, nil, errors.New("供应商配置字段 providers 不能为 null")
	}
	var records []json.RawMessage
	if err := json.Unmarshal(recordsRaw, &records); err != nil {
		return ProvidersState{}, nil, fmt.Errorf("供应商配置字段 providers 必须是数组：%w", err)
	}
	state.Providers = make([]ProviderRecord, 0, len(records))
	for _, recordRaw := range records {
		record, recordWarnings, err := parseProviderRecord(recordRaw)
		if err != nil {
			return ProvidersState{}, nil, err
		}
		warnings = append(warnings, recordWarnings...)
		state.Providers = append(state.Providers, record)
	}
	return state, warnings, nil
}

// parseProviderRecord parses one record object: required keys must be
// present, types must match exactly (null is only allowed for apiKey), and
// unknown keys were already reported by the caller-side walk in
// parseProvidersFile via partitionUnknownKeys — record-level warnings are
// collected here together with the typed parse.
func parseProviderRecord(recordRaw json.RawMessage) (ProviderRecord, []string, error) {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(recordRaw, &fields); err != nil {
		return ProviderRecord{}, nil, fmt.Errorf("供应商记录必须是对象：%w", err)
	}
	keys := sortedKeys(fields)
	typo, unknown := partitionUnknownKeys(keys, providerRecordKeys)
	recordID := "<缺少 id>"
	if raw, ok := fields["id"]; ok {
		if value, err := decodeJSONString(raw); err == nil {
			recordID = value
		}
	}
	if len(typo) > 0 {
		return ProviderRecord{}, nil, fmt.Errorf(
			"供应商 %s 的字段 `%s` 与已知字段仅大小写不同，疑似拼写错误；请修正字段名后重试", recordID, typo[0])
	}
	var warnings []string
	if len(unknown) > 0 {
		warnings = append(warnings, fmt.Sprintf(
			"供应商 %s 包含未知或已移除字段，已忽略：%s", recordID, strings.Join(unknown, ", ")))
	}

	record := ProviderRecord{}
	for _, key := range requiredRecordKeys {
		if _, ok := fields[key]; !ok {
			return ProviderRecord{}, nil, fmt.Errorf("供应商 %s 缺少必填字段 %s", recordID, key)
		}
	}
	var err error
	if record.ID, err = requiredString(fields, "id"); err != nil {
		return ProviderRecord{}, nil, err
	}
	if record.Name, err = requiredString(fields, "name"); err != nil {
		return ProviderRecord{}, nil, err
	}
	if record.BaseURL, err = requiredString(fields, "baseUrl"); err != nil {
		return ProviderRecord{}, nil, err
	}
	backend, err := requiredString(fields, "apiBackend")
	if err != nil {
		return ProviderRecord{}, nil, err
	}
	record.APIBackend = Protocol(backend)
	if raw, ok := fields["apiKey"]; !ok {
		return ProviderRecord{}, nil, fmt.Errorf("供应商 %s 缺少必填字段 apiKey", record.ID)
	} else if record.APIKey, err = decodeOptionalJSONString(raw); err != nil {
		return ProviderRecord{}, nil, fmt.Errorf("供应商 %s 的字段 apiKey 必须是字符串或 null：%w", record.ID, err)
	}
	if err := decodeStringSlice(fields, "models", record.ID, &record.Models); err != nil {
		return ProviderRecord{}, nil, err
	}
	if err := decodeUint64Map(fields, "contextWindows", record.ID, &record.ContextWindows); err != nil {
		return ProviderRecord{}, nil, err
	}
	if err := decodeUint32Map(fields, "maxOutputTokens", record.ID, &record.MaxOutputTokens); err != nil {
		return ProviderRecord{}, nil, err
	}
	if err := decodeBoolMap(fields, "supportsVision", record.ID, &record.SupportsVision); err != nil {
		return ProviderRecord{}, nil, err
	}
	if err := decodeStringSliceMap(fields, "reasoningEfforts", record.ID, &record.ReasoningEfforts); err != nil {
		return ProviderRecord{}, nil, err
	}
	if raw, ok := fields["chatOutputTokenField"]; ok {
		value, err := decodeJSONString(raw)
		if err != nil {
			return ProviderRecord{}, nil, fmt.Errorf("供应商 %s 的字段 chatOutputTokenField 必须是字符串：%w", record.ID, err)
		}
		field := ChatOutputTokenField(value)
		if !field.valid() {
			return ProviderRecord{}, nil, fmt.Errorf("供应商 %s 的 chatOutputTokenField 不受支持：%s", record.ID, value)
		}
		record.ChatOutputTokenField = field
	} else {
		record.ChatOutputTokenField = ChatOutputFieldMaxCompletionTokens
	}
	return record, warnings, nil
}

// normalizeLoadedState converges a loaded state onto the current schema,
// dropping stale per-model entries and stale selections and returning one
// warning per dropped group (providers.rs:904-1051).
func normalizeLoadedState(state *ProvidersState) []string {
	var warnings []string
	for index := range state.Providers {
		record := &state.Providers[index]
		models := record.Models

		staleWindows := sortedMapKeysMissingFrom(record.ContextWindows, models)
		for _, model := range staleWindows {
			delete(record.ContextWindows, model)
		}
		if len(staleWindows) > 0 {
			warnings = append(warnings, fmt.Sprintf(
				"供应商 %s 的上下文窗口配置指向已删除模型，已忽略：%s", record.ID, strings.Join(staleWindows, ", ")))
		}
		outOfRange := make([]string, 0, len(record.ContextWindows))
		for _, model := range sortedMapKeys(record.ContextWindows) {
			if window := record.ContextWindows[model]; window < MinContextWindow || window > MaxContextWindow {
				outOfRange = append(outOfRange, model)
			}
		}
		for _, model := range outOfRange {
			delete(record.ContextWindows, model)
		}
		if len(outOfRange) > 0 {
			warnings = append(warnings, fmt.Sprintf(
				"供应商 %s 的上下文窗口超出合法范围，已忽略：%s", record.ID, strings.Join(outOfRange, ", ")))
		}

		staleOutput := make([]string, 0, len(record.MaxOutputTokens))
		for _, model := range sortedMapKeys(record.MaxOutputTokens) {
			if record.MaxOutputTokens[model] == 0 || !containsString(models, model) {
				staleOutput = append(staleOutput, model)
			}
		}
		for _, model := range staleOutput {
			delete(record.MaxOutputTokens, model)
		}
		if len(staleOutput) > 0 {
			warnings = append(warnings, fmt.Sprintf(
				"供应商 %s 的输出预算配置无效或指向已删除模型，已忽略：%s", record.ID, strings.Join(staleOutput, ", ")))
		}

		staleVision := sortedMapKeysMissingFrom(record.SupportsVision, models)
		for _, model := range staleVision {
			delete(record.SupportsVision, model)
		}
		staleEfforts := make([]string, 0, len(record.ReasoningEfforts))
		for _, model := range sortedMapKeys(record.ReasoningEfforts) {
			efforts := record.ReasoningEfforts[model]
			if !containsString(models, model) || !reasoningEffortsInOrder(efforts) {
				staleEfforts = append(staleEfforts, model)
			}
		}
		for _, model := range staleEfforts {
			delete(record.ReasoningEfforts, model)
		}
		if len(staleEfforts) > 0 {
			warnings = append(warnings, fmt.Sprintf(
				"供应商 %s 的推理档位配置无效或指向已删除模型，已忽略：%s", record.ID, strings.Join(staleEfforts, ", ")))
		}
		missingVision := make([]string, 0, len(models))
		for _, model := range models {
			if _, ok := record.SupportsVision[model]; !ok {
				missingVision = append(missingVision, model)
			}
		}
		for _, model := range missingVision {
			record.SupportsVision[model] = false
		}
		if len(staleVision) > 0 || len(missingVision) > 0 {
			var parts []string
			if len(staleVision) > 0 {
				parts = append(parts, fmt.Sprintf("已忽略 %s", strings.Join(staleVision, ", ")))
			}
			if len(missingVision) > 0 {
				parts = append(parts, fmt.Sprintf("按不支持补齐 %s", strings.Join(missingVision, ", ")))
			}
			warnings = append(warnings, fmt.Sprintf(
				"供应商 %s 的视觉能力配置已收敛：%s", record.ID, strings.Join(parts, "；")))
		}
	}

	_, hasProvider := state.ActiveProvider()
	_, hasModel := state.ActiveModel()
	switch {
	case len(state.Providers) == 0 && !hasProvider && !hasModel:
		// Already the canonical empty shape.
	case len(state.Providers) == 0:
		state.ActiveProviderID = nil
		state.ActiveModelID = nil
		warnings = append(warnings, "配置没有任何供应商，已清除当前供应商与模型选择")
	default:
		activeID, _ := state.ActiveProvider()
		record, found := state.Provider(activeID)
		if !found {
			fallback := state.Providers[0]
			warnings = append(warnings, fmt.Sprintf("当前供应商不存在，已回退为 %s", fallback.ID))
			state.ActiveProviderID = &fallback.ID
			if len(fallback.Models) > 0 {
				model := fallback.Models[0]
				state.ActiveModelID = &model
			} else {
				state.ActiveModelID = nil
			}
			break
		}
		model, hasModel := state.ActiveModel()
		if hasModel && containsString(record.Models, model) {
			break
		}
		fallbackLabel := "<无可用模型>"
		state.ActiveModelID = nil
		if len(record.Models) > 0 {
			fallback := record.Models[0]
			fallbackLabel = fallback
			state.ActiveModelID = &fallback
		}
		warnings = append(warnings, fmt.Sprintf(
			"当前模型不属于供应商 %s，已回退为 %s", record.ID, fallbackLabel))
	}
	return warnings
}

// sortedMapKeys returns the map keys in sorted order.
func sortedMapKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for key := range m {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

// sortedMapKeysMissingFrom returns the map keys that are not present in
// models, in sorted order.
func sortedMapKeysMissingFrom[V any](m map[string]V, models []string) []string {
	var missing []string
	for _, key := range sortedMapKeys(m) {
		if !containsString(models, key) {
			missing = append(missing, key)
		}
	}
	return missing
}

// ValidateProvidersState enforces the strict invariants of the persisted
// schema without auto-correction (providers.rs:1054-1111). Loading runs it
// after normalization; saving runs it before writing.
func ValidateProvidersState(state ProvidersState) error {
	seen := map[string]bool{}
	for _, record := range state.Providers {
		id, err := ValidateProviderID(record.ID)
		if err != nil {
			return err
		}
		if id != record.ID {
			return fmt.Errorf("供应商标识必须使用规范格式：%s", record.ID)
		}
		if seen[record.ID] {
			return fmt.Errorf("供应商标识重复：%s", record.ID)
		}
		seen[record.ID] = true
		if strings.TrimSpace(record.Name) == "" || strings.TrimSpace(record.Name) != record.Name {
			return fmt.Errorf("供应商 %s 的名称不能为空或包含首尾空白", record.ID)
		}
		canonicalURL, err := ValidateBaseURL(record.BaseURL)
		if err != nil {
			return err
		}
		if canonicalURL != record.BaseURL {
			return fmt.Errorf("供应商 %s 的 API 地址不是规范格式", record.ID)
		}
		if err := ValidateExactEndpoint(record.BaseURL, string(record.APIBackend)); err != nil {
			return fmt.Errorf("供应商 %s 的 API 地址不合规：%w", record.ID, err)
		}
		canonicalModels, err := NormalizeModels(record.Models)
		if err != nil {
			return err
		}
		if !equalStringSlice(canonicalModels, record.Models) {
			return fmt.Errorf("供应商 %s 的模型列表包含空项、重复项或首尾空白", record.ID)
		}
		if err := validateContextWindows(record.ContextWindows, canonicalModels); err != nil {
			return err
		}
		if err := validateMaxOutputTokens(record.MaxOutputTokens, canonicalModels); err != nil {
			return err
		}
		if err := validateSupportsVision(record.SupportsVision, canonicalModels); err != nil {
			return err
		}
		if err := validateReasoningEfforts(record.ReasoningEfforts, canonicalModels); err != nil {
			return err
		}
		canonicalBackend, err := ValidateAPIBackend(string(record.APIBackend))
		if err != nil {
			return err
		}
		if canonicalBackend != record.APIBackend {
			return fmt.Errorf("供应商 %s 的协议类型不是规范格式", record.ID)
		}
		if err := ValidateAPIKey(record.APIKey); err != nil {
			return err
		}
	}

	switch {
	case len(state.Providers) == 0 && state.ActiveProviderID == nil && state.ActiveModelID == nil:
		return nil
	case len(state.Providers) == 0:
		return errors.New("没有供应商时不能保存当前供应商或模型")
	case state.ActiveProviderID == nil || state.ActiveModelID == nil:
		return errors.New("存在供应商时必须同时保存当前供应商和当前模型")
	}
	activeID := *state.ActiveProviderID
	record, found := state.Provider(activeID)
	if !found {
		return fmt.Errorf("当前供应商不存在：%s", activeID)
	}
	if !containsString(record.Models, *state.ActiveModelID) {
		return fmt.Errorf("当前模型 %s 不属于供应商 %s", *state.ActiveModelID, activeID)
	}
	return nil
}

// validateContextWindows requires every entry to reference a configured
// model and stay inside [MinContextWindow, MaxContextWindow]
// (providers.rs:1119-1134).
func validateContextWindows(windows map[string]uint64, models []string) error {
	for _, model := range sortedMapKeys(windows) {
		window := windows[model]
		if !containsString(models, model) {
			return fmt.Errorf("上下文窗口配置的模型 %s 不在供应商模型列表中", model)
		}
		if window < MinContextWindow || window > MaxContextWindow {
			return fmt.Errorf("模型 %s 的上下文窗口 %d 超出合法范围（%d..%d）",
				model, window, MinContextWindow, MaxContextWindow)
		}
	}
	return nil
}

// validateMaxOutputTokens requires positive budgets on configured models
// (providers.rs:1137-1147).
func validateMaxOutputTokens(values map[string]uint32, models []string) error {
	for _, model := range sortedMapKeys(values) {
		if !containsString(models, model) || values[model] == 0 {
			return fmt.Errorf("模型 %s 的最大输出 Token 配置无效", model)
		}
	}
	return nil
}

// validateSupportsVision requires an explicit flag for every configured
// model and no foreign entries (providers.rs:1150-1165).
func validateSupportsVision(vision map[string]bool, models []string) error {
	for _, model := range models {
		if _, ok := vision[model]; !ok {
			return fmt.Errorf("模型 %s 缺少视觉能力配置", model)
		}
	}
	for _, model := range sortedMapKeys(vision) {
		if !containsString(models, model) {
			return fmt.Errorf("视觉能力配置的模型 %s 不在供应商模型列表中", model)
		}
	}
	return nil
}

// validateReasoningEfforts allows only runtime-executable levels in strict
// ascending order on configured models (providers.rs:1185-1198).
func validateReasoningEfforts(efforts map[string][]string, models []string) error {
	for _, model := range sortedMapKeys(efforts) {
		values := efforts[model]
		if !containsString(models, model) || len(values) > len(reasoningEffortLevels) {
			return fmt.Errorf("模型 %s 的推理档位配置无效", model)
		}
		if !reasoningEffortsInOrder(values) {
			return fmt.Errorf("模型 %s 的推理档位不受支持、重复或顺序错误", model)
		}
	}
	return nil
}

// partitionUnknownKeys splits keys into case-only typos of known fields and
// the remaining unknown fields (providers.rs:881-898). The input must be
// sorted so the reported first typo is deterministic.
func partitionUnknownKeys(keys []string, known []string) (typo []string, unknown []string) {
	for _, key := range keys {
		if containsString(known, key) {
			continue
		}
		matched := false
		for _, candidate := range known {
			if strings.EqualFold(candidate, key) {
				matched = true
				break
			}
		}
		if matched {
			typo = append(typo, key)
		} else {
			unknown = append(unknown, key)
		}
	}
	return typo, unknown
}

// equalStringSlice compares two string slices element-wise.
func equalStringSlice(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// requiredString decodes a required non-null string field.
func requiredString(fields map[string]json.RawMessage, key string) (string, error) {
	raw, ok := fields[key]
	if !ok {
		return "", fmt.Errorf("缺少必填字段 %s", key)
	}
	value, err := decodeJSONString(raw)
	if err != nil {
		return "", fmt.Errorf("字段 %s 必须是字符串：%w", key, err)
	}
	return value, nil
}

// requiredOptionalString decodes a required key whose value may be null.
func requiredOptionalString(fields map[string]json.RawMessage, key string) (*string, error) {
	raw, ok := fields[key]
	if !ok {
		return nil, fmt.Errorf("缺少必填字段 %s", key)
	}
	value, err := decodeOptionalJSONString(raw)
	if err != nil {
		return nil, fmt.Errorf("字段 %s 必须是字符串或 null：%w", key, err)
	}
	return value, nil
}

// requiredUint32 decodes a required unsigned 32-bit integer field.
func requiredUint32(fields map[string]json.RawMessage, key string) (uint32, error) {
	raw, ok := fields[key]
	if !ok {
		return 0, fmt.Errorf("缺少必填字段 %s", key)
	}
	if isJSONNull(raw) {
		return 0, fmt.Errorf("字段 %s 不能为 null", key)
	}
	var value uint32
	if err := json.Unmarshal(raw, &value); err != nil {
		return 0, fmt.Errorf("字段 %s 必须是无符号整数：%w", key, err)
	}
	return value, nil
}

// decodeStringSlice decodes an optional-typed []string field.
func decodeStringSlice(fields map[string]json.RawMessage, key, recordID string, into *[]string) error {
	raw, ok := fields[key]
	if !ok {
		return nil
	}
	if isJSONNull(raw) {
		return fmt.Errorf("供应商 %s 的字段 %s 不能为 null", recordID, key)
	}
	var value []string
	if err := json.Unmarshal(raw, &value); err != nil {
		return fmt.Errorf("供应商 %s 的字段 %s 必须是字符串数组：%w", recordID, key, err)
	}
	*into = value
	return nil
}

// decodeUint64Map decodes a required map[string]uint64 field.
func decodeUint64Map(fields map[string]json.RawMessage, key, recordID string, into *map[string]uint64) error {
	raw, ok := fields[key]
	if !ok {
		return nil
	}
	if isJSONNull(raw) {
		return fmt.Errorf("供应商 %s 的字段 %s 不能为 null", recordID, key)
	}
	var value map[string]uint64
	if err := json.Unmarshal(raw, &value); err != nil {
		return fmt.Errorf("供应商 %s 的字段 %s 必须是数字对象：%w", recordID, key, err)
	}
	*into = value
	return nil
}

// decodeUint32Map decodes an optional map[string]uint32 field.
func decodeUint32Map(fields map[string]json.RawMessage, key, recordID string, into *map[string]uint32) error {
	raw, ok := fields[key]
	if !ok {
		return nil
	}
	if isJSONNull(raw) {
		return fmt.Errorf("供应商 %s 的字段 %s 不能为 null", recordID, key)
	}
	var value map[string]uint32
	if err := json.Unmarshal(raw, &value); err != nil {
		return fmt.Errorf("供应商 %s 的字段 %s 必须是数字对象：%w", recordID, key, err)
	}
	*into = value
	return nil
}

// decodeBoolMap decodes a required map[string]bool field.
func decodeBoolMap(fields map[string]json.RawMessage, key, recordID string, into *map[string]bool) error {
	raw, ok := fields[key]
	if !ok {
		return nil
	}
	if isJSONNull(raw) {
		return fmt.Errorf("供应商 %s 的字段 %s 不能为 null", recordID, key)
	}
	var value map[string]bool
	if err := json.Unmarshal(raw, &value); err != nil {
		return fmt.Errorf("供应商 %s 的字段 %s 必须是布尔对象：%w", recordID, key, err)
	}
	*into = value
	return nil
}

// decodeStringSliceMap decodes an optional map[string][]string field.
func decodeStringSliceMap(fields map[string]json.RawMessage, key, recordID string, into *map[string][]string) error {
	raw, ok := fields[key]
	if !ok {
		return nil
	}
	if isJSONNull(raw) {
		return fmt.Errorf("供应商 %s 的字段 %s 不能为 null", recordID, key)
	}
	var value map[string][]string
	if err := json.Unmarshal(raw, &value); err != nil {
		return fmt.Errorf("供应商 %s 的字段 %s 必须是字符串数组对象：%w", recordID, key, err)
	}
	*into = value
	return nil
}
