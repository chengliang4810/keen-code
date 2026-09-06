import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import {
  createExitConfirmation,
  createExitFailure,
  formatExitFailure,
  type ExitConfirmationDialog,
} from "@/lib/appExit";

/** 检查待显示结果仍使用既有确认对话框结构。 */
function asConfirmation(dialog: ExitConfirmationDialog): ExitConfirmationDialog {
  expect(dialog.kind).toBe("confirm");
  return dialog;
}

describe("应用退出保护", () => {
  it("拦截窗口关闭并明确说明任务和终端进程会中断", () => {
    const source = readFileSync(
      new URL("./hooks/useAppDialog.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain("onCloseRequested");
    expect(source).toContain("event.preventDefault()");
    expect(source).toContain("createExitConfirmation");
    expect(source).toContain("createExitFailure");
    expect(source).toContain("app://exit-failed");
  });

  it("退出清理成功时只执行一次且不重新显示对话框", async () => {
    const showDialog = vi.fn<(dialog: ExitConfirmationDialog) => void>();
    const confirmExit = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const dialog = createExitConfirmation(1, confirmExit, showDialog);

    await dialog.onConfirm();

    expect(confirmExit).toHaveBeenCalledTimes(1);
    expect(showDialog).not.toHaveBeenCalled();
    expect(dialog.message).toContain("仍有 1 个任务正在运行");
    expect(dialog.message).toContain("退出会中断这些任务及其启动的终端进程");
    expect(dialog.message).toContain("手动输入“继续”");
    expect(dialog.confirmLabel).toBe("停止任务并退出");
  });

  it("退出清理失败时保留确认对话框并禁止自动重试", async () => {
    const showDialog = vi.fn<(dialog: ExitConfirmationDialog) => void>();
    const confirmExit = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error("Runtime shutdown failed"))
      .mockResolvedValueOnce(undefined);
    const dialog = createExitConfirmation(0, confirmExit, showDialog);

    await dialog.onConfirm();

    expect(confirmExit).toHaveBeenCalledTimes(1);
    expect(showDialog).toHaveBeenCalledTimes(1);
    const failedDialog = asConfirmation(showDialog.mock.calls[0][0]);
    expect(failedDialog.title).toBe("退出未完成");
    expect(failedDialog.confirmLabel).toBe("知道了");
    expect(failedDialog.danger).toBe(false);
    expect(failedDialog.message).toContain("退出失败：Runtime shutdown failed");
    expect(failedDialog.message).toContain("应用保持打开，不会自动重试或强制退出");
    expect(failedDialog.message).toContain("请保留此错误用于排查");

    await failedDialog.onConfirm();

    expect(confirmExit).toHaveBeenCalledTimes(1);
    expect(showDialog).toHaveBeenCalledTimes(1);
  });

  it("无活跃任务的后端失败使用独立失败提示且知道了不重复调用", async () => {
    const confirmExit = vi.fn<() => Promise<void>>();
    const dialog = createExitFailure("记录刷新失败");

    expect(dialog.title).toBe("退出未完成");
    expect(dialog.confirmLabel).toBe("知道了");
    expect(dialog.danger).toBe(false);
    expect(dialog.message).toContain("退出失败：记录刷新失败");
    await dialog.onConfirm();
    expect(confirmExit).not.toHaveBeenCalled();
  });

  it("undefined 和空错误仍给出明确的退出失败说明", () => {
    expect(createExitFailure(undefined).message).toContain("退出失败：退出清理未完成");
    expect(formatExitFailure(null)).toContain("退出失败：退出清理未完成");
  });
});
