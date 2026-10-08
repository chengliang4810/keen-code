import type { UIMessage } from "ai";

export type TaskFileChange = {
  path: string;
  state: "pending" | "applied" | "failed";
};

/** 只统计本会话的文件写入工具；返回 error 或计划队列不能显示成已落盘。 */
export function taskFileChanges(
  messages: readonly UIMessage[],
): TaskFileChange[] {
  const changes = new Map<string, TaskFileChange>();
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    for (const part of message.parts) {
      if (
        !["tool-write_file", "tool-edit", "tool-multi_edit"].includes(part.type)
      )
        continue;
      const tool = part as unknown as {
        input?: { path?: unknown };
        output?: {
          path?: unknown;
          error?: unknown;
          queued_for_plan_review?: unknown;
        };
        state: string;
      };
      const path = tool.output?.path ?? tool.input?.path;
      if (typeof path !== "string" || !path) continue;
      // 计划入队只表示提案，落盘结果由审查面板展示，不能当作已执行文件操作。
      if (tool.output?.queued_for_plan_review) continue;
      if (tool.state === "output-denied" || tool.state === "approval-responded")
        continue;
      if (tool.state === "approval-requested")
        changes.set(path, { path, state: "pending" });
      else if (tool.state === "output-error" || tool.output?.error)
        changes.set(path, { path, state: "failed" });
      else if (tool.state === "output-available")
        changes.set(path, {
          path,
          state: "applied",
        });
    }
  }
  return [...changes.values()];
}
