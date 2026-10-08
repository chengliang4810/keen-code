package openai

import (
	"encoding/json"
	"testing"

	"keencode/internal/model"
)

// lookupTool mirrors the shared fixture of
// core/provider/src/adapters/chat_completions_tests.rs:12-23.
func lookupTool() model.ToolDefinition {
	return model.ToolDefinition{
		Name:        "lookup",
		Description: "查询城市信息",
		InputSchema: json.RawMessage(`{
			"type": "object",
			"properties": { "city": { "type": "string" } },
			"required": ["city"],
			"additionalProperties": false
		}`),
	}
}

// textUserMessage is a plain user text message.
func textUserMessage(text string) model.Message {
	return model.TextMessage(model.RoleUser, text)
}

// toolResultHistory builds the canonical user → assistant(tool call) → tool
// result history of the Rust tests (chat_completions_tests.rs:25-55).
func toolResultHistory() []model.Message {
	return []model.Message{
		textUserMessage("查询天气"),
		{
			Role: model.RoleAssistant,
			Content: []model.ContentBlock{
				model.ToolCallBlock{Call: model.ToolCall{ID: "call-1", Name: "lookup", Arguments: `{"city":"杭州"}`}},
			},
		},
		model.ToolResultMessage(model.ToolResult{CallID: "call-1", Content: "晴"}),
	}
}

// baseToolRequest is a valid request carrying one tool.
func baseToolRequest() model.ModelRequest {
	return model.ModelRequest{
		Model:    "deepseek-test",
		Messages: toolResultHistory(),
		Tools:    []model.ToolDefinition{lookupTool()},
	}
}

// mustEncodeRequest encodes and decodes the request body into a generic
// object for assertions.
func mustEncodeRequest(t *testing.T, req model.ModelRequest, streaming bool, outputTokenField string) map[string]any {
	t.Helper()
	data, err := encodeRequest(req, streaming, outputTokenField)
	if err != nil {
		t.Fatalf("encodeRequest 失败：%v", err)
	}
	var body map[string]any
	if err := json.Unmarshal(data, &body); err != nil {
		t.Fatalf("请求体不是有效 JSON：%v", err)
	}
	return body
}

// TestEncodeRequestBasicShape verifies the core fields of the wire body.
func TestEncodeRequestBasicShape(t *testing.T) {
	req := model.ModelRequest{Model: "deepseek-test", Messages: []model.Message{textUserMessage("查询天气")}}

	streaming := mustEncodeRequest(t, req, true, OutputTokenFieldMaxCompletionTokens)
	if streaming["model"] != "deepseek-test" {
		t.Fatalf("model 字段不符：%v", streaming["model"])
	}
	if streaming["stream"] != true {
		t.Fatalf("stream 字段应为 true：%v", streaming["stream"])
	}
	options, ok := streaming["stream_options"].(map[string]any)
	if !ok || options["include_usage"] != true {
		t.Fatalf("stream_options.include_usage 应为 true：%v", streaming["stream_options"])
	}
	if _, present := streaming["max_completion_tokens"]; present {
		t.Fatal("未设置预算时不应输出 max_completion_tokens")
	}
	if _, present := streaming["temperature"]; present {
		t.Fatal("未设置温度时不应输出 temperature")
	}
	if _, present := streaming["reasoning_effort"]; present {
		t.Fatal("未设置推理配置时不应输出 reasoning_effort")
	}

	nonStreaming := mustEncodeRequest(t, req, false, OutputTokenFieldMaxCompletionTokens)
	if nonStreaming["stream"] != false {
		t.Fatalf("stream 字段应为 false：%v", nonStreaming["stream"])
	}
	if _, present := nonStreaming["stream_options"]; present {
		t.Fatal("非流式请求不应输出 stream_options")
	}
}

// TestEncodeRequestMessageRoles verifies role mapping, assistant shapes, and
// the tool-result decomposition mandated by docs/go-migration.md §5.2.
func TestEncodeRequestMessageRoles(t *testing.T) {
	req := model.ModelRequest{
		Model: "deepseek-test",
		Messages: []model.Message{
			{Role: model.RoleSystem, Content: []model.ContentBlock{model.TextBlock{Text: "系统提示"}}},
			{Role: model.RoleDeveloper, Content: []model.ContentBlock{model.TextBlock{Text: "开发者指令"}}},
			textUserMessage("你好"),
			{
				Role: model.RoleAssistant,
				Content: []model.ContentBlock{
					model.TextBlock{Text: "回答"},
					model.ToolCallBlock{Call: model.ToolCall{ID: "call-1", Name: "lookup", Arguments: `{"city":"杭州"}`}},
				},
			},
			model.ToolResultMessage(
				model.ToolResult{CallID: "call-1", Content: "晴"},
				model.ToolResult{CallID: "call-2", Content: "雨", IsError: true},
			),
		},
	}
	body := mustEncodeRequest(t, req, false, OutputTokenFieldMaxCompletionTokens)
	messages, ok := body["messages"].([]any)
	if !ok {
		t.Fatalf("messages 应为数组：%T", body["messages"])
	}
	if len(messages) != 6 {
		t.Fatalf("应有 6 条 wire 消息（tool 结果逐条拆分），实际 %d 条", len(messages))
	}

	assertMessage := func(index int, role string) map[string]any {
		t.Helper()
		message, ok := messages[index].(map[string]any)
		if !ok {
			t.Fatalf("消息 %d 不是对象：%T", index, messages[index])
		}
		if message["role"] != role {
			t.Fatalf("消息 %d 角色应为 %s，实际 %v", index, role, message["role"])
		}
		return message
	}
	if message := assertMessage(0, "system"); message["content"] != "系统提示" {
		t.Fatalf("system 内容不符：%v", message["content"])
	}
	// Developer instructions map onto system in order.
	if message := assertMessage(1, "system"); message["content"] != "开发者指令" {
		t.Fatalf("developer 内容不符：%v", message["content"])
	}
	if message := assertMessage(2, "user"); message["content"] != "你好" {
		t.Fatalf("user 内容不符：%v", message["content"])
	}

	assistant := assertMessage(3, "assistant")
	if assistant["content"] != "回答" {
		t.Fatalf("assistant 文本不符：%v", assistant["content"])
	}
	toolCalls, ok := assistant["tool_calls"].([]any)
	if !ok || len(toolCalls) != 1 {
		t.Fatalf("assistant 应携带 1 个 tool_call：%v", assistant["tool_calls"])
	}
	call, _ := toolCalls[0].(map[string]any)
	if call["id"] != "call-1" || call["type"] != "function" {
		t.Fatalf("tool_call 外壳不符：%v", call)
	}
	function, _ := call["function"].(map[string]any)
	if function["name"] != "lookup" {
		t.Fatalf("tool_call 名称不符：%v", function["name"])
	}
	// Arguments cross the wire verbatim, never re-serialized mid-flight.
	if function["arguments"] != `{"city":"杭州"}` {
		t.Fatalf("tool_call 参数应原样携带：%v", function["arguments"])
	}

	tool := assertMessage(4, "tool")
	if tool["tool_call_id"] != "call-1" || tool["content"] != "晴" {
		t.Fatalf("tool 消息不符：%v", tool)
	}
	tool = assertMessage(5, "tool")
	if tool["tool_call_id"] != "call-2" || tool["content"] != "雨" {
		t.Fatalf("tool 消息不符：%v", tool)
	}
}

// TestEncodeRequestAssistantEmptyTextIsNull verifies the assistant content
// shape when only tool calls exist (chat_completions.rs:878-900).
func TestEncodeRequestAssistantEmptyTextIsNull(t *testing.T) {
	req := model.ModelRequest{
		Model: "m",
		Messages: []model.Message{{
			Role: model.RoleAssistant,
			Content: []model.ContentBlock{
				model.ToolCallBlock{Call: model.ToolCall{ID: "c1", Name: "lookup", Arguments: "{}"}},
			},
		}},
	}
	body := mustEncodeRequest(t, req, false, OutputTokenFieldMaxCompletionTokens)
	messages := body["messages"].([]any)
	assistant := messages[0].(map[string]any)
	value, present := assistant["content"]
	if !present {
		t.Fatal("assistant 应显式携带 content 键")
	}
	if value != nil {
		t.Fatalf("空文本 content 应为 null：%v", value)
	}
}

// TestEncodeRequestDropsReasoningBlocks verifies the v1 rule that history
// reasoning never reaches the Chat wire: the adapter never emits reasoning
// continuations, so nothing can be replayed (docs/go-migration.md §5.2;
// Rust test generic_reasoning_without_chat_state_is_not_replayed).
func TestEncodeRequestDropsReasoningBlocks(t *testing.T) {
	req := model.ModelRequest{
		Model: "deepseek-test",
		Messages: []model.Message{
			textUserMessage("继续"),
			{Role: model.RoleAssistant, Content: []model.ContentBlock{
				model.ReasoningBlock{Text: "仅供展示的推理", Signature: `{"reasoning_content":"历史推理"}`},
				model.TextBlock{Text: "回答"},
			}},
		},
	}
	body := mustEncodeRequest(t, req, false, OutputTokenFieldMaxCompletionTokens)
	assistant := body["messages"].([]any)[1].(map[string]any)
	if _, present := assistant["reasoning_content"]; present {
		t.Fatal("无续传状态的推理不应写入 reasoning_content")
	}
	if _, present := assistant["reasoning"]; present {
		t.Fatal("无续传状态的推理不应写入 reasoning")
	}
	if assistant["content"] != "回答" {
		t.Fatalf("assistant 文本不符：%v", assistant["content"])
	}
}

// TestEncodeRequestAssistantRejectsToolResults verifies the defensive guard
// against tool result blocks inside assistant messages.
func TestEncodeRequestAssistantRejectsToolResults(t *testing.T) {
	req := model.ModelRequest{
		Model: "m",
		Messages: []model.Message{{
			Role: model.RoleAssistant,
			Content: []model.ContentBlock{
				model.ToolResultBlock{Result: model.ToolResult{CallID: "c1", Content: "x"}},
			},
		}},
	}
	if _, err := encodeRequest(req, false, OutputTokenFieldMaxCompletionTokens); err == nil {
		t.Fatal("assistant 携带工具结果应报错")
	}
}

// TestEncodeRequestToolsAndChoice verifies the tools array and every
// tool_choice variant.
func TestEncodeRequestToolsAndChoice(t *testing.T) {
	requests := []struct {
		name       string
		choice     model.ToolChoice
		wantChoice any
	}{
		{"auto", model.ToolChoice{Mode: model.ToolChoiceAuto}, "auto"},
		{"empty-is-auto", model.ToolChoice{}, "auto"},
		{"none", model.ToolChoice{Mode: model.ToolChoiceNone}, "none"},
		{"required", model.ToolChoice{Mode: model.ToolChoiceRequired}, "required"},
		{"tool", model.ToolChoice{Mode: model.ToolChoiceTool, Name: "lookup"}, map[string]any{
			"type":     "function",
			"function": map[string]any{"name": "lookup"},
		}},
	}
	for _, tc := range requests {
		t.Run(tc.name, func(t *testing.T) {
			req := baseToolRequest()
			req.ToolChoice = tc.choice
			body := mustEncodeRequest(t, req, false, OutputTokenFieldMaxCompletionTokens)
			tools, ok := body["tools"].([]any)
			if !ok || len(tools) != 1 {
				t.Fatalf("tools 应为单元素数组：%v", body["tools"])
			}
			tool := tools[0].(map[string]any)
			if tool["type"] != "function" {
				t.Fatalf("tool 类型不符：%v", tool["type"])
			}
			function := tool["function"].(map[string]any)
			if function["name"] != "lookup" || function["description"] != "查询城市信息" {
				t.Fatalf("tool 定义不符：%v", function)
			}
			if function["strict"] != false {
				t.Fatal("strict 必须保持 false 以保留 Schema 可选字段")
			}
			// The input schema is embedded untouched.
			want := json.RawMessage(lookupTool().InputSchema)
			got, err := json.Marshal(function["parameters"])
			if err != nil {
				t.Fatalf("parameters 编码失败：%v", err)
			}
			var wantValue, gotValue any
			if json.Unmarshal(want, &wantValue) != nil || json.Unmarshal(got, &gotValue) != nil ||
				!jsonEqual(wantValue, gotValue) {
				t.Fatalf("parameters 应原样透传")
			}
			if !jsonEqual(body["tool_choice"], tc.wantChoice) {
				t.Fatalf("tool_choice 不符：got %v want %v", body["tool_choice"], tc.wantChoice)
			}
		})
	}
}

// TestEncodeRequestToolChoiceRequiresTools ports the guard of
// chat_completions.rs:165-167.
func TestEncodeRequestToolChoiceRequiresTools(t *testing.T) {
	req := model.ModelRequest{
		Model:      "m",
		Messages:   []model.Message{textUserMessage("hi")},
		ToolChoice: model.ToolChoice{Mode: model.ToolChoiceRequired},
	}
	if _, err := encodeRequest(req, false, OutputTokenFieldMaxCompletionTokens); err == nil {
		t.Fatal("空工具列表携带 required 工具选择应报错")
	}
	// none stays legal without tools.
	req.ToolChoice = model.ToolChoice{Mode: model.ToolChoiceNone}
	if _, err := encodeRequest(req, false, OutputTokenFieldMaxCompletionTokens); err != nil {
		t.Fatalf("空工具列表携带 none 工具选择应合法：%v", err)
	}
}

// TestEncodeRequestBudgetFieldSelection verifies that exactly one output
// budget field is emitted, selected by configuration
// (config.rs ChatOutputTokenField).
func TestEncodeRequestBudgetFieldSelection(t *testing.T) {
	req := baseToolRequest()
	req.MaxTokens = 512

	body := mustEncodeRequest(t, req, true, OutputTokenFieldMaxCompletionTokens)
	if body["max_completion_tokens"] != float64(512) {
		t.Fatalf("max_completion_tokens 不符：%v", body["max_completion_tokens"])
	}
	if _, present := body["max_tokens"]; present {
		t.Fatal("两种预算字段不得同时发送")
	}

	body = mustEncodeRequest(t, req, true, OutputTokenFieldMaxTokens)
	if body["max_tokens"] != float64(512) {
		t.Fatalf("max_tokens 不符：%v", body["max_tokens"])
	}
	if _, present := body["max_completion_tokens"]; present {
		t.Fatal("两种预算字段不得同时发送")
	}
}

// TestEncodeRequestTemperatureAndReasoning verifies explicit-zero
// temperature stays on the wire and reasoning_effort passes through.
func TestEncodeRequestTemperatureAndReasoning(t *testing.T) {
	zero := 0.0
	req := baseToolRequest()
	req.Temperature = &zero
	req.Reasoning = &model.ReasoningConfig{Effort: model.ReasoningEffortHigh}

	body := mustEncodeRequest(t, req, true, OutputTokenFieldMaxCompletionTokens)
	if body["temperature"] != float64(0) {
		t.Fatalf("显式零温度必须保留：%v", body["temperature"])
	}
	if body["reasoning_effort"] != "high" {
		t.Fatalf("reasoning_effort 不符：%v", body["reasoning_effort"])
	}

	req.Reasoning = &model.ReasoningConfig{}
	body = mustEncodeRequest(t, req, true, OutputTokenFieldMaxCompletionTokens)
	if _, present := body["reasoning_effort"]; present {
		t.Fatal("空推理强度不应输出 reasoning_effort")
	}
}

// TestEncodeRequestRejectsInvalidRequests verifies unified validation runs
// before encoding.
func TestEncodeRequestRejectsInvalidRequests(t *testing.T) {
	requests := map[string]model.ModelRequest{
		"空模型": {Messages: []model.Message{textUserMessage("hi")}},
		"空消息": {Model: "m"},
		"无效角色": {Model: "m", Messages: []model.Message{{Role: "robot",
			Content: []model.ContentBlock{model.TextBlock{Text: "x"}}}}},
	}
	for name, req := range requests {
		t.Run(name, func(t *testing.T) {
			if _, err := encodeRequest(req, true, OutputTokenFieldMaxCompletionTokens); err == nil {
				t.Fatalf("%s 应被校验拒绝", name)
			}
		})
	}
}

// TestDecodeUsage verifies the usage normalization table: unreported
// counters stay unknown (-1) and explicit zeros stay zero
// (core/model/src/tests.rs:78-97).
func TestDecodeUsage(t *testing.T) {
	tests := []struct {
		name          string
		wire          string
		input, output int64
		cached        int64
	}{
		{"完整字段", `{"prompt_tokens":10,"completion_tokens":5}`, 10, 5, -1},
		{"缓存读取", `{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":7}}`, 10, 5, 7},
		{"缺失按未知", `{}`, -1, -1, -1},
		{"显式零保留", `{"prompt_tokens":0,"completion_tokens":0}`, 0, 0, -1},
		{"非数值忽略", `{"prompt_tokens":"many","completion_tokens":null}`, -1, -1, -1},
		{"小数忽略", `{"prompt_tokens":1.5,"completion_tokens":2.5}`, -1, -1, -1},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			value, err := decodeJSONObject([]byte(tc.wire))
			if err != nil {
				t.Fatalf("夹具解析失败：%v", err)
			}
			usage := decodeUsage(value)
			if usage.InputTokens != tc.input || usage.OutputTokens != tc.output || usage.CachedTokens != tc.cached {
				t.Fatalf("usage 不符：got %+v want input=%d output=%d cached=%d", usage, tc.input, tc.output, tc.cached)
			}
		})
	}
}

// jsonEqual compares two decoded JSON values structurally.
func jsonEqual(a, b any) bool {
	encodedA, errA := json.Marshal(a)
	encodedB, errB := json.Marshal(b)
	if errA != nil || errB != nil {
		return false
	}
	return string(encodedA) == string(encodedB)
}
