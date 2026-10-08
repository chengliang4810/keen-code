package kit

import (
	"strings"
	"time"

	"github.com/egoist/mygo/ui"

	"github.com/ZacharyZhang-NY/MujicaUI/chat"
)

// ReasoningBlock renders the thinking section of an assistant turn on
// MujicaUI's official chat.ThinkingBlock (docs/go-migration.md §4.2): the
// block shows the running timer while thinking,「思考了 N 秒」once done,
// and folds the body under its header — open state, keyboard toggling and
// the left rule of the body are the official component's. The body is
// plain pre-wrapped text (not markdown, for performance).

// ReasoningState is the caller-held state of one reasoning block. Open
// survives frames and seeds the component's default fold; Started anchors
// the running timer and Elapsed freezes the final duration.
type ReasoningState struct {
	Running bool
	Started time.Time
	Elapsed time.Duration
	Text    string
	Open    bool
}

// ReasoningBlock renders one reasoning block, or nil while no text has
// arrived (empty reasoning is not rendered, ConversationRowView.tsx:
// 1601-1603). Handle nothing on the result.
func ReasoningBlock(c *ui.Context, st *ReasoningState) *ui.Element {
	if st == nil || strings.TrimSpace(st.Text) == "" {
		return nil
	}
	started := st.Started
	if st.Running && started.IsZero() {
		started = c.Now()
	}
	r := chat.ThinkingBlock(c, &st.Open, chat.ThinkingBlockOptions{
		Thinking: st.Running,
		Started:  started,
		Elapsed:  st.Elapsed,
	}, func() {
		ui.Text(c, st.Text)
	})
	return r.Element
}
