package kit

import (
	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// Select and Popover, the drawn floating layers (docs/go-migration.md
// §4.2 B-stream select.go / popover.go). Colors come from the palette via
// P; the panel metrics follow zcode-shell-specs.md §3 (Select) and §4
// (Dialog/Toast floating surfaces): popup "rounded-lg border bg-menu p-1
// shadow-md" with items rounded-md inside, popover panel bg-popover.

// SelectItem is one option of a Select: the value written into the
// selection pointer and the label shown for it.
type SelectItem[T ~string] struct {
	Value T
	Label string
}

// SelectOptions carries the trigger variants of Select. The zero value is
// the default trigger of zcode-shell-specs.md §3 (h-7 rounded-md pl-2
// pr-1 border bg-input); LG switches to the settings-page trigger
// (h-8 rounded-lg pl-3 pr-2, used with a fixed width by the caller).
// Plan deviation: the plan sketch has no options parameter; the variadic
// keeps the one-item call sites shaped exactly like it.
type SelectOptions struct {
	// LG renders the lg trigger tier.
	LG bool
	// Disabled renders the trigger inert: no popup opens and the whole
	// control dims to the opacity-50 of the ZCode disabled state.
	Disabled bool
	// Ghost is the composer model trigger (ModelConfigSelect's ghost sm).
	Ghost bool
}

// Select is a drawn drop-down choosing one of items into *sel, on top of
// ui.SelectBase (ui/base.go:254-358): the base owns the open/highlight
// state, the keyboard (arrows, Home/End, Enter/Space) and the item
// choice; this function paints the trigger and the popup in the ZCode
// look. It returns the trigger element; give it a Key or width at the
// call site by wrapping it, never directly (it consumes input at
// construction, ui/element.go:465-480).
func Select[T ~string](c *ui.Context, sel *T, items []SelectItem[T], opts ...SelectOptions) *ui.Element {
	pal := P(c)
	var o SelectOptions
	if len(opts) > 0 {
		o = opts[0]
	}

	// The label of the selected item, or the raw value when no item
	// matches (ZCode shows labels, the selection holds values).
	label := string(*sel)
	for _, it := range items {
		if it.Value == *sel {
			label = it.Label
			break
		}
	}

	if o.Disabled {
		// SelectBase would consume the opening click before Disabled can
		// take effect, so a disabled select is a plain painted trigger.
		trig := ui.ButtonBase(c)
		paintSelectTrigger(c, pal, trig, label, o, false)
		trig.Disabled(true)
		return trig
	}

	s := ui.SelectBase(c, sel)
	// Style the base's own trigger element (as ui.Select styles it): a
	// second button would not receive the clicks SelectBase listens for.
	paintSelectTrigger(c, pal, s.Trigger, label, o, true)
	revealed := ui.Local(s.Trigger, "revealedOption", func() int { return -1 })
	if !s.Open() {
		*revealed = -1
	}
	// The popup: rounded-lg border bg-menu p-1 shadow-md, items rounded-md
	// (zcode-shell-specs.md §3; components/ui/select.tsx:100-120).
	s.Popup(func(panel *ui.Element) {
		panel.Padding(theme.SpaceUnit).Radius(theme.RadiusLG).Background(pal.Menu).Border(1, pal.Border)
		panel.TextColor(pal.Foreground)
		panel.Shadow(0, 4, 12, 0, shadowMedium)
		// Bound the option list; PopoverBase flips this complete surface above
		// the trigger when there is not enough room below it.
		_, height := c.Size()
		ui.Scroll(c).Label("Select options").MaxHeight(min(320, height-24)).Children(func() {
			for i, it := range items {
				el := s.Item(it.Value)
				el.Padding(6, 8).Radius(theme.RadiusMD).Shrink(0)
				if el.Highlighted() {
					if *revealed != i {
						el.ScrollIntoView()
						*revealed = i
					}
					el.Background(pal.MenuHover)
				}
				selected := it.Value == *sel
				el.Children(func() {
					ui.Text(c, it.Label).SingleLine()
					if selected {
						Icon(c, IconCheck, 14)
					}
				})
			}
		})
	})
	return s.Trigger
}

// paintSelectTrigger styles the trigger row of a Select per
// zcode-shell-specs.md §3 (components/ui/select.tsx:14-44,63-98): input
// background, 1px border that hovers to input-border-hover and focuses to
// input-border-focused, the selected label left and a 14px chevron right.
// It paints the element it is given (SelectBase's trigger, or a fresh
// ButtonBase for the disabled case) and fills its children.
func paintSelectTrigger(c *ui.Context, pal *theme.Palette, trig *ui.Element, label string, o SelectOptions, interactive bool) {
	h, radius, pl, pr := float32(28), theme.RadiusMD, float32(8), float32(4)
	if o.LG {
		h, radius, pl, pr = 32, theme.RadiusLG, 12, 8
	}
	trig.Height(h).Radius(radius).Padding(0, pr, 0, pl).Justify(ui.SpaceBetween).Gap(4)
	bg, border := pal.Input, pal.Border
	if interactive {
		switch {
		case trig.Focused():
			border = pal.InputBorderFocused
		case trig.Hovered():
			border = pal.InputBorderHover
		}
	}
	if o.Ghost {
		trig.TextColor(pal.ForegroundSubtle)
		if interactive && trig.Hovered() {
			trig.Background(pal.Hover.Over(pal.Input)).TextColor(pal.Foreground)
		}
	} else {
		trig.Background(bg).Border(1, border).TextColor(pal.Foreground)
	}
	trig.Children(func() {
		ui.Text(c, label).SingleLine().Grow(1).MinWidth(0)
		Icon(c, IconChevronDown, 14).TextColor(pal.ForegroundSubtle)
	})
}
