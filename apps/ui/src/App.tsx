import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { useThemeAppearance } from "@/hooks/useThemeAppearance";
import { useWindowChrome } from "@/hooks/useWindowChrome";
import { useAppUpdate } from "@/hooks/useAppUpdate";
import { useAppSettings } from "@/hooks/useAppSettings";
import { useProviderModels } from "@/hooks/useProviderModels";
import { useAppDialog } from "@/hooks/useAppDialog";
import { useAppRoute } from "@/hooks/useAppRoute";
import { useChatFind } from "@/hooks/useChatFind";
import { useAcpSessionRuntime } from "@/hooks/useAcpSessionRuntime";
import { useSessionTurn } from "@/hooks/useSessionTurn";
import { useComposerController } from "@/hooks/useComposerController";
import { useSidebarController } from "@/hooks/useSidebarController";
import { useTrayMenu } from "@/hooks/useTrayMenu";
import { useUnreadTerminalResults } from "@/hooks/useUnreadTerminalResults";
import { useSessionNavigation, type SessionNavigationNewChat, type SessionNavigationOpenSession } from "@/hooks/useSessionNavigation";
import { useWorkbenchDragResize } from "@/hooks/useWorkbenchDragResize";
import { useFrontendObservability } from "@/hooks/useFrontendObservability";
import { useVisualViewportLayout } from "@/hooks/useVisualViewportLayout";
import { pruneUnprotectedSessionMessageCache } from "@/hooks/acp-runtime/messageCache";
import { useProjectDialog } from "@/hooks/useProjectDialog";
import { useAskUserGate } from "@/hooks/useAskUserGate";
import { useSessionUsagePanel } from "@/hooks/useSessionUsagePanel";
import { useStreamStatusBanner } from "@/hooks/useStreamStatusBanner";
import { useSubagentMetadata } from "@/hooks/useSubagentMetadata";
import { useWorktrees } from "@/hooks/useWorktrees";
import { acpSessionApi, useSessionLifecycleActions } from "@/hooks/useSessionLifecycleActions";
import {
  type SidebarResizeStart,
  loadInitialLayout,
} from "@/lib/layout";
import type { DragZone } from "@/lib/dragZone";
import {
  isSessionLiveStreaming,
  localizeUiError,
  type ErrorBannerView,
  IDLE_SNAPSHOT,
  type ChatMessage,
  type SessionSnapshot,
} from "@/lib/session";
import * as api from "@/lib/api";
import { type ViewFocus } from "@/lib/viewFocus";
import {
  busySessionIds,
  fillAuthoritativeBusyIds,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import { reconcileHostActiveTurnSnapshot } from "@/lib/activeTurn";
import { createT } from "@/i18n";
import { appUpdateActionFor } from "@/lib/appUpdate";
import { isProjectPathMissing } from "@/lib/projectPath";
import { type ResourceOpenTarget } from "@/components/ResourceViewer";
import { type TurnLatencyState } from "@/lib/turnLatency";
import { canWriteProjects } from "@/lib/hostCapabilities";
import type {
  DraftNavigationLocation,
  DraftNavigationSnapshot,
} from "@/lib/draftNavigation";
import { ImageViewerProvider } from "@/components/ImageViewer";
import { StartupScreen } from "@/components/StartupScreen";
import { extractFirstUserMessageText } from "@/lib/sessionTitle";
import { createAcpWorkspaceState, type AcpWorkspaceState } from "@/lib/acp/store";
import { projectAcpConversation } from "@/lib/sessionProjection";
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
import { RemoteControlModal } from "@/features/app/overlays/RemoteControlModal";
import { WorktreeCreateModal } from "@/features/app/overlays/WorktreeCreateModal";
import { WorktreeGcModal } from "@/features/app/overlays/WorktreeGcModal";
import { StatusModal } from "@/components/StatusModal";
import type { Project } from "@/features/app/models";
export default function App() {
  const projectWritesAllowed = canWriteProjects(api.isTauri());
  useVisualViewportLayout();
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
    setAcpWorkspace((previous) => ({
      sessions: Object.fromEntries(
        Object.entries(acpWorkspaceRef.current.sessions).map(([id, view]) => [
          id,
          view.replay.restoring && previous.sessions[id]
            ? previous.sessions[id]
            : { ...view },
        ]),
      ),
    }));
  }, []);
  const {
    themePreference,
    baseColor,
    primaryColor,
    secondaryColor,
    effortColor,
    applyBaseColorChoice,
    applyPrimaryColorChoice,
    applySecondaryColorChoice,
    applyEffortColorChoice,
    uiFontSize,
    applyThemeChoice,
    applyUiFontSizeChoice,
  } = useThemeAppearance();
  const [layout, setLayout] = useState(() => loadInitialLayout(localStorage, window.innerWidth));
  const sidebarRef = useRef<HTMLElement>(null);
  const sidebarResizeStartRef = useRef<SidebarResizeStart | null>(null);
  const asideRef = useRef<HTMLElement>(null);
  const [session, setSession] = useState<SessionSnapshot>(IDLE_SNAPSHOT);
  /** Host live agent (may differ from the session currently viewed in the UI). */
  const [liveHost, setLiveHost] = useState<SessionSnapshot>(IDLE_SNAPSHOT);
  /** 多会话运行状态投影，用于展示后台任务忙碌状态。 */
  const [liveMap, setLiveMap] = useState<SessionLiveMap>({});
  /** Latest live map for callbacks that must not close over a stale render. */
  const liveMapRef = useRef(liveMap);
  liveMapRef.current = liveMap;
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  /** 可见会话上下文用量与任务缓存用量投影。 */
  const {
    contextUsage,
    setContextUsage,
    contextUsageBySessionRef,
    taskCacheUsage,
    setTaskCacheUsage,
    taskCacheUsageRequestSeqRef,
  } = useSessionUsagePanel();
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
  /** 当前渲染 Session 的 ACP 原生视图；草稿没有持久化视图。 */
  const acpSessionView = useMemo(
    () =>
      session.sessionId
        ? acpWorkspace.sessions[session.sessionId] ?? null
        : null,
    [acpWorkspace, session.sessionId],
  );
  const subagentIdentityKey = (acpSessionView?.subagents ?? [])
    .map((agent) => `${agent.agent_id}:${agent.agent_name}`)
    .join("|");
  const {
    descriptions: subagentDescriptions,
    modelLabels: subagentModelLabels,
  } = useSubagentMetadata({
    projectPath: activeProject?.path ?? null,
    subagentIdentityKey,
  });
  const displayedSubagents = useMemo(
    () => (acpSessionView?.subagents ?? []).map((agent) => ({
      ...agent,
      agent_description: subagentDescriptions[agent.agent_name],
    })),
    [acpSessionView, subagentDescriptions],
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
  /** AskUser 追问门与按会话未回答问题账本。 */
  const {
    askUser,
    setAskUser,
    askUserWrapRef,
    pendingAskUserBySessionRef,
    pendingAskUserSessionIds,
    setPendingAskUserSessionIds,
    clearPendingAskUser,
    clearPendingAskUserRef,
  } = useAskUserGate();
  /** Desktop Connect panel (AC7) — close does not stop host. */
  /** While openSession loads, do not let session.sessionId effect clobber viewing id. */
  const openingSessionIdRef = useRef<string | null>(null);
  /** Distinguishes two overlapping opens of the same Session. */
  const openingSessionEpochRef = useRef<number | null>(null);
  // ContextMenu handles outside click + Escape for sidebar menus.
  const {
    appView,
    settingsSection,
    settingsProviderId,
    navigateWorkbench,
    navigateSettings,
  } = useAppRoute();
  useFrontendObservability({ appView, settingsSection });
  /** 首次渲染时展示品牌启动页；工作台外壳不等待会话状态。 */
  const [appBooting, setAppBooting] = useState(true);
  /** 后台终态未读结果：侧栏标记与 Dock 角标共用同一份状态。 */
  const { unreadTerminalResults, setUnreadTerminalResults } =
    useUnreadTerminalResults(appBooting);
  const [toast, setToast] = useState<string | null>(null);
  const showToast = useCallback((msg: string, ms = 3200) => {
    setToast(msg);
    window.setTimeout(() => {
      setToast((cur) => (cur === msg ? null : cur));
    }, ms);
  }, []);
  const [showShortcuts, setShowShortcuts] = useState(false);
  const [showRemoteControl, setShowRemoteControl] = useState(false);
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
  /** Polite SR announce for stream start/stop (not every token). */
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
    sessionModelReference,
    sessionProviderId,
    setSessionModelReference,
    applyHostConfigOptions,
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
  /** Composer 修改思考强度时同步工作区视图；投影恢复以视图值为准，避免旧值顶回。 */
  const composerSetEffort = useCallback(
    (next: SetStateAction<string>) => {
      setEffort(next);
      if (typeof next !== "string") return;
      const sessionId = viewingSessionIdRef.current;
      const view = sessionId
        ? acpWorkspaceRef.current.sessions[sessionId]
        : undefined;
      if (view) view.reasoning_effort = next;
    },
    [setEffort],
  );
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
  /** Agent 工具状态变化时驱动右侧文件树与 Git 状态同步。
   *
   * 只折叠工具段的状态序数与标识短后缀：流式期间 messages 每帧变化，
   * 全量逐字符哈希长会话的成本不可接受；状态迁移必然改变签名，标识
   * 后缀碰撞最多漏一次文件树刷新，属可接受的触发器语义。 */
  const resourceSyncRevision = useMemo(
    () =>
      messages.reduce((revision, message) => {
        for (const segment of message.segments ?? []) {
          if (segment.kind !== "tool") continue;
          const id = segment.toolCallId.slice(-8);
          let toolHash = 0;
          for (const char of id) toolHash = (toolHash * 31 + char.charCodeAt(0)) >>> 0;
          revision =
            (revision * 31 +
              toolHash +
              segment.status.length +
              (segment.streaming ? 1 : 0)) >>>
            0;
        }
        return revision;
      }, 0),
    [messages],
  );
  /** Live drag-drop target for the add-project source control (null = not dragging). */
  const [dragZone, setDragZone] = useState<DragZone>(null);
  /** 顶部流状态横幅：本地错误卡、流卡顿、重试进度与无障碍播报。 */
  const {
    streamA11yNote,
    setLocalError,
    errorDetailOpen,
    setErrorDetailOpen,
    streamStall,
    setStreamStall,
    retryStatus,
    setRetryStatus,
    errorBanner,
  } = useStreamStatusBanner({
    locale,
    streaming:
      session.state === "streaming" ||
      messages.some((m) => m.role === "assistant" && m.streaming),
    a11yStreaming: tr("a11y.assistantStreaming"),
    a11yDone: tr("a11y.assistantDone"),
  });
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
      goals: {
        get: acpSessionApi.goals.get,
        clear: acpSessionApi.goals.clear,
        transition: acpSessionApi.goals.transition,
        upsert: acpSessionApi.goals.upsert,
      },
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
    retryAttachment,
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
    closePromptHistory,
    openPromptHistory,
    applyPromptHistoryEntry,
    composerInputRef,
    composerShellRef,
    composerWrapRef,
    composerPlusTriggerRef,
    composerHeight,
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
    saveCurrentGoal,
    pauseCurrentGoal,
    resumeCurrentGoal,
  } = composer;
  const sidebar = useSidebarController({
    locale,
    canWriteProjects: projectWritesAllowed,
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
              backend: "acp",
            }
          : previous,
      );
      setLiveHost((previous) =>
        previous.sessionId ? { ...IDLE_SNAPSHOT } : previous,
      );
    },
    onActiveProjectRemoved: () => {
      viewingSessionIdRef.current = null;
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
    toggleProject,
    setExpandedProjects,
    visibleSessionsByProject,
    setVisibleSessionsByProject,
    sessionSortMode,
    setSessionSortMode,
    markSessionUserMessage,
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
    loadAllSessions,
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
    if (appBooting || appView !== "workbench") return;
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
  }, [appBooting, appView, openSearch]);
  const {
    applyViewProjection,
    applyViewProjectionRef,
    handleFirstVisibleToken,
    invalidateContextUsage,
    replayHistory,
    connectSession,
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
    applyHostConfigOptions,
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
    setEffort,
    setSessionModelReference,
    setPlanModeSessionKey,
    promptHistoryIndexRef,
    setPromptHistoryIndex,
    setPromptHistoryOpen,
    setPromptHistoryFilter,
    setPromptHistoryActive,
    setPromptHistoryFocusFilter,
    setUnreadTerminalResults,
  });
  composerApplyViewProjectionRef.current = applyViewProjection;
  /**
   * 多会话忙碌标识，用于侧栏运行中状态。
   * Uses liveMap projection + liveHost fallback. Excludes connecting.
   * 崩溃恢复或未重新连接的后台会话没有 live 投影，由 session/list 的权威
   * running 元数据补齐；已有 live 投影的会话以 live 状态为准。
   */
  const runningSessionIds = useMemo(
    () => new Set(sessions.filter((session) => session.running).map((session) => session.id)),
    [sessions],
  );
  const busyIds = useMemo(() => {
    const set = fillAuthoritativeBusyIds(busySessionIds(liveMap), liveMap, runningSessionIds);
    if (liveHost.sessionId && isSessionLiveStreaming(liveHost.state)) {
      set.add(liveHost.sessionId);
    }
    return set;
  }, [liveMap, liveHost.sessionId, liveHost.state, runningSessionIds]);
  /** 轨迹台账的数据源：内存缓存优先，其次通过标准 Session 恢复链重建。 */
  const loadTrajectoryMessages = useCallback(
    async (id: string): Promise<ChatMessage[]> => {
      const cached = messagesBySessionRef.current.get(id);
      if (cached?.length) return cached;
      try {
        await replayHistory(id);
        const view = acpWorkspaceRef.current.sessions[id];
        const recovered = view ? projectAcpConversation([], view, locale, false) : [];
        messagesBySessionRef.current.set(id, recovered);
        return recovered;
      } catch { return []; }
    },
    [locale, replayHistory],
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
   * 切换工作目录。ACP Session 的工作目录不可变，已有会话时进入目标项目的新草稿。
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
    canWriteProjects: projectWritesAllowed,
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
    sidebarRef,
    sidebarResizeStartRef,
    asideRef,
    layout,
    setLayout,
    resizingSidebar,
    setResizingSidebar,
    resizingAside,
    setResizingAside,
  });
  /**
   * openSession 会先切换 messages、等 connect/replay 完成才更新 session 快照；
   * 空态与欢迎态必须对齐 viewingSessionIdRef，否则加载窗口会把目标会话
   * 误报成“当前对话还没有消息”或闪现草稿欢迎页。
   */
  const viewingSessionId = viewingSessionIdRef.current;
  /** 仅在全新草稿中居中显示空态引导和输入框。 */
  const welcomeSession =
    !session.sessionId &&
    viewingSessionId == null &&
    messages.length === 0 &&
    session.state !== "streaming";
  const showWelcomeCopy = welcomeSession;
  const emptyExistingSession =
    !!session.sessionId &&
    session.sessionId === viewingSessionId &&
    messages.length === 0 &&
    session.state !== "streaming" &&
    session.state !== "connecting";
  const sessionTurn = useSessionTurn({
    locale,
    session,
    activeProject,
    draft,
    attachments,
    modelLabel,
    effort,
    hasConfiguredModel,
    modelReference: sessionModelReference,
    goalModeSessionKey,
    planModeSessionKey,
    ultraModeSessionKey,
    api: {
      isTauri: api.isTauri,
      connect: connectSession,
      setModel: acpSessionApi.setModel,
      setEffort: acpSessionApi.setEffort,
      send: acpSessionApi.send,
      stop: acpSessionApi.stop,
      steer: acpSessionApi.steer,
      rewind: acpSessionApi.rewind,
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
      markSessionUserMessage,
      clearDraftNavigationSnapshot: () => {
        draftNavigationSnapshotRef.current = null;
      },
    },
    ui: {
      setSession,
      setMessages,
      setLiveHost,
      setLiveMap,
      setRetryStatus,
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
    draftKeyRef,
    showToast,
    clearPendingAskUser,
  });
  const {
    ensureConnected,
    send,
    editAndResend: editAndResendLastUserMessage,
    stop,
    connecting,
    sendQueue,
    effectiveCanSend,
    effectiveCanStop,
    queuePreviewLabels,
    steerQueuedItem,
  } = sessionTurn;
  dropQueuedSessionsRef.current = sendQueue.dropSessions;
  useEffect(() => {
    pruneUnprotectedSessionMessageCache(messagesBySessionRef.current, busyIds,
      sendQueue.queuedSessionIds, session.sessionId, pendingAskUserSessionIds);
  }, [busyIds, pendingAskUserSessionIds, sendQueue.queuedSessionIds, session.sessionId]);
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
      connect: connectSession,
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
      sessions,
      activeProject,
      setActiveProject,
      setExpandedProjects,
      setHistoryOpen,
      setUnreadTerminalResults,
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
      setSessionModelReference,
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
  const {
    openSession,
    newChat,
    canGoBack,
    canGoForward,
    goBack,
    goForward,
  } = sessionNavigation;
  // 托盘菜单跨侧栏会话投影与导航，属于跨业务域协调，因此在这里装配。
  useTrayMenu({
    locale,
    sessions,
    projects,
    appBooting,
    newChat,
    openSession,
  });
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
      loadAllSessions,
      archiveSession,
      setExpandedProjects,
      setHistoryOpen,
      setCtxMenu,
    },
    runtime: {
      acpWorkspaceRef,
      replayHistory,
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
  // 设置路由与侧栏数据跨域协调：仅进入归档页时请求完整会话列表。
  useEffect(() => {
    if (appView === "settings" && settingsSection === "archived") {
      void loadAllSessions();
    }
  }, [appView, settingsSection, loadAllSessions]);
  const {
    confirmForkSession,
    exportActiveSessionMd,
    archivedSessions,
    restoreArchivedSession,
    deleteArchivedSession,
  } = sessionLifecycle;
  const currentForkSession = session.sessionId ? sessions.find((item) => item.id === session.sessionId) : undefined;
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
  /** Prefer in-thread turn error; avoid stacking with the top error banner. */
  const hasChatTurnError = useMemo(
    () => messages.some((m) => m.isError),
    [messages],
  );
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
      {appBooting ? (
        <StartupScreen useCustomWindowChrome={useCustomWindowChrome} />
      ) : appView === "settings" ? (
        <SettingsRoute
          onMemoryFileRefresh={appSettings.onMemoryFileRefresh}
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
            providerId: settingsProviderId,
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
            baseColor,
            primaryColor,
            secondaryColor,
            effortColor,
            onBaseColor: applyBaseColorChoice,
            onPrimaryColor: applyPrimaryColorChoice,
            onSecondaryColor: applySecondaryColorChoice,
            onEffortColor: applyEffortColorChoice,
            uiFontSize,
            onUiFontSize: applyUiFontSizeChoice,
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
            sidebarResizeStartRef,
            canGoBack,
            canGoForward,
            goBack, goForward,
            useCustomWindowChrome,
            toggleMaximizeFromTitlebar,
          }}
          navigation={{
            newChat,
            openSearch,
            openPluginMarketplace: () => navigateSettings("market"),
            searchTriggerRef,
          }}
          pinned={{
            pinnedSessions,
            pinnedOpen,
            setPinnedOpen,
            session,
            busyIds,
            unreadTerminalResults,
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
            canWriteProjects: projectWritesAllowed,
            projectsOpen,
            setProjectsOpen,
            expandedProjects,
            toggleProject,
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
            unreadTerminalResults,
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
            sessionSortMode,
            onSessionSortModeChange: setSessionSortMode,
          }}
          history={{
            orphanSessions,
            historyOpen,
            setHistoryOpen,
            session,
            busyIds,
            unreadTerminalResults,
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
            remoteControl: {
              label: tr("sidebar.remoteControl"),
              onClick: () => setShowRemoteControl(true),
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
            toast,
            tr,
            composerHeight,
            streamA11yNote,
          }}
          header={{
            useCustomWindowChrome,
            toggleMaximizeFromTitlebar,
            tr,
            sessions,
            activeProject,
            projects,
            gitWorktrees,
            bindSessionProject,
            session,
            summaryOpen,
            summaryTriggerRef,
            setSummaryOpen,
            openSessionMenu,
            newChat,
            canGoBack,
            canGoForward,
            goBack,
            goForward,
          }}
          notices={{
            tr,
            activeProject,
            canWriteProjects: projectWritesAllowed,
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
            showWelcomeCopy,
            turnStartedAt: acpSessionView?.turn_started_at ?? null,
            retryStatus,
            setResourceOpenTarget,
            setAttachments,
            editAndResendLastUserMessage,
            onForkCurrentSession: currentForkSession ? () => confirmForkSession(currentForkSession) : undefined,
            attachLabels,
            showChatFind,
            chatFindQuery,
            chatFindHitIds,
            chatFindActive,
            handleFirstVisibleToken,
            activeTurnIdBySessionRef,
            displayedSubagents,
            showThinkingProcess: appSettings.showThinkingProcess,
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
              canWriteProjects: projectWritesAllowed,
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
              editCurrentGoal: () => {
                if (!session.sessionId || !acpSessionView?.goal.goal) return;
                setResourceOpenTarget({ type: "goal", sessionId: session.sessionId });
                setLayout((current) => ({ ...current, asideCollapsed: false }));
              },
              confirmClearCurrentGoal,
              pauseCurrentGoal: () => void pauseCurrentGoal(),
              resumeCurrentGoal: () => {
                // 恢复状态由后端按当前迭代终态调度，避免额外发送重复的根 Turn。
                void resumeCurrentGoal();
              },
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
              retryAttachment,
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
              mentionSessions: sessions,
              loadMentionPlugins: () =>
                api.pluginsList(activeProject?.path ?? null).then((result) => result.plugins),
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
              resolveSlashTitle,
              resolveSlashDescription,
              promptHistoryOpen,
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
              sessionProviderId,
              setSessionModelReference,
              availableModels,
              activeCustomProvider,
              refreshProviderRoute,
              showToast,
              composerPanel,
              setComposerPanel,
              effort,
              setEffort: composerSetEffort,
              isValidEffort,
              isValidModelId,
              modelBySessionRef,
              viewingSessionIdRef,
              invalidateContextUsage,
              navigateSettings,
              contextUsageDisplay,
              taskCacheUsage,
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
          onSaveGoal={saveCurrentGoal}
          activeProject={activeProject}
          session={session}
          messages={messages}
          locale={locale}
          resourceSyncRevision={resourceSyncRevision}
          acpSessionView={acpSessionView}
          displayedSubagents={displayedSubagents}
          subagentModelLabels={subagentModelLabels}
          terminalFontFamily={terminalFontFamily}
          showThinkingProcess={appSettings.showThinkingProcess}
          modelLabel={modelLabel}
          loadTrajectoryMessages={loadTrajectoryMessages}
        />
      </div>
      {projectWritesAllowed ? <AddProjectModal
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
      /> : null}
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
      <RemoteControlModal
        tr={tr}
        open={showRemoteControl}
        setOpen={setShowRemoteControl}
        onOpenSettings={() => {
          setShowRemoteControl(false);
          navigateSettings("general");
        }}
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
        platform={platform}
        tr={tr}
        open={showSearch}
        setOpen={setShowSearch}
        query={searchQuery}
        setQuery={setSearchQuery}
        returnFocusRef={searchReturnFocusRef}
        hits={searchHits}
        projects={projects}
        canWriteProjects={projectWritesAllowed}
        sessions={sessions}
        activeProject={activeProject}
        openSession={openSession}
        newChat={newChat}
        addProject={addProject}
        setProjectsOpen={setProjectsOpen}
        setExpandedProjects={setExpandedProjects}
      />
      <SessionContextMenu
        tr={tr}
        locale={locale}
        menu={ctxMenu}
        setMenu={setCtxMenu}
        projects={projects}
        canWriteProjects={projectWritesAllowed}
        sessions={sessions}
        setLocalError={setLocalError}
        relocateProject={relocateProject}
        removeProjectFromApp={removeProjectFromApp}
        renameProject={renameProject}
        renameSession={renameSession}
        confirmForkSession={confirmForkSession}
        viewTrajectory={viewTrajectory}
        copySessionId={copySessionId}
        archiveSession={(session) => archiveSession(session)}
      /></>
      )}
      {/* 更新浮层与当前视图无关：设置页同样需要显示更新进度和安装确认。 */}
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
    </div>
    </ImageViewerProvider>
  );
}
