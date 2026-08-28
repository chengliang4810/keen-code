import { useEffect, useState } from "react";
import type { SessionRow } from "@/features/app/models";
import {
  autoArchiveExpiredSessions,
} from "@/lib/sessionPreferences";
import type { SidebarSetState } from "./types";

export interface SidebarAutoArchiveOptions {
  sessions: SessionRow[];
  setSessions: SidebarSetState<SessionRow[]>;
  autoArchiveConversations: boolean;
  archiveRetentionDays: number;
}

/** Keep local archive preferences reflected in the visible sidebar. */
export function useSidebarAutoArchive({
  sessions,
  setSessions,
  autoArchiveConversations,
  archiveRetentionDays,
}: SidebarAutoArchiveOptions): void {
  const [archiveClock, setArchiveClock] = useState(0);

  useEffect(() => {
    if (!autoArchiveConversations) return;
    const now = Date.now();
    const preferences = autoArchiveExpiredSessions(
      sessions,
      archiveRetentionDays,
      now,
    );
    setSessions((current) => {
      const next = current.map((item) => ({
        ...item,
        archived: preferences[item.id]?.archived ?? item.archived,
      }));
      return next.some(
        (item, index) => item.archived !== current[index]?.archived,
      )
        ? next
        : current;
    });
    const nextExpiry = sessions.reduce((next, item) => {
      const preference = preferences[item.id];
      if (preference?.pinned || preference?.archived) return next;
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
  }, [archiveClock, archiveRetentionDays, autoArchiveConversations, sessions, setSessions]);
}
