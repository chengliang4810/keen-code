import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import {
  createMarketplacePoller,
  resolveMarketplaceError,
} from "./ExtensionsBuildExtras";

describe("marketplace source controls", () => {
  it("使用标题栏图标分别打开市场源列表和添加表单", () => {
    const source = readFileSync(
      new URL("./ExtensionsBuildExtras.tsx", import.meta.url),
      "utf8",
    );
    expect(source).toContain("<IconSettings");
    expect(source).toContain("<IconPlus");
    expect(source).toContain("setSourcesOpen(true)");
    expect(source).toContain("setAddSourceOpen(true)");
    expect(source).not.toContain("Collapsible");
  });
});

describe("resolveMarketplaceError", () => {
  it("后端没有错误时不显示通用失败文案", () => {
    expect(resolveMarketplaceError(null, "zh")).toBeNull();
  });

  it("后端返回错误时按当前界面语言生成安全文案", () => {
    expect(resolveMarketplaceError("unexpected failure", "zh")).toBe(
      "操作失败，请重试。",
    );
  });
});

describe("createMarketplacePoller", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("只在后台返回 loading 时继续轮询，完成后停止", async () => {
    vi.useFakeTimers();
    const refresh = vi
      .fn<() => Promise<boolean>>()
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce(false);
    const poller = createMarketplacePoller(refresh, 100);

    poller.start();
    await Promise.resolve();
    expect(refresh).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(99);
    expect(refresh).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1);
    await Promise.resolve();
    await Promise.resolve();
    expect(refresh).toHaveBeenCalledTimes(2);

    vi.advanceTimersByTime(1_000);
    expect(refresh).toHaveBeenCalledTimes(2);
    poller.cancel();
  });

  it("取消后不会把尚未完成的请求重新排入轮询", async () => {
    vi.useFakeTimers();
    let resolveRefresh!: (pending: boolean) => void;
    const refresh = vi.fn(
      () => new Promise<boolean>((resolve) => {
        resolveRefresh = resolve;
      }),
    );
    const poller = createMarketplacePoller(refresh, 100);

    poller.start();
    await Promise.resolve();
    poller.cancel();
    resolveRefresh(true);
    await Promise.resolve();
    await Promise.resolve();
    vi.advanceTimersByTime(1_000);
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});
