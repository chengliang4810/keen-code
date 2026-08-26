import { Button } from "@/components/ui/button";
/**
 * Composer project chip — pick / add folder.
 * Git worktrees live in {@link ComposerWorktreeMenu} (branch chip).
 */

import { useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import {
  IconCheck,
  IconFolder,
  IconPlus,
} from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
import { useFloatingMenu } from "@/lib/floatingMenu";

export type ProjectOption = {
  id: string;
  name: string;
  path: string;
  pathOk: boolean;
};

type Props = {
  activeProject: ProjectOption | null;
  projects: ProjectOption[];
  labels: {
    pickProject: string;
    addProject: string;
    /** Badge when project folder is missing on disk. */
    pathMissing?: string;
  };
  disabled?: boolean;
  onSelect: (project: ProjectOption) => void;
  onAdd: (returnFocus: HTMLButtonElement | null) => void;
};

const LIST_MAX_H = 220;

export function ComposerProjectMenu({
  activeProject,
  projects,
  labels,
  disabled,
  onSelect,
  onAdd,
}: Props) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);

  const estHeight = Math.min(
    360,
    52 + Math.min(LIST_MAX_H, projects.length * 40 + 8),
  );
  const { pos, style: popStyle } = useFloatingMenu({
    open,
    triggerRef,
    panelRef: popRef,
    roots: [rootRef],
    onClose: () => setOpen(false),
    placement: "auto",
    fitContent: true,
    minWidth: 240,
    estHeight,
    gap: 8,
    deps: [projects.length],
  });

  const label = activeProject?.name ?? labels.pickProject;
  const activeMissing = activeProject?.pathOk === false;
  const tip = activeMissing
    ? (labels.pathMissing
        ? `${labels.pathMissing}: ${activeProject?.path || ""}`.trim()
        : activeProject?.path) || labels.pickProject
    : activeProject?.path || labels.pickProject;

  return (
    <div
      ref={rootRef}
      className={`cpm cpm--context${open ? " is-open" : ""}`}
    >
      <Tip label={tip} disabled={open}>
        <Button
          ref={triggerRef}
          type="button"
          className={
            "composer__context-item composer__context-item--project" +
            (open ? " is-open" : "") +
            (!activeProject ? " is-muted" : "") +
            (activeMissing ? " is-path-missing" : "")
          }
          disabled={disabled}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          <IconFolder size={14} />
          <span className="composer__context-label">
            {label}
          </span>
        </Button>
      </Tip>
      {open &&
        pos &&
        typeof document !== "undefined" &&
        createPortal(
          <div
            ref={popRef}
            className="cmm__pop cmm__pop--portal cpm__pop"
            role="menu"
            aria-label={labels.pickProject}
            style={popStyle as CSSProperties}
          >
            <div className="cpm__actions">
              <Button
                type="button"
                role="menuitem"
                className="cpm__action cpm__action--add"
                onClick={() => {
                  setOpen(false);
                  onAdd(triggerRef.current);
                }}
              >
                <IconPlus size={14} aria-hidden />
                <span>{labels.addProject}</span>
              </Button>
            </div>
            {projects.length > 0 ? (
              <div
                className="cpm__list"
                style={{ maxHeight: LIST_MAX_H }}
                role="group"
                aria-label={labels.pickProject}
              >
                {projects.map((p) => {
                  const active = activeProject?.id === p.id;
                  const missing = p.pathOk === false;
                  return (
                    <Button
                      key={p.id}
                      type="button"
                      role="menuitem"
                      className={
                        "cmm__opt cpm__item" +
                        (active ? " is-active" : "") +
                        (missing ? " cpm__item--path-missing" : "")
                      }
                      title={
                        missing && labels.pathMissing
                          ? `${labels.pathMissing}: ${p.path}`
                          : p.path
                      }
                      onClick={() => {
                        onSelect(p);
                        setOpen(false);
                      }}
                    >
                      <span className="cmm__opt-main">
                        <span className="cmm__opt-title">{p.name}</span>
                        {missing && labels.pathMissing ? (
                          <span className="cpm__path-badge">
                            {labels.pathMissing}
                          </span>
                        ) : null}
                      </span>
                      {active ? (
                        <span className="cmm__opt-check" aria-hidden>
                          <IconCheck size={16} />
                        </span>
                      ) : null}
                    </Button>
                  );
                })}
              </div>
            ) : null}
          </div>,
          document.body,
        )}
    </div>
  );
}
