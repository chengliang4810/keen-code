package app

import (
	"errors"
	"strings"
	"sync/atomic"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"

	"github.com/ZacharyZhang-NY/MujicaUI/core"

	"keencode/internal/runtime"
	"keencode/internal/ui/kit"
	"keencode/internal/ui/theme"
)

// App is the state root of the interface (docs/go-migration.md §5.7
// app.go): it owns the UI projection state and the window, and renders the
// shell — sidebar, header, chat area, composer, dialogs — as one
// state → interface function per frame.
//
// Threading (mygo contract, docs/go-migration.md §1.3): all fields below
// are touched on the main thread only — from the frame build (View), from
// win.Update callbacks posted by the event pumps, or from the synchronous
// fallback before a window is bound (headless tests drive the same code
// paths from the test goroutine). The pump goroutines themselves only
// drain channels and call update.
type App struct {
	svcs *Services
	win  atomic.Pointer[mygo.Window]

	// view routes the window body: "chat" (the shell) or "settings";
	// settingsTab selects the settings nav entry.
	view        string
	settingsTab string // "general" | "providers"

	// sessions is the sidebar snapshot; listDirty defers the disk listing
	// to the next frame.
	sessions  []runtime.SessionMeta
	listDirty bool

	// activeID is the open session ("" = the new-session draft state).
	activeID string
	views    map[string]*sessionView

	// draft state of the new-session view.
	draftText string
	draftDir  string
	draftList kit.ChatListState

	// draft debounce (runtime/draft.go keeps the 500ms debounce in the app
	// layer): saved text, pending flag, and the wake deadline.
	draftSaved       string
	draftPendingSave bool
	draftDeadline    time.Time

	// model selector state: value ("providerID\x1fmodelID"), the item list,
	// the last value persisted to the providers state, and the last value
	// whose persist failed (toast-once guard).
	modelSel     string
	modelItems   []kit.SelectItem[string]
	modelApplied string
	modelFailed  string

	// settings select sync: current value, last persisted value, last
	// value whose persist failed (toast-once guards).
	themeSel      string
	themeApplied  string
	themeFailed   string
	policySel     string
	policyApplied string
	policyFailed  string

	// provider editor form state: long-lived field buffers prefilled on
	// open (never created inside a view), the edited record id ("" =
	// create), and the delete confirm target.
	providerFormOpen   bool
	providerFormID     string
	providerName       string
	providerURL        string
	providerKey        string
	providerModels     string
	providerProto      string
	providerDeleteOpen bool
	providerDeleteID   string

	// dialog state: rename prompt, delete confirm, error detail.
	renameOpen bool
	renameID   string
	deleteOpen bool
	deleteID   string
	errOpen    bool

	// pendingToast carries one toast raised off-frame (the directory
	// picker goroutine) to the next frame.
	pendingToast string

	// pickCh carries directory-picker outcomes from the picker goroutine
	// to the next frame (buffered: one pending pick is enough).
	pickCh     chan pickOutcome
	navigation navigationState
	inspector  inspectorState
}

// Root builds the state root around the services (docs/go-migration.md
// §5.7: ui.View(app.Root(svcs).View)). It restores the persisted state:
// the sidebar session list, the newest conversation's journal history
// (opened so a restart continues where the user left off), the stored
// new-session draft, and the settings-backed selections. Frames never
// re-read them unless a dirty flag asks.
func Root(svcs *Services) *App {
	a := &App{
		svcs:        svcs,
		views:       map[string]*sessionView{},
		settingsTab: "general",
		draftList: kit.ChatListState{
			Draft:    true,
			Greeting: Greeting,
		},
	}
	a.refreshSessions()
	a.loadNavigation()
	a.refreshModelItems()
	// First launch auto-select: a default picked here (no stored pair, or
	// a stale one) persists immediately so a fresh install can send as
	// soon as a provider exists. The write touches the config store only.
	a.applyModelSelection(nil)
	if dir, text, ok, err := svcs.Sessions.NewDraft(); err == nil && ok {
		a.draftDir, a.draftText = dir, text
	}
	settings, _ := svcs.Settings.Get()
	if a.draftDir == "" {
		a.draftDir = settings.WorkingDirectory
	}
	a.themeSel, a.themeApplied = string(settings.Theme), string(settings.Theme)
	a.policySel, a.policyApplied = string(settings.ToolPermissionPolicy), string(settings.ToolPermissionPolicy)
	a.draftSaved = a.draftText
	a.pickCh = make(chan pickOutcome, 1)
	// Startup restore: the newest session opens from its journal replay
	// (interrupted turns arrive as in-memory failure projections).
	if len(a.sessions) > 0 {
		a.openSession(nil, a.sessions[0].ID)
	}
	return a
}

// BindWindow stores the main window so event pumps can request frames and
// native dialogs attach to it. Call once after mygo.NewWindow.
func (a *App) BindWindow(win *mygo.Window) {
	a.win.Store(win)
}

// update runs fn on the main thread: through the bound window when there
// is one, synchronously otherwise (headless assemblies drive the same
// state mutations from the test goroutine).
func (a *App) update(fn func()) {
	if w := a.win.Load(); w != nil {
		w.Update(fn)
		return
	}
	fn()
}

// toast is ShowToast guarded for frames without a context.
func toast(c *ui.Context, kind kit.ToastKind, msg string) {
	if c != nil {
		kit.ShowToast(c, kind, msg)
	}
}

// ApplyTheme pushes the persisted appearance preference to the desktop
// (docs/go-migration.md §3.1 rule 6). It is a blocking main-thread mygo
// call: call it once from cmd's WhenReady before the app runs, or from the
// main thread afterwards — never from a bare service write or a test.
func (a *App) ApplyTheme() {
	settings, err := a.svcs.Settings.Get()
	if err != nil {
		return
	}
	theme.SetTheme(theme.Setting(settings.Theme))
}

// View renders one frame. MujicaUI's components must find core.Use at the
// top of every frame, so it runs first — pinned appearance mirrored from
// the app preference, zh copy — and theme.Apply then overlays the zai
// palette for the kit's own widgets (MujicaUI reads its tokens from the
// settings Use installs, not from the window theme, so both coexist).
func (a *App) View(c *ui.Context) {
	mode := core.System
	switch theme.Theme() {
	case theme.SettingLight:
		mode = core.Light
	case theme.SettingDark:
		mode = core.Dark
	}
	core.Use(c, core.Settings{Mode: mode, Locale: core.ZhCN})

	pal := theme.Active(c)
	theme.Apply(c, pal)

	if a.listDirty {
		a.refreshSessions()
	}
	a.drainPicks()
	a.drainInspections(c)
	a.syncDraft(c)
	if a.pendingToast != "" {
		toast(c, kit.ToastWarning, a.pendingToast)
		a.pendingToast = ""
	}

	if a.view == "settings" {
		a.settingsView(c)
	} else {
		a.workbenchView(c)
	}
	a.dialogsView(c)
	kit.Toasts(c)
}

// --- session list & switching ---

// refreshSessions reloads the sidebar listing from the runtime.
func (a *App) refreshSessions() {
	a.sessions, _ = a.svcs.Sessions.List()
	a.listDirty = false
}

// activeView returns the projection of the open session, if any.
func (a *App) activeView() *sessionView {
	if a.activeID == "" {
		return nil
	}
	return a.views[a.activeID]
}

// sessionTitle returns the display title of a session.
func (a *App) sessionTitle(meta runtime.SessionMeta) string {
	if strings.TrimSpace(meta.Title) != "" {
		return meta.Title
	}
	return UntitledSession
}

// startNewDraft leaves the open session and enters the new-session draft
// state (zcode-shell-specs.md §2.1: clicking new enters the draft, it does
// not create a task).
func (a *App) startNewDraft(c *ui.Context) {
	if a.activeID == "" {
		return
	}
	a.flushDraft()
	a.activeID = ""
	// The stored new-session draft survives restarts; the project
	// directory of the session just left stays the default for the next
	// one, so only the text is loaded when a draft exists.
	if _, text, ok, err := a.svcs.Sessions.NewDraft(); err == nil && ok {
		a.draftText = text
	} else {
		a.draftText = ""
	}
	a.draftSaved = a.draftText
	a.draftPendingSave = false
}

// openSession switches to a session, subscribing on first open. The
// history applies synchronously from the journal projection; live events
// go through the pump once a window is bound (headless assemblies apply
// events from the test driver instead, so the subscription is dropped
// there). Events that land between the history read and the subscription
// arrive as replay duplicates and are dropped by the Seq deduplication in
// apply.
func (a *App) openSession(c *ui.Context, id string) {
	if id == "" || id == a.activeID {
		return
	}
	a.flushDraft()
	st, ok := a.views[id]
	if !ok {
		var meta runtime.SessionMeta
		found := false
		for _, m := range a.sessions {
			if m.ID == id {
				meta, found = m, true
				break
			}
		}
		if !found {
			metas, err := a.svcs.Sessions.List()
			if err != nil {
				toast(c, kit.ToastWarning, err.Error())
				return
			}
			a.sessions = metas
			for _, m := range metas {
				if m.ID == id {
					meta, found = m, true
					break
				}
			}
			if !found {
				return
			}
		}
		history, err := a.svcs.Sessions.History(id)
		if err != nil {
			toast(c, kit.ToastWarning, err.Error())
			return
		}
		st = newSessionView(meta)
		// The running flag first: a terminal event in the history clears
		// it again (finishTurn), while a session still mid-turn keeps it.
		st.running = a.svcs.Sessions.Running(id)
		for _, ev := range history {
			st.apply(ev)
		}
		ch, cancel, err := a.svcs.Sessions.Events(id)
		if err != nil {
			toast(c, kit.ToastWarning, err.Error())
			a.views[id] = st
		} else {
			st.cancelEvents = cancel
			a.views[id] = st
			if a.win.Load() != nil {
				go a.pump(id, ch)
			} else {
				// No window bound (headless assembly): nothing will drain
				// the subscription, so detach it again. The test driver
				// subscribes itself.
				cancel()
			}
		}
	}
	a.activeID = id
	a.draftSaved = st.draft
	a.draftPendingSave = false
}

// closeView detaches a deleted session's projection.
func (a *App) closeView(id string) {
	if st := a.views[id]; st != nil && st.cancelEvents != nil {
		st.cancelEvents()
	}
	delete(a.views, id)
	if a.activeID == id {
		a.activeID = ""
	}
}

// pump drains one session's live event channel: each batch is applied in a
// single main-thread update so streaming deltas coalesce into one repaint
// per frame (docs/go-migration.md §1.3, risk 5).
func (a *App) pump(id string, ch <-chan runtime.Event) {
	for ev := range ch {
		batch := []runtime.Event{ev}
	drain:
		for {
			select {
			case next, ok := <-ch:
				if !ok {
					break drain
				}
				batch = append(batch, next)
			default:
				break drain
			}
		}
		a.update(func() {
			a.deliver(id, batch)
		})
	}
}

// deliver applies a batch of live events to a session's projection and
// marks the derived state dirty.
func (a *App) deliver(id string, batch []runtime.Event) {
	st := a.views[id]
	if st == nil {
		return
	}
	for _, ev := range batch {
		st.apply(ev)
		if isTerminalEvent(ev.Type) {
			st.running = false
			a.listDirty = true
		}
	}
}

// isTerminalEvent reports whether the event type ends a turn.
func isTerminalEvent(t runtime.EventType) bool {
	return t == runtime.EventTurnCompleted ||
		t == runtime.EventTurnFailed ||
		t == runtime.EventTurnCancelled
}

// --- draft persistence (debounced) ---

// currentDraft returns the draft text of the visible target.
func (a *App) currentDraft() *string {
	if st := a.activeView(); st != nil {
		return &st.draft
	}
	return &a.draftText
}

// syncDraft persists the composer text 500ms after the last observed
// change (runtime/draft.go: the debounce lives in the app layer). The
// deadline wakes one frame via c.After; the save then runs on that frame.
func (a *App) syncDraft(c *ui.Context) {
	draft := a.currentDraft()
	if *draft == a.draftSaved {
		a.draftPendingSave = false
		return
	}
	now := c.Now()
	if !a.draftPendingSave {
		a.draftPendingSave = true
		a.draftDeadline = now.Add(500 * time.Millisecond)
		c.After(500 * time.Millisecond)
		return
	}
	if now.Before(a.draftDeadline) {
		return
	}
	a.saveDraft(*draft)
}

// flushDraft saves a pending draft change immediately (session switches
// and sends must not race the debounce).
func (a *App) flushDraft() {
	draft := a.currentDraft()
	if *draft != a.draftSaved {
		a.saveDraft(*draft)
	}
	a.draftPendingSave = false
}

// saveDraft writes the draft of the visible target. Persistence is
// best-effort: a failed write loses the draft on restart but never blocks
// the interface (the send path re-validates everything it needs).
func (a *App) saveDraft(text string) {
	a.draftSaved = text
	a.draftPendingSave = false
	id := a.activeID
	if id == "" {
		a.svcs.Sessions.SetDraftProjectDir(a.draftDir)
	}
	_ = a.svcs.Sessions.SaveDraft(id, text)
}

// --- sending & stopping ---

// modelReady reports whether the selector holds a configured model.
func (a *App) modelReady() bool {
	_, _, ok := decodeModelValue(a.modelSel)
	return ok
}

// canSend reports whether the send button accepts a click: a non-empty
// draft, a configured model, and no running turn (docs/go-migration.md
// 差异 8: v1 has no queue, a running turn only offers Stop).
func (a *App) canSend(draft string, running bool) bool {
	return strings.TrimSpace(draft) != "" && a.modelReady() && !running
}

// sendActive submits the composer text of the visible target.
func (a *App) sendActive(c *ui.Context) {
	if a.activeID == "" {
		a.sendNewSession(c)
		return
	}
	st := a.activeView()
	if st == nil || !a.canSend(st.draft, st.running) {
		a.refuseSubmit()
		return
	}
	if err := a.svcs.Sessions.Send(a.activeID, "", st.draft); err != nil {
		a.toastSendError(c, err)
		a.refuseSubmit()
		return
	}
	st.draft = ""
	a.draftSaved = ""
	a.draftPendingSave = false
	// The stored draft file must not resurrect the sent text after a
	// restart; the debouncer sees draft==saved and stays idle.
	_ = a.svcs.Sessions.SaveDraft(a.activeID, "")
	st.running = true
	a.listDirty = true
}

// refuseSubmit strips the newline a submitted Enter left in the draft when
// the send was refused (kit.Editor lets the key through before the app
// judges it). The saved marker is left alone so the debouncer persists the
// stripped text.
func (a *App) refuseSubmit() {
	draft := a.currentDraft()
	*draft = strings.TrimSuffix(*draft, "\n")
}

// sendNewSession converts the draft state into a session on first send
// (docs/go-migration.md §5.5), then switches to it. A failed send cleans
// up the just-created empty session in the service layer.
func (a *App) sendNewSession(c *ui.Context) {
	if strings.TrimSpace(a.draftText) == "" {
		return
	}
	if strings.TrimSpace(a.draftDir) == "" {
		toast(c, kit.ToastWarning, NoProjectToast)
		a.refuseSubmit()
		return
	}
	if !a.modelReady() {
		toast(c, kit.ToastWarning, SendNoModelHint)
		a.refuseSubmit()
		return
	}
	sess, err := a.svcs.Sessions.send("", a.draftDir, a.draftText)
	if err != nil {
		a.toastSendError(c, err)
		return
	}
	id := sess.Meta().ID
	a.draftText = ""
	a.draftSaved = ""
	a.draftPendingSave = false
	a.svcs.Sessions.SetDraftProjectDir("")
	_ = a.svcs.Sessions.SaveDraft("", "")
	a.refreshSessions()
	a.openSession(c, id)
	if st := a.views[id]; st != nil && a.svcs.Sessions.Running(id) {
		st.running = true
	}
}

// toastSendError maps send failures onto the toast copy.
func (a *App) toastSendError(c *ui.Context, err error) {
	switch {
	case errors.Is(err, runtime.ErrBusy):
		toast(c, kit.ToastWarning, BusyToast)
	default:
		toast(c, kit.ToastWarning, SendFailedToast+"："+err.Error())
	}
}

// stopActive cancels the running turn of the open session.
func (a *App) stopActive(c *ui.Context) {
	if a.activeID == "" {
		return
	}
	if err := a.svcs.Sessions.Stop(a.activeID); err != nil {
		toast(c, kit.ToastWarning, StopFailedToast+"："+err.Error())
	}
}

// --- model selector ---

// modelValueSeparator splits provider and model ids inside the selector
// value; the unit separator cannot occur in either identifier.
const modelValueSeparator = "\x1f"

// encodeModelValue joins provider and model id into a selector value.
func encodeModelValue(providerID, modelID string) string {
	return providerID + modelValueSeparator + modelID
}

// decodeModelValue splits a selector value back into provider and model
// id.
func decodeModelValue(value string) (providerID, modelID string, ok bool) {
	parts := strings.SplitN(value, modelValueSeparator, 2)
	if len(parts) != 2 || parts[0] == "" || parts[1] == "" {
		return "", "", false
	}
	return parts[0], parts[1], true
}

// refreshModelItems rebuilds the selector entries from the providers
// snapshot and keeps the current selection when it is still valid.
func (a *App) refreshModelItems() {
	state := a.svcs.Settings.Providers()
	items := make([]kit.SelectItem[string], 0, 16)
	for _, record := range state.Providers {
		for _, modelID := range record.Models {
			items = append(items, kit.SelectItem[string]{
				Value: encodeModelValue(record.ID, modelID),
				Label: record.Name + " / " + modelID,
			})
		}
	}
	a.modelItems = items
	selection := a.modelSel
	valid := false
	for _, item := range items {
		if item.Value == selection {
			valid = true
			break
		}
	}
	if !valid {
		selection = ""
		a.modelApplied, a.modelFailed = "", ""
		if providerID, ok := state.ActiveProvider(); ok {
			if modelID, ok := state.ActiveModel(); ok {
				candidate := encodeModelValue(providerID, modelID)
				for _, item := range items {
					if item.Value == candidate {
						selection = candidate
						// The stored pair is already persisted and still
						// listed; mark it applied so the apply step does
						// not rewrite the store every frame.
						a.modelApplied = selection
						break
					}
				}
			}
		}
	}
	if selection == "" && len(items) > 0 {
		// No usable selection anywhere: fall back to the first model and
		// leave it unapplied, so the next apply step persists the pick.
		selection = items[0].Value
		a.modelApplied = ""
	}
	a.modelSel = selection
}

// applyModelSelection persists a selector change (called from the frame
// that observes it; a change is one small JSON write, not a per-frame
// cost). A failing persist toasts once per attempted value — the retry
// would otherwise repeat every frame.
func (a *App) applyModelSelection(c *ui.Context) {
	if a.modelSel == a.modelApplied || a.modelSel == a.modelFailed {
		return
	}
	providerID, modelID, ok := decodeModelValue(a.modelSel)
	if !ok {
		return
	}
	if err := a.svcs.Settings.SetDefaultModel(providerID, modelID); err != nil {
		a.modelFailed = a.modelSel
		toast(c, kit.ToastWarning, err.Error())
		return
	}
	a.modelApplied = a.modelSel
	a.modelFailed = ""
}

// --- project directory ---

// pickOutcome is one directory-picker result on its way to the next frame.
type pickOutcome struct {
	dir string
	err error
}

// beginChooseProject opens the native directory picker off the UI thread
// (the dialog blocks) and hands the outcome to the frame through a
// buffered channel — goroutines never touch the state directly, in
// production (win.Update) or in headless assemblies alike.
func (a *App) beginChooseProject() {
	go func() {
		dir, err := a.svcs.Project.ChooseDirectory()
		select {
		case a.pickCh <- pickOutcome{dir: dir, err: err}:
		default: // a newer pick supersedes the one still pending
		}
	}()
}

// drainPicks applies the pending picker outcomes. The picked directory
// becomes both the draft state's project and the persisted working
// directory (Settings.WorkingDirectory). Called from the frame.
func (a *App) drainPicks() {
	for {
		select {
		case pick := <-a.pickCh:
			if pick.err != nil {
				a.pendingToast = pick.err.Error()
				continue
			}
			if pick.dir != "" {
				if err := a.registerProject(pick.dir); err != nil {
					a.pendingToast = err.Error()
					continue
				}
				a.selectProject(nil, pick.dir)
			}
		default:
			return
		}
	}
}
