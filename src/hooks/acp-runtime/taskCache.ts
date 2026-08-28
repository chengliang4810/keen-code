import { useCallback, useEffect } from "react";
import * as api from "@/lib/api";
import type { SessionSnapshot } from "@/lib/session";
import type { SessionContextUsage } from "@/features/app/models";
import type { Ref, SetState } from "./types";

export interface AcpRuntimeTaskCacheOptions {
  session: SessionSnapshot;
  viewingSessionIdRef: Ref<string | null>;
  contextUsageBySessionRef: Ref<Map<string, SessionContextUsage>>;
  taskCacheUsageRequestSeqRef: Ref<number>;
  setContextUsage: SetState<SessionContextUsage | null>;
  setTaskCacheUsage: SetState<api.TaskCacheUsage | null>;
}

/** 读取任务范围缓存率，并用序号隔离快速切换会话后的迟到结果。 */
export function useAcpRuntimeTaskCache({
  session,
  viewingSessionIdRef,
  contextUsageBySessionRef,
  taskCacheUsageRequestSeqRef,
  setContextUsage,
  setTaskCacheUsage,
}: AcpRuntimeTaskCacheOptions): {
  refreshTaskCacheUsage: (sessionId: string | null) => Promise<void>;
} {
  const refreshTaskCacheUsage = useCallback(async (sessionId: string | null) => {
    const requestSeq = ++taskCacheUsageRequestSeqRef.current;
    if (!sessionId || !api.isTauri()) {
      setTaskCacheUsage(null);
      return;
    }
    try {
      const usage = await api.taskCacheUsageGet(sessionId);
      if (
        requestSeq === taskCacheUsageRequestSeqRef.current &&
        viewingSessionIdRef.current === sessionId
      ) {
        setTaskCacheUsage(usage);
        if (
          !contextUsageBySessionRef.current.has(sessionId) &&
          usage.latestContextTokens != null
        ) {
          const restoredUsage = {
            used: usage.latestContextTokens,
            estimated: usage.latestContextEstimated,
          };
          contextUsageBySessionRef.current.set(sessionId, restoredUsage);
          setContextUsage(restoredUsage);
        }
      }
    } catch (error) {
      if (
        requestSeq === taskCacheUsageRequestSeqRef.current &&
        viewingSessionIdRef.current === sessionId
      ) {
        setTaskCacheUsage(null);
      }
      console.warn("load task cache usage failed", error);
    }
  }, []);

  useEffect(() => {
    void refreshTaskCacheUsage(session.sessionId);
  }, [refreshTaskCacheUsage, session.sessionId]);

  return { refreshTaskCacheUsage };
}
