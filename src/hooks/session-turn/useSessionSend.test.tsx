import { createElement, type SetStateAction } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionPromptResult } from "@/lib/acp/api";
import { AcpRpcError } from "@/lib/acp/client";
import { createAcpWorkspaceState, emptySession } from "@/lib/acp/store";
import type { ChatMessage, SessionSnapshot } from "@/lib/session";
import { localizeUiError } from "@/lib/session";
import type { SessionTurnApiPort, SessionTurnRuntimePort } from "./types";
import { useSessionSend, type UseSessionSendOptions } from "./useSessionSend";

/** 在合法 React SSR 上下文中捕获真实发送 Hook。 */
function renderSend(options: UseSessionSendOptions): ReturnType<typeof useSessionSend> {
  let captured!: ReturnType<typeof useSessionSend>;

  /** 测试专用 Hook 宿主，不渲染可见节点。 */
  function Harness() {
    captured = useSessionSend(options);
    return null;
  }

  renderToString(createElement(Harness));
  return captured;
}

/** 构造可控的异步回执，分别模拟发送确认与传输终态。 */
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

/** 将失败回执后的 catch 与状态更新全部排空。 */
async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

type SendUi = UseSessionSendOptions["ui"];

interface SendFixture {
  options: UseSessionSendOptions;
  api: SessionTurnApiPort & { send: ReturnType<typeof vi.fn<SessionTurnApiPort["send"]>> };
  setLocalError: ReturnType<typeof vi.fn<SendUi["setLocalError"]>>;
  getLocalError: () => string | null;
  events: string[];
}

/** 构造真实发送 Hook 所需的最小 Session、ACP view 和边界端口。 */
function makeOptions(input: {
  visibleSessionId?: string | null;
  targetSessionId?: string | null;
  hasConfiguredModel?: boolean;
  initialLocalError?: string | null;
  completedRuns?: Array<Promise<SessionPromptResult>>;
} = {}): SendFixture {
  const visibleSessionId = input.visibleSessionId ?? "session-visible";
  const targetSessionId = input.targetSessionId ?? visibleSessionId;
  const workspace = createAcpWorkspaceState();
  const sessionIds = new Set(
    [visibleSessionId, targetSessionId].filter(
      (sessionId): sessionId is string => sessionId !== null,
    ),
  );
  for (const sessionId of sessionIds) {
    const view = emptySession(sessionId);
    view.replay.loaded = true;
    workspace.sessions[sessionId] = view;
  }

  const messagesBySessionRef: SessionTurnRuntimePort["messagesBySessionRef"] = {
    current: new Map<string, ChatMessage[]>(),
  };
  const liveHostRef: { current: SessionSnapshot } = {
    current: {
      sessionId: targetSessionId,
      state: "ready",
      lastError: null,
      streamingMessageId: null,
      backend: "acp",
    },
  };
  const viewingSessionIdRef: SessionTurnRuntimePort["viewingSessionIdRef"] = {
    current: visibleSessionId,
  };
  const acpWorkspaceRef: SessionTurnRuntimePort["acpWorkspaceRef"] = {
    current: workspace,
  };
  const sendInFlightRef = { current: false };
  const turnLatencyBySessionRef: UseSessionSendOptions["state"]["turnLatencyBySessionRef"] = {
    current: new Map(),
  };
  const activeTurnIdBySessionRef = { current: new Map<string, string>() };
  const recoverableCompletedTurnIdBySessionRef = { current: new Map<string, string>() };
  const pendingVisibleTurnBySessionRef = { current: new Map<string, string>() };
  const events: string[] = [];
  const completedRuns = [...(input.completedRuns ?? [])];
  const send = vi.fn<SessionTurnApiPort["send"]>((args) => {
    events.push(`api.send:${args.requestId}`);
    return {
      started: Promise.resolve({ turnId: args.requestId, occurredAtMs: 1 }),
      completed:
        completedRuns.shift() ??
        Promise.resolve({ stopReason: "end_turn" as const }),
    };
  });
  let localError: string | null = input.initialLocalError ?? "旧错误";
  const setLocalError = vi.fn<SendUi["setLocalError"]>(
    (action: SetStateAction<string | null>) => {
      events.push(action === null ? "local:null" : "local:error");
      localError = typeof action === "function" ? action(localError) : action;
    },
  );

  const api: SessionTurnApiPort & { send: typeof send } = {
    isTauri: () => false,
    connect: async () => {
      throw new Error("测试不应通过 connect 分支");
    },
    setEffort: async () => undefined,
    send,
    stop: async () => undefined,
    steer: async () => undefined,
    rewind: async () => {
      throw new Error("测试不应调用 rewind");
    },
    goalUpsert: async () => {
      throw new Error("测试不应调用 goalUpsert");
    },
  };
  const runtime: UseSessionSendOptions["runtime"] = {
    acpWorkspaceRef,
    liveHostRef,
    messagesBySessionRef,
    viewingSessionIdRef,
    applyViewProjectionRef: { current: vi.fn() },
    commitWorkspace: vi.fn(),
    patchSessionMessages: vi.fn(),
    currentViewFocus: () => ({
      sessionId: visibleSessionId,
      epoch: 1,
    }),
    replayHistory: async () => undefined,
    refreshSessions: async () => undefined,
    applyMessagePrefixTitle: vi.fn(),
    applyAutomaticSessionTitle: async () => undefined,
    updateSessionPreference: vi.fn(),
  };
  const ui: SendUi = {
    setSession: vi.fn(),
    setMessages: vi.fn(),
    setLiveHost: vi.fn(),
    setLiveMap: vi.fn(),
    setRetryStatus: vi.fn(),
    setTurnStartedAt: vi.fn(),
    setLocalError,
    setPlanModeSessionKey: vi.fn(),
    setUltraModeSessionKey: vi.fn(),
  };
  const options: UseSessionSendOptions = {
    locale: "zh",
    tr: (key) => key,
    sessionId: targetSessionId,
    modelLabel: "测试模型",
    hasConfiguredModel: input.hasConfiguredModel ?? true,
    api,
    runtime,
    ui,
    state: {
      sendInFlightRef,
      turnLatencyBySessionRef,
      activeTurnIdBySessionRef,
      recoverableCompletedTurnIdBySessionRef,
      pendingVisibleTurnBySessionRef,
    },
    ensureConnected: async () => targetSessionId,
    sendQueue: {
      enqueue: vi.fn(),
      releaseFlushHold: vi.fn(),
      bindDraft: vi.fn(),
    },
  };

  return {
    options,
    api,
    setLocalError,
    getLocalError: () => localError,
    events,
  };
}

/** 发送一条真实有效的普通文本消息。 */
function validSend(requestId: string, targetSessionId?: string): Parameters<ReturnType<typeof useSessionSend>>[0] {
  return {
    storedDisplay: "有效问题",
    att: [],
    requestId,
    ...(targetSessionId === undefined ? {} : { targetSessionId }),
  };
}

describe("useSessionSend local error recovery", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("可见会话的新有效发送先清除旧错误，传输失败后显示新错误", async () => {
    const failedRun = deferred<SessionPromptResult>();
    const fixture = makeOptions({ completedRuns: [failedRun.promise] });
    const send = renderSend(fixture.options);
    const failure = new Error("provider request timeout");

    await expect(send(validSend("turn-visible"))).resolves.toBe(true);
    failedRun.reject(failure);
    await expect(failedRun.promise).rejects.toBe(failure);
    await flushMicrotasks();

    expect(fixture.setLocalError).toHaveBeenNthCalledWith(1, null);
    expect(fixture.setLocalError).toHaveBeenNthCalledWith(
      2,
      localizeUiError(failure, "zh"),
    );
    expect(fixture.events.indexOf("local:null")).toBeLessThan(
      fixture.events.indexOf("api.send:turn-visible"),
    );
    expect(fixture.getLocalError()).toBe(localizeUiError(failure, "zh"));
  });

  it("后台会话发送不会清除前台错误", async () => {
    const fixture = makeOptions({
      visibleSessionId: "session-foreground",
      targetSessionId: "session-background",
      initialLocalError: "前台错误",
    });
    const send = renderSend(fixture.options);

    await expect(
      send(validSend("turn-background", "session-background")),
    ).resolves.toBe(true);
    await flushMicrotasks();

    expect(fixture.setLocalError).not.toHaveBeenCalled();
    expect(fixture.getLocalError()).toBe("前台错误");
  });

  it.each([
    ["空草稿", { storedDisplay: "   ", att: [] }],
    ["无配置模型", { storedDisplay: "有效问题", att: [] }],
  ] as const)("%s 不清除已有错误", async (label, sendInput) => {
    const fixture = makeOptions({
      hasConfiguredModel: label !== "无配置模型",
      initialLocalError: "已有错误",
    });
    const send = renderSend(fixture.options);

    await expect(send({ ...sendInput, att: [], requestId: `turn-${label}` })).resolves.toBe(
      false,
    );

    expect(fixture.setLocalError).not.toHaveBeenCalled();
    expect(fixture.api.send).not.toHaveBeenCalled();
    expect(fixture.getLocalError()).toBe("已有错误");
  });

  it("成功重试会清除失败前的旧错误，不遗留前次错误", async () => {
    const failedRun = deferred<SessionPromptResult>();
    const fixture = makeOptions({
      completedRuns: [failedRun.promise],
      initialLocalError: "旧错误",
    });
    const send = renderSend(fixture.options);
    const failure = new Error("provider request timeout");

    await expect(send(validSend("turn-failed"))).resolves.toBe(true);
    failedRun.reject(failure);
    await expect(failedRun.promise).rejects.toBe(failure);
    await flushMicrotasks();
    expect(fixture.getLocalError()).toBe(localizeUiError(failure, "zh"));

    await expect(send(validSend("turn-retry"))).resolves.toBe(true);
    expect(fixture.setLocalError).toHaveBeenLastCalledWith(null);
    expect(fixture.getLocalError()).toBeNull();
  });

  it("配置变化在 TurnStarted 前拒绝后，新发送仍清除旧横幅", async () => {
    const fixture = makeOptions();
    const failure = new AcpRpcError(-32603, "provider_configuration_changed");
    // 对照原生失败：启动回执直接拒绝，尚未形成权威运行回合。
    fixture.api.send.mockImplementationOnce(() => ({
      started: Promise.reject(failure),
      completed: Promise.resolve({ stopReason: "end_turn" }),
    }));
    const send = renderSend(fixture.options);

    await expect(send(validSend("turn-rejected"))).resolves.toBe(false);
    expect(fixture.getLocalError()).toBe(
      "此会话的模型连接配置已改变。请在对话底部重新选择模型后重试。",
    );
    expect(fixture.options.state.activeTurnIdBySessionRef.current.size).toBe(0);
    await expect(send(validSend("turn-reselected"))).resolves.toBe(true);
    expect(fixture.getLocalError()).toBeNull();
    expect(fixture.api.send).toHaveBeenCalledTimes(2);
  });
});
