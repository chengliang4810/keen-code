import React from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { StructuredToolResultView } from "./StructuredToolResultView";

describe("StructuredToolResultView", () => {
  it("按类型展示 Diff、文件、命令和落盘产物", () => {
    const html = renderToString(
      React.createElement(StructuredToolResultView, {
        locale: "zh",
        toolName: "workspace_edit",
        result: {
          output: "完成",
          truncated: true,
          original_bytes: 8192,
          items: [
            {
              type: "diff",
              path: "src/App.tsx",
              old_path: "src/OldApp.tsx",
              patch: "@@ -1 +1 @@\n-old\n+new",
            },
            {
              type: "file",
              path: "src/App.tsx",
              operation: "modified",
              size_bytes: 2048,
              sha256: "abc",
            },
            {
              type: "command",
              command: "pnpm test",
              exit_code: 0,
              stdout: "3 tests passed",
              stderr: "",
              duration_ms: 1250,
            },
            {
              type: "artifact",
              artifact: {
                id: "artifact-1",
                path: "/tmp/result.log",
                media_type: "text/plain",
                size_bytes: 4096,
              },
            },
          ],
          extensions: [
            {
              namespace: "keencode.tool_metadata.v1",
              payload: { safe: true },
            },
          ],
        },
      }),
    );

    const textHtml = html.replaceAll("<!-- -->", "");
    expect(textHtml).toContain("结构化结果");
    expect(textHtml).toContain("输出已截断");
    expect(textHtml).toContain("8.0 KiB");
    expect(textHtml).toContain("src/OldApp.tsx → src/App.tsx");
    expect(textHtml).toContain("modified");
    expect(textHtml).toContain("pnpm test");
    expect(textHtml).toContain("退出码: 0");
    expect(textHtml).toContain("1.25 s");
    expect(textHtml).toContain("/tmp/result.log");
    expect(textHtml).toContain("keencode.tool_metadata.v1");
  });

  it("没有类型化条目时展示纯文本结果", () => {
    const html = renderToString(
      React.createElement(StructuredToolResultView, {
        locale: "en",
        result: {
          output: "fallback output",
          is_error: true,
        },
      }),
    );

    expect(html).toContain("Structured result");
    expect(html).toContain("fallback output");
    expect(html).toContain("structured-result is-error");
  });
});
