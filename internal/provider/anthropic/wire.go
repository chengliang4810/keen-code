package anthropic

import (
	"bytes"
	"encoding/json"

	"keencode/internal/model"
)

// Stable kind names of the adapter-managed opaque reasoning continuation
// states (messages.rs:13-14). They travel inside the opaque continuation
// string of EventReasoningContinuation / model.ReasoningBlock.Signature and
// select the wire block shape when history is replayed in a later request.
const (
	// signatureStateKind wraps a verifiable thinking signature.
	signatureStateKind = "messages-thinking-signature-v1"
	// redactedStateKind wraps provider-encrypted or hidden thinking data.
	redactedStateKind = "messages-redacted-thinking-v1"
)

// anthropicVersion is the required Messages API version header
// (client.rs:577).
const anthropicVersion = "2023-06-01"

// defaultMaxTokens is the wire max_tokens used when the unified request does
// not set one: Anthropic requires the field, 8192 covers every Claude 3.5+
// output ceiling, and it halves the truncation gap against Chat/Responses
// endpoints that default by omission (messages.rs:150-156).
const defaultMaxTokens = 8192

// continuationEnvelope is the JSON envelope serialized into the opaque
// continuation string. The Go v1 model carries one opaque signature string;
// the envelope keeps the Rust kind+data pairs distinguishable when history is
// echoed back.
type continuationEnvelope struct {
	Kind string          `json:"kind"`
	Data json.RawMessage `json:"data"`
}

// encodeContinuation serializes an opaque reasoning state into the opaque
// string form carried by the neutral model layer.
func encodeContinuation(kind string, data json.RawMessage) (string, error) {
	payload, err := json.Marshal(continuationEnvelope{Kind: kind, Data: data})
	if err != nil {
		return "", protocolError("推理续传状态无法编码：%v", err)
	}
	return string(payload), nil
}

// encodeSignatureContinuation wraps a plain thinking signature string.
func encodeSignatureContinuation(signature string) (string, error) {
	return encodeContinuation(signatureStateKind, json.RawMessage(quoteJSONString(signature)))
}

// quoteJSONString renders text as a JSON string literal.
func quoteJSONString(text string) []byte {
	payload, _ := json.Marshal(text)
	return payload
}

// decodeContinuationEnvelope parses the opaque continuation string and fails
// closed on anything this adapter cannot interpret
// (messages.rs:802-822 encode_assistant_block match arms).
func decodeContinuationEnvelope(signature string) (kind string, data json.RawMessage, err error) {
	var envelope continuationEnvelope
	if jsonErr := json.Unmarshal([]byte(signature), &envelope); jsonErr != nil || envelope.Kind == "" || len(envelope.Data) == 0 {
		return "", nil, invalidRequest("Messages 无法解释推理续传状态 %s", truncateForDisplay(signature))
	}
	return envelope.Kind, envelope.Data, nil
}

// truncateForDisplay bounds opaque state echoes inside error messages.
func truncateForDisplay(text string) string {
	const limit = 64
	if len(text) <= limit {
		return text
	}
	return boundedUTF8Prefix(text, limit) + "…"
}

// encodeRequest converts the provider-neutral request into the Messages API
// JSON body (messages.rs:84-256). streaming only toggles the "stream" field.
func (a *Adapter) encodeRequest(req *model.ModelRequest, streaming bool) (json.RawMessage, error) {
	if err := req.Validate(); err != nil {
		return nil, err
	}
	var system []map[string]any
	messages := []map[string]any{}

	for i := range req.Messages {
		message := &req.Messages[i]
		switch message.Role {
		case model.RoleSystem, model.RoleDeveloper:
			for _, block := range message.Content {
				text, ok := block.(model.TextBlock)
				if !ok {
					return nil, invalidRequest("Messages 的系统和开发消息只允许文本内容")
				}
				system = append(system, map[string]any{"type": "text", "text": text.Text})
			}
		case model.RoleUser:
			content := make([]map[string]any, 0, len(message.Content))
			for _, block := range message.Content {
				encoded, err := encodeUserBlock(block)
				if err != nil {
					return nil, err
				}
				content = append(content, encoded)
			}
			appendMessage(&messages, "user", content)
		case model.RoleAssistant:
			content := []map[string]any{}
			for _, block := range message.Content {
				encoded, err := a.encodeAssistantBlock(block)
				if err != nil {
					return nil, err
				}
				if encoded != nil {
					content = append(content, encoded)
				}
			}
			if len(content) > 0 {
				appendMessage(&messages, "assistant", content)
			}
		default:
			return nil, invalidRequest("Messages 消息角色 %q 不受支持", string(message.Role))
		}
	}

	if len(messages) == 0 {
		return nil, invalidRequest("Messages 请求至少需要一条用户、工具或 assistant 消息")
	}

	// The Anthropic prompt cache ladder depends on a session-frozen prefix:
	// request = [frozen system blocks…] + [transcript history…] + [trailing
	// dynamic is_meta user message], with a stable tool array, so the prefix
	// stays byte-identical across turns and breakpoints can hit.
	if a.promptCaching {
		applyUserMessageCacheLadder(messages)
	}

	body := map[string]any{}
	body["model"] = req.Model
	body["messages"] = messages
	body["stream"] = streaming
	maxTokens := defaultMaxTokens
	if req.MaxTokens > 0 {
		maxTokens = req.MaxTokens
	}
	body["max_tokens"] = maxTokens

	// The cached prefix order is tools → system → messages: when system is
	// present its final-block breakpoint already covers the whole tools
	// array, so a tools breakpoint would be a strict subset.
	systemPresent := len(system) > 0
	if systemPresent {
		if a.promptCaching {
			addEphemeralCacheControl(system[len(system)-1])
		}
		body["system"] = system
	}
	if len(req.Tools) > 0 {
		tools := make([]map[string]any, 0, len(req.Tools))
		for i := range req.Tools {
			tool := req.Tools[i]
			var schema json.RawMessage
			if len(tool.InputSchema) > 0 {
				schema = tool.InputSchema
			} else {
				schema = json.RawMessage("{}")
			}
			tools = append(tools, map[string]any{
				"name":         tool.Name,
				"description":  tool.Description,
				"input_schema": schema,
			})
		}
		if a.promptCaching && !systemPresent {
			// Only when system is empty does the last tool carry the fallback
			// cache breakpoint; with a system block present the ladder would
			// exceed the hard Anthropic cap of 4 breakpoints and 400.
			addEphemeralCacheControl(tools[len(tools)-1])
		}
		body["tools"] = tools
		body["tool_choice"] = encodeToolChoice(req.ToolChoice)
	} else if req.ToolChoice.Mode != "" && req.ToolChoice.Mode != model.ToolChoiceAuto && req.ToolChoice.Mode != model.ToolChoiceNone {
		// The zero-value Mode "" means auto (internal/model request.go), so
		// only explicit required/tool choices demand a tool list.
		return nil, invalidRequest("Messages 工具选择要求非空工具列表")
	}
	if req.Reasoning != nil {
		budget := reasoningBudget(req.Reasoning.Effort)
		// Validation and the wire body share the same budget; when no
		// explicit ceiling is provided the final answer keeps room beyond the
		// thinking budget (messages.rs:199-220, tests.rs:1605-1621).
		if req.MaxTokens <= 0 {
			maxTokens = budget + 4096
		}
		if budget < 1024 {
			return nil, invalidRequest("Messages 推理 Token 预算至少为 1024")
		}
		if budget >= maxTokens {
			return nil, invalidRequest("Messages 推理 Token 预算必须小于最大输出 Token")
		}
		body["thinking"] = map[string]any{"type": "enabled", "budget_tokens": budget}
		body["max_tokens"] = maxTokens
	}
	if req.Temperature != nil {
		body["temperature"] = *req.Temperature
	}
	payload, err := json.Marshal(body)
	if err != nil {
		return nil, invalidRequest("Messages 请求正文无法编码：%v", err)
	}
	return payload, nil
}

// appendMessage merges a wire message into the last one when the roles match
// (messages.rs:688-698 append_message).
func appendMessage(messages *[]map[string]any, role string, content []map[string]any) {
	if len(*messages) > 0 {
		last := (*messages)[len(*messages)-1]
		if last["role"] == role {
			last["content"] = append(last["content"].([]map[string]any), content...)
			return
		}
	}
	*messages = append(*messages, map[string]any{"role": role, "content": content})
}

// addEphemeralCacheControl attaches an Anthropic ephemeral cache breakpoint to
// one wire block or tool object (messages.rs:701-705).
func addEphemeralCacheControl(block map[string]any) {
	block["cache_control"] = map[string]any{"type": "ephemeral"}
}

// isCacheableUserBlock reports whether a wire user block can carry a cache
// breakpoint: non-empty text and tool_result blocks qualify; empty text does
// not (messages.rs:710-719).
func isCacheableUserBlock(block map[string]any) bool {
	if block["type"] != "text" && block["type"] != "tool_result" {
		return false
	}
	if block["type"] == "text" {
		text, _ := block["text"].(string)
		return text != ""
	}
	return true
}

// applyUserMessageCacheLadder marks ephemeral breakpoints onto the first,
// second-to-last (when three or more user messages exist), and last wire user
// messages. When a target has no cacheable block the marker falls back to the
// closest earlier user message that has one and is not yet marked, stopping at
// the first already-marked message so one block never carries two breakpoints
// (messages.rs:732-777).
func applyUserMessageCacheLadder(messages []map[string]any) {
	userPositions := make([]int, 0, len(messages))
	for i, message := range messages {
		if message["role"] == "user" {
			userPositions = append(userPositions, i)
		}
	}
	if len(userPositions) == 0 {
		return
	}
	targets := []int{userPositions[0]}
	if len(userPositions) >= 3 {
		targets = append(targets, userPositions[len(userPositions)-2])
	}
	targets = append(targets, userPositions[len(userPositions)-1])
	// A single user message is both first and last; deduplicate so it is
	// processed once.
	targets = deduplicate(targets)
	for _, target := range targets {
		rank := -1
		for i, position := range userPositions {
			if position == target {
				rank = i
				break
			}
		}
		if rank < 0 {
			continue
		}
		// Walk backwards from the target, skipping user messages without a
		// cacheable block and stopping at an already-marked message.
		for i := rank; i >= 0; i-- {
			content, ok := messages[userPositions[i]]["content"].([]map[string]any)
			if !ok {
				continue
			}
			marked := false
			for _, block := range content {
				if _, has := block["cache_control"]; has {
					marked = true
					break
				}
			}
			if marked {
				break
			}
			index := -1
			for j := len(content) - 1; j >= 0; j-- {
				if isCacheableUserBlock(content[j]) {
					index = j
					break
				}
			}
			if index < 0 {
				continue
			}
			addEphemeralCacheControl(content[index])
			break
		}
	}
}

// deduplicate returns the values in insertion order without repeats.
func deduplicate(values []int) []int {
	seen := make(map[int]bool, len(values))
	result := values[:0:0]
	for _, value := range values {
		if !seen[value] {
			seen[value] = true
			result = append(result, value)
		}
	}
	return result
}

// encodeUserBlock converts one neutral user content block into its wire
// shape. The v1 user shape is text-only or tool-result-only
// (messages.rs:780-790 with the image branches dropped: v1 has no image
// blocks).
func encodeUserBlock(block model.ContentBlock) (map[string]any, error) {
	switch typed := block.(type) {
	case model.TextBlock:
		return map[string]any{"type": "text", "text": typed.Text}, nil
	case model.ToolResultBlock:
		return map[string]any{
			"type":        "tool_result",
			"tool_use_id": typed.Result.CallID,
			"content":     []map[string]any{{"type": "text", "text": typed.Result.Content}},
			"is_error":    typed.Result.IsError,
		}, nil
	default:
		return nil, invalidRequest("Messages 用户消息包含不支持的内容块")
	}
}

// encodeAssistantBlock converts one neutral assistant content block.
// Reasoning text without a continuation state is not replayed because it
// cannot continue safely across turns (messages.rs:793-828).
func (a *Adapter) encodeAssistantBlock(block model.ContentBlock) (map[string]any, error) {
	switch typed := block.(type) {
	case model.TextBlock:
		return map[string]any{"type": "text", "text": typed.Text}, nil
	case model.ToolCallBlock:
		arguments, err := compactJSON([]byte(typed.Call.Arguments))
		if err != nil {
			return nil, invalidRequest("Messages 工具调用 %s 的参数不是有效 JSON", typed.Call.ID)
		}
		return map[string]any{
			"type":  "tool_use",
			"id":    typed.Call.ID,
			"name":  typed.Call.Name,
			"input": json.RawMessage(arguments),
		}, nil
	case model.ReasoningBlock:
		if typed.Signature == "" {
			return nil, nil
		}
		kind, data, err := decodeContinuationEnvelope(typed.Signature)
		if err != nil {
			return nil, err
		}
		switch kind {
		case signatureStateKind:
			var signature string
			if json.Unmarshal(data, &signature) != nil {
				return nil, invalidRequest("Messages 推理签名状态必须是字符串")
			}
			return map[string]any{
				"type":      "thinking",
				"thinking":  typed.Text,
				"signature": signature,
			}, nil
		case redactedStateKind:
			return map[string]any{
				"type": "redacted_thinking",
				"data": json.RawMessage(data),
			}, nil
		default:
			return nil, invalidRequest("Messages 无法解释推理续传状态 %s", kind)
		}
	default:
		return nil, invalidRequest("Messages assistant 消息包含不支持的内容块")
	}
}

// encodeToolChoice maps the neutral tool choice onto the Messages wire value
// (messages.rs:870-883; the v1 model has no parallel-tool-call override).
func encodeToolChoice(choice model.ToolChoice) map[string]any {
	switch choice.Mode {
	case model.ToolChoiceNone:
		return map[string]any{"type": "none"}
	case model.ToolChoiceRequired:
		return map[string]any{"type": "any"}
	case model.ToolChoiceTool:
		return map[string]any{"type": "tool", "name": choice.Name}
	default:
		return map[string]any{"type": "auto"}
	}
}

// reasoningBudget maps the neutral reasoning effort onto a conservative
// Messages token budget (messages.rs:886-895). An empty effort uses the
// medium budget.
func reasoningBudget(effort string) int {
	switch effort {
	case model.ReasoningEffortMinimal:
		return 1024
	case model.ReasoningEffortLow:
		return 2048
	case model.ReasoningEffortHigh:
		return 8192
	default:
		// "" and medium share the default budget.
		return 4096
	}
}

// decodeUsage normalizes a Messages usage object (messages.rs:973-993).
// Anthropic input_tokens excludes cache; the unified input total includes
// cache reads and writes. CachedTokens keeps the sum of both cache counters.
// A missing base input is never fabricated into a total.
func decodeUsage(value map[string]json.RawMessage) model.TokenUsage {
	cacheRead := jsonInt64Field(value, "cache_read_input_tokens")
	cacheWrite := jsonInt64Field(value, "cache_creation_input_tokens")
	input := jsonInt64Field(value, "input_tokens")
	usage := model.TokenUsage{InputTokens: -1, OutputTokens: -1, CachedTokens: -1}
	if input >= 0 {
		total := input
		if cacheRead > 0 {
			total += cacheRead
		}
		if cacheWrite > 0 {
			total += cacheWrite
		}
		usage.InputTokens = total
	}
	if output := jsonInt64Field(value, "output_tokens"); output >= 0 {
		usage.OutputTokens = output
	}
	if cacheRead >= 0 || cacheWrite >= 0 {
		total := int64(0)
		if cacheRead > 0 {
			total += cacheRead
		}
		if cacheWrite > 0 {
			total += cacheWrite
		}
		usage.CachedTokens = total
	}
	return usage
}

// jsonInt64Field reads a numeric JSON field; missing or non-numeric values
// return -1 so absent and explicit zero stay distinguishable.
func jsonInt64Field(object map[string]json.RawMessage, field string) int64 {
	raw, ok := object[field]
	if !ok {
		return -1
	}
	var number float64
	if json.Unmarshal(raw, &number) != nil || number < 0 {
		return -1
	}
	return int64(number)
}

// compactJSON normalizes raw JSON to its compact encoding for argument
// transport.
func compactJSON(raw []byte) (string, error) {
	var buffer bytes.Buffer
	if err := json.Compact(&buffer, raw); err != nil {
		return "", err
	}
	return buffer.String(), nil
}

// invalidRequest builds a unified invalid-request error with fmt-style
// formatting (core/provider/src/adapters/wire.rs:24-28).
func invalidRequest(format string, args ...any) *model.ModelError {
	return model.InvalidRequest(format, args...)
}

// protocolError builds a unified protocol error with fmt-style formatting
// (core/provider/src/adapters/wire.rs:31-35).
func protocolError(format string, args ...any) *model.ModelError {
	return model.ProtocolError(format, args...)
}
