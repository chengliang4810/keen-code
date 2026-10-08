import type { ToolExecutionOptions } from "ai";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ToolContext } from "./context";

const nativeMock = vi.hoisted(() => ({
  shellSessionOpen: vi.fn(async () => 1),
  shellSessionRun: vi.fn(),
}));

const securityMock = vi.hoisted(() => ({
  checkShellCommand: vi.fn<
    (command: string) => { ok: true } | { ok: false; reason: string }
  >(() => ({ ok: true })),
}));

vi.mock("../lib/native", () => ({ native: nativeMock }));
vi.mock("../lib/security", () => securityMock);
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "local" }),
  workspaceScopeKey: () => "local",
}));

import { buildShellTools } from "./shell";

const toolOptions: ToolExecutionOptions = {
  toolCallId: "tool-call",
  messages: [],
};

function makeContext(sessionId: string | null = "session"): ToolContext {
  return {
    getCwd: () => "/workspace",
    getWorkspaceRoot: () => "/workspace",
    getTerminalContext: () => null,
    isActiveTerminalPrivate: () => false,
    injectIntoActivePty: () => false,
    openPreview: () => false,
    spawnAgent: () => null,
    readAgentOutput: () => null,
    readCache: new Map(),
    getSessionId: () => sessionId,
  } as unknown as ToolContext;
}

// biome-ignore lint/suspicious/noExplicitAny: tool results are heterogeneous.
type Result = Record<string, any>;

async function run(
  toolName: "bash_run",
  ctx: ToolContext,
  input: Record<string, unknown>,
): Promise<Result> {
  const execute = buildShellTools(ctx)[toolName].execute;
  if (!execute) throw new Error(`${toolName} has no execute`);
  return (await execute(input as never, toolOptions)) as unknown as Result;
}

beforeEach(() => {
  vi.clearAllMocks();
  securityMock.checkShellCommand.mockReturnValue({ ok: true });
  nativeMock.shellSessionOpen.mockResolvedValue(1);
});

describe("bash_run", () => {
  it("refuses a command rejected by the shell guard without running it", async () => {
    securityMock.checkShellCommand.mockReturnValue({
      ok: false,
      reason: "blocked command",
    });
    const r = await run("bash_run", makeContext(), { command: "rm -rf /" });
    expect(r.error).toContain("blocked");
    expect(securityMock.checkShellCommand).toHaveBeenCalledWith("rm -rf /");
    expect(nativeMock.shellSessionRun).not.toHaveBeenCalled();
  });

  it("errors when there is no active chat session", async () => {
    const r = await run("bash_run", makeContext(null), { command: "ls" });
    expect(r.error).toContain("no active chat session");
    expect(nativeMock.shellSessionRun).not.toHaveBeenCalled();
  });

  it("passes the shell result through on success", async () => {
    nativeMock.shellSessionRun.mockResolvedValue({
      stdout: "hi",
      stderr: "",
      exit_code: 0,
      timed_out: false,
      truncated: false,
      cwd_after: "/workspace/sub",
    });
    const r = await run("bash_run", makeContext(), { command: "echo hi" });
    expect(r.stdout).toBe("hi");
    expect(r.exit_code).toBe(0);
    expect(r.cwd_after).toBe("/workspace/sub");
  });
});
