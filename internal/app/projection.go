package app

import (
	"strconv"
	"strings"
	"time"

	agentui "github.com/ZacharyZhang-NY/MujicaUI/agent"

	"keencode/internal/runtime"
	"keencode/internal/ui/kit"
)

// The projection bridge (ask item 2/8): runtime events → MujicaUI chat
// timeline entries. A sessionView owns the caller-held state the timeline
// requires (thinking folds, tool-call cards, scroll positions) and
// rebuilds the entry slice as events arrive; the view function reads it
// each frame. All mutations happen on the main thread — from win.Update
// callbacks in production, or directly from the test driver in headless
// assemblies.

// sessionView is the UI projection of one conversation.
type sessionView struct {
	id         string
	title      string
	projectDir string
	createdAt  time.Time

	entries []kit.Entry
	list    kit.ChatListState
	draft   string
	running bool

	// errSummary drives the error banner above the composer; errDetail is
	// the full text behind 查看详情.
	errSummary string
	errDetail  string

	// lastSeq deduplicates the journal stream; seen holds the IDs of
	// synthetic recovery events, which carry Seq 0.
	lastSeq int64
	seen    map[string]bool

	// reasoning is the still-open reasoning block of the response being
	// streamed (nil when none).
	reasoning *kit.ReasoningState
	// assistant is the index into entries of the streaming assistant text
	// entry (-1 when none); assistantText accumulates its raw markdown.
	assistant     int
	assistantText strings.Builder
	// tools maps call id → card state; entries hold the same pointers.
	// toolStarts anchors each call's duration at its start event.
	tools      map[string]*agentui.ToolCall
	toolStarts map[string]time.Time

	// cancelEvents detaches the event subscription (pump or the hub's
	// queue); set when the app subscribes.
	cancelEvents func()
}

// newSessionView builds the projection skeleton; the MessageList draws
// the day separator of the first entry's day itself.
func newSessionView(meta runtime.SessionMeta) *sessionView {
	return &sessionView{
		id:         meta.ID,
		title:      meta.Title,
		projectDir: meta.ProjectDir,
		createdAt:  meta.CreatedAt,
		assistant:  -1,
		tools:      map[string]*agentui.ToolCall{},
		toolStarts: map[string]time.Time{},
		seen:       map[string]bool{},
	}
}

// apply folds one runtime event into the projection. Events must arrive
// in stream order (the hub guarantees it); deduplication on Seq is the
// documented belt-and-braces for subscribers.
func (v *sessionView) apply(ev runtime.Event) {
	if ev.Seq > 0 {
		if ev.Seq <= v.lastSeq {
			return
		}
		v.lastSeq = ev.Seq
	} else {
		if v.seen[ev.ID] {
			return
		}
		v.seen[ev.ID] = true
	}
	at := ev.Time
	if at.IsZero() {
		at = time.Now()
	}

	switch ev.Type {
	case runtime.EventUserMessage:
		v.closeAssistant()
		v.closeReasoning(at)
		v.entries = append(v.entries, kit.Entry{
			ID:   entryID("u", ev),
			Kind: kit.EntryUser,
			Text: ev.Text,
			Time: at,
		})
		v.errSummary, v.errDetail = "", ""

	case runtime.EventTextDelta:
		v.closeReasoning(at)
		if v.assistant < 0 {
			v.openAssistant(ev, at)
		}
		v.assistantText.WriteString(ev.Text)
		v.refreshAssistant()

	case runtime.EventReasoningDelta:
		v.closeAssistant()
		if v.reasoning == nil {
			v.reasoning = &kit.ReasoningState{Running: true, Started: at}
			v.entries = append(v.entries, kit.Entry{
				ID:        entryID("r", ev),
				Kind:      kit.EntryReasoning,
				Time:      at,
				Reasoning: v.reasoning,
			})
		}
		v.reasoning.Text += ev.Text

	case runtime.EventReasoningContinuation:
		// The opaque signature is runtime/model state; the timeline never
		// renders it.

	case runtime.EventToolArgs:
		v.closeAssistant()
		if call := v.ensureCard(ev, at); call != nil {
			call.State = agentui.AgentPending
			if ev.Tool.Name != "" {
				call.Name = ev.Tool.Name
			}
			// tool_args carries the raw input JSON as Detail; the card
			// pretty-prints it in its 参数 section.
			if ev.Tool.Detail != "" {
				call.Args = ev.Tool.Detail
			}
		}

	case runtime.EventToolStart:
		v.closeAssistant()
		if call := v.ensureCard(ev, at); call != nil {
			call.State = agentui.AgentRunning
			v.toolStarts[call.ID] = at
		}

	case runtime.EventToolEnd:
		if call := v.ensureCard(ev, at); call != nil {
			call.State = toolStateOf(ev.Tool.Status)
			if start, ok := v.toolStarts[call.ID]; ok && at.After(start) {
				call.Duration = at.Sub(start)
			}
			// The bounded preview the card folds under 结果 (错误 on
			// failure, shown in the danger color).
			if ev.Tool.Detail != "" {
				if call.State == agentui.AgentFailed {
					call.Error = ev.Tool.Detail
				} else {
					call.Result = ev.Tool.Detail
				}
			}
		}

	case runtime.EventToolResult:
		// The full result text went back to the model; the card already
		// shows the bounded preview from tool_end. Nothing visual.

	case runtime.EventPermissionDenied:
		if call := v.ensureCard(ev, at); call != nil {
			call.State = agentui.AgentSkipped
		}

	case runtime.EventUsage:
		// Token accounting has no v1 surface (deferred scope).

	case runtime.EventTurnCompleted:
		v.finishTurn()

	case runtime.EventTurnFailed:
		v.finishTurn()
		v.errSummary = strings.TrimSpace(ev.Text)
		if v.errSummary == "" {
			v.errSummary = TurnFailedSummary
		}
		v.errDetail = ev.Text

	case runtime.EventTurnCancelled:
		v.finishTurn()
	}
}

// openAssistant appends a fresh streaming assistant text entry.
func (v *sessionView) openAssistant(ev runtime.Event, at time.Time) {
	v.assistantText.Reset()
	v.assistant = len(v.entries)
	v.entries = append(v.entries, kit.Entry{
		ID:   entryID("a", ev),
		Kind: kit.EntryAssistant,
		Time: at,
	})
}

// refreshAssistant feeds the entry the accumulated source; MarkdownView
// re-parses incrementally each frame, so no blocks are precomputed.
func (v *sessionView) refreshAssistant() {
	if v.assistant < 0 || v.assistant >= len(v.entries) {
		return
	}
	v.entries[v.assistant].Text = v.assistantText.String()
}

// closeAssistant freezes the streaming text entry.
func (v *sessionView) closeAssistant() {
	if v.assistant < 0 || v.assistant >= len(v.entries) {
		v.assistant = -1
		return
	}
	v.entries[v.assistant].Text = v.assistantText.String()
	v.assistant = -1
}

// closeReasoning marks the open reasoning block finished at the given
// moment, freezing its displayed duration.
func (v *sessionView) closeReasoning(at time.Time) {
	if v.reasoning == nil {
		return
	}
	v.reasoning.Running = false
	if !v.reasoning.Started.IsZero() && at.After(v.reasoning.Started) {
		v.reasoning.Elapsed = at.Sub(v.reasoning.Started)
	}
	v.reasoning = nil
}

// ensureCard returns the card for the event's call, creating the entry on
// first sight (positioned in stream order, keyed by call id so later
// events mutate the same state the entry points at). at is the event's
// normalized time.
func (v *sessionView) ensureCard(ev runtime.Event, at time.Time) *agentui.ToolCall {
	if ev.Tool == nil || ev.Tool.CallID == "" {
		return nil
	}
	if call, ok := v.tools[ev.Tool.CallID]; ok {
		return call
	}
	call := &agentui.ToolCall{
		ID:    ev.Tool.CallID,
		Name:  ev.Tool.Name,
		State: agentui.AgentPending,
	}
	v.tools[ev.Tool.CallID] = call
	v.entries = append(v.entries, kit.Entry{
		ID:   "t:" + ev.Tool.CallID,
		Kind: kit.EntryTool,
		Time: at,
		Tool: call,
	})
	return call
}

// finishTurn closes every open fragment: the streaming text, the open
// reasoning block, and any tool cards still pending or running (an
// interrupted turn must not strand a spinner).
func (v *sessionView) finishTurn() {
	v.closeAssistant()
	v.closeReasoning(time.Now())
	for _, call := range v.tools {
		if call.State == agentui.AgentPending || call.State == agentui.AgentRunning {
			call.State = agentui.AgentSkipped
		}
	}
	v.running = false
}

// entryID builds a stable, unique row identity from the entry kind and the
// event that opened it.
func entryID(kind string, ev runtime.Event) string {
	if ev.Seq > 0 {
		return kind + ":" + strconv.FormatInt(ev.Seq, 10)
	}
	return kind + ":s" + ev.ID
}

// toolStateOf maps the runtime status vocabulary onto MujicaUI's agent
// states; denied and stopped both land on skipped (not executed), unknown
// values fail visibly as failed.
func toolStateOf(status string) agentui.AgentState {
	switch status {
	case runtime.ToolStatusPending:
		return agentui.AgentPending
	case runtime.ToolStatusRunning:
		return agentui.AgentRunning
	case runtime.ToolStatusCompleted:
		return agentui.AgentDone
	case runtime.ToolStatusDenied:
		return agentui.AgentSkipped
	case runtime.ToolStatusStopped:
		return agentui.AgentSkipped
	default:
		return agentui.AgentFailed
	}
}
