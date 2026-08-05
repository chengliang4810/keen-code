import { describe, expect, it } from "vitest";
import {
  filterPluginsByLoadState,
  mergeInspectErrors,
  normalizePluginInstallSource,
  pluginProvidesLine,
  pluginRowKey,
  pluginStatusTone,
  shortPathLabel,
  skillMetaLine,
  skillSourceTone,
  sortMcpByName,
  sortPluginsByName,
  sortSkillsByName,
} from "./extensionsUi";

describe("skillSourceTone", () => {
  it("只映射当前合约的三种 Skill 来源", () => {
    expect(skillSourceTone("user")).toBe("user");
    expect(skillSourceTone("project")).toBe("project");
    expect(skillSourceTone("plugin")).toBe("plugin");
  });
});

describe("skillMetaLine", () => {
  it("builds skill meta", () => {
    expect(
      skillMetaLine({
        source: "user",
        userInvocable: true,
      }),
    ).toBe("user · user-invocable");
    expect(
      skillMetaLine({ source: "project", userInvocable: false }),
    ).toBe("project");
  });
});

describe("sort helpers", () => {
  it("sorts skills and mcp by name case-insensitively", () => {
    expect(sortSkillsByName([{ name: "zeta" }, { name: "Alpha" }]).map((s) => s.name)).toEqual([
      "Alpha",
      "zeta",
    ]);
    expect(sortMcpByName([{ name: "b" }, { name: "a" }]).map((s) => s.name)).toEqual([
      "a",
      "b",
    ]);
  });
});

describe("shortPathLabel", () => {
  it("returns short paths unchanged", () => {
    expect(shortPathLabel("/tmp/a")).toBe("/tmp/a");
  });

  it("truncates long paths keeping basename tail", () => {
    const long =
      "/Users/someone/Library/Application Support/com.keencode.desktop/skills/my-skill/SKILL.md";
    const label = shortPathLabel(long, 40);
    expect(label.startsWith("…")).toBe(true);
    expect(label.length).toBeLessThanOrEqual(40);
    expect(label.includes("SKILL.md") || label.includes("my-skill")).toBe(true);
  });

  it("handles empty", () => {
    expect(shortPathLabel("")).toBe("");
    expect(shortPathLabel(null)).toBe("");
  });
});

describe("mergeInspectErrors", () => {
  it("returns null when both empty", () => {
    expect(mergeInspectErrors(null, null, null)).toBeNull();
    expect(mergeInspectErrors("", "", null)).toBeNull();
  });

  it("dedupes identical messages", () => {
    expect(mergeInspectErrors("same", "same", null)).toBe("same");
  });

  it("joins distinct non-cli errors", () => {
    expect(mergeInspectErrors("a", "b", null)).toBe("a · b");
  });

  it("includes the plugins error", () => {
    expect(mergeInspectErrors("a", "b", "c")).toBe("a · b · c");
  });
});

describe("plugin helpers", () => {
  it("sorts plugins by name", () => {
    expect(
      sortPluginsByName([{ name: "zeta" }, { name: "Alpha" }]).map((p) => p.name),
    ).toEqual(["Alpha", "zeta"]);
  });

  it("maps the current load state", () => {
    expect(pluginStatusTone(false)).toBe("disabled");
    expect(pluginStatusTone(true)).toBe("enabled");
  });

  it("生成插件 Skill 数量和唯一行键", () => {
    expect(
      pluginProvidesLine({
        provides: { skills: 14 },
      }),
    ).toBe("14 skills");
    expect(
      pluginProvidesLine({
        provides: { skills: 0 },
      }),
    ).toBe("");
    expect(() => pluginProvidesLine({ provides: null })).toThrow(
      "插件 provides 缺失",
    );
    expect(
      pluginRowKey({
        name: "cloudflare",
      }),
    ).toBe("cloudflare");
    expect(pluginRowKey({ name: "solo" })).toBe("solo");
  });

  it("filters by load state", () => {
    const rows = [
      { name: "a", enabled: true },
      { name: "b", enabled: false },
      { name: "c", enabled: true },
    ];
    expect(filterPluginsByLoadState(rows, "all").map((p) => p.name)).toEqual([
      "a",
      "b",
      "c",
    ]);
    expect(filterPluginsByLoadState(rows, "enabled").map((p) => p.name)).toEqual([
      "a",
      "c",
    ]);
    expect(filterPluginsByLoadState(rows, "disabled").map((p) => p.name)).toEqual([
      "b",
    ]);
  });

  it("normalizes a local path or marketplace selector", () => {
    expect(normalizePluginInstallSource("  demo@local-tools  ")).toBe(
      "demo@local-tools",
    );
    expect(normalizePluginInstallSource("/tmp/plugin")).toBe("/tmp/plugin");
    expect(normalizePluginInstallSource("")).toBeNull();
    expect(normalizePluginInstallSource("   ")).toBeNull();
  });
});
