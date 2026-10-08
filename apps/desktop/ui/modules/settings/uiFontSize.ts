export const UI_FONT_SIZE_DEFAULT = 14;
export const UI_FONT_SIZE_MIN = 12;
export const UI_FONT_SIZE_MAX = 20;
export const UI_FONT_SIZES = Array.from(
  { length: UI_FONT_SIZE_MAX - UI_FONT_SIZE_MIN + 1 },
  (_, index) => UI_FONT_SIZE_MIN + index,
);

/** 存档和跨窗口事件都可能携带非法值，统一回退并限制为整数像素。 */
export function normalizeUiFontSize(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return UI_FONT_SIZE_DEFAULT;
  }
  return Math.min(
    UI_FONT_SIZE_MAX,
    Math.max(UI_FONT_SIZE_MIN, Math.round(value)),
  );
}
