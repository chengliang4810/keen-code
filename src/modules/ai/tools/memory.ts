import { tool, type ToolSet } from "ai";
import { z } from "zod";
import { memoryApi, type MemoryContext } from "@/modules/ai/lib/memory";

const name = z.string().min(4).max(100);

export function buildMemoryTools(memory?: MemoryContext): ToolSet {
  if (!memory) return {};
  const id = memory.id;
  return {
    memory_read: tool({
      description:
        "List project memory files, or read one Markdown file. Omit name to list.",
      inputSchema: z.object({ name: name.optional() }),
      execute: ({ name }, options) => {
        options.abortSignal?.throwIfAborted();
        return memoryApi.read(id, name ?? null);
      },
    }),
    memory_write: tool({
      description:
        "Atomically save a project memory Markdown file. Requires exact previous content, or null for a new file.",
      inputSchema: z.object({
        name,
        content: z.string().max(65536),
        expected: z.string().max(65536).nullable(),
      }),
      execute: async ({ name, content, expected }, options) => {
        options.abortSignal?.throwIfAborted();
        await memoryApi.change(id, name, content, expected);
        return { ok: true };
      },
    }),
    memory_delete: tool({
      description:
        "Delete an obsolete project memory file after reading it. Requires exact previous content.",
      inputSchema: z.object({ name, expected: z.string().max(65536) }),
      execute: async ({ name, expected }, options) => {
        options.abortSignal?.throwIfAborted();
        await memoryApi.change(id, name, null, expected);
        return { ok: true };
      },
    }),
  };
}
