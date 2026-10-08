package kit

import (
	"sync"

	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// Modal dialogs (docs/go-migration.md §4.2 B-stream dialog.go), on top of
// ui.DialogBase (ui/base.go:394-413), which owns the backdrop, the Escape
// key and the outside click. The shell follows zcode-shell-specs.md §4.1:
// mask black/60, panel rounded-2xl border bg-popover with the title at
// text-ui-base font-medium and the footer 取消 left / 确认 right
// (components/ui/dialog.tsx:58-140); §4.2 gives the compact confirm
// variant and §4.3 the rename prompt. The mask keeps its alpha per
// docs/go-migration.md §3.1 rule 4; ZCode's backdrop-blur has no native
// equivalent and is a recorded intentional difference (差异 2).

// DialogResult reports what closed a Dialog, Confirm or Prompt this
// frame. While the dialog stays open the constructors return DialogNone.
type DialogResult int

const (
	DialogNone    DialogResult = iota - 1 // still open this frame
	DialogCancel                          // the cancel button, Escape or the backdrop
	DialogConfirm                         // the confirm button
)

// DialogOptions describes the standard dialog. Empty labels fall back to
// 确认/取消 (copy is inline Chinese for v1; i18n extraction is deferred,
// docs/go-migration.md §7).
type DialogOptions struct {
	Title        string
	Description  string
	Body         func() // extra content between the description and the footer
	ConfirmLabel string
	CancelLabel  string
	// Destructive paints the confirm button in the destructive variant
	// (bg-destructive, white text).
	Destructive bool

	// submit is an extra confirm trigger, checked after Body builds (the
	// prompt's input field reports Submitted there). Kit-internal: Prompt
	// sets it; call sites cannot.
	submit func() bool
}

// Dialog shows a modal dialog while *open is true and reports what closed
// it. Escape, the backdrop and the cancel button yield DialogCancel and
// close it; the confirm button or opts.submit yields DialogConfirm. The
// confirm button takes the focus, so Enter confirms, matching
// ui.AlertDialog's key behavior (ui/feedback.go:160-210).
func Dialog(c *ui.Context, open *bool, opts DialogOptions) DialogResult {
	return runDialog(c, open, dialogChrome{
		pad:         16, // p-4
		gap:         16, // gap-4
		maxW:        0,  // max-w-[calc(100%-2rem)]
		titleSize:   theme.FontBase,
		titleWeight: theme.WeightMedium,
		cancel: func(c *ui.Context) *ui.Element {
			return dlgButton(c, VariantSecondary, orLabel(opts.CancelLabel, "取消"), "", 40, 0)
		},
		confirm: func(c *ui.Context) *ui.Element {
			v := VariantPrimary
			if opts.Destructive {
				v = VariantDestructive
			}
			return dlgButton(c, v, orLabel(opts.ConfirmLabel, "确认"), "", 40, 0)
		},
		focusConfirm: true,
	}, opts.Title, opts.Description, opts.Body, nil)
}

// ConfirmOptions describes the compact confirm dialog of
// zcode-shell-specs.md §4.2 (ConfirmDialog.tsx): p-5 gap-5 shell at most
// 400 DIP wide, a 16px semibold title, and footer buttons h-9 that carry
// their key hint (esc / ⏎) inside.
type ConfirmOptions struct {
	Title        string
	Description  string
	ConfirmLabel string
	CancelLabel  string
	// Destructive paints the confirm button destructive (session delete,
	// quit with unsent drafts).
	Destructive bool
}

// Confirm is the compact confirm variant of Dialog.
func Confirm(c *ui.Context, open *bool, opts ConfirmOptions) DialogResult {
	return runDialog(c, open, dialogChrome{
		pad:         20, // p-5
		gap:         20, // gap-5
		maxW:        400,
		titleSize:   theme.FontLG,
		titleWeight: theme.WeightSemibold,
		cancel: func(c *ui.Context) *ui.Element {
			return dlgButton(c, VariantSecondary, orLabel(opts.CancelLabel, "取消"), "esc", 36, 112) // min-w-28
		},
		confirm: func(c *ui.Context) *ui.Element {
			v := VariantPrimary
			if opts.Destructive {
				v = VariantDestructive
			}
			return dlgButton(c, v, orLabel(opts.ConfirmLabel, "确认"), "⏎", 36, 128) // min-w-32
		},
		focusConfirm: true,
	}, opts.Title, opts.Description, nil, nil)
}

// Prompt is the rename/text-input dialog (PromptOptions is frozen in
// kit.go). The lg input (h-8 rounded-lg px-3, zcode-shell-specs.md §3)
// takes the focus when the dialog opens; Enter confirms and Escape
// cancels. Composition Enter behavior follows the mygo built-in
// single-line editor (the commit insert lands in the editor queue before
// the key, internal/darwin/surface_input.go:126-135); the residual
// real-machine IME nuance is covered by W4 native acceptance
// (docs/go-migration.md risk 4). It returns the text as typed and the
// result. One Prompt runs at a time; the text buffer is kit state keyed
// to the open flag and resets to Initial on every open.
func Prompt(c *ui.Context, open *bool, opts PromptOptions) (string, DialogResult) {
	buf := promptBuffer(open, opts.Initial)
	confirm := false
	res := runDialog(c, open, dialogChrome{
		pad:         24, // p-6
		gap:         24, // gap-6
		maxW:        576,
		titleSize:   theme.FontBase,
		titleWeight: theme.WeightMedium,
		cancel: func(c *ui.Context) *ui.Element {
			return dlgButton(c, VariantSecondary, orLabel(opts.CancelLabel, "取消"), "", 40, 0)
		},
		confirm: func(c *ui.Context) *ui.Element {
			return dlgButton(c, VariantPrimary, orLabel(opts.ConfirmLabel, "确认"), "", 40, 0)
		},
	}, opts.Title, opts.Description, func() {
		field := promptField(c, buf, opts.Placeholder)
		field.AutoFocus()
		confirm = field.Submitted()
	}, func() bool { return confirm })
	return *buf, res
}

// dialogChrome is the shell spec of one dialog variant, per
// zcode-shell-specs.md §4.1-§4.3.
type dialogChrome struct {
	pad, gap, maxW  float32 // maxW 0 means max-w-[calc(100%-2rem)]
	titleSize       float32
	titleWeight     int
	cancel, confirm func(c *ui.Context) *ui.Element
	// focusConfirm gives the confirm button the AutoFocus so Enter
	// confirms. Prompt clears it: there the input field takes the focus
	// (TaskRenameDialog semantics).
	focusConfirm bool
}

// runDialog is the shared dialog core: the black/60 mask, the
// rounded-2xl bg-popover panel, title, description, optional body, then
// the footer row with cancel left and the focused confirm right.
func runDialog(c *ui.Context, open *bool, chrome dialogChrome, title, desc string, body func(), submit func() bool) DialogResult {
	pal := P(c)
	if !*open {
		return DialogNone
	}
	chosen := DialogNone
	ui.DialogBase(c, open, func(back, panel *ui.Element) {
		back.Background(ui.RGBA(0, 0, 0, 0.6)) // bg-black/60, no backdrop-blur (差异 2)
		panel.Padding(chrome.pad).Gap(chrome.gap).Radius(theme.RadiusXXL)
		panel.Background(pal.Popover).Border(1, pal.Border).TextColor(pal.Foreground)
		w, h := c.Size()
		if chrome.maxW > 0 && chrome.maxW < w-32 {
			panel.MaxWidth(chrome.maxW)
		} else {
			panel.MaxWidth(w - 32)
		}
		panel.MaxHeight(h - 32)
		panel.Shadow(0, 6, 16, 0, shadowMedium)
		ui.Text(c, title).FontSize(chrome.titleSize).FontWeight(chrome.titleWeight).SingleLine()
		if desc != "" {
			ui.Text(c, desc).TextColor(pal.ForegroundSubtle)
		}
		if body != nil {
			body()
		}
		if submit != nil && submit() && chosen == DialogNone {
			chosen = DialogConfirm
		}
		footer := ui.Row(c).Gap(8).Justify(ui.End)
		footer.Children(func() {
			if chrome.cancel(c).Clicked() {
				chosen = DialogCancel
			}
			confirm := chrome.confirm(c)
			if chrome.focusConfirm {
				confirm.AutoFocus()
			}
			if confirm.Clicked() {
				chosen = DialogConfirm
			}
		})
	})
	if chosen == DialogNone && !*open {
		chosen = DialogCancel // Escape or a backdrop click this frame
	}
	if chosen != DialogNone {
		*open = false
	}
	return chosen
}

// dlgButton is a dialog footer button: one Variant face at a fixed
// height, with an optional embedded key hint (the esc / ⏎ chips of
// ConfirmDialog.tsx:150-190). It is local to the dialog stream because
// the hint chips need children kit.Button does not take; faces follow the
// same palette rules as button.go (hover at the variant's CSS alpha,
// pre-composited over the panel the button sits on).
func dlgButton(c *ui.Context, v Variant, label, hint string, height, minW float32) *ui.Element {
	pal := P(c)
	px := float32(16) // h-9 (confirm footer) is px-4
	if height >= 40 {
		px = 20 // h-10 (dialog/prompt footer) is px-5, TaskRenameDialog.tsx:27-95
	}
	b := ui.ButtonBase(c).Height(height).Radius(theme.RadiusLG).Gap(8).PaddingX(px)
	if minW > 0 {
		b.MinWidth(minW)
	}
	fg := pal.Foreground
	var bg, hover, border ui.Color
	hintColor := pal.ForegroundSubtle
	switch v {
	case VariantPrimary:
		bg, fg = pal.Primary, pal.PrimaryForeground
		hover = pal.Primary.Alpha(0.8).Over(pal.Popover) // hover:bg-primary/80
		hintColor = fg.Alpha(0.55)
	case VariantDestructive:
		bg, fg = pal.Destructive, pal.DestructiveForeground
		hover = pal.Destructive.Alpha(0.9).Over(pal.Popover) // hover:bg-destructive/90
		hintColor = fg.Alpha(0.55)
	case VariantOutline:
		border = pal.Border
		hover = pal.Input.Alpha(0.5).Over(pal.Popover) // hover:bg-input/50
	default: // VariantSecondary
		bg = pal.Secondary
		hover = pal.Secondary.Alpha(0.8).Over(pal.Popover) // hover:bg-secondary/80
	}
	if b.IsDisabled() {
		bg, fg, border = bg.Alpha(0.5), fg.Alpha(0.5), border.Alpha(0.5)
	} else if b.Hovered() {
		bg = hover
	}
	if bg.A > 0 {
		b.Background(bg)
	}
	b.TextColor(fg)
	if border.A > 0 {
		b.Border(1, border)
	}
	b.Children(func() {
		ui.Text(c, label).SingleLine()
		if hint != "" {
			// The embedded key hint is font-mono text-ui-base
			// (ConfirmDialog.tsx:150-190).
			ui.Text(c, hint).Font("monospace").FontSize(theme.FontBase).TextColor(hintColor).SingleLine()
		}
	})
	return b
}

// promptField is the lg input of the prompt (h-8 rounded-lg px-3,
// zcode-shell-specs.md §3) around ui.TextInputBase: input background that
// hovers to input-border-hover and focuses to input-border-focused with
// bg-input-focused.
func promptField(c *ui.Context, value *string, placeholder string) *ui.Element {
	pal := P(c)
	row := ui.Row(c).Height(32).Radius(theme.RadiusLG).Padding(0, 12)
	var in *ui.Element
	row.Children(func() {
		in = ui.TextInputBase(c, value).Grow(1).Placeholder(placeholder)
	})
	bg, border := pal.Input, pal.Border
	switch {
	case in.Focused():
		bg, border = pal.InputFocused, pal.InputBorderFocused
	case row.Hovered():
		border = pal.InputBorderHover
	}
	row.Background(bg).Border(1, border)
	row.TextColor(pal.Foreground).FontSize(theme.FontBase)
	return in
}

// promptBuffer returns the text buffer of the prompt, kit state keyed to
// the open flag so it survives frames and resets to initial on every
// open. One prompt at a time.
var (
	promptMu      sync.Mutex
	promptFor     *bool
	promptValue   string
	promptWasOpen bool
)

func promptBuffer(open *bool, initial string) *string {
	promptMu.Lock()
	defer promptMu.Unlock()
	if promptFor != open {
		promptFor, promptWasOpen = open, false
	}
	if *open && !promptWasOpen {
		promptValue = initial
	}
	promptWasOpen = *open
	return &promptValue
}

// orLabel returns def for an empty label.
func orLabel(s, def string) string {
	if s == "" {
		return def
	}
	return s
}
