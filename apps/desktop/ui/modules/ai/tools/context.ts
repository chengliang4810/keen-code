import type { AgentFileScope } from "@/modules/ai/lib/native";
import { currentWorkspaceEnv } from "@/modules/workspace";

export type FileReadCache = Map<
  string,
  { size: number; hash: number; version?: string }
>;

export type ToolContext = {
  /** Active terminal tab cwd, used to resolve relative paths. Null = home. */
  getCwd: () => string | null;
  /** Workspace root (explorer root). Used by tools that operate over the project. */
  getWorkspaceRoot: () => string | null;
  /** Last N lines of the active terminal buffer (or null if not a terminal tab). */
  getTerminalContext: () => string | null;
  isActiveTerminalPrivate: () => boolean;
  /**
   * Type a string into the active terminal at the prompt — without executing.
   * Returns false if there is no active terminal tab to inject into.
   */
  injectIntoActivePty: (text: string) => boolean;
  /** Open a new preview tab (in-app iframe) at the given URL. */
  openPreview: (url: string) => boolean;
  /** Spawn a Claude Code agent in a new terminal tab, bound to this session. */
  spawnAgent: (prompt: string) => { tabId: number; leafId: number } | null;
  /** Read the terminal scrollback tail of a managed agent's leaf. */
  readAgentOutput: (leafId: number) => string | null;
  readCache: FileReadCache;
  /** Active chat session id — used by tools that persist per-session state (todos). */
  getSessionId: () => string | null;
};

export function taskFileScope(ctx: ToolContext): AgentFileScope {
  const root = ctx.getWorkspaceRoot() ?? ctx.getCwd();
  if (!root) throw new Error("current task has no workspace root");
  return { root, workspace: currentWorkspaceEnv() };
}

const mutations = new Map<string, Promise<unknown>>();

export async function withFileMutation<T>(
  scope: AgentFileScope,
  path: string,
  mutate: () => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  const normalized =
    scope.workspace.kind === "local" && /^[A-Za-z]:/.test(path)
      ? path.replace(/\\/g, "/").toLowerCase()
      : path;
  const key = `${JSON.stringify(scope.workspace)}:${normalized}`;
  const previous = mutations.get(key) ?? Promise.resolve();
  const current = previous
    .catch(() => undefined)
    .then(() => {
      if (signal?.aborted) throw new Error("file mutation cancelled");
      return mutate();
    });
  mutations.set(key, current);
  try {
    return await current;
  } finally {
    if (mutations.get(key) === current) mutations.delete(key);
  }
}

export function resolvePath(rawPath: string, cwd: string | null): string {
  if (rawPath.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(rawPath))
    return rawPath;
  if (!cwd)
    throw new Error(
      `cannot resolve relative path "${rawPath}": no active terminal cwd. Pass an absolute path.`,
    );
  const sep = cwd.includes("\\") && !cwd.includes("/") ? "\\" : "/";
  return cwd.endsWith(sep) ? `${cwd}${rawPath}` : `${cwd}${sep}${rawPath}`;
}
