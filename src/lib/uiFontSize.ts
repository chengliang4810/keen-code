/**
 * 界面字号偏好：全部界面文字尺寸的唯一输入。
 *
 * 值写入根元素的 `--ui-font-size`，其余字号令牌按相对 14px 基线的同一像素
 * 偏移（`--ui-font-delta`）派生，因此字号变化时层级与行距比例保持不变。
 * 图标、间距与控件几何不读这两个变量，不受影响。
 */

export const UI_FONT_SIZE_STORAGE_KEY = "keencode.uiFontSize";

/** 首次启动默认字号，与设计令牌基线一致。 */
export const DEFAULT_UI_FONT_SIZE = 14;
export const MIN_UI_FONT_SIZE = 12;
export const MAX_UI_FONT_SIZE = 20;

/** 界面字号存储接口，供 Hook 与测试替换。 */
export interface UiFontSizeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** 把任意数值收敛为允许范围内的整数像素值。 */
export function normalizeUiFontSize(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_UI_FONT_SIZE;
  return Math.min(
    MAX_UI_FONT_SIZE,
    Math.max(MIN_UI_FONT_SIZE, Math.round(value)),
  );
}

/** 解析当前字号是否为允许范围内的整数像素值。 */
export function isUiFontSize(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    value >= MIN_UI_FONT_SIZE &&
    value <= MAX_UI_FONT_SIZE
  );
}

/** 解析已持久化的界面字号；仅缺失值使用首次启动默认值。 */
export function parseUiFontSize(raw: unknown): number {
  if (raw === null) return DEFAULT_UI_FONT_SIZE;
  if (typeof raw !== "string" || raw.trim() === "") {
    throw new Error("界面字号格式无效");
  }
  const value = Number(raw);
  if (!isUiFontSize(value)) {
    throw new Error("界面字号格式无效");
  }
  return value;
}

/**
 * 读取持久化界面字号（12–20，默认 14）。
 * 存量值无效时回退默认值：过期数据不得阻断启动或界面渲染。
 */
export function loadUiFontSize(storage: UiFontSizeStorage): number {
  try {
    return parseUiFontSize(storage.getItem(UI_FONT_SIZE_STORAGE_KEY));
  } catch {
    return DEFAULT_UI_FONT_SIZE;
  }
}

/** 校验并持久化界面字号。 */
export function saveUiFontSize(
  storage: UiFontSizeStorage,
  value: number,
): void {
  if (!isUiFontSize(value)) {
    throw new Error("界面字号格式无效");
  }
  storage.setItem(UI_FONT_SIZE_STORAGE_KEY, String(value));
}

/** 把界面字号写入根元素；全部文字令牌据此派生。 */
export function applyUiFontSizeToDocument(
  value: number,
  root: HTMLElement = document.documentElement,
): void {
  root.style?.setProperty(
    "--ui-font-size",
    `${normalizeUiFontSize(value)}px`,
  );
}
