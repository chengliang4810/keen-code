import type { Dispatch, SetStateAction } from "react";
import * as api from "@/lib/api";
import { GlassModal } from "@/components/GlassModal";
import { Button } from "@appica/ui-react/button";
import { Checkbox } from "@appica/ui-react/checkbox";
import { Alert, AlertDescription } from "@appica/ui-react/alert";
import type { SetState, Translator } from "./types";

export interface WorktreeGcModalProps {
  tr: Translator;
  open: boolean;
  setOpen: SetState<boolean>;
  busy: boolean;
  previewBusy: boolean;
  force: boolean;
  setForce: SetState<boolean>;
  error: string | null;
  setError: SetState<string | null>;
  preview: api.GitWorktreeGcResult | null;
  setPreview: Dispatch<SetStateAction<api.GitWorktreeGcResult | null>>;
  submit: () => void | Promise<void>;
}

export function WorktreeGcModal({
  tr,
  open,
  setOpen,
  busy,
  previewBusy,
  force,
  setForce,
  error,
  setError,
  preview,
  setPreview,
  submit,
}: WorktreeGcModalProps) {
  const reset = () => {
    setOpen(false);
    setError(null);
    setPreview(null);
    setForce(false);
  };

  return (
    <GlassModal
      open={open}
      onClose={() => {
        if (busy) return;
        reset();
      }}
      title={tr("composer.worktreeGcTitle")}
      size="md"
      closeLabel={tr("common.close")}
      closeOnOverlay={!busy}
      showClose={!busy}
      wrapBody
      footer={
        <>
          <Button size="md"
            type="button"
            variant="ghost"
            disabled={busy}
            onClick={reset}
          >
            {tr("common.cancel")}
          </Button>
          <Button size="md"
            type="button"
            variant="primary"
            disabled={busy || previewBusy}
            onClick={() => void submit()}
          >
            {busy
              ? tr("composer.worktreeGcRunning")
              : tr("composer.worktreeGcConfirm")}
          </Button>
        </>
      }
    >
      <div className="wt-gc">
        <p className="wt-gc__hint">{tr("composer.worktreeGcHint")}</p>
        <div className="wt-gc__force">
          <Checkbox
            id="worktree-gc-force"
            checked={force}
            disabled={busy || previewBusy}
            onCheckedChange={(checked) => setForce(checked === true)}
            aria-labelledby="worktree-gc-force-label"
          />
          <label htmlFor="worktree-gc-force" id="worktree-gc-force-label">
            {tr("composer.worktreeGcForce")}
          </label>
        </div>
        <div className="wt-gc__preview-head">
          {tr("composer.worktreeGcPreview")}
        </div>
        {previewBusy ? (
          <p className="wt-gc__preview-status">
            {tr("composer.worktreeGcPreviewLoading")}
          </p>
        ) : preview ? (
          <>
            {preview.prunable.length > 0 ? (
              <p className="wt-gc__prunable">
                {tr("composer.worktreeGcPrunable", {
                  n: String(preview.prunable.length),
                })}
              </p>
            ) : null}
            {preview.output.trim() || preview.prunable.length > 0 ? (
              <pre className="wt-gc__output" tabIndex={0}>
                {preview.output.trim() || preview.prunable.join("\n")}
              </pre>
            ) : (
              <p className="wt-gc__preview-status">
                {tr("composer.worktreeGcPreviewEmpty")}
              </p>
            )}
          </>
        ) : error ? null : (
          <p className="wt-gc__preview-status">
            {tr("composer.worktreeGcPreviewEmpty")}
          </p>
        )}
        {error ? (
          <Alert variant="error"><AlertDescription>{error}</AlertDescription></Alert>
        ) : null}
      </div>
    </GlassModal>
  );
}
