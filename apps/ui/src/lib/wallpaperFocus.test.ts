import { describe, expect, it } from "vitest";
import {
  DEFAULT_WALLPAPER_FOCUS,
  WALLPAPER_FOCUS_MAX_ZOOM,
  containRect,
  coverVisibleSize,
  focusFromCenterZoom,
  focusFromVisibleRect,
  focusWithZoom,
  isDefaultWallpaperFocus,
  normalizeWallpaperFocus,
  parseWallpaperFocus,
  visibleRectToStageFrame,
  wallpaperMediaLayout,
  wallpaperVisibleRect,
} from "./wallpaperFocus";

describe("wallpaperFocus", () => {
  it("normalizes and clamps focus", () => {
    expect(normalizeWallpaperFocus(null)).toEqual(DEFAULT_WALLPAPER_FOCUS);
    expect(normalizeWallpaperFocus({ cx: -1, cy: 2, zoom: 99 })).toEqual({
      cx: 0,
      cy: 1,
      zoom: WALLPAPER_FOCUS_MAX_ZOOM,
    });
    expect(normalizeWallpaperFocus({ cx: 0.25, cy: 0.75, zoom: 0.5 })).toEqual({
      cx: 0.25,
      cy: 0.75,
      zoom: 1,
    });
  });

  it("parses partial focus objects", () => {
    expect(parseWallpaperFocus({ cx: 0.2 })).toMatchObject({
      cx: 0.2,
      cy: 0.5,
      zoom: 1,
    });
    expect(isDefaultWallpaperFocus(parseWallpaperFocus({}))).toBe(true);
  });

  it("cover-visible size uses full height when media is wider than view", () => {
    // 1920×1080 media, 1:1 viewport → cover uses height 1080, width 1080
    const s = coverVisibleSize(1920, 1080, 1, 1);
    expect(s.h).toBeCloseTo(1080, 5);
    expect(s.w).toBeCloseTo(1080, 5);
  });

  it("cover-visible size uses full width when media is taller than view", () => {
    // 1080×1920 media, 16:9 viewport
    const s = coverVisibleSize(1080, 1920, 16 / 9, 1);
    expect(s.w).toBeCloseTo(1080, 5);
    expect(s.h).toBeCloseTo(1080 / (16 / 9), 5);
  });

  it("zoom shrinks the visible rect while keeping aspect", () => {
    const a = wallpaperVisibleRect(2000, 1000, 2, { cx: 0.5, cy: 0.5, zoom: 1 });
    const b = wallpaperVisibleRect(2000, 1000, 2, { cx: 0.5, cy: 0.5, zoom: 2 });
    expect(b.w).toBeCloseTo(a.w / 2, 5);
    expect(b.h).toBeCloseTo(a.h / 2, 5);
    expect(b.x + b.w / 2).toBeCloseTo(0.5, 5);
    expect(b.y + b.h / 2).toBeCloseTo(0.5, 5);
  });

  it("clamps pan so the visible rect stays inside the media", () => {
    const vis = wallpaperVisibleRect(1000, 1000, 1, {
      cx: 0,
      cy: 0,
      zoom: 2,
    });
    expect(vis.x).toBeGreaterThanOrEqual(0);
    expect(vis.y).toBeGreaterThanOrEqual(0);
    expect(vis.x + vis.w).toBeLessThanOrEqual(1 + 1e-9);
    expect(vis.y + vis.h).toBeLessThanOrEqual(1 + 1e-9);
  });

  it("round-trips focus through visible rect", () => {
    const mediaW = 1600;
    const mediaH = 900;
    const aspect = 16 / 10;
    const original = focusFromCenterZoom(0.35, 0.6, 1.8, mediaW, mediaH, aspect);
    const vis = wallpaperVisibleRect(mediaW, mediaH, aspect, original);
    const back = focusFromVisibleRect(vis, mediaW, mediaH, aspect);
    expect(back.cx).toBeCloseTo(original.cx, 5);
    expect(back.cy).toBeCloseTo(original.cy, 5);
    expect(back.zoom).toBeCloseTo(original.zoom, 4);
  });

  it("layout covers the viewport without gaps", () => {
    const layout = wallpaperMediaLayout(
      1920,
      1080,
      800,
      600,
      { cx: 0.5, cy: 0.5, zoom: 1 },
    );
    expect(layout.width).toBeGreaterThanOrEqual(800 - 1e-6);
    expect(layout.height).toBeGreaterThanOrEqual(600 - 1e-6);
    expect(layout.left).toBeLessThanOrEqual(0);
    expect(layout.top).toBeLessThanOrEqual(0);
    expect(layout.left + layout.width).toBeGreaterThanOrEqual(800 - 1e-6);
    expect(layout.top + layout.height).toBeGreaterThanOrEqual(600 - 1e-6);
  });

  it("maps visible rect into a contain-fitted stage", () => {
    const mediaBox = containRect(2000, 1000, 400, 400);
    // Media letterboxed vertically in square stage.
    expect(mediaBox.w).toBeCloseTo(400, 5);
    expect(mediaBox.h).toBeCloseTo(200, 5);
    const vis = { x: 0.25, y: 0, w: 0.5, h: 1 };
    const frame = visibleRectToStageFrame(vis, mediaBox);
    expect(frame.x).toBeCloseTo(mediaBox.x + 0.25 * mediaBox.w, 5);
    expect(frame.w).toBeCloseTo(0.5 * mediaBox.w, 5);
    expect(frame.h).toBeCloseTo(mediaBox.h, 5);
  });

  it("focusWithZoom keeps center when possible", () => {
    const next = focusWithZoom(
      { cx: 0.5, cy: 0.5, zoom: 1 },
      2,
      2000,
      1000,
      2,
    );
    expect(next.zoom).toBeCloseTo(2, 5);
    expect(next.cx).toBeCloseTo(0.5, 5);
    expect(next.cy).toBeCloseTo(0.5, 5);
  });
});
