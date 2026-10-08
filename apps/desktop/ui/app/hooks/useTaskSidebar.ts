import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { useTaskSidebars, flushTaskSidebars } from "@/app/store/taskSidebars";
import {
  captureTaskTools,
  defaultTaskSidebar,
  initializeTaskTools,
  validTaskSidebarView,
  type TaskSidebarSnapshot,
} from "@/app/lib/taskSidebar";
import type { SessionMeta } from "@/modules/ai/lib/sessions";
import type { Tab } from "@/modules/tabs/lib/useTabs";
import { DEFAULT_SPACE_ID } from "@/modules/tabs/lib/useTabs";
import type { UtilityTool } from "@/app/lib/developmentTools";

const defaultSnapshot = defaultTaskSidebar();
const draftSnapshot = { ...defaultSnapshot, open: false };

export function useTaskSidebarChrome(taskId: string | null, isDraft = false) {
  const fallback = isDraft ? draftSnapshot : defaultSnapshot;
  const fallbackRef = useRef(fallback);
  const taskIdRef = useRef(taskId);
  useLayoutEffect(() => {
    taskIdRef.current = taskId;
    fallbackRef.current = fallback;
  }, [taskId, fallback]);
  const snapshot = useTaskSidebars(
    (s) => (taskId ? s.byTask[taskId] : undefined) ?? fallback,
  );
  const hydrated = useTaskSidebars((s) => s.hydrated);
  const error = useTaskSidebars((s) => s.error);
  useEffect(() => {
    void useTaskSidebars.getState().init();
  }, []);
  useEffect(() => {
    const flush = () => {
      void flushTaskSidebars().catch(() => {});
    };
    window.addEventListener("blur", flush);
    window.addEventListener("beforeunload", flush);
    document.addEventListener("visibilitychange", flush);
    return () => {
      flush();
      window.removeEventListener("blur", flush);
      window.removeEventListener("beforeunload", flush);
      document.removeEventListener("visibilitychange", flush);
    };
  }, []);

  const update = useCallback(
    <K extends keyof TaskSidebarSnapshot>(
      key: K,
      value: SetStateAction<TaskSidebarSnapshot[K]>,
    ) => {
      const currentTaskId = taskIdRef.current;
      if (!currentTaskId) return;
      const current =
        useTaskSidebars.getState().byTask[currentTaskId] ?? fallbackRef.current;
      const next =
        typeof value === "function"
          ? (
              value as (
                previous: TaskSidebarSnapshot[K],
              ) => TaskSidebarSnapshot[K]
            )(current[key])
          : value;
      useTaskSidebars.getState().update(currentTaskId, { [key]: next });
    },
    [],
  );
  const setToolsOpen = useCallback(
    (value: SetStateAction<boolean>) => update("open", value),
    [update],
  );
  const setToolView = useCallback(
    (value: SetStateAction<TaskSidebarSnapshot["view"]>) =>
      update("view", value),
    [update],
  );
  const setUtilityTabs = useCallback(
    (value: SetStateAction<UtilityTool[]>) => update("utilityTabs", value),
    [update],
  );
  const setToolsWidth = useCallback(
    (value: number) => update("width", value),
    [update],
  );
  return {
    snapshot,
    hydrated,
    error,
    toolsOpen: snapshot.open,
    toolView: snapshot.view,
    utilityTabs: snapshot.utilityTabs,
    setToolsOpen,
    setToolView,
    setUtilityTabs,
    setToolsWidth,
  };
}

type WorkspaceParams = {
  task: SessionMeta | undefined;
  projectId: string | null;
  sessions: SessionMeta[];
  ready: boolean;
  isDraft: boolean;
  tabs: Tab[];
  activeId: number;
  allocId: () => number;
  replaceTabs: (tabs: Tab[], activeId: number) => void;
  setActiveId: (id: number) => void;
  setActiveSpaceForNewTabs: (id: string) => void;
  setActiveTaskForNewTabs: (id: string) => void;
};

export function useTaskSidebarWorkspace({
  task,
  projectId,
  sessions,
  ready,
  isDraft,
  tabs,
  activeId,
  allocId,
  replaceTabs,
  setActiveId,
  setActiveSpaceForNewTabs,
  setActiveTaskForNewTabs,
}: WorkspaceParams): boolean {
  const visited = useRef(new Set<string>());
  const selections = useRef(new Map<string, number>());
  const owner = useRef<{ id: string; key: string } | null>(null);
  const [loadedOwner, setLoadedOwner] = useState<string | null>(null);
  const taskProjectId = task?.projectless ? DEFAULT_SPACE_ID : task?.projectId;
  const ownerKey = task ? `${task.id}:${projectId}` : null;

  useLayoutEffect(() => {
    if (
      !ready ||
      !task ||
      !ownerKey ||
      taskProjectId !== projectId ||
      !projectId ||
      owner.current?.key === ownerKey
    )
      return;
    const store = useTaskSidebars.getState();
    if (owner.current) {
      const previous = tabs.filter((tab) => tab.taskId === owner.current?.id);
      selections.current.set(owner.current.id, activeId);
      store.update(owner.current.id, captureTaskTools(previous, activeId));
    }
    const saved =
      store.byTask[task.id] ?? (isDraft ? draftSnapshot : defaultSnapshot);
    let owned = tabs.filter(
      (tab) => tab.taskId === task.id && tab.spaceId === projectId,
    );
    let nextTabs = tabs;
    if (!visited.current.has(ownerKey)) {
      // 旧项目标签只迁移给首次打开的一个对话，不能复制成多个终端实例。
      const initial = initializeTaskTools(
        tabs,
        projectId,
        task.id,
        // 无项目对话不能接管启动阶段留下的默认项目工具。
        store.byTask[task.id] ?? (task.projectless ? saved : undefined),
        allocId,
        !isDraft && !task.projectless,
      );
      owned = initial.owned;
      nextTabs = initial.tabs;
      visited.current.add(ownerKey);
    }
    const remembered = selections.current.get(task.id);
    const selected =
      owned.find((tab) => tab.id === remembered) ??
      owned[saved.activeTabIndex] ??
      owned[0];
    const nextId = selected?.id ?? -1;
    selections.current.set(task.id, nextId);
    setActiveSpaceForNewTabs(projectId);
    setActiveTaskForNewTabs(task.id);
    owner.current = { id: task.id, key: ownerKey };
    store.update(task.id, {
      open: saved.open,
      utilityTabs: saved.utilityTabs,
      view: validTaskSidebarView(
        saved.view,
        saved.utilityTabs,
        owned.length > 0,
      ),
    });
    if (nextTabs !== tabs) replaceTabs(nextTabs, nextId);
    else setActiveId(nextId);
    setLoadedOwner(ownerKey);
  }, [
    ready,
    isDraft,
    ownerKey,
    task,
    taskProjectId,
    projectId,
    tabs,
    activeId,
    allocId,
    replaceTabs,
    setActiveId,
    setActiveSpaceForNewTabs,
    setActiveTaskForNewTabs,
  ]);

  useEffect(() => {
    if (
      !ready ||
      !task ||
      loadedOwner !== ownerKey ||
      projectId !== taskProjectId
    )
      return;
    selections.current.set(task.id, activeId);
    const existingIds = new Set(sessions.map((session) => session.id));
    const store = useTaskSidebars.getState();
    const visitedIds = new Set(
      [...visited.current].map((key) => key.slice(0, key.lastIndexOf(":"))),
    );
    for (const id of Object.keys(store.byTask)) {
      if (!existingIds.has(id) && !visitedIds.has(id)) store.remove(id);
    }
    for (const id of visitedIds) {
      if (!existingIds.has(id)) {
        for (const key of visited.current) {
          if (key.startsWith(`${id}:`)) visited.current.delete(key);
        }
        selections.current.delete(id);
        store.remove(id);
        continue;
      }
      const owned = tabs.filter((tab) => tab.taskId === id);
      const selected = selections.current.get(id) ?? -1;
      const validId = owned.some((tab) => tab.id === selected)
        ? selected
        : (owned[0]?.id ?? -1);
      selections.current.set(id, validId);
      store.update(id, captureTaskTools(owned, validId));
    }
    const kept = tabs.filter(
      (tab) => !tab.taskId || existingIds.has(tab.taskId),
    );
    if (kept.length !== tabs.length) replaceTabs(kept, activeId);
  }, [
    ready,
    ownerKey,
    task,
    taskProjectId,
    projectId,
    sessions,
    loadedOwner,
    tabs,
    activeId,
    replaceTabs,
  ]);

  return (
    ready && !!task && taskProjectId === projectId && loadedOwner === ownerKey
  );
}
