package app

import (
	"fmt"
	"path/filepath"
	goruntime "runtime"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"

	"keencode/internal/runtime"
	"keencode/internal/ui/kit"
	"keencode/internal/ui/theme"
)

// The application shell follows WorkspaceShellLayout: a 264px sidebar,
// transparent 4px resize slot and inset, bordered bg-background panel.
// macOS pre-Tahoe uses a 6px panel radius; Windows uses 5px. The sidebar's
// macOS vibrancy and adaptive collapse remain listed as visual differences.
// Hidden titlebar controls overlay 48px drag bands in both columns.

// sidebarWidth is the default sidebar width (WORKSPACE_SIDEBAR_DEFAULT_
// WIDTH_PX, zcode-shell-specs.md §1.2).
const sidebarWidth float32 = 264

// headerHeight is the header row height (h-12, zcode-shell-specs.md §1.4).
const headerHeight float32 = 48

// dragBandHeight is the height of the in-app window-drag strips (h-12,
// zcode-shell-specs.md §1.2 item 1 and §1.4). The hidden-title-bar window
// controls never take more room than this band.
const dragBandHeight float32 = 48

// sidebarView renders the left panel: the drag band, the new-task row and
// the session list.
func (a *App) sidebarView(c *ui.Context) {
	pal := kit.P(c)
	sidebar := ui.Column(c).
		Width(sidebarWidth).
		Shrink(0).
		FillHeight().
		Background(pal.Sidebar).Label("左侧导航")
	sidebar.Children(func() {
		// Top drag band under the window controls (§1.2 item 1: h-12
		// [app-region:drag]). It carries no content, so the traffic lights
		// overlay it freely.
		ui.Row(c).Height(max(dragBandHeight, c.TitleBar().Height)).FillWidth().Padding(0, 8, 0, 120).AlignItems(ui.Center).Justify(ui.End).DragWindow().Children(func() {
			btn := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconPanelLeft).Label("收起侧栏").Tooltip("收起侧栏")
			if btn.Clicked() {
				a.navigation.sidebarHidden = true
			}
		})
		ui.Column(c).FillWidth().Padding(12, 8).Children(func() { a.newSessionRow(c) })
		ui.Scroll(c).Grow(1).MinHeight(0).Children(func() {
			list := ui.Column(c).FillWidth().Padding(0, theme.SpaceUnit*2).Gap(2)
			list.Children(func() {
				a.navigationList(c)
			})
		})
		a.sidebarFooter(c)
	})
}

// sidebarFooter is the bottom row of the sidebar (WorkspaceSidebarFooter
// reduced to the v1 entry points): the settings gear on the left.
func (a *App) sidebarFooter(c *ui.Context) {
	footer := ui.Row(c).FillWidth().Padding(8, 16, 12, 16).AlignItems(ui.Center)
	footer.Children(func() {
		gear := ui.ButtonBase(c).FillWidth().Height(32).Gap(8).Justify(ui.Start).Radius(theme.RadiusLG).Label(SettingsGearLabel)
		if gear.Hovered() {
			gear.Background(kit.P(c).SurfaceHover.Over(kit.P(c).Sidebar))
		}
		gear.Children(func() { kit.Icon(c, kit.IconGear, 16); ui.Text(c, "设置") })
		if gear.Clicked() {
			a.view = "settings"
		}
	})
}

// newSessionRow is the top action of the sidebar (NewTaskButtonGroup.tsx:
// 32-44): a full-width ghost row with a plus icon and the label; clicking
// enters the draft state instead of creating a session.
func (a *App) newSessionRow(c *ui.Context) {
	pal := kit.P(c)
	row := ui.ButtonBase(c).
		FillWidth().
		Height(32).
		Radius(theme.RadiusLG).
		Padding(0, 10).
		Gap(theme.SpaceUnit * 2).
		AlignItems(ui.Center).
		Justify(ui.Start)
	face := pal.Foreground
	if row.Hovered() {
		row.Background(pal.SurfaceHover.Over(pal.Sidebar))
	}
	row.TextColor(face)
	row.Children(func() {
		kit.Icon(c, kit.IconPlus, 16).Shrink(0)
		ui.Text(c, NewSessionButton).FontSize(theme.FontBase).SingleLine()
	})
	if row.Clicked() {
		a.startNewDraft(c)
	}
}

// sessionRow renders one entry of the session list (TaskListItem.tsx:
// 542-570): a 32px rounded row with the title, a relative timestamp, a
// spinner while its turn runs, and the rename/delete entries behind the
// context menu. Selected rows carry the selected tint, hovered rows the
// surface-hover tint; both pre-composited over the sidebar background.
func (a *App) sessionRow(c *ui.Context, meta runtime.SessionMeta) {
	pal := kit.P(c)
	selected := meta.ID == a.activeID
	running := false
	if st := a.views[meta.ID]; st != nil {
		running = st.running
	}

	row := ui.ButtonBase(c).
		FillWidth().
		Height(32).
		Shrink(0).
		Radius(theme.RadiusLG).
		Padding(0, 10).
		Gap(theme.SpaceUnit * 2).
		AlignItems(ui.Center)
	row.Label(a.sessionTitle(meta))
	switch {
	case selected:
		row.Background(pal.Selected.Over(pal.Sidebar))
	case row.Hovered():
		row.Background(pal.SurfaceHover.Over(pal.Sidebar))
	}
	row.TextColor(pal.Foreground)
	row.Children(func() {
		if running {
			kit.Spinner(c, 14).Shrink(0)
		}
		ui.Text(c, a.sessionTitle(meta)).
			FontSize(theme.FontBase).
			Grow(1).
			MinWidth(0).
			SingleLine()
		ui.Text(c, relativeTime(meta.UpdatedAt)).
			FontSize(theme.FontSM).
			TextColor(pal.ForegroundSubtle).
			Shrink(0)
	})
	// 重命名 / 删除 live in the context menu (TaskList.tsx:442-467); the
	// dialogs themselves open from the menu choice in dialogsView.
	menu := kit.SessionRowMenu(
		func() {
			a.renameID = meta.ID
			a.renameOpen = true
		},
		func() {
			a.deleteID = meta.ID
			a.deleteOpen = true
		},
	)
	kit.ContextMenu(row, func(m *ui.Menu) {
		label := "置顶对话"
		if meta.Pinned {
			label = "取消置顶"
		}
		if m.Item(label).Chosen() {
			a.pinSession(c, meta)
		}
		m.Separator()
		menu(m)
	})
	if row.Clicked() {
		a.openSession(c, meta.ID)
	}
}

// mainView renders the middle conversation on bg-background.
func (a *App) mainView(c *ui.Context) {
	pal := kit.P(c)
	// Desktop ZCode uses p-1 pl-0 with a 4px top drag strip. macOS
	// pre-Tahoe/unknown uses rounded-[6px]; Windows uses rounded-[5px].
	outer := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Padding(theme.SpaceUnit, theme.SpaceUnit, theme.SpaceUnit, 0)
	outer.Children(func() {
		radius := theme.RadiusMD
		if goruntime.GOOS == "windows" {
			radius = theme.RadiusPanelWindows
		}
		main := ui.Column(c).Fill().Radius(radius).Border(1, pal.Border).Background(pal.Background).Clip().Label("对话面板")
		main.Children(func() {
			a.headerView(c)
			a.chatView(c)
		})
	})
}

// headerView renders the 48px top bar (WorkspaceHeader.tsx:139-157): the
// project directory on the left — with the chooser button in the draft
// state — and the session title once a conversation is open. The bar is a
// window-drag strip ([app-region:drag], §1.4); interactive children keep
// their clicks because a drag target only claims presses that land on
// non-interactive ground (mygo ui/input.go).
func (a *App) headerView(c *ui.Context) {
	pal := kit.P(c)
	width, _ := c.Size()
	left := theme.SpaceUnit * 2
	if !a.navigationVisible(width) && c.TitleBar().Left > 0 {
		// Hidden title-bar controls now overlay the conversation. Reserve
		// their native bounds plus the same drag-band room as the sidebar.
		left = max(120, c.TitleBar().Left+theme.SpaceUnit*2)
	}
	row := ui.Row(c).
		FillWidth().
		Height(headerHeight).
		Shrink(0).
		Padding(0, theme.SpaceUnit*2, 0, left).
		Gap(theme.SpaceUnit * 2).
		AlignItems(ui.Center).
		DragWindow()
	row.Children(func() {
		if !a.navigationVisible(width) {
			btn := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconPanelLeft).Label("展开侧栏").Tooltip("展开侧栏")
			if btn.Clicked() {
				a.navigation.sidebarHidden = false
				if width < 1080 {
					a.inspector.hidden = true
				}
			}
		}
		if a.activeID == "" {
			choose := kit.Button(c, kit.VariantOutline, kit.SizeSM, ChooseProjectButton)
			if choose.Clicked() {
				a.beginChooseProject()
			}
			a.projectLabel(c, a.draftDir)
		} else if st := a.activeView(); st != nil {
			a.projectLabel(c, st.projectDir)
			title := a.sessionTitle(runtime.SessionMeta{Title: st.title})
			ui.Text(c, title).
				FontSize(theme.FontBase).
				FontWeight(theme.WeightMedium).
				TextColor(pal.Foreground).
				SingleLine().
				Grow(1).
				MinWidth(0)
		}
		ui.Box(c).Grow(1).MinWidth(0)
		if a.activeID != "" {
			pin := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconPin).Label("置顶当前对话").Tooltip("置顶 / 取消置顶")
			for _, meta := range a.sessions {
				if meta.ID == a.activeID {
					if meta.Pinned {
						pin.Background(pal.Selected.Over(pal.Background))
					}
					if pin.Clicked() {
						a.pinSession(c, meta)
					}
					break
				}
			}
		}
		if !a.inspectorVisible(width) {
			btn := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconPanelRight).Label("展开右侧面板").Tooltip("展开右侧面板")
			if btn.Clicked() {
				a.inspector.hidden = false
				a.inspector.requested = true
			}
		}
	})
	if a.activeID != "" {
		ui.Box(c).FillWidth().Height(1).Shrink(0).Background(pal.Border.Alpha(0.5))
	}
}

// projectLabel renders the current project directory: a file icon, the
// base name (full path on hover), the placeholder copy when unset.
func (a *App) projectLabel(c *ui.Context, dir string) {
	pal := kit.P(c)
	if strings.TrimSpace(dir) == "" {
		ui.Row(c).Gap(theme.SpaceUnit * 2).AlignItems(ui.Center).Children(func() {
			kit.Icon(c, kit.IconFile, 14).TextColor(pal.ForegroundSubtlest).Shrink(0)
			ui.Text(c, NoProjectHint).
				FontSize(theme.FontBase).
				TextColor(pal.ForegroundSubtlest).
				SingleLine().
				Grow(1).
				MinWidth(0)
		})
		return
	}
	ui.Row(c).Gap(theme.SpaceUnit * 2).AlignItems(ui.Center).Grow(1).MinWidth(0).Children(func() {
		kit.Icon(c, kit.IconFile, 14).TextColor(pal.ForegroundSubtle).Shrink(0)
		ui.Text(c, filepath.Base(dir)).
			FontSize(theme.FontBase).
			TextColor(pal.ForegroundSubtle).
			SingleLine().
			Grow(1).
			MinWidth(0).
			Tooltip(dir)
	})
}

// relativeTime renders the sidebar timestamp (TaskListItem.tsx:746-772):
// 刚刚 below a minute, then minutes, hours, days.
func relativeTime(t time.Time) string {
	if t.IsZero() {
		return ""
	}
	elapsed := time.Since(t)
	switch {
	case elapsed < time.Minute:
		return JustNow
	case elapsed < time.Hour:
		return fmt.Sprintf("%d 分钟前", int(elapsed.Minutes()))
	case elapsed < 24*time.Hour:
		return fmt.Sprintf("%d 小时前", int(elapsed.Hours()))
	default:
		return fmt.Sprintf("%d 天前", int(elapsed.Hours()/24))
	}
}
