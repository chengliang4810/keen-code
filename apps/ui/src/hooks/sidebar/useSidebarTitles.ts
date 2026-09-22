import { useCallback, useRef, type MutableRefObject } from "react";
import type { SessionRow } from "@/features/app/models";
import type { SessionSnapshot } from "@/lib/session";
import {
  buildSessionTitleFromFirstMessage,
  canGenerateAutomaticSessionTitle,
  isPlaceholderSessionTitle,
  sanitizeGeneratedSessionTitle,
} from "@/lib/sessionTitle";
import {
  loadSessionPreferences,
  updateSessionPreference,
} from "@/lib/sessionPreferences";
import {
  createOperationId,
  sessionGenerateTitle,
  sessionRename as acpSessionRename,
} from "@/lib/acp/api";
import type {
  SidebarSetState,
  SidebarTranslator,
} from "./types";

export interface SidebarTitlesOptions {
  tr: SidebarTranslator;
  sessionsRef: MutableRefObject<SessionRow[]>;
  setSessions: SidebarSetState<SessionRow[]>;
  setSession: SidebarSetState<SessionSnapshot>;
}

export interface SidebarTitlesResult {
  sessionTitleOverridesRef: MutableRefObject<Map<string, string>>;
  applyMessagePrefixTitle: (sessionId: string, userText: string) => void;
  applyAutomaticSessionTitle: (
    sessionId: string,
    firstUserMessage: string,
    expectedTitle?: string | null,
  ) => Promise<void>;
  applySessionTitle: (sessionId: string, title: string) => void;
}

export function useSidebarTitles({
  tr,
  sessionsRef,
  setSessions,
  setSession,
}: SidebarTitlesOptions): SidebarTitlesResult {
  const sessionTitleOverridesRef = useRef<Map<string, string>>(new Map());
  const autoTitleInFlightRef = useRef<Set<string>>(new Set());
  const autoTitleAttemptedRef = useRef<Set<string>>(new Set());

  const applySessionTitle = useCallback(
    (sessionId: string, title: string) => {
      sessionTitleOverridesRef.current.set(sessionId, title);
      setSessions((list) =>
        list.map((item) => (item.id === sessionId ? { ...item, title } : item)),
      );
      setSession((previous) =>
        previous.sessionId === sessionId ? { ...previous, title } : previous,
      );
    },
    [setSession, setSessions],
  );

  const applyMessagePrefixTitle = useCallback(
    (sessionId: string, userText: string) => {
      const source = loadSessionPreferences()[sessionId]?.titleSource;
      if (
        source === "manual" ||
        source === "automatic" ||
        source === "message-prefix"
      ) {
        return;
      }
      const title = buildSessionTitleFromFirstMessage([
        { role: "user", content: userText },
      ]);
      if (!title) return;
      const currentTitle =
        sessionTitleOverridesRef.current.get(sessionId) ??
        sessionsRef.current.find((row) => row.id === sessionId)?.title ??
        null;
      if (
        !isPlaceholderSessionTitle(currentTitle, [
          tr("session.new"),
          tr("session.placeholderTitle"),
          tr("session.untitled"),
        ])
      ) {
        return;
      }
      updateSessionPreference(sessionId, {
        title,
        titleSource: "message-prefix",
      });
      applySessionTitle(sessionId, title);
      void acpSessionRename({
        id: sessionId,
        title,
        operationId: createOperationId("session-rename"),
      }).catch((error) =>
        console.warn("persist message-prefix session title failed", error),
      );
    },
    [applySessionTitle, sessionsRef, tr],
  );

  const applyAutomaticSessionTitle = useCallback(
    async (
      sessionId: string,
      firstUserMessage: string,
      expectedTitle?: string | null,
    ): Promise<void> => {
      if (
        autoTitleAttemptedRef.current.has(sessionId) ||
        autoTitleInFlightRef.current.has(sessionId)
      ) {
        return;
      }
      const currentTitle =
        sessionTitleOverridesRef.current.get(sessionId) ??
        sessionsRef.current.find((row) => row.id === sessionId)?.title ??
        expectedTitle;
      const canReplaceCurrentTitle = canGenerateAutomaticSessionTitle({
        currentTitle,
        titleSource: loadSessionPreferences()[sessionId]?.titleSource,
        localizedPlaceholders: [
          tr("session.new"),
          tr("session.placeholderTitle"),
          tr("session.untitled"),
        ],
      });
      if (!canReplaceCurrentTitle) return;

      autoTitleAttemptedRef.current.add(sessionId);
      autoTitleInFlightRef.current.add(sessionId);
      try {
        const candidate = await sessionGenerateTitle({
          id: sessionId,
          userMessage: firstUserMessage,
          operationId: createOperationId("session-title"),
        });
        const title = sanitizeGeneratedSessionTitle(candidate);
        if (!title) return;

        const latestPreferences = loadSessionPreferences()[sessionId];
        const latestTitle =
          sessionTitleOverridesRef.current.get(sessionId) ??
          sessionsRef.current.find((row) => row.id === sessionId)?.title ??
          expectedTitle;
        const canReplaceLatestTitle = canGenerateAutomaticSessionTitle({
          currentTitle: latestTitle,
          titleSource: latestPreferences?.titleSource,
          localizedPlaceholders: [
            tr("session.new"),
            tr("session.placeholderTitle"),
            tr("session.untitled"),
          ],
        });
        if (!canReplaceLatestTitle) return;

        updateSessionPreference(sessionId, {
          title,
          titleSource: "automatic",
        });
        applySessionTitle(sessionId, title);
        try {
          await acpSessionRename({
            id: sessionId,
            title,
            operationId: createOperationId("session-rename"),
          });
        } catch (error) {
          console.warn("persist generated session title failed", error);
        }
      } catch (error) {
        console.warn("generate session title failed", error);
      } finally {
        autoTitleInFlightRef.current.delete(sessionId);
      }
    },
    [applySessionTitle, sessionsRef, tr],
  );

  return {
    sessionTitleOverridesRef,
    applyMessagePrefixTitle,
    applyAutomaticSessionTitle,
    applySessionTitle,
  };
}
