import { GlassModal } from "@/components/GlassModal";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Alert, AlertDescription } from "@appica/ui-react/alert";
import { Field, FieldLabel } from "@appica/ui-react/field";
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
      size="md"
      closeLabel={tr("common.close")}
      closeOnOverlay={!busy}
      showClose={!busy}
      wrapBody
      footer={
        <>
          <Button
            type="button"
            variant="ghost"
            disabled={busy}
            onClick={() => setOpen(false)}
          >
            {tr("common.cancel")}
          </Button>
          <Button
            type="button"
            variant="primary"
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
        <Field>
          <FieldLabel>
            {tr("composer.worktreeName")}
          </FieldLabel>
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
        </Field>
        <Field>
          <FieldLabel>
            {tr("composer.worktreeRef")}
          </FieldLabel>
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
        </Field>
        {previewPath ? (
          <p className="wt-create__preview">
            {tr("composer.worktreePathPreview", { path: previewPath })}
          </p>
        ) : null}
        {error ? (
          <Alert variant="error"><AlertDescription>{error}</AlertDescription></Alert>
        ) : null}
      </form>
    </GlassModal>
  );
}
