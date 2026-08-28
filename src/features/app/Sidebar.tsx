import type { RefObject } from "react";
import { UserMenu, type UserMenuProps } from "@/components/UserMenu";
import { OverlayScroll } from "@/components/OverlayScroll";
import type { LayoutPrefs } from "@/lib/layout";
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
  chrome: Omit<SidebarChromeProps, "layout" | "tr">;
  navigation: Omit<SidebarNavProps, "tr">;
  pinned: Omit<PinnedSessionListProps, "tr">;
  projectTree: Omit<ProjectTreeProps, "tr">;
  history: Omit<HistorySessionListProps, "tr">;
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
  user,
}: SidebarProps) {
  const { sidebarRef, layout, resizingSidebar } = frame;

  return (
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
      <SidebarChrome {...chrome} layout={layout} tr={tr} />
      <SidebarNav {...navigation} tr={tr} />
      <OverlayScroll
        className="sidebar__scroll"
        viewportClassName="sidebar__scroll-inner"
      >
        <PinnedSessionList {...pinned} tr={tr} />
        <ProjectTree {...projectTree} tr={tr} />
        <HistorySessionList {...history} tr={tr} />
      </OverlayScroll>
      <UserMenu {...user} />
    </aside>
  );
}
