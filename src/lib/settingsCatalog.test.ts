import { describe, expect, it } from "vitest";
import {
  SETTINGS_NAV,
  SETTINGS_NAV_GROUPS,
  SETTINGS_SECTION_IDS,
  buildSettingsHash,
  catalogInvariants,
  isSettingsSectionId,
  parseSettingsHash,
} from "./settingsCatalog";

describe("settingsCatalog", () => {
  it("保持设置目录结构完整", () => {
    expect(catalogInvariants()).toEqual([]);
  });

  it("只展示首版确认的同级入口", () => {
    const ids = SETTINGS_NAV.map((item) => item.id);
    expect(ids).toEqual([
      "general",
      "appearance",
      "account",
      "personalization",
      "skills",
      "plugins",
      "agents",
      "market",
      "mcp",
      "archive",
      "archived",
      "requests",
      "analytics",
      "about",
    ]);
    expect(ids).toEqual([...SETTINGS_SECTION_IDS]);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("按基础设置、Agent能力和使用数据三个稳定分组组织入口", () => {
    expect(SETTINGS_NAV_GROUPS.map((group) => group.id)).toEqual([
      "core",
      "extensions",
      "archive",
      "data",
    ]);
    expect(
      Object.fromEntries(
        SETTINGS_NAV_GROUPS.map((group) => [
          group.id,
          SETTINGS_NAV.filter((item) => item.group === group.id).map(
            (item) => item.id,
          ),
        ]),
      ),
    ).toEqual({
      core: ["general", "appearance", "account"],
      extensions: [
        "personalization",
        "skills",
        "plugins",
        "agents",
        "market",
        "mcp",
      ],
      archive: ["archive", "archived"],
      data: ["requests", "analytics"],
    });
    expect(SETTINGS_NAV.filter((item) => item.group === null).map((item) => item.id)).toEqual([
      "about",
    ]);
  });

  it("插件市场入口与插件页面使用独立文案", () => {
    expect(SETTINGS_NAV.find((item) => item.id === "market")?.labelKey).toBe(
      "settings.nav.market",
    );
    expect(SETTINGS_NAV.find((item) => item.id === "plugins")?.labelKey).toBe(
      "settings.nav.plugins",
    );
  });

  it("只接受当前唯一的设置深链结构", () => {
    expect(parseSettingsHash("#/settings/general")).toEqual({
      section: "general",
    });
    expect(parseSettingsHash("settings")).toBeNull();
    expect(parseSettingsHash("#/settings/general/composer")).toBeNull();
    expect(parseSettingsHash("settings/unknown/nested")).toBeNull();
    expect(buildSettingsHash({ section: "general" })).toBe(
      "#/settings/general",
    );
  });

  it("为扩展与子智能体入口生成独立路由", () => {
    expect(parseSettingsHash("settings/archived")).toEqual({
      section: "archived",
    });
    expect(parseSettingsHash("settings/market")).toEqual({
      section: "market",
    });
    expect(parseSettingsHash("settings/plugins")).toEqual({
      section: "plugins",
    });
    expect(parseSettingsHash("settings/skills")).toEqual({
      section: "skills",
    });
    expect(parseSettingsHash("settings/mcp")).toEqual({
      section: "mcp",
    });
    expect(parseSettingsHash("settings/agents")).toEqual({
      section: "agents",
    });
    expect(parseSettingsHash("settings/requests")).toEqual({
      section: "requests",
    });
    expect(buildSettingsHash({ section: "market" })).toBe(
      "#/settings/market",
    );
    expect(buildSettingsHash({ section: "skills" })).toBe(
      "#/settings/skills",
    );
    expect(buildSettingsHash({ section: "mcp" })).toBe("#/settings/mcp");
    expect(buildSettingsHash({ section: "agents" })).toBe(
      "#/settings/agents",
    );
    expect(buildSettingsHash({ section: "requests" })).toBe(
      "#/settings/requests",
    );
  });

  it("拒绝目录外的设置分区", () => {
    expect(isSettingsSectionId("unknown")).toBe(false);
  });

});
