package openai

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"strconv"
	"strings"

	"keencode/internal/model"
)

// doneMarker is the Chat Completions SSE terminator.
const doneMarker = "[DONE]"

// syntheticToolCallIDPrefix prefixes placeholder IDs minted for gateways
// (Mistral/llama.cpp family) that stream function calls without an id
// (core/provider/src/adapters/chat_completions.rs:643).
const syntheticToolCallIDPrefix = "chat_tool_call_synth_"

// toolChoiceWire values of the Chat Completions tool_choice field.
const (
	toolChoiceAuto     = "auto"
	toolChoiceNone     = "none"
	toolChoiceRequired = "required"
)

// streamOptionsWire is the Chat Completions stream_options object.
type streamOptionsWire struct {
	IncludeUsage bool `json:"include_usage"`
}

// encodeRequest converts the unified request into a Chat Completions request
// body (core/provider/src/adapters/chat_completions.rs:71-199). Streaming
// additionally requests usage in the final chunk via stream_options.
func encodeRequest(req model.ModelRequest, streaming bool, outputTokenField string) ([]byte, error) {
	if err := req.Validate(); err != nil {
		return nil, err
	}
	messages := make([]map[string]any, 0, len(req.Messages))
	for i := range req.Messages {
		message := &req.Messages[i]
		switch message.Role {
		case model.RoleSystem, model.RoleDeveloper, model.RoleUser:
			if message.Role == model.RoleUser && allToolResults(message.Content) {
				for _, block := range message.Content {
					messages = append(messages, encodeToolMessage(block.(model.ToolResultBlock)))
				}
				continue
			}
			encoded, err := encodeChatMessage(message.Role, message.Content)
			if err != nil {
				return nil, err
			}
			messages = append(messages, encoded)
		case model.RoleAssistant:
			encoded, err := encodeAssistantMessage(message.Content)
			if err != nil {
				return nil, err
			}
			messages = append(messages, encoded)
		default:
			return nil, model.InvalidRequest("Chat Completions 不支持的消息角色 %q", string(message.Role))
		}
	}

	body := map[string]any{
		"model":    req.Model,
		"messages": messages,
		"stream":   streaming,
	}
	if streaming {
		body["stream_options"] = streamOptionsWire{IncludeUsage: true}
	}
	if req.MaxTokens > 0 {
		body[outputTokenField] = req.MaxTokens
	}
	if req.Temperature != nil {
		body["temperature"] = *req.Temperature
	}
	if len(req.Tools) > 0 {
		tools := make([]map[string]any, 0, len(req.Tools))
		for i := range req.Tools {
			tool := &req.Tools[i]
			tools = append(tools, map[string]any{
				"type": "function",
				"function": map[string]any{
					"name":        tool.Name,
					"description": tool.Description,
					"parameters":  json.RawMessage(tool.InputSchema),
					// Keep optional fields and defaults of the tool schema;
					// the runtime validates arguments before execution
					// (chat_completions.rs:144-153).
					"strict": false,
				},
			})
		}
		body["tools"] = tools
		body["tool_choice"] = encodeToolChoice(req.ToolChoice)
	} else if req.ToolChoice.Mode != model.ToolChoiceAuto && req.ToolChoice.Mode != model.ToolChoiceNone &&
		req.ToolChoice.Mode != "" {
		// "" behaves like auto (request.Validate treats it the same way).
		return nil, model.InvalidRequest("Chat Completions 工具选择要求非空工具列表")
	}
	if req.Reasoning != nil && req.Reasoning.Effort != "" {
		body["reasoning_effort"] = req.Reasoning.Effort
	}
	return json.Marshal(body)
}

// allToolResults reports whether every block is a tool result.
func allToolResults(blocks []model.ContentBlock) bool {
	for _, block := range blocks {
		if _, ok := block.(model.ToolResultBlock); !ok {
			return false
		}
	}
	return len(blocks) > 0
}

// encodeChatMessage encodes a system, developer, or user message
// (chat_completions.rs:782-824). Developer instructions map to system in
// order-preserving fashion; the v1 model has no image blocks so content is
// always the concatenated text.
func encodeChatMessage(role model.Role, blocks []model.ContentBlock) (map[string]any, error) {
	wireRole := "user"
	if role != model.RoleUser {
		wireRole = "system"
	}
	var text strings.Builder
	for _, block := range blocks {
		textBlock, ok := block.(model.TextBlock)
		if !ok {
			return nil, model.InvalidRequest("Chat 文本消息包含不支持的内容块")
		}
		text.WriteString(textBlock.Text)
	}
	return map[string]any{"role": wireRole, "content": text.String()}, nil
}

// encodeAssistantMessage encodes assistant text and tool calls
// (chat_completions.rs:827-901). Reasoning blocks are display-only in the v1
// Go model: the adapter never emits reasoning continuations
// (docs/go-migration.md §5.2), so like the Rust no-state path they are not
// replayed onto the wire. An empty text is encoded as JSON null.
func encodeAssistantMessage(blocks []model.ContentBlock) (map[string]any, error) {
	var text strings.Builder
	var toolCalls []map[string]any
	for _, block := range blocks {
		switch typed := block.(type) {
		case model.TextBlock:
			text.WriteString(typed.Text)
		case model.ReasoningBlock:
			// Not replayed: no chat reasoning continuation state exists in v1.
		case model.ToolCallBlock:
			toolCalls = append(toolCalls, map[string]any{
				"id":   typed.Call.ID,
				"type": "function",
				"function": map[string]any{
					"name":      typed.Call.Name,
					"arguments": typed.Call.Arguments,
				},
			})
		default:
			return nil, model.InvalidRequest("Chat assistant 消息包含不支持的内容块")
		}
	}
	message := map[string]any{"role": "assistant"}
	if text.Len() == 0 {
		message["content"] = nil
	} else {
		message["content"] = text.String()
	}
	if len(toolCalls) > 0 {
		message["tool_calls"] = toolCalls
	}
	return message, nil
}

// encodeToolMessage encodes one Chat role=tool message
// (chat_completions.rs:903-930). The error flag stays off the wire; the
// content text explains failures to the model.
func encodeToolMessage(block model.ToolResultBlock) map[string]any {
	return map[string]any{
		"role":         "tool",
		"tool_call_id": block.Result.CallID,
		"content":      block.Result.Content,
	}
}

// encodeToolChoice encodes the unified tool selection strategy
// (chat_completions.rs:932-943). The empty mode behaves like auto.
func encodeToolChoice(choice model.ToolChoice) any {
	switch choice.Mode {
	case model.ToolChoiceNone:
		return toolChoiceNone
	case model.ToolChoiceRequired:
		return toolChoiceRequired
	case model.ToolChoiceTool:
		return map[string]any{
			"type":     "function",
			"function": map[string]any{"name": choice.Name},
		}
	default:
		return toolChoiceAuto
	}
}

// decodeUsage normalizes a Chat Completions usage object keeping unreported
// counters unknown (-1) (chat_completions.rs:945-964). The v1 model carries
// input, output, and cached tokens; reasoning and cache-write counters have
// no v1 field and are dropped.
func decodeUsage(value any) model.TokenUsage {
	usage := model.UnknownUsage()
	object, ok := value.(map[string]any)
	if !ok {
		return usage
	}
	if number, ok := asU64(object["prompt_tokens"]); ok {
		usage.InputTokens = int64(number)
	}
	if number, ok := asU64(object["completion_tokens"]); ok {
		usage.OutputTokens = int64(number)
	}
	if details, ok := asObject(object["prompt_tokens_details"]); ok {
		if number, ok := asU64(details["cached_tokens"]); ok {
			usage.CachedTokens = int64(number)
		}
	}
	return usage
}

// mapFinishReason maps a Chat Completions finish_reason to the unified stop
// reasons (chat_completions.rs:966-980). Unrecognized or missing reasons
// fail: the unified layer forbids MessageEnd events with non-unified
// reasons, so the adapter surfaces an EventError carrying the sanitized
// reason text instead (docs/go-migration.md §5.1). The [DONE] fallback
// covers the missing case before this is consulted for the sloppy-gateway
// path.
func mapFinishReason(reason string) (model.StopReason, error) {
	switch reason {
	case "stop":
		return model.StopEndTurn, nil
	case "tool_calls", "function_call":
		return model.StopToolUse, nil
	case "length":
		return model.StopMaxTokens, nil
	case "content_filter":
		return model.StopContentFilter, nil
	case "":
		return "", protocolError("Chat Completions 缺少 finish_reason，无法归一结束原因")
	default:
		return "", protocolError("Chat Completions finish_reason %s 不受支持", sanitizeReason(reason))
	}
}

// maxReasonTextBytes bounds a raw finish_reason echoed inside an error.
const maxReasonTextBytes = 200

// sanitizeReason strips control characters and bounds a raw endpoint-supplied
// reason before it enters an error message.
func sanitizeReason(reason string) string {
	var bounded strings.Builder
	for _, r := range reason {
		if bounded.Len() >= maxReasonTextBytes {
			bounded.WriteString("…")
			break
		}
		if isControlRune(r) {
			r = ' '
		}
		bounded.WriteRune(r)
	}
	return strconv.Quote(bounded.String())
}

// decodeJSONObject parses one JSON document and requires a top-level object.
func decodeJSONObject(data []byte) (map[string]any, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	object, ok := value.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("JSON 顶层不是对象")
	}
	return object, nil
}

// asString reports a JSON string value.
func asString(value any) (string, bool) {
	text, ok := value.(string)
	return text, ok
}

// asObject reports a JSON object value.
func asObject(value any) (map[string]any, bool) {
	object, ok := value.(map[string]any)
	return object, ok
}

// asArray reports a JSON array value.
func asArray(value any) ([]any, bool) {
	array, ok := value.([]any)
	return array, ok
}

// asU64 reports a JSON non-negative integer value. Fractions, exponents, and
// negative numbers report false, mirroring serde_json Value::as_u64.
func asU64(value any) (uint64, bool) {
	number, ok := value.(json.Number)
	if !ok {
		return 0, false
	}
	parsed, err := strconv.ParseUint(number.String(), 10, 64)
	if err != nil {
		return 0, false
	}
	return parsed, true
}

// isNull reports an explicit JSON null.
func isNull(value any) bool {
	return value == nil
}

// requiredStr reads a required string field from an object
// (core/provider/src/adapters/wire.rs:38-47).
func requiredStr(object map[string]any, field string) (string, error) {
	if text, ok := asString(object[field]); ok {
		return text, nil
	}
	return "", protocolError("Chat 字段 %s 必须是字符串", field)
}

// maxWireIndex is the inclusive upper bound of a Chat tool_call wire index.
const maxWireIndex = math.MaxUint32
