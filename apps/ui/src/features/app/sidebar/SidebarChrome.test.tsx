import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { SidebarChrome } from "./SidebarChrome";
import { DEFAULT_LAYOUT } from "@/lib/layout";

function renderChrome(overrides: Partial<Parameters<typeof SidebarChrome>[0]> = {}) {
  return renderToStaticMarkup(
    <SidebarChrome
      layout={DEFAULT_LAYOUT}
      setLayout={vi.fn()}
      setResizingSidebar={vi.fn()}
      sidebarRef={{ current: null }}
      sidebarResizeStartRef={{ current: null }}
      canGoBack={false}
      canGoForward={true}
      goBack={vi.fn()}
      goForward={vi.fn()}
      useCustomWindowChrome={false}
      toggleMaximizeFromTitlebar={vi.fn()}
      tr={(key) => key}
      {...overrides}
    />,
  );
}

describe("SidebarChrome task navigation", () => {
  it("renders the product mark and keeps task navigation in the no-drag chrome", () => {
    const html = renderChrome();

    expect(html).toContain('class="sidebar-chrome"');
    expect(html).toContain("data-tauri-drag-region");
    expect(html).toContain("sidebar-brand");
    expect(html).toContain('data-testid="sidebar-task-navigation"');
    expect(html).toContain('aria-label="resources.browserBack"');
    expect(html).toContain('aria-label="resources.browserForward"');
    expect(html).toContain("disabled");
    expect(html).not.toContain('aria-label="sidebar.newSession"');
    // 品牌入口与后退/前进三个按钮统一渲染官方 Appica md 几何（h-10）。
    expect((html.match(/h-10/g) ?? []).length).toBe(3);
  });

  it("enables both task controls when the history has both directions", () => {
    const html = renderChrome({ canGoBack: true, canGoForward: true });

    expect(html.match(/data-testid="sidebar-task-navigation"/g)).toHaveLength(1);
    expect(html.match(/aria-label="resources.browserBack"/g)).toHaveLength(1);
    expect(html.match(/aria-label="resources.browserForward"/g)).toHaveLength(1);
    // 官方 Button 的 class 含 data-disabled:* 变体，这里只匹配 disabled 属性本身。
    expect(html).not.toMatch(/aria-label="resources\.browserBack"[^>]*\sdisabled(?:[\s=>])/);
    expect(html).not.toMatch(/aria-label="resources\.browserForward"[^>]*\sdisabled(?:[\s=>])/);
  });
});
