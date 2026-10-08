import { describe, expect, it } from "vitest";
import {
  filterMarketplace,
  marketplaceSourceDirectory,
  pluginConfigValues,
} from "@/modules/ai/lib/pluginUi";
import type {
  AvailablePlugin,
  PluginConfigField,
} from "@/modules/ai/lib/extensions";

const field: PluginConfigField = {
  type: "string",
  title: null,
  default: null,
  required: false,
  sensitive: false,
  multiple: false,
  min: null,
  max: null,
  enum: [],
};
describe("plugin settings", () => {
  it("authorizes local directories and manifest parents without treating remote sources as paths", () => {
    expect(marketplaceSourceDirectory("D:\\plugins\\marketplace.json")).toBe(
      "D:/plugins",
    );
    expect(marketplaceSourceDirectory("D:/marketplace.json")).toBe("D:/");
    expect(marketplaceSourceDirectory("/marketplace.json")).toBe("/");
    expect(marketplaceSourceDirectory("/plugins/market")).toBe(
      "/plugins/market",
    );
    expect(marketplaceSourceDirectory("github:owner/repo")).toBeNull();
    expect(
      marketplaceSourceDirectory("https://example.com/marketplace.json"),
    ).toBeNull();
  });
  it("keeps duplicate names in separate marketplaces and searches metadata", () => {
    const plugins: AvailablePlugin[] = [
      {
        name: "review",
        marketplace: "official",
        description: "Code review",
        category: "development",
        version: "1",
        keywords: ["Rust"],
        installed: false,
      },
      {
        name: "review",
        marketplace: "team",
        description: "Team review",
        category: null,
        version: "2",
        keywords: [],
        installed: true,
      },
    ];
    expect(filterMarketplace(plugins, " REVIEW ", "team")).toEqual([
      plugins[1],
    ]);
    expect(filterMarketplace(plugins, "rust", "")).toEqual([plugins[0]]);
    expect(filterMarketplace(plugins, "development", "team")).toEqual([]);
  });
  it("sends only edited secrets while preserving zero, false and select value types", () => {
    const fields = {
      count: { ...field, type: "number" as const },
      token: { ...field, sensitive: true },
      flags: { ...field, type: "boolean" as const },
      choice: { ...field, type: "select" as const },
      ports: { ...field, type: "number" as const, multiple: true },
    };
    expect(
      pluginConfigValues(fields, {
        count: "0",
        flags: false,
        choice: 2,
        ports: ["80", "443"],
        stale: "ignored",
      }),
    ).toEqual({ count: 0, flags: false, choice: 2, ports: [80, 443] });
    expect(pluginConfigValues(fields, { token: "replacement" })).toEqual({
      token: "replacement",
    });
    for (const count of ["", " ", "abc", "Infinity"])
      expect(() => pluginConfigValues(fields, { count })).toThrow();
  });
});
