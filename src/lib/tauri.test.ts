import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke, isTauri } from "./tauri";

describe("Tauri IPC 公共边界", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("只在桌面运行时标记存在时识别为 Tauri", () => {
    vi.stubGlobal("window", {});
    expect(isTauri()).toBe(false);

    vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
    expect(isTauri()).toBe(true);
  });

  it("非桌面环境拒绝调用并保留命令名", async () => {
    vi.stubGlobal("window", {});
    await expect(invoke("projects_list")).rejects.toThrow(
      "Tauri required: projects_list",
    );
  });

  it("桌面环境把命令与参数完整转发给唯一 IPC 实现", async () => {
    const tauriInvoke = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("window", { __TAURI_INTERNALS__: { invoke: tauriInvoke } });

    await expect(invoke("example_command", { id: "item-1" })).resolves.toEqual({
      ok: true,
    });
    expect(tauriInvoke).toHaveBeenCalledWith(
      "example_command",
      { id: "item-1" },
      undefined,
    );
  });
});
