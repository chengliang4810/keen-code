package app

import (
	"testing"

	"github.com/egoist/mygo/ui"

	"keencode/internal/config"
	"keencode/internal/runtime"
)

// Settings page tests: navigation, the general rows (theme, default
// model, permission policy, working directory), the provider CRUD flow
// over the form dialog, and the startup restore from the journals.

// seedTwoModels persists one provider with two models and the first
// selected.
func seedTwoModels(t *testing.T, svcs *Services) {
	t.Helper()
	record, err := config.NewProviderRecord("p1", "测试供应商", "https://api.test.local/v1", config.ProtocolMessages, []string{"模型一", "模型二"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	state := config.DefaultProvidersState()
	state.Providers = []config.ProviderRecord{record}
	activeProvider, activeModel := "p1", "模型一"
	state.ActiveProviderID = &activeProvider
	state.ActiveModelID = &activeModel
	if err := svcs.Settings.ReplaceProviders(state); err != nil {
		t.Fatal(err)
	}
}

// openSettings drives the sidebar gear into the settings page.
func openSettings(t *testing.T, tt *ui.Tester, a *App) {
	t.Helper()
	if err := tt.Click(SettingsGearLabel); err != nil {
		t.Fatalf("open settings: %v", err)
	}
	if a.view != "settings" {
		t.Fatalf("view = %q, want settings", a.view)
	}
	tt.Frame()
}

// TestSettingsNavigation covers gear → settings → tab switch → back.
func TestSettingsNavigation(t *testing.T) {
	_, a := newTestHarness(t, nil, nil)
	tt := ui.NewTester(a.View, 1100, 720)

	openSettings(t, tt, a)
	if !tt.HasText(SettingsGeneralTitle) {
		t.Errorf("general panel missing: %q", tt.Texts())
	}
	if !tt.HasText(ThemeRowLabel) || !tt.HasText(PolicyRowLabel) || !tt.HasText(ProjectRowLabel) {
		t.Errorf("general rows missing: %q", tt.Texts())
	}
	if err := tt.Click(SettingsTabProviders); err != nil {
		t.Fatalf("open providers tab: %v", err)
	}
	if !tt.HasText(ProviderEmptyHint) {
		t.Errorf("empty provider hint missing: %q", tt.Texts())
	}
	if err := tt.Click(SettingsBack); err != nil {
		t.Fatalf("back: %v", err)
	}
	if a.view != "chat" {
		t.Fatalf("view = %q, want chat", a.view)
	}
	tt.Frame()
	if !tt.HasText(Greeting) {
		t.Error("the chat draft view is not back")
	}
}

// TestSettingsThemeChange covers the theme row: picking 亮色 persists the
// preference (the desktop application needs the running app and is
// skipped headless).
func TestSettingsThemeChange(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	tt := ui.NewTester(a.View, 1100, 720)
	openSettings(t, tt, a)

	if settings, _ := svcs.Settings.Get(); settings.Theme != config.ThemeDark {
		t.Fatalf("precondition: theme = %q, want dark", settings.Theme)
	}
	if err := tt.Click(ThemeDark); err != nil { // the trigger shows the current 暗色
		t.Fatalf("open theme select: %v", err)
	}
	if err := tt.Click(ThemeLight); err != nil {
		t.Fatalf("pick 亮色: %v", err)
	}
	waitFor(t, func() bool {
		settings, _ := svcs.Settings.Get()
		return settings.Theme == config.ThemeLight
	}, "theme persistence")
	if a.themeApplied != string(config.ThemeLight) {
		t.Errorf("applied marker = %q", a.themeApplied)
	}
}

// TestSettingsPolicyChange covers the permission policy row and its
// effect on the Authorize bridge.
func TestSettingsPolicyChange(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	tt := ui.NewTester(a.View, 1100, 720)
	openSettings(t, tt, a)

	if err := tt.Click(PolicyAsk); err != nil { // trigger shows the current 每次询问
		t.Fatalf("open policy select: %v", err)
	}
	if err := tt.Click(PolicyAllowAll); err != nil {
		t.Fatalf("pick 全部允许: %v", err)
	}
	waitFor(t, func() bool {
		settings, _ := svcs.Settings.Get()
		return settings.ToolPermissionPolicy == config.ToolPermissionAllowAll
	}, "policy persistence")
	ask := &askStub{answer: 2}
	svcs.Sessions.runner.setAsk(ask.ask)
	allowed, err := svcs.Sessions.runner.authorize(t.Context(), permRequest("s", "Bash"))
	if err != nil || !allowed {
		t.Fatalf("allow-all authorize = %v, %v; want true", allowed, err)
	}
	if ask.calls != 0 {
		t.Errorf("allow-all opened %d dialogs", ask.calls)
	}
}

// TestSettingsDefaultModelRow covers the default-model row sharing the
// composer's selection state.
func TestSettingsDefaultModelRow(t *testing.T) {
	svcs, a := newTestHarness(t, nil, func(s *Services) { seedTwoModels(t, s) })
	tt := ui.NewTester(a.View, 1100, 720)
	openSettings(t, tt, a)

	if err := tt.Click("测试供应商 / 模型一"); err != nil { // the trigger shows the active model
		t.Fatalf("open model select: %v", err)
	}
	if err := tt.Click("测试供应商 / 模型二"); err != nil {
		t.Fatalf("pick 模型二: %v", err)
	}
	waitFor(t, func() bool {
		if modelID, ok := svcs.Settings.ActiveModelID(); ok {
			return modelID == "模型二"
		}
		return false
	}, "default model persistence")
	if a.modelSel != encodeModelValue("p1", "模型二") {
		t.Errorf("composer selector value = %q, want the shared selection updated", a.modelSel)
	}
}

// TestSettingsProjectPick covers the working-directory row persisting the
// picked directory (the harness fake picker answers a temp dir).
func TestSettingsProjectPick(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	tt := ui.NewTester(a.View, 1100, 720)
	openSettings(t, tt, a)

	if err := tt.Click(ChooseProjectButton); err != nil {
		t.Fatalf("choose project: %v", err)
	}
	waitForPick(t, tt, a)
	waitFor(t, func() bool {
		settings, _ := svcs.Settings.Get()
		return settings.WorkingDirectory == a.draftDir && a.draftDir != ""
	}, "working directory persistence")
}

// TestProviderCRUD drives the full management flow: create via the form
// dialog, edit, delete via the destructive confirm.
func TestProviderCRUD(t *testing.T) {
	svcs, a := newTestHarness(t, nil, nil)
	tt := ui.NewTester(a.View, 1100, 720)
	openSettings(t, tt, a)
	if err := tt.Click(SettingsTabProviders); err != nil {
		t.Fatal(err)
	}

	// Create.
	if err := tt.Click(AddProviderButton); err != nil {
		t.Fatalf("open form: %v", err)
	}
	if !tt.HasText(ProviderFormCreateTitle) {
		t.Fatalf("form dialog missing: %q", tt.Texts())
	}
	if err := tt.Click(ProviderFormNamePlaceholder); err != nil {
		t.Fatal(err)
	}
	tt.Type("本地网关")
	if err := tt.Click(ProviderFormURLPlaceholder); err != nil {
		t.Fatal(err)
	}
	tt.Type("https://gw.test.local/v1")
	if err := tt.Click(ProviderFormKeyPlaceholder); err != nil {
		t.Fatal(err)
	}
	tt.Type("sk-test")
	if err := tt.Click(ProviderFormModelsPlaceholder); err != nil {
		t.Fatal(err)
	}
	tt.Type("m1, m2,  ， m1") // duplicates and full-width commas collapse
	if err := tt.Click(ProviderFormSave); err != nil {
		t.Fatalf("save: %v", err)
	}
	waitFor(t, func() bool {
		state := svcs.Settings.Providers()
		return len(state.Providers) == 1
	}, "provider creation")
	record := svcs.Settings.Providers().Providers[0]
	if record.Name != "本地网关" || record.APIBackend != config.ProtocolMessages {
		t.Errorf("record = %+v", record)
	}
	if len(record.Models) != 2 {
		t.Errorf("models = %q, want [m1 m2]", record.Models)
	}
	if key, ok := record.AuthKey(); !ok || key != "sk-test" {
		t.Errorf("api key = %q, %v", key, ok)
	}
	tt.Frame()
	if !tt.HasText("本地网关") {
		t.Errorf("provider row missing: %q", tt.Texts())
	}

	// Edit: the dialog prefills every field (a prefilled input shows no
	// placeholder, so fields are asserted via the form buffers and the
	// dialog is dismissed); the replace-in-place persistence is covered
	// right after at the service level.
	if err := tt.Click(ProviderEditLabel); err != nil {
		t.Fatalf("open edit: %v", err)
	}
	if !tt.HasText(ProviderFormEditTitle) {
		t.Fatalf("edit dialog missing: %q", tt.Texts())
	}
	if a.providerName != "本地网关" || a.providerURL != "https://gw.test.local/v1" {
		t.Errorf("edit prefill = %q / %q", a.providerName, a.providerURL)
	}
	if a.providerKey != "sk-test" || a.providerModels != "m1, m2" {
		t.Errorf("edit prefill key/models = %q / %q", a.providerKey, a.providerModels)
	}
	if err := tt.Click("取消"); err != nil {
		t.Fatalf("cancel edit: %v", err)
	}

	// Replace-in-place: an upsert over the same id keeps the position and
	// only rewrites the record.
	edited, err := config.NewProviderRecord(record.ID, "改名网关", "https://gw.test.local/v1", config.ProtocolMessages, []string{"m1", "m2", "m3"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := svcs.Settings.UpsertProvider(edited); err != nil {
		t.Fatal(err)
	}
	state := svcs.Settings.Providers()
	if len(state.Providers) != 1 || state.Providers[0].Name != "改名网关" || len(state.Providers[0].Models) != 3 {
		t.Fatalf("after upsert: %+v", state.Providers)
	}

	// Invalid save reopens the dialog with an error toast.
	if err := tt.Click(AddProviderButton); err != nil {
		t.Fatal(err)
	}
	if err := tt.Click(ProviderFormURLPlaceholder); err != nil {
		t.Fatal(err)
	}
	tt.Type("not-a-url")
	if err := tt.Click(ProviderFormSave); err != nil {
		t.Fatal(err)
	}
	waitFor(t, func() bool {
		return len(tt.Announcements()) > 0 && a.providerFormOpen
	}, "invalid form keeps the dialog open")
	if err := tt.Click("取消"); err != nil { // dismiss the broken form
		t.Fatalf("cancel form: %v", err)
	}

	// Delete through the destructive confirm.
	if err := tt.Click(ProviderDeleteLabel); err != nil {
		t.Fatalf("open delete: %v", err)
	}
	if !tt.HasText(ProviderDeleteTitle) {
		t.Fatalf("delete dialog missing: %q", tt.Texts())
	}
	if err := tt.Click("删除"); err != nil {
		t.Fatalf("confirm delete: %v", err)
	}
	waitFor(t, func() bool {
		state := svcs.Settings.Providers()
		return len(state.Providers) == 0
	}, "provider deletion")
	tt.Frame()
	if !tt.HasText(ProviderEmptyHint) {
		t.Errorf("empty hint missing after deletion: %q", tt.Texts())
	}
}

// TestSettingsDeleteActiveFallsBack covers the delete of the active
// provider among several: the validators reject a state with entries but
// no selection, so the first remaining provider takes over.
func TestSettingsDeleteActiveFallsBack(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, nil)
	for _, id := range []string{"p1", "p2"} {
		record, err := config.NewProviderRecord(id, "供应商 "+id, "https://api.test.local/v1", config.ProtocolMessages, []string{"m-" + id}, nil)
		if err != nil {
			t.Fatal(err)
		}
		if err := svcs.Settings.UpsertProvider(record); err != nil {
			t.Fatalf("upsert %s: %v", id, err)
		}
	}
	// The first save selected p1; make p2 active, then delete it.
	if err := svcs.Settings.SetDefaultModel("p2", "m-p2"); err != nil {
		t.Fatal(err)
	}
	if err := svcs.Settings.DeleteProvider("p2"); err != nil {
		t.Fatalf("delete active: %v", err)
	}
	providerID, ok := svcs.Settings.ActiveProviderID()
	if !ok || providerID != "p1" {
		t.Fatalf("active provider after delete = %q, %v; want the fallback p1", providerID, ok)
	}
	if modelID, ok := svcs.Settings.ActiveModelID(); !ok || modelID != "m-p1" {
		t.Fatalf("active model after delete = %q, %v", modelID, ok)
	}
}

// TestStartupRestoreJournal simulates a restart: a first "process"
// converses through a scripted turn; a second state root over the same
// data root restores the session list and replays the journal history.
func TestStartupRestoreJournal(t *testing.T) {
	root := t.TempDir()
	script := []runtime.Event{
		{ID: "r1", Type: runtime.EventTextDelta, Text: "重启前的回答"},
		{ID: "r2", Type: runtime.EventTurnCompleted},
	}
	// Phase 1: the first process.
	store1, err := config.OpenStore(root)
	if err != nil {
		t.Fatal(err)
	}
	svcs1 := NewServices(store1)
	mgr1, err := runtime.OpenManager(root, runtime.ManagerOptions{
		Agent: func() (runtime.TurnRunner, error) { return scriptedRunner{events: script}, nil },
		Model: func() string { return "测试模型" },
	})
	if err != nil {
		t.Fatal(err)
	}
	svcs1.AttachManager(mgr1)
	sess, err := svcs1.Sessions.send("", root, "重启前的消息")
	if err != nil {
		t.Fatal(err)
	}
	id := sess.Meta().ID
	waitFor(t, func() bool { return !svcs1.Sessions.Running(id) }, "phase-1 turn completion")

	// Phase 2: a fresh process over the same data root.
	store2, err := config.OpenStore(root)
	if err != nil {
		t.Fatal(err)
	}
	svcs2 := NewServices(store2)
	mgr2, err := runtime.OpenManager(root, runtime.ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	svcs2.AttachManager(mgr2)
	a2 := Root(svcs2)

	if a2.activeID != id {
		t.Fatalf("restored active session = %q, want %q", a2.activeID, id)
	}
	metas := mustList(t, svcs2)
	if len(metas) != 1 || metas[0].Title != "重启前的消息" {
		t.Fatalf("restored sessions = %+v", metas)
	}
	st := a2.views[id]
	if st == nil {
		t.Fatal("restored projection missing")
	}
	// The turn texts live in the entry sources.
	var texts []string
	for _, entry := range st.entries {
		texts = append(texts, entry.Text)
	}
	if !containsString(texts, "重启前的消息") || !containsString(texts, "重启前的回答") {
		t.Errorf("restored history missing the turn texts: %q", texts)
	}
	if st.running {
		t.Error("a restored finished turn must not be running")
	}
	// The restored session renders (frame without a tester is fine; the
	// projection already holds the entries).
	if len(st.entries) < 2 { // user + assistant
		t.Errorf("restored entries = %d, want >= 2", len(st.entries))
	}
}

// containsString reports whether the slice contains s.
func containsString(list []string, s string) bool {
	for _, item := range list {
		if item == s {
			return true
		}
	}
	return false
}
