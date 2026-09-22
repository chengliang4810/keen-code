import { describe, expect, it } from "vitest";
import {
  createT,
  messages,
  t,
  type MessageKey,
} from "./index";

describe("i18n catalog", () => {
  it("en and zh share the same keys", () => {
    const enKeys = Object.keys(messages.en).sort();
    const zhKeys = Object.keys(messages.zh).sort();
    expect(zhKeys).toEqual(enKeys);
  });

  it("zh-TW shares the same keys as en", () => {
    const enKeys = Object.keys(messages.en).sort();
    const twKeys = Object.keys(messages["zh-TW"]).sort();
    expect(twKeys).toEqual(enKeys);
  });

  it("interpolates variables", () => {
    expect(t("en", "project.pathMissing", { name: "Demo" })).toContain("Demo");
    expect(t("zh", "project.pathMissing", { name: "演示" })).toContain("演示");
  });

  it("createT binds each supported locale", () => {
    const tr = createT("en");
    expect(tr("sidebar.settings")).toBe("Settings");
    const zh = createT("zh");
    expect(zh("sidebar.settings")).toBe("设置");
  });

  it("every value is a non-empty string", () => {
    for (const loc of ["en", "zh", "zh-TW"] as const) {
      for (const [k, v] of Object.entries(messages[loc])) {
        expect(v.trim().length, `${loc}.${k}`).toBeGreaterThan(0);
      }
    }
  });

  it("type surface accepts known keys only", () => {
    const key: MessageKey = "composer.send";
    expect(t("en", key)).toBeTruthy();
  });

  it("uses a current session key for the empty conversation state", () => {
    expect(t("en", "session.empty")).toBe("This conversation has no messages yet.");
    expect(t("zh", "session.empty")).toBe("当前对话还没有消息。");
    expect(t("zh-TW", "session.empty")).toBe("目前對話還沒有訊息。");
  });

  it("兼容服务设置在三种语言中均有可见文案", () => {
    for (const locale of ["zh", "zh-TW", "en"] as const) {
      expect(t(locale, "settings.webServiceUrl")).not.toBe(
        "settings.webServiceUrl",
      );
      expect(t(locale, "settings.webServiceUrlDesc")).not.toBe(
        "settings.webServiceUrlDesc",
      );
      expect(t(locale, "settings.webServiceUrlPlaceholder")).not.toBe(
        "settings.webServiceUrlPlaceholder",
      );
    }
  });

  it("全局指令文案说明下一轮生效且保留远程发送提示", () => {
    expect(t("zh", "settings.personalization.description")).toContain("下一轮");
    expect(t("zh-TW", "settings.personalization.description")).toContain("下一輪");
    expect(t("en", "settings.personalization.description")).toContain("next turn");
    expect(t("zh", "settings.personalization.help")).toContain("发送给你配置的模型供应商");
    expect(t("zh-TW", "settings.personalization.help")).toContain("傳送給你設定的模型供應商");
    expect(t("en", "settings.personalization.help")).toContain("sent to your configured model provider");
  });

});
