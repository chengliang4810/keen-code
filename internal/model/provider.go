package model

import (
	"context"
	"encoding/json"
	"sort"
	"strings"
)

// Capabilities is the capability snapshot of one model on a provider
// instance (docs/go-migration.md §5.2).
type Capabilities struct {
	// Reasoning reports whether the model returns reasoning content.
	Reasoning bool `json:"reasoning"`
	// ReasoningEfforts lists the supported reasoning effort levels.
	ReasoningEfforts []string `json:"reasoningEfforts,omitempty"`
	// ContextWindow is the known maximum context tokens; 0 when unknown.
	ContextWindow int64 `json:"contextWindow,omitempty"`
	// MaxOutputTokens is the known maximum output tokens; 0 when unknown.
	MaxOutputTokens int64 `json:"maxOutputTokens,omitempty"`
}

// Provider is the single provider-neutral boundary through which the agent
// runtime calls models (docs/go-migration.md §5.2; Rust ModelProvider).
//
// Implementations are the protocol adapters; they must never mutate req.
// Cancelling ctx must terminate the HTTP connection, emit one EventError,
// and close the event channel.
type Provider interface {
	// Capabilities returns the capability snapshot of the given model on
	// this provider instance.
	Capabilities(model string) Capabilities
	// Stream validates the request and starts one streaming call. A failed
	// validation returns an error immediately; on success it returns an
	// event channel that closes after EventMessageEnd or EventError.
	Stream(ctx context.Context, req ModelRequest) (<-chan StreamEvent, error)
}

// pendingBlock accumulates one content block of a streaming response inside
// Complete, keyed by its stable index.
type pendingBlock struct {
	kind       string // BlockTypeText, BlockTypeReasoning, or BlockTypeToolCall
	text       string
	summary    string // typing only; v1 ReasoningBlock has no summary field
	signature  string
	toolCallID string
	toolName   string
	arguments  string
	toolEnded  bool
}

// Complete runs one streaming call on p and assembles the unified events
// into a full response. It is the Go equivalent of the Rust
// collect_model_stream (core/model/src/stream.rs:114-391): event order is
// validated strictly, and a stream cut before the terminal event returns an
// ErrorStreamInterrupted carrying the text already streamed so callers can
// persist it into history.
func Complete(ctx context.Context, p Provider, req ModelRequest) (ModelResponse, error) {
	events, err := p.Stream(ctx, req)
	if err != nil {
		return ModelResponse{}, err
	}

	state := &streamCollector{
		usage:  UnknownUsage(),
		blocks: make(map[uint32]*pendingBlock),
		ids:    make(map[string]bool),
	}

collect:
	for {
		select {
		case <-ctx.Done():
			// Keep draining so a well-behaved adapter is never blocked on
			// send after it observed the cancellation.
			go func() {
				for range events {
				}
			}()
			return ModelResponse{}, CancelledError(ctx.Err().Error())
		case event, ok := <-events:
			if !ok {
				break collect
			}
			if event.Type == EventError {
				if event.Err != nil {
					return ModelResponse{}, event.Err
				}
				return ModelResponse{}, ProtocolError("适配器发出未携带错误值的错误事件")
			}
			if err := state.apply(event); err != nil {
				return ModelResponse{}, err
			}
		}
	}
	return state.response(req.Model)
}

// streamCollector validates event order and accumulates block state for one
// response.
type streamCollector struct {
	started    bool
	ended      bool
	stopReason StopReason
	usage      TokenUsage
	blocks     map[uint32]*pendingBlock
	ids        map[string]bool
}

// apply ingests one event, mirroring the match arms of collect_model_stream.
func (c *streamCollector) apply(event StreamEvent) error {
	if c.ended {
		return ProtocolError("响应结束后仍收到事件")
	}
	switch event.Type {
	case EventMessageStart:
		if c.started {
			return ProtocolError("一个响应只能包含一次开始事件")
		}
		c.started = true
		return nil
	case EventTextDelta:
		if err := c.requireStarted(); err != nil {
			return err
		}
		block := c.blockAt(event.Index, BlockTypeText)
		if block.kind != BlockTypeText {
			return indexTypeError(event.Index)
		}
		block.text += event.Delta
		return nil
	case EventReasoningDelta, EventReasoningSummaryDelta:
		if err := c.requireStarted(); err != nil {
			return err
		}
		block := c.blockAt(event.Index, BlockTypeReasoning)
		if block.kind != BlockTypeReasoning {
			return indexTypeError(event.Index)
		}
		if event.Type == EventReasoningDelta {
			block.text += event.Delta
		} else {
			// Summary text is transient UI state in v1; tracked only so the
			// block keeps a reasoning typing, dropped at assembly.
			block.summary += event.Delta
		}
		return nil
	case EventReasoningContinuation:
		if err := c.requireStarted(); err != nil {
			return err
		}
		if strings.TrimSpace(event.Continuation) == "" {
			return ProtocolError("推理续传状态不能为空")
		}
		block := c.blockAt(event.Index, BlockTypeReasoning)
		if block.kind != BlockTypeReasoning {
			return indexTypeError(event.Index)
		}
		if block.signature != "" {
			return ProtocolError("内容块 %d 的推理续传状态重复", event.Index)
		}
		block.signature = event.Continuation
		return nil
	case EventToolCallStart:
		if err := c.requireStarted(); err != nil {
			return err
		}
		if strings.TrimSpace(event.CallID) == "" || strings.TrimSpace(event.Name) == "" {
			return ProtocolError("工具调用标识和名称不能为空")
		}
		if c.ids[event.CallID] {
			return ProtocolError("工具调用标识 %s 在响应中重复", event.CallID)
		}
		if _, exists := c.blocks[event.Index]; exists {
			return ProtocolError("内容块序号 %d 重复开始", event.Index)
		}
		c.ids[event.CallID] = true
		c.blocks[event.Index] = &pendingBlock{
			kind:       BlockTypeToolCall,
			toolCallID: event.CallID,
			toolName:   event.Name,
		}
		return nil
	case EventToolCallArgsDelta:
		if err := c.requireStarted(); err != nil {
			return err
		}
		block, exists := c.blocks[event.Index]
		if !exists {
			return ProtocolError("工具调用 %s 尚未开始", event.CallID)
		}
		if block.kind != BlockTypeToolCall {
			return indexTypeError(event.Index)
		}
		if block.toolCallID != event.CallID {
			return ProtocolError("内容块 %d 的工具调用标识不一致", event.Index)
		}
		if block.toolEnded {
			return ProtocolError("工具调用 %s 结束后仍收到参数增量", event.CallID)
		}
		block.arguments += event.Delta
		return nil
	case EventToolCallEnd:
		if err := c.requireStarted(); err != nil {
			return err
		}
		block, exists := c.blocks[event.Index]
		if !exists {
			return ProtocolError("工具调用 %s 尚未开始", event.CallID)
		}
		if block.kind != BlockTypeToolCall {
			return indexTypeError(event.Index)
		}
		if block.toolCallID != event.CallID {
			return ProtocolError("内容块 %d 的工具调用标识不一致", event.Index)
		}
		if block.toolEnded {
			return ProtocolError("工具调用 %s 重复结束", event.CallID)
		}
		block.toolEnded = true
		return nil
	case EventUsage:
		if err := c.requireStarted(); err != nil {
			return err
		}
		c.usage.UpdateFrom(event.Usage)
		return nil
	case EventMessageEnd:
		if err := c.requireStarted(); err != nil {
			return err
		}
		if !event.StopReason.IsValid() {
			// Adapters must convert unrecognized endpoint reasons into an
			// EventError with sanitized reason text; a MessageEnd may only
			// carry the unified reasons.
			return ProtocolError("响应携带未识别的结束原因 %q", string(event.StopReason))
		}
		c.ended = true
		c.stopReason = event.StopReason
		return nil
	default:
		return ProtocolError("未知的流事件类型 %q", string(event.Type))
	}
}

// response assembles the accumulated state into a ModelResponse after the
// event channel closed, mirroring the finalization of collect_model_stream.
func (c *streamCollector) response(model string) (ModelResponse, error) {
	if !c.started {
		return ModelResponse{}, interrupted("事件流在响应开始事件之前关闭", "")
	}
	if !c.ended {
		return ModelResponse{}, interrupted("事件流在响应结束事件之前关闭", c.partialStreamText())
	}

	indices := make([]uint32, 0, len(c.blocks))
	for index := range c.blocks {
		indices = append(indices, index)
	}
	sort.Slice(indices, func(i, j int) bool { return indices[i] < indices[j] })

	content := make([]ContentBlock, 0, len(indices))
	for _, index := range indices {
		block := c.blocks[index]
		switch block.kind {
		case BlockTypeText:
			if block.text == "" {
				return ModelResponse{}, ProtocolError("内容块 %d 的文本不能是空字符串", index)
			}
			content = append(content, TextBlock{Text: block.text})
		case BlockTypeReasoning:
			if block.text == "" && block.signature == "" {
				// v1 ReasoningBlock carries no summary field, so a summary
				// only payload has no representation and fails closed.
				return ModelResponse{}, ProtocolError("内容块 %d 的推理内容缺少文本或签名", index)
			}
			content = append(content, ReasoningBlock{Text: block.text, Signature: block.signature})
		case BlockTypeToolCall:
			assembled, err := c.assembleToolCall(index, block)
			if err != nil {
				return ModelResponse{}, err
			}
			if assembled != nil {
				content = append(content, *assembled)
			}
		}
	}
	return ModelResponse{
		Content:    content,
		StopReason: c.stopReason,
		Usage:      c.usage,
		Model:      model,
	}, nil
}

// assembleToolCall finalizes one tool call block. Truncated tails (unfinished
// or invalid argument JSON) under max_tokens/content_filter/cancelled are
// stripped instead of failing the response
// (core/model/src/stream.rs:349-384). A nil block means "stripped".
func (c *streamCollector) assembleToolCall(index uint32, block *pendingBlock) (*ToolCallBlock, error) {
	truncatedTail := c.stopReason.permitsTruncatedTail()
	if !block.toolEnded {
		if truncatedTail {
			return nil, nil
		}
		return nil, ProtocolError("内容块 %d 的工具调用未结束", index)
	}
	call := ToolCall{ID: block.toolCallID, Name: block.toolName, Arguments: block.arguments}
	if strings.TrimSpace(call.Arguments) == "" {
		call.Arguments = "{}"
	} else if !json.Valid([]byte(call.Arguments)) {
		if truncatedTail {
			return nil, nil
		}
		return nil, ProtocolError("工具调用 %s 的参数不是有效 JSON", call.ID)
	}
	if err := call.Validate(); err != nil {
		return nil, err
	}
	return &ToolCallBlock{Call: call}, nil
}

// partialStreamText concatenates the confirmed text and reasoning payloads;
// truncated tool arguments are unsafe to resume or account for and are
// excluded (core/model/src/stream.rs:397-411).
func (c *streamCollector) partialStreamText() string {
	indices := make([]uint32, 0, len(c.blocks))
	for index := range c.blocks {
		indices = append(indices, index)
	}
	sort.Slice(indices, func(i, j int) bool { return indices[i] < indices[j] })
	var partial strings.Builder
	for _, index := range indices {
		switch c.blocks[index].kind {
		case BlockTypeText:
			partial.WriteString(c.blocks[index].text)
		case BlockTypeReasoning:
			partial.WriteString(c.blocks[index].text)
		}
	}
	return partial.String()
}

// blockAt returns the pending block at index, creating one of the given kind
// when absent.
func (c *streamCollector) blockAt(index uint32, kind string) *pendingBlock {
	block, exists := c.blocks[index]
	if !exists {
		block = &pendingBlock{kind: kind}
		c.blocks[index] = block
	}
	return block
}

// requireStarted rejects content events before MessageStart.
func (c *streamCollector) requireStarted() error {
	if !c.started {
		return ProtocolError("响应开始事件之前收到内容事件")
	}
	return nil
}

// indexTypeError is the shared index/type conflict error.
func indexTypeError(index uint32) error {
	return ProtocolError("内容块序号 %d 被用于不同内容类型", index)
}

// interrupted builds a retryable stream interruption with optional partial
// text.
func interrupted(message, partialText string) *ModelError {
	err := &ModelError{Kind: ErrorStreamInterrupted, Message: message, Retryable: true}
	return err.WithPartialText(partialText)
}
