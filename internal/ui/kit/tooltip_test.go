package kit

import (
	"testing"
	"time"

	"github.com/egoist/mygo/ui"
)

// TestTipShowsAfterRest covers the drawn tooltip: hidden by default, shown
// after the pointer rests on the target (tipDelay shortened for the
// test), with the key pill content, and hidden again once the pointer
// leaves.
func TestTipShowsAfterRest(t *testing.T) {
	old := tipDelay
	tipDelay = 0
	defer func() { tipDelay = old }()

	view := func(c *ui.Context) {
		ui.Column(c).Fill().Padding(20).Children(func() {
			target := ui.Text(c, "悬停目标")
			Tip(c, target, func() {
				ui.Text(c, "提示内容")
				KeyPill(c, "⌘K")
			})
		})
	}
	tt := ui.NewTester(view, 320, 240)

	if tt.HasText("提示内容") || tt.HasText("⌘K") {
		t.Fatalf("the tip shows before any hover: %q", tt.Texts())
	}
	r, ok := tt.Find("悬停目标")
	if !ok {
		t.Fatal("the target is not on the frame")
	}
	tt.Move(r.X+r.W/2, r.Y+r.H/2)
	// After() only arms a wake time (ui/runtime.go:573-581); the headless
	// tester needs an explicit frame for the delay to elapse.
	tt.Frame()
	if !tt.HasText("提示内容") || !tt.HasText("⌘K") {
		t.Fatalf("the tip did not show after the pointer rested: %q", tt.Texts())
	}

	tt.Move(2, 2) // off the target
	if tt.HasText("提示内容") {
		t.Errorf("the tip stays after the pointer leaves: %q", tt.Texts())
	}
}

// TestTipIgnoresDisabled checks that a disabled target never shows a tip.
func TestTipIgnoresDisabled(t *testing.T) {
	old := tipDelay
	tipDelay = 0
	defer func() { tipDelay = old }()

	view := func(c *ui.Context) {
		ui.Column(c).Fill().Padding(20).Children(func() {
			target := ui.Text(c, "悬停目标").Disabled(true)
			Tip(c, target, func() { ui.Text(c, "提示内容") })
		})
	}
	tt := ui.NewTester(view, 320, 240)
	r, ok := tt.Find("悬停目标")
	if !ok {
		t.Fatal("the disabled target is not on the frame")
	}
	tt.Move(r.X+r.W/2, r.Y+r.H/2)
	if tt.HasText("提示内容") {
		t.Errorf("a disabled target showed a tip: %q", tt.Texts())
	}
}

// TestNativeTooltip covers the native string path: after the 600ms rest
// the engine tooltip shows the text. The sleep waits out the engine's
// fixed delay (ui/widgets.go:562-564); there is no test knob for it.
func TestNativeTooltip(t *testing.T) {
	view := func(c *ui.Context) {
		ui.Column(c).Fill().Padding(20).Children(func() {
			Tooltip(ui.Text(c, "悬停目标"), "原生提示")
		})
	}
	tt := ui.NewTester(view, 320, 240)
	if tt.HasText("原生提示") {
		t.Fatal("the native tooltip shows before any hover")
	}
	r, ok := tt.Find("悬停目标")
	if !ok {
		t.Fatal("the target is not on the frame")
	}
	tt.Move(r.X+r.W/2, r.Y+r.H/2)
	time.Sleep(700 * time.Millisecond)
	tt.Frame()
	if !tt.HasText("原生提示") {
		t.Errorf("the native tooltip did not show after the rest: %q", tt.Texts())
	}
}
