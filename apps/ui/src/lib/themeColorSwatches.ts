import { formatColor, parseColor } from "@appica/ui-react/color";
import { isCustomColor, type BaseColor, type BrandColor, type CustomColor } from "./themeColors";

export const DEFAULT_CUSTOM_PRIMARY: CustomColor = "#3b82f6";
export const DEFAULT_CUSTOM_SECONDARY: CustomColor = "#00bc7d";

export const BASE_COLOR_SWATCHES: Record<BaseColor, string> = {
  gray: "oklch(55% 0.012 264)",
  slate: "oklch(55% 0.035 260)",
  zinc: "oklch(55% 0.008 286)",
  neutral: "oklch(55% 0 0)",
  stone: "oklch(55% 0.015 56)",
};

export const BRAND_COLOR_SWATCHES: Record<BrandColor, string> = {
  red: "oklch(63.7% 0.237 25.331)",
  orange: "oklch(70.5% 0.213 47.604)",
  amber: "oklch(76.9% 0.188 70.08)",
  green: "oklch(62.7% 0.194 149.214)",
  teal: "oklch(60% 0.118 184.704)",
  blue: "oklch(62.3% 0.188 259.81)",
  violet: "oklch(60.6% 0.25 292.717)",
};

export function initialCustomColor<T extends string>(
  value: T | CustomColor,
  defaultPreset: T | undefined,
  fallback: CustomColor,
  swatches: Partial<Record<T, string>>,
  previous: CustomColor | null,
): CustomColor {
  if (previous) return previous;
  if (!isCustomColor(value) && value !== defaultPreset) {
    const swatch = swatches[value];
    if (swatch) return formatColor(parseColor(swatch), "hex") as CustomColor;
  }
  return fallback;
}
