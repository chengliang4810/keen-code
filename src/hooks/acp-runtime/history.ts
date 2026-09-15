import { parseHistoryPage, prependHistoryPage, type HistoryPage } from "@/lib/acp/historyPages";
import { useCallback, useEffect, useRef } from "react";
import {
  diagnosticsRecord,
  goalGet,
  sessionConnect,
  sessionLoad,
  sessionSnapshotFromResult,
  type ReplayResult,
  type SessionSnapshot,
} from "@/lib/acp/api";
import { ensureAcpSession } from "@/lib/acp/projection";
import { modelIdFromSessionReference } from "@/lib/modelCatalog";
import {
  beginSessionRecovery,
  completeSessionRecovery,
  failSessionRecovery,
  reduceGoalSnapshot,
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
  /** 权威 Session 模型缓存；历史恢复与实时配置事件共用。 */
  modelBySessionRef: Ref<Map<string, string>>;
  /** 仅当前视图恢复完成时更新模型菜单。 */
  setModelId: (modelId: string) => void;
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
  modelBySessionRef,
  setModelId,
}: AcpRuntimeHistoryOptions): AcpRuntimeHistoryResult {
  /** 每个 Session 当前唯一恢复任务。 */
  const recoveryBySessionRef = useRef(new Map<string, Promise<void>>());
  /** 每个 Session 最近一次显式导航所期待的 Composer 焦点。 */
  const recoveryFocusBySessionRef = useRef(new Map<string, ViewFocus>());
  /** 已收到 Host 水位但仍在等待前端事件处理的恢复。 */
  const deliveryWaitersRef = useRef(new Map<string, DeliveryWaiter>());
  /** 卸载使旧异步恢复失效，迟到响应不能回写新的页面生命周期。 */
  const lifecycleEpochRef = useRef(0);
  /** 每次恢复建立新的投影世代；旧 Goal 查询不得写入后续恢复的投影。 */
  const recoveryGenerationBySessionRef = useRef(new Map<string, number>());
  const backfillBySessionRef = useRef(new Map<string, { cursor: string; view: object; running: boolean }>());


  useEffect(() => () => {
    lifecycleEpochRef.current += 1;
    for (const waiter of deliveryWaitersRef.current.values()) {
      waiter.reject(new Error("Session 历史恢复已取消"));
    }
    deliveryWaitersRef.current.clear();
    recoveryBySessionRef.current.clear();
    recoveryGenerationBySessionRef.current.clear();
    backfillBySessionRef.current.clear();
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

  const startBackfill = useCallback((sessionId: string, first: HistoryPage, publish: () => void) => {
    if (!first.nextCursor) return;
    const view = acpWorkspaceRef.current.sessions[sessionId];
    const job = { cursor: first.nextCursor, view, running: true };
    backfillBySessionRef.current.set(sessionId, job);
    const epoch = lifecycleEpochRef.current;
    const valid = () => epoch === lifecycleEpochRef.current &&
      backfillBySessionRef.current.get(sessionId) === job &&
      acpWorkspaceRef.current.sessions[sessionId] === view && !view.delivery.frozen;
    void (async () => {
      try {
        while (valid()) {
          // 给首屏和每批渲染留出事件循环，不用 Promise 微任务连续占住主线程。
          await new Promise<void>((resolve) => setTimeout(resolve, 16));
          if (!valid()) return;
          // 根回合可能包含大量工具输出；保持与首屏相同的小窗口，避免多个
          // 合法回合合并后超过 ACP 单响应 1 MiB 的边界。
          const loaded = await sessionLoad(sessionId, { limit: 2, cursor: job.cursor });
          if (!valid()) return;
          const page = parseHistoryPage(loaded._meta, sessionId);
          if (page.nextCursor === job.cursor) throw new Error("历史分页游标未推进");
          prependHistoryPage(view, page);
          publish();
          if (!page.nextCursor) {
            backfillBySessionRef.current.delete(sessionId);
            return;
          }
          job.cursor = page.nextCursor;
        }
      } catch (error) {
        if (valid()) {
          // 首屏和实时流保持可用；保留游标，重新进入此对话可重试。
          job.running = false;
          view.last_error = { code: "history_load_failed", message: error instanceof Error ? error.message : String(error) };
          publish();
        }
      }
    })();
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
      const recoveryGeneration =
        (recoveryGenerationBySessionRef.current.get(sessionId) ?? 0) + 1;
      recoveryGenerationBySessionRef.current.set(sessionId, recoveryGeneration);
      const mayProjectView = () =>
        shouldAdoptView(recoveryOrigin, currentViewFocus(), sessionId);
      const publish = () => {
        commitWorkspace();
        if (mayProjectView()) applyViewProjectionRef.current(sessionId);
      };
      const recovery = (async () => {
        const view = ensureAcpSession(acpWorkspaceRef.current, sessionId);
        const visibleBeforeRecovery = view.replay.loaded ? structuredClone(view) : null;
        const visibleStatusBeforeRecovery = visibleBeforeRecovery?.status;
        const isCurrentProjection = () =>
          lifecycleEpoch === lifecycleEpochRef.current &&
          recoveryGenerationBySessionRef.current.get(sessionId) === recoveryGeneration &&
          acpWorkspaceRef.current.sessions[sessionId] === view;
        invalidateContextUsage(sessionId);
        const latency = turnLatencyBySessionRef.current.get(sessionId);
        if (latency) {
          turnLatencyBySessionRef.current.set(sessionId, reduceTurnLatency(latency, {
            type: "delivery_interrupted", turnId: latency.turnId,
          }));
        }
        pendingVisibleTurnBySessionRef.current.delete(sessionId);
        backfillBySessionRef.current.delete(sessionId);
        beginSessionRecovery(view);
        publish();
        try {
          const started = performance.now();
          const loaded = await sessionLoad(sessionId, { limit: 2 });
          const hostCompleted = performance.now();
          if (!isCurrentProjection()) throw new Error("Session 历史恢复已取消");
          const replay = completedLoadReplay(loaded._meta, sessionId);
          const page = parseHistoryPage(loaded._meta, sessionId);
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
          const modelValue = loaded.configOptions.find((option) => option.id === "model")?.currentValue;
          if (typeof modelValue === "string" && modelValue.length > 0) {
            modelBySessionRef.current.set(sessionId, modelIdFromSessionReference(modelValue));
          }
          const current = acpWorkspaceRef.current.sessions[sessionId];
          if (!current || !isCurrentProjection()) throw new Error("Session 恢复完成前投影已替换");
          // 首页 load 建立投递世代，旧页独立归约，不再从零 replay。
          reduceReplayResult(current, replay);
          await awaitDelivery(sessionId, replay.throughDeliverySequence);
          const deliveryCompleted = performance.now();
          if (!isCurrentProjection() || acpWorkspaceRef.current.sessions[sessionId] !== current) {
            throw new Error("Session 恢复期间投影已替换");
          }
          // 历史恢复期间 Goal 事件会因世代门禁被丢弃；只有重新取得并归约
          // 当前 Session 的权威快照后，才能宣布恢复成功。
          const goalSnapshot = await goalGet(sessionId);
          if (!isCurrentProjection() || goalSnapshot?.sessionId !== sessionId) {
            throw new Error("Session 恢复 Goal 快照标识不一致");
          }
          if (!Number.isSafeInteger(goalSnapshot.revision) || goalSnapshot.revision < 0) {
            throw new Error("Session 恢复 Goal 修订号无效");
          }
          // 恢复期间可能已经观察到更高修订；无法确认当前快照完整性时必须
          // 失败并冻结本次恢复，交给下一次恢复重新取得权威快照。
          if (!reduceGoalSnapshot(current, goalSnapshot.revision, goalSnapshot.goal ?? null)) {
            throw new Error("Session 恢复 Goal 快照修订号落后");
          }
          if (!isCurrentProjection()) throw new Error("Session 恢复 Goal 投影已替换");
          completeSessionRecovery(current);
          current.replay.hasMore = page.hasMore;
          publish();
          startBackfill(sessionId, page, publish);
          void diagnosticsRecord("session_load", JSON.stringify({
            sessionId,
            requestMs: Math.round(hostCompleted - started),
            deliveryWaitMs: Math.round(deliveryCompleted - hostCompleted),
            projectionMs: Math.round(performance.now() - deliveryCompleted),
          })).catch(() => {});
          // 缓存属于所有会话；迟到的后台恢复不得改写前台或新草稿菜单。
          if (mayProjectView()) {
            const model = modelBySessionRef.current.get(sessionId);
            if (model) setModelId(model);
          }
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
          if (current && isCurrentProjection()) {
            const message = error instanceof Error ? error.message : String(error);
            if (visibleBeforeRecovery) {
              acpWorkspaceRef.current.sessions[sessionId] = visibleBeforeRecovery;
              failSessionRecovery(visibleBeforeRecovery, message);
              visibleBeforeRecovery.status = visibleStatusBeforeRecovery ?? "ready";
            } else {
              failSessionRecovery(current, message);
            }
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
      modelBySessionRef,
      setModelId,
      awaitDelivery,
      startBackfill,
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
        const job = backfillBySessionRef.current.get(sessionId);
        if (job && !job.running) {
          // 上次页可能已在 Host 提交；重新建立首页，避免盲重试过期游标。
          await recoverSession(sessionId, originView);
          return;
        }
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
    [currentViewFocus, recoverSession, setPlanModeSessionKey, startBackfill, commitWorkspace],
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
    const existing = acpWorkspaceRef.current.sessions[args.sessionId];
    if (!existing?.replay.loaded || existing.replay.restoring) {
      await recoverSession(args.sessionId, currentViewFocus());
    }
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
