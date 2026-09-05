import { describe, expect, it } from "vitest";
import {
  getGoalMutationEpoch,
  invalidateGoalListRequests,
} from "./goalSync";

describe("Goal 列表同步纪元", () => {
  it("按 Session 推进并隔离在途列表失效通知", () => {
    const sessionId = "goal-sync-test-session";
    const otherSessionId = "goal-sync-test-other-session";
    const before = getGoalMutationEpoch(sessionId);
    const otherBefore = getGoalMutationEpoch(otherSessionId);

    expect(invalidateGoalListRequests(sessionId)).toBe(before + 1);
    expect(getGoalMutationEpoch(sessionId)).toBe(before + 1);
    expect(getGoalMutationEpoch(otherSessionId)).toBe(otherBefore);
  });
});
