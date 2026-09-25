import { Button } from "@appica/ui-react/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuGroupLabel,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
/**
 * Composer branch / worktree chip — switch linked worktrees, create, GC.
 * Lives next to the project picker on the new-session context bar.
 */

import { useEffect, useRef, useState } from "react";
import {
  IconGitBranch,
  IconPlus,
  IconTrash,
} from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
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
  const onOpenRef = useRef(onOpen);
  onOpenRef.current = onOpen;

  const current =
    worktrees.find((wt) => pathsEqual(wt.path, activePath)) ?? null;
  const branchLabel = current
    ? worktreeLabel(current)
    : worktreesLoading
      ? labels.worktreesLoading || "…"
      : "—";

  // Soft-refresh loading should not re-anchor / dim when we already have rows.
  const showLoading = worktreesLoading && worktrees.length === 0;

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
      className={
        `cwm${open ? " is-open" : ""}` + (isContext ? " cwm--context" : "")
      }
    >
      <DropdownMenu open={open} onOpenChange={setOpen} size="md">
        <Tip label={tip}>
          <DropdownMenuTrigger
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
          aria-label={labels.worktreeTip}
            render={<Button type="button" variant="ghost" size="md" />}
        >
          <IconGitBranch size={14} aria-hidden />
          <span
            className={isContext ? "composer__context-label" : "chip__label"}
          >
            {branchLabel}
          </span>
          </DropdownMenuTrigger>
        </Tip>
        <DropdownMenuContent
          className="cmm__pop cmm__pop--portal cwm__pop"
          side="top"
          align="start"
          sideOffset={8}
        >
          <DropdownMenuGroup>
            <DropdownMenuGroupLabel className="cwm__head">
              {labels.worktrees}
            </DropdownMenuGroupLabel>
            {worktrees.length > 0 ? (
              <DropdownMenuRadioGroup
                value={current?.path ?? ""}
                onValueChange={(path) => {
                  const selected = worktrees.find((candidate) => candidate.path === path);
                  if (selected && !pathsEqual(selected.path, activePath)) onSwitch(selected);
                }}
              >
                <div
                className={"cwm__list" + (showLoading ? " is-loading" : "")}
                aria-busy={showLoading || undefined}
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
                    <div key={wt.path} className="cwm__row">
                      <DropdownMenuRadioItem
                        className={
                          "cmm__opt cwm__item" + (isCurrent ? " is-active" : "")
                        }
                        value={wt.path}
                        title={wt.path}
                      >
                        <span className="cwm__item-main">
                          <span className="cwm__item-name">{name}</span>
                          {meta ? (
                            <span className="cwm__item-meta">{meta}</span>
                          ) : null}
                        </span>
                      </DropdownMenuRadioItem>
                    </div>
                  );
                })}
                </div>
              </DropdownMenuRadioGroup>
            ) : (
              <p className="cwm__empty">
                {worktreesReason?.trim()
                  ? labels.worktreesUnavailable
                  : labels.worktreesEmpty}
              </p>
            )}
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <div className="cwm__actions">
              <DropdownMenuItem
                className="cwm__action"
                onClick={onCreate}
              >
                <IconPlus size={14} aria-hidden />
                <span>{labels.worktreeNew}</span>
              </DropdownMenuItem>
              <DropdownMenuItem
                className="cwm__action"
                onClick={onCreateAndChat}
              >
                <IconPlus size={14} aria-hidden />
                <span>{labels.worktreeNewChat}</span>
              </DropdownMenuItem>
              <DropdownMenuItem
                className="cwm__action cwm__action--muted"
                onClick={onGc}
              >
                <IconTrash size={14} aria-hidden />
                <span>{labels.worktreeGc}</span>
              </DropdownMenuItem>
            </div>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
