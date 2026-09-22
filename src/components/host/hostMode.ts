import type {
  MobileRemoteConnection,
  MobileRemoteSessionSummary,
} from "./MobileRemoteShell";
import { createBrowserWebHostTransport } from "./webTransport";

/** 宿主模式由装配层决定；业务组件只消费该值，不探测或调用宿主 API。 */
export type HostMode = "desktop" | "web" | "mobile-remote";

export function isHostMode(value: string, expected: HostMode): boolean {
  return value === expected;
}

/** 解析宿主模式时只接受固定 wire 值，不把任意 query 当作权限开关。 */
export function parseHostMode(value: unknown): HostMode | null {
  return value === "desktop" || value === "web" || value === "mobile-remote"
    ? value
    : null;
}

export interface HostTransportLoginResult {
  authenticated: boolean;
  error?: string | null;
}

export interface HostTransportSnapshot {
  connection: MobileRemoteConnection;
  session?: MobileRemoteSessionSummary | null;
  asking?: boolean;
  message?: string | null;
}

export interface HostUploadedAttachment {
  resourceId: string;
  fileName: string;
  contentType: string;
  size: number;
  previewUrl: string;
}

/**
 * 前端只依赖这组注入边界；WebSocket/ACP 传输由现有 Host adapter 实现。
 * 未注入 adapter 时，Web/Mobile UI 会显示明确的不可用状态，不伪造已连接。
 */
export interface HostTransportAdapter {
  /** 将一条已认证的 ACP JSON-RPC 帧发送给 Host Core。 */
  dispatch?: (message: unknown) => Promise<unknown>;
  /** 上传到当前认证 Web 会话；返回值只能通过同源授权资源 URL 读取。 */
  uploadAttachment?: (file: File) => Promise<HostUploadedAttachment>;
  authenticate?: (
    token: string,
  ) => HostTransportLoginResult | Promise<HostTransportLoginResult>;
  snapshot?: () => HostTransportSnapshot | Promise<HostTransportSnapshot>;
  reconnect?: () => void | Promise<void>;
  subscribe?: (
    listener: (snapshot: HostTransportSnapshot) => void,
  ) => (() => void) | Promise<() => void>;
  /** 订阅 ACP SessionUpdate、KeenCode Event 和 Client Request 投递。 */
  subscribeDelivery?: (listener: (delivery: unknown) => void) => (() => void) | Promise<() => void>;
}

/** 从 URL、注入全局或 Vite 配置读取显式宿主模式；默认保持 Desktop。 */
export function resolveHostMode(search?: string): HostMode {
  const query = search ?? (
    typeof window === "undefined" || !window.location ? "" : window.location.search
  );
  const params = new URLSearchParams(query);
  const queryMode = parseHostMode(
    params.get("hostMode") ??
      params.get("host") ??
      params.get("mode") ??
      params.get("keencodeHost"),
  );
  if (queryMode) return queryMode;

  const globals = globalThis as typeof globalThis & {
    __KEENCODE_HOST_MODE__?: unknown;
  };
  const globalMode = parseHostMode(globals.__KEENCODE_HOST_MODE__);
  if (globalMode) return globalMode;

  const configuredMode = parseHostMode(import.meta.env?.VITE_KEENCODE_HOST_MODE);
  return configuredMode ?? "desktop";
}

/** 获取由本机 Web Host 或测试/嵌入宿主注入的 transport adapter。 */
export function getInjectedHostTransportAdapter(): HostTransportAdapter | null {
  const globals = globalThis as typeof globalThis & {
    __KEENCODE_HOST_TRANSPORT__?: unknown;
    __KEENCODE_HOST_ADAPTER__?: unknown;
  };
  const candidate =
    globals.__KEENCODE_HOST_TRANSPORT__ ?? globals.__KEENCODE_HOST_ADAPTER__;
  if (!candidate && typeof window !== "undefined" && resolveHostMode() !== "desktop") {
    const adapter = createBrowserWebHostTransport();
    globals.__KEENCODE_HOST_TRANSPORT__ = adapter;
    return adapter;
  }
  if (!candidate || typeof candidate !== "object") return null;
  const adapter = candidate as HostTransportAdapter;
  if (
    adapter.authenticate !== undefined &&
    typeof adapter.authenticate !== "function"
  ) {
    return null;
  }
  if (adapter.snapshot !== undefined && typeof adapter.snapshot !== "function") {
    return null;
  }
  if (adapter.reconnect !== undefined && typeof adapter.reconnect !== "function") {
    return null;
  }
  if (adapter.subscribe !== undefined && typeof adapter.subscribe !== "function") {
    return null;
  }
  if (
    adapter.subscribeDelivery !== undefined &&
    typeof adapter.subscribeDelivery !== "function"
  ) {
    return null;
  }
  if (adapter.dispatch !== undefined && typeof adapter.dispatch !== "function") {
    return null;
  }
  if (adapter.uploadAttachment !== undefined && typeof adapter.uploadAttachment !== "function") {
    return null;
  }
  return adapter;
}
