import { useEffect, useRef } from "react";
import type { IPlatformService, WorkspacePurpose } from "@zcode/shared";
import type { WindowTabState } from "@/store/tabStore.js";
import { useZCodeSessionStore } from "@/store/zcodeSessionStore.js";
import { logger } from "@/logger.js";
import { toast } from "@/components/ui/toast.js";
import { useConfirmDialog } from "@/hooks/useConfirmDialog.js";
import {
  buildTrayMenuProjection,
  isValidExitRequestedPayload,
  type TraySessionProjectionItem,
} from "@/root/traySessionProjection.js";
import { matchesPrimaryShortcut } from "@/lib/keyboardShortcuts.js";
import { isShortcutRecordingActive } from "@/shortcuts/bindings.js";
import { isRendererReloadNavigation } from "@/lib/rendererNavigation.js";

/**
 * 只处理桌面平台事件与本地窗口同步。账号、云分享和 OAuth 生命周期属于已移除的
 * 远程产品域，不能在 Root 平台副作用里保留第二套状态或隐式网络入口。
 */
export function useRootPlatformEffects({
  initialWorkspaceAbsPath,
  initialWorkspaceIdentity,
  initialWorkspacePurpose,
  initialTaskId,
  canBootstrapInitialWorkspace = true,
  addTab,
  setIsBootstrappingInitialWorkspace,
  platform,
  startDraftInWorkspace,
  startNewTaskFromActiveWorkspace,
  openWorkspace,
  openWorkspacePath,
  allowOpenWorkspace = true,
  isDesktop = false,
  locale,
  tabs,
  totalUnreadTaskCount,
  hasCompletedFullTabRestore = true,
  intl,
  traySessions = [],
  onTrayOpenSession,
  isRestoringOAuthSession: _isRestoringOAuthSession = false,
}: {
  initialWorkspaceAbsPath?: string;
  initialWorkspaceIdentity?: string;
  initialWorkspacePurpose?: WorkspacePurpose;
  initialTaskId?: string;
  canBootstrapInitialWorkspace?: boolean;
  addTab: (
    workspacePath: string,
    options?: { workspaceIdentity?: string; workspacePurpose?: WorkspacePurpose },
  ) => void;
  setIsBootstrappingInitialWorkspace: (value: boolean) => void;
  platform: IPlatformService;
  startDraftInWorkspace: (workspacePath: string, workspaceIdentity?: string) => void;
  startNewTaskFromActiveWorkspace: (source: string) => void;
  openWorkspace: () => void;
  openWorkspacePath: (workspacePath: string) => void;
  allowOpenWorkspace?: boolean;
  isDesktop?: boolean;
  locale: ReturnType<typeof import("@/i18n/IntlProvider.js").useZCodeIntl>["locale"];
  tabs: WindowTabState[];
  totalUnreadTaskCount: number;
  hasCompletedFullTabRestore?: boolean;
  intl: ReturnType<typeof import("@/i18n/IntlProvider.js").useZCodeIntl>["intl"];
  traySessions?: ReadonlyArray<TraySessionProjectionItem>;
  onTrayOpenSession?: (sessionId: string) => void;
  isRestoringOAuthSession?: boolean;
}) {
  const didBootstrapInitialWorkspaceRef = useRef(false);
  const lastTrayMenuPayloadRef = useRef<string | null>(null);
  const exitConfirmationInFlightRef = useRef(false);
  const requestConfirmation = useConfirmDialog();

  useEffect(() => {
    if (!canBootstrapInitialWorkspace || didBootstrapInitialWorkspaceRef.current) return;
    didBootstrapInitialWorkspaceRef.current = true;
    if (initialWorkspaceAbsPath) {
      addTab(
        initialWorkspaceAbsPath,
        initialWorkspaceIdentity || initialWorkspacePurpose
          ? {
              ...(initialWorkspaceIdentity ? { workspaceIdentity: initialWorkspaceIdentity } : {}),
              ...(initialWorkspacePurpose ? { workspacePurpose: initialWorkspacePurpose } : {}),
            }
          : undefined,
      );
      if (initialTaskId) {
        useZCodeSessionStore
          .getState()
          .setActiveTaskId(initialWorkspaceAbsPath, initialTaskId, initialWorkspaceIdentity);
      } else if (!isRendererReloadNavigation()) {
        startDraftInWorkspace(initialWorkspaceAbsPath, initialWorkspaceIdentity);
      }
    }
    setIsBootstrappingInitialWorkspace(false);
  }, [
    addTab,
    canBootstrapInitialWorkspace,
    initialTaskId,
    initialWorkspaceAbsPath,
    initialWorkspaceIdentity,
    initialWorkspacePurpose,
    setIsBootstrappingInitialWorkspace,
    startDraftInWorkspace,
  ]);

  useEffect(() => {
    const disposeNewTask = platform.onNewTask(() => startNewTaskFromActiveWorkspace("onNewTask"));
    const disposeExitRequested = platform.onExitRequested
      ? platform.onExitRequested((payload) => {
          if (!isValidExitRequestedPayload(payload) || exitConfirmationInFlightRef.current) {
            return;
          }

          exitConfirmationInFlightRef.current = true;
          void requestConfirmation({
            title: intl.formatMessage({ id: "confirmDialog.appExitTitle" }),
            description: intl.formatMessage(
              { id: "confirmDialog.appExitDescription" },
              { activeCount: payload.activeCount },
            ),
            confirmLabel: intl.formatMessage({ id: "confirmDialog.appExitConfirm" }),
            cancelLabel: intl.formatMessage({ id: "confirmDialog.appExitCancel" }),
            confirmVariant: "destructive",
            testId: "app-exit-confirm-dialog",
          })
            .then((confirmed) => {
              if (!confirmed) {
                return;
              }
              if (!platform.confirmExit) {
                logger.error("[Root] app_confirm_exit capability unavailable");
                return;
              }
              return platform.confirmExit().catch((error) => {
                logger.error("[Root] app_confirm_exit failed", error);
              });
            })
            .finally(() => {
              exitConfirmationInFlightRef.current = false;
            });
        })
      : () => {};
    const disposeTrayOpenSession = platform.onTrayOpenSession
      ? platform.onTrayOpenSession((payload) => {
          if (typeof payload.sessionId !== "string" || payload.sessionId.trim().length === 0) {
            logger.warn("[Root] ignored invalid tray session payload");
            return;
          }
          onTrayOpenSession?.(payload.sessionId);
        })
      : () => {};
    const disposeOpenWorkspacePath = platform.onOpenWorkspacePath
      ? platform.onOpenWorkspacePath((path) => {
          if (allowOpenWorkspace) openWorkspacePath(path);
        })
      : () => {};
    const disposeUpdateCheckResult = platform.onUpdateCheckResult
      ? platform.onUpdateCheckResult((payload) => {
          switch (payload.kind) {
            case "up-to-date":
              toast(intl.formatMessage({ id: "update.toast.upToDate" }, { version: payload.currentVersion }));
              return;
            case "downloading":
              toast(intl.formatMessage({ id: "update.toast.downloading" }, { version: payload.version }));
              return;
            case "available":
              toast(intl.formatMessage({ id: "update.toast.available" }, { version: payload.version }));
              return;
            case "already-downloading":
              toast(intl.formatMessage({ id: "update.toast.alreadyDownloading" }, { progress: payload.progress }));
              return;
            case "ready":
              toast(intl.formatMessage({ id: "update.toast.ready" }, { version: payload.version }));
              return;
            case "dev-skipped":
              toast(intl.formatMessage({ id: "update.toast.devSkipped" }));
              return;
            case "error":
              toast(intl.formatMessage({ id: "update.toast.error" }, { error: payload.message }));
              return;
          }
        })
      : () => {};
    return () => {
      disposeNewTask();
      disposeExitRequested();
      disposeTrayOpenSession();
      disposeOpenWorkspacePath();
      disposeUpdateCheckResult();
    };
  }, [
    allowOpenWorkspace,
    intl,
    openWorkspacePath,
    onTrayOpenSession,
    platform,
    requestConfirmation,
    startNewTaskFromActiveWorkspace,
  ]);

  useEffect(() => {
    if (!isDesktop || !hasCompletedFullTabRestore || !platform.setTrayMenu) {
      return;
    }

    const payload = buildTrayMenuProjection(locale, traySessions);
    const serializedPayload = JSON.stringify(payload);
    if (lastTrayMenuPayloadRef.current === serializedPayload) {
      return;
    }
    lastTrayMenuPayloadRef.current = serializedPayload;
    void platform.setTrayMenu(payload).catch((error) => {
      logger.error("[Root] tray_set_menu failed", error);
    });
  }, [hasCompletedFullTabRestore, isDesktop, locale, platform, traySessions]);

  useEffect(() => {
    if (isDesktop) return;
    const handleWindowKeydown = (event: KeyboardEvent) => {
      if (isShortcutRecordingActive()) return;
      const isNewTaskShortcut = matchesPrimaryShortcut(event, "n");
      const isOpenWorkspaceShortcut = matchesPrimaryShortcut(event, "o");
      if (!isNewTaskShortcut && !isOpenWorkspaceShortcut) return;
      event.preventDefault();
      if (isOpenWorkspaceShortcut) openWorkspace();
      else startNewTaskFromActiveWorkspace("web CmdOrCtrl+N");
    };
    window.addEventListener("keydown", handleWindowKeydown, true);
    return () => window.removeEventListener("keydown", handleWindowKeydown, true);
  }, [isDesktop, openWorkspace, startNewTaskFromActiveWorkspace]);

  useEffect(() => {
    if (!isDesktop) return;
    let disposed = false;
    platform.setApplicationLocale(locale).catch((error) => {
      if (!disposed) logger.error("[Root] application locale sync failed", { locale, error });
    });
    return () => {
      disposed = true;
    };
  }, [isDesktop, locale, platform]);

  useEffect(() => {
    platform.syncWindowUnreadCount(totalUnreadTaskCount);
  }, [platform, totalUnreadTaskCount]);

}
