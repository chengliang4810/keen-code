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

  it("把外层 tool_calls（chatcmpl 存储）投影为工具段并回填结果", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "user-1",
        role: "user",
        content: "生成过山车页面",
      },
      {
        id: "assistant-1",
        role: "assistant",
        content: [
          { type: "reasoning", text: "先看现有文件" },
          { type: "text", text: "开始读取" },
        ],
        tool_calls: [
          {
            id: "chatcmpl-tool-9b268c64",
            name: "Read",
            arguments: { file_path: "index.html" },
          },
        ],
      },
      {
        id: "result-1",
        role: "tool",
        content: "total 72\ndrwxr-xr-x",
        tool_call_id: "chatcmpl-tool-9b268c64",
        is_error: false,
      },
    ]);

    expect(messages).toHaveLength(2);
    const assistant = messages[1]!;
    expect(assistant.role).toBe("assistant");
    const tool = assistant.segments?.find((segment) => segment.kind === "tool");
    expect(tool).toMatchObject({
      kind: "tool",
      toolCallId: "chatcmpl-tool-9b268c64",
      title: "Read",
      toolKind: "Read",
      status: "completed",
      input: '{"file_path":"index.html"}',
      output: "total 72\ndrwxr-xr-x",
    });
    // 正文与思考不受外层 tool_calls 影响。
    expect(assistant.content).toBe("开始读取");
    expect(assistant.thought).toBe("先看现有文件");
  });

  it("非法 tool_calls 项跳过且与 tool_use 块去重", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "assistant-1",
        role: "assistant",
        content: [
          {
            type: "tool_use",
            id: "tool-1",
            name: "Bash",
            input: { command: "ls" },
          },
        ],
        tool_calls: [
          { id: "tool-1", name: "Bash", arguments: { command: "ls" } },
          { name: "缺 id 的非法项" },
          { id: "", name: "空 id" },
        ],
      },
    ]);

    const tools = messages[0]!.segments?.filter(
      (segment) => segment.kind === "tool",
    );
    expect(tools).toHaveLength(1);
    expect(tools?.[0]).toMatchObject({ toolCallId: "tool-1", title: "Bash" });
  });
});
