import type { ReasoningSelection } from "@/modules/ai/lib/reasoning";
import type { UIMessage } from "@ai-sdk/react";
import { LazyStore } from "@/lib/storage";
import type { PermissionMode } from "@/modules/ai/lib/permissions";

export type SessionMeta = {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  /** 首次提交后固定任务目录和运行环境，不随开发工具标签切换。 */
  workspaceRoot?: string;
  workspaceScope?: string;
  /** 对应现有 Space 的稳定 ID，项目是任务导航的一级节点。 */
  projectId?: string;
  /** 用户明确选择不关联项目；与需要自动迁移归属的旧会话区分。 */
  projectless?: boolean;
  archived?: boolean;
  permissionMode?: PermissionMode;
  reasoningSelection?: ReasoningSelection;
};

const STORE_PATH = "sessions";
const KEY_SESSIONS = "sessions";
const KEY_ACTIVE = "activeId";
const messagesKey = (id: string) => `messages:${id}`;

const store = new LazyStore(STORE_PATH, { defaults: {}, autoSave: 200 });

export type LoadedSessions = {
  sessions: SessionMeta[];
  activeId: string | null;
};

export async function loadAll(reload = false): Promise<LoadedSessions> {
  if (reload) await store.reload();
  // One IPC roundtrip via entries() rather than two parallel get()s. Per-
  // session messages are loaded lazily via `loadMessages` only when a
  // session is opened, so cold boot stays at a single store call.
  const entries = await store.entries();
  let sessions: SessionMeta[] | undefined;
  let activeId: string | null | undefined;
  for (const [k, v] of entries) {
    if (k === KEY_SESSIONS) {
      if (!Array.isArray(v) || v.some((session) => !validSession(session)))
        throw new Error("Invalid session metadata");
      sessions = v as SessionMeta[];
    } else if (k === KEY_ACTIVE) {
      if (v !== null && typeof v !== "string")
        throw new Error("Invalid active session ID");
      activeId = v;
    }
  }
  return { sessions: sessions ?? [], activeId: activeId ?? null };
}

export async function loadMessages(id: string): Promise<UIMessage[] | null> {
  const messages = await store.get<unknown>(messagesKey(id));
  if (messages === undefined || messages === null) return null;
  if (
    !Array.isArray(messages) ||
    messages.some(
      (message) =>
        !message ||
        typeof message !== "object" ||
        typeof message.id !== "string" ||
        !["system", "user", "assistant"].includes(message.role) ||
        !Array.isArray(message.parts) ||
        message.parts.some(
          (part: unknown) =>
            !part ||
            typeof part !== "object" ||
            !("type" in part) ||
            typeof part.type !== "string",
        ),
    )
  )
    throw new Error("Invalid conversation messages");
  return messages as UIMessage[];
}

function validSession(value: unknown): value is SessionMeta {
  if (!value || typeof value !== "object") return false;
  const session = value as SessionMeta;
  return (
    typeof session.id === "string" &&
    !!session.id &&
    typeof session.title === "string" &&
    Number.isFinite(session.createdAt) &&
    Number.isFinite(session.updatedAt)
  );
}

export async function saveSessionsList(sessions: SessionMeta[]): Promise<void> {
  await store.set(KEY_SESSIONS, sessions);
}

/** 设置页批量操作需要确认落盘后才报告成功。 */
export async function saveSessionsListAndFlush(
  sessions: SessionMeta[],
): Promise<void> {
  await saveSessionsList(sessions);
  await store.save();
}

export async function deleteArchivedSessionData(
  ids: readonly string[],
): Promise<void> {
  for (const id of ids) await store.delete(messagesKey(id));
  await store.save();
}

export async function saveActiveId(id: string | null): Promise<void> {
  await store.set(KEY_ACTIVE, id);
}

export async function saveMessages(
  id: string,
  messages: UIMessage[],
): Promise<void> {
  await store.set(messagesKey(id), messages);
}

export async function deleteSessionData(id: string): Promise<void> {
  await store.delete(messagesKey(id));
}

export function newSessionId(): string {
  return `s-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export function deriveTitle(messages: UIMessage[]): string {
  for (const m of messages) {
    if (m.role !== "user") continue;
    for (const p of m.parts) {
      if (p.type !== "text") continue;
      const text = (p as { text: string }).text
        .replace(/<terminal-context[\s\S]*?<\/terminal-context>\s*/g, "")
        .replace(/<selection[\s\S]*?<\/selection>\s*/g, "")
        .replace(/<file[\s\S]*?<\/file>\s*/g, "")
        .replace(/<context>[\s\S]*?<\/context>\s*/g, "")
        .trim();
      if (!text) continue;
      const first = text.split("\n")[0].trim();
      return first.length > 40 ? `${first.slice(0, 40)}…` : first;
    }
  }
  return "New chat";
}
