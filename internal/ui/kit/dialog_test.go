package kit

import (
	"testing"

	"github.com/egoist/mygo/ui"
)

// dialogView wires one dialog whose last non-none result lands in res, so
// assertions survive the frames after the close (a closed dialog reports
// DialogNone again).
func dialogView(open *bool, opts DialogOptions, res *DialogResult) func(c *ui.Context) {
	return func(c *ui.Context) {
		if r := Dialog(c, open, opts); r != DialogNone {
			*res = r
		}
	}
}

// TestDialogKeysAndButtons covers the default state (open with title,
// description and the focused confirm), the confirm path (Enter), the
// cancel paths (Escape, backdrop click, cancel button), and that a closed
// dialog renders nothing.
func TestDialogKeysAndButtons(t *testing.T) {
	open := true
	var res DialogResult
	opts := DialogOptions{Title: "标题文字", Description: "描述文字", ConfirmLabel: "确认", CancelLabel: "取消"}
	tt := ui.NewTester(dialogView(&open, opts, &res), 480, 360)

	if !tt.HasText("标题文字") || !tt.HasText("描述文字") {
		t.Fatalf("the dialog content is missing: %q", tt.Texts())
	}
	if !tt.Focused("确认") {
		t.Fatal("the confirm button has no focus")
	}

	tt.Key(0, ui.KeyEnter)
	if res != DialogConfirm || open {
		t.Fatalf("Enter: res=%v open=%v, want confirm and closed", res, open)
	}
	if tt.HasText("标题文字") {
		t.Errorf("the dialog stays on the frame after closing: %q", tt.Texts())
	}

	open, res = true, DialogNone
	tt.Frame()
	tt.Key(0, ui.KeyEscape)
	if res != DialogCancel || open {
		t.Fatalf("Escape: res=%v open=%v, want cancel and closed", res, open)
	}

	open, res = true, DialogNone
	tt.Frame()
	tt.ClickAt(5, 5) // the backdrop
	if res != DialogCancel || open {
		t.Fatalf("backdrop: res=%v open=%v, want cancel and closed", res, open)
	}

	open, res = true, DialogNone
	tt.Frame()
	tt.Click("取消")
	if res != DialogCancel || open {
		t.Fatalf("cancel button: res=%v open=%v, want cancel and closed", res, open)
	}

	open, res = true, DialogNone
	tt.Frame()
	tt.Click("确认")
	if res != DialogConfirm || open {
		t.Fatalf("confirm button: res=%v open=%v, want confirm and closed", res, open)
	}
}

// TestDialogDefaultLabels checks the fallback labels 确认/取消 and the
// closed default (nothing on the frame while *open is false).
func TestDialogDefaultLabels(t *testing.T) {
	open := false
	res := DialogNone
	tt := ui.NewTester(dialogView(&open, DialogOptions{Title: "标题"}, &res), 480, 360)
	if tt.HasText("标题") || res != DialogNone {
		t.Fatalf("a closed dialog renders or reports: res=%v %q", res, tt.Texts())
	}

	open = true
	tt.Frame()
	if !tt.HasText("标题") || !tt.HasText("确认") || !tt.HasText("取消") {
		t.Fatalf("default labels missing: %q", tt.Texts())
	}
}

// TestConfirmDestructive covers the compact confirm: key hint chips in the
// footer buttons, the focused destructive confirm, and the cancel path.
func TestConfirmDestructive(t *testing.T) {
	open := true
	var res DialogResult
	view := func(c *ui.Context) {
		if r := Confirm(c, &open, ConfirmOptions{
			Title:        "删除会话？",
			Description:  "此操作不可撤销。",
			ConfirmLabel: "删除",
			Destructive:  true,
		}); r != DialogNone {
			res = r
		}
	}
	tt := ui.NewTester(view, 480, 360)

	if !tt.HasText("删除会话？") || !tt.HasText("此操作不可撤销。") {
		t.Fatalf("confirm content missing: %q", tt.Texts())
	}
	if !tt.HasText("esc") || !tt.HasText("⏎") {
		t.Errorf("the footer key hints are missing: %q", tt.Texts())
	}
	if !tt.Focused("删除") {
		t.Fatal("the confirm button has no focus")
	}
	tt.Click("取消")
	if res != DialogCancel || open {
		t.Fatalf("cancel: res=%v open=%v, want cancel and closed", res, open)
	}
}

// TestPromptInput covers the rename prompt: the field takes the focus,
// typing then Enter confirms with the typed text, the cancel path keeps
// the dialog closed, and reopening resets the buffer to Initial.
func TestPromptInput(t *testing.T) {
	open := true
	var text string
	var res DialogResult
	view := func(c *ui.Context) {
		txt, r := Prompt(c, &open, PromptOptions{
			Title:        "重命名会话",
			Placeholder:  "输入名称",
			Initial:      "旧名称",
			ConfirmLabel: "保存",
			CancelLabel:  "取消",
		})
		text = txt
		if r != DialogNone {
			res = r
		}
	}
	tt := ui.NewTester(view, 480, 360)

	// The input holds the focus (TaskRenameDialog semantics), so typed
	// text lands in the field after the Initial (the editor caret sits at
	// the end; select-on-open is not part of v1).
	tt.Type("新名称")
	tt.Key(0, ui.KeyEnter)
	if res != DialogConfirm || open {
		t.Fatalf("Enter: res=%v open=%v, want confirm and closed", res, open)
	}
	if text != "旧名称新名称" {
		t.Errorf("after typing: text = %q, want %q", text, "旧名称新名称")
	}

	// Cancel keeps the text but closes.
	open, res = true, DialogNone
	tt.Frame()
	tt.Key(0, ui.KeyEscape)
	if res != DialogCancel || open {
		t.Fatalf("Escape: res=%v open=%v, want cancel and closed", res, open)
	}

	// Reopening resets the buffer to Initial, so a plain Enter confirms
	// 旧名称, not the previously typed text.
	open, res = true, DialogNone
	tt.Frame()
	tt.Key(0, ui.KeyEnter)
	if res != DialogConfirm {
		t.Fatalf("reopen + Enter: res=%v, want confirm", res)
	}
	if text != "旧名称" {
		t.Errorf("reopen: text = %q, want the Initial %q", text, "旧名称")
	}
}
