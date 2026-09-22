/**
 * Wallpaper focus (pan + zoom) — pure geometry, no media re-encode.
 *
 * Model: center (cx, cy) in media-normalized [0,1] + zoom ≥ 1 relative to
 * cover-fill. Window aspect can change; the visible slice always matches the
 * current viewport aspect. Applied at render time via absolute layout of the
 * <img>/<video> (GPU-friendly; works for video/gif without transcoding).
 */

export interface WallpaperFocus {
  /** Focus center X as fraction of media width [0, 1]. */
  cx: number;
  /** Focus center Y as fraction of media height [0, 1]. */
  cy: number;
  /** Zoom relative to cover fill. 1 = maximum visible (cover), higher = closer. */
  zoom: number;
}

/** Visible slice of the media in normalized media coordinates. */
export interface WallpaperVisibleRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface WallpaperMediaLayout {
  width: number;
  height: number;
  left: number;
  top: number;
}

/** Axis-aligned box (stage / contain fit). */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const DEFAULT_WALLPAPER_FOCUS: WallpaperFocus = {
  cx: 0.5,
  cy: 0.5,
  zoom: 1,
};

/** Soft cap so users cannot zoom into a few pixels of a huge photo. */
export const WALLPAPER_FOCUS_MAX_ZOOM = 5;

export function clamp(n: number, min: number, max: number): number {
  if (!Number.isFinite(n)) return min;
  return Math.min(max, Math.max(min, n));
}

export function normalizeWallpaperFocus(
  value: Partial<WallpaperFocus> | null | undefined,
): WallpaperFocus {
  if (!value || typeof value !== "object") return { ...DEFAULT_WALLPAPER_FOCUS };
  const cx = Number(value.cx);
  const cy = Number(value.cy);
  const zoom = Number(value.zoom);
  return {
    cx: Number.isFinite(cx) ? clamp(cx, 0, 1) : DEFAULT_WALLPAPER_FOCUS.cx,
    cy: Number.isFinite(cy) ? clamp(cy, 0, 1) : DEFAULT_WALLPAPER_FOCUS.cy,
    zoom: Number.isFinite(zoom)
      ? clamp(zoom, 1, WALLPAPER_FOCUS_MAX_ZOOM)
      : DEFAULT_WALLPAPER_FOCUS.zoom,
  };
}

export function parseWallpaperFocus(raw: unknown): WallpaperFocus {
  if (!raw || typeof raw !== "object") return { ...DEFAULT_WALLPAPER_FOCUS };
  const v = raw as Record<string, unknown>;
  return normalizeWallpaperFocus({
    cx: typeof v.cx === "number" ? v.cx : undefined,
    cy: typeof v.cy === "number" ? v.cy : undefined,
    zoom: typeof v.zoom === "number" ? v.zoom : undefined,
  });
}

export function isDefaultWallpaperFocus(focus: WallpaperFocus): boolean {
  const f = normalizeWallpaperFocus(focus);
  return (
    Math.abs(f.cx - 0.5) < 1e-6 &&
    Math.abs(f.cy - 0.5) < 1e-6 &&
    Math.abs(f.zoom - 1) < 1e-6
  );
}

/**
 * Cover-visible size (media pixels) at zoom=1 for a viewport aspect ratio
 * (width / height). Zoom then shrinks that window.
 */
export function coverVisibleSize(
  mediaW: number,
  mediaH: number,
  viewAspect: number,
  zoom = 1,
): { w: number; h: number } {
  const mw = Math.max(1, mediaW);
  const mh = Math.max(1, mediaH);
  const va =
    Number.isFinite(viewAspect) && viewAspect > 0 ? viewAspect : 16 / 10;
  const z = clamp(zoom, 1, WALLPAPER_FOCUS_MAX_ZOOM);
  const mediaAspect = mw / mh;
  let w: number;
  let h: number;
  if (mediaAspect > va) {
    // Media wider than view — cover uses full height.
    h = mh / z;
    w = h * va;
  } else {
    // Media taller (or equal) — cover uses full width.
    w = mw / z;
    h = w / va;
  }
  // Never exceed media bounds (numerical safety).
  w = Math.min(w, mw);
  h = Math.min(h, mh);
  return { w, h };
}

/** Visible rect for focus, clamped so the slice stays inside the media. */
export function wallpaperVisibleRect(
  mediaW: number,
  mediaH: number,
  viewAspect: number,
  focus: WallpaperFocus,
): WallpaperVisibleRect {
  const f = normalizeWallpaperFocus(focus);
  const mw = Math.max(1, mediaW);
  const mh = Math.max(1, mediaH);
  const { w, h } = coverVisibleSize(mw, mh, viewAspect, f.zoom);
  let x = f.cx * mw - w / 2;
  let y = f.cy * mh - h / 2;
  x = clamp(x, 0, mw - w);
  y = clamp(y, 0, mh - h);
  return { x: x / mw, y: y / mh, w: w / mw, h: h / mh };
}

/**
 * Re-derive focus from a visible rect (normalized). Zoom is inferred from how
 * small the rect is relative to cover-at-zoom-1.
 */
export function focusFromVisibleRect(
  rect: WallpaperVisibleRect,
  mediaW: number,
  mediaH: number,
  viewAspect: number,
): WallpaperFocus {
  const mw = Math.max(1, mediaW);
  const mh = Math.max(1, mediaH);
  const x = clamp(rect.x, 0, 1);
  const y = clamp(rect.y, 0, 1);
  const w = clamp(rect.w, 1e-6, 1);
  const h = clamp(rect.h, 1e-6, 1);
  const cover = coverVisibleSize(mw, mh, viewAspect, 1);
  // Prefer width ratio; height should match for correct aspect.
  const zoomW = cover.w / (w * mw);
  const zoomH = cover.h / (h * mh);
  const zoom = clamp(Math.max(zoomW, zoomH), 1, WALLPAPER_FOCUS_MAX_ZOOM);
  const cx = clamp(x + w / 2, 0, 1);
  const cy = clamp(y + h / 2, 0, 1);
  // Re-clamp via visible rect so edges snap cleanly.
  return focusFromCenterZoom(cx, cy, zoom, mw, mh, viewAspect);
}

export function focusFromCenterZoom(
  cx: number,
  cy: number,
  zoom: number,
  mediaW: number,
  mediaH: number,
  viewAspect: number,
): WallpaperFocus {
  const draft = normalizeWallpaperFocus({ cx, cy, zoom });
  const vis = wallpaperVisibleRect(mediaW, mediaH, viewAspect, draft);
  return normalizeWallpaperFocus({
    cx: vis.x + vis.w / 2,
    cy: vis.y + vis.h / 2,
    zoom: draft.zoom,
  });
}

/**
 * Absolute layout of the media element so the focus slice fills the viewport.
 * Caller applies as style on <img>/<video> inside an overflow:hidden container.
 */
export function wallpaperMediaLayout(
  mediaW: number,
  mediaH: number,
  viewW: number,
  viewH: number,
  focus: WallpaperFocus,
): WallpaperMediaLayout {
  const mw = Math.max(1, mediaW);
  const mh = Math.max(1, mediaH);
  const vw = Math.max(1, viewW);
  const vh = Math.max(1, viewH);
  const f = normalizeWallpaperFocus(focus);
  const scale = Math.max(vw / mw, vh / mh) * f.zoom;
  const width = mw * scale;
  const height = mh * scale;
  let left = vw / 2 - f.cx * width;
  let top = vh / 2 - f.cy * height;
  // Keep media covering the viewport (no letterbox).
  left = clamp(left, vw - width, 0);
  top = clamp(top, vh - height, 0);
  return { width, height, left, top };
}

/** object-fit: contain placement of media inside a stage. */
export function containRect(
  mediaW: number,
  mediaH: number,
  stageW: number,
  stageH: number,
): Rect {
  const mw = Math.max(1, mediaW);
  const mh = Math.max(1, mediaH);
  const sw = Math.max(1, stageW);
  const sh = Math.max(1, stageH);
  const scale = Math.min(sw / mw, sh / mh);
  const w = mw * scale;
  const h = mh * scale;
  return {
    x: (sw - w) / 2,
    y: (sh - h) / 2,
    w,
    h,
  };
}

/** Map a visible media rect onto the stage (media already contain-fitted). */
export function visibleRectToStageFrame(
  vis: WallpaperVisibleRect,
  mediaBox: Rect,
): Rect {
  return {
    x: mediaBox.x + vis.x * mediaBox.w,
    y: mediaBox.y + vis.y * mediaBox.h,
    w: vis.w * mediaBox.w,
    h: vis.h * mediaBox.h,
  };
}

/**
 * After the user drags the stage-space frame, convert back to focus.
 * Frame is clamped to the media box; aspect is re-imposed from viewAspect.
 */
export function focusFromStageFrame(
  frame: Rect,
  mediaBox: Rect,
  mediaW: number,
  mediaH: number,
  viewAspect: number,
  zoom: number,
): WallpaperFocus {
  if (mediaBox.w <= 0 || mediaBox.h <= 0) {
    return normalizeWallpaperFocus({ zoom });
  }
  // Center of dragged frame → media-normalized center; keep caller's zoom.
  const cx = (frame.x + frame.w / 2 - mediaBox.x) / mediaBox.w;
  const cy = (frame.y + frame.h / 2 - mediaBox.y) / mediaBox.h;
  return focusFromCenterZoom(cx, cy, zoom, mediaW, mediaH, viewAspect);
}

/**
 * Resize frame around its center by a zoom delta (wheel / pinch).
 * zoomNext is absolute zoom, not delta.
 */
export function focusWithZoom(
  focus: WallpaperFocus,
  zoomNext: number,
  mediaW: number,
  mediaH: number,
  viewAspect: number,
): WallpaperFocus {
  return focusFromCenterZoom(
    focus.cx,
    focus.cy,
    zoomNext,
    mediaW,
    mediaH,
    viewAspect,
  );
}
