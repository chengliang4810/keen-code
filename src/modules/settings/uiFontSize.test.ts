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

import {
  DEFAULT_PREFERENCES,
  loadPreferences,
  setUiFontSize,
} from "@/modules/settings/store";
import {
  normalizeUiFontSize,
  UI_FONT_SIZE_DEFAULT,
  UI_FONT_SIZE_MAX,
  UI_FONT_SIZE_MIN,
} from "@/modules/settings/uiFontSize";

describe("interface font size boundaries", () => {
  it("falls back for missing, nonnumeric and non-finite values", () => {
    for (const value of [
      undefined,
      null,
      "16",
      true,
      {},
      [],
      Number.NaN,
      Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY,
    ]) {
      expect(normalizeUiFontSize(value)).toBe(UI_FONT_SIZE_DEFAULT);
    }
  });

  it("rounds finite values and constrains both ends", () => {
    expect(normalizeUiFontSize(15.6)).toBe(16);
    expect(normalizeUiFontSize(12.4)).toBe(12);
    expect(normalizeUiFontSize(-100)).toBe(UI_FONT_SIZE_MIN);
    expect(normalizeUiFontSize(100)).toBe(UI_FONT_SIZE_MAX);
    expect(normalizeUiFontSize(14)).toBe(14);
  });
});

describe("persisted interface font size", () => {
  beforeEach(() => {
    mocks.values.clear();
    mocks.save.mockReset().mockResolvedValue(undefined);
    mocks.emit.mockClear();
    mocks.local = null;
    mocks.event = null;
  });

  it("migrates old stores to 14px and validates persisted data", async () => {
    expect((await loadPreferences()).uiFontSize).toBe(14);
    for (const [stored, expected] of [
      ["18", 14],
      [null, 14],
      [Number.NaN, 14],
      [Number.POSITIVE_INFINITY, 14],
      [16.8, 17],
      [2, 12],
      [50, 20],
    ]) {
      mocks.values.set("uiFontSize", stored);
      expect((await loadPreferences()).uiFontSize).toBe(expected);
    }
  });

  it("saves and broadcasts normalized choices without changing content fonts or zoom", async () => {
    mocks.values.set("editorFontSize", 18);
    mocks.values.set("terminalFontSize", 17);
    mocks.values.set("zoomLevel", 1.5);
    for (const [requested, expected] of [
      [12, 12],
      [14, 14],
      [16.8, 17],
      [50, 20],
    ]) {
      await setUiFontSize(requested);
      const restored = await loadPreferences();
      expect(restored.uiFontSize).toBe(expected);
      expect(restored.editorFontSize).toBe(18);
      expect(restored.terminalFontSize).toBe(17);
      expect(restored.zoomLevel).toBe(1.5);
      expect(mocks.emit).toHaveBeenLastCalledWith("rcode://prefs-changed", {
        key: "uiFontSize",
        value: expected,
      });
    }
    expect(mocks.save).toHaveBeenCalledTimes(4);
  });

  it("surfaces save failure and does not broadcast a successful write", async () => {
    mocks.save.mockRejectedValueOnce(new Error("disk unavailable"));
    await expect(setUiFontSize(16)).rejects.toThrow("disk unavailable");
    expect(mocks.emit).not.toHaveBeenCalled();
    await setUiFontSize(16);
    expect((await loadPreferences()).uiFontSize).toBe(16);
  });

  it("hydrates and normalizes local and cross-window changes in the shared preferences store", async () => {
    mocks.values.set("uiFontSize", 16);
    const { usePreferencesStore } = await import(
      "@/modules/settings/preferences"
    );
    await usePreferencesStore.getState().init();
    expect(usePreferencesStore.getState().uiFontSize).toBe(16);

    mocks.local?.("uiFontSize", 18.6);
    expect(usePreferencesStore.getState().uiFontSize).toBe(19);
    mocks.event?.({ payload: { key: "uiFontSize", value: 99 } });
    expect(usePreferencesStore.getState().uiFontSize).toBe(20);
    mocks.event?.({ payload: { key: "uiFontSize", value: "18" } });
    expect(usePreferencesStore.getState().uiFontSize).toBe(14);
    expect(usePreferencesStore.getState().editorFontSize).toBe(
      DEFAULT_PREFERENCES.editorFontSize,
    );
    expect(usePreferencesStore.getState().terminalFontSize).toBe(
      DEFAULT_PREFERENCES.terminalFontSize,
    );
  });
});
