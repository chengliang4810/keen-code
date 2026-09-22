import { useCallback, useState, type MouseEvent as ReactMouseEvent } from "react";
import type { ContextMenuState, Project, SessionRow } from "@/features/app/models";
import type { SidebarSetState } from "./types";

export interface SidebarMenusResult {
  ctxMenu: ContextMenuState;
  setCtxMenu: SidebarSetState<ContextMenuState>;
  openSessionMenu: (event: ReactMouseEvent, session: SessionRow) => void;
  openProjectMenu: (event: ReactMouseEvent, project: Project) => void;
}

export function useSidebarMenus(): SidebarMenusResult {
  const [ctxMenu, setCtxMenu] = useState<ContextMenuState>(null);

  const openSessionMenu = useCallback(
    (event: ReactMouseEvent, target: SessionRow) => {
      event.preventDefault();
      event.stopPropagation();
      setCtxMenu({
        kind: "session",
        id: target.id,
        x: event.clientX,
        y: event.clientY,
      });
    },
    [],
  );

  const openProjectMenu = useCallback(
    (event: ReactMouseEvent, project: Project) => {
      event.preventDefault();
      event.stopPropagation();
      setCtxMenu({
        kind: "project",
        id: project.id,
        x: event.clientX,
        y: event.clientY,
      });
    },
    [],
  );

  return { ctxMenu, setCtxMenu, openSessionMenu, openProjectMenu };
}
