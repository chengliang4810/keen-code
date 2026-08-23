/**
 * Theme preference + resolved light/dark for the document.
 * Preference is durable (`system` | `light` | `dark`); DOM always gets a
 * concrete `data-theme="light|dark"`. Default preference is follow system.
 */

export type Theme = "dark" | "light";
/** User-facing choice including follow-OS. */
export type ThemePreference = "system" | Theme;

export const THEME_STORAGE_KEY = "keencode.theme";
/** Fallback when OS scheme cannot be read (tests / SSR). */
export const DEFAULT_RESOLVED_THEME: Theme = "dark";
/** New installs / empty storage → follow system. */
export const DEFAULT_THEME_PREFERENCE: ThemePreference = "system";

export function isTheme(value: unknown): value is Theme {
  return value === "dark" || value === "light";
}

export function isThemePreference(value: unknown): value is ThemePreference {
  return value === "system" || isTheme(value);
}

/** 解析当前主题偏好；仅缺失值使用首次启动默认值。 */
export function parseThemePreference(raw: unknown): ThemePreference {
  if (raw === null) return DEFAULT_THEME_PREFERENCE;
  if (typeof raw === "string" && isThemePreference(raw)) return raw;
  throw new Error("主题偏好格式无效");
}

/** Read OS light/dark. Safe outside the browser. */
export function getSystemTheme(
  matchMedia: ((query: string) => MediaQueryList) | null = typeof window !==
  "undefined"
    ? window.matchMedia.bind(window)
    : null,
): Theme {
  try {
    if (!matchMedia) return DEFAULT_RESOLVED_THEME;
    return matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark"
      : "light";
  } catch {
    return DEFAULT_RESOLVED_THEME;
  }
}

/** Map preference → concrete theme applied to the document. */
export function resolveTheme(
  preference: ThemePreference,
  systemTheme: Theme = getSystemTheme(),
): Theme {
  if (preference === "system") return systemTheme;
  return preference;
}

export function toggleTheme(current: Theme): Theme {
  return current === "dark" ? "light" : "dark";
}

/** Apply theme to documentElement (data-theme attribute).
 *
 * Adds `.theme-switching` for one frame: app.css kills every transition/
 * animation under it so light↔dark snaps instead of smearing.
 */
export function applyThemeToDocument(
  theme: Theme,
  root: HTMLElement = document.documentElement,
): void {
  root.setAttribute("data-theme", theme);
  root.classList.add("theme-switching");
  if (typeof requestAnimationFrame === "function") {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => root.classList.remove("theme-switching"));
    });
  } else {
    root.classList.remove("theme-switching");
  }
}

/**
 * Sync Tauri / macOS native chrome (NSAppearance + vibrancy) with app theme.
 * Without this, light UI still sits on dark Sidebar vibrancy → dirty gray rail + black edges.
 *
 * Pass `null` to **follow the OS** (required for live system switching — locking
 * light/dark freezes `prefers-color-scheme` inside the WebView).
 * No-op outside Tauri.
 */
export async function applyNativeWindowTheme(
  theme: Theme | null,
): Promise<void> {
  try {
    const isTauri =
      typeof window !== "undefined" &&
      ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);
    if (!isTauri) return;
    const { setTheme } = await import("@tauri-apps/api/app");
    // Tauri: null/undefined = follow system theme
    await setTheme(theme);
  } catch {
    /* Native theme sync failed; the CSS theme remains authoritative. */
  }
}

/**
 * Apply preference end-to-end: unlock/lock native chrome, resolve system if
 * needed, write `data-theme`. When switching **to** system, native is unlocked
 * first so matchMedia reflects the real OS scheme.
 */
export async function applyThemePreference(
  preference: ThemePreference,
  options?: {
    /** Called with the concrete theme after resolve (for React state). */
    onResolved?: (resolved: Theme, system: Theme) => void;
  },
): Promise<Theme> {
  if (preference === "system") {
    // Unlock WebView appearance so prefers-color-scheme tracks the OS.
    await applyNativeWindowTheme(null);
    // matchMedia can lag one frame after native unlock — re-read twice.
    let system = getSystemTheme();
    if (typeof requestAnimationFrame === "function") {
      await new Promise<void>((r) => {
        requestAnimationFrame(() => r());
      });
      system = getSystemTheme();
    }
    applyThemeToDocument(system);
    options?.onResolved?.(system, system);
    return system;
  }
  applyThemeToDocument(preference);
  await applyNativeWindowTheme(preference);
  options?.onResolved?.(preference, getSystemTheme());
  return preference;
}

export interface ThemeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** 读取持久化主题偏好（跟随系统、浅色或深色）。 */
export function loadThemePreference(storage: ThemeStorage): ThemePreference {
  return parseThemePreference(storage.getItem(THEME_STORAGE_KEY));
}

/** 校验并持久化主题偏好，包括跟随系统。 */
export function saveThemePreference(
  storage: ThemeStorage,
  preference: ThemePreference,
): void {
  if (!isThemePreference(preference)) {
    throw new Error("主题偏好格式无效");
  }
  storage.setItem(THEME_STORAGE_KEY, preference);
}

/**
 * Subscribe to OS scheme changes. Returns unsubscribe.
 * No-op when matchMedia is unavailable.
 */
export function subscribeSystemTheme(
  onChange: (systemTheme: Theme) => void,
  matchMedia: ((query: string) => MediaQueryList) | null = typeof window !==
  "undefined"
    ? window.matchMedia.bind(window)
    : null,
): () => void {
  if (!matchMedia) return () => {};
  let mql: MediaQueryList;
  try {
    mql = matchMedia("(prefers-color-scheme: dark)");
  } catch {
    return () => {};
  }
  const handler = () => {
    onChange(mql.matches ? "dark" : "light");
  };
  if (typeof mql.addEventListener === "function") {
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }
  // 部分 WebView 仅提供 MediaQueryList 的 listener 方法。
  const mqlWithListenerMethods = mql as MediaQueryList & {
    addListener?: (cb: () => void) => void;
    removeListener?: (cb: () => void) => void;
  };
  mqlWithListenerMethods.addListener?.(handler);
  return () => mqlWithListenerMethods.removeListener?.(handler);
}
