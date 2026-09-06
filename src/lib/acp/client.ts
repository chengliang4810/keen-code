/** ACP JSON-RPC 客户端：所有前端出站请求都通过唯一 Tauri 命令发送。 */

import { invoke } from "../tauri";
import type { AcpJsonRpcId } from "./events";

/** 对外复用 ACP JSON-RPC 请求标识类型；默认请求标识仍由客户端生成 UUID。 */
export type { AcpJsonRpcId } from "./events";

/** ACP 初始化响应的最小客户端契约；其余能力字段由业务层按需读取。 */
export interface AcpInitializeResult {
  /** Agent 实际协商出的 ACP 协议版本。 */
  protocolVersion: number;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
  /** Agent 声明的能力目录。 */
  [key: string]: unknown;
}

/** Host 允许通过 JSON-RPC InternalError 传递的封闭错误原因。 */
export type AcpRpcErrorReason =
  | "provider_configuration_changed"
  | "provider_not_configured"
  | "provider_reload_failed"
  | null;

/** JSON-RPC 错误响应的安全错误；只保留错误码和受信原因，不暴露服务端正文。 */
export class AcpRpcError extends Error {
  /** ACP/JSON-RPC 原始错误码。 */
  readonly code: number;

  /** Host 传递的固定错误原因；未识别或不适用时为 null。 */
  readonly reason: AcpRpcErrorReason;

  /** 保留协议错误码和封闭原因，同时避免回显远端提供的任意错误正文。 */
  constructor(code: number, reason: AcpRpcErrorReason = null) {
    super("ACP 请求失败");
    this.name = "AcpRpcError";
    this.code = code;
    this.reason = reason;
  }
}

/** JSON-RPC 2.0 请求信封。 */
interface AcpRequestMessage {
  /** JSON-RPC 协议版本。 */
  jsonrpc: "2.0";
  /** 请求唯一标识。 */
  id: AcpJsonRpcId;
  /** ACP 方法名。 */
  method: string;
  /** ACP 方法参数。 */
  params: Record<string, unknown>;
}

/** JSON-RPC 2.0 通知信封。 */
interface AcpNotificationMessage {
  /** JSON-RPC 协议版本。 */
  jsonrpc: "2.0";
  /** ACP 方法名。 */
  method: string;
  /** ACP 方法参数。 */
  params: Record<string, unknown>;
}

/** Client 对 Agent 发起的标准 JSON-RPC 请求的成功响应。 */
interface AcpResponseMessage {
  /** JSON-RPC 协议版本。 */
  jsonrpc: "2.0";
  /** 必须与 Agent 请求完全相同的标识。 */
  id: AcpJsonRpcId;
  /** 调用方构造的标准响应载荷。 */
  result: unknown;
}

/** 共享的固定初始化参数；表单能力字段来自 ACP 0.11.7 Schema。 */
const INITIALIZE_PARAMS: Record<string, unknown> = {
  protocolVersion: 1,
  clientInfo: {
    name: "KeenCode",
    version: "0.0.1",
  },
  clientCapabilities: {
    elicitation: {
      form: {},
    },
  },
};

/** 当前进程唯一共享握手 Promise；失败时会清空以允许下一次调用重试。 */
let initializePromise: Promise<AcpInitializeResult> | null = null;

/** 判断未知值是否为非数组普通对象。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 从 InternalError 的固定 data key 中解析封闭错误原因。 */
function parseAcpRpcErrorReason(
  code: number,
  data: unknown,
): AcpRpcErrorReason {
  if (code !== -32603 || !isRecord(data) || !Object.hasOwn(data, "keencode/errorCode")) {
    return null;
  }
  const reason = data["keencode/errorCode"];
  if (reason === "provider_configuration_changed" ||
    reason === "provider_not_configured" ||
    reason === "provider_reload_failed") {
    return reason;
  }
  return null;
}

/** 构造只包含安全摘要的协议错误。 */
function protocolError(message: string): Error {
  return new Error(`ACP 响应无效：${message}`);
}

/** 调用唯一 Tauri ACP 命令，不允许业务层直接选择其他命令。 */
async function dispatch(message: AcpRequestMessage | AcpNotificationMessage | AcpResponseMessage): Promise<unknown> {
  return invoke<unknown>("acp_dispatch", { message });
}

/** 严格验证 JSON-RPC 响应外层，并返回唯一的 result 值。 */
async function requestRaw<T>(
  method: string,
  params: Record<string, unknown>,
  requestId?: AcpJsonRpcId,
): Promise<T> {
  const id = requestId ?? globalThis.crypto.randomUUID();
  const raw = await dispatch({
    jsonrpc: "2.0",
    id,
    method,
    params,
  });
  if (!isRecord(raw)) {
    throw protocolError("请求响应必须是对象");
  }
  const keys = Object.keys(raw);
  if (keys.some((key) => !["jsonrpc", "id", "result", "error"].includes(key))) {
    throw protocolError("请求响应包含未知字段");
  }
  if (raw.jsonrpc !== "2.0") {
    throw protocolError("jsonrpc 必须为 2.0");
  }
  if (raw.id !== id) {
    throw protocolError("响应 id 与请求不一致");
  }
  const hasResult = Object.hasOwn(raw, "result");
  const hasError = Object.hasOwn(raw, "error");
  if (hasResult === hasError) {
    throw protocolError("result 与 error 必须二选一");
  }
  if (hasResult) {
    return raw.result as T;
  }
  const error = raw.error;
  if (!isRecord(error) || typeof error.code !== "number" ||
    !Number.isSafeInteger(error.code) || typeof error.message !== "string" ||
    Object.keys(error).some((key) => !["code", "message", "data"].includes(key))) {
    throw protocolError("error 必须包含整数 code 和字符串 message");
  }
  throw new AcpRpcError(
    error.code,
    parseAcpRpcErrorReason(error.code, error.data),
  );
}

/** 校验并返回 ACP 版本 1 的初始化结果。 */
async function requestInitialize(): Promise<AcpInitializeResult> {
  const result = await requestRaw<unknown>("initialize", INITIALIZE_PARAMS);
  if (!isRecord(result) || typeof result.protocolVersion !== "number" ||
    !Number.isSafeInteger(result.protocolVersion)) {
    throw protocolError("initialize result 必须包含整数 protocolVersion");
  }
  if (result.protocolVersion !== 1) {
    throw protocolError("只支持 ACP protocolVersion 1");
  }
  return result as AcpInitializeResult;
}

/** 完成一次全局共享握手；失败会清除共享 Promise，下一次调用可以重试。 */
export function acpInitialize(): Promise<AcpInitializeResult> {
  if (!initializePromise) {
    const pending = requestInitialize();
    const shared = pending.catch((error: unknown) => {
      if (initializePromise === shared) initializePromise = null;
      throw error;
    });
    initializePromise = shared;
  }
  return initializePromise;
}

/** 发送一个 ACP JSON-RPC 请求，并严格校验响应信封。 */
export async function acpRequest<T>(
  method: string,
  params: Record<string, unknown>,
  requestId?: AcpJsonRpcId,
): Promise<T> {
  if (method === "initialize") {
    return acpInitialize() as Promise<T>;
  }
  await acpInitialize();
  return requestRaw<T>(method, params, requestId);
}

/** 发送一个 ACP JSON-RPC 通知；通知响应必须严格为 null。 */
export async function acpNotify(
  method: string,
  params: Record<string, unknown>,
): Promise<void> {
  await acpInitialize();
  const raw = await dispatch({
    jsonrpc: "2.0",
    method,
    params,
  });
  if (raw !== null) {
    throw protocolError("通知响应必须为 null");
  }
}

/** Client 响应也经唯一 ACP 入口发送；响应本身不再产生 JSON-RPC 响应。 */
export async function acpRespond(response: AcpResponseMessage): Promise<void> {
  await acpInitialize();
  const result = await dispatch(response);
  if (result !== null) throw protocolError("Client 响应的传输返回值必须为 null");
}
