import { useEffect, useRef, useState } from "react";
import type { AppDialog } from "@/features/app/models";
import * as api from "@/lib/api";
import { createExitConfirmation, createExitFailure } from "@/lib/appExit";

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
    let unlistenExitRequested: (() => void) | undefined;
    let unlistenExitFailed: (() => void) | undefined;

    const showExitConfirmation = (activeCount: number) => {
      if (disposed) return;
      setAppDialog(
        createExitConfirmation(
          activeCount,
          api.appConfirmExit,
          (dialog) => {
            if (!disposed) setAppDialog(dialog);
          },
        ),
      );
    };

    /** 显示退出失败提示，并在组件卸载后丢弃迟到结果。 */
    const showExitFailure = (error: unknown) => {
      if (disposed) return;
      setAppDialog(createExitFailure(error));
    };

    void (async () => {
      const [{ getCurrentWindow }, { listen }] = await Promise.all([
        import("@tauri-apps/api/window"),
        import("@tauri-apps/api/event"),
      ]);
      unlistenExitRequested = await listen<{
        activeCount: number;
      }>(
        "app://exit-requested",
        (event) => showExitConfirmation(event.payload.activeCount),
      );
      unlistenExitFailed = await listen<{ message: string }>(
        "app://exit-failed",
        (event) => showExitFailure(event.payload.message),
      );
      unlistenClose = await getCurrentWindow().onCloseRequested((event) => {
        event.preventDefault();
        void api.appRequestExit().catch((error) => {
          showExitFailure(error);
        });
      });
      if (disposed) {
        unlistenExitRequested();
        unlistenExitFailed();
        unlistenClose();
      }
    })();

    return () => {
      disposed = true;
      unlistenExitRequested?.();
      unlistenExitFailed?.();
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
