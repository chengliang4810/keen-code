import { Button } from "@appica/ui-react/button";
import { Card } from "@/components/ui/card";
import { Thumbnail } from "@appica/ui-react/thumbnail";
/**
 * File / folder card for chat history and composer.
 * Images: square thumb, click → lightbox, context menu includes copy image.
 * Other files: click → OS open; right-click → context menu.
 */

import { useEffect, useRef, useState } from "react";
import type { Attachment } from "@/lib/attachments";
import { isImageAttachment, isRemoteAttachment, pathExt } from "@/lib/attachments";
import * as api from "@/lib/api";
import {
  releaseImageSrc,
  resolveImageSrc,
  resolveImageSrcSync,
} from "@/lib/imageSrc";
import { copyImageFromPath } from "@/lib/copyImage";
import { useImageViewerOptional } from "@/components/ImageViewer";
import {
  IconClose,
  IconCopy,
  IconExternalLink,
  IconFileText,
  IconFolder,
  IconPaperclip,
  IconRefresh,
} from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
import { ContextMenu, type ContextMenuItem } from "@/components/ContextMenu";

export interface AttachmentCardLabels {
  open: string;
  reveal: string;
  copyPath: string;
  copyImage: string;
  addToComposer: string;
  remove?: string;
  viewImage?: string;
  retry?: string;
  uploading?: string;
  failed?: string;
}

interface AttachmentCardProps {
  attachment: Attachment;
  labels: AttachmentCardLabels;
  /** Compact chip-style (composer) vs message card */
  variant?: "card" | "chip";
  onAddToComposer?: (a: Attachment) => void;
  onRemove?: (a: Attachment) => void;
  onRetry?: (a: Attachment) => void;
  /**
   * Sibling image paths for lightbox prev/next.
   * When omitted, only the current image is shown.
   */
  galleryPaths?: string[];
}

export function AttachmentCard({
  attachment,
  labels,
  variant = "card",
  onAddToComposer,
  onRemove,
  onRetry,
  galleryPaths,
}: AttachmentCardProps) {
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const isImg = isImageAttachment(attachment);
  const remote = isRemoteAttachment(attachment);
  const displayRef = remote
    ? attachment.previewUrl ?? attachment.path
    : attachment.path;
  const uploadStatus = attachment.uploadStatus ?? "ready";
  const uploadLabel =
    uploadStatus === "uploading"
      ? labels.uploading ?? "Uploading"
      : uploadStatus === "failed"
        ? labels.failed ?? "Upload failed"
        : null;
  // 普通文件副标题优先使用扩展名；无扩展名时回落到 MIME 的子类型，便于远程附件识别。
  const fileTypeLabel = (() => {
    if (attachment.isDir || isImg) return null;
    const extension = pathExt(attachment.name || attachment.path);
    if (extension) return extension.toUpperCase();
    const mime = attachment.contentType?.split(";", 1)[0]?.trim();
    return mime ? (mime.split("/").pop() || mime).toUpperCase() : null;
  })();
  const [thumbSrc, setThumbSrc] = useState<string | null>(() =>
    isImg
      ? resolveImageSrcSync(remote ? attachment.previewUrl ?? "" : attachment.path)
      : null,
  );
  const fallbackSrcRef = useRef<string | null>(null);
  const fallbackLoadingRef = useRef(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const viewer = useImageViewerOptional();

  useEffect(() => {
    if (fallbackSrcRef.current) {
      releaseImageSrc(fallbackSrcRef.current);
      fallbackSrcRef.current = null;
    }
    if (!isImg) {
      setThumbSrc(null);
      return;
    }
    // Sync resolve + cache: avoid empty→thumb height flash in the thread.
    setThumbSrc(
      resolveImageSrcSync(remote ? attachment.previewUrl ?? "" : attachment.path),
    );
    return () => {
      if (fallbackSrcRef.current) releaseImageSrc(fallbackSrcRef.current);
      fallbackSrcRef.current = null;
    };
  }, [attachment.path, attachment.previewUrl, isImg, remote]);

  const recoverThumbnail = async () => {
    if (
      !isImg ||
      (!api.isTauri() && !remote) ||
      fallbackLoadingRef.current ||
      fallbackSrcRef.current
  )
      return;
    if (remote) {
      setThumbSrc(attachment.previewUrl ?? null);
      return;
    }
    fallbackLoadingRef.current = true;
    try {
      const src = await resolveImageSrc(attachment.path);
      if (!src) {
        setThumbSrc(null);
        return;
      }
      if (!rootRef.current) {
        releaseImageSrc(src);
        return;
      }
      fallbackSrcRef.current = src;
      setThumbSrc(src);
    } catch {
      setThumbSrc(null);
    } finally {
      fallbackLoadingRef.current = false;
    }
  };

  const openPath = async () => {
    try {
      if (remote) {
        if (attachment.previewUrl && typeof window !== "undefined") {
          window.open(attachment.previewUrl, "_blank", "noopener,noreferrer");
        }
      } else if (api.isTauri()) {
        await api.pathOpen(attachment.path);
      }
    } catch (e) {
      console.error(e);
    }
  };

  const revealPath = async () => {
    try {
      if (!remote && api.isTauri()) await api.pathReveal(attachment.path);
    } catch (e) {
      console.error(e);
    }
  };

  const copyPath = async () => {
    try {
      await navigator.clipboard.writeText(displayRef);
    } catch {
      /* ignore */
    }
  };

  const copyImage = async () => {
    if (!remote) await copyImageFromPath(attachment.path);
  };

  const openInViewer = () => {
    const gallery =
      galleryPaths && galleryPaths.length > 0
        ? galleryPaths
        : [displayRef];
    const idx = Math.max(0, gallery.indexOf(displayRef));
    viewer.open(
      gallery.map((p) => ({ src: p, title: p.split(/[/\\]/).pop() })),
      idx,
    );
  };

  const onPrimaryClick = () => {
    if (isImg) openInViewer();
    else void openPath();
  };

  const menuItems: ContextMenuItem[] = [
    {
      id: "open",
      label: isImg && labels.viewImage ? labels.viewImage : labels.open,
      icon: isImg ? <IconFileText size={16} /> : <IconExternalLink size={16} />,
      onClick: () => {
        if (isImg) openInViewer();
        else void openPath();
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
  ];
  if (isImg && !remote) {
    menuItems.push({
      id: "copy-image",
      label: labels.copyImage,
      icon: <IconCopy size={16} />,
      onClick: () => {
        void copyImage();
      },
    });
  }
  menuItems.push({
    id: "copy-path",
    label: labels.copyPath,
    icon: <IconCopy size={16} />,
    onClick: () => {
      void copyPath();
    },
  });
  if (onAddToComposer) {
    menuItems.push({
      id: "add",
      label: labels.addToComposer,
      icon: <IconPaperclip size={16} />,
      onClick: () => onAddToComposer(attachment),
    });
  }

  if (variant === "chip") {
    return (
      <Tip label={displayRef}>
        <span
          ref={rootRef as unknown as React.RefObject<HTMLSpanElement>}
          className={
            "attach-chip" +
            (attachment.isDir ? " attach-chip--dir" : "") +
            (isImg ? " attach-chip--image" : "") +
            (uploadStatus !== "ready" ? ` attach-chip--${uploadStatus}` : "")
          }
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setMenu({ x: e.clientX, y: e.clientY });
          }}
        >
          <Button size="md"
            type="button"
            variant="ghost"
            className="attach-chip__main"
            onClick={onPrimaryClick}
          >
            {isImg && thumbSrc ? (
              <img
                className="attach-chip__thumb"
                src={thumbSrc}
                alt={attachment.name}
                draggable={false}
                onError={() => void recoverThumbnail()}
              />
            ) : (
              <>
                <span className="attach-chip__icon" aria-hidden>
                  {attachment.isDir ? (
                    <IconFolder size={14} />
                  ) : (
                    <IconFileText size={14} />
                  )}
                </span>
                <span className="attach-chip__meta">
                  <span className="attach-chip__name">{attachment.name}</span>
                  {fileTypeLabel ? (
                    <span className="attach-chip__type">{fileTypeLabel}</span>
                  ) : null}
                </span>
              </>
            )}
          </Button>
          {uploadStatus !== "ready" ? (
            <span
              className="attach-chip__status"
              role={uploadStatus === "failed" ? "alert" : "status"}
              aria-label={uploadLabel ?? undefined}
            >
              {uploadStatus === "uploading"
                ? `${Math.round((attachment.uploadProgress ?? 0) * 100)}%`
                : uploadLabel}
            </span>
          ) : null}
          {uploadStatus === "failed" && onRetry ? (
            <Tip label={labels.retry ?? "Retry"}>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                className="attach-chip__retry"
                aria-label={labels.retry ?? "Retry"}
                onClick={() => onRetry(attachment)}
              >
                <IconRefresh size={12} />
              </Button>
            </Tip>
          ) : null}
          {onRemove && labels.remove ? (
            <Tip label={labels.remove}>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                className="attach-chip__x"
                aria-label={labels.remove}
                onClick={() => onRemove(attachment)}
              >
                <IconClose size={11} />
              </Button>
            </Tip>
          ) : onRemove ? (
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              className="attach-chip__x"
              aria-label={labels.remove}
              onClick={() => onRemove(attachment)}
            >
              <IconClose size={11} />
            </Button>
          ) : null}
          <ContextMenu
            open={!!menu}
            x={menu?.x ?? 0}
            y={menu?.y ?? 0}
            onClose={() => setMenu(null)}
            items={menuItems}
          />
        </span>
      </Tip>
    );
  }

  return (
    <Tip label={displayRef}>
    <Card
      inset={false}
      contentProps={{ className: "p-0" }}
      render={<div ref={rootRef} />}
      className={
        "att-card" +
        (attachment.isDir ? " att-card--dir" : "") +
        (isImg ? " att-card--image" : "") +
        (uploadStatus !== "ready" ? ` att-card--${uploadStatus}` : "")
      }
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      <Button
        type="button"
        variant="ghost"
        size="md"
        className={"att-card__btn" + (isImg ? " att-card__btn--image" : "")}
        onClick={onPrimaryClick}
      >
        {isImg ? (
          thumbSrc ? (
            <Thumbnail
              size="md"
              src={thumbSrc}
              alt={attachment.name}
              onLoadingStatusChange={(status) => { if (status === "error") void recoverThumbnail(); }}
            />
          ) : (
            <Thumbnail size="md" variant="icon-soft">
              <IconPaperclip size={18} />
            </Thumbnail>
          )
        ) : (
          <>
            <Thumbnail size="md" variant={attachment.isDir ? "icon-primary" : "icon-soft"} aria-hidden>
              {attachment.isDir ? (
                <IconFolder size={14} />
              ) : (
                <IconFileText size={14} />
              )}
            </Thumbnail>
            <span className="att-card__meta">
              <span className="att-card__name">
                {attachment.name}
              </span>
              {fileTypeLabel ? (
                <span className="att-card__type">{fileTypeLabel}</span>
              ) : null}
            </span>
          </>
        )}
      </Button>
      {uploadStatus !== "ready" ? (
        <div
          className="att-card__status"
          role={uploadStatus === "failed" ? "alert" : "status"}
          aria-label={uploadLabel ?? undefined}
        >
          {uploadStatus === "uploading"
            ? `${Math.round((attachment.uploadProgress ?? 0) * 100)}%`
            : uploadLabel}
          {uploadStatus === "failed" && onRetry ? (
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={labels.retry ?? "Retry"}
              onClick={() => onRetry(attachment)}
            >
              <IconRefresh size={12} />
            </Button>
          ) : null}
        </div>
      ) : null}
      <ContextMenu
        open={!!menu}
        x={menu?.x ?? 0}
        y={menu?.y ?? 0}
        onClose={() => setMenu(null)}
        items={menuItems}
      />
    </Card>
    </Tip>
  );
}
