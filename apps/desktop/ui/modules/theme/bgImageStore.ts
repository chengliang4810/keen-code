import { invoke } from "@tauri-apps/api/core";

const IMAGE_TYPES = ["image/jpeg", "image/png", "image/gif", "image/webp", "image/apng"];

export async function putBgImage(id: string, blob: Blob): Promise<void> {
  await invoke("storage_image_put", new Uint8Array(await blob.arrayBuffer()), {
    headers: { "x-rcode-image-id": id, "x-rcode-image-type": blob.type },
  });
}

export async function getBgImage(id: string): Promise<Blob | null> {
  const result = await invoke<ArrayBuffer | number[]>("storage_image_get", { id });
  const bytes = new Uint8Array(result);
  if (!bytes.length) return null;
  const type = IMAGE_TYPES[bytes[0]];
  if (!type) throw new Error("Invalid background image type.");
  return new Blob([bytes.subarray(1)], { type });
}

export async function deleteBgImage(id: string): Promise<void> {
  await invoke("storage_image_delete", { id });
}

const MAX_DIM = 2560;
const JPEG_QUALITY = 0.88;
const MAX_STATIC_BYTES = 30 * 1024 * 1024;
const MAX_ANIMATED_BYTES = 10 * 1024 * 1024;
const WEBP_SNIFF_BYTES = 64;

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

async function isAnimated(file: File): Promise<boolean> {
  const t = file.type.toLowerCase();
  if (t === "image/gif" || t === "image/apng") return true;
  if (t !== "image/webp") return false;
  const head = new Uint8Array(
    await file.slice(0, WEBP_SNIFF_BYTES).arrayBuffer(),
  );
  if (
    head.length < 30 ||
    head[0] !== 0x52 || head[1] !== 0x49 || head[2] !== 0x46 || head[3] !== 0x46 ||
    head[8] !== 0x57 || head[9] !== 0x45 || head[10] !== 0x42 || head[11] !== 0x50
  ) return false;
  if (
    head[12] === 0x56 && head[13] === 0x50 && head[14] === 0x38 && head[15] === 0x58
  ) {
    return (head[20] & 0x02) !== 0;
  }
  return false;
}

export async function importBgImageFromFile(file: File): Promise<{ id: string; blob: Blob }> {
  if (!file.type.startsWith("image/")) {
    throw new Error("This file isn't an image.");
  }
  const id = crypto.randomUUID();
  const animated = await isAnimated(file);
  const limit = animated ? MAX_ANIMATED_BYTES : MAX_STATIC_BYTES;
  if (file.size > limit) {
    const limitMb = Math.round(limit / 1024 / 1024);
    throw new Error(
      animated
        ? `Animated images are limited to ${limitMb} MB to keep things smooth. This one is ${formatBytes(file.size)}.`
        : `Images are limited to ${limitMb} MB. This one is ${formatBytes(file.size)}.`,
    );
  }
  if (animated) {
    const blob = file.slice(0, file.size, file.type);
    await putBgImage(id, blob);
    return { id, blob };
  }
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error("This image couldn't be decoded. Try a different file.");
  }
  const { width, height } = bitmap;
  const scale = Math.min(1, MAX_DIM / Math.max(width, height));
  const targetW = Math.max(1, Math.round(width * scale));
  const targetH = Math.max(1, Math.round(height * scale));
  try {
    const blob = await encodeJpeg(bitmap, targetW, targetH);
    await putBgImage(id, blob);
    return { id, blob };
  } finally {
    bitmap.close();
  }
}

async function encodeJpeg(
  bitmap: ImageBitmap,
  w: number,
  h: number,
): Promise<Blob> {
  if (typeof OffscreenCanvas !== "undefined") {
    const off = new OffscreenCanvas(w, h);
    const ctx = off.getContext("2d");
    if (!ctx) throw new Error("offscreen 2D context unavailable");
    ctx.drawImage(bitmap, 0, 0, w, h);
    return off.convertToBlob({ type: "image/jpeg", quality: JPEG_QUALITY });
  }
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("canvas 2D context unavailable");
  ctx.drawImage(bitmap, 0, 0, w, h);
  try {
    return await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob(
        (b) => (b ? resolve(b) : reject(new Error("failed to encode image"))),
        "image/jpeg",
        JPEG_QUALITY,
      );
    });
  } finally {
    canvas.width = 0;
    canvas.height = 0;
  }
}
