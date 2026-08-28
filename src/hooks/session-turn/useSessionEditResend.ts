import { useCallback } from "react";
import type { SessionSnapshot, ChatMessage } from "@/lib/session";
import { buildAgentPrompt } from "@/lib/attachments";
import { localizeUiError } from "@/lib/session";
import { ensureAcpSession } from "@/lib/acp/projection";
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
      if (
        !sessionId ||
        session.state === "streaming" ||
        sendInFlightRef.current
      ) {
        return false;
      }
      try {
        const prepared = await api.prepareEditLastUser({
          sessionId,
          expectedText: buildAgentPrompt(
            message.content,
            message.attachments ?? [],
          ),
        });
        updateSessionPreference(prepared.archivedBranchId, { archived: true });
        const view = ensureAcpSession(acpWorkspaceRef.current, sessionId);
        for (let index = view.history.length - 1; index >= 0; index -= 1) {
          if (view.history[index]?.role === "user") {
            view.history.splice(index);
            break;
          }
        }
        view.live_segments = [];
        commitWorkspace();
        patchSessionMessages(sessionId, (current) => {
          let index = -1;
          for (let cursor = current.length - 1; cursor >= 0; cursor -= 1) {
            if (current[cursor]?.role === "user") {
              index = cursor;
              break;
            }
          }
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
