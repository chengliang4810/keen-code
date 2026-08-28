import { useEffect, useRef, useState } from "react";
import type { AppDialog } from "@/features/app/models";
import * as api from "@/lib/api";

/** 管理应用级确认/输入弹窗，以及 Tauri 退出请求的统一处理。 */
export function useAppDialog() {
  const [appDialog, setAppDialog] = useState<AppDialog>(null);
  const [dialogInput, setDialogInput] = useState("");
  const dialogInputRef = useRef<HTMLInputElement>(null);
  const confirmBtnRef = useRef<HTMLButtonElement>(null);
  /** Latest dialog for Enter/Escape handlers (avoids stale chained confirms). */
  const appDialogRef = useRef<AppDialog>(null);
  appDialogRef.current = appDialog;

  useEffect(() => {
    if (!appDialog) return;
    if (appDialog.kind === "prompt") {
      setDialogInput(appDialog.initial);
      const t = window.setTimeout(() => {
        dialogInputRef.current?.focus();
        dialogInputRef.current?.select();
      }, 0);
      return () => window.clearTimeout(t);
    }
    // Confirm: focus primary action so keyboard users land on Confirm.
    // Enter is also handled globally below so it still confirms if focus
    // sits on Cancel / close (needed for reliable multi-step confirmation).
    if (appDialog.kind === "confirm") {
      const t = window.setTimeout(() => {
        confirmBtnRef.current?.focus();
      }, 0);
      return () => window.clearTimeout(t);
    }
  }, [appDialog]);

  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    let unlistenClose: (() => void) | undefined;
    let unlistenExit: (() => void) | undefined;

    const showExitConfirmation = (activeCount: number) => {
      if (disposed) return;
      setAppDialog({
        kind: "confirm",
        title: "退出 KeenCode？",
        message: `仍有 ${activeCount} 个任务正在运行。退出会中断这些任务及其启动的终端进程。下次启动后，你可以进入原任务并手动输入“继续”。`,
        confirmLabel: "停止任务并退出",
        danger: true,
        onConfirm: api.appConfirmExit,
      });
    };

    void (async () => {
      const [{ getCurrentWindow }, { listen }] = await Promise.all([
        import("@tauri-apps/api/window"),
        import("@tauri-apps/api/event"),
      ]);
      unlistenExit = await listen<{ activeCount: number }>(
        "app://exit-requested",
        (event) => showExitConfirmation(event.payload.activeCount),
      );
      unlistenClose = await getCurrentWindow().onCloseRequested((event) => {
        event.preventDefault();
        void api.appRequestExit();
      });
      if (disposed) {
        unlistenExit();
        unlistenClose();
      }
    })();

    return () => {
      disposed = true;
      unlistenExit?.();
      unlistenClose?.();
    };
  }, []);

  useEffect(() => {
    if (!appDialog) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setAppDialog(null);
        return;
      }
      // Confirm dialogs: Enter always accepts, including chained confirmations.
      // Capture phase + preventDefault so we don't double-fire with a focused
      // submit button's native activation.
      if (e.key !== "Enter" && e.key !== "NumpadEnter") return;
      if (e.isComposing || e.altKey || e.ctrlKey || e.metaKey) return;
      const dialog = appDialogRef.current;
      if (!dialog || dialog.kind !== "confirm") return;
      e.preventDefault();
      e.stopPropagation();
      const run = dialog.onConfirm;
      setAppDialog(null);
      void run();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [appDialog]);

  return {
    appDialog,
    setAppDialog,
    dialogInput,
    setDialogInput,
    dialogInputRef,
    confirmBtnRef,
    appDialogRef,
  };
}
