/* eslint-disable max-lines -- 跨端 platform contract 集中声明 renderer 能力；OAuth 与 browser lifecycle 必须保持 desktop/web 类型合同，本 MR 不拆分平台边界。 */
import type {
  DockerConnectOptions,
  RemoteTarget,
  SSHConnectOptions,
  WSLConnectOptions,
} from "./remoteTarget.js";
import type {
  LoadCliMcpFromUserDirectoryRequest,
  LoadCliMcpFromUserDirectoryResult,
  MigrateLegacyCommonMcpRequest,
  MigrateLegacyCommonMcpResult,
  SaveCliMcpToUserDirectoryRequest,
} from "./mcp.js";
import type { AppSettings, Locale } from "./protocol.js";
import type {
  GitWorktreeArchiveCleanupInput,
  GitWorktreeArchiveRecord,
  GitWorktreeCreateInput,
  GitWorktreeCreateResult,
  GitWorktreeHandoffInput,
  GitWorktreeHandoffResult,
  GitWorktreeRemoveInput,
  GitWorktreesResult,
} from "./git.js";
import type { ArmsCustomEventPayload, RendererTelemetryEventPayload } from "./telemetry.js";
import type {
  RendererActionTraceBatchV1,
  RendererActionTraceConfigV1,
} from "./rendererActionTrace.js";
import type { RendererHeapSample } from "./validation.js";
import type {
  UpdateCheckResultPayload,
  UpdateStatePayload,
} from "./update.js";
export type {
  UpdateCheckResultPayload,
  UpdateStatePayload,
} from "./update.js";

export interface TaskNotificationPayload {
  taskId: string;
  status: "completed" | "failed" | "permission_request" | "elicitation_request" | "feedback_update";
  requestId?: string;
  title: string;
  body: string;
}

export type BrowserTabResidencyState =
  | "live-visible"
  | "live-background"
  | "suspend-pending"
  | "suspended"
  | "restoring";

/** Tauri 原生子 WebView 的窗口坐标，使用主 WebView 内容区的 CSS 逻辑像素。 */
export interface NativeBrowserBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * 原生浏览器 child 的业务归属。Rust 不把 tabId 当作授权边界；workspace/session
 * 变化或 browser-use runtime 换代时，旧 child 的事件必须失效。
 */
export interface NativeBrowserOwner {
  workspaceKey: string;
  sessionId: string;
  browserGeneration?: number;
}

/** Rust 为一次真实 child WebView 分配的单调、不复用代次。 */
export interface NativeBrowserOpenResult {
  generation: number;
}

/** 所有 child WebView 控制命令都必须携带创建时的完整身份。 */
export interface NativeBrowserTarget {
  tabId: string;
  owner: NativeBrowserOwner;
  generation: number;
}

/** Rust 子 WebView 回传的导航状态；页面内容仍完全由原生 WebView 绘制。 */
export interface NativeBrowserStateEvent {
  tabId: string;
  owner: NativeBrowserOwner;
  generation: number;
  url: string;
  title?: string;
  /** child WebView 拦截 target=_blank 后通知主界面打开新的浏览器 tab。 */
  kind: "started" | "finished" | "title" | "new-window";
}

export interface NativeBrowserNavigationState {
  canGoBack: boolean;
  canGoForward: boolean;
}

/** 已安装的编辑器/终端信息 */
export interface EditorInfo {
  /** 编辑器标识 (e.g. "vscode", "zed", "terminal") */
  id: string;
  /** 显示名 */
  name: string;
  /** 图标 base64 data URL */
  iconDataUrl: string;
}

export interface ApplicationIconInfo {
  iconDataUrl: string;
}

export type ApplicationIconLocator =
  | { kind: "darwin-bundle-id"; value: string }
  | { kind: "windows-executable-path"; value: string }
  | { kind: "windows-aumid"; value: string };

export interface ApplicationIconRequest {
  locators: ApplicationIconLocator[];
}

export type OpenInEditorRemoteTarget =
  | Pick<SSHConnectOptions, "kind" | "host" | "port" | "username" | "sshConfigAlias">
  | Pick<WSLConnectOptions, "kind" | "distro" | "user">
  | Pick<DockerConnectOptions, "kind" | "container">;

export interface OpenInEditorOptions {
  remoteTarget?: OpenInEditorRemoteTarget;
  workspaceIdentity?: string;
  pathKind?: "file" | "directory";
}

export interface CreateTempTextAttachmentRequest {
  text: string;
  filename?: string;
}

export interface CreateTempTextAttachmentResult {
  filename: string;
  localPath: string;
  mimeType: "text/plain";
  sizeBytes: number;
}

export type SaveFileRequest =
  | {
      data: ArrayBuffer;
      sourceUrl?: never;
      suggestedName: string;
    }
  | {
      data?: never;
      sourceUrl: string;
      suggestedName: string;
    };

export interface SaveFileResult {
  canceled?: boolean;
  error?: string;
  path?: string;
  success: boolean;
}

export interface PrintPageToPdfResult {
  success: boolean;
  /** PDF 字节；success 时存在 */
  data?: ArrayBuffer;
  /** "print_in_progress" | "print_failed" */
  error?: string;
}

/** Tauri 原生文件拖放的路径投影；坐标已换算为 renderer CSS 像素。 */
export interface NativeFileDropEvent {
  paths: string[];
  position: { x: number; y: number };
}

/** 主窗口有活动任务时，宿主请求 renderer 展示退出确认。 */
export interface ExitRequestedPayload {
  activeCount: number;
}

/** 系统托盘中由 renderer 投影的会话入口。 */
export interface TraySessionEntry {
  id: string;
  title: string;
}

/** 系统托盘固定项和最近会话的本地化投影。 */
export interface TrayMenuPayload {
  labels: {
    newChat: string;
    show: string;
    quit: string;
  };
  sessions: TraySessionEntry[];
}

export function createOpenInEditorRemoteTarget(target: RemoteTarget): OpenInEditorRemoteTarget {
  switch (target.kind) {
    case "ssh":
      // openInEditor 只需要构造 VS Code Remote-SSH URI 的连接标识，
      // 不应该把 password/privateKeyPassphrase 等凭据字段继续穿过 renderer/preload/main IPC。
      return {
        kind: "ssh",
        host: target.host,
        port: target.port,
        username: target.username,
        ...(target.sshConfigAlias?.trim() ? { sshConfigAlias: target.sshConfigAlias.trim() } : {}),
      };
    case "wsl": {
      const user = target.user?.trim();
      return {
        kind: "wsl",
        distro: target.distro,
        ...(user ? { user } : {}),
      };
    }
    case "docker":
      return {
        kind: "docker",
        container: target.container,
      };
  }
}

export interface WSLDistro {
  name: string;
  isDefault: boolean;
  state: string;
  version: 1 | 2 | null;
}

export interface DockerContainerInfo {
  id: string;
  image: string;
  name: string;
  state: string;
  status: string;
}

export interface SSHConfigAliasOption {
  alias: string;
  host?: string;
  port?: number;
  username?: string;
  privateKeyPath?: string;
  source?: string;
}

export interface ZCodeStdioTapDevState {
  enabled: boolean;
  visible: boolean;
  logDir: string;
  statePath: string;
}

export type DesktopTitleBarTheme = "light" | "dark" | "system";

export interface WindowScreenshotResult {
  dataBase64: string;
  filename: string;
  contentType: string;
  size: number;
}

export type ChromeBrowserDataImportError =
  | "chrome_profile_not_found"
  | "chrome_profile_ambiguous"
  | "chrome_executable_not_found"
  | "chrome_cookie_access_denied"
  | "chrome_cookie_elevation_required"
  | "chrome_cookie_elevation_cancelled"
  | "chrome_cookie_helper_verification_failed"
  | "chrome_cookie_app_bound_decryption_failed"
  | "chrome_cookie_protection_unsupported"
  | "chrome_profile_locked"
  | "chrome_local_storage_import_failed"
  | "chrome_import_not_supported"
  | "chrome_browser_data_import_unavailable"
  | "chrome_data_import_failed"
  | "chrome_default_profile_not_found";

export interface ChromeBrowserDataImportOptions {
  /** Windows App-Bound Cookie 只能在本次显式确认后触发 UAC；不得持久化为全局授权。 */
  allowElevatedChromeDecryption?: boolean;
}

/** Chrome 浏览器数据导入只返回数量和状态；Cookie/LocalStorage 值和解密材料不得跨进程。 */
export interface ChromeBrowserDataImportResult {
  success: boolean;
  cookies: {
    imported: number;
    skipped: number;
    failed: number;
  };
  localStorage: {
    originsImported: number;
    entriesImported: number;
    originsSkipped: number;
    originsFailed: number;
    error?: ChromeBrowserDataImportError;
  };
  /** 部分成功时保留可操作问题；不得包含 Profile 绝对路径或站点数据。 */
  issues?: ChromeBrowserDataImportError[];
  error?: ChromeBrowserDataImportError;
}

export interface EmbeddedBrowserDataClearResult {
  success: boolean;
  error?: string;
}

export interface WindowControlsOverlayMetrics {
  leftPaddingPx?: number;
  rightPaddingPx?: number;
  titleBarHeightPx?: number;
}

export interface WindowControlsOverlayReadyPayload {
  zoomLevel: number;
  metrics: WindowControlsOverlayMetrics;
}

export interface DesktopZoomState {
  zoomLevel: number;
}

export interface DesktopWindowChromeState {
  isMaximized: boolean;
  /** 本机 macOS 主版本；非 macOS 或无法解析时为 null。 */
  macOSMajorVersion?: number | null;
  supportsNativeRoundedCorners: boolean;
}

export interface RemoteServiceSession {
  sessionId: string;
}

export interface RemoteConnectionRuntimeLog {
  label: string;
  requestId?: string;
  sessionId?: string;
  level: "info" | "warn" | "error";
  source: string;
  message: string;
  timestamp: string;
}

export interface RemoteSessionClosedEvent {
  sessionId: string;
  reason: "host-exit";
  exitCode: number | null;
  signal: string | null;
}

export interface EmbeddedBrowserOpenUrlRequest {
  url: string;
  disposition: "default" | "foreground-tab" | "background-tab" | "new-window" | "other";
  /** 触发 popup 的 browser-use tab 归属；旧版事件缺失时由 renderer 按当前 scope 兼容处理。 */
  workspaceKey?: string;
  remoteSessionId?: string;
  sessionId?: string;
  browserId?: string;
  browserGeneration?: number;
  sourceTabId?: string;
}

export interface BotRemoteWorkspaceReconnectedEvent {
  sessionId: string;
  workspacePath: string;
  workspaceIdentity: string;
  target: RemoteTarget;
}

export interface ConnectRemoteRequest {
  target: RemoteTarget;
  requestId?: string;
  workspacePath?: string;
  workspaceIdentity?: string;
  connectTrigger?: import("./remoteUsageTelemetry.js").RemoteWorkspaceConnectTrigger;
}

export interface CancelPendingRemoteConnectionRequest {
  requestId?: string;
}

export interface BindRemoteWorkspaceSessionContextRequest {
  remoteSessionId: string;
  workspacePath: string;
  workspaceIdentity?: string;
}

export const DesktopCommandIds = {
  NewTask: "newTask",
  OpenWorkspace: "openWorkspace",
  CloseActiveContext: "closeActiveContext",
  CloseWindow: "closeWindow",
  MinimizeWindow: "minimizeWindow",
  ToggleMaximizeWindow: "toggleMaximizeWindow",
  ToggleFullScreen: "toggleFullScreen",
  ResetWindowSize: "resetWindowSize",
  ResetZoom: "resetZoom",
  ZoomIn: "zoomIn",
  ZoomOut: "zoomOut",
  ShowAbout: "showAbout",
  OpenChangelog: "openChangelog",
  CheckForUpdates: "checkForUpdates",
  RelaunchApp: "relaunchApp",
  ExportLogs: "exportLogs",
  ToggleDevTools: "toggleDevTools",
  OpenResourceManager: "openResourceManager",
  ToggleZCodeStdioTapDevProxy: "toggleZCodeStdioTapDevProxy",
  SetZCodeEndpointProduction: "setZCodeEndpointProduction",
  SetZCodeEndpointTest: "setZCodeEndpointTest",
  SetZCodeEndpointCustom: "setZCodeEndpointCustom",
  ResetZCodeEndpoint: "resetZCodeEndpoint",
  ClearAllData: "clearAllData",
  ClearCodingPlanWebviewStorage: "clearCodingPlanWebviewStorage",
} as const;

export type DesktopCommandId = (typeof DesktopCommandIds)[keyof typeof DesktopCommandIds];

/**
 * 平台操作接口 —— 替代直接访问 window.zcode
 *
 * 定义需要宿主环境（Electron main / Web server）参与的操作。
 * Desktop 和 Web 各自提供不同的实现，UI 层通过此接口统一消费。
 *
 * 设计原则：只放"必须穿越进程边界且不适合做成 RPC service"的操作，
 * 比如 native dialog、窗口生命周期控制等。
 * 业务服务（文件、终端、凭据等）走 IServiceAccessor 的 RPC 通道。
 */
export interface IPlatformService {
  /** 当前平台的文件选择框是否能返回 agent 可访问的本地绝对路径 */
  canSelectFilePath?: boolean;

  /** 打开系统目录选择框，返回选中路径或 null */
  selectDirectory(): Promise<string | null>;

  /** 打开系统文件选择框，返回选中文件路径或 null */
  selectFile(): Promise<string | null>;

  /** 打开系统多文件选择框，返回选中的文件路径；取消时返回空数组 */
  selectFiles?(): Promise<string[]>;

  /** 使用宿主原生另存为对话框写入文件；普通 Web 端不实现 */
  saveFile?(payload: SaveFileRequest): Promise<SaveFileResult>;

  /**
   * 用 Chromium 打印引擎把当前 webContents 的 print 媒体版面输出为 PDF（矢量文本）。
   * 页面尺寸由 renderer 注入的 @page CSS 决定（preferCSSPageSize）；仅 Desktop 实现。
   */
  printPageToPdf?(): Promise<PrintPageToPdfResult>;

  /**
   * 从浏览器 File 对象解析宿主本地路径；只有 Desktop preload 能安全实现。
   * Web/手机端返回 null，避免 UI 层依赖 Electron 的非标准 File.path。
   */
  getPathForFile?(file: unknown): string | null;

  /** 订阅宿主原生文件拖放；路径与坐标均由 Tauri 提供，普通 Web 端不实现。 */
  onNativeFileDrop?(handler: (event: NativeFileDropEvent) => void): () => void;

  /**
   * 在宿主 ~/.zcode 临时目录创建文本附件文件。
   * 手机远控必须通过 shared-host/platform proxy 写到桌面宿主，避免大文本进入 prompt payload。
   */
  createTempTextAttachment?(
    payload: CreateTempTextAttachmentRequest,
  ): Promise<CreateTempTextAttachmentResult>;

  /** 检查目录是否已在其他窗口打开；如果是则激活该窗口并切到对应 tab */
  activateOrSetWorkspace(path: string): Promise<{ activated: boolean }>;

  /** 读取本地 Git linked worktree；非 Desktop 平台不提供此本地能力。 */
  listGitWorktrees?(projectPath: string): Promise<GitWorktreesResult>;

  /** 创建并登记由 KeenCode 管理的 linked worktree。 */
  createGitWorktree?(input: GitWorktreeCreateInput): Promise<GitWorktreeCreateResult>;

  /** 将已授权 Session 在主 checkout 与 linked worktree 间交接。 */
  handoffGitWorkspace?(
    input: GitWorktreeHandoffInput,
  ): Promise<GitWorktreeHandoffResult>;

  /** 关闭待交接 Session 的执行资源；历史 Journal 保留。 */
  stopGitWorkspaceSession?(sessionId: string): Promise<void>;

  /** 仅删除通过 Rust 身份校验的 linked worktree。 */
  removeGitWorktree?(input: GitWorktreeRemoveInput): Promise<void>;

  /** 使用归档 Journal 回执清理 managed worktree。 */
  archiveGitWorktree?(input: GitWorktreeArchiveCleanupInput): Promise<void>;

  /** 查询可恢复的 managed worktree 归档记录。 */
  listGitWorktreeArchiveRecords?(): Promise<GitWorktreeArchiveRecord[]>;

  /** 按归档记录恢复同一 managed worktree。 */
  recoverGitWorktree?(sessionId: string): Promise<boolean>;

  /** 建立远程连接（Desktop: 在当前窗口创建远程 session；Web: HTTP API） */
  connectRemote(
    options: RemoteTarget,
    requestId?: string,
    context?: {
      workspacePath: string;
      workspaceIdentity?: string;
      connectTrigger?: import("./remoteUsageTelemetry.js").RemoteWorkspaceConnectTrigger;
    },
  ): Promise<{ success: boolean; error?: string; sessionId?: string }>;

  /** 取消当前窗口尚未建立完成的远程连接（可选：Web 平台可忽略） */
  cancelPendingRemoteConnection?(requestId?: string): Promise<void>;

  /** 将 canonical workspace 身份绑定到已创建的远程 logical session。 */
  bindRemoteWorkspaceSessionContext?(
    context: BindRemoteWorkspaceSessionContextRequest,
  ): Promise<void>;

  /** 释放当前窗口里已创建的远程 session */
  disposeRemoteSession(sessionId: string): Promise<void>;

  /** 检查本机 Docker daemon 是否可用 */
  isDockerAvailable(): Promise<boolean>;

  /** 列出本机可用的 WSL 发行版 */
  listWSLDistros(): Promise<WSLDistro[]>;

  /** 列出当前可连接的 Docker 容器 */
  listDockerContainers(): Promise<DockerContainerInfo[]>;

  /** 列出当前机器 SSH config 中可用于快速填表的 alias */
  listSSHConfigAliases(): Promise<SSHConfigAliasOption[]>;

  /** 读取宿主环境中的原生 MCP 用户目录配置；手机远控通过已连接桌面 host 转发。 */
  loadMcpFromUserDirectory?(
    payload?: LoadCliMcpFromUserDirectoryRequest,
  ): Promise<LoadCliMcpFromUserDirectoryResult>;

  /** 写入宿主环境中的原生 MCP 用户目录配置；普通 Web 没有宿主时返回 unsupported。 */
  saveMcpToUserDirectory?(
    payload: SaveCliMcpToUserDirectoryRequest,
  ): Promise<{ success: boolean; error?: string }>;

  /** 迁移旧版 Common MCP 配置；仅宿主环境可执行，手机远控通过 desktop attachment 转发。 */
  migrateLegacyCommonMcp?(
    payload?: MigrateLegacyCommonMcpRequest,
  ): Promise<MigrateLegacyCommonMcpResult>;

  /** 打开外部 URL（用于 OAuth 跳转浏览器） */
  openExternal(url: string): void;

  /** 按系统应用标识读取真实 App 图标；非 Desktop 平台可不实现。 */
  getApplicationIcon?(
    request: string | ApplicationIconRequest,
  ): Promise<ApplicationIconInfo | null>;

  /** 在系统文件管理器中打开指定路径 */
  openInFileManager(path: string): Promise<{ success: boolean; error?: string }>;

  /** 使用系统默认应用打开本地文件；普通 Web 平台返回 unsupported。 */
  openExternalFile?(path: string): Promise<{ success: boolean; error?: string }>;

  /** 注册 `zcode://share/import?code=...` 导入意图。 */
  onShareImport?(callback: (payload: { shareCode: string }) => void): () => void;

  /** 通知 main process renderer 已就绪，触发缓存的冷启动 deep link 转发 */
  notifyRendererReady(): void;

  /** 触发任务状态对应的系统通知，由宿主环境决定是否真正展示 */
  showTaskNotification(payload: TaskNotificationPayload): void;

  /** 通过宿主环境统一上报 UI 侧 telemetry 事件 */
  reportTelemetryEvent(payload: RendererTelemetryEventPayload): Promise<void>;

  /** 通过宿主环境上报 ARMS 自定义事件；Web 端当前为空实现 */
  reportArmsCustomEvent(payload: ArmsCustomEventPayload): Promise<void>;

  /** 读取 Desktop Renderer 用户操作 Trace 的当前灰度配置；Web/手机不实现。 */
  getRendererActionTraceConfig?(): Promise<RendererActionTraceConfigV1>;
  /** 订阅 Main 推送的 Renderer 用户操作 Trace 配置；Web/手机不实现。 */
  onRendererActionTraceConfigChanged?(
    callback: (config: RendererActionTraceConfigV1) => void,
  ): () => void;
  /** Renderer → Main：发送已结束的 ui_action batch；严格旁路、fire-and-forget。 */
  reportRendererActionTraceBatch?(batch: RendererActionTraceBatchV1): void;
  reportLocalTtftBatch?(batch: import("./localTtft.js").LocalTtftBatch): void;

  /**
   * Renderer → Main：主窗口 renderer 每 60 秒的 heap 读数，进 `renderer_main` 角色事件。单向 send、fire-and-forget；
   * Web 端与手机远控没有桥，不实现即 no-op。
   */
  reportRendererHeapSample?(sample: RendererHeapSample): void;

  /** 同步当前窗口的未读 task 数给宿主环境，用于 Dock / 任务栏徽标聚合 */
  syncWindowUnreadCount(count: number): void;

  /** 同步需要 main 进程即时感知的应用设置；Web fallback 可忽略 */
  syncAppSettings?(patch: Partial<AppSettings>): void;

  /** 快捷键设置页录制态开关；桌面端 main 据此暂时摘除可配置菜单 accelerator，Web 可忽略 */
  setShortcutRecordingActive?(active: boolean): void;

  /** 注册 main 进程请求关闭当前上下文的回调，返回 disposer */
  onCloseActiveContextRequest?(handler: () => void): () => void;

  /** 主窗口关闭时由宿主请求 renderer 确认仍在运行的本地任务。 */
  onExitRequested?(handler: (payload: ExitRequestedPayload) => void): () => void;

  /** renderer 确认停止本地任务并退出应用。 */
  confirmExit?(): Promise<void>;

  /** 注册 main 进程触发新建任务的回调，返回 disposer */
  onNewTask(handler: () => void): () => void;

  /** 注册托盘会话打开请求，返回 disposer。 */
  onTrayOpenSession?(handler: (payload: { sessionId: string }) => void): () => void;

  /** 将当前 renderer 已确定的会话菜单投影到系统托盘。 */
  setTrayMenu?(payload: TrayMenuPayload): Promise<void>;

  /** 注册 main 进程通过 deep link 直接打开本地工作区目录的回调，返回 disposer */
  onOpenWorkspacePath?(handler: (path: string) => void): () => void;

  /** 注册窗口全屏状态变化回调，返回 disposer */
  onWindowFullscreenChanged(handler: (isFullscreen: boolean) => void): () => void;

  /** 读取当前桌面窗口最大化状态与系统原生圆角能力 */
  getDesktopWindowChromeState?(): Promise<DesktopWindowChromeState>;

  /** 注册桌面窗口最大化状态与系统原生圆角能力变化回调 */
  onDesktopWindowChromeStateChanged?(
    handler: (state: DesktopWindowChromeState) => void,
  ): () => void;

  /** 同步读取当前原生窗口控制区安全边距，用于首屏启动态初始化 */
  getWindowControlsOverlayMetrics?(): WindowControlsOverlayMetrics | null;

  /** 注册原生窗口控制区安全边距变化回调，返回 disposer */
  onWindowControlsOverlayChanged?(
    handler: (metrics: WindowControlsOverlayMetrics) => void,
  ): () => void;

  /** 同步读取当前桌面窗口页面缩放档位；Web fallback 可返回默认 0 */
  getDesktopZoomLevel?(): Promise<DesktopZoomState>;

  /** 注册当前桌面窗口页面缩放档位变化回调，返回 disposer */
  onDesktopZoomLevelChanged?(handler: (state: DesktopZoomState) => void): () => void;

  /** 导出已脱敏、有界的诊断 JSON；Desktop 通过原生另存为返回实际路径。 */
  exportLogs(): Promise<{ success: boolean; path?: string; error?: string }>;

  /** 截取当前窗口，用于错误反馈携带现场画面；Web fallback 可返回 null */
  captureWindowScreenshot?(): Promise<WindowScreenshotResult | null>;

  /**
   * Tauri 桌面使用原生子 WebView 承载内置浏览器；普通 Web/Electron 平台不实现。
   * 这些方法只管理宿主窗口和导航，不复制页面状态到前端。
   */
  nativeBrowserOpen?(payload: {
    tabId: string;
    url: string;
    bounds: NativeBrowserBounds;
    owner: NativeBrowserOwner;
  }): Promise<NativeBrowserOpenResult>;
  nativeBrowserBounds?(payload: NativeBrowserTarget & { bounds: NativeBrowserBounds }): Promise<void>;
  nativeBrowserShow?(target: NativeBrowserTarget): Promise<void>;
  nativeBrowserHide?(target: NativeBrowserTarget): Promise<void>;
  nativeBrowserClose?(target: NativeBrowserTarget): Promise<void>;
  nativeBrowserNavigate?(target: NativeBrowserTarget, url: string): Promise<void>;
  /** 将 native child WebView 的页面缩放限制在 Rust 校验的安全范围内。 */
  nativeBrowserZoom?(target: NativeBrowserTarget, zoom: number): Promise<void>;
  nativeBrowserReload?(target: NativeBrowserTarget): Promise<void>;
  nativeBrowserHistory?(target: NativeBrowserTarget, direction: "back" | "forward"): Promise<void>;
  nativeBrowserNavigationState?(target: NativeBrowserTarget): Promise<NativeBrowserNavigationState>;
  nativeBrowserReset?(): Promise<void>;
  onNativeBrowserState?(handler: (event: NativeBrowserStateEvent) => void): () => void;

  /** 从自动发现的本机 Chrome Profile 一次性导入 Cookie 与 LocalStorage。 */
  importChromeBrowserData?(
    options?: ChromeBrowserDataImportOptions,
  ): Promise<ChromeBrowserDataImportResult>;

  /** 清理内置浏览器持久化分区；cache 模式保留认证数据，all 模式清理全部站点数据。 */
  clearEmbeddedBrowserData?(mode: "cache" | "all"): Promise<EmbeddedBrowserDataClearResult>;

  /** 注册新版本已下载完毕的回调，参数为新版本号，返回 disposer */
  onUpdateReady(callback: (version: string) => void): () => void;

  /** 注册"手动检查更新"结果的回调（用于 toast 反馈），返回 disposer */
  onUpdateCheckResult(callback: (payload: UpdateCheckResultPayload) => void): () => void;

  /** 注册自动更新持续状态变化的回调，返回 disposer */
  onUpdateStateChanged?(callback: (payload: UpdateStatePayload) => void): () => void;

  /** 主动读取当前自动更新状态，用于菜单打开时补偿异步事件丢失 */
  getUpdateState?(): Promise<UpdateStatePayload>;

  /** 用户在更新弹窗中确认开始下载当前已发现版本 */
  downloadUpdate(): Promise<void>;

  /** 用户在更新弹窗中取消当前下载中的更新；宿主未提供取消命令时不实现。 */
  cancelUpdateDownload?(): Promise<void>;

  /** 打开桌面端独立更新窗口；非桌面端可不实现并回退到内嵌弹窗 */
  openUpdateStatusWindow?(): Promise<void>;

  /** 用户跳过当前已发现版本；宿主未提供持久化命令时不实现。 */
  skipUpdateVersion?(version: string): Promise<void>;

  /** 查询桌面端当前正在运行的会话数量；非桌面端可返回 0 */
  getDesktopSessionActivity?(): Promise<{
    runningAgentSessionCount: number;
  }>;

  /** 开发环境 stdio tap proxy 开关状态；非桌面平台可不实现 */
  getZCodeStdioTapDevState?(): Promise<ZCodeStdioTapDevState>;

  /** 是否为本地开发运行形态；桌面端用 !app.isPackaged 注入，Web 端可省略。 */
  isLocalDevelopmentRuntime?: boolean;

  /** 设置文件由桌面菜单命令更新后的通知，renderer 用于刷新 settings snapshot。 */
  onSettingsChanged?(callback: () => void): () => void;

  /** 桌面主进程解析后的应用语言变化；独立轻量窗口没有 settingService 时使用。 */
  onApplicationLocaleChanged?(callback: (locale: Locale) => void): () => void;

  /** 宿主系统语言；桌面端由 main 进程读取，Web 端可回退到 navigator.language。 */
  getSystemLocale?(): Promise<Locale>;

  /** 用户确认重启安装更新 */
  quitAndInstallUpdate(): Promise<void>;

  /** 获取系统中已安装的编辑器/终端列表（含图标） */
  getInstalledEditors(): Promise<EditorInfo[]>;

  /** 用指定编辑器打开路径 */
  openInEditor(
    editorId: string,
    path: string,
    options?: OpenInEditorOptions,
  ): Promise<{ success: boolean; error?: string }>;

  /** 执行桌面窗口级命令（标题栏菜单、缩放、窗口控制等）。 */
  executeDesktopCommand(command: DesktopCommandId): Promise<unknown>;

  /** 同步应用菜单语言，驱动 main 进程重建原生菜单 */
  setApplicationLocale(locale: Locale): Promise<void>;

  /** 同步桌面标题栏亮/暗色；Tauri 当前未提供原生窗口主题命令时不实现。 */
  setTitleBarTheme?(theme: DesktopTitleBarTheme): Promise<void>;

  /** 获取当前设备的稳定标识符
   *
   * - 桌面端：基于 userData 路径的 SHA-256，始终稳定且唯一
   * - 手机端（Web 远程控制）：物理属性指纹（browserPlatform | screen.width | screen.height | colorDepth），
   *   抗浏览器/网络/语言/时区变化，换手机才会变
   */
  getDeviceId(): string;
}
