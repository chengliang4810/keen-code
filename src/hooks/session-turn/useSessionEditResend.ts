import { useCallback } from "react";
import type { SessionSnapshot, ChatMessage } from "@/lib/session";
import { buildAgentPrompt } from "@/lib/attachments";
import { localizeUiError } from "@/lib/session";
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
    replayHistory,
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
      // 编辑重发只能回退 Journal 中的权威根用户消息；本地乐观气泡（u-*）或
      // 合成投影标识没有 Journal 对应物，直接拒绝，避免发出注定失败的 rewind。
      const targetMessageId = message.id;
      if (
        !currentView.history.some(
          (item) => item.role === "user" && item.messageId === targetMessageId,
        )
      ) {
        setLocalError("该消息尚未同步完成，不能编辑重发；请直接在输入框重新发送。");
        return false;
      }
      try {
        sendInFlightRef.current = true;
        try {
          const prepared = await api.rewind({
            sessionId,
            targetMessageId,
            expectedText: buildAgentPrompt(
              message.content,
              message.attachments ?? [],
            ),
            revertFiles: false,
            operationId: createOperationId("session-rewind"),
          });
          updateSessionPreference(prepared.archivedSessionId, { archived: true });
          // rewind 重开了后端 Session，必须通过标准 load 重建完整投影和投递游标。
          currentView.replay.loaded = false;
          await replayHistory(sessionId);
          await refreshSessions();
        } finally {
          sendInFlightRef.current = false;
        }
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
      replayHistory,
      executeSend,
      locale,
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
