import icons from "@iconify-json/catppuccin/icons.json";
import { describe, expect, it } from "vitest";
import { createCatppuccinAssets } from "../../scripts/catppuccin-assets.mjs";

describe("Catppuccin static resources", () => {
  it("preserves all legacy SVG bytes and aliases without dropping any icon", () => {
    const result = createCatppuccinAssets(icons);
    expect(result.svgs.size).toBe(Object.keys(icons.icons).length);
    for (const [name, icon] of Object.entries(icons.icons)) {
      expect(result.svgs.get(name), name).toBe(
        `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">${icon.body}</svg>`,
      );
    }
    expect(Object.fromEntries(result.aliases)).toEqual({ maven: "apache" });
  });

  it("retains declared set dimensions and the previous unknown-alias fallback", () => {
    const result = createCatppuccinAssets({
      width: 24,
      height: 32,
      icons: { file: { body: "<path/>" } },
      aliases: { known: { parent: "file" }, unknown: { parent: "absent" } },
    });
    expect(result.svgs.get("file")).toContain('viewBox="0 0 24 32"');
    expect(result.aliases.get("known")).toBe("file");
    expect(result.aliases.has("unknown")).toBe(false);
  });
});
