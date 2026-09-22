import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { DesignSystemShowcase } from "./DesignSystemShowcase";

describe("DesignSystemShowcase", () => {
  it("包含 desktop、Web、mobile remote 和 observability 状态矩阵", () => {
    const html = renderToStaticMarkup(<DesignSystemShowcase hostMode="desktop" />);
    expect(html).toContain('data-testid="design-system-showcase"');
    expect(html).toContain('data-host-mode="desktop"');
    expect(html).toContain('data-host-mode="web"');
    expect(html).toContain('data-host-mode="mobile-remote"');
    expect(html).toContain('data-testid="observability-state-matrix"');
    expect(html).toContain('data-state="loading"');
    expect(html).toContain('data-state="error"');
    expect(html).toContain('data-state="empty"');
    expect(html).toContain('data-state="data"');
    expect(html).toContain("观测数据暂不可用");
    expect(html).toContain("runtime.turn");
  });
});
