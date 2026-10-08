package kit

import (
	"slices"
	"testing"

	"github.com/egoist/mygo/ui"
)

// TestSessionRowMenu drives the session-row context menu headlessly: the
// native menu lists 重命名 / separator / 删除, and Chosen dispatches to the
// right callback (default state: no menu before a right click; the menu
// closes after a choice).
func TestSessionRowMenu(t *testing.T) {
	renamed, deleted := false, false
	view := func(c *ui.Context) {
		row := ui.Row(c).Padding(8).Children(func() {
			ui.Text(c, "会话 甲")
		})
		ContextMenu(row, SessionRowMenu(func() { renamed = true }, func() { deleted = true }))
	}
	tt := ui.NewTester(view, 320, 200)

	if tt.Menu() != nil {
		t.Fatalf("a menu shows before any click: %q", tt.Menu())
	}
	if err := tt.RightClick("会话 甲"); err != nil {
		t.Fatal(err)
	}
	if got, want := tt.Menu(), []string{"重命名", "-", "删除"}; !slices.Equal(got, want) {
		t.Fatalf("menu %q, want %q", got, want)
	}
	if err := tt.ChooseMenuItem("重命名"); err != nil {
		t.Fatal(err)
	}
	if !renamed || deleted {
		t.Errorf("after 重命名: renamed=%v deleted=%v", renamed, deleted)
	}
	if tt.Menu() != nil {
		t.Error("the menu stays after a choice")
	}

	tt.RightClick("会话 甲")
	if err := tt.ChooseMenuItem("删除"); err != nil {
		t.Fatal(err)
	}
	if !deleted {
		t.Error("删除 was not dispatched")
	}

	// Closing without a choice dispatches nothing.
	renamed, deleted = false, false
	tt.RightClick("会话 甲")
	tt.CloseMenu()
	if renamed || deleted {
		t.Error("a closed menu dispatched a callback")
	}
}

// TestSessionRowMenuNilCallbacks checks that nil callbacks stay a no-op
// instead of panicking when an item is chosen.
func TestSessionRowMenuNilCallbacks(t *testing.T) {
	view := func(c *ui.Context) {
		row := ui.Row(c).Padding(8).Children(func() {
			ui.Text(c, "会话 乙")
		})
		ContextMenu(row, SessionRowMenu(nil, nil))
	}
	tt := ui.NewTester(view, 320, 200)
	if err := tt.RightClick("会话 乙"); err != nil {
		t.Fatal(err)
	}
	if err := tt.ChooseMenuItem("重命名"); err != nil {
		t.Fatal(err)
	}
	if err := tt.RightClick("会话 乙"); err != nil {
		t.Fatal(err)
	}
	if err := tt.ChooseMenuItem("删除"); err != nil {
		t.Fatal(err)
	}
}
