import { afterEach, describe, expect, it, vi } from "vitest";
const imageSrcMocks = vi.hoisted(() => ({
  releaseImageSrc: vi.fn(),
  resolveImageSrc: vi.fn(),
}));
vi.mock("@/lib/imageSrc", () => imageSrcMocks);

import { copyImageFromPath, copyImageFromSrc } from "./copyImage";

/** 最小 ClipboardItem 替身：只暴露 write() 取数据所需的 getType。 */
function stubClipboardItem() {
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
}

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

  it("本地路径解析未完成时就发起写入，避免等待期间手势过期", async () => {
    let finishResolve!: (src: string) => void;
    imageSrcMocks.resolveImageSrc.mockReturnValue(
      new Promise<string>((resolve) => {
        finishResolve = resolve;
      }),
    );
    const write = vi.fn(async (items: ClipboardItem[]) => {
      await items[0]!.getType("image/png");
    });
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve(new Response(
      new Blob([new Uint8Array([1])], { type: "image/png" }),
    ))));
    vi.stubGlobal("navigator", { clipboard: { write } });
    stubClipboardItem();

    const result = copyImageFromPath("/tmp/slow.png");
    // 核心契约：解析还没结束，写入已经发起（此时手势仍然有效）。
    expect(write).toHaveBeenCalledOnce();

    finishResolve("blob:slow");
    await expect(result).resolves.toEqual({ ok: true });
  });

  it("复制完成后释放本地解析出的 Blob URL", async () => {
    imageSrcMocks.resolveImageSrc.mockResolvedValue("blob:local-copy");
    const write = vi.fn(async (items: ClipboardItem[]) => {
      await items[0]!.getType("image/png");
    });
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve(new Response(
      new Blob([new Uint8Array([1])], { type: "image/png" }),
    ))));
    vi.stubGlobal("navigator", { clipboard: { write } });
    stubClipboardItem();

    await expect(copyImageFromPath("/tmp/large.png")).resolves.toEqual({
      ok: true,
    });
    expect(imageSrcMocks.releaseImageSrc).toHaveBeenCalledWith(
      "blob:local-copy",
    );
  });

  it("剪贴板不可用时不再解析本地路径", async () => {
    vi.stubGlobal("navigator", {});

    await expect(copyImageFromPath("/tmp/large.png")).resolves.toEqual({
      ok: false,
      reason: "unsupported",
    });
    expect(imageSrcMocks.resolveImageSrc).not.toHaveBeenCalled();
  });
});
