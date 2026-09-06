import { createElement, type EffectCallback } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ReplayResult,
  SessionLoadResult,
  SessionSnapshot,
} from "@/lib/acp/api";
import type {
  AcpDeliveryEnvelope,
  SessionUpdate,
} from "@/lib/acp/events";
import {
  ensureAcpSession,
} from "@/lib/acp/projection";
import {
  createAcpWorkspaceState,
  reduceDeliveryEnvelope,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import type { ViewFocus } from "@/lib/viewFocus";
import {
  SESSION_DELIVERY_RECOVERY_TIMEOUT_MS,
  useAcpRuntimeHistory,
} from "./history";

/** 仅替换控制请求与 effect 调度；历史投递仍通过生产 Reducer 进入工作区。 */
const ports = vi.hoisted(() => ({
  /** 模拟标准 `session/load` 控制请求。 */
  sessionLoad: vi.fn(),
  /** 模拟新建 Session 的连接请求。 */
  sessionConnect: vi.fn(),
  /** SSR 渲染不会运行的 effect 队列，由测试显式调度。 */
  effects: [] as EffectCallback[],
}));

vi.mock("react", async (original) => ({
  ...await original<typeof import("react")>(),
  useEffect: (effect: EffectCallback) => {
    ports.effects.push(effect);
  },
}));

vi.mock("@/lib/acp/api", async (original) => ({
  ...await original<typeof import("@/lib/acp/api")>(),
  sessionLoad: ports.sessionLoad,
  sessionConnect: ports.sessionConnect,
}));

/** 可由测试显式控制完成顺序的 Promise。 */
interface Deferred<Value> {
  /** 等待测试显式完成的 Promise。 */
  promise: Promise<Value>;
  /** 以成功值完成 Promise。 */
  resolve: (value: Value | PromiseLike<Value>) => void;
  /** 以失败原因完成 Promise。 */
  reject: (reason?: unknown) => void;
}

/** 创建可在任意时刻完成或失败的测试 Promise。 */
function deferred<Value>(): Deferred<Value> {
  let resolve!: (value: Value | PromiseLike<Value>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<Value>((accept, fail) => {
    resolve = accept;
    reject = fail;
  });
  return { promise, resolve, reject };
}

/** 构造默认合法的恢复控制水位。 */
function replayResult(
  sessionId: string,
  overrides: Partial<ReplayResult> = {},
): ReplayResult {
  return {
    sessionId,
    startAfter: 0,
    nextAfter: 0,
    throughJournalSequence: 0,
    throughDeliverySequence: 0,
    replayedEvents: 0,
    hasMore: false,
    ...overrides,
  };
}

/** 构造携带标准 Session 快照和历史投递水位的 `session/load` 响应。 */
function loadResult(
  sessionId: string,
  replay: ReplayResult = replayResult(sessionId),
): SessionLoadResult {
  return {
    modes: {
      currentModeId: "default",
      availableModes: [
        { id: "default", name: "Default" },
        { id: "plan", name: "Plan" },
      ],
    },
    configOptions: [],
    _meta: {
      "keencode/replay": replay,
      "keencode/snapshot": {
        sessionId,
        state: "ready",
        activeTurnId: null,
        backend: "acp",
        projectPath: "D:/fixture",
        title: "历史会话",
        lastError: null,
      },
    },
  };
}

/** 构造新建 Session 成功返回的最小原生快照。 */
function newSessionSnapshot(sessionId: string): SessionSnapshot {
  return {
    sessionId,
    state: "ready",
    activeTurnId: null,
    backend: "acp",
    projectPath: "D:/new-session",
    title: null,
    lastError: null,
  };
}

/** 构造标准文本更新，交付时必须经过生产 Reducer 的顺序门禁。 */
function textUpdate(text: string): SessionUpdate {
  return {
    sessionUpdate: "agent_message_chunk",
    content: { type: "text", text },
  };
}

/** 构造当前 Session 的合法标准投递信封。 */
function deliveryEnvelope(
  sessionId: string,
  deliverySequence: number,
): AcpDeliveryEnvelope {
  return {
    schemaVersion: 1,
    sessionId,
    turnId: "turn-history",
    deliverySequence,
    occurredAtMs: 1_000 + deliverySequence,
    update: textUpdate(`历史投递 ${deliverySequence}`),
  };
}

/** 通过真实 `reduceDeliveryEnvelope` 归约投递，再通知恢复 Hook 消费水位。 */
function deliver(
  harness: ReturnType<typeof createHistoryHarness>,
  sessionId: string,
  deliverySequence: number,
) {
  const view = ensureAcpSession(harness.workspaceRef.current, sessionId);
  const reduction = reduceDeliveryEnvelope(
    view,
    deliveryEnvelope(sessionId, deliverySequence),
  );
  harness.history.observeSessionDelivery(sessionId);
  return reduction;
}

/** 让 await 链依次经过若干轮微任务，直到 load waiter 可观察。 */
async function flushMicrotasks(rounds = 5): Promise<void> {
  for (let index = 0; index < rounds; index += 1) {
    await Promise.resolve();
  }
}

/** 保存每个 Harness 的 effect 清理函数，避免测试残留卸载回调或计时器。 */
const disposers: Array<() => void> = [];

/** 渲染真实 history Hook，并显式执行 SSR 中被跳过的 effect。 */
function createHistoryHarness(initialFocus: ViewFocus) {
  /** 当前 ACP 工作区引用，模拟应用层共享的可变 Store。 */
  const workspaceRef: { current: AcpWorkspaceState } = {
    current: createAcpWorkspaceState(),
  };
  /** 恢复过程的可观察投影事件。 */
  const events: string[] = [];
  /** 模拟当前导航焦点。 */
  let focus = initialFocus;
  /** 捕获 Hook 返回的恢复入口。 */
  let history!: ReturnType<typeof useAcpRuntimeHistory>;
  /** 记录提交工作区的次数，不直接替换生产投影。 */
  const commitWorkspace = vi.fn(() => {
    events.push("commit");
  });
  /** 记录界面投影发布，不直接把 Session 标记为成功。 */
  const applyViewProjection = vi.fn((sessionId: string | null) => {
    events.push(`apply:${sessionId}`);
  });
  /** 记录上下文用量失效。 */
  const invalidateContextUsage = vi.fn((sessionId: string) => {
    events.push(`invalidate:${sessionId}`);
  });
  /** 记录持久 Plan 模式同步。 */
  const setPlanModeSessionKey = vi.fn((sessionKey: string | null) => {
    events.push(`set-plan:${sessionKey ?? "none"}`);
  });

  /** 在合法 React 渲染上下文中装配 history Hook。 */
  function Harness() {
    history = useAcpRuntimeHistory({
      acpWorkspaceRef: workspaceRef,
      turnLatencyBySessionRef: { current: new Map() },
      pendingVisibleTurnBySessionRef: { current: new Map() },
      applyViewProjectionRef: { current: applyViewProjection },
      commitWorkspace,
      currentViewFocus: () => focus,
      invalidateContextUsage,
      setPlanModeSessionKey,
    });
    return null;
  }

  const effectStart = ports.effects.length;
  renderToString(createElement(Harness));
  const ownEffects = ports.effects.splice(effectStart);
  const ownCleanups: Array<() => void> = [];
  for (const effect of ownEffects) {
    const cleanup = effect();
    if (cleanup) ownCleanups.push(cleanup);
  }
  if (!history) throw new Error("未捕获 ACP history Hook 结果");

  /** 模拟组件卸载，并保证清理函数只执行一次。 */
  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    for (const cleanup of ownCleanups.splice(0)) cleanup();
  };
  disposers.push(dispose);

  return {
    history,
    workspaceRef,
    events,
    commitWorkspace,
    applyViewProjection,
    invalidateContextUsage,
    setPlanModeSessionKey,
    /** 更新模拟导航焦点，复现恢复等待期间的用户导航。 */
    setFocus(next: ViewFocus) {
      focus = next;
    },
    dispose,
  };
}

beforeEach(() => {
  ports.sessionLoad.mockReset();
  ports.sessionConnect.mockReset();
  ports.effects.length = 0;
  vi.useRealTimers();
});

afterEach(() => {
  for (const dispose of disposers.splice(0)) dispose();
  vi.useRealTimers();
});

describe("useAcpRuntimeHistory 的 Session delivery barrier", () => {
  it("控制响应先到时只等待最终 delivery 水位，Journal 尾部可大于 delivery", async () => {
    const sessionId = "session-watermark";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    ports.sessionLoad.mockReturnValue(pendingLoad.promise);

    const recovery = harness.history.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await flushMicrotasks();
    expect(ports.sessionLoad).toHaveBeenCalledOnce();

    pendingLoad.resolve(loadResult(sessionId, replayResult(sessionId, {
      nextAfter: 10,
      throughJournalSequence: 10,
      throughDeliverySequence: 2,
      // Journal 尾部包含没有 UI 投影的记录，不能用 replayedEvents 代替 delivery 水位。
      replayedEvents: 0,
    })));
    await flushMicrotasks();

    const view = harness.workspaceRef.current.sessions[sessionId]!;
    expect(view.replay.throughJournalSequence).toBe(10);
    expect(view.replay.throughDeliverySequence).toBe(2);
    expect(view.delivery.lastSequence).toBe(null);
    expect(view.replay.restoring).toBe(true);

    expect(deliver(harness, sessionId, 1).status).toBe("applied");
    await flushMicrotasks();
    expect(view.delivery.lastSequence).toBe(1);
    expect(view.replay.restoring).toBe(true);

    expect(deliver(harness, sessionId, 2).status).toBe("applied");
    await recovery;
    expect(view.delivery.lastSequence).toBe(2);
    expect(view.replay.loaded).toBe(true);
    expect(view.replay.restoring).toBe(false);
  });

  it("事件先于控制响应到达时，响应返回后立即通过同一 delivery 水位", async () => {
    const sessionId = "session-event-first";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    ports.sessionLoad.mockReturnValue(pendingLoad.promise);

    const recovery = harness.history.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await flushMicrotasks();

    expect(deliver(harness, sessionId, 1).status).toBe("applied");
    const view = harness.workspaceRef.current.sessions[sessionId]!;
    expect(view.delivery.lastSequence).toBe(1);
    expect(view.replay.restoring).toBe(true);

    pendingLoad.resolve(loadResult(sessionId, replayResult(sessionId, {
      nextAfter: 4,
      throughJournalSequence: 4,
      throughDeliverySequence: 1,
    })));
    await recovery;

    expect(view.replay.loaded).toBe(true);
    expect(view.delivery.lastSequence).toBe(1);
    expect(view.replay.restoring).toBe(false);
  });

  it("无 origin 的并发 replayHistory 共享同一个 load 并共同等待消费", async () => {
    const sessionId = "session-concurrent";
    const harness = createHistoryHarness({ sessionId: null, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    ports.sessionLoad.mockReturnValue(pendingLoad.promise);

    const first = harness.history.replayHistory(sessionId);
    const second = harness.history.replayHistory(sessionId);
    await flushMicrotasks();
    expect(ports.sessionLoad).toHaveBeenCalledOnce();

    pendingLoad.resolve(loadResult(sessionId, replayResult(sessionId, {
      nextAfter: 1,
      throughJournalSequence: 1,
      throughDeliverySequence: 1,
    })));
    await flushMicrotasks();
    expect(harness.workspaceRef.current.sessions[sessionId]?.replay.restoring).toBe(true);

    deliver(harness, sessionId, 1);
    await Promise.all([first, second]);
    expect(harness.workspaceRef.current.sessions[sessionId]?.replay.loaded).toBe(true);
    expect(ports.sessionLoad).toHaveBeenCalledOnce();
  });

  it("跨 Session 的 delivery 不能释放当前 waiter", async () => {
    const sessionId = "session-foreground";
    const otherSessionId = "session-other";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    ports.sessionLoad.mockReturnValue(pendingLoad.promise);

    const recovery = harness.history.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await flushMicrotasks();
    pendingLoad.resolve(loadResult(sessionId, replayResult(sessionId, {
      throughDeliverySequence: 1,
    })));
    await flushMicrotasks();

    expect(deliver(harness, otherSessionId, 1).status).toBe("applied");
    expect(harness.workspaceRef.current.sessions[sessionId]?.replay.restoring).toBe(true);

    deliver(harness, sessionId, 1);
    await recovery;
    expect(harness.workspaceRef.current.sessions[sessionId]?.replay.loaded).toBe(true);
  });

  it("检测到 delivery gap 后冻结并拒绝恢复", async () => {
    const sessionId = "session-gap";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    ports.sessionLoad.mockResolvedValue(loadResult(sessionId, replayResult(sessionId, {
      throughDeliverySequence: 2,
    })));

    const recovery = harness.history.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await flushMicrotasks();
    expect(deliver(harness, sessionId, 1).status).toBe("applied");
    const reduction = deliver(harness, sessionId, 3);
    expect(reduction).toMatchObject({
      status: "gap",
      expectedSequence: 2,
      receivedSequence: 3,
    });

    await expect(recovery).rejects.toThrow("Session 历史投递出现缺口");
    const view = harness.workspaceRef.current.sessions[sessionId]!;
    expect(view.delivery.frozen).toBe(true);
    expect(view.delivery.expectedSequence).toBe(2);
    expect(view.delivery.receivedSequence).toBe(3);
    expect(view.replay.loaded).toBe(false);
  });

  it("delivery waiter 经过 fake timers 三十秒超时并清理 timer", async () => {
    vi.useFakeTimers();
    const sessionId = "session-timeout";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    ports.sessionLoad.mockResolvedValue(loadResult(sessionId, replayResult(sessionId, {
      throughDeliverySequence: 1,
    })));

    const recovery = harness.history.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await flushMicrotasks();
    expect(vi.getTimerCount()).toBe(1);

    await vi.advanceTimersByTimeAsync(SESSION_DELIVERY_RECOVERY_TIMEOUT_MS - 1);
    expect(harness.workspaceRef.current.sessions[sessionId]?.replay.restoring).toBe(true);
    const rejected = expect(recovery).rejects.toThrow("Session 历史投递等待超时");
    await vi.advanceTimersByTimeAsync(1);
    await rejected;
    expect(vi.getTimerCount()).toBe(0);
  });

  it("卸载时拒绝已有 waiter，并清理其计时器", async () => {
    vi.useFakeTimers();
    const sessionId = "session-unmount-waiter";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    ports.sessionLoad.mockResolvedValue(loadResult(sessionId, replayResult(sessionId, {
      throughDeliverySequence: 1,
    })));

    const recovery = harness.history.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await flushMicrotasks();
    expect(vi.getTimerCount()).toBe(1);
    const eventCountBeforeUnmount = harness.events.length;

    harness.dispose();
    await expect(recovery).rejects.toThrow("Session 历史恢复已取消");
    expect(vi.getTimerCount()).toBe(0);
    expect(harness.events).toHaveLength(eventCountBeforeUnmount);
  });

  it("卸载后迟到的 load 响应不能回写 Session 投影", async () => {
    const sessionId = "session-late-load";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    ports.sessionLoad.mockReturnValue(pendingLoad.promise);

    const recovery = harness.history.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await flushMicrotasks();
    const view = harness.workspaceRef.current.sessions[sessionId]!;
    const eventCountBeforeUnmount = harness.events.length;
    expect(view.project_path).toBe(null);
    expect(view.replay.loaded).toBe(false);

    harness.dispose();
    pendingLoad.resolve(loadResult(sessionId));
    await expect(recovery).rejects.toThrow("Session 历史恢复已取消");

    expect(view.project_path).toBe(null);
    expect(view.title).toBe(null);
    expect(view.replay.throughDeliverySequence).toBe(null);
    expect(view.replay.loaded).toBe(false);
    expect(harness.events).toHaveLength(eventCountBeforeUnmount);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
  });

  it("空历史标记 loaded 后再次 replay 不重复 load", async () => {
    const sessionId = "session-empty-history";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    ports.sessionLoad.mockResolvedValue(loadResult(sessionId));

    await harness.history.replayHistory(sessionId, { sessionId, epoch: 1 });
    await harness.history.replayHistory(sessionId, { sessionId, epoch: 1 });

    const view = harness.workspaceRef.current.sessions[sessionId]!;
    expect(view.history).toHaveLength(0);
    expect(view.replay.loaded).toBe(true);
    expect(ports.sessionLoad).toHaveBeenCalledOnce();
  });

  it("connect 新建 Session 不触发历史 load", async () => {
    const harness = createHistoryHarness({ sessionId: null, epoch: 1 });
    ports.sessionConnect.mockResolvedValue(newSessionSnapshot("session-new"));

    const snapshot = await harness.history.connectSession({
      projectPath: "D:/new-session",
      sessionId: null,
      operationId: "operation-new",
    });

    expect(snapshot.sessionId).toBe("session-new");
    expect(ports.sessionConnect).toHaveBeenCalledOnce();
    expect(ports.sessionLoad).not.toHaveBeenCalled();
    expect(harness.workspaceRef.current.sessions["session-new"]?.replay.loaded).toBe(true);
  });

  it("connect 既有 Session 只 load 一次并等待实际 delivery 消费", async () => {
    const sessionId = "session-connect-existing";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    ports.sessionLoad.mockResolvedValue(loadResult(sessionId, replayResult(sessionId, {
      throughDeliverySequence: 1,
    })));

    const connected = harness.history.connectSession({
      sessionId,
      operationId: "operation-existing",
    });
    await flushMicrotasks();
    expect(ports.sessionLoad).toHaveBeenCalledOnce();
    expect(harness.workspaceRef.current.sessions[sessionId]?.replay.restoring).toBe(true);

    deliver(harness, sessionId, 1);
    const snapshot = await connected;
    expect(snapshot).toMatchObject({
      sessionId,
      state: "ready",
      backend: "acp",
    });
    expect(ports.sessionLoad).toHaveBeenCalledOnce();
    expect(ports.sessionConnect).not.toHaveBeenCalled();
  });

  it("拒绝缺失、负数、小数 delivery 水位及超过水位的 replayedEvents", async () => {
    const sessionId = "session-invalid-delivery-control";
    const valid = replayResult(sessionId);
    const cases: Array<{
      name: string;
      replay: ReplayResult;
      message: string;
    }> = [
      {
        name: "缺失 throughDeliverySequence",
        replay: {
          ...valid,
          throughDeliverySequence: undefined as unknown as number,
        },
        message: "ACP load 历史恢复控制信息无效",
      },
      {
        name: "负数 throughDeliverySequence",
        replay: { ...valid, throughDeliverySequence: -1 },
        message: "ACP load 历史恢复控制信息无效",
      },
      {
        name: "小数 throughDeliverySequence",
        replay: { ...valid, throughDeliverySequence: 1.5 },
        message: "ACP load 历史恢复控制信息无效",
      },
      {
        name: "replayedEvents 超过 delivery 水位",
        replay: { ...valid, replayedEvents: 2, throughDeliverySequence: 1 },
        message: "ACP load 历史尚未完整恢复",
      },
    ];

    for (const testCase of cases) {
      const harness = createHistoryHarness({ sessionId, epoch: 1 });
      ports.sessionLoad.mockResolvedValueOnce(loadResult(sessionId, testCase.replay));

      await expect(
        harness.history.replayHistory(sessionId, { sessionId, epoch: 1 }),
      ).rejects.toThrow(testCase.message);
      const view = harness.workspaceRef.current.sessions[sessionId]!;
      expect(view.delivery.frozen, testCase.name).toBe(true);
      expect(view.replay.loaded, testCase.name).toBe(false);
      expect(view.last_error?.code, testCase.name).toBe("session_recovery_failed");
      expect(ports.sessionLoad, testCase.name).toHaveBeenCalledTimes(cases.indexOf(testCase) + 1);
    }
  });

  it("拒绝错误 Session snapshot，不能以 load 成功代替 Session 身份校验", async () => {
    const sessionId = "session-error-snapshot";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const result = loadResult(sessionId);
    result._meta = {
      ...result._meta,
      "keencode/snapshot": {
        sessionId,
        state: "error",
        activeTurnId: null,
        backend: "acp",
        projectPath: "D:/fixture",
        title: "错误快照",
        lastError: "历史恢复失败",
      },
    };
    ports.sessionLoad.mockResolvedValue(result);

    await expect(
      harness.history.replayHistory(sessionId, { sessionId, epoch: 1 }),
    ).rejects.toThrow("ACP Session 快照字段无效");
    const view = harness.workspaceRef.current.sessions[sessionId]!;
    expect(view.delivery.frozen).toBe(true);
    expect(view.replay.loaded).toBe(false);
    expect(view.last_error?.code).toBe("session_recovery_failed");
  });
});
