import { afterEach, describe, expect, it, vi } from "vitest";
import { canWriteProjects } from "./hostCapabilities";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("project host capability", () => {
  it("只允许 Desktop Tauri 写入项目", () => {
    expect(canWriteProjects(true)).toBe(true);
    expect(canWriteProjects(false)).toBe(false);
  });

  it("Web Host 即使存在 Tauri-like 容器也保持只读", () => {
    vi.stubGlobal("window", {
      location: { search: "?hostMode=web" },
    });
    expect(canWriteProjects(true)).toBe(false);
  });
});
