import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { UserMenu } from "./UserMenu";

const labels = {
  settings: "设置",
  update: "下载并安装 v20260805-abcdef0",
};

describe("UserMenu", () => {
  it("没有新版本时只显示设置入口", () => {
    const html = renderToStaticMarkup(
      <UserMenu
        labels={labels}
        updateAvailable={false}
        updateBusy={false}
        onSettings={vi.fn()}
        onUpdate={vi.fn()}
      />,
    );

    expect(html).toContain('aria-label="设置"');
    expect(html).not.toContain("sidebar-update-action");
    expect(html).not.toContain(labels.update);
  });

  it("发现新版本后在设置右侧显示下载按钮", () => {
    const html = renderToStaticMarkup(
      <UserMenu
        labels={labels}
        updateAvailable
        updateBusy={false}
        onSettings={vi.fn()}
        onUpdate={vi.fn()}
      />,
    );

    expect(html).toContain("sidebar-update-action");
    expect(html).toContain(`aria-label="${labels.update}"`);
    expect(html).not.toContain('disabled=""');
  });

  it("开始更新后禁用重复点击", () => {
    const html = renderToStaticMarkup(
      <UserMenu
        labels={labels}
        updateAvailable
        updateBusy
        onSettings={vi.fn()}
        onUpdate={vi.fn()}
      />,
    );

    expect(html).toContain("disabled");
    expect(html).toContain('aria-busy="true"');
  });
});
