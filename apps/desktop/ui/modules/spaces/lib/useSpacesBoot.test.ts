import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSpacesBoot } from "@/modules/spaces/lib/useSpacesBoot";
import { useSpaces } from "@/modules/spaces/lib/useSpaces";
import { loadAll, saveSpacesList } from "@/modules/spaces/lib/store";
import type { LoadedSpaces, SpaceMeta } from "@/modules/spaces/lib/store";

const renderer = vi.hoisted(() => ({
  cursor: 0,
  slots: [] as unknown[],
  effects: [] as (() => void)[],
}));
vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useRef: (value: unknown) => {
    const index = renderer.cursor++;
    if (!(index in renderer.slots)) renderer.slots[index] = { current: value };
    return renderer.slots[index];
  },
  useEffect: (
    effect: () => undefined | (() => void),
    dependencies: unknown[],
  ) => {
    const index = renderer.cursor++;
    const previous = renderer.slots[index] as
      | { dependencies: unknown[]; cleanup?: () => void }
      | undefined;
    if (
      previous &&
      dependencies.every((value, i) =>
        Object.is(value, previous.dependencies[i]),
      )
    )
      return;
    renderer.effects.push(() => {
      previous?.cleanup?.();
      renderer.slots[index] = { dependencies, cleanup: effect() };
    });
  },
}));
vi.mock("@/modules/spaces/lib/useSpaces", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/modules/spaces/lib/useSpaces")>();
  return {
    useSpaces: Object.assign(
      (
        selector: (
          state: ReturnType<typeof actual.useSpaces.getState>,
        ) => unknown,
      ) => selector(actual.useSpaces.getState()),
      actual.useSpaces,
    ),
  };
});
vi.mock("@/modules/settings/preferences", () => ({
  usePreferencesStore: { getState: () => ({ defaultWorkspaceEnv: "local" }) },
}));
vi.mock("@/modules/ai/lib/native", () => ({
  native: { workspaceAuthorize: vi.fn().mockResolvedValue(undefined) },
}));
vi.mock("@/modules/spaces/lib/store", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/modules/spaces/lib/store")>()),
  loadAll: vi.fn(),
  saveSpacesList: vi.fn(),
  saveActiveId: vi.fn(),
  saveProjectRemoval: vi.fn(),
}));

const project: SpaceMeta = {
  id: "project",
  name: "Project",
  root: "D:/project",
  env: { kind: "local" },
  createdAt: 1,
  updatedAt: 1,
};
const params = {
  ready: true,
  launchCwd: "D:/launch",
  home: "D:/home",
  allocId: vi.fn(() => 1),
  replaceTabs: vi.fn(),
  markBooted: vi.fn(),
  setActiveSpaceForNewTabs: vi.fn(),
  adoptWorkspaceEnv: vi.fn().mockResolvedValue("D:/home"),
};
function BootFixture(current = params) {
  renderer.cursor = 0;
  useSpacesBoot(current);
  for (const effect of renderer.effects.splice(0)) effect();
}

beforeEach(() => {
  vi.clearAllMocks();
  renderer.cursor = 0;
  renderer.slots = [];
  renderer.effects = [];
  useSpaces.setState({
    spaces: [],
    activeId: null,
    hydrated: false,
    loadError: null,
    loading: false,
    loadAttempt: 0,
  });
  vi.mocked(loadAll).mockResolvedValue({
    spaces: [project],
    activeId: null,
    states: new Map(),
  });
});

describe("project startup restoration", () => {
  it("exposes a failed read, prevents writes, and restores repaired data through retry", async () => {
    vi.mocked(loadAll).mockRejectedValueOnce(new Error("project read failed"));
    BootFixture();
    await vi.waitFor(() =>
      expect(useSpaces.getState().loadError).toBe("project read failed"),
    );
    expect(useSpaces.getState()).toMatchObject({
      hydrated: false,
      loading: false,
      spaces: [],
    });
    useSpaces.getState().rename(project.id, "Unread");
    expect(saveSpacesList).not.toHaveBeenCalled();
    BootFixture();
    expect(loadAll).toHaveBeenCalledOnce();
    useSpaces.getState().retryLoad();
    BootFixture();
    await vi.waitFor(() => expect(useSpaces.getState().hydrated).toBe(true));
    expect(loadAll).toHaveBeenLastCalledWith(true);
    expect(useSpaces.getState()).toMatchObject({
      spaces: [project],
      loadError: null,
      loading: false,
    });
    expect(params.replaceTabs).toHaveBeenCalledWith(
      [expect.objectContaining({ kind: "terminal", spaceId: project.id })],
      1,
    );
  });

  it("coalesces repeated retry clicks while a restore is loading", async () => {
    let finish!: (loaded: LoadedSpaces) => void;
    vi.mocked(loadAll).mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    BootFixture();
    for (let i = 0; i < 5; i++) {
      useSpaces.getState().retryLoad();
      BootFixture();
    }
    expect(loadAll).toHaveBeenCalledOnce();
    finish({ spaces: [project], activeId: null, states: new Map() });
    await vi.waitFor(() => expect(useSpaces.getState().hydrated).toBe(true));
    BootFixture({ ...params, home: "D:/changed-home" });
    expect(loadAll).toHaveBeenCalledOnce();
  });

  it("ignores a superseded load response after startup inputs change", async () => {
    let finish!: (loaded: LoadedSpaces) => void;
    vi.mocked(loadAll).mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    BootFixture();
    BootFixture({ ...params, home: "D:/changed-home" });
    await vi.waitFor(() => expect(useSpaces.getState().hydrated).toBe(true));
    finish({
      spaces: [{ ...project, id: "stale" }],
      activeId: null,
      states: new Map(),
    });
    await Promise.resolve();
    expect(useSpaces.getState().spaces).toEqual([project]);
    expect(params.replaceTabs).toHaveBeenCalledOnce();
  });
});
