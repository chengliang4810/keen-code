export const BASE_COLORS = ["gray", "slate", "zinc", "neutral", "stone"] as const;
export const BRAND_COLORS = ["red", "orange", "amber", "green", "teal", "blue", "violet"] as const;
export const PRIMARY_COLORS = ["base", ...BRAND_COLORS] as const;
export const SECONDARY_COLORS = BRAND_COLORS;
export const DEFAULT_BASE_COLOR = "gray";
export const DEFAULT_PRIMARY_COLOR = "base";
export const DEFAULT_SECONDARY_COLOR = "blue";
export type BaseColor = (typeof BASE_COLORS)[number];
export type BrandColor = (typeof BRAND_COLORS)[number];
export type CustomColor = `#${string}`;
export type PrimaryColor = (typeof PRIMARY_COLORS)[number] | CustomColor;
export type SecondaryColor = (typeof SECONDARY_COLORS)[number] | CustomColor;

export function isCustomColor(value: string): value is CustomColor {
  return /^#[0-9a-f]{6}$/i.test(value);
}

export function loadColor<T extends string>(
  key: string,
  choices: readonly T[],
  fallback: T,
  storage: Pick<Storage, "getItem"> = localStorage,
): T {
  const value = storage.getItem(key);
  return choices.find((choice) => choice === value) ?? fallback;
}

export function loadBrandColor<T extends string>(
  key: string,
  choices: readonly T[],
  fallback: T,
  storage: Pick<Storage, "getItem"> = localStorage,
): T | CustomColor {
  const value = storage.getItem(key);
  if (value && isCustomColor(value)) return value.toLowerCase() as CustomColor;
  return choices.find((choice) => choice === value) ?? fallback;
}

function customForeground(color: CustomColor): string {
  const rgb = [1, 3, 5].map((index) => {
    const channel = Number.parseInt(color.slice(index, index + 2), 16) / 255;
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  const luminance = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
  return luminance > 0.179 ? "oklch(0.17 0 0)" : "oklch(1 0 0)";
}

export function applyThemeColors(
  base: BaseColor,
  primary: PrimaryColor,
  secondary: SecondaryColor,
  root: HTMLElement = document.documentElement,
): void {
  root.dataset.baseColor = base;
  root.dataset.primaryColor = isCustomColor(primary) ? "custom" : primary;
  root.dataset.secondaryColor = isCustomColor(secondary) ? "custom" : secondary;
  if (isCustomColor(primary)) {
    root.style.setProperty("--custom-primary-color", primary);
    root.style.setProperty("--custom-primary-foreground", customForeground(primary));
  } else {
    root.style.removeProperty("--custom-primary-color");
    root.style.removeProperty("--custom-primary-foreground");
  }
  if (isCustomColor(secondary)) {
    root.style.setProperty("--custom-secondary-color", secondary);
    root.style.setProperty("--custom-secondary-foreground", customForeground(secondary));
  } else {
    root.style.removeProperty("--custom-secondary-color");
    root.style.removeProperty("--custom-secondary-foreground");
  }
}
