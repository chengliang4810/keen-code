import { afterEach, describe, expect, it, vi } from "vitest";
import { copyTextInGesture } from "./clipboardWrite";

/** 最小 ClipboardItem 替身：只暴露 write() 取数据所需的 getType。 */
function stubClipboardItem() {
  vi.stubGlobal(
    "ClipboardItem",
    class {
      readonly types = ["text/plain"];
      constructor(private readonly data: Record<string, Promise<Blob>>) {}
      getType(type: string) {
        return this.data[type]!;
      }
    },
  );
}

describe("copyTextInGesture", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("文本未就绪时就发起写入，等待期间不消耗用户手势", async () => {
    let finishRead!: (text: string) => void;
    const pending = new Promise<string>((resolve) => {
      finishRead = resolve;
    });
    const write = vi.fn(
      async (items: Array<{ getType(type: string): Promise<Blob> }>) => {
        await items[0]!.getType("text/plain");
      },
    );
    vi.stubGlobal("navigator", { clipboard: { write } });
    stubClipboardItem();

    const copy = copyTextInGesture(() => pending);
    // 核心契约：读取还没结束，写入已经发起（此时手势仍然有效）。
    expect(write).toHaveBeenCalledOnce();

    finishRead('{"schema":"keencode/providers-export"}');
    await expect(copy).resolves.toBeUndefined();

    const item = write.mock.calls[0]![0]![0]!;
    await expect(
      item.getType("text/plain").then((blob) => blob.text()),
    ).resolves.toBe('{"schema":"keencode/providers-export"}');
  });

  it("文本读取失败时抛出原始错误，而不是剪贴板错误", async () => {
    const failure = new Error("找不到供应商 provider");
    vi.stubGlobal("navigator", {
      clipboard: {
        write: vi.fn(async () => {
          throw new Error("NotAllowedError");
        }),
      },
    });
    stubClipboardItem();

    await expect(
      copyTextInGesture(() => Promise.reject(failure)),
    ).rejects.toBe(failure);
  });

  it("写入被拒绝且文本读取成功时向上传递剪贴板错误", async () => {
    const denied = new Error("NotAllowedError");
    vi.stubGlobal("navigator", {
      clipboard: {
        write: vi.fn(async () => {
          throw denied;
        }),
      },
    });
    stubClipboardItem();

    await expect(
      copyTextInGesture(() => Promise.resolve("{}")),
    ).rejects.toBe(denied);
  });
});
