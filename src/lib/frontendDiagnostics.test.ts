import { afterEach, describe, expect, it, vi } from "vitest";
import { formatFrontendError, installFrontendErrorHandlers } from "./frontendDiagnostics";

describe("formatFrontendError", () => {
  it("保留异常名称、消息和堆栈", () => {
    const error = new Error("渲染失败");
    expect(formatFrontendError(error)).toContain("Error: 渲染失败");
    expect(formatFrontendError(error)).toContain("frontendDiagnostics.test.ts");
  });

  it("无法序列化的值仍能安全转换", () => {
    const value: { self?: unknown } = {};
    value.self = value;
    expect(formatFrontendError(value)).toBe("[object Object]");
  });

  it("JSON.stringify 返回 undefined 时不会让异常上报再次抛错", () => {
    expect(formatFrontendError(undefined)).toBe("undefined");
    expect(formatFrontendError(Symbol("rejection"))).toBe(
      "Symbol(rejection)",
    );
  });
});

describe("前端错误落盘", () => {
  afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

  it("窗口异常、未处理拒绝和 console.error 都进入统一文件入口", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    const surface = Object.assign(new EventTarget(), { __TAURI_INTERNALS__: { invoke } });
    vi.stubGlobal("window", surface);
    vi.spyOn(console, "error").mockImplementation(() => {});
    const uninstall = installFrontendErrorHandlers();
    try {
      surface.dispatchEvent(Object.assign(new Event("error"), { error: new Error("window failure") }));
      surface.dispatchEvent(Object.assign(new Event("unhandledrejection"), { reason: new Error("promise failure") }));
      console.error("console failure");
      expect(invoke.mock.calls.map(([, args]) => args.component)).toEqual([
        "frontend.window_error", "frontend.unhandled_rejection", "frontend.console_error",
      ]);
      expect(JSON.stringify(invoke.mock.calls)).toContain("promise failure");
    } finally { uninstall(); }
    surface.dispatchEvent(new Event("error"));
    expect(invoke).toHaveBeenCalledTimes(3);
  });
});
