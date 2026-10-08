import { LazyStore } from "@/lib/storage";

const disk = new LazyStore("ui", { defaults: {}, autoSave: 200 });
const values = new Map<string, string>();
const pending = new Map<string, string | null>();
let ready = false;
let writing: Promise<void> | undefined;
let scheduled: ReturnType<typeof setTimeout> | undefined;

function schedule(): void {
  if (!ready || scheduled !== undefined) return;
  scheduled = setTimeout(() => {
    scheduled = undefined;
    void flush().catch((error) =>
      console.error("Could not save RCode UI state", error),
    );
  }, 100);
}

function flush(): Promise<void> {
  if (writing) return writing.then(flush);
  if (!ready || !pending.size) return Promise.resolve();
  writing = drain().finally(() => {
    writing = undefined;
  });
  return writing;
}

async function drain(): Promise<void> {
  while (pending.size) {
    const batch = [...pending];
    pending.clear();
    try {
      for (const [key, value] of batch) {
        if (value === null) await disk.delete(key);
        else await disk.set(key, value);
      }
      await disk.save();
    } catch (error) {
      for (const [key, value] of batch)
        if (!pending.has(key)) pending.set(key, value);
      throw error;
    }
  }
}

export function hasPendingUiState(): boolean {
  return pending.size > 0 || writing !== undefined;
}

export function flushUiState(): Promise<void> {
  if (scheduled !== undefined) {
    clearTimeout(scheduled);
    scheduled = undefined;
  }
  return flush();
}

export async function hydrateUiState(): Promise<void> {
  const entries = await disk.entries<unknown>();
  for (const [key, value] of entries) {
    if (typeof value === "string" && !pending.has(key)) values.set(key, value);
  }
  ready = true;
  await flush();
}

export const uiState = {
  getItem(key: string): string | null {
    return values.get(key) ?? null;
  },
  setItem(key: string, value: string): void {
    if (values.get(key) === value) return;
    values.set(key, value);
    pending.set(key, value);
    schedule();
  },
  removeItem(key: string): void {
    if (!values.delete(key)) return;
    pending.set(key, null);
    schedule();
  },
};
