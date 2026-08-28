import { useCallback, useRef, useState } from "react";
import type { TurnLatencyState } from "@/lib/turnLatency";
import type {
  SessionTurnState,
  SessionTurnStateRefs,
} from "./types";

export function useSessionTurnState(
  stateRefs?: SessionTurnStateRefs,
): SessionTurnState {
  const [connecting, setConnecting] = useState(false);
  const connectingRef = useRef(false);
  const localSendInFlightRef = useRef(false);
  const localTurnLatencyBySessionRef = useRef<Map<string, TurnLatencyState>>(
    new Map(),
  );
  const localActiveTurnIdBySessionRef = useRef<Map<string, string>>(new Map());
  const localRecoverableCompletedTurnIdBySessionRef = useRef<
    Map<string, string>
  >(new Map());
  const localCompletedTurnIdBySessionRef = useRef<Map<string, string>>(
    new Map(),
  );
  const localPendingVisibleTurnBySessionRef = useRef<Map<string, string>>(
    new Map(),
  );

  const sendInFlightRef = stateRefs?.sendInFlightRef ?? localSendInFlightRef;
  const turnLatencyBySessionRef =
    stateRefs?.turnLatencyBySessionRef ?? localTurnLatencyBySessionRef;
  const activeTurnIdBySessionRef =
    stateRefs?.activeTurnIdBySessionRef ?? localActiveTurnIdBySessionRef;
  const recoverableCompletedTurnIdBySessionRef =
    stateRefs?.recoverableCompletedTurnIdBySessionRef ??
    localRecoverableCompletedTurnIdBySessionRef;
  const completedTurnIdBySessionRef =
    stateRefs?.completedTurnIdBySessionRef ?? localCompletedTurnIdBySessionRef;
  const pendingVisibleTurnBySessionRef =
    stateRefs?.pendingVisibleTurnBySessionRef ??
    localPendingVisibleTurnBySessionRef;

  const observeHostActiveTurnInternal = useCallback(
    (snapshot: {
      sessionId?: string | null;
      activeTurnId?: string | null;
    }) => {
      const sessionId = snapshot.sessionId;
      if (!sessionId) return;
      const localLatency = turnLatencyBySessionRef.current.get(sessionId);
      const resolved = resolveActiveTurn({
        snapshotTurnId: snapshot.activeTurnId,
        localTurnId: localLatency?.turnId,
        completedTurnId: completedTurnIdBySessionRef.current.get(sessionId),
      });
      if (resolved) activeTurnIdBySessionRef.current.set(sessionId, resolved);
      else activeTurnIdBySessionRef.current.delete(sessionId);
      if (
        resolved &&
        recoverableCompletedTurnIdBySessionRef.current.get(sessionId) !==
          resolved
      ) {
        recoverableCompletedTurnIdBySessionRef.current.delete(sessionId);
      }
    },
    [],
  );

  return {
    connecting,
    connectingRef,
    setConnectingState: useCallback((value: boolean) => {
      connectingRef.current = value;
      setConnecting(value);
    }, []),
    sendInFlightRef,
    turnLatencyBySessionRef,
    activeTurnIdBySessionRef,
    recoverableCompletedTurnIdBySessionRef,
    completedTurnIdBySessionRef,
    pendingVisibleTurnBySessionRef,
    observeHostActiveTurn:
      stateRefs?.observeHostActiveTurn ?? observeHostActiveTurnInternal,
  };
}

function resolveActiveTurn(input: {
  snapshotTurnId?: string | null;
  localTurnId?: string;
  completedTurnId?: string;
}): string | null {
  const snapshot = input.snapshotTurnId?.trim() || null;
  if (snapshot) {
    if (snapshot === input.completedTurnId) return null;
    return snapshot;
  }
  if (input.localTurnId && input.localTurnId !== input.completedTurnId) {
    return input.localTurnId;
  }
  return null;
}
