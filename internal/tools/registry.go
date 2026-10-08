package tools

import (
	"encoding/json"
	"fmt"

	"keencode/internal/model"
)

// Registry holds the built-in tools available to one agent session
// (docs/go-migration.md §5.3; Rust ToolRegistry in
// core/agent/src/tool.rs:1110-1160 reduced to the v1 surface). Registration
// order is frozen: Definitions returns definitions in the order tools were
// registered, so request assembly is deterministic.
type Registry struct {
	order  []string
	byName map[string]Tool
	defs   map[string]model.ToolDefinition
}

// NewRegistry validates and registers tools. A nil tool, a duplicate name,
// or a definition that fails model.ToolDefinition.Validate fails the whole
// construction (fail closed: a half-registered table is never returned).
func NewRegistry(tools ...Tool) (*Registry, error) {
	r := &Registry{
		byName: make(map[string]Tool, len(tools)),
		defs:   make(map[string]model.ToolDefinition, len(tools)),
	}
	for _, tool := range tools {
		if tool == nil {
			return nil, fmt.Errorf("tools: 注册表拒绝了 nil 工具")
		}
		def := tool.Definition()
		if err := def.Validate(); err != nil {
			return nil, fmt.Errorf("tools: 工具定义无效：%w", err)
		}
		if _, dup := r.byName[def.Name]; dup {
			return nil, fmt.Errorf("tools: 工具名称 %s 重复注册", def.Name)
		}
		r.order = append(r.order, def.Name)
		r.byName[def.Name] = tool
		r.defs[def.Name] = def
	}
	return r, nil
}

// Definitions returns a snapshot of the tool definitions in frozen
// registration order. The caller may mutate the returned slice.
func (r *Registry) Definitions() []model.ToolDefinition {
	out := make([]model.ToolDefinition, 0, len(r.order))
	for _, name := range r.order {
		out = append(out, r.defs[name])
	}
	return out
}

// Get returns the tool registered under the exact name.
func (r *Registry) Get(name string) (Tool, bool) {
	tool, ok := r.byName[name]
	return tool, ok
}

// ValidateInput checks a raw argument object against the registered tool's
// InputSchema, supporting exactly the JSON Schema subset the built-in
// definitions use: type, required, properties (one nesting level per
// object), additionalProperties: false, enum, minLength/maxLength and
// minimum/maximum. Unknown schema keywords are ignored per JSON Schema
// semantics; anything the subset cannot verify is reported as an error so
// the boundary stays fail closed. Each tool re-parses its input strictly,
// so this is a first gate, not the only one.
func (r *Registry) ValidateInput(name string, input json.RawMessage) error {
	def, ok := r.defs[name]
	if !ok {
		return fmt.Errorf("tools: 未知工具 %s", name)
	}
	decodedSchema, err := decodeJSONValue(def.InputSchema)
	if err != nil {
		return fmt.Errorf("tools: 工具 %s 的输入 Schema 不是 JSON 对象", name)
	}
	schema, ok := decodedSchema.(map[string]any)
	if !ok {
		return fmt.Errorf("tools: 工具 %s 的输入 Schema 不是 JSON 对象", name)
	}
	decoded, err := decodeJSONValue(input)
	if err != nil {
		return err
	}
	return validateSchemaValue(schema, decoded, "input")
}
