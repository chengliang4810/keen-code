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
  AppDialog,
  ContextMenuState,
  Project,
  SessionContextUsage,
  SessionRow,
} from "@/features/app/models";
import {
  type AskUserPayload,
  IDLE_SNAPSHOT,
  localizeUiError,
  type ChatMessage,
  type SessionSnapshot,
} from "@/lib/session";
import {
  goalClear,
  goalUpsert,
  goalsList,
  sessionConnect,
  sessionDelete,
  sessionDisconnect,
  sessionFork,
  sessionMessages,
  sessionPrepareEditLastUser,
  sessionSend,
  sessionSetEffort,
  sessionSteer,
  sessionStop,
} from "@/lib/acp/api";
import {
  createAcpWorkspaceState,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import { projectPeriStoredMessages } from "@/lib/periStoredMessages";
import { removeSessionPreference } from "@/lib/sessionPreferences";
import {
  sessionExportFilename,
  sessionToMarkdown,
} from "@/lib/sessionExport";
import type { TurnLatencyState } from "@/lib/turnLatency";

type StateSetter<T> = Dispatch<SetStateAction<T>>;
type Ref<T> = MutableRefObject<T>;

/** ACP ports shared by the composer, turn controller, navigation and lifecycle actions. */
export const acpSessionApi = {
  goals: {
    list: goalsList,
    clear: goalClear,
    upsert: goalUpsert,
  },
  connect: sessionConnect,
  disconnect: sessionDisconnect,
  fork: sessionFork,
  messages: sessionMessages,
  delete: sessionDelete,
  send: sessionSend,
  steer: sessionSteer,
  stop: sessionStop,
  prepareEditLastUser: sessionPrepareEditLastUser,
  setEffort: sessionSetEffort,
};

type SessionNavigationNewChat = (
  project?: Project | null,
  options?: { seedDraft?: string },
) => Promise<void>;

type SessionNavigationOpenSession = (
  session: SessionRow,
  project?: Project | null,
) => Promise<void>;

export type SessionExportMeta = {
  id: string;
  title: string;
  projectId?: string | null;
};

export interface UseSessionLifecycleActionsOptions {
  locale: Locale;
  appBooting: boolean;
  providerRouteRevision: number;
  isTauri: () => boolean;
  session: SessionSnapshot;
  activeProject: Project | null;
  projects: Project[];
  sessions: SessionRow[];
  messages: ChatMessage[];
  navigation: {
    newChat: SessionNavigationNewChat;
    openSession: SessionNavigationOpenSession;
  };
  sidebar: {
    refreshSessions: () => Promise<void>;
    archiveSession: (session: SessionRow, archived?: boolean) => Promise<void>;
    setExpandedProjects: StateSetter<Record<string, boolean>>;
    setHistoryOpen: StateSetter<boolean>;
    setCtxMenu: StateSetter<ContextMenuState>;
  };
  runtime: {
    acpWorkspaceRef: Ref<AcpWorkspaceState>;
    setAcpWorkspace: StateSetter<AcpWorkspaceState>;
    activeTurnIdBySessionRef: Ref<Map<string, string>>;
    recoverableCompletedTurnIdBySessionRef: Ref<Map<string, string>>;
    completedTurnIdBySessionRef: Ref<Map<string, string>>;
    turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
    pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
    messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
    contextUsageBySessionRef: Ref<Map<string, SessionContextUsage>>;
    liveHostRef: Ref<SessionSnapshot>;
    viewingSessionIdRef: Ref<string | null>;
    openingSessionIdRef: Ref<string | null>;
    openingSessionEpochRef: Ref<number | null>;
    pendingAskUserBySessionRef: Ref<Map<string, AskUserPayload>>;
    dropQueuedSessionsRef: Ref<(sessionIds: Iterable<string>) => void>;
  };
  ui: {
    setSession: StateSetter<SessionSnapshot>;
    setLiveHost: StateSetter<SessionSnapshot>;
    setMessages: StateSetter<ChatMessage[]>;
    setContextUsage: StateSetter<SessionContextUsage | null>;
    setPendingAskUserSessionIds: StateSetter<Set<string>>;
    setAskUser: StateSetter<AskUserPayload | null>;
    setRetryStatus: StateSetter<{
      attempt: number;
      maxAttempts: number;
      delayMs: number;
      reason: string;
    } | null>;
    setLocalError: StateSetter<string | null>;
    setAppDialog: StateSetter<AppDialog>;
    showToast: (message: string, durationMs?: number) => void;
  };
}

export function useSessionLifecycleActions({
  locale,
  appBooting,
  providerRouteRevision,
  isTauri,
  session,
  activeProject,
  projects,
  sessions,
  messages,
  navigation,
  sidebar,
  runtime,
  ui,
}: UseSessionLifecycleActionsOptions) {
  const portsRef = useRef({
    locale,
    isTauri,
    session,
    activeProject,
    projects,
    sessions,
    messages,
    navigation,
    sidebar,
    runtime,
    ui,
  });
  portsRef.current = {
    locale,
    isTauri,
    session,
    activeProject,
    projects,
    sessions,
    messages,
    navigation,
    sidebar,
    runtime,
    ui,
  };

  const providerRouteReadyRef = useRef(false);
  useEffect(() => {
    if (appBooting) return;
    if (!providerRouteReadyRef.current) {
      providerRouteReadyRef.current = true;
      return;
    }

    const current = portsRef.current;
    void (async () => {
      try {
        await acpSessionApi.disconnect();
      } catch {
        /* ignore */
      }
      current.runtime.acpWorkspaceRef.current = createAcpWorkspaceState();
      current.runtime.setAcpWorkspace(createAcpWorkspaceState());
      current.runtime.activeTurnIdBySessionRef.current.clear();
      current.runtime.recoverableCompletedTurnIdBySessionRef.current.clear();
      current.runtime.completedTurnIdBySessionRef.current.clear();
      current.runtime.turnLatencyBySessionRef.current.clear();
      current.runtime.pendingVisibleTurnBySessionRef.current.clear();
      current.ui.setSession({ ...IDLE_SNAPSHOT, state: "idle" });
      current.ui.setLiveHost({ ...IDLE_SNAPSHOT });
      current.runtime.liveHostRef.current = { ...IDLE_SNAPSHOT };
      current.runtime.viewingSessionIdRef.current = null;
      current.runtime.openingSessionIdRef.current = null;
      current.runtime.openingSessionEpochRef.current = null;
      current.runtime.contextUsageBySessionRef.current.clear();
      current.ui.setContextUsage(null);
      current.runtime.pendingAskUserBySessionRef.current.clear();
      current.ui.setPendingAskUserSessionIds(new Set());
      current.ui.setAskUser(null);
      current.ui.setMessages([]);
      current.ui.setRetryStatus(null);
      current.ui.setLocalError(null);
    })();
  }, [appBooting, providerRouteRevision]);

  const runForkSession = useCallback(async (source: SessionRow) => {
    const current = portsRef.current;
    const tr = createT(current.locale);
    if (!current.isTauri()) {
      current.ui.showToast(tr("error.needTauri"));
      return;
    }
    try {
      const base = (source.title || tr("session.untitled")).trim();
      const title = /^(fork of|分叉：|分叉:)\s*/i.test(base)
        ? base
        : tr("session.forkTitleOf", { name: base || "chat" });
      const meta = await acpSessionApi.fork({ sourceId: source.id, title });
      await current.sidebar.refreshSessions();
      const row: SessionRow = {
        id: meta.id,
        title,
        projectId: source.projectId,
        updatedAt: new Date().toISOString(),
        archived: false,
        pinned: false,
      };
      const project = row.projectId
        ? current.projects.find((item) => item.id === row.projectId) ?? null
        : null;
      if (row.projectId) {
        current.sidebar.setExpandedProjects((expanded) => ({
          ...expanded,
          [row.projectId!]: true,
        }));
      } else {
        current.sidebar.setHistoryOpen(true);
      }
      await current.navigation.openSession(row, project);
      current.ui.showToast(tr("session.forkOk"), 2800);
    } catch (error) {
      current.ui.showToast(`${tr("session.forkFailed")}: ${String(error)}`, 4500);
    }
  }, []);

  const confirmForkSession = useCallback(
    (source: SessionRow) => {
      const current = portsRef.current;
      const tr = createT(current.locale);
      current.sidebar.setCtxMenu(null);
      current.ui.setAppDialog({
        kind: "confirm",
        title: tr("session.forkTitle"),
        message: tr("session.forkConfirm"),
        confirmLabel: tr("session.fork"),
        onConfirm: () => {
          void runForkSession(source);
        },
      });
    },
    [runForkSession],
  );

  const exportActiveSessionMd = useCallback(
    async (sessionMeta?: SessionExportMeta) => {
      const current = portsRef.current;
      const tr = createT(current.locale);
      try {
        const id = sessionMeta?.id ?? current.session.sessionId;
        if (!id) {
          current.ui.showToast(tr("session.exportFail"));
          return;
        }
        const title =
          sessionMeta?.title ||
          current.sessions.find((item) => item.id === id)?.title ||
          current.session.title ||
          tr("session.untitled");
        const projectId =
          sessionMeta?.projectId ??
          current.sessions.find((item) => item.id === id)?.projectId ??
          null;
        const project =
          current.projects.find((item) => item.id === projectId) ||
          current.activeProject ||
          null;
        let sessionMessagesForExport = current.messages;
        if (id !== current.session.sessionId) {
          sessionMessagesForExport = projectPeriStoredMessages(
            await acpSessionApi.messages(id),
          );
        }
        const md = sessionToMarkdown({
          title,
          projectName: project?.name,
          projectPath: project?.path,
          sessionId: id,
          messages: sessionMessagesForExport.map((message) => ({
            role: message.role,
            content: message.content,
            thought: message.thought,
            createdAt: message.createdAt,
          })),
        });
        const blob = new Blob([md], { type: "text/markdown;charset=utf-8" });
        const url = URL.createObjectURL(blob);
        const anchor = document.createElement("a");
        anchor.href = url;
        anchor.download = sessionExportFilename(title, id);
        anchor.click();
        URL.revokeObjectURL(url);
        current.ui.showToast(tr("session.exportDone"));
      } catch (error) {
        current.ui.showToast(`${tr("session.exportFail")}: ${String(error)}`);
      }
    },
    [],
  );

  const archivedSessions = useMemo(
    () =>
      sessions
        .filter((item) => item.archived)
        .map((item) => ({
          id: item.id,
          title: item.title,
          projectName:
            projects.find((project) => project.id === item.projectId)?.name ??
            null,
          updatedAt: item.updatedAt,
        })),
    [projects, sessions],
  );

  const restoreArchivedSession = useCallback(async (sessionId: string) => {
    const current = portsRef.current;
    const archivedSession = current.sessions.find(
      (item) => item.id === sessionId && item.archived,
    );
    if (archivedSession) {
      await current.sidebar.archiveSession(archivedSession, false);
    }
  }, []);

  const deleteArchivedSession = useCallback((sessionId: string) => {
    const current = portsRef.current;
    const archivedSession = current.sessions.find(
      (item) => item.id === sessionId && item.archived,
    );
    if (!archivedSession) return;
    const tr = createT(current.locale);
    current.ui.setAppDialog({
      kind: "confirm",
      title: tr("settings.archived.deleteTitle"),
      message: tr("settings.archived.deleteConfirm", {
        title: archivedSession.title,
      }),
      confirmLabel: tr("settings.archived.delete"),
      danger: true,
      onConfirm: async () => {
        try {
          await acpSessionApi.delete(archivedSession.id);
          removeSessionPreference(archivedSession.id);
          current.runtime.dropQueuedSessionsRef.current([archivedSession.id]);
          current.runtime.messagesBySessionRef.current.delete(archivedSession.id);
          current.runtime.activeTurnIdBySessionRef.current.delete(
            archivedSession.id,
          );
          current.runtime.recoverableCompletedTurnIdBySessionRef.current.delete(
            archivedSession.id,
          );
          current.runtime.completedTurnIdBySessionRef.current.delete(
            archivedSession.id,
          );
          current.runtime.turnLatencyBySessionRef.current.delete(
            archivedSession.id,
          );
          current.runtime.pendingVisibleTurnBySessionRef.current.delete(
            archivedSession.id,
          );
          await current.sidebar.refreshSessions();
          current.ui.showToast(tr("settings.archived.deleteDone"), 2800);
        } catch (error) {
          current.ui.setLocalError(localizeUiError(error, current.locale));
        }
      },
    });
  }, []);

  return {
    confirmForkSession,
    exportActiveSessionMd,
    archivedSessions,
    restoreArchivedSession,
    deleteArchivedSession,
  };
}
