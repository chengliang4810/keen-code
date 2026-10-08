import type { ToolContext } from "@/modules/ai/tools/context";
import type { ToolExecutionOptions } from "ai";
import { beforeEach, describe, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  bytes: new Uint8Array(),
  version: "original",
  changeBeforeWrite: false,
  size: undefined as number | undefined,
  snapshotGate: undefined as Promise<void> | undefined,
  invoke:
    vi.fn<(...args: [string, Record<string, unknown>?]) => Promise<unknown>>(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: state.invoke }));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "local" }),
  LOCAL_WORKSPACE: { kind: "local" },
}));
vi.mock("@/modules/ai/store/planStore", () => ({
  newQueuedEditId: () => "queued",
  usePlanStore: { getState: () => ({ active: false }) },
}));

import { buildFsTools } from "@/modules/ai/tools/fs";

const path = "D:/workspace/binary.txt";
const options: ToolExecutionOptions = { toolCallId: "write", messages: [] };

function context(): ToolContext {
  return {
    getCwd: () => "D:/workspace",
    getWorkspaceRoot: () => "D:/workspace",
    getTerminalContext: () => null,
    isActiveTerminalPrivate: () => false,
    injectIntoActivePty: () => false,
    openPreview: () => false,
    spawnAgent: () => null,
    readAgentOutput: () => null,
    readCache: new Map(),
    getSessionId: () => "binary-test",
  };
}

async function write(ctx: ToolContext, filePath = path, execution = options) {
  const execute = buildFsTools(ctx).write_file.execute;
  if (!execute) throw new Error("write_file has no execute");
  return (await execute(
    { path: filePath, content: "UTF-8 replacement" },
    execution,
  )) as { ok?: boolean; error?: string };
}

beforeEach(() => {
  state.version = "original";
  state.changeBeforeWrite = false;
  state.size = undefined;
  state.snapshotGate = undefined;
  state.invoke.mockReset();
  state.invoke.mockImplementation(async (command, args = {}) => {
    if (
      args.taskRoot !== "D:/workspace" ||
      typeof args.path !== "string" ||
      !args.path.startsWith("D:/workspace/")
    )
      throw new Error("outside task root");
    if (command === "agent_fs_canonicalize") return args.path;
    if (
      command === "agent_fs_read_file" ||
      command === "agent_fs_binary_snapshot"
    ) {
      const size = state.size ?? state.bytes.length;
      if (command === "agent_fs_binary_snapshot") await state.snapshotGate;
      if (command === "agent_fs_read_file" && size > 10 * 1024 * 1024)
        return { kind: "toolarge", size, limit: 10 * 1024 * 1024 };
      let content: string;
      try {
        content = new TextDecoder("utf-8", { fatal: true }).decode(state.bytes);
        if (state.bytes.includes(0)) throw new Error("binary");
      } catch {
        return {
          ...(command === "agent_fs_read_file" ? { kind: "binary" } : {}),
          size,
          version: state.version,
        };
      }
      if (command === "agent_fs_binary_snapshot") return null;
      return {
        kind: "text",
        content,
        size: state.bytes.length,
        mtime: 1,
        version: state.version,
      };
    }
    if (command === "agent_fs_write_file") {
      if (state.changeBeforeWrite) {
        state.bytes = new Uint8Array([0xff, 0x81]);
        state.version = "external";
      }
      if (args.expectedVersion !== state.version)
        throw new Error("FILE_CONFLICT: external change");
      state.bytes = new TextEncoder().encode(String(args.content));
      state.version = "written";
      return { mtime: 2, version: state.version };
    }
    throw new Error(`unexpected command ${command}`);
  });
});

describe("SDK binary full-file replacement", () => {
  it.each([
    ["UTF-16", [0xff, 0xfe, 0x41, 0x00]],
    ["invalid UTF-8", [0xff, 0xfe, 0xfd]],
  ])(
    "replaces %s with UTF-8 using a version without exposing binary content",
    async (_label, bytes) => {
      state.bytes = new Uint8Array(bytes as number[]);
      const ctx = context();
      const execute = buildFsTools(ctx).read_file.execute;
      if (!execute) throw new Error("read_file has no execute");
      const read = (await execute({ path }, options)) as {
        error?: string;
        content?: string;
      };
      expect(read.error).toContain("binary");
      expect(read.content).toBeUndefined();
      expect(ctx.readCache.size).toBe(0);
      expect((await write(ctx)).ok).toBe(true);
      expect(new TextDecoder().decode(state.bytes)).toBe("UTF-8 replacement");
      const call = state.invoke.mock.calls.find(
        ([command]) => command === "agent_fs_write_file",
      );
      expect(call?.[1]).toMatchObject({
        taskRoot: "D:/workspace",
        expectedVersion: "original",
      });
    },
  );

  it("rejects a binary change made after the write snapshot", async () => {
    state.bytes = new Uint8Array([0xff, 0xfe]);
    state.changeBeforeWrite = true;
    const result = await write(context());
    expect(result.error).toContain("FILE_CONFLICT:");
    expect(result.ok).toBeUndefined();
    expect([...state.bytes]).toEqual([0xff, 0x81]);
  });

  it("retains the task boundary when a binary overwrite is requested", async () => {
    state.bytes = new Uint8Array([0xff, 0xfe]);
    const result = await write(context(), "D:/outside/binary.txt");
    expect(result.error).toContain("outside task root");
    expect(
      state.invoke.mock.calls.some(([command]) => command === "fs_write_file"),
    ).toBe(false);
    expect([...state.bytes]).toEqual([0xff, 0xfe]);
  });

  it("still requires an explicit read before replacing existing text", async () => {
    state.bytes = new TextEncoder().encode("keep existing text");
    const result = await write(context());
    expect(result.error).toContain("read_file");
    expect(
      state.invoke.mock.calls.some(
        ([command]) => command === "agent_fs_write_file",
      ),
    ).toBe(false);
    expect(new TextDecoder().decode(state.bytes)).toBe("keep existing text");
  });

  it.each([11, 51])(
    "replaces an existing %i MiB binary through an opaque task snapshot",
    async (mebibytes) => {
      state.bytes = new Uint8Array([0xff, 0xfe]);
      state.size = mebibytes * 1024 * 1024;
      const ctx = context();
      const read = buildFsTools(ctx).read_file.execute;
      if (!read) throw new Error("read_file has no execute");
      expect(await read({ path }, options)).toMatchObject({
        error: expect.stringContaining("too large"),
      });
      expect((await write(ctx)).ok).toBe(true);
      expect(new TextDecoder().decode(state.bytes)).toBe("UTF-8 replacement");
      expect(state.invoke.mock.calls).toContainEqual([
        "agent_fs_binary_snapshot",
        { path, taskRoot: "D:/workspace", workspace: { kind: "local" } },
      ]);
      expect(
        state.invoke.mock.calls.find(
          ([command]) => command === "agent_fs_write_file",
        )?.[1],
      ).toMatchObject({ expectedVersion: "original" });
    },
  );

  it("does not implicitly authorize a large existing UTF-8 text file", async () => {
    state.bytes = new TextEncoder().encode("existing UTF-8 text");
    state.size = 51 * 1024 * 1024;
    expect((await write(context())).error).toContain("read_file");
    expect(
      state.invoke.mock.calls.some(
        ([command]) => command === "agent_fs_write_file",
      ),
    ).toBe(false);
  });

  it("does not write when cancelled while an opaque binary snapshot is in flight", async () => {
    state.bytes = new Uint8Array([0xff, 0xfe]);
    const controller = new AbortController();
    let finishSnapshot!: () => void;
    state.snapshotGate = new Promise<void>((resolve) => {
      finishSnapshot = resolve;
    });
    const pending = write(context(), path, {
      ...options,
      abortSignal: controller.signal,
    });
    await vi.waitFor(() =>
      expect(
        state.invoke.mock.calls.some(
          ([command]) => command === "agent_fs_binary_snapshot",
        ),
      ).toBe(true),
    );
    controller.abort();
    finishSnapshot();
    expect((await pending).error).toContain("cancelled");
    expect(
      state.invoke.mock.calls.some(
        ([command]) => command === "agent_fs_write_file",
      ),
    ).toBe(false);
    expect([...state.bytes]).toEqual([0xff, 0xfe]);
  });

  it("does not take an opaque snapshot of a sensitive file", async () => {
    state.bytes = new Uint8Array([0xff, 0xfe]);
    expect((await write(context(), "D:/workspace/.env")).error).toBeTruthy();
    expect(
      state.invoke.mock.calls.some(
        ([command]) => command === "agent_fs_binary_snapshot",
      ),
    ).toBe(false);
  });
});
