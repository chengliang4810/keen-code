import { useCallback, useRef, useState } from "react";
import type { TurnLatencyState } from "@/lib/turnLatency";
import { reconcileHostActiveTurnSnapshot } from "@/lib/activeTurn";
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
      reconcileHostActiveTurnSnapshot(snapshot, {
        turnLatencyBySession: turnLatencyBySessionRef.current,
        activeTurnIdBySession: activeTurnIdBySessionRef.current,
        recoverableCompletedTurnIdBySession:
          recoverableCompletedTurnIdBySessionRef.current,
        completedTurnIdBySession: completedTurnIdBySessionRef.current,
      });
    },
    [
      activeTurnIdBySessionRef,
      completedTurnIdBySessionRef,
      recoverableCompletedTurnIdBySessionRef,
      turnLatencyBySessionRef,
    ],
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
