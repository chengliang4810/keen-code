import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AcpTauriDelivery } from "./events";

const clientMocks = vi.hoisted(() => ({
  /** ACP 全局初始化握手桩。 */
  acpInitialize: vi.fn(),
  /** ACP 请求桩。 */
  acpRequest: vi.fn(),
}));

const apiMocks = vi.hoisted(() => ({
  /** `acp://delivery` 监听桩。 */
  listenAcp: vi.fn(),
}));

vi.mock("./client", () => clientMocks);
vi.mock("./api", () => apiMocks);

import { startSessionPrompt } from "./prompt";

/** 可由测试显式完成的 Promise。 */
interface Deferred<Value> {
  /** 测试控制的 Promise。 */
  promise: Promise<Value>;
  /** 以成功值结束 Promise。 */
  resolve: (value: Value | PromiseLike<Value>) => void;
  /** 以失败原因结束 Promise。 */
  reject: (cause?: unknown) => void;
}

/** 创建可控制完成顺序的 Promise。 */
function deferred<Value>(): Deferred<Value> {
  let resolve!: Deferred<Value>["resolve"];
  let reject!: Deferred<Value>["reject"];
  const promise = new Promise<Value>((accept, fail) => {
    resolve = accept;
    reject = fail;
  });
  return { promise, resolve, reject };
}

/** 构造根 Turn 的生命周期投递。 */
function delivery(
  type: "turn_started" | "turn_completed" | "turn_cancelled" | "turn_failed",
  sessionId = "session-1",
  turnId = "turn-1",
  occurredAtMs = 123,
): Extract<AcpTauriDelivery, { type: "keencode_event" }> {
  return {
    type: "keencode_event",
    envelope: {
      schemaVersion: 1,
      sessionId,
      turnId,
      sourceAgentId: "root",
      journalSequence: 1,
      deliverySequence: 1,
      occurredAtMs,
      event: type === "turn_started"
        ? { type, rootTurnId: turnId }
        : type === "turn_failed"
          ? { type, failureKind: "internal", message: "failed" }
          : { type },
    },
  };
}

/** 让监听注册完成，并返回可以投递事件的处理器和清理桩。 */
async function waitForListener(): Promise<{
  handler: (value: AcpTauriDelivery) => void;
  unlisten: ReturnType<typeof vi.fn>;
}> {
  await vi.waitFor(() => expect(apiMocks.listenAcp).toHaveBeenCalledOnce());
  const handler = apiMocks.listenAcp.mock.calls[0]?.[1] as
    | ((value: AcpTauriDelivery) => void)
    | undefined;
  if (!handler) throw new Error("监听桩未收到处理器");
  const unlisten = apiMocks.listenAcp.mock.results[0]?.value;
  if (!(unlisten instanceof Promise)) throw new Error("监听桩未返回 Promise");
  return { handler, unlisten: await unlisten };
}

describe("ACP Prompt 生命周期", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  beforeEach(() => {
    vi.resetAllMocks();
    clientMocks.acpInitialize.mockResolvedValue({ protocolVersion: 1 });
    clientMocks.acpRequest.mockImplementation((method: string) =>
      method === "session/prompt"
        ? Promise.resolve({ stopReason: "end_turn" })
        : Promise.resolve({}),
    );
    apiMocks.listenAcp.mockImplementation(async () => vi.fn());
  });

  it("同步返回句柄，按握手、模式、监听、Prompt 严格顺序发送", async () => {
    const promptResult = deferred<unknown>();
    clientMocks.acpRequest.mockImplementation((method: string) =>
      method === "session/prompt" ? promptResult.promise : Promise.resolve({}),
    );

    const run = startSessionPrompt({
      text: "检查项目",
      sessionId: "session-1",
      requestId: "turn-1",
      planMode: true,
      ultraMode: true,
    });
    expect(run.started).toBeInstanceOf(Promise);
    expect(run.completed).toBeInstanceOf(Promise);

    const { handler, unlisten } = await waitForListener();
    expect(clientMocks.acpInitialize).toHaveBeenCalledOnce();
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      1,
      "session/set_mode",
      { sessionId: "session-1", modeId: "plan" },
    );

    handler(delivery("turn_started"));
    promptResult.resolve({ stopReason: "end_turn", _meta: { trace: "ok" } });

    await expect(run.started).resolves.toEqual({
      turnId: "turn-1",
      occurredAtMs: 123,
    });
    await expect(run.completed).resolves.toEqual({
      stopReason: "end_turn",
      _meta: { trace: "ok" },
    });
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      2,
      "session/prompt",
      {
        sessionId: "session-1",
        prompt: [{ type: "text", text: "检查项目" }],
        _meta: {
          "keencode/turnId": "turn-1",
          "keencode/ultraMode": true,
        },
      },
      "turn-1",
    );
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("初始化、模式设置、监听或启动前 Prompt 失败会同时拒绝两个句柄", async () => {
    const cause = new Error("连接失败");
    clientMocks.acpInitialize.mockRejectedValue(cause);
    const run = startSessionPrompt({
      text: "检查",
      sessionId: "session-1",
      requestId: "turn-1",
    });

    await expect(run.started).rejects.toBe(cause);
    await expect(run.completed).rejects.toBe(cause);
    expect(apiMocks.listenAcp).not.toHaveBeenCalled();
  });

  it("TurnStarted 后的 Prompt 错误只拒绝 completed 并清理监听", async () => {
    const promptResult = deferred<unknown>();
    const unlisten = vi.fn();
    apiMocks.listenAcp.mockResolvedValue(unlisten);
    clientMocks.acpRequest.mockImplementation((method: string) =>
      method === "session/prompt" ? promptResult.promise : Promise.resolve({}),
    );
    const run = startSessionPrompt({
      text: "检查",
      sessionId: "session-1",
      requestId: "turn-1",
    });
    const { handler } = await waitForListener();
    handler(delivery("turn_started"));
    await expect(run.started).resolves.toEqual({
      turnId: "turn-1",
      occurredAtMs: 123,
    });
    const cause = new Error("Prompt 传输失败");
    promptResult.reject(cause);
    await expect(run.completed).rejects.toBe(cause);
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("Prompt 响应先到且完全缺少事件时，在有界窗口后拒绝 started 并清理", async () => {
    vi.useFakeTimers();
    const unlisten = vi.fn();
    apiMocks.listenAcp.mockResolvedValue(unlisten);
    clientMocks.acpRequest.mockImplementation((method: string) =>
      method === "session/prompt"
        ? Promise.resolve({ stopReason: "end_turn" })
        : Promise.resolve({}),
    );
    const run = startSessionPrompt({
      text: "检查",
      sessionId: "session-1",
      requestId: "turn-1",
    });
    await waitForListener();
    await expect(run.completed).resolves.toEqual({ stopReason: "end_turn" });
    await vi.advanceTimersByTimeAsync(4_999);
    await expect(Promise.race([
      run.started.then(() => "resolved", () => "rejected"),
      Promise.resolve("pending"),
    ])).resolves.toBe("pending");
    await vi.advanceTimersByTimeAsync(1);
    await expect(run.started).rejects.toThrow("TurnStarted");
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("匹配终态先到后，迟到的启动回调不能重新完成 started 或保留监听", async () => {
    const promptResult = deferred<unknown>();
    const unlisten = vi.fn();
    apiMocks.listenAcp.mockResolvedValue(unlisten);
    clientMocks.acpRequest.mockImplementation((method: string) =>
      method === "session/prompt" ? promptResult.promise : Promise.resolve({}),
    );
    const run = startSessionPrompt({
      text: "检查",
      sessionId: "session-1",
      requestId: "turn-1",
    });
    const { handler } = await waitForListener();
    handler(delivery("turn_completed"));
    await expect(run.started).rejects.toThrow("TurnStarted");
    handler(delivery("turn_started"));
    promptResult.resolve({ stopReason: "cancelled" });
    await expect(run.completed).resolves.toEqual({ stopReason: "cancelled" });
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("必须先让监听注册完成，再发送 set_mode 和 session/prompt", async () => {
    const registration = deferred<() => void>();
    apiMocks.listenAcp.mockReturnValue(registration.promise);
    const run = startSessionPrompt({
      text: "检查",
      sessionId: "session-1",
      requestId: "turn-1",
    });
    await vi.waitFor(() => expect(clientMocks.acpInitialize).toHaveBeenCalledOnce());
    expect(clientMocks.acpRequest).not.toHaveBeenCalled();
    registration.resolve(vi.fn());
    await vi.waitFor(() => expect(clientMocks.acpRequest).toHaveBeenCalledTimes(2));
    expect(clientMocks.acpRequest).toHaveBeenCalledWith(
      "session/set_mode",
      { sessionId: "session-1", modeId: "default" },
    );
    const { handler } = await waitForListener();
    handler(delivery("turn_started"));
    await expect(run.started).resolves.toBeDefined();
    await expect(run.completed).resolves.toBeDefined();
  });

  it("只接受匹配 Session/Turn 的启动事件，终态不会伪造 started", async () => {
    const promptResult = deferred<unknown>();
    clientMocks.acpRequest.mockImplementation((method: string) =>
      method === "session/prompt" ? promptResult.promise : Promise.resolve({}),
    );
    const run = startSessionPrompt({
      text: "检查",
      sessionId: "session-1",
      requestId: "turn-1",
    });
    const { handler, unlisten } = await waitForListener();
    handler(delivery("turn_started", "other-session", "turn-1"));
    handler(delivery("turn_started", "session-1", "other-turn"));
    const childStarted = delivery("turn_started");
    childStarted.envelope.sourceAgentId = "child";
    handler(childStarted);
    handler(delivery("turn_completed"));
    await expect(run.started).rejects.toThrow("TurnStarted");
    expect(unlisten).toHaveBeenCalledOnce();
    promptResult.resolve({ stopReason: "cancelled" });
    await expect(run.completed).resolves.toEqual({ stopReason: "cancelled" });
  });

  it("严格校验 Prompt 响应的停止原因和字段", async () => {
    const promptResult = deferred<unknown>();
    clientMocks.acpRequest.mockImplementation((method: string) =>
      method === "session/prompt"
        ? promptResult.promise
        : Promise.resolve({}),
    );
    const run = startSessionPrompt({
      text: "检查",
      sessionId: "session-1",
      requestId: "turn-1",
    });
    const { handler, unlisten } = await waitForListener();
    handler(delivery("turn_started"));
    await expect(run.started).resolves.toBeDefined();
    promptResult.resolve({ stopReason: "unknown" });
    await expect(run.completed).rejects.toThrow("stopReason");
    expect(unlisten).toHaveBeenCalledOnce();
  });
});
