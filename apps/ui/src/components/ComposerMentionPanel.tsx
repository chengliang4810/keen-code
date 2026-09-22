import { useEffect, useRef } from "react";
import { Button } from "@/components/ui/button";
import {
  composerMentionTriggerForKind,
  type ComposerMention,
  type ComposerMentionKind,
} from "@/lib/composerMentions";
import {
  IconBox,
  IconFileText,
  IconFolder,
  IconMessageCircle,
  IconPuzzle,
} from "@/components/icons";

export interface ComposerMentionPanelProps {
  entries: ComposerMention[];
  activeIndex: number;
  onActiveIndexChange: (index: number) => void;
  onSelect: (mention: ComposerMention) => void;
  groupLabels: Record<ComposerMentionKind, string>;
  loading?: boolean;
  emptyLabel: string;
  loadingLabel: string;
}

function mentionIcon(kind: ComposerMentionKind) {
  switch (kind) {
    case "directory":
      return <IconFolder size={15} />;
    case "file":
      return <IconFileText size={15} />;
    case "session":
      return <IconMessageCircle size={15} />;
    case "plugin":
      return <IconBox size={15} />;
    case "skill":
      return <IconPuzzle size={15} />;
  }
}

export type ComposerMentionRow =
  | { type: "section"; id: string; label: string }
  | { type: "entry"; entry: ComposerMention; navIndex: number };

/** Keep visual group headers on the same flat index space used by keyboard nav. */
export function buildComposerMentionRows(
  entries: ComposerMention[],
  groupLabels: Record<ComposerMentionKind, string>,
): ComposerMentionRow[] {
  const rows: ComposerMentionRow[] = [];
  let previousKind: ComposerMentionKind | null = null;
  entries.forEach((entry, navIndex) => {
    if (entry.kind !== previousKind) {
      const trigger = composerMentionTriggerForKind(entry.kind);
      rows.push({
        type: "section",
        id: `mention-section-${entry.kind}`,
        label: `${trigger} ${groupLabels[entry.kind]}`,
      });
      previousKind = entry.kind;
    }
    rows.push({ type: "entry", entry, navIndex });
  });
  return rows;
}

export function ComposerMentionPanel({
  entries,
  activeIndex,
  onActiveIndexChange,
  onSelect,
  groupLabels,
  loading = false,
  emptyLabel,
  loadingLabel,
}: ComposerMentionPanelProps) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const rows = buildComposerMentionRows(entries, groupLabels);

  useEffect(() => {
    const panel = panelRef.current;
    if (!panel || entries.length === 0) return;
    const active = panel.querySelector<HTMLElement>(
      `[data-mention-index="${activeIndex}"]`,
    );
    active?.scrollIntoView?.({ block: "nearest" });
  }, [activeIndex, entries]);

  const state = loading && entries.length === 0
    ? "loading"
    : entries.length === 0
      ? "empty"
      : "ready";

  return (
    <div
      ref={panelRef}
      className="menu-panel composer-mention-panel"
      role="listbox"
      aria-label="Mentions"
      aria-busy={loading || undefined}
      data-state={state}
    >
      {loading && entries.length === 0 ? (
        <div
          className="composer-plus__item composer-plus__item--muted"
          role="status"
          aria-live="polite"
          aria-busy="true"
        >
          {loadingLabel}
        </div>
      ) : null}
      {rows.map((row) => {
        if (row.type === "section") {
          return (
            <div key={row.id} className="composer-plus__section">
              {row.label}
            </div>
          );
        }
        const { entry, navIndex } = row;
        const active = navIndex === activeIndex;
        const trigger = composerMentionTriggerForKind(entry.kind);
        return (
          <Button
            key={entry.id}
            type="button"
            variant={active ? "soft" : "ghost"}
            size="md"
            role="option"
            aria-selected={active}
            data-mention-index={navIndex}
            data-mention-trigger={trigger}
            className={`composer-plus__item${active ? " is-active" : ""}`}
            onMouseDown={(event) => event.preventDefault()}
            onMouseEnter={() => onActiveIndexChange(navIndex)}
            onClick={() => onSelect(entry)}
          >
            <span className="composer-plus__ico" aria-hidden>
              {mentionIcon(entry.kind)}
            </span>
            <span className="composer-plus__title">{trigger}{entry.label}</span>
            <span className="composer-plus__desc">
              {entry.description || entry.value}
            </span>
          </Button>
        );
      })}
      {!loading && entries.length === 0 ? (
        <div className="composer-plus__item composer-plus__item--muted" role="status">
          {emptyLabel}
        </div>
      ) : null}
    </div>
  );
}
