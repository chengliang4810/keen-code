import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

function snapshot(fetchedAt = Date.now()) {
  return {
    schemaVersion: 1,
    fetchedAt,
    providers: [
      { id: "test", models: [{ id: "test-model", contextLimit: 256_000 }] },
    ],
  };
}

beforeEach(() => {
  vi.resetModules();
  invoke.mockReset();
});

describe("catalog startup and local configuration", () => {
  it("downloads on first startup, coalesces concurrent startup calls and performs no configuration requests", async () => {
    invoke.mockResolvedValueOnce(null).mockResolvedValueOnce(snapshot());
    const { refreshModelCatalog, useModelCatalogStore } = await import(
      "@/modules/ai/lib/modelCatalogState"
    );
    const first = refreshModelCatalog();
    expect(refreshModelCatalog()).toBe(first);
    await first;
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "model_catalog_load",
      "model_catalog_refresh",
    ]);
    const { recommendedModelConfig } = await import("@/modules/ai/config");
    for (let i = 0; i < 10; i++)
      expect(recommendedModelConfig("test-model").contextLimit).toBe(256_000);
    await refreshModelCatalog();
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(useModelCatalogStore.getState()).toMatchObject({
      loading: false,
      error: "",
    });
  });

  it("loads fresh local information without downloading again", async () => {
    invoke.mockResolvedValueOnce(snapshot());
    const { refreshModelCatalog } = await import(
      "@/modules/ai/lib/modelCatalogState"
    );
    await refreshModelCatalog();
    expect(invoke).toHaveBeenCalledExactlyOnceWith("model_catalog_load");
  });

  it("keeps stale cached information when a full update fails and lets an explicit update retry", async () => {
    invoke
      .mockResolvedValueOnce(snapshot(1))
      .mockRejectedValueOnce(new Error("offline"));
    const { refreshModelCatalog, useModelCatalogStore } = await import(
      "@/modules/ai/lib/modelCatalogState"
    );
    await refreshModelCatalog();
    const { recommendedModelConfig } = await import("@/modules/ai/config");
    expect(recommendedModelConfig("test-model").contextLimit).toBe(256_000);
    expect(useModelCatalogStore.getState().error).toContain("offline");
    invoke.mockResolvedValueOnce(snapshot());
    await refreshModelCatalog(true);
    expect(invoke).toHaveBeenLastCalledWith("model_catalog_refresh", {
      force: true,
    });
    expect(useModelCatalogStore.getState().error).toBe("");
  });

  it("recovers from a corrupt local file through a validated full download", async () => {
    invoke
      .mockRejectedValueOnce(new Error("corrupt cache"))
      .mockResolvedValueOnce(snapshot());
    const { refreshModelCatalog, useModelCatalogStore } = await import(
      "@/modules/ai/lib/modelCatalogState"
    );
    await refreshModelCatalog();
    expect(useModelCatalogStore.getState()).toMatchObject({
      fetchedAt: expect.any(Number),
      error: "",
    });
  });
});
