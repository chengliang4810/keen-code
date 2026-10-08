import { afterEach, describe, expect, it } from "vitest";
import {
  applyLanguagePreference,
  getLocale,
  subscribeLocale,
  t,
} from "./state";

afterEach(async () => {
  await applyLanguagePreference("en-US");
});

describe("live language switching", () => {
  it("notifies existing consumers once per effective locale change", async () => {
    await applyLanguagePreference("en-US");
    const changes: string[] = [];
    const unsubscribe = subscribeLocale(() => changes.push(getLocale()));
    try {
      await applyLanguagePreference("zh-CN");
      expect(t("Settings")).toBe("设置");
      await applyLanguagePreference("zh-CN");
      await applyLanguagePreference("en-US");
      expect(t("Settings")).toBe("Settings");
      expect(changes).toEqual(["zh-CN", "en-US"]);
    } finally {
      unsubscribe();
    }
  });

  it("does not overwrite a newer choice when a language load completes", async () => {
    await applyLanguagePreference("en-US");
    const pending = applyLanguagePreference("zh-CN");
    await applyLanguagePreference("en-US");
    await pending;
    expect(getLocale()).toBe("en-US");
  });
});
