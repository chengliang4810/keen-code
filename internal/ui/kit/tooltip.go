package kit

import (
	"time"

	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// Tooltips, both paths mygo offers (docs/go-migration.md §4.2 B-stream
// tooltip.go). They must not be mixed on one element:
//
//  1. the native string tooltip, target.Tooltip(s) — engine-drawn after a
//     600ms rest, styled by the bridged theme; the basic-control stream
//     (A) uses it directly and must not depend on this file;
//  2. Tip, the drawn tooltip below — palette colors (bg-tooltip,
//     text-ui-sm), rich content and keyboard-hint pills, for the tool-card
//     error details and key labels.

// tipDelay is how long the pointer must rest on the target before Tip
// shows: the 600ms of the native tooltip (ui/widgets.go:562-564). It is a
// variable only so headless tests can shorten it; treat it as a constant.
var tipDelay = 600 * time.Millisecond

// Tooltip attaches the native string tooltip s to target. It returns the
// target for chaining.
func Tooltip(target *ui.Element, s string) *ui.Element {
	return target.Tooltip(s)
}

// tipState is the hover clock of one Tip, kept from frame to frame on the
// target's element state (ui/context.go Local).
type tipState struct {
	since time.Time
}

// Tip shows content in a drawn tooltip while the pointer rests on target
// for tipDelay. The panel sits just below the target, clamped to the
// window and flipped above it when there is no room below, from the
// second visible frame on, once its measured size is known (mygo's
// keepInWindow is package-private, ui/widgets.go:678-687, so the kit
// clamps by hand). It paints bg-tooltip at text-ui-sm (zcode-tokens.md §2
// and §7.1); content supplies the elements, plain text or KeyPill rows.
func Tip(c *ui.Context, target *ui.Element, content func()) {
	pal := P(c)
	st := ui.Local(target, "kit.tip", func() tipState { return tipState{} })
	if target.IsDisabled() || !target.Hovered() {
		st.since = time.Time{}
		return
	}
	now := time.Now()
	if st.since.IsZero() {
		st.since = now
		c.After(tipDelay)
		return
	}
	if wait := tipDelay - now.Sub(st.since); wait > 0 {
		c.After(wait)
		return
	}
	b := target.Bounds()
	w, h := c.Size()
	x, y := b.X, b.Y+b.H+4
	ui.Overlay(c, func() {
		panel := ui.Box(c)
		panel.Padding(6, 10).Radius(theme.RadiusLG).Background(pal.Tooltip)
		panel.TextColor(pal.TooltipForeground).FontSize(theme.FontSM)
		panel.Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.25))
		panel.MaxWidth(384) // max-w-96, the tool-card error detail cap
		panel.Role(ui.RoleTooltip)
		panel.Children(content)
		if r := panel.Bounds(); r.W > 0 {
			if x+r.W > w-4 {
				x = w - 4 - r.W
			}
			if x < 4 {
				x = 4
			}
			if y+r.H > h-4 {
				y = b.Y - r.H - 4
			}
		}
		panel.Absolute().Left(x).Top(y)
	})
}

// KeyPill is one keyboard-hint pill of a tooltip: monospace text-ui-xs on
// a bg-tooltip-tag chip (zcode-tokens.md §2 --color-tooltip-tag,
// --color-tooltip-tag-foreground). Lay pills out in a Row inside the
// Tip content.
func KeyPill(c *ui.Context, key string) *ui.Element {
	pal := P(c)
	pill := ui.Box(c).Padding(2, 6).Radius(theme.RadiusSM).Shrink(0).Background(pal.TooltipTag)
	pill.Children(func() {
		ui.Text(c, key).Font("monospace").FontSize(theme.FontXS).TextColor(pal.TooltipTagForeground).SingleLine()
	})
	return pill
}
