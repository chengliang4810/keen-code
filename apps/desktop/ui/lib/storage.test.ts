import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  invoke: vi.fn(),
  paths: [] as string[],
  get: vi.fn(),
  init: vi.fn(),
  reload: vi.fn(),
  close: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: api.invoke }));
vi.mock("@tauri-apps/plugin-store", () => ({
  LazyStore: class {
    constructor(path: string) {
      api.paths.push(path);
    }
    get = api.get;
    init = api.init;
    reload = api.reload;
    close = api.close;
  },
}));

describe("RCode storage paths", () => {
  it("invalidates an already hydrated handle when explicit reload finds corrupt data", async () => {
    const { LazyStore } = await import("@/lib/storage");
    const disk = new LazyStore("agents");
    api.invoke.mockResolvedValueOnce({
      stores: { agents: "/home/user/.rcode/agents/state.json" },
    });
    await disk.get("subagents");
    api.reload.mockRejectedValueOnce(new Error("invalid JSON"));
    api.invoke.mockResolvedValueOnce(false);
    await expect(disk.reload()).rejects.toThrow("invalid JSON");
    expect(api.close).toHaveBeenCalledTimes(1);
    await disk.get("subagents");
    expect(api.paths).toHaveLength(2);
  });
  beforeEach(() => {
    vi.resetModules();
    api.invoke.mockReset();
    api.paths.length = 0;
    api.get.mockReset().mockResolvedValue("saved");
    api.init.mockReset().mockResolvedValue(undefined);
    api.reload.mockReset().mockResolvedValue(undefined);
    api.close.mockReset().mockResolvedValue(undefined);
  });

  it("does no I/O until used and shares path discovery across concurrent stores", async () => {
    const { LazyStore } = await import("@/lib/storage");
    const a = new LazyStore("settings");
    const b = new LazyStore("sessions");
    expect(api.invoke).not.toHaveBeenCalled();
    api.invoke.mockResolvedValue({
      stores: {
        settings: "/home/user/.rcode/config/settings.json",
        sessions: "/home/user/.rcode/sessions/conversations.json",
      },
    });
    expect(await Promise.all([a.get("key"), b.get("key")])).toEqual([
      "saved",
      "saved",
    ]);
    await a.get("again");
    expect(api.invoke).toHaveBeenCalledExactlyOnceWith("storage_paths");
    expect(api.paths).toEqual([
      "/home/user/.rcode/config/settings.json",
      "/home/user/.rcode/sessions/conversations.json",
    ]);
  });

  it("retries path failures without falling back to an old directory", async () => {
    const { LazyStore } = await import("@/lib/storage");
    const disk = new LazyStore("settings");
    api.invoke.mockRejectedValueOnce(new Error("unavailable"));
    await expect(disk.get("key")).rejects.toThrow("unavailable");
    expect(api.paths).toEqual([]);
    api.invoke.mockResolvedValueOnce({
      stores: { settings: "/home/user/.rcode/config/settings.json" },
    });
    await expect(disk.get("key")).resolves.toBe("saved");
    expect(api.invoke).toHaveBeenCalledTimes(2);
  });

  it("closes an unreadable store so exit cannot replace the file with defaults", async () => {
    const { LazyStore } = await import("@/lib/storage");
    const disk = new LazyStore("settings");
    api.invoke
      .mockResolvedValueOnce({
        stores: { settings: "/home/user/.rcode/config/settings.json" },
      })
      .mockResolvedValueOnce(false);
    api.reload.mockRejectedValueOnce(new Error("invalid JSON"));
    await expect(disk.get("key")).rejects.toThrow("invalid JSON");
    expect(api.close).toHaveBeenCalledTimes(1);
    expect(api.get).not.toHaveBeenCalled();
    await expect(disk.get("key")).resolves.toBe("saved");
  });

  it("allows default values only when the native path check confirms absence", async () => {
    const { LazyStore } = await import("@/lib/storage");
    const disk = new LazyStore("settings");
    api.invoke
      .mockResolvedValueOnce({
        stores: { settings: "/home/user/.rcode/config/settings.json" },
      })
      .mockResolvedValueOnce(true);
    api.reload.mockRejectedValueOnce(new Error("not found"));
    await expect(disk.get("key")).resolves.toBe("saved");
    expect(api.invoke).toHaveBeenLastCalledWith("storage_store_missing", {
      id: "settings",
    });
    expect(api.close).not.toHaveBeenCalled();
  });
});
