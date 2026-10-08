package app

// Centralized Chinese user-facing copy of the application shell
// (docs/go-migration.md §7: i18n extraction is deferred; every visible
// string of the app lives here so a later pass can lift them in one
// place). Kit components own a few fixed labels of their own (tool status
// words, dialog buttons); everything the app layer renders resolves from
// these constants.

// WindowTitle is the native window title shown in the OS title bar.
const WindowTitle = "KeenCode"

// Settings copy (zcode-shell-specs.md §3 visual language: grouped cards,
// labeled form rows, lg selects).
const (
	// SettingsBack is the nav back button.
	SettingsBack = "返回"
	// SettingsTabGeneral and SettingsTabProviders label the two nav
	// entries.
	SettingsTabGeneral   = "通用"
	SettingsTabProviders = "供应商"
	// SettingsGeneralTitle and SettingsProvidersTitle are the panel
	// breadcrumb titles.
	SettingsGeneralTitle   = "通用设置"
	SettingsProvidersTitle = "供应商管理"
	// SettingsGearLabel names the sidebar footer gear (and its tooltip).
	SettingsGearLabel = "设置"

	// ThemeRowLabel and ThemeRowDesc head the theme row.
	ThemeRowLabel = "主题"
	ThemeRowDesc  = "界面亮暗外观"
	// ThemeLight, ThemeDark and ThemeSystem are the theme options.
	ThemeLight  = "亮色"
	ThemeDark   = "暗色"
	ThemeSystem = "跟随系统"

	// DefaultModelRowLabel and DefaultModelRowDesc head the default model
	// row.
	DefaultModelRowLabel = "默认模型"
	DefaultModelRowDesc  = "新会话使用的模型，可在输入区随时切换"

	// PolicyRowLabel and PolicyRowDesc head the permission policy row.
	PolicyRowLabel = "工具权限"
	PolicyRowDesc  = "执行写入、编辑与命令等副作用工具前的确认方式"
	// PolicyAsk, PolicyAllowAll and PolicyReadOnly are the policy options.
	PolicyAsk      = "每次询问"
	PolicyAllowAll = "全部允许"
	PolicyReadOnly = "仅只读"

	// ProjectRowLabel and ProjectRowDesc head the working-directory row.
	ProjectRowLabel = "当前项目"
	ProjectRowDesc  = "新会话默认打开的项目目录"
	// ProjectUnset repeats the header placeholder inside settings.
	ProjectUnset = NoProjectHint

	// AddProviderButton opens the provider form.
	AddProviderButton = "新增供应商"
	// ProviderEmptyHint shows before any provider exists.
	ProviderEmptyHint = "尚未配置供应商，点击「新增供应商」开始"
	// ProviderModelsCount formats the model count of one provider.
	ProviderModelsCount = "%d 个模型"
	// ProviderEditLabel and ProviderDeleteLabel name the row actions (and
	// their tooltips); the delete confirm button stays the shorter 删除.
	ProviderEditLabel   = "编辑供应商"
	ProviderDeleteLabel = "删除供应商"

	// ProviderFormCreateTitle and ProviderFormEditTitle title the form
	// dialog.
	ProviderFormCreateTitle = "新增供应商"
	ProviderFormEditTitle   = "编辑供应商"
	// ProviderFormSave confirms the form dialog.
	ProviderFormSave = "保存"
	// Provider form field labels.
	ProviderFormName   = "名称"
	ProviderFormProto  = "协议"
	ProviderFormURL    = "Base URL"
	ProviderFormKey    = "API Key"
	ProviderFormModels = "模型列表"
	// Provider form placeholders (they double as the fields' test labels).
	ProviderFormNamePlaceholder   = "例如 主力供应商"
	ProviderFormURLPlaceholder    = "https://api.example.com/v1"
	ProviderFormKeyPlaceholder    = "留空表示不鉴权"
	ProviderFormModelsPlaceholder = "逗号分隔，例如 gpt-4o, gpt-4o-mini"
	// ProviderFormMessages and ProviderFormChat label the two protocols.
	ProviderFormMessages = "Anthropic Messages"
	ProviderFormChat     = "OpenAI Chat Completions"

	// ProviderDeleteTitle and ProviderDeleteDesc head the delete confirm.
	ProviderDeleteTitle = "删除供应商"
	ProviderDeleteDesc  = "将删除该供应商配置；正在使用它的会话需要重新选择模型。"

	// ProviderSavedToast confirms a saved provider form.
	ProviderSavedToast = "供应商已保存"
)

// Greeting is the static new-session draft greeting rendered centered in
// the timeline (zcode-chat-specs.md §3; the hour-segmented copy is
// deferred to V1.1 per docs/go-migration.md §7).
const Greeting = "今天想做点什么？"

// Sidebar copy.
const (
	// NewSessionButton labels the sidebar new-session row
	// (zcode-shell-specs.md §2.1).
	NewSessionButton = "新建对话"
	// UntitledSession is the fallback title of a session without one.
	UntitledSession = "未命名会话"
	// JustNow labels timestamps below one minute (TaskListItem.tsx:746-772).
	JustNow = "刚刚"
)

// Header copy.
const (
	// ChooseProjectButton opens the native directory picker in the draft
	// header.
	ChooseProjectButton = "选择目录"
	// ChooseProjectTitle is the title of the native directory picker.
	ChooseProjectTitle = "选择项目目录"
	// NoProjectHint shows in the draft header before a directory is picked.
	NoProjectHint = "未选择项目目录"
	// DraftHeaderTitle labels the draft state of the main header.
	DraftHeaderTitle = "新会话"
)

// Composer copy.
const (
	// PlaceholderNewTask is the composer placeholder without history
	// (zcode-chat-specs.md §2.3 branch one).
	PlaceholderNewTask = "描述新任务"
	// PlaceholderFollowUp is the composer placeholder with history
	// (branch two); a running turn keeps this branch (docs/go-migration.md
	// §6.5: 「排队追问」 rides the deferred queue feature).
	PlaceholderFollowUp = "继续追问"
	// SendDisabledHint explains a disabled send button in the native
	// tooltip (button.go SendButton covers the empty-draft case with its
	// own copy; this one names the missing model).
	SendNoModelHint = "请先在模型选择器中选择模型"
	// ManageModelsLabel is the composer entry shown instead of the model
	// selector before any model is configured (ModelConfigSelect's
	// manage-models branch, V4ComposerToolbar.tsx:1033-1041 §2.4 — the
	// entry stays visible to avoid the zero-model dead end); clicking it
	// opens the provider settings.
	ManageModelsLabel = "管理模型"
	// SendFailedToast is the toast prefix when submitting fails.
	SendFailedToast = "发送失败"
	// NoProjectToast blocks a send from the draft state without a project
	// directory.
	NoProjectToast = "请先选择项目目录"
	// BusyToast reports ErrBusy when a send races a running turn.
	BusyToast = "会话正在运行，请先停止"
)

// Permission dialog copy (docs/go-migration.md §6.8; the ask adds the
// session-scope allowance as the middle option).
const (
	// PermissionTitle is the native dialog title.
	PermissionTitle = "工具执行确认"
	// PermissionMessage is the dialog body question.
	PermissionMessage = "该工具会产生副作用，是否允许本次执行？"
	// PermissionAllowOnce is the Enter default (差异 9: Enter=允许).
	PermissionAllowOnce = "允许一次"
	// PermissionAllowSession scopes the allowance to the current session.
	PermissionAllowSession = "本会话允许"
	// PermissionDeny is the Escape target; refusing keeps the turn going
	// with a denied tool card.
	PermissionDeny = "拒绝"
	// PermissionDetailPrefix introduces the request summary in the dialog
	// detail area.
	PermissionDetailPrefix = "请求内容"
)

// Dialog copy.
const (
	// RenameTitle is the rename dialog title.
	RenameTitle = "重命名会话"
	// RenamePromptLabel describes the input field.
	RenamePromptLabel = "会话名称"
	// RenameConfirm is the rename dialog confirm button.
	RenameConfirm = "保存"
	// DeleteTitle is the delete confirm title.
	DeleteTitle = "删除会话"
	// DeleteDescription explains what deleting removes.
	DeleteDescription = "将删除该会话的全部消息记录，且无法恢复。"
	// DeleteConfirm is the destructive confirm button.
	DeleteConfirm = "删除"
	// ErrorDetailTitle is the full-error dialog title.
	ErrorDetailTitle = "错误详情"
	// TurnFailedSummary is the error-banner fallback when a turn failed
	// without a display-safe message.
	TurnFailedSummary = "回合失败"
	// StopFailedToast reports a failed Stop call.
	StopFailedToast = "停止失败"
	// CopyFailedToast is reserved for clipboard failures in later waves.
	CopyFailedToast = "复制失败"
)
