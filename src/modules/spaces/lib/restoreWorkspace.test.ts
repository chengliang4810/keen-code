import { describe, expect, it, vi } from "vitest";
import { restoreProjectWorkspace } from "@/modules/spaces/lib/restoreWorkspace";
import type { LoadedSpaces, SpaceMeta } from "@/modules/spaces/lib/store";

const project: SpaceMeta = {
  id: "default",
  name: "Default",
  root: "C:/Users/test",
  env: { kind: "local" },
  createdAt: 1,
  updatedAt: 1,
};

describe("project startup restoration", () => {
  it("keeps an empty project registry empty without creating a default project or terminal", () => {
    const loaded: LoadedSpaces = {
      spaces: [],
      activeId: "default",
      states: new Map(),
    };
    const allocate = vi.fn();
    expect(restoreProjectWorkspace(loaded, allocate)).toEqual({
      activeId: null,
      tabs: [],
      initialActiveIndex: {},
    });
    expect(loaded.spaces).toEqual([]);
    expect(allocate).not.toHaveBeenCalled();
  });
  it("does not restore a removed Default project or its tools after restart", () => {
    const loaded: LoadedSpaces = {
      spaces: [{ ...project, removed: true }],
      activeId: "default",
      states: new Map([
        [
          "default",
          {
            tabs: [{ kind: "editor", path: "C:/Users/test/keep.txt" }],
            activeTabIndex: 0,
          },
        ],
      ]),
    };
    const allocate = vi.fn();
    const restored = restoreProjectWorkspace(loaded, allocate);
    expect(restored.activeId).toBeNull();
    expect(restored.tabs).toEqual([]);
    expect(allocate).not.toHaveBeenCalled();
    expect(loaded.spaces[0].removed).toBe(true);
    expect(loaded.states.get("default")?.tabs).toHaveLength(1);
  });
  it("preserves a registered Default and restores another visible project when it is removed", () => {
    const other = { ...project, id: "other", name: "Other" };
    const loaded: LoadedSpaces = {
      spaces: [project, other],
      activeId: "default",
      states: new Map([
        [
          "other",
          {
            tabs: [{ kind: "editor", path: "C:/Users/test/keep.txt" }],
            activeTabIndex: 0,
          },
        ],
      ]),
    };
    expect(restoreProjectWorkspace(loaded, () => 1).activeId).toBe("default");
    loaded.spaces = [{ ...project, removed: true }, other];
    const restored = restoreProjectWorkspace(loaded, () => 1);
    expect(restored.activeId).toBe("other");
    expect(restored.tabs).toEqual([
      expect.objectContaining({
        kind: "editor",
        spaceId: "other",
        path: "C:/Users/test/keep.txt",
      }),
    ]);
  });
});
