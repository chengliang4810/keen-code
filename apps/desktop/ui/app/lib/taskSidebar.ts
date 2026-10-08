import type { UtilityTool } from "@/app/lib/developmentTools";
import {
  hydrateTabs,
  serializeTabs,
  isSerializableTab,
  type SerializedTab,
} from "@/modules/spaces/lib/serialize";
import type { Tab } from "@/modules/tabs/lib/useTabs";

export type TaskSidebarView = UtilityTool | "workspace" | "empty";
type GitTool = Extract<
  Tab,
  { kind: "git-diff" | "git-history" | "git-commit-file" }
>;
type WithoutRuntime<T> = T extends GitTool
  ? Omit<T, "id" | "spaceId" | "taskId" | "cold">
  : never;
type SavedGitTool = WithoutRuntime<GitTool>;
export type SavedTaskTool = SerializedTab | SavedGitTool;

export type TaskSidebarSnapshot = {
  version: 1;
  open: boolean;
  utilityTabs: UtilityTool[];
  view: TaskSidebarView;
  width: number;
  tabs: SavedTaskTool[];
  activeTabIndex: number;
};

export function defaultTaskSidebar(): TaskSidebarSnapshot {
  return {
    version: 1,
    open: true,
    utilityTabs: ["explorer"],
    view: "explorer",
    width: 32,
    tabs: [],
    activeTabIndex: -1,
  };
}

/** 持久化只保存工具布局和文件标识，不保存私密终端或待审批内容。 */
export function isSavedTaskTool(tab: Tab): boolean {
  return (
    isSerializableTab(tab) ||
    ["git-diff", "git-history", "git-commit-file"].includes(tab.kind)
  );
}

export function captureTaskTools(tabs: Tab[], selectedId: number) {
  const saved = tabs.filter(isSavedTaskTool);
  return {
    tabs: saved.flatMap((tab): SavedTaskTool[] => {
      if (
        tab.kind === "git-diff" ||
        tab.kind === "git-history" ||
        tab.kind === "git-commit-file"
      ) {
        const {
          id: _id,
          spaceId: _space,
          taskId: _task,
          cold: _cold,
          ...data
        } = tab;
        return [data];
      }
      return serializeTabs([tab]);
    }),
    activeTabIndex: saved.findIndex((tab) => tab.id === selectedId),
  };
}

export function restoreTaskTools(
  saved: SavedTaskTool[],
  projectId: string,
  taskId: string,
  allocId: () => number,
): Tab[] {
  return saved.flatMap((entry): Tab[] => {
    if (
      entry.kind === "git-diff" ||
      entry.kind === "git-history" ||
      entry.kind === "git-commit-file"
    ) {
      if (typeof entry.repoRoot !== "string" || typeof entry.title !== "string")
        return [];
      if (
        entry.kind !== "git-history" &&
        (typeof entry.path !== "string" ||
          (entry.originalPath !== null &&
            typeof entry.originalPath !== "string"))
      )
        return [];
      if (
        entry.kind === "git-diff" &&
        ((entry.mode !== "+" && entry.mode !== "-") ||
          typeof entry.preview !== "boolean")
      )
        return [];
      if (
        entry.kind === "git-commit-file" &&
        [entry.sha, entry.shortSha, entry.subject].some(
          (value) => typeof value !== "string",
        )
      )
        return [];
      return [
        {
          ...entry,
          id: allocId(),
          spaceId: projectId,
          taskId,
          cold: true,
        } as Tab,
      ];
    }
    return hydrateTabs([entry], projectId, allocId).map((tab) => ({
      ...tab,
      taskId,
    }));
  });
}

export function initializeTaskTools(
  tabs: Tab[],
  projectId: string,
  taskId: string,
  saved: TaskSidebarSnapshot | undefined,
  allocId: () => number,
  adoptLegacy = true,
) {
  const live = tabs.filter(
    (tab) => tab.taskId === taskId && tab.spaceId === projectId,
  );
  const legacy = adoptLegacy
    ? tabs.filter((tab) => !tab.taskId && tab.spaceId === projectId)
    : [];
  // 后台桥可能先于导航创建标签；已有实例优先，空存档也不能复活旧项目标签。
  const owned = live.length
    ? live
    : saved
      ? restoreTaskTools(saved.tabs, projectId, taskId, allocId)
      : legacy.map((tab) => ({ ...tab, taskId }));
  const legacyIds = new Set(legacy.map((tab) => tab.id));
  const kept = tabs.filter((tab) => !legacyIds.has(tab.id));
  return {
    owned,
    tabs: [...kept, ...(live.length ? [] : owned)],
  };
}

export function validTaskSidebarView(
  view: TaskSidebarView,
  utilities: UtilityTool[],
  hasNative: boolean,
): TaskSidebarView {
  if (view === "workspace" && hasNative) return view;
  if (utilities.includes(view as UtilityTool)) return view;
  return utilities[0] ?? (hasNative ? "workspace" : "empty");
}

export function normalizeTaskSidebar(value: unknown): TaskSidebarSnapshot {
  const fallback = defaultTaskSidebar();
  if (!value || typeof value !== "object") return fallback;
  const saved = value as Partial<TaskSidebarSnapshot>;
  const utilities = Array.isArray(saved.utilityTabs)
    ? [
        ...new Set(
          saved.utilityTabs.filter(
            (tab): tab is UtilityTool =>
              tab === "explorer" || tab === "source-control",
          ),
        ),
      ]
    : fallback.utilityTabs;
  const tabs = Array.isArray(saved.tabs)
    ? saved.tabs.filter((tab) => tab && typeof tab === "object")
    : [];
  return {
    version: 1,
    open: typeof saved.open === "boolean" ? saved.open : true,
    utilityTabs: utilities,
    view: validTaskSidebarView(
      saved.view ?? fallback.view,
      utilities,
      tabs.length > 0,
    ),
    width:
      typeof saved.width === "number" && Number.isFinite(saved.width)
        ? Math.max(20, Math.min(65, saved.width))
        : 32,
    tabs,
    activeTabIndex: Number.isInteger(saved.activeTabIndex)
      ? Math.max(-1, saved.activeTabIndex ?? -1)
      : -1,
  };
}
