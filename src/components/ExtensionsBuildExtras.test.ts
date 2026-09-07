import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import {
  createMarketplacePoller,
  markMarketplacePluginInstalled,
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
      "插件市场加载失败。请检查网络连接或系统代理，然后点击刷新重试。",
    );
  });

  it("市场超时、HTTP 和模型关键词不进入聊天错误分类，也不回显原始内容", () => {
    for (const error of [
      "DNS timeout private-token",
      "HTTP 503 base URL model unavailable",
      new Error("401 Unauthorized secret"),
    ]) {
      for (const locale of ["zh", "zh-TW", "en"] as const) {
        expect(resolveMarketplaceError(error, locale)).toBe(
          resolveMarketplaceError("failure", locale),
        );
      }
      expect(resolveMarketplaceError(error, "zh", "install")).toBe(
        "插件安装失败。请检查插件来源和网络连接后重试。",
      );
    }
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


describe("marketplace installed cards", () => {
  it("安装后保留卡片与顺序，只更新同一市场的插件", () => {
    const base = { name: "review", description: null, version: null, skillCount: 0, lspCount: 0, installed: false };
    const cards = [{ ...base, marketplace: "a" }, { ...base, marketplace: "b" }];
    const result = markMarketplacePluginInstalled(cards, cards[0]);
    expect(result).toHaveLength(2);
    expect(result.map((card) => card.marketplace)).toEqual(["a", "b"]);
    expect(result.map((card) => card.installed)).toEqual([true, false]);
    expect(cards[0].installed).toBe(false);
  });
});
