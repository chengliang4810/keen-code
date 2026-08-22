import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
/**
 * 当前 Session 的提示词历史选择器。
 * Newest-first list + optional fuzzy filter; Enter/click selects into composer.
 */

import { useEffect, useRef, type CSSProperties, type Ref } from "react";
import {
  promptHistoryListPreview,
  type PromptHistoryEntry,
} from "@/lib/composerPromptHistory";
import { previewStoredAsSlash } from "@/lib/draftDoc";
import { IconClock } from "@/components/icons";

export type PromptHistoryPanelLabels = {
  title: string;
  placeholder: string;
  empty: string;
  emptyFilter: string;
  aria: string;
};

export type PromptHistoryPanelProps = {
  open: boolean;
  entries: PromptHistoryEntry[];
  query: string;
  activeIndex: number;
  /** Focus the filter field on open (`/history`); leave false for empty-↑ browse. */
  focusFilter?: boolean;
  labels: PromptHistoryPanelLabels;
  onQueryChange: (q: string) => void;
  onActiveIndexChange: (i: number) => void;
  onSelect: (entry: PromptHistoryEntry) => void;
  onClose: () => void;
  style?: CSSProperties;
  panelRef?: Ref<HTMLDivElement | null>;
};

export function PromptHistoryPanel({
  open,
  entries,
  query,
  activeIndex,
  focusFilter = false,
  labels,
  onQueryChange,
  onActiveIndexChange,
  onSelect,
  onClose,
  style,
  panelRef,
}: PromptHistoryPanelProps) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const filterRef = useRef<HTMLInputElement | null>(null);

  const setRefs = (node: HTMLDivElement | null) => {
    listRef.current = node;
    if (typeof panelRef === "function") panelRef(node);
    else if (panelRef && "current" in panelRef) {
      (panelRef as { current: HTMLDivElement | null }).current = node;
    }
  };

  useEffect(() => {
    if (!open || !focusFilter) return;
    const t = window.setTimeout(() => {
      filterRef.current?.focus();
      filterRef.current?.select();
    }, 0);
    return () => window.clearTimeout(t);
  }, [open, focusFilter]);

  useEffect(() => {
    if (!open) return;
    const el = listRef.current?.querySelector<HTMLElement>(
      `[data-ph-idx="${activeIndex}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, open, entries.length]);

  if (!open) return null;

  const emptyText = query.trim() ? labels.emptyFilter : labels.empty;

  return (
    <div
      className="menu-panel prompt-history"
      role="listbox"
      aria-label={labels.aria}
      style={style}
      ref={setRefs}
      data-testid="prompt-history-panel"
    >
      <div className="prompt-history__head">
        <span className="prompt-history__title">{labels.title}</span>
      </div>
      <div className="prompt-history__filter">
        <span className="prompt-history__filter-ico" aria-hidden>
          <IconClock size={14} />
        </span>
        <Input
          ref={filterRef}
          type="search"
          className="prompt-history__input"
          value={query}
          placeholder={labels.placeholder}
          autoComplete="off"
          autoCorrect="off"
          spellCheck={false}
          aria-label={labels.placeholder}
          onChange={(e) => onQueryChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              onClose();
              return;
            }
            if (e.key === "ArrowDown") {
              e.preventDefault();
              if (entries.length === 0) return;
              onActiveIndexChange(
                Math.min(activeIndex + 1, entries.length - 1),
              );
              return;
            }
            if (e.key === "ArrowUp") {
              e.preventDefault();
              if (entries.length === 0) return;
              onActiveIndexChange(Math.max(activeIndex - 1, 0));
              return;
            }
            if (e.key === "Enter" || e.key === "Tab") {
              e.preventDefault();
              const entry = entries[activeIndex];
              if (entry) onSelect(entry);
            }
          }}
        />
      </div>
      <div className="prompt-history__list">
        {entries.length === 0 ? (
          <div className="prompt-history__empty">{emptyText}</div>
        ) : (
          entries.map((entry, i) => {
            const active = i === activeIndex;
            const preview = promptHistoryListPreview(
              previewStoredAsSlash(entry.text),
            );
            return (
              <Button
                key={`${entry.historyIndex}:${i}`}
                type="button"
                role="option"
                aria-selected={active}
                data-ph-idx={i}
                className={
                  "prompt-history__item" + (active ? " is-active" : "")
                }
                title={previewStoredAsSlash(entry.text)}
                onMouseEnter={() => onActiveIndexChange(i)}
                onClick={() => onSelect(entry)}
              >
                <span className="prompt-history__item-text">{preview}</span>
              </Button>
            );
          })
        )}
      </div>
    </div>
  );
}
