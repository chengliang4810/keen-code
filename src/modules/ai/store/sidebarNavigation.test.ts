import { beforeEach, describe, expect, it, vi } from "vitest";
import { normalizeSidebarNavigation } from "@/modules/ai/lib/sidebarNavigation";

const disk = vi.hoisted(() => ({
  load: vi.fn(),
  get: vi.fn(),
  set: vi.fn(),
  save: vi.fn(),
}));
vi.mock("@/lib/storage", () => ({
  LazyStore: class {
    // 对齐真实 LazyStore：加载失败的 Promise 会一直保留在该实例中。
    private loaded: Promise<void> | undefined;
    async get(key: string) {
      this.loaded ??= disk.load();
      await this.loaded;
      return disk.get(key);
    }
    set = disk.set;
    save = disk.save;
  },
}));

beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  disk.load.mockResolvedValue(undefined);
  disk.get.mockResolvedValue(undefined);
  disk.set.mockResolvedValue(undefined);
  disk.save.mockResolvedValue(undefined);
});

describe("sidebar navigation persistence", () => {
  it("serializes sort selection with a manual conversation move and restores both", async () => {
    const sessions = [
      { id: "one", title: "One", projectId: "a", createdAt: 1, updatedAt: 1 },
      { id: "two", title: "Two", projectId: "a", createdAt: 1, updatedAt: 2 },
    ];
    let { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    await Promise.all([
      useSidebarNavigation.getState().setConversationSort("manual", sessions),
      useSidebarNavigation
        .getState()
        .moveConversation(sessions, "one", "two", "before"),
    ]);
    const saved = useSidebarNavigation.getState().snapshot;
    expect(saved.conversationSort).toBe("manual");
    expect(saved.conversationOrder).toEqual(["one", "two"]);
    disk.get.mockResolvedValue(saved);
    vi.resetModules();
    ({ useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    ));
    await useSidebarNavigation.getState().init();
    expect(useSidebarNavigation.getState().snapshot).toEqual(saved);
  });
  it("keeps the current sort choice on save failure and retries it", async () => {
    const { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    disk.save.mockRejectedValueOnce(new Error("failed"));
    await useSidebarNavigation.getState().setConversationSort("manual", []);
    expect(
      useSidebarNavigation.getState().snapshot.conversationSort,
    ).toBeUndefined();
    expect(useSidebarNavigation.getState().error).toBe("save");
    await useSidebarNavigation.getState().retry();
    expect(useSidebarNavigation.getState().snapshot.conversationSort).toBe(
      "manual",
    );
  });
  it("serializes project ordering with pin changes and restores order on restart", async () => {
    let { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    await Promise.all([
      useSidebarNavigation.getState().togglePin({ kind: "task", id: "one" }),
      useSidebarNavigation
        .getState()
        .moveProject(
          ["a", "b"],
          { kind: "project", id: "a", section: "projects" },
          { kind: "project", id: "b", section: "projects" },
          "after",
        ),
    ]);
    const saved = useSidebarNavigation.getState().snapshot;
    expect(saved.projectOrder).toEqual(["b", "a"]);
    expect(saved.pins).toEqual([{ kind: "task", id: "one" }]);
    disk.get.mockResolvedValue(saved);
    vi.resetModules();
    ({ useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    ));
    await useSidebarNavigation.getState().init();
    expect(useSidebarNavigation.getState().snapshot).toEqual(saved);
  });
  it("leaves order unchanged on save failure and retries the move without losing pins", async () => {
    const { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    disk.save.mockRejectedValueOnce(new Error("write failed"));
    await useSidebarNavigation
      .getState()
      .moveProject(
        ["a", "b"],
        { kind: "project", id: "a", section: "projects" },
        { kind: "project", id: "b", section: "projects" },
        "after",
      );
    expect(
      useSidebarNavigation.getState().snapshot.projectOrder,
    ).toBeUndefined();
    expect(useSidebarNavigation.getState().error).toBe("save");
    await useSidebarNavigation.getState().retry();
    expect(useSidebarNavigation.getState().snapshot.projectOrder).toEqual([
      "b",
      "a",
    ]);
  });
  it("does not write no-op or invalid drops", async () => {
    const { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    await useSidebarNavigation
      .getState()
      .moveProject(
        ["a", "b"],
        { kind: "project", id: "a", section: "projects" },
        { kind: "project", id: "b", section: "projects" },
        "before",
      );
    expect(disk.set).not.toHaveBeenCalled();
    expect(useSidebarNavigation.getState().saving).toBe(false);
  });
  it("recreates a poisoned lazy handle so retry performs a fresh load", async () => {
    disk.load.mockRejectedValueOnce(new Error("load failed"));
    const { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    expect(useSidebarNavigation.getState().error).toBe("load");
    expect(disk.get).not.toHaveBeenCalled();
    await useSidebarNavigation.getState().retry();
    expect(disk.load).toHaveBeenCalledTimes(2);
    expect(useSidebarNavigation.getState().hydrated).toBe(true);
    expect(useSidebarNavigation.getState().error).toBeNull();
  });
  it("loads once and restores mixed pins and section states across restart", async () => {
    const snapshot = {
      pins: [
        { kind: "task", id: "one" },
        { kind: "project", id: "a" },
      ],
      collapsed: ["conversations"],
    };
    disk.get.mockResolvedValue(snapshot);
    let { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await Promise.all([
      useSidebarNavigation.getState().init(),
      useSidebarNavigation.getState().init(),
    ]);
    expect(disk.get).toHaveBeenCalledTimes(1);
    expect(useSidebarNavigation.getState().snapshot).toEqual(snapshot);
    await useSidebarNavigation.getState().toggleSection("projects");
    const saved = disk.set.mock.calls[0][1];
    vi.resetModules();
    disk.get.mockResolvedValue(saved);
    ({ useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    ));
    await useSidebarNavigation.getState().init();
    expect(useSidebarNavigation.getState().snapshot).toEqual(
      normalizeSidebarNavigation(saved),
    );
  });
  it("serializes simultaneous updates against the last committed snapshot", async () => {
    const { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    await Promise.all([
      useSidebarNavigation.getState().togglePin({ kind: "project", id: "a" }),
      useSidebarNavigation.getState().togglePin({ kind: "task", id: "one" }),
      useSidebarNavigation.getState().toggleSection("pinned"),
    ]);
    expect(useSidebarNavigation.getState().snapshot).toEqual({
      pins: [
        { kind: "project", id: "a" },
        { kind: "task", id: "one" },
      ],
      collapsed: ["pinned"],
    });
    expect(disk.save).toHaveBeenCalledTimes(3);
    expect(useSidebarNavigation.getState().saving).toBe(false);
  });
  it("protects unread storage after load failure and permits an explicit retry", async () => {
    disk.get.mockRejectedValueOnce(new Error("unavailable"));
    const { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    await useSidebarNavigation
      .getState()
      .togglePin({ kind: "project", id: "a" });
    expect(disk.set).not.toHaveBeenCalled();
    expect(useSidebarNavigation.getState().hydrated).toBe(false);
    expect(useSidebarNavigation.getState().error).toBe("load");
    disk.get.mockResolvedValue({
      pins: [{ kind: "task", id: "old" }],
      collapsed: [],
    });
    await useSidebarNavigation.getState().retry();
    expect(useSidebarNavigation.getState().snapshot.pins).toEqual([
      { kind: "task", id: "old" },
    ]);
    expect(useSidebarNavigation.getState().error).toBeNull();
  });
  it.each(["set", "save"] as const)(
    "does not report a failed %s as successful, and retries the intended action",
    async (method) => {
      const { useSidebarNavigation } = await import(
        "@/modules/ai/store/sidebarNavigation"
      );
      await useSidebarNavigation.getState().init();
      disk[method].mockRejectedValueOnce(new Error("failed"));
      await useSidebarNavigation
        .getState()
        .togglePin({ kind: "project", id: "a" });
      expect(useSidebarNavigation.getState().snapshot.pins).toEqual([]);
      expect(useSidebarNavigation.getState().error).toBe("save");
      expect(useSidebarNavigation.getState().saving).toBe(false);
      await useSidebarNavigation.getState().retry();
      expect(useSidebarNavigation.getState().snapshot.pins).toEqual([
        { kind: "project", id: "a" },
      ]);
      expect(useSidebarNavigation.getState().error).toBeNull();
    },
  );
  it("does not update the displayed list until disk save completes", async () => {
    const { useSidebarNavigation } = await import(
      "@/modules/ai/store/sidebarNavigation"
    );
    await useSidebarNavigation.getState().init();
    let finish!: () => void;
    disk.save.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const saving = useSidebarNavigation
      .getState()
      .togglePin({ kind: "task", id: "one" });
    await vi.waitFor(() => expect(disk.save).toHaveBeenCalled());
    expect(useSidebarNavigation.getState().saving).toBe(true);
    expect(useSidebarNavigation.getState().snapshot.pins).toEqual([]);
    finish();
    await saving;
    expect(useSidebarNavigation.getState().snapshot.pins).toEqual([
      { kind: "task", id: "one" },
    ]);
  });
});
