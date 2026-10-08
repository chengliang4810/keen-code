package kit

import (
	"strings"

	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// EditorOptions carries the composer editor's knobs
// (docs/go-migration.md §4.2).
type EditorOptions struct {
	// Placeholder shows while the draft is empty. It is not a Text
	// element, so tests cannot find it; the input carries the fixed label
	// "Draft" for test targeting instead (mygo-ui-notes.md §9.5).
	Placeholder string
	// Disabled blocks editing and focus.
	Disabled bool
	// Toolbar builds the composer toolbar inside the shell, under the
	// editor (zcode-chat-specs.md §2.1: the shell is editor → toolbar,
	// leading actions left and the send/stop button right — never outside
	// the shell). Nil renders the editor alone. It runs during the frame
	// that builds the shell and must not be stored.
	Toolbar func()
}

// EditorResult is the frame's outcome. Submitted means a plain Enter
// reached the editor and the buffer grew by exactly that newline: the app
// sends the draft and clears it. No callback exists — the single outcome
// is judged after the frame, in the mygo way.
type EditorResult struct {
	Submitted bool
}

// editorEnter is the per-editor state of the frame-time buffered diff that
// implements Enter-to-send (docs/go-migration.md §4.2). It lives in the
// element's state via ui.Local, one per editor instance.
type editorEnter struct {
	seen     bool   // a plain Enter arrived since the last judgment
	snapshot string // *draft just before it
}

// Editor is the multiline composer shell (ChatPromptEditor.tsx:346-351):
// rounded-2xl, border-input-border, bg-input, p-3; hovering brightens the
// border, focusing switches to input-border-focused over
// bg-input-focused — in zai the focused border is border-hover, not brand
// (zcode-tokens.md §2). The core is ui.TextAreaBase, the look-free base —
// never ui.TextArea, whose built-in face would double shell and border
// (docs/go-migration.md §4.2).
//
// Enter-to-send works by frame-time buffered diff: HandleInput records a
// plain Enter with a snapshot of *draft and lets it through, the built-in
// editor applies its queue while the element is built, and right after
// building, the buffer is compared against the snapshot — grown by exactly
// the one newline, the Enter meant "send"; anything else (an IME commit
// rode along) keeps the text and strips the newline the key left. The
// headless-verified sequences and the platform boundary of this design are
// docs/go-migration.md §4.3 and 差异 11.
func Editor(c *ui.Context, draft *string, opts EditorOptions) EditorResult {
	pal := P(c)
	res := EditorResult{}
	var st *editorEnter
	focused, hovered := false, false

	shell := ui.Column(c).Label("Composer")
	shell.Radius(theme.RadiusXXL).Padding(12) // rounded-2xl p-3
	shell.Gap(theme.SpaceUnit * 3)            // gap-3 between editor and toolbar (§2.1)
	shell.Children(func() {
		input := ui.TextAreaBase(c, draft)
		// LexicalChatInput: min-h-10 max-h-40, leading-5. The native
		// area keeps its own scrollbar when the text exceeds the cap.
		// Use the equivalent multiplier: mygo's placeholder layout does not
		// carry FixedLineHeight and would multiply 20 by the font size.
		input.MinHeight(theme.SpaceUnit * 10).MaxHeight(theme.SpaceUnit * 40).FontSize(theme.FontBase).LineHeight(theme.LineHeightEditor / theme.FontBase).FillWidth()
		input.Placeholder(opts.Placeholder)
		input.Label("Draft")
		if opts.Disabled {
			input.Disabled(true)
		}
		focused, hovered = input.Focused(), input.Hovered()
		st = ui.Local(input, "editorEnter", func() editorEnter { return editorEnter{} })
		input.HandleInput(func(ev ui.InputEvent) bool {
			if ev.Kind != ui.InputKeyDown || ev.Key != ui.KeyEnter {
				return false
			}
			if ev.Mods&ui.Shift != 0 { // Shift+Enter is a newline
				return false
			}
			st.seen = true
			st.snapshot = *draft
			return false // the built-in editor applies the key
		})
		if opts.Toolbar != nil {
			toolbar := ui.Row(c).FillWidth().Shrink(0)
			toolbar.Children(opts.Toolbar)
		}
	})

	// The shell mirrors the input (focus-within): hover brightens the
	// border, focus switches to the focused border over the focused face.
	border := pal.Border // border-input-border = border
	bg := pal.Input
	if focused {
		border = pal.InputBorderFocused
		bg = pal.InputFocused
	} else if hovered {
		border = pal.InputBorderHover
	}
	shell.Background(bg).Border(1, border)

	// The editor queue has been applied while TextAreaBase was built, so
	// *draft is already the post-Enter value here.
	if st != nil && st.seen {
		st.seen = false
		if insertedEnter(st.snapshot, *draft) {
			*draft = st.snapshot // submit the original text at any caret position
			res.Submitted = true // the app sends and clears the draft
		} else {
			// An IME commit rode on the key: keep the committed text,
			// drop the newline it was followed by (差异 11).
			*draft = strings.TrimSuffix(*draft, "\n")
		}
	}
	return res
}

// insertedEnter accepts exactly one inserted newline, including at the
// beginning, middle, or end. Other edits in the same batch are an IME
// commitment and do not submit the draft.
func insertedEnter(before, after string) bool {
	if len(after) != len(before)+1 {
		return false
	}
	i := 0
	for i < len(before) && before[i] == after[i] {
		i++
	}
	return after[i] == '\n' && after[i+1:] == before[i:]
}
