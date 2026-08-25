import { afterEach, describe, expect, it, vi } from "vitest";
const apiMocks = vi.hoisted(() => ({
  isTauri: vi.fn(() => false),
  readLocalImage: vi.fn(),
}));
vi.mock("@/lib/api", () => apiMocks);

import {
  clearImageSrcCache,
  isViewableSrc,
  releaseImageSrc,
  resolveImageSrc,
  resolveImageSrcSync,
} from "./imageSrc";

describe("isViewableSrc", () => {
  it("accepts http(s), data, blob and asset URLs", () => {
    expect(isViewableSrc("https://example.com/a.png")).toBe(true);
    expect(isViewableSrc("http://example.com/a.png")).toBe(true);
    expect(isViewableSrc("data:image/png;base64,xx")).toBe(true);
    expect(isViewableSrc("blob:http://localhost/1")).toBe(true);
    expect(isViewableSrc("asset://localhost/foo")).toBe(true);
    expect(isViewableSrc("media://localhost/foo")).toBe(false);
  });

  it("rejects bare paths", () => {
    expect(isViewableSrc("/Users/me/pic.png")).toBe(false);
    expect(isViewableSrc("C:\\Users\\me\\pic.png")).toBe(false);
  });
});

describe("resolveImageSrcSync", () => {
  afterEach(() => {
    clearImageSrcCache();
    apiMocks.isTauri.mockReturnValue(false);
    apiMocks.readLocalImage.mockReset();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("passes through already-viewable URLs without caching side effects", () => {
    expect(resolveImageSrcSync("https://cdn.example/a.jpg")).toBe(
      "https://cdn.example/a.jpg",
    );
  });

  it("returns null for empty / relative / ellipsis paths", () => {
    expect(resolveImageSrcSync("")).toBe(null);
    expect(resolveImageSrcSync("images/1.jpg")).toBe(null);
    expect(resolveImageSrcSync(".../foo/bar.png")).toBe(null);
  });

  it("returns null for absolute paths outside Tauri (no flash retry loop)", () => {
    // isTauri() is false in vitest — path must not throw, must resolve once.
    const a = resolveImageSrcSync("/Users/me/pic.png");
    const b = resolveImageSrcSync("/Users/me/pic.png");
    expect(a).toBe(null);
    expect(b).toBe(null);
  });

  it("uses typed binary IPC for local images and releases their Blob URLs", async () => {
    apiMocks.isTauri.mockReturnValue(true);
    apiMocks.readLocalImage.mockResolvedValue(new Uint8Array([1, 2, 3]).buffer);
    const blobTypes: string[] = [];
    vi.spyOn(URL, "createObjectURL").mockImplementation((value) => {
      blobTypes.push((value as Blob).type);
      return `blob:preview-${blobTypes.length}`;
    });
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});

    expect(await resolveImageSrc("/Users/me/pic.png")).toBe("blob:preview-1");
    expect(await resolveImageSrc("C:\\Users\\me\\vector.svg")).toBe(
      "blob:preview-2",
    );
    expect(await resolveImageSrc("\\\\server\\share\\photo.jpg")).toBe(
      "blob:preview-3",
    );
    expect(apiMocks.readLocalImage).toHaveBeenNthCalledWith(
      1,
      "/Users/me/pic.png",
    );
    expect(blobTypes).toEqual(["image/png", "image/svg+xml", "image/jpeg"]);
    releaseImageSrc("blob:preview-1");
    expect(revoke).toHaveBeenCalledWith("blob:preview-1");
  });
});
