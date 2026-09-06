import { useCallback } from "react";
import type { MessageKey, Vars } from "@/i18n";
import { ensureAcpSession } from "@/lib/acp/projection";
import { buildAgentPrompt } from "@/lib/attachments";
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
      const segments = parseStoredContent(item.storedDisplay);
      const agentBody = serializeForAgent(segments);
      if (item.createGoal) {
        const objective = agentBody.trim();
        if (!objective) throw new Error(tr("goal.objectiveRequired"));
        const currentView = ensureAcpSession(acpWorkspaceRef.current, sessionId);
        const result = await api.goalUpsert({
          sessionId,
          goal: {
            title: objective,
            objective,
            description: objective,
          },
          expectedRevision: currentView.goal.revision,
          requestNonce: `${item.id}-goal`,
        });
        currentView.goal = { revision: result.revision, goal: result.goal };
        commitWorkspace();
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
