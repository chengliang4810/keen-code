package app

import (
	"path/filepath"
	"slices"

	"github.com/egoist/mygo/ui"
	"keencode/internal/config"
	"keencode/internal/runtime"
	"keencode/internal/ui/kit"
	"keencode/internal/ui/theme"
	"keencode/internal/workspace"
)

// Navigation is a projection of registered projects and session metadata.
// Source: ZCode WorkspaceSidebar/WorkspacePinnedTasksSection and
// WorkspacePurposeSection at 872ad960 (Apache-2.0; see zcode-shell-specs).
type navigationState struct {
	projects      []string
	closed        map[string]bool
	sidebarHidden bool
}

func (a *App) loadNavigation() {
	a.navigation.closed = make(map[string]bool)
	state, err := a.svcs.Settings.store.LoadWorkspaces()
	if err != nil {
		a.pendingToast = "无法读取项目列表：" + err.Error()
		return
	}
	a.navigation.projects = state.Projects
}

func (a *App) registerProject(dir string) error {
	real, err := workspace.CanonicalDirectory(dir)
	if err != nil {
		return err
	}
	if slices.Contains(a.navigation.projects, real) {
		return nil
	}
	projects := append(slices.Clone(a.navigation.projects), real)
	if err := a.svcs.Settings.store.SaveWorkspaces(config.Workspaces{Projects: projects}); err != nil {
		return err
	}
	a.navigation.projects = projects
	return nil
}

func (a *App) removeProject(c *ui.Context, dir string) {
	projects := slices.DeleteFunc(slices.Clone(a.navigation.projects), func(p string) bool { return p == dir })
	if err := a.svcs.Settings.store.SaveWorkspaces(config.Workspaces{Projects: projects}); err != nil {
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	a.navigation.projects = projects
}

func (a *App) selectProject(c *ui.Context, dir string) {
	real, err := workspace.CanonicalDirectory(dir)
	if err != nil {
		a.pendingToast = err.Error()
		return
	}
	dir = real
	a.startNewDraft(c)
	a.draftDir = dir
	a.svcs.Sessions.SetDraftProjectDir(dir)
	_ = a.svcs.Sessions.SaveDraft("", a.draftText)
	if err := a.svcs.Settings.SetWorkingDirectory(dir); err != nil {
		toast(c, kit.ToastWarning, err.Error())
	}
}

func (a *App) pinSession(c *ui.Context, meta runtime.SessionMeta) {
	if err := a.svcs.Sessions.SetPinned(meta.ID, !meta.Pinned); err != nil {
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	a.refreshSessions()
}

func (a *App) navigationSection(c *ui.Context, title string, body func()) {
	pal := kit.P(c)
	ui.Column(c).FillWidth().Gap(theme.SpaceUnit).Padding(theme.SpaceUnit, 0, theme.SpaceUnit*2, 0).Children(func() {
		ui.Row(c).FillWidth().AlignItems(ui.Center).Children(func() {
			header := ui.ButtonBase(c).Height(32).Grow(1).Radius(theme.RadiusLG).Padding(0, 10).Gap(8).Justify(ui.Start).TextColor(pal.ForegroundSubtle).Label(title)
			if header.Hovered() {
				header.Background(pal.SurfaceHover.Over(pal.Sidebar))
			}
			header.Children(func() {
				icon := kit.IconChevronDown
				if a.navigation.closed[title] {
					icon = kit.IconChevronRight
				}
				kit.Icon(c, icon, 14)
				ui.Text(c, title).FontSize(theme.FontBase)
			})
			if header.Clicked() {
				a.navigation.closed[title] = !a.navigation.closed[title]
			}
			if title == "项目" {
				add := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconPlus).Label("添加项目").Tooltip("添加项目")
				if add.Clicked() {
					a.beginChooseProject()
				}
			}
		})
		if !a.navigation.closed[title] {
			body()
		}
	})
}

func (a *App) navigationList(c *ui.Context) {
	pal := kit.P(c)
	empty := func(text string) {
		ui.Text(c, text).FontSize(theme.FontSM).TextColor(pal.ForegroundSubtle).Padding(4, 10)
	}
	a.navigationSection(c, "置顶", func() {
		count := 0
		for _, meta := range a.sessions {
			if meta.Pinned {
				a.sessionRow(c, meta)
				count++
			}
		}
		if count == 0 {
			empty("暂无置顶对话")
		}
	})
	a.navigationSection(c, "项目", func() {
		for _, dir := range a.navigation.projects {
			row := ui.ButtonBase(c).Height(32).FillWidth().Radius(theme.RadiusLG).Padding(0, 10).Gap(8).Justify(ui.Start).Label("项目：" + dir).Tooltip(dir)
			if row.Hovered() {
				row.Background(pal.SurfaceHover.Over(pal.Sidebar))
			}
			row.Children(func() {
				icon := kit.IconChevronDown
				if a.navigation.closed[dir] {
					icon = kit.IconChevronRight
				}
				kit.Icon(c, icon, 12)
				kit.Icon(c, kit.IconFolder, 14)
				ui.Text(c, filepath.Base(dir)).FontSize(theme.FontBase).SingleLine().Grow(1).MinWidth(0)
			})
			if row.Clicked() {
				a.navigation.closed[dir] = !a.navigation.closed[dir]
			}
			kit.ContextMenu(row, func(m *ui.Menu) {
				if m.Item("在此项目新建对话").Chosen() {
					a.selectProject(c, dir)
				}
				if m.Item("从列表移除项目").Chosen() {
					a.removeProject(c, dir)
				}
			})
			if !a.navigation.closed[dir] {
				ui.Column(c).FillWidth().Padding(0, 0, 0, theme.SpaceUnit*4).Children(func() {
					count := 0
					for _, meta := range a.sessions {
						if !meta.Pinned && filepath.Clean(meta.ProjectDir) == dir {
							a.sessionRow(c, meta)
							count++
						}
					}
					if count == 0 {
						empty("暂无对话")
					}
					new := kit.Button(c, kit.VariantGhost, kit.SizeSM, "新建对话").Justify(ui.Start).Label("新建项目对话：" + dir)
					if new.Clicked() {
						a.selectProject(c, dir)
					}
				})
			}
		}
		if len(a.navigation.projects) == 0 {
			empty("添加项目后在此显示")
		}
	})
	a.navigationSection(c, "对话", func() {
		count := 0
		for _, meta := range a.sessions {
			if !meta.Pinned && !slices.Contains(a.navigation.projects, filepath.Clean(meta.ProjectDir)) {
				a.sessionRow(c, meta)
				count++
			}
		}
		if count == 0 {
			empty("暂无其他对话")
		}
	})
}
