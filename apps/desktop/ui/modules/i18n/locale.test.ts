import { describe, expect, it } from "vitest";
import { SHORTCUTS, SHORTCUT_GROUPS } from "@/modules/shortcuts/shortcuts";
import {
  interpolate,
  normalizeLanguagePreference,
  resolveLocale,
} from "./locale";
import { translate } from "./translate";
import { zhCN } from "./zh-CN";

describe("interface locale", () => {
  it("covers every built-in shortcut label and group for Chinese display and search", () => {
    for (const message of [
      ...SHORTCUT_GROUPS,
      ...SHORTCUTS.map((shortcut) => shortcut.label),
    ]) {
      expect(zhCN[message], message).toBeTypeOf("string");
    }
  });
  it("honors explicit choices before the system language", () => {
    expect(resolveLocale("en-US", ["zh-CN"])).toBe("en-US");
    expect(resolveLocale("zh-CN", ["en-US"])).toBe("zh-CN");
  });

  it("resolves supported system language families and falls back to English", () => {
    for (const language of ["zh-CN", "zh-TW", "ZH_hans", "zh"]) {
      expect(resolveLocale("system", [language])).toBe("zh-CN");
    }
    expect(resolveLocale("system", ["en-GB", "zh-CN"])).toBe("en-US");
    expect(resolveLocale("system", ["fr-FR", "zh-CN"])).toBe("zh-CN");
    expect(resolveLocale("system", ["ja-JP"])).toBe("en-US");
    expect(resolveLocale("system", [])).toBe("en-US");
  });

  it("normalizes missing or malformed persisted choices", () => {
    for (const value of [undefined, null, "", "zh", "fr-FR", {}, 42]) {
      expect(normalizeLanguagePreference(value)).toBe("system");
    }
    expect(normalizeLanguagePreference("zh-CN")).toBe("zh-CN");
    expect(normalizeLanguagePreference("en-US")).toBe("en-US");
  });

  it("preserves raw details and never interprets parameters a second time", () => {
    expect(
      interpolate("{name}: {count}; {missing}", { name: "{count}", count: 0 }),
    ).toBe("{count}: 0; {missing}");
    expect(interpolate("{toString}", {})).toBe("{toString}");
    expect(translate("zh-CN", "Unknown technical detail")).toBe(
      "Unknown technical detail",
    );
    expect(
      translate("en-US", "Branch: {value0}", { value0: "feature/中文" }, zhCN),
    ).toBe("Branch: feature/中文");
    expect(
      translate("zh-CN", "Branch: {value0}", { value0: "feature/中文" }, zhCN),
    ).toBe("分支：feature/中文");
  });

  it("retains every interpolation parameter in the Chinese catalog", () => {
    const parameters = (message: string) =>
      [...new Set(message.match(/\{[a-zA-Z][\w]*\}/g) ?? [])].sort();
    for (const [english, chinese] of Object.entries(zhCN)) {
      expect(parameters(chinese), english).toEqual(parameters(english));
    }
  });
});
