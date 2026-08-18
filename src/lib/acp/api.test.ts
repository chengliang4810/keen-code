import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { turnFirstVisibleObserve } from "./api";

describe("ACP IPC 参数边界", () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it("首可见 Token 时间以 Rust u64 可接收的整数毫秒发送", async () => {
    invoke.mockResolvedValue(true);

    await expect(
      turnFirstVisibleObserve({
        sessionId: "session-1",
        requestId: "turn-1",
        atMs: 1_787_001_234_567.75,
      }),
    ).resolves.toBe(true);

    expect(invoke).toHaveBeenCalledWith("turn_first_visible_observe", {
      sessionId: "session-1",
      requestId: "turn-1",
      atMs: 1_787_001_234_567,
    });
  });
});
