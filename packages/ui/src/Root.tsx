/* eslint-disable max-lines -- Root 当前集中编排启动和 workspace shell wiring，先保持入口收口避免跨层状态拆散。 */
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { LucideProvider, RefreshCw } from "lucide-react";
import {
  APP_RUNTIME_PREFERENCES_CHANGED_BROADCAST_CHANNEL,
  DesktopCommandIds,
  appRuntimePreferencesChangedBroadcastPayloadSchema,
} from "@zcode/shared";
import { TooltipProvider } from "@/components/ui/tooltip.js";
import { Button } from "@/components/ui/button.js";
import { PlatformProvider } from "@/hooks/usePlatform.js";
import { ServiceProvider } from "@/hooks/useServices.js";
import { DirectoryBrowser } from "@/DirectoryBrowser.js";
import { useTabPersistence } from "@/hooks/useTabPersistence.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { SettingsPage } from "@/SettingsPage.js";
import { setDefaultFileDisplayBasePath } from "@/lib/fileDisplay.js";
import { readRendererLaunchTimings, shouldReportLaunchToInput } from "@/lib/launchToInputReport.js";
import { reportUiLaunchToInput } from "@/lib/uiPerfArmsTelemetry.js";
import { countAllUnreadTasks } from "@/lib/unreadTaskCount.js";
import {
  isProviderStartupSyncPending,
  shouldBlockRootRender,
  shouldShowRootStartupLoading,
  shouldOpenFallbackWorkspaceAfterCreate,
} from "@/lib/rootStartupGate.js";
import { StoreProvider, useZCodeStore } from "@/store/StoreProvider.js";
import { setMcpStorePlatform } from "@/store/mcpStore.js";
import { useZCodeSessionStore } from "@/store/zcodeSessionStore.js";
import { TabStoreProvider, useTabStore, useTabStoreApi } from "@/store/TabStoreProvider.js";
import { isSettingsTab, isWorkspaceTab, type WorkspaceTabState } from "@/store/tabStore.js";
import { logger } from "@/logger.js";
import { RootShell } from "@/root/RootShell.js";
import { RootWorkspaceContent } from "@/root/RootWorkspaceContent.js";
import { OccupationOnboarding } from "@/onboarding/OccupationOnboarding.js";
import { OnboardingDialog } from "@/onboarding/OnboardingDialog.js";
import { useModelSelectionServiceView } from "@/hooks/useModelSelectionView.js";
import { useRootProviderSettingsSnapshot } from "@/root/useRootProviderSettingsSnapshot.js";
import { useDesktopNativeThemeSync } from "@/root/useDesktopNativeThemeSync.js";
import { useRootPlatformEffects } from "@/root/useRootPlatformEffects.js";
import { useRootWorkspaceActions } from "@/root/useRootWorkspaceActions.js";
import type { RootProps } from "@/root/types.js";
import type { TraySessionOpenRequest } from "@/app-shell/types.js";
import { findTraySession } from "@/root/traySessionProjection.js";
import { useGlobalTaskList } from "@/hooks/useGlobalTaskList.js";
import { ScopedErrorBoundary } from "@/ErrorBoundary.js";
import {
  CODE_COMMENT_REMOVE_BROADCAST_CHANNEL,
  CODE_COMMENT_PREVIEW_RESTORE_BROADCAST_CHANNEL,
  isCodeCommentPayload,
  isCodeCommentRemovePayload,
  markCodeCommentRemoved,
} from "@/lib/codeCommentContext.js";
import { useCodeCommentPreviewStore } from "@/store/codeCommentPreviewStore.js";
import { setUiPerfArmsReporter } from "@/lib/uiPerfArmsTelemetry.js";
import { setSessionOpenArmsReporter } from "@/lib/sessionOpenArmsTelemetry.js";
import { setSendFunnelArmsReporter } from "@/lib/sendFunnelArmsTelemetry.js";
import { RootStartupLoading } from "@/root/RootStartupLoading.js";
import { useSettings } from "@/hooks/useSettingService.js";
import { CLOSE_ACTIVE_CONTEXT_REQUEST_EVENT } from "@/lib/closeActiveContext.js";
import { AssistantCodeCommentFeatureProvider } from "@/AssistantCodeCommentFeatureProvider.js";
import {
  disposeConversationTelemetrySupervisors,
  reconcileConversationTelemetryWorkspaceScopes,
} from "@/v4/telemetry/ConversationTelemetryAttachment.js";

const DEFAULT_LUCIDE_STROKE_WIDTH = 1.5;
/**
 * Root —— 应用根组件
 *
 * 外层挂载 StoreProvider（连接广播服务）+ TabStoreProvider，内层处理认证和路由。
 */
export function Root(props: RootProps) {
  return (
    <LucideProvider strokeWidth={DEFAULT_LUCIDE_STROKE_WIDTH}>
      {/*
       * 之前通过 lucide.tsx 包装每个图标，把默认 strokeWidth 固定成 1.5。
       * 现在移除包装文件后，如果不在根层统一注入，按钮、列表和工具栏里的 Lucide 图标会回退到 2，
       * 导致同一套 size class 下视觉显得更粗、更挤。这里改用官方 LucideProvider 保持默认值，
       * 同时保留个别图标显式传入 strokeWidth 时的覆盖能力。
       */}
      <TooltipProvider>
        {/*
         * 大会话消息动作里会出现大量 tooltip。Provider 如果跟随每个 tooltip 实例创建，
         * React 点击切换任务时会同步构造数量级相同的 Radix 上下文树；根层共享一次即可保留零延迟配置。
         */}
        <ServiceProvider services={props.services}>
          <PlatformProvider platform={props.platform}>
            <StoreProvider
              broadcastService={props.services.broadcastService}
            >
              <TabStoreProvider>
                <AssistantCodeCommentFeatureProvider
                  enabled={props.assistantCodeCommentCardsEnabled}
                >
                  <RootInner {...props} />
                </AssistantCodeCommentFeatureProvider>
              </TabStoreProvider>
            </StoreProvider>
          </PlatformProvider>
        </ServiceProvider>
      </TooltipProvider>
    </LucideProvider>
  );
}

function RootInner({
  services,
  platform,
  initialWorkspaceAbsPath,
  unavailableWorkspacePath,
  initialWorkspaceIdentity,
  initialWorkspacePurpose,
  initialTaskId,
  isDesktop,
  isMacDesktop,
  isWindowsDesktop,
  restoreSession = true,
  supportsSettings = true,
  allowOpenWorkspace = true,
  preferDirectoryBrowser,
  supportsEmbeddedBrowser: explicitSupportsEmbeddedBrowser,
  initialWorkspaceLoadingFallback,
}: RootProps) {
  useEffect(() => {
    setMcpStorePlatform(platform);
    // 对话 UI perf 只属于 desktop-continuous；Web/mobile 即使能看到权威状态也不装 reporter。
    setUiPerfArmsReporter(isDesktop ? platform : null);
    setSessionOpenArmsReporter(isDesktop ? platform : null);
    // 发送漏斗同理：只在 Electron 桌面端上报，Web/mobile 的 reportArmsCustomEvent 是空实现。
    setSendFunnelArmsReporter(isDesktop ? platform : null);
    return () => {
      setMcpStorePlatform(null);
      setUiPerfArmsReporter(null);
      setSessionOpenArmsReporter(null);
      setSendFunnelArmsReporter(null);
    };
  }, [isDesktop, platform]);

  useEffect(
    () => () => {
      disposeConversationTelemetrySupervisors();
    },
    [],
  );

  const { intl, locale } = useZCodeIntl();
  const theme = useZCodeStore((state) => state.theme);
  const {
    settings: appSettings,
    refresh: refreshAppSettings,
  } = useSettings();
  const rootModelSelectionRead = useModelSelectionServiceView(services.modelSelectionService);
  const rootModelSelectionErrorNode =
    rootModelSelectionRead.state.status === "error" ? (
      <div className="fixed right-4 bottom-4 z-50 flex max-w-sm items-center gap-3 rounded-lg border border-destructive/30 bg-surface-raised px-3 py-2 text-ui-base text-foreground shadow-lg">
        <span className="min-w-0 flex-1">
          {intl.formatMessage({ id: "root.modelSelection.loadFailed" })}
        </span>
        <Button type="button" variant="ghost" size="sm" onClick={rootModelSelectionRead.reload}>
          <RefreshCw className="size-3.5" aria-hidden="true" />
          {intl.formatMessage({ id: "common.retry" })}
        </Button>
      </div>
    ) : null;
  const [directoryBrowserOpen, setDirectoryBrowserOpen] = useState(false);
  const [isCreatingFallbackWorkspace, setIsCreatingFallbackWorkspace] = useState(false);
  const [isBootstrappingInitialWorkspace, setIsBootstrappingInitialWorkspace] = useState(
    Boolean(initialWorkspaceAbsPath),
  );
  const didRequestFallbackWorkspaceRef = useRef(false);
  const rootInnerMountedRef = useRef(true);
  const [hasEnteredNativeThemeSyncSurface, setHasEnteredNativeThemeSyncSurface] = useState(false);

  useEffect(() => {
    return () => {
      rootInnerMountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const disposable = services.broadcastService.onMessage((message) => {
      if (message.channel === APP_RUNTIME_PREFERENCES_CHANGED_BROADCAST_CHANNEL) {
        const parsed = appRuntimePreferencesChangedBroadcastPayloadSchema.safeParse(
          message.payload,
        );
        if (!parsed.success) {
          logger.warn("[settings] 丢弃无效的运行时偏好广播", {
            issues: parsed.error.issues,
          });
          return;
        }
        void refreshAppSettings();
        void services.zcodeAgentService.syncAppRuntimePreferences(parsed.data).catch((error) => {
          logger.warn("[settings] 同步跨窗口运行时偏好失败", error);
        });
        return;
      }

      if (message.channel === CODE_COMMENT_PREVIEW_RESTORE_BROADCAST_CHANNEL) {
        if (!isCodeCommentPayload(message.payload) || !message.payload.id) {
          return;
        }
        useCodeCommentPreviewStore.getState().restoreCommentFromAttachment({
          ...message.payload,
          id: message.payload.id,
        });
        return;
      }

      if (message.channel !== CODE_COMMENT_REMOVE_BROADCAST_CHANNEL) {
        return;
      }

      if (!isCodeCommentRemovePayload(message.payload)) {
        return;
      }

      // 关闭文件 tab 后 PreviewPane 会卸载，不能再依赖 PreviewPane 自己监听跨窗口清理事件。
      // 这里在 Root 生命周期内同步清理 renderer 级 preview store，避免发送后重新打开文件又恢复旧 comment。
      markCodeCommentRemoved(message.payload);
      useCodeCommentPreviewStore.getState().removeCommentBySource(message.payload);
    });

    return () => {
      disposable.dispose();
    };
  }, [
    refreshAppSettings,
    services.broadcastService,
    services.zcodeAgentService,
  ]);

  useEffect(() => {
    if (!appSettings) {
      return;
    }
    void services.zcodeAgentService
      .syncAppRuntimePreferences({
        askUserQuestionAutoResolutionEnabled:
          appSettings.askUserQuestionAutoResolutionEnabled !== false,
      })
      .catch((error) => {
        logger.warn("[settings] 初始化运行时偏好失败", error);
      });
  }, [
    appSettings?.askUserQuestionAutoResolutionEnabled,
    services.zcodeAgentService,
  ]);

  const tabs = useTabStore((state) => state.tabs);
  const windowWorkspaceTabs = useMemo(() => tabs.filter(isWorkspaceTab), [tabs]);
  const activeTabId = useTabStore((state) => state.activeTabId);
  const activeWorkspacePath = useTabStore((state) => state.activeWorkspacePath);
  const activeWorkspaceIdentity = useTabStore((state) => state.activeWorkspaceIdentity);
  const activeTab = activeTabId ? (tabs.find((tab) => tab.id === activeTabId) ?? null) : null;
  const activeWorkspaceTab = activeTab && isWorkspaceTab(activeTab) ? activeTab : null;
  const isSettingsTabActive = activeTab ? isSettingsTab(activeTab) : false;
  const workspaceShellPath = activeWorkspaceTab?.workspacePath ?? activeWorkspacePath;
  const workspaceShellIdentity =
    activeWorkspaceTab?.workspaceIdentity ?? activeWorkspaceIdentity ?? undefined;
  const workspaceScopedServices = services;
  const totalUnreadTaskCount = useZCodeSessionStore((state) =>
    countAllUnreadTasks(state.workspaces),
  );
  const addTab = useTabStore((state) => state.addTab);
  const tabStoreApi = useTabStoreApi();
  const trayTaskList = useGlobalTaskList({
    kind: "timeline",
    // 托盘菜单复用窗口 Controller 的任务左表；Web 端没有原生托盘，避免额外查询。
    workspaceTabs: isDesktop ? windowWorkspaceTabs : [],
    sortBy: "updated",
    searchQuery: "",
    expanded: false,
    collapsedLimit: 12,
  });
  const [pendingTraySessionOpen, setPendingTraySessionOpen] =
    useState<TraySessionOpenRequest | null>(null);
  const traySessionRequestIdRef = useRef(0);
  const handleTrayOpenSession = useCallback(
    (sessionId: string) => {
      const task = findTraySession(trayTaskList.items, sessionId);
      if (!task) {
        logger.warn(`[Root] 托盘会话已不在当前任务投影中，忽略打开请求 session=${sessionId}`);
        return;
      }

      const workspaceIdentity = task.workspaceIdentity?.trim() || undefined;
      const activated = tabStoreApi.getState().activateTabByPath(
        task.workspacePath,
        workspaceIdentity ? { workspaceIdentity } : undefined,
      );
      if (!activated) {
        tabStoreApi.getState().addTab(
          task.workspacePath,
          workspaceIdentity ? { workspaceIdentity } : undefined,
        );
      }

      traySessionRequestIdRef.current += 1;
      setPendingTraySessionOpen({
        requestId: traySessionRequestIdRef.current,
        taskId: task.taskId,
        workspacePath: task.workspacePath,
        ...(workspaceIdentity ? { workspaceIdentity } : {}),
        ...(typeof task.unreadAt === "number" ? { expectedUnreadAt: task.unreadAt } : {}),
      });
    },
    [tabStoreApi, trayTaskList.items],
  );
  const handleTraySessionOpenConsumed = useCallback((requestId: number) => {
    setPendingTraySessionOpen((request) =>
      request?.requestId === requestId ? null : request,
    );
  }, []);
  useRootProviderSettingsSnapshot(services);

  const shouldPreferDirectoryBrowser = Boolean(preferDirectoryBrowser);
  const supportsEmbeddedBrowser = explicitSupportsEmbeddedBrowser ?? Boolean(isDesktop);
  // 本地模型目录由 KeenCode Rust 提供，不等待账户 OAuth 或云端 provider 门禁。
  const canRestoreWorkspaceSession = true;

  const handleOpenDirectoryBrowser = useCallback(() => {
    setDirectoryBrowserOpen(true);
  }, []);
  const {
    setWorkspaceActionError,
    startDraftInWorkspace,
    startNewTaskFromActiveWorkspace,
    handleSelectProject,
    handleSelectConversationWorkspace,
    handleResolveConversationWorkspace,
    handleCreateConversationTask,
    handleOpenWorkspace,
    handleOpenFolderFromWorkspaceMenu,
    handleCreateScratchWorkspace,
    handleCreateTask,
    handleBackFromSettings,
  } = useRootWorkspaceActions({
    intl,
    platform,
    services,
    tabStoreApi,
    addTab,
    activeWorkspacePath,
    activeWorkspaceIdentity,
    supportsSettings,
    allowOpenWorkspace,
    preferDirectoryBrowser: shouldPreferDirectoryBrowser,
    openDirectoryBrowser: handleOpenDirectoryBrowser,
  });

  useEffect(() => {
    // fileDisplay 默认不传 basePath 时需要落到“当前激活 workspace”。
    // 之前纯工具层拿不到窗口内的 workspace 上下文，只能退回绝对路径，导致 mention / 文件展示在输入框里不够简洁。
    // 这里由 Root 在 workspace 切换时同步一份当前上下文，既保留工具层复用性，也不把 Zustand 依赖硬塞进工具函数。
    setDefaultFileDisplayBasePath(activeWorkspacePath);
  }, [activeWorkspacePath]);

  useEffect(() => {
    if (!activeWorkspacePath) {
      return;
    }

    setWorkspaceActionError(null);
  }, [activeWorkspacePath, setWorkspaceActionError]);

  const { isRestoring, hasCompletedInitialRestore, hasCompletedFullRestore } = useTabPersistence({
    settingService: supportsSettings ? services.settingService : undefined,
    // 首次安装未连接账号时，provider 登录入口判定会晚于 workspace 恢复。
    // 如果这里先恢复 workspace，ChatView mount 会触发草稿 session 预热并在登录页背后报错。
    restoreSession: restoreSession && canRestoreWorkspaceSession,
    persistSession: restoreSession && canRestoreWorkspaceSession,
  });

  useEffect(() => {
    if (!isDesktop || !hasCompletedFullRestore) return;
    // Bug 原因：active-first 的单 workspace 只是 Renderer 首屏投影，若立刻对外同步，
    // 会短暂撤销其他 workspace 的 telemetry scope。完整补齐后才能发布全量集合。
    reconcileConversationTelemetryWorkspaceScopes(
      windowWorkspaceTabs.map((tab) => ({
        workspacePath: tab.workspacePath,
        ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
      })),
    );
  }, [hasCompletedFullRestore, isDesktop, windowWorkspaceTabs]);

  // 启动阻塞是桌面窗口保护期，手机 Web 远控在进入 Root 前已有配对/加载页。
  // Web 端继续使用该 gate 会在 workspace tab 注入前渲染空 RootShell，露出浏览器白底。
  const isStartupRenderBlocked = shouldShowRootStartupLoading({
    isDesktop,
    welcomeScreenOpen: false,
    isResolvingStartupAuthState: false,
    isResolvingProviderStartupState: false,
    isRestoring,
    isBootstrappingInitialWorkspace: isBootstrappingInitialWorkspace || isCreatingFallbackWorkspace,
  });

  const launchReportedRef = useRef(false);
  useEffect(() => {
    if (
      !shouldReportLaunchToInput({
        isStartupRenderBlocked,
        welcomeScreenOpen: false,
        alreadyReported: launchReportedRef.current,
      })
    ) {
      return;
    }
    launchReportedRef.current = true;
    const timings = readRendererLaunchTimings();
    if (!timings || !timings.marks) {
      return; // 锚点缺失(非桌面/未注入 marks),整批跳过
    }
    reportUiLaunchToInput({
      marks: timings.marks,
      rendererStart: timings.rendererStart,
      reactCommit: timings.reactCommit,
      inputReady: Date.now(), // T6
      sessionId: `launch-${timings.marks.createdAt}`,
    });
  }, [isStartupRenderBlocked]);

  useRootPlatformEffects({
    initialWorkspaceAbsPath,
    initialWorkspaceIdentity,
    initialWorkspacePurpose,
    initialTaskId,
    // 系统右键/Service 冷启动传入 initialWorkspacePath 时，必须先恢复历史 tabs，
    // 再把目标 workspace 合并并激活。否则先 addTab 会被 restoreTabs 整体替换掉；
    // 直接禁用 restoreSession 又会让其他 workspace 全部消失。
    canBootstrapInitialWorkspace: canRestoreWorkspaceSession && hasCompletedInitialRestore,
    addTab,
    setIsBootstrappingInitialWorkspace,
    platform,
    startDraftInWorkspace,
    startNewTaskFromActiveWorkspace,
    openWorkspace: handleOpenWorkspace,
    openWorkspacePath: (path) => {
      void handleSelectProject(path);
    },
    allowOpenWorkspace,
    isDesktop,
    locale,
    tabs,
    totalUnreadTaskCount,
    hasCompletedFullTabRestore: hasCompletedFullRestore,
    intl,
    traySessions: trayTaskList.items,
    onTrayOpenSession: handleTrayOpenSession,
    isRestoringOAuthSession: false,
  });

  useEffect(() => {
    if (!platform.onCloseActiveContextRequest) {
      return;
    }

    return platform.onCloseActiveContextRequest(() => {
      const event = new Event(CLOSE_ACTIVE_CONTEXT_REQUEST_EVENT, { cancelable: true });
      window.dispatchEvent(event);
      if (event.defaultPrevented) {
        return;
      }

      // 关闭请求先广播给 workspace 层判断 side pane active tab。
      // 没有可见 workspace 或没有可关闭的 side pane tab 时，才回落到关窗口语义。
      void platform.executeDesktopCommand(DesktopCommandIds.CloseWindow);
    });
  }, [platform]);

  const canEnterNativeThemeSyncSurface = Boolean(
    !isStartupRenderBlocked &&
    (workspaceShellPath || isSettingsTabActive),
  );

  useEffect(() => {
    if (!canEnterNativeThemeSyncSurface) {
      return;
    }

    // macOS nativeTheme 会影响窗口 vibrancy。这里等 RootStartupLoading
    // 真正退出并进入主界面/设置页后一轮再允许同步，避免启动壳背景被应用主题提前改写。
    setHasEnteredNativeThemeSyncSurface(true);
  }, [canEnterNativeThemeSyncSurface]);

  useDesktopNativeThemeSync({
    enabled: hasEnteredNativeThemeSyncSurface,
    isDesktop,
    platform,
    theme,
  });

  useEffect(() => {
    if (
      shouldBlockRootRender({
        isResolvingStartupAuthState: false,
        isResolvingProviderStartupState: false,
        isRestoring,
        isBootstrappingInitialWorkspace,
      }) ||
      workspaceShellPath ||
      isSettingsTabActive ||
      !allowOpenWorkspace ||
      didRequestFallbackWorkspaceRef.current
    ) {
      return;
    }

    didRequestFallbackWorkspaceRef.current = true;
    setIsCreatingFallbackWorkspace(true);
    // 以前 tab store 的默认空态会把 Root 带到打开工作区中间页。
    // 删除整页流程后，启动恢复为空或入口没有传 initialWorkspacePath 时必须在 Root
    // 兜底落到默认 workspace，避免用户先看到一张“打开工作区”中间页或空白页。
    // 这里不能用当前 effect 的 cleanup 作为异步取消标记：setIsCreatingFallbackWorkspace
    // 或 addTab 后的 workspaceShellPath 变化都会让 React 先跑 cleanup，若因此跳过 finally，
    // 启动 loading gate 会永远保持 true。
    services.fileService
      .ensureConversationWorkspace()
      .then((result) => {
        // 启动兜底创建目录可能早于会话恢复发起、晚于恢复完成返回。
        // 返回后必须按 tab store 最新 active workspace 再判一次，避免迟到的默认项目抢走上次恢复的 tab。
        if (
          !shouldOpenFallbackWorkspaceAfterCreate({
            isMounted: rootInnerMountedRef.current,
            activeWorkspacePath: tabStoreApi.getState().activeWorkspacePath,
          })
        ) {
          return;
        }
        handleSelectConversationWorkspace(result.path);
      })
      .catch((error) => {
        if (!rootInnerMountedRef.current) {
          return;
        }
        logger.error("[Root] 启动兜底创建默认 workspace 失败", { error });
        setWorkspaceActionError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => {
        if (rootInnerMountedRef.current) {
          setIsCreatingFallbackWorkspace(false);
        }
      });
  }, [
    allowOpenWorkspace,
    handleSelectConversationWorkspace,
    isBootstrappingInitialWorkspace,
    isRestoring,
    isSettingsTabActive,
    services.fileService,
    setWorkspaceActionError,
    tabStoreApi,
    workspaceShellPath,
  ]);

  useEffect(() => {
    if (!workspaceShellPath) {
      return;
    }

    logger.info(
      `[Root] settings view ${isSettingsTabActive ? "open" : "closed"} workspace=${workspaceShellPath}`,
    );
  }, [isSettingsTabActive, workspaceShellPath]);

  const directoryBrowserDialog = directoryBrowserOpen ? (
    <ScopedErrorBoundary
      scope="directory-browser"
      resetKeys={["directory-browser"]}
      variant="silent"
    >
      <DirectoryBrowser
        services={services}
        onCancel={() => setDirectoryBrowserOpen(false)}
        onSelect={(path) => {
          setDirectoryBrowserOpen(false);
          void handleSelectProject(path);
        }}
      />
    </ScopedErrorBoundary>
  ) : null;

  const settingsLayerProps = {
    isDesktop,
    isMacDesktop,
    isWindowsDesktop,
    captionWorkspacePath: activeWorkspacePath,
    onBack: activeWorkspacePath ? handleBackFromSettings : undefined,
    onCreateTask: handleCreateTask,
    onOpenWorkspace: handleOpenWorkspace,
    allowOpenWorkspace,
  };

  if (isStartupRenderBlocked) {
    const loadingLabel = intl.formatMessage({ id: "common.loading" });
    return (
      <RootShell>
        {rootModelSelectionErrorNode}
        {directoryBrowserDialog}
        {/* HTML 启动壳已经展示 ZCode SVG，但 React 接管 root 后旧壳会被整棵替换。
            之前阻塞恢复 tab / 初始 workspace 注入时重新渲染纯文字“加载中...”，所以启动被拆成两套 loading。
            这里复用同一套 SVG 启动画面，只把文案保留到 aria-label，保证视觉始终连续且不牺牲可访问性。 */}
        <RootStartupLoading label={loadingLabel} />
      </RootShell>
    );
  }

  if (
    !workspaceShellPath &&
    !isDesktop &&
    initialWorkspaceAbsPath &&
    initialWorkspaceLoadingFallback
  ) {
    // 非桌面入口的 workspace tab 由 effect 注入，首帧不能返回 null。
    // 这里延续入口 loading，等任务列表有 workspaceShellPath 后再切换，避免露出浏览器白底。
    return (
      <RootShell>
        {rootModelSelectionErrorNode}
        {initialWorkspaceLoadingFallback}
        {directoryBrowserDialog}
      </RootShell>
    );
  }

  return (
    <RootShell>
      {rootModelSelectionErrorNode}
      {directoryBrowserDialog}
      <OccupationOnboarding
        showWindowControls={Boolean(isWindowsDesktop || (isDesktop && !isMacDesktop))}
        showChildrenWhileLoading={!workspaceShellPath && isSettingsTabActive}
        isMacDesktop={isMacDesktop}
        isWindowsDesktop={isWindowsDesktop}
      >
        {/* 新引导属于应用级偏好；无项目时也要挂载，才能响应设置页的手动打开请求。 */}
        {!workspaceShellPath ? (
          isSettingsTabActive ? (
            <ScopedErrorBoundary
              scope="settings-page"
              resetKeys={["settings-root"]}
              variant="panel"
              className="h-full"
            >
              <SettingsPage {...settingsLayerProps} />
            </ScopedErrorBoundary>
          ) : null
        ) : (
          <RootWorkspaceContent
            workspaceScopedServices={workspaceScopedServices}
            workspaceShellPath={workspaceShellPath}
            workspaceIdentity={workspaceShellIdentity}
            activeWorkspacePath={activeWorkspacePath}
            isSettingsTabActive={isSettingsTabActive}
            handleCreateTask={handleCreateTask}
            handleCreateConversationTask={handleCreateConversationTask}
            handleResolveConversationWorkspace={handleResolveConversationWorkspace}
            handleOpenWorkspace={handleOpenWorkspace}
            handleOpenWorkspacePath={handleSelectProject}
            handleOpenFolderFromWorkspaceMenu={handleOpenFolderFromWorkspaceMenu}
            handleCreateScratchWorkspace={handleCreateScratchWorkspace}
            handleBackFromSettings={handleBackFromSettings}
            allowOpenWorkspace={allowOpenWorkspace}
            isDesktop={isDesktop}
            isMacDesktop={isMacDesktop}
            isWindowsDesktop={isWindowsDesktop}
            supportsEmbeddedBrowser={supportsEmbeddedBrowser}
            traySessionOpenRequest={pendingTraySessionOpen}
            onTraySessionOpenConsumed={handleTraySessionOpenConsumed}
          />
        )}
        <ScopedErrorBoundary
          scope="onboarding-dialog"
          resetKeys={[workspaceShellIdentity?.trim() || workspaceShellPath]}
          variant="silent"
        >
          <OnboardingDialog
            workspacePath={workspaceShellPath || undefined}
            workspaceIdentity={workspaceShellIdentity}
            isDesktop={isDesktop}
          />
        </ScopedErrorBoundary>
      </OccupationOnboarding>
    </RootShell>
  );
}
