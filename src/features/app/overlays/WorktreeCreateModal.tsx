import { GlassModal } from "@/components/GlassModal";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { SetState, Translator } from "./types";

export interface WorktreeCreateModalProps {
  tr: Translator;
  open: boolean;
  setOpen: SetState<boolean>;
  busy: boolean;
  startChat: boolean;
  name: string;
  setName: SetState<string>;
  refName: string;
  setRefName: SetState<string>;
  previewPath: string | null;
  error: string | null;
  setError: SetState<string | null>;
  submit: () => void | Promise<void>;
}

export function WorktreeCreateModal({
  tr,
  open,
  setOpen,
  busy,
  startChat,
  name,
  setName,
  refName,
  setRefName,
  previewPath,
  error,
  setError,
  submit,
}: WorktreeCreateModalProps) {
  return (
    <GlassModal
      open={open}
      onClose={() => {
        if (!busy) setOpen(false);
      }}
      title={
        startChat
          ? tr("composer.worktreeNewChatTitle")
          : tr("composer.worktreeNewTitle")
      }
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
            onClick={() => setOpen(false)}
          >
            {tr("common.cancel")}
          </Button>
          <Button
            type="button"
            className="btn btn--solid"
            disabled={busy || !name.trim()}
            onClick={() => void submit()}
          >
            {busy
              ? tr("composer.worktreeCreating")
              : startChat
                ? tr("composer.worktreeCreateChat")
                : tr("composer.worktreeCreate")}
          </Button>
        </>
      }
    >
      <form
        className="wt-create"
        onSubmit={(event) => {
          event.preventDefault();
          if (!busy) void submit();
        }}
      >
        <p className="wt-create__hint">
          {startChat
            ? tr("composer.worktreeNewChatHint")
            : tr("composer.worktreeNewHint")}
        </p>
        <Label className="wt-create__field">
          <span className="wt-create__label">
            {tr("composer.worktreeName")}
          </span>
          <Input
            className="settings-input"
            value={name}
            onChange={(event) => {
              setName(event.target.value);
              setError(null);
            }}
            placeholder={tr("composer.worktreeNamePlaceholder")}
            autoComplete="off"
            autoFocus
            disabled={busy}
            spellCheck={false}
          />
        </Label>
        <Label className="wt-create__field">
          <span className="wt-create__label">
            {tr("composer.worktreeRef")}
          </span>
          <Input
            className="settings-input"
            value={refName}
            onChange={(event) => {
              setRefName(event.target.value);
              setError(null);
            }}
            placeholder={tr("composer.worktreeRefPlaceholder")}
            autoComplete="off"
            disabled={busy}
            spellCheck={false}
          />
        </Label>
        {previewPath ? (
          <p className="wt-create__preview">
            {tr("composer.worktreePathPreview", { path: previewPath })}
          </p>
        ) : null}
        {error ? (
          <p className="wt-create__error" role="alert">
            {error}
          </p>
        ) : null}
      </form>
    </GlassModal>
  );
}
