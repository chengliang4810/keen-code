import { useCallback } from "react";
import type { MessageKey, Vars } from "@/i18n";
import { reduceGoalSnapshot } from "@/lib/acp/store";
import { ensureAcpSession } from "@/lib/acp/projection";
import { buildAgentPrompt } from "@/lib/attachments";
import { buildGoalDraft } from "@/lib/goalDraft";
import { parseStoredContent, serializeForAgent } from "@/lib/draftDoc";
import type { SessionState } from "@/lib/session";
import type { QueuedSend } from "@/lib/sendQueue";
import type {
  SessionTurnApiPort,
  SessionTurnRuntimePort,
} from "./types";

export interface UseSessionQueueSteeringOptions {
  tr: (key: MessageKey, vars?: Vars) => string;
  sessionId: string | null;
  sessionState: SessionState;
  api: SessionTurnApiPort;
  runtime: Pick<
    SessionTurnRuntimePort,
    "acpWorkspaceRef" | "commitWorkspace"
  >;
  showToast: (message: string, durationMs?: number) => void;
}

export function useSessionQueueSteering({
  tr,
  sessionId,
  sessionState,
  api,
  runtime,
  showToast,
}: UseSessionQueueSteeringOptions) {
  const { acpWorkspaceRef, commitWorkspace } = runtime;

  return useCallback(
    async (item: QueuedSend) => {
      if (!sessionId || sessionState !== "streaming") {
        throw new Error(tr("composer.queueSteerNotRunning"));
      }
      if (api.isTauri && !api.isTauri() && item.attachments.length > 0) {
        // `keencode/session/steer` 目前只有 text 字段，不能把 Web resource
        // 当作本机路径或静默丢弃；让队列等待当前回合结束后走 session/prompt。
        throw new Error("Web Host 附件会在当前回合结束后发送。");
      }
      const segments = parseStoredContent(item.storedDisplay);
      const agentBody = serializeForAgent(segments);
      if (item.createGoal) {
        const goal = buildGoalDraft(agentBody);
        const currentView = ensureAcpSession(acpWorkspaceRef.current, sessionId);
        const result = await api.goalUpsert({
          sessionId,
          goal,
          expectedRevision: currentView.goal.revision,
          requestNonce: `${item.id}-goal`,
        });
        if (reduceGoalSnapshot(currentView, result.revision, result.goal)) {
          commitWorkspace();
        }
      }
      await api.steer({
        sessionId,
        text: buildAgentPrompt(agentBody, item.attachments),
        operationId: `${item.id}-steer`,
      });
      showToast(tr("composer.queueSteered"), 2200);
    },
    [
      api,
      acpWorkspaceRef,
      commitWorkspace,
      sessionId,
      sessionState,
      showToast,
      tr,
    ],
  );
}
