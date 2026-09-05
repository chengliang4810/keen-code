import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  DEFAULT_WALLPAPER_FOCUS,
} from "@/lib/themeSkin";
import { useThemeAppearance } from "@/hooks/useThemeAppearance";
import { useWindowChrome } from "@/hooks/useWindowChrome";
import { useAppUpdate } from "@/hooks/useAppUpdate";
import { useAppSettings } from "@/hooks/useAppSettings";
import { useProviderModels } from "@/hooks/useProviderModels";
import { useAppDialog } from "@/hooks/useAppDialog";
import { useAppRoute } from "@/hooks/useAppRoute";
import { useChatFind } from "@/hooks/useChatFind";
import { useWallpaperAppearance } from "@/hooks/useWallpaperAppearance";
import { useAcpSessionRuntime } from "@/hooks/useAcpSessionRuntime";
import { useSessionTurn } from "@/hooks/useSessionTurn";
import { useComposerController } from "@/hooks/useComposerController";
import { useSidebarController } from "@/hooks/useSidebarController";
import {
  useSessionNavigation,
  type SessionNavigationNewChat,
  type SessionNavigationOpenSession,
} from "@/hooks/useSessionNavigation";
import { useWorkbenchDragResize } from "@/hooks/useWorkbenchDragResize";
import { useProjectDialog } from "@/hooks/useProjectDialog";
import { useWorktrees } from "@/hooks/useWorktrees";
import {
  acpSessionApi,
  useSessionLifecycleActions,
} from "@/hooks/useSessionLifecycleActions";
import { WallpaperMediaLayer } from "@/components/WallpaperMediaLayer";
import {
  DEFAULT_LAYOUT,
  loadLayout,
} from "@/lib/layout";
import type { DragZone } from "@/lib/dragZone";
import {
  isSessionLiveStreaming,
  localizeUiError,
  presentErrorBanner,
  type ErrorBannerView,
  IDLE_SNAPSHOT,
  type AskUserPayload,
  type ChatMessage,
  type SessionSnapshot,
} from "@/lib/session";
import * as api from "@/lib/api";
import { type ViewFocus } from "@/lib/viewFocus";
import {
  busySessionIds,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import { reconcileHostActiveTurnSnapshot } from "@/lib/activeTurn";
import { loadCompletedUnreadSessionIds } from "@/lib/sessionCompletion";
import { createT } from "@/i18n";
import { appUpdateActionFor } from "@/lib/appUpdate";
import { isProjectPathMissing } from "@/lib/projectPath";
import { type ResourceOpenTarget } from "@/components/ResourceViewer";
import { type TurnLatencyState } from "@/lib/turnLatency";
import type {
  DraftNavigationLocation,
  DraftNavigationSnapshot,
} from "@/lib/draftNavigation";
import { ImageViewerProvider } from "@/components/ImageViewer";
import { StartupScreen } from "@/components/StartupScreen";
import { updateSessionPreference } from "@/lib/sessionPreferences";
import { extractFirstUserMessageText } from "@/lib/sessionTitle";
import {
  projectPeriStoredMessages,
  withSubagentPrompts,
} from "@/lib/periStoredMessages";
import { createAcpWorkspaceState, type AcpWorkspaceState } from "@/lib/acp/store";
import { type SettingsSectionId } from "@/components/SettingsPage";
import {
  WindowControls,
  toggleMaximizeFromTitlebar,
} from "@/components/WindowControls";
import { Sidebar } from "@/features/app/Sidebar";
import { MainStage } from "@/features/app/MainStage";
import { ResourceAside } from "@/features/app/ResourceAside";
import { SettingsRoute } from "@/features/app/SettingsRoute";
import { AddProjectModal } from "@/features/app/overlays/AddProjectModal";
import { AppDialogPortal } from "@/features/app/overlays/AppDialogPortal";
import { AppUpdateModal } from "@/features/app/overlays/AppUpdateModal";
import { SessionContextMenu } from "@/features/app/overlays/SessionContextMenu";
import { SessionSearchPortal } from "@/features/app/overlays/SessionSearchPortal";
import { ShortcutsModal } from "@/features/app/overlays/ShortcutsModal";
import { WorktreeCreateModal } from "@/features/app/overlays/WorktreeCreateModal";
import { WorktreeGcModal } from "@/features/app/overlays/WorktreeGcModal";
import { StatusModal } from "@/components/StatusModal";
import type {
  Project,
  SessionContextUsage,
} from "@/features/app/models";

export default function App() {
  /** ACP 原生事件归约出的工作区投影（事件监听直接改 ref 内的 view）。 */
  const acpWorkspaceRef = useRef<AcpWorkspaceState>(createAcpWorkspaceState());
  /** 渲染用工作区状态：每次 commit 生成新对象，驱动派生视图与重渲染。 */
  const [acpWorkspace, setAcpWorkspace] = useState<AcpWorkspaceState>(() =>
    createAcpWorkspaceState(),
  );
  /** 当前运行回合的低延迟链路观测；Session 完成后固化进 Assistant 历史。 */
  const turnLatencyBySessionRef = useRef<Map<string, TurnLatencyState>>(
    new Map(),
  );
  /** Host 当前前台请求的稳定关联；值为唯一 requestId。 */
  const activeTurnIdBySessionRef = useRef<Map<string, string>>(new Map());
  /** Host 已完成但 Tauri done 可能仍在跨通道排队的 requestId。 */
  const recoverableCompletedTurnIdBySessionRef = useRef<Map<string, string>>(
    new Map(),
  );
  /** 每个 Session 最近已消费的 requestId，拒绝重放/跨通道迟到更新。 */
  const completedTurnIdBySessionRef = useRef<Map<string, string>>(new Map());
  /** 已完成但当前可见消息尚未 commit 的唯一 DOM 观测。 */
  const pendingVisibleTurnBySessionRef = useRef<Map<string, string>>(
    new Map(),
  );
  /** 从 Host 快照恢复运行中 requestId；null 快照不清理更晚的本地 send。 */
  const observeHostActiveTurn = useCallback(
    (snapshot: {
      sessionId?: string | null;
      activeTurnId?: string | null;
    }) => {
      reconcileHostActiveTurnSnapshot(snapshot, {
        turnLatencyBySession: turnLatencyBySessionRef.current,
        activeTurnIdBySession: activeTurnIdBySessionRef.current,
        recoverableCompletedTurnIdBySession:
          recoverableCompletedTurnIdBySessionRef.current,
        completedTurnIdBySession: completedTurnIdBySessionRef.current,
      });
    },
    [],
  );
  /** 把 ref 中的最新工作区提交到渲染状态。 */
  const commitWorkspace = useCallback(() => {
    setAcpWorkspace({
      sessions: Object.fromEntries(
        Object.entries(acpWorkspaceRef.current.sessions).map(([id, view]) => [
          id,
          { ...view },
        ]),
      ),
    });
  }, []);

  const { themePreference, skin, applyThemeChoice, applySkinChoice } =
    useThemeAppearance();
  const [layout, setLayout] = useState(() => loadLayout(localStorage));
  const sidebarRef = useRef<HTMLElement>(null);
  const asideRef = useRef<HTMLElement>(null);

  const [session, setSession] = useState<SessionSnapshot>(IDLE_SNAPSHOT);
  /** Host live agent (may differ from the session currently viewed in the UI). */
  const [liveHost, setLiveHost] = useState<SessionSnapshot>(IDLE_SNAPSHOT);
  /** 多会话运行状态投影，用于展示后台任务忙碌状态。 */
  const [liveMap, setLiveMap] = useState<SessionLiveMap>({});
  /** 后台正常完成且尚未由用户打开查看的 Session。 */
  const [completedUnreadIds, setCompletedUnreadIds] = useState<Set<string>>(
    () => loadCompletedUnreadSessionIds(localStorage),
  );
  /** Latest live map for callbacks that must not close over a stale render. */
  const liveMapRef = useRef(liveMap);
  liveMapRef.current = liveMap;
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  /** 当前可见 Session 最近一次由 ACP 上报的上下文用量。 */
  const [contextUsage, setContextUsage] =
    useState<SessionContextUsage | null>(null);
  /** 每个 Session 最近一次由 ACP 上报的真实上下文用量。 */
  const contextUsageBySessionRef = useRef<Map<string, SessionContextUsage>>(
    new Map(),
  );
  /** 从本地请求记录恢复的当前任务整体缓存用量，可跨应用重启。 */
  const [taskCacheUsage, setTaskCacheUsage] =
    useState<api.TaskCacheUsage | null>(null);
  const taskCacheUsageRequestSeqRef = useRef(0);
  const draftKeyRef = useRef(0);
  const draftNavigationSnapshotRef = useRef<DraftNavigationSnapshot | null>(null);
  const navigationActionsRef = useRef<{
    newChat: SessionNavigationNewChat;
    openSession: SessionNavigationOpenSession;
  }>({
    newChat: async () => {},
    openSession: async () => {},
  });
  /** Prevent overlapping executeSend / queue auto-flush races. */
  const sendInFlightRef = useRef(false);
  const [activeProject, setActiveProject] = useState<Project | null>(null);
  /** Per-session message cache so switching away mid-turn does not drop the UI. */
  const messagesBySessionRef = useRef<Map<string, ChatMessage[]>>(new Map());
  /** 每个会话最后确认的模型，避免切换对话时复用全局 composer 模型。 */
  const modelBySessionRef = useRef<Map<string, string>>(new Map());
  const viewingSessionIdRef = useRef<string | null>(null);
  const [subagentDescriptions, setSubagentDescriptions] = useState<
    Record<string, string>
  >({});
  /** 当前渲染 Session 的 ACP 原生视图；草稿没有持久化视图。 */
  const acpSessionView = useMemo(
    () =>
      session.sessionId
        ? acpWorkspace.sessions[session.sessionId] ?? null
        : null,
    [acpWorkspace, session.sessionId],
  );
  const displayedSubagents = useMemo(
    () =>
      withSubagentPrompts(messages, acpSessionView?.subagents ?? []).map(
        (agent) => ({
          ...agent,
          agent_description: subagentDescriptions[agent.agent_name],
        }),
      ),
    [acpSessionView?.subagents, messages, subagentDescriptions],
  );
  /**
   * Bumped on every user navigation (open chat / new chat). Async work captures
   * {@link currentViewFocus} before its first await and must re-check before
   * touching view state — otherwise a slow connect started on one draft yanks
   * the workbench away from the draft the user opened since.
   */
  const viewEpochRef = useRef(0);
  const currentViewFocus = useCallback(
    (): ViewFocus => ({
      sessionId: viewingSessionIdRef.current,
      epoch: viewEpochRef.current,
    }),
    [],
  );
  const draftNavigationLocation = useCallback(
    (): DraftNavigationLocation => ({
      sessionId: viewingSessionIdRef.current,
      draftKey: draftKeyRef.current,
      viewEpoch: viewEpochRef.current,
    }),
    [],
  );
  const liveHostRef = useRef<SessionSnapshot>(IDLE_SNAPSHOT);
  const messagesRef = useRef<ChatMessage[]>([]);
  const {
    appDialog,
    setAppDialog,
    dialogInput,
    setDialogInput,
    dialogInputRef,
    confirmBtnRef,
    appDialogRef,
  } = useAppDialog();
  const askUserWrapRef = useRef<HTMLDivElement>(null);
  /** Desktop Connect panel (AC7) — close does not stop host. */

  /** While openSession loads, do not let session.sessionId effect clobber viewing id. */
  const openingSessionIdRef = useRef<string | null>(null);
  /** Distinguishes two overlapping opens of the same Session. */
  const openingSessionEpochRef = useRef<number | null>(null);

  // ContextMenu handles outside click + Escape for sidebar menus.

  const { appView, settingsSection, navigateWorkbench, navigateSettings } =
    useAppRoute();

  /** 首次渲染时展示品牌启动页；工作台外壳不等待会话状态。 */
  const [appBooting, setAppBooting] = useState(true);
  const [toast, setToast] = useState<string | null>(null);
  const showToast = useCallback((msg: string, ms = 3200) => {
    setToast(msg);
    window.setTimeout(() => {
      setToast((cur) => (cur === msg ? null : cur));
    }, ms);
  }, []);
  const [showShortcuts, setShowShortcuts] = useState(false);
  /** In-conversation find (Cmd/Ctrl+F) — not the palette/session search. */
  const {
    showChatFind,
    setShowChatFind,
    chatFindQuery,
    setChatFindQuery,
    chatFindIndex,
    setChatFindIndex,
    chatFindFocusKey,
    chatFindMatches,
    chatFindHitIds,
    chatFindActive,
    openChatFind,
    chatFindNext,
    chatFindPrev,
  } = useChatFind({
    messages,
    sessionId: session.sessionId,
    dialogOpen: appDialog !== null,
  });
  const [askUser, setAskUser] = useState<AskUserPayload | null>(null);
  /**
   * Unanswered gates per session (`sessionId` → payload).
   *
   * 后台任务也可以在用户查看其他任务时提出问题。这里按 Session 暂存未回答问题，
   * 切回任务时恢复显示，回答或本轮结束后删除。
   */
  const pendingAskUserBySessionRef = useRef<Map<string, AskUserPayload>>(
    new Map(),
  );
  /** 触发侧栏重渲染，使后台等待输入的任务显示状态点。 */
  const [pendingAskUserSessionIds, setPendingAskUserSessionIds] = useState<Set<string>>(
    new Set(),
  );
  /** 清除指定 Session 尚未回答的问题；请求标识不同时保留后来到达的新问题。 */
  const clearPendingAskUser = useCallback(
    (sessionId?: string | null, rpcId?: number) => {
      if (!sessionId) return;
      const pending = pendingAskUserBySessionRef.current.get(sessionId);
      if (rpcId != null && pending?.rpcId !== rpcId) return;
      pendingAskUserBySessionRef.current.delete(sessionId);
      setPendingAskUserSessionIds((previous) => {
        if (!previous.has(sessionId)) return previous;
        const next = new Set(previous);
        next.delete(sessionId);
        return next;
      });
    },
    [],
  );
  /** 为只挂载一次的事件监听保存最新问题清理函数。 */
  const clearPendingAskUserRef = useRef(clearPendingAskUser);
  clearPendingAskUserRef.current = clearPendingAskUser;
  /** Polite SR announce for stream start/stop (not every token). */
  const [streamA11yNote, setStreamA11yNote] = useState("");
  const wasStreamingRef = useRef(false);
  const appSettings = useAppSettings({
    appBooting,
    onSaveError: showToast,
    showToast,
  });
  const {
    locale,
    appUpdateDownloadSource,
    onAppUpdateDownloadSource,
    terminalFontFamily,
    projectDirectory,
    autoArchiveConversations,
    onAutoArchiveConversations,
    archiveRetentionDays,
    onArchiveRetentionDays,
  } = appSettings;
  const tr = useMemo(() => createT(locale), [locale]);
  const trRef = useRef(tr);
  trRef.current = tr;
  const {
    status: appUpdateStatus,
    busy: appUpdateBusy,
    error: appUpdateError,
    progressOpen: appUpdateProgressOpen,
    setProgressOpen: setAppUpdateProgressOpen,
    check: checkAppUpdate,
    install: installAppUpdate,
  } = useAppUpdate(appBooting, locale);

  const providerModels = useProviderModels({
    sessionId: session.sessionId,
    locale,
    showToast,
  });
  const {
    modelId,
    setModelId,
    effort,
    setEffort,
    configuredModelsRef,
    availableModels,
    activeModel,
    modelLabel,
    activeCustomProvider,
    providerRouteRevision,
    refreshProviderRoute,
    handleProviderActivated,
    hasConfiguredModel,
    isValidEffort,
    isValidModelId,
  } = providerModels;

  const [subagentModelLabels, setSubagentModelLabels] = useState<
    Record<string, string>
  >({});
  const subagentIdentityKey = displayedSubagents
    .map((agent) => `${agent.agent_id}:${agent.agent_name}`)
    .join("|");
  useEffect(() => {
    if (!api.isTauri() || !subagentIdentityKey) return;
    let cancelled = false;
    void api
      .agentsList()
      .then(({ agents }) => {
        if (cancelled) return;
        setSubagentDescriptions(
          Object.fromEntries(
            agents.map((agent) => [agent.name, agent.description.trim()]),
          ),
        );
        setSubagentModelLabels(
          Object.fromEntries(
            agents.flatMap((agent) => {
              if (!agent.model) return [];
              const model = agent.model.split("::").at(-1)?.trim();
              return model ? [[agent.name, model]] : [];
            }),
          ),
        );
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [subagentIdentityKey]);
  /** Chat file/url card → open in right resource pane. */
  const [resourceOpenTarget, setResourceOpenTarget] =
    useState<ResourceOpenTarget | null>(null);
  /** 对话右上角的环境与子 Agent 摘要浮层。 */
  const [summaryOpen, setSummaryOpen] = useState(false);
  const previousAsideCollapsedRef = useRef(layout.asideCollapsed);
  useLayoutEffect(() => {
    const previous = previousAsideCollapsedRef.current;
    previousAsideCollapsedRef.current = layout.asideCollapsed;
    if (previous === layout.asideCollapsed) return;
    setSummaryOpen(layout.asideCollapsed && Boolean(session.sessionId));
  }, [layout.asideCollapsed, session.sessionId]);
  /** 任务摘要按钮引用，供浮层判断点击是否来自触发按钮。 */
  const summaryTriggerRef = useRef<HTMLButtonElement>(null);
  /** 关闭任务摘要浮层，避免流式更新期间反复重绑文档监听。 */
  const closeSummary = useCallback(() => setSummaryOpen(false), []);
  /** Agent 工具状态变化时驱动右侧文件树与 Git 状态同步。 */
  const resourceSyncRevision = useMemo(
    () =>
      messages.reduce((revision, message) => {
        for (const segment of message.segments ?? []) {
          if (segment.kind !== "tool") continue;
          for (const char of `${segment.toolCallId}:${segment.status}:${segment.streaming ?? false}`) {
            revision = (revision * 31 + char.charCodeAt(0)) >>> 0;
          }
        }
        return revision;
      }, 0),
    [messages],
  );
  /** Live drag-drop target for zone overlays (null = not dragging). */
  const [dragZone, setDragZone] = useState<DragZone>(null);
  const [localError, setLocalError] = useState<string | null>(null);
  const {
    wallpaperRecord,
    wallpaperUrl,
    wallpaperScrim,
    wallpaperBlur,
    applyWallpaperChoice,
    applyWallpaperAdjustChoice,
    applyWallpaperMediaSize,
    applyWallpaperScrimChoice,
    applyWallpaperBlurChoice,
    resetWallpaperAppearance,
  } = useWallpaperAppearance({
    locale,
    onError: (message, source) => {
      if (source === "clear" || source === "save") setToast(message);
      else setLocalError(message);
    },
  });
  /** Expand technical dump under the compact error banner. */
  const [errorDetailOpen, setErrorDetailOpen] = useState(false);
  /** Host stream-stall prompt (I06); null when dismissed or not stalled. */
  const [streamStall, setStreamStall] = useState<{
    sessionId?: string;
    stallSeconds: number;
    tier?: string;
    sawModelOutput?: boolean;
    sawToolActivity?: boolean;
  } | null>(null);
  /** Live provider retry progress (session://retry); cleared on success/stop/error. */
  const [retryStatus, setRetryStatus] = useState<{
    attempt: number;
    maxAttempts: number;
    delayMs: number;
    reason: string;
  } | null>(null);
  /** Epoch ms when the current agent turn became busy (for elapsed UI). */
  const [turnStartedAt, setTurnStartedAt] = useState<number | null>(null);
  const [resizingAside, setResizingAside] = useState(false);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const { platform, useCustomWindowChrome, windowMaximized, windowFullscreen } =
    useWindowChrome();

  /** Composer 业务边界：草稿、附件、Slash、历史、模式和目标均由该控制器管理。 */
  const composerApplyViewProjectionRef = useRef<
    (sessionId: string | null) => void
  >(() => {});
  const exportActiveSessionMdRef = useRef<() => Promise<void>>(
    async () => {},
  );
  const composer = useComposerController({
    locale,
    session: {
      sessionId: session.sessionId ?? null,
      state: session.state,
      activeProject,
      messages,
      acpSessionView,
      contextUsage,
      modelId,
      modelContextWindow: activeModel?.contextWindow,
    },
    api: {
      isTauri: api.isTauri,
      attachments: {
        pickFiles: api.pickAttachFiles,
        savePastedFile: api.savePastedAttachment,
        classifyPaths: async (paths) =>
          (await api.pathsClassify(paths)).map(({ path, name, isDir }) => ({
            path,
            name,
            isDir,
          })),
      },
      skillsList: api.skillsList,
      goals: acpSessionApi.goals,
    },
    workspace: {
      acpWorkspaceRef,
      commitWorkspace,
      applyViewProjectionRef: composerApplyViewProjectionRef,
    },
    navigation: {
      location: draftNavigationLocation,
      snapshotRef: draftNavigationSnapshotRef,
    },
    feedback: {
      showToast,
      setLocalError,
      setAppDialog,
    },
    actions: {
      newChat: () => navigationActionsRef.current.newChat(),
      exportActiveSession: () => exportActiveSessionMdRef.current(),
    },
    askUserWrapRef,
    askUserKey: askUser?.rpcId ?? null,
  });
  const {
    draft,
    setDraft,
    handleDraftChange,
    attachments,
    setAttachments,
    attachmentLabels: attachLabels,
    addAttachmentsFromPaths,
    addPastedFiles,
    pickComposerFiles,
    skillsLoading,
    liveSlash,
    slashFilterQuery,
    composerMenuOpen,
    setShowComposerPlus,
    composerPanel,
    setComposerPanel,
    slashActiveIndex,
    setSlashActiveIndex,
    composerMenuEntries,
    composerMenuEntriesRef,
    resolveSlashTitle,
    resolveSlashDescription,
    onSlashQueryChange,
    closeComposerMenu,
    applySlashItem,
    promptHistoryIndexRef,
    setPromptHistoryIndex,
    promptHistoryOpen,
    promptHistoryOpenRef,
    setPromptHistoryOpen,
    promptHistoryFilter,
    setPromptHistoryFilter,
    promptHistoryActive,
    setPromptHistoryActive,
    promptHistoryFocusFilter,
    setPromptHistoryFocusFilter,
    promptHistoryEntries,
    promptHistoryPanelRef,
    closePromptHistory,
    openPromptHistory,
    applyPromptHistoryEntry,
    composerInputRef,
    composerShellRef,
    composerWrapRef,
    composerPlusTriggerRef,
    composerPlusPanelRef,
    composerPlusPos,
    composerPlusStyle,
    promptHistoryPos,
    promptHistoryStyle,
    composerFloatPad,
    requestComposerFocus,
    contextUsageDisplay,
    goalModeSessionKey,
    setGoalModeSessionKey,
    planModeSessionKey,
    setPlanModeSessionKey,
    ultraModeSessionKey,
    setUltraModeSessionKey,
    showStatusModal,
    setShowStatusModal,
    confirmClearCurrentGoal,
  } = composer;

  const sidebar = useSidebarController({
    locale,
    currentSessionId: session.sessionId,
    activeProject,
    setActiveProject,
    setSession,
    setAppDialog,
    setLocalError,
    setLayout,
    setResourceOpenTarget,
    viewingSessionIdRef,
    setAppBooting,
    onActiveProjectRelocated: () => {
      setSession((previous) =>
        previous.sessionId
          ? {
              ...IDLE_SNAPSHOT,
              sessionId: previous.sessionId,
              title: previous.title,
              state: "idle",
              backend: "peri_acp",
            }
          : previous,
      );
      setLiveHost((previous) =>
        previous.sessionId ? { ...IDLE_SNAPSHOT } : previous,
      );
    },
    onActiveProjectRemoved: () => {
      setSession(IDLE_SNAPSHOT);
      setMessages([]);
      setContextUsage(null);
      setAskUser(null);
    },
    newChat: (project, options) =>
      navigationActionsRef.current.newChat(project, options),
    openSession: (row, project) =>
      navigationActionsRef.current.openSession(row, project),
    showToast,
    composerInputRef,
    autoArchiveConversations: autoArchiveConversations === true,
    archiveRetentionDays,
  });
  const {
    projects,
    setProjects,
    sessions,
    sessionsRef,
    sessionTitleOverridesRef,
    expandedProjects,
    setExpandedProjects,
    visibleSessionsByProject,
    setVisibleSessionsByProject,
    projectDropHint,
    setProjectDropHint,
    projectsOpen,
    setProjectsOpen,
    pinnedOpen,
    setPinnedOpen,
    historyOpen,
    setHistoryOpen,
    ctxMenu,
    setCtxMenu,
    showSearch,
    setShowSearch,
    searchQuery,
    setSearchQuery,
    searchHits,
    searchTriggerRef,
    searchReturnFocusRef,
    refreshSessions,
    sessionsForProject,
    pinnedSessions,
    orphanSessions,
    startSidebarDrag,
    endSidebarDrag,
    dragOverProject,
    dropProject,
    dropSession,
    applyProjectOrder,
    openSearch,
    openSessionMenu,
    openProjectMenu,
    renameProject,
    renameSession,
    relocateProject,
    removeProjectFromApp,
    archiveSession,
    pinSession,
    copySessionId,
    viewTrajectory,
    applyMessagePrefixTitle,
    applyAutomaticSessionTitle,
  } = sidebar;

  const dropQueuedSessionsRef = useRef<(sessionIds: Iterable<string>) => void>(
    () => {},
  );

  // Global shortcuts use refs so the listener stays mounted while handlers change.
  const shortcutHandlersRef = useRef({
    newChat: () => {},
    openSettings: () => {},
    openChatFind: () => {},
  });
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.isComposing) return;
      const modifier = event.metaKey || event.ctrlKey;
      if (!modifier) return;
      const target = event.target as HTMLElement | null;
      const tag = target?.tagName?.toLowerCase();
      const typing =
        tag === "input" || tag === "textarea" || !!target?.isContentEditable;
      const key = event.key.toLowerCase();
      if (key === "f" && !event.shiftKey) {
        event.preventDefault();
        shortcutHandlersRef.current.openChatFind();
        return;
      }
      if (key === "k") {
        event.preventDefault();
        openSearch();
        return;
      }
      if (key === "/") {
        event.preventDefault();
        setShowShortcuts((value) => !value);
        return;
      }
      if (key === "," && !typing) {
        event.preventDefault();
        shortcutHandlersRef.current.openSettings();
        return;
      }
      if (key === "n" && !typing) {
        event.preventDefault();
        shortcutHandlersRef.current.newChat();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [openSearch]);

  const {
    applyViewProjection,
    applyViewProjectionRef,
    handleFirstVisibleToken,
    replayHistory,
    patchSessionMessages,
  } = useAcpSessionRuntime({
    locale,
    session,
    messages,
    liveHost,
    acpWorkspace,
    observeHostActiveTurn,
    commitWorkspace,
    acpWorkspaceRef,
    turnLatencyBySessionRef,
    activeTurnIdBySessionRef,
    recoverableCompletedTurnIdBySessionRef,
    completedTurnIdBySessionRef,
    pendingVisibleTurnBySessionRef,
    liveHostRef,
    messagesRef,
    messagesBySessionRef,
    modelBySessionRef,
    contextUsageBySessionRef,
    taskCacheUsageRequestSeqRef,
    viewingSessionIdRef,
    openingSessionIdRef,
    currentViewFocus,
    sessionTitleOverridesRef,
    sessionsRef,
    sendInFlightRef,
    configuredModelsRef,
    clearPendingAskUserRef,
    pendingAskUserBySessionRef,
    setPendingAskUserSessionIds,
    setAskUser,
    setSession,
    setMessages,
    setLiveHost,
    setLiveMap,
    setContextUsage,
    setTaskCacheUsage,
    setRetryStatus,
    setTurnStartedAt,
    setEffort,
    setModelId,
    promptHistoryIndexRef,
    setPromptHistoryIndex,
    setPromptHistoryOpen,
    setPromptHistoryFilter,
    setPromptHistoryActive,
    setPromptHistoryFocusFilter,
    setCompletedUnreadIds,
  });
  composerApplyViewProjectionRef.current = applyViewProjection;

  /**
   * 多会话忙碌标识，用于侧栏运行中状态。
   * Uses liveMap projection + liveHost fallback. Excludes connecting.
   */
  const busyIds = useMemo(() => {
    const set = busySessionIds(liveMap);
    if (liveHost.sessionId && isSessionLiveStreaming(liveHost.state)) {
      set.add(liveHost.sessionId);
    }
    return set;
  }, [liveMap, liveHost.sessionId, liveHost.state]);
  /** 轨迹台账的数据源：内存缓存优先，其次回放持久化消息。 */
  const loadTrajectoryMessages = useCallback(
    async (id: string): Promise<ChatMessage[]> => {
      const cached = messagesBySessionRef.current.get(id);
      if (cached?.length) return cached;
      try {
        return projectPeriStoredMessages(await acpSessionApi.messages(id));
      } catch {
        return [];
      }
    },
    [],
  );

  /**
   * 兜底：非发送路径（如历史重放）进入首条消息时，同样立即应用消息前缀标题。
   */
  useEffect(() => {
    if (!api.isTauri() || !acpSessionView) return;
    const sessionId = acpSessionView.session_id;
    const firstUserText = extractFirstUserMessageText(acpSessionView.history);
    if (!firstUserText) return;
    applyMessagePrefixTitle(sessionId, firstUserText);
    void applyAutomaticSessionTitle(
      sessionId,
      firstUserText,
      acpSessionView.title ?? null,
    );
  }, [
    acpSessionView,
    applyAutomaticSessionTitle,
    applyMessagePrefixTitle,
  ]);

  /**
   * 切换工作目录。peri Session 的工作目录不可变，已有会话时进入目标项目的新草稿。
   */
  const bindSessionProject = useCallback(
    async (proj: Project | null, opts?: { silent?: boolean }) => {
      const sid = session.sessionId;
      if (!sid) {
        setActiveProject(proj);
        if (proj) {
          setExpandedProjects((e) => ({ ...e, [proj.id]: true }));
        } else {
          setHistoryOpen(true);
        }
        return;
      }
      if (proj && isProjectPathMissing(proj.pathOk)) {
        setLocalError(tr("project.pathMissing", { name: proj.name }));
        return;
      }
      try {
        await navigationActionsRef.current.newChat(proj);
        if (proj) {
          setExpandedProjects((e) => ({ ...e, [proj.id]: true }));
          if (!opts?.silent) {
            showToast(tr("composer.projectBound", { name: proj.name }), 2500);
          }
        } else {
          setHistoryOpen(true);
          if (!opts?.silent) {
            showToast(tr("composer.projectCleared"), 2200);
          }
        }
        setLocalError(null);
      } catch (error) {
        showToast(localizeUiError(error, locale), 4500);
      }
    },
    [locale, session.sessionId, showToast, tr],
  );

  /** 添加项目后刷新列表，并按调用场景选中项目或绑定当前任务。 */
  const finalizeAddedProject = useCallback(
    async (
      project: Project,
      options: { bindSession: boolean; silent?: boolean },
    ): Promise<Project> => {
      const list = (await api.projectsList()) as Project[];
      setProjects(list);
      const current = list.find((item) => item.id === project.id) ?? project;
      if (options.bindSession) {
        await bindSessionProject(current, { silent: options.silent });
      } else {
        setActiveProject(current);
        setExpandedProjects((expanded) => ({
          ...expanded,
          [current.id]: true,
        }));
        if (!options.silent) {
          showToast(tr("composer.projectAdded", { name: current.name }), 2500);
        }
      }
      return current;
    },
    [bindSessionProject, showToast, tr],
  );

  const finalizeProjectDialog = useCallback(
    async (project: Project, intent: { bindSession: boolean }) => {
      await finalizeAddedProject(project, { bindSession: intent.bindSession });
    },
    [finalizeAddedProject],
  );
  const {
    addProjectIntent,
    addProjectName,
    setAddProjectName,
    addProjectPath,
    addProjectBusy,
    addProjectError,
    setAddProjectError,
    addProjectNameRef,
    addProjectDropRef,
    addProjectReturnFocusRef,
    addProjectNameEditedRef,
    selectAddProjectSourceFromPaths,
    resetAddProject,
    openAddProject,
    closeAddProject,
    pickAddProjectDirectory,
    submitAddProject,
    addProject,
  } = useProjectDialog({
    projects,
    activeSession: session,
    finalizeAddedProject: finalizeProjectDialog,
    navigateSettings,
    locale,
    tr,
    setDragZone,
    setLocalError,
    showToast,
  });
  const worktrees = useWorktrees({
    activeProject,
    projects,
    locale,
    finalizeAddedProject,
    bindSessionProject,
    newChat: (project: Project | null | undefined) =>
      navigationActionsRef.current.newChat(project),
    showToast,
    setLocalError,
  });
  const {
    gitWorktrees,
    gitWorktreesAvailable,
    gitWorktreesLoading,
    gitWorktreesReason,
    refreshGitWorktrees,
    worktreeCreateOpen,
    setWorktreeCreateOpen,
    worktreeCreateName,
    setWorktreeCreateName,
    worktreeCreateRef,
    setWorktreeCreateRef,
    worktreeCreateBusy,
    worktreeCreateError,
    setWorktreeCreateError,
    worktreeCreateStartChat,
    worktreeCreatePreviewPath,
    openWorktreeCreate,
    submitWorktreeCreate,
    worktreeGcOpen,
    setWorktreeGcOpen,
    worktreeGcForce,
    setWorktreeGcForce,
    worktreeGcBusy,
    worktreeGcPreviewBusy,
    worktreeGcError,
    setWorktreeGcError,
    worktreeGcPreview,
    setWorktreeGcPreview,
    openWorktreeGc,
    submitWorktreeGc,
    switchToWorktree,
  } = worktrees;

  useWorkbenchDragResize({
    isTauri: api.isTauri,
    platform,
    addProjectOpen: addProjectIntent !== null,
    addProjectDropRef,
    setDragZone,
    selectAddProjectSourceFromPaths,
    addAttachmentsFromPaths,
    setLocalError,
    translate: tr,
    sidebarRef,
    asideRef,
    layout,
    setLayout,
    resizingSidebar,
    setResizingSidebar,
    resizingAside,
    setResizingAside,
  });

  /** 仅在全新草稿中居中显示空态引导和输入框。 */
  const welcomeSession =
    !session.sessionId &&
    messages.length === 0 &&
    session.state !== "streaming";
  const showWelcomeCopy = welcomeSession;
  const emptyExistingSession =
    !!session.sessionId &&
    messages.length === 0 &&
    session.state !== "streaming" &&
    session.state !== "connecting";
  /** 至少发出过一条用户消息后才展示上下文占用。 */
  const hasStartedConversation = messages.some(
    (message) => message.role === "user",
  );
  const sessionTurn = useSessionTurn({
    locale,
    session,
    activeProject,
    draft,
    attachments,
    modelLabel,
    effort,
    hasConfiguredModel,
    goalModeSessionKey,
    planModeSessionKey,
    ultraModeSessionKey,
    api: {
      isTauri: api.isTauri,
      connect: acpSessionApi.connect,
      setEffort: acpSessionApi.setEffort,
      send: acpSessionApi.send,
      stop: acpSessionApi.stop,
      steer: acpSessionApi.steer,
      prepareEditLastUser: acpSessionApi.prepareEditLastUser,
      goalUpsert: acpSessionApi.goals.upsert,
    },
    runtime: {
      acpWorkspaceRef,
      liveHostRef,
      messagesBySessionRef,
      viewingSessionIdRef,
      applyViewProjectionRef,
      commitWorkspace,
      patchSessionMessages,
      currentViewFocus,
      replayHistory,
      refreshSessions,
      applyMessagePrefixTitle,
      applyAutomaticSessionTitle,
      updateSessionPreference,
    },
    ui: {
      setSession,
      setMessages,
      setLiveHost,
      setLiveMap,
      setRetryStatus,
      setTurnStartedAt,
      setStreamStall,
      setLocalError,
      setAskUser,
      setDraft,
      setAttachments,
      setGoalModeSessionKey,
      setPlanModeSessionKey,
      setUltraModeSessionKey,
      promptHistoryIndexRef,
      setPromptHistoryIndex,
      setPromptHistoryOpen,
      setPromptHistoryFilter,
      setPromptHistoryActive,
      setPromptHistoryFocusFilter,
    },
    stateRefs: {
      sendInFlightRef,
      turnLatencyBySessionRef,
      activeTurnIdBySessionRef,
      recoverableCompletedTurnIdBySessionRef,
      completedTurnIdBySessionRef,
      pendingVisibleTurnBySessionRef,
      observeHostActiveTurn,
    },
    showToast,
    clearPendingAskUser,
  });
  const {
    ensureConnected,
    send,
    editAndResend: editAndResendLastUserMessage,
    stop,
    connecting,
    stopLatch,
    sendQueue,
    effectiveCanSend,
    effectiveCanStop,
    queuePreviewLabels,
    steerQueuedItem,
  } = sessionTurn;
  dropQueuedSessionsRef.current = sendQueue.dropSessions;
  const sessionNavigation = useSessionNavigation({
    locale,
    navigationRefs: {
      draftKeyRef,
      draftNavigationSnapshotRef,
      viewEpochRef,
      viewingSessionIdRef,
      openingSessionIdRef,
      openingSessionEpochRef,
    },
    route: { navigateWorkbench },
    runtime: {
      isTauri: api.isTauri,
      workspaceRef: acpWorkspaceRef,
      commitWorkspace,
      connect: acpSessionApi.connect,
      observeHostActiveTurn,
      replayHistory,
      applyViewProjection,
      refreshSessions,
      liveHostRef,
      messagesRef,
      messagesBySessionRef,
    },
    sidebar: {
      projects,
      activeProject,
      setActiveProject,
      setExpandedProjects,
      setHistoryOpen,
      setCompletedUnreadIds,
      pendingAskUserBySessionRef,
    },
    composer: {
      draftRef: composer.draftRef,
      attachmentsRef: composer.attachmentsRef,
      setDraft,
      setAttachments,
      requestComposerFocus,
      sendQueue,
    },
    providers: {
      modelBySessionRef,
      configuredModelsRef,
      setModelId,
    },
    ui: {
      session,
      setSession,
      setMessages,
      setLiveHost,
      setLiveMap,
      setContextUsage,
      setAskUser,
      setRetryStatus,
      setLocalError,
      closeSummary,
    },
  });
  navigationActionsRef.current = {
    newChat: sessionNavigation.newChat,
    openSession: sessionNavigation.openSession,
  };
  const { openSession, newChat } = sessionNavigation;
  const sessionLifecycle = useSessionLifecycleActions({
    locale,
    appBooting,
    providerRouteRevision,
    isTauri: api.isTauri,
    session,
    activeProject,
    projects,
    sessions,
    messages,
    navigation: { newChat, openSession },
    sidebar: {
      refreshSessions,
      archiveSession,
      setExpandedProjects,
      setHistoryOpen,
      setCtxMenu,
    },
    runtime: {
      acpWorkspaceRef,
      setAcpWorkspace,
      activeTurnIdBySessionRef,
      recoverableCompletedTurnIdBySessionRef,
      completedTurnIdBySessionRef,
      turnLatencyBySessionRef,
      pendingVisibleTurnBySessionRef,
      messagesBySessionRef,
      contextUsageBySessionRef,
      liveHostRef,
      viewingSessionIdRef,
      openingSessionIdRef,
      openingSessionEpochRef,
      pendingAskUserBySessionRef,
      dropQueuedSessionsRef,
    },
    ui: {
      setSession,
      setLiveHost,
      setMessages,
      setContextUsage,
      setPendingAskUserSessionIds,
      setAskUser,
      setRetryStatus,
      setLocalError,
      setAppDialog,
      showToast,
    },
  });
  const {
    confirmForkSession,
    exportActiveSessionMd,
    archivedSessions,
    restoreArchivedSession,
    deleteArchivedSession,
  } = sessionLifecycle;
  exportActiveSessionMdRef.current = exportActiveSessionMd;
  shortcutHandlersRef.current = {
    newChat: () => {
      void newChat();
    },
    openSettings: (section: SettingsSectionId = "general") => {
      navigateSettings(section);
    },
    openChatFind: () => {
      openChatFind();
    },
  };

  const availableUpdateVersion =
    appUpdateStatus?.latestRelease ?? appUpdateStatus?.latestVersion ?? "";
  const appUpdateAction = appUpdateActionFor(appUpdateStatus);
  const requestAppUpdateInstall = useCallback(() => {
    if (appUpdateAction !== "install") {
      setAppUpdateProgressOpen(true);
      return;
    }
    setAppUpdateProgressOpen(false);
    setAppDialog({
      kind: "confirm",
      title: tr("settings.updateConfirmTitle"),
      message: tr("settings.updateConfirm", {
        version: availableUpdateVersion,
      }),
      confirmLabel: tr("settings.updateInstall"),
      onConfirm: installAppUpdate,
    });
  }, [appUpdateAction, availableUpdateVersion, installAppUpdate, tr]);
  const sidebarUpdateLabel =
    appUpdateAction === "install"
      ? tr("sidebar.installUpdate", { version: availableUpdateVersion })
      : appUpdateAction === "retry"
        ? tr("settings.updateRetry")
        : tr("sidebar.updatePreparing", { version: availableUpdateVersion });
  // Agent 回合错误只进入对话气泡；顶部错误卡仅承载无法归属到回合的本地错误。
  const errorBanner = useMemo(
    () => presentErrorBanner(null, localError, locale),
    [localError, locale],
  );
  /** Prefer in-thread turn error; avoid stacking with the top error banner. */
  const hasChatTurnError = useMemo(
    () => messages.some((m) => m.isError),
    [messages],
  );
  // Collapse technical dump whenever the visible error changes.
  useEffect(() => {
    setErrorDetailOpen(false);
  }, [errorBanner?.code, errorBanner?.summary, errorBanner?.detail]);

  // T15: announce stream start/end once (avoid token-level noise).
  useEffect(() => {
    const streaming =
      session.state === "streaming" ||
      messages.some((m) => m.role === "assistant" && m.streaming);
    if (streaming && !wasStreamingRef.current) {
      setStreamA11yNote(tr("a11y.assistantStreaming"));
    } else if (!streaming && wasStreamingRef.current) {
      setStreamA11yNote(tr("a11y.assistantDone"));
      const t = window.setTimeout(() => setStreamA11yNote(""), 2500);
      wasStreamingRef.current = streaming;
      return () => window.clearTimeout(t);
    }
    wasStreamingRef.current = streaming;
  }, [session.state, messages, tr]);

  /** T04 错误卡片操作：重连、打开设置或关闭。 */
  const runErrorBannerAction = useCallback(
    (action: NonNullable<ErrorBannerView["primary"]>) => {
      setErrorDetailOpen(false);
      switch (action.id) {
        case "reconnect":
          setLocalError(null);
          void ensureConnected(true).then((sid) => {
            if (sid) setLocalError(null);
          });
          break;
        case "open_account":
          setLocalError(null);
          navigateSettings("account");
          break;
        case "open_providers":
          setLocalError(null);
          navigateSettings("account");
          break;
        case "dismiss":
        case "keep_waiting":
          // keep_waiting is for the stream-stall banner (clears prompt only).
          setLocalError(null);
          break;
        case "cancel_turn":
          setLocalError(null);
          void stop();
          break;
        default:
          break;
      }
    },
    [ensureConnected, navigateSettings, stop],
  );

  return (
    <ImageViewerProvider locale={locale}>
    <div
      className={
        `app-shell platform-${platform}` +
        (windowMaximized ? " is-maximized" : "") +
        (windowFullscreen ? " is-fullscreen" : "") +
        (useCustomWindowChrome ? " has-custom-chrome" : "")
      }
      data-testid="app-shell"
    >
      <WindowControls
        visible={useCustomWindowChrome}
        labels={{
          minimize: tr("window.minimize"),
          maximize: tr("window.maximize"),
          restore: tr("window.restore"),
          close: tr("window.close"),
        }}
      />

      {wallpaperUrl && wallpaperRecord ? (
        <WallpaperMediaLayer
          url={wallpaperUrl}
          kind={wallpaperRecord.kind}
          focus={wallpaperRecord.focus ?? DEFAULT_WALLPAPER_FOCUS}
          clip={wallpaperRecord.clip ?? null}
          intrinsicSize={
            wallpaperRecord.width && wallpaperRecord.height
              ? { w: wallpaperRecord.width, h: wallpaperRecord.height }
              : null
          }
          onIntrinsicSize={applyWallpaperMediaSize}
        />
      ) : null}

      {appBooting ? (
        <StartupScreen useCustomWindowChrome={useCustomWindowChrome} />
      ) : appView === "settings" ? (
        <SettingsRoute
          section={settingsSection}
          onSection={navigateSettings}
          onBack={navigateWorkbench}
          settings={{
            ...appSettings,
            onChromeHardwareAcceleration:
              platform === "win"
                ? appSettings.onChromeHardwareAcceleration
                : undefined,
          }}
          session={{
            projectPath: activeProject?.path ?? null,
            onProviderActivated: handleProviderActivated,
          }}
          archive={{
            autoArchiveConversations,
            onAutoArchiveConversations,
            archiveRetentionDays,
            onArchiveRetentionDays,
            archivedSessions,
            onRestoreArchivedSession: restoreArchivedSession,
            onDeleteArchivedSession: deleteArchivedSession,
          }}
          appearance={{
            themePreference,
            onTheme: applyThemeChoice,
            skin,
            onSkin: applySkinChoice,
            wallpaperUrl,
            wallpaperKind: wallpaperRecord?.kind ?? null,
            wallpaperFocus: wallpaperRecord?.focus ?? null,
            wallpaperClip: wallpaperRecord?.clip ?? null,
            wallpaperMediaSize:
              wallpaperRecord?.width && wallpaperRecord?.height
                ? { w: wallpaperRecord.width, h: wallpaperRecord.height }
                : null,
            onWallpaper: applyWallpaperChoice,
            onWallpaperAdjust: applyWallpaperAdjustChoice,
            onWallpaperMediaSize: applyWallpaperMediaSize,
            wallpaperScrim,
            onWallpaperScrim: applyWallpaperScrimChoice,
            wallpaperBlur,
            onWallpaperBlur: applyWallpaperBlurChoice,
            onWallpaperAppearanceReset: resetWallpaperAppearance,
          }}
          update={{
            versionFooter: appUpdateStatus
              ? `KeenCode ${appUpdateStatus.currentRelease} · MIT`
              : tr("app.versionFooter"),
            appUpdateStatus,
            appUpdateBusy,
            appUpdateError,
            appUpdateDownloadSource,
            onAppUpdateDownloadSource,
            onAppUpdateCheck: checkAppUpdate,
            onAppUpdateInstall: requestAppUpdateInstall,
          }}
        />
      ) : (
      <>
      <div className="workbench">
        {/* LEFT — fully hideable (not icon-rail); open via top-bar icon when closed */}
        <Sidebar
          frame={{ sidebarRef, layout, resizingSidebar }}
          tr={tr}
          chrome={{
            setLayout,
            setResizingSidebar,
            useCustomWindowChrome,
            toggleMaximizeFromTitlebar,
          }}
          navigation={{
            newChat,
            openSearch,
            searchTriggerRef,
            navigateSettings,
          }}
          pinned={{
            pinnedSessions,
            pinnedOpen,
            setPinnedOpen,
            session,
            busyIds,
            completedUnreadIds,
            projects,
            pendingAskUserSessionIds,
            startSidebarDrag,
            endSidebarDrag,
            dropSession,
            openSession,
            openSessionMenu,
            archiveSession,
            pinSession,
          }}
          projectTree={{
            projects,
            projectsOpen,
            setProjectsOpen,
            expandedProjects,
            setExpandedProjects,
            projectDropHint,
            startSidebarDrag,
            endSidebarDrag,
            dragOverProject,
            dropProject,
            setProjectDropHint,
            sessionsForProject,
            visibleSessionsByProject,
            setVisibleSessionsByProject,
            newChat,
            dropSession,
            session,
            busyIds,
            completedUnreadIds,
            pendingAskUserSessionIds,
            openProjectMenu,
            relocateProject,
            openSession,
            openSessionMenu,
            archiveSession,
            pinSession,
            applyProjectOrder,
            addProject,
            showToast,
          }}
          history={{
            orphanSessions,
            historyOpen,
            setHistoryOpen,
            session,
            busyIds,
            completedUnreadIds,
            pendingAskUserSessionIds,
            startSidebarDrag,
            endSidebarDrag,
            dropSession,
            openSession,
            openSessionMenu,
            archiveSession,
            pinSession,
          }}
          user={{
            labels: {
              settings: tr("sidebar.settings"),
              update: sidebarUpdateLabel,
            },
            updateAvailable: appUpdateStatus?.available === true,
            updateBusy: appUpdateBusy !== null,
            onSettings: () => navigateSettings("general"),
            onUpdate: requestAppUpdateInstall,
          }}
        />
        {/* CENTER — solid pane; top icons fully toggle L/R columns */}
        <MainStage
          stage={{
            layout,
            setLayout,
            dragZone,
            toast,
            tr,
            composerFloatPad,
            streamA11yNote,
          }}
          header={{
            useCustomWindowChrome,
            toggleMaximizeFromTitlebar,
            tr,
            sessions,
            session,
            summaryOpen,
            summaryTriggerRef,
            setSummaryOpen,
            openSessionMenu,
            newChat,
          }}
          notices={{
            tr,
            activeProject,
            relocateProject,
            emptyExistingSession,
            streamStall,
            liveMap,
            session,
            setStreamStall,
            stop,
            showChatFind,
            chatFindFocusKey,
            chatFindQuery,
            chatFindIndex,
            chatFindMatches,
            chatFindPrev,
            chatFindNext,
            setChatFindQuery,
            setChatFindIndex,
            setShowChatFind,
            errorBanner,
            hasChatTurnError,
            errorDetailOpen,
            setErrorDetailOpen,
            connecting,
            runErrorBannerAction,
            ensureConnected,
            setLocalError,
          }}
          conversation={{
            locale,
            messages,
            session,
            activeProject,
            stopLatch,
            showWelcomeCopy,
            turnStartedAt,
            retryStatus,
            setResourceOpenTarget,
            setAttachments,
            editAndResendLastUserMessage,
            attachLabels,
            showChatFind,
            chatFindQuery,
            chatFindHitIds,
            chatFindActive,
            handleFirstVisibleToken,
            activeTurnIdBySessionRef,
            displayedSubagents,
            summaryOpen,
            summaryTriggerRef,
            closeSummary,
          }}
          askUser={{
            askUser,
            askUserWrapRef,
            locale,
            tr,
            clearPendingAskUser,
            setAskUser,
            showToast,
          }}
          composer={{
            wrapRef: composerWrapRef,
            shellRef: composerShellRef,
            context: {
              locale,
              tr,
              session,
              activeProject,
              projects,
              acpSessionView,
              welcomeSession,
              bindSessionProject,
              openAddProject,
              gitWorktrees,
              gitWorktreesAvailable,
              gitWorktreesLoading,
              gitWorktreesReason,
              switchToWorktree,
              openWorktreeCreate,
              openWorktreeGc,
              refreshGitWorktrees,
              goalActions: composer,
            },
            queue: {
              tr,
              locale,
              session,
              sendQueue,
              queuePreviewLabels,
              steerQueuedItem,
              showToast,
            },
            attachments: {
              tr,
              attachments,
              attachLabels,
              setAttachments,
            },
            input: {
              locale,
              tr,
              session,
              messages,
              draft,
              setDraft,
              handleDraftChange,
              attachments,
              addPastedFiles,
              addAttachmentsFromPaths,
              pickComposerFiles,
              composerInputRef,
              composerMenuOpen,
              composerMenuEntries,
              composerMenuEntriesRef,
              slashActiveIndex,
              setSlashActiveIndex,
              applySlashItem,
              liveSlash,
              slashFilterQuery,
              skillsLoading,
              composerPlusPos,
              composerPlusStyle,
              composerPlusPanelRef,
              resolveSlashTitle,
              resolveSlashDescription,
              promptHistoryOpen,
              promptHistoryPos,
              promptHistoryStyle,
              promptHistoryPanelRef,
              promptHistoryEntries,
              promptHistoryActive,
              setPromptHistoryActive,
              promptHistoryFocusFilter,
              promptHistoryFilter,
              setPromptHistoryFilter,
              promptHistoryOpenRef,
              promptHistoryIndexRef,
              setPromptHistoryIndex,
              closePromptHistory,
              openPromptHistory,
              applyPromptHistoryEntry,
              closeComposerMenu,
              onSlashQueryChange,
              send,
              hasConfiguredModel,
            },
            toolbar: {
              tr,
              locale,
              session,
              composerPlusTriggerRef,
              composerMenuOpen,
              setShowComposerPlus,
              closeComposerMenu,
              goalModeSessionKey,
              setGoalModeSessionKey,
              planModeSessionKey,
              setPlanModeSessionKey,
              ultraModeSessionKey,
              setUltraModeSessionKey,
              acpSessionView,
              confirmClearCurrentGoal,
              modelId,
              setModelId,
              availableModels,
              activeCustomProvider,
              refreshProviderRoute,
              showToast,
              composerPanel,
              setComposerPanel,
              effort,
              setEffort,
              isValidEffort,
              isValidModelId,
              modelBySessionRef,
              viewingSessionIdRef,
              navigateSettings,
              contextUsageDisplay,
              taskCacheUsage,
              hasStartedConversation,
              draft,
              attachments,
              connecting,
              effectiveCanSend,
              effectiveCanStop,
              hasConfiguredModel,
              send,
              stop,
            },
          }}
        />
        <ResourceAside
          asideRef={asideRef}
          layout={layout}
          setLayout={setLayout}
          resizingAside={resizingAside}
          setResizingAside={setResizingAside}
          resourceOpenTarget={resourceOpenTarget}
          setResourceOpenTarget={setResourceOpenTarget}
          activeProject={activeProject}
          session={session}
          messages={messages}
          locale={locale}
          resourceSyncRevision={resourceSyncRevision}
          acpSessionView={acpSessionView}
          displayedSubagents={displayedSubagents}
          subagentModelLabels={subagentModelLabels}
          terminalFontFamily={terminalFontFamily}
          modelLabel={modelLabel}
          loadTrajectoryMessages={loadTrajectoryMessages}
        />
      </div>
      <AddProjectModal
        tr={tr}
        intent={addProjectIntent}
        name={addProjectName}
        setName={setAddProjectName}
        path={addProjectPath}
        busy={addProjectBusy}
        error={addProjectError}
        nameRef={addProjectNameRef}
        dropRef={addProjectDropRef}
        returnFocusRef={addProjectReturnFocusRef}
        nameEditedRef={addProjectNameEditedRef}
        setError={setAddProjectError}
        dragZone={dragZone}
        projectDirectory={projectDirectory}
        close={closeAddProject}
        submit={submitAddProject}
        pickDirectory={pickAddProjectDirectory}
        reset={resetAddProject}
        navigateSettings={navigateSettings}
      />
      <AppUpdateModal
        tr={tr}
        locale={locale}
        open={appUpdateProgressOpen}
        setOpen={setAppUpdateProgressOpen}
        status={appUpdateStatus}
        busy={appUpdateBusy}
        error={appUpdateError}
        check={checkAppUpdate}
        install={installAppUpdate}
      />
      <WorktreeCreateModal
        tr={tr}
        open={worktreeCreateOpen}
        setOpen={setWorktreeCreateOpen}
        busy={worktreeCreateBusy}
        startChat={worktreeCreateStartChat}
        name={worktreeCreateName}
        setName={setWorktreeCreateName}
        refName={worktreeCreateRef}
        setRefName={setWorktreeCreateRef}
        previewPath={worktreeCreatePreviewPath}
        error={worktreeCreateError}
        setError={setWorktreeCreateError}
        submit={submitWorktreeCreate}
      />
      <WorktreeGcModal
        tr={tr}
        open={worktreeGcOpen}
        setOpen={setWorktreeGcOpen}
        busy={worktreeGcBusy}
        previewBusy={worktreeGcPreviewBusy}
        force={worktreeGcForce}
        setForce={setWorktreeGcForce}
        error={worktreeGcError}
        setError={setWorktreeGcError}
        preview={worktreeGcPreview}
        setPreview={setWorktreeGcPreview}
        submit={submitWorktreeGc}
      />
      <ShortcutsModal
        tr={tr}
        open={showShortcuts}
        setOpen={setShowShortcuts}
        platform={platform}
      />
      <StatusModal
        open={showStatusModal}
        locale={locale}
        sessionId={session.sessionId}
        modelId={modelId || null}
        effort={
          availableModels
            .find((model) => model.id === modelId)
            ?.reasoningEfforts?.some((entry) => entry.id === effort)
            ? effort
            : null
        }
        projectPath={activeProject?.path ?? null}
        messageCount={messages.length}
        onClose={() => setShowStatusModal(false)}
      />
      <SessionSearchPortal
        tr={tr}
        open={showSearch}
        setOpen={setShowSearch}
        query={searchQuery}
        setQuery={setSearchQuery}
        returnFocusRef={searchReturnFocusRef}
        hits={searchHits}
        projects={projects}
        sessions={sessions}
        activeProject={activeProject}
        openSession={openSession}
        newChat={newChat}
        addProject={addProject}
        setProjectsOpen={setProjectsOpen}
        setExpandedProjects={setExpandedProjects}
      />
      <AppDialogPortal
        tr={tr}
        appDialog={appDialog}
        setAppDialog={setAppDialog}
        dialogInput={dialogInput}
        setDialogInput={setDialogInput}
        dialogInputRef={dialogInputRef}
        confirmBtnRef={confirmBtnRef}
        appDialogRef={appDialogRef}
      />
      <SessionContextMenu
        tr={tr}
        locale={locale}
        menu={ctxMenu}
        setMenu={setCtxMenu}
        projects={projects}
        sessions={sessions}
        setLocalError={setLocalError}
        relocateProject={relocateProject}
        removeProjectFromApp={removeProjectFromApp}
        renameProject={renameProject}
        renameSession={renameSession}
        confirmForkSession={confirmForkSession}
        viewTrajectory={viewTrajectory}
        copySessionId={copySessionId}
      />
      <span hidden data-layout-default={JSON.stringify(DEFAULT_LAYOUT)} />
      </>
      )}
    </div>
    </ImageViewerProvider>
  );
}
