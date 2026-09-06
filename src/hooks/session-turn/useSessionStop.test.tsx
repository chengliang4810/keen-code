import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAcpWorkspaceState, emptySession } from "@/lib/acp/store";
import type { AcpSessionView } from "@/lib/acp/store";
import type { SessionSnapshot } from "@/lib/session";
import { STOP_LATCH_MS } from "@/lib/stopLatch";
import type { SessionTurnApiPort } from "./types";
import {
  useSessionStop,
  type SessionStopResult,
  type UseSessionStopOptions,
} from "./useSessionStop";

/** 在合法 React 渲染上下文中捕获 Hook 返回值。 */
function renderStop(options: UseSessionStopOptions): SessionStopResult {
  let captured!: SessionStopResult;

  /** 测试专用的 Hook 宿主，不渲染任何可见节点。 */
  function Harness() {
    captured = useSessionStop(options);
    return null;
  }

  renderToString(createElement(Harness));
  return captured;
}

/** 构造 Stop Hook 所需的最小 Session 与 ACP 投影。 */
function makeOptions(
  sessionId = "session-a",
  requestId = "turn-a",
): UseSessionStopOptions & {
  api: SessionTurnApiPort & { stop: ReturnType<typeof vi.fn> };
  liveHostRef: { current: SessionSnapshot };
  acpWorkspaceRef: { current: ReturnType<typeof createAcpWorkspaceState> };
  setAskUser: ReturnType<typeof vi.fn>;
} {
  const acpWorkspace = createAcpWorkspaceState();
  const view = emptySession(sessionId);
  view.status = "streaming";
  acpWorkspace.sessions[sessionId] = view;
  const liveHost: SessionSnapshot = {
    sessionId,
    state: "streaming",
    lastError: null,
    streamingMessageId: "assistant-a",
    backend: "acp",
  };
  const liveHostRef = { current: liveHost };
  const acpWorkspaceRef = { current: acpWorkspace };
  const viewingSessionIdRef = { current: sessionId };
  const setAskUser = vi.fn();
  const api = {
    stop: vi.fn().mockResolvedValue(undefined),
  } as unknown as SessionTurnApiPort & { stop: ReturnType<typeof vi.fn> };
  const ui = {
    setRetryStatus: vi.fn(),
    setStreamStall: vi.fn(),
    setLocalError: vi.fn(),
    // 兼容测试中的旧调用端口；Stop Hook 不再接收或调用它。
    setAskUser,
  } as unknown as UseSessionStopOptions["ui"];

  return {
    locale: "zh",
    api,
    runtime: {
      acpWorkspaceRef,
      liveHostRef,
      viewingSessionIdRef,
    },
    ui,
    activeTurnIdBySessionRef: {
      current: new Map([[sessionId, requestId]]),
    },
    liveHostRef,
    acpWorkspaceRef,
    setAskUser,
  };
}

/** 创建可控的异步回执，验证迟到 Stop 回执不会越过回合边界。 */
function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("useSessionStop", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Hook 通过 window 定时器运行；node 测试环境没有真实 Window。
    vi.stubGlobal("window", globalThis);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("通知成功后仍保持 streaming，预算超时只提示并保留 waiting", async () => {
    const options = makeOptions();
    const result = renderStop(options);

    await result.stop();
    vi.advanceTimersByTime(STOP_LATCH_MS + 50);

    expect(options.api.stop).toHaveBeenCalledWith("session-a", "turn-a");
    expect(options.liveHostRef.current.state).toBe("streaming");
    expect(options.acpWorkspaceRef.current.sessions["session-a"]?.status).toBe(
      "streaming",
    );
    expect(result.stopLatchRef.current.phase).toBe("waiting");
    expect(options.ui.setLocalError).toHaveBeenCalledWith(
      "尚未确认停止，可重试",
    );
  });

  it("只有目标 Turn 的真实终态快照才清理 waiting latch", async () => {
    const options = makeOptions();
    const result = renderStop(options);

    await result.stop();
    options.activeTurnIdBySessionRef.current.delete("session-a");
    options.liveHostRef.current = {
      ...options.liveHostRef.current,
      state: "ready",
      streamingMessageId: null,
    };
    const view = options.acpWorkspaceRef.current.sessions["session-a"] as AcpSessionView;
    view.status = "idle";

    vi.advanceTimersByTime(STOP_LATCH_MS + 50);

    expect(result.stopLatchRef.current.phase).toBe("idle");
    expect(options.liveHostRef.current.state).toBe("ready");
  });

  it("超时后允许重试，但不会恢复 canSend 或伪造完成", async () => {
    const options = makeOptions();
    const result = renderStop(options);

    await result.stop();
    vi.advanceTimersByTime(STOP_LATCH_MS + 50);
    await result.stop();

    expect(options.api.stop).toHaveBeenCalledTimes(2);
    expect(result.stopLatchRef.current.phase).toBe("waiting");
    expect(options.liveHostRef.current.state).toBe("streaming");
  });

  it("同一 Session 新 Turn 抢先开始时立即释放旧 Stop latch", async () => {
    const options = makeOptions();
    const result = renderStop(options);

    await result.stop();
    options.activeTurnIdBySessionRef.current.set("session-a", "turn-new");
    vi.advanceTimersByTime(STOP_LATCH_MS + 50);

    expect(result.stopLatchRef.current.phase).toBe("idle");
    expect(options.ui.setLocalError).not.toHaveBeenCalledWith(
      "尚未确认停止，可重试",
    );
    expect(options.setAskUser).not.toHaveBeenCalled();
  });

  it("旧 Stop 的失败回执不会把错误写入新 Turn", async () => {
    const options = makeOptions();
    const oldReply = deferred<void>();
    options.api.stop.mockImplementationOnce(() => oldReply.promise);
    const result = renderStop(options);

    const oldStop = result.stop();
    options.activeTurnIdBySessionRef.current.set("session-a", "turn-new");
    oldReply.reject(new Error("old stop failed"));
    await oldStop;

    expect(result.stopLatchRef.current.phase).toBe("idle");
    expect(options.ui.setLocalError).not.toHaveBeenCalled();
    expect(options.setAskUser).not.toHaveBeenCalled();
  });

  it("迟到的旧 Stop 回执不会清理新 Turn 的 AskUser", async () => {
    const options = makeOptions();
    const oldReply = deferred<void>();
    const newReply = deferred<void>();
    options.api.stop
      .mockImplementationOnce(() => oldReply.promise)
      .mockImplementationOnce(() => newReply.promise);
    const result = renderStop(options);

    const oldStop = result.stop();
    vi.advanceTimersByTime(STOP_LATCH_MS + 50);
    options.activeTurnIdBySessionRef.current.set("session-a", "turn-new");
    const newStop = result.stop();
    oldReply.resolve(undefined);
    await oldStop;

    newReply.resolve(undefined);
    await newStop;
    expect(options.setAskUser).not.toHaveBeenCalled();
  });

  it("通知失败后复位 latch 并允许再次发送 Stop", async () => {
    const options = makeOptions();
    options.api.stop
      .mockRejectedValueOnce(new Error("transport closed"))
      .mockResolvedValueOnce(undefined);
    const result = renderStop(options);

    await result.stop();
    expect(result.stopLatchRef.current.phase).toBe("idle");
    expect(options.ui.setLocalError).toHaveBeenCalled();
    expect(options.setAskUser).not.toHaveBeenCalled();

    await result.stop();
    expect(options.api.stop).toHaveBeenCalledTimes(2);
    expect(result.stopLatchRef.current.phase).toBe("waiting");
  });

  it("停止目标固定为当前 Session，不会误处理 live Host 的其他 Session", async () => {
    const options = makeOptions();
    const other = emptySession("session-b");
    other.status = "streaming";
    options.acpWorkspaceRef.current.sessions["session-b"] = other;
    options.liveHostRef.current = {
      ...options.liveHostRef.current,
      sessionId: "session-b",
      state: "streaming",
    };
    const result = renderStop(options);

    await result.stop();

    expect(options.api.stop).toHaveBeenCalledWith("session-a", "turn-a");
    expect(options.liveHostRef.current.sessionId).toBe("session-b");
    expect(options.acpWorkspaceRef.current.sessions["session-b"]?.status).toBe(
      "streaming",
    );
  });
});
