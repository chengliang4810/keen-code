// Command keencode launches the KeenCode desktop shell: config load →
// runtime creation → services wiring → a native mygo window hosting the
// chat interface, with the light/dark preference and the session journals
// restored from the data root (docs/go-migration.md §5.7).
package main

import (
	"log"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"

	"keencode/internal/app"
	"keencode/internal/config"
	"keencode/internal/runtime"
)

func main() {
	// 1) Data root + config store (~/.keencode/go-v1, KEENCODE_GO_HOME
	// overrides for tests).
	root, err := config.DefaultRoot()
	if err != nil {
		log.Fatalf("定位数据目录：%v", err)
	}
	store, err := config.OpenStore(root)
	if err != nil {
		log.Fatalf("打开配置存储：%v", err)
	}

	// 2) Services before the manager: the manager options must carry the
	// app's agent assembly at OpenManager time (docs/go-migration.md
	// §5.7); the closures late-bind and only run at Send time.
	svcs := app.NewServices(store)
	mgr, err := runtime.OpenManager(root, svcs.RuntimeOptions())
	if err != nil {
		log.Fatalf("打开会话运行时：%v", err)
	}
	svcs.AttachManager(mgr)

	// 3) State root: restores the session list, replays the newest
	// session's journal, and loads the stored new-session draft.
	state := app.Root(svcs)

	mygo.App.WhenReady(func() {
		// 4a) Appearance from the persisted setting, on the main thread
		// before anything renders.
		state.ApplyTheme()
		// 4b) The main window: title and geometry per docs/go-migration.md
		// §5.7; StateKey remembers position and size across restarts. The
		// immersive chrome (TitleBarHiddenInset) puts the traffic lights
		// over the content with no title text — the ZCode window style of
		// zcode-shell-specs.md §1.1/§1.2; the shell draws the drag bands
		// (sidebar top, header row) the hidden title bar hands over.
		win := mygo.NewWindow(mygo.WindowOptions{
			Title:         app.WindowTitle,
			Width:         1280,
			Height:        800,
			MinWidth:      720,
			MinHeight:     480,
			TitleBarStyle: mygo.TitleBarHiddenInset,
			StateKey:      "main",
			Content:       ui.View(state.View),
		})
		// 4c) Bind the window so native dialogs attach to it and event
		// pumps can request frames.
		svcs.BindWindow(win)
		state.BindWindow(win)
		win.OnClosed(state.Close)
	})

	// 5) Run: the main goroutine drives the UI; session turn goroutines
	// deliver events through win.Update (docs/go-migration.md §1.3).
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}
