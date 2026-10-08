import type { ToolExecutionOptions } from "ai";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ToolContext } from "@/modules/ai/tools/context";
import type { WorkspaceEnv } from "@/modules/workspace";

const ports = vi.hoisted(() => ({
  shellSessionOpen: vi.fn(),
  shellSessionRun: vi.fn(),
  shellSessionCancel: vi.fn(),
  shellSessionClose: vi.fn(),
  shellBgSpawn: vi.fn(),
  shellBgLogs: vi.fn(),
  shellBgList: vi.fn(),
  shellBgKill: vi.fn(),
}));
const workspace = vi.hoisted(() => ({ env: { kind: "local" } as WorkspaceEnv }));
vi.mock("@/modules/ai/lib/native", () => ({ native: ports }));
vi.mock("@/modules/ai/lib/security", () => ({
  checkShellCommand: () => ({ ok: true }),
}));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => workspace.env,
  workspaceScopeKey: (env: WorkspaceEnv) => JSON.stringify(env),
}));

import {
  buildShellTools,
  releaseSessionShells,
} from "@/modules/ai/tools/shell";

const sessions = new Set<string>();
const output = {
  stdout: "result",
  stderr: "",
  exit_code: 0,
  timed_out: false,
  truncated: false,
  cwd_after: "/workspace",
};
const options: ToolExecutionOptions = { toolCallId: "call", messages: [] };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function context(id = "session"): ToolContext {
  sessions.add(id);
  return {
    getCwd: () => "/workspace",
    getWorkspaceRoot: () => "/workspace",
    getSessionId: () => id,
  } as ToolContext;
}

async function execute(
  name: keyof ReturnType<typeof buildShellTools>,
  input: Record<string, unknown>,
  opts: ToolExecutionOptions = options,
  id = "session",
): Promise<Record<string, unknown>> {
  const run = buildShellTools(context(id))[name].execute;
  if (!run) throw new Error("missing shell tool");
  return (await run(input as never, opts)) as Record<string, unknown>;
}

beforeEach(() => {
  vi.resetAllMocks();
  workspace.env = { kind: "local" };
  ports.shellSessionOpen.mockResolvedValue(1);
  ports.shellSessionRun.mockResolvedValue(output);
  ports.shellSessionCancel.mockResolvedValue(undefined);
  ports.shellSessionClose.mockResolvedValue(undefined);
  ports.shellBgSpawn.mockResolvedValue(9);
});

afterEach(async () => {
  await Promise.all([...sessions].map(releaseSessionShells));
  sessions.clear();
});

describe("SDK shell cancellation and ownership", () => {
  it.each(["bash_run", "bash_background"] as const)(
    "pre-cancellation prevents %s from opening or spawning",
    async (name) => {
      const abort = new AbortController();
      abort.abort();
      const result = await execute(name, { command: "echo result" }, {
        ...options,
        abortSignal: abort.signal,
      });
      expect(result.error).toContain("cancelled");
      expect(ports.shellSessionOpen).not.toHaveBeenCalled();
      expect(ports.shellSessionRun).not.toHaveBeenCalled();
      expect(ports.shellBgSpawn).not.toHaveBeenCalled();
    },
  );

  it("does not run when cancelled during shell creation", async () => {
    const opening = deferred<number>();
    ports.shellSessionOpen.mockReturnValue(opening.promise);
    const abort = new AbortController();
    const running = execute("bash_run", { command: "echo result" }, {
      ...options,
      abortSignal: abort.signal,
    });
    abort.abort();
    opening.resolve(1);
    expect((await running).error).toContain("cancelled");
    expect(ports.shellSessionRun).not.toHaveBeenCalled();
  });

  it("cancels only the executing foreground call and suppresses late success", async () => {
    const completed = deferred<typeof output>();
    ports.shellSessionRun.mockReturnValue(completed.promise);
    const abort = new AbortController();
    const running = execute("bash_run", { command: "echo result" }, {
      ...options,
      abortSignal: abort.signal,
    });
    await vi.waitFor(() => expect(ports.shellSessionRun).toHaveBeenCalledOnce());
    const callId = ports.shellSessionRun.mock.calls[0][4];
    abort.abort();
    expect(ports.shellSessionCancel).toHaveBeenCalledExactlyOnceWith(1, callId);
    completed.resolve(output);
    expect((await running).error).toContain("cancelled");
    expect(ports.shellBgKill).not.toHaveBeenCalled();
    expect(ports.shellSessionClose).not.toHaveBeenCalled();
  });

  it("removes the abort listener after a completed call", async () => {
    const abort = new AbortController();
    expect(await execute("bash_run", { command: "echo result" }, {
      ...options,
      abortSignal: abort.signal,
    })).toMatchObject({ stdout: "result", exit_code: 0 });
    abort.abort();
    expect(ports.shellSessionCancel).not.toHaveBeenCalled();
  });

  it("keeps explicitly spawned background processes across turn cancellation", async () => {
    const spawned = deferred<number>();
    ports.shellBgSpawn.mockReturnValue(spawned.promise);
    const abort = new AbortController();
    const running = execute("bash_background", { command: "dev-server" }, {
      ...options,
      abortSignal: abort.signal,
    });
    abort.abort();
    spawned.resolve(9);
    expect(await running).toMatchObject({ handle: 9, ok: true });
    expect(ports.shellBgKill).not.toHaveBeenCalled();
    expect(ports.shellSessionCancel).not.toHaveBeenCalled();
  });
});

describe("SDK shell cache lifecycle", () => {
  it("retries a failed shell opening and deduplicates concurrent opens", async () => {
    ports.shellSessionOpen.mockRejectedValueOnce(new Error("temporary error"));
    expect((await execute("bash_run", { command: "pwd" })).error).toContain(
      "temporary error",
    );
    const opening = deferred<number>();
    ports.shellSessionOpen.mockReturnValueOnce(opening.promise);
    const one = execute("bash_run", { command: "pwd" });
    const two = execute("bash_run", { command: "pwd" });
    expect(ports.shellSessionOpen).toHaveBeenCalledTimes(2);
    opening.resolve(2);
    expect((await one).exit_code).toBe(0);
    expect((await two).exit_code).toBe(0);
    expect(ports.shellSessionRun).toHaveBeenCalledTimes(2);
  });

  it("closes a pending opening on destruction without running the old call", async () => {
    const opening = deferred<number>();
    ports.shellSessionOpen.mockReturnValueOnce(opening.promise);
    const running = execute("bash_run", { command: "pwd" });
    const releasing = releaseSessionShells("session");
    opening.resolve(4);
    expect((await running).error).toContain("released");
    await releasing;
    expect(ports.shellSessionClose).toHaveBeenCalledExactlyOnceWith(4);
    expect(ports.shellSessionRun).not.toHaveBeenCalled();
  });

  it("an old rejection cannot evict a replacement shell", async () => {
    const old = deferred<number>();
    ports.shellSessionOpen.mockReturnValueOnce(old.promise);
    const oldRun = execute("bash_run", { command: "pwd" });
    const release = releaseSessionShells("session");
    ports.shellSessionOpen.mockResolvedValueOnce(2);
    expect((await execute("bash_run", { command: "pwd" })).exit_code).toBe(0);
    old.reject(new Error("late rejection"));
    expect((await oldRun).error).toContain("late rejection");
    await release;
    expect((await execute("bash_run", { command: "pwd" })).exit_code).toBe(0);
    expect(ports.shellSessionOpen).toHaveBeenCalledTimes(2);
    const calls = ports.shellSessionRun.mock.calls;
    expect(calls[calls.length - 1]?.[0]).toBe(2);
  });

  it("does not release another session whose id starts with the destroyed id", async () => {
    ports.shellSessionOpen.mockResolvedValueOnce(1).mockResolvedValueOnce(2);
    await execute("bash_run", { command: "pwd" });
    await execute("bash_run", { command: "pwd" }, options, "session:other");
    await releaseSessionShells("session");
    expect(ports.shellSessionClose).toHaveBeenCalledExactlyOnceWith(1);
    await execute("bash_run", { command: "pwd" }, options, "session:other");
    expect(ports.shellSessionOpen).toHaveBeenCalledTimes(2);
  });

  it("freezes the opening workspace and closes all scopes on destruction", async () => {
    const opening = deferred<number>();
    ports.shellSessionOpen.mockReturnValueOnce(opening.promise);
    const running = execute("bash_run", { command: "pwd" });
    const local = workspace.env;
    workspace.env = { kind: "wsl", distro: "Debian" };
    opening.resolve(1);
    await running;
    expect(ports.shellSessionRun.mock.calls[0][5]).toBe(local);
    ports.shellSessionOpen.mockResolvedValueOnce(2);
    await execute("bash_run", { command: "pwd" });
    expect(ports.shellSessionOpen).toHaveBeenCalledTimes(2);
    await releaseSessionShells("session");
    expect(ports.shellSessionClose).toHaveBeenCalledWith(1);
    expect(ports.shellSessionClose).toHaveBeenCalledWith(2);
  });
});

describe("SDK log pagination", () => {
  it("preserves page offsets and supports the existing incremental read", async () => {
    const page = { bytes: "日志", next_offset: 6, has_more: true, dropped: 0 };
    ports.shellBgLogs.mockResolvedValue(page);
    expect(await execute("bash_logs", { handle: 9, since_offset: 0 })).toEqual(page);
    expect(ports.shellBgLogs).toHaveBeenCalledWith(9, 0, 64 * 1024);
    await execute("bash_logs", { handle: 9, since_offset: 6, max_bytes: 32 });
    expect(ports.shellBgLogs).toHaveBeenCalledWith(9, 6, 32);
  });
});
