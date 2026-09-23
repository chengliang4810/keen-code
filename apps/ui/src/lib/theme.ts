/**
 * KeenCode 主题桥接层。
 *
 * 主题状态、持久化、系统跟随、`.dark` 类与防闪脚本由 Appica 官方
 * `ThemeProvider` / `useTheme` 管理（接线见 main.tsx 与 useThemeAppearance）。
 * 本模块只保留 provider 覆盖不到的产品表面：
 * - 把解析后的主题镜像到 `data-theme` 属性与 meta（tokens.css 与终端、
 *   代码预览等组件读取该属性）；`.dark` 类由 provider 维护，这里写入
 *   仅为启动首绘前的幂等镜像。
 * - Tauri / macOS 原生窗口外观同步。
 */

import { syncNativeThemeSurfaces } from "./nativeTheme";

export type Theme = "dark" | "light";
/** User-facing choice including follow-OS. */
export type ThemePreference = "system" | Theme;

/** ThemeProvider 的 storageKey；值为裸字符串 `system|light|dark`。 */
export const THEME_STORAGE_KEY = "keencode.theme";
/** Fallback when OS scheme cannot be read (tests / SSR). */
export const DEFAULT_RESOLVED_THEME: Theme = "dark";
/** New installs / empty storage → ZCode-compatible Zai dark. */
export const DEFAULT_THEME_PREFERENCE: ThemePreference = "dark";

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

export interface ThemeStorage {
  getItem(key: string): string | null;
}

/** 读取持久化主题偏好；写入与解析 system 的权威路径属于 ThemeProvider。 */
export function loadThemePreference(storage: ThemeStorage): ThemePreference {
  return parseThemePreference(storage.getItem(THEME_STORAGE_KEY));
}

/**
 * Mirror the resolved theme onto KeenCode product surfaces.
 *
 * 切换主题的过渡抑制由 ThemeProvider 的 `disableTransitionOnChange` 负责。
 * provider 的语义是在 `<html>` 上保留唯一主题类（`.light` 或 `.dark`），但它
 * 的防闪脚本在纯 CSR 入口下不会被执行、且挂载首轮 effect 被跳过——首帧作用域
 * 必须由本镜像落位；后续每次主题变更 provider effect 与这里写入结果一致（幂等）。
 */
export function applyThemeToDocument(
  theme: Theme,
  root: HTMLElement = document.documentElement,
): void {
  root.setAttribute("data-theme", theme);
  root.classList.toggle("dark", theme === "dark");
  root.classList.toggle("light", theme === "light");
  root.style.colorScheme = theme;

  const ownerDocument = root.ownerDocument;
  if (ownerDocument && root === ownerDocument.documentElement) {
    const syncMeta = (name: "color-scheme" | "theme-color", content: string) => {
      let meta = ownerDocument.querySelector<HTMLMetaElement>(`meta[name="${name}"]`);
      if (!meta) {
        meta = ownerDocument.createElement("meta");
        meta.name = name;
        ownerDocument.head.append(meta);
      }
      meta.content = content;
    };
    syncMeta("color-scheme", theme);
    const background = getComputedStyle(root)
      .getPropertyValue("--bg-main")
      .trim();
    if (background) syncMeta("theme-color", background);
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
  const isTauri =
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);
  if (!isTauri) return;
  try {
    const { setTheme } = await import("@tauri-apps/api/app");
    // Tauri: null/undefined = follow system theme
    await setTheme(theme);
  } catch {
    /* Native appearance sync failed; the CSS theme remains authoritative. */
  }
  await syncNativeThemeSurfaces();
}
