package runtime

import (
	"strings"

	"keencode/internal/model"
)

// turnBuffer accumulates one assistant response while replaying: reasoning
// and text deltas concatenate, tool calls assemble from start/args/end
// events. Block interleaving inside one response is flattened to the fixed
// order reasoning → text → tool calls; the v1 providers only need signature
// continuity and call/result pairing, not intra-response ordering.
type turnBuffer struct {
	reasoning strings.Builder
	signature string
	text      strings.Builder
	calls     []model.ToolCall
	// hasContent marks any assistant payload, so a response carrying only
	// ignored events does not materialize an empty message.
	hasContent bool
}

// historyBuilder reduces journal events into provider-neutral messages.
type historyBuilder struct {
	messages []model.Message
	current  *turnBuffer
	// results accumulates tool results waiting to be flushed as the
	// RoleUser message that must immediately follow the assistant message
	// holding the calls (docs/go-migration.md §5.1).
	results []model.ToolResult
	// resultsAnchored records that an assistant message with calls was
	// flushed since results started accumulating; unanchored results are
	// orphans and get dropped.
	resultsAnchored bool
	// resulted marks tool call ids that received a tool_result event; only
	// resulted calls enter the model-visible history.
	resulted map[string]bool
}

// eventsToHistory rebuilds the conversation from replayed events. The
// rules:
//   - user_message events become one RoleUser text message each;
//   - reasoning/text deltas accumulate into the current assistant response;
//     reasoning_continuation folds its text into the ReasoningBlock
//     signature (required to resume signed thinking across restarts);
//   - tool_start/tool_args/tool_end assemble ToolCallBlocks;
//   - a tool_result flushes the assistant message that holds the calls,
//     then queues the result; the queued results flush as the following
//     RoleUser tool-result message at the next content boundary;
//   - terminal turn events flush whatever is pending; tool calls that never
//     received a result are dropped from the model-visible history (their
//     card events stay visible in the timeline);
//   - unknown event types are ignored so newer journals replay forward
//     compatibly.
func eventsToHistory(events []Event) []model.Message {
	b := &historyBuilder{}
	for _, ev := range events {
		b.observe(ev)
	}
	b.flushTurn()
	return b.messages
}

// observe folds one event into the builder state.
func (b *historyBuilder) observe(ev Event) {
	switch ev.Type {
	case EventUserMessage:
		b.flushTurn()
		b.messages = append(b.messages, model.TextMessage(model.RoleUser, ev.Text))
	case EventReasoningDelta:
		b.flushResults()
		buf := b.turn()
		buf.reasoning.WriteString(ev.Text)
		buf.hasContent = true
	case EventReasoningContinuation:
		// The signature alone justifies a block: resuming signed thinking
		// needs it even when no reasoning delta was journaled.
		b.flushResults()
		buf := b.turn()
		buf.signature = ev.Text
		buf.hasContent = true
	case EventTextDelta:
		b.flushResults()
		buf := b.turn()
		buf.text.WriteString(ev.Text)
		buf.hasContent = true
	case EventToolStart:
		if ev.Tool == nil || ev.Tool.CallID == "" {
			return
		}
		b.flushResults()
		buf := b.turn()
		buf.calls = append(buf.calls, model.ToolCall{ID: ev.Tool.CallID, Name: ev.Tool.Name, Arguments: "{}"})
		buf.hasContent = true
	case EventToolArgs:
		if b.current == nil || ev.Tool == nil {
			return
		}
		for i := range b.current.calls {
			if b.current.calls[i].ID != ev.Tool.CallID {
				continue
			}
			if b.current.calls[i].Arguments == "{}" {
				b.current.calls[i].Arguments = ""
			}
			b.current.calls[i].Arguments += ev.Text
			break
		}
	case EventToolEnd:
		// Argument transport closed; the call stays as assembled.
	case EventToolResult:
		if ev.Tool != nil {
			if b.resulted == nil {
				b.resulted = make(map[string]bool)
			}
			b.resulted[ev.Tool.CallID] = true
		}
		// Flush the assistant message holding the calls before the first
		// result references them.
		if b.flushAssistant() {
			b.resultsAnchored = true
		}
		result := model.ToolResult{Content: ev.Text}
		if ev.Tool != nil {
			result.CallID = ev.Tool.CallID
			result.IsError = ev.Tool.Status == ToolStatusFailed
		}
		b.results = append(b.results, result)
	case EventTurnCompleted, EventTurnFailed, EventTurnCancelled:
		b.flushTurn()
	default:
		// usage, permission_denied and unknown types carry no message
		// content.
	}
}

// turn returns the current assistant buffer, creating one on demand.
func (b *historyBuilder) turn() *turnBuffer {
	if b.current == nil {
		b.current = &turnBuffer{}
	}
	return b.current
}

// flushAssistant materializes the accumulated assistant message. It reports
// whether a message was actually appended.
func (b *historyBuilder) flushAssistant() bool {
	if b.current == nil {
		return false
	}
	buf := b.current
	b.current = nil
	if !buf.hasContent {
		return false
	}
	var blocks []model.ContentBlock
	if buf.reasoning.Len() > 0 {
		blocks = append(blocks, model.ReasoningBlock{Text: buf.reasoning.String(), Signature: buf.signature})
	}
	if buf.text.Len() > 0 {
		blocks = append(blocks, model.TextBlock{Text: buf.text.String()})
	}
	for _, call := range buf.calls {
		if !b.resulted[call.ID] {
			// A call whose result never arrived (turn interrupted mid-flight)
			// must not reach the model: providers reject dangling calls.
			continue
		}
		args := call.Arguments
		if strings.TrimSpace(args) == "" {
			args = "{}"
		}
		blocks = append(blocks, model.ToolCallBlock{Call: model.ToolCall{ID: call.ID, Name: call.Name, Arguments: args}})
	}
	if len(blocks) == 0 {
		return false
	}
	b.messages = append(b.messages, model.Message{Role: model.RoleAssistant, Content: blocks})
	return true
}

// flushResults materializes the pending tool results message, if any.
func (b *historyBuilder) flushResults() {
	if len(b.results) == 0 {
		return
	}
	b.messages = append(b.messages, model.ToolResultMessage(b.results...))
	b.results = nil
	b.resultsAnchored = false
}

// flushTurn closes the current response: the assistant message first, then
// the tool results collected for its calls. Results without an anchored
// assistant message are orphans (a result delivered before any call, e.g.
// after a crash) and are dropped: providers reject tool results without
// their calls on the wire.
func (b *historyBuilder) flushTurn() {
	b.flushAssistant()
	if len(b.results) > 0 {
		if b.resultsAnchored {
			b.flushResults()
		} else {
			b.results = nil
			b.resultsAnchored = false
		}
	}
}
