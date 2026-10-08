import { cn } from "@/lib/utils";
import { useTranslation } from "@/modules/i18n";
import type { ConversationDiff } from "@/modules/ai/lib/conversationToolPresentation";

export function ConversationToolDiffPreview({
  preview,
}: {
  preview: ConversationDiff;
}) {
  const tr = useTranslation();
  return (
    <div
      className="mb-2 max-h-60 overflow-auto rounded-xl border border-border bg-card"
      data-conversation-diff
    >
      <div className="min-w-full font-mono text-ui-sm leading-relaxed text-foreground">
        {preview.lines.map((line, index) => (
          <div
            key={line.id}
            className={cn(
              "flex min-w-full",
              line.kind === "added" ? "bg-green-500/10" : "bg-destructive/10",
            )}
          >
            <span
              aria-hidden
              className={cn(
                "sticky left-0 w-12 shrink-0 select-none border-r border-border px-2 text-right tabular-nums",
                line.kind === "added"
                  ? "bg-green-500/20 text-green-600 dark:text-green-400 shadow-[inset_3px_0_0_var(--color-green-500)]"
                  : "bg-destructive/20 text-destructive shadow-[inset_3px_0_0_var(--destructive)]",
              )}
            >
              {index + 1}
            </span>
            <code className="min-w-0 flex-1 whitespace-pre-wrap break-words px-3">
              {line.text || " "}
            </code>
          </div>
        ))}
        {preview.truncated && (
          <p className="px-3 py-1 text-muted-foreground">
            {tr("Preview truncated")}
          </p>
        )}
      </div>
    </div>
  );
}
