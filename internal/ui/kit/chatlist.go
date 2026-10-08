package kit

import (
	"time"

	"github.com/egoist/mygo/ui"

	agentui "github.com/ZacharyZhang-NY/MujicaUI/agent"
	"github.com/ZacharyZhang-NY/MujicaUI/chat"

	"keencode/internal/ui/theme"
)

// ChatList renders the conversation timeline on MujicaUI's official chat
// components (docs/go-migration.md §4.2): chat.MessageList virtualizes the
// rows, follows the tail, floats the unread jump button and draws the day
// separators; every entry renders with the official widget of its kind —
// MessageBubble for the user text and the assistant body (MarkdownView
// inside), chat.ThinkingBlock for reasoning, agent.ToolCallCard for tool
// calls, and TypingIndicator as the synthetic row of a running turn. The
// draft branch keeps the product's own layout: greeting and composer
// center as one unit (zcode-chat-specs.md §3).

// EntryKind classifies one timeline row.
type EntryKind uint8

// Entry kinds.
const (
	EntryUser      EntryKind = iota // user message; Text is the body
	EntryAssistant                  // assistant markdown; Text is the source
	EntryReasoning                  // thinking block; Reasoning
	EntryTool                       // tool call; Tool
)

// Entry is one timeline row. The app owns entries (and the state structs
// they point at) across frames; IDs must be unique and stable — the
// MessageList keys its rows and every embedded widget's element state by
// them.
type Entry struct {
	ID        string
	Kind      EntryKind
	Text      string // user body or assistant markdown source
	Time      time.Time
	Reasoning *ReasoningState   // EntryReasoning
	Tool      *agentui.ToolCall // EntryTool
}

// TypingName names the assistant in the running-turn TypingIndicator row.
const TypingName = "助手"

// ChatListState is the caller-held state of the chat list. Draft switches
// to the greeting layout of a new-session draft (zcode-chat-specs.md §3);
// Greeting is its static copy (time-segmented copy is deferred to V1.1).
type ChatListState struct {
	list  chat.MessageListState
	Draft bool
	// Greeting heads the draft layout.
	Greeting string
	// Running appends the TypingIndicator row after the last entry while a
	// turn runs (zcode-chat-specs.md §1.10 signal 1).
	Running bool
	// DraftBody builds the composer below the greeting so greeting and
	// composer center as one unit (zcode-chat-specs.md §3). It runs during
	// the frame that renders the draft layout and must not be stored.
	DraftBody func()
}

// ScrollToEnd jumps to the newest entry and resumes tail following.
func (s *ChatListState) ScrollToEnd() { s.list.ScrollToEnd() }

// AtEnd reports whether the last frame showed the end of the timeline.
func (s *ChatListState) AtEnd() bool { return s.list.AtEnd() }

// turnLoaderID keys the synthetic running-turn row.
const turnLoaderID = "turn-loader"

// ChatList renders the timeline into a fill-height container. Handle
// nothing on the result; scrolling, the unread jump button and the day
// separators are self-contained.
func ChatList(c *ui.Context, st *ChatListState, entries []Entry) *ui.Element {
	root := ui.Box(c).Fill()
	root.Children(func() {
		if st.Draft {
			renderGreeting(c, st)
			return
		}
		if len(entries) == 0 {
			return
		}
		// The running turn appends one synthetic TypingIndicator row after
		// the last entry (§1.10); it leaves when the turn ends.
		n := len(entries)
		if st.Running {
			n++
		}
		last := entries[len(entries)-1].Time
		chat.MessageList(c, &st.list, n, chat.MessageListOptions{
			ID: func(i int) string {
				if i >= len(entries) {
					return turnLoaderID
				}
				return entries[i].ID
			},
			// The loader row shares the last entry's day so it never adds a
			// separator of its own.
			Date: func(i int) time.Time {
				if i >= len(entries) {
					return last
				}
				return entries[i].Time
			},
		}, func(i int) {
			if i >= len(entries) {
				chat.TypingIndicator(c, TypingName)
				return
			}
			renderChatEntry(c, entries[i])
		})
	})
	return root
}

// renderChatEntry renders the entry body with the official widget of its
// kind.
func renderChatEntry(c *ui.Context, e Entry) {
	switch e.Kind {
	case EntryUser:
		chat.MessageBubble(c, chat.MessageUser, chat.MessageBubbleOptions{}, func() {
			ui.Text(c, e.Text).LineHeight(theme.LineHeightBody)
		})
	case EntryAssistant:
		chat.MessageBubble(c, chat.MessageAssistant, chat.MessageBubbleOptions{}, func() {
			chat.MarkdownView(c, e.Text)
		})
	case EntryReasoning:
		ReasoningBlock(c, e.Reasoning)
	case EntryTool:
		if e.Tool != nil {
			agentui.ToolCallCard(c, *e.Tool, agentui.ToolCallCardOptions{})
		}
	}
}

// renderGreeting is the draft layout: greeting and composer center as one
// unit in elastic space (ConversationTimeline.tsx:1764-1771, §3) — the
// greeting block sits mb-10 right above the DraftBody composer, with the
// elastic top (min 52px, `before:`) and bottom (min 16px, `after:min-h-4`)
// whitespace around the pair. The adaptive 20-30px title of the original
// is fixed at its 30px upper bound in v1 (recorded approximation).
func renderGreeting(c *ui.Context, st *ChatListState) {
	width, height := c.Size()
	pal := P(c)
	ui.Column(c).Fill().Children(func() {
		// ZCode's before:basis-[29dvh] is a viewport-relative basis,
		// not half of the remaining height (which pushes the pair down).
		ui.Box(c).Basis(height * 0.29).MinHeight(52)
		ui.Column(c).MaxWidth(theme.ColumnDraft).Margin(0, ui.Auto, 0, ui.Auto).FillWidth().Children(func() {
			ui.Row(c).Justify(ui.Center).Children(func() {
				ui.Text(c, st.Greeting).
					FontSize(theme.FontGreeting).
					LineHeight(1.2).
					FontWeight(theme.WeightMedium).
					TextColor(pal.Foreground)
			})
			if st.DraftBody != nil {
				gap := theme.SpaceUnit * 10 // mb-10; sm:mb-8 at 640px
				if width >= 640 {
					gap = theme.SpaceUnit * 8
				}
				ui.Box(c).Height(gap)
				st.DraftBody()
			}
		})
		ui.Box(c).Grow(1).MinHeight(16) // elastic space below
	})
}
