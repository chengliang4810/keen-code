import { create } from "zustand";
import { LazyStore } from "@/lib/storage";
import {
  defaultTaskSidebar,
  normalizeTaskSidebar,
  type TaskSidebarSnapshot,
} from "@/app/lib/taskSidebar";

const disk = new LazyStore("taskSidebars", {
  defaults: {},
  autoSave: 200,
});
const prefix = "task:";
const pending = new Map<string, TaskSidebarSnapshot>();
let timer: ReturnType<typeof setTimeout> | null = null;
let writeQueue = Promise.resolve();
let loading: Promise<void> | null = null;

/** 合并连续拖拽和终端 cwd 更新，避免每次布局变化都发送存储 IPC。 */
export function flushTaskSidebars(): Promise<void> {
  if (timer) clearTimeout(timer);
  timer = null;
  const batch = [...pending];
  pending.clear();
  writeQueue = writeQueue
    .catch(() => {})
    .then(async () => {
      for (const [id, snapshot] of batch)
        await disk.set(`${prefix}${id}`, snapshot);
      if (batch.length) await disk.save();
    });
  return writeQueue;
}

type State = {
  byTask: Record<string, TaskSidebarSnapshot>;
  hydrated: boolean;
  error: string | null;
  init: () => Promise<void>;
  update: (id: string, patch: Partial<TaskSidebarSnapshot>) => void;
  remove: (id: string) => void;
};

export const useTaskSidebars = create<State>((set, get) => ({
  byTask: {},
  hydrated: false,
  error: null,
  init: () => {
    if (loading) return loading;
    loading = disk
      .entries()
      .then((entries) => {
        const byTask: State["byTask"] = {};
        for (const [key, value] of entries) {
          if (key.startsWith(prefix))
            byTask[key.slice(prefix.length)] = normalizeTaskSidebar(value);
        }
        set({ byTask, hydrated: true });
      })
      .catch((error) => {
        // 读取失败仍允许本次使用，但不覆盖可能尚未成功读取的旧存档。
        set({ hydrated: true, error: String(error) });
      });
    return loading;
  },
  update: (id, patch) => {
    if (!get().hydrated) return;
    const previous = get().byTask[id] ?? defaultTaskSidebar();
    const snapshot = { ...previous, ...patch };
    if (
      JSON.stringify(previous) === JSON.stringify(snapshot) &&
      get().byTask[id]
    )
      return;
    set({ byTask: { ...get().byTask, [id]: snapshot } });
    if (get().error) return;
    pending.set(id, snapshot);
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      void flushTaskSidebars().catch((error) => set({ error: String(error) }));
    }, 500);
  },
  remove: (id) => {
    const { [id]: _removed, ...byTask } = get().byTask;
    pending.delete(id);
    set({ byTask });
    if (get().error) return;
    // 删除排在在途写入之后，防止已删除对话的布局被旧写入重新创建。
    writeQueue = writeQueue
      .catch(() => {})
      .then(async () => {
        await disk.delete(`${prefix}${id}`);
        await disk.save();
      })
      .catch((error) => set({ error: String(error) }));
  },
}));
