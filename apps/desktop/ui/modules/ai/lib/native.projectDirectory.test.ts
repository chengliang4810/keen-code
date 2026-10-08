import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { native } from "@/modules/ai/lib/native";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "wsl", distro: "Ubuntu" }),
  LOCAL_WORKSPACE: { kind: "local" },
}));
beforeEach(() => vi.mocked(invoke).mockReset());

describe("local project directory IPC", () => {
  it("checks the local directory without changing workspace authorization, even in WSL", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce("D:/project")
      .mockResolvedValueOnce({ kind: "dir" });
    expect(await native.projectDirectory(["D:\\project"])).toBe("D:/project");
    expect(invoke).toHaveBeenNthCalledWith(1, "fs_canonicalize", {
      path: "D:\\project",
      workspace: { kind: "local" },
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "fs_stat", {
      path: "D:/project",
      workspace: { kind: "local" },
    });
    expect(invoke).toHaveBeenCalledTimes(2);
  });
  it("preserves cancel as null and authorizes the selected local folder only when requested", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce("D:/project");
    expect(await native.pickProjectDirectory("选择本地文件夹")).toBeNull();
    expect(invoke).toHaveBeenNthCalledWith(1, "workspace_pick_directory", {
      title: "选择本地文件夹",
    });
    await native.workspaceAuthorize("D:/project", { kind: "local" });
    expect(invoke).toHaveBeenNthCalledWith(2, "workspace_authorize", {
      path: "D:/project",
      workspace: { kind: "local" },
    });
  });
});
