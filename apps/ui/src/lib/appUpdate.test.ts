import { describe, expect, it } from "vitest";
import type { AppUpdateStatus } from "@/lib/api";
import {
  appUpdateActionFor,
  appUpdateProgressPercent,
  formatUpdateBytes,
} from "./appUpdate";

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
  downloadedBytes: 25,
  totalBytes: 100,
  downloadSource: null,
  downloadError: null,
  ...overrides,
});

describe("app update state", () => {
  it("only installs a fully downloaded and verified update", () => {
    expect(appUpdateActionFor(status())).toBe("showProgress");
    expect(appUpdateActionFor(status({ downloadState: "verifying" }))).toBe(
      "showProgress",
    );
    expect(appUpdateActionFor(status({ downloadState: "ready" }))).toBe(
      "install",
    );
    expect(appUpdateActionFor(status({ downloadState: "failed" }))).toBe(
      "retry",
    );
    expect(appUpdateActionFor(status({ available: false }))).toBe("check");
  });

  it("clamps progress and handles an unknown total", () => {
    expect(appUpdateProgressPercent(status())).toBe(25);
    expect(
      appUpdateProgressPercent(status({ downloadedBytes: 120, totalBytes: 100 })),
    ).toBe(100);
    expect(appUpdateProgressPercent(status({ totalBytes: null }))).toBeNull();
  });

  it("formats downloaded byte counts compactly", () => {
    expect(formatUpdateBytes(0, "en")).toBe("0 B");
    expect(formatUpdateBytes(1024 * 1024 * 6.5, "en")).toBe("6.5 MB");
  });
});
