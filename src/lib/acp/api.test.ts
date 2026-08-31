import { afterEach, describe, expect, it, vi } from "vitest";
import { sessionSend } from "./api";

describe("ACP 会话发送 IPC", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("把单轮参数作为一个请求对象完整转发", async () => {
    const accepted = {
      state: "streaming",
      activeTurnId: "request-1",
      backend: "peri_acp",
      acceptedAtMs: 123,
    };
    const invoke = vi.fn().mockResolvedValue(accepted);
    vi.stubGlobal("window", { __TAURI_INTERNALS__: { invoke } });

    await expect(
      sessionSend({
        text: "检查项目",
        sessionId: "session-1",
        requestId: "request-1",
        planMode: true,
        ultraMode: true,
      }),
    ).resolves.toEqual(accepted);
    expect(invoke).toHaveBeenCalledWith(
      "session_send",
      {
        request: {
          text: "检查项目",
          sessionId: "session-1",
          requestId: "request-1",
          planMode: true,
          ultraMode: true,
        },
      },
      undefined,
    );
  });

  it("在进入 IPC 前拒绝空请求标识", async () => {
    const invoke = vi.fn();
    vi.stubGlobal("window", { __TAURI_INTERNALS__: { invoke } });

    await expect(
      sessionSend({
        text: "检查项目",
        sessionId: "session-1",
        requestId: "  ",
      }),
    ).rejects.toThrow("requestId 不能为空");
    expect(invoke).not.toHaveBeenCalled();
  });
});
