import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  TopicWireFrameAssembler,
  conversationTopicFrameSchema,
  conversationTopicWireFrameSchema,
} from "../../../packages/shared/src/zcode-protocol-v4/index.ts";
import { ConversationProjectionStore } from "../../../packages/ui/src/v4/conversationProjectionStore.ts";
import type { ConversationTransport } from "../../../packages/ui/src/v4/transport.ts";
import { expect, test } from "vitest";

const fixturePath = resolve(
  import.meta.dirname,
  "../../../tooling/native-live/workflow-contract-fixtures/conversation_wire.json",
);
const onlineFixturePath = resolve(
  import.meta.dirname,
  "../../../tooling/native-live/workflow-contract-fixtures/conversation_online_wire.json",
);

test("Rust-generated complete wire is accepted by the source schema and assembler", () => {
  const wire = conversationTopicWireFrameSchema.parse(
    JSON.parse(readFileSync(fixturePath, "utf8")),
  );
  const assembler = new TopicWireFrameAssembler(conversationTopicFrameSchema);
  const events = assembler.accept(wire);

  expect(events).toHaveLength(1);
  expect(events[0]).toMatchObject({
    kind: "complete",
    deliveryKind: "initial",
    frame: wire.frame,
  });
});

test("Rust rich snapshot and online delta survive the source assembler and projection store", async () => {
  const initialWire = conversationTopicWireFrameSchema.parse(
    JSON.parse(readFileSync(fixturePath, "utf8")),
  );
  const onlineWire = conversationTopicWireFrameSchema.parse(
    JSON.parse(readFileSync(onlineFixturePath, "utf8")),
  );
  const assembler = new TopicWireFrameAssembler(conversationTopicFrameSchema);
  const initialEvents = assembler.accept(initialWire);
  const onlineEvents = assembler.accept(onlineWire);

  expect(initialEvents).toHaveLength(1);
  expect(onlineEvents).toHaveLength(1);
  expect(initialEvents[0].frame.payload.kind).toBe("snapshot");
  expect(initialEvents[0].frame.payload.snapshot.rows.window.length).toBeGreaterThan(0);
  expect(onlineEvents[0].frame.payload.kind).toBe("deltas");

  // The store receives the same complete logical frames as the production transport. The
  // fake only covers lifecycle methods because this test feeds the already-decoded frames
  // through the public handleFrame boundary; no second projection implementation is used.
  const resyncRequests: unknown[] = [];
  const transport = {
    subscribe: async () => ({
      ack: {
        subscriptionId: initialWire.subscriptionId,
        mode: "snapshot" as const,
        logEpoch: initialEvents[0].frame.payload.kind === "snapshot"
          ? initialEvents[0].frame.payload.snapshot.logEpoch
          : "epoch-contract",
      },
    }),
    activate: () => {},
    resync: async (request: unknown) => {
      resyncRequests.push(request);
      return {
        ack: {
          subscriptionId: initialWire.subscriptionId,
          mode: "snapshot" as const,
          logEpoch: "epoch-contract",
        },
      };
    },
    unsubscribe: async () => {},
    onAssemblyFault: () => () => {},
    onRuntimeRestart: () => () => {},
  } as unknown as ConversationTransport;
  const store = new ConversationProjectionStore(initialWire.topic, transport);
  await store.connect();
  store.handleFrame(initialEvents[0].frame, { deliveryKind: initialWire.deliveryKind });
  const initial = store.getState().snapshot;
  expect(initial?.rows.window.length).toBeGreaterThan(0);
  const assistant = initial?.rows.window.find((row) => row.kind === "assistantText");
  expect(assistant?.text).toBe("contract response");

  const onlineFrame = onlineEvents[0].frame;
  expect(onlineFrame.fromSeq).toBe(initial?.seq);
  expect(onlineFrame.toSeq).toBe((initial?.seq ?? 0) + 1);
  store.handleFrame(onlineFrame, { deliveryKind: onlineWire.deliveryKind });
  const next = store.getState().snapshot;
  // 这里断言动态水位衔接，而不是把新的 snapshot.seq 写死；若 delta 没有真正应用，
  // seq 会停在 initial.seq，下面的正文断言也会保持旧值。
  expect(next?.seq).toBe(onlineFrame.toSeq);
  expect(next?.rows.window.find((row) => row.kind === "assistantText")?.text).toBe(
    "contract response stream",
  );

  // 同一 logical frame 的迟到副本不能二次追加文本或推进水位。
  store.handleFrame(onlineFrame, { deliveryKind: onlineWire.deliveryKind });
  const afterDuplicate = store.getState().snapshot;
  expect(afterDuplicate?.seq).toBe(next?.seq);
  expect(afterDuplicate?.rows.window.find((row) => row.kind === "assistantText")?.text).toBe(
    "contract response stream",
  );

  // fromSeq 断档必须保留一致旧投影并触发同订阅 resync，不得猜测或本地拼接。
  const gapFrame = {
    ...onlineFrame,
    fromSeq: onlineFrame.toSeq + 1,
    toSeq: onlineFrame.toSeq + 2,
  };
  store.handleFrame(gapFrame, { deliveryKind: onlineWire.deliveryKind });
  await Promise.resolve();
  expect(resyncRequests).toHaveLength(1);
  expect(resyncRequests[0]).toMatchObject({
    subscriptionId: initialWire.subscriptionId,
    base: { logEpoch: initial?.logEpoch, seq: next?.seq },
  });
  const afterGap = store.getState().snapshot;
  expect(afterGap?.seq).toBe(next?.seq);
  expect(afterGap?.rows.window.find((row) => row.kind === "assistantText")?.text).toBe(
    "contract response stream",
  );
  await store.close();
});
