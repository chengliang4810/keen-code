import { afterEach, describe, expect, it, vi } from "vitest";
const imageSrcMocks = vi.hoisted(() => ({
  releaseImageSrc: vi.fn(),
  resolveImageSrc: vi.fn(),
}));
vi.mock("@/lib/imageSrc", () => imageSrcMocks);

import { copyImageFromPath, copyImageFromSrc } from "./copyImage";

describe("copyImageFromSrc", () => {
  afterEach(() => {
    imageSrcMocks.releaseImageSrc.mockReset();
    imageSrcMocks.resolveImageSrc.mockReset();
    vi.unstubAllGlobals();
  });

  it("requests clipboard access before the image finishes loading", async () => {
    let finishFetch!: (response: Response) => void;
    const fetchPromise = new Promise<Response>((resolve) => {
      finishFetch = resolve;
    });
    const write = vi.fn(async (items: ClipboardItem[]) => {
      await items[0]!.getType("image/png");
    });

    vi.stubGlobal("fetch", vi.fn(() => fetchPromise));
    vi.stubGlobal("navigator", { clipboard: { write } });
    vi.stubGlobal(
      "ClipboardItem",
      class {
        readonly types = ["image/png"];
        constructor(private readonly data: Record<string, Promise<Blob>>) {}
        getType(type: string) {
          return this.data[type]!;
        }
      },
    );

    const result = copyImageFromSrc("blob:preview");
    expect(write).toHaveBeenCalledOnce();

    finishFetch(
      new Response(new Blob([new Uint8Array([1])], { type: "image/png" })),
    );
    await expect(result).resolves.toEqual({ ok: true });
  });

  it("releases a locally resolved Blob after the copy attempt", async () => {
    imageSrcMocks.resolveImageSrc.mockResolvedValue("blob:local-copy");
    vi.stubGlobal("navigator", {});

    await expect(copyImageFromPath("/tmp/large.png")).resolves.toEqual({
      ok: false,
      reason: "unsupported",
    });
    expect(imageSrcMocks.releaseImageSrc).toHaveBeenCalledWith(
      "blob:local-copy",
    );
  });
});
