import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { AppUpdateStatus } from "@/lib/api";
import { AppUpdateProgress } from "./AppUpdateProgress";

const status = (overrides: Partial<AppUpdateStatus> = {}): AppUpdateStatus => ({
  currentVersion: "1.0.1",
  currentRelease: "v20260805-old0000",
  checked: true,
  available: true,
  latestVersion: "1.0.2",
  latestRelease: "v20260805-new0000",
  notes: null,
  publishedAt: null,
  downloadState: "downloading",
  downloadedBytes: 2 * 1024 * 1024,
  totalBytes: 8 * 1024 * 1024,
  downloadSource: "chinaMirror",
  downloadError: null,
  ...overrides,
});

describe("AppUpdateProgress", () => {
  it("shows determinate background download progress", () => {
    const html = renderToStaticMarkup(
      <AppUpdateProgress
        locale="zh"
        status={status()}
        installing={false}
        error={null}
        onRetry={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain('role="progressbar"');
    expect(html).toContain('aria-valuenow="25"');
    expect(html).toContain("2 MB / 8 MB · 25%");
    expect(html).toContain("下载源：国内加速");
    expect(html).toContain("关闭此窗口不会中断下载");
  });

  it("offers installation only after verification is ready", () => {
    const html = renderToStaticMarkup(
      <AppUpdateProgress
        locale="zh"
        status={status({ downloadState: "ready" })}
        installing={false}
        error={null}
        onRetry={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain("已下载并通过签名校验");
    expect(html).toContain("安装并重启");
    expect(html).not.toContain('role="progressbar"');
  });

  it("keeps a failed download actionable", () => {
    const html = renderToStaticMarkup(
      <AppUpdateProgress
        locale="zh"
        status={status({
          downloadState: "failed",
          downloadError: "下载超时",
        })}
        installing={false}
        error={null}
        onRetry={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain("下载超时");
    expect(html).toContain("重试下载");
  });
});
