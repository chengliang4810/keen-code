import { Channel, invoke } from "@tauri-apps/api/core";
import { ChannelClient, Emitter, VSBuffer, type IMessagePassingProtocol } from "@zcode/rpc";
import type { IServiceAccessor } from "@zcode/services";
import { RemoteServiceAccess } from "./remoteServiceAccess.js";

const RPC_CONNECTION_HEADER = "x-keencode-rpc-connection";
const MAX_PENDING_MESSAGES = 256;
const MAX_PENDING_BYTES = 16 * 1024 * 1024;

function normalizeIncomingPayload(payload: unknown): Uint8Array {
  if (payload instanceof Uint8Array) {
    return payload;
  }
  if (payload instanceof ArrayBuffer) {
    return new Uint8Array(payload);
  }
  if (Array.isArray(payload)) {
    if (payload.every((value) => Number.isInteger(value) && value >= 0 && value <= 255)) {
      return Uint8Array.from(payload as number[]);
    }
    throw new TypeError("zcode_rpc Channel 返回了无效字节数组");
  }
  throw new TypeError("zcode_rpc Channel 返回了非二进制消息");
}

/**
 * Tauri IPC 直接承载 Channel RPC 的二进制消息。
 *
 * ChannelClient 已经负责消息序列化与请求/事件语义，Tauri 层只传递原始
 * Uint8Array。这里不复用 SocketProtocol，避免额外添加 TCP/WebSocket 的 13
 * 字节帧头；连接身份通过 Tauri invoke 的请求头传给 Rust 网关。
 */
export class TauriProtocol implements IMessagePassingProtocol {
  private readonly onMessageEmitter = new Emitter<VSBuffer>();
  private readonly onErrorEmitter = new Emitter<Error>();
  private readonly onCloseEmitter = new Emitter<void>();
  private sendQueue: Promise<void> = Promise.resolve();
  private closed = false;
  private closePromise: Promise<void> | undefined;

  readonly onMessage = this.onMessageEmitter.event;
  readonly onError = this.onErrorEmitter.event;
  readonly onClose = this.onCloseEmitter.event;

  constructor(private readonly connectionId: string, channel: Channel<Uint8Array>) {
    channel.onmessage = (payload) => {
      if (this.closed) {
        return;
      }
      try {
        this.replay(normalizeIncomingPayload(payload));
      } catch (error) {
        this.onErrorEmitter.fire(error instanceof Error ? error : new Error(String(error)));
      }
    };
  }

  send(buffer: VSBuffer): void {
    if (this.closed) {
      return;
    }

    // invoke 本身是异步的，而 IMessagePassingProtocol.send 保持同步接口；
    // 用单一队列保留 RPC 消息顺序，并由 drain 等待宿主 IPC 完成。
    const payload = buffer.buffer.slice();
    this.sendQueue = this.sendQueue
      .catch((error: unknown) => {
        this.reportError(error);
      })
      .then(async () => {
        if (this.closed) {
          return;
        }
        try {
          await invoke<void>("zcode_rpc_send", payload, {
            headers: { [RPC_CONNECTION_HEADER]: this.connectionId },
          });
        } catch (error) {
          this.reportError(error);
        }
      });
  }

  async drain(): Promise<void> {
    await this.sendQueue;
  }

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    this.closed = true;
    // 先终结 ChannelClient，让排队和已发出的 Promise 都明确 reject；
    // 宿主 close 即使失败也不能使页面内请求永远悬挂。
    this.onCloseEmitter.fire();
    this.closePromise = (async () => {
      try {
        await this.drain();
        await invoke<void>("zcode_rpc_close", { connectionId: this.connectionId });
      } finally {
        this.onMessageEmitter.dispose();
        this.onErrorEmitter.dispose();
        this.onCloseEmitter.dispose();
      }
    })();
    return this.closePromise;
  }

  replay(payload: Uint8Array): void {
    if (!this.closed) {
      this.onMessageEmitter.fire(VSBuffer.wrap(payload));
    }
  }

  private reportError(error: unknown): void {
    this.onErrorEmitter.fire(error instanceof Error ? error : new Error(String(error)));
  }
}

export interface TauriServiceConnection {
  services: IServiceAccessor;
  protocol: TauriProtocol;
}

export async function connectViaTauri(): Promise<TauriServiceConnection> {
  const channel = new Channel<Uint8Array>();
  // zcode_rpc_open 可能在 invoke 返回 connectionId 前立即发送 Initialize；先缓冲首包，
  // 等 ChannelClient 完成监听后再回放，避免连接永远停留在 Uninitialized。
  const pendingMessages: Uint8Array[] = [];
  let pendingBytes = 0;
  let openingError: Error | undefined;
  channel.onmessage = (payload) => {
    if (openingError) return;
    try {
      const bytes = normalizeIncomingPayload(payload);
      if (pendingMessages.length >= MAX_PENDING_MESSAGES || pendingBytes + bytes.byteLength > MAX_PENDING_BYTES) {
        throw new Error("zcode_rpc 初始化消息超过连接预算");
      }
      pendingBytes += bytes.byteLength;
      pendingMessages.push(bytes);
    } catch (error) {
      openingError = error instanceof Error ? error : new Error(String(error));
    }
  };
  const connectionId = await invoke<string>("zcode_rpc_open", { onMessage: channel });
  if (openingError) {
    await invoke<void>("zcode_rpc_close", { connectionId });
    throw openingError;
  }
  const protocol = new TauriProtocol(connectionId, channel);
  const channelClient = new ChannelClient(protocol);
  const serviceErrorListener = channelClient.onDidEventError(({ channel: service, event, code }) => {
    void invoke("diagnostics_record", {
      component: "frontend.rpc.subscription",
      message: `${service}.${event} (${code})`,
    }).catch(() => console.error("[RPC] 无法记录服务订阅失败"));
  });
  const errorListener = protocol.onError((error) => {
    // 服务错误通过 RPC response 返回；IPC/二进制错误意味着连接已失效。
    // 释放真实 ChannelClient，旧请求不能跨重连复用或等待新的 Initialize。
    channelClient.dispose(error);
    void protocol.close().catch(() => {});
  });
  protocol.onClose(() => {
    errorListener.dispose();
    serviceErrorListener.dispose();
    channelClient.dispose();
  });
  for (const payload of pendingMessages) {
    protocol.replay(payload);
  }
  return {
    protocol,
    services: new RemoteServiceAccess(channelClient),
  };
}
