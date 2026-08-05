/**
 * Inline video card for chat: session-relative / local paths.
 * Plays via Tauri media:// (Range); right-click: open / reveal / copy path.
 *
 * Frame always reserves a non-zero size (default 16:9, then natural ratio)
 * so streaming remounts / metadata decode never collapse scrollHeight —
 * that thrash + stick-to-bottom follow was the chat flicker with video output.
 */

import {
  memo,
  useCallback,
  useEffect,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import * as api from "@/lib/api";
import { pathToPreviewUrl } from "@/lib/filePreviewSrc";
import { IconCopy, IconExternalLink, IconFolder } from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
import { ContextMenu, type ContextMenuItem } from "@/components/ContextMenu";
import { createT, type Locale } from "@/i18n";
import { pathBasename } from "@/lib/attachments";

export interface VideoUiLabels {
  open: string;
  reveal: string;
  copyPath: string;
  loadError?: string;
}

interface VideoUiProps {
  /** Absolute filesystem path (preferred) or already-viewable URL. */
  src: string;
  /** Absolute path for open/reveal/copy (when known). */
  path?: string;
  title?: string;
  className?: string;
  style?: CSSProperties;
  labels: VideoUiLabels;
  extraMenu?: ReactNode;
}

/** Chat card outer caps (px). */
const CARD_MAX_W = 360;
const CARD_MAX_H = 240;
/** Placeholder ratio before metadata is known (most agent clips are 16:9). */
const DEFAULT_AR = 16 / 9;

/** Remember natural ratios so remounts keep the right box. */
const aspectCache = new Map<string, number>();
/** Remember resolved media URLs so remounts skip the loading→ready height flip. */
const srcCache = new Map<string, string>();

function isLocalFsPath(path: string | undefined): path is string {
  if (!path) return false;
  if (path.startsWith("http://") || path.startsWith("https://")) return false;
  if (path.startsWith("data:") || path.startsWith("blob:")) return false;
  if (path.startsWith("asset:") || path.includes("asset.localhost")) return false;
  if (path.startsWith("media:") || path.includes("media.localhost")) return false;
  return path.startsWith("/") || /^[A-Za-z]:[\\/]/.test(path);
}

function isViewableVideoSrc(src: string): boolean {
  return (
    src.startsWith("http://") ||
    src.startsWith("https://") ||
    src.startsWith("data:") ||
    src.startsWith("blob:") ||
    src.startsWith("asset:") ||
    src.startsWith("media:") ||
    src.includes("asset.localhost") ||
    src.includes("media.localhost")
  );
}

function cacheKey(src: string, path?: string): string {
  return path || src;
}

function readCachedAr(src: string, path?: string): number | null {
  const k = cacheKey(src, path);
  return aspectCache.get(k) ?? aspectCache.get(src) ?? null;
}

function readCachedSrc(src: string): string | null {
  if (isViewableVideoSrc(src)) return src;
  return srcCache.get(src) ?? null;
}

/** Fit natural ratio into max box; returns width px + aspect ratio. */
function fitCardBox(ar: number): { widthPx: number; ar: number } {
  const ratio = ar > 0 && Number.isFinite(ar) ? ar : DEFAULT_AR;
  let widthPx = CARD_MAX_W;
  let heightPx = widthPx / ratio;
  if (heightPx > CARD_MAX_H) {
    heightPx = CARD_MAX_H;
    widthPx = heightPx * ratio;
  }
  return { widthPx, ar: ratio };
}

export const VideoUi = memo(function VideoUi({
  src,
  path,
  title = "",
  className,
  style,
  labels,
  extraMenu,
}: VideoUiProps) {
  const localPath = isLocalFsPath(path)
    ? path
    : isLocalFsPath(src)
      ? src
      : undefined;

  const [resolvedSrc, setResolvedSrc] = useState<string | null>(() =>
    readCachedSrc(src),
  );
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [error, setError] = useState(false);
  const [aspectRatio, setAspectRatio] = useState<number>(
    () => readCachedAr(src, path) ?? DEFAULT_AR,
  );

  const applyNaturalSize = useCallback(
    (nw: number, nh: number) => {
      if (!(nw > 0 && nh > 0)) return;
      const ar = nw / nh;
      aspectCache.set(cacheKey(src, path), ar);
      aspectCache.set(src, ar);
      if (localPath) aspectCache.set(localPath, ar);
      setAspectRatio(ar);
    },
    [src, path, localPath],
  );

  useEffect(() => {
    let cancelled = false;
    setError(false);
    const cachedAr = readCachedAr(src, path);
    if (cachedAr != null) setAspectRatio(cachedAr);
    else setAspectRatio(DEFAULT_AR);

    if (isViewableVideoSrc(src)) {
      setResolvedSrc(src);
      return;
    }
    const cached = srcCache.get(src);
    if (cached) {
      setResolvedSrc(cached);
      return;
    }
    // Keep previous frame size; only clear resolved when we have nothing cached
    // so a path resolve does not collapse the reserved box.
    void pathToPreviewUrl(src, "video").then((url) => {
      if (cancelled) return;
      if (url) {
        srcCache.set(src, url);
        setResolvedSrc(url);
      } else {
        setResolvedSrc(null);
        setError(true);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [src, path]);

  const openExternal = async () => {
    if (!localPath || !api.isTauri()) return;
    try {
      await api.pathOpen(localPath);
    } catch (e) {
      console.error(e);
    }
  };

  const revealPath = async () => {
    if (!localPath || !api.isTauri()) return;
    try {
      await api.pathReveal(localPath);
    } catch (e) {
      console.error(e);
    }
  };

  const copyPath = async () => {
    if (!localPath) return;
    try {
      await navigator.clipboard.writeText(localPath);
    } catch {
      /* ignore */
    }
  };

  const displayTitle = title || (localPath ? pathBasename(localPath) : "");

  const menuItems: ContextMenuItem[] = [];
  if (localPath) {
    menuItems.push(
      {
        id: "open",
        label: labels.open,
        icon: <IconExternalLink size={16} />,
        onClick: () => {
          void openExternal();
        },
      },
      {
        id: "reveal",
        label: labels.reveal,
        icon: <IconFolder size={16} />,
        onClick: () => {
          void revealPath();
        },
      },
      {
        id: "copy-path",
        label: labels.copyPath,
        icon: <IconCopy size={16} />,
        onClick: () => {
          void copyPath();
        },
      },
    );
  }

  const ar =
    aspectRatio > 0 && Number.isFinite(aspectRatio) ? aspectRatio : DEFAULT_AR;
  const box = fitCardBox(ar);
  // Size the outer card; aspect-ratio lives on the stage so caption height
  // does not fight the reserved media box during stream remounts.
  const cardStyle: CSSProperties = {
    ...style,
    width: box.widthPx,
    maxWidth: "100%",
  };
  const stageStyle: CSSProperties = {
    aspectRatio: `${box.ar}`,
  };

  const stateClass = error
    ? "is-error"
    : resolvedSrc
      ? "is-ready"
      : "is-pending";

  return (
    <>
      <div
        className={
          "md-body__video-card " +
          stateClass +
          (className ? " " + className : "")
        }
        style={cardStyle}
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        <div className="md-body__video-card__stage" style={stageStyle}>
          {error ? (
            <div className="md-body__video-card__error">
              <span>{labels.loadError || "Failed to load video"}</span>
              {localPath && (
                <button
                  type="button"
                  className="md-body__video-card__btn"
                  onClick={() => void openExternal()}
                >
                  {labels.open}
                </button>
              )}
            </div>
          ) : resolvedSrc ? (
            <video
              className="md-body__video-card__el"
              src={resolvedSrc}
              controls
              playsInline
              preload="metadata"
              onLoadedMetadata={(e) => {
                const el = e.currentTarget;
                applyNaturalSize(el.videoWidth, el.videoHeight);
              }}
              onError={() => setError(true)}
            />
          ) : (
            <div className="md-body__video-card__placeholder" aria-hidden>
              <span className="md-body__video-card__name">
                {displayTitle || "…"}
              </span>
            </div>
          )}
        </div>
        {displayTitle ? (
          <Tip label={localPath || displayTitle}>
            <div className="md-body__video-card__caption">{displayTitle}</div>
          </Tip>
        ) : null}
      </div>
      <ContextMenu
        open={!!menu}
        x={menu?.x ?? 0}
        y={menu?.y ?? 0}
        onClose={() => setMenu(null)}
        items={menuItems}
        extra={extraMenu}
      />
    </>
  );
});

export function videoUiLabels(locale: Locale): VideoUiLabels {
  const tr = createT(locale);
  return {
    open: tr("attach.open"),
    reveal: tr("attach.reveal"),
    copyPath: tr("attach.copyPath"),
    loadError: tr("video.loadError"),
  };
}
