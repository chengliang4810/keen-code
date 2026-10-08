import { describe, expect, it } from "vitest";
import {
  conversationTool,
  type ConversationTool,
} from "@/modules/ai/lib/conversationPresentation";
import {
  boundedConversationToolText,
  conversationToolDiff,
  conversationToolResultText,
  conversationToolSummary,
  conversationFilePresentation,
} from "@/modules/ai/lib/conversationToolPresentation";

const tool = (input: Record<string, unknown>): ConversationTool => ({
  name: "multi_edit",
  id: "edit-1",
  state: "output-available",
  input,
  output: {},
  result: undefined,
});

describe("真实工具结果的会话展示", () => {
  it("支持原生别名与 Windows 路径，摘要不会泄露文件正文", () => {
    const value = conversationTool({
      type: "dynamic-tool",
      toolName: "Write",
      toolCallId: "write-1",
      state: "output-available",
      input: { file_path: "D:\\work\\example.ts", content: "private body" },
      output: "created",
    });
    expect(value).not.toBeNull();
    if (!value) throw new Error("缺少工具投影");
    expect(conversationToolSummary(value)).toBe("D:\\work\\example.ts");
  });
  it("多处编辑保留每个 old/new 的顺序，不把失败结果作为文件内容", () => {
    const value = tool({
      edits: [
        { old_string: "a\nb\n", new_string: "c\n" },
        null,
        { old_string: "c", new_string: "d" },
      ],
    });
    expect(
      conversationToolDiff(value).lines.map(({ kind, text }) => ({
        kind,
        text,
      })),
    ).toEqual([
      { kind: "removed", text: "a" },
      { kind: "removed", text: "b" },
      { kind: "added", text: "c" },
      { kind: "removed", text: "c" },
      { kind: "added", text: "d" },
    ]);
    expect(
      conversationToolResultText({
        ...value,
        result: "ignored",
        errorText: "拒绝访问",
      }),
    ).toBe("拒绝访问");
  });
  it("文件路径按会话根显示，避免把相似目录误当作当前工作区", () => {
    expect(
      conversationFilePresentation("D:\\work\\src\\test.ts", "d:/work"),
    ).toEqual({ name: "test.ts", parent: "src" });
    expect(
      conversationFilePresentation(
        "//?/C:/workspace/test.json",
        "C:/workspace",
      ),
    ).toEqual({ name: "test.json", parent: "" });
    expect(
      conversationFilePresentation("/work-other/test.ts", "/work").parent,
    ).toBe("/work-other");
  });
  it("超大变更预览有行数和字符预算，并保留真实提交行数", () => {
    const preview = conversationToolDiff({
      ...tool({ content: "a\n".repeat(500) }),
      name: "write_file",
    });
    expect(preview.lines).toHaveLength(400);
    expect(preview.added).toBe(500);
    expect(preview.truncated).toBe(true);
    const long = conversationToolDiff({
      ...tool({ content: "x".repeat(100_000) }),
      name: "write_file",
    });
    expect(long.lines[0]?.text.length).toBe(64 * 1024);
    expect(long.truncated).toBe(true);
  });
  it("兼容原生 Shell 文本和 SDK stdout/stderr，并对大结果限制展示大小", () => {
    const value = tool({});
    expect(
      conversationToolResultText({
        ...value,
        result: "stdout（8 字节）：\nverified",
      }),
    ).toContain("verified");
    expect(
      conversationToolResultText({
        ...value,
        output: { stdout: "out", stderr: "err" },
        result: {},
      }),
    ).toBe("out\nerr");
    const preview = boundedConversationToolText("x".repeat(100_000));
    expect(preview.text.length).toBe(64 * 1024);
    expect(preview.truncated).toBe(true);
  });
});
