/**
 * Video wallpaper clip range — in-source seconds, no re-encode.
 * Applied at playback by seeking within [start, end) and looping.
 */

/** Minimum selectable clip length (seconds). */
export const WALLPAPER_CLIP_MIN_DURATION = 0.5;

export interface WallpaperClip {
  /** Inclusive start time in seconds from media start. */
  start: number;
  /** Exclusive-ish end time in seconds (playback rewinds at/after this). */
  end: number;
}

export function clamp(n: number, min: number, max: number): number {
  if (!Number.isFinite(n)) return min;
  return Math.min(max, Math.max(min, n));
}

/**
 * Normalize a clip against media duration.
 * Returns null when the range covers (essentially) the full media —
 * callers omit null from persistence.
 */
export function normalizeWallpaperClip(
  value: Partial<WallpaperClip> | null | undefined,
  duration: number,
): WallpaperClip | null {
  if (!Number.isFinite(duration) || duration <= 0) return null;
  if (!value || typeof value !== "object") return null;

  const minLen = Math.min(WALLPAPER_CLIP_MIN_DURATION, duration);
  let start = Number(value.start);
  let end = Number(value.end);
  if (!Number.isFinite(start)) start = 0;
  if (!Number.isFinite(end)) end = duration;

  start = clamp(start, 0, Math.max(0, duration - minLen));
  end = clamp(end, minLen, duration);
  if (end - start < minLen) {
    // Prefer extending end; if not possible, pull start back.
    if (start + minLen <= duration) {
      end = start + minLen;
    } else {
      start = Math.max(0, duration - minLen);
      end = duration;
    }
  }

  // Full-span (within 30ms) → treat as no clip.
  if (start <= 0.03 && end >= duration - 0.03) return null;

  // Round to ms to keep meta stable.
  return {
    start: Math.round(start * 1000) / 1000,
    end: Math.round(end * 1000) / 1000,
  };
}

export function parseWallpaperClip(
  raw: unknown,
  duration?: number,
): WallpaperClip | null {
  if (!raw || typeof raw !== "object") return null;
  const v = raw as Record<string, unknown>;
  const start = typeof v.start === "number" ? v.start : NaN;
  const end = typeof v.end === "number" ? v.end : NaN;
  if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) {
    return null;
  }
  if (typeof duration === "number" && duration > 0) {
    return normalizeWallpaperClip({ start, end }, duration);
  }
  // Without duration, still accept a basic ordered range (boot / meta mirror).
  if (start < 0 || end - start < WALLPAPER_CLIP_MIN_DURATION * 0.5) return null;
  return {
    start: Math.round(start * 1000) / 1000,
    end: Math.round(end * 1000) / 1000,
  };
}

export function clipsEqual(
  a: WallpaperClip | null | undefined,
  b: WallpaperClip | null | undefined,
): boolean {
  if (!a && !b) return true;
  if (!a || !b) return false;
  return (
    Math.abs(a.start - b.start) < 1e-3 && Math.abs(a.end - b.end) < 1e-3
  );
}

/** mm:ss or m:ss.d for short clips. */
export function formatClipTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const s = Math.floor(seconds % 60);
  const m = Math.floor(seconds / 60);
  const frac = seconds - Math.floor(seconds);
  // Show tenths under 10 minutes for finer scrubbing feedback.
  if (m < 10 && frac >= 0.05) {
    const tenths = Math.floor(frac * 10);
    return `${m}:${String(s).padStart(2, "0")}.${tenths}`;
  }
  return `${m}:${String(s).padStart(2, "0")}`;
}

/**
 * Keep a playing <video> inside [start, end).
 * Call from timeupdate; seeks are cheap vs re-encoding.
 */
export function enforceVideoClip(
  video: HTMLVideoElement,
  clip: WallpaperClip | null | undefined,
): void {
  if (!clip) return;
  const start = clip.start;
  const end = clip.end;
  if (!(end > start)) return;
  const t = video.currentTime;
  // Small epsilon so we don't thrash near the boundary.
  if (t >= end - 0.03 || t < start - 0.05) {
    try {
      video.currentTime = start;
    } catch {
      /* ignore seek errors mid-load */
    }
  }
}
