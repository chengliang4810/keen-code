export const BASE_COLORS = ["gray", "slate", "zinc", "neutral", "stone"] as const;
export const PRIMARY_COLORS = ["red", "orange", "amber", "green", "teal", "blue", "violet"] as const;
export type BaseColor = (typeof BASE_COLORS)[number];
export type PrimaryColor = (typeof PRIMARY_COLORS)[number];

export function loadColor<T extends string>(key: string, choices: readonly T[], fallback: T): T {
  const value = localStorage.getItem(key);
  return choices.find((choice) => choice === value) ?? fallback;
}

export function applyThemeColors(base: BaseColor, primary: PrimaryColor): void {
  document.documentElement.dataset.baseColor = base;
  document.documentElement.dataset.primaryColor = primary;
}
