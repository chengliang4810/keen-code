import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_RESOLVED_THEME,
  DEFAULT_THEME_PREFERENCE,
  getSystemTheme,
  loadThemePreference,
  parseThemePreference,
  resolveTheme,
  saveThemePreference,
  subscribeSystemTheme,
  THEME_STORAGE_KEY,
  toggleTheme,
  type ThemeStorage,
} from "./theme";

function memoryStorage(initial: Record<string, string> = {}): ThemeStorage & {
  data: Record<string, string>;
} {
  const data = { ...initial };
  return {
    data,
    getItem(key) {
      return key in data ? data[key]! : null;
    },
    setItem(key, value) {
      data[key] = value;
    },
  };
}

describe("theme preference + resolve", () => {
  it("defaults preference to system", () => {
    expect(DEFAULT_THEME_PREFERENCE).toBe("system");
    expect(parseThemePreference(null)).toBe("system");
    expect(() => parseThemePreference("nope")).toThrow("主题偏好格式无效");
    expect(() => parseThemePreference(undefined)).toThrow("主题偏好格式无效");
    expect(() => parseThemePreference("")).toThrow("主题偏好格式无效");
    expect(parseThemePreference("system")).toBe("system");
  });

  it("keeps explicit light/dark preferences", () => {
    expect(parseThemePreference("light")).toBe("light");
    expect(parseThemePreference("dark")).toBe("dark");
  });

  it("resolves system to the given OS theme", () => {
    expect(resolveTheme("system", "light")).toBe("light");
    expect(resolveTheme("system", "dark")).toBe("dark");
    expect(resolveTheme("light", "dark")).toBe("light");
    expect(resolveTheme("dark", "light")).toBe("dark");
  });

  it("getSystemTheme reads matchMedia when provided", () => {
    const darkMq = {
      matches: true,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    } as unknown as MediaQueryList;
    const lightMq = {
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    } as unknown as MediaQueryList;
    expect(getSystemTheme(() => darkMq)).toBe("dark");
    expect(getSystemTheme(() => lightMq)).toBe("light");
    expect(getSystemTheme(null)).toBe(DEFAULT_RESOLVED_THEME);
  });

  it("toggles dark ↔ light", () => {
    expect(toggleTheme("dark")).toBe("light");
    expect(toggleTheme("light")).toBe("dark");
  });

  it("empty storage loads system preference", () => {
    const storage = memoryStorage();
    expect(loadThemePreference(storage)).toBe("system");
  });

  it("loads an explicit light preference", () => {
    const storage = memoryStorage({ [THEME_STORAGE_KEY]: "light" });
    expect(loadThemePreference(storage)).toBe("light");
  });

  it("已存在的无效值会显式失败", () => {
    for (const raw of ["", "LIGHT", "unknown", " null "]) {
      const storage = memoryStorage({ [THEME_STORAGE_KEY]: raw });
      expect(() => loadThemePreference(storage)).toThrow("主题偏好格式无效");
    }
  });

  it("persists system preference", () => {
    const storage = memoryStorage();
    saveThemePreference(storage, "system");
    expect(storage.getItem(THEME_STORAGE_KEY)).toBe("system");
    expect(loadThemePreference(storage)).toBe("system");
  });

  it("拒绝写入非当前主题值", () => {
    const storage = memoryStorage();
    expect(() =>
      saveThemePreference(storage, "auto" as never),
    ).toThrow("主题偏好格式无效");
    expect(storage.data).toEqual({});
  });

  it("subscribeSystemTheme fires on change", () => {
    const listeners = new Set<() => void>();
    const mql = {
      matches: true,
      addEventListener: (_: string, cb: () => void) => {
        listeners.add(cb);
      },
      removeEventListener: (_: string, cb: () => void) => {
        listeners.delete(cb);
      },
    } as unknown as MediaQueryList;
    const seen: string[] = [];
    const unsub = subscribeSystemTheme((t) => seen.push(t), () => mql);
    // flip
    (mql as { matches: boolean }).matches = false;
    for (const cb of listeners) cb();
    expect(seen).toEqual(["light"]);
    unsub();
    expect(listeners.size).toBe(0);
  });

  it("resolveTheme(system) uses latest system argument (switch-to-system path)", () => {
    // After unlock, caller passes freshly read OS theme — must win over stale state.
    expect(resolveTheme("system", "dark")).toBe("dark");
    expect(resolveTheme("system", "light")).toBe("light");
  });
});
