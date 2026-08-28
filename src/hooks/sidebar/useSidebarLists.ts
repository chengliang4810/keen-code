import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
} from "react";
import type { Project, SessionRow } from "@/features/app/models";
import * as api from "@/lib/api";
import { collapsedIdsFromExpandMap, sameCollapsedIdSet } from "@/lib/sidebarExpand";
import {
  loadSessionOrder,
  orderedByIds,
} from "@/lib/sidebarOrder";
import {
  loadSessionPreferences,
} from "@/lib/sessionPreferences";
import { projectSidebar } from "@/lib/sessionProjection";
import {
  diagnosticsRecord,
  sessionsList,
} from "@/lib/acp/api";
import type { SidebarSetState } from "./types";

export interface SidebarListsOptions {
  setActiveProject: SidebarSetState<Project | null>;
  setAppBooting: SidebarSetState<boolean>;
  setLocalError: SidebarSetState<string | null>;
}

export interface SidebarListsResult {
  projects: Project[];
  setProjects: SidebarSetState<Project[]>;
  sessions: SessionRow[];
  setSessions: SidebarSetState<SessionRow[]>;
  sessionsRef: MutableRefObject<SessionRow[]>;
  expandedProjects: Record<string, boolean>;
  setExpandedProjects: SidebarSetState<Record<string, boolean>>;
  visibleSessionsByProject: Record<string, number>;
  setVisibleSessionsByProject: SidebarSetState<Record<string, number>>;
  sessionOrder: string[];
  setSessionOrder: SidebarSetState<string[]>;
  refreshLists: () => Promise<void>;
  refreshSessions: () => Promise<void>;
  refreshProjects: () => Promise<void>;
  sessionsForProject: (projectId: string) => SessionRow[];
  pinnedSessions: SessionRow[];
  orphanSessions: SessionRow[];
}

export function useSidebarLists({
  setActiveProject,
  setAppBooting,
  setLocalError,
}: SidebarListsOptions): SidebarListsResult {
  const [projects, setProjects] = useState<Project[]>([]);
  const [sessions, setSessions] = useState<SessionRow[]>([]);
  const sessionsRef = useRef<SessionRow[]>([]);
  sessionsRef.current = sessions;
  const [expandedProjects, setExpandedProjects] = useState<
    Record<string, boolean>
  >({});
  const [visibleSessionsByProject, setVisibleSessionsByProject] = useState<
    Record<string, number>
  >({});
  const [sessionOrder, setSessionOrder] = useState(() => loadSessionOrder());
  const expandedProjectsHydratedRef = useRef(false);

  const refreshLists = useCallback(async () => {
    setAppBooting(false);
    if (!api.isTauri()) return;
    const phase = "sessions_list/projects_list";
    try {
      const [rows, persistedProjects] = await Promise.all([
        sessionsList(),
        api.projectsList(),
      ]);
      const projection = projectSidebar(
        rows,
        loadSessionPreferences(),
        persistedProjects,
      );
      setProjects(projection.projects);
      setSessions(projection.sessions);
      setActiveProject((previous) => {
        if (
          previous &&
          projection.projects.some((project) => project.id === previous.id)
        ) {
          return (
            projection.projects.find((project) => project.id === previous.id) ??
            previous
          );
        }
        return null;
      });
      setExpandedProjects(
        Object.fromEntries(
          projection.projects.map((project) => [project.id, false]),
        ),
      );
      setLocalError(null);
      expandedProjectsHydratedRef.current = true;
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      await diagnosticsRecord(
        "frontend.refresh_lists",
        `${phase}: ${message}`,
      ).catch(() => {});
      console.error("[keencode] initial workspace data load failed", {
        phase,
        cause,
      });
      setLocalError("KeenCode 无法加载本地工作区数据，请稍后重试。");
    }
  }, [setActiveProject, setAppBooting, setLocalError]);

  useEffect(() => {
    void refreshLists();
  }, [refreshLists]);

  useEffect(() => {
    if (!expandedProjectsHydratedRef.current || !api.isTauri()) return;
    const ids = collapsedIdsFromExpandMap(expandedProjects);
    void api
      .settingsGet()
      .then((settings) => {
        if (sameCollapsedIdSet(settings.sidebarCollapsedProjectIds, ids)) return;
        return api.settingsSet({ sidebarCollapsedProjectIds: ids });
      })
      .catch(() => {});
  }, [expandedProjects]);

  const refreshSessions = useCallback(async () => {
    try {
      if (!api.isTauri()) return;
      const [rows, persistedProjects] = await Promise.all([
        sessionsList(),
        api.projectsList(),
      ]);
      const projection = projectSidebar(
        rows,
        loadSessionPreferences(),
        persistedProjects,
      );
      setProjects(projection.projects);
      setSessions(projection.sessions);
    } catch {
      /* Keep the current tree when a soft refresh fails. */
    }
  }, []);

  const refreshProjects = useCallback(async () => {
    try {
      const list = await api.projectsList();
      setProjects(list);
      setActiveProject((previous) => {
        if (!previous) return previous;
        return list.find((project) => project.id === previous.id) ?? previous;
      });
    } catch {
      /* Keep the current tree when a soft refresh fails. */
    }
  }, [setActiveProject]);

  const sessionsForProject = useCallback(
    (projectId: string) =>
      orderedByIds(
        sessions.filter(
          (item) =>
            item.projectId === projectId && !item.archived && !item.pinned,
        ),
        sessionOrder,
      ),
    [sessionOrder, sessions],
  );
  const pinnedSessions = useMemo(
    () =>
      orderedByIds(
        sessions.filter((item) => item.pinned && !item.archived),
        sessionOrder,
      ),
    [sessionOrder, sessions],
  );
  const orphanSessions = useMemo(
    () =>
      orderedByIds(
        sessions.filter(
          (item) =>
            (!item.projectId ||
              !projects.some((project) => project.id === item.projectId)) &&
            !item.archived &&
            !item.pinned,
        ),
        sessionOrder,
      ),
    [projects, sessionOrder, sessions],
  );

  return {
    projects,
    setProjects,
    sessions,
    setSessions,
    sessionsRef,
    expandedProjects,
    setExpandedProjects,
    visibleSessionsByProject,
    setVisibleSessionsByProject,
    sessionOrder,
    setSessionOrder,
    refreshLists,
    refreshSessions,
    refreshProjects,
    sessionsForProject,
    pinnedSessions,
    orphanSessions,
  };
}
