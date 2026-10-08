import { AlertCircleIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { AgentLauncherPanel } from "@/modules/agents/components/AgentLauncherPanel";
import type { AgentLaunchRequest } from "@/modules/agents/lib/launcher";
import { useTranslation } from "@/modules/i18n";
import { useShortcutLabel } from "@/modules/shortcuts";
import { MAX_PANES_PER_TAB } from "@/modules/tabs/lib/useTabs";
import {
  NEW_TAB_ACTIONS,
  type NewTabActionId,
} from "@/modules/tabs/lib/newTabActions";

export function EmptyToolsPanel({
  onOpenFiles,
  onOpenGit,
  onOpenTerminal,
  onOpenPreview,
  onOpenGitGraph,
  onLaunchAgents,
}: {
  onOpenFiles: () => void;
  onOpenGit: () => void;
  onOpenTerminal: () => void;
  onOpenPreview: () => void;
  onOpenGitGraph: () => void;
  onLaunchAgents: (request: AgentLaunchRequest) => void;
}) {
  const tr = useTranslation();
  const [launcherOpen, setLauncherOpen] = useState(false);
  const actions: Record<NewTabActionId, () => void> = {
    files: onOpenFiles,
    git: onOpenGit,
    terminal: onOpenTerminal,
    agents: () => setLauncherOpen(true),
    preview: onOpenPreview,
    gitGraph: onOpenGitGraph,
  };
  return (
    <div className="@container flex h-full flex-col items-center overflow-y-auto px-4 py-6">
      <div className="my-auto w-full max-w-md shrink-0 text-center">
        <h2 className="text-ui-lg font-semibold">{tr("Open tabs")}</h2>
        <p className="mt-2 text-ui-sm leading-relaxed text-muted-foreground">
          {tr("Select a tab to open in the sidebar.")}
        </p>
        {/* 按侧栏实际宽度切换列表与卡片，避免窄面板中的文字拥挤。 */}
        <div className="mt-5 grid grid-cols-1 gap-2 @min-[320px]:grid-cols-3">
          {NEW_TAB_ACTIONS.map((entry) => {
            const button = (
              <Button
                variant="secondary"
                className="absolute inset-0 h-full w-full rounded-md"
                aria-label={tr(entry.label)}
                onClick={actions[entry.id]}
              />
            );
            return (
              <div
                key={entry.id}
                className="relative h-12 min-w-0 @min-[320px]:h-22"
              >
                {entry.id === "agents" ? (
                  <Popover open={launcherOpen} onOpenChange={setLauncherOpen}>
                    <PopoverTrigger asChild>{button}</PopoverTrigger>
                    <PopoverContent
                      align="end"
                      className="w-[340px] max-w-[calc(100vw-2rem)] gap-0 overflow-hidden rounded-2xl p-1.5"
                    >
                      <AgentLauncherPanel
                        onBack={() => setLauncherOpen(false)}
                        onLaunch={(request) => {
                          setLauncherOpen(false);
                          onLaunchAgents(request);
                        }}
                      />
                    </PopoverContent>
                  </Popover>
                ) : (
                  button
                )}
                {/* 说明按钮与启动按钮互为兄弟，查看说明不会新建终端。 */}
                <div className="pointer-events-none relative flex h-full items-center justify-start gap-3 px-3 text-ui-base text-secondary-foreground @min-[320px]:flex-col @min-[320px]:justify-center @min-[320px]:gap-2 @min-[320px]:px-2">
                  <HugeiconsIcon
                    icon={entry.icon}
                    size={16}
                    strokeWidth={1.75}
                    className="text-muted-foreground"
                  />
                  <span className="flex items-center justify-center gap-1">
                    {tr(entry.label)}
                    {entry.id === "terminal" && <TerminalShortcutHelp />}
                  </span>
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function TerminalShortcutHelp() {
  const tr = useTranslation();
  const newTab = useShortcutLabel("tab.new");
  const rows = [
    { label: "New terminal tab", shortcut: newTab },
  ];
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={tr("Terminal shortcuts")}
          className="pointer-events-auto inline-flex size-4 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <HugeiconsIcon icon={AlertCircleIcon} size={13} strokeWidth={1.75} />
        </button>
      </TooltipTrigger>
      <TooltipContent
        side="top"
        sideOffset={8}
        className="block max-w-sm space-y-2 rounded-md px-3 py-2 text-left"
      >
        <p className="font-medium">{tr("Terminal shortcuts")}</p>
        {rows.map((row) => (
          <div
            key={row.label}
            className="flex items-center justify-between gap-4"
          >
            <span>{tr(row.label)}</span>
            <span className="shrink-0 font-mono">
              {row.shortcut || tr("Unassigned")}
            </span>
          </div>
        ))}
        <p className="border-t border-background/20 pt-2 opacity-80">
          {tr(
            "Select a terminal tab to split it. Up to {count} terminals per tab.",
            { count: MAX_PANES_PER_TAB },
          )}
        </p>
      </TooltipContent>
    </Tooltip>
  );
}
