import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable";
import { Toaster } from "@/components/ui/sonner";
import { toast } from "sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { consumeLaunchFiles, getLaunchDir } from "@/lib/launchDir";
import { quoteShellArg } from "@/lib/shellQuote";
import { usePresence } from "@/lib/usePresence";
import { useZoom } from "@/lib/useZoom";
import { useTranslation } from "@/modules/i18n";
import { cn, isMarkdownPath } from "@/lib/utils";
import {
  type AgentLaunchRequest,
  AgentNotificationsBridge,
  findAgentLauncher,
  validateAgentLaunchCommand,
} from "@/modules/agents";
import {
  AgentRunBridge,
  AiMiniWindow,
  LocalAgentNotificationsBridge,
  SelectionAskAi,
  useAiBootstrap,
  useAiLiveBridge,
  useChatStore,
  useSelectionAskAi,
} from "@/modules/ai";
import { AiComposerProvider } from "@/modules/ai/lib/composer";
import { native } from "@/modules/ai/lib/native";
import { ProjectTasksSidebar } from "@/modules/ai/components/ProjectTasksSidebar";
import { SettingsOverlay } from "@/app/components/SettingsOverlay";
import {
  ensureDefaultChatDirectory,
  getDefaultChatDirectory,
} from "@/modules/ai/lib/defaultChatDirectory";
import { useApplyUiFontSize } from "@/modules/settings/useApplyUiFontSize";
import {
  isSettingsShortcutAllowed,
  useSettingsOverlay,
} from "@/modules/settings/settingsOverlay";
import { NewProjectDialog } from "@/modules/ai/components/NewProjectDialog";
import { resolveSessionProject } from "@/modules/ai/lib/projectTasks";
import { isSessionNavigationLocked } from "@/modules/ai/store/chatStore";
import { CommandPalette, createCommandItems } from "@/modules/command-palette";
import { useControlBridge } from "@/modules/control";
import {
  type EditorPaneHandle,
  NewEditorDialog,
  useApplyEditorFontSize,
  useEditorFileSync,
} from "@/modules/editor";
import { FileExplorer, type FileExplorerHandle } from "@/modules/explorer";
import { Header } from "@/modules/header";
import { setLspNavigator } from "@/modules/lsp";
import type { PreviewPaneHandle } from "@/modules/preview";
import { openSettingsWindow } from "@/modules/settings/openSettingsWindow";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { setShowHidden } from "@/modules/settings/store";
import {
  type ShortcutHandlers,
  type ShortcutId,
  useGlobalShortcuts,
} from "@/modules/shortcuts";
import {
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  useSidebarPanel,
} from "@/modules/sidebar";
import {
  SourceControlPanel,
  useRepositoryTargeting,
  useSourceControlContext,
} from "@/modules/source-control";
import { useSpaces, useSpacesBoot } from "@/modules/spaces";
import {
  TabSwitcherHud,
  type CloseTabsPlan,
  useTabSwitcher,
  useTabs,
  useWindowTitle,
} from "@/modules/tabs";
import { DEFAULT_SPACE_ID } from "@/modules/tabs/lib/useTabs";
import {
  openUtilityTab,
  toolViewAfterClose,
  type UtilityTool,
} from "@/app/lib/developmentTools";
import {
  clearFocusedTerminal,
  disposeSession,
  findLeafCwd,
  hasLeaf,
  isTerminalSurfaceTarget,
  leafIds,
  navigateFocusedBlocks,
  ptyIdForLeaf,
  type TerminalPaneHandle,
  useAgentActivityStore,
  useTerminalFileDrop,
  whenSessionReady,
  writeToSession,
} from "@/modules/terminal";
import {
  ThemeProvider,
  useThemeFileEditing,
  WindowVibrancyBridge,
} from "@/modules/theme";
import { UpdaterDialog } from "@/modules/updater";
import {
  LOCAL_WORKSPACE,
  useWorkspaceEnvStore,
  workspaceScopeKey,
} from "@/modules/workspace";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  lazy,
  Suspense,
} from "react";
import type { PanelImperativeHandle } from "react-resizable-panels";
import { CloseDialogs } from "./components/CloseDialogs";
import { WorkspaceInputBar } from "@/app/components/WorkspaceInputBar";
import { DevelopmentToolsTabs } from "./components/DevelopmentToolsTabs";
import { EmptyToolsPanel } from "@/app/components/EmptyToolsPanel";
import { WorkspaceSurface } from "./components/WorkspaceSurface";
import { workspacePresentationId } from "@/app/lib/workspacePresentation";
import { useAppCloseGuard } from "./hooks/useAppCloseGuard";
import { hasOpenPathTab, renamedPath } from "./hooks/tabCloseGuards";
import { useTabCloseGuards } from "./hooks/useTabCloseGuards";
import { useWorkspaceSwitcher } from "./hooks/useWorkspaceSwitcher";
import {
  useTaskSidebarChrome,
  useTaskSidebarWorkspace,
} from "@/app/hooks/useTaskSidebar";
import { useTaskSidebars } from "@/app/store/taskSidebars";

const AgentWorkbench = lazy(() =>
  import("@/modules/ai/components/AgentWorkbench").then((module) => ({
    default: module.AgentWorkbench,
  })),
);

export default function App() {
  const tr = useTranslation();
  const activeTaskId = useChatStore((s) => s.activeSessionId);
  const draftSession = useChatStore((s) => s.draftSession);
  const isNewConversation = draftSession?.id === activeTaskId;
  const sessionLoading = useChatStore((s) => s.sessionLoading);
  const {
    toolsOpen,
    toolView,
    utilityTabs,
    setToolsOpen,
    setToolView,
    setUtilityTabs,
    setToolsWidth,
    hydrated: sidebarsHydrated,
    error: sidebarError,
  } = useTaskSidebarChrome(activeTaskId, isNewConversation);
  // 默认显示文件标签；仅进入终端或编辑器时才挂载原生工具栈。
  const [toolsLoaded, setToolsLoaded] = useState(false);
  const [filesLoaded, setFilesLoaded] = useState(false);
  const toolsPanelRef = useRef<PanelImperativeHandle | null>(null);
  const [newProjectOpen, setNewProjectOpen] = useState(false);
  const {
    tabs,
    activeId,
    setActiveId,
    allocId,
    booted,
    replaceTabs,
    reorderTabByGap,
    markBooted,
    setActiveSpaceForNewTabs,
    setActiveTaskForNewTabs,
    newTab,
    newBlockTab,
    newAgentTab,
    newAgentGroupTab,
    newPrivateTab,
    openFileTab,
    pinTab,
    newPreviewTab,
    newMarkdownTab,
    setMarkdownView,
    setOverrideLanguage,
    openAiDiffTab,
    closeAiDiffTab,
    openGitDiffTab,
    openCommitHistoryTab,
    openCommitFileDiffTab,
    closeTab,
    closeTabs,
    updateTab,
    selectByIndex,
    setLeafCwd,
    focusPane,
    splitActivePane,
    closeActivePane,
    closePaneByLeaf,
    resetWorkspace,
  } = useTabs(getLaunchDir() ? { cwd: getLaunchDir() } : undefined);

  // Mirror `tabs` into a ref so callbacks scheduled with `setTimeout`
  // (e.g. cdInNewTab) read the latest pane state instead of a stale closure.
  const tabsRef = useRef(tabs);
  const activeIdRef = useRef(activeId);

  const activeTerminalTab = useMemo(() => {
    const t = tabs.find((x) => x.id === activeId && x.taskId === activeTaskId);
    return t && t.kind === "terminal" ? t : null;
  }, [tabs, activeId, activeTaskId]);
  const activeLeafId = activeTerminalTab?.activeLeafId ?? null;

  const terminalRefs = useRef<Map<number, TerminalPaneHandle>>(new Map());
  const editorRefs = useRef<Map<number, EditorPaneHandle>>(new Map());
  const previewRefs = useRef<Map<number, PreviewPaneHandle>>(new Map());
  const [activeEditorHandle, setActiveEditorHandle] =
    useState<EditorPaneHandle | null>(null);
  const { zoomIn, zoomOut, zoomReset } = useZoom();
  useApplyEditorFontSize();
  useApplyUiFontSize();
  const terminalPathDropTarget = useTerminalFileDrop();
  const explorerRef = useRef<FileExplorerHandle>(null);

  // Drives session disposal off the pane tree, not React lifecycles —
  // split/unsplit re-mount components but the leaf is still live.
  const liveLeavesRef = useRef<Set<number>>(new Set());

  const clearWorkspaceState = useCallback(() => {
    for (const id of liveLeavesRef.current) disposeSession(id);
    terminalRefs.current.clear();
    editorRefs.current.clear();
    previewRefs.current.clear();
    setActiveEditorHandle(null);
  }, []);

  const workspaceEnv = useWorkspaceEnvStore((s) => s.env);
  const setWorkspaceEnv = useWorkspaceEnvStore((s) => s.setEnv);
  const { home, launchCwd, launchCwdResolved, adoptWorkspaceEnv } =
    useWorkspaceSwitcher({
      tabsRef,
      workspaceEnv,
      setWorkspaceEnv,
      resetWorkspace,
      clearWorkspaceState,
    });

  const activeSpaceId = useSpaces((s) => s.activeId);
  const projects = useSpaces((s) => s.spaces);
  const taskSessions = useChatStore((s) => s.sessions);
  const activeTask =
    taskSessions.find((s) => s.id === activeTaskId) ??
    (isNewConversation ? draftSession : undefined);
  const projectless = !!activeTask?.projectless;
  const [defaultChatRoot, setDefaultChatRoot] = useState(
    getDefaultChatDirectory,
  );
  useEffect(() => {
    if (!projectless || !activeTaskId) return;
    let cancelled = false;
    void ensureDefaultChatDirectory()
      .then((root) => {
        if (!cancelled) setDefaultChatRoot(root);
      })
      .catch((error) => {
        if (!cancelled) toast.error(String(error));
      });
    return () => {
      cancelled = true;
    };
  }, [projectless, activeTaskId]);
  const workspaceSessions = useMemo(
    () => (draftSession ? [...taskSessions, draftSession] : taskSessions),
    [taskSessions, draftSession],
  );
  // 独立对话的工具使用默认命名空间，不能归入最后选中的真实项目。
  const toolSpaceId = projectless
    ? DEFAULT_SPACE_ID
    : (activeSpaceId ?? DEFAULT_SPACE_ID);
  const sessionsHydrated = useChatStore((s) => s.sessionsHydrated);
  useEffect(() => {
    const started = listen<unknown>("rcode:settings-open", (event) =>
      useSettingsOverlay.getState().show(event.payload),
    ).catch((error) => {
      console.error("settings listener failed", error);
      return undefined;
    });
    return () => {
      void started
        .then((stop) => stop?.())
        .catch((error) =>
          console.error("settings listener cleanup failed", error),
        );
    };
  }, []);
  const spacesHydrated = useSpaces((s) => s.hydrated);
  const activeSpaceIdRef = useRef(activeSpaceId);
  useLayoutEffect(() => {
    tabsRef.current = tabs;
    activeIdRef.current = activeId;
    activeSpaceIdRef.current = activeSpaceId;
  }, [tabs, activeId, activeSpaceId]);
  const sourceControlSpaceId =
    activeTaskId ?? activeSpaceId ?? DEFAULT_SPACE_ID;

  useSpacesBoot({
    ready: launchCwdResolved,
    launchCwd,
    home,
    allocId,
    replaceTabs,
    markBooted,
    setActiveSpaceForNewTabs,
    adoptWorkspaceEnv,
  });

  const taskTabs = useMemo(
    () =>
      tabs.filter(
        (t) => t.spaceId === toolSpaceId && t.taskId === activeTaskId,
      ),
    [tabs, toolSpaceId, activeTaskId],
  );

  const closeUtility = useCallback(
    (tool: UtilityTool) => {
      setUtilityTabs((current) => current.filter((tab) => tab !== tool));
      setToolView((current) =>
        toolViewAfterClose(utilityTabs, current, tool, taskTabs.length > 0),
      );
      if (tool === "explorer") setFilesLoaded(false);
    },
    [utilityTabs, taskTabs.length, setUtilityTabs, setToolView],
  );

  const {
    sidebarRef,
    sidebarWidthRef,
    reportSidebarWidth,
    initialSidebarCollapsed,
    persistSidebarCollapsed,
    toggleSidebar,
    persistSidebarWidth,
  } = useSidebarPanel(explorerRef);

  // 保留旧快捷键入口，但文件和 Git 始终路由到右侧，左侧只显示项目与任务。
  const openSidebarView = useCallback(
    (view: "tasks" | "explorer" | "source-control") => {
      if (view === "tasks") {
        sidebarRef.current?.expand();
        return;
      }
      setUtilityTabs((current) => openUtilityTab(current, view));
      setToolView(view);
      setToolsOpen(true);
    },
    [sidebarRef, setToolsOpen, setToolView, setUtilityTabs],
  );
  const cycleSidebarView = useCallback(
    (view: "tasks" | "explorer" | "source-control") => {
      if (view !== "tasks" && toolsOpen && toolView === view)
        setToolsOpen(false);
      else openSidebarView(view);
    },
    [toolsOpen, toolView, openSidebarView, setToolsOpen],
  );
  const toggleExplorerFocus = useCallback(() => {
    openSidebarView("explorer");
    requestAnimationFrame(() => explorerRef.current?.focus());
  }, [openSidebarView]);

  const [newEditorOpen, setNewEditorOpen] = useState(false);
  const [commandPaletteOpen, setCommandPaletteOpen] = useState(false);
  const [paletteInitialMode, setPaletteInitialMode] = useState<
    "commands" | "content"
  >("commands");
  const openCommandPalette = useCallback(
    (mode: "commands" | "content" = "commands") => {
      setPaletteInitialMode(mode);
      setCommandPaletteOpen(true);
    },
    [],
  );
  const miniOpen = useChatStore((s) => s.mini.open);
  const miniPresence = usePresence(miniOpen, 200);
  const focusInput = useChatStore((s) => s.focusInput);
  const openPanel = useChatStore((s) => s.openPanel);
  const setLive = useChatStore((s) => s.setLive);
  const respondToApproval = useChatStore((s) => s.respondToApproval);

  const { hasComposer, keysLoaded } = useAiBootstrap();

  const activeTab = taskTabs.find((t) => t.id === activeId);
  const activeProject = projects.find((p) => p.id === activeSpaceId);
  // 项目目录是 Agent 的上下文；终端 cd 只改变工具目录。
  const projectRoot = projectless
    ? defaultChatRoot
    : activeTask?.projectId === activeSpaceId
      ? (activeTask.workspaceRoot ?? activeProject?.root ?? launchCwd ?? home)
      : (activeProject?.root ?? launchCwd ?? home);
  const rightSidebarReady = useTaskSidebarWorkspace({
    task: activeTask,
    isDraft: isNewConversation,
    projectId: toolSpaceId,
    sessions: workspaceSessions,
    ready:
      booted &&
      sidebarsHydrated &&
      spacesHydrated &&
      sessionsHydrated &&
      !sessionLoading &&
      (!projectless || !!defaultChatRoot),
    tabs,
    activeId,
    allocId,
    replaceTabs,
    setActiveId,
    setActiveSpaceForNewTabs,
    setActiveTaskForNewTabs,
  });
  const isTerminalTab = activeTab?.kind === "terminal";
  const isBlockTab = activeTerminalTab?.blocks === true;
  const isEditorTab = activeTab?.kind === "editor";

  useEditorFileSync({ tabs, tabsRef, editorRefs });
  useThemeFileEditing({ tabsRef, openFileTab });

  const inheritedCwdForNewTab = useCallback(
    () => projectRoot ?? undefined,
    [projectRoot],
  );
  const explorerRoot = projectRoot;

  useEffect(() => {
    if (!spacesHydrated || !sessionsHydrated || !projects.length) return;
    for (const session of [
      ...taskSessions,
      ...(draftSession ? [draftSession] : []),
    ]) {
      if (projects.some((p) => p.id === session.projectId)) continue;
      const projectId = resolveSessionProject(session, projects, activeSpaceId);
      if (projectId)
        useChatStore.getState().assignSessionProject(session.id, projectId);
    }
  }, [
    spacesHydrated,
    sessionsHydrated,
    projects,
    taskSessions,
    activeSpaceId,
    draftSession,
  ]);

  useLayoutEffect(() => {
    if (!toolsOpen) {
      toolsPanelRef.current?.collapse();
      return;
    }
    if (!rightSidebarReady) return;
    const width = activeTaskId
      ? (useTaskSidebars.getState().byTask[activeTaskId]?.width ?? 32)
      : 32;
    toolsPanelRef.current?.resize(`${width}%`);
  }, [toolsOpen, activeTaskId, rightSidebarReady]);

  useEffect(() => {
    if (sidebarError)
      toast.error(tr("Could not save or restore the conversation's sidebar."), {
        description: sidebarError,
      });
  }, [sidebarError, tr]);

  useEffect(() => {
    if (!toolsOpen) return;
    if (toolView === "workspace") setToolsLoaded(true);
    if (toolView === "explorer") setFilesLoaded(true);
  }, [toolsOpen, toolView]);

  const previousTool = useRef({ id: activeId, taskId: activeTaskId });
  useEffect(() => {
    if (
      activeTab &&
      previousTool.current.id !== activeId &&
      previousTool.current.taskId === activeTaskId &&
      // 关闭后台标签产生的回退不抢走当前文件树或 Git 面板。
      tabs.some((tab) => tab.id === previousTool.current.id) &&
      toolsOpen
    )
      setToolView("workspace");
    previousTool.current = { id: activeId, taskId: activeTaskId };
  }, [activeId, activeTaskId, activeTab, tabs, toolsOpen, setToolView]);

  const previousToolCount = useRef({ taskId: activeTaskId, count: 0 });
  useEffect(() => {
    if (!rightSidebarReady) return;
    const count = utilityTabs.length + taskTabs.length;
    // 只响应当前对话从有标签变为无标签，切换对话或手动展开空面板不会被误收起。
    if (
      previousToolCount.current.taskId === activeTaskId &&
      previousToolCount.current.count > 0 &&
      count === 0
    ) {
      setToolsOpen(false);
    }
    previousToolCount.current = { taskId: activeTaskId, count };
    if (
      toolView === "workspace" &&
      !taskTabs.some((tab) => tab.id === activeId)
    ) {
      setToolView(utilityTabs[0] ?? "empty");
    }
  }, [
    toolView,
    taskTabs,
    activeId,
    utilityTabs,
    rightSidebarReady,
    activeTaskId,
    setToolsOpen,
    setToolView,
  ]);

  const projectNavigationRequest = useRef(0);
  const selectProject = useCallback(
    async (projectId: string | null, request: number) => {
      if (isSessionNavigationLocked()) return false;
      if (projectId === null) {
        useChatStore.setState({ sessionLoading: true });
        const home = await adoptWorkspaceEnv({ kind: "local" });
        if (request !== projectNavigationRequest.current) return false;
        if (!home)
          throw new Error(tr("Could not open the project's environment."));
        return true;
      }
      const project = useSpaces
        .getState()
        .spaces.find((p) => p.id === projectId);
      if (!project || project.removed) return false;
      useChatStore.setState({ sessionLoading: true });
      const home = await adoptWorkspaceEnv(project.env);
      if (request !== projectNavigationRequest.current) return false;
      if (!home)
        throw new Error(tr("Could not open the project's environment."));
      if (project.root) await native.workspaceAuthorize(project.root);
      if (request !== projectNavigationRequest.current) return false;
      useSpaces.getState().setActive(project.id);
      return true;
    },
    [adoptWorkspaceEnv, tr],
  );

  const selectTask = useCallback(
    async (projectId: string | null, sessionId: string) => {
      if (isSessionNavigationLocked()) return;
      const previousProjectId = useSpaces.getState().activeId;
      const previousTask =
        useChatStore
          .getState()
          .sessions.find(
            (s) => s.id === useChatStore.getState().activeSessionId,
          ) ?? useChatStore.getState().draftSession;
      const request = ++projectNavigationRequest.current;
      try {
        if (!(await selectProject(projectId, request))) return;
      } catch (error) {
        if (request === projectNavigationRequest.current)
          useChatStore.setState({ sessionLoading: false });
        toast.error(String(error));
        return;
      }
      const switched = await useChatStore.getState().switchSession(sessionId);
      if (request !== projectNavigationRequest.current) return;
      if (switched) useChatStore.getState().openPanel();
      else {
        // 历史读取失败时回到原项目，避免旧对话加载新项目的文件和终端。
        try {
          if (previousTask?.projectless || previousProjectId)
            await selectProject(
              previousTask?.projectless ? null : previousProjectId,
              request,
            );
        } finally {
          if (request === projectNavigationRequest.current)
            useChatStore.setState({ sessionLoading: false });
        }
      }
    },
    [selectProject],
  );

  const createTask = useCallback(
    async (projectId: string | null) => {
      if (isSessionNavigationLocked()) return;
      const request = ++projectNavigationRequest.current;
      try {
        if (!(await selectProject(projectId, request))) return;
      } catch (error) {
        if (request === projectNavigationRequest.current)
          useChatStore.setState({ sessionLoading: false });
        toast.error(String(error));
        return;
      }
      const project = useSpaces
        .getState()
        .spaces.find((p) => p.id === projectId);
      if (projectId !== null && !project) return;
      const oldDraft = useChatStore.getState().draftSession;
      const preserveDraftWorkspace =
        !!oldDraft &&
        (tabsRef.current.some((tab) => tab.taskId === oldDraft.id) ||
          (!!useTaskSidebars.getState().byTask[oldDraft.id]?.open &&
            !!useTaskSidebars.getState().byTask[oldDraft.id]?.utilityTabs
              .length));
      const id = useChatStore.getState().newSession(
        project
          ? {
              id: project.id,
              root: project.root,
              scope: workspaceScopeKey(project.env),
            }
          : null,
        preserveDraftWorkspace,
      );
      if (!useTaskSidebars.getState().byTask[id]) {
        useTaskSidebars.getState().update(id, {
          open: false,
          utilityTabs: ["explorer"],
          view: "explorer",
          tabs: [],
          activeTabIndex: -1,
        });
      }
      useChatStore.getState().openPanel();
      useChatStore.getState().focusInput();
    },
    [selectProject],
  );

  const createProject = useCallback(
    async (name: string, root: string) => {
      if (isSessionNavigationLocked()) return;
      if (!/^([A-Za-z]:[\\/]|\/|\\\\)/.test(root))
        throw new Error(tr("Use an absolute project directory."));
      // 该表单选择的是本机目录，不能套用前一个 WSL 项目的环境。
      const directory = await native.projectDirectory([root]);
      const env = LOCAL_WORKSPACE;
      const canonical = await native.workspaceAuthorize(directory, env);
      const project = useSpaces
        .getState()
        .create({ name, root: canonical, env });
      await createTask(project.id);
    },
    [createTask, tr],
  );

  const removeProject = useCallback(
    async (projectId: string) => {
      if (isSessionNavigationLocked()) return;
      const store = useChatStore.getState();
      const current =
        store.sessions.find(
          (session) => session.id === store.activeSessionId,
        ) ?? store.draftSession;
      // 当前项目先退出到独立草稿，避免移除后仍从旧目录继续执行 Agent。
      if (
        current?.projectId === projectId ||
        useSpaces.getState().activeId === projectId
      ) {
        await createTask(null);
        const next = useChatStore.getState();
        const task =
          next.sessions.find(
            (session) => session.id === next.activeSessionId,
          ) ?? next.draftSession;
        if (!task?.projectless || next.sessionLoading) return;
      }
      try {
        await useSpaces.getState().removeProject(projectId);
      } catch (error) {
        toast.error(
          tr("Could not remove project: {error}", { error: String(error) }),
        );
      }
    },
    [createTask, tr],
  );

  const initialTaskRestored = useRef(false);
  const selectProjectTask = useCallback(
    async (projectId: string) => {
      const task = useChatStore
        .getState()
        .sessions.find((s) => s.projectId === projectId && !s.archived);
      if (task) await selectTask(projectId, task.id);
      else await createTask(projectId);
    },
    [selectTask, createTask],
  );
  useEffect(() => {
    if (initialTaskRestored.current || !spacesHydrated || !sessionsHydrated)
      return;
    if (!activeTask?.projectId && !activeTask?.projectless) {
      if (!projects.some((project) => !project.removed)) {
        initialTaskRestored.current = true;
        void createTask(null);
      }
      return;
    }
    initialTaskRestored.current = true;
    if (
      projects.some(
        (project) => project.id === activeTask.projectId && project.removed,
      )
    ) {
      const remaining = taskSessions.find(
        (task) =>
          !task.archived &&
          (task.projectless ||
            projects.some(
              (project) => project.id === task.projectId && !project.removed,
            )),
      );
      if (remaining)
        void selectTask(
          remaining.projectless ? null : (remaining.projectId ?? null),
          remaining.id,
        );
      else
        void createTask(
          projects.find((project) => !project.removed)?.id ?? null,
        );
      return;
    }
    if (isNewConversation) void createTask(activeTask.projectId ?? null);
    else void selectTask(activeTask.projectId ?? null, activeTask.id);
  }, [
    spacesHydrated,
    sessionsHydrated,
    activeTask,
    selectTask,
    createTask,
    isNewConversation,
    projects,
    taskSessions,
  ]);

  const openTaskDiff = useCallback(
    (input: Parameters<typeof openAiDiffTab>[0]) => {
      setToolsOpen(true);
      setToolView("workspace");
      return openAiDiffTab(input);
    },
    [openAiDiffTab, setToolsOpen, setToolView],
  );

  const openToolGitDiff = useCallback(
    (input: Parameters<typeof openGitDiffTab>[0]) => {
      setToolsOpen(true);
      setToolView("workspace");
      return openGitDiffTab(input);
    },
    [openGitDiffTab, setToolView, setToolsOpen],
  );

  useWindowTitle(activeTab, explorerRoot);

  useEffect(() => {
    setActiveEditorHandle(editorRefs.current.get(activeId) ?? null);
  }, [activeId]);

  const disposeTab = useCallback(
    (id: number) => {
      // 终端句柄随下方窗格树变化清理，此处只释放按标签保存的句柄。
      editorRefs.current.delete(id);
      previewRefs.current.delete(id);
      closeTab(id, true);
    },
    [closeTab],
  );

  const disposeTabs = useCallback(
    (anchorId: number, plan: CloseTabsPlan) => {
      const closedIds = closeTabs(anchorId, plan);
      for (const id of closedIds) {
        editorRefs.current.delete(id);
        previewRefs.current.delete(id);
      }
    },
    [closeTabs],
  );

  const disposeDeletedTabs = useCallback(
    (ids: number[]) => {
      if (ids.length === 0) return;
      for (const id of ids) disposeTab(id);
    },
    [disposeTab],
  );

  const {
    pendingCloseTab,
    pendingTerminalCloseTab,
    pendingDeleteTabs,
    pendingCloseMany,
    closeManyConfirming,
    handleClose,
    handleCloseTabsToRight,
    handleCloseOtherTabs,
    confirmClose,
    cancelClose,
    confirmTerminalClose,
    cancelTerminalClose,
    confirmDeleteClose,
    cancelDeleteClose,
    confirmCloseMany,
    cancelCloseMany,
    handlePathsDeleted,
  } = useTabCloseGuards({
    tabs,
    activeId,
    allowCloseLast: true,
    disposeTab,
    disposeDeletedTabs,
    disposeTabs,
  });

  const { pendingAppClose, confirmAppClose, cancelAppClose } =
    useAppCloseGuard(tabsRef);

  useEffect(() => {
    const live = new Set<number>();
    for (const t of tabs) {
      if (t.kind === "terminal") {
        for (const id of leafIds(t.paneTree)) live.add(id);
      }
    }
    for (const id of liveLeavesRef.current) {
      if (!live.has(id)) disposeSession(id);
    }
    liveLeavesRef.current = live;
    for (const k of [...terminalRefs.current.keys()])
      if (!live.has(k)) terminalRefs.current.delete(k);
  }, [tabs]);

  useEffect(() => {
    const tab = tabsRef.current.find((t) => t.id === activeId);
    if (tab?.kind !== "terminal") return;
    const ptyIds = leafIds(tab.paneTree).flatMap((leafId) => {
      const ptyId = ptyIdForLeaf(leafId);
      return ptyId === null ? [] : [ptyId];
    });
    useAgentActivityStore.getState().acknowledgeAttention(ptyIds);
  }, [activeId]);

  // Most-recently-used tab ids, most recent first, pruned to live tabs. Drives
  // the Ctrl+Tab quick switcher so it cycles by recency, not strip order.
  const mruRef = useRef<number[]>([activeId]);
  useEffect(() => {
    mruRef.current = [
      activeId,
      ...mruRef.current.filter((id) => id !== activeId),
    ];
  }, [activeId]);
  useEffect(() => {
    const live = new Set(tabs.map((t) => t.id));
    mruRef.current = mruRef.current.filter((id) => live.has(id));
  }, [tabs]);

  const getSwitcherOrder = useCallback(() => {
    const space = toolSpaceId;
    const inSpace = tabsRef.current
      .filter((t) => t.spaceId === space && t.taskId === activeTaskId)
      .map((t) => t.id);
    const present = new Set(inSpace);
    const ordered = mruRef.current.filter((id) => present.has(id));
    for (const id of inSpace) if (!ordered.includes(id)) ordered.push(id);
    return [activeId, ...ordered.filter((id) => id !== activeId)];
  }, [activeId, toolSpaceId, activeTaskId]);

  const { state: switcherState, step: stepSwitcher } = useTabSwitcher({
    getOrder: getSwitcherOrder,
    onCommit: (id) => {
      if (tabsRef.current.some((t) => t.id === id)) setActiveId(id);
    },
  });

  const cycleSpace = useCallback(
    (delta: 1 | -1) => {
      if (isSessionNavigationLocked()) return;
      const { spaces: allSpaces, activeId: sid } = useSpaces.getState();
      const spaces = allSpaces.filter((space) => !space.removed);
      if (spaces.length < 2) return;
      const idx = spaces.findIndex((s) => s.id === sid);
      const next = (idx + delta + spaces.length) % spaces.length;
      void selectProjectTask(spaces[next].id);
    },
    [selectProjectTask],
  );

  const captureActiveSelection = useCallback((): string | null => {
    const t = tabs.find((x) => x.id === activeId);
    if (!t) return null;
    if (t.kind === "terminal") {
      const lid = t.activeLeafId;
      return terminalRefs.current.get(lid)?.getSelection() ?? null;
    }
    if (t.kind === "editor") {
      return editorRefs.current.get(activeId)?.getSelection() ?? null;
    }
    return null;
  }, [tabs, activeId]);

  const togglePanelAndFocus = useCallback(() => {
    openPanel();
    focusInput(null);
  }, [openPanel, focusInput]);

  const attachSelection = useChatStore((s) => s.attachSelection);

  const handleAttachFileToAgent = useCallback(
    (path: string) => {
      // Dispatch a window event the composer listens for. Same pattern as
      // selections — keeps file-explorer decoupled from the AI module.
      window.dispatchEvent(
        new CustomEvent<string>("rcode:ai-attach-file", { detail: path }),
      );
      openPanel();
      focusInput(null);
    },
    [openPanel, focusInput],
  );

  const askFromSelection = useCallback(() => {
    const selection = captureActiveSelection();
    if (!selection || !selection.trim()) {
      focusInput(null);
      return;
    }
    const source: "terminal" | "editor" =
      activeTab?.kind === "editor" ? "editor" : "terminal";
    attachSelection(selection, source);
  }, [captureActiveSelection, focusInput, attachSelection, activeTab]);

  const { askPopup, setAskPopup, onAskFromSelection } = useSelectionAskAi({
    captureActiveSelection,
    askFromSelection,
  });
  const askPresence = usePresence(Boolean(askPopup), 120);

  const openNewTab = useCallback(() => {
    setToolsOpen(true);
    setToolView("workspace");
    newTab(inheritedCwdForNewTab());
  }, [newTab, inheritedCwdForNewTab, setToolsOpen, setToolView]);

  const openTerminalTools = useCallback(() => {
    setToolsOpen(true);
    setToolView("workspace");
    const terminal = tabsRef.current.find(
      (tab) =>
        tab.spaceId === toolSpaceId &&
        tab.taskId === activeTaskId &&
        tab.kind === "terminal",
    );
    if (terminal) setActiveId(terminal.id);
    else newTab(projectRoot ?? undefined);
  }, [
    toolSpaceId,
    activeTaskId,
    projectRoot,
    setActiveId,
    newTab,
    setToolsOpen,
    setToolView,
  ]);

  const openNewPrivateTab = useCallback(() => {
    setToolsOpen(true);
    setToolView("workspace");
    newPrivateTab(inheritedCwdForNewTab());
  }, [newPrivateTab, inheritedCwdForNewTab, setToolView, setToolsOpen]);

  const openNewBlockTab = useCallback(() => {
    setToolsOpen(true);
    setToolView("workspace");
    newBlockTab(inheritedCwdForNewTab());
  }, [newBlockTab, inheritedCwdForNewTab, setToolsOpen, setToolView]);

  const launchAgentGroup = useCallback(
    (request: AgentLaunchRequest) => {
      const command = validateAgentLaunchCommand(request.command);
      if (!command.ok) return;
      const launcher = findAgentLauncher(request.agent);
      const title =
        request.instances === 1
          ? launcher.label
          : `${launcher.label} × ${request.instances}`;
      const { leafIds: agentLeafIds } = newAgentGroupTab(
        inheritedCwdForNewTab(),
        title,
        request.instances,
      );
      const hooksReady = launcher.supportsHooks
        ? invoke("agent_enable_hooks", {
            agent: request.agent,
          }).catch((error) => {
            console.warn(
              `[rcode] could not enable ${request.agent} notifications:`,
              error,
            );
          })
        : Promise.resolve();

      for (const leafId of agentLeafIds) {
        void (async () => {
          await Promise.all([whenSessionReady(leafId), hooksReady]);
          if (!writeToSession(leafId, `${command.command}\r`)) {
            console.error(
              `[rcode] agent terminal ${leafId} closed before launch`,
            );
          }
        })();
      }
    },
    [inheritedCwdForNewTab, newAgentGroupTab],
  );

  const cdInNewTab = useCallback(
    (path: string) => {
      const tabId = newTab(path);
      setTimeout(() => {
        const tab = tabsRef.current.find((x) => x.id === tabId);
        if (!tab || tab.kind !== "terminal") return;
        const t = terminalRefs.current.get(tab.activeLeafId);
        if (!t) return;
        t.write(`cd ${quoteShellArg(path)}\r`);
        t.focus();
      }, 80);
    },
    [newTab],
  );

  const handleOpenFile = useCallback(
    (path: string, pin?: boolean) => {
      setToolsOpen(true);
      setToolView("workspace");
      // Markdown opens in its rendered view by default; a per-tab toggle flips
      // it to the raw editor. Other files default to preview (pin=false);
      // explicit actions like context-menu "Open" pass pin=true to persist.
      if (isMarkdownPath(path)) newMarkdownTab(path);
      else openFileTab(path, pin ?? false);
    },
    [openFileTab, newMarkdownTab, setToolsOpen, setToolView],
  );

  const openLaunchFiles = useCallback(
    (paths: string[]) => {
      for (const path of paths) handleOpenFile(path, true);
    },
    [handleOpenFile],
  );

  // Warm start: the backend emits once the window already exists. Attach on
  // mount so an "Open With" that lands mid-restore isn't dropped — the backend
  // also seeds the drain-once state, so the boot drain below is the safety net.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    (async () => {
      const off = await listen<string[]>("rcode:open-file", (e) => {
        openLaunchFiles(e.payload);
      });
      if (disposed) off();
      else unlisten = off;
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [openLaunchFiles]);

  // Cold start: files arrive as CLI args (Linux/Windows) or the macOS open-files
  // event, and get_launch_files drains them once. Wait for `booted` — the spaces
  // restore ends in replaceTabs(), which overwrites the whole tab list and would
  // discard a launch tab opened before it, making the file flash open and vanish.
  // Booting first also lands the tab in the restored active space, and lets
  // openFileTab dedupe against a session that already had the file open.
  useEffect(() => {
    if (!booted) return;
    void (async () => {
      openLaunchFiles(await consumeLaunchFiles());
    })();
  }, [booted, openLaunchFiles]);

  const handleExplorerPathRenamed = useCallback(
    (from: string, to: string) => {
      for (const tab of tabsRef.current) {
        if (tab.kind !== "editor" && tab.kind !== "markdown") continue;
        const path = renamedPath(tab.path, from, to);
        if (path === null) continue;
        const i = path.lastIndexOf("/");
        updateTab(tab.id, {
          path,
          title: i === -1 ? path : path.slice(i + 1),
        });
      }
    },
    [updateTab],
  );

  const canReplaceExplorerPath = useCallback(
    (path: string) => !hasOpenPathTab(tabsRef.current, path),
    [],
  );

  const activeTerminalLeafCwd =
    activeTab?.kind === "terminal"
      ? (findLeafCwd(activeTab.paneTree, activeTab.activeLeafId) ??
        activeTab.cwd ??
        null)
      : null;

  const explorerActiveFilePath =
    activeTab?.kind === "editor" || activeTab?.kind === "markdown"
      ? activeTab.path
      : null;
  const isRepositoryContextCurrent = useCallback(
    (spaceId: string, workspaceKey: string) => {
      const currentSpaceId =
        useChatStore.getState().activeSessionId ??
        useSpaces.getState().activeId ??
        DEFAULT_SPACE_ID;
      const currentWorkspaceKey = workspaceScopeKey(
        useWorkspaceEnvStore.getState().env,
      );
      return spaceId === currentSpaceId && workspaceKey === currentWorkspaceKey;
    },
    [],
  );
  const openSourceControl = useCallback(() => {
    openSidebarView("source-control");
  }, [openSidebarView]);
  const toggleHiddenFiles = useCallback(() => {
    openSidebarView("explorer");
    void setShowHidden(!usePreferencesStore.getState().showHidden);
  }, [openSidebarView]);
  const {
    repositoryTarget: sourceControlRepositoryTarget,
    openInSourceControl: handleOpenRepositoryInSourceControl,
    openGitHistory: handleOpenGitHistoryForPath,
    followActiveContext: handleFollowRepositoryContext,
  } = useRepositoryTargeting({
    spaceId: sourceControlSpaceId,
    workspaceKey: workspaceScopeKey(workspaceEnv),
    isContextCurrent: isRepositoryContextCurrent,
    openSourceControl,
    openCommitHistoryTab,
  });
  const { sourceControl, toggleSourceControl, openGitGraphFromContext } =
    useSourceControlContext({
      activeTab: toolView === "source-control" ? undefined : activeTab,
      tabs: taskTabs,
      activeTerminalLeafCwd:
        toolView === "source-control" ? null : activeTerminalLeafCwd,
      explorerRoot,
      launchCwd: projectless ? null : launchCwd,
      launchCwdResolved,
      home: projectless ? null : home,
      sidebarView:
        toolView === "workspace" || toolView === "empty" ? "tasks" : toolView,
      cycleSidebarView,
      repositoryTarget: sourceControlRepositoryTarget,
      openCommitHistoryTab,
      contextKey: activeTaskId ?? "",
    });
  const explorerGitDecorations = usePreferencesStore(
    (s) => s.explorerGitDecorations,
  );

  const openPreviewTab = useCallback(
    (url: string) => {
      setToolsOpen(true);
      setToolView("workspace");
      const id = newPreviewTab(url);
      // Focus the address bar if the URL is empty so the user can type.
      if (!url) {
        setTimeout(() => previewRefs.current.get(id)?.focusAddressBar(), 0);
      }
      return id;
    },
    [newPreviewTab, setToolsOpen, setToolView],
  );

  const splitActivePaneInActiveTab = useCallback(
    (dir: "row" | "col") => {
      const t = tabsRef.current.find((x) => x.id === activeId);
      if (!t || t.kind !== "terminal") return;
      splitActivePane(activeId, dir);
    },
    [activeId, splitActivePane],
  );

  const handleCloseTabOrPane = useCallback(() => {
    if (
      toolsOpen &&
      (toolView === "explorer" || toolView === "source-control")
    ) {
      closeUtility(toolView);
      return;
    }
    const t = tabsRef.current.find((x) => x.id === activeId);
    if (t?.kind === "terminal" && leafIds(t.paneTree).length > 1) {
      closeActivePane(activeId);
      return;
    }
    void handleClose(activeId);
  }, [
    activeId,
    closeActivePane,
    handleClose,
    toolsOpen,
    toolView,
    closeUtility,
  ]);

  const [zenMode, setZenMode] = useState(false);

  // Focus an agent's tab, switching to its space first so the header and tab
  // strip don't end up showing a different space than the focused pane.
  const activateAgentTarget = useCallback(
    async (tabId: number, leafId: number) => {
      const target = tabsRef.current.find((t) => t.id === tabId);
      if (
        target?.taskId &&
        target.taskId !== useChatStore.getState().activeSessionId
      ) {
        if (isSessionNavigationLocked()) return;
        await selectTask(
          target.spaceId === DEFAULT_SPACE_ID ? null : target.spaceId,
          target.taskId,
        );
        if (useChatStore.getState().activeSessionId !== target.taskId) return;
      }
      setToolsOpen(true);
      setToolView("workspace");
      setActiveId(tabId);
      focusPane(tabId, leafId);
    },
    [setActiveId, focusPane, selectTask, setToolsOpen, setToolView],
  );

  const shortcutHandlers = useMemo<ShortcutHandlers>(
    () => ({
      "commandPalette.open": () => openCommandPalette("commands"),
      "commandPalette.content": () => openCommandPalette("content"),
      "tab.new": openNewTab,
      "tab.newPreview": () => openPreviewTab(""),
      "tab.close": handleCloseTabOrPane,
      "tab.next": () => stepSwitcher(1),
      "tab.prev": () => stepSwitcher(-1),
      "tab.selectByIndex": (e) =>
        selectByIndex(parseInt(e.key, 10) - 1, toolSpaceId),
      "space.next": () => cycleSpace(1),
      "space.prev": () => cycleSpace(-1),
      "space.overview": () => openSidebarView("tasks"),
      "terminal.clear": () => {
        clearFocusedTerminal();
      },
      "blocks.prev": () => navigateFocusedBlocks(-1),
      "blocks.next": () => navigateFocusedBlocks(1),
      "search.focus": () => editorRefs.current.get(activeId)?.openSearch(),
      "settings.open": () => void openSettingsWindow(),
      "sidebar.toggle": toggleSidebar,
      "explorer.focus": toggleExplorerFocus,
      "explorer.toggleHidden": toggleHiddenFiles,
      "view.zoomIn": zoomIn,
      "view.zoomOut": zoomOut,
      "view.zoomReset": zoomReset,
      "view.zenMode": () => setZenMode((v) => !v),
      "editor.undo": () => editorRefs.current.get(activeId)?.undo(),
      "editor.redo": () => editorRefs.current.get(activeId)?.redo(),
      "editor.save": () => {
        void editorRefs.current.get(activeId)?.save();
      },
    }),
    [
      activeId,
      openCommandPalette,
      stepSwitcher,
      cycleSpace,
      handleCloseTabOrPane,
      openNewTab,
      openPreviewTab,
      toolSpaceId,
      selectByIndex,
      toggleSidebar,
      toggleHiddenFiles,
      zoomIn,
      zoomOut,
      zoomReset,
      openSidebarView,
      toggleExplorerFocus,
    ],
  );

  const shortcutsDisabled = useCallback(
    (id: ShortcutId, e: KeyboardEvent) => {
      if (useSettingsOverlay.getState().open && !isSettingsShortcutAllowed(id))
        return true;
      if (
        id === "editor.undo" ||
        id === "editor.redo" ||
        id === "editor.save" ||
        id === "search.focus"
      ) {
        return activeTab?.kind !== "editor";
      }
      if (id === "terminal.clear") {
        // Only intercept ⌘K while a terminal is focused; elsewhere let the key
        // fall through (we never preventDefault when disabled).
        const target =
          (e.target as HTMLElement | null) ?? document.activeElement;
        return !isTerminalSurfaceTarget(target);
      }
      if (id === "blocks.prev" || id === "blocks.next") {
        return !(activeTab?.kind === "terminal" && activeTab.blocks === true);
      }
      if (id === "sidebar.toggle") {
        // Ctrl+B is also Claude Code's "run in background" key. While a terminal
        // is focused, let Ctrl+B reach the shell/Claude instead of toggling the
        // sidebar. Ctrl+Shift+B (second binding) still toggles it from anywhere.
        const target =
          (e.target as HTMLElement | null) ?? document.activeElement;
        const inTerminal = isTerminalSurfaceTarget(target);
        // Only defer the plain (no-shift) Ctrl/⌘+B binding; the Shift variant
        // is the always-on toggle and is never claimed by the terminal.
        return inTerminal && !e.shiftKey;
      }
      return false;
    },
    [activeTab],
  );

  useGlobalShortcuts(shortcutHandlers, { isDisabled: shortcutsDisabled });

  const registerTerminalHandle = useCallback(
    (leafId: number, h: TerminalPaneHandle | null) => {
      if (h) terminalRefs.current.set(leafId, h);
      else terminalRefs.current.delete(leafId);
    },
    [],
  );

  const registerEditorHandle = useCallback(
    (id: number, h: EditorPaneHandle | null) => {
      if (h) {
        editorRefs.current.set(id, h);
        const pending = pendingEditorNavigation.current.get(id);
        if (pending != null) {
          pendingEditorNavigation.current.delete(id);
          if (pending.line === undefined) h.focus();
          else h.gotoLine(pending.line, { focus: pending.focus });
        }
      } else {
        editorRefs.current.delete(id);
      }
      if (id === activeId) setActiveEditorHandle(h);
    },
    [activeId],
  );

  const registerPreviewHandle = useCallback(
    (id: number, h: PreviewPaneHandle | null) => {
      if (h) previewRefs.current.set(id, h);
      else previewRefs.current.delete(id);
    },
    [],
  );

  const handlePreviewUrl = useCallback(
    (id: number, url: string) => updateTab(id, { url }),
    [updateTab],
  );

  const authorizedCwds = useRef(new Set<string>());
  const handleTerminalCwd = useCallback(
    (leafId: number, cwd: string) => {
      setLeafCwd(leafId, cwd);
      if (cwd && !authorizedCwds.current.has(cwd)) {
        authorizedCwds.current.add(cwd);
        native.workspaceAuthorize(cwd).catch(() => {
          authorizedCwds.current.delete(cwd);
        });
      }
    },
    [setLeafCwd],
  );

  const handleFocusLeaf = useCallback(
    (tabId: number, leafId: number) => focusPane(tabId, leafId),
    [focusPane],
  );

  const onActivateAgent = activateAgentTarget;

  const handleLeafExit = useCallback(
    (leafId: number, _code: number) => {
      const all = tabsRef.current;
      const tab = all.find(
        (t) => t.kind === "terminal" && hasLeaf(t.paneTree, leafId),
      );
      if (!tab || tab.kind !== "terminal") return;
      // shell 退出释放对应标签，最后一个终端也不影响中央 Agent 会话。
      if (leafIds(tab.paneTree).length === 1) disposeTab(tab.id);
      else closePaneByLeaf(leafId);
    },
    [closePaneByLeaf, disposeTab],
  );

  const handleEditorDirty = useCallback(
    (id: number, dirty: boolean) => updateTab(id, { dirty }),
    [updateTab],
  );

  const handleRenameTab = useCallback(
    (id: number, title: string) => updateTab(id, { customTitle: title.trim() }),
    [updateTab],
  );

  const activeCwd = projectRoot ?? activeTerminalLeafCwd;

  const handleNewSpace = useCallback(() => {
    if (!isSessionNavigationLocked()) setNewProjectOpen(true);
  }, []);

  const commandPaletteItems = useMemo(
    () =>
      commandPaletteOpen
        ? createCommandItems(
            {
              tabs,
              activeId,
              canSearch: isEditorTab && activeEditorHandle !== null,
              explorerRoot,
              home,
              openNewTab,
              openNewBlock: openNewBlockTab,
              openNewPrivate: openNewPrivateTab,
              openNewEditor: () => setNewEditorOpen(true),
              openNewPreview: () => openPreviewTab(""),
              openGitGraph: openGitGraphFromContext,
              toggleSourceControl,
              closeActiveTabOrPane: handleCloseTabOrPane,
              splitPaneRight: () => splitActivePaneInActiveTab("row"),
              splitPaneDown: () => splitActivePaneInActiveTab("col"),
              focusSearch: () => editorRefs.current.get(activeId)?.openSearch(),
              focusExplorerSearch: () => explorerRef.current?.focusSearch(),
              toggleSidebar,
              toggleHiddenFiles,
              toggleAi: togglePanelAndFocus,
              askAiSelection: askFromSelection,
              openSettings: () => void openSettingsWindow(),
              openKeyboardShortcuts: () => void openSettingsWindow("shortcuts"),
              spaces: useSpaces
                .getState()
                .spaces.filter((space) => !space.removed),
              activeSpaceId,
              openSpacesOverview: () => openSidebarView("tasks"),
              newSpace: () => void handleNewSpace(),
              switchSpace: (id) => void selectProjectTask(id),
            },
            tr,
          )
        : [],
    [
      tr,
      commandPaletteOpen,
      tabs,
      activeId,
      isEditorTab,
      activeEditorHandle,
      explorerRoot,
      home,
      openNewTab,
      openNewBlockTab,
      openNewPrivateTab,
      openPreviewTab,
      openGitGraphFromContext,
      toggleSourceControl,
      handleCloseTabOrPane,
      splitActivePaneInActiveTab,
      toggleSidebar,
      toggleHiddenFiles,
      togglePanelAndFocus,
      askFromSelection,
      activeSpaceId,
      handleNewSpace,
      openSidebarView,
      selectProjectTask,
    ],
  );

  const pendingEditorNavigation = useRef<
    Map<number, { line?: number; focus: boolean }>
  >(new Map());
  const openContentHit = useCallback(
    (path: string, line: number) => {
      setToolsOpen(true);
      setToolView("workspace");
      const id = openFileTab(path, true);
      if (id == null) return;
      const h = editorRefs.current.get(id);
      if (h) h.gotoLine(line);
      else pendingEditorNavigation.current.set(id, { line, focus: true });
    },
    [openFileTab, setToolsOpen, setToolView],
  );

  const openControlFile = useCallback(
    async ({
      path,
      line,
      focus,
      spaceId,
    }: {
      path: string;
      line?: number;
      focus: boolean;
      spaceId: string;
    }) => {
      if (focus && useSpaces.getState().activeId !== spaceId) {
        if (isSessionNavigationLocked()) return null;
        await selectProjectTask(spaceId);
        if (useSpaces.getState().activeId !== spaceId) return null;
      }
      const state = useChatStore.getState();
      const targetTask =
        state.sessions.find(
          (task) =>
            task.id === state.activeSessionId && task.projectId === spaceId,
        ) ??
        state.sessions.find(
          (task) => task.projectId === spaceId && !task.archived,
        );
      if (!targetTask) return null;
      if (focus)
        useTaskSidebars
          .getState()
          .update(targetTask.id, { open: true, view: "workspace" });
      const id = openFileTab(path, true, {
        spaceId,
        taskId: targetTask.id,
        activate: focus,
      });
      const editor = editorRefs.current.get(id);
      if (line !== undefined) {
        if (editor) editor.gotoLine(line, { focus });
        else pendingEditorNavigation.current.set(id, { line, focus });
      } else if (focus) {
        if (editor) editor.focus();
        else pendingEditorNavigation.current.set(id, { focus: true });
      }
      return id;
    },
    [openFileTab, selectProjectTask],
  );

  useControlBridge({
    ready: spacesHydrated && launchCwdResolved,
    tabsRef,
    activeTabIdRef: activeIdRef,
    activeSpaceIdRef,
    onOpen: openControlFile,
  });

  useEffect(() => {
    setLspNavigator({ openFile: openContentHit });
    return () => setLspNavigator(null);
  }, [openContentHit]);

  const insertHistoryCommand = useMemo(
    () =>
      isTerminalTab && activeLeafId !== null
        ? (cmd: string) => {
            writeToSession(activeLeafId, cmd);
            terminalRefs.current.get(activeLeafId)?.focus();
          }
        : null,
    [isTerminalTab, activeLeafId],
  );

  useAiLiveBridge({
    setLive,
    activeId,
    tabs: taskTabs,
    explorerRoot,
    launchCwd: projectless ? null : launchCwd,
    home: projectless ? null : home,
    openPreviewTab,
    newAgentTab,
    terminalRefs,
  });

  const shell = (
    <ThemeProvider>
      <TooltipProvider>
        <div className="relative flex h-full flex-col overflow-hidden bg-frame text-foreground">
          {!zenMode && (
            <Header
              onToggleSidebar={toggleSidebar}
              onSettingsClick={() => void openSettingsWindow()}
              toolsOpen={toolsOpen}
              onToggleTools={() => setToolsOpen((open) => !open)}
            />
          )}

          <main className="zoom-content flex min-h-0 flex-1 flex-col p-2">
            {/* 外框由面板组统一承载，收起任一侧栏时仍保持完整的圆角卡片。 */}
            <ResizablePanelGroup
              orientation="horizontal"
              className="rcode-pane min-h-0 flex-1"
              onLayoutChanged={(_, { isUserInteraction }) => {
                const width = sidebarRef.current?.getSize().inPixels ?? 0;
                persistSidebarWidth(width, isUserInteraction);
                if (isUserInteraction) {
                  const size = toolsPanelRef.current?.getSize();
                  if (size) {
                    if (rightSidebarReady) {
                      setToolsOpen(size.inPixels > 0);
                      if (size.inPixels > 0) setToolsWidth(size.asPercentage);
                    }
                  }
                }
              }}
            >
              <ResizablePanel
                id="sidebar"
                panelRef={sidebarRef}
                defaultSize={
                  initialSidebarCollapsed
                    ? "0px"
                    : `${sidebarWidthRef.current}px`
                }
                minSize={`${SIDEBAR_MIN_WIDTH}px`}
                maxSize={`${SIDEBAR_MAX_WIDTH}px`}
                collapsible
                collapsedSize={0}
                onResize={(size) => {
                  reportSidebarWidth(size.inPixels);
                  persistSidebarCollapsed(size.inPixels <= 0);
                }}
              >
                <div className="rcode-left-sidebar h-full min-h-0">
                  <div className="flex h-full min-h-0 flex-col">
                    <ProjectTasksSidebar
                      onNewConversation={() => {
                        const projectId = draftSession?.projectless
                          ? null
                          : (draftSession?.projectId ?? activeSpaceId);
                        void createTask(projectId);
                      }}
                      onNewIndependentConversation={() => void createTask(null)}
                      onNewProject={() => setNewProjectOpen(true)}
                      onRemoveProject={removeProject}
                      onNewTask={(projectId) => void createTask(projectId)}
                      onSelectTask={(projectId, sessionId) =>
                        void selectTask(projectId, sessionId)
                      }
                    />
                  </div>
                </div>
              </ResizablePanel>
              <ResizableHandle className="w-px bg-border/60 transition-colors duration-[var(--dur-fast)] after:w-4 hover:bg-border" />
              <ResizablePanel id="agent" minSize="30%">
                <div className="h-full min-h-0">
                  <div className="flex h-full min-h-0 flex-col">
                    <Suspense
                      fallback={
                        <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
                          {tr("Loading sessions…")}
                        </div>
                      }
                    >
                      <AgentWorkbench
                        onSelectProject={(projectId) =>
                          void createTask(projectId)
                        }
                        onNewProject={() => setNewProjectOpen(true)}
                        workspaceRoot={activeTask?.workspaceRoot ?? projectRoot}
                        home={home}
                        hasComposer={hasComposer}
                        keysLoaded={keysLoaded}
                        gitStatus={sourceControl.status}
                        toolsOpen={
                          toolsOpen && toolView === "workspace" && isTerminalTab
                        }
                        onOpenTerminal={openTerminalTools}
                        onOpenFiles={() => openSidebarView("explorer")}
                        onOpenFile={handleOpenFile}
                        onOpenGit={() => openSidebarView("source-control")}
                        onConnect={() => void openSettingsWindow("models")}
                      />
                    </Suspense>
                  </div>
                </div>
              </ResizablePanel>
              <ResizableHandle
                className={cn(
                  "w-px bg-border/60 transition-colors duration-[var(--dur-fast)] after:w-4 hover:bg-border",
                  !toolsOpen && "hidden",
                )}
              />
              <ResizablePanel
                id="workspace"
                panelRef={toolsPanelRef}
                defaultSize="0%"
                minSize="200px"
                maxSize="65%"
                collapsible
                collapsedSize={0}
              >
                <div className="h-full min-h-0">
                  <div
                    className={cn(
                      "flex h-full min-h-0 flex-col",
                      !toolsOpen && "invisible pointer-events-none",
                    )}
                    inert={!toolsOpen || !rightSidebarReady}
                  >
                    <DevelopmentToolsTabs
                      utilityTabs={utilityTabs}
                      onOpenUtility={openSidebarView}
                      onCloseUtility={closeUtility}
                      view={toolView}
                      onViewChange={setToolView}
                      tabProps={{
                        tabs: taskTabs.map((tab, index) =>
                          tab.kind === "terminal" && !tab.customTitle
                            ? {
                                ...tab,
                                customTitle: `${tr("Terminal")}${index ? ` ${index + 1}` : ""}`,
                              }
                            : tab,
                        ),
                        activeId,
                        onSelect: setActiveId,
                        onNew: openNewTab,
                        onNewPreview: () => openPreviewTab(""),
                        onNewGitGraph: openGitGraphFromContext,
                        onLaunchAgents: launchAgentGroup,
                        onClose: handleClose,
                        onCloseTabsToRight: handleCloseTabsToRight,
                        onCloseOtherTabs: handleCloseOtherTabs,
                        onPin: pinTab,
                        onRename: handleRenameTab,
                        onReorder: reorderTabByGap,
                        onOverrideLanguage: setOverrideLanguage,
                      }}
                    />
                    <div className="relative min-h-0 flex-1">
                      {toolView === "empty" && (
                        <EmptyToolsPanel
                          onOpenFiles={() => openSidebarView("explorer")}
                          onOpenGit={() => openSidebarView("source-control")}
                          onOpenTerminal={openNewTab}
                          onOpenPreview={() => openPreviewTab("")}
                          onOpenGitGraph={openGitGraphFromContext}
                          onLaunchAgents={launchAgentGroup}
                        />
                      )}
                      {filesLoaded && rightSidebarReady && (
                        <div
                          id="tool-explorer-panel"
                          role="tabpanel"
                          aria-label={tr("Files")}
                          aria-hidden={!toolsOpen || toolView !== "explorer"}
                          className={cn(
                            "absolute inset-0",
                            toolView !== "explorer" &&
                              "invisible pointer-events-none",
                          )}
                          inert={toolView !== "explorer"}
                        >
                          <FileExplorer
                            key={activeTaskId}
                            ref={explorerRef}
                            rootPath={explorerRoot}
                            gitStatus={
                              explorerGitDecorations
                                ? sourceControl.status
                                : null
                            }
                            activeFilePath={explorerActiveFilePath}
                            onOpenFile={handleOpenFile}
                            onPathRenamed={handleExplorerPathRenamed}
                            onPathsDeleted={handlePathsDeleted}
                            canReplacePath={canReplaceExplorerPath}
                            onRevealInTerminal={cdInNewTab}
                            onOpenInSourceControl={
                              handleOpenRepositoryInSourceControl
                            }
                            onOpenGitHistory={handleOpenGitHistoryForPath}
                            onAttachToAgent={handleAttachFileToAgent}
                            pathDropTarget={terminalPathDropTarget}
                          />
                        </div>
                      )}
                      {toolView === "source-control" &&
                        toolsOpen &&
                        rightSidebarReady && (
                          <div
                            id="tool-git-panel"
                            role="tabpanel"
                            aria-label={tr("Git")}
                            className="absolute inset-0"
                          >
                            <SourceControlPanel
                              open={toolsOpen && toolView === "source-control"}
                              sourceControl={sourceControl}
                              onOpenDiff={openToolGitDiff}
                              onOpenGitGraph={openGitGraphFromContext}
                              onOpenFile={handleOpenFile}
                              onNavigateToPath={cdInNewTab}
                              repositoryTarget={sourceControlRepositoryTarget}
                              onFollowRepositoryContext={
                                handleFollowRepositoryContext
                              }
                            />
                          </div>
                        )}

                      {toolsLoaded && (
                        <div
                          id="tool-workspace-panel"
                          role="tabpanel"
                          aria-label={tr("Right sidebar")}
                          aria-hidden={!toolsOpen || toolView !== "workspace"}
                          className={cn(
                            "absolute inset-0",
                            toolView !== "workspace" &&
                              "invisible pointer-events-none",
                          )}
                          inert={toolView !== "workspace"}
                        >
                          <WorkspaceSurface
                            tabs={tabs}
                            activeId={
                              workspacePresentationId(activeId, toolsOpen, toolView, rightSidebarReady)
                            }
                            activeTab={
                              toolsOpen &&
                              toolView === "workspace" &&
                              rightSidebarReady
                                ? activeTab
                                : undefined
                            }
                            registerTerminalHandle={registerTerminalHandle}
                            onCwd={handleTerminalCwd}
                            onExit={handleLeafExit}
                            onFocusLeaf={handleFocusLeaf}
                            registerEditorHandle={registerEditorHandle}
                            onEditorDirtyChange={handleEditorDirty}
                            onEditorCloseTab={disposeTab}
                            registerPreviewHandle={registerPreviewHandle}
                            onPreviewUrlChange={handlePreviewUrl}
                            onAiDiffAccept={(id) => respondToApproval(id, true)}
                            onAiDiffReject={(id) =>
                              respondToApproval(id, false)
                            }
                            onOpenCommitFile={openCommitFileDiffTab}
                            onSetMarkdownView={setMarkdownView}
                          />
                        </div>
                      )}
                    </div>

                    {toolView === "workspace" && (
                      <WorkspaceInputBar
                        isBlockTab={isBlockTab}
                        isTerminalTab={isTerminalTab}
                        activeLeafId={activeLeafId}
                        cwd={activeCwd}
                        home={home}
                        hasComposer={false}
                        panelOpen={false}
                        keysLoaded={false}
                        onConnect={() => void openSettingsWindow("models")}
                      />
                    )}
                  </div>
                </div>
              </ResizablePanel>
            </ResizablePanelGroup>
          </main>

          <WindowVibrancyBridge />

          <AgentNotificationsBridge
            tabs={tabs}
            activeId={
              workspacePresentationId(activeId, toolsOpen, toolView, rightSidebarReady)
            }
            onActivate={onActivateAgent}
          />
          <Toaster position="bottom-right" />
          {newProjectOpen && (
            <NewProjectDialog
              onClose={() => setNewProjectOpen(false)}
              onCreate={createProject}
            />
          )}

          {hasComposer ? (
            <>
              <AgentRunBridge
                openAiDiffTab={openTaskDiff}
                closeAiDiffTab={closeAiDiffTab}
              />
              <LocalAgentNotificationsBridge />
            </>
          ) : null}

          {hasComposer && miniPresence.mounted ? (
            <AiMiniWindow state={miniPresence.state} />
          ) : null}
          {askPresence.mounted ? (
            <SelectionAskAi
              state={askPresence.state}
              x={askPopup?.x ?? 0}
              y={askPopup?.y ?? 0}
              onAsk={onAskFromSelection}
              onDismiss={() => setAskPopup(null)}
            />
          ) : null}

          {switcherState && (
            <TabSwitcherHud tabs={taskTabs} state={switcherState} />
          )}

          <CommandPalette
            open={commandPaletteOpen}
            onOpenChange={setCommandPaletteOpen}
            initialMode={paletteInitialMode}
            commandItems={commandPaletteItems}
            workspaceRoot={explorerRoot}
            onOpenContentHit={openContentHit}
            insertCommand={insertHistoryCommand}
          />

          <NewEditorDialog
            open={newEditorOpen}
            onOpenChange={setNewEditorOpen}
            rootPath={explorerRoot ?? home}
            onCreated={(path) => handleOpenFile(path, true)}
          />

          <UpdaterDialog />

          <CloseDialogs
            tabs={tabs}
            pendingCloseTab={pendingCloseTab}
            onCancelClose={cancelClose}
            onConfirmClose={confirmClose}
            pendingTerminalCloseTab={pendingTerminalCloseTab}
            onCancelTerminalClose={cancelTerminalClose}
            onConfirmTerminalClose={confirmTerminalClose}
            pendingDeleteTabs={pendingDeleteTabs}
            onCancelDeleteClose={cancelDeleteClose}
            onConfirmDeleteClose={confirmDeleteClose}
            pendingCloseMany={pendingCloseMany}
            closeManyConfirming={closeManyConfirming}
            onCancelCloseMany={cancelCloseMany}
            onConfirmCloseMany={confirmCloseMany}
            pendingAppClose={pendingAppClose}
            onCancelAppClose={cancelAppClose}
            onConfirmAppClose={confirmAppClose}
          />
          <SettingsOverlay />
        </div>
      </TooltipProvider>
    </ThemeProvider>
  );

  return <AiComposerProvider>{shell}</AiComposerProvider>;
}
