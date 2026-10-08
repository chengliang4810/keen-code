import { useTranslation } from "@/modules/i18n";
import { PopoverContent } from "@/components/ui/popover";
import { Spinner } from "@/components/localized/spinner";
import { cn } from "@/lib/utils";
import { fileIconUrl } from "@/modules/explorer/lib/iconResolver";
import { useEffect, useRef } from "react";

type Props = {
  files: readonly string[];
  activeIndex: number;
  indexing: boolean;
  truncated: boolean;
  hasWorkspace: boolean;
  onPick: (file: string) => void;
  onHover: (index: number) => void;
};

export function FilePickerContent({
  files,
  activeIndex,
  indexing,
  truncated,
  hasWorkspace,
  onPick,
  onHover,
}: Props) {
  const tr = useTranslation();
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const listRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const el = itemRefs.current[activeIndex];
    if (!el) return;
    el.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  return (
    <PopoverContent
      side="top"
      align="start"
      sideOffset={6}
      onOpenAutoFocus={(e) => e.preventDefault()}
      onCloseAutoFocus={(e) => e.preventDefault()}
      onMouseDown={(e) => e.preventDefault()}
      className="w-80 overflow-hidden rounded-lg border border-border/60 bg-popover/95 p-0 shadow-xl backdrop-blur-xl"
    >
      <div className="border-b border-border/60 px-2.5 py-1.5 text-ui-base font-medium uppercase tracking-wide text-muted-foreground/70">
        {tr("Workspace files")}
      </div>
      {!hasWorkspace ? (
        <div className="px-3 py-3 text-ui-sm text-muted-foreground">
          {tr("No workspace open")}
        </div>
      ) : indexing && files.length === 0 ? (
        <div className="flex items-center gap-2 px-3 py-3 text-ui-sm text-muted-foreground">
          <Spinner className="size-3" />
          <span>{tr("Indexing workspace…")}</span>
        </div>
      ) : files.length === 0 ? (
        <div className="px-3 py-3 text-ui-sm text-muted-foreground">
          {tr("No matching files")}
        </div>
      ) : (
        <>
          <div ref={listRef} className="max-h-64 overflow-y-auto py-1">
            {files.map((path, idx) => {
              const slash = path.lastIndexOf("/");
              const name = slash === -1 ? path : path.slice(slash + 1);
              const dir = slash === -1 ? "" : path.slice(0, slash);
              return (
                <button
                  key={path}
                  ref={(el) => {
                    itemRefs.current[idx] = el;
                  }}
                  type="button"
                  onClick={() => onPick(path)}
                  onMouseEnter={() => onHover(idx)}
                  className={cn(
                    "flex w-full items-center gap-2 px-2 py-1.5 text-left text-ui-base",
                    idx === activeIndex ? "bg-accent" : "hover:bg-accent/60",
                  )}
                >
                  <img
                    src={fileIconUrl(name)}
                    alt=""
                    className="size-4 shrink-0"
                  />
                  <span className="flex min-w-0 flex-1 items-baseline gap-1.5">
                    <span className="truncate font-medium">{name}</span>
                    {dir && (
                      <span className="truncate text-ui-caption text-muted-foreground">
                        {dir}
                      </span>
                    )}
                  </span>
                </button>
              );
            })}
          </div>
          {truncated && (
            <div className="border-t border-border/60 px-2.5 py-1.5 text-ui-sm text-muted-foreground">
              {tr("Workspace is large - refine your query to narrow results.")}
            </div>
          )}
        </>
      )}
    </PopoverContent>
  );
}
