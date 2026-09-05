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

  it("后台任务范围文案明确限定所有活跃会话", () => {
    const keys = [
      "summary.backgroundTasks.title",
      "summary.backgroundTasks.allSessionsCount",
      "summary.backgroundTasks.stopAll",
      "summary.backgroundTasks.stopAllTitle",
      "summary.backgroundTasks.stopAllConfirm",
      "summary.backgroundTasks.stopAllSuccess",
      "summary.backgroundTasks.stopAllFailed",
    ] as const;
    const locales = [
      ["en", "active sessions"],
      ["zh", "所有活跃会话"],
      ["zh-TW", "所有活躍會話"],
    ] as const;

    for (const [locale, phrase] of locales) {
      for (const key of keys) {
        expect(
          t(locale, key, { count: 3, error: "network" }),
          `${locale}.${key}`,
        ).toContain(phrase);
      }
    }
  });

});
