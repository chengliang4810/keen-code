package kit

import (
	"time"

	"github.com/egoist/mygo/ui"
)

// Spinner is the chat loading indicator (zcode-chat-specs.md §1.10,
// chat-loading.tsx:21-36): the lucide loader-circle turning, in
// foreground-subtle, at size DIP — ZCode uses 16 (sm) and 24 (default).
// ui.Loop keeps frames coming while the element is built, so the turn
// costs nothing while idle (mygo svg.go Rotate example).
func Spinner(c *ui.Context, size float32) *ui.Element {
	pal := P(c)
	e := ui.Icon(c, loaderCircle).FontSize(size).TextColor(pal.ForegroundSubtle).Shrink(0)
	e.Rotate(e.Loop("spin", time.Second, ui.Linear) * 360)
	return e
}
