package app

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"strings"

	"github.com/egoist/mygo/ui"

	"keencode/internal/config"
	"keencode/internal/ui/kit"
	"keencode/internal/ui/theme"
)

// The settings page (ask item 1; zcode-shell-specs.md §3 visual language):
// a two-column frame — 268px nav with a back button and the 通用 / 供应商
// entries — over a bordered rounded panel with a 48px breadcrumb row and a
// centered scroll column of grouped cards. Rows carry the label left and
// the control right, separated by hairlines. The narrow 68px icon rail of
// the original collapses below the wide breakpoint; v1 always renders the
// wide nav (recorded simplification).

// settingsNavWidth is the wide nav width (268px, SettingsPage.tsx:1373).
const settingsNavWidth float32 = 268

// settingsPanelInset is the desktop panel's 4px inset around the rounded
// panel (zcode-shell-specs.md §1.1 hasDesktopPanelInset).
const settingsPanelInset float32 = 4

// settingsContentWidth is the content column cap (max-w-4xl, §3).
const settingsContentWidth float32 = 896

// settingsView renders the settings page: nav over panel.
func (a *App) settingsView(c *ui.Context) {
	ui.Row(c).Fill().AlignItems(ui.Stretch).Children(func() {
		a.settingsNav(c)
		a.settingsPanel(c)
	})
}

// settingsNav renders the left column: the back button and the section
// entries (SettingsPage.tsx:1400-1500: ghost lg rounded-xl back, h-8
// rounded-xl entries, active bg-surface-hover).
func (a *App) settingsNav(c *ui.Context) {
	pal := kit.P(c)
	nav := ui.Column(c).Width(settingsNavWidth).Shrink(0).FillHeight().Background(pal.Sidebar)
	nav.Children(func() {
		ui.Box(c).Height(max(dragBandHeight, c.TitleBar().Height)).DragWindow()
		back := ui.ButtonBase(c).
			FillWidth().
			Height(36).
			Radius(theme.RadiusXL).
			Padding(0, 10).
			Gap(theme.SpaceUnit * 2).
			AlignItems(ui.Center)
		if back.Hovered() {
			back.Background(pal.SurfaceHover.Over(pal.Sidebar))
		}
		back.TextColor(pal.Foreground)
		back.Children(func() {
			kit.Icon(c, kit.IconChevronRight, 16).Rotate(180).Shrink(0)
			ui.Text(c, SettingsBack).FontSize(theme.FontBase)
		})
		back.Label(SettingsBack)
		if back.Clicked() {
			a.view = "chat"
		}
		ui.Box(c).Height(theme.SpaceUnit * 2)
		a.settingsNavItem(c, SettingsTabGeneral, "general")
		a.settingsNavItem(c, SettingsTabProviders, "providers")
	})
}

// settingsNavItem renders one nav entry of the settings page.
func (a *App) settingsNavItem(c *ui.Context, label, tab string) {
	pal := kit.P(c)
	item := ui.ButtonBase(c).
		FillWidth().
		Height(32).
		Radius(theme.RadiusXL).
		Padding(0, 10).
		AlignItems(ui.Center).
		Justify(ui.Start)
	switch {
	case a.settingsTab == tab:
		item.Background(pal.SurfaceHover.Over(pal.Sidebar))
	case item.Hovered():
		item.Background(pal.Hover.Over(pal.Sidebar))
	}
	item.TextColor(pal.Foreground)
	item.Children(func() {
		ui.Text(c, label).FontSize(theme.FontBase).SingleLine()
	})
	if item.Clicked() {
		a.settingsTab = tab
	}
}

// settingsPanel renders the right column: the inset rounded panel with a
// breadcrumb row and the scrollable content column (SettingsPage.tsx:
// 1554-1667).
func (a *App) settingsPanel(c *ui.Context) {
	pal := kit.P(c)
	outer := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Padding(settingsPanelInset, settingsPanelInset, settingsPanelInset, 0) // p-1 pl-0
	outer.Children(func() {
		panel := ui.Column(c).Fill().Radius(theme.RadiusXL).Border(1, pal.Border).Background(pal.Background)
		panel.Children(func() {
			title := SettingsGeneralTitle
			if a.settingsTab == "providers" {
				title = SettingsProvidersTitle
			}
			ui.Row(c).FillWidth().Height(48).Shrink(0).Padding(0, 16).AlignItems(ui.Center).Children(func() {
				ui.Text(c, title).
					FontSize(theme.FontBase).
					FontWeight(theme.WeightMedium).
					TextColor(pal.Foreground).
					SingleLine()
			})
			ui.Box(c).Grow(1).MinHeight(0).Children(func() {
				ui.Scroll(c).Fill().Children(func() {
					content := ui.Column(c).FillWidth().MaxWidth(settingsContentWidth).Margin(0, ui.Auto, 0, ui.Auto).
						Padding(0, 32, 40, 32). // px-8 pb-10
						Gap(32)                 // gap-8 between cards
					content.Children(func() {
						if a.settingsTab == "providers" {
							a.providersTab(c)
						} else {
							a.generalTab(c)
						}
					})
				})
			})
		})
	})
}

// settingsCard is one grouped card (SettingsGroupCard: rounded-xl border
// bg-card); the rows arrive through build.
func settingsCard(c *ui.Context, build func()) *ui.Element {
	pal := kit.P(c)
	card := ui.Column(c).FillWidth().Radius(theme.RadiusXL).Border(1, pal.Border).Background(pal.Card)
	card.Children(build)
	return card
}

// settingsRow is one labeled form row (SettingsRow: px-4 py-3, label +
// description left, control right-aligned in a fixed column).
func settingsRow(c *ui.Context, label, desc string, control func()) {
	pal := kit.P(c)
	row := ui.Row(c).FillWidth().Padding(12, 16).Gap(16).AlignItems(ui.Center)
	row.Children(func() {
		text := ui.Column(c).Grow(1).MinWidth(0).Gap(4)
		text.Children(func() {
			ui.Text(c, label).FontSize(theme.FontBase).FontWeight(theme.WeightMedium).TextColor(pal.Foreground).SingleLine()
			ui.Text(c, desc).FontSize(theme.FontBase).TextColor(pal.ForegroundSubtle)
		})
		control()
	})
}

// settingsSelect renders an lg select of fixed width inside the control
// column (the trigger stretches to the wrapper column's width).
func settingsSelect(c *ui.Context, width float32, sel *string, items []kit.SelectItem[string]) {
	pal := kit.P(c)
	wrap := ui.Column(c).Width(width).Shrink(0).AlignItems(ui.End)
	wrap.TextColor(pal.Foreground)
	wrap.Children(func() {
		kit.Select(c, sel, items, kit.SelectOptions{LG: true})
	})
}

// generalTab is the 通用 settings card: theme, default model, tool
// permission policy, current project.
func (a *App) generalTab(c *ui.Context) {
	settingsCard(c, func() {
		settingsRow(c, ThemeRowLabel, ThemeRowDesc, func() {
			items := []kit.SelectItem[string]{
				{Value: string(config.ThemeSystem), Label: ThemeSystem},
				{Value: string(config.ThemeLight), Label: ThemeLight},
				{Value: string(config.ThemeDark), Label: ThemeDark},
			}
			settingsSelect(c, 200, &a.themeSel, items)
			a.applyThemeSelection(c)
		})
		kit.Divider(c)
		settingsRow(c, DefaultModelRowLabel, DefaultModelRowDesc, func() {
			settingsSelect(c, 320, &a.modelSel, a.modelItems)
			a.applyModelSelection(c)
		})
		kit.Divider(c)
		settingsRow(c, PolicyRowLabel, PolicyRowDesc, func() {
			items := []kit.SelectItem[string]{
				{Value: string(config.ToolPermissionAsk), Label: PolicyAsk},
				{Value: string(config.ToolPermissionAllowAll), Label: PolicyAllowAll},
				{Value: string(config.ToolPermissionReadOnly), Label: PolicyReadOnly},
			}
			settingsSelect(c, 200, &a.policySel, items)
			a.applyPolicySelection(c)
		})
		kit.Divider(c)
		desc := a.draftDir
		if strings.TrimSpace(desc) == "" {
			desc = ProjectUnset
		}
		settingsRow(c, ProjectRowLabel, desc, func() {
			choose := kit.Button(c, kit.VariantOutline, kit.SizeSM, ChooseProjectButton)
			if choose.Clicked() {
				a.beginChooseProject()
			}
		})
	})
}

// applyThemeSelection persists a theme select change and applies it to the
// desktop when the app runs (theme.SetTheme runs inline on the main
// thread, mygo loop.go:87-91; without a bound window — headless
// assemblies — only the store is written).
func (a *App) applyThemeSelection(c *ui.Context) {
	if a.themeSel == a.themeApplied || a.themeSel == a.themeFailed {
		return
	}
	if err := a.svcs.Settings.SetTheme(config.Theme(a.themeSel)); err != nil {
		a.themeFailed = a.themeSel
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	a.themeApplied = a.themeSel
	a.themeFailed = ""
	if a.win.Load() != nil {
		theme.SetTheme(theme.Setting(a.themeSel))
	}
}

// applyPolicySelection persists a tool-permission policy change.
func (a *App) applyPolicySelection(c *ui.Context) {
	if a.policySel == a.policyApplied || a.policySel == a.policyFailed {
		return
	}
	if err := a.svcs.Settings.SetToolPermissionPolicy(config.ToolPermissionPolicy(a.policySel)); err != nil {
		a.policyFailed = a.policySel
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	a.policyApplied = a.policySel
	a.policyFailed = ""
}

// providersTab is the 供应商 card: the create action over the provider
// list (or the empty hint).
func (a *App) providersTab(c *ui.Context) {
	pal := kit.P(c)
	add := kit.Button(c, kit.VariantPrimary, kit.SizeSM, AddProviderButton)
	if add.Clicked() {
		a.openProviderForm("")
	}
	state := a.svcs.Settings.Providers()
	if len(state.Providers) == 0 {
		ui.Row(c).FillWidth().Justify(ui.Center).Padding(24, 0).Children(func() {
			ui.Text(c, ProviderEmptyHint).
				FontSize(theme.FontBase).
				TextColor(pal.ForegroundSubtlest)
		})
		return
	}
	settingsCard(c, func() {
		for i, record := range state.Providers {
			if i > 0 {
				kit.Divider(c)
			}
			a.providerRow(c, record)
		}
	})
}

// providerRow renders one provider of the management list: identity block
// left, edit and delete actions right.
func (a *App) providerRow(c *ui.Context, record config.ProviderRecord) {
	pal := kit.P(c)
	row := ui.Row(c).FillWidth().Padding(12, 16).Gap(12).AlignItems(ui.Center)
	row.Children(func() {
		info := ui.Column(c).Grow(1).MinWidth(0).Gap(4)
		info.Children(func() {
			ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
				ui.Text(c, record.Name).
					FontSize(theme.FontBase).
					FontWeight(theme.WeightMedium).
					TextColor(pal.Foreground).
					SingleLine()
				ui.Text(c, providerProtocolLabel(string(record.APIBackend))).
					FontSize(theme.FontSM).
					TextColor(pal.ForegroundSubtlest).
					SingleLine()
			})
			ui.Text(c, record.BaseURL).
				FontSize(theme.FontSM).
				TextColor(pal.ForegroundSubtle).
				SingleLine().
				Grow(1).
				MinWidth(0)
			ui.Text(c, fmt.Sprintf(ProviderModelsCount, len(record.Models))).
				FontSize(theme.FontSM).
				TextColor(pal.ForegroundSubtlest)
		})
		edit := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconPencil)
		edit.Label(ProviderEditLabel).Tooltip(ProviderEditLabel)
		if edit.Clicked() {
			a.openProviderForm(record.ID)
		}
		del := kit.IconButton(c, kit.VariantGhost, kit.SizeIconSM, kit.IconTrash)
		del.Label(ProviderDeleteLabel).Tooltip(ProviderDeleteLabel)
		if del.Clicked() {
			a.providerDeleteID = record.ID
			a.providerDeleteOpen = true
		}
	})
}

// providerProtocolLabel maps the persisted protocol value to its label.
func providerProtocolLabel(proto string) string {
	switch config.Protocol(proto) {
	case config.ProtocolChatCompletions:
		return ProviderFormChat
	default:
		return ProviderFormMessages
	}
}

// openProviderForm prefills the editor form: empty fields for a new
// provider, the record's values for an edit.
func (a *App) openProviderForm(id string) {
	a.providerFormID = id
	if id == "" {
		a.providerName, a.providerURL, a.providerKey, a.providerModels = "", "", "", ""
		a.providerProto = string(config.ProtocolMessages)
	} else {
		record, ok := a.svcs.Settings.Providers().Provider(id)
		if !ok {
			return
		}
		a.providerName = record.Name
		a.providerURL = record.BaseURL
		if key, ok := record.AuthKey(); ok {
			a.providerKey = key
		} else {
			a.providerKey = ""
		}
		a.providerModels = strings.Join(record.Models, ", ")
		a.providerProto = string(record.APIBackend)
	}
	a.providerFormOpen = true
}

// providerFormDialog is the create/edit dialog: labeled fields over the
// input row controls, 保存 on the right. A failed validation toasts and
// reopens the dialog (the field buffers keep their values).
func (a *App) providerFormDialog(c *ui.Context) {
	if !a.providerFormOpen {
		return
	}
	title := ProviderFormCreateTitle
	if a.providerFormID != "" {
		title = ProviderFormEditTitle
	}
	result := kit.Dialog(c, &a.providerFormOpen, kit.DialogOptions{
		Title:        title,
		ConfirmLabel: ProviderFormSave,
		Body: func() {
			form := ui.Column(c).FillWidth().Gap(12)
			form.Children(func() {
				formField(c, ProviderFormName, func() {
					kit.TextField(c, &a.providerName, ProviderFormNamePlaceholder)
				})
				formField(c, ProviderFormProto, func() {
					kit.Select(c, &a.providerProto, []kit.SelectItem[string]{
						{Value: string(config.ProtocolMessages), Label: ProviderFormMessages},
						{Value: string(config.ProtocolChatCompletions), Label: ProviderFormChat},
					})
				})
				formField(c, ProviderFormURL, func() {
					kit.TextField(c, &a.providerURL, ProviderFormURLPlaceholder)
				})
				formField(c, ProviderFormKey, func() {
					kit.TextField(c, &a.providerKey, ProviderFormKeyPlaceholder)
				})
				formField(c, ProviderFormModels, func() {
					kit.TextField(c, &a.providerModels, ProviderFormModelsPlaceholder)
				})
			})
		},
	})
	if result != kit.DialogConfirm {
		return
	}
	a.saveProviderForm(c)
}

// formField renders one label-over-control field of the provider form.
func formField(c *ui.Context, label string, control func()) {
	pal := kit.P(c)
	field := ui.Column(c).FillWidth().Gap(4)
	field.Children(func() {
		ui.Text(c, label).FontSize(theme.FontSM).FontWeight(theme.WeightMedium).TextColor(pal.ForegroundSubtle)
		control()
	})
}

// saveProviderForm validates and persists the form. The shipped config
// validation (NewProviderRecord) rejects bad URLs, unknown protocols and
// empty model lists; failures keep the dialog open.
func (a *App) saveProviderForm(c *ui.Context) {
	id := a.providerFormID
	if id == "" {
		id = newProviderID()
	}
	var apiKey *string
	if key := strings.TrimSpace(a.providerKey); key != "" {
		apiKey = &key
	}
	record, err := config.NewProviderRecord(
		id,
		strings.TrimSpace(a.providerName),
		strings.TrimSpace(a.providerURL),
		config.Protocol(a.providerProto),
		splitModelList(a.providerModels),
		apiKey,
	)
	if err != nil {
		a.providerFormOpen = true
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	if err := a.svcs.Settings.UpsertProvider(record); err != nil {
		a.providerFormOpen = true
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	a.refreshModelItems()
	toast(c, kit.ToastDefault, ProviderSavedToast)
}

// newProviderID generates a stable identifier for a new provider record
// (the form has no ID field; the ID is opaque and never shown).
func newProviderID() string {
	var buf [6]byte
	if _, err := rand.Read(buf[:]); err != nil {
		return fmt.Sprintf("p-%d", len(buf)) // unreachable in practice
	}
	return "p-" + hex.EncodeToString(buf[:])
}

// splitModelList splits the model list field on commas (ASCII and full
// width), whitespace and newlines, dropping empties and duplicates in
// order.
func splitModelList(raw string) []string {
	seen := map[string]bool{}
	var out []string
	for _, part := range strings.FieldsFunc(raw, func(r rune) bool {
		return r == ',' || r == '，' || r == '、' || r == '\n' || r == ' ' || r == '\t'
	}) {
		model := strings.TrimSpace(part)
		if model == "" || seen[model] {
			continue
		}
		seen[model] = true
		out = append(out, model)
	}
	return out
}

// providerDeleteDialog is the destructive confirm of the provider list.
func (a *App) providerDeleteDialog(c *ui.Context) {
	if !a.providerDeleteOpen {
		return
	}
	result := kit.Confirm(c, &a.providerDeleteOpen, kit.ConfirmOptions{
		Title:        ProviderDeleteTitle,
		Description:  ProviderDeleteDesc,
		ConfirmLabel: DeleteConfirm,
		Destructive:  true,
	})
	if result != kit.DialogConfirm {
		return
	}
	if err := a.svcs.Settings.DeleteProvider(a.providerDeleteID); err != nil {
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	a.refreshModelItems()
}
