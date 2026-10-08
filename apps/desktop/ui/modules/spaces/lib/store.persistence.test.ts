import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SpaceMeta } from "@/modules/spaces/lib/store";

const fixtures = vi.hoisted(() => {
  const instances: LazyStore[] = [];
  class LazyStore {
    data = new Map<string, unknown>();
    persisted = new Map<string, unknown>();
    entries = vi.fn(async () => [...this.data.entries()]);
    set = vi.fn(async (key: string, value: unknown) => {
      this.data.set(key, value);
    });
    get = vi.fn(async (key: string) => this.data.get(key));
    delete = vi.fn(async (key: string) => this.data.delete(key));
    save = vi.fn(async () => {
      this.persisted = new Map(this.data);
    });
    reload = vi.fn(async () => {});
    constructor() {
      instances.push(this);
    }
  }
  return { instances, LazyStore };
});
vi.mock("@/lib/storage", () => ({ LazyStore: fixtures.LazyStore }));
vi.mock("@/modules/settings/preferences", () => ({
  usePreferencesStore: { getState: () => ({ defaultWorkspaceEnv: "local" }) },
}));

const original: SpaceMeta = {
  id: "project",
  name: "Original",
  root: "D:/project",
  env: { kind: "local" },
  createdAt: 1,
  updatedAt: 1,
};
const other: SpaceMeta = { ...original, id: "other", root: "D:/other" };

async function setup() {
  const persistence = await import("@/modules/spaces/lib/store");
  const { useSpaces } = await import("@/modules/spaces/lib/useSpaces");
  useSpaces.getState().hydrate([original, other], original.id);
  const disk = fixtures.instances[0];
  disk.data.set("spaces", [original, other]);
  disk.data.set("activeId", original.id);
  disk.data.set("state:project", { tabs: [], activeTabIndex: 0 });
  disk.persisted = new Map(disk.data);
  return { persistence, useSpaces, disk };
}

beforeEach(() => {
  vi.resetModules();
  fixtures.instances.length = 0;
});

describe("project removal persistence", () => {
  it("awaits disk save, commits list and active ID together, and retains tool archives", async () => {
    const { useSpaces, disk } = await setup();
    let finish!: () => void;
    disk.save.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finish = () => {
            disk.persisted = new Map(disk.data);
            resolve();
          };
        }),
    );
    const pending = useSpaces.getState().removeProject(original.id);
    await vi.waitFor(() => expect(disk.save).toHaveBeenCalledOnce());
    expect(useSpaces.getState().spaces[0].removed).toBeUndefined();
    expect(useSpaces.getState().activeId).toBe(original.id);
    finish();
    await pending;
    expect(disk.persisted.get("spaces")).toEqual([
      { ...original, removed: true },
      other,
    ]);
    expect(disk.persisted.get("activeId")).toBe(other.id);
    expect(disk.data.get("state:project")).toEqual({
      tabs: [],
      activeTabIndex: 0,
    });
    expect(useSpaces.getState().spaces[0].removed).toBe(true);
    expect(disk.delete).not.toHaveBeenCalled();
  });

  it("rolls back plugin memory on disk failure and retries without hiding the project", async () => {
    const { useSpaces, disk } = await setup();
    disk.save.mockRejectedValueOnce(new Error("disk full"));
    await expect(
      useSpaces.getState().removeProject(original.id),
    ).rejects.toThrow("disk full");
    expect(useSpaces.getState().spaces).toEqual([original, other]);
    expect(disk.data.get("spaces")).toEqual([original, other]);
    expect(disk.data.get("activeId")).toBe(original.id);
    await disk.save();
    expect(disk.persisted.get("spaces")).toEqual([original, other]);
    await useSpaces.getState().removeProject(original.id);
    expect(disk.persisted.get("spaces")).toEqual([
      { ...original, removed: true },
      other,
    ]);
  });

  it("rolls back a partial transaction when updating active ID fails", async () => {
    const { useSpaces, disk } = await setup();
    disk.set
      .mockImplementationOnce(async (key, value) => {
        disk.data.set(key, value);
      })
      .mockRejectedValueOnce(new Error("active write failed"));
    await expect(
      useSpaces.getState().removeProject(original.id),
    ).rejects.toThrow("active write failed");
    expect(disk.data.get("spaces")).toEqual([original, other]);
    expect(disk.data.get("activeId")).toBe(original.id);
    expect(useSpaces.getState().activeId).toBe(original.id);
  });

  it.each([false, true])(
    "preserves concurrent rename, reorder and selection while the pending save fails=%s",
    async (fail) => {
      const { useSpaces, disk } = await setup();
      let finish!: () => void;
      disk.save.mockImplementationOnce(
        () =>
          new Promise<void>((resolve, reject) => {
            finish = () => (fail ? reject(new Error("disk full")) : resolve());
          }),
      );
      const pending = useSpaces.getState().removeProject(original.id);
      const checked = fail
        ? expect(pending).rejects.toThrow("disk full")
        : pending;
      await vi.waitFor(() => expect(disk.save).toHaveBeenCalledOnce());
      useSpaces.getState().rename(original.id, "Renamed");
      useSpaces.getState().reorder([other.id, original.id]);
      useSpaces.getState().setActive(other.id);
      finish();
      await checked;
      const current = useSpaces.getState();
      expect(current.spaces.map((space) => space.id)).toEqual([
        other.id,
        original.id,
      ]);
      expect(current.spaces[1]).toMatchObject({
        name: "Renamed",
        ...(fail ? {} : { removed: true }),
      });
      expect(current.activeId).toBe(other.id);
      if (fail) expect(current.spaces[1].removed).toBeUndefined();
      await disk.save();
      expect(disk.persisted.get("spaces")).toEqual(current.spaces);
      expect(disk.persisted.get("activeId")).toBe(current.activeId);
    },
  );

  it("rejects damaged project data, allows repaired reload, and never writes defaults over the unread data", async () => {
    const { persistence, disk } = await setup();
    disk.data.set("spaces", { corrupted: true });
    await expect(persistence.loadAll()).rejects.toThrow(
      "Invalid project metadata",
    );
    expect(disk.set).not.toHaveBeenCalled();
    disk.reload.mockImplementationOnce(async () => {
      disk.data.set("spaces", [original]);
    });
    await expect(persistence.loadAll(true)).resolves.toMatchObject({
      spaces: [original],
    });
    expect(disk.reload).toHaveBeenCalledOnce();
  });
});
