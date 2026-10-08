import { useTranslation } from "@/modules/i18n";
import { PopoverContent } from "@/components/ui/popover";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { HugeiconsIcon } from "@hugeicons/react";
import type { SlashCommandMeta } from "@/modules/ai/lib/slashCommands";

export function CommandPickerContent({
  items,
  activeIndex,
  onPick,
  onHover,
  loading,
  error,
  onRetry,
}: {
  items: readonly SlashCommandMeta[];
  activeIndex: number;
  onPick: (item: SlashCommandMeta) => void;
  onHover: (index: number) => void;
  loading: boolean;
  error: string;
  onRetry: () => void;
}) {
  const tr = useTranslation();
  return (
    <PopoverContent
      side="top"
      align="start"
      sideOffset={6}
      onOpenAutoFocus={(event) => event.preventDefault()}
      onCloseAutoFocus={(event) => event.preventDefault()}
      onMouseDown={(event) => event.preventDefault()}
      className="w-80 overflow-hidden rounded-lg border border-border/60 bg-popover p-0 shadow-xl"
    >
      <div className="max-h-64 overflow-y-auto py-1">
        {items.map((command, index) => (
          <button
            key={command.name}
            type="button"
            onMouseEnter={() => onHover(index)}
            onClick={() => onPick(command)}
            className={cn(
              "flex w-full items-center gap-2 px-3 py-2 text-left text-ui-sm",
              index === activeIndex ? "bg-accent" : "hover:bg-accent/60",
            )}
          >
            <HugeiconsIcon
              icon={command.icon}
              size={14}
              className="shrink-0 text-muted-foreground"
            />
            <span className="min-w-0 flex-1">
              <span className="block truncate font-mono">
                {command.invocation}
              </span>
              <span className="block truncate text-ui-xs text-muted-foreground">
                {command.source ? command.label : tr(command.label)}
              </span>
            </span>
          </button>
        ))}
        {loading && (
          <p
            role="status"
            className="px-3 py-2 text-ui-sm text-muted-foreground"
          >
            {tr("Loading…")}
          </p>
        )}
        {!loading && !items.length && (
          <p className="px-3 py-2 text-ui-sm text-muted-foreground">
            {tr("No commands match your search")}
          </p>
        )}
        {error && (
          <div className="px-3 py-2">
            <p role="alert" className="text-ui-xs text-destructive">
              {tr("Could not load extensions.")}
            </p>
            <Button size="xs" variant="ghost" onClick={onRetry}>
              {tr("Retry")}
            </Button>
          </div>
        )}
      </div>
    </PopoverContent>
  );
}
