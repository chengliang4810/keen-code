package kit

import (
	"testing"

	"github.com/egoist/mygo/ui"
)

func selectView(sel *string) func(c *ui.Context) {
	items := []SelectItem[string]{
		{Value: "a", Label: "甲"},
		{Value: "b", Label: "乙"},
		{Value: "c", Label: "丙"},
	}
	return func(c *ui.Context) {
		ui.Column(c).Fill().Padding(20).Children(func() {
			Select(c, sel, items)
		})
	}
}

// TestSelectDefaultAndChoose covers the default state (closed trigger
// showing the selected label only) and the open/active state (popup lists
// all items; picking one writes the value and closes).
func TestSelectDefaultAndChoose(t *testing.T) {
	sel := "b"
	tt := ui.NewTester(selectView(&sel), 320, 240)

	if !tt.HasText("乙") {
		t.Fatalf("the trigger does not show the selected label: %q", tt.Texts())
	}
	if tt.HasText("甲") || tt.HasText("丙") {
		t.Fatalf("the popup shows while closed: %q", tt.Texts())
	}

	if err := tt.Click("乙"); err != nil { // the trigger opens the popup
		t.Fatal(err)
	}
	if !tt.HasText("甲") || !tt.HasText("丙") {
		t.Fatalf("the popup did not open: %q", tt.Texts())
	}
	if err := tt.Click("丙"); err != nil {
		t.Fatal(err)
	}
	if sel != "c" {
		t.Errorf("after choosing 丙: sel = %q, want %q", sel, "c")
	}
	if tt.HasText("甲") {
		t.Errorf("the popup stays open after a choice: %q", tt.Texts())
	}
	if !tt.HasText("丙") {
		t.Errorf("the trigger does not show the new label: %q", tt.Texts())
	}
}

// TestSelectKeyboard covers the active keyboard path of the base: reopen,
// arrow down from the highlighted selected item, Enter chooses it.
func TestSelectKeyboard(t *testing.T) {
	sel := "a"
	tt := ui.NewTester(selectView(&sel), 320, 240)

	if err := tt.Click("甲"); err != nil { // open
		t.Fatal(err)
	}
	tt.Key(0, ui.KeyDown) // highlight 乙
	tt.Key(0, ui.KeyEnter)
	if sel != "b" {
		t.Errorf("arrow down + Enter: sel = %q, want %q", sel, "b")
	}
	if tt.HasText("丙") {
		t.Errorf("the popup stays open after Enter: %q", tt.Texts())
	}
}

// TestSelectEscape checks Escape closes the popup without changing the
// selection.
func TestSelectEscape(t *testing.T) {
	sel := "a"
	tt := ui.NewTester(selectView(&sel), 320, 240)

	if err := tt.Click("甲"); err != nil {
		t.Fatal(err)
	}
	if !tt.HasText("丙") {
		t.Fatal("the popup did not open")
	}
	tt.Key(0, ui.KeyEscape)
	if tt.HasText("丙") {
		t.Errorf("Escape did not close the popup: %q", tt.Texts())
	}
	if sel != "a" {
		t.Errorf("Escape changed the selection: sel = %q", sel)
	}
}

// TestSelectDisabled covers the disabled state: the trigger renders (its
// label is visible) but clicks open nothing and the selection stays.
func TestSelectDisabled(t *testing.T) {
	sel := "a"
	view := func(c *ui.Context) {
		ui.Column(c).Fill().Padding(20).Children(func() {
			Select(c, &sel, []SelectItem[string]{{Value: "a", Label: "甲"}, {Value: "b", Label: "乙"}},
				SelectOptions{Disabled: true})
		})
	}
	tt := ui.NewTester(view, 320, 240)

	if !tt.HasText("甲") {
		t.Fatalf("the disabled trigger does not show its label: %q", tt.Texts())
	}
	if err := tt.Click("甲"); err != nil {
		t.Fatal(err)
	}
	if tt.HasText("乙") {
		t.Errorf("the popup of a disabled select opened: %q", tt.Texts())
	}
	if sel != "a" {
		t.Errorf("a disabled select changed the selection: sel = %q", sel)
	}
}
