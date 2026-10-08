import { type AgentFileScope, native } from "@/modules/ai/lib/native";
import { withFileMutation } from "@/modules/ai/tools/context";
import { create } from "zustand";

export type QueuedEdit = {
  id: string;
  /** Tool that produced the queued mutation. */
  kind: "write_file" | "edit" | "multi_edit" | "create_directory";
  path: string;
  /** Original file content (empty for new files / create_directory). */
  originalContent: string;
  /** Proposed full content after edit (empty for create_directory). */
  proposedContent: string;
  /** True if the file did not exist when the edit was queued. */
  isNewFile: boolean;
  /** Human-readable description, used for create_directory. */
  description?: string;
  scope: AgentFileScope;
  expectedVersion?: string | null;
};

type PlanState = {
  active: boolean;
  queue: QueuedEdit[];
  toggle: () => void;
  enable: () => void;
  disable: () => void;
  enqueue: (q: QueuedEdit) => void;
  removeOne: (id: string) => void;
  clear: () => void;
  /** Apply queued edits in order. Returns per-edit results. */
  applyAll: () => Promise<{ id: string; ok: boolean; error?: string }[]>;
};

let nextId = 1;
export function newQueuedEditId(): string {
  return `q-${Date.now().toString(36)}-${(nextId++).toString(36)}`;
}

export const usePlanStore = create<PlanState>((set, get) => ({
  active: false,
  queue: [],
  toggle: () =>
    set((s) => ({ active: !s.active, queue: s.active ? [] : s.queue })),
  enable: () => set({ active: true }),
  disable: () => set({ active: false, queue: [] }),
  enqueue: (q) => set((s) => ({ queue: [...s.queue, q] })),
  removeOne: (id) =>
    set((s) => ({ queue: s.queue.filter((q) => q.id !== id) })),
  clear: () => set({ queue: [] }),
  async applyAll() {
    const items = get().queue;
    const results: { id: string; ok: boolean; error?: string }[] = [];
    for (const q of items) {
      try {
        if (!q.scope) throw new Error("queued edit has no task file scope");
        if (q.kind === "create_directory") {
          await native.createDir(q.path, q.scope);
        } else {
          const apply = () =>
            native.writeFile(q.path, q.proposedContent, {
              expectedVersion: q.expectedVersion ?? null,
              scope: q.scope,
            });
          await withFileMutation(q.scope, q.path, apply);
        }
        results.push({ id: q.id, ok: true });
      } catch (e) {
        results.push({ id: q.id, ok: false, error: String(e) });
      }
    }
    set({ queue: [] });
    return results;
  },
}));
