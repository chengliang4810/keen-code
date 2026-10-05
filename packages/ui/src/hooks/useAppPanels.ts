/* eslint-disable max-lines */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createUuid } from "@zcode/shared";
import type { EmbeddedBrowserOpenUrlRequest, IPlatformService } from "@zcode/shared";
import type { CodeViewerSource } from "@/lib/codeViewer.js";
// 保活：side pane terminal 跨 workspace 会话上移到模块级 registry。
// 关闭 terminal tab 时必须显式 release，杀掉 PTY，避免常驻 registry 造成孤儿进程。
import { sidePaneTerminalSessionRegistry } from "@/terminal/sidePaneTerminalSessionRegistry.js";
import {
  buildTaskSidePaneMemoryKey,
  getSidePaneCollapsedPreference,
  readTaskSidePaneMemoryState,
  saveTaskSidePaneCollapsedPreference,
  saveTaskSidePaneMemoryState,
} from "@/lib/taskSidePaneMemory.js";
import {
  closeSidePaneTab,
  closeSidePaneTabForParent,
  closeVisibleOtherSidePaneTabs,
  closeVisibleSidePaneTabs,
  closeGitSidePane,
  closeCodeViewerSidePane,
  openWhiteboardSidePane,
  openModelTrajectorySidePane,
  openTerminalSidePane,
  openSubagentSessionSidePane,
  openSubagentDirectorySidePane,
  openSelectionSideChatPane,
  openPlanDetailSidePane,
  openWorkflowRunSidePane,
  replaceWorkflowRunSidePane,
  openWorkflowRunDirectorySidePane,
  openWorkflowActorSessionSidePane,
  openWorkflowWorkspaceSidePane,
  openWorkflowArtifactSidePane,
  activateDeveloperToolsSidePane,
  openBrowserSidePane,
  openOrActivateBrowserSidePaneByUrl,
  findBrowserSidePaneTabByUrl,
  openCodeViewerSidePane,
  openCodeViewerSidePanes,
  activateGitSidePane,
  getActiveSidePaneTab,
  getVisibleSidePaneTabs,
  sidePaneOwnerKey,
  reorderSidePaneTab,
  resolveSidePaneScopeState,
  restoreSidePaneTab,
  setActiveSidePaneTab,
  syncSubagentSessionSidePaneTabs,
  toggleBrowserSidePane,
  toggleGitSidePane,
  updateBrowserSidePaneTab,
  stampSidePaneTabsOwnership,
  type BrowserSidePaneMetadata,
  type TreemappingSidePaneTab,
  type OpenScopedSubagentSideTabRequest,
  type OpenBackgroundBashSideTabRequest,
  openBackgroundBashSidePane,
  type OpenScopedSubagentDirectorySideTabRequest,
  type OpenSelectionSideChatRequest,
  type OpenScopedPlanDetailSideTabRequest,
  type OpenScopedWorkflowRunSideTabRequest,
  type OpenScopedWorkflowRunDirectorySideTabRequest,
  type OpenScopedWorkflowActorSessionSideTabRequest,
  type OpenScopedWorkflowArtifactSideTabRequest,
  type OpenScopedWorkflowWorkspaceSideTabRequest,
  type WorkspaceSidePaneState,
  type WorkspaceSidePaneTab,
} from "@/lib/workspaceSidePane.js";
import { isSidePaneTabVisibleForParent } from "@/lib/workspaceSidePane.js";
import { logger } from "@/logger.js";
import { getPathLeaf, joinFilePath, toFileUrl } from "@/lib/path.js";
import { shouldOpenWorkflowArtifactInBrowser } from "@/lib/workflowArtifactOpen.js";
import { useWhiteboardStore } from "@/store/whiteboardStore.js";
import { useModelTrajectoryOpenBridge } from "@/hooks/useModelTrajectoryOpenBridge.js";
import { useServices } from "@/hooks/useServices.js";
import { getNativeBrowserTarget } from "@/browser-use/nativeBrowserTargetRegistry.js";
import { useIsOfficeMode } from "@/hooks/useInterfaceMode.js";
import { clearSelectionSideChat } from "@/lib/selectionSideChatRuntime.js";
import { clearConversationSelectionReferenceScope } from "@/lib/conversationSelectionReference.js";
import { subscribeTaskLifecycle } from "@/lib/taskLifecycleEvents.js";

export interface BrowserNavigationRequest {
  id: string;
  targetTabId: string;
  url: string;
}

function isAgentOpenedBrowserPopup(payload: EmbeddedBrowserOpenUrlRequest): boolean {
  // human native browser 的 owner 使用 unclaimed-iab；legacy-iab 与 iab:<uuid> 都是 Agent
  // 控制上下文。sourceTabId 还可避免把普通外链/终端链接误判为模型 popup。
  return Boolean(payload.sourceTabId && payload.browserId && payload.browserId !== "unclaimed-iab");
}

export interface RecentClosedSidePaneTab {
  tab: WorkspaceSidePaneTab;
  closedAt: number;
}

const RECENT_CLOSED_SIDE_PANE_TAB_LIMIT = 8;

function createTerminalSidePaneTitle(
  current: WorkspaceSidePaneState | null,
  workspaceAbsPath: string,
): string {
  const baseTitle = getPathLeaf(workspaceAbsPath) || "Terminal";
  const usedTitles = new Set(
    current?.tabs
      .filter((tab) => tab.type === "terminal")
      .map((tab) => tab.title.trim())
      .filter(Boolean) ?? [],
  );

  if (!usedTitles.has(baseTitle)) {
    return baseTitle;
  }

  for (let index = 2; ; index += 1) {
    const candidate = `${baseTitle} ${index}`;
    if (!usedTitles.has(candidate)) {
      return candidate;
    }
  }
}

export function useAppPanels(options: {
  workspaceAbsPath: string;
  workspaceIdentity?: string;
  workspaceRemoteSessionId?: string | null;
  activeTaskId: string | null;
  /** 草稿态与正式任务共用稳定 id，作为侧栏对话隔离边界。 */
  sidePaneOwnerId: string | null;
  isDesktop?: boolean;
  /**
   * 展示语义：视图当前是否呈现给用户（设置页覆盖时为 false）。
   * 保留该入参以维持工作区壳层调用契约；浏览器 child WebView 的生命周期由
   * NativeBrowserView 与 Rust browser_* 命令管理，不依赖旧 Electron 事件订阅。
   */
  isWorkspaceVisible?: boolean;
  supportsEmbeddedBrowser?: boolean;
  defaultWhiteboardNamePrefix: string;
  platform?: Pick<
    IPlatformService,
    | "nativeBrowserClose"
  >;
}) {
  const {
    workspaceAbsPath,
    workspaceIdentity,
    workspaceRemoteSessionId,
    activeTaskId,
    sidePaneOwnerId,
    isDesktop,
    supportsEmbeddedBrowser: explicitSupportsEmbeddedBrowser,
    defaultWhiteboardNamePrefix,
    platform,
  } = options;
  const supportsEmbeddedBrowser = explicitSupportsEmbeddedBrowser ?? Boolean(isDesktop);
  const activeWorkspaceKey = workspaceIdentity?.trim() || workspaceAbsPath;
  const { zcodeAgentService, zcodeSessionService } = useServices();
  const isOfficeMode = useIsOfficeMode();
  const sidePaneMemoryKey = useMemo(
    () =>
      buildTaskSidePaneMemoryKey({
        workspacePath: workspaceAbsPath,
        workspaceIdentity,
        taskId: activeTaskId,
      }),
    [activeTaskId, workspaceAbsPath, workspaceIdentity],
  );
  const initialSidePaneMemoryState = readTaskSidePaneMemoryState(sidePaneMemoryKey);

  // 修复说明：之前终端区域被固定写死在 <main> 底部，页面一进入就会直接挂载 Terminal，
  // 不仅默认占用高度，还会立刻创建终端会话。这里改成显式开关，默认关闭，
  // 只有用户主动点击后才渲染 Terminal，这样就不会出现"terminal 不用默认打开"的问题。
  const [isTerminalOpen, setIsTerminalOpen] = useState(false);
  // 修复说明：右侧共享面板（browser / code-viewer）需要默认关闭。
  // 否则一进工作区就会先占住主会话空间，和"只在用户主动查看时再展开"的预期相反。
  const [sidePaneState, setSidePaneState] = useState<WorkspaceSidePaneState | null>(
    initialSidePaneMemoryState.sidePaneState,
  );
  const [isSidePaneCollapsed, setIsSidePaneCollapsed] = useState(
    getSidePaneCollapsedPreference(initialSidePaneMemoryState, sidePaneOwnerId) ??
      initialSidePaneMemoryState.isSidePaneCollapsed,
  );
  // 交互说明：侧栏显隐按钮放在 App 外层，而不是 Sidebar 内部。
  // 这样即使侧栏被隐藏，入口也仍然留在左上角，不会出现"收起后没有地方再展开"的问题；
  // 同时这里统一处理 macOS 红绿灯安全区，避免按钮和系统窗口控件重叠。
  const [isSidebarVisible, setIsSidebarVisible] = useState(true);
  const [browserNavigationRequest, setBrowserNavigationRequest] =
    useState<BrowserNavigationRequest | null>(null);
  const [allRecentClosedSidePaneTabs, setAllRecentClosedSidePaneTabs] = useState<
    RecentClosedSidePaneTab[]
  >([]);
  const lastActiveSubagentTabByRootRef = useRef<Map<string, string>>(new Map());
  const sidePaneOwnerIdRef = useRef(sidePaneOwnerId);
  sidePaneOwnerIdRef.current = sidePaneOwnerId;
  const activeWorkspaceKeyRef = useRef(activeWorkspaceKey);
  activeWorkspaceKeyRef.current = activeWorkspaceKey;
  const activeSidePaneMemoryKeyRef = useRef<string | null>(sidePaneMemoryKey);
  const latestSidePaneMemoryRef = useRef({
    sidePaneState,
    isSidePaneCollapsed,
  });
  latestSidePaneMemoryRef.current = {
    sidePaneState,
    isSidePaneCollapsed,
  };

  const revealSidePaneForCurrentOwner = useCallback(() => {
    setIsSidePaneCollapsed(false);
    latestSidePaneMemoryRef.current = {
      ...latestSidePaneMemoryRef.current,
      isSidePaneCollapsed: false,
    };
    saveTaskSidePaneCollapsedPreference(
      activeSidePaneMemoryKeyRef.current,
      sidePaneOwnerIdRef.current,
      false,
    );
  }, []);

  const commitSidePaneState = useCallback(
    (updater: (current: WorkspaceSidePaneState | null) => WorkspaceSidePaneState | null) => {
      const next = updater(latestSidePaneMemoryRef.current.sidePaneState);
      // 普通侧栏交互和 BrowserView IPC 可在同一 React batch 内相邻到达。
      // 若只有 BrowserView 更新同步 ref，后到的具体值会覆盖尚未提交的打开、激活或排序更新。
      // 所有侧栏写入口统一先提交 canonical ref，保证交互与 lifecycle 严格按接收顺序合并。
      latestSidePaneMemoryRef.current = {
        ...latestSidePaneMemoryRef.current,
        sidePaneState: next,
      };
      setSidePaneState(next);
      return next;
    },
    [],
  );

  const commitOpenedSidePaneState = useCallback(
    (updater: (current: WorkspaceSidePaneState | null) => WorkspaceSidePaneState | null) => {
      commitSidePaneState((current) =>
        stampSidePaneTabsOwnership(updater(current), {
          ownerTaskId: sidePaneOwnerIdRef.current,
          workspaceKey: activeWorkspaceKeyRef.current,
          remoteSessionId: workspaceRemoteSessionId ?? null,
        }),
      );
    },
    [commitSidePaneState, workspaceRemoteSessionId],
  );

  useEffect(() => {
    const previousKey = activeSidePaneMemoryKeyRef.current;
    if (previousKey === sidePaneMemoryKey) {
      return;
    }

    // side pane 原来是 workspace 组件内的瞬时状态，跨 workspace 切换会被新渲染覆盖。
    // 这里在 key 变更前先把旧 workspace 的 tabs/折叠态写入内存，再恢复新 workspace，避免不同 workspace 串状态。
    // 同一 workspace 内切换 task 不应该影响 side pane，所以 key 已经不再按 task 维度切分。
    saveTaskSidePaneMemoryState(previousKey, latestSidePaneMemoryRef.current);
    const restored = readTaskSidePaneMemoryState(sidePaneMemoryKey);
    activeSidePaneMemoryKeyRef.current = sidePaneMemoryKey;
    commitSidePaneState(() => restored.sidePaneState);
    setIsSidePaneCollapsed(
      getSidePaneCollapsedPreference(restored, sidePaneOwnerIdRef.current) ??
        restored.isSidePaneCollapsed,
    );
  }, [commitSidePaneState, sidePaneMemoryKey]);

  useEffect(() => {
    return () => {
      saveTaskSidePaneMemoryState(
        activeSidePaneMemoryKeyRef.current,
        latestSidePaneMemoryRef.current,
      );
    };
  }, []);

  const hasVisibleSidePaneTabs = useCallback(
    (next: WorkspaceSidePaneState | null) =>
      Boolean(
        next &&
        getVisibleSidePaneTabs(next.tabs, {
          workspaceKey: activeWorkspaceKey,
          ownerTaskId: sidePaneOwnerId,
        }).length,
      ),
    [activeWorkspaceKey, sidePaneOwnerId],
  );

  const syncSidePaneCollapsedWithTabs = useCallback(
    (next: WorkspaceSidePaneState | null) => {
      if (!hasVisibleSidePaneTabs(next)) {
        // 生命周期收口不能因为仍有其他可见 tab 就展开面板，否则会覆盖用户对当前
        // owner 的主动收起偏好；只有没有可见内容时才强制收起空面板。
        setIsSidePaneCollapsed(true);
      }
    },
    [hasVisibleSidePaneTabs],
  );

  useEffect(() => {
    commitSidePaneState((current) => {
      const preferredTabId = activeTaskId
        ? lastActiveSubagentTabByRootRef.current.get(activeTaskId)
        : undefined;
      const collapsedPreference = getSidePaneCollapsedPreference(
        readTaskSidePaneMemoryState(activeSidePaneMemoryKeyRef.current),
        sidePaneOwnerId,
      );
      const resolved = resolveSidePaneScopeState(
        current,
        { workspaceKey: activeWorkspaceKey, ownerTaskId: sidePaneOwnerId },
        preferredTabId,
        collapsedPreference,
      );
      setIsSidePaneCollapsed(resolved.isSidePaneCollapsed);
      logger.debug("[App] 同步对话右侧面板 scope", {
        activeTabId: resolved.sidePaneState?.activeTabId ?? null,
        activeTaskId,
        isSidePaneCollapsed: resolved.isSidePaneCollapsed,
        ownerTaskId: sidePaneOwnerId,
        preferredTabId: preferredTabId ?? null,
        workspaceKey: activeWorkspaceKey,
      });
      return resolved.sidePaneState;
    });
  }, [activeTaskId, activeWorkspaceKey, commitSidePaneState, sidePaneOwnerId]);

  const handleOpenCodeViewer = useCallback(
    (source: CodeViewerSource) => {
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) => {
        const next = openCodeViewerSidePane(current, source, sidePaneOwnerIdRef.current);
        const activeTab = getActiveSidePaneTab(next);
        const activePath =
          activeTab?.type === "code-viewer" ? (activeTab.source.path ?? "none") : "none";
        logger.info(
          `[App] 切换右侧面板 mode=code-viewer workspace=${workspaceAbsPath} title=${source.title} path=${activePath} tabs=${next.tabs.length}`,
        );
        return next;
      });
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner, workspaceAbsPath],
  );

  const handleOpenCodeViewers = useCallback(
    (sources: readonly CodeViewerSource[]) => {
      if (sources.length === 0) return;
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) => {
        const next = openCodeViewerSidePanes(current, sources, sidePaneOwnerIdRef.current, 0);
        logger.info(
          `[App] 批量打开右侧预览 workspace=${workspaceAbsPath} sources=${sources.length} tabs=${next.tabs.length}`,
        );
        return next;
      });
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner, workspaceAbsPath],
  );

  const handleOpenBrowserUrl = useCallback(
    (request: string | EmbeddedBrowserOpenUrlRequest) => {
      const payload: EmbeddedBrowserOpenUrlRequest =
        typeof request === "string" ? { url: request, disposition: "foreground-tab" } : request;
      const sourceWorkspaceKey = payload.workspaceKey ?? activeWorkspaceKeyRef.current;
      const sourceSessionId = payload.sessionId ?? sidePaneOwnerIdRef.current;
      const sourceRemoteSessionId =
        payload.remoteSessionId ?? workspaceRemoteSessionId ?? undefined;
      const isCurrentOwner =
        sourceWorkspaceKey === activeWorkspaceKeyRef.current &&
        (sourceRemoteSessionId ?? "") === (workspaceRemoteSessionId ?? "") &&
        sourceSessionId === sidePaneOwnerIdRef.current;
      if (!supportsEmbeddedBrowser) {
        // Web 端没有内置浏览器面板，这里退回浏览器新标签，至少保证外链是可访问的。
        window.open(payload.url, "_blank", "noopener,noreferrer");
        return;
      }

      // 交互说明：消息区只负责抛出"打开这个 URL"的意图，
      // 真正的 native child WebView 导航、地址校验和面板显隐仍统一收口在浏览器面板一侧处理。
      const isShareUrl = /^https?:\/\/[^/]+\/(?:cn\/)?share\/[^/]+$/u.test(payload.url);
      const targetTabId = `browser:${createUuid()}`;
      // Agent 控制的 guest 触发 popup 时，新的页面仍属于模型操作链路；不能把它
      // 当作人类新开的 Browser tab，继承 setting.json 中保存的自由尺寸/缩放偏好。
      const agentOpened = isAgentOpenedBrowserPopup(payload);
      // 新 tab 已携带 initialUrl；只对已有 tab 的复用路径发 navigationRequest，
      // 避免同一个新 tab 同时消费 initialUrl 和 request，导致 Rust 重建两代 child。
      if (isCurrentOwner) revealSidePaneForCurrentOwner();
      logger.info(
        `[App] ${isCurrentOwner ? "切换" : "后台挂载"}右侧面板 mode=browser workspace=${sourceWorkspaceKey} sessionId=${sourceSessionId} url=${payload.url}`,
      );
      commitOpenedSidePaneState((current) =>
        isShareUrl
          ? openOrActivateBrowserSidePaneByUrl(current, {
              initialUrl: payload.url,
              ownerTaskId: sourceSessionId,
              workspaceKey: sourceWorkspaceKey,
              ...(sourceRemoteSessionId ? { remoteSessionId: sourceRemoteSessionId } : {}),
            })
          : openBrowserSidePane(current, {
              tabId: targetTabId,
              initialUrl: payload.url,
              ownerTaskId: sourceSessionId,
              workspaceKey: sourceWorkspaceKey,
              // 这两条路径都带 ownerTaskId，stampSidePaneTabsOwnership 不会再补 scope，
              // remoteSessionId 必须在创建时就冻结，否则远程下这个 tab 关不掉。
              ...(sourceRemoteSessionId ? { remoteSessionId: sourceRemoteSessionId } : {}),
              activate: isCurrentOwner,
              agentOpened,
            }),
      );
    },
    [
      commitOpenedSidePaneState,
      revealSidePaneForCurrentOwner,
      supportsEmbeddedBrowser,
      workspaceAbsPath,
      workspaceRemoteSessionId,
    ],
  );

  const handleToggleBrowser = useCallback(() => {
    if (!supportsEmbeddedBrowser) {
      // 能力边界：Web/mobile 当前不支持 native child browser，不创建 Browser side pane。
      logger.info(`[App] 当前壳层不支持内嵌浏览器，忽略切换请求 workspace=${workspaceAbsPath}`);
      return;
    }

    commitOpenedSidePaneState((current) => {
      const activeTab = getActiveSidePaneTab(current);
      const closingActiveBrowser =
        activeTab?.type === "browser" &&
        sidePaneOwnerKey(activeTab.ownerTaskId) === sidePaneOwnerKey(sidePaneOwnerIdRef.current);
      const next = toggleBrowserSidePane(
        current,
        sidePaneOwnerIdRef.current,
        workspaceRemoteSessionId,
      );
      if (closingActiveBrowser) {
        syncSidePaneCollapsedWithTabs(next);
      } else {
        revealSidePaneForCurrentOwner();
      }
      const nextActiveTab = getActiveSidePaneTab(next);
      logger.info(
        `[App] 切换右侧面板 mode=${nextActiveTab?.type ?? "none"} workspace=${workspaceAbsPath} tabs=${next?.tabs.length ?? 0}`,
      );
      return next;
    });
  }, [
    commitOpenedSidePaneState,
    revealSidePaneForCurrentOwner,
    supportsEmbeddedBrowser,
    syncSidePaneCollapsedWithTabs,
    workspaceAbsPath,
    workspaceRemoteSessionId,
  ]);

  const handleOpenBrowserTab = useCallback(() => {
    if (!supportsEmbeddedBrowser) {
      // 能力边界：Web/mobile 当前不支持 native child browser，不创建 Browser side pane。
      logger.info(`[App] 当前壳层不支持内嵌浏览器，忽略新建请求 workspace=${workspaceAbsPath}`);
      return;
    }

    revealSidePaneForCurrentOwner();
    commitOpenedSidePaneState((current) => {
      const next = openBrowserSidePane(current);
      const activeTab = getActiveSidePaneTab(next);
      logger.info(
        `[App] 新建右侧浏览器 tab=${activeTab?.id ?? "none"} workspace=${workspaceAbsPath} tabs=${next.tabs.length}`,
      );
      return next;
    });
  }, [
    commitOpenedSidePaneState,
    revealSidePaneForCurrentOwner,
    supportsEmbeddedBrowser,
    workspaceAbsPath,
  ]);

  const handleToggleGit = useCallback(() => {
    commitOpenedSidePaneState((current) => {
      if (isOfficeMode && !current?.tabs.some((tab) => tab.type === "git")) return current;
      const closingActiveGit = getActiveSidePaneTab(current)?.type === "git";
      const next = toggleGitSidePane(current);
      if (closingActiveGit) {
        syncSidePaneCollapsedWithTabs(next);
      } else {
        revealSidePaneForCurrentOwner();
      }
      const activeTab = getActiveSidePaneTab(next);
      logger.info(
        `[App] 切换右侧面板 mode=${activeTab?.type ?? "none"} workspace=${workspaceAbsPath} tabs=${next?.tabs.length ?? 0}`,
      );
      return next;
    });
  }, [
    isOfficeMode,
    commitOpenedSidePaneState,
    revealSidePaneForCurrentOwner,
    syncSidePaneCollapsedWithTabs,
    workspaceAbsPath,
  ]);

  const handleOpenGit = useCallback(() => {
    commitOpenedSidePaneState((current) => {
      if (isOfficeMode && !current?.tabs.some((tab) => tab.type === "git")) return current;
      const next = activateGitSidePane(current);
      // 文件变更查找只需要“确保 Git 面板打开”，不能复用 toggle。
      // 如果当前已经在 Git tab 上，toggle 会把它关掉，导致切到文件变更范围反而看不到内容。
      revealSidePaneForCurrentOwner();
      logger.info(
        `[App] 打开右侧面板 mode=git workspace=${workspaceAbsPath} tabs=${next.tabs.length}`,
      );
      return next;
    });
  }, [isOfficeMode, commitOpenedSidePaneState, revealSidePaneForCurrentOwner, workspaceAbsPath]);

  const handleOpenTreemapping = useCallback(
    (source?: TreemappingSidePaneTab["source"]) => {
      // Treemapping 功能当前需要从侧边栏隐藏。保留回调形状给消息链路兼容，
      // 但不再创建 side pane tab，避免 header 或旧入口绕过菜单隐藏。
      logger.debug(
        `[App] 已隐藏 treemapping 侧边栏入口 workspace=${workspaceAbsPath} source=${source?.kind ?? "current"}`,
      );
    },
    [workspaceAbsPath],
  );

  const handleOpenWhiteboard = useCallback(() => {
    const board = useWhiteboardStore.getState().createBoard({
      defaultNamePrefix: defaultWhiteboardNamePrefix,
      workspaceIdentity,
      workspacePath: workspaceAbsPath,
    });
    revealSidePaneForCurrentOwner();
    commitOpenedSidePaneState((current) => {
      const next = openWhiteboardSidePane(current, {
        boardId: board.id,
        title: board.name,
      });
      logger.info(
        `[App] 打开右侧面板 mode=whiteboard workspace=${workspaceAbsPath} board=${board.id} tabs=${next.tabs.length}`,
      );
      return next;
    });
  }, [
    commitOpenedSidePaneState,
    defaultWhiteboardNamePrefix,
    revealSidePaneForCurrentOwner,
    workspaceAbsPath,
    workspaceIdentity,
  ]);

  const handleOpenDeveloperTools = useCallback(() => {
    revealSidePaneForCurrentOwner();
    commitOpenedSidePaneState((current) => {
      const next = activateDeveloperToolsSidePane(current);
      logger.info(
        `[App] 打开右侧面板 mode=developer-tools workspace=${workspaceAbsPath} tabs=${next.tabs.length}`,
      );
      return next;
    });
  }, [commitOpenedSidePaneState, revealSidePaneForCurrentOwner, workspaceAbsPath]);

  const handleOpenTerminalTab = useCallback(() => {
    if (isOfficeMode) return;
    revealSidePaneForCurrentOwner();
    commitOpenedSidePaneState((current) => {
      const title = createTerminalSidePaneTitle(current, workspaceAbsPath);
      const next = openTerminalSidePane(current, {
        title,
        cwd: workspaceAbsPath,
        remoteSessionId: workspaceRemoteSessionId,
      });
      logger.info(
        `[App] 新建右侧终端 tab=${title} workspace=${workspaceAbsPath} tabs=${next.tabs.length}`,
      );
      return next;
    });
  }, [
    isOfficeMode,
    commitOpenedSidePaneState,
    revealSidePaneForCurrentOwner,
    workspaceAbsPath,
    workspaceRemoteSessionId,
  ]);

  const handleOpenModelTrajectory = useCallback(
    (params: { taskId: string; title?: string | null }) => {
      if (!params.taskId) {
        return;
      }
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) => {
        const next = openModelTrajectorySidePane(current, params);
        logger.info(
          `[App] 打开右侧面板 mode=model-trajectory workspace=${workspaceAbsPath} taskId=${params.taskId} tabs=${next.tabs.length}`,
        );
        return next;
      });
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner, workspaceAbsPath],
  );

  const handleOpenBackgroundBash = useCallback(
    (request: OpenBackgroundBashSideTabRequest) => {
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) => openBackgroundBashSidePane(current, request));
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner],
  );

  const handleOpenSubagentSession = useCallback(
    (request: OpenScopedSubagentSideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      const rootSessionId = request.rootSessionId ?? request.parentSessionId;
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) => {
        const next = openSubagentSessionSidePane(current, {
          workspaceKey,
          workspacePath: request.workspacePath,
          ...(request.workspaceIdentity ? { workspaceIdentity: request.workspaceIdentity } : {}),
          ...(request.remoteSessionId ? { remoteSessionId: request.remoteSessionId } : {}),
          rootSessionId,
          parentSessionId: request.parentSessionId,
          childSessionId: request.childSessionId,
          subagentType: request.subagentType,
          title: request.title,
        });
        lastActiveSubagentTabByRootRef.current.set(rootSessionId, next.activeTabId);
        logger.debug(
          `[App] 打开子智能体右侧 tab parent=${request.parentSessionId} child=${request.childSessionId} workspace=${workspaceKey}`,
        );
        return next;
      });
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner],
  );

  const handleOpenSubagentDirectory = useCallback(
    (request: OpenScopedSubagentDirectorySideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      const rootSessionId = request.rootSessionId ?? request.parentSessionId;
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) => {
        const next = openSubagentDirectorySidePane(current, {
          workspaceKey,
          workspacePath: request.workspacePath,
          ...(request.workspaceIdentity ? { workspaceIdentity: request.workspaceIdentity } : {}),
          ...(request.remoteSessionId ? { remoteSessionId: request.remoteSessionId } : {}),
          rootSessionId,
          parentSessionId: request.parentSessionId,
        });
        lastActiveSubagentTabByRootRef.current.set(rootSessionId, next.activeTabId);
        return next;
      });
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner],
  );

  const handleSyncSubagentSessionTabs = useCallback(
    (request: import("@/lib/workspaceSidePane.js").SyncSubagentSessionTabsRequest) => {
      // 分支 edit/retry 的失效 tab 属于投影清理，不进入“最近关闭”。
      commitSidePaneState((current) => syncSubagentSessionSidePaneTabs(current, request));
    },
    [commitSidePaneState],
  );

  const handleOpenSelectionSideChat = useCallback(
    (request: OpenSelectionSideChatRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) => {
        const staleTab = request.replacesChildSessionId
          ? current?.tabs.find(
              (tab) =>
                tab.type === "selection-side-chat" &&
                tab.workspaceKey === workspaceKey &&
                tab.parentSessionId === request.parentSessionId &&
                tab.childSessionId === request.replacesChildSessionId,
            )
          : undefined;
        const withoutStale = staleTab ? closeSidePaneTab(current, staleTab.id) : current;
        return openSelectionSideChatPane(withoutStale, {
          ...request,
          workspaceKey,
        });
      });
      logger.debug("[App] 打开框选副屏会话", {
        childSessionId: request.childSessionId,
        parentSessionId: request.parentSessionId,
        workspaceKey,
      });
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner],
  );

  const handleOpenPlanDetail = useCallback(
    (request: OpenScopedPlanDetailSideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      revealSidePaneForCurrentOwner();
      commitOpenedSidePaneState((current) =>
        openPlanDetailSidePane(current, {
          ...request,
          workspaceKey,
        }),
      );
      logger.debug("[App] 打开计划详情右侧 tab", {
        parentSessionId: request.parentSessionId,
        toolCallId: request.toolCallId,
        workspaceKey,
      });
    },
    [commitOpenedSidePaneState, revealSidePaneForCurrentOwner],
  );

  const handleOpenWorkflowRun = useCallback(
    (request: OpenScopedWorkflowRunSideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      // 「配置」之后的原地替换不是一次打开：收起的侧栏保持收起。
      const replaceRunId = request.replaceRunId;
      if (replaceRunId === undefined) setIsSidePaneCollapsed(false);
      commitOpenedSidePaneState((current) =>
        replaceRunId === undefined
          ? openWorkflowRunSidePane(current, { ...request, workspaceKey })
          : replaceWorkflowRunSidePane(current, { ...request, workspaceKey, replaceRunId }),
      );
      logger.debug("[App] 打开工作流运行详情右侧 tab", {
        parentSessionId: request.parentSessionId,
        runId: request.runId,
        toolCallId: request.toolCallId,
        workspaceKey,
      });
    },
    [commitOpenedSidePaneState],
  );

  const handleOpenWorkflowRunDirectory = useCallback(
    (request: OpenScopedWorkflowRunDirectorySideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      setIsSidePaneCollapsed(false);
      commitOpenedSidePaneState((current) =>
        openWorkflowRunDirectorySidePane(current, {
          ...request,
          workspaceKey,
        }),
      );
      logger.debug("[App] 打开工作流运行目录右侧 tab", {
        parentSessionId: request.parentSessionId,
        workspaceKey,
      });
    },
    [commitOpenedSidePaneState],
  );

  const handleOpenWorkflowActorSession = useCallback(
    (request: OpenScopedWorkflowActorSessionSideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      setIsSidePaneCollapsed(false);
      commitOpenedSidePaneState((current) =>
        openWorkflowActorSessionSidePane(current, {
          ...request,
          workspaceKey,
        }),
      );
      logger.debug("[App] 打开工作流 actor 会话右侧 tab", {
        actorSessionId: request.actorSessionId,
        parentSessionId: request.parentSessionId,
        runId: request.runId,
        workspaceKey,
      });
    },
    [commitOpenedSidePaneState],
  );

  const handleOpenWorkflowWorkspace = useCallback(
    (request: OpenScopedWorkflowWorkspaceSideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      setIsSidePaneCollapsed(false);
      commitOpenedSidePaneState((current) =>
        openWorkflowWorkspaceSidePane(current, {
          ...request,
          workspaceKey,
        }),
      );
      logger.debug("[App] 打开工作流工作区右侧 tab", {
        parentSessionId: request.parentSessionId,
        phaseId: request.phaseId,
        runId: request.runId,
        workspaceKey,
      });
    },
    [commitOpenedSidePaneState],
  );

  /**
   * 按 URL 复用的 browser tab（html 产物直开的落点）。
   *
   * 与 `handleOpenBrowserUrl` 有三条区别：
   * ① 落点按 URL 认领而不是每次新开；② 命中已有 tab 时**仍然**发一次导航请求 —— native browser
   * 停在旧字节上，`initialUrl` 又没变，挂载时那次导航不会再跑
   * （UnifiedBrowserView 的 `lastAppliedInitialUrlRef` 按 URL 去重），v2 就永远显示不出来；
   * ③ 归属由调用方给定，且**恒激活**——这条路径只由用户点击产物触发，没有
   * `isCurrentOwner` 那种后台挂载的情形（详见下面 ownerTaskId 处的注释）。
   */
  const openFileUrlInBrowserSidePane = useCallback(
    (params: { url: string; ownerTaskId: string; workspaceKey: string }) => {
      const state = latestSidePaneMemoryRef.current.sidePaneState;
      const existing = findBrowserSidePaneTabByUrl(state, {
        initialUrl: params.url,
        ownerTaskId: params.ownerTaskId,
        workspaceKey: params.workspaceKey,
      });
      const targetTabId = existing?.id ?? `browser:${createUuid()}`;
      setBrowserNavigationRequest({ id: createUuid(), targetTabId, url: params.url });
      // 与产物 tab 一样只置当前折叠态，不落盘偏好：归属写的是 params.ownerTaskId，
      // 而 revealSidePaneForCurrentOwner 会把 false 记在**当前** owner 名下——中枢那条
      // 路径上两者还不是同一个人。
      setIsSidePaneCollapsed(false);
      commitOpenedSidePaneState((current) =>
        openOrActivateBrowserSidePaneByUrl(current, {
          initialUrl: params.url,
          tabId: targetTabId,
          // 归属显式冻结，不靠 stampSidePaneTabsOwnership 盖当前 owner：中枢的产物 chip
          // 先 handleSelectTaskInChat 再 handleOpenWorkflowArtifact，同一个同步块里
          // sidePaneOwnerIdRef 还停在上一条会话上。browser tab 的可见性按 ownerTaskId 收窄
          // （getVisibleSidePaneTabsByScope 的默认分支），盖错了这个 tab 切换落定后就再也看不见。
          // 产物 tab 不怕这一手，是因为它按 parentSessionId 收窄。
          ownerTaskId: params.ownerTaskId,
          workspaceKey: params.workspaceKey,
        }),
      );
      return targetTabId;
    },
    [commitOpenedSidePaneState],
  );

  /**
   * 产物点击的**唯一**落点裁决处：html 且开得起内嵌浏览器就直接开页面，其余一律开产物 tab。
   *
   * 直开缺的那一段是 `sourcePath`：run 侧板带得到（它已经合过 journal），药丸摘要刻意不带
   * （状态帧体积，见 `workflowRunArtifactSummarySchema`）。缺席时这里补查一次 journal。
   * 查询失败、老 CLI 没有这条查询、产物根本没有出处，全部退回产物 tab —— 点击绝不落空。
   */
  const handleOpenWorkflowArtifact = useCallback(
    (request: OpenScopedWorkflowArtifactSideTabRequest) => {
      const workspaceKey = request.workspaceIdentity?.trim() || request.workspacePath;
      const openArtifactTab = () => {
        setIsSidePaneCollapsed(false);
        commitOpenedSidePaneState((current) =>
          openWorkflowArtifactSidePane(current, {
            ...request,
            workspaceKey,
          }),
        );
        logger.debug("[App] 打开工作流产物右侧 tab", {
          artifactId: request.artifactId,
          parentSessionId: request.parentSessionId,
          runId: request.runId,
          workspaceKey,
        });
      };

      if (
        !shouldOpenWorkflowArtifactInBrowser({
          contentType: request.contentType,
          supportsEmbeddedBrowser,
          workspaceIdentity: request.workspaceIdentity,
          remoteSessionId: request.remoteSessionId,
        })
      ) {
        openArtifactTab();
        return;
      }

      const openInBrowser = (sourcePath: string) => {
        // 工作区相对的 sourcePath 拼上 workspacePath 才是本机真实位置；与产物卡上
        // 「在浏览器中打开」走同一条路径（WorkflowArtifactSidePane 的 localSourcePath）。
        const url = toFileUrl(joinFilePath(request.workspacePath, sourcePath));
        const tabId = openFileUrlInBrowserSidePane({
          url,
          ownerTaskId: request.parentSessionId,
          workspaceKey,
        });
        logger.debug("[App] html 产物直接打开浏览器 tab", {
          artifactId: request.artifactId,
          parentSessionId: request.parentSessionId,
          runId: request.runId,
          tabId,
          workspaceKey,
        });
      };

      const knownSourcePath = request.sourcePath?.trim();
      if (knownSourcePath) {
        openInBrowser(knownSourcePath);
        return;
      }

      void (async () => {
        try {
          // 判据已经保证是本地 workspace（无 workspaceIdentity / remoteSessionId），
          // 这里只带 workspacePath。
          const result = await zcodeAgentService.conversationWorkflowRunArtifactsV4({
            workspacePath: request.workspacePath,
            sessionId: request.parentSessionId,
            runId: request.runId,
          });
          const sourcePath = result.artifacts
            .find((artifact) => artifact.id === request.artifactId)
            ?.sourcePath?.trim();
          if (!sourcePath) {
            logger.debug("[App] html 产物没有工作区出处，退回产物 tab", {
              artifactId: request.artifactId,
              runId: request.runId,
            });
            openArtifactTab();
            return;
          }
          openInBrowser(sourcePath);
        } catch (error) {
          logger.warn("[App] 查产物出处失败，退回产物 tab", {
            artifactId: request.artifactId,
            error: error instanceof Error ? error.message : String(error),
            runId: request.runId,
          });
          openArtifactTab();
        }
      })();
    },
    [
      commitOpenedSidePaneState,
      openFileUrlInBrowserSidePane,
      supportsEmbeddedBrowser,
      zcodeAgentService,
    ],
  );

  const closeSelectionSideChatRuntime = useCallback(
    (tab: Extract<WorkspaceSidePaneTab, { type: "selection-side-chat" }>) => {
      clearSelectionSideChat(tab.childSessionId);
      clearConversationSelectionReferenceScope(tab.childSessionId, tab.workspaceKey);
      void zcodeSessionService
        .closeSession({
          workspacePath: tab.workspacePath,
          ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
          sessionId: tab.childSessionId,
        })
        .catch((error) => {
          if (String(error).includes("sessionNotFound")) return;
          logger.warn("[App] 关闭框选副屏 runtime 失败", {
            childSessionId: tab.childSessionId,
            error: error instanceof Error ? error.message : String(error),
          });
        });
    },
    [zcodeSessionService],
  );

  useEffect(() => {
    const workspaceKey = workspaceIdentity?.trim() || workspaceAbsPath;
    return subscribeTaskLifecycle((event) => {
      if (event.workspaceKey !== workspaceKey) return;
      clearConversationSelectionReferenceScope(event.taskId, event.workspaceKey);
      const closingTabs =
        latestSidePaneMemoryRef.current.sidePaneState?.tabs.filter(
          (tab): tab is Extract<WorkspaceSidePaneTab, { type: "selection-side-chat" }> =>
            tab.type === "selection-side-chat" && tab.parentSessionId === event.taskId,
        ) ?? [];
      if (closingTabs.length === 0) return;

      for (const tab of closingTabs) closeSelectionSideChatRuntime(tab);
      const closingIds = new Set(closingTabs.map((tab) => tab.id));
      commitSidePaneState((current) => {
        if (!current) return current;
        let next: WorkspaceSidePaneState | null = current;
        for (const tabId of closingIds) {
          next = closeSidePaneTabForParent(next, tabId, activeTaskId);
        }
        syncSidePaneCollapsedWithTabs(next);
        return next;
      });
      logger.info("[App] 父任务结束，清理框选副屏会话", {
        event: event.type,
        parentSessionId: event.taskId,
        workspaceKey,
      });
    });
  }, [
    activeTaskId,
    closeSelectionSideChatRuntime,
    commitSidePaneState,
    syncSidePaneCollapsedWithTabs,
    workspaceAbsPath,
    workspaceIdentity,
  ]);

  // 订阅“打开模型调用轨迹”请求：菜单深处通过单例 store 发起，这里按 workspaceKey 匹配后消费。
  useModelTrajectoryOpenBridge(
    workspaceIdentity?.trim() || workspaceAbsPath,
    handleOpenModelTrajectory,
  );

  const handleToggleTerminal = useCallback(() => {
    setIsTerminalOpen((open) => {
      if (isOfficeMode && !open) return open;
      const nextOpen = !open;
      logger.info("[App] 切换底部终端面板", {
        open: nextOpen,
        workspace: workspaceAbsPath,
      });
      return nextOpen;
    });
  }, [isOfficeMode, workspaceAbsPath]);

  const handleToggleSidebar = useCallback(() => {
    setIsSidebarVisible((visible) => !visible);
  }, []);

  const handleToggleSidePaneCollapse = useCallback(() => {
    setIsSidePaneCollapsed((collapsed) => {
      const nextCollapsed = !collapsed;
      // tabs 按 workspace 复用，但顶部收起是用户对当前对话的明确选择；
      // 记录 owner 偏好，避免切换对话后 scope 解析又被可见 tab 自动展开覆盖。
      saveTaskSidePaneCollapsedPreference(
        activeSidePaneMemoryKeyRef.current,
        sidePaneOwnerIdRef.current,
        nextCollapsed,
      );
      latestSidePaneMemoryRef.current = {
        ...latestSidePaneMemoryRef.current,
        isSidePaneCollapsed: nextCollapsed,
      };
      logger.info(
        `[App] ${nextCollapsed ? "收起" : "展开"}右侧面板 workspace=${workspaceAbsPath} tabs=${sidePaneState?.tabs.length ?? 0}`,
      );
      return nextCollapsed;
    });
  }, [sidePaneState, workspaceAbsPath]);

  const handleCloseCodeViewer = useCallback(() => {
    logger.info(`[App] 关闭右侧面板 mode=code-viewer workspace=${workspaceAbsPath}`);
    commitSidePaneState((current) => {
      const next = closeCodeViewerSidePane(current);
      syncSidePaneCollapsedWithTabs(next);
      return next;
    });
  }, [commitSidePaneState, syncSidePaneCollapsedWithTabs, workspaceAbsPath]);

  const handleCloseGit = useCallback(() => {
    logger.info(`[App] 关闭右侧面板 mode=git workspace=${workspaceAbsPath}`);
    commitSidePaneState((current) => {
      const next = closeGitSidePane(current);
      syncSidePaneCollapsedWithTabs(next);
      return next;
    });
  }, [commitSidePaneState, syncSidePaneCollapsedWithTabs, workspaceAbsPath]);

  const handleActivateSidePaneTab = useCallback(
    (tabId: string) => {
      commitSidePaneState((current) => {
        const target = current?.tabs.find((candidate) => candidate.id === tabId);
        if (target?.type === "subagent-session" || target?.type === "subagent-directory") {
          lastActiveSubagentTabByRootRef.current.set(target.rootSessionId, target.id);
        }
        return setActiveSidePaneTab(current, tabId);
      });
    },
    [commitSidePaneState],
  );

  const handleReorderSidePaneTab = useCallback(
    (activeTabId: string, overTabId: string) => {
      commitSidePaneState((current) => reorderSidePaneTab(current, activeTabId, overTabId));
    },
    [commitSidePaneState],
  );

  const rememberClosedSidePaneTabs = useCallback((tabs: WorkspaceSidePaneTab[]) => {
    const restorableTabs = tabs.filter(
      (tab) => tab.type !== "selection-side-chat" && tab.type !== "browser-use",
    );
    if (restorableTabs.length === 0) {
      return;
    }

    const closedAt = Date.now();
    setAllRecentClosedSidePaneTabs((current) => {
      const closingIds = new Set(restorableTabs.map((tab) => tab.id));
      return [
        ...restorableTabs.map((tab) => ({ tab, closedAt })),
        ...current.filter((item) => !closingIds.has(item.tab.id)),
      ].slice(0, RECENT_CLOSED_SIDE_PANE_TAB_LIMIT);
    });
  }, []);

  const closeBrowserTabsWithAuthority = useCallback(
    async (tabs: readonly WorkspaceSidePaneTab[]): Promise<boolean> => {
      const browserTabs = tabs.filter(
        (tab) => tab.type === "browser" || tab.type === "browser-use",
      );
      if (browserTabs.length === 0) return true;
      if (!platform?.nativeBrowserClose) return !isDesktop;
      const targets = browserTabs
        .map((tab) => getNativeBrowserTarget(tab.type === "browser-use" ? tab.tabId : tab.id))
        .filter((target): target is NonNullable<typeof target> => target !== null);
      // child 的 close 必须带创建时的 owner+generation；未挂载的 suspended tab 已在
      // 进入 suspended 时完成卸载清理，因此没有 target 时不再发送裸 tabId。
      await Promise.all(targets.map((target) => platform.nativeBrowserClose!(target)));
      return true;
    },
    [isDesktop, platform],
  );

  const handleCloseSidePaneTab = useCallback(
    (tabId: string) => {
      const closingTab = sidePaneState?.tabs.find((tab) => tab.id === tabId);
      if (closingTab?.type === "selection-side-chat") {
        closeSelectionSideChatRuntime(closingTab);
      }
      void closeBrowserTabsWithAuthority(closingTab ? [closingTab] : []).then((authorized) => {
        if (!authorized) return;
        if (closingTab) rememberClosedSidePaneTabs([closingTab]);
        // 保活：显式关闭 terminal tab 必须真回收 PTY/xterm（registry 常驻，不会随卸载自动回收）。
        if (closingTab?.type === "terminal") {
          sidePaneTerminalSessionRegistry.release(tabId);
        }
        const next = commitSidePaneState((current) =>
          closeSidePaneTabForParent(
            current,
            tabId,
            activeTaskId,
            activeTaskId ? lastActiveSubagentTabByRootRef.current.get(activeTaskId) : null,
          ),
        );
        syncSidePaneCollapsedWithTabs(next);
        const activeTab = getActiveSidePaneTab(next);
        logger.info(
          `[App] 关闭右侧面板 tab=${tabId} mode=${activeTab?.type ?? "none"} workspace=${workspaceAbsPath} tabs=${next?.tabs.length ?? 0}`,
        );
      });
    },
    [
      activeTaskId,
      closeSelectionSideChatRuntime,
      closeBrowserTabsWithAuthority,
      commitSidePaneState,
      rememberClosedSidePaneTabs,
      sidePaneState?.tabs,
      syncSidePaneCollapsedWithTabs,
      workspaceAbsPath,
    ],
  );

  const handleCloseOtherSidePaneTabs = useCallback(
    (tabId: string) => {
      const visibleTabs =
        sidePaneState?.tabs.filter((tab) => isSidePaneTabVisibleForParent(tab, activeTaskId)) ?? [];
      const targetExists = visibleTabs.some((tab) => tab.id === tabId);
      const closingTabs = targetExists ? visibleTabs.filter((tab) => tab.id !== tabId) : [];
      void closeBrowserTabsWithAuthority(closingTabs).then((authorized) => {
        if (!authorized) return;
        for (const tab of closingTabs) {
          if (tab.type === "selection-side-chat") closeSelectionSideChatRuntime(tab);
        }
        // 保活：批量关闭其他 tab 时，回收其中 terminal tab 的常驻 PTY/xterm。
        for (const tab of closingTabs) {
          if (tab.type === "terminal") {
            sidePaneTerminalSessionRegistry.release(tab.id);
          }
        }
        rememberClosedSidePaneTabs(closingTabs);
        commitSidePaneState((current) => {
          const next = closeVisibleOtherSidePaneTabs(current, tabId, activeTaskId);
          logger.info(
            `[App] 关闭其他右侧面板 tab=${tabId} workspace=${workspaceAbsPath} tabs=${next?.tabs.length ?? 0}`,
          );
          return next;
        });
      });
    },
    [
      activeTaskId,
      closeSelectionSideChatRuntime,
      closeBrowserTabsWithAuthority,
      commitSidePaneState,
      rememberClosedSidePaneTabs,
      sidePaneState?.tabs,
      workspaceAbsPath,
    ],
  );

  const handleCloseAllSidePaneTabs = useCallback(() => {
    const visibleTabs =
      sidePaneState?.tabs.filter((tab) => isSidePaneTabVisibleForParent(tab, activeTaskId)) ?? [];
    void closeBrowserTabsWithAuthority(visibleTabs).then((authorized) => {
      if (!authorized) return;
      for (const tab of visibleTabs) {
        if (tab.type === "selection-side-chat") closeSelectionSideChatRuntime(tab);
      }
      // 保活：关闭全部 tab 时，回收其中 terminal tab 的常驻 PTY/xterm。
      for (const tab of visibleTabs) {
        if (tab.type === "terminal") {
          sidePaneTerminalSessionRegistry.release(tab.id);
        }
      }
      rememberClosedSidePaneTabs(visibleTabs);
      commitSidePaneState((current) => {
        logger.info(`[App] 关闭全部右侧面板 tabs workspace=${workspaceAbsPath}`);
        const next = closeVisibleSidePaneTabs(current, activeTaskId);
        syncSidePaneCollapsedWithTabs(next);
        return next;
      });
    });
  }, [
    activeTaskId,
    closeSelectionSideChatRuntime,
    closeBrowserTabsWithAuthority,
    commitSidePaneState,
    rememberClosedSidePaneTabs,
    sidePaneState?.tabs,
    syncSidePaneCollapsedWithTabs,
    workspaceAbsPath,
  ]);

  const handleReopenClosedSidePaneTab = useCallback(
    (tabId: string) => {
      const item = allRecentClosedSidePaneTabs.find((entry) => entry.tab.id === tabId);
      if (!item || (isOfficeMode && (item.tab.type === "terminal" || item.tab.type === "git")))
        return;

      // 交互说明：最近关闭列表里的 tab 被点回打开时，需要同步展开右侧面板。
      // 否则 tab 状态已经恢复，但用户看到的还是折叠态，会误以为点击没有生效。
      revealSidePaneForCurrentOwner();
      commitSidePaneState((sidePaneCurrent) => {
        let restoredTab = item.tab;
        if (item.tab.type === "browser") {
          const {
            residency: _residency,
            residencyGeneration: _residencyGeneration,
            ...browserTab
          } = item.tab;
          restoredTab = { ...browserTab, id: `browser:${createUuid()}` };
        }
        const next = restoreSidePaneTab(sidePaneCurrent, restoredTab);
        if (restoredTab.type === "subagent-session" || restoredTab.type === "subagent-directory") {
          lastActiveSubagentTabByRootRef.current.set(restoredTab.rootSessionId, restoredTab.id);
        }
        return next;
      });
      setAllRecentClosedSidePaneTabs((current) =>
        current.filter((entry) => entry.tab.id !== tabId),
      );
      logger.info(`[App] 重新打开最近关闭右侧面板 tab=${tabId} workspace=${workspaceAbsPath}`);
    },
    [
      isOfficeMode,
      allRecentClosedSidePaneTabs,
      commitSidePaneState,
      revealSidePaneForCurrentOwner,
      workspaceAbsPath,
    ],
  );

  const handleBrowserNavigationRequestHandled = useCallback((requestId: string) => {
    setBrowserNavigationRequest((current) => (current?.id === requestId ? null : current));
  }, []);

  const handleBrowserPageMetadataChange = useCallback(
    (tabId: string, metadata: BrowserSidePaneMetadata) => {
      // 交互说明：Browser 的 title/favicon 来自 native browser state 事件，必须按 tab id 写回。
      // 多 Browser tab 共存时如果只存一份全局元数据，会导致后加载的页面覆盖其他 tab 标题。
      commitSidePaneState((current) => updateBrowserSidePaneTab(current, tabId, metadata));
    },
    [commitSidePaneState],
  );

  const recentClosedSidePaneTabs = useMemo(
    () =>
      allRecentClosedSidePaneTabs.filter(
        (item) =>
          isSidePaneTabVisibleForParent(item.tab, activeTaskId) &&
          (!isOfficeMode || (item.tab.type !== "terminal" && item.tab.type !== "git")),
      ),
    [activeTaskId, allRecentClosedSidePaneTabs, isOfficeMode],
  );

  return {
    // 办公模式只隐藏新建入口；过滤面板状态会让已打开的终端和审查在切换时消失。
    isTerminalOpen,
    setIsTerminalOpen,
    sidePaneState,
    recentClosedSidePaneTabs,
    isSidePaneCollapsed,
    setIsSidePaneCollapsed,
    isSidebarVisible,
    browserNavigationRequest,
    setBrowserNavigationRequest,
    // 回调
    handleOpenCodeViewer,
    handleOpenCodeViewers,
    handleOpenBrowserUrl,
    handleToggleBrowser,
    handleOpenBrowserTab,
    handleToggleGit,
    handleOpenGit,
    handleOpenTreemapping,
    handleOpenWhiteboard,
    handleOpenDeveloperTools,
    handleOpenTerminalTab,
    handleOpenModelTrajectory,
    handleOpenSubagentSession,
    handleOpenBackgroundBash,
    handleOpenSubagentDirectory,
    handleSyncSubagentSessionTabs,
    handleOpenSelectionSideChat,
    handleOpenPlanDetail,
    handleOpenWorkflowRun,
    handleOpenWorkflowRunDirectory,
    handleOpenWorkflowActorSession,
    handleOpenWorkflowWorkspace,
    handleOpenWorkflowArtifact,
    handleToggleTerminal,
    handleToggleSidebar,
    handleToggleSidePaneCollapse,
    handleCloseCodeViewer,
    handleCloseGit,
    handleActivateSidePaneTab,
    handleReorderSidePaneTab,
    handleCloseSidePaneTab,
    handleCloseOtherSidePaneTabs,
    handleCloseAllSidePaneTabs,
    handleReopenClosedSidePaneTab,
    handleBrowserNavigationRequestHandled,
    handleBrowserPageMetadataChange,
  };
}
