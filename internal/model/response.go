package model

import "encoding/json"

// ModelResponse is the complete provider-neutral representation of one model
// response (docs/go-migration.md §5.1; the Rust ResponseMetadata collapsed
// into the single Model field).
type ModelResponse struct {
	// Content keeps the model-generated block order.
	Content []ContentBlock `json:"content"`
	// StopReason is the unified reason the model ended the response.
	StopReason StopReason `json:"stopReason"`
	// Usage is the token usage reported by the endpoint; unknown fields are
	// -1.
	Usage TokenUsage `json:"usage"`
	// Model is the model identifier the response belongs to; the stream
	// collector fills it with the requested model.
	Model string `json:"model,omitempty"`
}

// MarshalJSON encodes the response with tagged content blocks so concrete
// block types survive a round trip.
func (r ModelResponse) MarshalJSON() ([]byte, error) {
	content, err := MarshalContentBlocks(r.Content)
	if err != nil {
		return nil, err
	}
	return json.Marshal(struct {
		Content    json.RawMessage `json:"content"`
		StopReason StopReason      `json:"stopReason"`
		Usage      TokenUsage      `json:"usage"`
		Model      string          `json:"model,omitempty"`
	}{content, r.StopReason, r.Usage, r.Model})
}

// UnmarshalJSON decodes a response and restores the concrete content block
// types from their tags.
func (r *ModelResponse) UnmarshalJSON(data []byte) error {
	var wire struct {
		Content    json.RawMessage `json:"content"`
		StopReason StopReason      `json:"stopReason"`
		Usage      TokenUsage      `json:"usage"`
		Model      string          `json:"model"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	blocks, err := UnmarshalContentBlocks(wire.Content)
	if err != nil {
		return err
	}
	r.Content, r.StopReason, r.Usage, r.Model = blocks, wire.StopReason, wire.Usage, wire.Model
	return nil
}
