package app

import (
	"github.com/egoist/mygo/ui"
	"keencode/internal/ui/theme"
)

// ZCode's WorkspaceShellLayout uses sidebar / conversation / side pane.
// At narrow widths explicit side-pane opening replaces the sidebar so that
// the conversation and composer retain usable space. Split supplies pointer
// resizing and accessible arrow-key resizing, rather than a custom dragger.
func (a *App) navigationVisible(width float32) bool {
	return !a.navigation.sidebarHidden && (!a.inspectorVisible(width) || width >= 1080)
}

func (a *App) workbenchView(c *ui.Context) {
	width, _ := c.Size()
	showSide := a.inspectorVisible(width)
	if !showSide && (a.inspector.term != nil || a.inspector.tasks["terminal"]) {
		a.resetInspector(a.projectDirectory())
	}
	showNav := a.navigationVisible(width)
	available := width
	if showNav {
		available -= sidebarWidth + theme.SpaceUnit
	}
	ui.Row(c).Fill().AlignItems(ui.Stretch).Children(func() {
		if showNav {
			a.sidebarView(c)
			ui.Box(c).Width(theme.SpaceUnit).Shrink(0).FillHeight()
		}
		if !showSide {
			a.mainView(c)
			return
		}
		p := &a.inspector
		if p.lastAvailable != available {
			right := inspectorDefaultWidth
			if p.lastAvailable > 0 {
				right = p.lastAvailable - p.centerWidth - 1
			}
			p.centerWidth = available - right - 1
			p.lastAvailable = available
		}
		p.centerWidth = max(conversationMinWidth, min(p.centerWidth, available-inspectorMinWidth-1))
		ui.Split(c, &p.centerWidth, func() { a.mainView(c) }, func() { a.inspectorView(c) }).Grow(1).MinWidth(0).FillHeight().Label("对话与右侧面板分隔")
	})
}
