package theme

import "github.com/egoist/mygo/ui"

// Palette holds the ZCode design tokens for one appearance (zai-light or
// zai-dark), converted to mygo ui.Color values. It is the single color
// source for the kit and the app: views read it each frame through Active,
// and must not hard-code hex literals (docs/go-migration.md §3.1 rule 8).
//
// Field names are the CSS variable names minus the "--color-" prefix in
// CamelCase, and every field comment cites the variable and the
// zcode-tokens.md section it came from, so the table can be diffed back
// against the source spec.
//
// Colors that ZCode defines with transparency (color-mix(... transparent)
// or rgba(...)) keep their alpha here as ui.RGBA values. Draw them over a
// known opaque base with ui.Color.Over when stacking matters, or rely on
// Apply, which pre-composites the tokens the built-in widgets use.
type Palette struct {
	// Dark reports which face this palette is: false for zai-light, true
	// for zai-dark. Prefer it over reading ui.Context.Theme().Dark.
	Dark bool

	// Background is the application background (--color-background,
	// zcode-tokens.md §2).
	Background ui.Color
	// WinAlt is the alternative window background
	// (--color-background-win-alt, §2).
	WinAlt ui.Color
	// BackgroundAlt is the translucent alternative background layer
	// (--color-background-alt, §2): zai-light mixes the background at 70%,
	// zai-dark mixes the win-alt background at 60%.
	BackgroundAlt ui.Color
	// Header is the top bar background (--color-header, §2).
	Header ui.Color
	// Panel is the panel background (--color-panel, §2).
	Panel ui.Color
	// Sidebar is the sidebar background (--color-sidebar, §2).
	Sidebar ui.Color
	// Surface is the row/list surface tint (--color-surface, §2).
	Surface ui.Color
	// SurfaceHover is the hovered row tint (--color-surface-hover, §2).
	SurfaceHover ui.Color
	// Card is the card background (--color-card, §2).
	Card ui.Color
	// CardSelected is the selected card background
	// (--color-card-selected = --color-input in zai, §2).
	CardSelected ui.Color
	// Popover is the floating layer background (--color-popover, §2).
	Popover ui.Color
	// PopoverHeader is the floating layer header background
	// (--color-popover-header, §2).
	PopoverHeader ui.Color
	// Input is the input background (--color-input, §2).
	Input ui.Color
	// InputFocused is the focused input background
	// (--color-input-focused = --color-input in zai, §2).
	InputFocused ui.Color
	// Tab is the inactive tab background (--color-tab, §2).
	Tab ui.Color
	// TabActive is the active tab background (--color-tab-active, §2).
	TabActive ui.Color
	// Menu is the menu background (--color-menu, §2).
	Menu ui.Color
	// MenuHover is the hovered menu item background (--color-menu-hover, §2).
	MenuHover ui.Color
	// Toast is the toast background (--color-toast, §2).
	Toast ui.Color
	// Tooltip is the tooltip background (--color-tooltip, §2).
	Tooltip ui.Color
	// TooltipTag is the keyboard-hint pill background inside tooltips
	// (--color-tooltip-tag, §2).
	TooltipTag ui.Color

	// Foreground is the primary text color (--color-foreground =
	// neutral-800 in zai-light, neutral-300 in zai-dark, §2/§4).
	Foreground ui.Color
	// ForegroundSubtle is secondary text (--color-foreground-subtle:
	// foreground mixed to 60% opacity, §2).
	ForegroundSubtle ui.Color
	// ForegroundSubtlest is the faintest text tier
	// (--color-foreground-subtlest: neutral-800 at 40% in zai-light,
	// neutral-300 at 30% in zai-dark, §2).
	ForegroundSubtlest ui.Color
	// ForegroundInverse is text on filled (primary) surfaces
	// (--color-foreground-inverse, §2).
	ForegroundInverse ui.Color
	// Primary is the primary button background (--color-primary, §2).
	Primary ui.Color
	// PrimaryForeground is text on primary surfaces
	// (--color-primary-foreground, §2). Bridging it into ui.Theme is
	// mandatory in zai-dark: Primary is white there, and mygo's dark theme
	// defaults AccentText to white, which would render white-on-white.
	PrimaryForeground ui.Color
	// Secondary is the secondary control background (--color-secondary, §2).
	Secondary ui.Color
	// TooltipForeground is tooltip text (--color-tooltip-foreground, §2).
	TooltipForeground ui.Color
	// TooltipTagForeground is keyboard-hint text inside tooltips
	// (--color-tooltip-tag-foreground, styles.css literal in zai).
	TooltipTagForeground ui.Color
	// Tag is the tag and inline-code background (--color-tag, §2).
	Tag ui.Color

	// Border is the generic border (--color-border, §2).
	Border ui.Color
	// BorderHover is the hovered border (--color-border-hover, §2).
	BorderHover ui.Color
	// InputBorderHover is the hovered input border
	// (--color-input-border-hover = --color-border-hover, §2).
	InputBorderHover ui.Color
	// InputBorderFocused is the focused input border. In zai it resolves
	// to border-hover, not brand (--color-input-border-focused, §2).
	InputBorderFocused ui.Color

	// Brand is the brand emphasis color (--color-brand: black in
	// zai-light, white in zai-dark, §2).
	Brand ui.Color
	// Accent is the accent surface (--color-accent, §2).
	Accent ui.Color
	// Hover is the generic hover tint (--color-hover, §2).
	Hover ui.Color
	// Selected is the selection tint (--color-selected, §2).
	Selected ui.Color
	// IconBlue is the link/icon blue (--color-icon-blue =
	// --color-terminal-bright-blue in zai: #0066dd light, #80beff dark,
	// §2 and zcode-tokens.md §3.3 note).
	IconBlue ui.Color
	// Success is the success color (--color-success, §2).
	Success ui.Color
	// Warning is the warning color (--color-warning, §2).
	Warning ui.Color
	// Destructive is the danger color (--color-destructive, §2).
	Destructive ui.Color
	// DestructiveForeground is text on destructive surfaces
	// (--color-destructive-foreground, §2: white in both zai faces —
	// styles.css:708 keeps white-on-red even in zai-dark, where it
	// differs from ForegroundInverse).
	DestructiveForeground ui.Color
	// DiffAdded colors added diff lines (--color-diff-added, §2).
	DiffAdded ui.Color
	// DiffRemoved colors removed diff lines (--color-diff-removed, §2).
	DiffRemoved ui.Color
	// FindHighlight is the search-hit background (--color-find-highlight, §2).
	FindHighlight ui.Color
	// FindHighlightActive is the current search-hit background
	// (--color-find-highlight-active, §2).
	FindHighlightActive ui.Color

	// TrajectoryUser colors user message accents
	// (--color-trajectory-user, §2: same as the default theme).
	TrajectoryUser ui.Color
	// TrajectoryAssistant colors assistant message accents
	// (--color-trajectory-assistant, §2).
	TrajectoryAssistant ui.Color
	// TrajectoryReasoning colors reasoning accents
	// (--color-trajectory-reasoning, §2).
	TrajectoryReasoning ui.Color
	// TrajectoryToolCall colors tool-call accents
	// (--color-trajectory-tool-call, §2).
	TrajectoryToolCall ui.Color
	// TrajectoryToolResult colors tool-result accents
	// (--color-trajectory-tool-result, §2).
	TrajectoryToolResult ui.Color
}

// zaiLight is the zai-light palette. Every value is copied verbatim from
// docs/go-migration/zcode-tokens.md §2, which extracts them from the ZCode
// styles.css .theme-zai-light block.
var zaiLight = Palette{
	Dark: false,

	Background:    ui.Hex("#f8f8f8"),
	WinAlt:        ui.Hex("#ececee"),
	BackgroundAlt: ui.RGBA(0xf8, 0xf8, 0xf8, 0.70), // background @ 70%
	Header:        ui.Hex("#ffffff"),
	Panel:         ui.Hex("#ffffff"),
	Sidebar:       ui.Hex("#f0f0f0"),
	Surface:       ui.RGBA(13, 13, 13, 0.03),
	SurfaceHover:  ui.RGBA(13, 13, 13, 0.05),
	Card:          ui.Hex("#ffffff"),
	CardSelected:  ui.Hex("#ffffff"), // = --color-input
	Popover:       ui.Hex("#ffffff"),
	PopoverHeader: ui.Hex("#f8f8f8"),
	Input:         ui.Hex("#ffffff"),
	InputFocused:  ui.Hex("#ffffff"), // = --color-input
	Tab:           ui.Hex("#f0f0f0"),
	TabActive:     ui.Hex("#ffffff"),
	Menu:          ui.Hex("#ffffff"),
	MenuHover:     ui.Hex("#f0f0f0"),
	Toast:         ui.Hex("#ffffff"),
	Tooltip:       ui.Hex("#f0f0f0"),
	TooltipTag:    ui.Hex("#e6e6e6"),

	Foreground:           ui.Hex("#262626"),         // = neutral-800
	ForegroundSubtle:     ui.RGBA(38, 38, 38, 0.60), // neutral-800 @ 60%
	ForegroundSubtlest:   ui.RGBA(38, 38, 38, 0.40), // neutral-800 @ 40%
	ForegroundInverse:    ui.Hex("#ffffff"),
	Primary:              ui.Hex("#000000"),
	PrimaryForeground:    ui.Hex("#ffffff"),
	Secondary:            ui.Hex("#e6e6e6"),
	TooltipForeground:    ui.Hex("#0d0d0d"),
	TooltipTagForeground: ui.Hex("#5c5c5c"),
	Tag:                  ui.Hex("#e6e6e6"),

	Border:             ui.RGBA(13, 13, 13, 0.10),
	BorderHover:        ui.RGBA(13, 13, 13, 0.15),
	InputBorderHover:   ui.RGBA(13, 13, 13, 0.15), // = --color-border-hover
	InputBorderFocused: ui.RGBA(13, 13, 13, 0.15), // = --color-border-hover (not brand in zai)

	Brand:                 ui.Hex("#000000"),
	Accent:                ui.Hex("#ebf4ff"),
	Hover:                 ui.RGBA(13, 13, 13, 0.05),
	Selected:              ui.RGBA(13, 13, 13, 0.05),
	IconBlue:              ui.Hex("#0066dd"),
	Success:               ui.Hex("#1e8a3e"),
	Warning:               ui.Hex("#e07b00"),
	Destructive:           ui.Hex("#e03131"),
	DestructiveForeground: ui.Hex("#ffffff"),
	DiffAdded:             ui.Hex("#1e8a3e"),
	DiffRemoved:           ui.Hex("#e03131"),
	FindHighlight:         ui.Hex("#fff4eb"),
	FindHighlightActive:   ui.Hex("#ffb26b"),

	TrajectoryUser:       ui.Hex("#2563eb"),
	TrajectoryAssistant:  ui.Hex("#0f766e"),
	TrajectoryReasoning:  ui.Hex("#7c3aed"),
	TrajectoryToolCall:   ui.Hex("#d97706"),
	TrajectoryToolResult: ui.Hex("#0284c7"),
}

// zaiDark is the zai-dark palette and the product default (docs/go-migration.md
// D3). Every value is copied verbatim from docs/go-migration/zcode-tokens.md
// §2, which extracts them from the ZCode styles.css .theme-zai-dark block.
var zaiDark = Palette{
	Dark: true,

	Background:    ui.Hex("#161616"),
	WinAlt:        ui.Hex("#2b2b2b"),
	BackgroundAlt: ui.RGBA(0x2b, 0x2b, 0x2b, 0.60), // win-alt @ 60%
	Header:        ui.Hex("#202020"),
	Panel:         ui.Hex("#202020"),
	Sidebar:       ui.Hex("#161616"),
	Surface:       ui.RGBA(255, 255, 255, 0.05),
	SurfaceHover:  ui.RGBA(255, 255, 255, 0.10),
	Card:          ui.Hex("#2b2b2b"),
	CardSelected:  ui.Hex("#2b2b2b"), // = --color-input
	Popover:       ui.Hex("#2b2b2b"),
	PopoverHeader: ui.Hex("#202020"),
	Input:         ui.Hex("#2b2b2b"),
	InputFocused:  ui.Hex("#2b2b2b"), // = --color-input
	Tab:           ui.Hex("#202020"),
	TabActive:     ui.Hex("#161616"),
	Menu:          ui.Hex("#2b2b2b"),
	MenuHover:     ui.Hex("#363636"),
	Toast:         ui.Hex("#2b2b2b"),
	Tooltip:       ui.Hex("#2b2b2b"),
	TooltipTag:    ui.Hex("#363636"),

	Foreground:           ui.Hex("#d4d4d4"),            // = neutral-300
	ForegroundSubtle:     ui.RGBA(212, 212, 212, 0.60), // neutral-300 @ 60%
	ForegroundSubtlest:   ui.RGBA(212, 212, 212, 0.30), // neutral-300 @ 30%
	ForegroundInverse:    ui.Hex("#000000"),
	Primary:              ui.Hex("#ffffff"),
	PrimaryForeground:    ui.Hex("#000000"),
	Secondary:            ui.Hex("#363636"),
	TooltipForeground:    ui.Hex("#f8f8f8"),
	TooltipTagForeground: ui.Hex("#adadad"),
	Tag:                  ui.Hex("#363636"),

	Border:             ui.RGBA(255, 255, 255, 0.10),
	BorderHover:        ui.RGBA(255, 255, 255, 0.15),
	InputBorderHover:   ui.RGBA(255, 255, 255, 0.15), // = --color-border-hover
	InputBorderFocused: ui.RGBA(255, 255, 255, 0.15), // = --color-border-hover (not brand in zai)

	Brand:                 ui.Hex("#ffffff"),
	Accent:                ui.Hex("#001d3d"),
	Hover:                 ui.RGBA(255, 255, 255, 0.05),
	Selected:              ui.RGBA(255, 255, 255, 0.10),
	IconBlue:              ui.Hex("#80beff"),
	Success:               ui.Hex("#46bf72"),
	Warning:               ui.Hex("#ff8a30"),
	Destructive:           ui.Hex("#ff5c5c"),
	DestructiveForeground: ui.Hex("#ffffff"),
	DiffAdded:             ui.Hex("#46bf72"),
	DiffRemoved:           ui.Hex("#ff5c5c"),
	FindHighlight:         ui.Hex("#542500"),
	FindHighlightActive:   ui.Hex("#ff8a30"),

	TrajectoryUser:       ui.Hex("#60a5fa"),
	TrajectoryAssistant:  ui.Hex("#2dd4bf"),
	TrajectoryReasoning:  ui.Hex("#a78bfa"),
	TrajectoryToolCall:   ui.Hex("#f59e0b"),
	TrajectoryToolResult: ui.Hex("#38bdf8"),
}
