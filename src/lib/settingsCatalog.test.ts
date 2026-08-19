import { describe, expect, it } from "vitest";
import {
  SETTINGS_ENTRIES,
  SETTINGS_NAV,
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
      "account",
      "appearance",
      "personalization",
      "archived",
      "market",
      "skills",
      "agents",
      "mcp",
      "requests",
      "analytics",
      "about",
    ]);
    expect(ids).toEqual([...SETTINGS_SECTION_IDS]);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of SETTINGS_SECTION_IDS) {
      expect(
        SETTINGS_ENTRIES.some((entry) => entry.section === id),
        `missing entries for ${id}`,
      ).toBe(true);
    }
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

  it("仅注册首版可见设置的搜索项", () => {
    expect(
      SETTINGS_ENTRIES.every((entry) =>
        SETTINGS_SECTION_IDS.includes(entry.section),
      ),
    ).toBe(true);
    expect(
      SETTINGS_ENTRIES.find((entry) => entry.id === "ext.plugins")?.section,
    ).toBe("market");
    expect(
      SETTINGS_ENTRIES.find((entry) => entry.id === "ext.skills")?.section,
    ).toBe("skills");
    expect(
      SETTINGS_ENTRIES.find((entry) => entry.id === "ext.mcp")?.section,
    ).toBe("mcp");
    expect(
      SETTINGS_ENTRIES.find((entry) => entry.id === "requests.history")?.section,
    ).toBe("requests");
    expect(
      SETTINGS_ENTRIES.filter((entry) => entry.section === "general").map(
        (entry) => entry.id,
      ),
    ).toEqual([
      "general.interfaceLanguage",
      "general.hardwareAcceleration",
      "general.taskNotifications",
      "general.notificationSound",
      "general.keepComputerAwake",
      "general.showFullThinking",
    ]);
  });
});
