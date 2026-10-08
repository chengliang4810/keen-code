import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/modules/ai/lib/agents";

const api = vi.hoisted(() => ({
  invoke: vi.fn(),
  get: vi.fn(),
  set: vi.fn(),
  save: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: api.invoke }));
vi.mock("@/lib/storage", () => ({
  LazyStore: class {
    get = api.get;
    set = api.set;
    save = api.save;
  },
}));

const role: Agent = {
  id: "custom",
  name: "Custom",
  description: "",
  icon: "spark",
  builtIn: false,
  instructions: "Complete instructions",
};

describe("agent Markdown persistence", () => {
  beforeEach(() => {
    vi.resetModules();
    api.invoke.mockReset();
    api.get.mockReset().mockResolvedValue("custom");
    api.set.mockReset().mockResolvedValue(undefined);
    api.save.mockReset().mockResolvedValue(undefined);
  });

  it("loads custom files independently from the selected role state", async () => {
    api.invoke.mockResolvedValueOnce([role]);
    const { loadAgents, saveCustomAgents } = await import(
      "@/modules/ai/lib/agents"
    );
    const loaded = await loadAgents();
    expect(loaded.activeId).toBe("custom");
    loaded.custom[0] = { ...role, instructions: "Updated instructions" };
    await saveCustomAgents(loaded.custom);
    expect(api.invoke).toHaveBeenLastCalledWith("storage_agents_save", {
      roles: loaded.custom,
      expected: [role],
    });
    expect(api.set).not.toHaveBeenCalled();
  });

  it("keeps the original file snapshot when native saving reports a conflict", async () => {
    api.invoke
      .mockResolvedValueOnce([role])
      .mockRejectedValueOnce(new Error("changed on disk"));
    const { loadAgents, saveCustomAgents } = await import(
      "@/modules/ai/lib/agents"
    );
    await loadAgents();
    const changed = [{ ...role, name: "Updated" }];
    await expect(saveCustomAgents(changed)).rejects.toThrow("changed on disk");
    api.invoke.mockResolvedValueOnce(undefined);
    await saveCustomAgents(changed);
    expect(api.invoke).toHaveBeenLastCalledWith("storage_agents_save", {
      roles: changed,
      expected: [role],
    });
  });
});
