import { Button } from "@/components/ui/button";
/**
 * Composer branch / worktree chip — switch linked worktrees, create, GC.
 * Lives next to the project picker on the new-session context bar.
 */

import { useEffect, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import {
  IconCheck,
  IconGitBranch,
  IconPlus,
  IconTrash,
} from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
import { useFloatingMenu } from "@/lib/floatingMenu";
import { pathsEqual, worktreeLabel } from "@/lib/gitWorktree";
import type { GitWorktreeEntry } from "@/lib/api";

export type ComposerWorktreeMenuLabels = {
  worktrees: string;
  worktreesEmpty: string;
  worktreesUnavailable: string;
  worktreesLoading?: string;
  worktreeCurrent: string;
  worktreeMain: string;
  worktreeDetached: string;
  /** Trigger tip / aria. */
  worktreeTip: string;
  worktreeNew: string;
  worktreeNewChat: string;
  worktreeGc: string;
};

type Props = {
  /** Absolute path of the bound project (current worktree root). */
  activePath: string | null;
  worktrees: GitWorktreeEntry[];
  /**
   * `true` only after host confirmed a git work tree.
   * When not true the whole chip is hidden by the parent.
   */
  worktreesAvailable?: boolean | null;
  worktreesLoading?: boolean;
  worktreesReason?: string | null;
  disabled?: boolean;
  /**
   * `chip` — generic toolbar.
   * `context` — new-session bar (flat trigger).
   */
  variant?: "chip" | "context";
  labels: ComposerWorktreeMenuLabels;
  onSwitch: (wt: GitWorktreeEntry) => void;
  onCreate: () => void;
  onCreateAndChat: () => void;
  onGc: () => void;
  onOpen?: () => void;
};

const LIST_MAX_H = 200;

export function ComposerWorktreeMenu({
  activePath,
  worktrees = [],
  worktreesLoading = false,
  worktreesReason = null,
  disabled,
  variant = "context",
  labels,
  onSwitch,
  onCreate,
  onCreateAndChat,
  onGc,
  onOpen,
}: Props) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const onOpenRef = useRef(onOpen);
  onOpenRef.current = onOpen;

  const current =
    worktrees.find((wt) => pathsEqual(wt.path, activePath)) ?? null;
  const branchLabel = current
    ? worktreeLabel(current)
    : worktreesLoading
      ? labels.worktreesLoading || "…"
      : "—";

  // Fixed size estimate so first paint matches final layout (avoids open flash).
  const listCount = Math.max(worktrees.length, 1);
  const estHeight = Math.min(
    420,
    44 + Math.min(LIST_MAX_H, listCount * 36 + 8) + 3 * 36 + 16,
  );
  // Soft-refresh loading should not re-anchor / dim when we already have rows.
  const showLoading = worktreesLoading && worktrees.length === 0;

  const { pos, style: popStyle } = useFloatingMenu({
    open,
    triggerRef,
    panelRef: popRef,
    roots: [rootRef],
    onClose: () => setOpen(false),
    // Welcome composer is vertically centered — auto picks up/down so the menu fits.
    placement: "auto",
    // Fixed width: fitContent + label measure caused first-open width "squeeze" flash.
    fitContent: false,
    width: 288,
    minWidth: 288,
    estHeight,
    gap: 8,
    // Only re-anchor when row count changes, not on soft-refresh loading toggles.
    deps: [worktrees.length],
  });

  useEffect(() => {
    if (!open) return;
    onOpenRef.current?.();
  }, [open]);

  const isContext = variant === "context";
  const tip = current?.path
    ? `${labels.worktreeTip}\n${current.path}`
    : labels.worktreeTip;

  return (
    <div
      ref={rootRef}
      className={
        `cwm${open ? " is-open" : ""}` + (isContext ? " cwm--context" : "")
      }
    >
      <Tip label={tip} disabled={open}>
        <Button
          ref={triggerRef}
          type="button"
          className={
            isContext
              ? "composer__context-item composer__context-item--branch" +
                (open ? " is-open" : "") +
                (showLoading ? " is-loading" : "")
              : "chip chip--branch" +
                (open ? " is-open" : "") +
                (showLoading ? " is-loading" : "")
          }
          disabled={disabled}
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label={labels.worktreeTip}
          onClick={() => setOpen((v) => !v)}
        >
          <IconGitBranch size={14} aria-hidden />
          <span
            className={isContext ? "composer__context-label" : "chip__label"}
          >
            {branchLabel}
          </span>
        </Button>
      </Tip>
      {open &&
        pos &&
        typeof document !== "undefined" &&
        createPortal(
          <div
            ref={popRef}
            className="cmm__pop cmm__pop--portal cwm__pop"
            role="menu"
            aria-label={labels.worktrees}
            style={popStyle as CSSProperties}
          >
            <div className="cwm__head">{labels.worktrees}</div>
            {worktrees.length > 0 ? (
              <ul
                className={"cwm__list" + (showLoading ? " is-loading" : "")}
                aria-busy={showLoading || undefined}
                style={{ maxHeight: LIST_MAX_H }}
              >
                {worktrees.map((wt) => {
                  const isCurrent = pathsEqual(wt.path, activePath);
                  const name = worktreeLabel(wt);
                  const meta = [
                    wt.isMain ? labels.worktreeMain : null,
                    wt.detached ? labels.worktreeDetached : null,
                    isCurrent ? labels.worktreeCurrent : null,
                  ]
                    .filter(Boolean)
                    .join(" · ");
                  return (
                    <li key={wt.path} className="cwm__row">
                      <Button
                        type="button"
                        role="menuitem"
                        className={
                          "cmm__opt cwm__item" + (isCurrent ? " is-active" : "")
                        }
                        title={wt.path}
                        disabled={isCurrent}
                        onClick={() => {
                          if (isCurrent) return;
                          setOpen(false);
                          onSwitch(wt);
                        }}
                      >
                        <span className="cwm__item-main">
                          <span className="cwm__item-name">{name}</span>
                          {meta ? (
                            <span className="cwm__item-meta">{meta}</span>
                          ) : null}
                        </span>
                        {isCurrent ? (
                          <span className="cmm__opt-check" aria-hidden>
                            <IconCheck size={16} />
                          </span>
                        ) : null}
                      </Button>
                    </li>
                  );
                })}
              </ul>
            ) : (
              <p className="cwm__empty">
                {worktreesReason?.trim()
                  ? labels.worktreesUnavailable
                  : labels.worktreesEmpty}
              </p>
            )}

            <div className="cwm__actions">
              <Button
                type="button"
                role="menuitem"
                className="cwm__action"
                onClick={() => {
                  setOpen(false);
                  onCreate();
                }}
              >
                <IconPlus size={14} aria-hidden />
                <span>{labels.worktreeNew}</span>
              </Button>
              <Button
                type="button"
                role="menuitem"
                className="cwm__action"
                onClick={() => {
                  setOpen(false);
                  onCreateAndChat();
                }}
              >
                <IconPlus size={14} aria-hidden />
                <span>{labels.worktreeNewChat}</span>
              </Button>
              <Button
                type="button"
                role="menuitem"
                className="cwm__action cwm__action--muted"
                onClick={() => {
                  setOpen(false);
                  onGc();
                }}
              >
                <IconTrash size={14} aria-hidden />
                <span>{labels.worktreeGc}</span>
              </Button>
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}
