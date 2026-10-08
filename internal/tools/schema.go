package tools

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"unicode/utf8"
)

// decodeJSONValue decodes one JSON document into generic Go values, keeping
// numbers as json.Number so the integer check can distinguish 2 from 2.0.
func decodeJSONValue(raw json.RawMessage) (any, error) {
	if !json.Valid(raw) {
		return nil, fmt.Errorf("tools: 工具输入不是有效 JSON")
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	var v any
	if err := dec.Decode(&v); err != nil {
		return nil, fmt.Errorf("tools: 工具输入无效：%w", err)
	}
	return v, nil
}

// validateSchemaValue checks one decoded value against one schema object.
// path is the human-readable location used in error messages.
func validateSchemaValue(schema map[string]any, value any, path string) error {
	if err := checkSchemaType(schema, value, path); err != nil {
		return err
	}
	switch typed := value.(type) {
	case map[string]any:
		return validateSchemaObject(schema, typed, path)
	case string:
		return validateSchemaString(schema, typed, path)
	case json.Number:
		return validateSchemaNumber(schema, typed, path)
	case bool:
		return checkSchemaEnum(schema, typed, path)
	default:
		// Arrays carry no constrained keywords in the v1 built-in schemas;
		// values of other shapes pass through unconstrained.
		return nil
	}
}

// checkSchemaType enforces the "type" keyword. A type the subset does not
// understand is an error, not a silent pass (fail closed).
func checkSchemaType(schema map[string]any, value any, path string) error {
	declared, _ := schema["type"].(string)
	switch declared {
	case "":
		return nil
	case "object":
		if _, ok := value.(map[string]any); !ok {
			return fmt.Errorf("tools: %s 必须是对象", path)
		}
	case "string":
		if _, ok := value.(string); !ok {
			return fmt.Errorf("tools: %s 必须是字符串", path)
		}
	case "integer":
		number, ok := value.(json.Number)
		if !ok || !isJSONInteger(number) {
			return fmt.Errorf("tools: %s 必须是整数", path)
		}
	case "number":
		if _, ok := value.(json.Number); !ok {
			return fmt.Errorf("tools: %s 必须是数字", path)
		}
	case "boolean":
		if _, ok := value.(bool); !ok {
			return fmt.Errorf("tools: %s 必须是布尔值", path)
		}
	case "array":
		if _, ok := value.([]any); !ok {
			return fmt.Errorf("tools: %s 必须是数组", path)
		}
	default:
		return fmt.Errorf("tools: %s 声明了不支持的类型 %q", path, declared)
	}
	return nil
}

// validateSchemaObject enforces required, additionalProperties: false and
// one level of per-property schemas.
func validateSchemaObject(schema map[string]any, value map[string]any, path string) error {
	properties, _ := schema["properties"].(map[string]any)
	if required, ok := schema["required"].([]any); ok {
		for _, entry := range required {
			name, ok := entry.(string)
			if !ok {
				continue
			}
			if _, present := value[name]; !present {
				return fmt.Errorf("tools: %s 缺少必填字段 %s", path, name)
			}
		}
	}
	if closed, ok := schema["additionalProperties"].(bool); ok && !closed {
		for key := range value {
			if _, known := properties[key]; !known {
				return fmt.Errorf("tools: %s 存在未知字段 %s", path, key)
			}
		}
	}
	for key, raw := range value {
		propertySchema, ok := properties[key].(map[string]any)
		if !ok {
			continue
		}
		if err := validateSchemaValue(propertySchema, raw, path+"."+key); err != nil {
			return err
		}
	}
	return nil
}

// validateSchemaString enforces minLength, maxLength and enum.
func validateSchemaString(schema map[string]any, value, path string) error {
	if limit, ok := schemaKeywordInt(schema, "minLength"); ok && utf8.RuneCountInString(value) < limit {
		return fmt.Errorf("tools: %s 长度不能少于 %d", path, limit)
	}
	if limit, ok := schemaKeywordInt(schema, "maxLength"); ok && limit >= 0 && utf8.RuneCountInString(value) > limit {
		return fmt.Errorf("tools: %s 长度不能超过 %d", path, limit)
	}
	return checkSchemaEnum(schema, value, path)
}

// validateSchemaNumber enforces minimum, maximum and enum on JSON numbers.
func validateSchemaNumber(schema map[string]any, value json.Number, path string) error {
	parsed, err := value.Float64()
	if err != nil {
		return fmt.Errorf("tools: %s 不是有限数字", path)
	}
	if bound, ok := schemaKeywordFloat(schema, "minimum"); ok && parsed < bound {
		return fmt.Errorf("tools: %s 不能小于 %s", path, value.String())
	}
	if bound, ok := schemaKeywordFloat(schema, "maximum"); ok && parsed > bound {
		return fmt.Errorf("tools: %s 不能大于 %s", path, value.String())
	}
	return checkSchemaEnum(schema, value, path)
}

// checkSchemaEnum enforces the enum keyword by canonical JSON comparison so
// schema floats and input json.Number compare by value.
func checkSchemaEnum(schema map[string]any, value any, path string) error {
	allowed, ok := schema["enum"].([]any)
	if !ok {
		return nil
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("tools: %s 无法编码用于枚举比对：%w", path, err)
	}
	for _, candidate := range allowed {
		candidateEncoded, err := json.Marshal(candidate)
		if err != nil {
			continue
		}
		if string(candidateEncoded) == string(encoded) {
			return nil
		}
	}
	return fmt.Errorf("tools: %s 不在允许的取值范围内", path)
}

// schemaKeywordInt extracts a non-negative integer constraint keyword.
func schemaKeywordInt(schema map[string]any, key string) (int, bool) {
	raw, ok := schema[key].(json.Number)
	if !ok {
		return 0, false
	}
	parsed, err := raw.Int64()
	if err != nil || parsed < 0 || parsed > math.MaxInt {
		return 0, false
	}
	return int(parsed), true
}

// schemaKeywordFloat extracts a numeric bound keyword.
func schemaKeywordFloat(schema map[string]any, key string) (float64, bool) {
	raw, ok := schema[key].(json.Number)
	if !ok {
		return 0, false
	}
	parsed, err := raw.Float64()
	if err != nil {
		return 0, false
	}
	return parsed, true
}

// isJSONInteger reports whether a json.Number has no fraction or exponent.
func isJSONInteger(number json.Number) bool {
	text := number.String()
	for i := 0; i < len(text); i++ {
		switch text[i] {
		case '.', 'e', 'E':
			return false
		}
	}
	return true
}
