import type { SessionMeta } from "./sessions";
import type { AgentRunStatus } from "../store/chatStore";

/** 旧会话在首次发送时绑定目录；已绑定任务禁止跨运行环境继续执行。 */
export function resolveTaskWorkspace(
  session:
    | Pick<SessionMeta, "workspaceRoot" | "workspaceScope" | "projectless">
    | undefined,
  scope: string,
  fallbackRoot: string | null,
  independentRoot: string | null = null,
): string | null {
  if (session?.workspaceScope && session.workspaceScope !== scope) {
    throw new Error(
      "Switch to the task's workspace environment before continuing.",
    );
  }
  // 独立对话不继承上一个项目或终端的目录。
  return session?.projectless
    ? independentRoot
    : (session?.workspaceRoot ?? fallbackRoot);
}

export function isTaskRunning(status: AgentRunStatus): boolean {
  return (
    status === "thinking" ||
    status === "streaming" ||
    status === "awaiting-approval"
  );
}
