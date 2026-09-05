import { createPortal } from "react-dom";
import { useEffect, useRef, type FormEvent, type RefObject } from "react";
import type { AppDialog } from "@/features/app/models";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IconClose } from "@/components/icons";
import { trapTabKey } from "@/lib/a11yFocus";
import type { SetState, Translator } from "./types";

export interface AppDialogPortalProps {
  tr: Translator;
  appDialog: AppDialog;
  setAppDialog: SetState<AppDialog>;
  dialogInput: string;
  setDialogInput: SetState<string>;
  dialogInputRef: RefObject<HTMLInputElement | null>;
  confirmBtnRef: RefObject<HTMLButtonElement | null>;
  appDialogRef: RefObject<AppDialog>;
}

export function AppDialogPortal({
  tr,
  appDialog,
  setAppDialog,
  dialogInput,
  setDialogInput,
  dialogInputRef,
  confirmBtnRef,
  appDialogRef,
}: AppDialogPortalProps) {
  /** 当前弹窗容器，用于把 Tab 键焦点限制在模态区域内。 */
  const dialogRef = useRef<HTMLDivElement | null>(null);
  /** 打开弹窗前的焦点元素，最终关闭弹窗时恢复。 */
  const previousFocusRef = useRef<HTMLElement | null>(null);
  /** 只区分弹窗是否存在，链式替换弹窗时保持同一焦点生命周期。 */
  const hasDialog = appDialog !== null;

  // 仅以弹窗是否存在作为依赖，避免链式弹窗替换时错误恢复焦点。
  useEffect(() => {
    if (!hasDialog) return;

    previousFocusRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;

    /** 把 Tab 与 Shift+Tab 循环限制在当前弹窗内。 */
    const onKeyDown = (event: KeyboardEvent) => {
      trapTabKey(event, dialogRef.current);
    };
    document.addEventListener("keydown", onKeyDown, true);

    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      const previous = previousFocusRef.current;
      previousFocusRef.current = null;
      if (previous?.isConnected) previous.focus();
    };
  }, [hasDialog]);

  if (!appDialog || typeof document === "undefined") return null;

  const submitConfirm = () => {
    const dialog = appDialogRef.current;
    if (!dialog || dialog.kind !== "confirm") return;
    const run = dialog.onConfirm;
    setAppDialog(null);
    void run();
  };

  const submitPrompt = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (appDialog.kind !== "prompt") return;
    const submit = appDialog.onSubmit;
    const value = dialogInput;
    setAppDialog(null);
    void submit(value);
  };

  return createPortal(
    <div
      className="overlay app-dialog-overlay"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) setAppDialog(null);
      }}
    >
      <div
        ref={dialogRef}
        className="modal app-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="app-dialog-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="modal-head">
          <h2 id="app-dialog-title" className="modal-title">
            {appDialog.title}
          </h2>
          <Button
            type="button"
            className="icon-btn modal-close"
            onClick={() => setAppDialog(null)}
            aria-label={tr("common.close")}
          >
            <IconClose size={16} />
          </Button>
        </header>
        {appDialog.kind === "confirm" ? (
          <form
            className="app-dialog__form"
            onSubmit={(event) => {
              event.preventDefault();
              submitConfirm();
            }}
          >
            <p className="app-dialog__msg">{appDialog.message}</p>
            <div className="app-dialog__actions modal-actions">
              <Button
                type="button"
                className="btn btn--ghost"
                onClick={() => setAppDialog(null)}
              >
                {tr("common.cancel")}
              </Button>
              <Button
                ref={confirmBtnRef}
                type="submit"
                className={`btn ${appDialog.danger ? "btn--danger" : "btn--solid"}`}
              >
                {appDialog.confirmLabel || tr("common.confirm")}
              </Button>
            </div>
          </form>
        ) : (
          <form className="app-dialog__form" onSubmit={submitPrompt}>
            {appDialog.message ? (
              <p className="app-dialog__msg">{appDialog.message}</p>
            ) : null}
            <Input
              ref={dialogInputRef}
              className="app-dialog__input"
              aria-label={appDialog.title}
              value={dialogInput}
              placeholder={appDialog.placeholder}
              onChange={(event) => setDialogInput(event.target.value)}
              autoComplete="off"
            />
            <div className="app-dialog__actions modal-actions">
              <Button
                type="button"
                className="btn btn--ghost"
                onClick={() => setAppDialog(null)}
              >
                {tr("common.cancel")}
              </Button>
              <Button type="submit" className="btn btn--solid">
                {appDialog.submitLabel || tr("common.save")}
              </Button>
            </div>
          </form>
        )}
      </div>
    </div>,
    document.body,
  );
}
