package kit

import (
	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// This file is the shared contract of the kit (docs/go-migration.md §4.1).
// It is frozen in W0: the other implementers build against these types and
// signatures, and only the A stream (basic controls) changes this file.

// Variant is a control semantic variant, the ZCode button variants
// (zcode-shell-specs.md §4.1; packages/ui/src/components/ui/button.tsx).
type Variant string

const (
	VariantPrimary     Variant = "primary"     // bg-primary text-primary-foreground (black/white inverted in zai)
	VariantSecondary   Variant = "secondary"   // bg-secondary text-foreground
	VariantGhost       Variant = "ghost"       // no face; hover bg-hover
	VariantOutline     Variant = "outline"     // border + transparent face
	VariantDestructive Variant = "destructive" // bg-destructive white text
)

// Size is a control size tier. The DIP metrics of each tier follow the
// ZCode sizes the tier stands for (docs/go-migration.md §4.1):
// SizeSM is ZCode's size "lg" row button (h-8), SizeMD the default (h-7),
// SizeLG the dialog primary button (h-10, TaskRenameDialog.tsx:27-95),
// SizeIconSM the 24px square "icon-sm" and SizeIconMD the 28px square
// "icon-md" of the composer send/stop buttons.
type Size string

const (
	SizeSM     Size = "sm"      // h-8 rounded-lg px-2.5
	SizeMD     Size = "md"      // h-7 rounded-md px-2 (the default)
	SizeLG     Size = "lg"      // h-10 px-5 (dialog primary buttons)
	SizeIconSM Size = "icon-sm" // 24px square
	SizeIconMD Size = "icon-md" // 28px square (composer send/stop)
)

// IconName names one icon of the embedded SVG registry (icon.go). Declare
// a new icon here before implementing it; the other kit streams must ask
// the A stream instead of editing this file.
type IconName string

const (
	IconArrowUp      IconName = "arrow-up"       // send
	IconArrowDown    IconName = "arrow-down"     // scroll to bottom
	IconSquare       IconName = "square"         // stop
	IconBrain        IconName = "brain"          // reasoning
	IconChevronRight IconName = "chevron-right"  // collapsed disclosure
	IconChevronDown  IconName = "chevron-down"   // expanded disclosure
	IconCopy         IconName = "copy"           // copy
	IconCheck        IconName = "check"          // copied / done
	IconPlus         IconName = "plus"           // new session
	IconGear         IconName = "gear"           // settings (lucide "settings")
	IconInfo         IconName = "info"           // error banner
	IconAlert        IconName = "triangle-alert" // warning toast
	IconTerminal     IconName = "terminal"       // bash tool card
	IconFile         IconName = "file"
	IconFolder       IconName = "folder"
	IconFolderOpen   IconName = "folder-open"
	IconPin          IconName = "pin"
	IconPanelLeft    IconName = "panel-left"
	IconPanelRight   IconName = "panel-right"
	IconFileDiff     IconName = "file-diff"
	IconRefresh      IconName = "refresh-cw"
	IconPencil       IconName = "pencil" // rename
	IconTrash        IconName = "trash"  // delete
	IconX            IconName = "x"      // close
)

// P returns the current frame's ZCode palette. It is the kit's only color
// source: views read P each frame and never touch ui.Context.Theme fields
// or hex literals (docs/go-migration.md §3.1 rule 8).
func P(c *ui.Context) *theme.Palette { return theme.Active(c) }

// PromptOptions is the contract of the rename/input dialog. The type is
// frozen here; B implements it in dialog.go.
type PromptOptions struct {
	Title, Description        string
	Placeholder, Initial      string
	ConfirmLabel, CancelLabel string
}
