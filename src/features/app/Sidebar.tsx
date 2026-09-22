import { useState, type RefObject } from "react";
import { UserMenu, type UserMenuProps } from "@/components/UserMenu";
import { OverlayScroll } from "@/components/OverlayScroll";
import { saveLayout, type LayoutPrefs } from "@/lib/layout";
import { Button } from "@/components/ui/button";
import { SidebarChrome } from "./sidebar/SidebarChrome";
import type { SidebarChromeProps } from "./sidebar/SidebarChrome";
import { SidebarNav } from "./sidebar/SidebarNav";
import type { SidebarNavProps } from "./sidebar/SidebarNav";
import { PinnedSessionList } from "./sidebar/PinnedSessionList";
import type { PinnedSessionListProps } from "./sidebar/PinnedSessionList";
import { ProjectTree } from "./sidebar/ProjectTree";
import type { ProjectTreeProps } from "./sidebar/ProjectTree";
import { HistorySessionList } from "./sidebar/HistorySessionList";
import type { HistorySessionListProps } from "./sidebar/HistorySessionList";
import { ArchivedSessionList } from "./sidebar/ArchivedSessionList";
import type { SidebarSortMode } from "@/lib/sidebarOrder";
import type { SessionRow } from "@/features/app/models";
import type { SidebarTranslator } from "./sidebar/types";

export interface SidebarFrameProps {
  sidebarRef: RefObject<HTMLElement | null>;
  layout: LayoutPrefs;
  resizingSidebar: boolean;
}

export type SidebarUserProps = UserMenuProps;

export interface SidebarProps {
  frame: SidebarFrameProps;
  tr: SidebarTranslator;
  chrome: Omit<SidebarChromeProps, "layout" | "tr" | "sidebarRef">;
  navigation: Omit<
    SidebarNavProps,
    "tr" | "showArchivedSessions" | "onToggleArchivedSessions"
  >;
  pinned: Omit<PinnedSessionListProps, "tr">;
  projectTree: Omit<ProjectTreeProps, "tr">;
  history: Omit<HistorySessionListProps, "tr">;
  archive: {
    sessions: SessionRow[];
    sessionOrder: string[];
    sessionSortMode: SidebarSortMode;
    loadAllSessions: () => Promise<void>;
    deleteArchivedSession: (sessionId: string) => void;
  };
  user: SidebarUserProps;
}

export function Sidebar({
  frame,
  tr,
  chrome,
  navigation,
  pinned,
  projectTree,
  history,
  archive,
  user,
}: SidebarProps) {
  const { sidebarRef, layout, resizingSidebar } = frame;
  const [showArchivedSessions, setShowArchivedSessions] = useState(false);

  const toggleArchivedSessions = () => {
    const next = !showArchivedSessions;
    setShowArchivedSessions(next);
    if (next) void archive.loadAllSessions();
  };

  const closeMobileSidebar = () => {
    chrome.setLayout((current) => {
      const next = { ...current, sidebarCollapsed: true };
      saveLayout(localStorage, next);
      return next;
    });
  };

  return (
    <>
      {!layout.sidebarCollapsed ? (
        <Button
          type="button"
          variant="ghost"
          size="md"
          className="sidebar-mobile-backdrop"
          aria-label={tr("main.leftPaneHide")}
          onClick={closeMobileSidebar}
        />
      ) : null}
      <aside
        ref={sidebarRef}
        className={
          "sidebar" +
          (layout.sidebarCollapsed ? " sidebar--hidden" : "") +
          (resizingSidebar ? " is-resizing" : "")
        }
        aria-hidden={layout.sidebarCollapsed}
        style={
          !layout.sidebarCollapsed
            ? {
                width: layout.sidebarWidth,
                minWidth: layout.sidebarWidth,
                maxWidth: layout.sidebarWidth,
              }
            : undefined
        }
      >
        <SidebarChrome
          {...chrome}
          layout={layout}
          sidebarRef={sidebarRef}
          tr={tr}
        />
        <SidebarNav
          {...navigation}
          tr={tr}
          showArchivedSessions={showArchivedSessions}
          onToggleArchivedSessions={toggleArchivedSessions}
        />
        <OverlayScroll
          className="sidebar__scroll"
          viewportClassName="sidebar__scroll-inner"
        >
          {showArchivedSessions ? (
            <>
              <PinnedSessionList {...pinned} tr={tr} />
              <ArchivedSessionList {...pinned} {...archive} tr={tr} />
            </>
          ) : (
            <>
              <PinnedSessionList {...pinned} tr={tr} />
              <ProjectTree {...projectTree} tr={tr} />
              <HistorySessionList {...history} tr={tr} />
            </>
          )}
        </OverlayScroll>
        <UserMenu {...user} />
      </aside>
    </>
  );
}
