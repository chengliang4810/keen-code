import { createElement, type EffectCallback } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionLoadResult } from "@/lib/acp/api";
import type { KeenCodeEvent, SessionUpdate } from "@/lib/acp/events";
import { beginLocalSessionTurn, createAcpWorkspaceState } from "@/lib/acp/store";
import { createTurnLatencyState } from "@/lib/turnLatency";
import { projectAcpSnapshot } from "@/lib/sessionProjection";
import { useAcpRuntimeEvents, type AcpRuntimeEventsOptions } from "./events";
import { useAcpRuntimeHistory, type AcpRuntimeHistoryResult } from "./history";
import { useAcpRuntimeTurnMetrics } from "./turnMetrics";

/** 仅替换外部订阅、时钟及 effect 调度；生产解析器、归约器、三个 Hook 保持真实。 */
const ports = vi.hoisted(() => ({
  effects: [] as EffectCallback[],
  receive: null as ((value: unknown) => void) | null,
  now: 1000,
  load: vi.fn(),
  diagnostics: vi.fn().mockResolvedValue(undefined),
  unlisten: vi.fn(),
}));

vi.mock("react", async (original) => ({
  ...await original<typeof import("react")>(),
  useEffect: (effect: EffectCallback) => { ports.effects.push(effect); },
}));
vi.mock("@/lib/api", () => ({ isTauri: () => true }));
vi.mock("@/lib/turnLatency", async (original) => ({
  ...await original<typeof import("@/lib/turnLatency")>(),
  turnLatencyNow: () => ports.now,
}));
vi.mock("@/lib/acp/api", async (original) => ({
  ...await original<typeof import("@/lib/acp/api")>(),
  sessionLoad: ports.load,
  listenAcp: async (_event: string, receive: (value: unknown) => void) => {
    ports.receive = receive;
    return ports.unlisten;
  },
  diagnosticsRecord: ports.diagnostics,
  acpClientRespond: vi.fn().mockResolvedValue(undefined),
  cancelledClientResponse: vi.fn(),
  goalGet: vi.fn(),
}));

/** 保存 effect 的真实清理函数，避免测试遗留订阅或批处理定时器。 */
let cleanups: Array<() => void> = [];

/** 构造与页面装配相同的三个 Hook，共享真实 Ref，而非复制生产分支。 */
async function harness() {
  const options: AcpRuntimeEventsOptions = {
    acpWorkspaceRef: { current: createAcpWorkspaceState() },
    turnLatencyBySessionRef: { current: new Map([["session-1", createTurnLatencyState("turn-1", 1000)]]) },
    activeTurnIdBySessionRef: { current: new Map() },
    recoverableCompletedTurnIdBySessionRef: { current: new Map() },
    completedTurnIdBySessionRef: { current: new Map() },
    pendingVisibleTurnBySessionRef: { current: new Map() },
    liveHostRef: { current: { sessionId: "session-1", state: "ready", lastError: null, streamingMessageId: null, backend: "acp" } },
    messagesBySessionRef: { current: new Map() },
    modelBySessionRef: { current: new Map() },
    contextUsageBySessionRef: { current: new Map() },
    viewingSessionIdRef: { current: "session-1" },
    configuredModelsRef: { current: [] },
    clearPendingAskUserRef: { current: vi.fn() },
    pendingAskUserBySessionRef: { current: new Map() },
    setPendingAskUserSessionIds: vi.fn(),
    setAskUser: vi.fn(),
    setContextUsage: vi.fn(),
    setLiveHost: vi.fn(),
    setLiveMap: vi.fn(),
    setTurnStartedAt: vi.fn(),
    setModelId: vi.fn(),
    setCompletedUnreadIds: vi.fn(),
    applyViewProjectionRef: { current: vi.fn() },
    commitWorkspace: vi.fn(),
    refreshTaskCacheUsage: vi.fn().mockResolvedValue(undefined),
    recoverSession: vi.fn(),
    observeSessionDelivery: vi.fn(),
  };
  // 执行 React setter 的函数式更新，验证生产 Host 状态投影而非只记录回调。
  options.setLiveHost = vi.fn((next) => {
    options.liveHostRef.current = typeof next === "function"
      ? next(options.liveHostRef.current) : next;
  });
  let history!: AcpRuntimeHistoryResult;
  let visible!: ReturnType<typeof useAcpRuntimeTurnMetrics>;
  /** 合法 React 上下文提供 Ref/Callback，订阅 effect 在渲染后由测试显式执行。 */
  function Harness() {
    history = useAcpRuntimeHistory({
      acpWorkspaceRef: options.acpWorkspaceRef,
      turnLatencyBySessionRef: options.turnLatencyBySessionRef,
      pendingVisibleTurnBySessionRef: options.pendingVisibleTurnBySessionRef,
      applyViewProjectionRef: options.applyViewProjectionRef,
      commitWorkspace: options.commitWorkspace,
      currentViewFocus: () => ({ sessionId: "session-1", epoch: 1 }),
      invalidateContextUsage: vi.fn(),
      setPlanModeSessionKey: vi.fn(),
    });
    useAcpRuntimeEvents({ ...options, recoverSession: history.recoverSession, observeSessionDelivery: history.observeSessionDelivery });
    visible = useAcpRuntimeTurnMetrics({
      sessionId: "session-1",
      ...options,
      applyViewProjection: options.applyViewProjectionRef,
    });
    return null;
  }
  renderToString(createElement(Harness));
  for (const effect of ports.effects.splice(0)) {
    const cleanup = effect();
    if (cleanup) cleanups.push(cleanup);
  }
  await Promise.resolve();
  expect(ports.receive).toBeTypeOf("function");
  return { options, history, visible };
}

/** 向生产订阅回调投递合法信封，并使用可控的前端单调接收时钟。 */
function deliver(sequence: number, payload: KeenCodeEvent | SessionUpdate, atMs: number, turnId = "turn-1") {
  ports.now = atMs;
  const standard = "sessionUpdate" in payload;
  ports.receive!({
    type: standard ? "session_update" : "keencode_event",
    envelope: {
      schemaVersion: 1, sessionId: "session-1", turnId, sourceAgentId: "root",
      deliverySequence: sequence, occurredAtMs: 999_999,
      ...(standard ? { update: payload } : { event: payload, journalSequence: sequence }),
    },
  });
}

/** 构造根正文或思考，不把工具/子 Agent 的内容冒充根文本。 */
function textChunk(text: string): SessionUpdate {
  return { sessionUpdate: "agent_message_chunk", content: { type: "text", text } };
}

/** load 的控制响应确认已投递完毕，正文仍须通过生产订阅回调进入。 */
function loaded(through: number, delivered = through): SessionLoadResult {
  return {
    configOptions: [],
    modes: { currentModeId: "default", availableModes: [] },
    _meta: {
      "keencode/snapshot": { sessionId: "session-1", state: "ready", activeTurnId: null, backend: "acp", projectPath: "D:/fixture", title: "历史会话", lastError: null },
      "keencode/replay": {
      sessionId: "session-1", startAfter: 0, nextAfter: through,
      throughJournalSequence: through, replayedEvents: through, hasMore: false,
      throughDeliverySequence: delivered,
    } },
  };
}

beforeEach(() => {
  ports.now = 1000;
  ports.receive = null;
  ports.load.mockReset();
  ports.diagnostics.mockClear();
  ports.unlisten.mockClear();
  vi.stubGlobal("window", {
    setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout,
    dispatchEvent: vi.fn(),
  });
});

afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
  ports.effects.length = 0;
  vi.unstubAllGlobals();
});

describe("ACP 接收与恢复计时的真实订阅接线", () => {
  it("历史回调消费完成前不能放行 load 后紧接着发送的新回合", async () => {
    const { options, history } = await harness();
    ports.load.mockResolvedValue(loaded(3));
    let completed = false;
    const recovery = history.recoverSession("session-1").then(() => { completed = true; });
    await Promise.resolve();
    await Promise.resolve();
    const view = options.acpWorkspaceRef.current.sessions["session-1"]!;
    expect(completed).toBe(false);
    expect(view.replay.restoring).toBe(true);
    deliver(1, { type: "turn_started", rootTurnId: "turn-old" }, 4100, "turn-old");
    expect(projectAcpSnapshot(view).state).toBe("connecting");
    expect(options.liveHostRef.current.state).toBe("connecting");
    deliver(2, textChunk("旧历史"), 4110, "turn-old");
    expect(completed).toBe(false);
    deliver(3, { type: "turn_completed" }, 4120, "turn-old");
    expect(projectAcpSnapshot(view).state).toBe("connecting");
    expect(options.liveHostRef.current.state).toBe("connecting");
    await recovery;
    expect(view.replay.restoring).toBe(false);
    expect(projectAcpSnapshot(view).state).toBe("ready");
    // 精确复用发送链在 ensureConnected 完成后建立的新回合状态。
    beginLocalSessionTurn(view, 5000);
    options.activeTurnIdBySessionRef.current.set("session-1", "turn-new");
    options.turnLatencyBySessionRef.current.set("session-1", createTurnLatencyState("turn-new", 5000));
    // 重复历史投递仍必须由同一水位拒绝，不清理已经开始的新回合。
    deliver(3, { type: "turn_completed" }, 5100, "turn-old");
    expect(options.activeTurnIdBySessionRef.current.get("session-1")).toBe("turn-new");
    expect(view.status).toBe("streaming");
  });

  it("正常后台完成允许迟到 DOM 诊断且保持真实首Token/终态", async () => {
    const { options, visible } = await harness();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 1010);
    deliver(2, textChunk("正文"), 1020);
    deliver(3, { type: "turn_completed" }, 1030);
    expect(options.pendingVisibleTurnBySessionRef.current.get("session-1")).toBe("turn-1");
    ports.now = 9000;
    visible.handleFirstVisibleToken("turn-1");
    const message = options.acpWorkspaceRef.current.sessions["session-1"]!.history.at(-1);
    expect(message?.turnMetrics).toMatchObject({ timeToFirstTokenMs: 20, totalMs: 30, timeToFirstVisibleTokenMs: 8000 });
    expect(options.turnLatencyBySessionRef.current.size).toBe(0);
    expect(options.pendingVisibleTurnBySessionRef.current.size).toBe(0);
  });

  it("缺口期间回放终态保留已知首Token，但不留下等待DOM的未完成观测", async () => {
    const { options, history, visible } = await harness();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 1010);
    deliver(2, textChunk("最初正文"), 1020);
    let finish!: (value: SessionLoadResult) => void;
    ports.load.mockReturnValue(new Promise<SessionLoadResult>(resolve => { finish = resolve; }));
    deliver(4, textChunk("缺口"), 2000);
    expect(ports.load).toHaveBeenCalledOnce();
    const recovery = history.recoverSession("session-1");
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 3000);
    deliver(2, textChunk("最初正文及后续"), 3010);
    deliver(3, { type: "turn_completed" }, 3020);
    finish(loaded(3));
    await recovery;
    const message = options.acpWorkspaceRef.current.sessions["session-1"]!.history.at(-1);
    expect(message?.turnMetrics).toMatchObject({ timeToFirstTokenMs: 20, totalMs: null });
    expect(options.turnLatencyBySessionRef.current.size).toBe(0);
    expect(options.pendingVisibleTurnBySessionRef.current.size).toBe(0);
    ports.now = 9000;
    visible.handleFirstVisibleToken("turn-1");
    expect(message?.turnMetrics).toMatchObject({ timeToFirstTokenMs: 20, totalMs: null, timeToFirstVisibleTokenMs: null });
  });

  it.each([false, true])("恢复后仍活跃的Turn不把后续文本冒充首Token，先前已知=%s", async known => {
    const { options, history } = await harness();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 1010);
    if (known) deliver(2, textChunk("已观测首段"), 1020);
    let finish!: (value: SessionLoadResult) => void;
    ports.load.mockReturnValue(new Promise<SessionLoadResult>(resolve => { finish = resolve; }));
    deliver(4, textChunk("缺口"), 2000);
    const recovery = history.recoverSession("session-1");
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 3000);
    deliver(2, textChunk("恢复出来的历史首段"), 3010);
    finish(loaded(1));
    await recovery;
    deliver(3, textChunk("恢复后的实时后续"), 4000);
    deliver(4, { type: "turn_completed" }, 4100);
    const message = options.acpWorkspaceRef.current.sessions["session-1"]!.history.at(-1);
    expect(message?.turnMetrics).toMatchObject({ timeToFirstTokenMs: known ? 20 : null, totalMs: 3100 });
    expect(options.turnLatencyBySessionRef.current.size).toBe(0);
    expect(options.pendingVisibleTurnBySessionRef.current.size).toBe(0);
  });

  it.each([false, true])("load响应先于历史终态回调时不补造完成耗时，已有部分回放=%s", async partial => {
    const { options, history } = await harness();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 1010);
    deliver(2, textChunk("已知首段"), 1020);
    options.viewingSessionIdRef.current = "other-session";
    ports.load.mockResolvedValue(loaded(3));
    const recovery = history.recoverSession("session-1");
    if (partial) {
      deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 2000);
      deliver(2, textChunk("部分回放正文"), 2010);
    }
    // Host已将消息交给Tauri，但WebView的回调尚未处理；先放行load的microtask。
    await Promise.resolve();
    await Promise.resolve();
    if (!partial) {
      deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 3000);
      deliver(2, textChunk("回放正文"), 3010);
    }
    deliver(3, { type: "turn_completed" }, 3020);
    await recovery;
    const message = options.acpWorkspaceRef.current.sessions["session-1"]!.history.at(-1);
    expect(message?.turnMetrics).toMatchObject({ timeToFirstTokenMs: 20, totalMs: null });
    expect(options.turnLatencyBySessionRef.current.size).toBe(0);
    expect(options.pendingVisibleTurnBySessionRef.current.size).toBe(0);
    expect(options.setCompletedUnreadIds).not.toHaveBeenCalled();
    expect(options.refreshTaskCacheUsage).not.toHaveBeenCalled();
  });

  it("load先返回但水位之后是真实终态时仍记录实时完成耗时", async () => {
    const { options, history } = await harness();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 1010);
    deliver(2, textChunk("已知首段"), 1020);
    ports.load.mockResolvedValue(loaded(1));
    const recovery = history.recoverSession("session-1");
    await Promise.resolve();
    await Promise.resolve();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 3000);
    deliver(2, textChunk("回放正文"), 3010);
    deliver(3, { type: "turn_completed" }, 3100);
    await recovery;
    const message = options.acpWorkspaceRef.current.sessions["session-1"]!.history.at(-1);
    expect(message?.turnMetrics).toMatchObject({ timeToFirstTokenMs: 20, totalMs: 2100 });
    expect(options.refreshTaskCacheUsage).toHaveBeenCalledOnce();
    expect(options.turnLatencyBySessionRef.current.size).toBe(0);
  });

  it("旧Turn终态不能把活跃的新Turn置为ready或删除当前计时", async () => {
    const { options } = await harness();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 1010);
    deliver(2, textChunk("当前正文"), 1020);
    vi.mocked(options.setTurnStartedAt).mockClear();
    vi.mocked(options.setLiveMap).mockClear();
    deliver(3, { type: "turn_completed" }, 1100, "turn-old");
    expect(options.acpWorkspaceRef.current.sessions["session-1"]?.active_root_turn_id).toBe("turn-1");
    expect(options.turnLatencyBySessionRef.current.get("session-1")?.firstTokenAtMs).toBe(1020);
    expect(options.activeTurnIdBySessionRef.current.get("session-1")).toBe("turn-1");
    expect(options.setTurnStartedAt).not.toHaveBeenCalled();
    expect(options.setLiveMap).not.toHaveBeenCalled();
  });

  it("load先重放旧Turn终态不丢失当前Turn已知观测，且重复终态无副作用", async () => {
    const { options, history } = await harness();
    deliver(1, { type: "turn_started", rootTurnId: "turn-1" }, 1010);
    deliver(2, textChunk("已知首段"), 1020);
    ports.load.mockImplementation(async () => {
      deliver(1, { type: "turn_started", rootTurnId: "turn-old" }, 3000, "turn-old");
      deliver(2, textChunk("历史正文"), 3010, "turn-old");
      deliver(3, { type: "turn_completed" }, 3020, "turn-old");
      deliver(4, { type: "turn_started", rootTurnId: "turn-1" }, 3030);
      deliver(5, textChunk("已知首段及回放正文"), 3040);
      return loaded(4);
    });
    await history.recoverSession("session-1");
    expect(options.turnLatencyBySessionRef.current.get("session-1")?.firstTokenAtMs).toBe(1020);
    deliver(6, { type: "turn_completed" }, 4000);
    const completed = options.acpWorkspaceRef.current.sessions["session-1"]!.history.at(-1);
    expect(completed?.turnMetrics).toMatchObject({ timeToFirstTokenMs: 20, totalMs: 3000 });
    vi.mocked(options.setLiveMap).mockClear();
    deliver(7, { type: "turn_completed" }, 5000);
    expect(options.setLiveMap).not.toHaveBeenCalled();
    expect(completed?.turnMetrics).toMatchObject({ timeToFirstTokenMs: 20, totalMs: 3000 });
  });
});
