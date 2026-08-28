import {
  useCallback,
  useEffect,
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
import type { ModelOption } from "@/lib/modelCatalog";
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
import type { SessionSnapshot as AcpSessionSnapshot } from "@/lib/acp/api";
import { ensureAcpSession } from "@/lib/acp/projection";
import type { AcpWorkspaceState } from "@/lib/acp/store";
import { projectAcpSnapshot } from "@/lib/sessionProjection";
import {
  projectHostIntoLiveMap,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import { isProjectPathMissing } from "@/lib/projectPath";
import { saveCompletedUnreadSessionIds } from "@/lib/sessionCompletion";
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
  }) => Promise<AcpSessionSnapshot>;
  observeHostActiveTurn: (snapshot: {
    sessionId?: string | null;
    activeTurnId?: string | null;
  }) => void;
  replayHistory: (sessionId: string, originView?: ViewFocus) => Promise<void>;
  applyViewProjection: (sessionId: string | null) => void;
  refreshSessions: () => Promise<void>;
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
  setCompletedUnreadIds: StateSetter<Set<string>>;
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
  setModelId: StateSetter<string>;
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

      const projectForSession =
        project !== undefined
          ? project
          : current.sidebar.projects.find((item) => item.id === row.projectId) ??
            null;
      current.route.navigateWorkbench();
      current.sidebar.setCompletedUnreadIds((previous) => {
        if (!previous.has(row.id)) return previous;
        const next = new Set(previous);
        next.delete(row.id);
        saveCompletedUnreadSessionIds(next, localStorage);
        return next;
      });

      snapshotOutgoingDraft();
      bumpViewEpoch();
      snapshotOutgoingSession();

      openingSessionIdRef.current = row.id;
      viewingSessionIdRef.current = row.id;
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
        let hostState: AcpSessionSnapshot["state"] | null = null;
        let view = current.runtime.workspaceRef.current.sessions[row.id];
        if (!view) {
          const connected = await current.runtime.connect({
            projectPath: projectForSession?.path || undefined,
            sessionId: row.id,
          });
          current.runtime.observeHostActiveTurn(connected);
          hostState = connected.state;
          view = ensureAcpSession(current.runtime.workspaceRef.current, row.id);
          view.project_path = projectForSession?.path ?? null;
          await current.runtime.replayHistory(row.id, originView);
        } else {
          // 后台 Session 重新获得原生焦点，提问通知才能正确归属当前任务。
          const connected = await current.runtime.connect({
            projectPath: projectForSession?.path || undefined,
            sessionId: row.id,
          });
          current.runtime.observeHostActiveTurn(connected);
          hostState = connected.state;
          try {
            await current.runtime.replayHistory(row.id, originView);
          } catch {
            const reconnected = await current.runtime.connect({
              projectPath: projectForSession?.path || undefined,
              sessionId: row.id,
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
        current.ui.setLiveHost(snapshot);
        current.runtime.liveHostRef.current = snapshot;
        current.sidebar.setActiveProject(projectForSession);
        current.composer.setAttachments([]);
        current.ui.setLocalError(null);
        clearOpeningSlot();
        current.runtime.commitWorkspace();
        current.runtime.applyViewProjection(row.id);
        const sessionModel = current.providers.modelBySessionRef.current.get(row.id);
        if (
          sessionModel &&
          current.providers.configuredModelsRef.current.some(
            (model) => model.id === sessionModel,
          )
        ) {
          current.providers.setModelId(sessionModel);
        }
        await current.runtime.refreshSessions();
      } catch (cause) {
        if (canAdoptOpenView()) {
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
      openingSessionIdRef.current = null;
      openingSessionEpochRef.current = null;
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
        backend: "peri_acp",
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
