import { useTranslation } from "@/modules/i18n";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/localized/spinner";
import { cn } from "@/lib/utils";
import type { ProviderInfo } from "@/modules/ai/config";
import {
  ArrowUpRight01Icon,
  Cancel01Icon,
  CheckmarkCircle02Icon,
  Edit02Icon,
  ViewIcon,
  ViewOffSlashIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useEffect, useState } from "react";
import { ProviderIcon } from "./ProviderIcon";

type Props = {
  provider: ProviderInfo;
  currentKey: string | null;
  onSave: (key: string) => Promise<void>;
  onClear: () => Promise<void>;
  onRemove?: () => void;
  embedded?: boolean;
  fieldsOnly?: boolean;
};

function maskKey(key: string): string {
  if (key.length <= 8) return "•".repeat(key.length);
  return `${key.slice(0, 4)}${"•".repeat(8)}${key.slice(-4)}`;
}

export function ProviderKeyCard({
  provider,
  currentKey,
  onSave,
  onClear,
  onRemove,
  embedded = false,
  fieldsOnly = false,
}: Props) {
  const tr = useTranslation();
  const [editing, setEditing] = useState(!currentKey);
  const [value, setValue] = useState("");
  const [reveal, setReveal] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setEditing(!currentKey);
    setReveal(false);
  }, [currentKey]);

  const submit = async () => {
    const trimmed = value.trim();
    if (!trimmed) {
      setError("Enter your API key.");
      return;
    }
    if (provider.keyPrefix && !trimmed.startsWith(provider.keyPrefix)) {
      setError(`${provider.label} keys start with "${provider.keyPrefix}".`);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onSave(trimmed);
      setValue("");
      setReveal(false);
    } catch (e) {
      setError(`Failed to save: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className={cn(
        "flex min-w-0 flex-col gap-4",
        !embedded &&
          "rounded-lg border border-border/60 bg-card/60 px-3 py-2.5",
      )}
    >
      {!fieldsOnly && (
        <div className="flex items-center gap-2">
          <ProviderIcon provider={provider.id} size={15} />
          <span className="text-ui-base font-medium">{provider.label}</span>
          {currentKey ? (
            <Badge
              variant="outline"
              className="ml-1 h-4 gap-1 border-border/60 bg-muted/40 px-1.5 text-ui-xs font-normal text-muted-foreground"
            >
              <HugeiconsIcon
                icon={CheckmarkCircle02Icon}
                size={9}
                strokeWidth={2}
              />
              {tr("Connected")}
            </Badge>
          ) : null}
          <button
            type="button"
            onClick={() => void openUrl(provider.consoleUrl)}
            className="ml-auto inline-flex items-center gap-0.5 text-ui-base text-muted-foreground transition-colors hover:text-foreground"
          >
            {tr("Get key")}
            <HugeiconsIcon
              icon={ArrowUpRight01Icon}
              size={11}
              strokeWidth={1.75}
            />
          </button>
          {onRemove ? (
            <Button
              size="icon"
              variant="ghost"
              onClick={onRemove}
              title={tr("Remove provider")}
              className="size-7 text-muted-foreground hover:text-destructive"
            >
              <HugeiconsIcon icon={Cancel01Icon} size={12} strokeWidth={1.75} />
            </Button>
          ) : null}
        </div>
      )}

      {editing ? (
        <div className="flex flex-col gap-1.5">
          <div className="flex gap-1.5">
            <div className="relative min-w-0 flex-1">
              <Input
                aria-label={tr("API key")}
                type={reveal ? "text" : "password"}
                autoComplete="off"
                spellCheck={false}
                placeholder={
                  provider.keyPrefix
                    ? `${provider.keyPrefix}…`
                    : tr("Paste API key")
                }
                value={value}
                disabled={saving}
                onChange={(e) => {
                  setValue(e.target.value);
                  if (error) setError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void submit();
                  } else if (e.key === "Escape" && currentKey) {
                    setValue("");
                    setReveal(false);
                    setError(null);
                    setEditing(false);
                  }
                }}
                className="h-8 pr-7 font-mono text-ui-base"
              />
              <button
                type="button"
                onClick={() => setReveal((v) => !v)}
                className="absolute top-1/2 right-2 -translate-y-1/2 text-muted-foreground/60 hover:text-foreground"
                aria-label={reveal ? tr("Hide key") : tr("Show key")}
              >
                <HugeiconsIcon
                  icon={reveal ? ViewOffSlashIcon : ViewIcon}
                  size={12}
                  strokeWidth={1.75}
                />
              </button>
            </div>
            <Button
              size="sm"
              onClick={() => void submit()}
              disabled={saving || !value.trim()}
              className="h-8 gap-1 px-3 text-ui-base"
            >
              {saving ? <Spinner className="size-3" /> : null}
              {tr("Save")}
            </Button>
          </div>
          {error ? (
            <p className="text-ui-sm text-destructive">{error}</p>
          ) : null}
        </div>
      ) : (
        <div className="flex items-center gap-1.5">
          <code
            className={cn(
              "min-w-0 flex-1 truncate rounded bg-muted/40 px-2 py-1 font-mono text-ui-base text-muted-foreground",
              embedded &&
                "rounded-md border border-border/60 bg-transparent px-3 py-1.5",
            )}
          >
            {fieldsOnly && reveal ? currentKey : maskKey(currentKey ?? "")}
          </code>
          {fieldsOnly && (
            <Button
              size="icon"
              variant="ghost"
              onClick={() => setReveal((v) => !v)}
              aria-label={reveal ? tr("Hide key") : tr("Show key")}
              className="size-7 shrink-0 text-muted-foreground"
            >
              <HugeiconsIcon
                icon={reveal ? ViewOffSlashIcon : ViewIcon}
                size={12}
                strokeWidth={1.75}
              />
            </Button>
          )}
          <Button
            size="icon"
            variant="ghost"
            onClick={() => setEditing(true)}
            title={tr("Replace")}
            aria-label={tr("Replace")}
            className="size-7"
          >
            <HugeiconsIcon icon={Edit02Icon} size={12} strokeWidth={1.75} />
          </Button>
          {!onRemove ? (
            <Button
              size="icon"
              variant="ghost"
              onClick={() => void onClear()}
              title={tr("Remove")}
              aria-label={tr("Remove")}
              className="size-7 text-muted-foreground hover:text-destructive"
            >
              <HugeiconsIcon icon={Cancel01Icon} size={12} strokeWidth={1.75} />
            </Button>
          ) : null}
        </div>
      )}
    </div>
  );
}
