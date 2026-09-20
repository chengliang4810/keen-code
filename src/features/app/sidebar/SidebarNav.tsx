import type { RefObject } from "react";
import type {
  SidebarNewChat,
  SidebarTranslator,
} from "./types";
import { Button } from "@/components/ui/button";
import {
  IconNewChat,
  IconSearch,
} from "@/components/icons";

export interface SidebarNavProps {
  tr: SidebarTranslator;
  newChat: SidebarNewChat;
  openSearch: () => void;
  searchTriggerRef: RefObject<HTMLButtonElement | null>;
}

export function SidebarNav({
  tr,
  newChat,
  openSearch,
  searchTriggerRef,
}: SidebarNavProps) {
  return (
    <div className="sidebar-nav">
      <Button
        type="button"
        variant="ghost"
        size="md"
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
        variant="ghost"
        size="md"
        className="nav-new"
        onClick={openSearch}
      >
        <span className="nav-item__icon">
          <IconSearch size={18} />
        </span>
        {tr("sidebar.search")}
      </Button>
    </div>
  );
}
