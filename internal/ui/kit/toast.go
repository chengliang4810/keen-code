package kit

import (
	"sync"
	"time"

	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// Toasts (docs/go-migration.md §4.2 B-stream toast.go). ZCode shows them
// top-center at top-16 with a 3s life (components/ui/toast.tsx:47-71);
// the fill is bg-toast. The surface is opaque here on purpose: ZCode's
// bg-toast/60 + backdrop-blur-xl has no native equivalent, recorded as an
// intentional difference (差异 2).
//
// The stack is two call sites with a fixed contract, because overlay
// elements only live during the frame that builds them (ui/element.go:192)
// while a toast must outlive the click that raised it:
//
//   - ShowToast records a toast; call it where the outcome happens, from
//     a view build (a Clicked branch) or inside win.Update;
//   - Toasts renders the live stack; the app view calls it once per
//     frame, last, so toasts float above everything (the app root wires
//     it next to theme.Apply).

// ToastKind selects the toast variant. ZCode also has update/info and
// action toasts (toast.tsx:311-401); v1 ships the plain and warning
// variants (docs/go-migration.md §4.2).
type ToastKind string

const (
	ToastDefault ToastKind = "default"
	ToastWarning ToastKind = "warning" // TriangleAlert prefix in the warning color
)

const (
	toastTop  = 64                     // top-16
	toastGap  = 8                      // stack gap-2
	toastPadX = 16                     // px-4
	toastPadY = 12                     // py-3
	toastFade = 200 * time.Millisecond // in/out fade, engine toast parity
)

// toastLife is how long a toast shows: the 3s of ZCode toast.tsx:70-71.
// A variable only so headless tests can shorten it; treat it as a
// constant.
var toastLife = 3 * time.Second

// toastItem is one entry of the stack. at/stamp freeze the engine clock
// the first time Toasts renders the entry, so expiry runs on one clock.
type toastItem struct {
	kind  ToastKind
	msg   string
	id    uint64
	at    time.Time
	stamp bool
}

var (
	toastMu    sync.Mutex
	toastStack []toastItem
	toastSeq   uint64
)

// ShowToast records a toast of kind with msg. A toast with the same
// message replaces the one showing (mygo c.Toast semantics,
// ui/toast.go:62-70); the stack keeps the newest three.
func ShowToast(c *ui.Context, kind ToastKind, msg string) {
	toastMu.Lock()
	for i := range toastStack {
		if toastStack[i].msg == msg {
			toastStack = append(toastStack[:i], toastStack[i+1:]...)
			break
		}
	}
	toastSeq++
	toastStack = append(toastStack, toastItem{kind: kind, msg: msg, id: toastSeq})
	if len(toastStack) > 3 {
		toastStack = toastStack[len(toastStack)-3:]
	}
	toastMu.Unlock()
	c.Invalidate()  // repaint even when the caller was not in a build
	c.Announce(msg) // screen readers, as ui/toast.go:66 does
}

// Toasts renders the live toasts top-center and forgets the expired
// ones. Call it once per frame from the app view, last. Each toast fades
// in and out over 200ms like the engine toast (ui/toast.go:122-141).
func Toasts(c *ui.Context) {
	pal := P(c)
	now := time.Now()
	toastMu.Lock()
	live := toastStack[:0]
	for _, ts := range toastStack {
		if !ts.stamp {
			ts.at, ts.stamp = now, true
		}
		if now.Sub(ts.at) < toastLife {
			live = append(live, ts)
		}
	}
	toastStack = live
	if len(live) == 0 {
		toastMu.Unlock()
		return
	}
	ui.Overlay(c, func() {
		stack := ui.Column(c).Absolute().Left(0).Right(0).Top(toastTop).AlignItems(ui.Center).Gap(toastGap).PassThrough()
		stack.Children(func() {
			for i := range live {
				ts := &live[i]
				age, left := now.Sub(ts.at), toastLife-now.Sub(ts.at)
				w, _ := c.Size()
				box := ui.Row(c).Key(ts.id).AlignItems(ui.Center).Gap(toastGap).Padding(toastPadY, toastPadX).Radius(theme.RadiusXXL)
				box.Background(pal.Toast).Border(1, pal.Border).TextColor(pal.Foreground).FontSize(theme.FontBase)
				box.MaxWidth(w - 48) // window width minus margins, as toast.tsx caps at 100vw-1rem
				box.Shadow(0, 10, 20, 0, ui.RGBA(0, 0, 0, 0.15))
				box.Role(ui.RoleStatus).PassThrough()
				// The icon and the message must be built inside the box
				// (zcode-shell-specs.md §4.4: icon and text live in the
				// rounded-2xl bg-toast container) — without the Children
				// scope they would attach to the stack column instead and
				// leave the shell empty.
				box.Children(func() {
					if ts.kind == ToastWarning {
						Icon(c, IconAlert, 16).TextColor(pal.Warning)
					}
					ui.Text(c, ts.msg)
				})
				var opacity float32 = 1
				switch {
				case age < toastFade:
					opacity = float32(age) / float32(toastFade)
				case left < toastFade:
					opacity = float32(left) / float32(toastFade)
				}
				if opacity < 1 {
					box.Opacity(opacity)
					c.AnimationFrame()
				} else if left > toastFade {
					c.After(left - toastFade) // wake for the fade-out
				}
			}
		})
	})
	toastMu.Unlock()
}
