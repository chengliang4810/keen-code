import { useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  ArrowDown01Icon,
  Cancel01Icon,
  Folder01Icon,
} from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Command,
  CommandInput,
  CommandList,
  CommandGroup,
  CommandItem,
  CommandSeparator,
} from "@/components/ui/command";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/modules/i18n";
import type { SpaceMeta } from "@/modules/spaces";

type Props = {
  projects: SpaceMeta[];
  projectId?: string;
  projectless: boolean;
  disabled: boolean;
  onSelectProject: (id: string | null) => void;
  onNewProject: () => void;
};

export function ProjectPicker({
  projects,
  projectId,
  projectless,
  disabled,
  onSelectProject,
  onNewProject,
}: Props) {
  const tr = useTranslation();
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const project = projectless
    ? undefined
    : projects.find((item) => item.id === projectId && !item.removed);
  const filtered = projects.filter(
    (item) =>
      !item.removed &&
      item.name.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase()),
  );
  const close = () => {
    setOpen(false);
    setSearch("");
  };

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) setSearch("");
      }}
    >
      <div className="group/project-picker relative inline-flex min-w-0 max-w-full">
        <PopoverTrigger asChild>
          <Button
            variant="ghost"
            size="xs"
            className={cn(
              "max-w-full gap-2 text-ui-base",
              !project && "text-muted-foreground",
            )}
            disabled={disabled}
            aria-label={tr("Select project")}
            aria-expanded={open}
          >
            <HugeiconsIcon
              icon={Folder01Icon}
              size={14}
              className={cn(
                project &&
                  !disabled &&
                  "group-hover/project-picker:opacity-0 group-focus-within/project-picker:opacity-0",
              )}
            />
            <span className="truncate">
              {project?.name ?? tr("Select project")}
            </span>
            <HugeiconsIcon icon={ArrowDown01Icon} size={12} />
          </Button>
        </PopoverTrigger>
        {project && !disabled && (
          <Tooltip>
            <TooltipTrigger asChild>
              {/* 退出按钮覆盖原图标位置但独立于弹层触发器，避免嵌套按钮与误开菜单。 */}
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="absolute top-0 left-1 opacity-0 hover:bg-transparent group-hover/project-picker:opacity-100 group-focus-within/project-picker:opacity-100"
                aria-label={tr("Work outside a project")}
                onClick={() => {
                  close();
                  onSelectProject(null);
                }}
              >
                <span className="flex size-3 items-center justify-center rounded-full bg-foreground text-background">
                  <HugeiconsIcon
                    icon={Cancel01Icon}
                    className="size-2"
                    strokeWidth={2.5}
                  />
                </span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{tr("Work outside a project")}</TooltipContent>
          </Tooltip>
        )}
      </div>
      <PopoverContent
        align="start"
        side="top"
        className="w-72 gap-0 overflow-hidden p-0"
        aria-label={tr("Select project")}
      >
        <Command
          shouldFilter={false}
          defaultValue={projectless ? "projectless" : projectId}
        >
          <CommandInput
            placeholder={tr("Search projects")}
            aria-label={tr("Search projects")}
            value={search}
            onValueChange={setSearch}
            className="text-ui-base"
          />
          <CommandList className="max-h-none">
            <CommandGroup className="max-h-48 overflow-y-auto">
              {filtered.length === 0 && (
                <p
                  role="status"
                  className="px-3 py-4 text-center text-ui-sm text-muted-foreground"
                >
                  {tr("No matching projects")}
                </p>
              )}
              {filtered.map((item) => (
                <CommandItem
                  key={item.id}
                  value={item.id}
                  data-checked={item.id === projectId && !projectless}
                  title={item.root ?? item.name}
                  className="text-ui-base data-[checked=true]:bg-accent"
                  onSelect={() => {
                    close();
                    onSelectProject(item.id);
                  }}
                >
                  <HugeiconsIcon icon={Folder01Icon} size={14} />
                  <span className="truncate">{item.name}</span>
                </CommandItem>
              ))}
            </CommandGroup>
            {/* 底部操作不参与项目搜索，始终可用且支持键盘选择。 */}
            <CommandSeparator />
            <CommandGroup>
              <CommandItem
                value="new-project"
                className="text-ui-base"
                onSelect={() => {
                  close();
                  onNewProject();
                }}
              >
                <HugeiconsIcon icon={Add01Icon} size={14} />
                {tr("New project")}
              </CommandItem>
              <CommandItem
                value="projectless"
                data-checked={projectless}
                className="text-ui-base data-[checked=true]:bg-accent"
                onSelect={() => {
                  close();
                  onSelectProject(null);
                }}
              >
                <HugeiconsIcon icon={Cancel01Icon} size={14} />
                {tr("Work outside a project")}
              </CommandItem>
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
