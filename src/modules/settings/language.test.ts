import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  values: new Map<string, unknown>(),
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
    save() {
      return Promise.resolve();
    }
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
  loadPreferences,
  onPreferencesChange,
  setUiLanguage,
  setMemoryEnabled,
} from "./store";

describe("persisted interface language", () => {
  beforeEach(() => {
    mocks.values.clear();
    mocks.emit.mockClear();
  });

  it("persists memory choices and projects local and cross-window changes", async () => {
    expect((await loadPreferences()).memoryEnabled).toBe(false);
    mocks.values.set("memoryEnabled", "true");
    expect((await loadPreferences()).memoryEnabled).toBe(false);
    const receive = vi.fn();
    const unsubscribe = await onPreferencesChange(receive);
    await setMemoryEnabled(true);
    expect((await loadPreferences()).memoryEnabled).toBe(true);
    expect(mocks.emit).toHaveBeenLastCalledWith("rcode://prefs-changed", {
      key: "memoryEnabled",
      value: true,
    });
    mocks.local?.("memoryEnabled", true);
    mocks.event?.({ payload: { key: "memoryEnabled", value: false } });
    expect(receive.mock.calls).toEqual([
      ["memoryEnabled", true],
      ["memoryEnabled", false],
    ]);
    unsubscribe();
  });

  it("migrates old or unsupported preferences to system language", async () => {
    expect((await loadPreferences()).uiLanguage).toBe("system");
    mocks.values.set("uiLanguage", "unsupported");
    expect((await loadPreferences()).uiLanguage).toBe("system");
  });

  it("persists and broadcasts each supported choice for other windows", async () => {
    for (const language of ["zh-CN", "en-US", "system"] as const) {
      await setUiLanguage(language);
      expect((await loadPreferences()).uiLanguage).toBe(language);
      expect(mocks.emit).toHaveBeenLastCalledWith("rcode://prefs-changed", {
        key: "uiLanguage",
        value: language,
      });
    }
  });

  it("delivers language changes from local writes and another window", async () => {
    const receive = vi.fn();
    const unsubscribe = await onPreferencesChange(receive);
    mocks.local?.("uiLanguage", "zh-CN");
    mocks.event?.({ payload: { key: "uiLanguage", value: "en-US" } });
    expect(receive.mock.calls).toEqual([
      ["uiLanguage", "zh-CN"],
      ["uiLanguage", "en-US"],
    ]);
    unsubscribe();
  });
});
