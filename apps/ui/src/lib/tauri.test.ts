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

  it("IPC 失败保留原异常并落盘，日志入口失败不递归", async () => {
    const error = new Error("IPC failed");
    const native = vi.fn().mockRejectedValue(error);
    vi.stubGlobal("window", { __TAURI_INTERNALS__: { invoke: native } });
    await expect(invoke("projects_list", { privateInput: "hidden" })).rejects.toBe(error);
    expect(native).toHaveBeenCalledTimes(2);
    expect(native.mock.calls[1]?.[0]).toBe("diagnostics_record");
    expect(JSON.stringify(native.mock.calls[1])).not.toContain("hidden");
    await expect(invoke("diagnostics_record")).rejects.toBe(error);
    expect(native).toHaveBeenCalledTimes(3);
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
