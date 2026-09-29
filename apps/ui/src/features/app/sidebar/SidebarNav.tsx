import type { RefObject } from "react";
import type {
  SidebarNewChat,
  SidebarTranslator,
} from "./types";
import { Button } from "@appica/ui-react/button";
import {
  IconNewChat,
  IconPuzzle,
  IconSearch,
} from "@/components/icons";

export interface SidebarNavProps {
  tr: SidebarTranslator;
  newChat: SidebarNewChat;
  openSearch: () => void;
  openPluginMarketplace: () => void;
  searchTriggerRef: RefObject<HTMLButtonElement | null>;
}

export function SidebarNav({
  tr,
  newChat,
  openSearch,
  openPluginMarketplace,
  searchTriggerRef,
}: SidebarNavProps) {
  return (
    <div className="sidebar-nav">
      <Button
        type="button"
        variant="ghost"
        size="md"
        className="nav-new"
        onClick={() => void newChat()}
      >
        <span className="nav-item__icon">
          <IconNewChat size={16} />
        </span>
        {tr("sidebar.newSession")}
      </Button>
      <Button
        ref={searchTriggerRef}
        type="button"
        variant="ghost"
        size="md"
        className="nav-new"
        onClick={openSearch}
      >
        <span className="nav-item__icon">
          <IconSearch size={16} />
        </span>
        {tr("sidebar.search")}
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="md"
        className="nav-new"
        onClick={openPluginMarketplace}
      >
        <span className="nav-item__icon">
          <IconPuzzle size={16} />
        </span>
        {tr("sidebar.plugins")}
      </Button>
    </div>
  );
}
