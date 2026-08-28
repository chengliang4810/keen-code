import type { Dispatch, SetStateAction } from "react";
import * as api from "@/lib/api";
import { GlassModal } from "@/components/GlassModal";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
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
      size="sm"
      closeLabel={tr("common.close")}
      closeOnOverlay={!busy}
      showClose={!busy}
      wrapBody
      footer={
        <>
          <Button
            type="button"
            className="btn btn--ghost"
            disabled={busy}
            onClick={reset}
          >
            {tr("common.cancel")}
          </Button>
          <Button
            type="button"
            className="btn btn--solid"
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
          <Label htmlFor="worktree-gc-force" id="worktree-gc-force-label">
            {tr("composer.worktreeGcForce")}
          </Label>
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
          <p className="wt-gc__error" role="alert">
            {error}
          </p>
        ) : null}
      </div>
    </GlassModal>
  );
}
