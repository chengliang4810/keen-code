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
import {
  projectSidebar,
  projectsFromSessions,
} from "@/lib/sessionProjection";
import { canUseAcpHost } from "@/lib/hostCapabilities";
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
  const [baseSessions, setSessions] = useState<SessionRow[]>([]);
  // 未进入列表的会话（新建对话首条消息）先在这里推进排序键，
  // 列表刷新载入该会话后合并，避免排序键在下一次 refreshLists 前推不动。
  const [pendingUserMessageAt, setPendingUserMessageAt] = useState<
    Record<string, string>
  >({});
  const sessions = useMemo(() => {
    const entries = Object.entries(pendingUserMessageAt);
    if (!entries.length) return baseSessions;
    const byId = new Map(entries);
    return baseSessions.map((item) => {
      const at = byId.get(item.id);
      return at && at > (item.lastUserMessageAt ?? "")
        ? { ...item, lastUserMessageAt: at, updatedAt: at }
        : item;
    });
  }, [baseSessions, pendingUserMessageAt]);
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
   * 直到下一次导航才跳动。会话尚未进入列表（新建对话首条消息）时记入
   * pendingUserMessageAt，待列表载入该会话后合并。
   */
  const markSessionUserMessage = useCallback(
    (sessionId: string, atIso: string) => {
      setPendingUserMessageAt((previous) =>
        previous[sessionId] && previous[sessionId]! >= atIso
          ? previous
          : { ...previous, [sessionId]: atIso },
      );
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

  // 列表已带相同或更新的排序键时清掉 pending 条目，避免覆盖表无界增长。
  useEffect(() => {
    const stale = Object.entries(pendingUserMessageAt).filter(
      ([id, at]) =>
        !sessions.some(
          (item) => item.id === id && (item.lastUserMessageAt ?? "") < at,
        ),
    );
    if (!stale.length) return;
    setPendingUserMessageAt((previous) => {
      const next = { ...previous };
      for (const [id] of stale) delete next[id];
      return next;
    });
  }, [sessions, pendingUserMessageAt]);

  const projectsRef = useRef(projects);
  projectsRef.current = projects;
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const isCurrentProject = (project: Project) => mounted.current &&
    projectsRef.current.some((item) => item.id === project.id && item.path === project.path);

  const refreshLists = useCallback(async () => {
    setAppBooting(false);
    if (!canUseAcpHost(api.isTauri())) return;
    const phase = "sessions_list/projects_list";
    try {
      const rows = await sessionsList();
      const persistedProjects = api.isTauri()
        ? await api.projectsList()
        : projectsFromSessions(rows);
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
      // Web 项目是由 Session cwd 派生的只读投影，不再经过 Tauri 项目登记。
      const checked = api.isTauri()
        ? await api.projectValidate(project.id)
        : project;
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
      // Host 持有 Session 与项目根的权威绑定；客户端路径只用于本地投影，
      // 不再次作为协议过滤条件，避免不同 Transport 的路径序列化产生授权分歧。
      const rows = await sessionsList();
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
      if (!canUseAcpHost(api.isTauri())) return;
      if (!api.isTauri()) {
        const rows = await sessionsList();
        const projection = projectSidebar(
          rows,
          loadSessionPreferences(),
          projectsFromSessions(rows),
        );
        setProjects(projection.projects);
        setSessions(projection.sessions);
        setActiveProject((previous) =>
          previous
            ? projection.projects.find((project) => project.id === previous.id) ?? previous
            : previous,
        );
        return;
      }
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
    if (!canUseAcpHost(api.isTauri())) return;
    try {
      const rows = await sessionsList();
      const sourceProjects = api.isTauri() ? projects : projectsFromSessions(rows);
      const projection = projectSidebar(rows, loadSessionPreferences(), sourceProjects);
      setProjects(projection.projects);
      setSessions(projection.sessions);
      projection.projects.forEach((project) => loadedProjects.current.add(project.id));
    } catch (cause) {
      showToast(cause instanceof Error ? cause.message : String(cause));
    }
  }, [projects, showToast]);

  const refreshProjects = useCallback(async () => {
    try {
      const list = api.isTauri()
        ? await api.projectsList()
        : projectsFromSessions(await sessionsList());
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
