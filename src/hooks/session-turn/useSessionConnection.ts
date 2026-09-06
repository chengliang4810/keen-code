import { useCallback, useRef } from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import type { Project } from "@/features/app/models";
import { localizeUiError } from "@/lib/session";
import { isProjectPathMissing } from "@/lib/projectPath";
import { ensureAcpSession } from "@/lib/acp/projection";
import { projectAcpSnapshot } from "@/lib/sessionProjection";
import { createOperationId } from "@/lib/acp/api";
import {
  isSameView,
  shouldAdoptView,
} from "@/lib/viewFocus";
import type {
  EnsureConnected,
  SessionTurnApiPort,
  SessionTurnRuntimePort,
  SessionTurnState,
  SessionTurnUiPort,
} from "./types";

export interface UseSessionConnectionOptions {
  locale: Locale;
  tr: (key: MessageKey, vars?: Vars) => string;
  sessionId: string | null;
  activeProject: Project | null;
  effort: string;
  api: SessionTurnApiPort;
  runtime: SessionTurnRuntimePort;
  ui: Pick<SessionTurnUiPort, "setSession" | "setLocalError" | "setLiveHost">;
  state: Pick<
    SessionTurnState,
    "connectingRef" | "setConnectingState" | "observeHostActiveTurn"
  >;
}

export function useSessionConnection({
  locale,
  tr,
  sessionId,
  activeProject,
  effort,
  api,
  runtime,
  ui,
  state,
}: UseSessionConnectionOptions): EnsureConnected {
  const {
    acpWorkspaceRef,
    liveHostRef,
    messagesBySessionRef,
    viewingSessionIdRef,
    applyViewProjectionRef,
    commitWorkspace,
    currentViewFocus,
    replayHistory,
    refreshSessions,
  } = runtime;
  const { setSession, setLocalError, setLiveHost } = ui;
  const {
    connectingRef,
    setConnectingState,
    observeHostActiveTurn,
  } = state;
  /** 新建 Session 的响应丢失后，下一次连接必须复用同一确定性标识。 */
  const draftConnectOperationIdRef = useRef<string | null>(null);

  return useCallback<EnsureConnected>(
    async (forceOrOptions = false) => {
      const options =
        typeof forceOrOptions === "boolean"
          ? {
              force: forceOrOptions,
              sessionId: undefined as string | null | undefined,
            }
          : forceOrOptions;
      const force = !!options.force;
      const preferredId =
        options.sessionId !== undefined ? options.sessionId : sessionId;

      if (activeProject && isProjectPathMissing(activeProject.pathOk)) {
        setLocalError(tr("project.pathMissing", { name: activeProject.name }));
        return null;
      }
      const originView = currentViewFocus();
      if (!api.isTauri()) return null;
      if (connectingRef.current) return null;
      setConnectingState(true);
      try {
        const existing = preferredId
          ? acpWorkspaceRef.current.sessions[preferredId]
          : undefined;
        if (existing && !force) {
          await replayHistory(existing.session_id, originView);
          if (existing.delivery.frozen) throw new Error("Session 历史恢复未完成");
          return preferredId ?? existing.session_id;
        }

        const draftMessages =
          preferredId == null
            ? messagesBySessionRef.current.get("__draft__")
            : undefined;
        const operationId = preferredId
          ? createOperationId("session-connect")
          : (draftConnectOperationIdRef.current ??=
              createOperationId("session-connect"));
        const opened = await api.connect({
          projectPath: activeProject?.path || undefined,
          sessionId: preferredId ?? null,
          operationId,
        });
        const openedSessionId = opened.sessionId ?? null;
        if (!openedSessionId) {
          throw new Error("session_connect 未返回 sessionId");
        }
        observeHostActiveTurn(opened);
        const view = ensureAcpSession(
          acpWorkspaceRef.current,
          openedSessionId,
        );
        if (!preferredId) {
          await api.setEffort({
            sessionId: openedSessionId,
            effort,
            operationId: `${operationId}-effort`,
          });
        }
        if (draftMessages?.length) {
          messagesBySessionRef.current.set(openedSessionId, draftMessages);
          if (
            messagesBySessionRef.current.get("__draft__") === draftMessages
          ) {
            messagesBySessionRef.current.delete("__draft__");
          }
        }
        await replayHistory(openedSessionId, originView);
        view.project_path = opened.projectPath ?? null;
        const snapshot = {
          ...projectAcpSnapshot(view),
          state: opened.state,
        };
        setLiveHost(snapshot);
        liveHostRef.current = snapshot;
        commitWorkspace();
        if (
          shouldAdoptView(
            originView,
            currentViewFocus(),
            openedSessionId,
          )
        ) {
          viewingSessionIdRef.current = openedSessionId;
          setSession(snapshot);
          setLocalError(null);
          applyViewProjectionRef.current(openedSessionId);
        }
        await refreshSessions();
        if (!preferredId) draftConnectOperationIdRef.current = null;
        return openedSessionId;
      } catch (cause) {
        if (
          (preferredId != null &&
            viewingSessionIdRef.current === preferredId) ||
          isSameView(originView, currentViewFocus())
        ) {
          setLocalError(localizeUiError(cause, locale));
        }
        return null;
      } finally {
        setConnectingState(false);
      }
    },
    [
      activeProject,
      acpWorkspaceRef,
      api,
      applyViewProjectionRef,
      commitWorkspace,
      currentViewFocus,
      effort,
      locale,
      messagesBySessionRef,
      observeHostActiveTurn,
      refreshSessions,
      replayHistory,
      sessionId,
      setConnectingState,
      setLiveHost,
      setLocalError,
      setSession,
      tr,
      viewingSessionIdRef,
      liveHostRef,
      connectingRef,
    ],
  );
}
