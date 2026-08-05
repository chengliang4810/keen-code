import { describe, expect, it } from "vitest";
import { formatMessageTime } from "./messageTime";

describe("formatMessageTime", () => {
  it("formats weekday and time", () => {
    const iso = "2026-07-21T07:23:00.000Z";
    expect(formatMessageTime(iso, "zh").length).toBeGreaterThan(4);
    expect(formatMessageTime(iso, "en").length).toBeGreaterThan(4);
    expect(formatMessageTime(null, "zh")).toBe("");
  });
});
