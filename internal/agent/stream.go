package agent

import (
	"context"
	"encoding/json"
	"errors"
	"strings"

	"keencode/internal/model"
)

// roundBlock accumulates one content block of a model round, keyed by the
// stable stream index. It mirrors the pending-block rules of the unified
// stream collector (core/model/src/stream.rs; internal/model/provider.go) so
// the agent can forward deltas live and still assemble the same final
// response the collector would produce.
type roundBlock struct {
	kind      string // model.BlockTypeText, BlockTypeReasoning or BlockTypeToolCall
	text      string
	signature string // reasoning continuation state
	callID    string
	name      string
	arguments string
	ended     bool // tool call argument transport finished
}

// streamOutcome is the terminal state of one consumed model stream.
type streamOutcome struct {
	// started / ended mirror the unified stream invariants.
	started bool
	ended   bool
	// stop is the unified stop reason from the terminal event.
	stop model.StopReason
	// usage is the merged usage across all usage snapshots of the round.
	usage model.TokenUsage
	// order keeps first-appearance block order; blocks holds the state.
	order  []uint32
	blocks map[uint32]*roundBlock
	// err carries a stream failure or a wrapped sink failure; cancelled
	// reports that ctx ended the round.
	err       error
	cancelled bool
}

// consumeStream drains one provider event channel, forwarding live timeline
// events through the sink and accumulating the round state. Cancellation wins
// over waiting for the provider, mirroring the runner's select
// (core/agent/src/runner.rs:5923-5944): when ctx ends, the channel is drained
// in the background and the round reports cancelled. Reasoning summary deltas
// are intentionally not forwarded — the journal never carries them
// (docs/go-migration.md §5.4).
func (run *turnRun) consumeStream(ctx context.Context, events <-chan model.StreamEvent) *streamOutcome {
	outcome := &streamOutcome{
		usage:  model.UnknownUsage(),
		blocks: make(map[uint32]*roundBlock),
	}

	// drain unblocks a provider still sending after the loop stopped caring.
	// A contract-bound provider closes the channel on its own; the drain
	// goroutine then exits immediately.
	drain := func() {
		go func() {
			for range events {
			}
		}()
	}

	for {
		select {
		case <-ctx.Done():
			drain()
			outcome.cancelled = true
			return outcome
		case event, ok := <-events:
			if !ok {
				outcome.finishIncomplete()
				return outcome
			}
			err := run.applyStreamEvent(outcome, event)
			if err == nil {
				continue
			}
			drain()
			var sinkErr *sinkFailure
			if errors.As(err, &sinkErr) {
				outcome.err = err
				return outcome
			}
			if ctx.Err() != nil {
				// Cancellation wins the race against a provider that reports
				// the teardown as an EventError
				// (core/agent/src/runner.rs:5923-5944 selects cancellation
				// first for the same reason).
				outcome.cancelled = true
				return outcome
			}
			outcome.err = err
			return outcome
		}
	}
}

// applyStreamEvent ingests one event: it updates the round state, forwards
// the live timeline event, and returns nil to keep consuming. A *sinkFailure
// aborts the round for delivery; any other error is a stream failure.
func (run *turnRun) applyStreamEvent(outcome *streamOutcome, event model.StreamEvent) error {
	if outcome.ended {
		return model.ProtocolError("响应结束后仍收到事件")
	}
	switch event.Type {
	case model.EventMessageStart:
		if outcome.started {
			return model.ProtocolError("一个响应只能包含一次开始事件")
		}
		outcome.started = true
		return nil
	case model.EventTextDelta:
		outcome.blockAt(event.Index, model.BlockTypeText).text += event.Delta
		return run.sink.send(EventTextDelta, func(e *Event) { e.Text = event.Delta })
	case model.EventReasoningDelta:
		outcome.blockAt(event.Index, model.BlockTypeReasoning).text += event.Delta
		return run.sink.send(EventReasoningDelta, func(e *Event) { e.Text = event.Delta })
	case model.EventReasoningContinuation:
		outcome.blockAt(event.Index, model.BlockTypeReasoning).signature = event.Continuation
		return run.sink.send(EventReasoningContinuation, func(e *Event) { e.Text = event.Continuation })
	case model.EventReasoningSummaryDelta:
		// Transient provider-layer UI state; the journal never carries it.
		return nil
	case model.EventToolCallStart:
		block := outcome.blockAt(event.Index, model.BlockTypeToolCall)
		block.callID = event.CallID
		block.name = event.Name
		return nil
	case model.EventToolCallArgsDelta:
		outcome.blockAt(event.Index, model.BlockTypeToolCall).arguments += event.Delta
		return nil
	case model.EventToolCallEnd:
		outcome.blockAt(event.Index, model.BlockTypeToolCall).ended = true
		return nil
	case model.EventUsage:
		outcome.usage.UpdateFrom(event.Usage)
		return nil
	case model.EventMessageEnd:
		outcome.ended = true
		outcome.stop = event.StopReason
		return nil
	case model.EventError:
		if event.Err != nil {
			return event.Err
		}
		return model.ProtocolError("适配器发出未携带错误值的错误事件")
	default:
		return model.ProtocolError("未知的流事件类型 %q", string(event.Type))
	}
}

// finishIncomplete classifies a channel that closed before the protocol
// terminal event, mirroring the collector's interruption rules
// (internal/model/provider.go:248-253).
func (o *streamOutcome) finishIncomplete() {
	if o.err != nil || o.ended {
		return
	}
	if !o.started {
		o.err = model.ProtocolError("事件流在响应开始事件之前关闭")
		return
	}
	o.err = model.ProtocolError("事件流在响应结束事件之前关闭")
}

// blockAt returns the block at index, creating one of the given kind when
// absent and recording first-appearance order.
func (o *streamOutcome) blockAt(index uint32, kind string) *roundBlock {
	block, exists := o.blocks[index]
	if !exists {
		block = &roundBlock{kind: kind}
		o.blocks[index] = block
		o.order = append(o.order, index)
	}
	return block
}

// assembleAssistant freezes the consumed stream into the assistant history
// message and the ordered list of executable tool calls. Truncated tool call
// tails are stripped under the same stop reasons as the unified collector
// (core/model/src/stream.rs:349-384); any other malformation fails the round.
// Empty text and reasoning blocks are dropped instead of rejected: they would
// fail model.Message.Validate and never belong in history.
func (o *streamOutcome) assembleAssistant() (model.Message, []model.ToolCall, error) {
	content := make([]model.ContentBlock, 0, len(o.order))
	calls := make([]model.ToolCall, 0)
	for _, index := range o.order {
		block := o.blocks[index]
		switch block.kind {
		case model.BlockTypeText:
			if block.text == "" {
				continue
			}
			content = append(content, model.TextBlock{Text: block.text})
		case model.BlockTypeReasoning:
			if block.text == "" && block.signature == "" {
				continue
			}
			content = append(content, model.ReasoningBlock{Text: block.text, Signature: block.signature})
		case model.BlockTypeToolCall:
			call, keep, err := finalizeToolCall(o.stop, index, block)
			if err != nil {
				return model.Message{}, nil, err
			}
			if keep {
				calls = append(calls, call)
				content = append(content, model.ToolCallBlock{Call: call})
			}
		}
	}
	return model.Message{Role: model.RoleAssistant, Content: content}, calls, nil
}

// finalizeToolCall completes one tool call block. keep=false marks a call
// stripped as a truncated tail.
func finalizeToolCall(stop model.StopReason, index uint32, block *roundBlock) (call model.ToolCall, keep bool, err error) {
	if !block.ended {
		if permitsTruncatedTail(stop) {
			return model.ToolCall{}, false, nil
		}
		return model.ToolCall{}, false, model.ProtocolError("内容块 %d 的工具调用未结束", index)
	}
	call = model.ToolCall{ID: block.callID, Name: block.name, Arguments: block.arguments}
	if strings.TrimSpace(call.Arguments) == "" {
		call.Arguments = "{}"
	} else if !json.Valid([]byte(call.Arguments)) {
		if permitsTruncatedTail(stop) {
			return model.ToolCall{}, false, nil
		}
		return model.ToolCall{}, false, model.ProtocolError("工具调用 %s 的参数不是有效 JSON", call.ID)
	}
	if err := call.Validate(); err != nil {
		return model.ToolCall{}, false, err
	}
	return call, true, nil
}

// permitsTruncatedTail reports whether the stop reason tolerates incomplete
// tool call blocks (internal/model/stream.go:42-44, the unified collector's
// rule).
func permitsTruncatedTail(stop model.StopReason) bool {
	return stop == model.StopMaxTokens || stop == model.StopContentFilter || stop == model.StopCancelled
}
