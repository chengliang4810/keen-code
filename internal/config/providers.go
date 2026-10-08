package config

import (
	"errors"
	"fmt"
	"net/url"
	"strings"
	"unicode"
	"unicode/utf8"
)

// Protocol identifies the provider-neutral wire protocol of one provider
// entry. The persisted values follow the Rust api_backend strings validated
// by validate_api_backend (apps/desktop/src/providers.rs:1353-1360); the
// protocol family names (Anthropic / OpenAI) only live here and in the
// internal/provider adapters, never in the agent runtime.
type Protocol string

const (
	// ProtocolMessages is the Anthropic Messages protocol (/v1/messages, SSE).
	ProtocolMessages Protocol = "messages"
	// ProtocolChatCompletions is the OpenAI Chat Completions protocol
	// (/chat/completions, SSE).
	ProtocolChatCompletions Protocol = "chat_completions"
	// ProtocolResponses is the OpenAI Responses protocol (/responses, SSE).
	ProtocolResponses Protocol = "responses"
)

// endpointPath returns the generation endpoint suffix each protocol appends
// to its base URL (apps/desktop/src/providers.rs:331-335).
func (p Protocol) endpointPath() string {
	switch p {
	case ProtocolMessages:
		return "/messages"
	case ProtocolChatCompletions:
		return "/chat/completions"
	case ProtocolResponses:
		return "/responses"
	}
	return ""
}

// valid reports whether p is one of the three supported protocols.
func (p Protocol) valid() bool {
	return p == ProtocolMessages || p == ProtocolChatCompletions || p == ProtocolResponses
}

// ValidateAPIBackend validates a raw protocol string and returns its
// canonical spelling. Trimming is allowed on user input; persisted values
// must already be canonical, which ValidateProvidersState enforces.
func ValidateAPIBackend(raw string) (Protocol, error) {
	switch trimmed := Protocol(strings.TrimSpace(raw)); trimmed {
	case ProtocolMessages, ProtocolChatCompletions, ProtocolResponses:
		return trimmed, nil
	}
	return "", fmt.Errorf("不支持的模型协议：%s", raw)
}

// ChatOutputTokenField selects which budget field Chat Completions requests
// carry (core/provider/src/config.rs:190-201). Unspecified persists as the
// standard field; gateways that only understand max_tokens opt out
// explicitly.
type ChatOutputTokenField string

const (
	// ChatOutputFieldMaxCompletionTokens is the standard Chat output budget
	// field including provider-counted reasoning tokens; it is the default.
	ChatOutputFieldMaxCompletionTokens ChatOutputTokenField = "max_completion_tokens"
	// ChatOutputFieldMaxTokens is the legacy budget field some compatible
	// gateways exclusively recognize.
	ChatOutputFieldMaxTokens ChatOutputTokenField = "max_tokens"
)

// valid reports whether f is a persisted-able output token field.
func (f ChatOutputTokenField) valid() bool {
	return f == ChatOutputFieldMaxCompletionTokens || f == ChatOutputFieldMaxTokens
}

// Reasoning effort levels accepted in reasoningEfforts, ordered from lowest
// to highest. A stored list must be a strictly ascending subsequence
// (providers.rs:1168-1183).
var reasoningEffortLevels = []string{"none", "minimal", "low", "medium", "high", "xhigh", "max"}

// reasoningEffortsInOrder reports whether values form a strictly ascending
// subsequence of reasoningEffortLevels. An empty list is valid.
func reasoningEffortsInOrder(values []string) bool {
	previous := -1
	for _, value := range values {
		index := -1
		for i, level := range reasoningEffortLevels {
			if level == value {
				index = i
				break
			}
		}
		if index < 0 || index <= previous {
			return false
		}
		previous = index
	}
	return true
}

// Bounds for manually configured per-model context windows
// (providers.rs:1114-1116).
const (
	// MinContextWindow is the lower bound for a manual context window (1K
	// tokens).
	MinContextWindow uint64 = 1_024
	// MaxContextWindow is the upper bound for a manual context window (10M
	// tokens).
	MaxContextWindow uint64 = 10_000_000
	// DefaultContextWindowTokens is the conservative fallback when a model
	// has no manual context window (providers.rs:38).
	DefaultContextWindowTokens uint64 = 200_000
	// DefaultMaxOutputTokens is the output budget fallback when a model has
	// no manual budget (providers.rs:303-309).
	DefaultMaxOutputTokens uint64 = 128_000
)

// maxProviderAPIKeyBytes caps one stored API key (providers.rs:25).
const maxProviderAPIKeyBytes = 16 * 1024

// ProviderRecord is one persisted custom provider (providers.rs:48-74). The
// zero value is not valid; build records through NewProviderRecord or load
// them from disk so the per-model invariants documented on each field hold.
type ProviderRecord struct {
	// ID is the stable provider identifier (1-64 chars, ASCII alphanumeric
	// plus . _ -, starting alphanumeric).
	ID string `json:"id"`
	// Name is the display name; trimmed and non-empty.
	Name string `json:"name"`
	// BaseURL is the model API base URL. A single trailing '#' marks an
	// exact request path: the value persists verbatim and the runtime must
	// not append /v1 or a protocol endpoint (providers.rs:1305-1327).
	BaseURL string `json:"baseUrl"`
	// Models lists the model IDs selectable for tasks; trimmed, unique,
	// non-empty.
	Models []string `json:"models"`
	// APIBackend is the wire protocol (Protocol values).
	APIBackend Protocol `json:"apiBackend"`
	// APIKey is the stored credential; nil explicitly means "no
	// authentication". The key must be explicitly present in JSON — either a
	// string or null (providers.rs:59-60).
	APIKey *string `json:"apiKey"`
	// ContextWindows maps model ID to a manually configured context window
	// in tokens; models without an entry fall back to
	// DefaultContextWindowTokens at runtime.
	ContextWindows map[string]uint64 `json:"contextWindows"`
	// MaxOutputTokens maps model ID to a positive output budget; models
	// without an entry fall back to DefaultMaxOutputTokens.
	MaxOutputTokens map[string]uint32 `json:"maxOutputTokens"`
	// ChatOutputTokenField selects the Chat output budget field; empty means
	// the default (ChatOutputFieldMaxCompletionTokens).
	ChatOutputTokenField ChatOutputTokenField `json:"chatOutputTokenField"`
	// SupportsVision maps every configured model ID to an explicit image
	// input flag; missing entries are rejected on load.
	SupportsVision map[string]bool `json:"supportsVision"`
	// ReasoningEfforts maps model ID to the explicitly unlocked reasoning
	// levels; absent entries fall back to the shared model catalog.
	ReasoningEfforts map[string][]string `json:"reasoningEfforts"`
}

// AuthKey returns the stored credential. The boolean is false when the
// record explicitly carries no authentication.
func (p ProviderRecord) AuthKey() (string, bool) {
	if p.APIKey == nil {
		return "", false
	}
	return *p.APIKey, true
}

// NewProviderRecord validates its input and fills the per-model defaults the
// persistence format requires: every model gets an explicit vision entry of
// false, empty per-model maps, and the standard output token field.
func NewProviderRecord(id, name, baseURL string, backend Protocol, models []string, apiKey *string) (ProviderRecord, error) {
	canonicalID, err := ValidateProviderID(id)
	if err != nil {
		return ProviderRecord{}, err
	}
	if strings.TrimSpace(name) == "" || strings.TrimSpace(name) != name {
		return ProviderRecord{}, fmt.Errorf("供应商 %s 的名称不能为空或包含首尾空白", canonicalID)
	}
	canonicalURL, err := ValidateBaseURL(baseURL)
	if err != nil {
		return ProviderRecord{}, err
	}
	canonicalBackend, err := ValidateAPIBackend(string(backend))
	if err != nil {
		return ProviderRecord{}, err
	}
	if err := ValidateExactEndpoint(canonicalURL, string(canonicalBackend)); err != nil {
		return ProviderRecord{}, err
	}
	canonicalModels, err := NormalizeModels(models)
	if err != nil {
		return ProviderRecord{}, err
	}
	if apiKey != nil {
		if err := ValidateSecret(*apiKey); err != nil {
			return ProviderRecord{}, err
		}
	}
	vision := make(map[string]bool, len(canonicalModels))
	for _, model := range canonicalModels {
		vision[model] = false
	}
	return ProviderRecord{
		ID:                   canonicalID,
		Name:                 name,
		BaseURL:              canonicalURL,
		Models:               canonicalModels,
		APIBackend:           canonicalBackend,
		APIKey:               apiKey,
		ContextWindows:       map[string]uint64{},
		MaxOutputTokens:      map[string]uint32{},
		ChatOutputTokenField: ChatOutputFieldMaxCompletionTokens,
		SupportsVision:       vision,
		ReasoningEfforts:     map[string][]string{},
	}, nil
}

// ProvidersState is the full persisted providers.json state
// (providers.rs:80-91). Both active IDs follow the "explicitly present, may
// be null" rule: nil means no selection and persists as null.
type ProvidersState struct {
	// ActiveProviderID is the currently selected provider; nil when unset.
	ActiveProviderID *string `json:"activeProviderId"`
	// ActiveModelID is the model actually handed to the agent runtime; nil
	// when unset.
	ActiveModelID *string `json:"activeModelId"`
	// Providers lists all saved provider records in persistence order.
	Providers []ProviderRecord `json:"providers"`
}

// DefaultProvidersState returns the empty state used when the file is
// missing.
func DefaultProvidersState() ProvidersState {
	return ProvidersState{Providers: []ProviderRecord{}}
}

// ActiveProvider returns the selected provider ID. False means no provider
// is selected.
func (s ProvidersState) ActiveProvider() (string, bool) {
	if s.ActiveProviderID == nil {
		return "", false
	}
	return *s.ActiveProviderID, true
}

// ActiveModel returns the selected model ID. False means no model is
// selected.
func (s ProvidersState) ActiveModel() (string, bool) {
	if s.ActiveModelID == nil {
		return "", false
	}
	return *s.ActiveModelID, true
}

// Provider looks up a record by ID.
func (s ProvidersState) Provider(id string) (ProviderRecord, bool) {
	for _, record := range s.Providers {
		if record.ID == id {
			return record, true
		}
	}
	return ProviderRecord{}, false
}

// ValidateProviderID validates a stable provider identifier
// (providers.rs:1285-1301) and returns its canonical (trimmed) form.
func ValidateProviderID(raw string) (string, error) {
	id := strings.TrimSpace(raw)
	if id == "" || len(id) > 64 {
		return "", errors.New("供应商标识长度必须为 1 到 64 个字符")
	}
	first, _ := utf8.DecodeRuneInString(id)
	if !isASCIIAlphaNumeric(first) {
		return "", errors.New("供应商标识只能使用字母、数字、点、下划线和短横线")
	}
	for _, r := range id {
		if !isASCIIAlphaNumeric(r) && !strings.ContainsRune("._-", r) {
			return "", errors.New("供应商标识只能使用字母、数字、点、下划线和短横线")
		}
	}
	return id, nil
}

// isASCIIAlphaNumeric reports whether r is an ASCII letter or digit.
func isASCIIAlphaNumeric(r rune) bool {
	return r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9'
}

// ValidateBaseURL validates and canonicalizes a model API base URL
// (providers.rs:1307-1327):
//
//   - one trailing '#' marks an exact request path; the marker is preserved
//     in the canonical value and the runtime must not append /v1 or a
//     protocol endpoint;
//   - a named fragment like #section is rejected;
//   - an exact-path marker on an empty path is rejected;
//   - a bare domain without '#' gains the standard /v1 path; an explicit
//     custom path is kept;
//   - only http/https URLs with a host are accepted.
func ValidateBaseURL(raw string) (string, error) {
	value := strings.TrimRight(strings.TrimSpace(raw), "/")
	if value == "" {
		return "", errors.New("模型 API 地址无效")
	}
	exactMarker := strings.HasSuffix(value, "#")
	body := strings.TrimSuffix(value, "#")
	parsed, err := url.Parse(body)
	if err != nil || parsed.Scheme == "" {
		return "", errors.New("模型 API 地址无效")
	}
	// The Rust url crate lowercases scheme and host during canonicalization;
	// mirror that so canonical-equality checks behave the same way.
	parsed.Scheme = strings.ToLower(parsed.Scheme)
	parsed.Host = strings.ToLower(parsed.Host)
	if (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.Host == "" {
		return "", errors.New("模型 API 地址必须是有效的 http 或 https 地址")
	}
	if parsed.Fragment != "" {
		return "", errors.New("模型 API 地址不支持 # 片段，# 仅可作为末尾的完整路径标记")
	}
	if exactMarker {
		if parsed.Path == "" || parsed.Path == "/" {
			return "", errors.New("以 # 结尾的地址必须包含完整的请求路径")
		}
	} else if parsed.Path == "" || parsed.Path == "/" {
		parsed.Path = "/v1"
	}
	canonical := strings.TrimRight(parsed.String(), "/")
	if exactMarker {
		canonical += "#"
	}
	return canonical, nil
}

// ValidateExactEndpoint enforces that a '#' exact-path URL ends with the
// generation endpoint of the chosen protocol (providers.rs:1333-1350);
// otherwise the runtime would append a path and hit the wrong address.
// Non-marker URLs are always accepted.
func ValidateExactEndpoint(baseURL string, apiBackend string) error {
	if !strings.HasSuffix(baseURL, "#") {
		return nil
	}
	backend, err := ValidateAPIBackend(apiBackend)
	if err != nil {
		return err
	}
	suffix := backend.endpointPath()
	trimmed := strings.TrimRight(strings.TrimSuffix(baseURL, "#"), "/")
	if !strings.HasSuffix(trimmed, suffix) {
		return fmt.Errorf("以 # 结尾的完整路径地址必须以 %s 结尾", suffix)
	}
	return nil
}

// ValidateSecret validates a stored credential without trimming or repairing
// the input (providers.rs:1243-1257).
func ValidateSecret(secret string) error {
	if secret == "" {
		return errors.New("API Key 不能为空")
	}
	if strings.TrimSpace(secret) != secret {
		return errors.New("API Key 不能包含首尾空白")
	}
	for _, r := range secret {
		if unicode.IsControl(r) {
			return errors.New("API Key 不能包含控制字符")
		}
	}
	if len(secret) > maxProviderAPIKeyBytes {
		return errors.New("API Key 超过大小限制")
	}
	return nil
}

// ValidateAPIKey validates an optional credential; nil explicitly keeps "no
// authentication" (providers.rs:1260-1265).
func ValidateAPIKey(apiKey *string) error {
	if apiKey == nil {
		return nil
	}
	return ValidateSecret(*apiKey)
}

// NormalizeModels trims, de-duplicates, and keeps the order of a model list,
// rejecting control characters and an empty result
// (providers.rs:1201-1217).
func NormalizeModels(models []string) ([]string, error) {
	normalized := make([]string, 0, len(models))
	for _, model := range models {
		trimmed := strings.TrimSpace(model)
		if trimmed == "" || containsString(normalized, trimmed) {
			continue
		}
		for _, r := range trimmed {
			if unicode.IsControl(r) {
				return nil, errors.New("模型标识不能包含控制字符")
			}
		}
		normalized = append(normalized, trimmed)
	}
	if len(normalized) == 0 {
		return nil, errors.New("至少需要添加一个模型")
	}
	return normalized, nil
}

// Endpoint is the resolved runtime endpoint of one provider record: the '#'
// marker and the protocol endpoint suffix are stripped so an adapter can
// append its own generation endpoint exactly once. This mirrors the
// runtime_provider_base_url mapping (providers.rs:319-341); the app layer
// maps it onto the internal/provider adapters.
type Endpoint struct {
	// Protocol is the wire protocol of the endpoint.
	Protocol Protocol
	// BaseURL is the request base without trailing '#', trailing slashes,
	// and without the protocol endpoint suffix.
	BaseURL string
	// APIKey is the credential to send; empty when Authenticated is false.
	APIKey string
	// Authenticated reports whether the provider carries a credential.
	Authenticated bool
}

// Endpoint validates the record's URL/protocol pair and resolves the runtime
// endpoint. A stored API key is re-validated here so a mapped endpoint can
// never carry a malformed secret.
func (p ProviderRecord) Endpoint() (Endpoint, error) {
	backend, err := ValidateAPIBackend(string(p.APIBackend))
	if err != nil {
		return Endpoint{}, err
	}
	if err := ValidateExactEndpoint(p.BaseURL, string(backend)); err != nil {
		return Endpoint{}, err
	}
	baseURL, err := ValidateBaseURL(p.BaseURL)
	if err != nil {
		return Endpoint{}, err
	}
	withoutMarker := strings.TrimRight(strings.TrimSuffix(baseURL, "#"), "/")
	stripped := strings.TrimSuffix(withoutMarker, backend.endpointPath())
	endpoint := Endpoint{
		Protocol: backend,
		BaseURL:  strings.TrimRight(stripped, "/"),
	}
	if key, ok := p.AuthKey(); ok {
		if err := ValidateSecret(key); err != nil {
			return Endpoint{}, err
		}
		endpoint.APIKey = key
		endpoint.Authenticated = true
	}
	return endpoint, nil
}
