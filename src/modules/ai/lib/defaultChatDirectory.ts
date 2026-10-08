import { native } from "@/modules/ai/lib/native";

let directory: string | null = null;
let pending: Promise<string> | null = null;

export function getDefaultChatDirectory(): string | null {
  return directory;
}

export function ensureDefaultChatDirectory(): Promise<string> {
  if (directory) return Promise.resolve(directory);
  pending ??= native
    .defaultChatDirectory()
    .then((path) => {
      directory = path;
      return path;
    })
    .finally(() => {
      pending = null;
    });
  return pending;
}
