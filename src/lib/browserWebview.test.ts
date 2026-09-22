import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  browserOpen: vi.fn(async () => undefined),
  syncNativeThemeSurfaces: vi.fn(async () => undefined),
}));

vi.mock("@/lib/api", () => ({ browserOpen: mocks.browserOpen }));
vi.mock("./nativeTheme", () => ({
  syncNativeThemeSurfaces: mocks.syncNativeThemeSurfaces,
}));

import { openBrowserWebview } from "./browserWebview";

describe("browser webview theme synchronization", () => {
  beforeEach(() => {
    mocks.browserOpen.mockClear();
    mocks.syncNativeThemeSurfaces.mockClear();
  });

  it("在子 WebView 创建或复用后同步当前原生主题底色", async () => {
    await openBrowserWebview("tab_1", "https://example.com", {
      left: 0,
      top: 0,
      width: 640,
      height: 480,
    });
    expect(mocks.browserOpen).toHaveBeenCalledWith("tab_1", "https://example.com", {
      left: 0,
      top: 0,
      width: 640,
      height: 480,
    });
    expect(mocks.syncNativeThemeSurfaces).toHaveBeenCalledOnce();
  });
});
