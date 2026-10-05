import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "vitest";

import {
  controllerResyncParamsSchema,
  controllerResyncResultSchema,
  controllerSubscribeParamsSchema,
  controllerSubscribeResultSchema,
  windowHostControllerTaskFrameSchema,
  windowHostControllerTasksSnapshotSchema,
  windowHostControllerWorkspaceFrameSchema,
  windowHostControllerWorkspacesSnapshotSchema,
} from "../../../packages/shared/src/zcode-protocol-v4/controller.ts";

const fixturePath = resolve(
  import.meta.dirname,
  "../../../tooling/native-live/workflow-contract-fixtures/window_controller.json",
);

test("Rust window Controller frames, ACKs, and list output satisfy source contracts", () => {
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as {
    acks: {
      subscribe: unknown;
      resync: unknown;
    };
    list: unknown;
    tasks: unknown;
    workspace: unknown;
  };

  const workspaceFrame = windowHostControllerWorkspaceFrameSchema.parse(fixture.workspace);
  const taskFrame = windowHostControllerTaskFrameSchema.parse(fixture.tasks);
  expect(workspaceFrame.payload.kind).toBe("snapshot");
  expect(taskFrame.payload.kind).toBe("snapshot");
  if (workspaceFrame.payload.kind !== "snapshot" || taskFrame.payload.kind !== "snapshot") {
    throw new Error("Controller fixture must contain snapshot frames");
  }
  windowHostControllerWorkspacesSnapshotSchema.parse(workspaceFrame.payload.snapshot);
  const taskSnapshot = windowHostControllerTasksSnapshotSchema.parse(taskFrame.payload.snapshot);

  const subscribeAck = controllerSubscribeResultSchema.parse(fixture.acks.subscribe);
  const resyncAck = controllerResyncResultSchema.parse(fixture.acks.resync);
  expect(subscribeAck.ack).toEqual(resyncAck.ack);
  expect(subscribeAck.ack.subscriptionId).toBe(taskFrame.subscriptionId);
  expect(subscribeAck.ack.logEpoch).toBe(taskFrame.logEpoch);

  controllerSubscribeParamsSchema.parse({
    topic: taskFrame.topic,
    visibility: "foreground",
  });
  controllerResyncParamsSchema.parse({
    subscriptionId: taskFrame.subscriptionId,
    base: null,
    forceSnapshot: true,
  });

  // windowController.ts 暴露的是 TypeScript list result，shared 当前没有对应 runtime schema。
  // 列表行仍复用已由源 schema 严格校验的 task row 字段，并对 envelope/行键做 strict guard。
  const list = fixture.list;
  expect(list).toMatchObject({
    items: expect.any(Array),
    total: expect.any(Number),
    hasMore: expect.any(Boolean),
  });
  const listRecord = list as Record<string, unknown>;
  expect(Object.keys(listRecord).sort()).toEqual(["hasMore", "items", "total"]);
  const items = listRecord.items as unknown[];
  expect(listRecord.total).toBe(items.length);
  expect(listRecord.hasMore).toBe(false);

  const taskRows = taskSnapshot.tasks;
  expect(items).toHaveLength(taskRows.length);
  for (const [index, item] of items.entries()) {
    const listItem = item as Record<string, unknown>;
    const row = taskRows[index];
    const expectedKeys = [
      ...Object.keys(row.meta),
      "activity",
      "liveStatus",
      "sourceAvailability",
    ].sort();
    expect(Object.keys(listItem).sort()).toEqual(expectedKeys);
    expect(listItem).toMatchObject({
      ...row.meta,
      activity: row.activity,
      liveStatus: row.liveStatus,
      sourceAvailability: row.sourceAvailability,
    });
  }

  expect(workspaceFrame.toSeq).toBeGreaterThanOrEqual(workspaceFrame.fromSeq);
  expect(taskFrame.toSeq).toBeGreaterThanOrEqual(taskFrame.fromSeq);
});
