package model

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"
)

// Role is the semantic role of a message in the conversation. The Go v1 model
// drops the Rust MessageRole::Tool variant: tool results are carried as
// ToolResultBlocks inside a RoleUser message that immediately follows the
// assistant turn (docs/go-migration.md §5.1).
type Role string

const (
	// RoleSystem marks system-level instructions constraining the whole call.
	RoleSystem Role = "system"
	// RoleDeveloper marks developer constraints injected by the application
	// with priority over ordinary user input.
	RoleDeveloper Role = "developer"
	// RoleUser marks input provided by the user or by the runtime on the
	// user's behalf. Tool results are delivered in RoleUser messages that
	// contain only ToolResultBlocks.
	RoleUser Role = "user"
	// RoleAssistant marks model-generated text, reasoning, or tool calls.
	RoleAssistant Role = "assistant"
)

// Stable JSON type tags of the concrete ContentBlock implementations. The
// journal persists every block as a tagged object; the tag selects the
// concrete type when replaying.
const (
	BlockTypeText       = "text"
	BlockTypeReasoning  = "reasoning"
	BlockTypeToolCall   = "tool_call"
	BlockTypeToolResult = "tool_result"
)

// MaxToolNameBytes is the maximum ASCII byte length of a tool name that
// round trips losslessly across all target model protocols
// (core/model/src/tool.rs:7).
const MaxToolNameBytes = 64

// ContentBlock is one ordered item of message content. The interface is
// sealed: only the four concrete block types below (text, reasoning, tool
// call, tool result) can implement it, enforced by the unexported isBlock
// method. Discriminate with a type switch or BlockType.
type ContentBlock interface {
	// isBlock seals the interface to this package's implementations.
	isBlock()
	// BlockType returns the stable JSON type tag of the block.
	BlockType() string
	// Validate reports whether the block satisfies the unified layer
	// invariants.
	Validate() error
}

// TextBlock is plain text content (Rust ContentBlock::Text).
type TextBlock struct {
	Text string `json:"text"`
}

// ReasoningBlock is model reasoning content. Signature carries the opaque
// continuation state returned by providers that require it (Anthropic
// thinking signatures): the runtime only persists and echoes it back
// verbatim; providers that have no signature mechanism leave it empty
// (Rust ReasoningContent.text + continuation collapsed per
// docs/go-migration.md §5.1).
type ReasoningBlock struct {
	Text      string `json:"text"`
	Signature string `json:"signature,omitempty"`
}

// ToolCallBlock is a tool call initiated by the model
// (Rust ContentBlock::ToolCall).
type ToolCallBlock struct {
	Call ToolCall `json:"call"`
}

// ToolResultBlock is the result of a completed tool execution
// (Rust ContentBlock::ToolResult).
type ToolResultBlock struct {
	Result ToolResult `json:"result"`
}

func (TextBlock) isBlock()       {}
func (ReasoningBlock) isBlock()  {}
func (ToolCallBlock) isBlock()   {}
func (ToolResultBlock) isBlock() {}

// BlockType implements ContentBlock.
func (TextBlock) BlockType() string { return BlockTypeText }

// BlockType implements ContentBlock.
func (ReasoningBlock) BlockType() string { return BlockTypeReasoning }

// BlockType implements ContentBlock.
func (ToolCallBlock) BlockType() string { return BlockTypeToolCall }

// BlockType implements ContentBlock.
func (ToolResultBlock) BlockType() string { return BlockTypeToolResult }

// Validate implements ContentBlock. Whitespace-only text is valid, mirroring
// the Rust message validation tests (empty rejected, whitespace preserved).
func (b TextBlock) Validate() error {
	if b.Text == "" {
		return InvalidRequest("文本内容块不能是空字符串")
	}
	return nil
}

// Validate implements ContentBlock. At least one of text or signature must be
// present (Rust ReasoningContent::validate: at least one payload; the v1 Go
// shape has no separate summary field).
func (b ReasoningBlock) Validate() error {
	if b.Text == "" && b.Signature == "" {
		return ProtocolError("推理内容至少需要文本或签名中的一项")
	}
	return nil
}

// Validate implements ContentBlock.
func (b ToolCallBlock) Validate() error { return b.Call.Validate() }

// Validate implements ContentBlock.
func (b ToolResultBlock) Validate() error { return b.Result.Validate() }

// MarshalJSON encodes the block with its stable "type" tag.
func (b TextBlock) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Type string `json:"type"`
		Text string `json:"text"`
	}{BlockTypeText, b.Text})
}

// UnmarshalJSON decodes a tagged text block.
func (b *TextBlock) UnmarshalJSON(data []byte) error {
	var wire struct {
		Type string `json:"type"`
		Text string `json:"text"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	if wire.Type != "" && wire.Type != BlockTypeText {
		return fmt.Errorf("内容块类型 %q 不是 %q", wire.Type, BlockTypeText)
	}
	b.Text = wire.Text
	return nil
}

// MarshalJSON encodes the block with its stable "type" tag.
func (b ReasoningBlock) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Type      string `json:"type"`
		Text      string `json:"text"`
		Signature string `json:"signature,omitempty"`
	}{BlockTypeReasoning, b.Text, b.Signature})
}

// UnmarshalJSON decodes a tagged reasoning block.
func (b *ReasoningBlock) UnmarshalJSON(data []byte) error {
	var wire struct {
		Type      string `json:"type"`
		Text      string `json:"text"`
		Signature string `json:"signature"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	if wire.Type != "" && wire.Type != BlockTypeReasoning {
		return fmt.Errorf("内容块类型 %q 不是 %q", wire.Type, BlockTypeReasoning)
	}
	b.Text, b.Signature = wire.Text, wire.Signature
	return nil
}

// MarshalJSON encodes the block with its stable "type" tag.
func (b ToolCallBlock) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Type string   `json:"type"`
		Call ToolCall `json:"call"`
	}{BlockTypeToolCall, b.Call})
}

// UnmarshalJSON decodes a tagged tool call block.
func (b *ToolCallBlock) UnmarshalJSON(data []byte) error {
	var wire struct {
		Type string   `json:"type"`
		Call ToolCall `json:"call"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	if wire.Type != "" && wire.Type != BlockTypeToolCall {
		return fmt.Errorf("内容块类型 %q 不是 %q", wire.Type, BlockTypeToolCall)
	}
	b.Call = wire.Call
	return nil
}

// MarshalJSON encodes the block with its stable "type" tag.
func (b ToolResultBlock) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Type   string     `json:"type"`
		Result ToolResult `json:"result"`
	}{BlockTypeToolResult, b.Result})
}

// UnmarshalJSON decodes a tagged tool result block.
func (b *ToolResultBlock) UnmarshalJSON(data []byte) error {
	var wire struct {
		Type   string     `json:"type"`
		Result ToolResult `json:"result"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	if wire.Type != "" && wire.Type != BlockTypeToolResult {
		return fmt.Errorf("内容块类型 %q 不是 %q", wire.Type, BlockTypeToolResult)
	}
	b.Result = wire.Result
	return nil
}

// ToolCall is a unified tool call produced by a model
// (core/model/src/tool.rs:86-124). Arguments keeps the raw JSON object text
// exactly as the provider streamed it; it is never re-serialized mid-flight.
// An empty argument object is normalized to "{}" by the stream collector.
type ToolCall struct {
	// ID is unique within one model response.
	ID string `json:"id"`
	// Name matches a ToolDefinition.Name in the same request.
	Name string `json:"name"`
	// Arguments is the raw JSON object text of the call arguments.
	Arguments string `json:"arguments"`
}

// Validate checks the call id, the portable tool name, and that Arguments is
// a JSON object (Rust ToolCall::validate; protocol-class because a call is
// produced by the remote side).
func (c ToolCall) Validate() error {
	if strings.TrimSpace(c.ID) == "" {
		return ProtocolError("工具调用标识不能为空")
	}
	if !isPortableName(c.Name) {
		return ProtocolError("工具调用名称不满足跨协议可移植格式")
	}
	if !isJSONObject(c.Arguments) {
		return ProtocolError("工具调用 %s 的参数必须是 JSON 对象", c.ID)
	}
	return nil
}

// ToolResult is the unified result returned to the model after a tool
// execution (core/model/src/tool.rs:143-191). The v1 Go shape carries one
// text payload; IsError marks a failed execution whose Content explains the
// error to the model.
type ToolResult struct {
	// CallID corresponds to the ToolCall.ID of the invoked tool.
	CallID string `json:"callId"`
	// Content is the text returned to the model.
	Content string `json:"content"`
	// IsError reports a failed execution.
	IsError bool `json:"isError,omitempty"`
}

// Validate checks that the result can be associated with a prior tool call
// (Rust ToolResult::validate; request-class because results are assembled
// locally).
func (r ToolResult) Validate() error {
	if strings.TrimSpace(r.CallID) == "" {
		return InvalidRequest("工具结果的调用标识不能为空")
	}
	return nil
}

// Message is one ordered provider-neutral conversation message
// (core/model/src/message.rs:227-289).
type Message struct {
	// IsMeta marks internal context that participates in requests and
	// persistence but is not shown as a user utterance.
	IsMeta bool `json:"isMeta,omitempty"`
	// Role is the semantic role of the message.
	Role Role `json:"role"`
	// Content keeps the original block order.
	Content []ContentBlock `json:"content"`
}

// TextMessage returns a message with a single text block
// (Rust Message::text).
func TextMessage(role Role, text string) Message {
	return Message{Role: role, Content: []ContentBlock{TextBlock{Text: text}}}
}

// ToolResultMessage returns a RoleUser message carrying only tool result
// blocks in call order. Per docs/go-migration.md §5.1 such a message must
// immediately follow the assistant message that requested the calls.
func ToolResultMessage(results ...ToolResult) Message {
	blocks := make([]ContentBlock, 0, len(results))
	for _, result := range results {
		blocks = append(blocks, ToolResultBlock{Result: result})
	}
	return Message{Role: RoleUser, Content: blocks}
}

// MarshalJSON encodes the message with tagged content blocks so the concrete
// block types survive a round trip (journal persistence).
func (m Message) MarshalJSON() ([]byte, error) {
	content, err := MarshalContentBlocks(m.Content)
	if err != nil {
		return nil, err
	}
	return json.Marshal(struct {
		IsMeta  bool            `json:"isMeta,omitempty"`
		Role    Role            `json:"role"`
		Content json.RawMessage `json:"content"`
	}{m.IsMeta, m.Role, content})
}

// UnmarshalJSON decodes a message and restores the concrete content block
// types from their tags. Unknown block types fail closed.
func (m *Message) UnmarshalJSON(data []byte) error {
	var wire struct {
		IsMeta  bool            `json:"isMeta"`
		Role    Role            `json:"role"`
		Content json.RawMessage `json:"content"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	blocks, err := UnmarshalContentBlocks(wire.Content)
	if err != nil {
		return err
	}
	m.IsMeta, m.Role, m.Content = wire.IsMeta, wire.Role, blocks
	return nil
}

// Validate requires at least one block, every block valid, and block types
// allowed for the role (Rust Message::validate, adapted: user messages carry
// either only text blocks or only tool result blocks — the merged shape of
// the dropped Tool role).
func (m Message) Validate() error {
	if len(m.Content) == 0 {
		return InvalidRequest("消息内容不能为空")
	}
	for _, block := range m.Content {
		if block == nil {
			return InvalidRequest("消息内容不能包含空内容块")
		}
		if err := block.Validate(); err != nil {
			return err
		}
	}
	switch m.Role {
	case RoleSystem, RoleDeveloper:
		if !allBlocksOfType(m.Content, BlockTypeText) {
			return InvalidRequest("消息角色 %q 只允许文本内容块", m.Role)
		}
	case RoleUser:
		if !allBlocksOfType(m.Content, BlockTypeText) && !allBlocksOfType(m.Content, BlockTypeToolResult) {
			return InvalidRequest("用户消息不能混合文本与工具结果内容块")
		}
	case RoleAssistant:
		for _, block := range m.Content {
			switch block.BlockType() {
			case BlockTypeText, BlockTypeReasoning, BlockTypeToolCall:
			default:
				return InvalidRequest("消息角色 %q 包含了不允许的内容类型 %q", m.Role, block.BlockType())
			}
		}
	default:
		return InvalidRequest("消息角色 %q 不受支持", m.Role)
	}
	return nil
}

// LastNonEmptyText returns the last non-blank plain text of ordered model
// content. Providers may return several text blocks in one response; a final
// summary must only represent the last assistant body, never a concatenation
// of earlier blocks. OK is false when no such block exists
// (Rust last_non_empty_text, core/model/src/message.rs:219-224).
func LastNonEmptyText(content []ContentBlock) (string, bool) {
	for i := len(content) - 1; i >= 0; i-- {
		if block, ok := content[i].(TextBlock); ok && strings.TrimSpace(block.Text) != "" {
			return block.Text, true
		}
	}
	return "", false
}

// MarshalContentBlocks encodes ordered content blocks as a JSON array of
// tagged objects. Nil encodes as null.
func MarshalContentBlocks(blocks []ContentBlock) ([]byte, error) {
	if blocks == nil {
		return []byte("null"), nil
	}
	var buf bytes.Buffer
	buf.WriteByte('[')
	for i, block := range blocks {
		if block == nil {
			return nil, InvalidRequest("内容块数组不能包含空项")
		}
		if i > 0 {
			buf.WriteByte(',')
		}
		data, err := json.Marshal(block)
		if err != nil {
			return nil, err
		}
		buf.Write(data)
	}
	buf.WriteByte(']')
	return buf.Bytes(), nil
}

// UnmarshalContentBlocks decodes a JSON array of tagged content blocks and
// restores the concrete block types. Unknown or missing type tags fail
// closed instead of silently dropping content.
func UnmarshalContentBlocks(data []byte) ([]ContentBlock, error) {
	if len(data) == 0 || string(data) == "null" {
		return nil, nil
	}
	var raw []json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	if raw == nil {
		return nil, nil
	}
	blocks := make([]ContentBlock, 0, len(raw))
	for i, item := range raw {
		var tag struct {
			Type string `json:"type"`
		}
		if err := json.Unmarshal(item, &tag); err != nil {
			return nil, fmt.Errorf("内容块 %d: %w", i, err)
		}
		switch tag.Type {
		case BlockTypeText:
			var block TextBlock
			if err := json.Unmarshal(item, &block); err != nil {
				return nil, fmt.Errorf("内容块 %d: %w", i, err)
			}
			blocks = append(blocks, block)
		case BlockTypeReasoning:
			var block ReasoningBlock
			if err := json.Unmarshal(item, &block); err != nil {
				return nil, fmt.Errorf("内容块 %d: %w", i, err)
			}
			blocks = append(blocks, block)
		case BlockTypeToolCall:
			var block ToolCallBlock
			if err := json.Unmarshal(item, &block); err != nil {
				return nil, fmt.Errorf("内容块 %d: %w", i, err)
			}
			blocks = append(blocks, block)
		case BlockTypeToolResult:
			var block ToolResultBlock
			if err := json.Unmarshal(item, &block); err != nil {
				return nil, fmt.Errorf("内容块 %d: %w", i, err)
			}
			blocks = append(blocks, block)
		default:
			return nil, InvalidRequest("内容块 %d 的类型 %q 不受支持", i, tag.Type)
		}
	}
	return blocks, nil
}

// allBlocksOfType reports whether every block has the given type tag.
func allBlocksOfType(blocks []ContentBlock, blockType string) bool {
	for _, block := range blocks {
		if block.BlockType() != blockType {
			return false
		}
	}
	return true
}

// isPortableName reports whether a tool name survives all target model
// protocols unchanged: 1..64 bytes of ASCII letters, digits, underscore, or
// hyphen (core/model/src/tool.rs:10-16).
func isPortableName(name string) bool {
	if name == "" || len(name) > MaxToolNameBytes {
		return false
	}
	for i := 0; i < len(name); i++ {
		b := name[i]
		if !('a' <= b && b <= 'z' || 'A' <= b && b <= 'Z' || '0' <= b && b <= '9' || b == '_' || b == '-') {
			return false
		}
	}
	return true
}

// isJSONObject reports whether text is a non-empty JSON object document.
func isJSONObject(text string) bool {
	trimmed := strings.TrimSpace(text)
	if trimmed == "" {
		return false
	}
	var decoded map[string]any
	return json.Unmarshal([]byte(trimmed), &decoded) == nil && decoded != nil
}
