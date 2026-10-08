import type { ComponentProps } from "react";
import { TabBar } from "@/modules/tabs";
import { useTranslation } from "@/modules/i18n";
import { HugeiconsIcon } from "@hugeicons/react";
import { FolderTreeIcon, FolderGitTwoIcon } from "@hugeicons/core-free-icons";
import type { UtilityTool } from "@/app/lib/developmentTools";

export type DevelopmentToolView = UtilityTool | "workspace" | "empty";

/** 单实例工具与多实例终端共用标签栏，每个标签独立关闭。 */
export function DevelopmentToolsTabs({
  view,
  utilityTabs,
  onViewChange,
  onOpenUtility,
  onCloseUtility,
  tabProps,
}: {
  view: DevelopmentToolView;
  utilityTabs: UtilityTool[];
  onViewChange: (view: DevelopmentToolView) => void;
  onOpenUtility: (tool: UtilityTool) => void;
  onCloseUtility: (tool: UtilityTool) => void;
  tabProps: ComponentProps<typeof TabBar>;
}) {
  const tr = useTranslation();
  return (
    <div
      role="toolbar"
      className="flex h-10 min-w-0 shrink-0 items-center gap-1 border-b border-border/60 px-2"
      aria-label={tr("Right sidebar")}
    >
      <div className="min-w-0 flex-1">
        <TabBar
          {...tabProps}
          activeId={view === "workspace" ? tabProps.activeId : -1}
          activeLeadingTab={
            utilityTabs.includes(view as UtilityTool) ? view : undefined
          }
          leadingTabs={utilityTabs.map((tool) => ({
            id: tool,
            label: tr(tool === "explorer" ? "Files" : "Git"),
            icon: (
              <HugeiconsIcon
                icon={tool === "explorer" ? FolderTreeIcon : FolderGitTwoIcon}
                size={14}
              />
            ),
            onClose: () => onCloseUtility(tool),
          }))}
          onSelectLeadingTab={(tool) => onViewChange(tool as UtilityTool)}
          onSelect={(id) => {
            onViewChange("workspace");
            tabProps.onSelect(id);
          }}
          onOpenFiles={() => onOpenUtility("explorer")}
          onOpenGit={() => onOpenUtility("source-control")}
          allowCloseLast
          compact
        />
      </div>
    </div>
  );
}
