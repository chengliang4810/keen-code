package kit

import (
	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// TextField is the single-line input of the ZCode Input component
// (zcode-shell-specs.md §3, packages/ui/src/components/ui/input.tsx:9-22):
// "h-7 rounded-md px-2" on "border border-input-border bg-input
// text-foreground", hovering brightens the border
// (border-input-border-hover) and focus switches it to
// input-border-focused over bg-input-focused. It edits *value through
// ui.TextInputBase, the look-free input base — never ui.TextInput, whose
// theme face would double the shell (docs/go-migration.md §4.2).
//
// The input is labeled with its placeholder so tests can find and click
// it (input contents are not findable, mygo-ui-notes.md §9.5).
func TextField(c *ui.Context, value *string, placeholder string) *ui.Element {
	pal := P(c)
	e := ui.TextInputBase(c, value)
	e.Height(28).Radius(theme.RadiusMD).PaddingX(8) // h-7 rounded-md px-2
	bg := pal.Input
	border := pal.Border // border-input-border = border
	if e.Focused() {
		// focus-visible:border-input-border-focused focus-visible:bg-input-focused;
		// text inputs are always focus-visible once focused.
		border = pal.InputBorderFocused
		bg = pal.InputFocused
	} else if e.Hovered() {
		border = pal.InputBorderHover
	}
	e.Background(bg).TextColor(pal.Foreground).Border(1, border)
	if placeholder != "" {
		e.Placeholder(placeholder)
		// mygo paints placeholders in theme.TextMuted (bridged
		// ForegroundSubtle) rather than ZCode's foreground-subtlest;
		// recorded as an intentional difference in docs/go-migration.md
		// §10.
		e.Label(placeholder)
	}
	return e
}
