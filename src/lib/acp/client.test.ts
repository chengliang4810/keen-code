import { afterEach, describe, expect, it, vi } from "vitest";

/** 每个测试都重新加载客户端，避免共享握手状态污染其他断言。 */
async function loadClient() {
  vi.resetModules();
  return import("./client");
}

/** 在测试中安装 Tauri 内部 invoke 桩，并返回可检查的调用记录。 */
function stubTauriInvoke(handler: (command: string, args: unknown) => unknown) {
  const invoke = vi.fn(handler);
  vi.stubGlobal("window", { __TAURI_INTERNALS__: { invoke } });
  return invoke;
}

/** 从 ACP dispatch 调用中读取完整 JSON-RPC 消息。 */
function messageFrom(call: unknown[]): Record<string, unknown> {
  const args = call[1];
  if (typeof args !== "object" || args === null || Array.isArray(args)) {
    throw new Error("测试桩没有收到 ACP 参数");
  }
  const message = (args as Record<string, unknown>).message;
  if (typeof message !== "object" || message === null || Array.isArray(message)) {
    throw new Error("测试桩没有收到 ACP message");
  }
  return message as Record<string, unknown>;
}

describe("ACP JSON-RPC 客户端握手与边界", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("并发请求只发送一次固定初始化握手", async () => {
    let resolveInitialize!: (value: unknown) => void;
    const initializeResponse = new Promise<unknown>((resolve) => {
      resolveInitialize = resolve;
    });
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      if (message.method === "initialize") return initializeResponse;
      return {
        jsonrpc: "2.0",
        id: message.id,
        result: { ok: true },
      };
    });
    const client = await loadClient();
    const first = client.acpRequest<{ ok: boolean }>("session/list", {});
    const second = client.acpRequest<{ ok: boolean }>("session/list", {});
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    const initialize = messageFrom(invoke.mock.calls[0]!);
    expect(initialize).toMatchObject({
      jsonrpc: "2.0",
      method: "initialize",
      params: {
        protocolVersion: 1,
        clientInfo: { name: "KeenCode", version: "0.0.1" },
        clientCapabilities: { elicitation: { form: {} } },
      },
    });
    expect(initialize.id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
    );
    resolveInitialize({
      jsonrpc: "2.0",
      id: initialize.id,
      result: { protocolVersion: 1 },
    });
    await expect(Promise.all([first, second])).resolves.toEqual([
      { ok: true },
      { ok: true },
    ]);
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("握手失败后清除共享 Promise 并允许下一次重试", async () => {
    let attempt = 0;
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      if (message.method === "initialize") {
        attempt += 1;
        return {
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32001, message: "internal detail" },
        };
      }
      return null;
    });
    const client = await loadClient();
    await expect(client.acpInitialize()).rejects.toMatchObject({
      name: "AcpRpcError",
      code: -32001,
    });
    await expect(client.acpInitialize()).rejects.toMatchObject({
      name: "AcpRpcError",
      code: -32001,
    });
    expect(attempt).toBe(2);
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("拒绝不匹配的响应 ID", async () => {
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      return {
        jsonrpc: "2.0",
        id: "different-id",
        result: message.method === "initialize"
          ? { protocolVersion: 1 }
          : {},
      };
    });
    const client = await loadClient();
    await expect(client.acpRequest("session/list", {})).rejects.toThrow(
      "响应 id 与请求不一致",
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("使用调用方提供的 JSON-RPC ID 并要求响应精确匹配", async () => {
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      return {
        jsonrpc: "2.0",
        id: message.id,
        result: message.method === "initialize"
          ? { protocolVersion: 1 }
          : { accepted: true },
      };
    });
    const client = await loadClient();
    await expect(
      client.acpRequest("keencode/session/rename", { sessionId: "s", title: "t" }, "operation-1"),
    ).resolves.toEqual({ accepted: true });
    expect(messageFrom(invoke.mock.calls[1]!)).toMatchObject({
      method: "keencode/session/rename",
      id: "operation-1",
    });
  });

  it("Client 响应经 acp_dispatch 原样发送，并与并发调用共享一次 initialize", async () => {
    const responses = [
      {
        jsonrpc: "2.0" as const,
        id: "elicitation-1",
        result: {
          action: "accept" as const,
          content: { target: "local", scopes: ["read", "write"] },
        },
      },
      {
        jsonrpc: "2.0" as const,
        id: "elicitation-2",
        result: { action: "cancel" as const },
      },
    ];
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      if (message.method === "initialize") {
        return {
          jsonrpc: "2.0",
          id: message.id,
          result: { protocolVersion: 1 },
        };
      }
      expect(message).not.toHaveProperty("method");
      expect(message).not.toHaveProperty("params");
      return null;
    });
    const client = await loadClient();

    await expect(
      Promise.all(responses.map((response) => client.acpRespond(response))),
    ).resolves.toEqual([undefined, undefined]);

    expect(invoke).toHaveBeenCalledTimes(3);
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "acp_dispatch",
      "acp_dispatch",
      "acp_dispatch",
    ]);
    expect(messageFrom(invoke.mock.calls[0]!)).toMatchObject({
      jsonrpc: "2.0",
      method: "initialize",
      params: {
        protocolVersion: 1,
        clientInfo: { name: "KeenCode", version: "0.0.1" },
        clientCapabilities: { elicitation: { form: {} } },
      },
    });
    expect(messageFrom(invoke.mock.calls[1]!)).toEqual(responses[0]);
    expect(messageFrom(invoke.mock.calls[2]!)).toEqual(responses[1]);
    expect(typeof messageFrom(invoke.mock.calls[1]!).result).toBe("object");
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain(
      "acp_client_respond",
    );
  });

  it("拒绝 Client 响应的非 null ACP 传输返回值", async () => {
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      if (message.method === "initialize") {
        return {
          jsonrpc: "2.0",
          id: message.id,
          result: { protocolVersion: 1 },
        };
      }
      return { unexpected: true };
    });
    const client = await loadClient();

    await expect(
      client.acpRespond({
        jsonrpc: "2.0",
        id: "elicitation-1",
        result: { action: "cancel" },
      }),
    ).rejects.toThrow("Client 响应的传输返回值必须为 null");
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("拒绝同时包含 result 和 error 的响应", async () => {
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      return {
        jsonrpc: "2.0",
        id: message.id,
        result: message.method === "initialize"
          ? { protocolVersion: 1 }
          : {},
        error: { code: -32600 },
      };
    });
    const client = await loadClient();
    await expect(client.acpRequest("session/list", {})).rejects.toThrow(
      "result 与 error 必须二选一",
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("拒绝 code 非整数、message 非字符串或包含多余字段的错误响应", async () => {
    const malformedErrors = [
      { code: 1.5, message: "bad code" },
      { code: -32000, message: 123 },
      { code: -32000, message: "bad fields", extra: true },
    ];

    for (const malformedError of malformedErrors) {
      const invoke = stubTauriInvoke(async (_command, args) => {
        const message = messageFrom([_command, args]);
        if (message.method === "initialize") {
          return {
            jsonrpc: "2.0",
            id: message.id,
            result: { protocolVersion: 1 },
          };
        }
        return {
          jsonrpc: "2.0",
          id: message.id,
          error: malformedError,
        };
      });
      const client = await loadClient();

      await expect(client.acpRequest("session/list", {})).rejects.toThrow(
        "error 必须包含整数 code 和字符串 message",
      );
      expect(invoke).toHaveBeenCalledTimes(2);
      vi.unstubAllGlobals();
    }
  });

  it("只接受通知的 null 返回值", async () => {
    let initialize = true;
    const invoke = stubTauriInvoke(async (_command, args) => {
      const message = messageFrom([_command, args]);
      if (message.method === "initialize") {
        return {
          jsonrpc: "2.0",
          id: message.id,
          result: { protocolVersion: 1 },
        };
      }
      if (initialize) {
        initialize = false;
        return null;
      }
      return {
        jsonrpc: "2.0",
        id: message.id,
        result: {},
      };
    });
    const client = await loadClient();
    await expect(client.acpNotify("keencode/config/update", {})).resolves.toBeUndefined();
    await expect(client.acpRequest("session/list", {})).resolves.toEqual({});
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("真实 acpRequest 链只提取白名单 reason，并丢弃错误正文与原始 data", async () => {
    const cases: Array<{
      /** 当前输入边界用例的诊断标签。 */
      name: string;
      /** 模拟的标准 JSON-RPC 错误码。 */
      code: number;
      /** 不可信的可选错误扩展数据。 */
      data?: unknown;
      /** 唯一允许保留的错误分类，未识别时为空。 */
      reason: string | null;
    }> = [
      {
        name: "configuration changed",
        code: -32603,
        data: {
          "keencode/errorCode": "provider_configuration_changed",
          secret: "provider-token",
        },
        reason: "provider_configuration_changed",
      },
      {
        name: "provider not configured",
        code: -32603,
        data: {
          "keencode/errorCode": "provider_not_configured",
          nested: { message: "private provider detail" },
        },
        reason: "provider_not_configured",
      },
      {
        name: "provider reload failed",
        code: -32603,
        data: {
          "keencode/errorCode": "provider_reload_failed",
        },
        reason: "provider_reload_failed",
      },
      {
        name: "unknown reason",
        code: -32603,
        data: {
          "keencode/errorCode": "provider_secret_leaked",
          secret: "unknown reason detail",
        },
        reason: null,
      },
      {
        name: "non-string reason",
        code: -32603,
        data: {
          "keencode/errorCode": { value: "provider_not_configured" },
          secret: "object reason detail",
        },
        reason: null,
      },
      {
        name: "array data",
        code: -32603,
        data: [{ "keencode/errorCode": "provider_not_configured" }],
        reason: null,
      },
      { name: "missing data", code: -32603, reason: null },
      { name: "null data", code: -32603, data: null, reason: null },
      { name: "string data", code: -32603, data: "provider_not_configured", reason: null },
      { name: "numeric data", code: -32603, data: 401, reason: null },
      { name: "missing key", code: -32603, data: { reason: "provider_not_configured" }, reason: null },
      {
        name: "non-internal error code",
        code: -32001,
        data: {
          "keencode/errorCode": "provider_not_configured",
          secret: "wrong json-rpc code detail",
        },
        reason: null,
      },
    ];

    for (const testCase of cases) {
      const sensitiveMessage = `sensitive provider message: ${testCase.name}`;
      const invoke = stubTauriInvoke(async (_command, args) => {
        const message = messageFrom([_command, args]);
        if (message.method === "initialize") {
          return {
            jsonrpc: "2.0",
            id: message.id,
            result: { protocolVersion: 1 },
          };
        }
        const error: Record<string, unknown> = {
          code: testCase.code,
          message: sensitiveMessage,
        };
        if (Object.hasOwn(testCase, "data")) error.data = testCase.data;
        return {
          jsonrpc: "2.0",
          id: message.id,
          error,
        };
      });
      const client = await loadClient();

      let rejected: unknown;
      try {
        await client.acpRequest("session/list", {});
      } catch (error) {
        rejected = error;
      }

      expect(rejected, testCase.name).toBeInstanceOf(client.AcpRpcError);
      expect(rejected, testCase.name).toMatchObject({
        code: testCase.code,
        reason: testCase.reason,
      });
      expect(rejected, testCase.name).not.toHaveProperty("data");
      if (rejected instanceof Error) {
        expect(rejected.message, testCase.name).toBe("ACP 请求失败");
        expect(rejected.message, testCase.name).not.toContain(sensitiveMessage);
      }
      expect(JSON.stringify(rejected), testCase.name).not.toContain("sensitive");
      expect(invoke).toHaveBeenCalledTimes(2);
      vi.unstubAllGlobals();
    }
  });
});
