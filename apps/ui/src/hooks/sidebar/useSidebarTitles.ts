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

  /** 读取权威投影中的标题来源；行缺失时按未记录处理。 */
  const titleSourceOf = useCallback(
    (sessionId: string) =>
      sessionsRef.current.find((row) => row.id === sessionId)?.titleSource,
    [sessionsRef],
  );

  const applyMessagePrefixTitle = useCallback(
    (sessionId: string, userText: string) => {
      const source = titleSourceOf(sessionId);
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
      applySessionTitle(sessionId, title);
      void acpSessionRename({
        id: sessionId,
        title,
        operationId: createOperationId("session-rename"),
        source: "message-prefix",
      }).catch((error) =>
        console.warn("persist message-prefix session title failed", error),
      );
    },
    [applySessionTitle, sessionsRef, titleSourceOf, tr],
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
        titleSource: titleSourceOf(sessionId),
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

        const latestTitle =
          sessionTitleOverridesRef.current.get(sessionId) ??
          sessionsRef.current.find((row) => row.id === sessionId)?.title ??
          expectedTitle;
        const canReplaceLatestTitle = canGenerateAutomaticSessionTitle({
          currentTitle: latestTitle,
          titleSource: titleSourceOf(sessionId),
          localizedPlaceholders: [
            tr("session.new"),
            tr("session.placeholderTitle"),
            tr("session.untitled"),
          ],
        });
        if (!canReplaceLatestTitle) return;

        applySessionTitle(sessionId, title);
        try {
          await acpSessionRename({
            id: sessionId,
            title,
            operationId: createOperationId("session-rename"),
            source: "automatic",
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
    [applySessionTitle, sessionsRef, titleSourceOf, tr],
  );

  return {
    sessionTitleOverridesRef,
    applyMessagePrefixTitle,
    applyAutomaticSessionTitle,
    applySessionTitle,
  };
}
