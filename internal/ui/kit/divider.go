package kit

import (
	"github.com/egoist/mygo/ui"
)

// Divider is the 1px separator of the shell and lists (zcode-shell-specs
// §1.1: 1px separator bar between sidebar and main area), drawn in the
// palette border. Place it in a column; DividerVertical goes in a row.
// Both stretch across the cross axis like ui.Divider.
func Divider(c *ui.Context) *ui.Element {
	pal := P(c)
	return ui.Box(c).Height(1).Shrink(0).AlignSelf(ui.Stretch).Background(pal.Border)
}

// DividerVertical is the 1px vertical separator for rows.
func DividerVertical(c *ui.Context) *ui.Element {
	pal := P(c)
	return ui.Box(c).Width(1).Shrink(0).AlignSelf(ui.Stretch).Background(pal.Border)
}
