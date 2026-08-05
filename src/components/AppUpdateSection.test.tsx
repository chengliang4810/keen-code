import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { AppUpdateSection } from "./AppUpdateSection";

const current = {
  currentVersion: "2026.730.1",
  currentRelease: "v20260730-49ad19b",
  checked: true,
  available: false,
  latestVersion: null,
  latestRelease: null,
  notes: null,
  publishedAt: null,
};

describe("AppUpdateSection", () => {
  it("shows the current state after a successful check", () => {
    const html = renderToStaticMarkup(
      <AppUpdateSection
        locale="zh"
        status={current}
        busy={null}
        error={null}
        onCheck={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain("当前已是最新版本");
    expect(html).toContain("检查更新");
  });

  it("offers a signed install when a newer release exists", () => {
    const html = renderToStaticMarkup(
      <AppUpdateSection
        locale="zh"
        status={{
          ...current,
          available: true,
          latestVersion: "2026.805.10",
          latestRelease: "v20260805-acde123",
          notes: "修复与改进",
        }}
        busy={null}
        error={null}
        onCheck={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain("v20260805-acde123");
    expect(html).toContain("下载并安装");
    expect(html).toContain("修复与改进");
  });
});
