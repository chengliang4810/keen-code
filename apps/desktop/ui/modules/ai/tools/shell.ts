import { tool } from "ai";
import { z } from "zod";
import { native } from "@/modules/ai/lib/native";
import { checkShellCommand } from "@/modules/ai/lib/security";
import type { ToolContext } from "@/modules/ai/tools/context";
import {
  currentWorkspaceEnv,
  workspaceScopeKey,
  type WorkspaceEnv,
} from "@/modules/workspace";

type SessionShell = { sessionId: string; opening: Promise<number> };
const sessionShells = new Map<string, SessionShell>();

async function getSessionShell(
  sessionId: string,
  cwd: string | null,
  workspace: WorkspaceEnv,
): Promise<number> {
  const key = JSON.stringify([sessionId, workspaceScopeKey(workspace)]);
  let entry = sessionShells.get(key);
  if (!entry) {
    const created: SessionShell = {
      sessionId,
      opening: native.shellSessionOpen(cwd, workspace),
    };
    created.opening = created.opening.catch((error) => {
      if (sessionShells.get(key) === created) sessionShells.delete(key);
      throw error;
    });
    sessionShells.set(key, created);
    entry = created;
  }
  const id = await entry.opening;
  if (sessionShells.get(key) !== entry)
    throw new Error("shell session was released");
  return id;
}

export async function releaseSessionShells(sessionId: string): Promise<void> {
  const released: Promise<void>[] = [];
  for (const [key, entry] of sessionShells) {
    if (entry.sessionId !== sessionId) continue;
    sessionShells.delete(key);
    released.push(
      entry.opening.then(
        (id) => native.shellSessionClose(id),
        () => {},
      ),
    );
  }
  await Promise.all(released);
}

function requireActive(signal: AbortSignal | undefined): void {
  if (signal?.aborted) throw new Error("shell command cancelled");
}

export function buildShellTools(ctx: ToolContext) {
  return {
    bash_run: tool({
      description:
        "Run a foreground shell command in this session's persistent agent shell. cwd persists across calls (so `cd foo` then `bash_run pwd` works). Use for short-lived commands (lint, test, search, build). For long-running or daemon processes (dev servers, watch tasks), use `bash_background`. NEVER invoke interactive tools (vim, less, top) - they will hang.",
      inputSchema: z.object({
        command: z.string(),
        timeout_secs: z.number().int().min(1).max(300).optional(),
      }),
      needsApproval: true,
      execute: async ({ command, timeout_secs }, { abortSignal }) => {
        if (abortSignal?.aborted) return { error: "shell command cancelled" };
        const safety = checkShellCommand(command);
        if (!safety.ok) return { error: safety.reason };
        const sid = ctx.getSessionId();
        if (!sid) return { error: "no active chat session" };
        try {
          const cwd = ctx.getCwd();
          const workspace = currentWorkspaceEnv();
          const shellId = await getSessionShell(sid, cwd, workspace);
          requireActive(abortSignal);
          const callId = crypto.randomUUID();
          const running = native.shellSessionRun(
            shellId,
            command,
            cwd,
            timeout_secs,
            callId,
            workspace,
          );
          let cancellation: Promise<void> | undefined;
          const cancel = () => {
            cancellation ??= native.shellSessionCancel(shellId, callId);
            void cancellation.catch(() => {});
          };
          abortSignal?.addEventListener("abort", cancel, { once: true });
          if (abortSignal?.aborted) cancel();
          let r: Awaited<typeof running>;
          try {
            r = await running;
            if (cancellation) await cancellation;
            requireActive(abortSignal);
          } finally {
            abortSignal?.removeEventListener("abort", cancel);
          }
          return {
            command,
            stdout: r.stdout,
            stderr: r.stderr,
            exit_code: r.exit_code,
            timed_out: r.timed_out,
            truncated: r.truncated,
            cwd_after: r.cwd_after,
          };
        } catch (e) {
          return { error: String(e) };
        }
      },
    }),
  } as const;
}
