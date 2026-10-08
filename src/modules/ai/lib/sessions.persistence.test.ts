import { beforeEach, describe, expect, it, vi } from "vitest";
import type { UIMessage } from "@ai-sdk/react";

const pluginStore = vi.hoisted(() => {
  const instances: {
    data: Map<string, unknown>;
    get: ReturnType<typeof vi.fn>;
    set: ReturnType<typeof vi.fn>;
    delete: ReturnType<typeof vi.fn>;
    entries: ReturnType<typeof vi.fn>;
    save: ReturnType<typeof vi.fn>;
  }[] = [];
  class LazyStore {
    data = new Map<string, unknown>();
    get = vi.fn(async (key: string) => this.data.get(key));
    set = vi.fn(async (key: string, value: unknown) => {
      this.data.set(key, value);
    });
    delete = vi.fn(async (key: string) => {
      this.data.delete(key);
    });
    entries = vi.fn(async () => [...this.data.entries()]);
    save = vi.fn(async () => {});
    reload = vi.fn(async () => {});
    constructor() {
      instances.push(this);
    }
  }
  return { instances, LazyStore };
});

vi.mock("@/lib/storage", () => ({
  LazyStore: pluginStore.LazyStore,
}));

function store() {
  return pluginStore.instances[pluginStore.instances.length - 1];
}

async function reload() {
  vi.resetModules();
  return await import("./sessions");
}

describe("session persistence", () => {
  beforeEach(() => {
    pluginStore.instances.length = 0;
  });

  it("reads sessions and active id, tolerating an empty store", async () => {
    const mod = await reload();
    await expect(mod.loadAll()).resolves.toEqual({
      sessions: [],
      activeId: null,
    });

    store().data.set("sessions", [
      { id: "s-1", title: "T", createdAt: 1, updatedAt: 2 },
    ]);
    store().data.set("activeId", "s-1");

    await expect(mod.loadAll()).resolves.toEqual({
      sessions: [{ id: "s-1", title: "T", createdAt: 1, updatedAt: 2 }],
      activeId: "s-1",
    });
  });

  it("writes the session list and the active id under fixed keys", async () => {
    const mod = await reload();
    const metas = [{ id: "s-9", title: "X", createdAt: 0, updatedAt: 0 }];

    await mod.saveSessionsList(metas);
    expect(store().data.get("sessions")).toEqual(metas);

    await mod.saveActiveId("s-9");
    expect(store().data.get("activeId")).toBe("s-9");
  });

  it("namespaces per-session messages and returns null when missing", async () => {
    const mod = await reload();
    const messages = [{ id: "m1", role: "user", parts: [] }] as UIMessage[];

    await expect(mod.loadMessages("s-1")).resolves.toBeNull();

    await mod.saveMessages("s-1", messages);
    expect(store().data.get("messages:s-1")).toEqual(messages);
    await expect(mod.loadMessages("s-1")).resolves.toEqual(messages);
  });

  it("deletes only the targeted session's messages", async () => {
    const mod = await reload();
    await mod.saveMessages("s-1", [{ id: "a", role: "user", parts: [] }]);
    await mod.saveMessages("s-2", [{ id: "b", role: "user", parts: [] }]);

    await mod.deleteSessionData("s-1");

    expect(store().data.has("messages:s-1")).toBe(false);
    expect(store().data.has("messages:s-2")).toBe(true);
  });

  it("generates prefixed unique ids", async () => {
    const mod = await reload();
    const a = mod.newSessionId();
    const b = mod.newSessionId();
    expect(a.startsWith("s-")).toBe(true);
    expect(a).not.toBe(b);
  });

  it("flushes archive index writes and propagates storage failures", async () => {
    const mod = await reload();
    const sessions = [
      {
        id: "archived",
        title: "Old",
        archived: true,
        createdAt: 1,
        updatedAt: 2,
      },
    ];
    await mod.saveSessionsListAndFlush(sessions);
    expect(store().data.get("sessions")).toEqual(sessions);
    expect(store().save).toHaveBeenCalledOnce();
    store().save.mockRejectedValueOnce(new Error("disk failed"));
    await expect(mod.saveSessionsListAndFlush([])).rejects.toThrow(
      "disk failed",
    );
  });
  it("flushes batch deletion without touching unrelated messages or active ID", async () => {
    const mod = await reload();
    store().data.set("messages:a", []);
    store().data.set("messages:b", []);
    store().data.set("messages:active", [1]);
    store().data.set("activeId", "active");
    await mod.deleteArchivedSessionData(["a", "b", "missing"]);
    expect(store().data.get("messages:active")).toEqual([1]);
    expect(store().data.get("activeId")).toBe("active");
    expect(store().data.has("messages:a")).toBe(false);
    expect(store().data.has("messages:b")).toBe(false);
    expect(store().save).toHaveBeenCalledOnce();
  });

  it("rejects malformed metadata and messages without clearing or overwriting them", async () => {
    const mod = await reload();
    store().data.set("sessions", { damaged: true });
    await expect(mod.loadAll()).rejects.toThrow("Invalid session metadata");
    store().data.set("messages:s-1", { damaged: true });
    await expect(mod.loadMessages("s-1")).rejects.toThrow(
      "Invalid conversation messages",
    );
    expect(store().set).not.toHaveBeenCalled();
    expect(store().save).not.toHaveBeenCalled();
    expect(store().data.get("messages:s-1")).toEqual({ damaged: true });
  });

  it("reloads repaired disk data on retry instead of accepting a stale in-memory store", async () => {
    const mod = await reload();
    store().data.set("sessions", { damaged: true });
    await expect(mod.loadAll()).rejects.toThrow();
    (
      store() as unknown as { reload: ReturnType<typeof vi.fn> }
    ).reload.mockImplementationOnce(async () => {
      store().data.set("sessions", [
        { id: "restored", title: "T", createdAt: 1, updatedAt: 2 },
      ]);
    });
    await expect(mod.loadAll(true)).resolves.toEqual({
      sessions: [{ id: "restored", title: "T", createdAt: 1, updatedAt: 2 }],
      activeId: null,
    });
  });
});
