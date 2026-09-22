/**
 * Resolve a previewable URL for local files.
 * Video, audio and images use Tauri's scoped `asset://` protocol.
 * HTML is rendered via HtmlBrowser (srcDoc) — not via this URL helper —
 * because `file://` is blocked inside Tauri's main webview iframes.
 * Small previews may still use data URLs.
 */

import type { FsReadResult } from "@/lib/api";
import { isTauri } from "@/lib/api";

/** 返回当前由前端富文本渲染器支持的文档类型。 */
export function isOfficeKind(kind: string): boolean {
  return (
    kind === "docx" ||
    kind === "xlsx" ||
    kind === "pptx" ||
    kind === "odf" ||
    kind === "office"
  );
}

/**
 * Absolute filesystem path → `file://` URL (encode segments; keep `/`).
 * Used for local HTML so relative CSS/JS resolve like a real browser tab.
 */
export function pathToFileUrl(absolutePath: string): string {
  let p = absolutePath.trim().replace(/\\/g, "/");
  if (!p) return "";
  // Windows drive → file:///C:/...
  const win = p.match(/^([A-Za-z]:)(\/.*)?$/);
  if (win) {
    const drive = win[1]!;
    const rest = win[2] || "/";
    const segs = rest.split("/").map((s) => (s ? encodeURIComponent(s) : ""));
    return `file:///${drive}${segs.join("/")}`;
  }
  if (!p.startsWith("/")) p = `/${p}`;
  const segs = p.split("/").map((s, i) => (i === 0 || !s ? "" : encodeURIComponent(s)));
  // segs[0] is empty before first / → join gives leading /
  return `file://${segs.join("/")}`;
}

/**
 * Convert absolute path → URL the webview can load.
 * `asset` protocol: scoped local-file streaming.
 * `file` protocol: local HTML.
 */
export async function pathToPreviewUrl(
  absolutePath: string,
  kind?: string,
): Promise<string | null> {
  if (!absolutePath) return null;
  // HTML is handled by HtmlBrowser (srcDoc); asset URL is only a fetch fallback
  if (!isTauri()) {
    if (kind === "html") return pathToFileUrl(absolutePath);
    return null;
  }
  try {
    const { convertFileSrc } = await import("@tauri-apps/api/core");
    return convertFileSrc(absolutePath);
  } catch {
    return null;
  }
}

export async function resolvePreviewSrc(
  preview: FsReadResult,
): Promise<string | null> {
  // HTML: don't put file:// into iframe src (blank). HtmlBrowser uses text/srcDoc.
  if (preview.kind === "html") {
    return null;
  }

  // 视频、音频和大图优先使用流式地址。
  if (preview.stream && preview.absolutePath && isTauri()) {
    const url = await pathToPreviewUrl(preview.absolutePath, preview.kind);
    if (url) return url;
  }

  if (preview.base64 && preview.mime) {
    return `data:${preview.mime};base64,${preview.base64}`;
  }

  return null;
}

/** 为 Word、Excel、PowerPoint 和 ODF 渲染器读取本地文件字节。 */
export async function fetchPreviewArrayBuffer(
  absolutePath: string,
  kind?: string,
): Promise<ArrayBuffer> {
  const url = await pathToPreviewUrl(absolutePath, kind);
  if (!url) {
    throw new Error("cannot resolve local file URL");
  }
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`failed to load file (${res.status})`);
  }
  return res.arrayBuffer();
}
