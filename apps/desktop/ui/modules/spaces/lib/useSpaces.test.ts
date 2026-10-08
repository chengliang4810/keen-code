import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSpaces } from "@/modules/spaces/lib/useSpaces";
import {
  deleteSpaceData,
  saveProjectRemoval,
  type SpaceMeta,
} from "@/modules/spaces/lib/store";

vi.mock("@/modules/spaces/lib/store", () => ({
  deleteSpaceData: vi.fn().mockResolvedValue(undefined),
  saveActiveId: vi.fn().mockResolvedValue(undefined),
  saveSpacesList: vi.fn().mockResolvedValue(undefined),
  saveProjectRemoval: vi.fn().mockResolvedValue(undefined),
  newSpaceId: () => "new-project",
}));
vi.mock("@/modules/settings/preferences", () => ({
  usePreferencesStore: { getState: () => ({ defaultWorkspaceEnv: "local" }) },
}));
const project: SpaceMeta = {
  id: "original",
  name: "Original",
  root: "/project",
  env: { kind: "local" },
  color: 2,
  createdAt: 1,
  updatedAt: 1,
};
beforeEach(() => {
  vi.clearAllMocks();
  useSpaces.getState().hydrate([project], project.id);
});
describe("project removal", () => {
  it("allows removing the sole Default project and keeps it removed on restoration", async () => {
    const fallback = { ...project, id: "default", name: "Default" };
    useSpaces.getState().hydrate([fallback], fallback.id);
    await useSpaces.getState().removeProject(fallback.id);
    const saved = useSpaces.getState().spaces;
    useSpaces.getState().hydrate(saved, null);
    expect(useSpaces.getState().activeId).toBeNull();
    expect(
      useSpaces.getState().spaces.filter((space) => !space.removed),
    ).toEqual([]);
    expect(saveProjectRemoval).toHaveBeenCalledWith(
      { spaces: [{ ...fallback, removed: true }], activeId: null },
      expect.any(Function),
    );
    expect(deleteSpaceData).not.toHaveBeenCalled();
  });
  it("keeps the project visible when saving fails and allows retry", async () => {
    vi.mocked(saveProjectRemoval).mockRejectedValueOnce(
      new Error("disk error"),
    );
    await expect(
      useSpaces.getState().removeProject(project.id),
    ).rejects.toThrow("disk error");
    expect(useSpaces.getState().spaces[0].removed).toBeUndefined();
    await useSpaces.getState().removeProject(project.id);
    expect(useSpaces.getState().spaces[0].removed).toBe(true);
  });
  it("retains edits and active selection made while removal is saving", async () => {
    let saved!: () => void;
    vi.mocked(saveProjectRemoval).mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          saved = resolve;
        }),
    );
    const pending = useSpaces.getState().removeProject(project.id);
    useSpaces.getState().rename(project.id, "Edited");
    useSpaces.getState().setActive("other");
    saved();
    await pending;
    expect(useSpaces.getState().spaces[0]).toMatchObject({
      name: "Edited",
      removed: true,
    });
    expect(useSpaces.getState().activeId).toBe("other");
    expect(saveProjectRemoval).toHaveBeenLastCalledWith(
      {
        spaces: useSpaces.getState().spaces,
        activeId: "other",
      },
      expect.any(Function),
    );
  });
  it("retains project data and saves the removal marker without deleting tool archives", async () => {
    await useSpaces.getState().removeProject(project.id);
    expect(useSpaces.getState().spaces).toEqual([
      { ...project, removed: true },
    ]);
    expect(useSpaces.getState().activeId).toBeNull();
    expect(saveProjectRemoval).toHaveBeenCalledWith(
      { spaces: [{ ...project, removed: true }], activeId: null },
      expect.any(Function),
    );
    expect(deleteSpaceData).not.toHaveBeenCalled();
  });
  it("restores the stable ID and archive binding when the same directory is added", async () => {
    await useSpaces.getState().removeProject(project.id);
    const restored = useSpaces
      .getState()
      .create({ name: "Restored", root: project.root });
    expect(restored).toMatchObject({
      id: project.id,
      name: "Restored",
      createdAt: 1,
      color: 2,
    });
    expect(restored.removed).toBeUndefined();
    expect(useSpaces.getState().spaces).toHaveLength(1);
  });
  it("does not restore a matching path in a different environment", async () => {
    await useSpaces.getState().removeProject(project.id);
    const other = useSpaces.getState().create({
      name: "WSL",
      root: project.root,
      env: { kind: "wsl", distro: "Ubuntu" },
    });
    expect(other.id).not.toBe(project.id);
    expect(useSpaces.getState().spaces[0].removed).toBe(true);
  });
  it("selects another visible project and ignores repeat removals", async () => {
    useSpaces
      .getState()
      .hydrate(
        [
          project,
          { ...project, id: "hidden", removed: true },
          { ...project, id: "other" },
        ],
        project.id,
      );
    await useSpaces.getState().removeProject(project.id);
    expect(useSpaces.getState().activeId).toBe("other");
    await useSpaces.getState().removeProject(project.id);
    expect(saveProjectRemoval).toHaveBeenCalledTimes(1);
  });
});
