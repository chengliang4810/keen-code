import { loadColor } from "@/lib/themeColors";

/**
 * 思考强度色预设：滑块轨道、标题与画布动效共用的品牌色族，色板对照
 * Droppy Code 的供应商品牌色。每个预设的完整令牌组定义在 `tokens.css`
 * 的 `html[data-effort-color]` 规则里；`--effort-brand-solid`、`--effort-fast`
 * 与 `--effort-spark` 必须是固定 hex（画布的颜色解析器不认渐变与函数值）。
 */
export const EFFORT_COLORS = [
  "purple",
  "terracotta",
  "indigo",
  "azure",
  "spectrum",
  "silver",
] as const;
export type EffortColor = (typeof EFFORT_COLORS)[number];
export const DEFAULT_EFFORT_COLOR: EffortColor = "purple";

/**
 * 预设展示色板；渐变预设取主导色，
 * 与 tokens.css 中该预设的 `--effort-brand-solid` 一致。
 */
export const EFFORT_COLOR_SWATCHES: Record<EffortColor, string> = {
  purple: "#8c57f7",
  terracotta: "#cc6b47",
  indigo: "#3d5cfa",
  azure: "#0063e0",
  spectrum: "#9c73cc",
  silver: "#e0e0e8",
};

export function loadEffortColor(
  storage: Pick<Storage, "getItem"> = localStorage,
): EffortColor {
  return loadColor(
    "keencode.effort-color",
    EFFORT_COLORS,
    DEFAULT_EFFORT_COLOR,
    storage,
  );
}

export function applyEffortColor(
  color: EffortColor,
  root: HTMLElement = document.documentElement,
): void {
  root.dataset.effortColor = color;
}
