/**
 * Copy an image (from URL / data URL / asset protocol) onto the system clipboard.
 * ClipboardItem typically requires image/png — we convert when needed.
 */

import { releaseImageSrc, resolveImageSrc } from "@/lib/imageSrc";

export type CopyImageResult =
  | { ok: true }
  | { ok: false; reason: "unsupported" | "fetch" | "encode" | "write" };

function canWriteImage(): boolean {
  return (
    typeof navigator !== "undefined" &&
    !!navigator.clipboard &&
    typeof ClipboardItem !== "undefined"
  );
}

/** Draw arbitrary image blob into a PNG blob (clipboard-friendly). */
async function blobToPng(blob: Blob): Promise<Blob> {
  if (blob.type === "image/png") return blob;

  const bitmap = await createImageBitmap(blob);
  try {
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no 2d context");
    ctx.drawImage(bitmap, 0, 0);
    const png = await new Promise<Blob | null>((resolve) =>
      canvas.toBlob((b) => resolve(b), "image/png"),
    );
    if (!png) throw new Error("toBlob failed");
    return png;
  } finally {
    bitmap.close();
  }
}

/**
 * 在用户手势的同步路径内发起图片写入。
 *
 * `resolveSrc` 在调用时同步执行，其等待被放进 ClipboardItem 的载荷：先 `await`
 * 解析地址再写入，手势已经过期，WebKit 会拒绝 clipboard.write。
 *
 * `releaseSrc` 在载荷用完地址后立即调用，让临时资源（如 blob URL）的寿命与
 * 载荷一致，无需调用方额外协调写入失败的情况。
 */
async function writeImageInGesture(
  resolveSrc: () => Promise<string | null>,
  releaseSrc?: (src: string) => void,
): Promise<CopyImageResult> {
  if (!canWriteImage()) return { ok: false, reason: "unsupported" };

  let reason: "fetch" | "encode" | "write" = "write";
  // ClipboardItem may receive the pending data — write() must run during the click.
  const png = (async (): Promise<Blob> => {
    let src: string | null = null;
    try {
      try {
        src = await resolveSrc();
      } catch (error) {
        reason = "fetch";
        throw error;
      }
      if (!src) {
        reason = "fetch";
        throw new Error("image source unavailable");
      }

      let blob: Blob;
      try {
        const res = await fetch(src);
        if (!res.ok) throw new Error(`image fetch failed: ${res.status}`);
        blob = await res.blob();
      } catch (error) {
        reason = "fetch";
        throw error;
      }

      try {
        return await blobToPng(blob);
      } catch (error) {
        reason = "encode";
        throw error;
      }
    } finally {
      if (src) releaseSrc?.(src);
    }
  })();
  // write() 可能在载荷就绪前失败，避免派生 Promise 留下未处理的拒绝。
  void png.catch(() => {});

  try {
    await navigator.clipboard.write([
      new ClipboardItem({ "image/png": png }),
    ]);
    return { ok: true };
  } catch {
    return { ok: false, reason };
  }
}

/**
 * Copy image at `src` (viewable URL) to clipboard as PNG.
 */
export async function copyImageFromSrc(src: string): Promise<CopyImageResult> {
  return writeImageInGesture(async () => src);
}

/**
 * Copy image from a local absolute path (or already-viewable URL).
 *
 * 本地路径解析要走 IPC 读文件，解析等待必须发生在手势内的写入载荷里。
 */
export async function copyImageFromPath(
  pathOrUrl: string,
): Promise<CopyImageResult> {
  return writeImageInGesture(
    () => resolveImageSrc(pathOrUrl),
    releaseImageSrc,
  );
}
