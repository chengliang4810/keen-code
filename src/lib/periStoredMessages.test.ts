import { describe, expect, it } from "vitest";
import { projectPeriStoredMessages } from "./periStoredMessages";

describe("projectPeriStoredMessages", () => {
  it("保留思考、工具和正文的真实顺序并回填工具结果", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "user-1",
        role: "user",
        content: "检查项目",
      },
      {
        id: "assistant-1",
        role: "assistant",
        content: [
          { type: "reasoning", text: "先读取" },
          {
            type: "tool_use",
            id: "tool-1",
            name: "Read",
            input: { file_path: "README.md" },
          },
          { type: "text", text: "已完成" },
        ],
      },
      {
        id: "result-1",
        role: "tool",
        content: "README 内容",
        tool_call_id: "tool-1",
        is_error: false,
      },
    ]);

    expect(messages).toHaveLength(2);
    expect(messages[1]?.segments).toEqual([
      { kind: "thought", text: "先读取" },
      {
        kind: "tool",
        toolCallId: "tool-1",
        title: "Read",
        toolKind: "Read",
        status: "completed",
        streaming: false,
        input: '{"file_path":"README.md"}',
        output: "README 内容",
        detail: "README 内容",
      },
      { kind: "content", text: "已完成" },
    ]);
  });

  it("保留没有匹配调用的失败工具结果", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "result-1",
        role: "tool",
        content: "permission denied",
        tool_call_id: "tool-1",
        is_error: true,
      },
    ]);

    expect(messages[0]).toMatchObject({
      role: "tool",
      marker: "tool_step",
      toolCallId: "tool-1",
      toolStatus: "failed",
      isError: true,
    });
  });
});
