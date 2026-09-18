import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import type {
  Project,
  SessionContextUsage,
  SessionRow,
} from "@/features/app/models";
import { formatSessionModelReference, type ModelOption } from "@/lib/modelCatalog";
import type { Attachment } from "@/lib/attachments";
import {
  IDLE_SNAPSHOT,
  isSessionLiveStreaming,
  localizeUiError,
  snapshotOutgoingMessages,
  type AskUserPayload,
  type ChatMessage,
  type SessionSnapshot,
} from "@/lib/session";
import {
  createOperationId,
  diagnosticsRecord,
  type SessionSnapshot as AcpSessionSnapshot,
} from "@/lib/acp/api";
import { ensureAcpSession } from "@/lib/acp/projection";
import type { AcpWorkspaceState } from "@/lib/acp/store";
import { projectAcpSnapshot } from "@/lib/sessionProjection";
import {
  projectHostIntoLiveMap,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import { isProjectPathMissing } from "@/lib/projectPath";
import {
  saveUnreadTerminalResults,
  type UnreadTerminalResult,
} from "@/lib/sessionCompletion";
import {
  restoreDraftNavigation,
  snapshotDraftNavigation,
  type DraftNavigationLocation,
  type DraftNavigationSnapshot,
} from "@/lib/draftNavigation";
import { shouldAdoptView, type ViewFocus } from "@/lib/viewFocus";

type StateSetter<T> = Dispatch<SetStateAction<T>>;
type Ref<T> = MutableRefObject<T>;

export type SessionNavigationOpenSession = (
  session: SessionRow,
  project?: Project | null,
) => Promise<void>;

export type SessionNavigationNewChat = (
  project?: Project | null,
  options?: { seedDraft?: string },
) => Promise<void>;

export interface SessionNavigationRoutePort {
  navigateWorkbench: () => void;
}

/** ACP calls and projections needed while changing the viewed Session. */
export interface SessionNavigationAcpRuntimePort {
  isTauri: () => boolean;
  workspaceRef: Ref<AcpWorkspaceState>;
  commitWorkspace: () => void;
  connect: (args: {
    projectPath?: string;
    sessionId?: string | null;
    /** 本次连接操作的稳定标识。 */
    operationId: string;
  }) => Promise<AcpSessionSnapshot>;
  observeHostActiveTurn: (snapshot: {
    sessionId?: string | null;
    activeTurnId?: string | null;
  }) => void;
  replayHistory: (sessionId: string, originView?: ViewFocus) => Promise<void>;
  applyViewProjection: (sessionId: string | null) => void;
  refreshSessions: (projectId?: string) => Promise<void>;
  liveHostRef: Ref<SessionSnapshot>;
  messagesRef: Ref<ChatMessage[]>;
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
}

/** Sidebar state that is intentionally changed as part of navigation. */
export interface SessionNavigationSidebarPort {
  projects: Project[];
  activeProject: Project | null;
  setActiveProject: StateSetter<Project | null>;
  setExpandedProjects: StateSetter<Record<string, boolean>>;
  setHistoryOpen: StateSetter<boolean>;
  setUnreadTerminalResults: StateSetter<Map<string, UnreadTerminalResult>>;
  pendingAskUserBySessionRef: Ref<Map<string, AskUserPayload>>;
}

/** Composer state touched by draft transitions and the explicit focus request. */
export interface SessionNavigationComposerPort {
  draftRef: Ref<string>;
  attachmentsRef: Ref<Attachment[]>;
  setDraft: StateSetter<string>;
  setAttachments: StateSetter<Attachment[]>;
  requestComposerFocus: () => void;
  sendQueue: { clearDraftQueue: () => void };
}

/** Provider state needed to restore a per-Session model after opening it. */
export interface SessionNavigationProviderModelsPort {
  modelBySessionRef: Ref<Map<string, string>>;
  configuredModelsRef: Ref<ModelOption[]>;
  /** 接收 `providerId::modelId` 引用，保留会话实际供应商。 */
  setSessionModelReference: StateSetter<string>;
}

export interface SessionNavigationUiPort {
  session: SessionSnapshot;
  setSession: StateSetter<SessionSnapshot>;
  setMessages: StateSetter<ChatMessage[]>;
  setLiveHost: StateSetter<SessionSnapshot>;
  setLiveMap: StateSetter<SessionLiveMap>;
  setContextUsage: StateSetter<SessionContextUsage | null>;
  setAskUser: StateSetter<AskUserPayload | null>;
  setRetryStatus: StateSetter<{
    attempt: number;
    maxAttempts: number;
    delayMs: number;
    reason: string;
  } | null>;
  setLocalError: StateSetter<string | null>;
  closeSummary: () => void;
}

export interface UseSessionNavigationOptions {
  locale: Locale;
  navigationRefs: SessionNavigationRefs;
  route: SessionNavigationRoutePort;
  runtime: SessionNavigationAcpRuntimePort;
  sidebar: SessionNavigationSidebarPort;
  composer: SessionNavigationComposerPort;
  providers: SessionNavigationProviderModelsPort;
  ui: SessionNavigationUiPort;
}

export interface SessionNavigationRefs {
  draftKeyRef: Ref<number>;
  draftNavigationSnapshotRef: Ref<DraftNavigationSnapshot | null>;
  viewEpochRef: Ref<number>;
  viewingSessionIdRef: Ref<string | null>;
  openingSessionIdRef: Ref<string | null>;
  openingSessionEpochRef: Ref<number | null>;
}

export interface UseSessionNavigationResult extends SessionNavigationRefs {
  currentViewFocus: () => ViewFocus;
  draftNavigationLocation: () => DraftNavigationLocation;
  bumpViewEpoch: () => void;
  openSession: SessionNavigationOpenSession;
  newChat: SessionNavigationNewChat;
}

/** 管理任务导航、草稿隔离、Session 打开和新草稿切换。 */
export function useSessionNavigation({
  locale,
  navigationRefs,
  route,
  runtime,
  sidebar,
  composer,
  providers,
  ui,
}: UseSessionNavigationOptions): UseSessionNavigationResult {
  const tr = useMemo(() => createT(locale), [locale]);
  const portsRef = useRef({
    route,
    runtime,
    sidebar,
    composer,
    providers,
    ui,
  });
  portsRef.current = { route, runtime, sidebar, composer, providers, ui };
  /** 会话草稿只在当前桌面生命周期保存；空草稿不占缓存，不与新对话草稿混用。 */
  const sessionDraftsRef = useRef(new Map<string, { text: string; attachments: Attachment[] }>());
  const navigationTimingRef = useRef<{
    sessionId: string; epoch: number; started: number; firstCommit?: number;
  } | null>(null);

  const {
    draftKeyRef,
    draftNavigationSnapshotRef,
    viewEpochRef,
    viewingSessionIdRef,
    openingSessionIdRef,
    openingSessionEpochRef,
  } = navigationRefs;

  const currentViewFocus = useCallback(
    (): ViewFocus => ({
      sessionId: viewingSessionIdRef.current,
      epoch: viewEpochRef.current,
    }),
    [],
  );

  useLayoutEffect(() => {
    const timing = navigationTimingRef.current;
    if (!timing || timing.sessionId !== ui.session.sessionId ||
        timing.epoch !== viewEpochRef.current || timing.firstCommit !== undefined) return;
    timing.firstCommit = performance.now();
    if (typeof requestAnimationFrame !== "function") return;
    requestAnimationFrame(() => requestAnimationFrame(() => {
      if (navigationTimingRef.current !== timing) return;
      void diagnosticsRecord("session_navigation", JSON.stringify({
        sessionId: timing.sessionId, phase: "first_frame",
        commitMs: Math.round(timing.firstCommit! - timing.started),
        frameMs: Math.round(performance.now() - timing.started),
        frameAfterCommitMs: Math.round(performance.now() - timing.firstCommit!),
      })).catch(() => {});
    }));
  }, [ui.session.sessionId, viewEpochRef]);

  const draftNavigationLocation = useCallback(
    (): DraftNavigationLocation => ({
      sessionId: viewingSessionIdRef.current,
      draftKey: draftKeyRef.current,
      viewEpoch: viewEpochRef.current,
    }),
    [],
  );

  const bumpViewEpoch = useCallback(() => {
    viewEpochRef.current += 1;
  }, []);

  const snapshotOutgoingSession = useCallback(() => {
    const current = portsRef.current;
    const sessionId = viewingSessionIdRef.current;
    if (!sessionId) return;
    const text = current.composer.draftRef.current;
    const attachments = current.composer.attachmentsRef.current;
    if (text || attachments.length) {
      sessionDraftsRef.current.set(sessionId, { text, attachments: [...attachments] });
    } else {
      sessionDraftsRef.current.delete(sessionId);
    }
    current.runtime.messagesBySessionRef.current.set(
      sessionId,
      snapshotOutgoingMessages(
        current.runtime.messagesBySessionRef.current.get(sessionId),
        current.runtime.messagesRef.current,
      ),
    );
  }, []);

  /** Preserve a draft while an async attachment request finishes elsewhere. */
  const snapshotOutgoingDraft = useCallback(() => {
    const current = portsRef.current;
    if (viewingSessionIdRef.current !== null) return;
    const text = current.composer.draftRef.current;
    const attachments = current.composer.attachmentsRef.current;
    draftNavigationSnapshotRef.current = snapshotDraftNavigation(
      text,
      attachments,
      current.sidebar.activeProject,
      draftKeyRef.current,
    );
  }, []);

  const openSession = useCallback<SessionNavigationOpenSession>(
    async (row, project) => {
      const current = portsRef.current;
      // The browser preview deliberately does not create ACP sessions.
      if (!current.runtime.isTauri() || typeof window === "undefined") return;
      const started = performance.now();

      const projectForSession =
        project !== undefined
          ? project
          : current.sidebar.projects.find((item) => item.id === row.projectId) ??
            null;
      current.route.navigateWorkbench();
      current.sidebar.setUnreadTerminalResults((previous) => {
        if (!previous.has(row.id)) return previous;
        const next = new Map(previous);
        next.delete(row.id);
        saveUnreadTerminalResults(next, localStorage);
        return next;
      });

      snapshotOutgoingDraft();
      bumpViewEpoch();
      snapshotOutgoingSession();
      navigationTimingRef.current = {
        sessionId: row.id, epoch: viewEpochRef.current, started,
      };

      openingSessionIdRef.current = row.id;
      viewingSessionIdRef.current = row.id;
      // 在任何异步恢复之前切换草稿和本地消息的归属，快速连续导航也不能快照上一会话内容。
      const targetDraft = sessionDraftsRef.current.get(row.id);
      current.composer.setDraft(targetDraft?.text ?? "");
      current.composer.setAttachments(targetDraft?.attachments ?? []);
      const targetMessages = current.runtime.messagesBySessionRef.current.get(row.id) ?? [];
      current.runtime.messagesRef.current = targetMessages;
      current.ui.setMessages(targetMessages);
      const cachedView = current.runtime.workspaceRef.current.sessions[row.id];
      current.ui.setSession(cachedView?.replay.loaded
        ? { ...projectAcpSnapshot(cachedView), title: row.title }
        : { ...IDLE_SNAPSHOT, sessionId: row.id, state: "connecting", title: row.title,
            projectPath: projectForSession?.path ?? null });
      current.sidebar.setActiveProject(projectForSession);
      const originView = currentViewFocus();
      openingSessionEpochRef.current = originView.epoch;
      const canAdoptOpenView = () =>
        shouldAdoptView(originView, currentViewFocus(), row.id);
      const ownsOpeningSlot = () =>
        openingSessionIdRef.current === row.id &&
        openingSessionEpochRef.current === originView.epoch;
      const clearOpeningSlot = () => {
        if (!ownsOpeningSlot()) return;
        openingSessionIdRef.current = null;
        openingSessionEpochRef.current = null;
      };
      current.ui.setAskUser(
        current.sidebar.pendingAskUserBySessionRef.current.get(row.id) ?? null,
      );

      try {
        const connectStarted = performance.now();
        const operationId = createOperationId("session-connect");
        let hostState: AcpSessionSnapshot["state"] | null = null;
        let view = current.runtime.workspaceRef.current.sessions[row.id];
        let connectMs = 0;
        let replayMs = 0;
        if (!view) {
          const connected = await current.runtime.connect({
            projectPath: projectForSession?.path || undefined,
            sessionId: row.id,
            operationId,
          });
          current.runtime.observeHostActiveTurn(connected);
          hostState = connected.state;
          connectMs = performance.now() - connectStarted;
          view = ensureAcpSession(current.runtime.workspaceRef.current, row.id);
          view.project_path = projectForSession?.path ?? null;
          const replayStarted = performance.now();
          await current.runtime.replayHistory(row.id, originView);
          replayMs = performance.now() - replayStarted;
        } else {
          // 后台 Session 重新获得原生焦点，提问通知才能正确归属当前任务。
          const connected = await current.runtime.connect({
            projectPath: projectForSession?.path || undefined,
            sessionId: row.id,
            operationId,
          });
          current.runtime.observeHostActiveTurn(connected);
          hostState = connected.state;
          connectMs = performance.now() - connectStarted;
          try {
            const replayStarted = performance.now();
            await current.runtime.replayHistory(row.id, originView);
            replayMs = performance.now() - replayStarted;
          } catch {
            const reconnected = await current.runtime.connect({
              projectPath: projectForSession?.path || undefined,
              sessionId: row.id,
              operationId,
            });
            current.runtime.observeHostActiveTurn(reconnected);
            hostState = reconnected.state;
            view = ensureAcpSession(
              current.runtime.workspaceRef.current,
              row.id,
            );
            view.project_path = projectForSession?.path ?? null;
          }
        }
        if (!view) throw new Error(`ACP Session 未登记：${row.id}`);
        if (!canAdoptOpenView()) {
          clearOpeningSlot();
          return;
        }
        const projected = projectAcpSnapshot(view);
        const snapshot = hostState
          ? { ...projected, state: hostState }
          : projected;
        current.ui.setSession(snapshot);
        void diagnosticsRecord("session_navigation", JSON.stringify({
          sessionId: row.id, phase: "ready", cached: cachedView?.replay.loaded === true,
          connectMs: Math.round(connectMs), replayMs: Math.round(replayMs),
          readyMs: Math.round(performance.now() - started),
        })).catch(() => {});
        current.ui.setLiveHost(snapshot);
        current.runtime.liveHostRef.current = snapshot;
        current.sidebar.setActiveProject(projectForSession);
        current.ui.setLocalError(null);
        clearOpeningSlot();
        current.runtime.commitWorkspace();
        current.runtime.applyViewProjection(row.id);
        // 会话模型引用必须原样回填：只回填模型 ID 会让同名模型显示成全局活跃供应商。
        const sessionModelReference = current.providers.modelBySessionRef.current.get(row.id);
        if (sessionModelReference) {
          current.providers.setSessionModelReference(sessionModelReference);
        }
        await current.runtime.refreshSessions();
      } catch (cause) {
        if (canAdoptOpenView()) {
          void diagnosticsRecord("session_navigation", JSON.stringify({
            sessionId: row.id, phase: "failed",
            elapsedMs: Math.round(performance.now() - started),
            reason: cause instanceof Error ? cause.message : String(cause),
          })).catch(() => {});
          current.ui.setSession((previous) => previous.sessionId === row.id
            ? { ...previous, state: "disconnected" }
            : previous);
          current.ui.setLocalError(localizeUiError(cause, locale));
        }
        clearOpeningSlot();
      }
    },
    [bumpViewEpoch, currentViewFocus, locale, snapshotOutgoingDraft, snapshotOutgoingSession],
  );

  const newChat = useCallback<SessionNavigationNewChat>(
    async (project, options) => {
      const current = portsRef.current;
      const projectForDraft =
        project === undefined ? current.sidebar.activeProject : project;
      if (projectForDraft && isProjectPathMissing(projectForDraft.pathOk)) {
        current.ui.setLocalError(
          tr("project.pathMissing", { name: projectForDraft.name }),
        );
        return;
      }

      const leavingSessionId = viewingSessionIdRef.current;
      snapshotOutgoingDraft();
      const restoredDraft = leavingSessionId
        ? restoreDraftNavigation(
            draftNavigationSnapshotRef.current,
            projectForDraft,
          )
        : null;
      draftKeyRef.current += 1;
      if (restoredDraft && options?.seedDraft === undefined) {
        draftNavigationSnapshotRef.current = {
          ...restoredDraft,
          draftKey: draftKeyRef.current,
        };
      }

      current.route.navigateWorkbench();
      current.sidebar.setActiveProject(projectForDraft);
      if (projectForDraft) {
        current.sidebar.setExpandedProjects((expanded) => ({
          ...expanded,
          [projectForDraft.id]: true,
        }));
      } else {
        current.sidebar.setHistoryOpen(true);
      }
      bumpViewEpoch();
      snapshotOutgoingSession();
      viewingSessionIdRef.current = null;
      // 新草稿采用已保存默认值，不能继承上一会话的模型标签。
      const defaultModel = current.providers.configuredModelsRef.current.find(
        (model) => model.isDefault,
      );
      current.providers.setSessionModelReference(
        defaultModel
          ? formatSessionModelReference(defaultModel.providerId, defaultModel.id)
          : "",
      );
      openingSessionIdRef.current = null;
      openingSessionEpochRef.current = null;
      current.runtime.messagesRef.current = [];
      current.ui.setMessages([]);
      current.ui.setContextUsage(null);
      current.composer.setDraft(
        options?.seedDraft ?? restoredDraft?.text ?? "",
      );
      current.composer.setAttachments(
        options?.seedDraft === undefined
          ? restoredDraft?.attachments ?? []
          : [],
      );
      current.composer.sendQueue.clearDraftQueue();
      current.ui.setAskUser(null);
      current.ui.setRetryStatus(null);
      current.ui.closeSummary();
      current.ui.setSession({
        ...IDLE_SNAPSHOT,
        sessionId: null,
        title: tr("session.new"),
        state: "idle",
        backend: "acp",
      });
      current.ui.setLocalError(null);

      const previousLive = current.runtime.liveHostRef.current;
      if (previousLive.sessionId && isSessionLiveStreaming(previousLive.state)) {
        current.ui.setLiveMap((previous) =>
          projectHostIntoLiveMap(previous, {
            sessionId: previousLive.sessionId!,
            state: previousLive.state,
            streamingMessageId: previousLive.streamingMessageId,
          }),
        );
      }
      current.composer.requestComposerFocus();
    },
    [bumpViewEpoch, snapshotOutgoingDraft, snapshotOutgoingSession, tr],
  );

  useEffect(() => {
    // Do not let the intermediate null Session state clobber an in-flight open.
    if (openingSessionIdRef.current) return;
    viewingSessionIdRef.current = ui.session.sessionId;
  }, [ui.session.sessionId]);

  return {
    draftKeyRef,
    draftNavigationSnapshotRef,
    viewEpochRef,
    viewingSessionIdRef,
    openingSessionIdRef,
    openingSessionEpochRef,
    currentViewFocus,
    draftNavigationLocation,
    bumpViewEpoch,
    openSession,
    newChat,
  };
}
