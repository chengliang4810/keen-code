import { beforeEach, describe, expect, it, vi } from "vitest";
import { VSBuffer } from "../../../packages/rpc/src/buffer.js";
import { BufferReader, BufferWriter, deserialize, serialize } from "../../../packages/rpc/src/serialization.js";

// 这里只隔离 IPC 边界，检查真实 TauriProtocol 的字节和生命周期契约；
// 真实模型与桌面验收使用另一个原生入口，不使用这里的测试替身。
const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: bridge.invoke,
  Channel: class { onmessage = (_message: unknown) => {}; },
}));

import { connectViaTauri, TauriProtocol } from "../../../packages/client/src/tauri.js";

const channel = () => ({ onmessage: (_message: unknown) => {} });

beforeEach(() => bridge.invoke.mockReset());

describe("原始 Channel RPC 的 Tauri 承载", () => {
  it("订阅错误不会作为业务帧投递，也不会使其他模型服务连接失效", async () => {
    let input: ReturnType<typeof channel> | undefined;
    const respond = (header: unknown[], body: unknown) => {
      const writer = new BufferWriter();
      serialize(writer, header); serialize(writer, body);
      input?.onmessage(writer.buffer.buffer);
    };
    bridge.invoke.mockImplementation(async (command: string, args: unknown) => {
      if (command === "zcode_rpc_open") {
        input = (args as { onMessage: ReturnType<typeof channel> }).onMessage;
        respond([200], undefined);
        return "owned";
      }
      if (command === "zcode_rpc_send") {
        const reader = new BufferReader(VSBuffer.wrap(args as Uint8Array));
        const header = deserialize(reader) as [number, number, string, string];
        if (header[0] === 102) {
          respond([202, header[1]], { name: "RpcError", code: "rpc.unknownMethod", message: "测试订阅失败" });
        } else if (header[0] === 100) {
          respond([201, header[1]], { revision: 1, providers: [] });
        }
      }
      return undefined;
    });
    const connection = await connectViaTauri();
    const events: unknown[] = [];
    const subscription = connection.services.windowControllerService.onDynamicControllerFrame()((event) => events.push(event));
    await connection.protocol.drain();
    expect(events).toEqual([]);
    expect(bridge.invoke.mock.calls).toContainEqual(["diagnostics_record", {
      component: "frontend.rpc.subscription",
      message: "window-controller.onDynamicControllerFrame (rpc.unknownMethod)",
    }]);
    await expect(connection.services.modelSelectionService.getView()).resolves.toEqual({ revision: 1, providers: [] });
    expect(bridge.invoke.mock.calls.some(([command]) => command === "zcode_rpc_close")).toBe(false);
    subscription.dispose();
    await connection.protocol.close();
  });
  it("open 响应之前收到 Initialize 仍完成握手，close 收口真实 ProxyChannel 的待决请求", async () => {
    bridge.invoke.mockImplementation(async (command: string, args: unknown) => {
      if (command === "zcode_rpc_open") {
        const writer = new BufferWriter();
        serialize(writer, [200]); serialize(writer, undefined);
        (args as { onMessage: ReturnType<typeof channel> }).onMessage.onmessage(writer.buffer.buffer);
        return "owned";
      }
      return undefined;
    });
    const connection = await connectViaTauri();
    const response = connection.services.systemService.info();
    const rejected = expect(response).rejects.toMatchObject({ name: "ConnectionClosed" });
    await vi.waitFor(() => expect(bridge.invoke.mock.calls.some(([command]) => command === "zcode_rpc_send")).toBe(true));
    const raw = bridge.invoke.mock.calls.find(([command]) => command === "zcode_rpc_send")?.[1] as Uint8Array;
    const reader = new BufferReader(VSBuffer.wrap(raw));
    expect(deserialize(reader)).toEqual([100, 0, "system", "info"]);
    await connection.protocol.close();
    await rejected;
  });

  it("连接在 Initialize 之前关闭也收口排队请求", async () => {
    bridge.invoke.mockResolvedValueOnce("owned").mockResolvedValue(undefined);
    const connection = await connectViaTauri();
    const response = connection.services.systemService.info();
    const rejected = expect(response).rejects.toMatchObject({ name: "ConnectionClosed" });
    await connection.protocol.close();
    await rejected;
    expect(bridge.invoke.mock.calls.some(([command]) => command === "zcode_rpc_send")).toBe(false);
  });

  it("初始化期间的无效数组会关闭已创建的宿主连接", async () => {
    bridge.invoke.mockImplementation(async (command: string, args: unknown) => {
      if (command === "zcode_rpc_open") {
        (args as { onMessage: ReturnType<typeof channel> }).onMessage.onmessage([300]);
        return "owned";
      }
      return undefined;
    });
    await expect(connectViaTauri()).rejects.toThrow("无效字节数组");
    expect(bridge.invoke.mock.calls[1]).toEqual(["zcode_rpc_close", { connectionId: "owned" }]);
  });

  it("发送原字节副本与连接头，等待前一条 IPC 完成后再发送下一条", async () => {
    let firstDone: (() => void) | undefined;
    bridge.invoke.mockImplementationOnce(() => new Promise<void>((done) => { firstDone = done; }));
    bridge.invoke.mockResolvedValue(undefined);
    const protocol = new TauriProtocol("owned-connection", channel() as never);
    const source = new Uint8Array([0, 1, 100, 255]);
    protocol.send(VSBuffer.wrap(source));
    protocol.send(VSBuffer.wrap(new Uint8Array([3, 7])));
    source[0] = 9;
    await vi.waitFor(() => expect(bridge.invoke).toHaveBeenCalledTimes(1));
    expect(bridge.invoke.mock.calls[0]).toEqual([
      "zcode_rpc_send", new Uint8Array([0, 1, 100, 255]),
      { headers: { "x-keencode-rpc-connection": "owned-connection" } },
    ]);
    firstDone?.();
    await protocol.drain();
    expect(bridge.invoke.mock.calls[1]?.[1]).toEqual(new Uint8Array([3, 7]));
    await protocol.close();
  });

  it("接收 Tauri 的三个二进制载荷形态，拒绝其他形态且继续接收", async () => {
    bridge.invoke.mockResolvedValue(undefined);
    const input = channel();
    const protocol = new TauriProtocol("owned", input as never);
    const messages: number[][] = [];
    const errors: Error[] = [];
    protocol.onMessage((value) => messages.push(Array.from(value.buffer)));
    protocol.onError((error) => errors.push(error));
    input.onmessage(new Uint8Array([0, 255]));
    input.onmessage(new Uint8Array([1, 2]).buffer);
    input.onmessage([3, 4]);
    input.onmessage({ data: [5] });
    input.onmessage(new Uint8Array([6]));
    expect(messages).toEqual([[0, 255], [1, 2], [3, 4], [6]]);
    expect(errors).toHaveLength(1);
    await protocol.close();
  });

  it("发送错误经诊断事件通知且不阻塞下一条消息", async () => {
    bridge.invoke.mockRejectedValueOnce(new Error("IPC disconnected"));
    bridge.invoke.mockResolvedValue(undefined);
    const protocol = new TauriProtocol("owned", channel() as never);
    const errors: Error[] = [];
    protocol.onError((error) => errors.push(error));
    protocol.send(VSBuffer.wrap(new Uint8Array([1])));
    protocol.send(VSBuffer.wrap(new Uint8Array([2])));
    await protocol.drain();
    expect(errors.map((error) => error.message)).toEqual(["IPC disconnected"]);
    expect(bridge.invoke).toHaveBeenCalledTimes(2);
    await protocol.close();
  });

  it("关闭幂等，只关闭自己的连接并丢弃迟到消息", async () => {
    bridge.invoke.mockResolvedValue(undefined);
    const input = channel();
    const protocol = new TauriProtocol("owned", input as never);
    const messages: number[][] = [];
    protocol.onMessage((value) => messages.push(Array.from(value.buffer)));
    await protocol.close();
    await protocol.close();
    input.onmessage(new Uint8Array([3]));
    protocol.send(VSBuffer.wrap(new Uint8Array([4])));
    await protocol.drain();
    expect(bridge.invoke.mock.calls).toEqual([["zcode_rpc_close", { connectionId: "owned" }]]);
    expect(messages).toEqual([]);
  });
});
