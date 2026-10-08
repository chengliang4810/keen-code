package kit

import (
	"testing"
	"time"

	"github.com/egoist/mygo/ui"
)

// TestToastShowAndStack covers the default state (no toast before an
// action), showing, stacking two kinds, same-message replacement, the
// accessibility announcement, and expiry.
func TestToastShowAndStack(t *testing.T) {
	view := func(c *ui.Context) {
		ui.Column(c).Fill().Padding(20).Children(func() {
			if ui.Button(c, "保存").Clicked() {
				ShowToast(c, ToastDefault, "已保存")
			}
			if ui.Button(c, "出错").Clicked() {
				ShowToast(c, ToastWarning, "保存失败")
			}
		})
		Toasts(c) // the app view renders the stack once per frame
	}
	tt := ui.NewTester(view, 480, 360)

	if tt.HasText("已保存") || tt.HasText("保存失败") {
		t.Fatalf("toasts show before any action: %q", tt.Texts())
	}

	tt.Click("保存")
	if !tt.HasText("已保存") {
		t.Fatalf("the toast did not show: %q", tt.Texts())
	}
	if anns := tt.Announcements(); len(anns) == 0 || anns[len(anns)-1] != "已保存" {
		t.Errorf("the toast was not announced: %q", anns)
	}

	// A second kind stacks above the first.
	tt.Click("出错")
	if !tt.HasText("保存失败") || !tt.HasText("已保存") {
		t.Fatalf("the stack does not hold both toasts: %q", tt.Texts())
	}

	// The same message replaces its toast instead of duplicating it.
	tt.Click("保存")
	n := 0
	for _, s := range tt.Texts() {
		if s == "已保存" {
			n++
		}
	}
	if n != 1 {
		t.Errorf("已保存 shows %d times, want 1 (replacement): %q", n, tt.Texts())
	}

	// Toasts expire.
	old := toastLife
	toastLife = 30 * time.Millisecond
	time.Sleep(50 * time.Millisecond)
	tt.Frame()
	if tt.HasText("已保存") || tt.HasText("保存失败") {
		t.Errorf("toasts outlived their life: %q", tt.Texts())
	}
	toastLife = old
}
