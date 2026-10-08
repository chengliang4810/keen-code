package kit

import (
	"fmt"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"
)

func TestUserBubbleContainsBody(t *testing.T) {
	tt := newMujicaDark(t, 640, 320, func(c *ui.Context) {
		ChatList(c, &ChatListState{}, []Entry{{
			ID:   "u1",
			Kind: EntryUser,
			Text: "User message inside its bubble",
			Time: time.Now(),
		}})
	})
	bubble := mustFind(t, tt, "你")
	body := mustFind(t, tt, "User message inside its bubble")
	if body.X < bubble.X || body.Y < bubble.Y || body.X+body.W > bubble.X+bubble.W || body.Y+body.H > bubble.Y+bubble.H {
		t.Fatalf("body %#v escapes bubble %#v", body, bubble)
	}
	// The user bubble hugs the right edge: the body starts in the right
	// half of the timeline.
	if body.X < bubble.X+bubble.W/2 {
		t.Fatalf("bubble not right-aligned: body %#v bubble %#v", body, bubble)
	}
}

func TestEditorEnterAtMiddleSubmitsEntireDraft(t *testing.T) {
	h, tt := newEditorTest(t)
	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Type("abcd")
	tt.Key(0, ui.KeyLeft)
	tt.Key(0, ui.KeyLeft)
	tt.Key(0, ui.KeyEnter)
	if h.sent != 1 || h.draft != "" {
		t.Fatalf("Enter at middle: sent=%d draft=%q", h.sent, h.draft)
	}
}

func TestLargeSelectStaysInWindowAndKeyboardReachesLastOption(t *testing.T) {
	items := make([]SelectItem[string], 60)
	for i := range items {
		items[i] = SelectItem[string]{Value: fmt.Sprintf("model-%02d", i), Label: fmt.Sprintf("Model %02d", i)}
	}
	selected := items[0].Value
	tt := newDark(t, 720, 480, func(c *ui.Context) {
		ui.Column(c).Fill().Justify(ui.End).Padding(16).Children(func() {
			Select(c, &selected, items)
		})
	})
	if err := tt.Click("Model 00"); err != nil {
		t.Fatal(err)
	}
	viewport := mustFind(t, tt, "Select options")
	if viewport.Y < 0 || viewport.Y+viewport.H > 480 || viewport.H > 320 {
		t.Fatalf("menu escapes window: %#v", viewport)
	}
	tt.Key(0, ui.KeyEnd)
	last := mustFind(t, tt, "Model 59")
	if last.Y < viewport.Y || last.Y+last.H > viewport.Y+viewport.H {
		t.Fatalf("keyboard highlight hidden: last=%#v viewport=%#v", last, viewport)
	}
	tt.Key(0, ui.KeyEnter)
	if selected != items[59].Value {
		t.Fatalf("last model was not chosen: %s", selected)
	}
}
