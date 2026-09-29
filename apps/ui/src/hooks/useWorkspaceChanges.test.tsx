import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { useWorkspaceChanges } from "./useWorkspaceChanges";

const apiMocks = vi.hoisted(() => ({
  gitStatus: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  isTauri: () => true,
  gitStatus: apiMocks.gitStatus,
}));

/**
 * node 环境下的 SSR 捕获模式无法观察异步状态流转（renderToString 之间
 * 不共享 hook 状态）；防抖同步、快照保留等时序行为由 ResourceViewer 的
 * 契约断言按源码锁定，这里覆盖可同步观察的分支语义。
 */
function renderHookCapture(options: Parameters<typeof useWorkspaceChanges>[0]) {
  let captured!: ReturnType<typeof useWorkspaceChanges>;
  function Harness() {
    captured = useWorkspaceChanges(options);
    return null;
  }
  renderToString(createElement(Harness));
  return captured;
}

function baseOptions(overrides: Partial<Parameters<typeof useWorkspaceChanges>[0]> = {}) {
  return {
    projectPath: "C:/repo",
    query: "",
    paneActive: true,
    changesActive: true,
    syncRevision: 1,
    locale: "zh" as const,
    onError: vi.fn(),
    ...overrides,
  };
}

describe("useWorkspaceChanges", () => {
  it("初始状态为空且不可用，不带任何旧快照", () => {
    const state = renderHookCapture(baseOptions());
    expect(state.files).toEqual([]);
    expect(state.loading).toBe(false);
    expect(state.available).toBe(false);
    expect(state.reason).toBeNull();
    expect(state.branch).toBeNull();
    expect(state.count).toBe(0);
    expect(state.filtered).toEqual([]);
  });

  it("项目根为空时 refresh 直接短路，不发起 Git 请求", async () => {
    const state = renderHookCapture(baseOptions({ projectPath: null }));
    await state.refresh();
    await state.refresh(true);
    expect(apiMocks.gitStatus).not.toHaveBeenCalled();
  });

  it("refresh 把 force 参数原样透传给底层状态读取", async () => {
    apiMocks.gitStatus.mockResolvedValue({ available: true, files: [], branch: null });
    const state = renderHookCapture(baseOptions());
    await state.refresh(true);
    expect(apiMocks.gitStatus).toHaveBeenLastCalledWith("C:/repo", { force: true });
    await state.refresh();
    expect(apiMocks.gitStatus).toHaveBeenLastCalledWith("C:/repo", { force: false });
  });
});
