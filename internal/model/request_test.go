package model

import (
	"encoding/json"
	"errors"
	"math"
	"reflect"
	"strings"
	"testing"
)

func sampleRequest() ModelRequest {
	temperature := 0.7
	return ModelRequest{
		Model: "test-model",
		Messages: []Message{
			TextMessage(RoleSystem, "系统指令"),
			TextMessage(RoleUser, "用户输入"),
		},
		Tools: []ToolDefinition{
			{
				Name:        "read_file",
				Description: "读取文件",
				InputSchema: json.RawMessage(`{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}`),
			},
		},
		ToolChoice:  ToolChoice{Mode: ToolChoiceAuto},
		MaxTokens:   4096,
		Temperature: &temperature,
		Reasoning:   &ReasoningConfig{Effort: ReasoningEffortMedium},
	}
}

func TestModelRequestJSONRoundTrip(t *testing.T) {
	request := sampleRequest()
	data, err := json.Marshal(request)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var decoded ModelRequest
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if !reflect.DeepEqual(decoded, request) {
		t.Fatalf("round trip mismatch:\n want %+v\n got  %+v", request, decoded)
	}
	var wire map[string]json.RawMessage
	if err := json.Unmarshal(data, &wire); err != nil {
		t.Fatalf("unmarshal wire: %v", err)
	}
	for _, key := range []string{"model", "messages", "tools", "toolChoice", "maxTokens", "temperature", "reasoning"} {
		if _, ok := wire[key]; !ok {
			t.Fatalf("wire is missing key %q in %s", key, data)
		}
	}
}

func TestModelRequestValidate(t *testing.T) {
	zero := 0.0
	negative := -0.5
	tests := []struct {
		name    string
		mutate  func(*ModelRequest)
		wantErr bool
	}{
		{name: "valid request", mutate: func(*ModelRequest) {}, wantErr: false},
		{name: "empty model", mutate: func(r *ModelRequest) { r.Model = "  " }, wantErr: true},
		{name: "no messages", mutate: func(r *ModelRequest) { r.Messages = nil }, wantErr: true},
		{
			name: "invalid message fails",
			mutate: func(r *ModelRequest) {
				r.Messages = append(r.Messages, Message{Role: RoleUser})
			},
			wantErr: true,
		},
		{
			name: "tool without description",
			mutate: func(r *ModelRequest) {
				r.Tools = append(r.Tools, ToolDefinition{Name: "extra", Description: " ", InputSchema: json.RawMessage(`{}`)})
			},
			wantErr: true,
		},
		{
			name: "tool with non object schema",
			mutate: func(r *ModelRequest) {
				r.Tools = append(r.Tools, ToolDefinition{Name: "extra", Description: "说明", InputSchema: json.RawMessage(`[]`)})
			},
			wantErr: true,
		},
		{
			name: "tool with invalid schema json",
			mutate: func(r *ModelRequest) {
				r.Tools = append(r.Tools, ToolDefinition{Name: "extra", Description: "说明", InputSchema: json.RawMessage(`{`)})
			},
			wantErr: true,
		},
		{
			name: "duplicate tool names",
			mutate: func(r *ModelRequest) {
				r.Tools = append(r.Tools, ToolDefinition{Name: "read_file", Description: "重复", InputSchema: json.RawMessage(`{}`)})
			},
			wantErr: true,
		},
		{
			name:    "required choice without tools",
			mutate:  func(r *ModelRequest) { r.Tools = nil; r.ToolChoice = ToolChoice{Mode: ToolChoiceRequired} },
			wantErr: true,
		},
		{
			name:    "specific choice with unknown tool",
			mutate:  func(r *ModelRequest) { r.ToolChoice = ToolChoice{Mode: ToolChoiceTool, Name: "missing_tool"} },
			wantErr: true,
		},
		{
			name:    "specific choice with empty name",
			mutate:  func(r *ModelRequest) { r.ToolChoice = ToolChoice{Mode: ToolChoiceTool, Name: " "} },
			wantErr: true,
		},
		{
			name:    "specific choice with listed tool",
			mutate:  func(r *ModelRequest) { r.ToolChoice = ToolChoice{Mode: ToolChoiceTool, Name: "read_file"} },
			wantErr: false,
		},
		{
			name:    "none choice without tools",
			mutate:  func(r *ModelRequest) { r.Tools = nil; r.ToolChoice = ToolChoice{Mode: ToolChoiceNone} },
			wantErr: false,
		},
		{
			name:    "unknown choice mode",
			mutate:  func(r *ModelRequest) { r.ToolChoice = ToolChoice{Mode: "any"} },
			wantErr: true,
		},
		{
			name:    "unsupported reasoning effort",
			mutate:  func(r *ModelRequest) { r.Reasoning = &ReasoningConfig{Effort: "ultra"} },
			wantErr: true,
		},
		{
			name:    "default reasoning effort",
			mutate:  func(r *ModelRequest) { r.Reasoning = &ReasoningConfig{} },
			wantErr: false,
		},
		{name: "negative max tokens", mutate: func(r *ModelRequest) { r.MaxTokens = -1 }, wantErr: true},
		{name: "zero max tokens means default", mutate: func(r *ModelRequest) { r.MaxTokens = 0 }, wantErr: false},
		{name: "negative temperature", mutate: func(r *ModelRequest) { r.Temperature = &negative }, wantErr: true},
		{name: "zero temperature", mutate: func(r *ModelRequest) { r.Temperature = &zero }, wantErr: false},
		{
			name:    "infinite temperature",
			mutate:  func(r *ModelRequest) { v := math.Inf(1); r.Temperature = &v },
			wantErr: true,
		},
		{
			name:    "nan temperature",
			mutate:  func(r *ModelRequest) { v := math.NaN(); r.Temperature = &v },
			wantErr: true,
		},
		{
			name:    "nil temperature uses default",
			mutate:  func(r *ModelRequest) { r.Temperature = nil },
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			request := sampleRequest()
			tt.mutate(&request)
			err := request.Validate()
			if (err != nil) != tt.wantErr {
				t.Fatalf("Validate() = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

func TestToolDefinitionValidateNameRules(t *testing.T) {
	tests := []struct {
		name    string
		def     ToolDefinition
		wantErr bool
	}{
		{name: "valid", def: ToolDefinition{Name: "glob_files", Description: "查找文件", InputSchema: json.RawMessage(`{"type":"object"}`)}, wantErr: false},
		{name: "overlong name", def: ToolDefinition{Name: strings.Repeat("a", MaxToolNameBytes+1), Description: "说明", InputSchema: json.RawMessage(`{}`)}, wantErr: true},
		{name: "empty name", def: ToolDefinition{Description: "说明", InputSchema: json.RawMessage(`{}`)}, wantErr: true},
		{name: "missing schema", def: ToolDefinition{Name: "glob_files", Description: "说明"}, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.def.Validate()
			if (err != nil) != tt.wantErr {
				t.Fatalf("Validate() = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

func TestReasoningConfigValidate(t *testing.T) {
	tests := []struct {
		effort  string
		wantErr bool
	}{
		{"", false},
		{ReasoningEffortMinimal, false},
		{ReasoningEffortLow, false},
		{ReasoningEffortMedium, false},
		{ReasoningEffortHigh, false},
		{"maximum", true},
		{"HIGH", true},
	}
	for _, tt := range tests {
		t.Run(tt.effort, func(t *testing.T) {
			err := (ReasoningConfig{Effort: tt.effort}).Validate()
			if (err != nil) != tt.wantErr {
				t.Fatalf("Validate() = %v, wantErr %v", err, tt.wantErr)
			}
			if tt.wantErr {
				var modelErr *ModelError
				if !errors.As(err, &modelErr) || modelErr.Kind != ErrorInvalidRequest {
					t.Fatalf("expected invalid request error, got %v", err)
				}
			}
		})
	}
}
