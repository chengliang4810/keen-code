import { useId, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { useTranslation } from "@/modules/i18n";
import {
  isValidCommandName,
  type CommandConfig,
  type CommandFile,
} from "@/modules/ai/lib/commands";

export function CommandForm({
  initial,
  busy,
  onSave,
  onCancel,
  onDelete,
}: {
  initial: CommandFile | null;
  busy: boolean;
  onSave: (config: CommandConfig) => Promise<void>;
  onCancel: () => void;
  onDelete: (() => void) | undefined;
}) {
  const tr = useTranslation();
  const id = useId();
  const [name, setName] = useState(initial?.name ?? "");
  const [description, setDescription] = useState(initial?.description ?? "");
  const [argumentHint, setArgumentHint] = useState(initial?.argumentHint ?? "");
  const [prompt, setPrompt] = useState(initial?.prompt ?? "");
  const canSave = isValidCommandName(name.trim()) && prompt.trim().length > 0;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!canSave || busy) return;
    void onSave({
      name: name.trim(),
      description,
      argumentHint,
      prompt,
      enabled: initial?.enabled ?? true,
    });
  };
  return (
    <form onSubmit={submit} className="space-y-4">
      <fieldset disabled={busy} className="space-y-4">
        <div className="flex flex-wrap items-end gap-4">
          <div className="min-w-0 flex-1 space-y-1.5">
            <label htmlFor={`${id}-name`} className="text-ui-sm font-medium">
              {tr("Name")}
            </label>
            <Input
              id={`${id}-name`}
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="my-command"
              maxLength={50}
              disabled={!!initial}
              aria-invalid={!!name && !isValidCommandName(name.trim())}
              className="h-8 font-mono"
              autoFocus
            />
            {!!name && !isValidCommandName(name.trim()) && (
              <p role="alert" className="text-ui-xs text-destructive">
                {tr("Invalid command name.")}
              </p>
            )}
          </div>
        </div>
        <div className="space-y-1.5">
          <label
            htmlFor={`${id}-description`}
            className="text-ui-sm font-medium"
          >
            {tr("Description (optional)")}
          </label>
          <Input
            id={`${id}-description`}
            value={description}
            onChange={(event) => setDescription(event.target.value)}
            maxLength={4096}
            className="h-8"
          />
        </div>
        <div className="space-y-1.5">
          <label htmlFor={`${id}-hint`} className="text-ui-sm font-medium">
            {tr("Argument hint (optional)")}
          </label>
          <Input
            id={`${id}-hint`}
            value={argumentHint}
            onChange={(event) => setArgumentHint(event.target.value)}
            placeholder="<file-path>"
            maxLength={256}
            className="h-8 font-mono"
          />
        </div>
        <div className="space-y-1.5">
          <label htmlFor={`${id}-prompt`} className="text-ui-sm font-medium">
            {tr("Prompt")}
          </label>
          <Textarea
            id={`${id}-prompt`}
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            maxLength={65536}
            className="min-h-64 resize-y font-mono text-ui-sm"
          />
        </div>
      </fieldset>
      <div className="flex items-center gap-2">
        {onDelete && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="mr-auto text-destructive hover:text-destructive"
            disabled={busy}
            onClick={onDelete}
          >
            {tr("Delete")}
          </Button>
        )}
        <Button type="submit" size="sm" disabled={!canSave || busy}>
          {tr(busy ? "Saving…" : "Save")}
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={busy}
          onClick={onCancel}
        >
          {tr("Cancel")}
        </Button>
      </div>
    </form>
  );
}
