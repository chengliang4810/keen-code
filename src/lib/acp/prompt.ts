/** 标准 ACP Prompt 的启动与完成生命周期。 */

import type {
  SessionPromptResult,
  SessionPromptRun,
  SessionPromptStarted,
} from "./api";
import { listenAcp } from "./api";
import type { AcpTauriDelivery } from "./events";
import { acpInitialize, acpRequest } from "./client";

/** 根 Agent 的固定来源标识；子 Agent 终态不能完成根 Prompt。 */
const ROOT_SOURCE_AGENT_ID = "root";
/** Prompt 响应先于事件到达时，允许真实 TurnStarted 迟到的最大等待时间。 */
const STARTED_EVENT_GRACE_PERIOD_MS = 5_000;

/** 启动一轮标准 ACP Prompt 所需的前端参数。 */
export interface StartSessionPromptArgs {
  /** 发给根 Agent 的完整用户文本。 */
  text: string;
  /** 目标根 Session 标识。 */
  sessionId: string;
  /** 同时作为根 Turn 标识的本轮唯一请求标识。 */
  requestId: string;
  /** 是否在发送前切换到持久 Plan 模式。 */
  planMode?: boolean;
  /** 是否为本轮启用主动委派契约。 */
  ultraMode?: boolean;
}

/** 可在任意时刻完成或失败的内部 Promise。 */
interface Deferred<Value> {
  /** 暴露给调用方的原始 Promise。 */
  promise: Promise<Value>;
  /** 以成功值完成 Promise。 */
  resolve: (value: Value | PromiseLike<Value>) => void;
  /** 以失败原因完成 Promise。 */
  reject: (cause?: unknown) => void;
}

/** 创建一个只由当前 Prompt 状态机完成的 Promise。 */
function createDeferred<Value>(): Deferred<Value> {
  let resolve!: Deferred<Value>["resolve"];
  let reject!: Deferred<Value>["reject"];
  const promise = new Promise<Value>((accept, fail) => {
    resolve = accept;
    reject = fail;
  });
  return { promise, resolve, reject };
}

/** 判断值是否为非数组普通对象。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 严格校验 ACP `session/prompt` 的成功响应。 */
function parsePromptResult(value: unknown): SessionPromptResult {
  if (!isRecord(value)) throw new Error("ACP Prompt 响应必须是对象");
  const allowedKeys = new Set(["stopReason", "_meta"]);
  if (Object.keys(value).some((key) => !allowedKeys.has(key))) {
    throw new Error("ACP Prompt 响应包含未知字段");
  }
  const stopReason = value.stopReason;
  if (
    stopReason !== "end_turn" &&
    stopReason !== "max_tokens" &&
    stopReason !== "max_turn_requests" &&
    stopReason !== "refusal" &&
    stopReason !== "cancelled"
  ) {
    throw new Error("ACP Prompt 响应 stopReason 无效");
  }
  if (Object.hasOwn(value, "_meta") && !isRecord(value._meta)) {
    throw new Error("ACP Prompt 响应 _meta 无效");
  }
  return value as unknown as SessionPromptResult;
}

/** 判断事件时间是否可作为权威 Turn 起始时间。 */
function isEventTimestamp(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

/** 构造启动前终态的安全错误；不回显 Agent 或 Provider 正文。 */
function turnEndedBeforeStartError(): Error {
  return new Error("ACP Prompt 在 TurnStarted 事件前结束");
}

/** 判断投递是否属于指定 Prompt 的 KeenCode 生命周期事件。 */
function isMatchingKeenCodeEvent(
  delivery: AcpTauriDelivery,
  sessionId: string,
  turnId: string,
): delivery is Extract<AcpTauriDelivery, { type: "keencode_event" }> {
  return delivery.type === "keencode_event" &&
    delivery.envelope.sessionId === sessionId &&
    delivery.envelope.turnId === turnId &&
    delivery.envelope.sourceAgentId === ROOT_SOURCE_AGENT_ID;
}

/** 启动一轮 ACP Prompt，并同步返回独立的 started/completed 句柄。 */
export function startSessionPrompt(
  args: StartSessionPromptArgs,
): SessionPromptRun {
  const startedDeferred = createDeferred<SessionPromptStarted>();
  const completedDeferred = createDeferred<SessionPromptResult>();

  // 两个原始 Promise 立即挂载 noop catch，调用方仍可收到原始拒绝。
  void startedDeferred.promise.catch(() => {});
  void completedDeferred.promise.catch(() => {});

  let startedObserved = false;
  let terminalObserved = false;
  let completedObserved = false;
  let listenerCleaned = false;
  let unlisten: (() => void) | null = null;
  let startedWaitTimer: ReturnType<typeof setTimeout> | null = null;

  /** 清除等待迟到 TurnStarted 的定时器。 */
  const clearStartedWaitTimer = () => {
    if (startedWaitTimer === null) return;
    clearTimeout(startedWaitTimer);
    startedWaitTimer = null;
  };

  /** 幂等移除本轮专属事件监听器。 */
  const cleanupListener = () => {
    if (listenerCleaned) return;
    listenerCleaned = true;
    clearStartedWaitTimer();
    const cleanup = unlisten;
    unlisten = null;
    cleanup?.();
  };

  /** 接收与本轮配对的生命周期事件。 */
  const handleDelivery = (delivery: AcpTauriDelivery) => {
    if (listenerCleaned) return;
    if (!isMatchingKeenCodeEvent(delivery, args.sessionId, args.requestId)) {
      return;
    }
    const event = delivery.envelope.event;
    if (event.type === "turn_started") {
      if (startedObserved || !isEventTimestamp(delivery.envelope.occurredAtMs)) {
        return;
      }
      startedObserved = true;
      clearStartedWaitTimer();
      startedDeferred.resolve({
        turnId: args.requestId,
        occurredAtMs: delivery.envelope.occurredAtMs,
      });
      if (completedObserved) cleanupListener();
      return;
    }
    if (
      event.type !== "turn_completed" &&
      event.type !== "turn_cancelled" &&
      event.type !== "turn_failed"
    ) {
      return;
    }
    terminalObserved = true;
    cleanupListener();
    // 终态不能冒充 started；Prompt 响应仍负责完成 completed。
    if (!startedObserved) startedDeferred.reject(turnEndedBeforeStartError());
  };

  /** Prompt 响应先到时，为真实启动事件保留一个有界迟到窗口。 */
  const waitForLateStartedEvent = () => {
    if (startedObserved || terminalObserved || startedWaitTimer !== null) return;
    startedWaitTimer = setTimeout(() => {
      startedWaitTimer = null;
      if (startedObserved || listenerCleaned) return;
      cleanupListener();
      startedDeferred.reject(new Error("ACP Prompt 未收到 TurnStarted 事件"));
    }, STARTED_EVENT_GRACE_PERIOD_MS);
  };

  /** 执行严格有序的 ACP 初始化、模式设置、监听注册和 Prompt 请求。 */
  const run = async () => {
    try {
      if (!args.requestId.trim()) throw new Error("requestId 不能为空");
      if (!args.sessionId.trim()) throw new Error("sessionId 不能为空");

      await acpInitialize();
      const registered = await listenAcp("acp://delivery", handleDelivery);
      if (listenerCleaned) {
        // 监听注册与回调可能在同一个微任务中竞态完成，不能遗留句柄。
        registered();
      } else {
        unlisten = registered;
      }

      await acpRequest("session/set_mode", {
        sessionId: args.sessionId,
        modeId: args.planMode === true ? "plan" : "default",
      });

      const rawResult = await acpRequest<unknown>(
        "session/prompt",
        {
          sessionId: args.sessionId,
          prompt: [{ type: "text", text: args.text }],
          _meta: {
            "keencode/turnId": args.requestId,
            "keencode/ultraMode": args.ultraMode === true,
          },
        },
        args.requestId,
      );
      const result = parsePromptResult(rawResult);
      completedObserved = true;
      completedDeferred.resolve(result);
      // 没有启动事件时保留有界窗口，允许事件投递与 JSON-RPC 响应发生竞态。
      if (startedObserved || terminalObserved) cleanupListener();
      else waitForLateStartedEvent();
    } catch (cause) {
      cleanupListener();
      if (startedObserved) {
        // 启动后的传输/解码失败只影响 completed，started 保持成功。
        completedDeferred.reject(cause);
      } else {
        // 初始化、模式设置、监听注册或启动前 Prompt 失败同时结束两个句柄。
        startedDeferred.reject(cause);
        completedDeferred.reject(cause);
      }
    }
  };

  // run 自身始终消费内部错误；两个对外 Promise 保留给调用方观察。
  void run();
  return {
    started: startedDeferred.promise,
    completed: completedDeferred.promise,
  };
}
