package app

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"
	goruntime "runtime"
	"slices"
	"strings"
	"sync"

	"github.com/egoist/mygo"

	"keencode/internal/agent"
	"keencode/internal/config"
	"keencode/internal/model"
	"keencode/internal/provider/anthropic"
	"keencode/internal/provider/openai"
	"keencode/internal/runtime"
	"keencode/internal/tools"
)

// Services is the service collection of the app (docs/go-migration.md
// §5.7): main constructs it once and injects it into the UI state root.
// The service method sets keep the Bind-compatible shape of the plan
// (exported, JSON-encodable parameters, error last) so a future web
// surface can wrap them with mygo.Bind; Events stays a plain channel API
// by design (§5.7).
//
// Assembly order for cmd/keencode (the manager options must exist before
// runtime.OpenManager runs, so the services are built in two steps):
//
//	svcs := app.NewServices(store)
//	mgr, err := runtime.OpenManager(root, svcs.RuntimeOptions())
//	svcs.AttachManager(mgr)            // or app.Assemble(store, mgr) for a manager
//	                                   // assembled elsewhere with the same options
//	win := mygo.NewWindow(mygo.WindowOptions{..., Content: ui.View(app.Root(svcs).View)})
//	svcs.BindWindow(win)
type Services struct {
	Settings *SettingsService
	Sessions *SessionService
	Project  *ProjectService
}

// NewServices builds the config-backed half of the services. The session
// service starts unattached; RuntimeOptions and AttachManager wire the
// runtime manager afterwards.
func NewServices(store *config.Store) *Services {
	settings := newSettingsService(store)
	runner := newRunnerSource(settings)
	sessions := &SessionService{settings: settings, runner: runner}
	return &Services{
		Settings: settings,
		Sessions: sessions,
		Project: &ProjectService{
			window:     func() *mygo.Window { return nil },
			openDialog: mygo.Dialog.Open,
		},
	}
}

// RuntimeOptions returns the manager options that route turns through
// this app's agent assembly (docs/go-migration.md §5.7: deps func()
// agent.Dependencies). The returned closures late-bind the manager, so
// they are safe to hand to runtime.OpenManager before AttachManager runs;
// they are only invoked at Send time.
func (s *Services) RuntimeOptions() runtime.ManagerOptions {
	return runtime.ManagerOptions{
		Agent: s.Sessions.runner.agentFactory(),
		Model: s.ModelID,
	}
}

// AttachManager binds the session service to the runtime manager. Call it
// once, right after runtime.OpenManager.
func (s *Services) AttachManager(mgr *runtime.Manager) {
	s.Sessions.attach(mgr)
}

// Assemble is the plan-signature constructor (docs/go-migration.md §5.7):
// it builds the services around an already-constructed manager. Use it
// only when the manager was opened with the same services' RuntimeOptions
// (see NewServices for the two-step order); otherwise sessions would send
// turns without this app's agent assembly.
func Assemble(store *config.Store, mgr *runtime.Manager) *Services {
	svcs := NewServices(store)
	svcs.AttachManager(mgr)
	return svcs
}

// BindWindow stores the main window so blocking native dialogs attach to
// it and cross-goroutine state updates repaint the interface (plan §5.7:
// svcs.BindWindow(win) for ProjectService.Parent).
func (s *Services) BindWindow(win *mygo.Window) {
	w := win
	s.Project.window = func() *mygo.Window { return w }
	s.Sessions.runner.bindWindow(w)
}

// ModelID resolves the model id recorded into TurnRequest at Send time
// (runtime.ModelFunc): the active model of the providers state.
func (s *Services) ModelID() string {
	providerID, ok := s.Settings.ActiveProviderID()
	if !ok {
		return ""
	}
	if modelID, ok := s.Settings.ActiveModelID(); ok {
		return modelID
	}
	_ = providerID
	return ""
}

// SessionService exposes the session lifecycle to the UI (plan §5.7). All
// methods are UI-thread calls; Send/Create do bounded disk IO (journal
// fsync of the user message) which v1 accepts on the main thread.
type SessionService struct {
	mu       sync.Mutex // guards mgr
	mgr      *runtime.Manager
	settings *SettingsService
	runner   *runnerSource
}

// attach binds the runtime manager; called once by AttachManager.
func (s *SessionService) attach(mgr *runtime.Manager) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.mgr = mgr
}

// manager returns the bound manager or an error for the not-yet-attached
// state.
func (s *SessionService) manager() (*runtime.Manager, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.mgr == nil {
		return nil, errors.New("会话服务尚未连接运行时")
	}
	return s.mgr, nil
}

// Create starts a new session for the project directory (plan §5.7). The
// plan signature returns the meta; the underlying runtime hands back the
// live session, and only its identity is surfaced here.
func (s *SessionService) Create(projectDir string) (runtime.SessionMeta, error) {
	mgr, err := s.manager()
	if err != nil {
		return runtime.SessionMeta{}, err
	}
	sess, err := mgr.Create(projectDir)
	if err != nil {
		return runtime.SessionMeta{}, err
	}
	return sess.Meta(), nil
}

// List returns every session ordered by UpdatedAt descending.
func (s *SessionService) List() ([]runtime.SessionMeta, error) {
	mgr, err := s.manager()
	if err != nil {
		return nil, err
	}
	return mgr.List()
}

// Rename sets the display title of a session.
func (s *SessionService) Rename(id, title string) error {
	mgr, err := s.manager()
	if err != nil {
		return err
	}
	return mgr.Rename(id, title)
}

func (s *SessionService) SetPinned(id string, pinned bool) error {
	mgr, err := s.manager()
	if err != nil {
		return err
	}
	return mgr.SetPinned(id, pinned)
}

// Delete removes a session and its directory. A running session is
// refused; the caller surfaces the failure.
func (s *SessionService) Delete(id string) error {
	mgr, err := s.manager()
	if err != nil {
		return err
	}
	s.runner.forgetSession(id)
	return mgr.Delete(id)
}

// Send submits one user message (plan §5.7). id=="" means the new-session
// draft state: the session is created from projectDir first (the
// draft→session conversion, docs/go-migration.md §5.5) and the message is
// then sent into it. projectDir is the UI's in-memory value, never
// re-read from the debounced draft file. The turn context lives for the
// process lifetime; Stop cancels it.
func (s *SessionService) Send(id, projectDir, text string) error {
	_, err := s.send(id, projectDir, text)
	return err
}

// send is Send returning the session, so the caller can switch to the
// freshly created conversation. It resolves the effective project
// directory (session meta for existing sessions) and hands it to the
// runner source, whose factory consumes it under the session lock at Send
// time. All sends run on the UI thread, so the handoff cannot interleave.
func (s *SessionService) send(id, projectDir, text string) (*runtime.Session, error) {
	mgr, err := s.manager()
	if err != nil {
		return nil, err
	}
	if strings.TrimSpace(text) == "" {
		return nil, errors.New("消息内容不能为空")
	}
	var sess *runtime.Session
	if id == "" {
		if strings.TrimSpace(projectDir) == "" {
			return nil, errors.New(NoProjectToast)
		}
		sess, err = mgr.Create(projectDir)
		if err != nil {
			return nil, err
		}
	} else {
		sess, err = mgr.Get(id)
		if err != nil {
			return nil, err
		}
	}
	meta := sess.Meta()
	s.runner.prepareTurn(meta.ID, meta.ProjectDir)
	err = sess.Send(context.Background(), text)
	if err != nil {
		// A draft conversion that failed before its first message must not
		// strand an empty session in the sidebar; a refusal on a running
		// session (ErrBusy) is ignored by the best-effort delete.
		if id == "" {
			_ = mgr.Delete(sess.Meta().ID)
		}
		return sess, err
	}
	return sess, nil
}

// Stop cancels the running turn of a session (idempotent).
func (s *SessionService) Stop(id string) error {
	mgr, err := s.manager()
	if err != nil {
		return err
	}
	sess, err := mgr.Get(id)
	if err != nil {
		return err
	}
	sess.Stop()
	return nil
}

// Running reports whether the session has an active turn.
func (s *SessionService) Running(id string) bool {
	mgr, err := s.manager()
	if err != nil {
		return false
	}
	sess, err := mgr.Get(id)
	if err != nil {
		return false
	}
	return sess.Running()
}

// SaveDraft stores the unsent composer text; id=="" targets the
// new-session draft (plan §5.7; runtime/draft.go owns the file layout).
// Writes go through immediately — the input debounce lives in the app
// view (runtime/draft.go documents the split).
func (s *SessionService) SaveDraft(id, text string) error {
	mgr, err := s.manager()
	if err != nil {
		return err
	}
	if id == "" {
		return mgr.SaveNewDraft(s.draftProjectDir(), text)
	}
	return mgr.SaveSessionDraft(id, text)
}

// draftProjectDir returns the project directory stored alongside the
// new-session draft text; the field lives on the app state root and is
// pushed here before saving.
func (s *SessionService) draftProjectDir() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.runner.newDraftDir
}

// SetDraftProjectDir records the project directory of the new-session
// draft so SaveDraft("") persists the pair.
func (s *SessionService) SetDraftProjectDir(dir string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.runner.newDraftDir = dir
}

// Draft returns the unsent draft of a session; id=="" reads the
// new-session draft including its project directory (plan §5.7 carries
// only the text; the draft state needs the directory too, so callers use
// NewDraft for the pair).
func (s *SessionService) Draft(id string) (string, error) {
	text, _, err := s.draftPair(id)
	return text, err
}

// draftPair returns (text, projectDir) of the target draft.
func (s *SessionService) draftPair(id string) (string, string, error) {
	mgr, err := s.manager()
	if err != nil {
		return "", "", err
	}
	if id == "" {
		dir, text, _, err := mgr.NewDraft()
		return text, dir, err
	}
	text, _, err := mgr.SessionDraft(id)
	return text, "", err
}

// NewDraft returns the stored new-session draft (text and project
// directory).
func (s *SessionService) NewDraft() (projectDir, text string, ok bool, err error) {
	mgr, err := s.manager()
	if err != nil {
		return "", "", false, err
	}
	return mgr.NewDraft()
}

// History returns the session's full event history in journal order
// (including the in-memory recovery projections for interrupted turns).
func (s *SessionService) History(id string) ([]runtime.Event, error) {
	mgr, err := s.manager()
	if err != nil {
		return nil, err
	}
	sess, err := mgr.Get(id)
	if err != nil {
		return nil, err
	}
	return sess.History(), nil
}

// Events subscribes to the session's unified event stream (plan §5.7):
// the channel first delivers the full history with Replay=true, then live
// events, ordered by Seq without gaps or duplicates. Unsubscribe when the
// app stops showing the session.
func (s *SessionService) Events(id string) (<-chan runtime.Event, func(), error) {
	mgr, err := s.manager()
	if err != nil {
		return nil, nil, err
	}
	sess, err := mgr.Get(id)
	if err != nil {
		return nil, nil, err
	}
	ch, cancel := sess.Subscribe()
	return ch, cancel, nil
}

// SettingsService serves the app settings and provider/model directory
// (plan §5.7, adapted to the shipped config API: the providers state
// carries the active selection instead of a separate default model ref).
// Reads come from an in-memory snapshot; the store is touched only on
// load and save, so view frames never do disk IO.
type SettingsService struct {
	mu        sync.Mutex
	store     *config.Store
	settings  config.Settings
	providers config.ProvidersState
}

// newSettingsService loads the snapshot; load failures degrade to the
// defaults (config.Store never fails loads, it reports warnings).
func newSettingsService(store *config.Store) *SettingsService {
	s := &SettingsService{store: store}
	s.settings = store.LoadSettings().Settings
	providers, _, err := store.LoadProviders()
	if err != nil {
		providers = config.DefaultProvidersState()
	}
	s.providers = providers
	return s
}

// Get returns the settings snapshot (plan §5.7 Get; the shipped config
// splits settings and providers, so Providers is a second accessor).
func (s *SettingsService) Get() (config.Settings, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.settings, nil
}

// Loaded returns the settings snapshot without the error channel; loads
// never fail (config.SettingsLoad degrades to defaults), so callers that
// only read policy fields can skip the error handling.
func (s *SettingsService) Loaded() config.Settings {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.settings
}

// Providers returns the provider/model directory snapshot.
func (s *SettingsService) Providers() config.ProvidersState {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.providers
}

// ActiveProviderID returns the selected provider, if any.
func (s *SettingsService) ActiveProviderID() (string, bool) {
	return s.Providers().ActiveProvider()
}

// ActiveModelID returns the selected model, if any.
func (s *SettingsService) ActiveModelID() (string, bool) {
	return s.Providers().ActiveModel()
}

// SetDefaultModel selects the active provider/model pair and persists it
// (plan §5.7 SetDefaultModel(ref config.ModelRef); the shipped config
// stores the selection as the providers state's active IDs, so the ref is
// spelled as two fields).
func (s *SettingsService) SetDefaultModel(providerID, modelID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.providers.Provider(providerID); !ok {
		return fmt.Errorf("供应商 %s 不存在", providerID)
	}
	found := false
	for _, m := range s.providers.Providers {
		if m.ID != providerID {
			continue
		}
		for _, id := range m.Models {
			if id == modelID {
				found = true
			}
		}
	}
	if !found {
		return fmt.Errorf("模型 %s 不在供应商 %s 的模型列表中", modelID, providerID)
	}
	s.providers.ActiveProviderID = &providerID
	s.providers.ActiveModelID = &modelID
	return s.store.SaveProviders(s.providers)
}

// SetTheme persists the appearance preference. The desktop application of
// the preference is a separate step (ApplyTheme): mygo.Theme.SetSource is
// a blocking main-thread call, so it belongs to startup or to the
// settings UI behind a running app — never to a bare service write.
func (s *SettingsService) SetTheme(t config.Theme) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	switch t {
	case config.ThemeSystem, config.ThemeLight, config.ThemeDark:
	default:
		return fmt.Errorf("主题 %q 不受支持", string(t))
	}
	s.settings.Theme = t
	return s.store.SaveSettings(s.settings)
}

// ReplaceProviders persists a full providers state snapshot (the settings
// page's bulk save path). The state is validated first, so an invalid pair
// of active IDs never reaches disk.
func (s *SettingsService) ReplaceProviders(state config.ProvidersState) error {
	if err := config.ValidateProvidersState(state); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.providers = state
	return s.store.SaveProviders(s.providers)
}

// SetToolPermissionPolicy persists the side-effect authorization policy
// that the Authorize bridge consults (config.ToolPermissionPolicy).
func (s *SettingsService) SetToolPermissionPolicy(p config.ToolPermissionPolicy) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	switch p {
	case config.ToolPermissionAsk, config.ToolPermissionAllowAll, config.ToolPermissionReadOnly:
	default:
		return fmt.Errorf("工具权限策略 %q 不受支持", string(p))
	}
	s.settings.ToolPermissionPolicy = p
	return s.store.SaveSettings(s.settings)
}

// SetWorkingDirectory persists the default project directory offered for
// new conversations (Settings.WorkingDirectory). Empty clears it.
func (s *SettingsService) SetWorkingDirectory(dir string) error {
	dir = strings.TrimSpace(dir)
	if dir != "" && !filepath.IsAbs(dir) {
		return fmt.Errorf("默认工作目录必须是绝对路径：%s", dir)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.settings.WorkingDirectory = dir
	return s.store.SaveSettings(s.settings)
}

// UpsertProvider replaces or appends one provider record and persists the
// state (plan §5.7). The persisted-state validators require an active
// provider/model pair whenever any provider exists, so saving into an
// empty or inconsistent state selects the upserted record with its first
// model — the natural meaning of "adding the first provider".
func (s *SettingsService) UpsertProvider(p config.ProviderRecord) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	state := s.providers
	state.Providers = slices.Clone(state.Providers)
	replaced := false
	for i := range state.Providers {
		if state.Providers[i].ID == p.ID {
			state.Providers[i] = p
			replaced = true
			break
		}
	}
	if !replaced {
		state.Providers = append(state.Providers, p)
	}
	active, ok := state.ActiveProvider()
	record, found := state.Provider(active)
	if !ok || !found {
		record = p
		id := p.ID
		state.ActiveProviderID = &id
	}
	modelID, ok := state.ActiveModel()
	if !ok || !slices.Contains(record.Models, modelID) {
		if len(record.Models) > 0 {
			modelID = record.Models[0]
			state.ActiveModelID = &modelID
		}
	}
	if err := s.store.SaveProviders(state); err != nil {
		return err
	}
	s.providers = state
	return nil
}

// DeleteProvider removes one provider record (plan §5.7). When the removed
// record was the active selection, the first remaining provider and its
// first model take over — the validators reject a providers state with
// entries but no selection, and a leftover selection of a deleted record
// would dangle.
func (s *SettingsService) DeleteProvider(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	kept := s.providers.Providers[:0]
	for _, record := range s.providers.Providers {
		if record.ID != id {
			kept = append(kept, record)
		}
	}
	s.providers.Providers = kept
	if active, ok := s.providers.ActiveProvider(); ok && active == id {
		s.providers.ActiveProviderID = nil
		s.providers.ActiveModelID = nil
		for _, record := range s.providers.Providers {
			providerID := record.ID
			s.providers.ActiveProviderID = &providerID
			if len(record.Models) > 0 {
				model := record.Models[0]
				s.providers.ActiveModelID = &model
			}
			break
		}
	}
	return s.store.SaveProviders(s.providers)
}

// ProjectService offers the native directory picker (plan §5.7). The
// blocking mygo dialog must run off the UI thread: callers invoke
// ChooseDirectory from a goroutine and deliver the outcome through
// App.update.
type ProjectService struct {
	mu         sync.Mutex
	window     func() *mygo.Window
	openDialog func(mygo.OpenDialogOptions) ([]string, error)
}

// ChooseDirectory shows the native directory picker attached to the main
// window (mygo.Dialog.Open with Directory:true, dialog.go:24-44). It
// returns "" without error when the user cancels.
func (p *ProjectService) ChooseDirectory() (string, error) {
	p.mu.Lock()
	parent, open := p.window, p.openDialog
	p.mu.Unlock()
	opts := mygo.OpenDialogOptions{
		Title:             ChooseProjectTitle,
		Directory:         true,
		CreateDirectories: true,
		Parent:            parent(),
	}
	paths, err := open(opts)
	if err != nil {
		return "", fmt.Errorf("打开目录选择器: %w", err)
	}
	if len(paths) == 0 {
		return "", nil
	}
	return paths[0], nil
}

// setOpenDialog replaces the dialog seam (test injection).
func (p *ProjectService) setOpenDialog(fn func(mygo.OpenDialogOptions) ([]string, error)) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.openDialog = fn
}

// --- provider assembly (docs/go-migration.md §5.2) ---

// providerFor builds the neutral provider for one configured record by
// mapping its resolved endpoint onto the protocol adapters. The OpenAI
// Responses protocol is deferred (docs/go-migration.md §7) and fails with
// a stable message.
func providerFor(record config.ProviderRecord) (model.Provider, error) {
	endpoint, err := record.Endpoint()
	if err != nil {
		return nil, err
	}
	switch endpoint.Protocol {
	case config.ProtocolMessages:
		return anthropic.New(anthropic.Options{
			BaseURL: endpoint.BaseURL,
			APIKey:  endpoint.APIKey,
		})
	case config.ProtocolChatCompletions:
		return openai.New(openai.Options{
			Endpoint: strings.TrimRight(endpoint.BaseURL, "/") + "/chat/completions",
			APIKey:   endpoint.APIKey,
		})
	default:
		return nil, fmt.Errorf("协议 %s 暂未支持，请改用 Messages 或 Chat Completions", endpoint.Protocol)
	}
}

// toolRegistryFor assembles the v1 six-tool registry bound to one project
// directory (docs/go-migration.md §5.3). A zero timeout keeps the Bash
// default of 120s.
func toolRegistryFor(workDir string) (*tools.Registry, error) {
	return tools.NewRegistry(
		tools.NewRead(workDir),
		tools.NewGlob(workDir),
		tools.NewGrep(workDir),
		tools.NewWrite(workDir),
		tools.NewEdit(workDir),
		tools.NewBash(workDir, 0),
	)
}

// systemPromptFor assembles the v1 fixed system prompt for one project
// directory (docs/go-migration.md §5.4 prompt.go).
func systemPromptFor(workDir string) model.Message {
	return agent.BuildSystemPrompt(workDir, runtimeOS())
}

// runtimeOS indirection keeps the standard-library call in one testable
// place. The alias goruntime disambiguates the standard runtime package
// from keencode/internal/runtime.
func runtimeOS() string { return goruntime.GOOS }
