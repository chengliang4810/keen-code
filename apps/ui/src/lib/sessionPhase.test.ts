import { describe, expect, it } from "vitest";
import {
  stallMessageKey,
  stallTierFromProgress,
  normalizeStallTier,
} from "./sessionPhase";

describe("sessionPhase", () => {
  it("stall tiers never pretends pre-token after tools or body", () => {
    expect(stallTierFromProgress({ sawModelOutput: false })).toBe(
      "pre_first_token",
    );
    expect(
      stallTierFromProgress({ sawModelOutput: false, sawToolActivity: true }),
    ).toBe("working_tools");
    expect(stallTierFromProgress({ sawModelOutput: true })).toBe("post_output");
    expect(
      stallTierFromProgress({
        sawModelOutput: true,
        terminalCandidate: true,
      }),
    ).toBe("maybe_done");
    expect(stallMessageKey("pre_first_token")).toBe("endOfTurn.stallPreToken");
    expect(stallMessageKey("working_tools")).toBe(
      "endOfTurn.stallWorkingTools",
    );
    expect(stallMessageKey("post_output")).toBe("endOfTurn.stall");
    expect(stallMessageKey("maybe_done")).toBe("endOfTurn.stallMaybeDone");
  });

  it("normalizeStallTier maps host strings", () => {
    expect(normalizeStallTier("post_output")).toBe("post_output");
    expect(normalizeStallTier("working_tools")).toBe("working_tools");
    expect(normalizeStallTier("nope")).toBeNull();
  });
});
