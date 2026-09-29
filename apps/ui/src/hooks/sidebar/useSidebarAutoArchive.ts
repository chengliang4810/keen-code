import { useEffect, useRef, useState } from "react";
import type { SessionRow } from "@/features/app/models";
import {
  createOperationId,
  sessionSetPreference,
} from "@/lib/acp/api";

export interface SidebarAutoArchiveOptions {
  sessions: SessionRow[];
  autoArchiveConversations: boolean;
  archiveRetentionDays: number;
  /** 归档提交成功后刷新权威列表。 */
  refreshSessions: () => Promise<void>;
}

/** 把超过保留期且未置顶的会话标记为权威归档状态。 */
export function useSidebarAutoArchive({
  sessions,
  autoArchiveConversations,
  archiveRetentionDays,
  refreshSessions,
}: SidebarAutoArchiveOptions): void {
  const [archiveClock, setArchiveClock] = useState(0);
  // 同一会话只提交一次权威归档，避免刷新循环反复请求。
  const archivedRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    if (!autoArchiveConversations) return;
    const now = Date.now();
    const cutoff = now - archiveRetentionDays * 86_400_000;
    const expired = sessions.filter(
      (item) =>
        !item.pinned &&
        !item.archived &&
        !archivedRef.current.has(item.id) &&
        Number.isFinite(Date.parse(item.updatedAt)) &&
        Date.parse(item.updatedAt) <= cutoff,
    );
    if (expired.length > 0) {
      void (async () => {
        try {
          for (const item of expired) {
            await sessionSetPreference({
              id: item.id,
              archived: true,
              operationId: createOperationId("session-auto-archive"),
            });
            // 只在提交成功后标记，失败时下次 effect 仍会重试。
            archivedRef.current.add(item.id);
          }
          await refreshSessions();
        } catch (error) {
          console.warn("auto archive expired sessions failed", error);
        }
      })();
    }
    const nextExpiry = sessions.reduce((next, item) => {
      if (item.pinned || item.archived) return next;
      const expiry =
        Date.parse(item.updatedAt) + archiveRetentionDays * 86_400_000;
      return Number.isFinite(expiry) && expiry > now
        ? Math.min(next, expiry)
        : next;
    }, Number.POSITIVE_INFINITY);
    if (!Number.isFinite(nextExpiry)) return;
    const timer = window.setTimeout(
      () => setArchiveClock((value) => value + 1),
      Math.min(nextExpiry - now, 2_147_483_647),
    );
    return () => window.clearTimeout(timer);
  }, [archiveClock, archiveRetentionDays, autoArchiveConversations, refreshSessions, sessions]);
}
