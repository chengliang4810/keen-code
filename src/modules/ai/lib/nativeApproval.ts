import { invoke } from "@tauri-apps/api/core";

// 原生循环仍在等审批，不能调用 SDK 自动重发并启动第二个 Turn。
export function respondToToolApproval(
  id: string,
  approved: boolean,
  sdk: (arg: { id: string; approved: boolean }) => void | PromiseLike<void>,
): void | PromiseLike<void> {
  if (id.startsWith("rcode:"))
    return invoke("agent_core_approve", { approvalId: id, approved });
  return sdk({ id, approved });
}
