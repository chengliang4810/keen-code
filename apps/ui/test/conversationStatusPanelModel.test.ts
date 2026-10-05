import { describe, expect, it } from "vitest";

import { resolveConversationStatusPanelSummaryFallback } from "../../../packages/ui/src/v4/conversationStatusPanelModel.ts";

describe("conversation status panel cold summary", () => {
  it("keeps the ended Agent directory reachable when cold state has no active work", () => {
    expect(
      resolveConversationStatusPanelSummaryFallback({
        hasPrimarySummary: false,
        runningCount: 0,
        endedSubagentCount: 1,
        endedWorkflowRunCount: 0,
      }),
    ).toBe("endedAgents");
  });

  it("does not replace a primary or running summary with a terminal count", () => {
    expect(
      resolveConversationStatusPanelSummaryFallback({
        hasPrimarySummary: true,
        runningCount: 0,
        endedSubagentCount: 1,
        endedWorkflowRunCount: 1,
      }),
    ).toBeNull();
    expect(
      resolveConversationStatusPanelSummaryFallback({
        hasPrimarySummary: false,
        runningCount: 1,
        endedSubagentCount: 1,
        endedWorkflowRunCount: 1,
      }),
    ).toBeNull();
  });
});
