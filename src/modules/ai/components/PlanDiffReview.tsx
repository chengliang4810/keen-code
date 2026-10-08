import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import {
  ArrowDown01Icon,
  Cancel01Icon,
  FileEditIcon,
  FilePlusIcon,
  FolderAddIcon,
  Tick02Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useState } from "react";
import { usePlanStore, type QueuedEdit } from "../store/planStore";

function basename(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i >= 0 ? p.slice(i + 1) : p;
}

function diffStats(
  original: string,
  proposed: string,
): { added: number; removed: number } {
  const a = original.split("\n");
  const b = proposed.split("\n");
  const setA = new Set(a);
  const setB = new Set(b);
  let added = 0;
  let removed = 0;
  for (const line of b) if (!setA.has(line)) added++;
  for (const line of a) if (!setB.has(line)) removed++;
  return { added, removed };
}

export function PlanDiffReview() {
  const tr = useTranslation();
  const queue = usePlanStore((s) => s.queue);
  const removeOne = usePlanStore((s) => s.removeOne);
  const clear = usePlanStore((s) => s.clear);
  const applyAll = usePlanStore((s) => s.applyAll);
  const [busy, setBusy] = useState(false);

  if (queue.length === 0) return null;

  const onApply = async () => {
    setBusy(true);
    try {
      const results = await applyAll();
      const failed = results.filter((r) => !r.ok);
      if (failed.length) {
        console.error("plan apply failures:", failed);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="absolute inset-0 z-10 flex flex-col bg-background/85 backdrop-blur-xl">
      <div className="flex items-center justify-between border-b border-border/40 px-3 py-2">
        <div className="flex flex-col">
          <span className="text-ui-base font-semibold tracking-tight">
            {tr("Plan review")}
          </span>
          <span className="text-ui-sm text-muted-foreground">
            {queue.length}{" "}
            {tr(queue.length === 1 ? "pending change" : "pending changes")}
          </span>
        </div>
        <div className="flex items-center gap-1.5">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-7 gap-1.5 text-ui-base hover:bg-destructive/10 hover:text-destructive"
            onClick={() => clear()}
            disabled={busy}
          >
            <HugeiconsIcon icon={Cancel01Icon} size={12} strokeWidth={2} />
            {tr("Discard all")}
          </Button>
          <Button
            type="button"
            size="sm"
            className="h-7 gap-1.5 text-ui-base"
            onClick={onApply}
            disabled={busy}
          >
            <HugeiconsIcon icon={Tick02Icon} size={12} strokeWidth={2} />
            {tr("Apply")} {queue.length}
          </Button>
        </div>
      </div>
      <ul className="flex flex-1 flex-col gap-1.5 overflow-auto p-3">
        {queue.map((q) => (
          <PlanRow key={q.id} item={q} onReject={() => removeOne(q.id)} />
        ))}
      </ul>
    </div>
  );
}

function PlanRow({
  item,
  onReject,
}: {
  item: QueuedEdit;
  onReject: () => void;
}) {
  const tr = useTranslation();
  const [open, setOpen] = useState(false);
  const isDir = item.kind === "create_directory";
  const isNew = item.isNewFile && !isDir;
  const stats = isDir
    ? null
    : diffStats(item.originalContent, item.proposedContent);
  const Icon = isDir ? FolderAddIcon : isNew ? FilePlusIcon : FileEditIcon;

  return (
    <li className="group/row overflow-hidden rounded-md border border-border/50 bg-card">
      <div className="flex items-start gap-2 px-2.5 py-1.5">
        <button
          type="button"
          onClick={() => !isDir && setOpen((v) => !v)}
          disabled={isDir}
          className={cn(
            "mt-0.5 shrink-0 text-muted-foreground transition-transform",
            open && "rotate-180",
            isDir && "invisible",
          )}
          aria-label={tr("Toggle diff")}
        >
          <HugeiconsIcon icon={ArrowDown01Icon} size={11} strokeWidth={1.75} />
        </button>
        <HugeiconsIcon
          icon={Icon}
          size={13}
          strokeWidth={1.75}
          className="mt-0.5 shrink-0 text-muted-foreground"
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-1.5 font-mono text-ui-base">
            <span className="truncate text-foreground">
              {basename(item.path)}
            </span>
            {isNew ? (
              <span className="text-ui-xs text-emerald-600 dark:text-emerald-400">
                {tr("new")}
              </span>
            ) : null}
          </div>
          <div className="truncate font-mono text-ui-xs text-muted-foreground">
            {item.path}
          </div>
          {stats ? (
            <div className="mt-0.5 flex items-center gap-2 text-ui-xs tabular-nums">
              <span className="text-emerald-600 dark:text-emerald-400">
                +{stats.added}
              </span>
              <span className="text-destructive">−{stats.removed}</span>
              <span className="text-muted-foreground">
                {item.kind === "multi_edit" ? tr("multi-edit") : item.kind}
              </span>
            </div>
          ) : (
            <div className="mt-0.5 text-ui-sm text-muted-foreground">
              {item.description ?? tr("create directory")}
            </div>
          )}
        </div>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-5 shrink-0 opacity-0 transition-opacity group-hover/row:opacity-100"
          onClick={onReject}
          aria-label={tr("Reject")}
        >
          <HugeiconsIcon icon={Cancel01Icon} size={11} strokeWidth={1.75} />
        </Button>
      </div>
      {open && !isDir ? (
        <div className="border-t border-border/40 bg-muted/20 px-2.5 py-2">
          <UnifiedDiffPreview
            original={item.originalContent}
            proposed={item.proposedContent}
          />
        </div>
      ) : null}
    </li>
  );
}

function UnifiedDiffPreview({
  original,
  proposed,
}: {
  original: string;
  proposed: string;
}) {
  const tr = useTranslation();
  // Coarse line-level diff (LCS-lite via set membership). For real diffs
  // we'd reach for a library; this is good enough for at-a-glance review.
  const a = original.split("\n");
  const b = proposed.split("\n");
  const setA = new Set(a);
  const setB = new Set(b);

  const lines: Array<{ kind: "add" | "del" | "ctx"; text: string }> = [];
  // First pass: removed (in a, not in b).
  for (const l of a) if (!setB.has(l)) lines.push({ kind: "del", text: l });
  // Then: added (in b, not in a).
  for (const l of b) if (!setA.has(l)) lines.push({ kind: "add", text: l });

  if (lines.length === 0) {
    return (
      <div className="text-ui-sm italic text-muted-foreground">
        {tr("no line-level changes")}
      </div>
    );
  }

  const MAX = 80;
  const shown = lines.slice(0, MAX);
  const rest = lines.length - shown.length;

  return (
    <div className="overflow-hidden rounded border border-border/40 font-mono text-ui-11 leading-relaxed">
      <div className="max-h-72 overflow-auto">
        {shown.map((l, i) => (
          <div
            key={i}
            className={cn(
              "flex whitespace-pre",
              l.kind === "add"
                ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
                : "bg-destructive/10 text-destructive",
            )}
          >
            <span className="w-4 shrink-0 select-none px-1 text-center opacity-70">
              {l.kind === "add" ? "+" : "-"}
            </span>
            <span className="min-w-0 flex-1 overflow-x-auto pr-2">
              {l.text || " "}
            </span>
          </div>
        ))}
        {rest > 0 ? (
          <div className="px-2 py-1 text-ui-xs italic text-muted-foreground">
            … {rest} {tr("more changes")}
          </div>
        ) : null}
      </div>
    </div>
  );
}
