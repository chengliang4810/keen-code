import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ReplayResult,
  SessionLoadResult,
} from "@/lib/acp/api";
import { ensureAcpSession } from "@/lib/acp/projection";
import {
  createAcpWorkspaceState,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import type { ViewFocus } from "@/lib/viewFocus";
import type { AcpRuntimeHistoryResult } from "./history";
import { useAcpRuntimeHistory } from "./history";

const apiMocks = vi.hoisted(() => ({
  /** 模拟标准 `session/load` 控制请求。 */
  sessionLoad: vi.fn(),
  /** 模拟分页 `session/replay` 控制请求。 */
  sessionReplay: vi.fn(),
  /** 新建连接只创建 Session，不对空历史重复 load。 */
  sessionConnect: vi.fn(),
}));

vi.mock("@/lib/acp/api", async (original) => ({
  ...await original<typeof import("@/lib/acp/api")>(), ...apiMocks,
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

/** 构造携带完整历史恢复水位的标准 `session/load` 可观察响应。 */
function loadResult(
  sessionId: string,
  currentModeId: SessionLoadResult["modes"]["currentModeId"],
  replay: ReplayResult = replayResult(sessionId),
): SessionLoadResult {
  return {
    modes: {
      currentModeId,
      availableModes: [
        { id: "default", name: "Default" },
        { id: "plan", name: "Plan" },
      ],
    },
    configOptions: [],
    _meta: {
      "keencode/replay": replay,
      "keencode/snapshot": { sessionId, state: "ready", activeTurnId: null, backend: "acp", projectPath: "D:/fixture", title: "历史会话", lastError: null },
    },
  };
}

/** 构造不携带事件正文的单页 replay 控制响应。 */
function replayResult(sessionId: string): ReplayResult {
  return {
    sessionId,
    startAfter: 0,
    nextAfter: 0,
    throughJournalSequence: 0,
    throughDeliverySequence: 0,
    replayedEvents: 0,
    hasMore: false,
  };
}

/** 用服务端恢复的最小历史消息标记 replay 已投递历史。 */
function recoveredHistoryMessage() {
  return { role: "user", content: "已恢复的历史" };
}

/** 渲染 Hook 并暴露其引用状态和可观察的 Composer 端口。 */
function createHistoryHarness(
  initialFocus: ViewFocus,
  initialPlanModeSessionKey: string | null = null,
) {
  /** 当前 ACP 工作区引用，模拟应用层的可变投影存储。 */
  const workspaceRef: { current: AcpWorkspaceState } = {
    current: createAcpWorkspaceState(),
  };
  /** 恢复过程对工作台投影的调用记录。 */
  const events: string[] = [];
  /** 模拟当前 Composer 的本地 Plan 模式键。 */
  const composer = { planModeSessionKey: initialPlanModeSessionKey };
  /** 当前导航焦点；测试会在可控 Promise 等待期间修改它。 */
  let focus = initialFocus;
  const applyViewProjection = vi.fn((sessionId: string | null) => {
    events.push(`apply:${sessionId}`);
  });
  const commitWorkspace = vi.fn(() => {
    events.push("commit");
  });
  const invalidateContextUsage = vi.fn((sessionId: string) => {
    events.push(`invalidate:${sessionId}`);
  });
  const setPlanModeSessionKey = vi.fn((sessionKey: string | null) => {
    composer.planModeSessionKey = sessionKey;
    events.push(`set-plan:${sessionKey ?? "none"}`);
  });
  let hookResult: AcpRuntimeHistoryResult | undefined;

  /** 在合法 React 渲染上下文中捕获 Hook 返回的恢复入口。 */
  function Harness() {
    hookResult = useAcpRuntimeHistory({
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

  renderToString(createElement(Harness));
  if (!hookResult) throw new Error("未捕获 ACP history Hook 结果");
  return {
    ...hookResult,
    workspaceRef,
    composer,
    events,
    applyViewProjection,
    commitWorkspace,
    invalidateContextUsage,
    setPlanModeSessionKey,
    /** 更新模拟导航焦点，复现恢复等待期间的用户导航。 */
    setFocus(next: ViewFocus) {
      focus = next;
    },
  };
}

describe("useAcpRuntimeHistory 的 Plan 模式恢复", () => {
  beforeEach(() => {
    apiMocks.sessionLoad.mockReset();
    apiMocks.sessionReplay.mockReset();
    apiMocks.sessionConnect.mockReset();
  });

  it("当前 Session 恢复为 plan 后，完成投影再同步 Composer", async () => {
    const sessionId = "session-plan";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    apiMocks.sessionLoad.mockImplementation(async () => {
      harness.events.push("load");
      return loadResult(sessionId, "plan");
    });

    await harness.replayHistory(sessionId, { sessionId, epoch: 1 });

    const view = harness.workspaceRef.current.sessions[sessionId];
    expect(view?.plan_mode).toBe(true);
    expect(view?.replay.restoring).toBe(false);
    expect(harness.composer.planModeSessionKey).toBe(sessionId);
    expect(harness.setPlanModeSessionKey).toHaveBeenCalledWith(sessionId);
    expect(harness.events.at(-1)).toBe(`set-plan:${sessionId}`);
    expect(harness.events.lastIndexOf("commit")).toBeLessThan(
      harness.events.indexOf(`set-plan:${sessionId}`),
    );
    expect(apiMocks.sessionLoad).toHaveBeenCalledOnce();
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it("当前 Session 恢复为普通模式后同步 Composer 的关闭状态", async () => {
    const sessionId = "session-default";
    const harness = createHistoryHarness({ sessionId, epoch: 1 }, sessionId);
    apiMocks.sessionLoad.mockResolvedValue(loadResult(sessionId, "default"));

    await harness.replayHistory(sessionId, { sessionId, epoch: 1 });

    const view = harness.workspaceRef.current.sessions[sessionId];
    expect(view?.plan_mode).toBe(false);
    expect(harness.composer.planModeSessionKey).toBe(null);
    expect(harness.setPlanModeSessionKey).toHaveBeenCalledWith(null);
    expect(apiMocks.sessionLoad).toHaveBeenCalledOnce();
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it.each(["__draft__", "session-created"])(
    "草稿创建期间迟到的默认模式不覆盖本地 Plan 键 %s",
    async (localPlanKey) => {
      const sessionId = "session-created";
      const originView = { sessionId: null, epoch: 1 };
      const harness = createHistoryHarness(originView, localPlanKey);
      const pendingLoad = deferred<SessionLoadResult>();
      apiMocks.sessionLoad.mockReturnValue(pendingLoad.promise);

      const recovery = harness.replayHistory(sessionId, originView);
      // 同一草稿实体化不增加导航 epoch；模式只能由发送路径绑定到新 Session。
      harness.setFocus({ sessionId, epoch: 1 });
      pendingLoad.resolve(loadResult(sessionId, "default"));
      await recovery;

      expect(harness.workspaceRef.current.sessions[sessionId]?.plan_mode).toBe(false);
      expect(harness.composer.planModeSessionKey).toBe(localPlanKey);
      expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
    },
  );

  it("草稿实体化时已有历史也不把持久默认模式覆盖到 Composer", async () => {
    const sessionId = "session-created-with-history";
    const originView = { sessionId: null, epoch: 1 };
    const harness = createHistoryHarness(originView, sessionId);
    const view = ensureAcpSession(harness.workspaceRef.current, sessionId);
    view.history.push(recoveredHistoryMessage());
    view.replay.loaded = true;
    harness.setFocus({ sessionId, epoch: 1 });

    await harness.replayHistory(sessionId, originView);

    expect(harness.composer.planModeSessionKey).toBe(sessionId);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
    expect(apiMocks.sessionLoad).not.toHaveBeenCalled();
  });

  it("load 等待期间导航到另一个 Session 时不覆盖 Composer", async () => {
    const sessionId = "session-plan";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    apiMocks.sessionLoad.mockReturnValue(pendingLoad.promise);

    const recovery = harness.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await vi.waitFor(() => expect(apiMocks.sessionLoad).toHaveBeenCalledOnce());
    harness.workspaceRef.current.sessions[sessionId]?.history.push(
      recoveredHistoryMessage(),
    );
    harness.setFocus({ sessionId: "other-session", epoch: 2 });
    pendingLoad.resolve(loadResult(sessionId, "plan"));
    await recovery;

    expect(harness.composer.planModeSessionKey).toBe(null);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
    expect(apiMocks.sessionLoad).toHaveBeenCalledOnce();
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it("load 等待期间切换到新草稿时不覆盖 Composer", async () => {
    const sessionId = "session-plan";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    apiMocks.sessionLoad.mockReturnValue(pendingLoad.promise);

    const recovery = harness.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await vi.waitFor(() => expect(apiMocks.sessionLoad).toHaveBeenCalledOnce());
    harness.workspaceRef.current.sessions[sessionId]?.history.push(
      recoveredHistoryMessage(),
    );
    harness.setFocus({ sessionId: null, epoch: 2 });
    pendingLoad.resolve(loadResult(sessionId, "plan"));
    await recovery;

    expect(harness.composer.planModeSessionKey).toBe(null);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
    expect(apiMocks.sessionLoad).toHaveBeenCalledOnce();
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it("A 恢复中切到 B 再回 A 时按最新导航 epoch 同步 Composer", async () => {
    const sessionId = "session-plan";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    const pendingLoad = deferred<SessionLoadResult>();
    apiMocks.sessionLoad.mockReturnValue(pendingLoad.promise);

    const firstNavigation = harness.replayHistory(sessionId, {
      sessionId,
      epoch: 1,
    });
    await vi.waitFor(() => expect(apiMocks.sessionLoad).toHaveBeenCalledOnce());
    harness.workspaceRef.current.sessions[sessionId]?.history.push(
      recoveredHistoryMessage(),
    );

    // A -> B -> A：回到 A 时必须把新的 epoch 交给正在进行的恢复任务。
    harness.setFocus({ sessionId: "session-b", epoch: 2 });
    harness.setFocus({ sessionId, epoch: 3 });
    /** 回到 A 的导航调用必须等待首个恢复 Promise，而不是提前完成。 */
    let latestNavigationDone = false;
    const latestNavigation = harness.replayHistory(sessionId, {
      sessionId,
      epoch: 3,
    }).then(() => {
      latestNavigationDone = true;
    });
    await Promise.resolve();
    expect(latestNavigationDone).toBe(false);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();

    pendingLoad.resolve(loadResult(sessionId, "plan"));
    await Promise.all([firstNavigation, latestNavigation]);

    expect(harness.composer.planModeSessionKey).toBe(sessionId);
    expect(harness.setPlanModeSessionKey).toHaveBeenCalledTimes(1);
    expect(apiMocks.sessionLoad).toHaveBeenCalledOnce();
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it("后台恢复完成后，切回已有 history 的 Session 才同步持久模式", async () => {
    const sessionId = "session-background-plan";
    const harness = createHistoryHarness({
      sessionId: "foreground-session",
      epoch: 1,
    });
    const pendingLoad = deferred<SessionLoadResult>();
    apiMocks.sessionLoad.mockImplementation(async () => {
      harness.events.push("load");
      harness.workspaceRef.current.sessions[sessionId]?.history.push(
        recoveredHistoryMessage(),
      );
      return pendingLoad.promise;
    });

    // 后台调用不携带导航焦点，不应在恢复完成时改写当前 Composer。
    const recovery = harness.recoverSession(sessionId);
    await vi.waitFor(() => expect(apiMocks.sessionLoad).toHaveBeenCalledOnce());
    pendingLoad.resolve(loadResult(sessionId, "plan"));
    await recovery;

    expect(harness.workspaceRef.current.sessions[sessionId]?.history).toHaveLength(1);
    expect(harness.composer.planModeSessionKey).toBe(null);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();

    harness.setFocus({ sessionId, epoch: 2 });
    await harness.replayHistory(sessionId, { sessionId, epoch: 2 });

    expect(harness.composer.planModeSessionKey).toBe(sessionId);
    expect(harness.setPlanModeSessionKey).toHaveBeenCalledTimes(1);
    expect(apiMocks.sessionLoad).toHaveBeenCalledTimes(1);
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it("load 失败时保持 Composer 模式未完成且记录恢复失败", async () => {
    const sessionId = "session-load-failed";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    apiMocks.sessionLoad.mockRejectedValue(new Error("load failed"));

    await expect(
      harness.replayHistory(sessionId, { sessionId, epoch: 1 }),
    ).rejects.toThrow("load failed");

    const view = harness.workspaceRef.current.sessions[sessionId];
    expect(view?.replay.restoring).toBe(false);
    expect(view?.last_error?.code).toBe("session_recovery_failed");
    expect(harness.composer.planModeSessionKey).toBe(null);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
    expect(apiMocks.sessionLoad).toHaveBeenCalledOnce();
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it("load 历史控制信息缺失、串 Session、未结束或水位非法时冻结投影", async () => {
    const sessionId = "session-load-control-invalid";
    const valid = loadResult(sessionId, "plan");
    /** 构造故意违反 load replay 控制契约的响应。 */
    const withReplay = (
      replay: Record<string, unknown>,
    ): SessionLoadResult => ({
      ...valid,
      _meta: { ...valid._meta, "keencode/replay": replay },
    });
    const cases: Array<{
      name: string;
      result: SessionLoadResult;
      error: string;
    }> = [
      {
        name: "缺 meta",
        result: { ...valid, _meta: undefined },
        error: "ACP load 缺少历史恢复完成水位",
      },
      {
        name: "串 Session",
        result: withReplay({ ...replayResult("other-session") }),
        error: "ACP load 历史恢复控制信息无效",
      },
      {
        name: "hasMore 为 true",
        result: withReplay({ ...replayResult(sessionId), hasMore: true }),
        error: "ACP load 历史恢复控制信息无效",
      },
      {
        name: "水位不是整数",
        result: withReplay({ ...replayResult(sessionId), nextAfter: 1.5 }),
        error: "ACP load 历史恢复控制信息无效",
      },
      {
        name: "首尾水位不闭合",
        result: withReplay({
          ...replayResult(sessionId),
          nextAfter: 2,
          throughJournalSequence: 3,
        }),
        error: "ACP load 历史尚未完整恢复",
      },
    ];

    for (const testCase of cases) {
      const harness = createHistoryHarness({ sessionId, epoch: 1 });
      apiMocks.sessionLoad.mockResolvedValue(testCase.result);

      await expect(
        harness.replayHistory(sessionId, { sessionId, epoch: 1 }),
      ).rejects.toThrow(testCase.error);

      const view = harness.workspaceRef.current.sessions[sessionId];
      expect(view?.plan_mode, testCase.name).toBe(false);
      expect(view?.replay.restoring, testCase.name).toBe(false);
      expect(view?.delivery.frozen, testCase.name).toBe(true);
      expect(view?.last_error?.code, testCase.name).toBe(
        "session_recovery_failed",
      );
      expect(harness.composer.planModeSessionKey, testCase.name).toBe(null);
      expect(harness.setPlanModeSessionKey, testCase.name).not.toHaveBeenCalled();
      expect(apiMocks.sessionLoad, testCase.name).toHaveBeenCalledOnce();
      expect(apiMocks.sessionReplay, testCase.name).not.toHaveBeenCalled();
      apiMocks.sessionLoad.mockReset();
      apiMocks.sessionReplay.mockReset();
    }
  });

  it("自动 replay 已有 history 时不覆盖用户未提交的本地模式选择", async () => {
    const sessionId = "session-existing-history";
    const harness = createHistoryHarness(
      { sessionId, epoch: 1 },
      sessionId,
    );
    /** 已存在的 Session 投影，模拟后台恢复后留下的历史。 */
    const view = ensureAcpSession(harness.workspaceRef.current, sessionId);
    view.status = "ready";
    view.plan_mode = false;
    view.replay.loaded = true;
    view.history.push(recoveredHistoryMessage());

    await harness.replayHistory(sessionId);

    expect(view.history).toHaveLength(1);
    expect(view.plan_mode).toBe(false);
    expect(harness.composer.planModeSessionKey).toBe(sessionId);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
    expect(apiMocks.sessionLoad).not.toHaveBeenCalled();
    expect(apiMocks.sessionReplay).not.toHaveBeenCalled();
  });

  it.each([
    undefined,
    null,
    { currentModeId: "bogus", availableModes: [] },
    { currentModeId: "default", availableModes: null },
    { currentModeId: "plan", availableModes: [{ id: "bogus", name: "未知" }] },
    { currentModeId: "plan", availableModes: [{ id: "plan", name: " " }] },
  ])("拒绝无效模式结构，不把未知值隐式当作普通模式：%j", async (modes) => {
    const sessionId = "session-invalid-mode";
    const harness = createHistoryHarness({ sessionId, epoch: 1 });
    apiMocks.sessionLoad.mockResolvedValue({ ...loadResult(sessionId, "plan"), modes });

    await expect(harness.replayHistory(sessionId)).rejects.toThrow("Session 恢复模式字段无效");
    const view = harness.workspaceRef.current.sessions[sessionId];
    expect(view?.replay.loaded).toBe(false);
    expect(view?.delivery.frozen).toBe(true);
    expect(view?.plan_mode).toBe(false);
    expect(view?.title).toBe(null);
    expect(harness.setPlanModeSessionKey).not.toHaveBeenCalled();
  });
});
