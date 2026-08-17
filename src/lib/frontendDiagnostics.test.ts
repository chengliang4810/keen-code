import { describe, expect, it } from "vitest";
import { formatFrontendError } from "./frontendDiagnostics";

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
