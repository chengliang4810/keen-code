import type { UIMessage } from "ai";
import { describe, expect, it } from "vitest";
import { taskFileChanges } from "./taskChanges";

function messages(parts: unknown[]): UIMessage[] {
  return [{ id: "assistant", role: "assistant", parts }] as UIMessage[];
}
describe("task file summary", () => {
  it("distinguishes pending approval, a successful write, and a tool failure", () => {
    expect(
      taskFileChanges(
        messages([
          {
            type: "tool-write_file",
            input: { path: "a.ts" },
            state: "approval-requested",
          },
          {
            type: "tool-edit",
            output: { path: "b.ts", ok: true },
            state: "output-available",
          },
          {
            type: "tool-edit",
            output: { path: "c.ts", error: "conflict" },
            state: "output-available",
          },
        ]),
      ),
    ).toEqual([
      { path: "a.ts", state: "pending" },
      { path: "b.ts", state: "applied" },
      { path: "c.ts", state: "failed" },
    ]);
  });
  it("does not describe a queued edit as applied or count reads and denied writes", () => {
    expect(
      taskFileChanges(
        messages([
          {
            type: "tool-read_file",
            input: { path: "a.ts" },
            state: "output-available",
          },
          {
            type: "tool-write_file",
            input: { path: "b.ts" },
            state: "output-denied",
          },
          {
            type: "tool-edit",
            input: { path: "c.ts" },
            output: { queued_for_plan_review: true },
            state: "output-available",
          },
        ]),
      ),
    ).toEqual([]);
  });
  it("updates a file's state from the latest mutation without duplicate rows", () => {
    expect(
      taskFileChanges(
        messages([
          { type: "tool-edit", input: { path: "a.ts" }, state: "output-error" },
          {
            type: "tool-edit",
            input: { path: "a.ts" },
            state: "output-available",
            output: { ok: true },
          },
        ]),
      ),
    ).toEqual([{ path: "a.ts", state: "applied" }]);
  });
});
