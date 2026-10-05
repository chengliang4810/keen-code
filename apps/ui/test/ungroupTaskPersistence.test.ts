import { expect, test, vi } from "vitest";

import { persistUngroupedTaskView } from "../../../packages/ui/src/lib/ungroupTaskPersistence.ts";
import type { ZCodeGroupedTaskViewOrderInput } from "../../../packages/services/src/session/zcodeTaskListTypes.ts";
import type {
  IZCodeTaskService,
  ZCodeGroupedTaskView,
} from "../../../packages/services/src/index.ts";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function orderInput(): ZCodeGroupedTaskViewOrderInput {
  return {
    workspaceScopes: [{ workspacePath: "C:/isolated/project" }],
    topLevelNodes: [],
    groups: [],
  };
}

function serviceDouble(): {
  service: Pick<IZCodeTaskService, "deleteTaskGroup" | "applyGroupedTaskViewOrder">;
  deleteTaskGroup: ReturnType<typeof vi.fn>;
  applyGroupedTaskViewOrder: ReturnType<typeof vi.fn>;
} {
  const deleteTaskGroup = vi.fn<IZCodeTaskService["deleteTaskGroup"]>();
  const applyGroupedTaskViewOrder = vi.fn<IZCodeTaskService["applyGroupedTaskViewOrder"]>();
  return {
    service: { deleteTaskGroup, applyGroupedTaskViewOrder },
    deleteTaskGroup,
    applyGroupedTaskViewOrder,
  };
}

test("删除 ACK 之前不发送顶层排序 RPC", async () => {
  const deletion = deferred<void>();
  const ordering = deferred<ZCodeGroupedTaskView>();
  const { service, deleteTaskGroup, applyGroupedTaskViewOrder } = serviceDouble();
  deleteTaskGroup.mockReturnValueOnce(deletion.promise);
  applyGroupedTaskViewOrder.mockReturnValueOnce(ordering.promise);
  const order = orderInput();

  const pending = persistUngroupedTaskView({ service, groupId: "group-1", order });
  await Promise.resolve();
  expect(deleteTaskGroup).toHaveBeenCalledWith({
    groupId: "group-1",
    workspaceScopes: order.workspaceScopes,
  });
  expect(applyGroupedTaskViewOrder).not.toHaveBeenCalled();

  deletion.resolve();
  await Promise.resolve();
  await Promise.resolve();
  expect(applyGroupedTaskViewOrder).toHaveBeenCalledWith(order);

  const persisted = { nodes: [] } satisfies ZCodeGroupedTaskView;
  ordering.resolve(persisted);
  await expect(pending).resolves.toEqual(persisted);
});

test("删除 ACK 后排序失败显式暴露阶段且不吞掉错误", async () => {
  const orderError = new Error("order write unavailable");
  const { service, deleteTaskGroup, applyGroupedTaskViewOrder } = serviceDouble();
  deleteTaskGroup.mockResolvedValueOnce();
  applyGroupedTaskViewOrder.mockRejectedValueOnce(orderError);

  const failure = persistUngroupedTaskView({
    service,
    groupId: "group-1",
    order: orderInput(),
  });

  await expect(failure).rejects.toMatchObject({
    name: "UngroupTaskOrderPersistenceError",
    stage: "apply-order",
    cause: orderError,
  });
  expect(deleteTaskGroup).toHaveBeenCalledTimes(1);
  expect(applyGroupedTaskViewOrder).toHaveBeenCalledTimes(1);
});
