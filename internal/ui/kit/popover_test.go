package kit

import (
	"testing"

	"github.com/egoist/mygo/ui"
)

// TestPopoverOpenClose covers the closed default, the open state with the
// panel content visible, the outside click and Escape both closing.
func TestPopoverOpenClose(t *testing.T) {
	open := false
	view := func(c *ui.Context) {
		ui.Column(c).Fill().Padding(20).Children(func() {
			anchor := ui.Button(c, "锚点")
			if anchor.Clicked() {
				open = !open
			}
			Popover(c, anchor, &open, func() {
				ui.Text(c, "浮层内容")
			})
		})
	}
	tt := ui.NewTester(view, 320, 240)

	if open || tt.HasText("浮层内容") {
		t.Fatalf("the popover shows while closed: open=%v %q", open, tt.Texts())
	}

	tt.Click("锚点")
	if !open || !tt.HasText("浮层内容") {
		t.Fatalf("the popover did not open: open=%v %q", open, tt.Texts())
	}

	// A click outside closes it.
	tt.ClickAt(5, 5)
	if open || tt.HasText("浮层内容") {
		t.Errorf("an outside click did not close the popover: open=%v %q", open, tt.Texts())
	}

	// Escape closes it too.
	tt.Click("锚点")
	if !tt.HasText("浮层内容") {
		t.Fatal("the popover did not reopen")
	}
	tt.Key(0, ui.KeyEscape)
	if open || tt.HasText("浮层内容") {
		t.Errorf("Escape did not close the popover: open=%v %q", open, tt.Texts())
	}
}
