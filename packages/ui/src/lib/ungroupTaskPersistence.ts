import type {
  IZCodeTaskService,
  ZCodeGroupedTaskView,
  ZCodeGroupedTaskViewOrderInput,
} from "@zcode/services";

export class UngroupTaskOrderPersistenceError extends Error {
  readonly stage = "apply-order" as const;

  constructor(readonly cause: unknown) {
    super("取消分组已完成，但顶层任务排序保存失败");
    this.name = "UngroupTaskOrderPersistenceError";
  }
}

/**
 * 取消分组必须先等待删除 RPC 成功，再写入顶层排序。
 *
 * 两次写入之间允许宿主重启，因此删除成功后排序失败也不能把旧 group 恢复到
 * UI；调用方通过错误类型识别这个阶段，并保留已删除的视图事实。
 */
export async function persistUngroupedTaskView(params: {
  service: Pick<IZCodeTaskService, "deleteTaskGroup" | "applyGroupedTaskViewOrder">;
  groupId: string;
  order: ZCodeGroupedTaskViewOrderInput;
}): Promise<ZCodeGroupedTaskView> {
  await params.service.deleteTaskGroup({
    groupId: params.groupId,
    workspaceScopes: params.order.workspaceScopes,
  });
  try {
    return await params.service.applyGroupedTaskViewOrder(params.order);
  } catch (error) {
    throw new UngroupTaskOrderPersistenceError(error);
  }
}
