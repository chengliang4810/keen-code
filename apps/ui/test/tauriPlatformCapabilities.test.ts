import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
const events = vi.hoisted(() => ({
  listen: vi.fn(),
}));
const webview = vi.hoisted(() => ({
  onDragDropEvent: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: bridge.invoke,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: events.listen,
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => webview,
}));
vi.mock("@zcode/ui", () => ({
  formatDiagnosticArgs: (args: unknown[]) => args.map(String).join(" "),
  reportRendererError: vi.fn(),
  setRendererCrashSink: vi.fn(),
  setRendererDiagnosticsSink: vi.fn(),
}));

import { createTauriPlatform } from "../src/tauriPlatform.js";

beforeEach(() => {
  bridge.invoke.mockReset();
  events.listen.mockReset();
  webview.onDragDropEvent.mockReset();
  bridge.invoke.mockResolvedValue(undefined);
  events.listen.mockResolvedValue(vi.fn());
  webview.onDragDropEvent.mockResolvedValue(vi.fn());
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("Tauri 平台能力适配", () => {
  it("把 ArrayBuffer 转成原生 save_file 可反序列化的字节数组", async () => {
    bridge.invoke.mockImplementation(async (command: string) => {
      if (command === "platform_file_save_file") {
        return { success: true, path: "C:/tmp/output.bin" };
      }
      return undefined;
    });
    const platform = createTauriPlatform();

    await expect(
      platform.saveFile?.({
        data: new Uint8Array([0, 1, 255]).buffer,
        suggestedName: "output.bin",
      }),
    ).resolves.toEqual({ success: true, path: "C:/tmp/output.bin" });
    expect(bridge.invoke).toHaveBeenCalledWith(
      "platform_file_save_file",
      { suggestedName: "output.bin", data: [0, 1, 255] },
    );
  });

  it("让日志导出复用原生保存并保留取消/失败结果", async () => {
    bridge.invoke.mockImplementation(async (command: string) => {
      if (command === "diagnostics_export") return '{"redacted":true}';
      if (command === "platform_file_save_file") return { success: false, canceled: true };
      return undefined;
    });
    const platform = createTauriPlatform();

    await expect(platform.exportLogs()).resolves.toEqual({
      success: false,
      canceled: true,
    });
    expect(bridge.invoke).toHaveBeenNthCalledWith(1, "browser_reset", undefined);
    expect(bridge.invoke).toHaveBeenCalledWith("diagnostics_export", undefined);
    expect(bridge.invoke).toHaveBeenCalledWith(
      "platform_file_save_file",
      {
        suggestedName: "keencode-diagnostics.json",
        data: Array.from(new TextEncoder().encode('{"redacted":true}')),
      },
    );
  });

  it("把 Rust 返回的 PDF number[] 还原为 ArrayBuffer", async () => {
    vi.stubGlobal("navigator", { userAgent: "Windows NT", platform: "Win32" });
    const host = {
      getAttribute: vi.fn((attribute: string) => {
        if (attribute === "data-keencode-print-width-px") return "1280";
        if (attribute === "data-keencode-print-height-px") return "720";
        return null;
      }),
    };
    vi.stubGlobal("document", { querySelector: vi.fn(() => host) });
    bridge.invoke.mockImplementation(async (command: string) => {
      if (command === "platform_file_print_page_to_pdf") {
        return { success: true, data: [37, 80, 68, 70, 45] };
      }
      return undefined;
    });
    const platform = createTauriPlatform();

    const result = await platform.printPageToPdf?.();
    expect(result).toMatchObject({ success: true });
    expect(result?.data).toBeInstanceOf(ArrayBuffer);
    expect(Array.from(new Uint8Array(result?.data ?? new ArrayBuffer(0)))).toEqual([
      37, 80, 68, 70, 45,
    ]);
    expect(bridge.invoke).toHaveBeenCalledWith("platform_file_print_page_to_pdf", {
      pageSize: { widthPx: 1280, heightPx: 720 },
    });
  });

  it("拒绝打印宿主的非法页面尺寸，不回退为无参打印", async () => {
    vi.stubGlobal("navigator", { userAgent: "Windows NT", platform: "Win32" });
    const host = {
      getAttribute: vi.fn((attribute: string) => {
        if (attribute === "data-keencode-print-width-px") return "0";
        if (attribute === "data-keencode-print-height-px") return "720";
        return null;
      }),
    };
    vi.stubGlobal("document", { querySelector: vi.fn(() => host) });
    const platform = createTauriPlatform();

    await expect(Promise.resolve().then(() => platform.printPageToPdf?.())).rejects.toThrow(
      "页面尺寸无效",
    );
    expect(bridge.invoke).not.toHaveBeenCalledWith("platform_file_print_page_to_pdf", undefined);
  });

  it("没有打印宿主时保留无参打印命令", async () => {
    vi.stubGlobal("navigator", { userAgent: "Windows NT", platform: "Win32" });
    vi.stubGlobal("document", { querySelector: vi.fn(() => null) });
    bridge.invoke.mockImplementation(async (command: string) => {
      if (command === "platform_file_print_page_to_pdf") {
        return { success: true, data: [37, 80, 68, 70, 45] };
      }
      return undefined;
    });
    const platform = createTauriPlatform();

    await expect(platform.printPageToPdf?.()).resolves.toMatchObject({ success: true });
    expect(bridge.invoke).toHaveBeenCalledWith("platform_file_print_page_to_pdf", undefined);
  });

  it("拒绝 PDF 返回中的非法字节，并在非 Windows 平台不暴露打印能力", async () => {
    vi.stubGlobal("navigator", { userAgent: "Linux", platform: "Linux x86_64" });
    const linuxPlatform = createTauriPlatform();
    expect(linuxPlatform.printPageToPdf).toBeUndefined();

    vi.stubGlobal("navigator", { userAgent: "Windows NT", platform: "Win32" });
    bridge.invoke.mockImplementation(async (command: string) => {
      if (command === "platform_file_print_page_to_pdf") {
        return { success: true, data: [0, 256] };
      }
      return undefined;
    });
    const windowsPlatform = createTauriPlatform();
    await expect(windowsPlatform.printPageToPdf?.()).rejects.toThrow("非法字节");
  });

  it("将 Tauri 物理坐标换算为 CSS 坐标并只投影 drop 路径", async () => {
    let callback: ((event: { payload: unknown }) => void) | undefined;
    webview.onDragDropEvent.mockImplementation(async (handler) => {
      callback = handler;
      return vi.fn();
    });
    const platform = createTauriPlatform();
    const drops: unknown[] = [];
    const dispose = platform.onNativeFileDrop?.((event) => drops.push(event));
    await vi.waitFor(() => expect(webview.onDragDropEvent).toHaveBeenCalledOnce());

    callback?.({ payload: { type: "over", position: { x: 10, y: 20 } } });
    callback?.({
      payload: {
        type: "drop",
        paths: ["C:/workspace/plugin"],
        position: { x: 20, y: 40 },
      },
    });
    expect(drops).toEqual([
      { paths: ["C:/workspace/plugin"], position: { x: 20, y: 40 } },
    ]);
    dispose?.();
    callback?.({
      payload: {
        type: "drop",
        paths: ["C:/workspace/late-plugin"],
        position: { x: 20, y: 40 },
      },
    });
    expect(drops).toHaveLength(1);
  });

  it("映射退出确认和托盘会话事件到已有 Rust 命令/事件", async () => {
    const handlers = new Map<string, (event: { payload: unknown }) => void>();
    events.listen.mockImplementation(async (name: string, handler: (event: { payload: unknown }) => void) => {
      handlers.set(name, handler);
      return vi.fn();
    });
    const platform = createTauriPlatform();
    const exitPayloads: unknown[] = [];
    const sessionPayloads: unknown[] = [];
    platform.onExitRequested?.((payload) => exitPayloads.push(payload));
    platform.onTrayOpenSession?.((payload) => sessionPayloads.push(payload));
    await vi.waitFor(() => expect(handlers.size).toBe(2));

    for (const payload of [null, {}, { activeCount: 0 }, { activeCount: -1 }, { activeCount: 1.5 }, { activeCount: "2" }]) {
      handlers.get("app://exit-requested")?.({ payload });
    }
    for (const payload of [null, {}, { sessionId: "" }, { sessionId: "   " }, { sessionId: 42 }]) {
      handlers.get("app://tray-open-session")?.({ payload });
    }
    expect(exitPayloads).toEqual([]);
    expect(sessionPayloads).toEqual([]);
    handlers.get("app://exit-requested")?.({ payload: { activeCount: 2 } });
    handlers.get("app://tray-open-session")?.({ payload: { sessionId: "session-1" } });
    expect(exitPayloads).toEqual([{ activeCount: 2 }]);
    expect(sessionPayloads).toEqual([{ sessionId: "session-1" }]);

    await platform.confirmExit?.();
    await platform.setTrayMenu?.({
      labels: { newChat: "New", show: "Show", quit: "Quit" },
      sessions: [{ id: "session-1", title: "Task" }],
    });
    expect(bridge.invoke).toHaveBeenCalledWith("app_confirm_exit", undefined);
    expect(bridge.invoke).toHaveBeenCalledWith("tray_set_menu", {
      menu: {
        labels: { newChat: "New", show: "Show", quit: "Quit" },
        sessions: [{ id: "session-1", title: "Task" }],
      },
    });
  });
});
