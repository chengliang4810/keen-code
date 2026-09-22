import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { useComposerAttachments } from "./useComposerAttachments";

describe("useComposerAttachments upload state", () => {
  it("keeps a failed pasted file visible and removes it after retry succeeds", async () => {
    const savePastedFile = vi
      .fn()
      .mockRejectedValueOnce(new Error("save failed"))
      .mockResolvedValueOnce("/tmp/clip.png");
    const classifyPaths = vi.fn().mockResolvedValue([
      { path: "/tmp/clip.png", name: "clip.png", isDir: false },
    ]);
    const setLocalError = vi.fn();
    const api = {
      isTauri: () => true,
      attachments: {
        pickFiles: vi.fn(),
        savePastedFile,
        classifyPaths,
      },
      skillsList: vi.fn(),
      goals: {},
    } as never;
    const options = {
      locale: "zh" as const,
      api,
      navigation: {
        location: () => ({ sessionId: null, draftKey: 1, viewEpoch: 1 }),
        snapshotRef: { current: null },
      },
      feedback: {
        showToast: vi.fn(),
        setLocalError,
        setAppDialog: vi.fn(),
      },
      closeComposerMenu: vi.fn(),
    } as never;
    let controller!: ReturnType<typeof useComposerAttachments>;
    function Harness() {
      controller = useComposerAttachments(options);
      return null;
    }
    renderToString(createElement(Harness));

    const globalWithWindow = globalThis as unknown as {
      window?: Window & typeof globalThis;
    };
    const previousWindow = globalWithWindow.window;
    globalWithWindow.window = globalThis as unknown as Window & typeof globalThis;
    try {
      const file = {
        name: "clip.png",
        size: 4,
        type: "image/png",
        lastModified: 1,
        arrayBuffer: async () => new Uint8Array([1, 2, 3, 4]).buffer,
      } as unknown as File;

      await controller.addPastedFiles([file]);
      expect(controller.attachmentsRef.current).toEqual([
        expect.objectContaining({
          name: "clip.png",
          uploadStatus: "failed",
          uploadError: "操作失败，请重试。",
        }),
      ]);

      await controller.retryAttachment(controller.attachmentsRef.current[0]!);
      expect(controller.attachmentsRef.current).toEqual([
        { path: "/tmp/clip.png", name: "clip.png", isDir: false },
      ]);
      expect(savePastedFile).toHaveBeenCalledTimes(2);
      expect(setLocalError).toHaveBeenLastCalledWith(null);
    } finally {
      if (previousWindow === undefined) delete globalWithWindow.window;
      else globalWithWindow.window = previousWindow;
    }
  });

  it("Web Host 粘贴文件通过 injected adapter 上传并保存远程资源元数据", async () => {
    const uploadAttachment = vi.fn().mockResolvedValue({
      resourceId: "resource_web_1",
      fileName: "clip.png",
      contentType: "image/png",
      size: 4,
      previewUrl: "/api/resources/resource_web_1",
    });
    const hostGlobal = globalThis as typeof globalThis & {
      __KEENCODE_HOST_TRANSPORT__?: unknown;
    };
    const previousTransport = hostGlobal.__KEENCODE_HOST_TRANSPORT__;
    hostGlobal.__KEENCODE_HOST_TRANSPORT__ = { uploadAttachment };
    const api = {
      isTauri: () => false,
      attachments: {
        pickFiles: vi.fn(),
        savePastedFile: vi.fn(),
        classifyPaths: vi.fn(),
      },
      skillsList: vi.fn(),
      goals: {},
    } as never;
    const options = {
      locale: "zh" as const,
      api,
      navigation: {
        location: () => ({ sessionId: null, draftKey: 1, viewEpoch: 1 }),
        snapshotRef: { current: null },
      },
      feedback: {
        showToast: vi.fn(),
        setLocalError: vi.fn(),
        setAppDialog: vi.fn(),
      },
      closeComposerMenu: vi.fn(),
    } as never;
    let controller!: ReturnType<typeof useComposerAttachments>;
    function Harness() {
      controller = useComposerAttachments(options);
      return null;
    }
    renderToString(createElement(Harness));
    const file = {
      name: "clip.png",
      size: 4,
      type: "image/png",
      lastModified: 1,
      arrayBuffer: async () => new Uint8Array([1, 2, 3, 4]).buffer,
    } as unknown as File;

    try {
      await controller.addPastedFiles([file]);
      expect(uploadAttachment).toHaveBeenCalledWith(file);
      expect(controller.attachmentsRef.current).toEqual([
        expect.objectContaining({
          source: "remote",
          path: "remote-attachment://resource_web_1",
          name: "clip.png",
          resourceId: "resource_web_1",
          contentType: "image/png",
          size: 4,
          previewUrl: "/api/resources/resource_web_1",
          uploadStatus: "ready",
        }),
      ]);
    } finally {
      if (previousTransport === undefined) delete hostGlobal.__KEENCODE_HOST_TRANSPORT__;
      else hostGlobal.__KEENCODE_HOST_TRANSPORT__ = previousTransport;
    }
  });

  it("Web Host 上传失败保留失败项，重试复用原 File 并替换为远程资源", async () => {
    const uploadAttachment = vi
      .fn()
      .mockRejectedValueOnce(new Error("upload failed"))
      .mockResolvedValueOnce({
        resourceId: "resource_web_retry",
        fileName: "retry.txt",
        contentType: "text/plain",
        size: 5,
        previewUrl: "/api/resources/resource_web_retry",
      });
    const hostGlobal = globalThis as typeof globalThis & {
      __KEENCODE_HOST_TRANSPORT__?: unknown;
    };
    const previousTransport = hostGlobal.__KEENCODE_HOST_TRANSPORT__;
    hostGlobal.__KEENCODE_HOST_TRANSPORT__ = { uploadAttachment };
    const api = {
      isTauri: () => false,
      attachments: {
        pickFiles: vi.fn(),
        savePastedFile: vi.fn(),
        classifyPaths: vi.fn(),
      },
      skillsList: vi.fn(),
      goals: {},
    } as never;
    const options = {
      locale: "zh" as const,
      api,
      navigation: {
        location: () => ({ sessionId: null, draftKey: 1, viewEpoch: 1 }),
        snapshotRef: { current: null },
      },
      feedback: {
        showToast: vi.fn(),
        setLocalError: vi.fn(),
        setAppDialog: vi.fn(),
      },
      closeComposerMenu: vi.fn(),
    } as never;
    let controller!: ReturnType<typeof useComposerAttachments>;
    function Harness() {
      controller = useComposerAttachments(options);
      return null;
    }
    renderToString(createElement(Harness));
    const file = {
      name: "retry.txt",
      size: 5,
      type: "text/plain",
      lastModified: 2,
      arrayBuffer: async () => new Uint8Array([1, 2, 3, 4, 5]).buffer,
    } as unknown as File;

    try {
      await controller.addPastedFiles([file]);
      const failed = controller.attachmentsRef.current[0]!;
      expect(failed).toEqual(expect.objectContaining({
        source: "remote",
        uploadStatus: "failed",
      }));
      await controller.retryAttachment(failed);
      expect(uploadAttachment).toHaveBeenNthCalledWith(2, file);
      expect(controller.attachmentsRef.current).toEqual([
        expect.objectContaining({
          source: "remote",
          path: "remote-attachment://resource_web_retry",
          resourceId: "resource_web_retry",
          uploadStatus: "ready",
        }),
      ]);
    } finally {
      if (previousTransport === undefined) delete hostGlobal.__KEENCODE_HOST_TRANSPORT__;
      else hostGlobal.__KEENCODE_HOST_TRANSPORT__ = previousTransport;
    }
  });
});
