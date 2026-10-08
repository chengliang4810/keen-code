import { LazyStore } from "@/lib/storage";
import type { WorkspaceEnv } from "@/modules/workspace";
import type { SerializedTab } from "./serialize";

export type SpaceMeta = {
  id: string;
  name: string;
  root: string | null;
  env: WorkspaceEnv;
  /** Opt-in accent, index into SPACE_COLORS. Undefined = theme primary. */
  color?: number;
  createdAt: number;
  updatedAt: number;
  /** 移除仅隐藏项目注册，保留 ID 和存档，重新添加相同目录时恢复。 */
  removed?: true;
};

export type SpaceState = {
  tabs: SerializedTab[];
  activeTabIndex: number;
};

const STORE_PATH = "projects";
const KEY_SPACES = "spaces";
const KEY_ACTIVE = "activeId";
const STATE_PREFIX = "state:";
const stateKey = (id: string) => `${STATE_PREFIX}${id}`;

const store = new LazyStore(STORE_PATH, { defaults: {}, autoSave: 500 });
let writeQueue = Promise.resolve();

function serializeWrite(operation: () => Promise<void>): Promise<void> {
  const pending = writeQueue.then(operation);
  writeQueue = pending.catch(() => {});
  return pending;
}

export type LoadedSpaces = {
  spaces: SpaceMeta[];
  activeId: string | null;
  states: Map<string, SpaceState>;
};

export async function loadAll(reload = false): Promise<LoadedSpaces> {
  if (reload) await store.reload();
  const entries = await store.entries();
  let spaces: SpaceMeta[] = [];
  let activeId: string | null = null;
  const states = new Map<string, SpaceState>();
  for (const [k, v] of entries) {
    if (k === KEY_SPACES) {
      if (!Array.isArray(v) || v.some((space) => !validSpace(space)))
        throw new Error("Invalid project metadata");
      spaces = v as SpaceMeta[];
    } else if (k === KEY_ACTIVE) {
      if (v !== null && typeof v !== "string")
        throw new Error("Invalid active project ID");
      activeId = v;
    } else if (k.startsWith(STATE_PREFIX)) {
      if (
        !v ||
        typeof v !== "object" ||
        !Array.isArray((v as SpaceState).tabs) ||
        !Number.isInteger((v as SpaceState).activeTabIndex)
      )
        throw new Error("Invalid project tool state");
      states.set(k.slice(STATE_PREFIX.length), v as SpaceState);
    }
  }
  return { spaces, activeId, states };
}

function validSpace(value: unknown): value is SpaceMeta {
  if (!value || typeof value !== "object") return false;
  const space = value as SpaceMeta;
  return (
    typeof space.id === "string" &&
    !!space.id &&
    typeof space.name === "string" &&
    (space.root === null || typeof space.root === "string") &&
    Number.isFinite(space.createdAt) &&
    Number.isFinite(space.updatedAt) &&
    !!space.env &&
    (space.env.kind === "local" ||
      (space.env.kind === "wsl" && typeof space.env.distro === "string"))
  );
}

export async function saveSpacesList(spaces: SpaceMeta[]): Promise<void> {
  await serializeWrite(() => store.set(KEY_SPACES, spaces));
}

export async function saveActiveId(id: string | null): Promise<void> {
  await serializeWrite(() => store.set(KEY_ACTIVE, id));
}

export type ProjectSnapshot = { spaces: SpaceMeta[]; activeId: string | null };

export async function saveProjectRemoval(
  snapshot: ProjectSnapshot,
  rollback: () => ProjectSnapshot,
): Promise<void> {
  await serializeWrite(async () => {
    try {
      await store.set(KEY_SPACES, snapshot.spaces);
      await store.set(KEY_ACTIVE, snapshot.activeId);
      await store.save();
    } catch (error) {
      const current = rollback();
      await store.set(KEY_SPACES, current.spaces);
      await store.set(KEY_ACTIVE, current.activeId);
      throw error;
    }
  });
}

export async function saveState(id: string, state: SpaceState): Promise<void> {
  await serializeWrite(() => store.set(stateKey(id), state));
}

export async function deleteSpaceData(id: string): Promise<void> {
  await serializeWrite(async () => {
    await store.delete(stateKey(id));
  });
}

export function newSpaceId(): string {
  return `sp-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}
