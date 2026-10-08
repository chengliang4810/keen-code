package kit

import (
	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// Popover, the drawn floating panel (docs/go-migration.md §4.2 B-stream
// popover.go). Colors come from the palette via P; the panel metrics
// follow zcode-shell-specs.md §4: "rounded-lg border bg-popover shadow-md".

// shadowMedium approximates the ZCode shadow-md (zcode-tokens.md §8: two
// neutral-black layers at 0.1) as a single soft layer; mygo paints one
// shadow per element (ui/element.go:803). It is a shadow, not a palette
// token, and shared by the drawn floating layers of this stream.
var shadowMedium = ui.RGBA(0, 0, 0, 0.12)

// Popover shows fn's elements in a floating panel below anchor while
// *open is true; clicking outside it or pressing Escape sets *open to
// false, and the panel flips above the anchor when there is no room
// below (ui/base.go:366-387). The panel carries the floating-layer look
// (rounded-lg border bg-popover p-1 shadow-md, zcode-shell-specs.md §4);
// the model selector popup uses it. fn builds into the current frame, so
// read nothing from the anchor element it does not own.
func Popover(c *ui.Context, anchor *ui.Element, open *bool, fn func()) *ui.Element {
	pal := P(c)
	return ui.PopoverBase(c, anchor, open, func(panel *ui.Element) {
		panel.MinWidth(anchor.Bounds().W)
		panel.Padding(theme.SpaceUnit).Radius(theme.RadiusLG).Background(pal.Popover).Border(1, pal.Border)
		panel.TextColor(pal.Foreground)
		panel.Shadow(0, 4, 12, 0, shadowMedium)
		fn()
	})
}
