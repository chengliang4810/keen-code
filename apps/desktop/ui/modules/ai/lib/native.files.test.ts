import type { ToolContext } from "@/modules/ai/tools/context";
import { describe, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  workspace: { kind: "local" } as
    | { kind: "local" }
    | { kind: "wsl"; distro: string },
  invoke: vi.fn(
    async (_command: string, _args?: Record<string, unknown>) => undefined,
  ),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: state.invoke }));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => state.workspace,
  LOCAL_WORKSPACE: { kind: "local" },
}));

import { native } from "@/modules/ai/lib/native";
import { taskFileScope } from "@/modules/ai/tools/context";

describe("Agent file IPC ownership", () => {
  it("keeps the task root and environment frozen for every model file operation", async () => {
    state.invoke.mockClear();
    state.workspace = { kind: "wsl", distro: "Debian" };
    let root = "/home/test/project";
    const scope = taskFileScope({
      getWorkspaceRoot: () => root,
      getCwd: () => root,
    } as ToolContext);
    root = "D:/another-project";
    state.workspace = { kind: "local" };
    await native.canonicalize("/home/test/project/a.txt", scope);
    await native.readFile("/home/test/project/a.txt", scope);
    await native.binaryFileSnapshot("/home/test/project/a.txt", scope);
    await native.writeFile("/home/test/project/a.txt", "updated", {
      scope,
      expectedVersion: "snapshot",
    });
    await native.createDir("/home/test/project/subdir", scope);
    await native.readDir("/home/test/project", scope);
    await native.grep({ pattern: "needle", root: "/home/test/project" }, scope);
    await native.glob({ pattern: "**/*", root: "/home/test/project" }, scope);
    expect(state.invoke.mock.calls.map(([command]) => command)).toEqual([
      "agent_fs_canonicalize",
      "agent_fs_read_file",
      "agent_fs_binary_snapshot",
      "agent_fs_write_file",
      "agent_fs_create_dir",
      "agent_fs_read_dir",
      "agent_fs_grep",
      "agent_fs_glob",
    ]);
    for (const [, args] of state.invoke.mock.calls) {
      expect(args).toMatchObject({
        taskRoot: "/home/test/project",
        workspace: { kind: "wsl", distro: "Debian" },
      });
    }
    expect(state.invoke.mock.calls[3][1]).toMatchObject({
      expectedVersion: "snapshot",
    });
  });

  it("preserves explicit UI reads outside the Agent task scope", async () => {
    state.invoke.mockClear();
    state.workspace = { kind: "local" };
    await native.readFile("D:/explicitly-selected/file.txt");
    expect(state.invoke).toHaveBeenCalledWith("fs_read_file", {
      path: "D:/explicitly-selected/file.txt",
      workspace: { kind: "local" },
    });
  });
});
