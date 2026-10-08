package theme

import "time"

// Non-color ZCode design tokens as Go constants: typography, radius,
// spacing, message column widths, and motion durations. Values are copied
// verbatim from docs/go-migration/zcode-tokens.md §5-§9 and the mapping
// rules in docs/go-migration.md §3.2. All sizes are DIPs (mygo's unit),
// matching the px values of the 4px CSS spacing grid at the default rem
// base. Kit and app code must use these constants instead of raw px or
// type literals (docs/go-migration.md §3.1 rule 8).

// Font sizes, the text-ui-* scale at the default --ui-font-size base of
// 14px (zcode-tokens.md §7.1; mapping rules docs/go-migration.md §3.2).
const (
	FontXL       float32 = 18 // text-ui-xl: markdown h1
	FontLG       float32 = 16 // text-ui-lg: markdown h2
	FontBase     float32 = 14 // text-ui-base: body, buttons, h3-h6
	FontCaption  float32 = 13 // text-ui-caption: compact secondary notes
	FontSM       float32 = 12 // text-ui-sm: tooltips, inline code, code blocks
	FontXS       float32 = 10 // text-ui-xs: shortcuts, badges, faint metadata
	FontGreeting float32 = 30 // text-3xl draft greeting upper bound
)

// LineHeightBody is the assistant body line height (1.75, from
// message.tsx leading-[1.75]; docs/go-migration.md §3.2). Tailwind's
// relaxed 1.625 is deliberately not used.
const LineHeightBody float32 = 1.75

// LineHeightEditor is LexicalChatInput's leading-5 in DIPs.
const LineHeightEditor float32 = 20

// Font weights (zcode-tokens.md §7.3: Tailwind font-normal/medium/
// semibold/bold).
const (
	WeightNormal   = 400
	WeightMedium   = 500
	WeightSemibold = 600
	WeightBold     = 700
)

// Corner radii in DIP, the Tailwind rounded-* ladder
// (zcode-tokens.md §5). Nesting descends xl > lg > md > sm; RadiusFull is
// reserved for pills and circles.
const (
	RadiusXS           float32 = 2    // rounded-xs
	RadiusSM           float32 = 4    // rounded-sm
	RadiusMD           float32 = 6    // rounded-md: menu items, inline code
	RadiusLG           float32 = 8    // rounded-lg: buttons and inputs
	RadiusXL           float32 = 12   // rounded-xl: first-level cards and panels
	RadiusXXL          float32 = 16   // rounded-2xl: composer shell, dialogs, toasts
	RadiusFull         float32 = 9999 // rounded-full: pills and circles only
	RadiusPanelWindows float32 = 5    // workspaceShellWindowChrome Windows panel
)

// Spacing rhythm in DIP on the 4px grid (zcode-tokens.md §6 and the
// per-turn rhythm in docs/go-migration.md §3.2).
const (
	SpaceUnit       float32 = 4  // --spacing: the base unit
	SpaceTurnTop    float32 = 56 // between-turn top padding (pt-14)
	SpaceTurnBottom float32 = 20 // between-turn bottom padding (pb-5)
	SpaceInTurn     float32 = 20 // gap inside a turn (gap-5)
	SpaceWorkItem   float32 = 16 // gap between work items (gap-4)
)

// Message column widths in DIP, the v1 approximation of the
// conversationLayout container queries at 864/1280
// (docs/go-migration.md §3.2, zcode-chat-specs §1.1).
const (
	ColumnDraft       float32 = 672  // draft (new session) column
	ColumnSession     float32 = 896  // session column
	ColumnSessionWide float32 = 1152 // session column when width >= 1280
)

// ScrollbarWidth is the global scrollbar thickness in DIP
// (zcode-tokens.md §3: 14px, rounded-full thumb). It is bridged into
// ui.Theme.ScrollbarWidth by Apply.
const ScrollbarWidth float32 = 14

// Motion durations from zcode-tokens.md §9 (source literals from ZCode
// styles.css). The first version switches collapses instantly and skips
// entrance animations (docs/go-migration.md §7 deferredScope), but the
// spec values are kept here so later waves reuse exact durations instead
// of inventing new ones.
const (
	AnimCollapse     = 300 * time.Millisecond // collapse/expand (§9: 300ms ease-in-out)
	AnimEnterExit    = 150 * time.Millisecond // component enter/exit (tw-animate-css default)
	AnimStreamTextIn = 900 * time.Millisecond // streamed text entrance (§9)
	AnimDraftCascade = 260 * time.Millisecond // draft cascade entrance (§9)
	AnimGradientFlow = 4 * time.Second        // gradient shimmer loop (§9)
	AnimWidgetFast   = 120 * time.Millisecond // workflow speed: fast (§9)
	AnimWidgetBase   = 160 * time.Millisecond // workflow speed: base (§9)
	AnimWidgetEnter  = 200 * time.Millisecond // workflow speed: enter (§9)
	AnimWidgetInk    = 320 * time.Millisecond // workflow speed: ink (§9)
)
