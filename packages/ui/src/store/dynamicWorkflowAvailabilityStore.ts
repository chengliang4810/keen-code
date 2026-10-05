import { create } from "zustand";
import {
  createDynamicWorkflowClientConfig,
  type DynamicWorkflowClientConfig,
} from "@zcode/shared";

// ============================================================
// 动态工作流灰度快照在 renderer 的唯一副本
// ============================================================
//
// 本地前端不再向 Coding Plan/云端服务取灰度配置。Rust Agent 持有动态工作流事实，
// renderer 直接展示协议回放的 JSON 工作流；默认配置只负责让完整入口可见。

export type DynamicWorkflowAvailabilityStatus = "loading" | "ready";

export interface DynamicWorkflowAvailabilitySnapshot {
  readonly status: DynamicWorkflowAvailabilityStatus;
  /** loading 期间恒为 false：未知即不提供，入口宁可晚半拍出现也不闪一下再收起。 */
  readonly enabled: boolean;
  /** 未就绪或取数失败时为 null；`source` 只用于观测，区分「服务端关」与「本地覆盖」。 */
  readonly config: DynamicWorkflowClientConfig | null;
}

interface DynamicWorkflowAvailabilityState extends DynamicWorkflowAvailabilitySnapshot {
  ensureLoaded(): Promise<void>;
  refresh(): Promise<void>;
}

const INITIAL_SNAPSHOT: DynamicWorkflowAvailabilitySnapshot = {
  status: "ready",
  enabled: true,
  config: createDynamicWorkflowClientConfig("alwaysOn", "default"),
};

export const useDynamicWorkflowAvailabilityStore = create<DynamicWorkflowAvailabilityState>(
  (set, get) => ({
    ...INITIAL_SNAPSHOT,

    ensureLoaded(): Promise<void> {
      return Promise.resolve();
    },

    refresh(): Promise<void> {
      set(INITIAL_SNAPSHOT);
      return Promise.resolve();
    },
  }),
);
