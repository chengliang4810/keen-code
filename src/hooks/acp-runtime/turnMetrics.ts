import { useCallback, useEffect } from "react";
import type { AcpWorkspaceState } from "@/lib/acp/store";
import {
  replaceHistoryTurnMetrics,
} from "@/lib/acp/projection";
import type { SessionSnapshot } from "@/lib/session";
import {
  reduceTurnLatency,
  summarizeTurnLatency,
  turnLatencyNow,
  type TurnLatencyState,
} from "@/lib/turnLatency";
import type { Ref } from "./types";

export interface AcpRuntimeTurnMetricsOptions {
  sessionId: SessionSnapshot["sessionId"];
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  viewingSessionIdRef: Ref<string | null>;
  commitWorkspace: () => void;
  applyViewProjection: Ref<(sessionId: string | null) => void>;
}

/** 只在首段正文/思考真正提交后结束可见性计时。 */
export function useAcpRuntimeTurnMetrics({
  sessionId,
  acpWorkspaceRef,
  turnLatencyBySessionRef,
  pendingVisibleTurnBySessionRef,
  viewingSessionIdRef,
  commitWorkspace,
  applyViewProjection,
}: AcpRuntimeTurnMetricsOptions): {
  handleFirstVisibleToken: (turnId: string) => void;
} {
  const handleFirstVisibleToken = useCallback(
    (turnId: string) => {
      if (!sessionId) return;
      const latency = turnLatencyBySessionRef.current.get(sessionId);
      if (!latency || turnId !== latency.turnId) return;
      if (
        latency.completedAtMs != null &&
        pendingVisibleTurnBySessionRef.current.get(sessionId) !== turnId
      ) {
        return;
      }
      const visibleLatency = reduceTurnLatency(latency, {
        type: "first_visible_token",
        turnId: latency.turnId,
        atMs: turnLatencyNow(),
      });
      if (visibleLatency === latency) return;
      pendingVisibleTurnBySessionRef.current.delete(sessionId);
      turnLatencyBySessionRef.current.set(sessionId, visibleLatency);
      if (visibleLatency.completedAtMs == null) return;

      const view = acpWorkspaceRef.current.sessions[sessionId];
      if (
        view &&
        replaceHistoryTurnMetrics(
          view,
          summarizeTurnLatency(visibleLatency),
        )
      ) {
        commitWorkspace();
        applyViewProjection.current(viewingSessionIdRef.current);
      }
      if (visibleLatency.sendAcknowledgedAtMs != null) {
        turnLatencyBySessionRef.current.delete(sessionId);
      }
    },
    [commitWorkspace, sessionId],
  );

  // A completed turn that was never committed while visible must not be
  // assigned a delayed visibility timestamp after a later session switch.
  useEffect(() => {
    for (const [pendingSessionId, turnId] of pendingVisibleTurnBySessionRef.current) {
      if (pendingSessionId === sessionId) continue;
      pendingVisibleTurnBySessionRef.current.delete(pendingSessionId);
      const latency = turnLatencyBySessionRef.current.get(pendingSessionId);
      if (
        latency?.turnId === turnId &&
        latency.completedAtMs != null &&
        latency.sendAcknowledgedAtMs != null
      ) {
        turnLatencyBySessionRef.current.delete(pendingSessionId);
      }
    }
  }, [sessionId]);

  return { handleFirstVisibleToken };
}
