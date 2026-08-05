import { afterEach, describe, expect, it, vi } from "vitest";
import {
  customInstructionsGet,
  customInstructionsSet,
  gitCommit,
  gitPush,
} from "./api";

describe("个性化设置 IPC", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("通过独立的全局指令命令读取和保存原始文本", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce("使用中文回答")
      .mockResolvedValueOnce("  使用中文回答  ");
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(customInstructionsGet()).resolves.toBe("使用中文回答");
    await expect(customInstructionsSet("  使用中文回答  ")).resolves.toBe(
      "  使用中文回答  ",
    );
    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "custom_instructions_get",
      {},
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "custom_instructions_set",
      { instructions: "  使用中文回答  " },
      undefined,
    );
  });

  it("Git 提交和推送参数通过类型化 IPC 传递", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce({ commit: "abc1234", branch: "main", output: "ok" })
      .mockResolvedValueOnce({ branch: "main", output: "up to date" });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitCommit({
      projectPath: "/repo",
      message: "Add summary panel",
      includeUnstaged: true,
    });
    await gitPush("/repo");

    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "git_commit",
      {
        projectPath: "/repo",
        message: "Add summary panel",
        includeUnstaged: true,
      },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "git_push",
      { projectPath: "/repo" },
      undefined,
    );
  });
});
