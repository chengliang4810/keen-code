package model

import (
	"encoding/json"
	"math"
	"strings"
)

// ToolChoice modes (docs/go-migration.md §5.1; Rust ToolChoice enum with the
// Specific variant flattened into Mode=ToolChoiceTool + Name).
const (
	// ToolChoiceAuto lets the model decide whether to call tools. It is also
	// the meaning of the zero-value Mode "".
	ToolChoiceAuto = "auto"
	// ToolChoiceNone forbids the model from calling any tool.
	ToolChoiceNone = "none"
	// ToolChoiceRequired demands at least one tool call.
	ToolChoiceRequired = "required"
	// ToolChoiceTool demands a call of the tool named in ToolChoice.Name.
	ToolChoiceTool = "tool"
)

// Reasoning effort levels accepted by ReasoningConfig.Effort
// (docs/go-migration.md §5.1; the v1 endpoints expose these four levels).
const (
	ReasoningEffortMinimal = "minimal"
	ReasoningEffortLow     = "low"
	ReasoningEffortMedium  = "medium"
	ReasoningEffortHigh    = "high"
)

// ToolDefinition is the provider-neutral definition of a tool the model may
// call (core/model/src/tool.rs:19-81).
type ToolDefinition struct {
	// Name is unique and non-empty within one request and must satisfy the
	// portable name format (see isPortableName).
	Name string `json:"name"`
	// Description explains purpose, boundaries, and input requirements to
	// the model.
	Description string `json:"description"`
	// InputSchema is the JSON Schema object describing the tool input.
	InputSchema json.RawMessage `json:"inputSchema"`
}

// Validate checks the name, the description, and that InputSchema is a JSON
// object. Deep schema keyword validation (Rust structured.rs) is not part of
// the v1 Go model layer; adapters and the tool registry own input checking.
func (t ToolDefinition) Validate() error {
	if !isPortableName(t.Name) {
		return InvalidRequest("工具名称必须是 1 到 64 字节的 ASCII 字母、数字、下划线或短横线")
	}
	if strings.TrimSpace(t.Description) == "" {
		return InvalidRequest("工具 %s 的说明不能为空", t.Name)
	}
	var schema map[string]any
	if err := json.Unmarshal(t.InputSchema, &schema); err != nil || schema == nil {
		return InvalidRequest("工具 %s 的输入 Schema 必须是 JSON 对象", t.Name)
	}
	return nil
}

// ToolChoice is the tool selection strategy for one request
// (Rust ToolChoice). The zero value means auto.
type ToolChoice struct {
	// Mode is one of the ToolChoice* constants ("", "auto", "none",
	// "required", "tool").
	Mode string `json:"mode"`
	// Name is the required tool when Mode is ToolChoiceTool.
	Name string `json:"name,omitempty"`
}

// ReasoningConfig is the provider-neutral reasoning request configuration
// (docs/go-migration.md §5.1). An empty Effort asks the endpoint for its
// default.
type ReasoningConfig struct {
	// Effort is one of the ReasoningEffort* constants or "".
	Effort string `json:"effort,omitempty"`
}

// Validate checks the effort level (Rust ReasoningConfig::validate reduced
// to the v1 fields).
func (c ReasoningConfig) Validate() error {
	switch c.Effort {
	case "", ReasoningEffortMinimal, ReasoningEffortLow, ReasoningEffortMedium, ReasoningEffortHigh:
		return nil
	default:
		return InvalidRequest("推理强度 %q 不受支持", c.Effort)
	}
}

// ModelRequest is the unified request an agent runtime submits to any model
// provider (docs/go-migration.md §5.1; Rust ModelRequest reduced to the v1
// fields — no structured output, no request metadata, no parallel tool call
// override).
type ModelRequest struct {
	// Model is the model identifier selected in the provider configuration.
	Model string `json:"model"`
	// Messages is the full effective message list in conversation order.
	Messages []Message `json:"messages"`
	// Tools are the tool definitions allowed for this call.
	Tools []ToolDefinition `json:"tools,omitempty"`
	// ToolChoice is the tool selection strategy.
	ToolChoice ToolChoice `json:"toolChoice"`
	// MaxTokens caps output tokens; 0 uses the model default.
	MaxTokens int `json:"maxTokens,omitempty"`
	// Temperature is the provider-neutral sampling temperature; nil uses the
	// model default.
	Temperature *float64 `json:"temperature,omitempty"`
	// Reasoning enables reasoning configuration; nil leaves the endpoint
	// default.
	Reasoning *ReasoningConfig `json:"reasoning,omitempty"`
}

// Validate enforces the unified model layer invariants
// (Rust ModelRequest::validate, core/model/src/request.rs:612-683).
func (r ModelRequest) Validate() error {
	if strings.TrimSpace(r.Model) == "" {
		return InvalidRequest("模型标识不能为空")
	}
	if len(r.Messages) == 0 {
		return InvalidRequest("模型请求至少需要一条消息")
	}
	for i := range r.Messages {
		if err := r.Messages[i].Validate(); err != nil {
			return err
		}
	}

	toolNames := make(map[string]bool, len(r.Tools))
	for i := range r.Tools {
		if err := r.Tools[i].Validate(); err != nil {
			return err
		}
		if toolNames[r.Tools[i].Name] {
			return InvalidRequest("工具名称 %s 在同一请求中重复", r.Tools[i].Name)
		}
		toolNames[r.Tools[i].Name] = true
	}

	switch r.ToolChoice.Mode {
	case "", ToolChoiceAuto, ToolChoiceNone:
	case ToolChoiceRequired:
		if len(r.Tools) == 0 {
			return InvalidRequest("要求调用工具时工具列表不能为空")
		}
	case ToolChoiceTool:
		if strings.TrimSpace(r.ToolChoice.Name) == "" {
			return InvalidRequest("指定工具名称不能为空")
		}
		if !toolNames[r.ToolChoice.Name] {
			return InvalidRequest("指定工具 %s 不在当前工具列表中", r.ToolChoice.Name)
		}
	default:
		return InvalidRequest("工具选择策略 %q 不受支持", r.ToolChoice.Mode)
	}

	if r.Reasoning != nil {
		if err := r.Reasoning.Validate(); err != nil {
			return err
		}
	}
	if r.MaxTokens < 0 {
		return InvalidRequest("最大输出 Token 必须大于零")
	}
	if r.Temperature != nil {
		t := *r.Temperature
		if math.IsNaN(t) || math.IsInf(t, 0) || t < 0 {
			return InvalidRequest("采样温度必须是大于等于零的有限数值")
		}
	}
	return nil
}
