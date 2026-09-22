import { useCallback } from "react";
import type { SessionSnapshot, ChatMessage } from "@/lib/session";
import { buildAgentPrompt } from "@/lib/attachments";
import { localizeUiError } from "@/lib/session";
import { createOperationId } from "@/lib/acp/api";
import { beginSessionRecovery } from "@/lib/acp/store";
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
        currentView.active_root_turn_id !== null ||
        currentView.subagents.some((agent) => agent.status === "running") ||
        sendInFlightRef.current
      ) {
        return false;
      }
      // 编辑重发只能回退 Journal 中的权威根用户消息；本地乐观气泡（u-*）或
      // 合成投影标识没有 Journal 对应物，直接拒绝，避免发出注定失败的 rewind。
      const targetMessageId = message.id;
      const latestAuthoritativeUserMessage = currentView.history
        .filter((item) => item.role === "user" && item.messageId)
        .at(-1);
      if (latestAuthoritativeUserMessage?.messageId !== targetMessageId) {
        setLocalError("只能编辑最后一条已同步的用户消息。");
        return false;
      }
      try {
        sendInFlightRef.current = true;
        try {
          // 变更请求可能已经在 Host 提交但响应在 IPC 边界丢失；先清空旧投递世代，
          // 让随后恢复的序号 1/2 不会被误判为旧事件。发送锁覆盖整个恢复窗口。
          beginSessionRecovery(currentView);
          let rewindFailed = false;
          let rewindFailure: unknown;
          try {
            const prepared = await api.rewind({
              sessionId,
              targetMessageId,
              expectedText: buildAgentPrompt(
                message.content,
                message.attachments ?? [],
              ),
              operationId: createOperationId("session-rewind"),
            });
            updateSessionPreference(prepared.archivedSessionId, { archived: true });
          } catch (cause) {
            // Rewind 可能已在 Host 完成事务后才丢失响应；无论 API 成功与否都必须
            // 继续标准恢复，成功时建立新投递世代，失败时也重建当前权威历史。
            rewindFailed = true;
            rewindFailure = cause;
          }
          let replayFailed = false;
          let replayFailure: unknown;
          try {
            await replayHistory(sessionId);
          } catch (cause) {
            replayFailed = true;
            replayFailure = cause;
          }
          if (rewindFailed) throw rewindFailure;
          if (replayFailed) throw replayFailure;
          try {
            await refreshSessions();
          } catch {
            // 权威 rewind 与 replay 已完成；列表刷新失败不能阻断新 Turn。
          }
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
