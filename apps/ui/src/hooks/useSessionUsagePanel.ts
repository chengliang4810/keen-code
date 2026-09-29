/** 会话上下文与任务缓存用量的前端投影状态。 */

import { useRef, useState, type Dispatch, type RefObject, type SetStateAction } from "react";
import * as api from "@/lib/api";
import type { SessionContextUsage } from "@/features/app/models";

/** `useSessionUsagePanel` 返回的用量状态。 */
export interface SessionUsagePanelState {
  /** 当前可见 Session 最近一次由 ACP 上报的上下文用量。 */
  contextUsage: SessionContextUsage | null;
  /** 更新可见会话用量的稳定 setter。 */
  setContextUsage: Dispatch<SetStateAction<SessionContextUsage | null>>;
  /** 每个 Session 最近一次由 ACP 上报的真实上下文用量。 */
  contextUsageBySessionRef: RefObject<Map<string, SessionContextUsage>>;
  /** 从本地请求记录恢复的当前任务整体缓存用量，可跨应用重启。 */
  taskCacheUsage: api.TaskCacheUsage | null;
  /** 更新任务缓存用量的稳定 setter。 */
  setTaskCacheUsage: Dispatch<SetStateAction<api.TaskCacheUsage | null>>;
  /** 缓存用量读取的并发序号；旧响应不得覆盖新响应。 */
  taskCacheUsageRequestSeqRef: RefObject<number>;
}

/** 管理可见会话上下文用量与任务缓存用量的投影状态。 */
export function useSessionUsagePanel(): SessionUsagePanelState {
  const [contextUsage, setContextUsage] =
    useState<SessionContextUsage | null>(null);
  const contextUsageBySessionRef = useRef<Map<string, SessionContextUsage>>(
    new Map(),
  );
  const [taskCacheUsage, setTaskCacheUsage] =
    useState<api.TaskCacheUsage | null>(null);
  const taskCacheUsageRequestSeqRef = useRef(0);

  return {
    contextUsage,
    setContextUsage,
    contextUsageBySessionRef,
    taskCacheUsage,
    setTaskCacheUsage,
    taskCacheUsageRequestSeqRef,
  };
}
