import { useCallback } from "react";
import {
  sessionMessages,
  sessionReplay,
  sessionSubagents,
} from "@/lib/acp/api";
import {
  reduceReplayResult,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import {
  projectPeriStoredMessages,
  projectPeriStoredSubagentThreads,
  projectPeriStoredSubagents,
} from "@/lib/periStoredMessages";
import {
  shouldAdoptView,
  type ViewFocus,
} from "@/lib/viewFocus";
import type { Ref, ViewProjection } from "./types";

export interface AcpRuntimeHistoryOptions {
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  applyViewProjectionRef: Ref<ViewProjection>;
  commitWorkspace: () => void;
  currentViewFocus: () => ViewFocus;
}

/** 重放 ACP 扩展事件，再用 ThreadStore 消息重建稳定历史顺序。 */
export function useAcpRuntimeHistory({
  acpWorkspaceRef,
  applyViewProjectionRef,
  commitWorkspace,
  currentViewFocus,
}: AcpRuntimeHistoryOptions): {
  replayHistory: (sessionId: string, originView?: ViewFocus) => Promise<void>;
} {
  const replayHistory = useCallback(
    async (sessionId: string, originView?: ViewFocus) => {
      const view = acpWorkspaceRef.current.sessions[sessionId];
      if (!view || view.history.length > 0) return;
      const mayProjectView = () =>
        originView == null ||
        shouldAdoptView(originView, currentViewFocus(), sessionId);
      const restoreStoredHistory = async () => {
        const current = acpWorkspaceRef.current.sessions[sessionId];
        if (!current) return;
        try {
          const [stored, storedSubagents] = await Promise.all([
            sessionMessages(sessionId),
            sessionSubagents(sessionId),
          ]);
          const projected = projectPeriStoredMessages(stored);
          const history = projected.map((message) => ({
            role: message.role,
            content: message.content,
            thought: message.thought,
            segments: message.segments,
          }));
          if (history.length > 0) {
            current.history = history;
            current.subagents = storedSubagents.length
              ? projectPeriStoredSubagentThreads(storedSubagents)
              : projectPeriStoredSubagents(projected);
            current.live_segments = [];
            commitWorkspace();
            if (mayProjectView()) {
              applyViewProjectionRef.current(sessionId);
            }
          }
        } catch {
          /* ignore */
        }
      };
      try {
        const result = await sessionReplay({
          sessionId,
          after: null,
          limit: 500,
        });
        reduceReplayResult(view, result);
        commitWorkspace();
        if (mayProjectView()) {
          applyViewProjectionRef.current(sessionId);
        }
        if (result.replayed_events > 0) {
          // 通知通过窗口事件异步投递；等事件落地后再以 ThreadStore 顺序覆盖。
          await new Promise((resolve) => window.setTimeout(resolve, 150));
        }
        await restoreStoredHistory();
      } catch {
        await restoreStoredHistory();
      }
    },
    [commitWorkspace, currentViewFocus],
  );

  return { replayHistory };
}
