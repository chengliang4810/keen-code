import { ReasoningLevelEditor } from "@/settings/components/ReasoningLevelEditor";
import { Textarea } from "@/components/ui/textarea";
import { useId, useRef, useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/localized/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowRight01Icon, HelpCircleIcon } from "@hugeicons/core-free-icons";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/modules/i18n";
import { useModelCatalogStore } from "@/modules/ai/lib/modelCatalogState";
import {
  effectiveCustomModel,
  recommendedModelConfig,
  type CustomModel,
} from "@/modules/ai/config";
import {
  createCustomModelDraft,
  commitCustomModelDraft,
  restoreCustomModelDraft,
  toggleCustomModelSmart,
} from "@/modules/ai/lib/customModelDraft";

function ModelFieldHelp({ label, text }: { label: string; text: string }) {
  const tr = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          className="size-6 shrink-0 text-muted-foreground"
          aria-label={tr("Help for {value0}", { value0: label })}
          onPointerEnter={(event) => {
            if (event.pointerType === "mouse") setOpen(true);
          }}
          onPointerLeave={(event) => {
            if (event.pointerType === "mouse") setOpen(false);
          }}
          onFocus={() => setOpen(true)}
          onClick={(event) => {
            event.preventDefault();
            setOpen(true);
          }}
        >
          <HugeiconsIcon icon={HelpCircleIcon} className="size-3.5" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        role="tooltip"
        align="start"
        collisionPadding={12}
        onOpenAutoFocus={(event) => event.preventDefault()}
        onCloseAutoFocus={(event) => event.preventDefault()}
        className="w-80 max-w-[calc(100vw-2rem)] text-ui-sm leading-relaxed"
      >
        {text}
      </PopoverContent>
    </Popover>
  );
}

export function CustomModelDialog({
  model,
  models,
  baseURL,
  onSave,
  onClose,
}: {
  model?: CustomModel;
  models: readonly CustomModel[];
  baseURL?: string;
  onSave: (model: CustomModel) => Promise<void>;
  onClose: () => void;
}) {
  const tr = useTranslation();
  useModelCatalogStore((s) => s.revision);
  const id = useId();
  const [draft, setDraft] = useState(() => createCustomModelDraft(model));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const composing = useRef(false);
  const modelIdInput = useRef<HTMLInputElement>(null);
  const recommended = recommendedModelConfig(draft.id, baseURL);
  const inherited = effectiveCustomModel(
    {
      id: draft.id,
      contextLimit: /^\d+$/.test(draft.context.trim())
        ? Number(draft.context)
        : undefined,
    },
    baseURL,
  );
  const save = async () => {
    if (saving) return;
    setError("");
    try {
      const next = commitCustomModelDraft(
        draft,
        model?.enabled !== false,
        baseURL,
      );
      if (
        models.some((entry) => entry.id === next.id && entry.id !== model?.id)
      )
        throw new Error(tr("This model ID already exists in the provider."));
      setSaving(true);
      await onSave(next);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? tr(e.message) : String(e));
    } finally {
      setSaving(false);
    }
  };
  const numericField = (
    field: "context" | "output",
    label: string,
    fallback: number,
    help: string,
  ) => (
    <div className="space-y-1">
      <div className="flex items-center text-ui-base text-muted-foreground">
        <label htmlFor={`${id}-${field}`}>{tr(label)}</label>
        <ModelFieldHelp label={tr(label)} text={tr(help)} />
        {draft.smart && draft[field].trim() && (
          <Badge variant="outline" className="ml-1 text-ui-xs">
            {tr("Override")}
          </Badge>
        )}
      </div>
      <Input
        id={`${id}-${field}`}
        aria-label={tr(label)}
        value={draft[field]}
        onChange={(event) =>
          setDraft({ ...draft, [field]: event.target.value })
        }
        placeholder={draft.id.trim() ? String(fallback) : undefined}
        disabled={saving || (field === "output" && !model && !draft.id.trim())}
        inputMode="numeric"
        pattern="[0-9]*"
        spellCheck={false}
        className={cn(
          "h-9 w-full font-mono",
          draft.smart && draft[field].trim() && "border-primary/45",
        )}
      />
    </div>
  );
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !saving) onClose();
      }}
    >
      <DialogContent
        className="grid max-h-[min(48rem,calc(100vh-2rem))] grid-rows-[auto_minmax(0,1fr)_auto] gap-5 overflow-clip p-4 pb-8 sm:max-w-2xl"
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          modelIdInput.current?.focus();
        }}
      >
        <DialogHeader className="gap-4 pr-10">
          <DialogTitle className="text-ui-base leading-5">
            {tr(model ? "Edit model" : "Add model")}
          </DialogTitle>
          <DialogDescription className="sr-only">
            {tr(
              "Configure the model ID and capabilities used by this provider.",
            )}
          </DialogDescription>
          <div className="flex items-center gap-2">
            <div className="flex items-center text-ui-base">
              <span>{tr("Smart configuration")}</span>
              <ModelFieldHelp
                label={tr("Smart configuration")}
                text={tr(
                  "Inherit recommended settings and keep your explicit overrides.",
                )}
              />
            </div>
            <Switch
              aria-label={tr("Smart configuration")}
              checked={draft.smart}
              disabled={saving}
              onCheckedChange={(smart) => {
                try {
                  setDraft(toggleCustomModelSmart(draft, smart, baseURL));
                  setError("");
                } catch (e) {
                  setError(e instanceof Error ? tr(e.message) : String(e));
                }
              }}
            />
          </div>
        </DialogHeader>
        <div className="min-h-0 space-y-3 overflow-y-auto pr-1" inert={saving}>
          <div className="space-y-1">
            <label htmlFor={`${id}-model`} className="flex flex-col gap-1">
              <span className="text-ui-base text-muted-foreground">
                {tr("Model ID")}
              </span>
              <Input
                ref={modelIdInput}
                id={`${id}-model`}
                autoFocus
                value={draft.id}
                onChange={(event) =>
                  setDraft({ ...draft, id: event.target.value })
                }
                onCompositionStart={() => {
                  composing.current = true;
                }}
                onCompositionEnd={() => {
                  composing.current = false;
                }}
                onKeyDown={(event) => {
                  if (
                    event.key === "Enter" &&
                    !composing.current &&
                    !event.nativeEvent.isComposing &&
                    event.keyCode !== 229
                  ) {
                    event.preventDefault();
                    void save();
                  }
                }}
                placeholder={tr("Model ID")}
                autoComplete="off"
                spellCheck={false}
                className="h-9 font-mono"
              />
            </label>
            {draft.smart && draft.id.trim() && (
              <p className="text-ui-xs text-muted-foreground">
                {tr(
                  recommended.known
                    ? "Model recognized. Recommended settings are loaded."
                    : "Model not recognized. Conservative defaults are shown; verify its limits with the provider.",
                )}
              </p>
            )}
          </div>
          {numericField(
            "context",
            "Context window",
            recommended.contextLimit,
            "The context window includes the conversation, tool results and model output. Enter a whole number from 8,192 to 4,000,000 tokens; leave it blank to use the recommended value.",
          )}
          {numericField(
            "output",
            "Maximum output tokens",
            inherited.maxOutputTokens,
            "Limit the tokens generated in one response. Enter a whole number from 1 to 1,000,000 that does not exceed the context window; leave it blank to use the recommended value.",
          )}
          <Collapsible disabled={saving}>
            <CollapsibleTrigger asChild>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="group gap-2 px-2 text-ui-base"
              >
                <HugeiconsIcon
                  icon={ArrowRight01Icon}
                  className="size-4 transition-transform group-data-[state=open]:rotate-90 motion-reduce:transition-none"
                />
                {tr("Advanced configuration")}
              </Button>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <section className="space-y-3 pt-4">
                <h3 className="text-ui-sm font-medium text-muted-foreground">
                  {tr("Input capabilities")}
                </h3>
                <div className="flex items-center justify-between">
                  <span className="text-ui-base">{tr("Text")}</span>
                  <Badge variant="secondary">{tr("Always enabled")}</Badge>
                </div>
                <div className="flex items-center justify-between gap-3">
                  <span className="text-ui-base">{tr("Image input")}</span>
                  <Switch
                    aria-label={tr("Image input")}
                    checked={draft.vision ?? recommended.vision}
                    onCheckedChange={(vision) => setDraft({ ...draft, vision })}
                  />
                </div>
                <div className="space-y-2 border-t border-border pt-3">
                  <div className="flex items-center text-ui-base text-muted-foreground">
                    <span>{tr("Reasoning levels (low to high)")}</span>
                    <ModelFieldHelp
                      label={tr("Reasoning intensity")}
                      text={tr(
                        "Click to edit, drag to reorder. The last level is selected when switching models. Remove all levels to disable the selector.",
                      )}
                    />
                    {draft.smart && draft.reasoningLevels !== null && (
                      <Badge variant="outline">{tr("Override")}</Badge>
                    )}
                  </div>
                  <ReasoningLevelEditor
                    values={
                      draft.reasoningLevels ?? recommended.reasoningLevels
                    }
                    onChange={(reasoningLevels) =>
                      setDraft({ ...draft, reasoningLevels })
                    }
                  />
                </div>
                <div className="space-y-1">
                  <label
                    htmlFor={`${id}-reasoning-map`}
                    className="text-ui-base text-muted-foreground"
                  >
                    {tr("Reasoning parameter mapping (JSON)")}
                  </label>
                  <Textarea
                    id={`${id}-reasoning-map`}
                    value={draft.reasoningMap}
                    onChange={(event) =>
                      setDraft({ ...draft, reasoningMap: event.target.value })
                    }
                    spellCheck={false}
                    className="min-h-28 font-mono text-ui-sm"
                    placeholder={'{ "high": { "reasoning_effort": "high" } }'}
                  />
                  <p className="text-ui-xs leading-relaxed text-muted-foreground">
                    {tr(
                      "Map each level to its request parameters. Blank uses protocol defaults; an empty object for a level sends no reasoning parameters. Only reasoning fields are allowed.",
                    )}
                  </p>
                </div>
              </section>
            </CollapsibleContent>
          </Collapsible>
        </div>
        <div className="space-y-3">
          {error && (
            <p role="alert" className="text-ui-sm text-destructive">
              {error}
            </p>
          )}
          <DialogFooter className="flex-row items-center">
            <Button
              variant="ghost"
              size="sm"
              className="mr-auto px-0 text-ui-sm text-muted-foreground underline underline-offset-4 hover:bg-transparent"
              disabled={saving}
              onClick={() => {
                setDraft(restoreCustomModelDraft(draft));
                setError("");
              }}
            >
              {tr("Reset form")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={saving}
              onClick={onClose}
            >
              {tr("Cancel")}
            </Button>
            <Button
              size="sm"
              disabled={saving || !draft.id.trim()}
              onClick={() => void save()}
            >
              {tr(saving ? "Saving…" : "Save")}
            </Button>
          </DialogFooter>
        </div>
      </DialogContent>
    </Dialog>
  );
}
