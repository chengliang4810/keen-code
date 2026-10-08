import { beforeEach, describe, expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import {
  deleteBgImage,
  getBgImage,
  putBgImage,
} from "@/modules/theme/bgImageStore";

describe("native background images", () => {
  beforeEach(() => invoke.mockReset());
  it("uploads raw bytes with an explicit identity and MIME type", async () => {
    await putBgImage(
      "image-id",
      new Blob([new Uint8Array([1, 2, 3])], { type: "image/png" }),
    );
    expect(invoke).toHaveBeenCalledExactlyOnceWith(
      "storage_image_put",
      new Uint8Array([1, 2, 3]),
      {
        headers: {
          "x-rcode-image-id": "image-id",
          "x-rcode-image-type": "image/png",
        },
      },
    );
  });
  it("restores binary images and treats absent images as empty", async () => {
    invoke.mockResolvedValueOnce(new Uint8Array([2, 1, 2, 3]).buffer);
    const blob = await getBgImage("image-id");
    if (!blob) throw new Error("Expected a stored image.");
    expect(blob.type).toBe("image/gif");
    expect(new Uint8Array(await blob.arrayBuffer())).toEqual(
      new Uint8Array([1, 2, 3]),
    );
    invoke.mockResolvedValueOnce([]);
    expect(await getBgImage("missing")).toBeNull();
    await deleteBgImage("image-id");
    expect(invoke).toHaveBeenLastCalledWith("storage_image_delete", {
      id: "image-id",
    });
  });
});
