package anthropic

import (
	"encoding/json"
	"errors"
	"reflect"
	"testing"

	"keencode/internal/model"
)

// newTestAdapter returns an adapter with caching optionally enabled.
func newTestAdapter(promptCaching bool) *Adapter {
	adapter, err := New(Options{BaseURL: "https://api.anthropic.com/v1", APIKey: "sk-test", PromptCaching: promptCaching})
	if err != nil {
		panic(err)
	}
	return adapter
}

// textMessage is a shorthand for a single-text message.
func textMessage(role model.Role, text string) model.Message {
	return model.TextMessage(role, text)
}

// toolHistoryRequest mirrors tool_history_request from the Rust wire tests
// (core/provider/src/tests.rs): system, user question, assistant tool call,
// and the tool result reply.
func toolHistoryRequest() model.ModelRequest {
	return model.ModelRequest{
		Model: "test-model",
		Messages: []model.Message{
			textMessage(model.RoleSystem, "工具规则"),
			textMessage(model.RoleUser, "北京天气如何？"),
			{
				Role: model.RoleAssistant,
				Content: []model.ContentBlock{
					model.ToolCallBlock{Call: model.ToolCall{ID: "call-1", Name: "weather", Arguments: `{"city":"北京"}`}},
				},
			},
			model.ToolResultMessage(model.ToolResult{CallID: "call-1", Content: "晴，26 度"}),
		},
		Tools: []model.ToolDefinition{{
			Name:        "weather",
			Description: "查询城市天气",
			InputSchema: json.RawMessage(`{"type":"object","properties":{"city":{"type":"string"}}}`),
		}},
	}
}

// encodeForTest encodes a request or fails the test.
func encodeForTest(t *testing.T, adapter *Adapter, req model.ModelRequest) map[string]any {
	t.Helper()
	payload, err := adapter.encodeRequest(&req, true)
	if err != nil {
		t.Fatalf("encodeRequest: %v", err)
	}
	var body map[string]any
	if err := json.Unmarshal(payload, &body); err != nil {
		t.Fatalf("decode body: %v", err)
	}
	return body
}

// TestEncodeRequestKeepsToolLoopShapes ports
// three_protocol_requests_keep_tool_loop_shapes_separate (tests.rs:1553-1574).
func TestEncodeRequestKeepsToolLoopShapes(t *testing.T) {
	body := encodeForTest(t, newTestAdapter(false), toolHistoryRequest())

	if body["stream"] != true {
		t.Fatalf("stream = %v, want true", body["stream"])
	}
	if body["model"] != "test-model" {
		t.Fatalf("model = %v", body["model"])
	}
	messages, ok := body["messages"].([]any)
	if !ok || len(messages) != 3 {
		t.Fatalf("messages = %#v, want 3 wire messages", body["messages"])
	}
	// user question and tool_result stay separate wire messages; the tool
	// result rides in a user message per the Go merged shape.
	assistant, ok := messages[1].(map[string]any)
	if !ok || assistant["role"] != "assistant" {
		t.Fatalf("messages[1] = %#v, want assistant", messages[1])
	}
	assistantContent := assistant["content"].([]any)
	toolUse := assistantContent[0].(map[string]any)
	if toolUse["type"] != "tool_use" {
		t.Fatalf("messages[1].content[0].type = %v, want tool_use", toolUse["type"])
	}
	if toolUse["id"] != "call-1" || toolUse["name"] != "weather" {
		t.Fatalf("tool_use = %#v", toolUse)
	}
	if input, ok := toolUse["input"].(map[string]any); !ok || input["city"] != "北京" {
		t.Fatalf("tool_use.input = %#v", toolUse["input"])
	}
	result := messages[2].(map[string]any)
	if result["role"] != "user" {
		t.Fatalf("messages[2].role = %v, want user", result["role"])
	}
	toolResult := result["content"].([]any)[0].(map[string]any)
	if toolResult["type"] != "tool_result" {
		t.Fatalf("messages[2].content[0].type = %v, want tool_result", toolResult["type"])
	}
	if toolResult["tool_use_id"] != "call-1" || toolResult["is_error"] != false {
		t.Fatalf("tool_result = %#v", toolResult)
	}
	content := toolResult["content"].([]any)[0].(map[string]any)
	if content["type"] != "text" || content["text"] != "晴，26 度" {
		t.Fatalf("tool_result.content = %#v", toolResult["content"])
	}

	tools := body["tools"].([]any)
	tool := tools[0].(map[string]any)
	if tool["name"] != "weather" || tool["description"] != "查询城市天气" {
		t.Fatalf("tools[0] = %#v", tool)
	}
	schema := tool["input_schema"].(map[string]any)
	if schema["type"] != "object" {
		t.Fatalf("input_schema = %#v", schema)
	}
	choice := body["tool_choice"].(map[string]any)
	if choice["type"] != "auto" {
		t.Fatalf("tool_choice = %#v", choice)
	}
}

// TestEncodeRequestMergesAdjacentRoles covers append_message role merging
// (messages.rs:688-698).
func TestEncodeRequestMergesAdjacentRoles(t *testing.T) {
	req := model.ModelRequest{
		Model: "test-model",
		Messages: []model.Message{
			textMessage(model.RoleSystem, "系统"),
			textMessage(model.RoleDeveloper, "开发者约束"),
			textMessage(model.RoleUser, "第一条"),
			textMessage(model.RoleUser, "第二条"),
			textMessage(model.RoleAssistant, "回复"),
		},
	}
	body := encodeForTest(t, newTestAdapter(false), req)
	messages := body["messages"].([]any)
	if len(messages) != 2 {
		t.Fatalf("messages = %d, want 2 (merged user pair + assistant)", len(messages))
	}
	first := messages[0].(map[string]any)
	if first["role"] != "user" {
		t.Fatalf("messages[0].role = %v", first["role"])
	}
	content := first["content"].([]any)
	if len(content) != 2 {
		t.Fatalf("user content = %d blocks, want 2", len(content))
	}
	// The two system messages land in the shared system array in order.
	system := body["system"].([]any)
	if len(system) != 2 {
		t.Fatalf("system = %#v, want 2 text blocks", system)
	}
}

// TestEncodeRequestThinkingBudget covers the reasoning wire rules
// (tests.rs:1600-1647): no explicit ceiling leaves budget+4096, an explicit
// ceiling stays, and a budget swallowing the ceiling fails before the wire.
func TestEncodeRequestThinkingBudget(t *testing.T) {
	newRequest := func(maxTokens int, effort string) model.ModelRequest {
		req := model.ModelRequest{Model: "test-model", Messages: []model.Message{textMessage(model.RoleUser, "问题")}}
		req.MaxTokens = maxTokens
		if effort != "" {
			req.Reasoning = &model.ReasoningConfig{Effort: effort}
		}
		return req
	}
	adapter := newTestAdapter(false)

	// No explicit max_tokens: medium budget 4096 + 4096 answer room.
	body := encodeForTest(t, adapter, newRequest(0, model.ReasoningEffortMedium))
	if got := int(body["max_tokens"].(float64)); got != 8192 {
		t.Fatalf("max_tokens = %d, want 8192", got)
	}
	thinking := body["thinking"].(map[string]any)
	if thinking["type"] != "enabled" || thinking["budget_tokens"].(float64) != 4096 {
		t.Fatalf("thinking = %#v", thinking)
	}

	// Explicit ceiling is preserved verbatim even with a smaller budget.
	body = encodeForTest(t, adapter, newRequest(4096, model.ReasoningEffortLow))
	if got := int(body["max_tokens"].(float64)); got != 4096 {
		t.Fatalf("max_tokens = %d, want 4096", got)
	}
	if got := body["thinking"].(map[string]any)["budget_tokens"].(float64); got != 2048 {
		t.Fatalf("budget = %v, want 2048", got)
	}

	// Budget >= ceiling fails before any network call.
	for _, tt := range []struct {
		maxTokens int
		effort    string
		budget    int
	}{
		{4096, model.ReasoningEffortHigh, 8192},
		{1024, model.ReasoningEffortMinimal, 1024},
	} {
		req := newRequest(tt.maxTokens, tt.effort)
		_, err := adapter.encodeRequest(&req, false)
		var modelErr *model.ModelError
		if !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorInvalidRequest {
			t.Fatalf("budget %d ceiling %d: err = %v, want invalid request", tt.budget, tt.maxTokens, err)
		}
	}
}

// TestEncodeRequestErrors covers the encode-time validation failures.
func TestEncodeRequestErrors(t *testing.T) {
	adapter := newTestAdapter(false)
	// Tool choice without tools.
	req := model.ModelRequest{
		Model:      "test-model",
		Messages:   []model.Message{textMessage(model.RoleUser, "问题")},
		ToolChoice: model.ToolChoice{Mode: model.ToolChoiceRequired},
	}
	if _, err := adapter.encodeRequest(&req, true); err == nil {
		t.Fatalf("tool choice without tools accepted")
	}
	// Empty transcript (system only).
	req = model.ModelRequest{Model: "test-model", Messages: []model.Message{textMessage(model.RoleSystem, "系统")}}
	if _, err := adapter.encodeRequest(&req, true); err == nil {
		t.Fatalf("system-only request accepted")
	}
	// Default max_tokens on the wire when the request leaves it unset.
	req = model.ModelRequest{Model: "test-model", Messages: []model.Message{textMessage(model.RoleUser, "问题")}}
	payload, err := adapter.encodeRequest(&req, true)
	if err != nil {
		t.Fatalf("encodeRequest: %v", err)
	}
	var body map[string]any
	if json.Unmarshal(payload, &body) != nil {
		t.Fatalf("body decode failed")
	}
	if got := body["max_tokens"].(float64); got != defaultMaxTokens {
		t.Fatalf("max_tokens = %v, want %v", got, float64(defaultMaxTokens))
	}
}

// countCacheControl counts cache_control breakpoints anywhere in the body.
func countCacheControl(value any) int {
	switch typed := value.(type) {
	case map[string]any:
		total := 0
		if _, ok := typed["cache_control"]; ok {
			total++
		}
		for _, nested := range typed {
			total += countCacheControl(nested)
		}
		return total
	case []any:
		total := 0
		for _, item := range typed {
			total += countCacheControl(item)
		}
		return total
	default:
		return 0
	}
}

// messageCacheControlPositions returns (message index, block index) pairs of
// every cache_control found in the wire messages array.
func messageCacheControlPositions(t *testing.T, body map[string]any) [][2]int {
	t.Helper()
	messages := body["messages"].([]any)
	var positions [][2]int
	for i, raw := range messages {
		message := raw.(map[string]any)
		content := message["content"].([]any)
		for j, rawBlock := range content {
			block := rawBlock.(map[string]any)
			if _, ok := block["cache_control"]; ok {
				positions = append(positions, [2]int{i, j})
			}
		}
	}
	return positions
}

// ladderRequest builds alternating user/assistant text turns.
func ladderRequest(texts ...string) model.ModelRequest {
	messages := make([]model.Message, 0, len(texts))
	for i, text := range texts {
		role := model.RoleUser
		if i%2 == 1 {
			role = model.RoleAssistant
		}
		messages = append(messages, textMessage(role, text))
	}
	return model.ModelRequest{Model: "test-model", Messages: messages}
}

// TestPromptCachingBreakpointTotalStaysWithinAnthropicLimit ports
// tests.rs:4862-4903.
func TestPromptCachingBreakpointTotalStaysWithinAnthropicLimit(t *testing.T) {
	// System + probe tool + five user turns: full form is system 1 + user
	// ladder 3 = 4 breakpoints, exactly the Anthropic hard cap.
	req := ladderRequest("u1", "a1", "u2", "a2", "u3", "a3", "u4", "a4", "u5")
	req.Messages = append([]model.Message{model.TextMessage(model.RoleSystem, "冻结系统段")}, req.Messages...)
	req.Tools = []model.ToolDefinition{{Name: "weather", Description: "查询", InputSchema: json.RawMessage(`{}`)}}
	body := encodeForTest(t, newTestAdapter(true), req)
	if total := countCacheControl(body); total != 4 {
		t.Fatalf("breakpoints = %d, want 4", total)
	}

	// Borderline form: two user turns → system 1 + user 2 = 3, tools unmarked.
	borderline := ladderRequest("第一轮", "回复一", "第二轮")
	borderline.Messages = append([]model.Message{model.TextMessage(model.RoleSystem, "冻结系统段")}, borderline.Messages...)
	borderline.Tools = []model.ToolDefinition{{Name: "weather", Description: "查询", InputSchema: json.RawMessage(`{}`)}}
	body = encodeForTest(t, newTestAdapter(true), borderline)
	if total := countCacheControl(body); total != 3 {
		t.Fatalf("borderline breakpoints = %d, want 3", total)
	}
	if tools, ok := body["tools"].([]any); ok {
		for _, raw := range tools {
			if _, marked := raw.(map[string]any)["cache_control"]; marked {
				t.Fatalf("tools must not carry breakpoints when system is present")
			}
		}
	}
}

// TestPromptCachingLadderMarksFirstSecondToLastAndLastUsers ports
// tests.rs:4906-4933.
func TestPromptCachingLadderMarksFirstSecondToLastAndLastUsers(t *testing.T) {
	body := encodeForTest(t, newTestAdapter(true), ladderRequest("u1", "a1", "u2", "a2", "u3", "a3", "u4", "a4", "u5"))
	// Wire user positions are 0/2/4/6/8; targets are first(0), second-to-last
	// (6), and last(8).
	want := [][2]int{{0, 0}, {6, 0}, {8, 0}}
	got := messageCacheControlPositions(t, body)
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("positions = %v, want %v", got, want)
	}
	messages := body["messages"].([]any)
	for _, index := range []int{6, 8} {
		block := messages[index].(map[string]any)["content"].([]any)[0].(map[string]any)
		if !reflect.DeepEqual(block["cache_control"], map[string]any{"type": "ephemeral"}) {
			t.Fatalf("messages[%d] cache_control = %#v", index, block["cache_control"])
		}
	}
}

// TestPromptCachingLadderWithTwoOrFewerUsers ports tests.rs:4936-4956.
func TestPromptCachingLadderWithTwoOrFewerUsers(t *testing.T) {
	body := encodeForTest(t, newTestAdapter(true), ladderRequest("u1", "a1", "u2"))
	want := [][2]int{{0, 0}, {2, 0}}
	if got := messageCacheControlPositions(t, body); !reflect.DeepEqual(got, want) {
		t.Fatalf("two users positions = %v, want %v", got, want)
	}

	body = encodeForTest(t, newTestAdapter(true), ladderRequest("唯一一轮"))
	if got := messageCacheControlPositions(t, body); !reflect.DeepEqual(got, [][2]int{{0, 0}}) {
		t.Fatalf("single user positions = %v", got)
	}
	if total := countCacheControl(body); total != 1 {
		t.Fatalf("breakpoints = %d, want 1 (no system/tools)", total)
	}
}

// TestPromptCachingNoBreakpointsWithoutCachingEnabled verifies the ladder is
// opt-in.
func TestPromptCachingNoBreakpointsWithoutCachingEnabled(t *testing.T) {
	body := encodeForTest(t, newTestAdapter(false), ladderRequest("u1", "a1", "u2"))
	if total := countCacheControl(body); total != 0 {
		t.Fatalf("breakpoints = %d, want 0 with caching disabled", total)
	}
}

// TestEncodeRequestReplaysThinkingSignature covers the continuation
// round trip: a streamed signature becomes a thinking block on the next
// request, and an unrecognized state fails closed (messages.rs:802-822).
func TestEncodeRequestReplaysThinkingSignature(t *testing.T) {
	continuation, err := encodeSignatureContinuation("opaque-signature")
	if err != nil {
		t.Fatalf("encodeSignatureContinuation: %v", err)
	}
	redactedData := json.RawMessage(`{"encrypted":"state"}`)
	redacted, err := encodeContinuation(redactedStateKind, redactedData)
	if err != nil {
		t.Fatalf("encodeContinuation: %v", err)
	}

	req := model.ModelRequest{
		Model: "test-model",
		Messages: []model.Message{
			{
				Role: model.RoleAssistant,
				Content: []model.ContentBlock{
					model.ReasoningBlock{Text: "想一想", Signature: continuation},
					model.ReasoningBlock{Signature: redacted},
					model.ReasoningBlock{Text: "纯推理文本，不回传"},
				},
			},
		},
	}
	body := encodeForTest(t, newTestAdapter(false), req)
	messages := body["messages"].([]any)
	content := messages[0].(map[string]any)["content"].([]any)
	if len(content) != 2 {
		t.Fatalf("assistant content = %d blocks, want 2 (pure reasoning text dropped)", len(content))
	}
	thinking := content[0].(map[string]any)
	if thinking["type"] != "thinking" || thinking["thinking"] != "想一想" || thinking["signature"] != "opaque-signature" {
		t.Fatalf("thinking block = %#v", thinking)
	}
	redactedBlock := content[1].(map[string]any)
	if redactedBlock["type"] != "redacted_thinking" {
		t.Fatalf("redacted block = %#v", redactedBlock)
	}
	data := redactedBlock["data"].(map[string]any)
	if data["encrypted"] != "state" {
		t.Fatalf("redacted data = %#v", data)
	}

	// Unknown continuation kinds fail closed.
	req.Messages[0].Content[0] = model.ReasoningBlock{Text: "文本", Signature: `{"kind":"someone-elses-v9","data":"x"}`}
	_, err = newTestAdapter(false).encodeRequest(&req, true)
	var modelErr *model.ModelError
	if !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorInvalidRequest {
		t.Fatalf("unknown continuation kind: err = %v, want invalid request", err)
	}
	// Non-envelope strings fail closed too.
	req.Messages[0].Content[0] = model.ReasoningBlock{Text: "文本", Signature: "bare-signature"}
	if _, err = newTestAdapter(false).encodeRequest(&req, true); err == nil {
		t.Fatalf("bare signature accepted")
	}
}

// TestDecodeUsageNormalization covers the usage rules
// (messages.rs:973-993): input includes cache reads and writes, a missing
// base input is never fabricated, and absent stays -1.
func TestDecodeUsageNormalization(t *testing.T) {
	tests := []struct {
		name string
		raw  string
		want model.TokenUsage
	}{
		{
			name: "all fields",
			raw:  `{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":7,"cache_creation_input_tokens":3}`,
			want: model.TokenUsage{InputTokens: 20, OutputTokens: 5, CachedTokens: 10},
		},
		{
			name: "cache counters omitted",
			raw:  `{"input_tokens":10,"output_tokens":5}`,
			want: model.TokenUsage{InputTokens: 10, OutputTokens: 5, CachedTokens: -1},
		},
		{
			name: "cache only without base input stays unknown",
			raw:  `{"cache_read_input_tokens":7,"output_tokens":2}`,
			want: model.TokenUsage{InputTokens: -1, OutputTokens: 2, CachedTokens: 7},
		},
		{
			name: "explicit zero output",
			raw:  `{"input_tokens":4,"output_tokens":0}`,
			want: model.TokenUsage{InputTokens: 4, OutputTokens: 0, CachedTokens: -1},
		},
		{
			name: "empty usage object",
			raw:  `{}`,
			want: model.TokenUsage{InputTokens: -1, OutputTokens: -1, CachedTokens: -1},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var fields map[string]json.RawMessage
			if err := json.Unmarshal([]byte(tt.raw), &fields); err != nil {
				t.Fatalf("fixture: %v", err)
			}
			got := decodeUsage(fields)
			if got != tt.want {
				t.Fatalf("decodeUsage = %+v, want %+v", got, tt.want)
			}
		})
	}
}
