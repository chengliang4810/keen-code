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
  /** Whether the left project rail is collapsed. */
  sidebarCollapsed: boolean;
}

export const DEFAULT_LAYOUT: LayoutPrefs = {
  // ZCode 桌面侧栏默认与最小宽度均为 264px；用户拖动后的宽度继续持久化。
  sidebarWidth: 264,
  asideWidth: 360,
  /** Right resource pane starts closed; open via top-bar files icon. */
  asideCollapsed: true,
  /** Left session rail starts open; can fully hide via top-bar panel icon. */
  sidebarCollapsed: false,
};

export const SIDEBAR_WIDTH_MIN = 264;
/**
 * 侧栏不再使用固定像素上限；运行时上限由工作区可用宽度的 50% 决定。
 * Infinity 仅作为无 viewport/持久化解析时的开放上限，实际拖动仍传入
 * getSidebarWidthMax() 返回的有限值。
 */
export const SIDEBAR_WIDTH_MAX = Number.POSITIVE_INFINITY;
export const MAIN_WIDTH_MIN = 460;
export const ASIDE_WIDTH_MIN = 240;
export const ASIDE_WIDTH_MAX = 1920;
export const PANE_COLLAPSE_OVERSHOOT = 40;

export function shouldCollapsePane(width: number, minWidth: number): boolean {
  return width <= minWidth - PANE_COLLAPSE_OVERSHOOT;
}

/** ZCode keeps the navigation rail within half of the available shell. */
export function getSidebarWidthMax(availableWidth = Number.POSITIVE_INFINITY): number {
  if (!Number.isFinite(availableWidth) || availableWidth <= 0) {
    return SIDEBAR_WIDTH_MAX;
  }
  // 与 ZCode WorkspaceShellLayout 保持一致：侧栏最多占工作区一半，
  // 但窄窗口仍保留可用的 264px 最小宽度。
  return Math.max(SIDEBAR_WIDTH_MIN, Math.floor(availableWidth / 2));
}

export interface SidebarResizeStart {
  clientX: number;
  width: number;
}

export function clampSidebarWidth(
  w: number,
  maxWidth = SIDEBAR_WIDTH_MAX,
): number {
  if (!Number.isFinite(w)) return DEFAULT_LAYOUT.sidebarWidth;
  const max = Number.isFinite(maxWidth)
    ? Math.max(SIDEBAR_WIDTH_MIN, Math.round(maxWidth))
    : SIDEBAR_WIDTH_MAX;
  return Math.min(
    max,
    Math.max(SIDEBAR_WIDTH_MIN, Math.round(w)),
  );
}

export function clampAsideWidth(
  w: number,
  availableWidth = ASIDE_WIDTH_MAX + MAIN_WIDTH_MIN,
): number {
  if (!Number.isFinite(w)) return DEFAULT_LAYOUT.asideWidth;
  const max = Number.isFinite(availableWidth)
    ? Math.min(
        ASIDE_WIDTH_MAX,
        Math.max(ASIDE_WIDTH_MIN, Math.round(availableWidth) - MAIN_WIDTH_MIN),
      )
    : ASIDE_WIDTH_MAX;
  return Math.min(max, Math.max(ASIDE_WIDTH_MIN, Math.round(w)));
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
    o.sidebarWidth < SIDEBAR_WIDTH_MIN ||
    o.sidebarWidth > SIDEBAR_WIDTH_MAX
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

/** 手机首次进入先展示主工作区，侧栏仍可由标题栏按钮打开为抽屉。 */
export function loadInitialLayout(
  storage: { getItem(k: string): string | null },
  viewportWidth: number,
): LayoutPrefs {
  // 布局偏好只影响界面，不应因损坏或超出当前尺寸约束而阻止应用启动。
  // 这里回退到当前默认值，不改写原值，避免初始化阶段产生额外存储副作用。
  let layout: LayoutPrefs;
  try {
    layout = loadLayout(storage);
  } catch {
    layout = { ...DEFAULT_LAYOUT };
  }
  // 保留持久化值本身，但在当前窗口过窄时只把本次启动的布局限制在可用范围内；
  // 窗口放大后仍可恢复用户保存的宽度，而不会让当前工作区溢出。
  layout = {
    ...layout,
    sidebarWidth: clampSidebarWidth(
      layout.sidebarWidth,
      getSidebarWidthMax(viewportWidth),
    ),
  };
  return viewportWidth <= 760 ? { ...layout, sidebarCollapsed: true } : layout;
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
