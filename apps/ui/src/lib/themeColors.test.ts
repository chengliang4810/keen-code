import { describe, expect, it } from "vitest";
import {
  applyThemeColors,
  BASE_COLORS,
  DEFAULT_BASE_COLOR,
  DEFAULT_PRIMARY_COLOR,
  DEFAULT_SECONDARY_COLOR,
  PRIMARY_COLORS,
  SECONDARY_COLORS,
  isCustomColor,
  loadBrandColor,
  loadColor,
} from "./themeColors";
import {
  BRAND_COLOR_SWATCHES,
  DEFAULT_CUSTOM_PRIMARY,
  DEFAULT_CUSTOM_SECONDARY,
  initialCustomColor,
} from "./themeColorSwatches";

function storage(value: string | null): Pick<Storage, "getItem"> {
  return { getItem: () => value };
}

function themeRoot() {
  const properties = new Map<string, string>();
  const root = {
    dataset: {} as Record<string, string>,
    style: {
      setProperty: (name: string, value: string) => properties.set(name, value),
      removeProperty: (name: string) => properties.delete(name),
    },
  } as unknown as HTMLElement;
  return { root, properties };
}

describe("theme color choices", () => {
  it("uses the official Gray / Base / Blue defaults", () => {
    expect(DEFAULT_BASE_COLOR).toBe("gray");
    expect(DEFAULT_PRIMARY_COLOR).toBe("base");
    expect(DEFAULT_SECONDARY_COLOR).toBe("blue");
    expect(loadColor("base", BASE_COLORS, DEFAULT_BASE_COLOR, storage(null))).toBe("gray");
    expect(loadBrandColor("primary", PRIMARY_COLORS, DEFAULT_PRIMARY_COLOR, storage(null))).toBe("base");
    expect(loadBrandColor("secondary", SECONDARY_COLORS, DEFAULT_SECONDARY_COLOR, storage(null))).toBe("blue");
  });

  it("accepts only opaque six-digit custom colors and restores them canonically", () => {
    expect(isCustomColor("#12AbCD")).toBe(true);
    expect(isCustomColor("#fff")).toBe(false);
    expect(isCustomColor("#12345678")).toBe(false);
    expect(loadBrandColor("primary", PRIMARY_COLORS, DEFAULT_PRIMARY_COLOR, storage("#12AbCD"))).toBe("#12abcd");
    expect(loadBrandColor("primary", PRIMARY_COLORS, DEFAULT_PRIMARY_COLOR, storage("javascript:alert(1)"))).toBe("base");
  });

  it("applies custom roles and clears them after returning to presets", () => {
    const { root, properties } = themeRoot();
    applyThemeColors("slate", "#ffffff", "#123456", root);
    expect(root.dataset).toEqual({
      baseColor: "slate",
      primaryColor: "custom",
      secondaryColor: "custom",
    });
    expect(properties.get("--custom-primary-color")).toBe("#ffffff");
    expect(properties.get("--custom-primary-foreground")).toBe("oklch(0.17 0 0)");
    expect(properties.get("--custom-secondary-color")).toBe("#123456");
    expect(properties.get("--custom-secondary-foreground")).toBe("oklch(1 0 0)");

    applyThemeColors("gray", "base", "blue", root);
    expect(root.dataset).toEqual({
      baseColor: "gray",
      primaryColor: "base",
      secondaryColor: "blue",
    });
    expect(properties.size).toBe(0);
  });

  it("starts custom picking from the official fallback or the current preset", () => {
    expect(initialCustomColor<(typeof PRIMARY_COLORS)[number]>("base", "base", DEFAULT_CUSTOM_PRIMARY, BRAND_COLOR_SWATCHES, null)).toBe(DEFAULT_CUSTOM_PRIMARY);
    expect(initialCustomColor("blue", "blue", DEFAULT_CUSTOM_SECONDARY, BRAND_COLOR_SWATCHES, null)).toBe(DEFAULT_CUSTOM_SECONDARY);
    expect(initialCustomColor("red", "base", DEFAULT_CUSTOM_PRIMARY, BRAND_COLOR_SWATCHES, null)).toBe("#fb2c36");
    expect(initialCustomColor("red", "base", DEFAULT_CUSTOM_PRIMARY, BRAND_COLOR_SWATCHES, "#123456")).toBe("#123456");
  });
});
