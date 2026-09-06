import type { AppDialog } from "@/features/app/models";

/** 退出确认对话框的具体类型。 */
export type ExitConfirmationDialog = Extract<
  NonNullable<AppDialog>,
  { kind: "confirm" }
>;

/** 将退出失败转换为可在确认对话框中持续展示的中文说明。 */
export function formatExitFailure(error: unknown): string {
  const detail = getExitFailureDetail(error);
  const normalized = detail.trim() || "退出清理未完成";
  return `退出失败：${normalized}\n应用保持打开，不会自动重试或强制退出。请保留此错误用于排查。`;
}

/** 提取调用层可能返回的错误文本，不改变原始错误语义。 */
function getExitFailureDetail(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  if (typeof error === "object" && error !== null && "message" in error) {
    return String(error.message);
  }
  return String(error ?? "");
}

/** 创建退出失败提示对话框；失败提示不再提供再次退出的动作。 */
export function createExitFailure(error: unknown): ExitConfirmationDialog {
  return {
    kind: "confirm",
    title: "退出未完成",
    message: formatExitFailure(error),
    confirmLabel: "知道了",
    danger: false,
    onConfirm: () => {},
  };
}

/** 创建退出确认对话框，并把失败结果转换为独立的失败提示。 */
export function createExitConfirmation(
  activeCount: number,
  confirmExit: () => Promise<void>,
  showDialog: (dialog: ExitConfirmationDialog) => void,
): ExitConfirmationDialog {
  const baseMessage =
    activeCount > 0
      ? `仍有 ${activeCount} 个任务正在运行。退出会中断这些任务及其启动的终端进程。下次启动后，你可以进入原任务并手动输入“继续”。`
      : "没有正在运行的任务。";

  /** 执行一次用户明确确认的退出，不在失败后自动重试。 */
  const onConfirm = async (): Promise<void> => {
    try {
      await confirmExit();
    } catch (error) {
      showDialog(createExitFailure(error));
    }
  };

  return {
    kind: "confirm",
    title: "退出 KeenCode？",
    message: baseMessage,
    confirmLabel: "停止任务并退出",
    danger: true,
    onConfirm,
  };
}
