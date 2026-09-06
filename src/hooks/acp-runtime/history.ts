import { useCallback, useEffect, useRef } from "react";
import {
  sessionConnect,
  sessionLoad,
  sessionSnapshotFromResult,
  type ReplayResult,
  type SessionSnapshot,
} from "@/lib/acp/api";
import { ensureAcpSession } from "@/lib/acp/projection";
import {
  beginSessionRecovery,
  completeSessionRecovery,
  failSessionRecovery,
  reduceReplayResult,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import {
  isSameView,
  shouldAdoptView,
  type ViewFocus,
} from "@/lib/viewFocus";
import type { Ref, ViewProjection } from "./types";
import { reduceTurnLatency, type TurnLatencyState } from "@/lib/turnLatency";
import { projectAcpSnapshot } from "@/lib/sessionProjection";

/** Host 已返回后，界面消费投递队列的有界等待；超时拒绝，不伪造恢复成功。 */
export const SESSION_DELIVERY_RECOVERY_TIMEOUT_MS = 30_000;

/** 单个 Session 等待共享 Reducer 消费历史水位的回执。 */
interface DeliveryWaiter {
  /** 目标是当前投递世代序号，不是 Journal 序号。 */
  through: number;
  /** 水位满足后结束等待，并清理计时器。 */
  resolve: () => void;
  /** 缺口、卸载或超时拒绝当前恢复。 */
  reject: (error: Error) => void;
}

/** Session load/replay 恢复 Hook 的依赖。 */
export interface AcpRuntimeHistoryOptions {
  /** 当前 ACP UI 投影。 */
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  /** 当前实时计时，恢复只保留已经观测到的时间，禁止补造首次观测。 */
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  /** 恢复前撤销当前 Session 尚未完成的 DOM 可见性等待。 */
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  /** 把指定 Session 投影到当前界面的稳定引用。 */
  applyViewProjectionRef: Ref<ViewProjection>;
  /** 发布已在引用中完成的工作区变更。 */
  commitWorkspace: () => void;
  /** 返回当前界面焦点，用于阻止迟到恢复覆盖用户导航。 */
  currentViewFocus: () => ViewFocus;
  /** 在开始恢复前清除该 Session 的旧上下文用量。 */
  invalidateContextUsage: (sessionId: string) => void;
  /** 将恢复出的持久 Plan 模式同步到 Composer 的当前 Session 键。 */
  setPlanModeSessionKey: (sessionKey: string | null) => void;
}

/** 一次恢复公开的两个入口。 */
export interface AcpRuntimeHistoryResult {
  /** 新建会话或完整恢复既有会话；连接入口不得另行触发无消费屏障的 load。 */
  connectSession: typeof sessionConnect;
  /** 初次打开空投影时恢复完整历史。 */
  replayHistory: (sessionId: string, originView?: ViewFocus) => Promise<void>;
  /** 检测到投递缺口后强制丢弃不可信投影并重建。 */
  recoverSession: (sessionId: string, originView?: ViewFocus) => Promise<void>;
  /** 共享 Reducer 处理投递后通知等待方；不能由 invoke 响应代替。 */
  observeSessionDelivery: (sessionId: string) => void;
}

/**
 * 标准 `session/load` 返回只证明 Host 已投递，前端还必须消费到实际投递水位。
 * 每个 Session 同时最多运行一个恢复任务；实时与 replay 事件共用 Store Reducer。
 */
export function useAcpRuntimeHistory({
  acpWorkspaceRef,
  turnLatencyBySessionRef,
  pendingVisibleTurnBySessionRef,
  applyViewProjectionRef,
  commitWorkspace,
  currentViewFocus,
  invalidateContextUsage,
  setPlanModeSessionKey,
}: AcpRuntimeHistoryOptions): AcpRuntimeHistoryResult {
  /** 每个 Session 当前唯一恢复任务。 */
  const recoveryBySessionRef = useRef(new Map<string, Promise<void>>());
  /** 每个 Session 最近一次显式导航所期待的 Composer 焦点。 */
  const recoveryFocusBySessionRef = useRef(new Map<string, ViewFocus>());
  /** 已收到 Host 水位但仍在等待前端事件处理的恢复。 */
  const deliveryWaitersRef = useRef(new Map<string, DeliveryWaiter>());
  /** 卸载使旧异步恢复失效，迟到响应不能回写新的页面生命周期。 */
  const lifecycleEpochRef = useRef(0);

  useEffect(() => () => {
    lifecycleEpochRef.current += 1;
    for (const waiter of deliveryWaitersRef.current.values()) {
      waiter.reject(new Error("Session 历史恢复已取消"));
    }
    deliveryWaitersRef.current.clear();
    recoveryBySessionRef.current.clear();
    recoveryFocusBySessionRef.current.clear();
  }, []);

  const observeSessionDelivery = useCallback((sessionId: string) => {
    const waiter = deliveryWaitersRef.current.get(sessionId);
    if (!waiter) return;
    const view = acpWorkspaceRef.current.sessions[sessionId];
    if (!view || view.delivery.frozen) {
      waiter.reject(new Error("Session 历史投递出现缺口"));
    } else if ((view.delivery.lastSequence ?? 0) >= waiter.through) {
      waiter.resolve();
    }
  }, []);

  /** 先检查已消费水位，兼容事件早于控制响应到达；不使用轮询或固定延时。 */
  const awaitDelivery = useCallback((sessionId: string, through: number) => {
    const view = acpWorkspaceRef.current.sessions[sessionId];
    if (!view || view.delivery.frozen) {
      return Promise.reject(new Error("Session 历史投递出现缺口"));
    }
    if ((view.delivery.lastSequence ?? 0) >= through) return Promise.resolve();
    return new Promise<void>((resolve, reject) => {
      const finish = (error?: Error) => {
        clearTimeout(timer);
        deliveryWaitersRef.current.delete(sessionId);
        if (error) reject(error);
        else resolve();
      };
      const timer = setTimeout(() => finish(new Error("Session 历史投递等待超时")), SESSION_DELIVERY_RECOVERY_TIMEOUT_MS);
      deliveryWaitersRef.current.set(sessionId, {
        through,
        resolve: () => finish(),
        reject: finish,
      });
    });
  }, []);

  const recoverSession = useCallback(
    (sessionId: string, originView?: ViewFocus): Promise<void> => {
      const existing = recoveryBySessionRef.current.get(sessionId);
      if (existing) {
        if (originView) {
          recoveryFocusBySessionRef.current.set(sessionId, originView);
        }
        return existing;
      }
      if (originView) {
        recoveryFocusBySessionRef.current.set(sessionId, originView);
      } else {
        recoveryFocusBySessionRef.current.delete(sessionId);
      }
      const recoveryOrigin = originView ?? currentViewFocus();
      const lifecycleEpoch = lifecycleEpochRef.current;
      const mayProjectView = () =>
        shouldAdoptView(recoveryOrigin, currentViewFocus(), sessionId);
      const publish = () => {
        commitWorkspace();
        if (mayProjectView()) applyViewProjectionRef.current(sessionId);
      };
      const recovery = (async () => {
        const view = ensureAcpSession(acpWorkspaceRef.current, sessionId);
        invalidateContextUsage(sessionId);
        const latency = turnLatencyBySessionRef.current.get(sessionId);
        if (latency) {
          turnLatencyBySessionRef.current.set(sessionId, reduceTurnLatency(latency, {
            type: "delivery_interrupted", turnId: latency.turnId,
          }));
        }
        pendingVisibleTurnBySessionRef.current.delete(sessionId);
        beginSessionRecovery(view);
        publish();
        try {
          const loaded = await sessionLoad(sessionId);
          if (lifecycleEpoch !== lifecycleEpochRef.current) throw new Error("Session 历史恢复已取消");
          const replay = completedLoadReplay(loaded._meta, sessionId);
          const snapshot = sessionSnapshotFromResult(loaded);
          if (snapshot.sessionId !== sessionId) throw new Error("Session 恢复快照标识不一致");
          const mode = loaded.modes?.currentModeId;
          if ((mode !== "default" && mode !== "plan") ||
            !Array.isArray(loaded.modes?.availableModes) ||
            loaded.modes.availableModes.some((item) => !item ||
              (item.id !== "default" && item.id !== "plan") ||
              typeof item.name !== "string" || !item.name.trim())) {
            throw new Error("Session 恢复模式字段无效");
          }
          view.project_path = snapshot.projectPath ?? null;
          view.title = snapshot.title ?? null;
          view.plan_mode = mode === "plan";
          const current = acpWorkspaceRef.current.sessions[sessionId];
          if (!current) throw new Error("Session 恢复完成前投影已移除");
          // load 是完整历史的唯一所有者，禁止再次从零 replay 重置投递世代。
          reduceReplayResult(current, replay);
          await awaitDelivery(sessionId, replay.throughDeliverySequence);
          if (lifecycleEpoch !== lifecycleEpochRef.current) throw new Error("Session 历史恢复已取消");
          if (acpWorkspaceRef.current.sessions[sessionId] !== current) throw new Error("Session 恢复期间投影已替换");
          completeSessionRecovery(current);
          publish();
          // 只有最终恢复出的当前 Session 才能改变 Composer；后台恢复不能覆盖
          // 用户当前会话或尚未提交的新草稿的本地模式选择；草稿实体化不是显式导航。
          const expectedFocus = recoveryFocusBySessionRef.current.get(sessionId);
          const focus = currentViewFocus();
          if (
            expectedFocus &&
            expectedFocus.sessionId === sessionId &&
            focus.sessionId === sessionId &&
            isSameView(expectedFocus, focus)
          ) {
            setPlanModeSessionKey(current.plan_mode ? sessionId : null);
          }
        } catch (error) {
          if (lifecycleEpoch !== lifecycleEpochRef.current) throw error;
          const current = acpWorkspaceRef.current.sessions[sessionId];
          if (current) {
            failSessionRecovery(
              current,
              error instanceof Error ? error.message : String(error),
            );
            publish();
          }
          throw error;
        } finally {
          if (lifecycleEpoch === lifecycleEpochRef.current) {
            recoveryBySessionRef.current.delete(sessionId);
            recoveryFocusBySessionRef.current.delete(sessionId);
          }
        }
      })();
      recoveryBySessionRef.current.set(sessionId, recovery);
      return recovery;
    }, [
      commitWorkspace,
      currentViewFocus,
      invalidateContextUsage,
      setPlanModeSessionKey,
      awaitDelivery,
    ],
  );

  const replayHistory = useCallback(
    async (sessionId: string, originView?: ViewFocus): Promise<void> => {
      const view = acpWorkspaceRef.current.sessions[sessionId];
      if (view?.replay.restoring) {
        const recovery = recoveryBySessionRef.current.get(sessionId);
        if (originView && recovery) {
          recoveryFocusBySessionRef.current.set(sessionId, originView);
        }
        if (recovery) await recovery;
        else await recoverSession(sessionId, originView);
        return;
      }
      if (view?.replay.loaded) {
        // 已由后台恢复的 Session 不会再次触发 load；切回时仍需把持久模式
        // 投影回 Composer。当前会话的本地未提交模式不受后台恢复影响。
        const focus = currentViewFocus();
        if (
          originView &&
          originView.sessionId === sessionId &&
          focus.sessionId === sessionId &&
          isSameView(originView, focus)
        ) {
          setPlanModeSessionKey(view.plan_mode ? sessionId : null);
        }
        return;
      }
      await recoverSession(sessionId, originView);
    },
    [currentViewFocus, recoverSession, setPlanModeSessionKey],
  );

  /** 既有会话仅由恢复 Hook 加载一次；新会话没有历史，不再二次 load 重置世代。 */
  const connectSession = useCallback<typeof sessionConnect>(async (args) => {
    if (!args.sessionId) {
      const opened = await sessionConnect(args);
      if (opened.sessionId) {
        const view = ensureAcpSession(acpWorkspaceRef.current, opened.sessionId);
        view.project_path = opened.projectPath ?? null;
        view.replay.loaded = true;
      }
      return opened;
    }
    await recoverSession(args.sessionId, currentViewFocus());
    const view = acpWorkspaceRef.current.sessions[args.sessionId];
    if (!view || view.delivery.frozen) throw new Error("Session 历史恢复未完成");
    const projected = projectAcpSnapshot(view);
    const snapshot: SessionSnapshot = {
      sessionId: args.sessionId, state: projected.state,
      activeTurnId: view.active_root_turn_id, backend: "acp",
      projectPath: view.project_path ?? args.projectPath ?? null,
      title: view.title, lastError: view.last_error?.message ?? null,
    };
    return snapshot;
  }, [recoverSession, currentViewFocus]);

  return { replayHistory, recoverSession, observeSessionDelivery, connectSession };
}

/** 校验 Host 随标准 load 返回的恢复完成事实，拒绝残缺或串会话水位。 */
function completedLoadReplay(
  meta: Record<string, unknown> | undefined,
  sessionId: string,
): ReplayResult {
  const raw = meta?.["keencode/replay"];
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("ACP load 缺少历史恢复完成水位");
  }
  const value = raw as Record<string, unknown>;
  const fields = ["startAfter", "nextAfter", "throughJournalSequence", "throughDeliverySequence", "replayedEvents"] as const;
  if (value.sessionId !== sessionId || value.hasMore !== false ||
    fields.some((field) => typeof value[field] !== "number" ||
      !Number.isSafeInteger(value[field]) || (value[field] as number) < 0)) {
    throw new Error("ACP load 历史恢复控制信息无效");
  }
  if ((value.startAfter as number) > (value.nextAfter as number) ||
    value.nextAfter !== value.throughJournalSequence ||
    (value.replayedEvents as number) > (value.throughDeliverySequence as number)) {
    throw new Error("ACP load 历史尚未完整恢复");
  }
  return value as unknown as ReplayResult;
}
