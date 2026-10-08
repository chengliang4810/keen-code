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

export const EFFORT_COLOR_LABELS: Record<EffortColor, string> = {
  purple: "Purple",
  terracotta: "Terracotta",
  indigo: "Indigo",
  azure: "Azure",
  spectrum: "Spectrum",
  silver: "Silver",
};

export const EFFORT_COLOR_SWATCHES: Record<EffortColor, string> = {
  purple: "#8c57f7",
  terracotta: "#cc6b47",
  indigo: "#3d5cfa",
  azure: "linear-gradient(90deg, #0063e0, #0082fa)",
  spectrum: "linear-gradient(90deg, #4285f5, #9c73cc, #d96670)",
  silver: "linear-gradient(90deg, #ffffff, #e0e0e8)",
};

export function normalizeEffortColor(value: unknown): EffortColor {
  return typeof value === "string" &&
    (EFFORT_COLORS as readonly string[]).includes(value)
    ? (value as EffortColor)
    : DEFAULT_EFFORT_COLOR;
}
