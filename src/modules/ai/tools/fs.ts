import { native } from "@/modules/ai/lib/native";
import {
  checkReadableCanonical,
  checkWritableCanonical,
} from "@/modules/ai/lib/security";
import { newQueuedEditId, usePlanStore } from "@/modules/ai/store/planStore";
import {
  resolvePath,
  type ToolContext,
  taskFileScope,
  withFileMutation,
} from "@/modules/ai/tools/context";
import { tool } from "ai";
import { z } from "zod";

const READ_BYTE_CAP = 25 * 1024;
const READ_LINE_CAP = 2000;

function djb2(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  return h >>> 0;
}

export function buildFsTools(ctx: ToolContext) {
  const scope = taskFileScope(ctx);
  const canonicalize = (path: string) => native.canonicalize(path, scope);
  return {
    read_file: tool({
      description:
        "Read a UTF-8 text file. Defaults to the first 2000 lines (capped at 25KB). Pass `offset`/`limit` for line-based windowing of large files. Refuses binary, oversized, or sensitive files (.env, keys, credentials). If you call this on the same path twice in a session without edits in between, the second call returns `unchanged: true` instead of re-emitting the content - re-read the prior tool result.",
      inputSchema: z.object({
        path: z
          .string()
          .describe("Absolute path, or relative to the active terminal cwd."),
        offset: z
          .number()
          .int()
          .min(0)
          .optional()
          .describe("0-based start line. Default 0."),
        limit: z
          .number()
          .int()
          .min(1)
          .max(10000)
          .optional()
          .describe("Max lines to return. Default 2000."),
      }),
      execute: async ({ path, offset, limit }) => {
        const reqPath = resolvePath(path, ctx.getCwd());
        const safety = await checkReadableCanonical(reqPath, canonicalize);
        if (!safety.ok) return { error: safety.reason, path: reqPath };
        const abs = safety.canonical;
        try {
          const r = await native.readFile(abs, scope);
          if (r.kind === "binary") {
            ctx.readCache.delete(abs);
            return { error: "binary file refused", path: abs, size: r.size };
          }
          if (r.kind === "toolarge")
            return {
              error: `file too large (${r.size} bytes, limit ${r.limit})`,
              path: abs,
            };

          const hash = djb2(r.content);
          const isFullRead = offset === undefined && limit === undefined;
          const prior = ctx.readCache.get(abs);
          ctx.readCache.set(abs, { size: r.size, hash, version: r.version });
          if (
            isFullRead &&
            prior &&
            prior.size === r.size &&
            prior.hash === hash
          ) {
            return { path: abs, unchanged: true, size: r.size };
          }

          if (isFullRead) {
            const lines = r.content.split("\n");
            const sliceEnd = Math.min(lines.length, READ_LINE_CAP);
            let content = lines.slice(0, sliceEnd).join("\n");
            let truncated = sliceEnd < lines.length;
            if (content.length > READ_BYTE_CAP) {
              content = content.slice(0, READ_BYTE_CAP);
              truncated = true;
            }
            return {
              path: abs,
              content,
              size: r.size,
              total_lines: lines.length,
              ...(truncated
                ? {
                    truncated: true,
                    hint: "call read_file with offset to continue",
                  }
                : {}),
            };
          }

          const lines = r.content.split("\n");
          const start = offset ?? 0;
          const requested = limit ?? READ_LINE_CAP;
          const end = Math.min(lines.length, start + requested);
          let content = lines.slice(start, end).join("\n");
          let truncated = end < lines.length;
          if (content.length > READ_BYTE_CAP) {
            content = content.slice(0, READ_BYTE_CAP);
            truncated = true;
          }
          return {
            path: abs,
            content,
            size: r.size,
            total_lines: lines.length,
            start_line: start,
            end_line: end,
            ...(truncated ? { truncated: true } : {}),
          };
        } catch (e) {
          return { error: String(e), path: abs };
        }
      },
    }),

    list_directory: tool({
      description:
        "List immediate entries (files + directories) in a directory. Hidden entries are omitted.",
      inputSchema: z.object({
        path: z
          .string()
          .describe("Absolute path, or relative to the active terminal cwd."),
      }),
      execute: async ({ path }) => {
        const reqPath = resolvePath(path, ctx.getCwd());
        const safety = await checkReadableCanonical(reqPath, canonicalize);
        if (!safety.ok) return { error: safety.reason, path: reqPath };
        const abs = safety.canonical;
        try {
          const entries = await native.readDir(abs, scope);
          return {
            path: abs,
            entries: entries.map((e) => ({ name: e.name, kind: e.kind })),
          };
        } catch (e) {
          return { error: String(e), path: abs };
        }
      },
    }),

    write_file: tool({
      description:
        "Create or overwrite a file with the given content. Prefer `edit` / `multi_edit` for in-place changes - only use `write_file` for creating a brand-new file or fully replacing a tiny one. Call `read_file` on this path before overwriting an existing text file.",
      inputSchema: z.object({
        path: z.string(),
        content: z.string(),
      }),
      needsApproval: true,
      execute: async ({ path, content }, { abortSignal }) => {
        const reqPath = resolvePath(path, ctx.getCwd());
        const safety = await checkWritableCanonical(reqPath, canonicalize);
        if (!safety.ok) return { error: safety.reason, path: reqPath };
        const abs = safety.canonical;

        if (usePlanStore.getState().active) {
          let original = "";
          let isNewFile = false;
          let expectedVersion: string | null = null;
          try {
            const r = await native.readFile(abs, scope);
            if (r.kind === "text") {
              original = r.content;
              expectedVersion = r.version;
            } else if (r.kind === "binary") {
              expectedVersion = r.version;
            } else {
              const snapshot = await native.binaryFileSnapshot(abs, scope);
              expectedVersion = snapshot?.version ?? null;
            }
          } catch {
            isNewFile = true;
          }
          usePlanStore.getState().enqueue({
            id: newQueuedEditId(),
            kind: "write_file",
            path: abs,
            originalContent: original,
            proposedContent: content,
            isNewFile,
            scope,
            expectedVersion,
          });
          return {
            path: abs,
            queued_for_plan_review: true,
            ok: true,
          };
        }

        try {
          return await withFileMutation(
            scope,
            abs,
            async () => {
              let expectedVersion = ctx.readCache.get(abs)?.version ?? null;
              if (!expectedVersion) {
                let snapshot:
                  | { size: number; version: string }
                  | null
                  | undefined;
                try {
                  snapshot = await native.binaryFileSnapshot(abs, scope);
                } catch {
                  // A null CAS still permits only creation if the target is absent.
                }
                if (snapshot === null)
                  return {
                    error:
                      "must call read_file on this path before overwriting an existing text file",
                    path: abs,
                  };
                if (snapshot) {
                  if (!snapshot.version)
                    return {
                      error: "binary file snapshot has no version",
                      path: abs,
                    };
                  expectedVersion = snapshot.version;
                }
              }
              if (abortSignal?.aborted)
                throw new Error("file mutation cancelled");
              const result = await native.writeFile(abs, content, {
                scope,
                expectedVersion,
              });
              ctx.readCache.set(abs, {
                size: content.length,
                hash: djb2(content),
                version: result.version,
              });
              return { path: abs, bytesWritten: content.length, ok: true };
            },
            abortSignal,
          );
        } catch (e) {
          return { error: String(e), path: abs };
        }
      },
    }),

    create_directory: tool({
      description: "Create a directory (and any missing parents).",
      inputSchema: z.object({
        path: z.string(),
      }),
      needsApproval: true,
      execute: async ({ path }) => {
        const reqPath = resolvePath(path, ctx.getCwd());
        const safety = await checkWritableCanonical(reqPath, canonicalize);
        if (!safety.ok) return { error: safety.reason, path: reqPath };
        const abs = safety.canonical;
        if (usePlanStore.getState().active) {
          usePlanStore.getState().enqueue({
            id: newQueuedEditId(),
            kind: "create_directory",
            path: abs,
            originalContent: "",
            proposedContent: "",
            isNewFile: true,
            description: "Create directory",
            scope,
          });
          return { path: abs, queued_for_plan_review: true, ok: true };
        }
        try {
          await native.createDir(abs, scope);
          return { path: abs, ok: true };
        } catch (e) {
          return { error: String(e), path: abs };
        }
      },
    }),
  } as const;
}
