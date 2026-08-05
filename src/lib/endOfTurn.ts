/**
 * Unified end-of-turn reason mapping for transcript chips.
 */

export type EndOfTurnReason =
  | "user_stop"
  | "agent_exit"
  | "stall"
  | "error"
  | "cancelled"
  | "unknown";

export interface EndOfTurnChipModel {
  reason: EndOfTurnReason;
  /** i18n message key under activity.* / endOfTurn.* */
  messageKey:
    | "activity.cancelledByUser"
    | "activity.cancelledAgentExit"
    | "activity.cancelled"
    | "endOfTurn.stall"
    | "endOfTurn.error"
    | "endOfTurn.unknown";
  tone: "neutral" | "warning" | "error";
}

/** Normalize host/UI reason strings into a chip model. */
export function mapEndOfTurnReason(
  raw: string | null | undefined,
): EndOfTurnChipModel {
  const r = (raw || "").toLowerCase().trim();
  if (
    r === "user_stop" ||
    r === "user" ||
    r === "stop" ||
    r === "cancelled_by_user" ||
    r === "user_cancel"
  ) {
    return {
      reason: "user_stop",
      messageKey: "activity.cancelledByUser",
      tone: "neutral",
    };
  }
  if (r === "agent_exit" || r === "agent" || r === "process_exit") {
    return {
      reason: "agent_exit",
      messageKey: "activity.cancelledAgentExit",
      tone: "warning",
    };
  }
  if (r === "stall" || r === "stream_stall" || r === "idle_timeout") {
    return {
      reason: "stall",
      messageKey: "endOfTurn.stall",
      tone: "warning",
    };
  }
  if (r === "error" || r === "failed" || r === "turn_error") {
    return {
      reason: "error",
      messageKey: "endOfTurn.error",
      tone: "error",
    };
  }
  if (r === "cancelled" || r === "canceled" || r === "turn_cancelled") {
    return {
      reason: "cancelled",
      messageKey: "activity.cancelled",
      tone: "neutral",
    };
  }
  return {
    reason: "unknown",
    messageKey: "endOfTurn.unknown",
    tone: "neutral",
  };
}

/** Markers that should render as EndOfTurnChip family. */
export function isEndOfTurnMarker(marker: string | null | undefined): boolean {
  const m = (marker || "").toLowerCase();
  return (
    m === "turn_cancelled" ||
    m === "turn_end" ||
    m === "stream_stall" ||
    m === "end_of_turn"
  );
}

/**
 * Build content for applyTurnMarker so journal reload stays consistent.
 */
export function endOfTurnMarkerContent(reason: EndOfTurnReason): string {
  return `turn_end|${reason}`;
}

export function parseEndOfTurnContent(
  content: string | null | undefined,
): EndOfTurnReason | null {
  if (!content) return null;
  if (content.startsWith("turn_end|")) {
    return mapEndOfTurnReason(content.slice("turn_end|".length)).reason;
  }
  if (content.startsWith("turn_cancelled")) {
    return "cancelled";
  }
  return null;
}
