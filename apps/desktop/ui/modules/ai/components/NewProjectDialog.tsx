import { useEffect, useRef, useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/localized/dialog";
import { Button } from "@/components/ui/button";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group";
import { Label } from "@/components/ui/label";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Cancel01Icon,
  Folder01Icon,
  FolderAddIcon,
} from "@hugeicons/core-free-icons";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/modules/i18n";
import { native } from "@/modules/ai/lib/native";
import { projectNameForDirectory } from "@/modules/ai/lib/projectDirectory";
import { useProjectDirectoryDrop } from "@/modules/ai/lib/useProjectDirectoryDrop";

export function NewProjectDialog({
  onClose,
  onCreate,
}: {
  onClose: () => void;
  onCreate: (name: string, root: string) => Promise<void>;
}) {
  const tr = useTranslation();
  const [name, setName] = useState("");
  const [root, setRoot] = useState("");
  const [selecting, setSelecting] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const target = useRef<HTMLFieldSetElement>(null);
  const operation = useRef(false);
  const request = useRef(0);
  const busy = selecting || creating;
  useEffect(
    () => () => {
      request.current++;
    },
    [],
  );

  // 取消目录选择保留旧目录；卸载后的系统选择结果不能再更新表单。
  const selectDirectory = async (paths?: string[]) => {
    if (operation.current) return;
    operation.current = true;
    const current = ++request.current;
    setSelecting(true);
    setError(null);
    try {
      const picked =
        paths ?? (await native.pickProjectDirectory(tr("Select local folder")));
      if (!picked || request.current !== current) return;
      const directory = await native.projectDirectory(
        Array.isArray(picked) ? picked : [picked],
      );
      if (request.current === current) setRoot(directory);
    } catch (cause) {
      if (request.current === current)
        setError(tr(cause instanceof Error ? cause.message : String(cause)));
    } finally {
      if (request.current === current) {
        operation.current = false;
        setSelecting(false);
      }
    }
  };
  const hovering = useProjectDirectoryDrop(
    target,
    busy,
    (paths) => void selectDirectory(paths),
  );
  const inferredName = projectNameForDirectory("", root, tr("Project name"));
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (operation.current || !root) return;
    operation.current = true;
    setCreating(true);
    setError(null);
    const current = ++request.current;
    try {
      await onCreate(projectNameForDirectory(name, root, tr("Project")), root);
      if (request.current === current) onClose();
    } catch (cause) {
      if (request.current === current)
        setError(tr(cause instanceof Error ? cause.message : String(cause)));
    } finally {
      if (request.current === current) {
        operation.current = false;
        setCreating(false);
      }
    }
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent showCloseButton={!busy}>
        <form onSubmit={(event) => void submit(event)}>
          <DialogHeader>
            <DialogTitle>{tr("Create project")}</DialogTitle>
            <DialogDescription className="sr-only">
              {tr(
                "Choose a project directory. Each project contains its own agent tasks.",
              )}
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-4 py-5">
            <div className="flex flex-col gap-2">
              <Label htmlFor="project-name" className="sr-only">
                {tr("Project name")}
              </Label>
              <InputGroup>
                <InputGroupAddon>
                  <HugeiconsIcon icon={Folder01Icon} size={16} />
                </InputGroupAddon>
                <InputGroupInput
                  id="project-name"
                  className="text-ui-base md:text-ui-base"
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder={inferredName}
                  autoFocus
                  disabled={busy}
                  aria-describedby="project-name-hint"
                />
              </InputGroup>
              <p
                id="project-name-hint"
                className="text-ui-sm text-muted-foreground"
              >
                {tr("Leave the name empty to use the folder name.")}
              </p>
            </div>
            <div className="flex flex-col gap-2">
              <Label id="project-source-label">{tr("Source folder")}</Label>
              <fieldset
                ref={target}
                disabled={busy}
                aria-labelledby="project-source-label"
                data-project-directory-drop
                className={cn(
                  "flex min-h-40 min-w-0 flex-col items-center justify-center gap-3 rounded-xl border border-border/60 bg-foreground/[0.02] p-4 transition-colors",
                  hovering && "border-ring bg-accent",
                )}
              >
                {root ? (
                  <>
                    <HugeiconsIcon
                      icon={Folder01Icon}
                      size={24}
                      className="text-muted-foreground"
                    />
                    <div className="w-full min-w-0 text-center">
                      <p className="truncate text-ui-base font-medium">
                        {projectNameForDirectory("", root, tr("Project"))}
                      </p>
                      <p
                        className="mt-1 truncate text-ui-sm text-muted-foreground"
                        title={root}
                      >
                        {root}
                      </p>
                    </div>
                    <div className="flex items-center gap-2">
                      <Button
                        type="button"
                        size="xs"
                        variant="ghost"
                        disabled={busy}
                        onClick={() => void selectDirectory()}
                      >
                        {tr("Change folder")}
                      </Button>
                      <Button
                        type="button"
                        size="icon-xs"
                        variant="ghost"
                        disabled={busy}
                        aria-label={tr("Remove folder from form")}
                        title={tr("Remove folder from form")}
                        onClick={() => {
                          setRoot("");
                          setError(null);
                        }}
                      >
                        <HugeiconsIcon icon={Cancel01Icon} size={14} />
                      </Button>
                    </div>
                  </>
                ) : (
                  <>
                    <p className="text-ui-sm text-muted-foreground">
                      {tr("Add a folder on this computer")}
                    </p>
                    <Button
                      type="button"
                      size="xs"
                      variant="ghost"
                      disabled={busy}
                      onClick={() => void selectDirectory()}
                    >
                      <HugeiconsIcon icon={FolderAddIcon} size={16} />
                      {tr(selecting ? "Selecting…" : "Add")}
                    </Button>
                  </>
                )}
                <p className="text-ui-sm text-muted-foreground">
                  {tr(
                    hovering
                      ? "Drop the folder here"
                      : "Drag a folder here, or select one.",
                  )}
                </p>
              </fieldset>
            </div>
            {error && (
              <p className="text-ui-sm text-destructive" role="alert">
                {error}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="ghost"
              disabled={busy}
              onClick={onClose}
            >
              {tr("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !root}>
              {tr(creating ? "Creating…" : "Create project")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
