import { describe, expect, it } from "vitest";
import {
  draftGreeting,
  fitDraftGreeting,
  nextDraftGreetingDelay,
} from "@/modules/ai/lib/draftGreeting";

describe("draft greeting", () => {
  it.each([
    [5, "Good morning. A new day begins."],
    [9, "Good morning. How can I help you?"],
    [12, "Good afternoon. Time for a break?"],
    [14, "Good afternoon. Leave the next step to me."],
    [18, "Good evening. You've worked hard today."],
    [23, "It's getting late. Remember to take care of yourself."],
  ])("changes at %i:00 in local time", (hour, message) => {
    const boundary = new Date(2026, 9, 7, hour, 0, 0, 0);
    const before = new Date(boundary.getTime() - 1);
    expect(draftGreeting(before)).not.toBe(message);
    expect(draftGreeting(boundary)).toBe(message);
    expect(draftGreeting(new Date(boundary.getTime() + 1))).toBe(message);
    expect(nextDraftGreetingDelay(before)).toBe(1);
    expect(nextDraftGreetingDelay(boundary)).toBeGreaterThan(1);
  });

  it("schedules the next boundary once from the current time", () => {
    const now = new Date(2026, 9, 7, 10, 35, 21, 123);
    expect(now.getTime() + nextDraftGreetingDelay(now)).toBe(
      new Date(2026, 9, 7, 12).getTime(),
    );
  });

  it("keeps the late-night greeting across midnight and schedules tomorrow's morning", () => {
    const now = new Date(2026, 11, 31, 23, 59, 59, 999);
    const midnight = new Date(2027, 0, 1);
    expect(draftGreeting(now)).toBe(draftGreeting(midnight));
    const next = new Date(now.getTime() + nextDraftGreetingDelay(now));
    expect(next).toEqual(new Date(2027, 0, 1, 5));
    expect(draftGreeting(next)).toBe("Good morning. A new day begins.");
  });

  it("fits the actual title width while preserving readable text", () => {
    expect(fitDraftGreeting(672, 450, 30)).toBe(30);
    expect(fitDraftGreeting(360, 450, 30)).toBe(24);
    expect(fitDraftGreeting(120, 450, 30)).toBe(20);
    expect(fitDraftGreeting(120, 540, 36)).toBe(26);
  });

  it("keeps the theme's title size before layout is measurable", () => {
    expect(fitDraftGreeting(0, 450, 30)).toBe(30);
    expect(fitDraftGreeting(450, 0, 36)).toBe(36);
    expect(fitDraftGreeting(Number.NaN, 450, 30)).toBe(30);
  });
});
