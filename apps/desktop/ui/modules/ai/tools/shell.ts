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
      entry.opening.then((id) => native.shellSessionClose(id), () => {}),
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

    bash_background: tool({
      description:
        "Spawn a long-running background process (e.g. `pnpm dev`, `cargo watch`, log tailers). Returns a handle; use `bash_logs` to read its output and `bash_kill` to stop it. Output is captured into a 4MB ring buffer.",
      inputSchema: z.object({
        command: z.string(),
        cwd: z.string().nullable().optional(),
      }),
      needsApproval: true,
      execute: async ({ command, cwd }, { abortSignal }) => {
        if (abortSignal?.aborted) return { error: "shell command cancelled" };
        const safety = checkShellCommand(command);
        if (!safety.ok) return { error: safety.reason };
        const effectiveCwd = cwd ?? ctx.getCwd();
        try {
          const handle = await native.shellBgSpawn(command, effectiveCwd);
          return { handle, command, cwd: effectiveCwd, ok: true };
        } catch (e) {
          return { error: String(e) };
        }
      },
    }),

    bash_logs: tool({
      description:
        "Read a page of logs from a `bash_background` process (64 KiB by default, at most 128 KiB). Pass `since_offset` from the previous response's `next_offset` to read the next page when `has_more` is true or to tail incrementally. `dropped` reports bytes evicted by the ring buffer.",
      inputSchema: z.object({
        handle: z.number().int(),
        since_offset: z.number().int().optional(),
        max_bytes: z.number().int().min(1).max(128 * 1024).optional(),
      }),
      execute: async ({ handle, since_offset, max_bytes }, { abortSignal }) => {
        try {
          requireActive(abortSignal);
          const r = await native.shellBgLogs(
            handle,
            since_offset,
            max_bytes ?? 64 * 1024,
          );
          return r;
        } catch (e) {
          return { error: String(e) };
        }
      },
    }),

    bash_list: tool({
      description:
        "List all background processes spawned by `bash_background` in this app - running and exited. **Always call this BEFORE spawning a new long-running process** (especially dev servers like `pnpm dev`, `next dev`, `vite`) to avoid duplicates. If a matching process is already running, reuse it (call `open_preview` again instead of respawning). Auto-executes.",
      inputSchema: z.object({}),
      execute: async (_, { abortSignal }) => {
        try {
          requireActive(abortSignal);
          const list = await native.shellBgList();
          return { processes: list };
        } catch (e) {
          return { error: String(e) };
        }
      },
    }),

    bash_kill: tool({
      description:
        "Terminate a `bash_background` process by handle. Idempotent - kills nothing if the handle is unknown or already exited.",
      inputSchema: z.object({ handle: z.number().int() }),
      execute: async ({ handle }, { abortSignal }) => {
        try {
          requireActive(abortSignal);
          await native.shellBgKill(handle);
          return { handle, ok: true };
        } catch (e) {
          return { error: String(e) };
        }
      },
    }),
  } as const;
}
