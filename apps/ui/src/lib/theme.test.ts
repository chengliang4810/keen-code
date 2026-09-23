import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_RESOLVED_THEME,
  DEFAULT_THEME_PREFERENCE,
  applyThemeToDocument,
  getSystemTheme,
  loadThemePreference,
  parseThemePreference,
  resolveTheme,
  THEME_STORAGE_KEY,
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
  };
}

function fakeRoot() {
  const attributes = new Map<string, string>();
  const classes = new Set<string>();
  const root = {
    setAttribute(name: string, value: string) {
      attributes.set(name, value);
    },
    classList: {
      add(name: string) {
        classes.add(name);
      },
      remove(name: string) {
        classes.delete(name);
      },
      toggle(name: string, force?: boolean) {
        const next = force ?? !classes.has(name);
        if (next) classes.add(name);
        else classes.delete(name);
        return next;
      },
      contains(name: string) {
        return classes.has(name);
      },
    },
    style: {} as Record<string, string>,
  } as unknown as HTMLElement;
  return { attributes, classes, root };
}

describe("theme preference + resolve", () => {
  it("defaults preference to Zai dark", () => {
    expect(DEFAULT_THEME_PREFERENCE).toBe("dark");
    expect(parseThemePreference(null)).toBe("dark");
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

  it("empty storage loads the Zai dark preference", () => {
    const storage = memoryStorage();
    expect(loadThemePreference(storage)).toBe("dark");
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
});

describe("theme chrome mirror (KeenCode 产品表面)", () => {
  it("同步 data-theme 属性、.light/.dark 类与 color-scheme", () => {
    const { attributes, classes, root } = fakeRoot();

    applyThemeToDocument("dark", root);
    expect(attributes.get("data-theme")).toBe("dark");
    expect(classes.has("dark")).toBe(true);
    expect(classes.has("light")).toBe(false);
    expect(root.style.colorScheme).toBe("dark");

    applyThemeToDocument("light", root);
    expect(attributes.get("data-theme")).toBe("light");
    expect(classes.has("dark")).toBe(false);
    expect(classes.has("light")).toBe(true);
    expect(root.style.colorScheme).toBe("light");
  });

  it("不再保留自定义的 theme-zai / theme-switching 作用域", () => {
    const { classes, root } = fakeRoot();
    applyThemeToDocument("dark", root);
    expect(classes.has("theme-zai-dark")).toBe(false);
    expect(classes.has("theme-zai-light")).toBe(false);
    expect(classes.has("theme-switching")).toBe(false);
  });
});

describe("Appica ThemeProvider 接线", () => {
  const main = readFileSync(new URL("../main.tsx", import.meta.url), "utf8");
  const hook = readFileSync(
    new URL("../hooks/useThemeAppearance.ts", import.meta.url),
    "utf8",
  );

  it("main.tsx 用官方 ThemeProvider 持有主题状态", () => {
    expect(main).toContain(
      'import { ThemeProvider } from "@appica/ui-react/providers/theme-provider"',
    );
    expect(main).toContain("<ThemeProvider");
    expect(main).toContain("storageKey={THEME_STORAGE_KEY}");
    expect(main).toContain("defaultTheme={DEFAULT_THEME_PREFERENCE}");
    expect(main).toContain("enableSystem");
    expect(main).toContain("disableTransitionOnChange");
  });

  it("useThemeAppearance 通过官方 useTheme 读写主题", () => {
    expect(hook).toContain(
      'import { useTheme } from "@appica/ui-react/hooks/use-theme"',
    );
    expect(hook).toContain("useTheme()");
    expect(hook).toContain("setTheme(next)");
  });

  it("自定义状态实现不复活：持久化、系统订阅与切换编排归 provider", () => {
    expect(hook).not.toContain("subscribeSystemTheme");
    expect(hook).not.toContain("loadThemePreference");
    expect(hook).not.toContain("saveThemePreference");
    expect(hook).not.toContain("applyThemePreference");
    expect(main).not.toContain("theme-switching");
  });
});
