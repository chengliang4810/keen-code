import {
  useCallback,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
} from "react";
import type { Project, SessionRow } from "@/features/app/models";
import * as api from "@/lib/api";
import { localizeUiError } from "@/lib/session";
import { moveId, orderedByIds, saveSessionOrder } from "@/lib/sidebarOrder";
import type {
  SidebarDragKind,
  SidebarDropHint,
  SidebarSetState,
} from "./types";
import type { Locale } from "@/i18n";

export interface SidebarDragOptions {
  locale: Locale;
  projects: Project[];
  setProjects: SidebarSetState<Project[]>;
  sessions: SessionRow[];
  sessionOrder: string[];
  setSessionOrder: SidebarSetState<string[]>;
  refreshProjects: () => Promise<void>;
  setLocalError: SidebarSetState<string | null>;
}

export interface SidebarDragResult {
  projectDropHint: SidebarDropHint | null;
  setProjectDropHint: SidebarSetState<SidebarDropHint | null>;
  startSidebarDrag: (
    event: ReactDragEvent<HTMLElement>,
    kind: SidebarDragKind,
    id: string,
  ) => void;
  endSidebarDrag: () => void;
  dragOverProject: (
    event: ReactDragEvent<HTMLElement>,
    targetId: string,
  ) => void;
  dropProject: (
    event: ReactDragEvent<HTMLElement>,
    targetId: string,
  ) => void;
  dropSession: (
    event: ReactDragEvent<HTMLElement>,
    targetId: string,
  ) => void;
  applyProjectOrder: (ids: string[]) => void;
}

export function useSidebarDrag({
  locale,
  projects,
  setProjects,
  sessions,
  sessionOrder,
  setSessionOrder,
  refreshProjects,
  setLocalError,
}: SidebarDragOptions): SidebarDragResult {
  const draggedSidebarItemRef = useRef<{
    kind: SidebarDragKind;
    id: string;
  } | null>(null);
  const [projectDropHint, setProjectDropHint] =
    useState<SidebarDropHint | null>(null);
  const projectReorderRevisionRef = useRef(0);
  const projectReorderQueueRef = useRef<Promise<void>>(Promise.resolve());

  const startSidebarDrag = useCallback(
    (
      event: ReactDragEvent<HTMLElement>,
      kind: SidebarDragKind,
      id: string,
    ) => {
      draggedSidebarItemRef.current = { kind, id };
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData("text/plain", id);
    },
    [],
  );

  const endSidebarDrag = useCallback(() => {
    draggedSidebarItemRef.current = null;
    setProjectDropHint(null);
  }, []);

  const applyProjectOrder = useCallback(
    (ids: string[]) => {
      if (ids.every((id, index) => id === projects[index]?.id)) return;
      setProjects(orderedByIds(projects, ids));
      const revision = ++projectReorderRevisionRef.current;
      projectReorderQueueRef.current = projectReorderQueueRef.current.then(
        async () => {
          try {
            const saved = await api.projectsReorder(ids);
            if (revision === projectReorderRevisionRef.current) setProjects(saved);
          } catch (error) {
            if (revision !== projectReorderRevisionRef.current) return;
            await refreshProjects();
            setLocalError(localizeUiError(error, locale));
          }
        },
      );
    },
    [locale, projects, refreshProjects, setLocalError, setProjects],
  );

  const dragOverProject = useCallback(
    (event: ReactDragEvent<HTMLElement>, targetId: string) => {
      if (draggedSidebarItemRef.current?.kind !== "project") return;
      event.preventDefault();
      const { top, height } = event.currentTarget.getBoundingClientRect();
      const after = event.clientY > top + height / 2;
      setProjectDropHint((current) =>
        current?.id === targetId && current.after === after
          ? current
          : { id: targetId, after },
      );
    },
    [],
  );

  const dropProject = useCallback(
    (event: ReactDragEvent<HTMLElement>, targetId: string) => {
      event.preventDefault();
      event.stopPropagation();
      const dragged = draggedSidebarItemRef.current;
      if (dragged?.kind !== "project") return;
      const { top, height } = event.currentTarget.getBoundingClientRect();
      const ids = moveId(
        projects.map(({ id }) => id),
        dragged.id,
        targetId,
        event.clientY > top + height / 2,
      );
      draggedSidebarItemRef.current = null;
      setProjectDropHint(null);
      applyProjectOrder(ids);
    },
    [applyProjectOrder, projects],
  );

  const dropSession = useCallback(
    (event: ReactDragEvent<HTMLElement>, targetId: string) => {
      event.preventDefault();
      event.stopPropagation();
      const dragged = draggedSidebarItemRef.current;
      if (dragged?.kind !== "session") return;
      const { top, height } = event.currentTarget.getBoundingClientRect();
      const ids = moveId(
        orderedByIds(sessions, sessionOrder).map(({ id }) => id),
        dragged.id,
        targetId,
        event.clientY > top + height / 2,
      );
      draggedSidebarItemRef.current = null;
      setSessionOrder(ids);
      saveSessionOrder(ids);
    },
    [sessionOrder, sessions, setSessionOrder],
  );

  return {
    projectDropHint,
    setProjectDropHint,
    startSidebarDrag,
    endSidebarDrag,
    dragOverProject,
    dropProject,
    dropSession,
    applyProjectOrder,
  };
}
