import { useCallback } from "react";
import type { SessionSnapshot, ChatMessage } from "@/lib/session";
import { buildAgentPrompt } from "@/lib/attachments";
import { localizeUiError } from "@/lib/session";
import { ensureAcpSession } from "@/lib/acp/projection";
import { createOperationId } from "@/lib/acp/api";
import type {
  ExecuteSend,
  SessionTurnApiPort,
  SessionTurnRuntimePort,
  SessionTurnState,
  SessionTurnUiPort,
} from "./types";

export interface UseSessionEditResendOptions {
  locale: Parameters<typeof localizeUiError>[1];
  session: SessionSnapshot;
  planModeSessionKey: string | null;
  ultraModeSessionKey: string | null;
  api: SessionTurnApiPort;
  runtime: SessionTurnRuntimePort;
  ui: Pick<SessionTurnUiPort, "setLocalError">;
  state: Pick<SessionTurnState, "sendInFlightRef">;
  executeSend: ExecuteSend;
}

export function useSessionEditResend({
  locale,
  session,
  planModeSessionKey,
  ultraModeSessionKey,
  api,
  runtime,
  ui,
  state,
  executeSend,
}: UseSessionEditResendOptions) {
  const {
    acpWorkspaceRef,
    applyViewProjectionRef,
    commitWorkspace,
    patchSessionMessages,
    refreshSessions,
    updateSessionPreference,
  } = runtime;
  const { setLocalError } = ui;
  const { sendInFlightRef } = state;

  return useCallback(
    async (message: ChatMessage, content: string): Promise<boolean> => {
      const sessionId = session.sessionId;
      // 编辑重发会先修改权威历史，必须等待当前真实投影完成恢复。
      const currentView = sessionId
        ? acpWorkspaceRef.current.sessions[sessionId]
        : undefined;
      if (
        !sessionId ||
        session.state !== "ready" ||
        !currentView ||
        !currentView.replay.loaded ||
        currentView.replay.restoring ||
        currentView.delivery.frozen ||
        sendInFlightRef.current
      ) {
        return false;
      }
      try {
        const prepared = await api.rewind({
          sessionId,
          targetMessageId: message.id,
          expectedText: buildAgentPrompt(
            message.content,
            message.attachments ?? [],
          ),
          revertFiles: false,
          operationId: createOperationId("session-rewind"),
        });
        updateSessionPreference(prepared.archivedSessionId, { archived: true });
        const view = ensureAcpSession(acpWorkspaceRef.current, sessionId);
        const historyIndex = view.history.findIndex((historyMessage, index) =>
          historyMessage.messageId === message.id ||
          (!historyMessage.messageId &&
            `${sessionId}:history:${index}` === message.id),
        );
        if (historyIndex >= 0) {
          view.history.splice(historyIndex);
        }
        view.live_segments = [];
        view.live_turn_metadata = null;
        commitWorkspace();
        patchSessionMessages(sessionId, (current) => {
          const index = current.findIndex(
            (currentMessage) => currentMessage.id === message.id,
          );
          return index >= 0 ? current.slice(0, index) : current;
        });
        applyViewProjectionRef.current(sessionId);
        await refreshSessions();
        return await executeSend({
          storedDisplay: content,
          att: message.attachments ?? [],
          planMode: planModeSessionKey === sessionId,
          ultraMode: ultraModeSessionKey === sessionId,
          targetSessionId: sessionId,
        });
      } catch (cause) {
        setLocalError(localizeUiError(cause, locale));
        return false;
      }
    },
    [
      api,
      acpWorkspaceRef,
      applyViewProjectionRef,
      commitWorkspace,
      executeSend,
      locale,
      patchSessionMessages,
      planModeSessionKey,
      refreshSessions,
      sendInFlightRef,
      session.sessionId,
      session.state,
      setLocalError,
      ultraModeSessionKey,
      updateSessionPreference,
    ],
  );
}
