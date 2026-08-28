import type { RefObject } from "react";
import type { SettingsSectionId } from "@/lib/settingsCatalog";
import type {
  SidebarNewChat,
  SidebarTranslator,
} from "./types";
import { Button } from "@/components/ui/button";
import {
  IconNewChat,
  IconPuzzle,
  IconSearch,
  IconSkills,
} from "@/components/icons";

export interface SidebarNavProps {
  tr: SidebarTranslator;
  newChat: SidebarNewChat;
  openSearch: () => void;
  searchTriggerRef: RefObject<HTMLButtonElement | null>;
  navigateSettings: (section?: SettingsSectionId) => void;
}

export function SidebarNav({
  tr,
  newChat,
  openSearch,
  searchTriggerRef,
  navigateSettings,
}: SidebarNavProps) {
  return (
    <div className="sidebar-nav">
      <Button
        type="button"
        className="nav-new"
        onClick={() => void newChat(null)}
      >
        <span className="nav-item__icon">
          <IconNewChat size={18} />
        </span>
        {tr("sidebar.newSession")}
      </Button>
      <Button
        ref={searchTriggerRef}
        type="button"
        className="nav-new"
        onClick={openSearch}
      >
        <span className="nav-item__icon">
          <IconSearch size={18} />
        </span>
        {tr("sidebar.search")}
      </Button>
      <Button
        type="button"
        className="nav-new"
        onClick={() => navigateSettings("skills")}
      >
        <span className="nav-item__icon">
          <IconSkills size={18} />
        </span>
        {tr("sidebar.skills")}
      </Button>
      <Button
        type="button"
        className="nav-new"
        onClick={() => navigateSettings("market")}
      >
        <span className="nav-item__icon">
          <IconPuzzle size={18} />
        </span>
        {tr("sidebar.plugins")}
      </Button>
    </div>
  );
}
