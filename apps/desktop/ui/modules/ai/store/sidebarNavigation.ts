import { create } from "zustand";
import { LazyStore } from "@/lib/storage";
import {
  normalizeSidebarNavigation,
  toggleSidebarPin,
  toggleSidebarSection,
  moveSidebarProject,
  setSidebarConversationSort,
  moveSidebarConversation,
  type ConversationSort,
  type ProjectDropTarget,
  type ProjectDropEdge,
  type SidebarNavigationSnapshot,
  type SidebarPin,
  type SidebarSectionId,
} from "@/modules/ai/lib/sidebarNavigation";
import type { SessionMeta } from "@/modules/ai/lib/sessions";

const diskOptions = {
  defaults: {},
  autoSave: false,
};
let disk = new LazyStore("navigation", diskOptions);
type Update = (
  snapshot: SidebarNavigationSnapshot,
) => SidebarNavigationSnapshot;
let loading: Promise<void> | null = null;
let queue = Promise.resolve();
let pending = 0;
let failedUpdate: Update | null = null;

type State = {
  snapshot: SidebarNavigationSnapshot;
  hydrated: boolean;
  saving: boolean;
  error: "load" | "save" | null;
  init: () => Promise<void>;
  togglePin: (pin: SidebarPin) => Promise<void>;
  toggleSection: (id: SidebarSectionId) => Promise<void>;
  setConversationSort: (
    sort: ConversationSort,
    sessions: readonly SessionMeta[],
  ) => Promise<void>;
  moveConversation: (
    sessions: readonly SessionMeta[],
    sourceId: string,
    targetId: string,
    edge: ProjectDropEdge,
  ) => Promise<void>;
  moveProject: (
    ids: readonly string[],
    source: ProjectDropTarget,
    target: ProjectDropTarget,
    edge: ProjectDropEdge,
  ) => Promise<void>;
  retry: () => Promise<void>;
};

/** 串行计算并写入，保存成功后才更新展示，避免快速操作丢更新或假报成功。 */
function commit(update: Update): Promise<void> {
  if (!useSidebarNavigation.getState().hydrated) return Promise.resolve();
  pending++;
  useSidebarNavigation.setState({ saving: true });
  queue = queue.then(async () => {
    const snapshot = update(useSidebarNavigation.getState().snapshot);
    try {
      if (snapshot === useSidebarNavigation.getState().snapshot) return;
      await disk.set("navigation", snapshot);
      await disk.save();
      failedUpdate = null;
      useSidebarNavigation.setState({ snapshot, error: null });
    } catch {
      failedUpdate = update;
      useSidebarNavigation.setState({ error: "save" });
    } finally {
      pending--;
      useSidebarNavigation.setState({ saving: pending > 0 });
    }
  });
  return queue;
}

export const useSidebarNavigation = create<State>((set, get) => ({
  snapshot: { pins: [], collapsed: [] },
  hydrated: false,
  saving: false,
  error: null,
  init: () => {
    if (get().hydrated) return Promise.resolve();
    if (loading) return loading;
    loading = disk
      .get("navigation")
      .then((value) => {
        set({
          snapshot: normalizeSidebarNavigation(value),
          hydrated: true,
          error: null,
        });
      })
      .catch(() => {
        // 未成功读取时禁止写入，保护磁盘中可能存在的旧置顶列表。
        // LazyStore 会缓存失败的加载 Promise，重建句柄才能真正重试加载。
        disk = new LazyStore("navigation", diskOptions);
        set({ error: "load" });
      })
      .finally(() => {
        loading = null;
      });
    return loading;
  },
  togglePin: (pin) => commit((snapshot) => toggleSidebarPin(snapshot, pin)),
  toggleSection: (id) =>
    commit((snapshot) => toggleSidebarSection(snapshot, id)),
  setConversationSort: (sort, sessions) =>
    commit((snapshot) => setSidebarConversationSort(snapshot, sort, sessions)),
  moveConversation: (sessions, sourceId, targetId, edge) =>
    commit((snapshot) =>
      moveSidebarConversation(snapshot, sessions, sourceId, targetId, edge),
    ),
  moveProject: (ids, source, target, edge) =>
    commit((snapshot) =>
      moveSidebarProject(snapshot, ids, source, target, edge),
    ),
  retry: () =>
    get().hydrated
      ? failedUpdate
        ? commit(failedUpdate)
        : Promise.resolve()
      : get().init(),
}));
