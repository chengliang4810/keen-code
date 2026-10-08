package openai

import (
	"encoding/json"
	"errors"
	"reflect"
	"strings"
	"testing"

	"keencode/internal/model"
)

// consumeFrames feeds SSE data payloads to a fresh adapter and returns the
// produced events.
func consumeFrames(t *testing.T, adapter *streamAdapter, payloads ...string) []model.StreamEvent {
	t.Helper()
	var events []model.StreamEvent
	for _, payload := range payloads {
		if err := adapter.consumeSSE(sseFrame{Data: payload}, &events); err != nil {
			t.Fatalf("消费 SSE 帧失败（%q）：%v", payload, err)
		}
	}
	return events
}

// mustJSONString marshals a fixture value into a data payload.
func mustJSONString(t *testing.T, value any) string {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("夹具编码失败：%v", err)
	}
	return string(data)
}

// eventsOfType returns every event of the given type.
func eventsOfType(events []model.StreamEvent, eventType model.StreamEventType) []model.StreamEvent {
	var matched []model.StreamEvent
	for _, event := range events {
		if event.Type == eventType {
			matched = append(matched, event)
		}
	}
	return matched
}

// eventKinds lists the event types in order.
func eventKinds(events []model.StreamEvent) []model.StreamEventType {
	kinds := make([]model.StreamEventType, 0, len(events))
	for _, event := range events {
		kinds = append(kinds, event.Type)
	}
	return kinds
}

// chunkFixture is one streamed Chat Completions chunk; a nil or absent
// finish leaves the finish_reason field out.
func chunkFixture(t *testing.T, delta map[string]any, finish ...any) string {
	t.Helper()
	choice := map[string]any{"index": 0, "delta": delta}
	if len(finish) > 0 && finish[0] != nil {
		choice["finish_reason"] = finish[0]
	}
	body := map[string]any{"id": "stream-test", "choices": []any{choice}}
	return mustJSONString(t, body)
}

// TestStreamingTextAndReasoningDeltas verifies content index assignment and
// delta merging for text and reasoning fields.
func TestStreamingTextAndReasoningDeltas(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"role": "assistant", "reasoning_content": "先"}, nil),
		chunkFixture(t, map[string]any{"content": "你好"}, nil),
		chunkFixture(t, map[string]any{"content": "，世界"}, nil),
		chunkFixture(t, map[string]any{}, "stop"),
		"[DONE]",
	)

	kinds := eventKinds(events)
	want := []model.StreamEventType{
		model.EventMessageStart,
		model.EventReasoningDelta,
		model.EventTextDelta,
		model.EventTextDelta,
		model.EventMessageEnd,
	}
	if !reflect.DeepEqual(kinds, want) {
		t.Fatalf("事件序列不符：got %v want %v", kinds, want)
	}
	reasoning := events[1]
	if reasoning.Index != 0 || reasoning.Delta != "先" {
		t.Fatalf("推理增量不符：%+v", reasoning)
	}
	if events[2].Index != 1 || events[3].Index != 1 {
		t.Fatalf("文本块序号应稳定为 1：%+v %+v", events[2], events[3])
	}
	end := events[4]
	if end.StopReason != model.StopEndTurn {
		t.Fatalf("stop 应归一为 end_turn：%v", end.StopReason)
	}
}

// TestBufferedReasoningAndToolCallDecode ports
// chat_completions_tests.rs:83-134 minus the reasoning continuation, which
// the v1 Go model does not carry.
func TestBufferedReasoningAndToolCallDecode(t *testing.T) {
	adapter := newStreamAdapter()
	var events []model.StreamEvent
	body := `{
		"id": "response-1",
		"model": "deepseek-test",
		"choices": [{
			"index": 0,
			"message": {
				"role": "assistant",
				"content": null,
				"reasoning_content": "先分析",
				"tool_calls": [{
					"id": "call-1",
					"type": "function",
					"function": { "name": "lookup", "arguments": "{\"city\":\"杭州\"}" }
				}]
			},
			"finish_reason": "tool_calls"
		}]
	}`
	if err := adapter.decodeJSON([]byte(body), &events); err != nil {
		t.Fatalf("完整 Chat JSON 应可解码：%v", err)
	}
	starts := eventsOfType(events, model.EventToolCallStart)
	if len(starts) != 1 || starts[0].CallID != "call-1" || starts[0].Name != "lookup" {
		t.Fatalf("工具开始事件不符：%v", starts)
	}
	deltas := eventsOfType(events, model.EventToolCallArgsDelta)
	if len(deltas) != 1 || deltas[0].Delta != `{"city":"杭州"}` {
		t.Fatalf("工具参数增量不符：%v", deltas)
	}
	ends := eventsOfType(events, model.EventToolCallEnd)
	if len(ends) != 1 || ends[0].CallID != "call-1" {
		t.Fatalf("工具结束事件不符：%v", ends)
	}
	reasoning := eventsOfType(events, model.EventReasoningDelta)
	if len(reasoning) != 1 || reasoning[0].Delta != "先分析" {
		t.Fatalf("推理增量不符：%v", reasoning)
	}
	last := events[len(events)-1]
	if last.Type != model.EventMessageEnd || last.StopReason != model.StopToolUse {
		t.Fatalf("tool_calls 应归一为 tool_use：%+v", last)
	}
}

// TestBufferedReasoningFieldVariants ports
// chat_completions_tests.rs:137-172: both native reasoning field names
// produce reasoning deltas.
func TestBufferedReasoningFieldVariants(t *testing.T) {
	for _, field := range []string{"reasoning_content", "reasoning"} {
		t.Run(field, func(t *testing.T) {
			adapter := newStreamAdapter()
			var events []model.StreamEvent
			body := map[string]any{
				"id": "response-field",
				"choices": []any{map[string]any{
					"index": 0,
					"message": map[string]any{
						"role":    "assistant",
						"content": nil,
						field:     "字段推理",
						"tool_calls": []any{map[string]any{
							"id":   "call-1",
							"type": "function",
							"function": map[string]any{
								"name":      "lookup",
								"arguments": `{"city":"杭州"}`,
							},
						}},
					},
					"finish_reason": "tool_calls",
				}},
			}
			if err := adapter.decodeJSON([]byte(mustJSONString(t, body)), &events); err != nil {
				t.Fatalf("原生推理字段应可解码：%v", err)
			}
			reasoning := eventsOfType(events, model.EventReasoningDelta)
			if len(reasoning) != 1 || reasoning[0].Delta != "字段推理" {
				t.Fatalf("字段 %s 的推理增量不符：%v", field, reasoning)
			}
		})
	}
}

// TestDoneWithoutFinishReasonCompletesPendingToolCalls ports
// chat_completions_tests.rs:233-295: a sloppy gateway that sends [DONE]
// without finish_reason completes with end_turn, emitting the pending tool
// call end first.
func TestDoneWithoutFinishReasonCompletesPendingToolCalls(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"role": "assistant"}, nil),
		chunkFixture(t, map[string]any{"tool_calls": []any{map[string]any{
			"index": 0,
			"id":    "call-9",
			"function": map[string]any{
				"name":      "lookup",
				"arguments": `{"city":"杭州"}`,
			},
		}}}, nil),
	)
	if before := len(eventsOfType(events, model.EventToolCallEnd)); before != 0 {
		t.Fatalf("结束前不应有 ToolCallEnd：%d", before)
	}
	if err := adapter.consumeSSE(sseFrame{Data: "[DONE]"}, &events); err != nil {
		t.Fatalf("[DONE] 兜底应成功：%v", err)
	}
	ends := eventsOfType(events, model.EventToolCallEnd)
	if len(ends) != 1 || ends[0].Index != 0 || ends[0].CallID != "call-9" {
		t.Fatalf("[DONE] 兜底应补发 ToolCallEnd：%v", ends)
	}
	last := events[len(events)-1]
	if last.Type != model.EventMessageEnd || last.StopReason != model.StopEndTurn {
		t.Fatalf("无 finish_reason 的 [DONE] 应按 end_turn 收尾：%+v", last)
	}
}

// TestIncompleteStreamFailsAtFinish ports
// chat_completions_tests.rs:298-324: EOF before finish_reason is an error
// and produces no fabricated terminal events.
func TestIncompleteStreamFailsAtFinish(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"reasoning_content": "未完成"}, nil),
	)
	if err := adapter.finishStream(&events); err == nil {
		t.Fatal("finish_reason 之前关闭应报错")
	}
	if len(eventsOfType(events, model.EventMessageEnd)) != 0 {
		t.Fatal("中断的流不应产生 MessageEnd")
	}
}

// TestFinishReasonSeenThenEOFCompletes verifies the graceful path: a stream
// that delivered finish_reason but no [DONE] still completes at EOF.
func TestFinishReasonSeenThenEOFCompletes(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"content": "好"}, "stop"),
	)
	if err := adapter.finishStream(&events); err != nil {
		t.Fatalf("finish_reason 之后的 EOF 应正常收尾：%v", err)
	}
	last := events[len(events)-1]
	if last.Type != model.EventMessageEnd || last.StopReason != model.StopEndTurn {
		t.Fatalf("EOF 收尾不符：%+v", last)
	}
}

// TestTrailingFramesAfterDoneAreIgnored verifies drain mode
// (chat_completions.rs:207-212).
func TestTrailingFramesAfterDoneAreIgnored(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter, chunkFixture(t, map[string]any{"content": "好"}, "stop"), "[DONE]")
	count := len(events)
	for _, payload := range []string{"[DONE]", "", `{"choices":[{"index":0,"delta":{"content":"迟到"}}]}`} {
		if err := adapter.consumeSSE(sseFrame{Data: payload}, &events); err != nil {
			t.Fatalf("终态后的尾帧不应报错（%q）：%v", payload, err)
		}
	}
	if len(events) != count {
		t.Fatalf("终态后的尾帧不应产生事件：%d → %d", count, len(events))
	}
}

// TestToolCallIDAndNameChangesAreRejected verifies stream identity checks
// (chat_completions.rs:605-635).
func TestToolCallIDAndNameChangesAreRejected(t *testing.T) {
	t.Run("ID 变化", func(t *testing.T) {
		adapter := newStreamAdapter()
		var events []model.StreamEvent
		if err := adapter.consumeSSE(sseFrame{Data: chunkFixture(t, map[string]any{
			"tool_calls": []any{map[string]any{"index": 0, "id": "call-a", "function": map[string]any{"name": "f"}}},
		})}, &events); err != nil {
			t.Fatalf("首帧应成功：%v", err)
		}
		err := adapter.consumeSSE(sseFrame{Data: chunkFixture(t, map[string]any{
			"tool_calls": []any{map[string]any{"index": 0, "id": "call-b", "function": map[string]any{}}},
		})}, &events)
		if err == nil || !strings.Contains(err.Error(), "ID 在流中发生变化") {
			t.Fatalf("ID 变化应报错：%v", err)
		}
	})
	t.Run("名称变化", func(t *testing.T) {
		adapter := newStreamAdapter()
		var events []model.StreamEvent
		if err := adapter.consumeSSE(sseFrame{Data: chunkFixture(t, map[string]any{
			"tool_calls": []any{map[string]any{"index": 0, "id": "call-a", "function": map[string]any{"name": "f"}}},
		})}, &events); err != nil {
			t.Fatalf("首帧应成功：%v", err)
		}
		err := adapter.consumeSSE(sseFrame{Data: chunkFixture(t, map[string]any{
			"tool_calls": []any{map[string]any{"index": 0, "function": map[string]any{"name": "g"}}},
		})}, &events)
		if err == nil || !strings.Contains(err.Error(), "名称在流中发生变化") {
			t.Fatalf("名称变化应报错：%v", err)
		}
	})
}

// TestSyntheticToolCallIDWithoutGatewayID verifies the placeholder id minted
// for gateways that never send ids (chat_completions.rs:637-655).
func TestSyntheticToolCallIDWithoutGatewayID(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"tool_calls": []any{map[string]any{
			"index":    0,
			"function": map[string]any{"name": "lookup"},
		}}}, nil),
	)
	starts := eventsOfType(events, model.EventToolCallStart)
	if len(starts) != 1 {
		t.Fatalf("应恰好一次开始：%v", starts)
	}
	if starts[0].CallID != syntheticToolCallIDPrefix+"0" {
		t.Fatalf("合成 ID 不符：%s", starts[0].CallID)
	}
	if starts[0].Name != "lookup" {
		t.Fatalf("开始事件名称不符：%s", starts[0].Name)
	}
}

// TestRealIDBeforeStartWinsOverSynthetic verifies an id that arrives before
// the tool starts is used for the start event.
func TestRealIDBeforeStartWinsOverSynthetic(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"tool_calls": []any{map[string]any{"index": 0, "id": "call-9"}}}, nil),
		chunkFixture(t, map[string]any{"tool_calls": []any{map[string]any{
			"index":    0,
			"function": map[string]any{"name": "lookup"},
		}}}, nil),
	)
	starts := eventsOfType(events, model.EventToolCallStart)
	if len(starts) != 1 || starts[0].CallID != "call-9" {
		t.Fatalf("开始前到达的真实 ID 应被使用：%v", starts)
	}
}

// TestArgumentsBeforeStartAreSkipped ports chat_completions.rs:661-665:
// argument increments before the identity is complete are dropped without
// failing the stream.
func TestArgumentsBeforeStartAreSkipped(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"tool_calls": []any{map[string]any{
			"index":    0,
			"function": map[string]any{"arguments": "{\"a\":"},
		}}}, nil),
		chunkFixture(t, map[string]any{"tool_calls": []any{map[string]any{
			"index":    0,
			"id":       "call-1",
			"function": map[string]any{"name": "lookup", "arguments": "1}"},
		}}}, nil),
	)
	starts := eventsOfType(events, model.EventToolCallStart)
	deltas := eventsOfType(events, model.EventToolCallArgsDelta)
	if len(starts) != 1 {
		t.Fatalf("应恰好一次开始：%v", starts)
	}
	if len(deltas) != 1 || deltas[0].Delta != "1}" {
		t.Fatalf("开始前的参数增量应被跳过：%v", deltas)
	}
}

// TestParallelToolCallsEndInWireOrder verifies two interleaved tool calls
// keep stable content indexes and end in wire-index order
// (chat_completions.rs:707-725).
func TestParallelToolCallsEndInWireOrder(t *testing.T) {
	adapter := newStreamAdapter()
	toolCall := func(index int, id string) map[string]any {
		return map[string]any{"index": index, "id": id, "function": map[string]any{
			"name": "lookup", "arguments": `{"city":"杭州"}`,
		}}
	}
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"tool_calls": []any{toolCall(0, "call-a")}}, nil),
		chunkFixture(t, map[string]any{"tool_calls": []any{toolCall(1, "call-b")}}, nil),
		chunkFixture(t, map[string]any{}, "tool_calls"),
	)
	starts := eventsOfType(events, model.EventToolCallStart)
	if len(starts) != 2 || starts[0].Index != 0 || starts[1].Index != 1 {
		t.Fatalf("并行工具调用应获得递增内容序号：%v", starts)
	}
	ends := eventsOfType(events, model.EventToolCallEnd)
	if len(ends) != 2 || ends[0].CallID != "call-a" || ends[1].CallID != "call-b" {
		t.Fatalf("工具结束应按 wire 序完成：%v", ends)
	}
}

// TestContentPartsAndUnknownPartType verifies array-form content and the
// unknown-part policy (chat_completions.rs:541-568; adapters/wire.rs 未知
// 内容类跳过 vs 结构类报错：未知 part 类型报错).
func TestContentPartsAndUnknownPartType(t *testing.T) {
	t.Run("数组内容", func(t *testing.T) {
		adapter := newStreamAdapter()
		events := consumeFrames(t, adapter, chunkFixture(t, map[string]any{
			"content": []any{
				map[string]any{"type": "text", "text": "甲"},
				map[string]any{"type": "output_text", "text": "乙"},
				map[string]any{"type": "text"},
			},
		}, nil))
		deltas := eventsOfType(events, model.EventTextDelta)
		if len(deltas) != 2 || deltas[0].Delta != "甲" || deltas[1].Delta != "乙" {
			t.Fatalf("数组内容增量不符：%v", deltas)
		}
	})
	t.Run("未知 part 类型", func(t *testing.T) {
		adapter := newStreamAdapter()
		var events []model.StreamEvent
		err := adapter.consumeSSE(sseFrame{Data: chunkFixture(t, map[string]any{
			"content": []any{map[string]any{"type": "image_url", "image_url": map[string]any{}}},
		}, nil)}, &events)
		if err == nil || !strings.Contains(err.Error(), "未知 part 类型") {
			t.Fatalf("未知 part 类型应报错：%v", err)
		}
	})
}

// TestRefusalMapsToContentFilter verifies the structured-output refusal
// field forces content_filter and surfaces the refusal text
// (chat_completions.rs:499-518).
func TestRefusalMapsToContentFilter(t *testing.T) {
	adapter := newStreamAdapter()
	events := consumeFrames(t, adapter,
		chunkFixture(t, map[string]any{"refusal": "无法协助完成该请求"}, "stop"),
		"[DONE]",
	)
	deltas := eventsOfType(events, model.EventTextDelta)
	if len(deltas) != 1 || deltas[0].Delta != "无法协助完成该请求" {
		t.Fatalf("拒绝文本应作为正文增量：%v", deltas)
	}
	end := events[len(events)-1]
	if end.Type != model.EventMessageEnd || end.StopReason != model.StopContentFilter {
		t.Fatalf("拒绝应归一为 content_filter：%+v", end)
	}
}

// TestFinishReasonNormalizationTable verifies every mapping of
// mapFinishReason through the streaming path; unrecognized reasons must
// fail instead of silently becoming end_turn (docs/go-migration.md §5.1).
func TestFinishReasonNormalizationTable(t *testing.T) {
	tests := []struct {
		name       string
		reason     string
		want       model.StopReason
		wantErr    bool
		errMessage string
	}{
		{"stop", "stop", model.StopEndTurn, false, ""},
		{"tool_calls", "tool_calls", model.StopToolUse, false, ""},
		{"function_call", "function_call", model.StopToolUse, false, ""},
		{"length", "length", model.StopMaxTokens, false, ""},
		{"content_filter", "content_filter", model.StopContentFilter, false, ""},
		{"unknown", "semantic", "", true, "semantic"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			adapter := newStreamAdapter()
			var events []model.StreamEvent
			err := adapter.consumeSSE(sseFrame{Data: chunkFixture(t, map[string]any{}, tc.reason)}, &events)
			if err != nil {
				if tc.wantErr {
					if !strings.Contains(err.Error(), tc.errMessage) {
						t.Fatalf("未识别原因应报错并携带原文：%v", err)
					}
					return
				}
				t.Fatalf("归一失败：%v", err)
			}
			// The finish_reason chunk only records the reason; [DONE] emits
			// the terminal event.
			if err := adapter.consumeSSE(sseFrame{Data: doneMarker}, &events); err != nil {
				t.Fatalf("[DONE] 失败：%v", err)
			}
			last := events[len(events)-1]
			if last.Type != model.EventMessageEnd || last.StopReason != tc.want {
				t.Fatalf("结束原因不符：got %v want %v", last.StopReason, tc.want)
			}
		})
	}
}

// TestDecodeJSONMissingFinishReasonFails verifies the non-streaming path
// rejects a missing finish_reason instead of guessing.
func TestDecodeJSONMissingFinishReasonFails(t *testing.T) {
	adapter := newStreamAdapter()
	var events []model.StreamEvent
	body := `{"id":"r","choices":[{"index":0,"message":{"role":"assistant","content":"好"}}]}`
	err := adapter.decodeJSON([]byte(body), &events)
	if err == nil || !strings.Contains(err.Error(), "缺少 finish_reason") {
		t.Fatalf("缺失 finish_reason 应报错：%v", err)
	}
}

// TestUsageOnlyTailChunkAfterFinish ports chat_completions.rs:314-335: the
// inert usage-only choice after finish_reason is accepted; anything else
// with choices after finish is a protocol violation.
func TestUsageOnlyTailChunkAfterFinish(t *testing.T) {
	usageChunk := `{"id":"stream","usage":{"prompt_tokens":3,"completion_tokens":2},"choices":[{"index":0,"delta":{},"finish_reason":null,"logprobs":null}]}`
	t.Run("惰性尾块", func(t *testing.T) {
		adapter := newStreamAdapter()
		events := consumeFrames(t, adapter,
			chunkFixture(t, map[string]any{"content": "好"}, "stop"),
			usageChunk,
			"[DONE]",
		)
		usages := eventsOfType(events, model.EventUsage)
		if len(usages) != 1 || usages[0].Usage.InputTokens != 3 || usages[0].Usage.OutputTokens != 2 {
			t.Fatalf("usage 尾块应被接受：%v", usages)
		}
	})
	t.Run("非惰性尾块", func(t *testing.T) {
		adapter := newStreamAdapter()
		var events []model.StreamEvent
		consumeFrames(t, adapter, chunkFixture(t, map[string]any{"content": "好"}, "stop"))
		err := adapter.consumeSSE(sseFrame{Data: `{"choices":[{"index":0,"delta":{"content":"迟到"}}]}`}, &events)
		if err == nil || !strings.Contains(err.Error(), "finish_reason 后仍返回非空 choices") {
			t.Fatalf("finish_reason 后的非惰性 choice 应报错：%v", err)
		}
	})
}

// TestInBandErrorObjectIsClassified verifies an error object inside the
// stream normalizes into the unified classification
// (chat_completions.rs:221-224).
func TestInBandErrorObjectIsClassified(t *testing.T) {
	adapter := newStreamAdapter()
	var events []model.StreamEvent
	err := adapter.consumeSSE(sseFrame{Data: `{"error":{"message":"invalid api key provided","code":"invalid_api_key"}}`}, &events)
	var modelErr *model.ModelError
	if err == nil || !errors.As(err, &modelErr) || modelErr.Kind != model.ErrorAuthentication {
		t.Fatalf("带内认证错误应归一为 authentication：%v", err)
	}
}

// TestStreamLevelStructuralViolations verifies choice-count and index
// guards (chat_completions.rs:340-350).
func TestStreamLevelStructuralViolations(t *testing.T) {
	t.Run("多 choice", func(t *testing.T) {
		adapter := newStreamAdapter()
		var events []model.StreamEvent
		err := adapter.consumeSSE(sseFrame{Data: `{"choices":[{"index":0,"delta":{}},{"index":1,"delta":{}}]}`}, &events)
		if err == nil || !strings.Contains(err.Error(), "不支持多个 choice") {
			t.Fatalf("多 choice 应报错：%v", err)
		}
	})
	t.Run("非零 index", func(t *testing.T) {
		adapter := newStreamAdapter()
		var events []model.StreamEvent
		err := adapter.consumeSSE(sseFrame{Data: `{"choices":[{"index":1,"delta":{}}]}`}, &events)
		if err == nil || !strings.Contains(err.Error(), "index=0") {
			t.Fatalf("非零 choice index 应报错：%v", err)
		}
	})
	t.Run("坏 JSON", func(t *testing.T) {
		adapter := newStreamAdapter()
		var events []model.StreamEvent
		err := adapter.consumeSSE(sseFrame{Data: "{not json"}, &events)
		if err == nil || !strings.Contains(err.Error(), "不是有效 JSON") {
			t.Fatalf("坏 JSON 应报协议错误：%v", err)
		}
	})
}

// TestDecodeJSONChoicesShape verifies the buffered path requires exactly one
// choice with a message object (chat_completions.rs:239-255).
func TestDecodeJSONChoicesShape(t *testing.T) {
	tests := []struct {
		name string
		body string
		want string
	}{
		{"缺 choices", `{"id":"r"}`, "缺少 choices 数组"},
		{"双 choice", `{"choices":[{"message":{"content":"a"}},{"message":{"content":"b"}}]}`, "期望一个 choice"},
		{"choice 非对象", `{"choices":["x"]}`, "choice 必须是对象"},
		{"缺 message", `{"choices":[{"index":0}]}`, "缺少 message"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			adapter := newStreamAdapter()
			var events []model.StreamEvent
			err := adapter.decodeJSON([]byte(tc.body), &events)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("%s 应报 %q：%v", tc.name, tc.want, err)
			}
		})
	}
}

// TestDecodeCompleteToolCallRequiresFields verifies required_str guards of
// the buffered tool call path (chat_completions.rs:678-705).
func TestDecodeCompleteToolCallRequiresFields(t *testing.T) {
	tests := []struct {
		name string
		call map[string]any
		want string
	}{
		{"缺 id", map[string]any{"function": map[string]any{"name": "f", "arguments": "{}"}}, "id 必须是字符串"},
		{"缺 function", map[string]any{"id": "c"}, "缺少 function"},
		{"缺 name", map[string]any{"id": "c", "function": map[string]any{"arguments": "{}"}}, "name 必须是字符串"},
		{"缺 arguments", map[string]any{"id": "c", "function": map[string]any{"name": "f"}}, "arguments 必须是字符串"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			adapter := newStreamAdapter()
			var events []model.StreamEvent
			body := mustJSONString(t, map[string]any{
				"id": "r",
				"choices": []any{map[string]any{
					"index":         0,
					"message":       map[string]any{"role": "assistant", "content": nil, "tool_calls": []any{tc.call}},
					"finish_reason": "tool_calls",
				}},
			})
			err := adapter.decodeJSON([]byte(body), &events)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("%s 应报 %q：%v", tc.name, tc.want, err)
			}
		})
	}
}

// TestDecodeJSONUsagePresentEvenWhenNull verifies the buffered path emits a
// usage event for an explicit null usage (all counters unknown), mirroring
// chat_completions.rs:267-271.
func TestDecodeJSONUsagePresentEvenWhenNull(t *testing.T) {
	adapter := newStreamAdapter()
	var events []model.StreamEvent
	body := `{"id":"r","choices":[{"index":0,"message":{"role":"assistant","content":"好"},"finish_reason":"stop"}],"usage":null}`
	if err := adapter.decodeJSON([]byte(body), &events); err != nil {
		t.Fatalf("null usage 不应失败：%v", err)
	}
	usages := eventsOfType(events, model.EventUsage)
	if len(usages) != 1 || usages[0].Usage.IsReported() {
		t.Fatalf("null usage 应产出全未知快照：%v", usages)
	}
}
