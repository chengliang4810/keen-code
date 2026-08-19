import { afterEach, describe, expect, it, vi } from "vitest";
import {
  customInstructionsGet,
  customInstructionsSet,
  memoriesGet,
  memoriesSet,
  fsWriteAbsolute,
  fsWriteFile,
  gitCommit,
  gitPush,
  gitStatus,
  gitUntrackedDirectory,
  type GitStatusResult,
} from "./api";

/** 构造测试使用的空 Git 状态。 */
function gitStatusResult(branch = "main"): GitStatusResult {
  return {
    available: true,
    files: [],
    branch,
    additions: 0,
    deletions: 0,
    hasUnstagedChanges: false,
  };
}

/** 构造文件写入成功结果。 */
function fsWriteResult(absolutePath: string) {
  return {
    relativePath: "src/file.ts",
    absolutePath,
    size: 12,
    mtimeMs: 1_723_600_000_000,
  };
}

describe("个性化设置 IPC", () => {
  afterEach(() => {
    vi.useRealTimers();
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

  it("通过独立的长期记忆命令读取和保存 MEMORY.md", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce("# 长期记忆")
      .mockResolvedValueOnce("# 更新后的记忆");
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(memoriesGet()).resolves.toBe("# 长期记忆");
    await expect(memoriesSet("# 更新后的记忆")).resolves.toBe("# 更新后的记忆");
    expect(invoke).toHaveBeenNthCalledWith(1, "memories_get", {}, undefined);
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "memories_set",
      { content: "# 更新后的记忆" },
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

  it("按需展开未跟踪目录时传递项目和目录路径", async () => {
    const invoke = vi.fn().mockResolvedValue({ files: [], truncated: false });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitUntrackedDirectory("/repo", "vendor");

    expect(invoke).toHaveBeenCalledWith(
      "git_untracked_directory",
      { projectPath: "/repo", path: "vendor" },
      undefined,
    );
  });

  it("同一项目的并发 Git 状态查询只调用一次 IPC", async () => {
    let resolveStatus!: (value: GitStatusResult) => void;
    const pending = new Promise<GitStatusResult>((resolve) => {
      resolveStatus = resolve;
    });
    const invoke = vi.fn().mockReturnValue(pending);
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    const first = gitStatus("/repo/concurrent", { force: true });
    const second = gitStatus("/repo/concurrent");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    resolveStatus(gitStatusResult());

    await expect(Promise.all([first, second])).resolves.toEqual([
      gitStatusResult(),
      gitStatusResult(),
    ]);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("每轮查询运行时到达的 force 都合并为紧随其后的一轮刷新", async () => {
    let resolveFirst!: (value: GitStatusResult) => void;
    let resolveSecond!: (value: GitStatusResult) => void;
    let resolveThird!: (value: GitStatusResult) => void;
    const firstPending = new Promise<GitStatusResult>((resolve) => {
      resolveFirst = resolve;
    });
    const secondPending = new Promise<GitStatusResult>((resolve) => {
      resolveSecond = resolve;
    });
    const thirdPending = new Promise<GitStatusResult>((resolve) => {
      resolveThird = resolve;
    });
    const invoke = vi
      .fn()
      .mockReturnValueOnce(firstPending)
      .mockReturnValueOnce(secondPending)
      .mockReturnValueOnce(thirdPending);
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    const first = gitStatus("/repo/queued-force");
    const second = gitStatus("/repo/queued-force", { force: true });
    const sameSecond = gitStatus("/repo/queued-force", { force: true });
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    resolveFirst(gitStatusResult("round-a"));
    await expect(first).resolves.toEqual(gitStatusResult("round-a"));
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));

    const third = gitStatus("/repo/queued-force", { force: true });
    const sameThird = gitStatus("/repo/queued-force", { force: true });
    expect(invoke).toHaveBeenCalledTimes(2);
    resolveSecond(gitStatusResult("round-b"));
    await expect(Promise.all([second, sameSecond])).resolves.toEqual([
      gitStatusResult("round-b"),
      gitStatusResult("round-b"),
    ]);
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
    resolveThird(gitStatusResult("round-c"));

    await expect(Promise.all([third, sameThird])).resolves.toEqual([
      gitStatusResult("round-c"),
      gitStatusResult("round-c"),
    ]);
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("Git 状态在 1500 毫秒内复用缓存，force 与过期查询会刷新", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-08-14T00:00:00.000Z"));
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("first"))
      .mockResolvedValueOnce(gitStatusResult("forced"))
      .mockResolvedValueOnce(gitStatusResult("expired"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(
      gitStatus("/repo/cache", { force: true }),
    ).resolves.toEqual(gitStatusResult("first"));
    await expect(gitStatus("/repo/cache")).resolves.toEqual(
      gitStatusResult("first"),
    );
    expect(invoke).toHaveBeenCalledTimes(1);

    await expect(
      gitStatus("/repo/cache", { force: true }),
    ).resolves.toEqual(gitStatusResult("forced"));
    expect(invoke).toHaveBeenCalledTimes(2);

    vi.advanceTimersByTime(1_500);
    await expect(gitStatus("/repo/cache")).resolves.toEqual(
      gitStatusResult("expired"),
    );
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("Git 提交成功后使对应项目的状态缓存失效", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("before"))
      .mockResolvedValueOnce({ commit: "abc1234", branch: "main", output: "ok" })
      .mockResolvedValueOnce(gitStatusResult("after"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitStatus("/repo/commit", { force: true });
    await gitCommit({
      projectPath: "/repo/commit",
      message: "刷新状态 / Refresh status",
      includeUnstaged: false,
    });
    await expect(gitStatus("/repo/commit")).resolves.toEqual(
      gitStatusResult("after"),
    );

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "git_status",
      "git_commit",
      "git_status",
    ]);
  });

  it("项目内文件写入成功后使对应 Git 状态缓存失效", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("before-write"))
      .mockResolvedValueOnce(fsWriteResult("/repo/write-file/src/file.ts"))
      .mockResolvedValueOnce(gitStatusResult("after-write"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitStatus("/repo/write-file", { force: true });
    await fsWriteFile(
      "/repo/write-file",
      "src/file.ts",
      "export const value = 1;",
      1_723_600_000_000,
    );
    await expect(gitStatus("/repo/write-file")).resolves.toEqual(
      gitStatusResult("after-write"),
    );

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "git_status",
      "fs_write_file",
      "git_status",
    ]);
  });

  it("绝对路径写入成功后按 Windows 路径定位并失效项目缓存", async () => {
    const projectPath = "D:\\repo\\write-absolute";
    const absolutePath = "\\\\?\\D:\\repo\\write-absolute\\src\\file.ts";
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("before-write"))
      .mockResolvedValueOnce(fsWriteResult(absolutePath))
      .mockResolvedValueOnce(gitStatusResult("after-write"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitStatus(projectPath, { force: true });
    await fsWriteAbsolute(
      "D:\\repo\\write-absolute\\src\\file.ts",
      "export const value = 2;",
    );
    await expect(gitStatus(projectPath)).resolves.toEqual(
      gitStatusResult("after-write"),
    );

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "git_status",
      "fs_write_absolute",
      "git_status",
    ]);
  });
});
