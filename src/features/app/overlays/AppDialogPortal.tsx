import type { FormEvent, RefObject } from "react";
import type { AppDialog } from "@/features/app/models";
import { Button } from "@/components/ui/button";
import { Input } from "@appica/ui-react/input";
import { GlassModal } from "@/components/GlassModal";
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
  if (!appDialog) return null;

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

  return (
    <GlassModal
      open
      size="md"
      className="app-dialog"
      overlayClassName="app-dialog-overlay"
      title={appDialog.title}
      closeLabel={tr("common.close")}
      onClose={() => setAppDialog(null)}
    >
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
              variant="ghost"
              onClick={() => setAppDialog(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              ref={confirmBtnRef}
              data-modal-autofocus
              type="submit"
              variant={appDialog.danger ? "destructive" : "primary"}
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
            data-modal-autofocus
            className="app-dialog__input"
            value={dialogInput}
            placeholder={appDialog.placeholder}
            onChange={(event) => setDialogInput(event.target.value)}
            autoComplete="off"
          />
          <div className="app-dialog__actions modal-actions">
            <Button
              type="button"
              variant="ghost"
              onClick={() => setAppDialog(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button type="submit" variant="primary">
              {appDialog.submitLabel || tr("common.save")}
            </Button>
          </div>
        </form>
      )}
    </GlassModal>
  );
}
