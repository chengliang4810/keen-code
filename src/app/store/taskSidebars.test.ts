import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { defaultTaskSidebar } from "@/app/lib/taskSidebar";

const disk = vi.hoisted(() => ({
  entries: vi.fn(),
  set: vi.fn(),
  save: vi.fn(),
  delete: vi.fn(),
}));
vi.mock("@/lib/storage", () => ({
  LazyStore: class {
    entries = disk.entries;
    set = disk.set;
    save = disk.save;
    delete = disk.delete;
  },
}));

beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  vi.clearAllMocks();
  disk.entries.mockResolvedValue([]);
  disk.set.mockResolvedValue(undefined);
  disk.save.mockResolvedValue(undefined);
  disk.delete.mockResolvedValue(undefined);
});
afterEach(() => {
  vi.useRealTimers();
});

describe("task sidebar persistence", () => {
  it("writes separate conversation records without overwriting either layout", async () => {
    const { useTaskSidebars, flushTaskSidebars } = await import(
      "@/app/store/taskSidebars"
    );
    await useTaskSidebars.getState().init();
    useTaskSidebars
      .getState()
      .update("a", {
        open: false,
        utilityTabs: ["source-control"],
        view: "source-control",
      });
    useTaskSidebars.getState().update("b", { utilityTabs: [], view: "empty" });
    await flushTaskSidebars();
    expect(disk.set).toHaveBeenCalledWith(
      "task:a",
      expect.objectContaining({ open: false, utilityTabs: ["source-control"] }),
    );
    expect(disk.set).toHaveBeenCalledWith(
      "task:b",
      expect.objectContaining({ open: true, utilityTabs: [] }),
    );
  });
  it("hydrates saved layouts before allowing updates and coalesces drag changes", async () => {
    disk.entries.mockResolvedValue([
      ["task:a", { ...defaultTaskSidebar(), open: false, width: 40 }],
    ]);
    const { useTaskSidebars, flushTaskSidebars } = await import(
      "@/app/store/taskSidebars"
    );
    useTaskSidebars.getState().update("a", { open: true });
    await useTaskSidebars.getState().init();
    expect(useTaskSidebars.getState().byTask.a.open).toBe(false);
    useTaskSidebars.getState().update("a", { width: 41 });
    useTaskSidebars.getState().update("a", { width: 42 });
    await flushTaskSidebars();
    expect(disk.set).toHaveBeenCalledTimes(1);
    expect(disk.set).toHaveBeenCalledWith(
      "task:a",
      expect.objectContaining({ width: 42, open: false }),
    );
  });
  it("does not overwrite unread records after a load failure", async () => {
    disk.entries.mockRejectedValue(new Error("read failed"));
    const { useTaskSidebars, flushTaskSidebars } = await import(
      "@/app/store/taskSidebars"
    );
    await useTaskSidebars.getState().init();
    useTaskSidebars.getState().update("a", { open: false });
    await flushTaskSidebars();
    expect(disk.set).not.toHaveBeenCalled();
    expect(useTaskSidebars.getState().error).toContain("read failed");
  });
  it("removes pending records before a delayed flush can recreate a deleted conversation", async () => {
    const { useTaskSidebars, flushTaskSidebars } = await import(
      "@/app/store/taskSidebars"
    );
    await useTaskSidebars.getState().init();
    useTaskSidebars.getState().update("a", { width: 40 });
    useTaskSidebars.getState().remove("a");
    await flushTaskSidebars();
    expect(disk.set).not.toHaveBeenCalled();
    expect(disk.delete).toHaveBeenCalledWith("task:a");
  });
});
