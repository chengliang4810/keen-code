import { describe, expect, it } from "vitest";
import type { UIMessage } from "ai";
import {
  attachmentPreviewUrl,
  buildConversationStatus,
  createConversationStatusSelector,
  groupConversationTurns,
  presentConversationTurn,
  workspaceGitStatus,
} from "@/modules/ai/lib/conversationPresentation";
import type { GitStatusSnapshot } from "@/modules/ai/lib/native";

function message(
  id: string,
  role: UIMessage["role"],
  parts: unknown[],
): UIMessage {
  return { id, role, parts } as UIMessage;
}

function tool(
  name: string,
  id: string,
  state: string,
  input: unknown = {},
  output?: unknown,
) {
  return {
    type: "dynamic-tool",
    toolName: name,
    toolCallId: id,
    state,
    input,
    output,
  };
}

describe("conversation turn presentation", () => {
  it("keeps multiple assistant steps with their user and retains completed turn identities", () => {
    const history = [
      message("u1", "user", []),
      message("a1", "assistant", []),
      message("a2", "assistant", []),
      message("u2", "user", []),
    ];
    const before = groupConversationTurns(history);
    const after = groupConversationTurns(
      [
        ...history,
        message("a3", "assistant", [
          { type: "text", text: "live", state: "streaming" },
        ]),
      ],
      before,
    );
    expect(after.map((turn) => turn.messages.map((entry) => entry.id))).toEqual(
      [
        ["u1", "a1", "a2"],
        ["u2", "a3"],
      ],
    );
    expect(after[0]).toBe(before[0]);
    expect(after[1]).not.toBe(before[1]);
  });

  it("never hides an approval inside completed work and preserves surrounding order", () => {
    const turns = groupConversationTurns([
      message("a1", "assistant", [
        { type: "reasoning", text: "inspect first" },
        tool("Read", "r1", "output-available", { file_path: "a.ts" }),
        { type: "step-start" },
        tool("Write", "w1", "approval-requested", { file_path: "a.ts" }),
        { type: "data-rcode-diagnostic", data: { message: "waiting" } },
        { type: "text", text: "Please review" },
      ]),
    ]);
    const groups = presentConversationTurn(turns[0]);
    expect(groups.map((group) => group.kind)).toEqual([
      "work",
      "approval",
      "notice",
      "response",
    ]);
    expect(groups[1].entries[0].key).toBe("a1:w1");
    expect(
      groups.flatMap((group) => group.entries).map((entry) => entry.part.type),
    ).not.toContain("step-start");
  });

  it("keeps commentary preceding a still-running tool in the execution process", () => {
    const [turn] = groupConversationTurns([
      message("a1", "assistant", [
        { type: "text", text: "Running tests" },
        tool("Bash", "b1", "input-available", { command: "pnpm test" }),
      ]),
    ]);
    expect(presentConversationTurn(turn).map((group) => group.kind)).toEqual([
      "work",
    ]);
  });

  it("keeps assistant-first histories and ignores hidden system messages", () => {
    const turns = groupConversationTurns([
      message("system", "system", []),
      message("restored", "assistant", []),
      message("next", "user", []),
    ]);
    expect(turns.map((turn) => turn.id)).toEqual(["restored", "next"]);
  });
});

describe("conversation status facts", () => {
  it("reuses the status model during text and file-body streaming but updates on execution results", () => {
    const select = createConversationStatusSelector();
    const write = tool("Write", "write", "input-streaming", {
      file_path: "a.ts",
      content: "a",
    });
    const first = select([
      message("a1", "assistant", [write, { type: "text", text: "one" }]),
    ]);
    const second = select([
      message("a1", "assistant", [
        { ...write, input: { file_path: "a.ts", content: "a lot more" } },
        { type: "text", text: "one two" },
      ]),
    ]);
    expect(second).toBe(first);
    const applied = select([
      message("a1", "assistant", [
        { ...write, state: "output-available", output: { ok: true } },
      ]),
    ]);
    expect(applied).not.toBe(first);
    expect(applied.files[0].state).toBe("applied");
  });
  it("counts only applied writes, retains earlier successful changes, and excludes review proposals", () => {
    const state = buildConversationStatus([
      message("a1", "assistant", [
        tool(
          "Write",
          "one",
          "output-available",
          { file_path: "src\\a.ts" },
          "Wrote file",
        ),
        tool("Edit", "two", "approval-requested", { path: "src/a.ts" }),
        tool(
          "Write",
          "three",
          "output-available",
          { path: "b.ts" },
          { error: "permission denied" },
        ),
        tool("Write", "four", "output-denied", { path: "c.ts" }),
        tool(
          "MultiEdit",
          "five",
          "output-available",
          { path: "plan.ts" },
          { queued_for_plan_review: true },
        ),
      ]),
    ]);
    expect(state.files).toEqual([
      { path: "src\\a.ts", state: "applied" },
      { path: "b.ts", state: "failed" },
    ]);
  });

  it("does not mistake a background launch result for a finished process", () => {
    const launch = tool(
      "bash_background",
      "launch",
      "output-available",
      { command: "pnpm dev" },
      { handle: 7, ok: true },
    );
    const start = message("a1", "assistant", [launch]);
    expect(buildConversationStatus([start]).terminals[0].state).toBe("running");
    const exit = message("a2", "assistant", [
      tool(
        "bash_logs",
        "logs",
        "output-available",
        { handle: 7 },
        { bytes: "failed", exited: true, exit_code: 1 },
      ),
    ]);
    expect(buildConversationStatus([start, exit]).terminals[0].state).toBe(
      "failed",
    );
    // 从同一数据再次推导不受上次推导过程的内部更新影响。
    expect(buildConversationStatus([start]).terminals[0].state).toBe("running");
  });

  it("tracks failed commands, denied commands and completed subagents independently", () => {
    const state = buildConversationStatus([
      message("a1", "assistant", [
        tool(
          "PowerShell",
          "fail",
          "output-available",
          { command: "pnpm test" },
          { exit_code: 1 },
        ),
        tool("Bash", "deny", "output-denied", { command: "delete" }),
        tool(
          "run_subagent",
          "agent",
          "output-available",
          { description: "Read source" },
          { summary: "found", durationMs: 1500 },
        ),
      ]),
    ]);
    expect(state.terminals.map((entry) => entry.state)).toEqual([
      "failed",
      "cancelled",
    ]);
    expect(state.agents[0]).toMatchObject({
      title: "Read source",
      state: "completed",
      durationMs: 1500,
    });
  });

  it("keeps explicit cancellation distinct from ordinary errors with similar text", () => {
    const cancelled = {
      ...tool("Bash", "cancel", "output-error", { command: "sleep" }),
      errorText: "操作已取消",
      resultProviderMetadata: { rcode: { cancelled: true } },
    };
    const failed = {
      ...cancelled,
      toolCallId: "fail",
      resultProviderMetadata: undefined,
    };
    const messages = [message("a1", "assistant", [failed])];
    const select = createConversationStatusSelector();
    expect(select(messages).terminals[0].state).toBe("failed");
    expect(
      select([message("a1", "assistant", [cancelled])]).terminals[0].state,
    ).toBe("cancelled");
  });

  it("keeps the shared Git snapshot scoped to the actual conversation workspace", () => {
    const status = {
      repoRoot: "D:/Projects/App",
      changedFiles: [],
    } as unknown as GitStatusSnapshot;
    expect(workspaceGitStatus("d:\\projects\\app\\src", status)).toBe(status);
    expect(workspaceGitStatus("D:/Projects/AppOther", status)).toBeNull();
    expect(workspaceGitStatus("D:/Projects", status)).toBeNull();
    expect(workspaceGitStatus(null, status)).toBeNull();
    const unc = { ...status, repoRoot: "//Server/Share/App" };
    expect(workspaceGitStatus("\\\\server\\share\\app\\src", unc)).toBe(unc);
    const unix = { ...status, repoRoot: "/projects/App" };
    expect(workspaceGitStatus("/projects/app", unix)).toBeNull();
  });

  it("rejects executable attachment URLs", () => {
    expect(attachmentPreviewUrl("javascript:alert(1)")).toBeUndefined();
    expect(attachmentPreviewUrl("data:text/html,<script>")).toBeUndefined();
    expect(
      attachmentPreviewUrl("data:image/svg+xml;base64,PHN2Zy8+"),
    ).toBeUndefined();
    expect(attachmentPreviewUrl("data:image/png;base64,test")).toBe(
      "data:image/png;base64,test",
    );
  });
});
