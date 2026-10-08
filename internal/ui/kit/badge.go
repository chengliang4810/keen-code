package kit

import (
	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// BadgeStyle selects a badge pill's colors. The neutral pill is the
// settings badge (zcode-shell-specs.md §3, SettingsPageParts.tsx:156-162:
// "rounded-md bg-surface px-2.5 py-1 text-ui-base font-medium
// text-foreground-subtle"); the success pill is the task-list notice
// badge (TaskListItem.tsx:636-640: "h-5 px-2 bg-success/14 text-success").
type BadgeStyle string

const (
	BadgeNeutral BadgeStyle = "neutral"
	BadgeSuccess BadgeStyle = "success"
)

// Badge creates a pill showing text.
func Badge(c *ui.Context, text string, style BadgeStyle) *ui.Element {
	pal := P(c)
	bg, fg := pal.Surface.Over(pal.Background), pal.ForegroundSubtle
	height := float32(0)
	paddingY := float32(4)  // py-1
	paddingX := float32(10) // px-2.5
	switch style {
	case BadgeSuccess:
		bg, fg = pal.Success.Alpha(0.14).Over(pal.Background), pal.Success
		height, paddingY, paddingX = 20, 0, 8 // h-5 px-2
	}
	pill := ui.Row(c).Radius(theme.RadiusMD).Padding(paddingY, paddingX).Shrink(0)
	if height > 0 {
		pill.Height(height)
	}
	pill.Background(bg)
	pill.Children(func() {
		ui.Text(c, text).SingleLine().FontSize(theme.FontBase).FontWeight(theme.WeightMedium).TextColor(fg)
	})
	return pill
}

// StatusDot is the 6px state dot of the task list (TaskListItem.tsx:573-582:
// error bg-destructive, unread sky, idle bg-border). A running indicator
// swaps in Spinner at the call site. Name the color with a palette field,
// not a literal.
func StatusDot(c *ui.Context, color ui.Color) *ui.Element {
	return ui.Box(c).Size(6, 6).Radius(3).Background(color).Shrink(0)
}
