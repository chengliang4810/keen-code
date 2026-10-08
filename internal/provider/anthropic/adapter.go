package anthropic

import (
	"encoding/json"
	"math"
	"strings"

	"keencode/internal/model"
)

// activeBlock is the wire content type of one opened, not yet stopped content
// block in an Anthropic Messages stream (messages.rs:17-27).
type activeBlock int

const (
	blockText             activeBlock = iota + 1
	blockThinking                     // displayable reasoning content
	blockRedactedThinking             // provider-encrypted or hidden reasoning
	blockToolUse                      // model-initiated tool call
)

// streamAdapter converts one Messages SSE stream (or buffered JSON response)
// into neutral model stream events. It is per-request state with no residue,
// mirroring MessagesAdapter (core/provider/src/adapters/messages.rs:29-68).
type streamAdapter struct {
	started bool
	ended   bool
	// stopReason stores the reason delivered by message_delta until the
	// terminal message_stop arrives.
	stopReason    model.StopReason
	hasStopReason bool
	// activeBlocks tracks open content blocks by remote index so missing or
	// duplicate stops are never silently accepted.
	activeBlocks map[uint32]activeBlock
	// ignoredBlocks collects block indexes skipped entirely: unknown block
	// types (server_tool_use, web_search_tool_result, …) or orphaned deltas
	// from gateways that drop content_block_start. Start/delta/stop frames of
	// these indexes are skipped instead of failing an already-billed stream.
	ignoredBlocks      map[uint32]bool
	toolCalls          map[uint32]string
	thinkingSignatures map[uint32]string
	// sawTextBlock reports whether an ordinary text block was opened.
	sawTextBlock bool
	// sawMeaningfulContent reports whether at least one meaningful content
	// event was produced in this response.
	sawMeaningfulContent bool
	sawToolCall          bool
}

// newStreamAdapter returns a fresh per-request adapter.
func newStreamAdapter() *streamAdapter {
	return &streamAdapter{
		activeBlocks:       map[uint32]activeBlock{},
		ignoredBlocks:      map[uint32]bool{},
		toolCalls:          map[uint32]string{},
		thinkingSignatures: map[uint32]string{},
	}
}

// consumeSSE consumes one decoded Messages SSE frame and appends neutral
// events (messages.rs:259-311).
func (s *streamAdapter) consumeSSE(frame sseFrame, output *[]model.StreamEvent) error {
	if s.ended {
		// Drain mode: tail frames after the protocol terminal event (gateway
		// appended [DONE], keepalives, pings, late events) can no longer
		// change a completed response. Ignore them instead of failing an
		// already-billed response; genuine protocol violations happen before
		// the terminal state and are rejected below.
		return nil
	}
	if frame.data == "" && frame.event != nil && *frame.event == "ping" {
		return nil
	}
	var value map[string]json.RawMessage
	if err := json.Unmarshal([]byte(frame.data), &value); err != nil {
		return protocolError("Messages SSE data 不是有效 JSON：%v", err)
	}
	eventType := ""
	if frame.event != nil {
		eventType = *frame.event
	} else if raw, ok := value["type"]; ok {
		if text, isText := decodeString(raw); isText {
			eventType = text
		}
	}
	if eventType == "" {
		if hasExplicitProviderError(value) {
			return classifyProviderErrorPayload(value, "Messages Provider 返回未说明错误")
		}
		return protocolError("Messages SSE 缺少事件类型")
	}
	if frame.event != nil {
		if raw, ok := value["type"]; ok {
			if text, isText := decodeString(raw); isText && text != *frame.event {
				return protocolError("Messages SSE event 与 data.type 不一致")
			}
		}
	}

	switch eventType {
	case "message_start":
		return s.consumeMessageStart(value, output)
	case "content_block_start":
		return s.consumeContentStart(value, output)
	case "content_block_delta":
		return s.consumeContentDelta(value, output)
	case "content_block_stop":
		return s.consumeContentStop(value, output)
	case "message_delta":
		return s.consumeMessageDelta(value, output)
	case "message_stop":
		return s.consumeMessageStop(output)
	case "ping":
		return nil
	case "error":
		return classifyProviderErrorPayload(value, "Messages Provider 返回未说明错误")
	default:
		// Upstream additions (server tool lifecycle events) and gateway
		// custom events are skipped so a billed stream never fails on an
		// unknown event type.
		return nil
	}
}

// finishStream verifies the stream ended through message_stop
// (messages.rs:370-376).
func (s *streamAdapter) finishStream() error {
	if s.ended {
		return nil
	}
	return protocolError("Messages SSE 在 message_stop 之前关闭")
}

// consumeMessageStart handles message_start and the first usage snapshot
// (messages.rs:379-412).
func (s *streamAdapter) consumeMessageStart(value map[string]json.RawMessage, output *[]model.StreamEvent) error {
	if s.started {
		return protocolError("Messages SSE 重复 message_start")
	}
	// Gateways such as Bedrock send a message_start without a message body
	// (or an empty one); treat it as an empty start and synthesize the start
	// event so the stream keeps a valid skeleton.
	raw, ok := value["message"]
	if !ok {
		s.started = true
		*output = append(*output, model.StreamEvent{Type: model.EventMessageStart})
		return nil
	}
	var message map[string]json.RawMessage
	if json.Unmarshal(raw, &message) != nil || message == nil {
		s.started = true
		*output = append(*output, model.StreamEvent{Type: model.EventMessageStart})
		return nil
	}
	s.started = true
	*output = append(*output, model.StreamEvent{Type: model.EventMessageStart})
	if rawUsage, ok := message["usage"]; ok {
		var usage map[string]json.RawMessage
		if json.Unmarshal(rawUsage, &usage) == nil {
			*output = append(*output, model.StreamEvent{Type: model.EventUsage, Usage: decodeUsage(usage)})
		}
	}
	return nil
}

// consumeContentStart handles the content block start event and its possible
// first payload (messages.rs:415-519).
func (s *streamAdapter) consumeContentStart(value map[string]json.RawMessage, output *[]model.StreamEvent) error {
	if !s.started {
		return protocolError("Messages 内容事件早于 message_start")
	}
	index, err := requiredUint32(value, "index")
	if err != nil {
		return err
	}
	rawBlock, ok := value["content_block"]
	if !ok {
		return protocolError("content_block_start 缺少 content_block")
	}
	var block map[string]json.RawMessage
	if err := json.Unmarshal(rawBlock, &block); err != nil || block == nil {
		return protocolError("content_block_start 缺少 content_block")
	}
	blockType, err := requiredString(block, "type")
	if err != nil {
		return err
	}
	s.sawToolCall = s.sawToolCall || blockType == "tool_use"
	var kind activeBlock
	switch blockType {
	case "text":
		kind = blockText
	case "thinking":
		kind = blockThinking
	case "redacted_thinking":
		kind = blockRedactedThinking
	case "tool_use":
		kind = blockToolUse
	default:
		// server_tool_use, web_search_tool_result, document, and other server
		// blocks are skipped whole instead of failing the stream.
		s.ignoredBlocks[index] = true
		return nil
	}
	// A badly ordered stream may deliver start after orphaned deltas: lift
	// the ignore so the block's remaining frames follow the normal path.
	delete(s.ignoredBlocks, index)
	if _, exists := s.activeBlocks[index]; exists {
		return protocolError("Messages 内容块序号 %d 重复开始", index)
	}
	s.activeBlocks[index] = kind

	switch blockType {
	case "text":
		s.sawTextBlock = true
		text, err := requiredString(block, "text")
		if err != nil {
			return err
		}
		if text != "" {
			s.sawMeaningfulContent = true
			*output = append(*output, model.StreamEvent{Type: model.EventTextDelta, Index: index, Delta: text})
		}
	case "thinking":
		if text, ok := optionalString(block, "thinking"); ok && text != "" {
			s.sawMeaningfulContent = true
			*output = append(*output, model.StreamEvent{Type: model.EventReasoningDelta, Index: index, Delta: text})
		}
		if signature, ok := optionalString(block, "signature"); ok && signature != "" {
			s.thinkingSignatures[index] = signature
		}
	case "redacted_thinking":
		rawData, ok := block["data"]
		if !ok {
			return protocolError("redacted_thinking 内容块缺少不透明 data")
		}
		continuation, err := encodeContinuation(redactedStateKind, rawData)
		if err != nil {
			return err
		}
		s.sawMeaningfulContent = true
		*output = append(*output, model.StreamEvent{Type: model.EventReasoningContinuation, Index: index, Continuation: continuation})
	case "tool_use":
		id, err := requiredString(block, "id")
		if err != nil {
			return err
		}
		name, err := requiredString(block, "name")
		if err != nil {
			return err
		}
		rawInput, ok := block["input"]
		if !ok {
			return protocolError("流式 tool_use input 必须是对象")
		}
		var input map[string]any
		if err := json.Unmarshal(rawInput, &input); err != nil || input == nil {
			return protocolError("流式 tool_use input 必须是对象")
		}
		s.toolCalls[index] = id
		s.sawMeaningfulContent = true
		*output = append(*output, model.StreamEvent{Type: model.EventToolCallStart, Index: index, CallID: id, Name: name})
		if len(input) > 0 {
			delta, err := compactJSON(rawInput)
			if err != nil {
				return protocolError("tool_use input 无法编码：%v", err)
			}
			*output = append(*output, model.StreamEvent{Type: model.EventToolCallArgsDelta, Index: index, CallID: id, Delta: delta})
		}
	}
	return nil
}

// consumeContentDelta handles text, reasoning, signature, and tool argument
// deltas (messages.rs:522-599).
func (s *streamAdapter) consumeContentDelta(value map[string]json.RawMessage, output *[]model.StreamEvent) error {
	if !s.started {
		return protocolError("Messages 内容事件早于 message_start")
	}
	index, err := requiredUint32(value, "index")
	if err != nil {
		return err
	}
	rawDelta, ok := value["delta"]
	if !ok {
		return protocolError("content_block_delta 缺少 delta")
	}
	var delta map[string]json.RawMessage
	if err := json.Unmarshal(rawDelta, &delta); err != nil || delta == nil {
		return protocolError("content_block_delta 缺少 delta")
	}
	deltaType, err := requiredString(delta, "type")
	if err != nil {
		return err
	}
	if s.ignoredBlocks[index] {
		return nil
	}
	kind, open := s.activeBlocks[index]
	if !open {
		// Gateways occasionally drop content_block_start; demote the whole
		// index to an ignored block so the rest of the stream stays usable.
		// Silently dropping beats voiding an already-billed response.
		s.ignoredBlocks[index] = true
		return nil
	}
	switch deltaType {
	case "text_delta":
		if kind != blockText {
			return indexTypeError(index)
		}
		text, err := requiredString(delta, "text")
		if err != nil {
			return err
		}
		if text != "" {
			s.sawMeaningfulContent = true
			*output = append(*output, model.StreamEvent{Type: model.EventTextDelta, Index: index, Delta: text})
		}
	case "thinking_delta":
		if kind != blockThinking {
			return indexTypeError(index)
		}
		text, err := requiredString(delta, "thinking")
		if err != nil {
			return err
		}
		*output = append(*output, model.StreamEvent{Type: model.EventReasoningDelta, Index: index, Delta: text})
		// Emitted reasoning events still go to the neutral layer for
		// validation; empty same-response text must not read as "no content".
		s.sawMeaningfulContent = true
	case "signature_delta":
		if kind != blockThinking {
			return indexTypeError(index)
		}
		signature, err := requiredString(delta, "signature")
		if err != nil {
			return err
		}
		s.thinkingSignatures[index] += signature
	case "input_json_delta":
		if kind != blockToolUse {
			return indexTypeError(index)
		}
		id, open := s.toolCalls[index]
		if !open {
			return protocolError("工具参数增量的内容块 %d 尚未开始", index)
		}
		partial, err := requiredString(delta, "partial_json")
		if err != nil {
			return err
		}
		*output = append(*output, model.StreamEvent{Type: model.EventToolCallArgsDelta, Index: index, CallID: id, Delta: partial})
	default:
		// citations_delta and other unknown deltas are skipped without
		// touching the recognized channels.
		return nil
	}
	return nil
}

// consumeContentStop finalizes a tool call or commits the full reasoning
// signature (messages.rs:602-636).
func (s *streamAdapter) consumeContentStop(value map[string]json.RawMessage, output *[]model.StreamEvent) error {
	if !s.started {
		return protocolError("Messages 内容事件早于 message_start")
	}
	index, err := requiredUint32(value, "index")
	if err != nil {
		return err
	}
	if s.ignoredBlocks[index] {
		delete(s.ignoredBlocks, index)
		return nil
	}
	kind, open := s.activeBlocks[index]
	if !open {
		return protocolError("内容块 %d 尚未开始或已结束", index)
	}
	delete(s.activeBlocks, index)
	if id, hasTool := s.toolCalls[index]; hasTool {
		if kind != blockToolUse {
			return indexTypeError(index)
		}
		delete(s.toolCalls, index)
		*output = append(*output, model.StreamEvent{Type: model.EventToolCallEnd, Index: index, CallID: id})
	}
	if signature, hasSignature := s.thinkingSignatures[index]; hasSignature {
		if kind != blockThinking {
			return indexTypeError(index)
		}
		delete(s.thinkingSignatures, index)
		continuation, err := encodeSignatureContinuation(signature)
		if err != nil {
			return err
		}
		*output = append(*output, model.StreamEvent{Type: model.EventReasoningContinuation, Index: index, Continuation: continuation})
		s.sawMeaningfulContent = true
	}
	return nil
}

// consumeMessageDelta stores the stop reason and merges incremental usage
// (messages.rs:639-658).
func (s *streamAdapter) consumeMessageDelta(value map[string]json.RawMessage, output *[]model.StreamEvent) error {
	if !s.started {
		return protocolError("Messages 内容事件早于 message_start")
	}
	if rawDelta, ok := value["delta"]; ok {
		var delta map[string]json.RawMessage
		if json.Unmarshal(rawDelta, &delta) == nil && delta != nil {
			if raw, ok := delta["stop_reason"]; ok {
				if text, isText := decodeString(raw); isText {
					s.stopReason = mapStopReason(&text)
					s.hasStopReason = true
				}
			}
		}
	}
	if rawUsage, ok := value["usage"]; ok {
		var usage map[string]json.RawMessage
		if json.Unmarshal(rawUsage, &usage) == nil {
			*output = append(*output, model.StreamEvent{Type: model.EventUsage, Usage: decodeUsage(usage)})
		}
	}
	return nil
}

// consumeMessageStop produces the single response end event
// (messages.rs:661-685). Unrecognized endpoint reasons produce an EventError
// instead of a MessageEnd per the v1 normalization policy
// (docs/go-migration.md §5.1).
func (s *streamAdapter) consumeMessageStop(output *[]model.StreamEvent) error {
	if !s.started {
		return protocolError("Messages 内容事件早于 message_start")
	}
	if len(s.activeBlocks) > 0 || len(s.toolCalls) > 0 || len(s.thinkingSignatures) > 0 {
		return protocolError("Messages message_stop 前仍有未结束内容块")
	}
	if s.sawTextBlock && !s.sawMeaningfulContent {
		return protocolError("Messages 响应不能只有空文本内容")
	}
	stopReason := s.stopReason
	if !s.hasStopReason {
		stopReason = mapStopReason(nil)
	}
	if err := validateToolStopReason(stopReason, s.sawToolCall); err != nil {
		return err
	}
	// An unrecognized reason (including the native refusal and pause_turn)
	// must not silently become a MessageEnd: fail the stream with the
	// sanitized reason text instead.
	if !stopReason.IsValid() {
		return protocolError("Messages 未识别的结束原因 %s", string(stopReason))
	}
	*output = append(*output, model.StreamEvent{Type: model.EventMessageEnd, StopReason: stopReason})
	s.ended = true
	return nil
}

// decodeJSON converts a non-streaming Messages JSON response into the full
// neutral event sequence (messages.rs:314-367). The receiver state mirrors
// the Rust adapter's decode_json bookkeeping.
func (s *streamAdapter) decodeJSON(body []byte) ([]model.StreamEvent, error) {
	var value map[string]json.RawMessage
	if err := json.Unmarshal(body, &value); err != nil || value == nil {
		return nil, protocolError("Messages 响应必须是 JSON 对象")
	}
	if raw, ok := value["type"]; ok {
		if text, isText := decodeString(raw); isText && text == "error" {
			return nil, classifyProviderErrorPayload(value, "Messages Provider 返回未说明错误")
		}
	}
	if raw, ok := value["error"]; ok && string(raw) != "null" {
		// The buffered JSON path treats any non-null error object as a
		// provider error, mirroring the Rust decode_json check.
		return nil, classifyProviderErrorPayload(value, "Messages Provider 返回未说明错误")
	}

	rawContent, ok := value["content"]
	if !ok {
		return nil, protocolError("Messages 响应缺少 content 数组")
	}
	var content []json.RawMessage
	if err := json.Unmarshal(rawContent, &content); err != nil {
		return nil, protocolError("Messages 响应缺少 content 数组")
	}

	events := []model.StreamEvent{{Type: model.EventMessageStart}}
	s.started = true
	sawTextBlock := false
	sawMeaningfulContent := false
	for position, block := range content {
		if position > math.MaxUint32 {
			return nil, protocolError("Messages 内容块数量超过 u32 范围")
		}
		index := uint32(position)
		var probe map[string]json.RawMessage
		if json.Unmarshal(block, &probe) == nil {
			if text, isText := optionalString(probe, "type"); isText && text == "text" {
				sawTextBlock = true
			}
		}
		count := len(events)
		if err := decodeCompleteContent(index, block, &events); err != nil {
			return nil, err
		}
		sawMeaningfulContent = sawMeaningfulContent || len(events) > count
	}
	if sawTextBlock && !sawMeaningfulContent {
		return nil, protocolError("Messages 响应不能只有空文本内容")
	}
	if rawUsage, ok := value["usage"]; ok {
		var usage map[string]json.RawMessage
		if json.Unmarshal(rawUsage, &usage) == nil {
			events = append(events, model.StreamEvent{Type: model.EventUsage, Usage: decodeUsage(usage)})
		}
	}
	stopReason := mapStopReason(optionalJSONString(value, "stop_reason"))
	sawToolCall := false
	for _, block := range content {
		var probe map[string]json.RawMessage
		if json.Unmarshal(block, &probe) == nil {
			if text, isText := optionalString(probe, "type"); isText && text == "tool_use" {
				sawToolCall = true
			}
		}
	}
	if err := validateToolStopReason(stopReason, sawToolCall); err != nil {
		return nil, err
	}
	if !stopReason.IsValid() {
		return nil, protocolError("Messages 未识别的结束原因 %s", string(stopReason))
	}
	events = append(events, model.StreamEvent{Type: model.EventMessageEnd, StopReason: stopReason})
	s.ended = true
	return events, nil
}

// decodeCompleteContent converts one complete Messages content block into
// neutral events (messages.rs:898-969).
func decodeCompleteContent(index uint32, block []byte, events *[]model.StreamEvent) error {
	var object map[string]json.RawMessage
	if err := json.Unmarshal(block, &object); err != nil || object == nil {
		return protocolError("Messages content 元素必须是对象")
	}
	blockType, err := requiredString(object, "type")
	if err != nil {
		return err
	}
	switch blockType {
	case "text":
		text, err := requiredString(object, "text")
		if err != nil {
			return err
		}
		if text != "" {
			*events = append(*events, model.StreamEvent{Type: model.EventTextDelta, Index: index, Delta: text})
		}
	case "thinking":
		text, err := requiredString(object, "thinking")
		if err != nil {
			return err
		}
		*events = append(*events, model.StreamEvent{Type: model.EventReasoningDelta, Index: index, Delta: text})
		if signature, ok := optionalString(object, "signature"); ok {
			continuation, err := encodeSignatureContinuation(signature)
			if err != nil {
				return err
			}
			*events = append(*events, model.StreamEvent{Type: model.EventReasoningContinuation, Index: index, Continuation: continuation})
		}
	case "redacted_thinking":
		rawData, ok := object["data"]
		if !ok {
			return protocolError("redacted_thinking 缺少 data")
		}
		continuation, err := encodeContinuation(redactedStateKind, rawData)
		if err != nil {
			return err
		}
		*events = append(*events, model.StreamEvent{Type: model.EventReasoningContinuation, Index: index, Continuation: continuation})
	case "tool_use":
		id, err := requiredString(object, "id")
		if err != nil {
			return err
		}
		name, err := requiredString(object, "name")
		if err != nil {
			return err
		}
		rawInput, ok := object["input"]
		if !ok {
			return protocolError("tool_use 缺少 input")
		}
		delta, err := compactJSON(rawInput)
		if err != nil {
			return protocolError("tool_use input 无法编码：%v", err)
		}
		*events = append(*events, model.StreamEvent{Type: model.EventToolCallStart, Index: index, CallID: id, Name: name})
		*events = append(*events, model.StreamEvent{Type: model.EventToolCallArgsDelta, Index: index, CallID: id, Delta: delta})
		*events = append(*events, model.StreamEvent{Type: model.EventToolCallEnd, Index: index, CallID: id})
	default:
		return protocolError("Messages 包含未知完整内容块 %s", blockType)
	}
	return nil
}

// mapStopReason maps a Messages stop reason onto the neutral enum
// (messages.rs:1004-1017). A nil reason and unrecognized reasons become the
// raw reason string; the caller turns invalid values into an EventError per
// the v1 normalization policy. The native refusal reason is unrecognized in
// the v1 model (internal/model stream_test.go denies the "refusal" value), so
// it flows through the same EventError path.
func mapStopReason(reason *string) model.StopReason {
	if reason == nil {
		return model.StopReason("missing_stop_reason")
	}
	switch *reason {
	case "end_turn", "stop_sequence":
		return model.StopEndTurn
	case "tool_use":
		return model.StopToolUse
	case "max_tokens", "model_context_window_exceeded":
		return model.StopMaxTokens
	default:
		return model.StopReason(sanitizeStopReason(*reason))
	}
}

// sanitizeStopReason bounds a raw endpoint reason for error display
// (core/model/src/usage.rs:7 MAX_FAILURE_REASON_BYTES).
func sanitizeStopReason(reason string) string {
	trimmed := strings.TrimSpace(reason)
	if trimmed == "" {
		return "missing_stop_reason"
	}
	return model.RedactErrorSecretsBounded(trimmed, 256)
}

// validateToolStopReason rejects server-side pause signals as grounds for
// committing client tool side effects (messages.rs:996-1001).
func validateToolStopReason(reason model.StopReason, sawToolCall bool) error {
	if sawToolCall && reason == "pause_turn" {
		return protocolError("Messages 暂停响应不能提交客户端工具调用")
	}
	return nil
}

// requiredString reads a required string field from a JSON object
// (core/provider/src/adapters/wire.rs:38-47).
func requiredString(object map[string]json.RawMessage, field string) (string, error) {
	if raw, ok := object[field]; ok {
		if text, isText := decodeString(raw); isText {
			return text, nil
		}
	}
	return "", protocolError("Messages 字段 %s 必须是字符串", field)
}

// optionalString reads an optional string field.
func optionalString(object map[string]json.RawMessage, field string) (string, bool) {
	if raw, ok := object[field]; ok {
		if text, isText := decodeString(raw); isText {
			return text, true
		}
	}
	return "", false
}

// optionalJSONString reads an optional top-level JSON string field
// (messages.rs:1043-1045).
func optionalJSONString(object map[string]json.RawMessage, field string) *string {
	if text, ok := optionalString(object, field); ok {
		return &text
	}
	return nil
}

// decodeString decodes a JSON string raw value.
func decodeString(raw json.RawMessage) (string, bool) {
	var text string
	if err := json.Unmarshal(raw, &text); err != nil {
		return "", false
	}
	return text, true
}

// requiredUint32 reads a required top-level non-negative integer that fits
// u32 (core/provider/src/adapters/wire.rs:50-57).
func requiredUint32(object map[string]json.RawMessage, field string) (uint32, error) {
	raw, ok := object[field]
	if !ok {
		return 0, protocolError("Messages 字段 %s 必须是非负整数", field)
	}
	var number float64
	if err := json.Unmarshal(raw, &number); err != nil || number < 0 || number != math.Trunc(number) {
		return 0, protocolError("Messages 字段 %s 必须是非负整数", field)
	}
	if number > math.MaxUint32 {
		return 0, protocolError("Messages 字段 %s 超过 u32 范围", field)
	}
	return uint32(number), nil
}

// indexTypeError is the shared index/type conflict protocol error
// (messages.rs:1053-1055).
func indexTypeError(index uint32) error {
	return protocolError("内容块序号 %d 被用于不同内容类型", index)
}
