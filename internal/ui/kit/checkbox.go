package kit

import (
	"time"

	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// fbool maps a bool to 0 or 1 for animation targets and offsets.
func fbool(b bool) float32 {
	if b {
		return 1
	}
	return 0
}

// Checkbox replicates the ZCode checkbox
// (packages/ui/src/components/ui/checkbox.tsx): a 16px box, rounded-sm,
// border-input-border over bg-input; checked it turns border-primary over
// bg-primary with a 12px check in primary-foreground. Disabling is
// mygo-native — the engine paints a disabled subtree at half opacity
// (ui/paint.go), the CSS disabled:opacity-50. It toggles *checked through
// ui.CheckboxBase; Changed on the returned row reports a new value.
func Checkbox(c *ui.Context, checked *bool, label string) *ui.Element {
	pal := P(c)
	row := ui.CheckboxBase(c, checked).Gap(8)
	on := *checked
	row.Children(func() {
		box := ui.Box(c).Size(16, 16).Radius(theme.RadiusSM).Shrink(0).Center()
		if on {
			box.Background(pal.Primary).Border(1, pal.Primary)
			box.Children(func() { Icon(c, IconCheck, 12).TextColor(pal.PrimaryForeground) })
		} else {
			box.Background(pal.Input).Border(1, pal.Border)
		}
		if label != "" {
			ui.Text(c, label).TextColor(pal.Foreground)
		}
	})
	return row
}

// Switch replicates the ZCode switch
// (packages/ui/src/components/ui/switch.tsx:13-34): a 32×18 rounded-full
// track, bg-primary when on and bg-primary/30 when off, with a 16px thumb
// of primary-foreground sitting 1px in that slides right when on.
// Disabling is mygo-native (half paint opacity). It toggles *on through
// ui.SwitchBase; Changed on the returned switch reports a new value.
func Switch(c *ui.Context, on *bool) *ui.Element {
	pal := P(c)
	sw := ui.SwitchBase(c, on).Size(32, 18).Radius(9) // rounded-full track
	pos := sw.Animate("knob", fbool(*on), 140*time.Millisecond)
	track, thumb := pal.Primary, pal.PrimaryForeground
	if !*on {
		track = pal.Primary.Alpha(0.3) // bg-primary/30
	}
	sw.Background(track)
	// The thumb slides from x=1 to x=32-16-1=15, the checked state's
	// translate-x-[calc(100%-2px)].
	x := 1 + pos*14
	sw.Draw(func(p *ui.Painter, r ui.Rect) {
		p.Fill(ui.Rect{X: r.X + x, Y: r.Y + 1, W: 16, H: 16}, thumb, 8)
	})
	return sw
}
