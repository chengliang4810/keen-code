package kit

import (
	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// Buttons replicate the ZCode button (packages/ui/src/components/ui/
// button.tsx, zcode-shell-specs.md §4.1): rounded corners, a transparent
// 1px border, text-ui-base, whitespace-nowrap, disabled at 50% opacity,
// painted per variant, with the sizes mapped in docs/go-migration.md §4.1.
// Every color comes from the palette; ask .Clicked() on the element the
// constructors return.

// stopSquare is the stop glyph: the lucide square with its fill turned on,
// as ZCode's SquareIcon fill-current (ConversationComposer.tsx:2070-2073).
// The derivation is noted in svg/square-fill.svg's license header.
var stopSquare = mustIcon("square-fill.svg")

// sizeSpec is the metric table of one Size tier, read from the ZCode size
// classes: SizeSM is ZCode "lg" (h-8 rounded-lg px-2.5), SizeMD the
// default (h-7 rounded-md px-2), SizeLG the dialog primary button
// (h-10 px-5, TaskRenameDialog.tsx:27-95), SizeIconSM "icon-sm" (size-6,
// 12px icon), SizeIconMD "icon-md" (size-7 rounded-lg, 16px icon).
type sizeSpec struct {
	height float32
	px     float32 // horizontal padding of text buttons
	radius float32
	icon   float32 // icon glyph size for icon buttons
	square bool    // icon buttons: width equals height, no padding
}

func sizeSpecOf(s Size) sizeSpec {
	switch s {
	case SizeSM: // ZCode button size "lg"
		return sizeSpec{height: 32, px: 10, radius: theme.RadiusLG, icon: 16}
	case SizeLG: // dialog primary buttons, base rounded-md
		return sizeSpec{height: 40, px: 20, radius: theme.RadiusMD, icon: 16}
	case SizeIconSM:
		return sizeSpec{height: 24, radius: theme.RadiusMD, icon: 12, square: true}
	case SizeIconMD:
		return sizeSpec{height: 28, radius: theme.RadiusLG, icon: 16, square: true}
	default: // SizeMD, the ZCode default tier
		return sizeSpec{height: 28, px: 8, radius: theme.RadiusMD, icon: 14}
	}
}

// variantFace is the paint of one Variant, copied from button.tsx:
// primary "bg-primary text-primary-foreground hover:bg-primary/80",
// secondary "bg-secondary text-foreground hover:bg-secondary/80", ghost
// "text-foreground hover:bg-hover", outline "border-border text-foreground
// hover:border-border-hover hover:bg-input/50", destructive
// "bg-destructive text-destructive-foreground hover:bg-destructive/90".
// The partial-alpha hover faces are pre-composited over the palette
// background, as buttons sit on it rather than stacking on other surfaces
// (docs/go-migration.md §3.1 rule 4).
type variantFace struct {
	bg, fg      ui.Color
	border      ui.Color
	hoverBG     ui.Color
	hoverBorder ui.Color
}

func faceOf(pal *theme.Palette, v Variant) variantFace {
	switch v {
	case VariantSecondary:
		return variantFace{bg: pal.Secondary, fg: pal.Foreground,
			hoverBG: pal.Secondary.Alpha(0.8).Over(pal.Background)}
	case VariantGhost:
		return variantFace{fg: pal.Foreground, hoverBG: pal.Hover.Over(pal.Background)}
	case VariantOutline:
		return variantFace{fg: pal.Foreground, border: pal.Border,
			hoverBG: pal.Input.Alpha(0.5).Over(pal.Background), hoverBorder: pal.BorderHover}
	case VariantDestructive:
		return variantFace{bg: pal.Destructive, fg: pal.DestructiveForeground,
			hoverBG: pal.Destructive.Alpha(0.9).Over(pal.Background)}
	default: // VariantPrimary
		return variantFace{bg: pal.Primary, fg: pal.PrimaryForeground,
			hoverBG: pal.Primary.Alpha(0.8).Over(pal.Background)}
	}
}

// Button creates a labeled button of variant v and size s. Handle
// Clicked() where it is built; disable it by chaining .Disabled on the
// returned element (or on an ancestor).
func Button(c *ui.Context, v Variant, s Size, label string) *ui.Element {
	pal := P(c)
	spec := sizeSpecOf(s)
	b := ui.ButtonBase(c)
	paintButton(b, pal, v, spec)
	if label != "" {
		b.Children(func() { ui.Text(c, label).SingleLine() })
	}
	return b
}

// IconButton creates a button showing one icon of the registry. Handle
// Clicked() where it is built; disable it by chaining .Disabled.
func IconButton(c *ui.Context, v Variant, s Size, name IconName) *ui.Element {
	pal := P(c)
	spec := sizeSpecOf(s)
	b := ui.ButtonBase(c)
	paintButton(b, pal, v, spec)
	b.Children(func() { Icon(c, name, spec.icon) })
	return b
}

// paintButton sizes and paints a button base per the variant table. The
// hover face is read at build time; hover-flagged elements get a frame on
// every pointer move across them (mygo ui/input.go setHover), so it
// follows the pointer. Disabling is mygo-native: the engine paints a
// disabled subtree at half opacity (ui/paint.go multiplies p.opacity by
// 0.5), which is exactly the base class's disabled:opacity-50 and covers
// both ancestor and chained .Disabled without extra paint here.
func paintButton(b *ui.Element, pal *theme.Palette, v Variant, spec sizeSpec) {
	f := faceOf(pal, v)
	b.Height(spec.height).Radius(spec.radius).Gap(theme.SpaceUnit) // gap-1
	if spec.square {
		b.Width(spec.height)
	} else {
		b.PaddingX(spec.px)
	}
	bg, fg, border := f.bg, f.fg, f.border
	if !b.IsDisabled() && b.Hovered() {
		if f.hoverBG.A > 0 {
			bg = f.hoverBG
		}
		if f.hoverBorder.A > 0 {
			border = f.hoverBorder
		}
	}
	if bg.A > 0 {
		b.Background(bg)
	}
	b.TextColor(fg)
	if border.A > 0 {
		b.Border(1, border)
	}
}

// SendButton is the composer send/stop state machine
// (docs/go-migration.md §4.2; it diverges from ZCode on purpose, see
// 差异 8: v1 has no queue, so while a turn runs the button is always
// Stop). Not running: a rounded-lg brand square with an up arrow,
// disabled with a native string tooltip while the draft cannot be sent
// (ConversationComposer.tsx:2085-2096). Running: a secondary square with
// the filled square glyph; the Esc shortcut belongs to app/chat.go.
func SendButton(c *ui.Context, canSend bool, running bool) *ui.Element {
	pal := P(c)
	b := ui.ButtonBase(c)
	b.Size(28, 28).Radius(theme.RadiusLG) // icon-md
	if running {
		bg := pal.Secondary
		if b.Hovered() {
			bg = pal.Secondary.Alpha(0.8).Over(pal.Background)
		}
		b.Background(bg).TextColor(pal.Foreground)
		b.Label("Stop")
		b.Children(func() { ui.Icon(c, stopSquare).FontSize(16).Shrink(0) })
		return b
	}
	if !canSend {
		// Disabled at the native 50% paint opacity; the native string
		// tooltip explains why (docs/go-migration.md §4.2, A stream uses
		// no self-drawn tooltip).
		b.Disabled(true)
		b.Tooltip("输入内容后按 Enter 发送")
	}
	bg, fg := pal.Brand, pal.ForegroundInverse
	if canSend && b.Hovered() {
		bg = pal.Brand.Alpha(0.8).Over(pal.Background)
	}
	b.Background(bg).TextColor(fg)
	b.Label("Send")
	b.Children(func() { Icon(c, IconArrowUp, 16) })
	return b
}
