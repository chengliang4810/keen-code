import { describe, expect, it } from "vitest";
import {
  classifyToolKind,
  isContextToolKind,
  isGoalToolName,
  isPlanToolName,
  summarizeToolDisplay,
  toolDetailTail,
} from "./toolDisplay";

describe("toolDisplay", () => {
  it("classifies bash / read / edit / search", () => {
    expect(classifyToolKind("Bash")).toBe("bash");
    expect(classifyToolKind("Read")).toBe("read");
    expect(classifyToolKind("Edit")).toBe("edit");
    expect(classifyToolKind("grep")).toBe("search");
    expect(isContextToolKind("Read")).toBe(true);
    expect(isContextToolKind("Edit")).toBe(false);
  });

  it("summarizes path basename", () => {
    const d = summarizeToolDisplay({
      kind: "Read",
      path: "/Users/me/proj/src/lib/session.ts",
    });
    expect(d.summary).toBe("session.ts");
    expect(d.isContext).toBe(true);
  });

  it("识别由输入框专用状态界面承载的 Plan 与 Goal 工具", () => {
    expect(isPlanToolName("TodoWrite")).toBe(true);
    expect(isPlanToolName("tool", "Update Plan")).toBe(true);
    expect(isGoalToolName("goal")).toBe(true);
    expect(isGoalToolName("tool", "create_goal")).toBe(true);
    expect(isGoalToolName("Bash", "检查 goal 状态")).toBe(false);
  });

  it("toolDetailTail keeps last N lines", () => {
    const detail = Array.from({ length: 12 }, (_, i) => `line${i}`).join("\n");
    const tail = toolDetailTail(detail, 3);
    expect(tail).toBe("line9\nline10\nline11");
  });
});
