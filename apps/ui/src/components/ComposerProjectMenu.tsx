import { Button } from "@/components/ui/button";
import { Badge } from "@appica/ui-react/badge";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
/**
 * Composer project chip — pick / add folder.
 * Git worktrees live in {@link ComposerWorktreeMenu} (branch chip).
 */

import { useRef, useState } from "react";
import { IconFolder, IconPlus } from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";

export type ProjectOption = {
  id: string;
  name: string;
  path: string;
  pathOk: boolean | null;
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
  /** Web Host 项目来自 Session cwd，只保留选择，不显示添加入口。 */
  canAddProject?: boolean;
  disabled?: boolean;
  onSelect: (project: ProjectOption) => void;
  onAdd: (returnFocus: HTMLButtonElement | null) => void;
};

export function ComposerProjectMenu({
  activeProject,
  projects,
  labels,
  canAddProject = true,
  disabled,
  onSelect,
  onAdd,
}: Props) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const label = activeProject?.name ?? labels.pickProject;
  const activeMissing = activeProject?.pathOk === false;
  const tip = activeMissing
    ? (labels.pathMissing
        ? `${labels.pathMissing}: ${activeProject?.path || ""}`.trim()
        : activeProject?.path) || labels.pickProject
    : activeProject?.path || labels.pickProject;

  return (
    <div className={`cpm cpm--context${open ? " is-open" : ""}`}>
      <DropdownMenu open={open} onOpenChange={setOpen} size="md">
        <Tip label={tip} disabled={open}>
          <DropdownMenuTrigger
            ref={triggerRef}
          className={
            "composer__context-item composer__context-item--project" +
            (open ? " is-open" : "") +
            (!activeProject ? " is-muted" : "") +
            (activeMissing ? " is-path-missing" : "")
          }
          disabled={disabled}
            render={<Button type="button" variant="ghost" size="md" />}
        >
          <IconFolder size={14} />
          <span className="composer__context-label">
            {label}
          </span>
          </DropdownMenuTrigger>
        </Tip>
        <DropdownMenuContent
          className="cmm__pop cmm__pop--portal cpm__pop"
          side="top"
          align="start"
          sideOffset={8}
        >
          <DropdownMenuGroup>
            <DropdownMenuRadioGroup
              value={activeProject?.id ?? ""}
              onValueChange={(projectId) => {
                const project = projects.find((candidate) => candidate.id === projectId);
                if (project) onSelect(project);
              }}
            >
              <div className="cpm__list">
              {projects.map((p) => {
                const missing = p.pathOk === false;
                return (
                    <DropdownMenuRadioItem
                    key={p.id}
                    className={
                        "cmm__opt cpm__item" +
                      (missing ? " cpm__item--path-missing" : "")
                    }
                      value={p.id}
                    title={
                      missing && labels.pathMissing
                        ? `${labels.pathMissing}: ${p.path}`
                        : p.path
                    }
                  >
                    <span className="cmm__opt-main">
                      <span className="cmm__opt-title">{p.name}</span>
                      {missing && labels.pathMissing ? (
                        <Badge size="md" variant="error">
                          {labels.pathMissing}
                        </Badge>
                      ) : null}
                    </span>
                    </DropdownMenuRadioItem>
                );
              })}
              </div>
            </DropdownMenuRadioGroup>
          </DropdownMenuGroup>
          {canAddProject ? (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                onClick={() => {
                  onAdd(triggerRef.current);
                }}
              >
                <IconPlus size={14} aria-hidden />
                <span>{labels.addProject}</span>
              </DropdownMenuItem>
            </>
          ) : null}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
