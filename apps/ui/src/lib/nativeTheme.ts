import { isTauri } from "./tauri";

/** Tauri accepts an RGBA tuple; native surfaces must always receive opaque colors. */
export type NativeThemeColor = [number, number, number, number];

function clampChannel(value: number): number {
  return Math.max(0, Math.min(255, Math.round(value)));
}

function parseChannel(raw: string): number | null {
  const value = raw.trim();
  if (!value) return null;
  const number = Number(value.endsWith("%") ? value.slice(0, -1) : value);
  if (!Number.isFinite(number)) return null;
  return clampChannel(value.endsWith("%") ? (number / 100) * 255 : number);
}

function parseAlpha(raw: string): number | null {
  const value = raw.trim();
  if (!value) return null;
  const number = Number(value.endsWith("%") ? value.slice(0, -1) : value);
  if (!Number.isFinite(number)) return null;
  return clampChannel(value.endsWith("%") ? (number / 100) * 255 : number * 255);
}

/** 解析浏览器计算后的 hex/rgb 颜色，供 Tauri 的严格 RGBA 类型使用。 */
export function parseCssColor(raw: string): NativeThemeColor | null {
  const value = raw.trim().toLowerCase();
  if (value.startsWith("#")) {
    const hex = value.slice(1);
    if (![3, 4, 6, 8].includes(hex.length) || !/^[0-9a-f]+$/.test(hex)) {
      return null;
    }
    const expanded = hex.length <= 4
      ? [...hex].map((digit) => `${digit}${digit}`).join("")
      : hex;
    const channels = [
      Number.parseInt(expanded.slice(0, 2), 16),
      Number.parseInt(expanded.slice(2, 4), 16),
      Number.parseInt(expanded.slice(4, 6), 16),
      expanded.length === 8 ? Number.parseInt(expanded.slice(6, 8), 16) : 255,
    ];
    return channels as NativeThemeColor;
  }

  const match = value.match(/^rgba?\((.*)\)$/);
  if (!match) return null;
  const [channelText, slashAlpha] = match[1]!.split("/");
  const parts = channelText!.trim().split(/[\s,]+/).filter(Boolean);
  const alphaText = slashAlpha?.trim() || (parts.length === 4 ? parts.pop() : undefined);
  if (parts.length !== 3) return null;
  const channels = parts.map(parseChannel);
  if (channels.some((channel) => channel == null)) return null;
  const alpha = alphaText ? parseAlpha(alphaText) : 255;
  if (alpha == null) return null;
  return [channels[0]!, channels[1]!, channels[2]!, alpha];
}

/** 读取当前 KeenCode 应用底色；未完成 DOM/CSS 计算时返回 null。 */
export function readThemeBackgroundColor(
  root: HTMLElement | null =
    typeof document === "undefined" ? null : document.documentElement,
): NativeThemeColor | null {
  if (!root || typeof getComputedStyle !== "function") return null;
  const raw = getComputedStyle(root).getPropertyValue("--bg-app").trim();
  return parseCssColor(raw);
}

/** 同步主窗口和所有内置浏览器子 WebView 的当前主题底色。 */
export async function syncNativeThemeSurfaces(): Promise<void> {
  if (!isTauri()) return;
  const color = readThemeBackgroundColor();
  if (!color) return;

  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setBackgroundColor(color);
  } catch {
    // Native background is a visual fallback; CSS remains authoritative.
  }

  try {
    const { getAllWebviews } = await import("@tauri-apps/api/webview");
    const webviews = await getAllWebviews();
    await Promise.allSettled(
      webviews
        .filter((webview) => webview.label.startsWith("browser-"))
        .map((webview) => webview.setBackgroundColor(color)),
    );
  } catch {
    // A missing optional WebView permission must not block the main surface.
  }
}
