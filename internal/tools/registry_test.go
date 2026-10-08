package tools

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"keencode/internal/model"
)

// Compile-time interface conformance for every built-in tool.
var (
	_ Tool = (*ReadTool)(nil)
	_ Tool = (*EditTool)(nil)
	_ Tool = (*WriteTool)(nil)
	_ Tool = (*globTool)(nil)
	_ Tool = (*grepTool)(nil)
	_ Tool = (*bashTool)(nil)
)

// fakeTool is a minimal Tool for registry and schema tests.
type fakeTool struct {
	def    model.ToolDefinition
	effect Effect
}

func (f fakeTool) Definition() model.ToolDefinition { return f.def }

func (f fakeTool) Effect(json.RawMessage) Effect { return f.effect }

func (f fakeTool) Execute(context.Context, Invocation) (ToolOutput, error) {
	return ToolOutput{}, nil
}

// mustTool builds a valid fake tool or fails the test.
func mustTool(t *testing.T, name, schema string, effect Effect) fakeTool {
	t.Helper()
	return fakeTool{
		def: model.ToolDefinition{
			Name:        name,
			Description: "test tool " + name,
			InputSchema: json.RawMessage(schema),
		},
		effect: effect,
	}
}

const fakeSchema = `{
	"type": "object",
	"properties": {
		"query":  {"type": "string", "minLength": 2},
		"count":  {"type": "integer", "minimum": 0, "maximum": 10},
		"force":  {"type": "boolean"},
		"mode":   {"type": "string", "enum": ["fast", "slow"]}
	},
	"required": ["query"],
	"additionalProperties": false
}`

func TestRegistryRegistersInOrder(t *testing.T) {
	registry, err := NewRegistry(
		mustTool(t, "Beta", fakeSchema, EffectSideEffect),
		mustTool(t, "Alpha", fakeSchema, EffectReadOnly),
	)
	if err != nil {
		t.Fatalf("NewRegistry 失败：%v", err)
	}
	defs := registry.Definitions()
	if len(defs) != 2 {
		t.Fatalf("Definitions() 数量 = %d，want 2", len(defs))
	}
	// Registration order is frozen, not name-sorted.
	if defs[0].Name != "Beta" || defs[1].Name != "Alpha" {
		t.Fatalf("Definitions() 顺序 = [%s, %s]，want [Beta, Alpha]", defs[0].Name, defs[1].Name)
	}
	if _, ok := registry.Get("Alpha"); !ok {
		t.Fatalf("Get(Alpha) 未命中已注册工具")
	}
	if _, ok := registry.Get("Gamma"); ok {
		t.Fatalf("Get(Gamma) 不应命中")
	}
}

func TestRegistryRejectsBadRegistrations(t *testing.T) {
	cases := []struct {
		name    string
		tools   []Tool
		wantSub string
	}{
		{
			name:    "nil tool",
			tools:   []Tool{nil},
			wantSub: "nil",
		},
		{
			name: "duplicate name",
			tools: []Tool{
				mustTool(t, "Same", fakeSchema, EffectReadOnly),
				mustTool(t, "Same", fakeSchema, EffectSideEffect),
			},
			wantSub: "重复注册",
		},
		{
			name: "empty description",
			tools: []Tool{fakeTool{def: model.ToolDefinition{
				Name:        "NoDesc",
				InputSchema: json.RawMessage(fakeSchema),
			}}},
			wantSub: "说明不能为空",
		},
		{
			name:    "invalid schema json",
			tools:   []Tool{mustTool(t, "BadSchema", "{not json", EffectReadOnly)},
			wantSub: "Schema",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := NewRegistry(tc.tools...)
			if err == nil {
				t.Fatalf("NewRegistry 应失败")
			}
			if !strings.Contains(err.Error(), tc.wantSub) {
				t.Fatalf("错误 %q 不包含 %q", err.Error(), tc.wantSub)
			}
		})
	}
}

func TestRegistryValidateInput(t *testing.T) {
	registry, err := NewRegistry(mustTool(t, "Probe", fakeSchema, EffectReadOnly))
	if err != nil {
		t.Fatalf("NewRegistry 失败：%v", err)
	}
	cases := []struct {
		name    string
		input   string
		wantSub string // empty means valid
	}{
		{"valid minimal", `{"query":"abc"}`, ""},
		{"valid full", `{"query":"abc","count":3,"force":true,"mode":"fast"}`, ""},
		{"missing required", `{}`, "缺少必填字段"},
		{"unknown field", `{"query":"abc","extra":1}`, "未知字段"},
		{"string too short", `{"query":"a"}`, "长度不能少于"},
		{"not integer", `{"query":"abc","count":1.5}`, "必须是整数"},
		{"below minimum", `{"query":"abc","count":-1}`, "不能小于"},
		{"above maximum", `{"query":"abc","count":11}`, "不能大于"},
		{"enum miss", `{"query":"abc","mode":"quick"}`, "取值范围"},
		{"wrong type", `{"query":42}`, "必须是字符串"},
		{"not json", `nonsense`, "不是有效 JSON"},
		{"unknown tool", `{"query":"abc"}`, "未知工具"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			name := "Probe"
			if tc.name == "unknown tool" {
				name = "Missing"
			}
			err := registry.ValidateInput(name, json.RawMessage(tc.input))
			if tc.wantSub == "" {
				if err != nil {
					t.Fatalf("ValidateInput(%s) 应通过，得到 %v", tc.input, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.wantSub) {
				t.Fatalf("ValidateInput(%s) 错误 = %v，want 包含 %q", tc.input, err, tc.wantSub)
			}
		})
	}
}
