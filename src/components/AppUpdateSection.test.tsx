import { readFileSync } from "node:fs";
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
  downloadState: "idle" as const,
  downloadedBytes: 0,
  totalBytes: null,
  downloadSource: null,
  downloadError: null,
};

describe("AppUpdateSection", () => {
  it("更新源使用分组 Select，并拒绝未知值", () => {
    const source = readFileSync(
      new URL("./AppUpdateSection.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('from "@/components/ui/select"');
    expect(source).not.toMatch(/<select(?:\s|>)/);
    expect(source.match(/<SelectGroup>/g)?.length).toBe(1);
    expect(source.match(/<SelectItem\b/g)?.length).toBe(3);
    expect(source).toContain('t("settings.updateSourceAuto")');
    expect(source).toContain('t("settings.updateSourceGithub")');
    expect(source).toContain('t("settings.updateSourceChinaMirror")');
    expect(source).toContain("if (isAppUpdateDownloadSource(value))");
  });

  it("shows the current state after a successful check", () => {
    const html = renderToStaticMarkup(
      <AppUpdateSection
        locale="zh"
        status={current}
        busy={null}
        error={null}
        downloadSourcePreference="auto"
        onDownloadSourcePreferenceChange={vi.fn()}
        onCheck={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain("当前已是最新版本");
    expect(html).toContain("检查更新");
    expect(html).toContain('role="combobox"');
    expect(html).toContain(
      'aria-labelledby="app-update-download-source-label"',
    );
    expect(html).toContain(
      'aria-describedby="app-update-download-source-description"',
    );
  });

  it("shows background download progress when a newer release exists", () => {
    const html = renderToStaticMarkup(
      <AppUpdateSection
        locale="zh"
        status={{
          ...current,
          available: true,
          latestVersion: "2026.805.10",
          latestRelease: "v20260805-acde123",
          notes: "修复与改进",
          downloadState: "downloading",
          downloadedBytes: 1024,
          totalBytes: 4096,
        }}
        busy={null}
        error={null}
        downloadSourcePreference="auto"
        onDownloadSourcePreferenceChange={vi.fn()}
        onCheck={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain("v20260805-acde123");
    expect(html).toContain("正在后台下载");
    expect(html).toContain("查看进度");
    expect(html).toContain("修复与改进");
  });

  it("offers installation only after the package is verified", () => {
    const html = renderToStaticMarkup(
      <AppUpdateSection
        locale="zh"
        status={{
          ...current,
          available: true,
          latestVersion: "2026.805.10",
          latestRelease: "v20260805-acde123",
          downloadState: "ready",
        }}
        busy={null}
        error={null}
        downloadSourcePreference="auto"
        onDownloadSourcePreferenceChange={vi.fn()}
        onCheck={vi.fn()}
        onInstall={vi.fn()}
      />,
    );

    expect(html).toContain("已下载并通过签名校验");
    expect(html).toContain("安装并重启");
  });
});
