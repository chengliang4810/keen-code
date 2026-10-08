import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const disk = vi.hoisted(() => ({
  entries: vi.fn(),
  set: vi.fn(),
  delete: vi.fn(),
  save: vi.fn(),
}));
vi.mock("@/lib/storage", () => ({
  LazyStore: class {
    entries = disk.entries;
    set = disk.set;
    delete = disk.delete;
    save = disk.save;
  },
}));

describe("file-backed UI state", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers();
    disk.entries.mockReset().mockResolvedValue([]);
    disk.set.mockReset().mockResolvedValue(undefined);
    disk.delete.mockReset().mockResolvedValue(true);
    disk.save.mockReset().mockResolvedValue(undefined);
  });
  afterEach(() => vi.useRealTimers());

  it("hydrates before reads and coalesces repeated changes", async () => {
    disk.entries.mockResolvedValue([
      ["width", "300"],
      ["invalid", 42],
    ]);
    const { hydrateUiState, uiState } = await import("@/lib/uiState");
    await hydrateUiState();
    expect(uiState.getItem("width")).toBe("300");
    expect(uiState.getItem("invalid")).toBeNull();
    for (let width = 301; width <= 350; width++)
      uiState.setItem("width", String(width));
    expect(disk.set).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(100);
    expect(disk.set).toHaveBeenCalledExactlyOnceWith("width", "350");
    expect(disk.save).toHaveBeenCalledTimes(1);
  });

  it("preserves writes made while hydration is pending and persists deletion", async () => {
    const { hydrateUiState, uiState } = await import("@/lib/uiState");
    uiState.setItem("theme", "dark");
    disk.entries.mockResolvedValue([["theme", "light"]]);
    await hydrateUiState();
    expect(uiState.getItem("theme")).toBe("dark");
    expect(disk.set).toHaveBeenCalledWith("theme", "dark");
    uiState.removeItem("theme");
    await vi.advanceTimersByTimeAsync(100);
    expect(disk.delete).toHaveBeenCalledWith("theme");
  });

  it("does not overwrite unread data after a hydration failure", async () => {
    const { hydrateUiState, uiState } = await import("@/lib/uiState");
    disk.entries.mockRejectedValueOnce(new Error("unreadable"));
    await expect(hydrateUiState()).rejects.toThrow("unreadable");
    uiState.setItem("theme", "dark");
    await vi.advanceTimersByTimeAsync(200);
    expect(disk.set).not.toHaveBeenCalled();
    await hydrateUiState();
    expect(disk.set).toHaveBeenCalledWith("theme", "dark");
  });

  it("flushes pending values before shutdown without waiting for the debounce", async () => {
    const { hydrateUiState, uiState, flushUiState, hasPendingUiState } =
      await import("@/lib/uiState");
    await hydrateUiState();
    uiState.setItem("width", "400");
    expect(hasPendingUiState()).toBe(true);
    await flushUiState();
    expect(disk.set).toHaveBeenCalledExactlyOnceWith("width", "400");
    expect(hasPendingUiState()).toBe(false);
    await vi.advanceTimersByTimeAsync(200);
    expect(disk.save).toHaveBeenCalledTimes(1);
  });
});
