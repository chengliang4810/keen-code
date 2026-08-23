/** Stream-stall classification and copy selection shared by the chat surface. */

/**
 * Stall copy tiers — never use pre-token when tools or assistant body exist.
 * Aligns with Host `stream_stall::StallTier`.
 */
export type StallTier =
  | "pre_first_token"
  | "working_tools"
  | "post_output"
  | "maybe_done";

export function stallTierFromProgress(input: {
  sawModelOutput: boolean;
  sawToolActivity?: boolean;
  terminalCandidate?: boolean;
}): StallTier {
  if (input.terminalCandidate) return "maybe_done";
  if (input.sawModelOutput) return "post_output";
  if (input.sawToolActivity) return "working_tools";
  return "pre_first_token";
}

export function stallMessageKey(tier: StallTier):
  | "endOfTurn.stallPreToken"
  | "endOfTurn.stallWorkingTools"
  | "endOfTurn.stall"
  | "endOfTurn.stallMaybeDone" {
  switch (tier) {
    case "pre_first_token":
      return "endOfTurn.stallPreToken";
    case "working_tools":
      return "endOfTurn.stallWorkingTools";
    case "maybe_done":
      return "endOfTurn.stallMaybeDone";
    case "post_output":
    default:
      return "endOfTurn.stall";
  }
}

/** Normalize host-emitted tier strings. */
export function normalizeStallTier(
  raw: string | null | undefined,
): StallTier | null {
  if (!raw) return null;
  const t = raw.toLowerCase().trim();
  if (t === "pre_first_token") return "pre_first_token";
  if (t === "working_tools") return "working_tools";
  if (t === "post_output") return "post_output";
  if (t === "maybe_done") return "maybe_done";
  return null;
}
