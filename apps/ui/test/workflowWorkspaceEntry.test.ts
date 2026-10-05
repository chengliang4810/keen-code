import { expect, test } from "vitest";
import { isWorkflowRunWorkspaceOpenable } from "../../../packages/ui/src/app-shell/workflowWorkspaceEntry.js";

test("workspace run entry requires a successful journal node query", () => {
  expect(
    isWorkflowRunWorkspaceOpenable({ loaded: false, unavailable: false, nodeCount: 1 }),
  ).toBe(false);
  expect(
    isWorkflowRunWorkspaceOpenable({ loaded: true, unavailable: true, nodeCount: 1 }),
  ).toBe(false);
  expect(
    isWorkflowRunWorkspaceOpenable({ loaded: true, unavailable: false, nodeCount: 0 }),
  ).toBe(false);
  expect(
    isWorkflowRunWorkspaceOpenable({ loaded: true, unavailable: false, nodeCount: 1 }),
  ).toBe(true);
});
