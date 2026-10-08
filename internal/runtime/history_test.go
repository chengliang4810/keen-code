package runtime

import (
	"reflect"
	"testing"

	"keencode/internal/model"
)

// toolPtr is a small helper for event fixtures.
func toolPtr(callID, name, status string) *ToolEvent {
	return &ToolEvent{CallID: callID, Name: name, Status: status}
}

func TestEventsToHistory(t *testing.T) {
	toolResult := func(callID, content string) Event {
		return Event{ID: newID(), Type: EventToolResult, Text: content, Tool: toolPtr(callID, "", ToolStatusCompleted)}
	}
	cases := []struct {
		name   string
		events []Event
		want   []model.Message
	}{
		{
			name: "user and assistant text",
			events: []Event{
				{ID: "u1", Type: EventUserMessage, Text: "你好"},
				{ID: "d1", Type: EventTextDelta, Text: "你"},
				{ID: "d2", Type: EventTextDelta, Text: "好"},
				{ID: "e1", Type: EventTurnCompleted, StopReason: "end_turn"},
			},
			want: []model.Message{
				model.TextMessage(model.RoleUser, "你好"),
				{Role: model.RoleAssistant, Content: []model.ContentBlock{model.TextBlock{Text: "你好"}}},
			},
		},
		{
			name: "reasoning with signature folds into block",
			events: []Event{
				{ID: "u1", Type: EventUserMessage, Text: "想想"},
				{ID: "r1", Type: EventReasoningDelta, Text: "先想"},
				{ID: "r2", Type: EventReasoningDelta, Text: "再答"},
				{ID: "rc", Type: EventReasoningContinuation, Text: "SIG"},
				{ID: "d1", Type: EventTextDelta, Text: "答案"},
				{ID: "e1", Type: EventTurnCompleted, StopReason: "end_turn"},
			},
			want: []model.Message{
				model.TextMessage(model.RoleUser, "想想"),
				{Role: model.RoleAssistant, Content: []model.ContentBlock{
					model.ReasoningBlock{Text: "先想再答", Signature: "SIG"},
					model.TextBlock{Text: "答案"},
				}},
			},
		},
		{
			name: "tool call and result pair into two messages",
			events: []Event{
				{ID: "u1", Type: EventUserMessage, Text: "读文件"},
				{ID: "d1", Type: EventTextDelta, Text: "我来读"},
				{ID: "t1", Type: EventToolStart, Tool: toolPtr("c1", "read_file", ToolStatusRunning)},
				{ID: "a1", Type: EventToolArgs, Text: `{"path":`, Tool: toolPtr("c1", "", "")},
				{ID: "a2", Type: EventToolArgs, Text: `"a.txt"}`, Tool: toolPtr("c1", "", "")},
				{ID: "te", Type: EventToolEnd, Tool: toolPtr("c1", "", "")},
				toolResult("c1", "文件内容"),
				{ID: "d2", Type: EventTextDelta, Text: "读完了"},
				{ID: "e1", Type: EventTurnCompleted, StopReason: "end_turn"},
			},
			want: []model.Message{
				model.TextMessage(model.RoleUser, "读文件"),
				{Role: model.RoleAssistant, Content: []model.ContentBlock{
					model.TextBlock{Text: "我来读"},
					model.ToolCallBlock{Call: model.ToolCall{ID: "c1", Name: "read_file", Arguments: `{"path":"a.txt"}`}},
				}},
				model.ToolResultMessage(model.ToolResult{CallID: "c1", Content: "文件内容"}),
				{Role: model.RoleAssistant, Content: []model.ContentBlock{model.TextBlock{Text: "读完了"}}},
			},
		},
		{
			name: "dangling call without result is dropped",
			events: []Event{
				{ID: "u1", Type: EventUserMessage, Text: "崩溃前"},
				{ID: "d1", Type: EventTextDelta, Text: "部分"},
				{ID: "t1", Type: EventToolStart, Tool: toolPtr("c1", "bash", ToolStatusRunning)},
				{ID: "e1", Type: EventTurnFailed, Text: "应用中断"},
			},
			want: []model.Message{
				model.TextMessage(model.RoleUser, "崩溃前"),
				{Role: model.RoleAssistant, Content: []model.ContentBlock{model.TextBlock{Text: "部分"}}},
			},
		},
		{
			name: "orphan result is dropped",
			events: []Event{
				{ID: "u1", Type: EventUserMessage, Text: "问"},
				toolResult("ghost", "无主结果"),
				{ID: "e1", Type: EventTurnCompleted, StopReason: "end_turn"},
			},
			want: []model.Message{
				model.TextMessage(model.RoleUser, "问"),
			},
		},
		{
			name: "unknown event types are ignored",
			events: []Event{
				{ID: "u1", Type: EventUserMessage, Text: "问"},
				{ID: "x1", Type: EventType("future_thing"), Text: "新东西"},
				{ID: "e1", Type: EventTurnCompleted, StopReason: "end_turn"},
			},
			want: []model.Message{
				model.TextMessage(model.RoleUser, "问"),
			},
		},
		{
			name:   "empty stream builds nothing",
			events: nil,
			want:   nil,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := eventsToHistory(tc.events)
			if len(got) != len(tc.want) {
				t.Fatalf("got %d messages, want %d:\n%+v", len(got), len(tc.want), got)
			}
			for i := range got {
				if !reflect.DeepEqual(got[i], tc.want[i]) {
					t.Fatalf("message %d =\n%+v\nwant\n%+v", i, got[i], tc.want[i])
				}
			}
		})
	}
}

// TestEventsToHistoryMessagesValidate pins that rebuilt messages pass the
// unified layer validation (a rebuild that produces invalid messages would
// poison the next request).
func TestEventsToHistoryMessagesValidate(t *testing.T) {
	events := []Event{
		{ID: "u1", Type: EventUserMessage, Text: "问"},
		{ID: "r1", Type: EventReasoningDelta, Text: "推理"},
		{ID: "rc", Type: EventReasoningContinuation, Text: "SIG"},
		{ID: "t1", Type: EventToolStart, Tool: toolPtr("c1", "read_file", ToolStatusRunning)},
		{ID: "a1", Type: EventToolArgs, Text: `{"path":"a"}`, Tool: toolPtr("c1", "", "")},
		{ID: "tr", Type: EventToolResult, Text: "结果", Tool: toolPtr("c1", "", ToolStatusFailed)},
		{ID: "e1", Type: EventTurnCompleted, StopReason: "end_turn"},
	}
	for i, msg := range eventsToHistory(events) {
		if err := msg.Validate(); err != nil {
			t.Fatalf("message %d invalid: %v\n%+v", i, err, msg)
		}
	}
}
