import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
} from "@/components/ui/popover";
import { AgentLauncherPanel } from "@/modules/agents/components/AgentLauncherPanel";
import type { AgentLaunchRequest } from "@/modules/agents/lib/launcher";
import { ArrowRight01Icon, PlusSignIcon } from "@hugeicons/core-free-icons";
import { useShortcutLabel } from "@/modules/shortcuts";
import {
  NEW_TAB_ACTIONS,
  type NewTabActionId,
} from "@/modules/tabs/lib/newTabActions";
import { HugeiconsIcon } from "@hugeicons/react";
import { useRef, useState } from "react";

type Props = {
  onNew: () => void;
  onNewPreview: () => void;
  onNewGitGraph: () => void;
  onLaunchAgents: (request: AgentLaunchRequest) => void;
  onOpenFiles?: () => void;
  onOpenGit?: () => void;
};

export function NewTabMenu({
  onNew,
  onNewPreview,
  onNewGitGraph,
  onLaunchAgents,
  onOpenFiles,
  onOpenGit,
}: Props) {
  const tr = useTranslation();
  const terminalShortcut = useShortcutLabel("tab.new");
  const previewShortcut = useShortcutLabel("tab.newPreview");
  const [menuOpen, setMenuOpen] = useState(false);
  const [launcherOpen, setLauncherOpen] = useState(false);
  const openLauncherAfterMenuClose = useRef(false);
  const openMenuAfterLauncherClose = useRef(false);

  const onMenuOpenChange = (next: boolean) => {
    if (next) {
      openLauncherAfterMenuClose.current = false;
      setLauncherOpen(false);
    }
    setMenuOpen(next);
  };

  const openLauncher = () => {
    openLauncherAfterMenuClose.current = true;
  };

  const backToMenu = () => {
    openMenuAfterLauncherClose.current = true;
    setLauncherOpen(false);
  };

  const actions: Record<NewTabActionId, (() => void) | undefined> = {
    files: onOpenFiles,
    git: onOpenGit,
    terminal: onNew,
    agents: openLauncher,
    preview: onNewPreview,
    gitGraph: onNewGitGraph,
  };

  return (
    <Popover open={launcherOpen} onOpenChange={setLauncherOpen}>
      <PopoverAnchor asChild>
        <span className="inline-flex">
          <DropdownMenu open={menuOpen} onOpenChange={onMenuOpenChange}>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="ml-1 size-6 shrink-0 rounded-full bg-foreground/[0.06] text-muted-foreground ring-1 ring-inset ring-foreground/[0.04] hover:bg-foreground/[0.12] hover:text-foreground"
                title={tr("New tab")}
                aria-label={tr("New tab")}
              >
                <HugeiconsIcon icon={PlusSignIcon} size={14} strokeWidth={2} />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent
              align="start"
              className="min-w-44"
              onCloseAutoFocus={(event) => {
                event.preventDefault();
                if (!openLauncherAfterMenuClose.current) return;

                openLauncherAfterMenuClose.current = false;
                requestAnimationFrame(() => setLauncherOpen(true));
              }}
            >
              {NEW_TAB_ACTIONS.map((entry) => {
                const action = actions[entry.id];
                if (!action) return null;
                const shortcut =
                  entry.id === "terminal"
                    ? terminalShortcut
                    : entry.id === "preview"
                      ? previewShortcut
                      : "";
                return (
                  <DropdownMenuItem key={entry.id} onSelect={action}>
                    <HugeiconsIcon
                      icon={entry.icon}
                      size={14}
                      strokeWidth={1.75}
                    />
                    <span className="flex-1">{tr(entry.label)}</span>
                    {entry.id === "agents" ? (
                      <HugeiconsIcon
                        icon={ArrowRight01Icon}
                        size={14}
                        strokeWidth={1.75}
                        className="text-muted-foreground"
                      />
                    ) : shortcut ? (
                      <span className="text-ui-sm text-muted-foreground">
                        {shortcut}
                      </span>
                    ) : null}
                  </DropdownMenuItem>
                );
              })}
            </DropdownMenuContent>
          </DropdownMenu>
        </span>
      </PopoverAnchor>
      <PopoverContent
        align="start"
        sideOffset={6}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          if (!openMenuAfterLauncherClose.current) return;

          openMenuAfterLauncherClose.current = false;
          requestAnimationFrame(() => setMenuOpen(true));
        }}
        className="w-[340px] gap-0 overflow-hidden rounded-2xl p-1.5"
      >
        <AgentLauncherPanel
          onBack={backToMenu}
          onLaunch={(request) => {
            setLauncherOpen(false);
            onLaunchAgents(request);
          }}
        />
      </PopoverContent>
    </Popover>
  );
}
