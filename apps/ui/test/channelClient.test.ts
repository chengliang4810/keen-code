import { expect, it } from "vitest";
import { VSBuffer } from "../../../packages/rpc/src/buffer.js";
import { ChannelClient } from "../../../packages/rpc/src/channelClient.js";
import { Emitter } from "../../../packages/rpc/src/foundation.js";
import { BufferReader, BufferWriter, deserialize, serialize } from "../../../packages/rpc/src/serialization.js";

function createProtocol() {
  const incoming = new Emitter<VSBuffer>();
  const sent: VSBuffer[] = [];
  return {
    incoming,
    sent,
    protocol: {
      send: (buffer: VSBuffer) => sent.push(buffer),
      onMessage: incoming.event,
    },
  };
}

function emitResponse(incoming: Emitter<VSBuffer>, header: unknown[], body: unknown = undefined): void {
  const writer = new BufferWriter();
  serialize(writer, header);
  serialize(writer, body);
  incoming.fire(writer.buffer);
}

function requestHeader(buffer: VSBuffer): unknown {
  return deserialize(new BufferReader(buffer));
}

async function flushInitializationCallbacks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

it("初始化前取消监听不会发送 103，也不会让延迟的 102 创建空订阅", async () => {
  const transport = createProtocol();
  const client = new ChannelClient(transport.protocol);
  const event = client.getChannel("events").listen("changed");
  const listener = event(() => undefined);

  listener.dispose();
  emitResponse(transport.incoming, [200]);
  await flushInitializationCallbacks();

  expect(transport.sent).toHaveLength(0);
  client.dispose();
});

it("初始化前取消后重新监听只发送最后有效代次的 102", async () => {
  const transport = createProtocol();
  const client = new ChannelClient(transport.protocol);
  const event = client.getChannel("events").listen("changed");
  const first = event(() => undefined);
  first.dispose();
  const second = event(() => undefined);

  emitResponse(transport.incoming, [200]);
  await flushInitializationCallbacks();

  expect(transport.sent.map(requestHeader)).toEqual([[102, 0, "events", "changed"]]);
  second.dispose();
  client.dispose();
});

it("已发送 102 后只发送一次 103，重新监听会重新注册 handler", async () => {
  const transport = createProtocol();
  const client = new ChannelClient(transport.protocol);
  const event = client.getChannel("events").listen("changed");
  emitResponse(transport.incoming, [200]);
  const received: unknown[] = [];
  const first = event(() => undefined);
  await flushInitializationCallbacks();

  first.dispose();
  first.dispose();
  const second = event((value) => received.push(value));

  expect(transport.sent.map(requestHeader)).toEqual([
    [102, 0, "events", "changed"],
    [103, 0],
    [102, 0, "events", "changed"],
  ]);

  emitResponse(transport.incoming, [204, 0], { revision: 2 });
  expect(received).toEqual([{ revision: 2 }]);
  second.dispose();
  client.dispose();
});
