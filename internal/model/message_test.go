package model

import (
	"encoding/json"
	"errors"
	"reflect"
	"strings"
	"testing"
)

func TestMessageJSONRoundTripKeepsBlockTypes(t *testing.T) {
	tests := []struct {
		name    string
		message Message
	}{
		{
			name:    "text only",
			message: TextMessage(RoleUser, "你好，世界"),
		},
		{
			name: "assistant with reasoning, text, and tool calls",
			message: Message{
				Role: RoleAssistant,
				Content: []ContentBlock{
					ReasoningBlock{Text: "先想想", Signature: "opaque-signature"},
					TextBlock{Text: "正文"},
					ToolCallBlock{Call: ToolCall{ID: "call-1", Name: "read_file", Arguments: `{"path":"main.go"}`}},
					ToolCallBlock{Call: ToolCall{ID: "call-2", Name: "bash", Arguments: `{}`}},
				},
			},
		},
		{
			name:    "user message with tool results",
			message: ToolResultMessage(ToolResult{CallID: "call-1", Content: "文件内容"}, ToolResult{CallID: "call-2", Content: "失败", IsError: true}),
		},
		{
			name: "meta system message",
			message: Message{
				IsMeta:  true,
				Role:    RoleSystem,
				Content: []ContentBlock{TextBlock{Text: "系统指令"}},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data, err := json.Marshal(tt.message)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			var decoded Message
			if err := json.Unmarshal(data, &decoded); err != nil {
				t.Fatalf("unmarshal: %v", err)
			}
			if !reflect.DeepEqual(decoded, tt.message) {
				t.Fatalf("round trip mismatch:\n want %+v\n got  %+v", tt.message, decoded)
			}
		})
	}
}

func TestMessageWireCarriesTypeTags(t *testing.T) {
	message := Message{
		Role: RoleAssistant,
		Content: []ContentBlock{
			ReasoningBlock{Text: "推理", Signature: "sig"},
			TextBlock{Text: "正文"},
			ToolCallBlock{Call: ToolCall{ID: "c1", Name: "bash", Arguments: "{}"}},
		},
	}
	data, err := json.Marshal(message)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var wire struct {
		Role    string `json:"role"`
		Content []struct {
			Type string `json:"type"`
		} `json:"content"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		t.Fatalf("unmarshal wire: %v", err)
	}
	wantTypes := []string{BlockTypeReasoning, BlockTypeText, BlockTypeToolCall}
	if len(wire.Content) != len(wantTypes) {
		t.Fatalf("content length = %d, want %d", len(wire.Content), len(wantTypes))
	}
	for i, want := range wantTypes {
		if wire.Content[i].Type != want {
			t.Fatalf("content[%d] type = %q, want %q", i, wire.Content[i].Type, want)
		}
	}
	if wire.Role != "assistant" {
		t.Fatalf("role = %q, want assistant", wire.Role)
	}
}

func TestContentBlockDiscrimination(t *testing.T) {
	blocks := []ContentBlock{
		TextBlock{Text: "正文"},
		ReasoningBlock{Text: "推理"},
		ToolCallBlock{Call: ToolCall{ID: "c1", Name: "bash", Arguments: "{}"}},
		ToolResultBlock{Result: ToolResult{CallID: "c1", Content: "输出"}},
	}
	wantTypes := []string{BlockTypeText, BlockTypeReasoning, BlockTypeToolCall, BlockTypeToolResult}
	for i, block := range blocks {
		if got := block.BlockType(); got != wantTypes[i] {
			t.Fatalf("block %d BlockType = %q, want %q", i, got, wantTypes[i])
		}
		if err := block.Validate(); err != nil {
			t.Fatalf("block %d validate: %v", i, err)
		}
	}
	// Type switch discriminates concrete types.
	switch blocks[2].(type) {
	case ToolCallBlock:
	default:
		t.Fatalf("type switch did not identify ToolCallBlock: %T", blocks[2])
	}
	switch blocks[3].(type) {
	case ToolResultBlock:
	default:
		t.Fatalf("type switch did not identify ToolResultBlock: %T", blocks[3])
	}
}

func TestUnmarshalContentBlocksRejectsUnknownType(t *testing.T) {
	tests := []struct {
		name string
		wire string
	}{
		{name: "unknown tag", wire: `[{"type":"image","url":"https://example.invalid/a.png"}]`},
		{name: "missing tag", wire: `[{"text":"无标签"}]`},
		{name: "not an array", wire: `{"type":"text"}`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if _, err := UnmarshalContentBlocks([]byte(tt.wire)); err == nil {
				t.Fatalf("expected error for %s", tt.wire)
			}
		})
	}
}

func TestMessageValidateRoleRules(t *testing.T) {
	tests := []struct {
		name    string
		message Message
		wantErr bool
	}{
		{
			name:    "system accepts text",
			message: TextMessage(RoleSystem, "系统"),
			wantErr: false,
		},
		{
			name: "system rejects reasoning",
			message: Message{
				Role:    RoleSystem,
				Content: []ContentBlock{ReasoningBlock{Text: "推理"}},
			},
			wantErr: true,
		},
		{
			name:    "developer accepts text",
			message: TextMessage(RoleDeveloper, "约束"),
			wantErr: false,
		},
		{
			name: "assistant accepts reasoning, text, tool calls",
			message: Message{
				Role: RoleAssistant,
				Content: []ContentBlock{
					ReasoningBlock{Text: "推理"},
					TextBlock{Text: "正文"},
					ToolCallBlock{Call: ToolCall{ID: "c1", Name: "bash", Arguments: "{}"}},
				},
			},
			wantErr: false,
		},
		{
			name: "assistant rejects tool results",
			message: Message{
				Role:    RoleAssistant,
				Content: []ContentBlock{ToolResultBlock{Result: ToolResult{CallID: "c1"}}},
			},
			wantErr: true,
		},
		{
			name:    "user accepts tool results only",
			message: ToolResultMessage(ToolResult{CallID: "c1", Content: "结果"}),
			wantErr: false,
		},
		{
			name: "user rejects mixed text and tool results",
			message: Message{
				Role: RoleUser,
				Content: []ContentBlock{
					TextBlock{Text: "附带文本"},
					ToolResultBlock{Result: ToolResult{CallID: "c1", Content: "结果"}},
				},
			},
			wantErr: true,
		},
		{
			name: "user rejects tool calls",
			message: Message{
				Role:    RoleUser,
				Content: []ContentBlock{ToolCallBlock{Call: ToolCall{ID: "c1", Name: "bash", Arguments: "{}"}}},
			},
			wantErr: true,
		},
		{
			name:    "empty content rejected",
			message: Message{Role: RoleUser},
			wantErr: true,
		},
		{
			name:    "unknown role rejected",
			message: Message{Role: Role("tool"), Content: []ContentBlock{TextBlock{Text: "x"}}},
			wantErr: true,
		},
		{
			name:    "empty text block rejected",
			message: Message{Role: RoleUser, Content: []ContentBlock{TextBlock{}}},
			wantErr: true,
		},
		{
			name:    "whitespace-only text preserved",
			message: Message{Role: RoleUser, Content: []ContentBlock{TextBlock{Text: "   "}}},
			wantErr: false,
		},
		{
			name:    "reasoning needs text or signature",
			message: Message{Role: RoleAssistant, Content: []ContentBlock{ReasoningBlock{}}},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.message.Validate()
			if (err != nil) != tt.wantErr {
				t.Fatalf("Validate() = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

func TestLastNonEmptyText(t *testing.T) {
	tests := []struct {
		name    string
		content []ContentBlock
		want    string
		wantOK  bool
	}{
		{
			name:    "last text wins",
			content: []ContentBlock{TextBlock{Text: "第一段"}, TextBlock{Text: "最后一段"}},
			want:    "最后一段",
			wantOK:  true,
		},
		{
			name:    "blank trailing text is skipped",
			content: []ContentBlock{TextBlock{Text: "正文"}, TextBlock{Text: "  "}},
			want:    "正文",
			wantOK:  true,
		},
		{
			name:    "non text blocks are skipped",
			content: []ContentBlock{TextBlock{Text: "前言"}, ReasoningBlock{Text: "推理"}, ToolCallBlock{Call: ToolCall{ID: "c", Name: "bash", Arguments: "{}"}}},
			want:    "前言",
			wantOK:  true,
		},
		{
			name:    "no text at all",
			content: []ContentBlock{ReasoningBlock{Text: "推理"}},
			wantOK:  false,
		},
		{
			name:    "empty content",
			content: nil,
			wantOK:  false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, ok := LastNonEmptyText(tt.content)
			if got != tt.want || ok != tt.wantOK {
				t.Fatalf("LastNonEmptyText() = (%q, %v), want (%q, %v)", got, ok, tt.want, tt.wantOK)
			}
		})
	}
}

func TestToolNamePortability(t *testing.T) {
	tests := []struct {
		name  string
		valid bool
	}{
		{"read_file", true},
		{"Bash-2", true},
		{"_hidden", true},
		{"", false},
		{"工具", false},
		{"has space", false},
		{"slash/name", false},
		{strings.Repeat("a", MaxToolNameBytes), true},
		{strings.Repeat("a", MaxToolNameBytes+1), false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := isPortableName(tt.name); got != tt.valid {
				t.Fatalf("isPortableName(%q) = %v, want %v", tt.name, got, tt.valid)
			}
		})
	}
}

func TestToolCallValidate(t *testing.T) {
	tests := []struct {
		name    string
		call    ToolCall
		wantErr bool
	}{
		{name: "object arguments", call: ToolCall{ID: "c1", Name: "bash", Arguments: `{"command":"ls"}`}, wantErr: false},
		{name: "empty object arguments", call: ToolCall{ID: "c1", Name: "bash", Arguments: "{}"}, wantErr: false},
		{name: "empty id", call: ToolCall{Name: "bash", Arguments: "{}"}, wantErr: true},
		{name: "blank id", call: ToolCall{ID: "  ", Name: "bash", Arguments: "{}"}, wantErr: true},
		{name: "non portable name", call: ToolCall{ID: "c1", Name: "读取文件", Arguments: "{}"}, wantErr: true},
		{name: "empty arguments", call: ToolCall{ID: "c1", Name: "bash", Arguments: ""}, wantErr: true},
		{name: "array arguments", call: ToolCall{ID: "c1", Name: "bash", Arguments: "[]"}, wantErr: true},
		{name: "invalid json arguments", call: ToolCall{ID: "c1", Name: "bash", Arguments: `{"path":`}, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.call.Validate()
			if (err != nil) != tt.wantErr {
				t.Fatalf("Validate() = %v, wantErr %v", err, tt.wantErr)
			}
			if tt.wantErr {
				var modelErr *ModelError
				if !errors.As(err, &modelErr) || modelErr.Kind != ErrorProtocol {
					t.Fatalf("expected protocol-class ModelError, got %v", err)
				}
			}
		})
	}
}

func TestToolResultValidate(t *testing.T) {
	if err := (ToolResult{CallID: "c1", Content: "ok"}).Validate(); err != nil {
		t.Fatalf("valid result rejected: %v", err)
	}
	err := (ToolResult{Content: "缺标识"}).Validate()
	var modelErr *ModelError
	if !errors.As(err, &modelErr) || modelErr.Kind != ErrorInvalidRequest {
		t.Fatalf("expected invalid request error, got %v", err)
	}
}

func TestContentBlockJSONRoundTripSingle(t *testing.T) {
	blocks := []ContentBlock{
		TextBlock{Text: "正文"},
		ReasoningBlock{Text: "推理", Signature: "sig"},
		ToolCallBlock{Call: ToolCall{ID: "c1", Name: "bash", Arguments: `{"command":"ls"}`}},
		ToolResultBlock{Result: ToolResult{CallID: "c1", Content: "输出", IsError: true}},
	}
	for i, block := range blocks {
		data, err := json.Marshal(block)
		if err != nil {
			t.Fatalf("block %d marshal: %v", i, err)
		}
		decoded, err := UnmarshalContentBlocks([]byte("[" + string(data) + "]"))
		if err != nil {
			t.Fatalf("block %d unmarshal: %v", i, err)
		}
		if len(decoded) != 1 || !reflect.DeepEqual(decoded[0], block) {
			t.Fatalf("block %d round trip mismatch: want %#v got %#v", i, block, decoded[0])
		}
	}
}
