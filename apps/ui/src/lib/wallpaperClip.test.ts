import { describe, expect, it } from "vitest";
import {
  WALLPAPER_CLIP_MIN_DURATION,
  formatClipTime,
  normalizeWallpaperClip,
  parseWallpaperClip,
} from "./wallpaperClip";

describe("wallpaperClip", () => {
  it("returns null for full-span clips", () => {
    expect(normalizeWallpaperClip({ start: 0, end: 10 }, 10)).toBeNull();
    expect(normalizeWallpaperClip({ start: 0.01, end: 9.99 }, 10)).toBeNull();
  });

  it("clamps and enforces minimum duration", () => {
    const c = normalizeWallpaperClip({ start: 2, end: 2.1 }, 10);
    expect(c).not.toBeNull();
    expect(c!.end - c!.start).toBeGreaterThanOrEqual(WALLPAPER_CLIP_MIN_DURATION - 1e-6);
    expect(c!.start).toBeGreaterThanOrEqual(0);
    expect(c!.end).toBeLessThanOrEqual(10);
  });

  it("accepts a mid-range selection", () => {
    const c = normalizeWallpaperClip({ start: 1.5, end: 4.25 }, 12);
    expect(c).toEqual({ start: 1.5, end: 4.25 });
  });

  it("parses raw meta without duration", () => {
    expect(parseWallpaperClip({ start: 1, end: 3 })).toEqual({
      start: 1,
      end: 3,
    });
    expect(parseWallpaperClip({ start: 5, end: 2 })).toBeNull();
    expect(parseWallpaperClip(null)).toBeNull();
  });

  it("formats times", () => {
    expect(formatClipTime(0)).toBe("0:00");
    expect(formatClipTime(65)).toBe("1:05");
    expect(formatClipTime(3.4)).toMatch(/^0:03/);
  });
});
