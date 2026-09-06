import { createPortal } from "react-dom";
import type { FormEvent, RefObject } from "react";
import type { AppDialog } from "@/features/app/models";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IconClose } from "@/components/icons";
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
