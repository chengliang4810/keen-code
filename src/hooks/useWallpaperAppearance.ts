import { useCallback, useEffect, useRef, useState } from "react";
import { localizeUiError } from "@/lib/session";
import type { Locale } from "@/i18n";
import {
  applyWallpaperBlurToDocument,
  applyWallpaperFlag,
  applyWallpaperScrimToDocument,
  clearWallpaper,
  DEFAULT_WALLPAPER_BLUR,
  DEFAULT_WALLPAPER_SCRIM,
  loadWallpaperBlur,
  loadWallpaperRecord,
  loadWallpaperScrim,
  saveWallpaper,
  saveWallpaperAdjust,
  saveWallpaperBlur,
  saveWallpaperMediaSize,
  saveWallpaperScrim,
  type WallpaperClip,
  type WallpaperFocus,
  type WallpaperRecord,
} from "@/lib/themeSkin";

export type WallpaperAppearanceErrorSource =
  | "load"
  | "clear"
  | "save"
  | "adjust"
  | "media-size";

export interface UseWallpaperAppearanceOptions {
  /** Used when the hook formats storage errors before handing them to the caller. */
  locale?: Locale;
  /** Keep presentation policy (toast vs. page error) in the consuming layer. */
  onError?: (
    message: string,
    source: WallpaperAppearanceErrorSource,
    cause: unknown,
  ) => void;
}

export interface WallpaperAppearanceAdjust {
  focus: WallpaperFocus;
  clip: WallpaperClip | null;
  duration?: number;
}

export function useWallpaperAppearance({
  locale = "zh",
  onError,
}: UseWallpaperAppearanceOptions = {}) {
  const [wallpaperRecord, setWallpaperRecord] =
    useState<WallpaperRecord | null>(null);
  const [wallpaperUrl, setWallpaperUrl] = useState<string | null>(null);
  const wallpaperUrlRef = useRef<string | null>(null);
  const [wallpaperScrim, setWallpaperScrim] = useState(() =>
    loadWallpaperScrim(localStorage),
  );
  const [wallpaperBlur, setWallpaperBlur] = useState(() =>
    loadWallpaperBlur(localStorage),
  );

  const localeRef = useRef(locale);
  localeRef.current = locale;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  const reportError = useCallback(
    (source: WallpaperAppearanceErrorSource, cause: unknown) => {
      onErrorRef.current?.(localizeUiError(cause, localeRef.current), source, cause);
    },
    [],
  );

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const record = await loadWallpaperRecord();
        if (cancelled || !record) return;
        const url = URL.createObjectURL(record.blob);
        if (cancelled) {
          URL.revokeObjectURL(url);
          return;
        }
        wallpaperUrlRef.current = url;
        setWallpaperRecord(record);
        setWallpaperUrl(url);
      } catch (cause) {
        if (!cancelled) reportError("load", cause);
      }
    })();
    return () => {
      cancelled = true;
      const url = wallpaperUrlRef.current;
      if (url) {
        URL.revokeObjectURL(url);
        wallpaperUrlRef.current = null;
      }
    };
  }, [reportError]);

  useEffect(() => {
    applyWallpaperFlag(wallpaperUrl !== null);
  }, [wallpaperUrl]);

  useEffect(() => {
    applyWallpaperScrimToDocument(wallpaperScrim);
  }, [wallpaperScrim]);

  useEffect(() => {
    applyWallpaperBlurToDocument(wallpaperBlur);
  }, [wallpaperBlur]);

  const replaceWallpaperUrl = useCallback((url: string | null) => {
    const previous = wallpaperUrlRef.current;
    if (previous) URL.revokeObjectURL(previous);
    wallpaperUrlRef.current = url;
    setWallpaperUrl(url);
  }, []);

  const applyWallpaperChoice = useCallback(
    async (record: WallpaperRecord | null) => {
      if (!record) {
        try {
          await clearWallpaper();
        } catch (cause) {
          reportError("clear", cause);
          return;
        }
        replaceWallpaperUrl(null);
        setWallpaperRecord(null);
        return;
      }

      // New uploads use cover-center unless the prepared record already has focus metadata.
      const toSave: WallpaperRecord = {
        ...record,
        focus: record.focus ?? undefined,
      };
      try {
        await saveWallpaper(toSave);
      } catch (cause) {
        reportError("save", cause);
        return;
      }
      const url = URL.createObjectURL(toSave.blob);
      replaceWallpaperUrl(url);
      setWallpaperRecord(toSave);
    },
    [replaceWallpaperUrl, reportError],
  );

  const applyWallpaperAdjustChoice = useCallback(
    async (patch: WallpaperAppearanceAdjust) => {
      try {
        const meta = await saveWallpaperAdjust({
          focus: patch.focus,
          clip: patch.clip,
          duration: patch.duration,
        });
        if (!meta) return;
        setWallpaperRecord((previous) => {
          if (!previous) return previous;
          const next: WallpaperRecord = {
            ...previous,
            focus: meta.focus,
            clip: meta.clip,
          };
          if (!meta.focus) delete next.focus;
          if (!meta.clip) delete next.clip;
          return next;
        });
      } catch (cause) {
        reportError("adjust", cause);
      }
    },
    [reportError],
  );

  /** Persist intrinsic media dimensions after the first successful decode. */
  const applyWallpaperMediaSize = useCallback(
    async (size: { w: number; h: number }) => {
      try {
        const meta = await saveWallpaperMediaSize(size.w, size.h);
        if (!meta) return;
        setWallpaperRecord((previous) => {
          if (!previous) return previous;
          if (previous.width === meta.width && previous.height === meta.height) {
            return previous;
          }
          return {
            ...previous,
            width: meta.width,
            height: meta.height,
          };
        });
      } catch (cause) {
        reportError("media-size", cause);
      }
    },
    [reportError],
  );

  const applyWallpaperScrimChoice = useCallback((value: number) => {
    saveWallpaperScrim(localStorage, value);
    applyWallpaperScrimToDocument(value);
    setWallpaperScrim(value);
  }, []);

  const applyWallpaperBlurChoice = useCallback((value: number) => {
    saveWallpaperBlur(localStorage, value);
    applyWallpaperBlurToDocument(value);
    setWallpaperBlur(value);
  }, []);

  const resetWallpaperAppearance = useCallback(() => {
    applyWallpaperScrimChoice(DEFAULT_WALLPAPER_SCRIM);
    applyWallpaperBlurChoice(DEFAULT_WALLPAPER_BLUR);
  }, [applyWallpaperBlurChoice, applyWallpaperScrimChoice]);

  return {
    wallpaperRecord,
    wallpaperUrl,
    wallpaperScrim,
    wallpaperBlur,
    applyWallpaperChoice,
    applyWallpaperAdjustChoice,
    applyWallpaperMediaSize,
    applyWallpaperScrimChoice,
    applyWallpaperBlurChoice,
    resetWallpaperAppearance,
  };
}
