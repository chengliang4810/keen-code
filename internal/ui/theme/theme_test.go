package theme

import (
	"testing"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

// eq reports whether two colors are identical byte for byte.
func eq(got, want ui.Color) bool {
	return got.R == want.R && got.G == want.G && got.B == want.B && got.A == want.A
}

// col builds a ui.Color literal for expectations.
func col(r, g, b, a uint8) ui.Color { return ui.Color{R: r, G: g, B: b, A: a} }

// TestPaletteTokensSpotCheck asserts sampled palette values byte for byte
// against docs/go-migration/zcode-tokens.md §2 (the zai theme tables
// extracted from ZCode styles.css). Opaque tokens use the literal hex;
// tokens ZCode defines with transparency keep the documented alpha, which
// ui.RGBA converts with alphaByte(a) = uint8(a*255+0.5).
func TestPaletteTokensSpotCheck(t *testing.T) {
	tests := []struct {
		name string
		got  ui.Color
		want ui.Color
	}{
		// zai-light (zcode-tokens.md §2, styles.css .theme-zai-light).
		{"light.Background", zaiLight.Background, col(248, 248, 248, 255)},                   // #f8f8f8
		{"light.WinAlt", zaiLight.WinAlt, col(236, 236, 238, 255)},                           // #ececee
		{"light.BackgroundAlt", zaiLight.BackgroundAlt, col(248, 248, 248, 179)},             // background @70% (0.70*255+0.5 rounds to 179)
		{"light.Header", zaiLight.Header, col(255, 255, 255, 255)},                           // #ffffff
		{"light.Sidebar", zaiLight.Sidebar, col(240, 240, 240, 255)},                         // #f0f0f0
		{"light.Surface", zaiLight.Surface, col(13, 13, 13, 8)},                              // rgba(13,13,13,0.03)
		{"light.SurfaceHover", zaiLight.SurfaceHover, col(13, 13, 13, 13)},                   // rgba(13,13,13,0.05)
		{"light.Card", zaiLight.Card, col(255, 255, 255, 255)},                               // #ffffff
		{"light.CardSelected", zaiLight.CardSelected, col(255, 255, 255, 255)},               // = input
		{"light.PopoverHeader", zaiLight.PopoverHeader, col(248, 248, 248, 255)},             // #f8f8f8
		{"light.InputFocused", zaiLight.InputFocused, col(255, 255, 255, 255)},               // = input
		{"light.Tab", zaiLight.Tab, col(240, 240, 240, 255)},                                 // #f0f0f0
		{"light.TabActive", zaiLight.TabActive, col(255, 255, 255, 255)},                     // #ffffff
		{"light.Menu", zaiLight.Menu, col(255, 255, 255, 255)},                               // #ffffff
		{"light.MenuHover", zaiLight.MenuHover, col(240, 240, 240, 255)},                     // #f0f0f0
		{"light.Toast", zaiLight.Toast, col(255, 255, 255, 255)},                             // #ffffff
		{"light.Tooltip", zaiLight.Tooltip, col(240, 240, 240, 255)},                         // #f0f0f0
		{"light.TooltipTag", zaiLight.TooltipTag, col(230, 230, 230, 255)},                   // #e6e6e6
		{"light.Foreground", zaiLight.Foreground, col(38, 38, 38, 255)},                      // neutral-800 #262626
		{"light.ForegroundSubtle", zaiLight.ForegroundSubtle, col(38, 38, 38, 153)},          // neutral-800 @60%
		{"light.ForegroundSubtlest", zaiLight.ForegroundSubtlest, col(38, 38, 38, 102)},      // neutral-800 @40%
		{"light.ForegroundInverse", zaiLight.ForegroundInverse, col(255, 255, 255, 255)},     // #ffffff
		{"light.Primary", zaiLight.Primary, col(0, 0, 0, 255)},                               // #000000
		{"light.PrimaryForeground", zaiLight.PrimaryForeground, col(255, 255, 255, 255)},     // #ffffff
		{"light.Secondary", zaiLight.Secondary, col(230, 230, 230, 255)},                     // #e6e6e6
		{"light.TooltipForeground", zaiLight.TooltipForeground, col(13, 13, 13, 255)},        // #0d0d0d
		{"light.TooltipTagForeground", zaiLight.TooltipTagForeground, col(92, 92, 92, 255)},  // #5c5c5c
		{"light.Tag", zaiLight.Tag, col(230, 230, 230, 255)},                                 // #e6e6e6
		{"light.Border", zaiLight.Border, col(13, 13, 13, 26)},                               // rgba(13,13,13,0.1)
		{"light.BorderHover", zaiLight.BorderHover, col(13, 13, 13, 38)},                     // rgba(13,13,13,0.15)
		{"light.InputBorderHover", zaiLight.InputBorderHover, col(13, 13, 13, 38)},           // = border-hover
		{"light.InputBorderFocused", zaiLight.InputBorderFocused, col(13, 13, 13, 38)},       // = border-hover
		{"light.Brand", zaiLight.Brand, col(0, 0, 0, 255)},                                   // #000000
		{"light.Accent", zaiLight.Accent, col(235, 244, 255, 255)},                           // #ebf4ff
		{"light.Hover", zaiLight.Hover, col(13, 13, 13, 13)},                                 // rgba(13,13,13,0.05)
		{"light.Selected", zaiLight.Selected, col(13, 13, 13, 13)},                           // rgba(13,13,13,0.05)
		{"light.IconBlue", zaiLight.IconBlue, col(0, 102, 221, 255)},                         // #0066dd (terminal-bright-blue)
		{"light.Success", zaiLight.Success, col(30, 138, 62, 255)},                           // #1e8a3e
		{"light.Warning", zaiLight.Warning, col(224, 123, 0, 255)},                           // #e07b00
		{"light.Destructive", zaiLight.Destructive, col(224, 49, 49, 255)},                   // #e03131
		{"light.DiffAdded", zaiLight.DiffAdded, col(30, 138, 62, 255)},                       // #1e8a3e
		{"light.DiffRemoved", zaiLight.DiffRemoved, col(224, 49, 49, 255)},                   // #e03131
		{"light.FindHighlight", zaiLight.FindHighlight, col(255, 244, 235, 255)},             // #fff4eb
		{"light.FindHighlightActive", zaiLight.FindHighlightActive, col(255, 178, 107, 255)}, // #ffb26b
		{"light.TrajectoryUser", zaiLight.TrajectoryUser, col(37, 99, 235, 255)},             // #2563eb
		{"light.TrajectoryAssistant", zaiLight.TrajectoryAssistant, col(15, 118, 110, 255)},  // #0f766e
		{"light.TrajectoryReasoning", zaiLight.TrajectoryReasoning, col(124, 58, 237, 255)},  // #7c3aed
		{"light.TrajectoryToolCall", zaiLight.TrajectoryToolCall, col(217, 119, 6, 255)},     // #d97706
		{"light.TrajectoryToolResult", zaiLight.TrajectoryToolResult, col(2, 132, 199, 255)}, // #0284c7

		// zai-dark (zcode-tokens.md §2, styles.css .theme-zai-dark).
		{"dark.Background", zaiDark.Background, col(22, 22, 22, 255)},                        // #161616
		{"dark.WinAlt", zaiDark.WinAlt, col(43, 43, 43, 255)},                                // #2b2b2b
		{"dark.BackgroundAlt", zaiDark.BackgroundAlt, col(43, 43, 43, 153)},                  // win-alt @60%
		{"dark.Header", zaiDark.Header, col(32, 32, 32, 255)},                                // #202020
		{"dark.Panel", zaiDark.Panel, col(32, 32, 32, 255)},                                  // #202020
		{"dark.Sidebar", zaiDark.Sidebar, col(22, 22, 22, 255)},                              // #161616
		{"dark.Surface", zaiDark.Surface, col(255, 255, 255, 13)},                            // rgba(255,255,255,0.05)
		{"dark.SurfaceHover", zaiDark.SurfaceHover, col(255, 255, 255, 26)},                  // rgba(255,255,255,0.1)
		{"dark.Card", zaiDark.Card, col(43, 43, 43, 255)},                                    // #2b2b2b
		{"dark.CardSelected", zaiDark.CardSelected, col(43, 43, 43, 255)},                    // = input
		{"dark.Popover", zaiDark.Popover, col(43, 43, 43, 255)},                              // #2b2b2b
		{"dark.PopoverHeader", zaiDark.PopoverHeader, col(32, 32, 32, 255)},                  // #202020
		{"dark.Input", zaiDark.Input, col(43, 43, 43, 255)},                                  // #2b2b2b
		{"dark.InputFocused", zaiDark.InputFocused, col(43, 43, 43, 255)},                    // = input
		{"dark.Tab", zaiDark.Tab, col(32, 32, 32, 255)},                                      // #202020
		{"dark.TabActive", zaiDark.TabActive, col(22, 22, 22, 255)},                          // #161616
		{"dark.Menu", zaiDark.Menu, col(43, 43, 43, 255)},                                    // #2b2b2b
		{"dark.MenuHover", zaiDark.MenuHover, col(54, 54, 54, 255)},                          // #363636
		{"dark.Toast", zaiDark.Toast, col(43, 43, 43, 255)},                                  // #2b2b2b
		{"dark.Tooltip", zaiDark.Tooltip, col(43, 43, 43, 255)},                              // #2b2b2b
		{"dark.TooltipTag", zaiDark.TooltipTag, col(54, 54, 54, 255)},                        // #363636
		{"dark.Foreground", zaiDark.Foreground, col(212, 212, 212, 255)},                     // neutral-300 #d4d4d4
		{"dark.ForegroundSubtle", zaiDark.ForegroundSubtle, col(212, 212, 212, 153)},         // neutral-300 @60%
		{"dark.ForegroundSubtlest", zaiDark.ForegroundSubtlest, col(212, 212, 212, 77)},      // neutral-300 @30%
		{"dark.ForegroundInverse", zaiDark.ForegroundInverse, col(0, 0, 0, 255)},             // #000000
		{"dark.Primary", zaiDark.Primary, col(255, 255, 255, 255)},                           // #ffffff
		{"dark.PrimaryForeground", zaiDark.PrimaryForeground, col(0, 0, 0, 255)},             // #000000
		{"dark.Secondary", zaiDark.Secondary, col(54, 54, 54, 255)},                          // #363636
		{"dark.TooltipForeground", zaiDark.TooltipForeground, col(248, 248, 248, 255)},       // #f8f8f8
		{"dark.TooltipTagForeground", zaiDark.TooltipTagForeground, col(173, 173, 173, 255)}, // #adadad
		{"dark.Tag", zaiDark.Tag, col(54, 54, 54, 255)},                                      // #363636
		{"dark.Border", zaiDark.Border, col(255, 255, 255, 26)},                              // rgba(255,255,255,0.1)
		{"dark.BorderHover", zaiDark.BorderHover, col(255, 255, 255, 38)},                    // rgba(255,255,255,0.15)
		{"dark.InputBorderHover", zaiDark.InputBorderHover, col(255, 255, 255, 38)},          // = border-hover
		{"dark.InputBorderFocused", zaiDark.InputBorderFocused, col(255, 255, 255, 38)},      // = border-hover
		{"dark.Brand", zaiDark.Brand, col(255, 255, 255, 255)},                               // #ffffff
		{"dark.Accent", zaiDark.Accent, col(0, 29, 61, 255)},                                 // #001d3d
		{"dark.Hover", zaiDark.Hover, col(255, 255, 255, 13)},                                // rgba(255,255,255,0.05)
		{"dark.Selected", zaiDark.Selected, col(255, 255, 255, 26)},                          // rgba(255,255,255,0.1)
		{"dark.IconBlue", zaiDark.IconBlue, col(128, 190, 255, 255)},                         // #80beff (terminal-bright-blue)
		{"dark.Success", zaiDark.Success, col(70, 191, 114, 255)},                            // #46bf72
		{"dark.Warning", zaiDark.Warning, col(255, 138, 48, 255)},                            // #ff8a30
		{"dark.Destructive", zaiDark.Destructive, col(255, 92, 92, 255)},                     // #ff5c5c
		{"dark.DiffAdded", zaiDark.DiffAdded, col(70, 191, 114, 255)},                        // #46bf72
		{"dark.DiffRemoved", zaiDark.DiffRemoved, col(255, 92, 92, 255)},                     // #ff5c5c
		{"dark.FindHighlight", zaiDark.FindHighlight, col(84, 37, 0, 255)},                   // #542500
		{"dark.FindHighlightActive", zaiDark.FindHighlightActive, col(255, 138, 48, 255)},    // #ff8a30
		{"dark.TrajectoryUser", zaiDark.TrajectoryUser, col(96, 165, 250, 255)},              // #60a5fa
		{"dark.TrajectoryAssistant", zaiDark.TrajectoryAssistant, col(45, 212, 191, 255)},    // #2dd4bf
		{"dark.TrajectoryReasoning", zaiDark.TrajectoryReasoning, col(167, 139, 250, 255)},   // #a78bfa
		{"dark.TrajectoryToolCall", zaiDark.TrajectoryToolCall, col(245, 158, 11, 255)},      // #f59e0b
		{"dark.TrajectoryToolResult", zaiDark.TrajectoryToolResult, col(56, 189, 248, 255)},  // #38bdf8
	}
	for _, tt := range tests {
		if !eq(tt.got, tt.want) {
			t.Errorf("%s = R%d G%d B%d A%d, want R%d G%d B%d A%d (zcode-tokens.md §2)",
				tt.name, tt.got.R, tt.got.G, tt.got.B, tt.got.A, tt.want.R, tt.want.G, tt.want.B, tt.want.A)
		}
	}
}

// TestDarkFlag checks the face flags on both palettes.
func TestDarkFlag(t *testing.T) {
	if zaiLight.Dark {
		t.Error("zaiLight.Dark = true, want false")
	}
	if !zaiDark.Dark {
		t.Error("zaiDark.Dark = false, want true")
	}
}

// TestActiveFollowsAppearance runs a view headlessly and checks that
// Active returns the zai-dark palette in the dark appearance and the
// zai-light palette in the light one.
func TestActiveFollowsAppearance(t *testing.T) {
	var pal *Palette
	view := func(c *ui.Context) { pal = Active(c) }
	tt := ui.NewTester(view, 320, 200)

	tt.SetDark(true)
	if pal != &zaiDark {
		t.Fatalf("Active(c) in dark appearance is not the zai-dark palette")
	}
	if !pal.Dark {
		t.Error("zaiDark.Dark = false, want true")
	}
	if !eq(pal.Background, col(22, 22, 22, 255)) { // #161616
		t.Errorf("dark Background = R%d G%d B%d A%d, want R22 G22 B22 A255",
			pal.Background.R, pal.Background.G, pal.Background.B, pal.Background.A)
	}

	tt.SetDark(false)
	if pal != &zaiLight {
		t.Fatalf("Active(c) in light appearance is not the zai-light palette")
	}
	if pal.Dark {
		t.Error("zaiLight.Dark = true, want false")
	}
	if !eq(pal.Background, col(248, 248, 248, 255)) { // #f8f8f8
		t.Errorf("light Background = R%d G%d B%d A%d, want R248 G248 B248 A255",
			pal.Background.R, pal.Background.G, pal.Background.B, pal.Background.A)
	}
}

// TestApplyBridgesPalette checks the ui.Theme mapping of docs/go-migration.md
// §3.1 rule 7 in both appearances, including the pre-composited translucent
// tokens. Composited expectations are derived by hand:
//
//	dark Surface:   rgba(255,255,255,0.05) over #161616 → #222222 (R34)
//	dark Hover:     rgba(255,255,255,0.05) over #161616 → #222222 (R34)
//	dark Selected:  rgba(255,255,255,0.10) over #161616 → #2e2e2e (R46)
//	dark BorderHover over #161616:  rgba(255,255,255,0.15) → #393939 (R57)
//	light Surface:  rgba(13,13,13,0.03) over #f8f8f8 → #f1f1f1 (R241)
//	light Selected: rgba(13,13,13,0.05) over #f8f8f8 → #ececec (R236)
//	light BorderHover over #f8f8f8: rgba(13,13,13,0.15) → #d5d5d5 (R213)
func TestApplyBridgesPalette(t *testing.T) {
	var snap ui.Theme
	view := func(c *ui.Context) {
		Apply(c, Active(c))
		snap = *c.Theme()
	}
	tt := ui.NewTester(view, 320, 200)

	tt.SetDark(true)
	want := []struct {
		name string
		got  ui.Color
		want ui.Color
	}{
		{"Background", snap.Background, col(22, 22, 22, 255)},
		{"Surface", snap.Surface, col(34, 34, 34, 255)},
		{"SurfaceHover", snap.SurfaceHover, col(46, 46, 46, 255)},
		{"SurfacePressed", snap.SurfacePressed, col(46, 46, 46, 255)},
		{"Border", snap.Border, col(255, 255, 255, 26)},
		{"Text", snap.Text, col(212, 212, 212, 255)},
		{"TextMuted", snap.TextMuted, col(212, 212, 212, 153)},
		{"Accent", snap.Accent, col(255, 255, 255, 255)},
		{"AccentHover", snap.AccentHover, col(34, 34, 34, 255)},
		{"AccentPressed", snap.AccentPressed, col(46, 46, 46, 255)},
		{"AccentText", snap.AccentText, col(0, 0, 0, 255)},
		{"Danger", snap.Danger, col(255, 92, 92, 255)},
		{"Warning", snap.Warning, col(255, 138, 48, 255)},
		{"Success", snap.Success, col(70, 191, 114, 255)},
		{"Selection", snap.Selection, col(46, 46, 46, 255)},
		{"Focus", snap.Focus, col(255, 255, 255, 38)},
		{"Scrollbar", snap.Scrollbar, col(57, 57, 57, 255)},
	}
	for _, w := range want {
		if !eq(w.got, w.want) {
			t.Errorf("dark %s = R%d G%d B%d A%d, want R%d G%d B%d A%d",
				w.name, w.got.R, w.got.G, w.got.B, w.got.A, w.want.R, w.want.G, w.want.B, w.want.A)
		}
	}
	if !snap.Dark {
		t.Error("bridged dark theme has Dark = false, want true")
	}
	checkMetrics(t, snap)

	tt.SetDark(false)
	wantLight := []struct {
		name string
		got  ui.Color
		want ui.Color
	}{
		{"Background", snap.Background, col(248, 248, 248, 255)},
		{"Surface", snap.Surface, col(241, 241, 241, 255)},
		{"SurfaceHover", snap.SurfaceHover, col(236, 236, 236, 255)},
		{"SurfacePressed", snap.SurfacePressed, col(236, 236, 236, 255)},
		{"Border", snap.Border, col(13, 13, 13, 26)},
		{"Text", snap.Text, col(38, 38, 38, 255)},
		{"TextMuted", snap.TextMuted, col(38, 38, 38, 153)},
		{"Accent", snap.Accent, col(0, 0, 0, 255)},
		{"AccentHover", snap.AccentHover, col(236, 236, 236, 255)},
		{"AccentPressed", snap.AccentPressed, col(236, 236, 236, 255)},
		{"AccentText", snap.AccentText, col(255, 255, 255, 255)},
		{"Danger", snap.Danger, col(224, 49, 49, 255)},
		{"Warning", snap.Warning, col(224, 123, 0, 255)},
		{"Success", snap.Success, col(30, 138, 62, 255)},
		{"Selection", snap.Selection, col(236, 236, 236, 255)},
		{"Focus", snap.Focus, col(13, 13, 13, 38)},
		{"Scrollbar", snap.Scrollbar, col(213, 213, 213, 255)},
	}
	for _, w := range wantLight {
		if !eq(w.got, w.want) {
			t.Errorf("light %s = R%d G%d B%d A%d, want R%d G%d B%d A%d",
				w.name, w.got.R, w.got.G, w.got.B, w.got.A, w.want.R, w.want.G, w.want.B, w.want.A)
		}
	}
	if snap.Dark {
		t.Error("bridged light theme has Dark = true, want false")
	}
	checkMetrics(t, snap)
}

// checkMetrics asserts the metric part of the bridge: scrollbar width 14
// (zcode-tokens.md §3), radius 8 (lg), spacing 4, base font size 14
// (docs/go-migration.md §3.1 rule 7).
func checkMetrics(t *testing.T, snap ui.Theme) {
	t.Helper()
	if snap.ScrollbarWidth != ScrollbarWidth {
		t.Errorf("ScrollbarWidth = %v, want %v", snap.ScrollbarWidth, ScrollbarWidth)
	}
	if snap.Radius != RadiusLG {
		t.Errorf("Radius = %v, want %v", snap.Radius, RadiusLG)
	}
	if snap.Spacing != SpaceUnit {
		t.Errorf("Spacing = %v, want %v", snap.Spacing, SpaceUnit)
	}
	if snap.FontSize != FontBase {
		t.Errorf("FontSize = %v, want %v", snap.FontSize, FontBase)
	}
}

// TestThemeSettingAndQuery checks the appearance preference: recording,
// query, and normalization of unknown values, plus the string equivalence
// between the theme settings and mygo's ThemeSource constants that
// SetTheme relies on. SetTheme itself forwards through
// mygo.Theme.SetSource; that dispatch needs the running app and follows
// mygo's documented threading contract, so it is exercised by W3/W4
// native acceptance rather than here.
func TestThemeSettingAndQuery(t *testing.T) {
	defer storeSetting(SettingSystem) // restore the default for other tests

	storeSetting(SettingDark)
	if got := Theme(); got != SettingDark {
		t.Errorf("after storing dark, Theme() = %q, want %q", got, SettingDark)
	}
	storeSetting(SettingLight)
	if got := Theme(); got != SettingLight {
		t.Errorf("after storing light, Theme() = %q, want %q", got, SettingLight)
	}
	storeSetting(Setting("bogus"))
	if got := Theme(); got != SettingSystem {
		t.Errorf("after storing %q, Theme() = %q, want %q", "bogus", got, SettingSystem)
	}
	storeSetting(Setting(""))
	if got := Theme(); got != SettingSystem {
		t.Errorf("after storing the empty setting, Theme() = %q, want %q", got, SettingSystem)
	}

	sources := []struct {
		setting Setting
		source  mygo.ThemeSource
	}{
		{SettingSystem, mygo.ThemeSystem},
		{SettingLight, mygo.ThemeLight},
		{SettingDark, mygo.ThemeDark},
	}
	for _, s := range sources {
		if mygo.ThemeSource(s.setting) != s.source {
			t.Errorf("mygo.ThemeSource(%q) = %q, want %q", s.setting, mygo.ThemeSource(s.setting), s.source)
		}
	}
}

// TestScaleConstants pins the non-color tokens to their documented values
// (docs/go-migration.md §3.2, zcode-tokens.md §5-§9).
func TestScaleConstants(t *testing.T) {
	tests := []struct {
		name string
		got  float32
		want float32
	}{
		{"FontXL", FontXL, 18},
		{"FontLG", FontLG, 16},
		{"FontBase", FontBase, 14},
		{"FontCaption", FontCaption, 13},
		{"FontSM", FontSM, 12},
		{"FontXS", FontXS, 10},
		{"LineHeightBody", LineHeightBody, 1.75},
		{"RadiusXS", RadiusXS, 2},
		{"RadiusSM", RadiusSM, 4},
		{"RadiusMD", RadiusMD, 6},
		{"RadiusLG", RadiusLG, 8},
		{"RadiusXL", RadiusXL, 12},
		{"RadiusXXL", RadiusXXL, 16},
		{"RadiusFull", RadiusFull, 9999},
		{"SpaceUnit", SpaceUnit, 4},
		{"SpaceTurnTop", SpaceTurnTop, 56},
		{"SpaceTurnBottom", SpaceTurnBottom, 20},
		{"SpaceInTurn", SpaceInTurn, 20},
		{"SpaceWorkItem", SpaceWorkItem, 16},
		{"ColumnDraft", ColumnDraft, 672},
		{"ColumnSession", ColumnSession, 896},
		{"ColumnSessionWide", ColumnSessionWide, 1152},
		{"ScrollbarWidth", ScrollbarWidth, 14},
	}
	for _, tt := range tests {
		if tt.got != tt.want {
			t.Errorf("%s = %v, want %v", tt.name, tt.got, tt.want)
		}
	}

	weights := []struct {
		name string
		got  int
		want int
	}{
		{"WeightNormal", WeightNormal, 400},
		{"WeightMedium", WeightMedium, 500},
		{"WeightSemibold", WeightSemibold, 600},
		{"WeightBold", WeightBold, 700},
	}
	for _, tt := range weights {
		if tt.got != tt.want {
			t.Errorf("%s = %d, want %d", tt.name, tt.got, tt.want)
		}
	}

	durations := []struct {
		name string
		got  time.Duration
		want time.Duration
	}{
		{"AnimCollapse", AnimCollapse, 300 * time.Millisecond},
		{"AnimEnterExit", AnimEnterExit, 150 * time.Millisecond},
		{"AnimStreamTextIn", AnimStreamTextIn, 900 * time.Millisecond},
		{"AnimDraftCascade", AnimDraftCascade, 260 * time.Millisecond},
		{"AnimGradientFlow", AnimGradientFlow, 4 * time.Second},
		{"AnimWidgetFast", AnimWidgetFast, 120 * time.Millisecond},
		{"AnimWidgetBase", AnimWidgetBase, 160 * time.Millisecond},
		{"AnimWidgetEnter", AnimWidgetEnter, 200 * time.Millisecond},
		{"AnimWidgetInk", AnimWidgetInk, 320 * time.Millisecond},
	}
	for _, tt := range durations {
		if tt.got != tt.want {
			t.Errorf("%s = %v, want %v", tt.name, tt.got, tt.want)
		}
	}
}
