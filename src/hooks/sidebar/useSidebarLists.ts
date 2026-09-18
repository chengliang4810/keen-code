import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
} from "react";
import { t, type Locale } from "@/i18n";
import type { Project, SessionRow } from "@/features/app/models";
import * as api from "@/lib/api";
import {
  loadSessionOrder,
  loadSessionSortMode,
  saveSessionSortMode,
  sortSessionRows,
  type SidebarSortMode,
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
  locale: Locale;
  setActiveProject: SidebarSetState<Project | null>;
  setAppBooting: SidebarSetState<boolean>;
  setLocalError: SidebarSetState<string | null>;
  showToast: (message: string) => void;
  onProjectRemoved?: (project: Project) => void;
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
  sessionSortMode: SidebarSortMode;
  setSessionSortMode: (mode: SidebarSortMode) => void;
  /** 用户发出消息时把该会话的排序键推进到当前时间。 */
  markSessionUserMessage: (sessionId: string, atIso: string) => void;
  refreshLists: () => Promise<void>;
  loadAllSessions: () => Promise<void>;
  toggleProject: (project: Project) => Promise<void>;
  refreshSessions: (projectId?: string) => Promise<void>;
  refreshProjects: () => Promise<void>;
  sessionsForProject: (projectId: string) => SessionRow[];
  pinnedSessions: SessionRow[];
  orphanSessions: SessionRow[];
}

export function useSidebarLists({
  locale,
  setActiveProject,
  setAppBooting,
  setLocalError,
  showToast,
  onProjectRemoved,
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
  const [sessionSortMode, setSessionSortModeState] = useState(() =>
    loadSessionSortMode(),
  );
  const setSessionSortMode = useCallback((mode: SidebarSortMode) => {
    saveSessionSortMode(mode);
    setSessionSortModeState(mode);
  }, []);
  /**
   * 用户发送消息后立即推进本地排序键。等待后端列表刷新会让会话停在原位，
   * 直到下一次导航才跳动。
   */
  const markSessionUserMessage = useCallback(
    (sessionId: string, atIso: string) => {
      setSessions((previous) =>
        previous.some((item) => item.id === sessionId)
          ? previous.map((item) =>
              item.id === sessionId
                ? { ...item, lastUserMessageAt: atIso, updatedAt: atIso }
                : item,
            )
          : previous,
      );
    },
    [],
  );

  const projectsRef = useRef(projects);
  projectsRef.current = projects;
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const isCurrentProject = (project: Project) => mounted.current &&
    projectsRef.current.some((item) => item.id === project.id && item.path === project.path);

  const refreshLists = useCallback(async () => {
    setAppBooting(false);
    if (!api.isTauri()) return;
    const phase = "sessions_list/projects_list";
    try {
      const [rows, persistedProjects] = await Promise.all([
        sessionsList(),
        api.projectsList(),
      ]);
      if (!mounted.current) return;
      const projection = projectSidebar(
        rows,
        loadSessionPreferences(),
        persistedProjects,
      );
      setProjects(projection.projects);
      setSessions(
        projection.sessions.filter((session) => session.projectId === null),
      );
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

  const loadingProjects = useRef(new Set<string>());
  const loadedProjects = useRef(new Set<string>());
  const toggleProject = useCallback(async (project: Project) => {
    if (expandedProjects[project.id]) {
      setExpandedProjects((previous) => ({ ...previous, [project.id]: false }));
      return;
    }
    if (loadingProjects.current.has(project.id)) return;
    loadingProjects.current.add(project.id);
    try {
      const checked = await api.projectValidate(project.id);
      if (!isCurrentProject(project)) return;
      if (!checked) {
        loadedProjects.current.delete(project.id);
        setProjects((previous) => previous.filter((item) => item.id !== project.id));
        setSessions((previous) => previous.filter((item) => item.projectId !== project.id));
        setActiveProject((previous) => previous?.id === project.id ? null : previous);
        onProjectRemoved?.(project);
        showToast(t(locale, "project.removedMissing", { name: project.name }));
        return;
      }
      const rows = await sessionsList(checked.path);
      if (!isCurrentProject(project)) return;
      const projection = projectSidebar(rows, loadSessionPreferences(), [checked]);
      setProjects((previous) => previous.map((item) => item.id === project.id ? checked : item));
      setSessions((previous) => [...previous.filter((item) => item.projectId !== project.id), ...projection.sessions]);
      loadedProjects.current.add(project.id);
      setExpandedProjects((previous) => ({ ...previous, [project.id]: true }));
    } catch (cause) {
      showToast(cause instanceof Error ? cause.message : String(cause));
    } finally {
      loadingProjects.current.delete(project.id);
    }
  }, [expandedProjects, locale, onProjectRemoved, setActiveProject, showToast]);

  const refreshSessions = useCallback(async (projectId?: string) => {
    try {
      if (!api.isTauri()) return;
      const targets = projects.filter((project) => project.id === projectId || loadedProjects.current.has(project.id) || expandedProjects[project.id]);
      const rows = (await Promise.all(targets.map((project) => sessionsList(project.path)))).flat();
      const projection = projectSidebar(rows, loadSessionPreferences(), projects);
      const ids = new Set(targets.map((project) => project.id));
      ids.forEach((id) => loadedProjects.current.add(id));
      setSessions((previous) => [...previous.filter((item) => !item.projectId || !ids.has(item.projectId)), ...projection.sessions]);
    } catch {
      /* Keep the current tree when a soft refresh fails. */
    }
  }, [projects, expandedProjects]);

  const loadAllSessions = useCallback(async () => {
    if (!api.isTauri()) return;
    try {
      const rows = await sessionsList();
      setSessions(projectSidebar(rows, loadSessionPreferences(), projects).sessions);
      projects.forEach((project) => loadedProjects.current.add(project.id));
    } catch (cause) {
      showToast(cause instanceof Error ? cause.message : String(cause));
    }
  }, [projects, showToast]);

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
      sortSessionRows(
        sessions.filter(
          (item) =>
            item.projectId === projectId && !item.archived && !item.pinned,
        ),
        sessionOrder,
        sessionSortMode,
      ),
    [sessionOrder, sessionSortMode, sessions],
  );
  const pinnedSessions = useMemo(
    () =>
      sortSessionRows(
        sessions.filter((item) => item.pinned && !item.archived),
        sessionOrder,
        sessionSortMode,
      ),
    [sessionOrder, sessionSortMode, sessions],
  );
  const orphanSessions = useMemo(
    () =>
      sortSessionRows(
        sessions.filter(
          (item) =>
            (!item.projectId ||
              !projects.some((project) => project.id === item.projectId)) &&
            !item.archived &&
            !item.pinned,
        ),
        sessionOrder,
        sessionSortMode,
      ),
    [projects, sessionOrder, sessionSortMode, sessions],
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
    sessionSortMode,
    setSessionSortMode,
    markSessionUserMessage,
    refreshLists,
    loadAllSessions,
    toggleProject,
    refreshSessions,
    refreshProjects,
    sessionsForProject,
    pinnedSessions,
    orphanSessions,
  };
}
