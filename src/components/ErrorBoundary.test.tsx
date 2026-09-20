import { describe, expect, it } from "vitest";
import { createElement, isValidElement } from "react";
import { ErrorBoundary } from "./ErrorBoundary";
import { Alert } from "@appica/ui-react/alert";

describe("ErrorBoundary", () => {
  it("getDerivedStateFromError 把异常转为兜底状态", () => {
    const error = new Error("boom");
    expect(ErrorBoundary.getDerivedStateFromError(error)).toEqual({ error });
  });

  it("兜底状态渲染告警卡片而非子内容", () => {
    const boundary = new ErrorBoundary({
      scope: "会话时间线",
      children: createElement("span", null, "child"),
    });
    boundary.state = { error: new Error("boom") };
    const rendered = boundary.render();
    expect(isValidElement(rendered)).toBe(true);
    if (!isValidElement(rendered)) return;
    expect(rendered.type).toBe(Alert);
    const html = JSON.stringify(rendered);
    expect(html).toContain("会话时间线");
    expect(html).toContain("重试");
    expect(html).not.toContain(">child<");
  });

  it("正常状态透传子内容", () => {
    const boundary = new ErrorBoundary({
      scope: "测试",
      children: createElement("span", null, "child"),
    });
    expect(boundary.render()).toBe(boundary.props.children);
  });
});
