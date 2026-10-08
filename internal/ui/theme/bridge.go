package theme

import (
	"sync"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

// Setting selects which appearance the app uses.
type Setting string

// Appearance settings. The zero value and any unrecognized value resolve
// to SettingSystem.
const (
	SettingSystem Setting = "system" // follow the OS appearance
	SettingLight  Setting = "light"  // pin zai-light
	SettingDark   Setting = "dark"   // pin zai-dark (the product default)
)

var (
	settingMu sync.Mutex
	setting   = SettingSystem
)

// SetTheme pins the current appearance and applies it to the desktop via
// mygo.Theme.SetSource, which repaints running windows. Follow mygo's
// threading contract: call it from the main goroutine (including before
// mygo.App.Run, e.g. at startup with a saved preference) or from any
// goroutine after the app runs; before Run, other goroutines would block
// until the app starts. Unrecognized values are normalized to
// SettingSystem.
func SetTheme(s Setting) {
	storeSetting(s)
	// The mygo and theme constants share the same string values.
	mygo.Theme.SetSource(mygo.ThemeSource(Theme()))
}

// storeSetting records the appearance preference, normalizing unknown
// values to SettingSystem. It never touches the desktop, so it is safe
// from any goroutine at any time.
func storeSetting(s Setting) {
	s = normalize(s)
	settingMu.Lock()
	setting = s
	settingMu.Unlock()
}

// Theme reports the currently pinned appearance preference. It defaults to
// SettingSystem until SetTheme pins something else.
func Theme() Setting {
	settingMu.Lock()
	defer settingMu.Unlock()
	return setting
}

// normalize maps the zero value and unknown settings to SettingSystem.
func normalize(s Setting) Setting {
	switch s {
	case SettingLight, SettingDark:
		return s
	default:
		return SettingSystem
	}
}

// Active returns the zai palette matching the frame's current appearance.
// Call it every frame at the top of a view and read colors only from the
// result: the engine re-resolves the theme when the OS appearance changes
// and reruns the view, so this follows light/dark switches for free
// (docs/go-migration.md §3.1 rule 6).
func Active(c *ui.Context) *Palette {
	if c.Theme().Dark {
		return &zaiDark
	}
	return &zaiLight
}

// Apply bridges the palette into mygo's built-in widgets: it overlays the
// palette on the frame theme per the mapping table in docs/go-migration.md
// §3.1 rule 7 and installs it with ui.Context.SetTheme. Call it from the
// view each frame, right after Active. Semitransparent tokens are
// pre-composited over the palette background with ui.Color.Over, because
// the built-in widgets paint their own surface and do not stack over the
// window background.
func Apply(c *ui.Context, pal *Palette) {
	t := *c.Theme() // keeps Dark and the platform default Font
	t.Background = pal.Background
	t.Surface = pal.Surface.Over(pal.Background)
	t.SurfaceHover = pal.SurfaceHover.Over(pal.Background)
	t.SurfacePressed = pal.Selected.Over(pal.Background)
	t.Border = pal.Border
	t.Text = pal.Foreground
	t.TextMuted = pal.ForegroundSubtle
	t.Accent = pal.Brand
	t.AccentHover = pal.Hover.Over(pal.Background)
	t.AccentPressed = pal.Selected.Over(pal.Background)
	t.AccentText = pal.PrimaryForeground
	t.Danger = pal.Destructive
	t.Warning = pal.Warning
	t.Success = pal.Success
	t.Selection = pal.Selected.Over(pal.Background)
	t.Focus = pal.InputBorderFocused
	t.Scrollbar = pal.BorderHover.Over(pal.Background)
	t.ScrollbarWidth = ScrollbarWidth
	t.Radius = RadiusLG
	t.Spacing = SpaceUnit
	t.FontSize = FontBase
	c.SetTheme(&t)
}
