import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  values: new Map<string, unknown>(),
  save: vi.fn().mockResolvedValue(undefined),
  emit: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/lib/storage", () => ({
  LazyStore: class {
    entries() {
      return Promise.resolve([...mocks.values]);
    }
    set(key: string, value: unknown) {
      mocks.values.set(key, value);
      return Promise.resolve();
    }
    save = mocks.save;
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ emit: mocks.emit }));

import { loadPreferences, setCustomEndpoints } from "@/modules/settings/store";

describe("custom provider persistence", () => {
  beforeEach(() => {
    mocks.values.clear();
    mocks.save.mockReset().mockResolvedValue(undefined);
    mocks.emit.mockClear();
    mocks.values.set("openaiCompatibleBaseURL", "http://localhost:1234/v1");
    mocks.values.set("openaiCompatibleModelId", "legacy-model");
    mocks.values.set("openaiCompatibleContextLimit", 32_768);
  });

  it("imports the old custom configuration only when the provider list is absent", async () => {
    expect((await loadPreferences()).customEndpoints).toEqual([
      expect.objectContaining({
        baseURL: "http://localhost:1234/v1",
        modelId: "legacy-model",
        contextLimit: 32_768,
      }),
    ]);
  });

  it("keeps all providers deleted when preferences hydrate again", async () => {
    const imported = (await loadPreferences()).customEndpoints;
    await setCustomEndpoints(imported);
    await setCustomEndpoints([]);

    expect(mocks.save).toHaveBeenCalledTimes(2);
    expect((await loadPreferences()).customEndpoints).toEqual([]);
    expect(mocks.emit).toHaveBeenLastCalledWith("rcode://prefs-changed", {
      key: "customEndpoints",
      value: [],
    });
  });

  it("preserves a saved empty list even when legacy fields remain", async () => {
    mocks.values.set("customEndpoints", []);
    expect((await loadPreferences()).customEndpoints).toEqual([]);
  });

  it("drops built-in selections from default, favorite and recent preferences", async () => {
    const custom = "compat-custom/model";
    mocks.values.set("defaultModelId", "openai:gpt-5.4");
    mocks.values.set("favoriteModelIds", ["openai:gpt-5.4", custom]);
    mocks.values.set("recentModelIds", ["anthropic:claude-sonnet", custom]);

    const restored = await loadPreferences();
    expect(restored.defaultModelId).toBe("compat-");
    expect(restored.favoriteModelIds).toEqual([custom]);
    expect(restored.recentModelIds).toEqual([custom]);
  });
});
