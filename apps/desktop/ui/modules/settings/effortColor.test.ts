import {
  DEFAULT_EFFORT_COLOR,
  EFFORT_COLORS,
  type EffortColor,
} from "@/modules/settings/effortColor";
import {
  DEFAULT_PREFERENCES,
  loadPreferences,
  setEffortColor,
} from "@/modules/settings/store";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  values: new Map<string, unknown>(),
  save: vi.fn().mockResolvedValue(undefined),
  emit: vi.fn(),
  local: null as null | ((key: string, value: unknown) => void),
  event: null as
    | null
    | ((event: { payload: { key: string; value: unknown } }) => void),
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
    onChange(cb: typeof mocks.local) {
      mocks.local = cb;
      return Promise.resolve(vi.fn());
    }
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  emit: mocks.emit,
  listen: vi.fn((_name: string, cb: typeof mocks.event) => {
    mocks.event = cb;
    return Promise.resolve(vi.fn());
  }),
}));

describe("reasoning effort color preference", () => {
  beforeEach(() => {
    mocks.values.clear();
    mocks.save.mockReset().mockResolvedValue(undefined);
    mocks.emit.mockClear();
    mocks.local = null;
    mocks.event = null;
  });

  it("uses purple for old settings and rejects malformed persisted choices", async () => {
    expect((await loadPreferences()).effortColor).toBe(DEFAULT_EFFORT_COLOR);
    for (const value of [null, "", "Purple", "red", 1, {}, ["silver"]]) {
      mocks.values.set("effortColor", value);
      expect((await loadPreferences()).effortColor).toBe(DEFAULT_EFFORT_COLOR);
    }
  });

  it("persists every preset and broadcasts it without changing theme or model", async () => {
    mocks.values.set("themeId", "kanagawa");
    for (const color of EFFORT_COLORS) {
      await setEffortColor(color);
      const restored = await loadPreferences();
      expect(restored.effortColor).toBe(color);
      expect(restored.themeId).toBe("kanagawa");
      expect(restored.defaultModelId).toBe(DEFAULT_PREFERENCES.defaultModelId);
      expect(mocks.emit).toHaveBeenLastCalledWith("rcode://prefs-changed", {
        key: "effortColor",
        value: color,
      });
    }
    await setEffortColor("unknown" as EffortColor);
    expect((await loadPreferences()).effortColor).toBe(DEFAULT_EFFORT_COLOR);
    expect(mocks.save).toHaveBeenCalledTimes(EFFORT_COLORS.length + 1);
  });

  it("surfaces save failures and does not broadcast a successful write", async () => {
    mocks.save.mockRejectedValueOnce(new Error("disk unavailable"));
    await expect(setEffortColor("silver")).rejects.toThrow("disk unavailable");
    expect(mocks.emit).not.toHaveBeenCalled();
    await setEffortColor("azure");
    expect((await loadPreferences()).effortColor).toBe("azure");
  });

  it("restores the selection and validates local and cross-window changes", async () => {
    mocks.values.set("effortColor", "spectrum");
    const { usePreferencesStore } = await import(
      "@/modules/settings/preferences"
    );
    await usePreferencesStore.getState().init();
    expect(usePreferencesStore.getState().effortColor).toBe("spectrum");
    mocks.local?.("effortColor", "terracotta");
    expect(usePreferencesStore.getState().effortColor).toBe("terracotta");
    mocks.event?.({ payload: { key: "effortColor", value: "silver" } });
    expect(usePreferencesStore.getState().effortColor).toBe("silver");
    mocks.event?.({ payload: { key: "effortColor", value: "unknown" } });
    expect(usePreferencesStore.getState().effortColor).toBe(
      DEFAULT_EFFORT_COLOR,
    );
    expect(usePreferencesStore.getState().themeId).toBe(
      DEFAULT_PREFERENCES.themeId,
    );
  });
});
