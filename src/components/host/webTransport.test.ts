import { afterEach, describe, expect, it, vi } from "vitest";
import { createBrowserWebHostTransport } from "./webTransport";

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  static readonly OPEN = 1;
  static readonly CLOSED = 3;
  readonly url: string;
  readonly sent: string[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(value: string): void {
    this.sent.push(value);
  }

  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }

  open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  receive(value: unknown): void {
    this.onmessage?.({ data: JSON.stringify(value) });
  }
}

const originalFetch = globalThis.fetch;
const originalWebSocket = globalThis.WebSocket;

afterEach(() => {
  FakeWebSocket.instances = [];
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  globalThis.fetch = originalFetch;
  globalThis.WebSocket = originalWebSocket;
});

describe("Browser Web Host transport", () => {
  it("上传附件携带双提交 CSRF 并只接受同源资源元数据", async () => {
    vi.stubGlobal("document", { cookie: "other=x; keencode_csrf=csrf-token" });
    globalThis.fetch = vi.fn(async () => new Response(JSON.stringify({
      resourceId: "resource_123",
      fileName: "photo.png",
      contentType: "image/png",
      size: 4,
    }), { status: 200, headers: { "content-type": "application/json" } }));
    const transport = createBrowserWebHostTransport();
    const file = new File(["data"], "照片.png", { type: "image/png" });

    await expect(transport.uploadAttachment!(file)).resolves.toEqual({
      resourceId: "resource_123",
      fileName: "照片.png",
      contentType: "image/png",
      size: 4,
      previewUrl: "/api/resources/resource_123",
    });
    expect(globalThis.fetch).toHaveBeenCalledWith("/api/uploads", expect.objectContaining({
      method: "POST",
      credentials: "include",
      headers: expect.objectContaining({ "X-KeenCode-CSRF": "csrf-token" }),
      body: file,
    }));
  });

  it("通过登录 POST 建立 Cookie 会话，Token 不进入 WebSocket URL", async () => {
    vi.stubGlobal("window", { location: { href: "http://127.0.0.1:32123/" } });
    vi.stubGlobal("WebSocket", FakeWebSocket);
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ tokenVersion: 1, expiresInSecs: 43_200 }), { status: 200 }),
    );
    const transport = createBrowserWebHostTransport();
    const login = transport.authenticate!("secret-token");
    await new Promise((resolve) => setTimeout(resolve, 0));
    const socket = FakeWebSocket.instances.at(-1)!;
    socket.open();
    await login;

    expect(globalThis.fetch).toHaveBeenCalledWith(
      "/api/auth/login",
      expect.objectContaining({
        credentials: "include",
        body: JSON.stringify({ token: "secret-token" }),
      }),
    );
    expect(socket.url).not.toContain("secret-token");
    expect((await transport.snapshot!()).connection).toBe("connected");
  });

  it("刷新后使用 HttpOnly Cookie 会话恢复连接，不要求再次输入 Token", async () => {
    vi.stubGlobal("window", { location: { href: "http://127.0.0.1:32123/" } });
    vi.stubGlobal("WebSocket", FakeWebSocket);
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({
        authenticated: true,
        tokenVersion: 1,
        expiresInSecs: 43_200,
      }), { status: 200 }),
    );
    const transport = createBrowserWebHostTransport();
    const restored = transport.snapshot!();
    await new Promise((resolve) => setTimeout(resolve, 0));
    FakeWebSocket.instances.at(-1)!.open();

    await expect(restored).resolves.toMatchObject({ connection: "connected" });
    expect(globalThis.fetch).toHaveBeenCalledWith("/api/auth/session", {
      method: "GET",
      credentials: "include",
    });
  });

  it("严格配对 ACP 初始化响应和后续投递事件", async () => {
    vi.stubGlobal("window", { location: { href: "http://127.0.0.1:32123/" } });
    vi.stubGlobal("WebSocket", FakeWebSocket);
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ tokenVersion: 1, expiresInSecs: 43_200 }), { status: 200 }),
    );
    const transport = createBrowserWebHostTransport();
    const connection = transport.authenticate!("secret-token");
    await new Promise((resolve) => setTimeout(resolve, 0));
    const socket = FakeWebSocket.instances.at(-1)!;
    socket.open();
    await connection;

    const deliveries: unknown[] = [];
    transport.subscribeDelivery!((value) => deliveries.push(value));
    const request = transport.dispatch!({
      jsonrpc: "2.0",
      id: "request-1",
      method: "session/list",
      params: {},
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const initialize = JSON.parse(socket.sent[0]);
    expect(initialize.method).toBe("initialize");
    socket.receive({ jsonrpc: "2.0", id: initialize.id, result: { protocolVersion: 1 } });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const outbound = JSON.parse(socket.sent.at(-1)!);
    expect(outbound.method).toBe("session/list");
    socket.receive({ jsonrpc: "2.0", id: "request-1", result: { sessions: [] } });
    await expect(request).resolves.toEqual({
      jsonrpc: "2.0",
      id: "request-1",
      result: { sessions: [] },
    });

    socket.receive({ type: "session_update", envelope: { sessionId: "s" } });
    expect(deliveries).toEqual([{ type: "session_update", envelope: { sessionId: "s" } }]);
  });

  it("Client Response 单向发送，不建立悬挂的 pending 请求", async () => {
    vi.stubGlobal("window", { location: { href: "http://127.0.0.1:32123/" } });
    vi.stubGlobal("WebSocket", FakeWebSocket);
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ tokenVersion: 1, expiresInSecs: 43_200 }), { status: 200 }),
    );
    const transport = createBrowserWebHostTransport();
    const login = transport.authenticate!("secret-token");
    await new Promise((resolve) => setTimeout(resolve, 0));
    const socket = FakeWebSocket.instances.at(-1)!;
    socket.open();
    await login;
    const initializeRequest = transport.dispatch!({
      jsonrpc: "2.0",
      id: "initialize-1",
      method: "initialize",
      params: { protocolVersion: 1 },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const initialize = JSON.parse(socket.sent.at(-1)!);
    socket.receive({ jsonrpc: "2.0", id: initialize.id, result: { protocolVersion: 1 } });
    await initializeRequest;
    await transport.dispatch!({
      jsonrpc: "2.0",
      id: "elicitation-1",
      result: { action: "cancel" },
    });
    expect(JSON.parse(socket.sent.at(-1)!)).toEqual({
      jsonrpc: "2.0",
      id: "elicitation-1",
      result: { action: "cancel" },
    });
  });
});
