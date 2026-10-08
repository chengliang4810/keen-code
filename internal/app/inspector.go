package app

import (
	"context"
	"path/filepath"
	"strings"
	"time"

	"github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
	"keencode/internal/ui/kit"
	"keencode/internal/ui/theme"
	"keencode/internal/workspace"
)

const inspectorDefaultWidth float32 = 360
const inspectorMinWidth float32 = 280
const conversationMinWidth float32 = 360

type directoryView struct {
	open    bool
	entries []workspace.Entry
	loaded  bool
	err     string
}
type inspectionResult struct {
	generation      uint64
	key, path, text string
	entries         []workspace.Entry
	changes         workspace.Changes
	term            *terminal.Terminal
	err             error
}
type inspectorState struct {
	tab                         string
	hidden, requested           bool
	centerWidth, lastAvailable  float32
	dir                         string
	owner                       string
	generation                  uint64
	ctx                         context.Context
	cancel                      context.CancelFunc
	results                     chan inspectionResult
	tasks                       map[string]bool
	directories                 map[string]*directoryView
	file, content, previewError string
	changes                     workspace.Changes
	diffLoaded                  bool
	diffError                   string
	term                        *terminal.Terminal
	termError                   string
	termAppearance              *theme.Palette
}

func (a *App) projectDirectory() string {
	if st := a.activeView(); st != nil {
		return st.projectDir
	}
	return a.draftDir
}

func (a *App) resetInspector(dir string) {
	p := &a.inspector
	if p.cancel != nil {
		p.cancel()
	}
	if p.term != nil {
		_ = p.term.Close()
	}
	p.dir = dir
	p.owner = a.activeID
	p.generation++
	p.ctx, p.cancel = context.WithCancel(context.Background())
	if p.results == nil {
		p.results = make(chan inspectionResult, 64)
	}
	p.tasks = map[string]bool{}
	p.directories = map[string]*directoryView{}
	p.file, p.content, p.previewError = "", "", ""
	p.changes, p.diffLoaded, p.diffError = workspace.Changes{}, false, ""
	p.term, p.termError = nil, ""
	p.termAppearance = nil
}

// Close releases event subscriptions, pending inspections and the PTY.
func (a *App) Close() {
	a.flushDraft()
	if a.inspector.cancel != nil {
		a.inspector.cancel()
	}
	if a.inspector.term != nil {
		_ = a.inspector.term.Close()
	}
	for _, view := range a.views {
		if view.cancelEvents != nil {
			view.cancelEvents()
		}
	}
}

// Workers only return values; state changes happen while building a frame.
// A frame timer exists only while a requested read/start is pending.
func (a *App) inspect(key, path string, work func(context.Context) inspectionResult) {
	p := &a.inspector
	if p.tasks[key] || len(p.tasks) >= 32 {
		return
	}
	p.tasks[key] = true
	ctx, generation, results, win := p.ctx, p.generation, p.results, a.win.Load()
	go func() {
		result := work(ctx)
		result.key, result.path, result.generation = key, path, generation
		if result.term != nil {
			if ctx.Err() != nil {
				_ = result.term.Close()
				return
			}
			go func() { <-ctx.Done(); _ = result.term.Close() }()
		}
		select {
		case results <- result:
			if win != nil {
				win.Update(func() {})
			}
		case <-ctx.Done():
			if result.term != nil {
				_ = result.term.Close()
			}
		}
	}()
}

func (a *App) drainInspections(c *ui.Context) {
	p := &a.inspector
	dir := a.projectDirectory()
	if dir != p.dir || a.activeID != p.owner || p.ctx == nil {
		a.resetInspector(dir)
	}
	for {
		select {
		case r := <-p.results:
			if r.generation != p.generation {
				if r.term != nil {
					_ = r.term.Close()
				}
				continue
			}
			delete(p.tasks, r.key)
			errText := ""
			if r.err != nil {
				errText = r.err.Error()
			}
			switch {
			case strings.HasPrefix(r.key, "dir:"):
				if d := p.directories[r.path]; d != nil {
					d.entries, d.loaded, d.err = r.entries, true, errText
				}
			case strings.HasPrefix(r.key, "file:"):
				if p.file == r.path {
					p.content, p.previewError = r.text, errText
				}
			case r.key == "diff":
				p.changes, p.diffLoaded, p.diffError = r.changes, true, errText
			case r.key == "terminal":
				p.term, p.termError = r.term, errText
			}
		default:
			if len(p.tasks) > 0 {
				c.After(50 * time.Millisecond)
			}
			return
		}
	}
}

func (a *App) inspectorVisible(width float32) bool {
	return !a.inspector.hidden && (width >= 1200 || a.inspector.requested)
}

func (a *App) inspectorView(c *ui.Context) {
	pal := kit.P(c)
	ui.Column(c).Fill().Padding(theme.SpaceUnit, theme.SpaceUnit, theme.SpaceUnit, 0).Label("右侧面板").Children(func() {
		ui.Column(c).Fill().Radius(theme.RadiusMD).Border(1, pal.Border).Background(pal.Background).Clip().Children(func() {
			ui.Row(c).FillWidth().Height(headerHeight).Shrink(0).Padding(0, 8).AlignItems(ui.Center).Gap(2).DragWindow().Children(func() {
				for _, tab := range []struct {
					label string
					icon  kit.IconName
				}{{"终端", kit.IconTerminal}, {"差异", kit.IconFileDiff}, {"文件", kit.IconFolder}} {
					btn := kit.Button(c, kit.VariantGhost, kit.SizeSM, tab.label).Label("面板：" + tab.label)
					if a.inspector.tab == tab.label {
						btn.Background(pal.Selected.Over(pal.Background))
					}
					if btn.Clicked() {
						a.inspector.tab = tab.label
					}
				}
				ui.Box(c).Grow(1)
				close := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconPanelRight).Label("关闭右侧面板").Tooltip("关闭右侧面板")
				if close.Clicked() {
					a.inspector.hidden = true
					a.inspector.requested = false
					a.resetInspector(a.projectDirectory())
				}
			})
			kit.Divider(c)
			if a.inspector.hidden {
				return
			}
			if a.inspector.tab == "" {
				a.inspectorLauncher(c)
				return
			}
			if a.inspector.dir == "" {
				a.inspectorNotice(c, "请先选择项目目录")
				return
			}
			ui.Column(c).Key(a.inspector.tab).Grow(1).MinHeight(0).FillWidth().Children(func() {
				switch a.inspector.tab {
				case "终端":
					a.terminalView(c)
				case "差异":
					a.diffView(c)
				case "文件":
					a.filesView(c)
				}
			})
		})
	})
}

func (a *App) inspectorNotice(c *ui.Context, text string) {
	ui.Column(c).Grow(1).FillWidth().MinHeight(0).Padding(20).Justify(ui.Center).AlignItems(ui.Center).Children(func() {
		ui.Text(c, text).FontSize(theme.FontBase).TextColor(kit.P(c).ForegroundSubtle)
	})
}

func (a *App) inspectorLauncher(c *ui.Context) {
	ui.Column(c).Grow(1).MinHeight(0).FillWidth().Justify(ui.Center).Padding(24).Gap(8).Children(func() {
		ui.Text(c, "打开标签页").FontSize(theme.FontXL).FontWeight(theme.WeightSemibold).AlignSelf(ui.Center)
		ui.Text(c, "查看当前对话的终端、差异和文件。").FontSize(theme.FontSM).TextColor(kit.P(c).ForegroundSubtle).Margin(0, 0, 12, 0)
		for _, item := range []struct {
			label string
			icon  kit.IconName
		}{{"终端", kit.IconTerminal}, {"差异", kit.IconFileDiff}, {"文件", kit.IconFolder}} {
			btn := ui.ButtonBase(c).FillWidth().Height(48).Radius(theme.RadiusXL).Padding(0, 12).Gap(12).Justify(ui.Start).Background(kit.P(c).Surface.Over(kit.P(c).Background)).Label("打开" + item.label)
			if btn.Hovered() {
				btn.Background(kit.P(c).SurfaceHover.Over(kit.P(c).Background))
			}
			btn.Children(func() { kit.Icon(c, item.icon, 16); ui.Text(c, item.label) })
			if btn.Clicked() {
				a.inspector.tab = item.label
			}
		}
	})
}

func (a *App) inspectorToolbar(c *ui.Context, title string, refresh func()) {
	ui.Row(c).FillWidth().Height(36).Shrink(0).Padding(0, 8).AlignItems(ui.Center).Children(func() {
		ui.Text(c, title).FontSize(theme.FontSM).SingleLine().Grow(1).MinWidth(0).TextColor(kit.P(c).ForegroundSubtle)
		label := "刷新" + a.inspector.tab
		if a.inspector.tab == "终端" {
			label = "重启终端"
		}
		btn := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconRefresh).Label(label).Tooltip(label)
		if btn.Clicked() {
			refresh()
		}
	})
}

func (a *App) terminalView(c *ui.Context) {
	p := &a.inspector
	a.inspectorToolbar(c, filepath.Base(p.dir), func() {
		if !p.tasks["terminal"] {
			a.resetInspector(p.dir)
		}
	})
	if p.term == nil && p.termError == "" && !p.tasks["terminal"] {
		dir := p.dir
		a.inspect("terminal", "", func(ctx context.Context) inspectionResult {
			t, err := terminal.New(terminal.Options{Dir: dir, Font: terminal.Font{Size: theme.FontCaption}})
			return inspectionResult{term: t, err: err}
		})
	}
	if p.termError != "" {
		a.inspectorNotice(c, "终端启动失败："+p.termError)
		return
	}
	if p.term == nil {
		a.inspectorNotice(c, "正在启动终端…")
		return
	}
	pal := kit.P(c)
	if p.termAppearance != pal {
		colors := terminal.LightTheme()
		if pal.Dark {
			colors = terminal.DarkTheme()
		}
		colors.Background, colors.Foreground, colors.Selection = pal.Background, pal.Foreground, pal.Selected.Over(pal.Background)
		p.term.SetTheme(colors, nil)
		p.termAppearance = pal
	}
	select {
	case <-p.term.Done():
		ui.Text(c, "终端已退出，可点击重启终端。").FontSize(theme.FontSM).TextColor(pal.ForegroundSubtle).Padding(8)
	default:
	}
	terminal.View(c, p.term).Grow(1).MinHeight(0).FillWidth()
}

func (a *App) diffView(c *ui.Context) {
	p := &a.inspector
	a.inspectorToolbar(c, filepath.Base(p.dir), func() { p.diffLoaded = false })
	if !p.diffLoaded && !p.tasks["diff"] {
		dir := p.dir
		a.inspect("diff", "", func(ctx context.Context) inspectionResult {
			changes, err := workspace.Diff(ctx, dir)
			return inspectionResult{changes: changes, err: err}
		})
	}
	if !p.diffLoaded {
		a.inspectorNotice(c, "正在读取 Git 差异…")
		return
	}
	if p.diffError != "" {
		a.inspectorNotice(c, p.diffError)
		return
	}
	if p.changes.Staged == "" && p.changes.Unstaged == "" && len(p.changes.Untracked) == 0 {
		a.inspectorNotice(c, "工作区没有变更")
		return
	}
	ui.Scroll(c).Grow(1).MinHeight(0).FillWidth().Children(func() {
		ui.Column(c).FillWidth().Padding(12).Gap(8).Children(func() {
			for _, part := range []struct{ label, text string }{{"未暂存", p.changes.Unstaged}, {"已暂存", p.changes.Staged}} {
				if part.text == "" {
					continue
				}
				ui.Text(c, part.label).FontWeight(theme.WeightMedium)
				lines := strings.Split(part.text, "\n")
				for _, line := range lines[:min(len(lines), 2000)] {
					color := kit.P(c).Foreground
					if strings.HasPrefix(line, "+") {
						color = kit.P(c).DiffAdded
					} else if strings.HasPrefix(line, "-") {
						color = kit.P(c).DiffRemoved
					}
					ui.Text(c, line).Font("monospace").FontSize(theme.FontSM).TextColor(color)
				}
				if len(lines) > 2000 {
					ui.Text(c, "仅显示前2000行，请在终端查看完整差异").TextColor(kit.P(c).ForegroundSubtle)
				}
			}
			if len(p.changes.Untracked) > 0 {
				ui.Text(c, "未跟踪文件").FontWeight(theme.WeightMedium)
				for _, path := range p.changes.Untracked {
					btn := kit.Button(c, kit.VariantGhost, kit.SizeSM, path).FillWidth().Justify(ui.Start).Label("查看未跟踪文件：" + path)
					if btn.Clicked() {
						p.tab = "文件"
						a.previewFile(path)
					}
				}
			}
		})
	})
}

func (a *App) loadDirectory(path string) *directoryView {
	p := &a.inspector
	d := p.directories[path]
	if d == nil {
		d = &directoryView{}
		p.directories[path] = d
	}
	if !d.loaded && !p.tasks["dir:"+path] {
		dir := p.dir
		a.inspect("dir:"+path, path, func(ctx context.Context) inspectionResult {
			entries, err := workspace.List(dir, path)
			return inspectionResult{entries: entries, err: err}
		})
	}
	return d
}

func (a *App) previewFile(path string) {
	p := &a.inspector
	p.file, p.content, p.previewError = path, "", ""
	dir := p.dir
	a.inspect("file:"+path, path, func(ctx context.Context) inspectionResult {
		text, err := workspace.Preview(dir, path)
		return inspectionResult{text: text, err: err}
	})
}

func (a *App) filesView(c *ui.Context) {
	p := &a.inspector
	a.inspectorToolbar(c, filepath.Base(p.dir), func() {
		for _, d := range p.directories {
			d.loaded = false
		}
		if p.file != "" {
			a.previewFile(p.file)
		}
	})
	ui.Column(c).Key("file:" + p.file).Grow(1).MinHeight(0).FillWidth().Children(func() { a.fileContent(c) })
}

func (a *App) fileContent(c *ui.Context) {
	p := &a.inspector
	if p.file != "" {
		back := kit.Button(c, kit.VariantGhost, kit.SizeSM, "返回文件列表").Label("返回文件列表")
		if back.Clicked() {
			p.file = ""
			return
		}
		ui.Text(c, p.file).FontSize(theme.FontSM).TextColor(kit.P(c).ForegroundSubtle).Padding(8)
		if p.tasks["file:"+p.file] {
			a.inspectorNotice(c, "正在读取文件…")
			return
		}
		if p.previewError != "" {
			a.inspectorNotice(c, p.previewError)
			return
		}
		ui.Scroll(c).Grow(1).MinHeight(0).FillWidth().Children(func() {
			ui.Text(c, p.content).Font("monospace").FontSize(theme.FontSM).Selectable().Padding(12)
		})
		return
	}
	root := a.loadDirectory(".")
	if root.err != "" {
		a.inspectorNotice(c, root.err)
		return
	}
	ui.Scroll(c).Grow(1).MinHeight(0).FillWidth().Children(func() {
		ui.Tree(c, func() { a.fileTree(c, root, 0) }).FillWidth()
	})
}

func (a *App) fileTree(c *ui.Context, dir *directoryView, depth int) {
	if depth >= 32 {
		ui.Text(c, "目录层级超过32层").TextColor(kit.P(c).ForegroundSubtle)
		return
	}
	if !dir.loaded {
		ui.Text(c, "正在读取目录…").Padding(8)
		return
	}
	if dir.err != "" {
		ui.Text(c, dir.err).TextColor(kit.P(c).ForegroundSubtle)
		return
	}
	for _, entry := range dir.entries {
		if entry.Directory {
			d := a.inspector.directories[entry.Path]
			if d == nil {
				d = &directoryView{}
				a.inspector.directories[entry.Path] = d
			}
			row := ui.TreeItem(c, entry.Name, &d.open, func() { a.fileTree(c, a.loadDirectory(entry.Path), depth+1) }).Label("目录：" + entry.Path)
			if row.Clicked() {
				d.open = !d.open
			}
		} else {
			row := ui.TreeItem(c, entry.Name, nil, nil).Label("文件：" + entry.Path)
			if row.Clicked() {
				a.previewFile(entry.Path)
			}
		}
	}
}
