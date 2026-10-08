import { useTranslation } from "@/modules/i18n";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { SidebarPrimaryAction } from "@/modules/sidebar/SidebarPrimaryAction";
import type { SettingsTab } from "@/modules/settings/settingsOverlay";
import { usePreferencesStore } from "@/modules/settings/preferences";
import {
  AiScanIcon,
  BrainIcon,
  PuzzleIcon,
  ServerStack01Icon,
  BookOpen01Icon,
  CommandLineIcon,
  AnchorIcon,
  Archive02Icon,
  ArrowLeft01Icon,
  InformationCircleIcon,
  KeyboardIcon,
  PaintBoardIcon,
  Settings01Icon,
  SourceCodeIcon,
  UserMultiple02Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { type JSX, useEffect } from "react";
import { AboutSection } from "@/settings/sections/AboutSection";
import { AgentsSection } from "@/settings/sections/AgentsSection";
import { CommandsSection } from "@/settings/sections/CommandsSection";
import { SubagentsSection } from "@/settings/sections/SubagentsSection";
import {
  MemorySection,
  SkillsSection,
  HooksSection,
} from "@/settings/sections/AgentCapabilitiesSections";
import { EditorSection } from "@/settings/sections/EditorSection";
import { GeneralSection } from "@/settings/sections/GeneralSection";
import { ModelsSection } from "@/settings/sections/ModelsSection";
import { ExtensionsSection } from "@/settings/sections/ExtensionsSection";
import { ShortcutsSection } from "@/settings/sections/ShortcutsSection";
import { ThemesSection } from "@/settings/sections/ThemesSection";
import { ArchivedConversationsSection } from "@/settings/sections/ArchivedConversationsSection";
import { MarketplaceSection } from "@/settings/sections/MarketplaceSection";
import { PluginsSection } from "@/settings/sections/PluginsSection";

const TABS: {
  id: SettingsTab;
  label: string;
  icon: typeof Settings01Icon;
  component: () => JSX.Element;
}[] = [
  {
    id: "general",
    label: "General",
    icon: Settings01Icon,
    component: GeneralSection,
  },
  {
    id: "editor",
    label: "Editor",
    icon: SourceCodeIcon,
    component: EditorSection,
  },
  {
    id: "themes",
    label: "Themes",
    icon: PaintBoardIcon,
    component: ThemesSection,
  },
  {
    id: "shortcuts",
    label: "Shortcuts",
    icon: KeyboardIcon,
    component: ShortcutsSection,
  },
  { id: "models", label: "Models", icon: AiScanIcon, component: ModelsSection },
  { id: "memory", label: "Memory", icon: BrainIcon, component: MemorySection },
  { id: "agents", label: "Agents", icon: AiScanIcon, component: AgentsSection },
  {
    id: "subagents",
    label: "Subagents",
    icon: UserMultiple02Icon,
    component: SubagentsSection,
  },
  {
    id: "plugins",
    label: "Plugins",
    icon: PuzzleIcon,
    component: PluginsSection,
  },
  {
    id: "market",
    label: "Plugin marketplace",
    icon: PuzzleIcon,
    component: MarketplaceSection,
  },
  {
    id: "mcp",
    label: "MCP servers",
    icon: ServerStack01Icon,
    component: ExtensionsSection,
  },
  {
    id: "skills",
    label: "Skills",
    icon: BookOpen01Icon,
    component: SkillsSection,
  },
  {
    id: "commands",
    label: "Commands",
    icon: CommandLineIcon,
    component: CommandsSection,
  },
  { id: "hooks", label: "Hooks", icon: AnchorIcon, component: HooksSection },
  {
    id: "archives",
    label: "Archived conversations",
    icon: Archive02Icon,
    component: ArchivedConversationsSection,
  },
  {
    id: "about",
    label: "About",
    icon: InformationCircleIcon,
    component: AboutSection,
  },
];

const TAB_GROUPS = [
  {
    label: "Preferences",
    ids: ["general", "themes", "models", "editor", "shortcuts"],
  },
  {
    label: "Agent capabilities",
    ids: [
      "memory",
      "plugins",
      "market",
      "skills",
      "commands",
      "hooks",
      "agents",
      "subagents",
      "mcp",
    ],
  },
  { label: "Archived", ids: ["archives"] },
  { label: "Application", ids: ["about"] },
] as const;

export function SettingsApp({
  active,
  onTabChange,
  onClose,
  sidebarWidth,
}: {
  active: SettingsTab;
  onTabChange: (tab: SettingsTab) => void;
  onClose: () => void;
  sidebarWidth: number;
}) {
  const tr = useTranslation();
  const init = usePreferencesStore((s) => s.init);
  const activeTab = TABS.find((t) => t.id === active);
  const ActiveSection = activeTab?.component;

  useEffect(() => {
    void init();
  }, [init]);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden bg-frame text-foreground select-none">
      {/* 与主对话共用缩放范围，窗口操作栏维持原生尺寸。 */}
      <div className="zoom-content flex min-h-0 flex-1 p-2">
        <Tabs
          value={active}
          onValueChange={(v) => onTabChange(v as SettingsTab)}
          orientation="vertical"
          className="rcode-pane min-h-0 flex-1 gap-0"
        >
          <aside
            className="rcode-left-sidebar box-content flex shrink-0 flex-col border-r border-border/60 bg-card/35 max-[720px]:w-40!"
            style={{ width: sidebarWidth }}
          >
            <SidebarPrimaryAction onClick={onClose} muted>
              <HugeiconsIcon
                icon={ArrowLeft01Icon}
                size={16}
                strokeWidth={1.75}
              />
              {tr("Back to conversations")}
            </SidebarPrimaryAction>
            <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-5">
              <TabsList
                aria-label={tr("Settings")}
                className="h-auto! w-full items-stretch justify-start gap-5 rounded-none! bg-transparent p-0"
              >
                {TAB_GROUPS.map((group) => {
                  const tabs = group.ids
                    .map((id) => TABS.find((tab) => tab.id === id))
                    .filter((tab) => tab !== undefined);
                  if (tabs.length === 0) return null;
                  return (
                    <div
                      key={group.label}
                      className="flex w-full flex-col gap-1"
                    >
                      <span className="px-2 pb-1 text-ui-base text-muted-foreground">
                        {tr(group.label)}
                      </span>
                      {tabs.map((tab) => (
                        <TabsTrigger
                          key={tab.id}
                          value={tab.id}
                          className="h-8! flex-none justify-start gap-2 rounded-md! px-2! py-1.5! text-ui-base font-normal shadow-none data-active:bg-accent dark:data-active:bg-accent"
                        >
                          <HugeiconsIcon
                            icon={tab.icon}
                            size={14}
                            strokeWidth={1.75}
                          />
                          <span>{tr(tab.label)}</span>
                        </TabsTrigger>
                      ))}
                    </div>
                  );
                })}
              </TabsList>
            </div>
          </aside>
          {/* 切换分类时重建滚动区，避免沿用上一页的滚动位置。 */}
          <main
            key={active}
            className="min-h-0 min-w-0 flex-1 overflow-y-auto px-8 pt-14 pb-12 max-[720px]:px-4 max-[720px]:pt-8"
          >
            <TabsContent
              key={active}
              value={active}
              aria-label={activeTab ? tr(activeTab.label) : undefined}
              className={`mx-auto w-full ${active === "models" ? "max-w-208" : "max-w-182"}`}
            >
              {ActiveSection && <ActiveSection />}
            </TabsContent>
          </main>
        </Tabs>
      </div>
    </div>
  );
}
