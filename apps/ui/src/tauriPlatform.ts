import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview, type DragDropEvent } from "@tauri-apps/api/webview";
import {
  type DesktopWindowChromeState,
  type DesktopZoomState,
  type ApplicationIconInfo,
  type ApplicationIconRequest,
  type EditorInfo,
  type GitWorktreeArchiveCleanupInput,
  type GitWorktreeArchiveRecord,
  type GitWorktreeCreateInput,
  type GitWorktreeCreateResult,
  type GitWorktreeHandoffInput,
  type GitWorktreeHandoffResult,
  type GitWorktreeRemoveInput,
  type GitWorktreesResult,
  type IPlatformService,
  type Locale,
  type NativeFileDropEvent,
  type NativeBrowserOpenResult,
  type NativeBrowserNavigationState,
  type NativeBrowserStateEvent,
  type PrintPageToPdfResult,
  type ResourceUsageSnapshot,
  type SaveFileRequest,
  type SaveFileResult,
  type StorageCleanRequest,
  type StorageCleanResult,
  type StorageManagementBridge,
  type StorageUsageSnapshot,
  type UpdateCheckResultPayload,
  type UpdateStatePayload,
} from "@zcode/shared";
import {
  formatDiagnosticArgs,
  reportRendererError,
  setRendererCrashSink,
  setRendererDiagnosticsSink,
} from "@zcode/ui";

type EventHandler<T> = (payload: T) => void;

interface TauriUpdateStatusPayload {
  currentVersion: string;
  currentRelease: string;
  checked: boolean;
  available: boolean;
  latestVersion?: string;
  latestRelease?: string;
  notes?: string;
  publishedAt?: string;
  downloadState: "idle" | "downloading" | "verifying" | "ready" | "installing" | "failed";
  downloadedBytes: number;
  totalBytes?: number;
  downloadSource?: "auto" | "github" | "chinaMirror";
  downloadError?: string;
}

function listenTauriEvent<T>(name: string, handler: EventHandler<T>): () => void {
  let disposed = false;
  let unlisten: UnlistenFn | undefined;

  const release = (remove: UnlistenFn) => {
    try {
      remove();
    } catch (error) {
      reportRendererError(error, `[tauri-event] ${name} unlisten failed`);
    }
  };

  void listen<T>(name, (event) => handler(event.payload)).then((dispose) => {
    if (disposed) {
      release(dispose);
    } else {
      unlisten = dispose;
    }
  }, (error: unknown) => {
    if (!disposed) {
      reportRendererError(error, `[tauri-event] ${name} listen failed`);
    }
  });
  return () => {
    if (disposed) return;
    disposed = true;
    const remove = unlisten;
    unlisten = undefined;
    if (remove) release(remove);
  };
}

/**
 * Tauri drop 事件提供的是宿主真实路径；这里只把物理坐标换成 renderer CSS 像素，
 * 具体哪个可见控件接收路径由 UI 自己按 DOM 边界判定，避免全窗口误填来源。
 */
function listenNativeFileDrop(handler: (event: NativeFileDropEvent) => void): () => void {
  let disposed = false;
  let unlisten: UnlistenFn | undefined;
  const release = (remove: UnlistenFn) => {
    try {
      remove();
    } catch (error) {
      reportRendererError(error, "[tauri-drop] unlisten failed");
    }
  };

  void getCurrentWebview()
    .onDragDropEvent((event: { payload: DragDropEvent }) => {
      if (disposed) return;
      const payload = event.payload;
      if (payload.type !== "drop" || payload.paths.length === 0) return;
      const devicePixelRatio =
        typeof window !== "undefined" && Number.isFinite(window.devicePixelRatio)
          ? Math.max(window.devicePixelRatio, 1)
          : 1;
      handler({
        paths: [...payload.paths],
        position: {
          x: payload.position.x / devicePixelRatio,
          y: payload.position.y / devicePixelRatio,
        },
      });
    })
    .then((remove) => {
      if (disposed) {
        release(remove);
      } else {
        unlisten = remove;
      }
    })
    .catch((error: unknown) => {
      if (!disposed) reportRendererError(error, "[tauri-drop] listen failed");
    });

  return () => {
    if (disposed) return;
    disposed = true;
    const remove = unlisten;
    unlisten = undefined;
    if (remove) release(remove);
  };
}

/** 资源管理器只在主窗口消费；数据读取仍由 Rust 诊断与磁盘扫描命令提供。 */
export function listenResourceManagerOpen(handler: () => void): () => void {
  return listenTauriEvent("keencode://resource-manager-open", handler);
}

export function createTauriResourceManagerBridge(): {
  getSnapshot: () => Promise<ResourceUsageSnapshot>;
  storage: StorageManagementBridge;
} {
  const storage: StorageManagementBridge = {
    startScan: () =>
      invokeCommand<{ jobId: string }>("desktop_resource_manager_storage_start_scan"),
    cancelScan: (jobId: string) =>
      invokeCommand<void>("desktop_resource_manager_storage_cancel_scan", { jobId }),
    getSnapshot: () =>
      invokeCommand<StorageUsageSnapshot | null>(
        "desktop_resource_manager_storage_get_snapshot",
      ),
    clean: (request: StorageCleanRequest) =>
      invokeCommand<StorageCleanResult>("desktop_resource_manager_storage_clean", { request }),
    subscribeScanProgress: (handler: (snapshot: StorageUsageSnapshot) => void) =>
      listenTauriEvent<StorageUsageSnapshot>(
        "keencode://resource-manager-storage-progress",
        handler,
      ),
    revealPath: (path: string) =>
      invokeCommand<void>("desktop_resource_manager_storage_reveal_path", { path }),
  };
  return {
    getSnapshot: () => invokeCommand<ResourceUsageSnapshot>("desktop_resource_usage_snapshot"),
    storage,
  };
}

function unsupported<T>(capability: string): Promise<T> {
  return Promise.reject(new Error(`${capability} 当前由本地 KeenCode Host 以外的宿主提供`));
}

function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args);
}

function updateReleaseNotes(status: TauriUpdateStatusPayload) {
  if (!status.latestVersion || !status.notes) return undefined;
  return {
    version: status.latestVersion,
    title: status.latestRelease || status.latestVersion,
    markdown: status.notes,
    ...(status.publishedAt ? { releaseDate: status.publishedAt } : {}),
  };
}

function toUpdateState(status: TauriUpdateStatusPayload): UpdateStatePayload {
  const releaseNotes = updateReleaseNotes(status);
  const version = status.latestVersion || status.currentVersion;
  const progress =
    typeof status.totalBytes === "number" && status.totalBytes > 0
      ? String(Math.min(100, Math.round((status.downloadedBytes / status.totalBytes) * 100)))
      : "0";
  const progressState = {
    kind: "download-progress" as const,
    enabled: true,
    progress,
    transferredBytes: status.downloadedBytes,
    ...(status.totalBytes !== undefined ? { totalBytes: status.totalBytes } : {}),
    ...(status.latestVersion ? { version: status.latestVersion } : {}),
    ...(releaseNotes ? { releaseNotes } : {}),
  };

  if (
    status.available &&
    (status.downloadState === "downloading" ||
      status.downloadState === "verifying" ||
      status.downloadState === "installing")
  ) {
    return progressState;
  }
  if (status.available && status.downloadState === "ready" && status.latestVersion) {
    return {
      kind: "update-downloaded",
      enabled: true,
      version: status.latestVersion,
      ...(releaseNotes ? { releaseNotes } : {}),
    };
  }
  if (status.available && status.latestVersion) {
    return {
      kind: "update-available",
      enabled: true,
      version: status.latestVersion,
      ...(releaseNotes ? { releaseNotes } : {}),
    };
  }
  return { kind: "idle", enabled: true };
}

function listenUpdateStatus(handler: (status: TauriUpdateStatusPayload) => void): () => void {
  return listenTauriEvent<TauriUpdateStatusPayload>("app://update-status", handler);
}

function isWindowsTauriRuntime(): boolean {
  if (typeof navigator === "undefined") return false;
  return /Windows/i.test(`${navigator.userAgent} ${navigator.platform}`);
}

/**
 * Rust 通过 Tauri JSON 返回 Vec<u8> 数组；必须逐字节校验后再组装 ArrayBuffer，
 * 否则直接把数组交给 Blob 会按逗号连接，生成不可读的 PDF。
 */
function normalizePrintPageToPdfResult(value: unknown): PrintPageToPdfResult {
  if (value === null || typeof value !== "object") {
    throw new Error("PDF 打印返回值不是对象");
  }
  const payload = value as {
    success?: unknown;
    data?: unknown;
    error?: unknown;
  };
  if (typeof payload.success !== "boolean") {
    throw new Error("PDF 打印返回值缺少 success");
  }
  if (!payload.success) {
    return {
      success: false,
      ...(typeof payload.error === "string" ? { error: payload.error } : {}),
    };
  }
  if (!Array.isArray(payload.data)) {
    throw new Error("PDF 打印成功但未返回字节数组");
  }
  const bytes = new Uint8Array(payload.data.length);
  for (let index = 0; index < payload.data.length; index += 1) {
    const byte = payload.data[index];
    if (!Number.isInteger(byte) || byte < 0 || byte > 255) {
      throw new Error(`PDF 打印返回了非法字节（索引 ${index}）`);
    }
    bytes[index] = byte;
  }
  return { success: true, data: bytes.buffer };
}

const PRINT_HOST_ATTRIBUTE = "data-zcode-pptx-print-host";
const PRINT_WIDTH_ATTRIBUTE = "data-keencode-print-width-px";
const PRINT_HEIGHT_ATTRIBUTE = "data-keencode-print-height-px";
const MIN_PRINT_PAGE_SIZE_PX = 96;
const MAX_PRINT_PAGE_SIZE_PX = 16_384;

function readPrintPageSizeFromHost(): { widthPx: number; heightPx: number } | undefined {
  if (typeof document === "undefined") {
    return undefined;
  }
  const host = document.querySelector<HTMLElement>(`[${PRINT_HOST_ATTRIBUTE}]`);
  if (!host) {
    return undefined;
  }

  const readDimension = (attribute: string): number => {
    const raw = host.getAttribute(attribute);
    const value = raw === null ? Number.NaN : Number(raw);
    if (
      !Number.isFinite(value) ||
      value < MIN_PRINT_PAGE_SIZE_PX ||
      value > MAX_PRINT_PAGE_SIZE_PX
    ) {
      throw new Error(`PDF 打印页面尺寸无效：${attribute}`);
    }
    return value;
  };

  return {
    widthPx: readDimension(PRINT_WIDTH_ATTRIBUTE),
    heightPx: readDimension(PRINT_HEIGHT_ATTRIBUTE),
  };
}

function normalizeExitRequestedPayload(value: unknown): { activeCount: number } | undefined {
  if (value === null || typeof value !== "object") return undefined;
  const activeCount = (value as { activeCount?: unknown }).activeCount;
  return typeof activeCount === "number" && Number.isInteger(activeCount) && activeCount > 0
    ? { activeCount }
    : undefined;
}

function normalizeTrayOpenSessionPayload(value: unknown): { sessionId: string } | undefined {
  if (value === null || typeof value !== "object") return undefined;
  const sessionId = (value as { sessionId?: unknown }).sessionId;
  if (typeof sessionId !== "string" || sessionId.trim().length === 0) return undefined;
  return { sessionId: sessionId.trim() };
}

/**
 * Tauri 平台适配只负责系统对话框、窗口和事件边界；文件、会话、模型与工作流
 * 业务均通过 RemoteServiceAccess 的 RPC 服务访问，避免前端再维护一套状态事实源。
 */
export function createTauriPlatform(): IPlatformService {
  let browserResetFailure: unknown;
  // 记录启动清理的完成状态；首次创建 child 前显式等待，避免旧 WebView 仍在
  // 主页面上方时 renderer 已开始工作。失败保留给后续 open 传播，不转成静默成功。
  const browserResetReady = invokeCommand<void>("browser_reset").catch((error: unknown) => {
    browserResetFailure = error;
  });
  setRendererDiagnosticsSink((level, args) => {
    // lifecycle info 走 Rust 的性能观测入口，warn/error 走诊断错误入口；按级别
    // 分流，避免把正常生命周期提升为错误，也不重新打开普通消息流日志。
    const message = `${level}: ${formatDiagnosticArgs(args)}`.slice(0, 4000);
    if (level === "info") {
      void invokeCommand<void>("performance_record", {
        component: "renderer.lifecycle",
        message,
      }).catch((error) => {
        console.error("[tauri-diagnostics] performance_record failed", error);
      });
      return;
    }
    void invokeCommand<void>("diagnostics_record", {
      component: `renderer.${level}`,
      message: message.slice(0, 4000),
    }).catch((error) => {
      // 诊断命令自身失败时只保留一条低频控制台摘要，避免错误链路递归调用 logger。
      console.error("[tauri-diagnostics] diagnostics_record failed", error);
    });
  });
  setRendererCrashSink((error, context) => {
    void invokeCommand<void>("diagnostics_crash_record", {
      kind: context ?? "renderer",
      message: `${error.name}: ${error.message}${error.stack ? `\n${error.stack}` : ""}`.slice(0, 4000),
    }).catch((cause) => {
      console.error("[tauri-diagnostics] diagnostics_crash_record failed", cause);
    });
  });
  const saveFile = async (payload: SaveFileRequest): Promise<SaveFileResult> => {
    const args: Record<string, unknown> = { suggestedName: payload.suggestedName };
    if (payload.data !== undefined) {
      // Tauri command 参数按 JSON 序列化；显式复制为 number[]，避免 ArrayBuffer
      // 在不同 WebView 版本中被当成空对象或不可序列化的宿主对象。
      args.data = Array.from(new Uint8Array(payload.data));
    } else {
      args.sourceUrl = payload.sourceUrl;
    }
    return invokeCommand<SaveFileResult>("platform_file_save_file", args);
  };
  const printPageToPdf = isWindowsTauriRuntime()
    ? () => {
        const pageSize = readPrintPageSizeFromHost();
        return invokeCommand<unknown>(
          "platform_file_print_page_to_pdf",
          pageSize ? { pageSize } : undefined,
        ).then(normalizePrintPageToPdfResult);
      }
    : undefined;
  return {
    canSelectFilePath: true,
    isLocalDevelopmentRuntime: import.meta.env.DEV,
    selectDirectory: () => invokeCommand<string | null>("pick_directory"),
    selectFile: async () => {
      const files = await invokeCommand<string[]>("pick_attach_files");
      return files[0] ?? null;
    },
    selectFiles: () => invokeCommand<string[]>("pick_attach_files"),
    saveFile,
    ...(printPageToPdf ? { printPageToPdf } : {}),
    getPathForFile: () => null,
    onNativeFileDrop: listenNativeFileDrop,
    createTempTextAttachment: async ({ text, filename }: { text: string; filename?: string }) => {
      const safeFilename = filename?.trim() || "attachment.txt";
      const localPath = await invokeCommand<string>("save_pasted_attachment", {
        name: safeFilename,
        bytes: Array.from(new TextEncoder().encode(text)),
      });
      return {
        filename: safeFilename,
        localPath,
        mimeType: "text/plain",
        sizeBytes: new TextEncoder().encode(text).byteLength,
      };
    },

    // 由 Rust 统一登记 canonical project root，并在已有窗口时激活对应 workspace。
    // 不能只返回 activated=false：未登记项目会在后续 RPC 首次访问时被拒绝。
    activateOrSetWorkspace: (path: string) =>
      invokeCommand<{ activated: boolean }>("window_activate_workspace", { path }),
    // Worktree 的路径、Session 身份和 Journal 回执均由 Rust 校验；renderer 只传输
    // 共享类型，避免为 Git 生命周期在前端复制一套本地状态。
    listGitWorktrees: (projectPath: string) =>
      invokeCommand<GitWorktreesResult>("git_worktrees_list", { projectPath }),
    createGitWorktree: (input: GitWorktreeCreateInput) =>
      invokeCommand<GitWorktreeCreateResult>("ui_git_worktree_create", { input }),
    handoffGitWorkspace: (input: GitWorktreeHandoffInput) =>
      invokeCommand<GitWorktreeHandoffResult>("ui_git_handoff", { input }),
    stopGitWorkspaceSession: (sessionId: string) =>
      invokeCommand<void>("ui_thread_session_stop", { sessionId }),
    removeGitWorktree: (input: GitWorktreeRemoveInput) =>
      invokeCommand<void>("ui_git_worktree_remove", { input }),
    archiveGitWorktree: (input: GitWorktreeArchiveCleanupInput) =>
      invokeCommand<void>("ui_git_archive_cleanup", { input }),
    listGitWorktreeArchiveRecords: () =>
      invokeCommand<GitWorktreeArchiveRecord[]>("ui_git_archive_records"),
    recoverGitWorktree: (sessionId: string) =>
      invokeCommand<boolean>("ui_git_archive_recover", { sessionId }),
    // 远程工作区已从本地产品面移除。残留调用收到明确错误，避免伪造激活或连接成功。
    connectRemote: () => unsupported("远程工作区"),
    cancelPendingRemoteConnection: () => unsupported("远程连接取消"),
    bindRemoteWorkspaceSessionContext: () => unsupported("远程会话绑定"),
    disposeRemoteSession: () => unsupported("远程会话释放"),
    isDockerAvailable: () => unsupported("Docker"),
    listWSLDistros: () => unsupported("WSL"),
    listDockerContainers: () => unsupported("Docker"),
    listSSHConfigAliases: () => unsupported("SSH"),

    openExternal: (url: string) => {
      void invokeCommand<void>("url_open", { url }).catch((error) => {
        reportRendererError(error, "[tauri-openExternal] url_open failed");
      });
    },
    openInFileManager: async (path: string) => {
      await invokeCommand<void>("path_reveal", { path });
      return { success: true };
    },
    openExternalFile: async (path: string) => {
      await invokeCommand<void>("path_open", { path });
      return { success: true };
    },

    notifyRendererReady: () => {
      void invokeCommand("startup_frontend_ready");
    },
    // 通知必须经过 Rust 的系统通知命令；renderer 事件没有宿主消费者时会静默丢失。
    showTaskNotification: (payload: unknown) => {
      void invokeCommand<void>("task_notification_show", { payload }).catch((error) => {
        console.warn("[tauri-notification] task_notification_show failed", error);
      });
    },
    reportTelemetryEvent: () => unsupported("云端 telemetry"),
    reportArmsCustomEvent: () => unsupported("云端 ARMS telemetry"),

    syncWindowUnreadCount: (count: number) => {
      void invokeCommand("tray_set_badge", { count });
    },
    // 托盘菜单由 Rust 发射 app://tray-new-chat；旧的 keencode://new-task 没有发射方。
    onNewTask: (handler: () => void) => listenTauriEvent("app://tray-new-chat", handler),
    onExitRequested: (handler) =>
      listenTauriEvent<unknown>("app://exit-requested", (payload) => {
        const normalized = normalizeExitRequestedPayload(payload);
        if (normalized) handler(normalized);
      }),
    confirmExit: () => invokeCommand<void>("app_confirm_exit"),
    onTrayOpenSession: (handler) =>
      listenTauriEvent<unknown>("app://tray-open-session", (payload) => {
        const normalized = normalizeTrayOpenSessionPayload(payload);
        if (normalized) handler(normalized);
      }),
    setTrayMenu: (payload) => invokeCommand<void>("tray_set_menu", { menu: payload }),
    onWindowFullscreenChanged: (handler: (isFullscreen: boolean) => void) =>
      listenTauriEvent<boolean>("keencode://window-fullscreen-changed", handler),
    getDesktopWindowChromeState: () =>
      invokeCommand<DesktopWindowChromeState>("desktop_window_chrome_state"),
    onDesktopWindowChromeStateChanged: (handler: (state: DesktopWindowChromeState) => void) =>
      listenTauriEvent<DesktopWindowChromeState>(
        "keencode://desktop-window-chrome-state-changed",
        handler,
      ),
    getDesktopZoomLevel: () => invokeCommand<DesktopZoomState>("desktop_zoom_level"),
    onDesktopZoomLevelChanged: (handler: (state: DesktopZoomState) => void) =>
      listenTauriEvent<DesktopZoomState>("keencode://desktop-zoom-level-changed", handler),
    exportLogs: async () => {
      const data = await invokeCommand<string>("diagnostics_export");
      return saveFile({
        data: new TextEncoder().encode(data).buffer,
        suggestedName: "keencode-diagnostics.json",
      });
    },
    nativeBrowserOpen: async ({ tabId, url, bounds, owner }) => {
      await browserResetReady;
      if (browserResetFailure) throw browserResetFailure;
      return invokeCommand<NativeBrowserOpenResult>("browser_open", {
        tabId,
        url,
        bounds,
        owner,
      });
    },
    nativeBrowserBounds: ({ tabId, bounds, owner, generation }) =>
      invokeCommand<void>("browser_bounds", { tabId, bounds, owner, generation }),
    nativeBrowserShow: ({ tabId, owner, generation }) =>
      invokeCommand<void>("browser_show", { tabId, owner, generation }),
    nativeBrowserHide: ({ tabId, owner, generation }) =>
      invokeCommand<void>("browser_hide", { tabId, owner, generation }),
    nativeBrowserClose: ({ tabId, owner, generation }) =>
      invokeCommand<void>("browser_close", { tabId, owner, generation }),
    nativeBrowserNavigate: ({ tabId, owner, generation }, url) =>
      invokeCommand<void>("browser_navigate", { tabId, owner, generation, url }),
    nativeBrowserZoom: ({ tabId, owner, generation }, zoom) =>
      invokeCommand<void>("browser_zoom", { tabId, owner, generation, zoom }),
    nativeBrowserReload: ({ tabId, owner, generation }) =>
      invokeCommand<void>("browser_reload", { tabId, owner, generation }),
    nativeBrowserHistory: ({ tabId, owner, generation }, direction) =>
      invokeCommand<void>("browser_history", { tabId, owner, generation, direction }),
    nativeBrowserNavigationState: ({ tabId, owner, generation }) =>
      invokeCommand<NativeBrowserNavigationState>("browser_navigation_state", {
        tabId,
        owner,
        generation,
      }),
    nativeBrowserReset: () => invokeCommand<void>("browser_reset"),
    onNativeBrowserState: (handler: (event: NativeBrowserStateEvent) => void) =>
      listenTauriEvent<NativeBrowserStateEvent>("browser://state", handler),
    onUpdateReady: (handler: (version: string) => void) => {
      let lastVersion: string | null = null;
      return listenUpdateStatus((status) => {
        const state = toUpdateState(status);
        if (state.kind !== "update-downloaded" || state.version === lastVersion) return;
        lastVersion = state.version;
        handler(state.version);
      });
    },
    onUpdateCheckResult: (handler: (payload: UpdateCheckResultPayload) => void) => {
      let previous: UpdateStatePayload | null = null;
      let previousFailure: string | undefined;
      return listenUpdateStatus((status) => {
        const state = toUpdateState(status);
        const previousKind = previous?.kind;
        previous = state;
        if (status.downloadState === "failed") {
          const message = status.downloadError?.trim() || "更新下载失败";
          if (previousFailure !== message) {
            previousFailure = message;
            handler({ kind: "error", message });
          }
          return;
        }
        previousFailure = undefined;
        if (state.kind === "idle" && status.checked && !status.available && previousKind !== "idle") {
          handler({ kind: "up-to-date", currentVersion: status.currentVersion });
        } else if (state.kind === "update-available" && previousKind !== "update-available") {
          handler({
            kind: "available",
            version: state.version,
            ...(state.releaseNotes ? { releaseNotes: state.releaseNotes } : {}),
          });
        } else if (state.kind === "download-progress" && previousKind !== "download-progress") {
          handler({ kind: "downloading", version: state.version ?? status.currentVersion });
        } else if (state.kind === "update-downloaded" && previousKind !== "update-downloaded") {
          handler({ kind: "ready", version: state.version });
        }
      });
    },
    onUpdateStateChanged: (handler: (payload: UpdateStatePayload) => void) =>
      listenUpdateStatus((status) => handler(toUpdateState(status))),
    getUpdateState: () =>
      invokeCommand<TauriUpdateStatusPayload>("app_update_info").then(toUpdateState),
    // Rust 的 check 命令负责检查清单并启动后台下载；install 只接受 ready 缓存。
    downloadUpdate: () => invokeCommand("app_update_check"),
    quitAndInstallUpdate: () => invokeCommand("app_update_install"),
    // 编辑器发现与启动均由 Rust 白名单和 workspace 根授权，renderer 不执行命令。
    getApplicationIcon: (request: string | ApplicationIconRequest) =>
      invokeCommand<ApplicationIconInfo | null>("ui_editors_get_icon", { request }),
    getInstalledEditors: () => invokeCommand<EditorInfo[]>("ui_editors_get_installed"),
    openInEditor: (editorId, path, options) =>
      invokeCommand<{ success: boolean; error?: string }>("ui_editors_open", {
        editorId,
        path,
        ...(options ? { options } : {}),
      }),
    executeDesktopCommand: (command: string) => {
      switch (command) {
        case "closeWindow":
          return invokeCommand("app_close_window");
        case "checkForUpdates":
          return invokeCommand("app_update_check");
        case "minimizeWindow":
        case "toggleMaximizeWindow":
        case "zoomIn":
        case "zoomOut":
        case "resetZoom":
        case "openResourceManager":
        case "showAbout":
          return invokeCommand("desktop_execute_command", { command });
        default:
          return unsupported(`桌面命令 ${command}`);
      }
    },
    setApplicationLocale: async (locale: Locale) => {
      const interfaceLanguage = locale === "en-US" ? "en" : "zh";
      await invokeCommand("settings_set", { settings: { interfaceLanguage } });
    },
    getDeviceId: () => "tauri-local",
  };
}
