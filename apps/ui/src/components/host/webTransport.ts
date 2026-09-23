import type {
  HostTransportAdapter,
  HostTransportLoginResult,
  HostTransportSnapshot,
  HostUploadedAttachment,
} from "./hostMode";
import { randomUuid } from "@/lib/randomUuid";

type JsonRpcId = string | number;
type JsonRecord = Record<string, unknown>;

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
}

const INITIALIZE_PARAMS: JsonRecord = {
  protocolVersion: 1,
  clientInfo: { name: "KeenCode", version: "0.0.1" },
  clientCapabilities: { elicitation: { form: {} } },
};

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isRpcId(value: unknown): value is JsonRpcId {
  return typeof value === "string" ||
    (typeof value === "number" && Number.isSafeInteger(value));
}

function requestKey(id: JsonRpcId): string {
  return `${typeof id}:${String(id)}`;
}

function safeErrorMessage(value: unknown): string {
  if (isRecord(value) && typeof value.message === "string" && value.message) {
    return value.message;
  }
  return "Web Host 登录失败。";
}

/**
 * 浏览器 Web Host transport：认证使用 HttpOnly Cookie，ACP 只经同源 WebSocket
 * 传输。Token 只存在于 authenticate 调用栈内，不进入 adapter 状态或 URL。
 */
class BrowserWebHostTransport implements HostTransportAdapter {
  private socket: WebSocket | null = null;
  private socketReady: Promise<void> | null = null;
  private initialized = false;
  private initializing: Promise<void> | null = null;
  private readonly pending = new Map<string, PendingRequest>();
  private readonly deliveryListeners = new Set<(delivery: unknown) => void>();
  private readonly snapshotListeners = new Set<
    (snapshot: HostTransportSnapshot) => void
  >();
  private snapshotState: HostTransportSnapshot = {
    connection: "unauthorized",
    session: null,
    asking: false,
    message: null,
  };

  async authenticate(token: string): Promise<HostTransportLoginResult> {
    const value = token.trim();
    if (!value) return { authenticated: false, error: "Web Token 不能为空。" };
    try {
      const response = await fetch("/api/auth/login", {
        method: "POST",
        credentials: "include",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ token: value }),
      });
      const body = await response.json().catch(() => null);
      const validSession = isRecord(body) &&
        typeof body.tokenVersion === "number" && Number.isSafeInteger(body.tokenVersion) &&
        body.tokenVersion > 0 &&
        typeof body.expiresInSecs === "number" && Number.isSafeInteger(body.expiresInSecs) &&
        body.expiresInSecs > 0;
      if (!response.ok || !validSession) {
        return { authenticated: false, error: safeErrorMessage(body) };
      }
      await this.connect();
      this.updateSnapshot({ connection: "connected", message: null });
      return { authenticated: true };
    } catch (error) {
      this.updateSnapshot({
        connection: "offline",
        message: error instanceof Error ? error.message : "无法连接 Web Host。",
      });
      return {
        authenticated: false,
        error: error instanceof Error ? error.message : "无法连接 Web Host。",
      };
    }
  }

  async dispatch(message: unknown): Promise<unknown> {
    if (!isRecord(message)) throw new Error("ACP 消息必须是对象。");
    await this.connect();
    const method = typeof message.method === "string" ? message.method : null;
    if (method && method !== "initialize") await this.ensureInitialized();
    const result = await this.send(message);
    if (method === "initialize" && isRecord(result) && Object.hasOwn(result, "result")) {
      this.initialized = true;
    }
    return result;
  }

  async uploadAttachment(file: File): Promise<HostUploadedAttachment> {
    const csrf = this.cookieValue("keencode_csrf");
    if (!csrf) throw new Error("Web 会话缺少 CSRF 凭据，请重新登录。");
    const safeName = file.name.replace(/[^\x20-\x7e]/g, "_").slice(0, 240) || "upload";
    const response = await fetch("/api/uploads", {
      method: "POST",
      credentials: "include",
      headers: {
        "X-KeenCode-CSRF": csrf,
        "X-KeenCode-File-Name": safeName,
        "Content-Type": "application/octet-stream",
      },
      body: file,
    });
    const body: unknown = await response.json().catch(() => null);
    if (!response.ok || !isRecord(body)) throw new Error(safeErrorMessage(body));
    const resourceId = body.resourceId;
    const fileName = body.fileName;
    const contentType = body.contentType;
    const size = body.size;
    if (typeof resourceId !== "string" || !/^[A-Za-z0-9_-]+$/.test(resourceId) ||
      typeof fileName !== "string" || typeof contentType !== "string" ||
      typeof size !== "number" || !Number.isSafeInteger(size) || size < 0) {
      throw new Error("Web Host 返回了无效的附件信息。");
    }
    return {
      resourceId,
      fileName: file.name || fileName,
      contentType,
      size,
      previewUrl: `/api/resources/${resourceId}`,
    };
  }

  async snapshot(): Promise<HostTransportSnapshot> {
    if (this.snapshotState.connection !== "unauthorized") return this.snapshotState;
    try {
      const response = await fetch("/api/auth/session", {
        method: "GET",
        credentials: "include",
      });
      const body: unknown = await response.json().catch(() => null);
      const validSession = response.ok && isRecord(body) && body.authenticated === true &&
        typeof body.tokenVersion === "number" && Number.isSafeInteger(body.tokenVersion) &&
        body.tokenVersion > 0 &&
        typeof body.expiresInSecs === "number" && Number.isSafeInteger(body.expiresInSecs) &&
        body.expiresInSecs > 0;
      if (!validSession) return this.snapshotState;
      await this.connect();
      this.updateSnapshot({ connection: "connected", message: null });
    } catch {
      // 没有可恢复的 Cookie 会话时保持未认证，由登录界面接管。
    }
    return this.snapshotState;
  }

  subscribe(listener: (snapshot: HostTransportSnapshot) => void): () => void {
    this.snapshotListeners.add(listener);
    return () => this.snapshotListeners.delete(listener);
  }

  subscribeDelivery(listener: (delivery: unknown) => void): () => void {
    this.deliveryListeners.add(listener);
    return () => this.deliveryListeners.delete(listener);
  }

  async reconnect(): Promise<void> {
    this.closeSocket(new Error("WebSocket reconnecting"));
    this.updateSnapshot({ connection: "reconnecting", message: null });
    await this.connect();
    await this.ensureInitialized();
    this.updateSnapshot({ connection: "connected", message: null });
  }

  private updateSnapshot(patch: Partial<HostTransportSnapshot>): void {
    this.snapshotState = { ...this.snapshotState, ...patch };
    for (const listener of this.snapshotListeners) listener(this.snapshotState);
  }

  private async connect(): Promise<void> {
    if (this.socket?.readyState === WebSocket.OPEN) return;
    if (this.socketReady) return this.socketReady;
    this.updateSnapshot({ connection: "connecting", message: null });
    const promise = new Promise<void>((resolve, reject) => {
      const socket = new WebSocket(this.websocketUrl());
      let opened = false;
      this.socket = socket;
      socket.onopen = () => {
        opened = true;
        this.socketReady = null;
        resolve();
      };
      socket.onerror = () => {
        if (!opened) reject(new Error("WebSocket 连接失败。"));
      };
      socket.onclose = () => {
        if (this.socket !== socket) return;
        this.socket = null;
        this.socketReady = null;
        this.initialized = false;
        this.rejectPending(new Error("WebSocket 连接已关闭。"));
        this.updateSnapshot({ connection: "offline" });
        if (!opened) reject(new Error("WebSocket 连接已关闭。"));
      };
      socket.onmessage = (event) => {
        if (typeof event.data === "string") {
          this.handleText(event.data);
        } else if (event.data instanceof Blob) {
          void event.data.text().then((text) => this.handleText(text));
        }
      };
    });
    this.socketReady = promise.catch((error) => {
      this.socketReady = null;
      if (this.socket?.readyState !== WebSocket.OPEN) this.socket = null;
      this.updateSnapshot({ connection: "offline" });
      throw error;
    });
    return this.socketReady;
  }

  private websocketUrl(): string {
    const url = new URL("/api/ws", window.location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    return url.toString();
  }

  private cookieValue(name: string): string | null {
    if (typeof document === "undefined") return null;
    const prefix = `${name}=`;
    const value = document.cookie.split(";").map((part) => part.trim())
      .find((part) => part.startsWith(prefix))?.slice(prefix.length);
    return value ? decodeURIComponent(value) : null;
  }

  private async ensureInitialized(): Promise<void> {
    if (this.initialized) return;
    if (this.initializing) return this.initializing;
    const id = `web-init-${randomUuid()}`;
    this.initializing = this.send({
      jsonrpc: "2.0",
      id,
      method: "initialize",
      params: INITIALIZE_PARAMS,
    }).then((value) => {
      if (!isRecord(value) || !Object.hasOwn(value, "result")) {
        throw new Error("ACP WebSocket 初始化失败。");
      }
      this.initialized = true;
    }).finally(() => {
      this.initializing = null;
    });
    return this.initializing;
  }

  private send(message: JsonRecord): Promise<unknown> {
    const socket = this.socket;
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error("WebSocket 尚未连接。"));
    }
    const id = isRpcId(message.id) ? message.id : null;
    // Host 发来的 Elicitation Response 也带 id，但它是单向 Client
    // Response，不会再收到一个 JSON-RPC 响应；只有带 method 的请求才登记
    // pending，避免移动端回答后永久悬挂一个 Promise。
    const expectsResponse = typeof message.method === "string";
    let pendingRequest: Promise<unknown> | null = null;
    if (id !== null && expectsResponse) {
      pendingRequest = new Promise((resolve, reject) => {
        this.pending.set(requestKey(id), { resolve, reject });
      });
    }
    try {
      socket.send(JSON.stringify(message));
    } catch (error) {
      if (id !== null && expectsResponse) this.pending.delete(requestKey(id));
      return Promise.reject(error);
    }
    return pendingRequest ?? Promise.resolve(null);
  }

  private handleText(text: string): void {
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch {
      return;
    }
    if (!isRecord(value)) return;
    if (value.type === "ready") return;
    if (isRpcId(value.id) && (Object.hasOwn(value, "result") || Object.hasOwn(value, "error"))) {
      const key = requestKey(value.id);
      const request = this.pending.get(key);
      if (!request) return;
      this.pending.delete(key);
      if (Object.hasOwn(value, "error")) request.reject(new Error("ACP 请求失败。"));
      else request.resolve(value);
      return;
    }
    for (const listener of this.deliveryListeners) listener(value);
  }

  private rejectPending(error: Error): void {
    for (const request of this.pending.values()) request.reject(error);
    this.pending.clear();
  }

  private closeSocket(reason: Error): void {
    this.rejectPending(reason);
    const socket = this.socket;
    this.socket = null;
    this.socketReady = null;
    this.initialized = false;
    if (socket && socket.readyState === WebSocket.OPEN) socket.close(1000, "reconnect");
  }
}

/** 创建当前同源 Web Host 使用的浏览器传输；不在 Desktop/Tauri 启动路径调用。 */
export function createBrowserWebHostTransport(): HostTransportAdapter {
  return new BrowserWebHostTransport();
}
