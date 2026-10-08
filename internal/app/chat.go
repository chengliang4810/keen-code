package app

import (
	"strings"

	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/kit"
	"keencode/internal/ui/theme"
)

// The chat column (docs/go-migration.md §5.7 chat.go; zcode-chat-specs.md
// §1/§2): the virtualized timeline, the error banner above the composer,
// the multiline editor with the Enter send / Shift+Enter newline contract,
// the send/stop state machine, the model selector, and the modal dialogs
// (rename, delete, error detail).

// chatView renders the chat area of the visible target: the timeline and
// the composer dock. The draft state centers the greeting and the composer
// as one unit inside the timeline (zcode-chat-specs.md §3, via the draft
// branch of kit.ChatList); Esc stops a running turn (docs/go-migration.md
// §6.4 — window-level shortcut, dead while a modal covers the context).
func (a *App) chatView(c *ui.Context) {
	if a.activeID == "" {
		a.draftList.DraftBody = func() { a.composerDock(c, nil) }
		ui.Box(c).Grow(1).MinHeight(0).Children(func() {
			kit.ChatList(c, &a.draftList, nil)
		})
		return
	}
	st := a.activeView()
	if st == nil {
		return
	}
	if st.running && c.Shortcut(0, ui.KeyEscape) {
		a.stopActive(c)
	}
	st.list.Running = st.running
	ui.Box(c).Grow(1).MinHeight(0).Children(func() {
		kit.ChatList(c, &st.list, st.entries)
	})
	a.composerView(c, st)
}

// composerView renders the error banner above the composer dock
// (ChatErrorBanner.tsx:187-207: independent of the input surface).
func (a *App) composerView(c *ui.Context, st *sessionView) {
	if st != nil && st.errSummary != "" {
		ui.Column(c).FillWidth().Shrink(0).Padding(0, theme.SpaceUnit*4).Children(func() {
			kit.ErrorBanner(c, st.errSummary, func() { a.errOpen = true })
		})
	}
	a.composerDock(c, st)
}

// composerDock renders the composer: the single rounded-2xl input shell
// holding the editor and, inside it, the toolbar — model selector left,
// send/stop right (ConversationComposer.tsx:2189-2299 + ChatPromptEditor
// .tsx:346-414). The dock keeps the message column width of the timeline
// and centers itself. In the draft state the dock sits inside the centered
// greeting column, so it carries no extra top padding.
func (a *App) composerDock(c *ui.Context, st *sessionView) {
	draft := &a.draftText
	running := false
	placeholder := PlaceholderNewTask
	dockTop := float32(0)
	if st != nil {
		draft = &st.draft
		running = st.running
		placeholder = PlaceholderFollowUp
		dockTop = theme.SpaceUnit * 3 // pb-4 dock breathing room over the timeline
	}
	dockWidth := theme.ColumnSession
	if st == nil {
		dockWidth = theme.ColumnDraft
	}
	dock := ui.Column(c).
		FillWidth().
		Shrink(0).
		Padding(dockTop, theme.SpaceUnit*4, theme.SpaceUnit*4, theme.SpaceUnit*4)
	dock.Children(func() {
		inner := ui.Column(c).FillWidth().MaxWidth(dockWidth).Margin(0, ui.Auto, 0, ui.Auto)
		inner.Children(func() {
			result := kit.Editor(c, draft, kit.EditorOptions{
				Placeholder: placeholder,
				Disabled:    false, // the draft stays editable while running; only sending is gated (差异 8)
				Toolbar:     func() { a.composerToolbar(c, draft, running) },
			})
			if result.Submitted {
				a.sendActive(c)
			}
		})
	})
}

// composerToolbar is the toolbar row inside the input shell
// (ChatPromptEditor.tsx:388-414): the model cluster left (ModelConfigSelect,
// zcode-chat-specs.md §2.4), the send/stop button right. With no models
// configured the cluster shows the 管理模型 entry so the composer never
// dead-ends (§2.4: the entry stays visible when there is nothing to pick).
func (a *App) composerToolbar(c *ui.Context, draft *string, running bool) {
	row := ui.Row(c).FillWidth().Gap(theme.SpaceUnit * 3).AlignItems(ui.End)
	row.Children(func() {
		cluster := ui.Row(c).Grow(1).MinWidth(0).Gap(theme.SpaceUnit).AlignItems(ui.Center)
		cluster.Children(func() {
			if len(a.modelItems) == 0 {
				manage := kit.Button(c, kit.VariantGhost, kit.SizeSM, ManageModelsLabel)
				manage.Label(ManageModelsLabel).Tooltip(ManageModelsLabel)
				if manage.Clicked() {
					a.view = "settings"
					a.settingsTab = "providers"
				}
				return
			}
			kit.Select(c, &a.modelSel, a.modelItems, kit.SelectOptions{Disabled: running, Ghost: true}).MaxWidth(256)
			a.applyModelSelection(c)
		})
		send := kit.SendButton(c, a.canSend(*draft, running), running).Shrink(0)
		if send.Clicked() {
			if running {
				a.stopActive(c)
			} else {
				a.sendActive(c)
			}
		}
	})
}

// dialogsView renders the modal dialogs of the current frame. They are
// plain frame content (kit dialogs mount their own overlay), evaluated
// after the shell so menu choices and settings actions can open them in
// the same frame.
func (a *App) dialogsView(c *ui.Context) {
	a.renameDialog(c)
	a.deleteDialog(c)
	a.errorDetailDialog(c)
	a.providerFormDialog(c)
	a.providerDeleteDialog(c)
}

// renameDialog is the rename prompt (TaskRenameDialog semantics): lg
// input, Enter confirms, Escape cancels.
func (a *App) renameDialog(c *ui.Context) {
	if !a.renameOpen {
		return
	}
	initial := ""
	for _, meta := range a.sessions {
		if meta.ID == a.renameID {
			initial = meta.Title
			break
		}
	}
	text, result := kit.Prompt(c, &a.renameOpen, kit.PromptOptions{
		Title:        RenameTitle,
		Description:  RenamePromptLabel,
		Initial:      initial,
		ConfirmLabel: RenameConfirm,
	})
	if result != kit.DialogConfirm {
		return
	}
	title := strings.TrimSpace(text)
	if title == "" {
		return
	}
	if err := a.svcs.Sessions.Rename(a.renameID, title); err != nil {
		kit.ShowToast(c, kit.ToastWarning, err.Error())
		return
	}
	if st := a.views[a.renameID]; st != nil {
		st.title = title
	}
	a.listDirty = true
}

// deleteDialog is the destructive delete confirm (ConfirmDialog.tsx with
// confirmVariant destructive).
func (a *App) deleteDialog(c *ui.Context) {
	if !a.deleteOpen {
		return
	}
	result := kit.Confirm(c, &a.deleteOpen, kit.ConfirmOptions{
		Title:        DeleteTitle,
		Description:  DeleteDescription,
		ConfirmLabel: DeleteConfirm,
		Destructive:  true,
	})
	if result != kit.DialogConfirm {
		return
	}
	id := a.deleteID
	if err := a.svcs.Sessions.Delete(id); err != nil {
		kit.ShowToast(c, kit.ToastWarning, err.Error())
		return
	}
	a.closeView(id)
	a.listDirty = true
}

// errorDetailDialog shows the full error text behind the banner's
// 查看详情 (ChatErrorBanner.tsx:257-269: pre block inside a dialog).
func (a *App) errorDetailDialog(c *ui.Context) {
	if !a.errOpen {
		return
	}
	st := a.activeView()
	detail := ""
	if st != nil {
		detail = st.errDetail
		if strings.TrimSpace(detail) == "" {
			detail = st.errSummary
		}
	}
	kit.Dialog(c, &a.errOpen, kit.DialogOptions{
		Title:        ErrorDetailTitle,
		ConfirmLabel: "关闭",
		Body: func() {
			ui.Scroll(c).MaxHeight(360).FillWidth().Children(func() {
				ui.Text(c, detail).
					FontSize(theme.FontSM).
					TextColor(theme.Active(c).ForegroundSubtle)
			})
		},
	})
}
