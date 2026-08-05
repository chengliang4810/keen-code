/**
 * Layout preferences: sidebar width, aside width, aside collapsed default.
 * Durable key in localStorage (App config later).
 */

export const LAYOUT_STORAGE_KEY = "keencode.layout";

export interface LayoutPrefs {
  sidebarWidth: number;
  asideWidth: number;
  /** Right pane defaults collapsed per §17.1 / autoplan Design D7. */
  asideCollapsed: boolean;
  /** Left project rail collapsed (Codex-style). */
  sidebarCollapsed: boolean;
}

export const DEFAULT_LAYOUT: LayoutPrefs = {
  sidebarWidth: 260,
  asideWidth: 360,
  /** Right resource pane starts closed; open via top-bar files icon. */
  asideCollapsed: true,
  /** Left session rail starts open; can fully hide via top-bar panel icon. */
  sidebarCollapsed: false,
};

export const ASIDE_WIDTH_MIN = 240;
export const ASIDE_WIDTH_MAX = 720;

export function clampAsideWidth(w: number): number {
  if (!Number.isFinite(w)) return DEFAULT_LAYOUT.asideWidth;
  return Math.min(ASIDE_WIDTH_MAX, Math.max(ASIDE_WIDTH_MIN, Math.round(w)));
}

/** 严格解析当前唯一的布局持久化结构。 */
export function parseLayout(raw: unknown): LayoutPrefs {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("布局配置必须是对象");
  }
  const o = raw as Record<string, unknown>;
  const keys = Object.keys(o).sort();
  const expectedKeys = ["asideWidth", "sidebarCollapsed", "sidebarWidth"];
  if (
    keys.length !== expectedKeys.length ||
    keys.some((key, index) => key !== expectedKeys[index])
  ) {
    throw new Error("布局配置字段不完整或包含未知字段");
  }
  if (
    typeof o.sidebarWidth !== "number" ||
    !Number.isFinite(o.sidebarWidth) ||
    o.sidebarWidth <= 0
  ) {
    throw new Error("侧栏宽度无效");
  }
  if (
    typeof o.asideWidth !== "number" ||
    !Number.isFinite(o.asideWidth) ||
    o.asideWidth < ASIDE_WIDTH_MIN ||
    o.asideWidth > ASIDE_WIDTH_MAX
  ) {
    throw new Error("资源栏宽度无效");
  }
  if (typeof o.sidebarCollapsed !== "boolean") {
    throw new Error("侧栏折叠状态无效");
  }
  return {
    sidebarWidth: o.sidebarWidth,
    asideWidth: o.asideWidth,
    // Cold start always closed; open state is session-only (not restored).
    asideCollapsed: DEFAULT_LAYOUT.asideCollapsed,
    sidebarCollapsed: o.sidebarCollapsed,
  };
}

/** 读取持久化布局；仅存储键缺失时使用首次启动默认值。 */
export function loadLayout(storage: {
  getItem(k: string): string | null;
}): LayoutPrefs {
  const raw = storage.getItem(LAYOUT_STORAGE_KEY);
  if (raw === null) return { ...DEFAULT_LAYOUT };
  if (!raw.trim()) throw new Error("布局配置不能为空");
  return parseLayout(JSON.parse(raw));
}

/** 校验并持久化当前唯一的布局结构。 */
export function saveLayout(
  storage: { setItem(k: string, v: string): void },
  layout: LayoutPrefs,
): void {
  const persistedLayout = {
    sidebarWidth: layout.sidebarWidth,
    asideWidth: layout.asideWidth,
    sidebarCollapsed: layout.sidebarCollapsed,
  };
  parseLayout(persistedLayout);
  storage.setItem(LAYOUT_STORAGE_KEY, JSON.stringify(persistedLayout));
}
