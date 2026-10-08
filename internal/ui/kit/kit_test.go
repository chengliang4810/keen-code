package kit

import (
	"image/color"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// Headless view tests for the A-stream controls (docs/go-migration.md
// §4.3): every control is driven in a ui.NewTester with no window — the
// default face, the hover/active face where the control has one, and the
// disabled face — asserted on state, on findable elements, and on pixels
// of the software-rendered frame (risk 11 of docs/go-migration.md). All
// faces run in the product-default zai-dark appearance.

// darkWindow is the zai-dark window face #161616 the assertions probe
// against. Derived composites met below:
//
//	primary/80 over #161616        → rgb(208,208,208)
//	Surface 5% white over #161616  → rgb(34,34,34)
//	Secondary #363636              → rgb(54,54,54)
//	Input #2b2b2b                  → rgb(43,43,43)
//	primary/30 over #161616        → rgb(92,92,92)
//	Border 10% white over #161616  → rgb(46,46,46)
//	native 50% disabled dim        → rgb(138,138,138) for a white face
var darkWindow = ui.Color{R: 22, G: 22, B: 22, A: 255}

// dark wraps a body into a view that bridges the palette each frame, like
// the app shell does, and pads the content.
func dark(body func(c *ui.Context)) func(c *ui.Context) {
	return func(c *ui.Context) {
		theme.Apply(c, P(c))
		ui.Column(c).Padding(8).Children(func() { body(c) })
	}
}

// newDark starts a tester on the body in the zai-dark appearance.
func newDark(t *testing.T, w, h int, body func(c *ui.Context)) *ui.Tester {
	t.Helper()
	tt := ui.NewTester(dark(body), w, h)
	tt.SetDark(true)
	return tt
}

// asUI converts a rendered pixel to a ui.Color; the structs are
// field-identical, so Go converts them directly.
func asUI(p color.RGBA) ui.Color { return ui.Color{R: p.R, G: p.G, B: p.B, A: p.A} }

// pxAt samples one pixel of the tester's last frame.
func pxAt(tt *ui.Tester, x, y int) ui.Color {
	return asUI(tt.Image().RGBAAt(x, y))
}

// eqc compares two colors channel by channel.
func eqc(a, b ui.Color) bool {
	return a.R == b.R && a.G == b.G && a.B == b.B && a.A == b.A
}

// mustFind fails the test when s is not on screen and returns its box.
func mustFind(t *testing.T, tt *ui.Tester, s string) ui.Rect {
	t.Helper()
	r, ok := tt.Find(s)
	if !ok {
		t.Fatalf("no element shows or is labeled %q", s)
	}
	return r
}

// anyInk reports whether any pixel of r differs from bg — proof that
// something visible was drawn inside.
func anyInk(tt *ui.Tester, r ui.Rect, bg ui.Color) bool {
	img := tt.Image()
	for y := int(r.Y); y < int(r.Y+r.H) && y < img.Bounds().Dy(); y++ {
		for x := int(r.X); x < int(r.X+r.W) && x < img.Bounds().Dx(); x++ {
			if !eqc(asUI(img.RGBAAt(x, y)), bg) {
				return true
			}
		}
	}
	return false
}

// TestButtonVariantsAndStates drives the button variants in zai-dark: the
// default face matches the variant table (button.tsx), hovering shows the
// variant's hover face, and clicks reach the handler that built them.
func TestButtonVariantsAndStates(t *testing.T) {
	secClicks := 0
	view := dark(func(c *ui.Context) {
		Button(c, VariantPrimary, SizeMD, "Save").Label("btn-primary")
		sec := Button(c, VariantSecondary, SizeMD, "Sec")
		sec.Label("btn-sec")
		if sec.Clicked() {
			secClicks++
		}
		Button(c, VariantGhost, SizeMD, "Ghost").Label("btn-ghost")
		Button(c, VariantOutline, SizeMD, "Outline").Label("btn-outline")
		Button(c, VariantDestructive, SizeMD, "Delete").Label("btn-des")
	})
	tt := ui.NewTester(view, 560, 400)
	tt.SetDark(true)

	r := mustFind(t, tt, "btn-primary")
	cy := int(r.Y + r.H/2)

	// Default: bg-primary #ffffff, probed in the padding zone away from
	// the label.
	if got := pxAt(tt, int(r.X)+3, cy); !eqc(got, ui.Color{R: 255, G: 255, B: 255, A: 255}) {
		t.Errorf("primary default face = %v, want #ffffff", got)
	}
	// Hover: bg-primary/80 over the window face → rgb(208,208,208).
	tt.Move(float32(int(r.X+r.W/2)), float32(cy))
	if got := pxAt(tt, int(r.X)+3, cy); !eqc(got, ui.Color{R: 208, G: 208, B: 208, A: 255}) {
		t.Errorf("primary hover face = %v, want rgb(208,208,208)", got)
	}

	// The ghost face is transparent until hovered (hover:bg-hover).
	ghost := mustFind(t, tt, "btn-ghost")
	gx, gy := int(ghost.X+ghost.W/2), int(ghost.Y+ghost.H/2)
	if got := pxAt(tt, int(ghost.X)+3, gy); !eqc(got, darkWindow) {
		t.Errorf("ghost default face = %v, want the window face #161616", got)
	}
	tt.Move(float32(gx), float32(gy))
	if got := pxAt(tt, int(ghost.X)+3, gy); !eqc(got, ui.Color{R: 34, G: 34, B: 34, A: 255}) {
		t.Errorf("ghost hover face = %v, want bg-hover over background rgb(34,34,34)", got)
	}

	// Secondary sits on bg-secondary; destructive on bg-destructive.
	sec := mustFind(t, tt, "btn-sec")
	if got := pxAt(tt, int(sec.X)+3, int(sec.Y+sec.H/2)); !eqc(got, ui.Color{R: 54, G: 54, B: 54, A: 255}) {
		t.Errorf("secondary default face = %v, want #363636", got)
	}
	des := mustFind(t, tt, "btn-des")
	if got := pxAt(tt, int(des.X)+3, int(des.Y+des.H/2)); !eqc(got, ui.Color{R: 255, G: 92, B: 92, A: 255}) {
		t.Errorf("destructive default face = %v, want #ff5c5c", got)
	}

	if err := tt.Click("Sec"); err != nil {
		t.Fatal(err)
	}
	if secClicks != 1 {
		t.Errorf("secondary clicks = %d, want 1", secClicks)
	}
}

// TestButtonDisabled checks the disabled state: mygo paints a disabled
// subtree at half opacity (ui/paint.go), the CSS disabled:opacity-50, and
// clicks are swallowed.
func TestButtonDisabled(t *testing.T) {
	clicks := 0
	view := dark(func(c *ui.Context) {
		b := Button(c, VariantPrimary, SizeMD, "Save")
		b.Label("btn").Disabled(true)
		if b.Clicked() {
			clicks++
		}
	})
	tt := newDark(t, 320, 200, view)
	r := mustFind(t, tt, "btn")
	cy := int(r.Y + r.H/2)
	// The white primary face dimmed to 50% over the window face
	// (255·0.5+22·0.5 rounds to 139).
	if got := pxAt(tt, int(r.X)+3, cy); !eqc(got, ui.Color{R: 139, G: 139, B: 139, A: 255}) {
		t.Errorf("disabled face = %v, want the 50%% dim rgb(139,139,139)", got)
	}
	if err := tt.Click("Save"); err == nil {
		t.Log("click on a disabled button's label is a no-op")
	}
	tt.ClickAt(r.X+r.W/2, r.Y+r.H/2)
	if clicks != 0 {
		t.Errorf("disabled button took a click, clicks = %d", clicks)
	}
}

// TestIconButton checks the icon button renders its glyph and clicks.
func TestIconButton(t *testing.T) {
	clicks := 0
	view := dark(func(c *ui.Context) {
		b := IconButton(c, VariantGhost, SizeIconMD, IconGear)
		b.Label("gear")
		if b.Clicked() {
			clicks++
		}
	})
	tt := newDark(t, 200, 120, view)
	r := mustFind(t, tt, "gear")
	if !anyInk(tt, r, darkWindow) {
		t.Error("icon button drew no glyph ink")
	}
	if err := tt.Click("gear"); err != nil {
		t.Fatal(err)
	}
	if clicks != 1 {
		t.Errorf("icon button clicks = %d, want 1", clicks)
	}
}

// TestSendButtonStateMachine drives the composer button: idle with a
// sendable draft it is the brand send square and clicking sends; while
// running it is the secondary stop square and clicking stops.
func TestSendButtonStateMachine(t *testing.T) {
	sent, stopped := 0, 0
	view := dark(func(c *ui.Context) {
		if SendButton(c, true, false).Clicked() {
			sent++
		}
		if SendButton(c, false, true).Clicked() {
			stopped++
		}
	})
	tt := newDark(t, 320, 220, view)

	send := mustFind(t, tt, "Send")
	if got := pxAt(tt, int(send.X)+2, int(send.Y+send.H/2)); !eqc(got, ui.Color{R: 255, G: 255, B: 255, A: 255}) {
		t.Errorf("send face = %v, want brand #ffffff", got)
	}
	if err := tt.Click("Send"); err != nil {
		t.Fatal(err)
	}
	if sent != 1 {
		t.Errorf("send clicks = %d, want 1", sent)
	}

	stop := mustFind(t, tt, "Stop")
	if got := pxAt(tt, int(stop.X)+2, int(stop.Y+stop.H/2)); !eqc(got, ui.Color{R: 54, G: 54, B: 54, A: 255}) {
		t.Errorf("stop face = %v, want secondary #363636", got)
	}
	if !anyInk(tt, stop, ui.Color{R: 54, G: 54, B: 54, A: 255}) {
		t.Error("stop button drew no glyph ink")
	}
	if err := tt.Click("Stop"); err != nil {
		t.Fatal(err)
	}
	if stopped != 1 {
		t.Errorf("stop clicks = %d, want 1", stopped)
	}
}

// TestSendButtonDisabled checks the cannot-send state: the native 50% dim
// of the brand face and no click delivery.
func TestSendButtonDisabled(t *testing.T) {
	sent := 0
	view := dark(func(c *ui.Context) {
		if SendButton(c, false, false).Clicked() {
			sent++
		}
	})
	tt := newDark(t, 200, 120, view)
	r := mustFind(t, tt, "Send")
	if got := pxAt(tt, int(r.X)+2, int(r.Y+r.H/2)); !eqc(got, ui.Color{R: 139, G: 139, B: 139, A: 255}) {
		t.Errorf("disabled send face = %v, want rgb(139,139,139)", got)
	}
	tt.ClickAt(r.X+r.W/2, r.Y+r.H/2)
	if sent != 0 {
		t.Errorf("disabled send took a click, sent = %d", sent)
	}
}

// TestTextField checks the single-line input: value binding, focus, and a
// disabled ancestor blocking input.
func TestTextField(t *testing.T) {
	name, other := "", ""
	view := dark(func(c *ui.Context) {
		TextField(c, &name, "Name")
		row := ui.Column(c)
		row.Children(func() { TextField(c, &other, "Locked") })
		row.Disabled(true)
	})
	tt := newDark(t, 360, 160, view)

	if err := tt.Click("Name"); err != nil {
		t.Fatal(err)
	}
	if !tt.Focused("Name") {
		t.Error("input is not focused after a click")
	}
	tt.Type("keen")
	if name != "keen" {
		t.Errorf("value = %q, want %q", name, "keen")
	}

	if err := tt.Click("Locked"); err == nil {
		t.Log("click on a disabled label is a no-op")
	}
	tt.Type("x")
	if other != "" {
		t.Errorf("disabled input took text: %q", other)
	}
	if tt.Focused("Locked") {
		t.Error("disabled input took focus")
	}
}

// editorHarness hosts one kit editor; the app contract on Submitted is to
// send and clear the draft.
type editorHarness struct {
	draft string
	sent  int
}

func (h *editorHarness) view(c *ui.Context) {
	theme.Apply(c, P(c))
	ui.Column(c).Padding(8).Children(func() {
		res := Editor(c, &h.draft, EditorOptions{Placeholder: "描述新任务"})
		if res.Submitted {
			h.sent++
			h.draft = ""
		}
	})
}

func newEditorTest(t *testing.T) (*editorHarness, *ui.Tester) {
	t.Helper()
	h := &editorHarness{}
	tt := ui.NewTester(h.view, 480, 240)
	tt.SetDark(true)
	return h, tt
}

// TestEditorSequences covers the documented sequences
// (docs/go-migration.md §4.3): ① plain Enter submits and the app clears
// the draft; ② Shift+Enter inserts a newline and does not submit; ③ an
// IME commit followed by Enter submits.
func TestEditorSequences(t *testing.T) {
	// ① Type + Enter → submitted, draft cleared.
	h, tt := newEditorTest(t)
	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Type("hello")
	if h.draft != "hello" {
		t.Fatalf("① draft = %q after typing, want hello", h.draft)
	}
	tt.Key(0, ui.KeyEnter)
	if h.sent != 1 || h.draft != "" {
		t.Errorf("① sent = %d, draft = %q; want submitted and cleared", h.sent, h.draft)
	}

	// ② Type + Shift+Enter → newline, no submit.
	h, tt = newEditorTest(t)
	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Type("hello")
	tt.Key(ui.Shift, ui.KeyEnter)
	if h.sent != 0 || h.draft != "hello\n" {
		t.Errorf("② sent = %d, draft = %q; want 0 and \"hello\\n\"", h.sent, h.draft)
	}

	// ③ Compose, commit the composition, then Enter → submitted.
	h, tt = newEditorTest(t)
	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Compose("ni", 2)
	tt.Type("你")
	if h.draft != "你" {
		t.Fatalf("③ draft = %q after committing the composition, want 你", h.draft)
	}
	tt.Key(0, ui.KeyEnter)
	if h.sent != 1 || h.draft != "" {
		t.Errorf("③ sent = %d, draft = %q; want submitted and cleared", h.sent, h.draft)
	}
}

// TestEditorNaivePredicateMisfires is the reverse regression ④: the naive
// "intercept Enter in HandleInput" predicate sends on the very Enter an
// input method uses to commit a composition — the bug the frame-time
// buffered diff avoids on the real platform (docs/go-migration.md §11
// B1). The assertion pins the misfire so the design is not reverted.
func TestEditorNaivePredicateMisfires(t *testing.T) {
	draft, sent := "", 0
	view := dark(func(c *ui.Context) {
		in := ui.TextAreaBase(c, &draft)
		in.Label("Draft")
		in.HandleInput(func(ev ui.InputEvent) bool {
			if ev.Kind == ui.InputKeyDown && ev.Key == ui.KeyEnter && ev.Mods&ui.Shift == 0 {
				sent++ // send right away: the naive design
				return true
			}
			return false
		})
	})
	tt := newDark(t, 480, 240, view)
	if err := tt.Click("Draft"); err != nil {
		t.Fatal(err)
	}
	tt.Compose("ni", 2) // a composition is open, nothing committed yet
	tt.Key(0, ui.KeyEnter)
	if sent != 1 {
		t.Errorf("naive predicate sent = %d, want 1 (the documented misfire)", sent)
	}
	if draft != "" {
		t.Errorf("naive predicate left draft %q, want the composition dropped", draft)
	}
}

// TestEditorDisabled checks the disabled editor takes neither focus nor
// text.
func TestEditorDisabled(t *testing.T) {
	draft := ""
	view := dark(func(c *ui.Context) {
		Editor(c, &draft, EditorOptions{Placeholder: "描述新任务", Disabled: true})
	})
	tt := newDark(t, 480, 240, view)
	if tt.Focused("Draft") {
		t.Error("disabled editor took focus")
	}
	if err := tt.Click("Draft"); err == nil {
		t.Log("click on a disabled label is a no-op")
	}
	tt.Type("nope")
	if draft != "" {
		t.Errorf("disabled editor took text: %q", draft)
	}
}

// TestCheckboxStates checks toggling, both faces, and the disabled state.
func TestCheckboxStates(t *testing.T) {
	on, locked := false, false
	view := dark(func(c *ui.Context) {
		Checkbox(c, &on, "同步").Label("cb")
		row := ui.Column(c)
		row.Children(func() { Checkbox(c, &locked, "锁定项").Label("cb-locked") })
		row.Disabled(true)
	})
	tt := newDark(t, 360, 180, view)
	r := mustFind(t, tt, "cb")
	// The 16px box sits at the row start; probe inside it, clear of the
	// rounded corners and of the 12px check glyph (inset by 2px).
	bx, by := int(r.X)+2, int(r.Y+r.H/2)
	if got := pxAt(tt, bx, by); !eqc(got, ui.Color{R: 43, G: 43, B: 43, A: 255}) {
		t.Errorf("unchecked box = %v, want bg-input #2b2b2b", got)
	}
	if err := tt.Click("同步"); err != nil {
		t.Fatal(err)
	}
	if !on {
		t.Error("checkbox did not toggle")
	}
	if got := pxAt(tt, bx, by); !eqc(got, ui.Color{R: 255, G: 255, B: 255, A: 255}) {
		t.Errorf("checked box = %v, want bg-primary #ffffff", got)
	}

	lr := mustFind(t, tt, "cb-locked")
	tt.ClickAt(lr.X+8, lr.Y+lr.H/2)
	if locked {
		t.Error("disabled checkbox toggled")
	}
}

// TestSwitchStates checks the track faces and toggling.
func TestSwitchStates(t *testing.T) {
	on := false
	view := dark(func(c *ui.Context) {
		Switch(c, &on).Label("sw")
	})
	tt := newDark(t, 200, 120, view)
	r := mustFind(t, tt, "sw")
	ry := int(r.Y + r.H/2)
	// Off: the right end of the track shows bg-primary/30 over the face.
	if got := pxAt(tt, int(r.X+r.W)-2, ry); !eqc(got, ui.Color{R: 92, G: 92, B: 92, A: 255}) {
		t.Errorf("off track = %v, want rgb(92,92,92)", got)
	}
	if err := tt.Click("sw"); err != nil {
		t.Fatal(err)
	}
	if !on {
		t.Error("switch did not toggle")
	}
	// The thumb animates for 140ms; let it land before probing the track.
	time.Sleep(160 * time.Millisecond)
	tt.Frame()
	// On: the left end of the track shows bg-primary (the thumb sits at
	// the right end).
	if got := pxAt(tt, int(r.X)+2, ry); !eqc(got, ui.Color{R: 255, G: 255, B: 255, A: 255}) {
		t.Errorf("on track = %v, want bg-primary #ffffff", got)
	}
}

// TestBadgeAndStatusDot checks the pill text and faces plus the dot.
func TestBadgeAndStatusDot(t *testing.T) {
	view := dark(func(c *ui.Context) {
		ui.Row(c).Gap(8).Children(func() {
			Badge(c, "Beta", BadgeNeutral)
			Badge(c, "等待输入", BadgeSuccess)
			StatusDot(c, P(c).Destructive)
		})
	})
	tt := newDark(t, 420, 140, view)
	if !tt.HasText("Beta") || !tt.HasText("等待输入") {
		t.Fatalf("badge texts missing: %q", tt.Texts())
	}
	// The pills pad px-2.5 around their label, so 6px left of the label
	// sits on the pill face. Neutral: bg-surface over the window face.
	beta := mustFind(t, tt, "Beta")
	if got := pxAt(tt, int(beta.X)-6, int(beta.Y+beta.H/2)); !eqc(got, ui.Color{R: 34, G: 34, B: 34, A: 255}) {
		t.Errorf("neutral pill face = %v, want rgb(34,34,34)", got)
	}
	// Success: success/14 over the window face with success text.
	notice, ok := tt.Find("等待输入")
	if !ok {
		t.Fatal("no notice pill")
	}
	// success/14: alphaByte(0.14)=36 → #46bf72 at 14% over #161616.
	want := ui.Color{R: 29, G: 46, B: 35, A: 255}
	if got := pxAt(tt, int(notice.X)-6, int(notice.Y+notice.H/2)); !eqc(got, want) {
		t.Errorf("success pill face = %v, want success/14 over background %v", got, want)
	}
}

// TestSpinner checks that the spinner draws ink left of a text label.
func TestSpinner(t *testing.T) {
	view := dark(func(c *ui.Context) {
		ui.Row(c).Gap(8).Children(func() {
			Spinner(c, 16)
			ui.Text(c, "加载中")
		})
	})
	tt := newDark(t, 320, 120, view)
	if !tt.HasText("加载中") {
		t.Error("label missing")
	}
	// The spinner turns at the row start, left of the text.
	tr, _ := tt.Find("加载中")
	region := ui.Rect{X: 8, Y: 8, W: tr.X - 8, H: tr.H}
	if !anyInk(tt, region, darkWindow) {
		t.Error("spinner drew no ink")
	}
}

// TestDivider checks the 1px separator line between two texts.
func TestDivider(t *testing.T) {
	view := dark(func(c *ui.Context) {
		ui.Column(c).Children(func() {
			ui.Text(c, "above")
			Divider(c)
			ui.Text(c, "below")
		})
	})
	tt := newDark(t, 320, 160, view)
	top, _ := tt.Find("above")
	bot, _ := tt.Find("below")
	if bot.Y-top.Y < 3 {
		t.Fatal("no room for the divider between the texts")
	}
	// The border pre-composited over the face is rgb(46,46,46).
	img := tt.Image()
	want := ui.Color{R: 46, G: 46, B: 46, A: 255}
	found := false
	for y := int(top.Y + top.H); y < int(bot.Y); y++ {
		if eqc(asUI(img.RGBAAt(int(top.X)+40, y)), want) {
			found = true
			break
		}
	}
	if !found {
		t.Error("no border-colored divider row between the texts")
	}
}

// TestIconRegistryRender builds every registered icon plus the spinner's
// loader and checks they draw ink; an unparseable SVG would have panicked
// at package init.
func TestIconRegistryRender(t *testing.T) {
	names := []IconName{
		IconArrowUp, IconArrowDown, IconSquare, IconBrain,
		IconChevronRight, IconChevronDown, IconCopy, IconCheck,
		IconPlus, IconGear, IconInfo, IconAlert, IconTerminal,
		IconFile, IconPencil, IconTrash, IconX,
	}
	view := dark(func(c *ui.Context) {
		ui.Column(c).Gap(2).Children(func() {
			for _, name := range names {
				n := name
				ui.Row(c).Gap(2).Children(func() {
					Icon(c, n, 16)
					ui.Text(c, string(n)).SingleLine()
				})
			}
			Spinner(c, 16)
		})
	})
	tt := newDark(t, 320, 560, view)
	if tt.Image() == nil {
		t.Fatal("no frame")
	}
	for _, name := range names {
		r, ok := tt.Find(string(name))
		if !ok {
			t.Errorf("icon row %q not found", name)
			continue
		}
		region := ui.Rect{X: r.X - 20, Y: r.Y, W: 20, H: r.H}
		if !anyInk(tt, region, darkWindow) {
			t.Errorf("icon %q drew no ink", name)
		}
	}
}

// TestDestructiveForegroundToken pins the destructive-foreground token the
// destructive button text needs: white in both zai faces (zcode-tokens.md
// §2; styles.css:708 keeps white-on-red even where ForegroundInverse is
// black).
func TestDestructiveForegroundToken(t *testing.T) {
	var pal *theme.Palette
	view := func(c *ui.Context) { pal = P(c) }
	tt := ui.NewTester(view, 40, 40)

	tt.SetDark(true)
	if !eqc(pal.DestructiveForeground, ui.Color{R: 255, G: 255, B: 255, A: 255}) {
		t.Errorf("dark DestructiveForeground = %v, want white", pal.DestructiveForeground)
	}
	tt.SetDark(false)
	if !eqc(pal.DestructiveForeground, ui.Color{R: 255, G: 255, B: 255, A: 255}) {
		t.Errorf("light DestructiveForeground = %v, want white", pal.DestructiveForeground)
	}
}
