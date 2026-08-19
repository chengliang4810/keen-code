import { afterEach, describe, expect, it, vi } from "vitest";
import { createMarketplacePoller } from "./ExtensionsBuildExtras";

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
