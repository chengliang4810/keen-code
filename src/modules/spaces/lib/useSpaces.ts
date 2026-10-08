import { create } from "zustand";
import { usePreferencesStore } from "@/modules/settings/preferences";
import {
  parseWorkspaceScopeKey,
  workspaceScopeKey,
  type WorkspaceEnv,
} from "@/modules/workspace";
import {
  deleteSpaceData,
  newSpaceId,
  saveActiveId,
  saveSpacesList,
  saveProjectRemoval,
  type SpaceMeta,
} from "./store";

type CreateInput = {
  id?: string;
  name: string;
  root: string | null;
  env?: WorkspaceEnv;
};

type State = {
  spaces: SpaceMeta[];
  activeId: string | null;
  hydrated: boolean;
  loadError: string | null;
  loading: boolean;
  loadAttempt: number;
  retryLoad: () => void;
  setLoadState: (loading: boolean, error?: string | null) => void;
  // Per-space active tab index loaded from disk, so persistence preserves it
  // for spaces the user never visits this session.
  initialActiveIndex: Record<string, number>;
  hydrate: (
    spaces: SpaceMeta[],
    activeId: string | null,
    initialActiveIndex?: Record<string, number>,
  ) => void;
  create: (input: CreateInput) => SpaceMeta;
  rename: (id: string, name: string) => void;
  setEnv: (id: string, env: WorkspaceEnv) => void;
  setColor: (id: string, color: number | undefined) => void;
  reorder: (orderedIds: string[]) => void;
  remove: (id: string) => string | null;
  removeProject: (id: string) => Promise<void>;
  setActive: (id: string) => void;
};

export const useSpaces = create<State>((set, get) => ({
  spaces: [],
  activeId: null,
  hydrated: false,
  loadError: null,
  loading: false,
  loadAttempt: 0,
  retryLoad: () => {
    if (get().loading || get().hydrated) return;
    set((state) => ({ loadAttempt: state.loadAttempt + 1, loadError: null }));
  },
  setLoadState: (loading, loadError = null) => set({ loading, loadError }),
  initialActiveIndex: {},

  hydrate: (spaces, activeId, initialActiveIndex = {}) => {
    set({
      spaces,
      activeId,
      initialActiveIndex,
      hydrated: true,
      loading: false,
      loadError: null,
    });
  },

  create: (input) => {
    const loadError = get().loadError;
    if (loadError) throw new Error(loadError);
    const now = Date.now();
    const env =
      input.env ??
      parseWorkspaceScopeKey(
        usePreferencesStore.getState().defaultWorkspaceEnv,
      );
    // 复用被移除项目的稳定 ID，原会话与工具布局才能重新关联。
    const removed = input.root
      ? get().spaces.find(
          (space) =>
            space.removed &&
            space.root === input.root &&
            workspaceScopeKey(space.env) === workspaceScopeKey(env),
        )
      : undefined;
    const meta: SpaceMeta = {
      id: removed?.id ?? input.id ?? newSpaceId(),
      name: input.name,
      root: input.root,
      env,
      createdAt: removed?.createdAt ?? now,
      updatedAt: now,
      ...(removed?.color === undefined ? {} : { color: removed.color }),
    };
    const spaces = removed
      ? get().spaces.map((space) => (space.id === removed.id ? meta : space))
      : [...get().spaces, meta];
    set({ spaces });
    void saveSpacesList(spaces);
    return meta;
  },

  rename: (id, name) => {
    if (get().loadError || get().loading) return;
    const spaces = get().spaces.map((s) =>
      s.id === id ? { ...s, name, updatedAt: Date.now() } : s,
    );
    set({ spaces });
    void saveSpacesList(spaces);
  },

  setEnv: (id, env) => {
    if (get().loadError || get().loading) return;
    const spaces = get().spaces.map((s) =>
      s.id === id ? { ...s, env, updatedAt: Date.now() } : s,
    );
    set({ spaces });
    void saveSpacesList(spaces);
  },

  setColor: (id, color) => {
    if (get().loadError || get().loading) return;
    const spaces = get().spaces.map((s) =>
      s.id === id ? { ...s, color, updatedAt: Date.now() } : s,
    );
    set({ spaces });
    void saveSpacesList(spaces);
  },

  reorder: (orderedIds) => {
    if (get().loadError || get().loading) return;
    const byId = new Map(get().spaces.map((s) => [s.id, s]));
    const next: SpaceMeta[] = [];
    for (const id of orderedIds) {
      const s = byId.get(id);
      if (s) next.push(s);
    }
    for (const s of get().spaces) {
      if (!next.includes(s)) next.push(s);
    }
    if (next.length !== get().spaces.length) return;
    set({ spaces: next });
    void saveSpacesList(next);
  },

  remove: (id) => {
    if (get().loadError || get().loading) return get().activeId;
    const prev = get();
    const spaces = prev.spaces.filter((s) => s.id !== id);
    let activeId = prev.activeId;
    if (activeId === id) activeId = spaces[0]?.id ?? null;
    set({ spaces, activeId });
    void saveSpacesList(spaces);
    void deleteSpaceData(id);
    if (activeId !== prev.activeId) void saveActiveId(activeId);
    return activeId;
  },

  removeProject: async (id) => {
    const loadError = get().loadError;
    if (loadError) throw new Error(loadError);
    // 保存成功后再隐藏；等待期间若项目被编辑，基于新列表重试以保留并发变更。
    for (;;) {
      const prev = get();
      if (!prev.spaces.some((space) => space.id === id && !space.removed))
        return;
      const spaces = prev.spaces.map((space) =>
        space.id === id ? { ...space, removed: true as const } : space,
      );
      const activeId =
        prev.activeId === id
          ? (spaces.find((space) => !space.removed)?.id ?? null)
          : prev.activeId;
      await saveProjectRemoval({ spaces, activeId }, () => ({
        spaces: get().spaces,
        activeId: get().activeId,
      }));
      if (get().spaces !== prev.spaces || get().activeId !== prev.activeId)
        continue;
      set({ spaces, activeId });
      return;
    }
  },

  setActive: (id) => {
    if (get().loadError || get().loading) return;
    if (get().activeId === id) return;
    set({ activeId: id });
    void saveActiveId(id);
  },
}));
