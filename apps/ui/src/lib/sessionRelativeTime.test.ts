import { describe, expect, it } from "vitest";
import { createT } from "@/i18n";
import { formatSessionRelativeTime } from "./sessionRelativeTime";

describe("formatSessionRelativeTime", () => {
  const now = Date.parse("2026-09-22T12:00:00.000Z");

  it("uses compact relative units matching the ZCode sidebar", () => {
    expect(formatSessionRelativeTime("2026-09-22T11:59:40.000Z", createT("zh"), now)).toBe("刚刚");
    expect(formatSessionRelativeTime("2026-09-22T11:35:00.000Z", createT("zh"), now)).toBe("25分");
    expect(formatSessionRelativeTime("2026-09-22T09:00:00.000Z", createT("zh"), now)).toBe("3小时");
    expect(formatSessionRelativeTime("2026-09-20T12:00:00.000Z", createT("zh"), now)).toBe("2天");
  });

  it("does not fabricate time for missing or invalid timestamps", () => {
    expect(formatSessionRelativeTime("", createT("zh"), now)).toBe("");
    expect(formatSessionRelativeTime("invalid", createT("zh"), now)).toBe("");
  });
});
