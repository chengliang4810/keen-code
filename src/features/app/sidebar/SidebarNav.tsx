import type { RefObject } from "react";
import type {
  SidebarNewChat,
  SidebarTranslator,
} from "./types";
import { Button } from "@/components/ui/button";
import {
  IconArchive,
  IconClose,
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
  showArchivedSessions: boolean;
  onToggleArchivedSessions: () => void;
}

export function SidebarNav({
  tr,
  newChat,
  openSearch,
  openPluginMarketplace,
  searchTriggerRef,
  showArchivedSessions,
  onToggleArchivedSessions,
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
      <Button
        type="button"
        variant="ghost"
        size="md"
        className="nav-new"
        aria-pressed={showArchivedSessions}
        aria-label={
          showArchivedSessions
            ? tr("sidebar.current")
            : tr("sidebar.archived")
        }
        onClick={onToggleArchivedSessions}
      >
        <span className="nav-item__icon">
          {showArchivedSessions ? <IconClose size={16} /> : <IconArchive size={16} />}
        </span>
        {showArchivedSessions
          ? tr("sidebar.current")
          : tr("sidebar.archived")}
      </Button>
    </div>
  );
}
