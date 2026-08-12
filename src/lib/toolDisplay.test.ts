import { describe, expect, it } from "vitest";
import {
  classifyToolKind,
  isContextToolKind,
  isGoalToolName,
  isPlanToolName,
  summarizeToolDisplay,
  summarizeCompletedTools,
  summarizeRunningTool,
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

  it("按最后一个运行工具生成包含目标的中文描述", () => {
    expect(
      summarizeRunningTool(
        { kind: "Read", input: '{"file_path":"src/App.tsx"}' },
        "zh",
      ),
    ).toBe("正在读取 App.tsx");
    expect(
      summarizeRunningTool(
        {
          kind: "Bash",
          input:
            '{"command":"rg -n \\\"terminal_create|terminalCreate|terminals\\\" src"}',
        },
        "zh",
      ),
    ).toBe('正在运行 rg -n "terminal_create|terminalCreate|terminals" src');
  });

  it("按首次出现顺序汇总历史工具类型", () => {
    expect(
      summarizeCompletedTools(
        [
          { kind: "Edit" },
          { kind: "Write" },
          { kind: "Bash" },
        ],
        "zh",
      ),
    ).toBe("编辑了文件、运行了命令");
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
