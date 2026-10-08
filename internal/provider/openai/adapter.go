package openai

import (
	"fmt"
	"sort"
	"strings"

	"keencode/internal/model"
)

// protocolError builds a unified protocol error for remote responses that
// cannot be converted into unified events.
func protocolError(format string, args ...any) *model.ModelError {
	return model.ProtocolError(format, args...)
}

// pendingToolCall is one tool call being assembled from streamed deltas
// (core/provider/src/adapters/chat_completions.rs:19-29).
type pendingToolCall struct {
	// contentIndex is the unified content block index assigned in arrival
	// order, decoupled from the gateway wire index.
	contentIndex uint32
	id           string
	hasID        bool
	// idSynthetic marks id as a placeholder minted for gateways that send no
	// id; a real id arriving before the start still replaces it.
	idSynthetic bool
	name        string
	hasName     bool
	started     bool
}

// streamAdapter converts Chat Completions chunks and buffered JSON into
// unified stream events. One instance serves exactly one response; it is a
// port of core/provider/src/adapters/chat_completions.rs:31-49 minus the
// reasoning continuation state, which the v1 Go model does not carry
// (docs/go-migration.md §5.2: the OpenAI adapter never emits
// EventReasoningContinuation).
type streamAdapter struct {
	started          bool
	ended            bool
	finishSet        bool
	finishReason     model.StopReason
	sawRefusal       bool
	nextContentIndex uint32
	textIndex        int64 // -1 when unset
	reasoningIndex   int64 // -1 when unset
	tools            map[uint32]*pendingToolCall
}

// newStreamAdapter creates a per-request adapter without residual state.
func newStreamAdapter() *streamAdapter {
	return &streamAdapter{
		textIndex:      -1,
		reasoningIndex: -1,
		tools:          map[uint32]*pendingToolCall{},
	}
}

// consumeSSE consumes one SSE frame and appends unified events
// (chat_completions.rs:202-225). After the terminal event the adapter
// switches to drain mode: trailing frames (repeated [DONE], keep-alive
// frames, late data) no longer affect the completed response.
func (a *streamAdapter) consumeSSE(frame sseFrame, out *[]model.StreamEvent) error {
	if a.ended {
		return nil
	}
	trimmed := strings.TrimSpace(frame.Data)
	if trimmed == doneMarker {
		return a.emitMessageEnd(out)
	}
	if trimmed == "" {
		return nil
	}
	value, err := decodeJSONObject([]byte(frame.Data))
	if err != nil {
		return protocolError("Chat Completions SSE data 不是有效 JSON：%v", err)
	}
	if inBand, present := value["error"]; present && !isNull(inBand) {
		return classifyInBandProviderError(value, "Chat Completions Provider 返回未说明错误")
	}
	return a.consumeChunk(value, out)
}

// decodeJSON converts one non-streaming Chat Completions JSON response into
// the full event sequence (chat_completions.rs:228-280).
func (a *streamAdapter) decodeJSON(data []byte, out *[]model.StreamEvent) error {
	response, err := decodeJSONObject(data)
	if err != nil {
		return protocolError("Chat Completions 响应必须是 JSON 对象")
	}
	if inBand, present := response["error"]; present && !isNull(inBand) {
		return classifyInBandProviderError(response, "Chat Completions Provider 返回未说明错误")
	}
	*out = append(*out, model.StreamEvent{Type: model.EventMessageStart})
	a.started = true

	choices, ok := asArray(response["choices"])
	if !ok {
		return protocolError("Chat Completions 响应缺少 choices 数组")
	}
	if len(choices) != 1 {
		return protocolError("Chat Completions 期望一个 choice，实际为 %d", len(choices))
	}
	choice, ok := asObject(choices[0])
	if !ok {
		return protocolError("Chat Completions choice 必须是对象")
	}
	message, ok := asObject(choice["message"])
	if !ok {
		return protocolError("Chat Completions choice 缺少 message")
	}
	if err := a.decodeReasoningFields(message, out); err != nil {
		return err
	}
	if err := a.decodeContentValue(message["content"], out); err != nil {
		return err
	}
	if err := a.decodeRefusalField(message, out); err != nil {
		return err
	}
	if toolCalls, ok := asArray(message["tool_calls"]); ok {
		for _, call := range toolCalls {
			callObject, ok := asObject(call)
			if !ok {
				return protocolError("Chat tool_call 必须是对象")
			}
			if err := a.decodeCompleteToolCall(callObject, out); err != nil {
				return err
			}
		}
	}
	if usage, present := response["usage"]; present {
		*out = append(*out, model.StreamEvent{Type: model.EventUsage, Usage: decodeUsage(usage)})
	}
	if a.sawRefusal {
		*out = append(*out, model.StreamEvent{Type: model.EventMessageEnd, StopReason: model.StopContentFilter})
	} else {
		stop, err := mapFinishReason(asStringOrEmpty(choice["finish_reason"]))
		if err != nil {
			return err
		}
		*out = append(*out, model.StreamEvent{Type: model.EventMessageEnd, StopReason: stop})
	}
	a.ended = true
	return nil
}

// finishStream validates that the SSE stream observed a usable terminal
// condition before EOF (chat_completions.rs:283-298). A connection cut
// without finish_reason stays an error; the explicit-[DONE] fallback lives
// in emitMessageEnd.
func (a *streamAdapter) finishStream(out *[]model.StreamEvent) error {
	if a.ended {
		return nil
	}
	if a.finishSet {
		return a.emitMessageEnd(out)
	}
	return protocolError("Chat Completions SSE 在 finish_reason 之前关闭")
}

// consumeChunk parses one streamed chunk's metadata, content, tools, usage,
// and finish reason (chat_completions.rs:301-376).
func (a *streamAdapter) consumeChunk(value map[string]any, out *[]model.StreamEvent) error {
	if !a.started {
		*out = append(*out, model.StreamEvent{Type: model.EventMessageStart})
		a.started = true
	}
	rawUsage, usagePresent := value["usage"]
	usagePresent = usagePresent && !isNull(rawUsage)
	// Gateways that only send usage at the end without choices: treat a
	// missing choices field as empty (chat_completions.rs:314-320).
	choices, _ := asArray(value["choices"])
	if a.finishSet && len(choices) > 0 && !(usagePresent && isInertUsageChoice(choices)) {
		return protocolError("Chat Completions 在 finish_reason 后仍返回非空 choices")
	}
	if usagePresent {
		*out = append(*out, model.StreamEvent{Type: model.EventUsage, Usage: decodeUsage(rawUsage)})
	}
	if len(choices) == 0 {
		return nil
	}
	if a.finishSet {
		return nil
	}
	if len(choices) != 1 {
		return protocolError("Chat Completions 流式响应不支持多个 choice")
	}
	choice, ok := asObject(choices[0])
	if !ok {
		return protocolError("Chat Completions choice 必须是对象")
	}
	if index, ok := asU64(choice["index"]); ok && index != 0 {
		return protocolError("Chat Completions 只接受 index=0 的单一 choice")
	}
	if delta, ok := asObject(choice["delta"]); ok {
		if err := a.decodeReasoningFields(delta, out); err != nil {
			return err
		}
		if err := a.decodeContentValue(delta["content"], out); err != nil {
			return err
		}
		if err := a.decodeRefusalField(delta, out); err != nil {
			return err
		}
		if toolCalls, ok := asArray(delta["tool_calls"]); ok {
			for _, call := range toolCalls {
				callObject, ok := asObject(call)
				if !ok {
					return protocolError("Chat tool_call delta 必须是对象")
				}
				if err := a.consumeToolDelta(callObject, out); err != nil {
					return err
				}
			}
		}
	}
	if reason := asStringOrEmpty(choice["finish_reason"]); reason != "" {
		if err := a.finishTools(out); err != nil {
			return err
		}
		if a.sawRefusal {
			a.finishReason, a.finishSet = model.StopContentFilter, true
		} else {
			stop, err := mapFinishReason(reason)
			if err != nil {
				return err
			}
			a.finishReason, a.finishSet = stop, true
		}
	}
	return nil
}

// emitMessageEnd emits the unified terminal event
// (chat_completions.rs:379-395). Gateways that never send a finish_reason
// but do send [DONE] are completed with the default end_turn reason — the
// same fallback the Rust adapter inherited from rig. Pending tool calls are
// finished first so an already-billed response is not discarded by the
// unified layer's unfinished-tool-call check.
func (a *streamAdapter) emitMessageEnd(out *[]model.StreamEvent) error {
	stop := model.StopEndTurn
	if a.finishSet {
		stop = a.finishReason
	}
	if err := a.finishTools(out); err != nil {
		return err
	}
	*out = append(*out, model.StreamEvent{Type: model.EventMessageEnd, StopReason: stop})
	a.ended = true
	return nil
}

// decodeReasoningFields parses the common Chat reasoning text fields while
// keeping them separate from ordinary text (chat_completions.rs:397-433).
// The v1 model has no continuation slot for them, so they stay display-only
// reasoning deltas.
func (a *streamAdapter) decodeReasoningFields(object map[string]any, out *[]model.StreamEvent) error {
	for _, field := range []string{"reasoning_content", "reasoning"} {
		if text, ok := asString(object[field]); ok && text != "" {
			index, err := a.reasoningContentIndex()
			if err != nil {
				return err
			}
			*out = append(*out, model.StreamEvent{Type: model.EventReasoningDelta, Index: index, Delta: text})
		}
	}
	details, ok := asArray(object["reasoning_details"])
	if !ok {
		return nil
	}
	for _, detail := range details {
		object, ok := asObject(detail)
		if !ok {
			continue
		}
		text, hasText := asString(object["text"])
		if !hasText {
			text, hasText = asString(object["delta"])
		}
		if !hasText || text == "" {
			continue
		}
		index, err := a.reasoningContentIndex()
		if err != nil {
			return err
		}
		*out = append(*out, model.StreamEvent{Type: model.EventReasoningDelta, Index: index, Delta: text})
	}
	return nil
}

// decodeRefusalField parses the structured-output safety refusal field and
// forces a non-normal completion reason (chat_completions.rs:499-518).
func (a *streamAdapter) decodeRefusalField(object map[string]any, out *[]model.StreamEvent) error {
	text, ok := asString(object["refusal"])
	if !ok || text == "" {
		return nil
	}
	a.sawRefusal = true
	index, err := a.textContentIndex()
	if err != nil {
		return err
	}
	*out = append(*out, model.StreamEvent{Type: model.EventTextDelta, Index: index, Delta: text})
	return nil
}

// decodeContentValue parses Chat text in string or content-part array form
// (chat_completions.rs:520-569).
func (a *streamAdapter) decodeContentValue(value any, out *[]model.StreamEvent) error {
	if isNull(value) {
		return nil
	}
	if text, ok := asString(value); ok {
		if text != "" {
			index, err := a.textContentIndex()
			if err != nil {
				return err
			}
			*out = append(*out, model.StreamEvent{Type: model.EventTextDelta, Index: index, Delta: text})
		}
		return nil
	}
	parts, ok := asArray(value)
	if !ok {
		return protocolError("Chat content 必须是字符串、数组或 null")
	}
	for _, part := range parts {
		object, ok := asObject(part)
		if !ok {
			// Non-object parts carry no text field; skipped like Rust.
			continue
		}
		partType := "text"
		if typed, ok := asString(object["type"]); ok {
			partType = typed
		}
		switch partType {
		case "text", "output_text":
			if text, ok := asString(object["text"]); ok && text != "" {
				index, err := a.textContentIndex()
				if err != nil {
					return err
				}
				*out = append(*out, model.StreamEvent{Type: model.EventTextDelta, Index: index, Delta: text})
			}
		default:
			return protocolError("Chat content 包含未知 part 类型 %s", partType)
		}
	}
	return nil
}

// consumeToolDelta consumes one streamed function-call increment and emits a
// start event once its fields are complete
// (chat_completions.rs:571-676).
func (a *streamAdapter) consumeToolDelta(object map[string]any, out *[]model.StreamEvent) error {
	// Gateways such as Mistral/llama.cpp omit the index for single-tool
	// streams; default to 0 like rig (chat_completions.rs:580-586).
	wireIndex := uint32(0)
	if raw, present := object["index"]; present {
		if number, ok := asU64(raw); ok {
			if number > maxWireIndex {
				return protocolError("Chat tool_call index 溢出")
			}
			wireIndex = uint32(number)
		}
	}
	pending, exists := a.tools[wireIndex]
	if !exists {
		if a.nextContentIndex == maxWireIndex {
			return protocolError("Chat 内容块序号溢出")
		}
		pending = &pendingToolCall{contentIndex: a.nextContentIndex}
		a.nextContentIndex++
		a.tools[wireIndex] = pending
	}
	if id, ok := asString(object["id"]); ok && id != "" {
		replacedSynthetic := pending.idSynthetic && !pending.started
		if pending.hasID && pending.id != id && !replacedSynthetic {
			return protocolError("Chat 工具调用 ID 在流中发生变化")
		}
		if replacedSynthetic {
			pending.id, pending.idSynthetic = id, false
		} else if !pending.hasID {
			pending.id, pending.hasID = id, true
		}
	}
	function, _ := asObject(object["function"])
	if function != nil {
		if name, ok := asString(function["name"]); ok && name != "" {
			if pending.hasName && pending.name != name {
				return protocolError("Chat 工具名称在流中发生变化")
			}
			pending.name, pending.hasName = name, true
		}
	}
	if !pending.started && pending.hasName {
		// Gateways without ids mint a placeholder once the name is known.
		id := pending.id
		if !pending.hasID {
			id = fmt.Sprintf("%s%d", syntheticToolCallIDPrefix, wireIndex)
			pending.id, pending.hasID, pending.idSynthetic = id, true, true
		}
		*out = append(*out, model.StreamEvent{
			Type:   model.EventToolCallStart,
			Index:  pending.contentIndex,
			CallID: id,
			Name:   pending.name,
		})
		pending.started = true
	}
	if function != nil {
		if arguments, ok := asString(function["arguments"]); ok && arguments != "" {
			if !pending.started {
				// Arguments arriving before id/name are complete: skip the
				// increment instead of failing the whole stream
				// (chat_completions.rs:662-665).
				return nil
			}
			if !pending.hasID {
				return protocolError("Chat 工具调用缺少 ID")
			}
			*out = append(*out, model.StreamEvent{
				Type:   model.EventToolCallArgsDelta,
				Index:  pending.contentIndex,
				CallID: pending.id,
				Delta:  arguments,
			})
		}
	}
	return nil
}

// decodeCompleteToolCall parses one non-streaming complete tool call
// (chat_completions.rs:678-705).
func (a *streamAdapter) decodeCompleteToolCall(object map[string]any, out *[]model.StreamEvent) error {
	id, err := requiredStr(object, "id")
	if err != nil {
		return err
	}
	function, ok := asObject(object["function"])
	if !ok {
		return protocolError("Chat tool_call 缺少 function")
	}
	name, err := requiredStr(function, "name")
	if err != nil {
		return err
	}
	arguments, err := requiredStr(function, "arguments")
	if err != nil {
		return err
	}
	index, err := a.allocateContentIndex()
	if err != nil {
		return err
	}
	*out = append(*out,
		model.StreamEvent{Type: model.EventToolCallStart, Index: index, CallID: id, Name: name},
		model.StreamEvent{Type: model.EventToolCallArgsDelta, Index: index, CallID: id, Delta: arguments},
		model.StreamEvent{Type: model.EventToolCallEnd, Index: index, CallID: id},
	)
	return nil
}

// finishTools completes every started tool call in wire index order
// (chat_completions.rs:707-725). Calls that never received a name cannot be
// acted on and are silently dropped so the completed stream does not fail.
func (a *streamAdapter) finishTools(out *[]model.StreamEvent) error {
	indexes := make([]int, 0, len(a.tools))
	for wireIndex := range a.tools {
		indexes = append(indexes, int(wireIndex))
	}
	sort.Ints(indexes)
	for _, wireIndex := range indexes {
		pending := a.tools[uint32(wireIndex)]
		if !pending.started {
			continue
		}
		if !pending.hasID {
			return protocolError("Chat 工具调用缺少 ID")
		}
		*out = append(*out, model.StreamEvent{
			Type:   model.EventToolCallEnd,
			Index:  pending.contentIndex,
			CallID: pending.id,
		})
	}
	a.tools = map[uint32]*pendingToolCall{}
	return nil
}

// textContentIndex returns or allocates the ordinary text block index
// (chat_completions.rs:727-736).
func (a *streamAdapter) textContentIndex() (uint32, error) {
	if a.textIndex >= 0 {
		return uint32(a.textIndex), nil
	}
	index, err := a.allocateContentIndex()
	if err != nil {
		return 0, err
	}
	a.textIndex = int64(index)
	return index, nil
}

// reasoningContentIndex returns or allocates the reasoning block index
// (chat_completions.rs:738-747).
func (a *streamAdapter) reasoningContentIndex() (uint32, error) {
	if a.reasoningIndex >= 0 {
		return uint32(a.reasoningIndex), nil
	}
	index, err := a.allocateContentIndex()
	if err != nil {
		return 0, err
	}
	a.reasoningIndex = int64(index)
	return index, nil
}

// allocateContentIndex assigns an index that never collides with existing
// content blocks (chat_completions.rs:749-757).
func (a *streamAdapter) allocateContentIndex() (uint32, error) {
	if a.nextContentIndex == maxWireIndex {
		return 0, protocolError("Chat 内容块序号溢出")
	}
	index := a.nextContentIndex
	a.nextContentIndex++
	return index, nil
}

// asStringOrEmpty returns the string value or "" for anything else.
func asStringOrEmpty(value any) string {
	text, _ := asString(value)
	return text
}

// isInertUsageChoice recognizes the single placeholder choice some
// compatible gateways attach to their usage-only trailing chunk
// (chat_completions.rs:760-780).
func isInertUsageChoice(choices []any) bool {
	if len(choices) != 1 {
		return false
	}
	choice, ok := asObject(choices[0])
	if !ok {
		return false
	}
	for key := range choice {
		switch key {
		case "index", "delta", "finish_reason", "logprobs":
		default:
			return false
		}
	}
	if index, ok := asU64(choice["index"]); !ok || index != 0 {
		return false
	}
	delta, hasDelta := asObject(choice["delta"])
	if !hasDelta || len(delta) != 0 {
		return false
	}
	if reason, present := choice["finish_reason"]; present && !isNull(reason) {
		return false
	}
	if logprobs, present := choice["logprobs"]; present && !isNull(logprobs) {
		return false
	}
	return true
}
