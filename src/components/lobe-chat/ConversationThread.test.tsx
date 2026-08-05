import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ConversationThread } from "./ConversationThread";

/** 测试用附件操作文案，满足 ConversationThread 的完整输入契约。 */
const attachLabels = {
  open: "打开",
  reveal: "显示",
  copyPath: "复制路径",
  copyImage: "复制图片",
  addToComposer: "添加到输入框",
  remove: "移除",
};

describe("ConversationThread 思考耗时", () => {
  it("首次发送后在模型返回内容前立即展示处理耗时", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          { id: "user-1", role: "user", content: "帮我修复登录页" },
          {
            id: "assistant-pending",
            role: "assistant",
            content: "",
            streaming: true,
          },
        ]}
        sessionState="streaming"
        turnStartedAt={Date.now()}
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain("已处理 1秒");
    expect(html).not.toContain("思考中");
  });

  it("模型未返回思考内容时只展示耗时", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-1",
            role: "assistant",
            content: "你好，我是 KeenCode。",
            thinkingDurationMs: 0,
            streaming: false,
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain("已处理 1秒");
    expect(html).not.toContain("本轮模型未返回思考内容");
    expect(html).toContain("你好，我是 KeenCode。");
  });

  it("用户消息正文使用行内容器，避免整条复制产生块级尾随换行", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "user-1",
            role: "user",
            content: "以及本地/远程模型连接能力",
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain(
      '<div class="lobe-chat-bubble"><span class="user-msg-body">以及本地/远程模型连接能力</span></div>',
    );
  });

  it("用户消息复制逻辑同时接入文档事件和正文选择边界", () => {
    const threadSource = readFileSync(
      new URL("./ConversationThread.tsx", import.meta.url),
      "utf8",
    );
    const chatCss = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );
    const rowRule = chatCss.match(
      /\.lobe-chat \.lobe-chat-item--user,\s*\.lobe-chat \.lobe-chat-item--user \*\s*\{([^}]*)\}/,
    )?.[1];
    const bodyRule = chatCss.match(
      /\.lobe-chat \.lobe-chat-item--user \.user-msg-body,\s*\.lobe-chat \.lobe-chat-item--user \.user-msg-body \*\s*\{([^}]*)\}/,
    )?.[1];

    expect(threadSource).toContain("writeUserMessageSelectionToClipboard(");
    expect(threadSource).toContain(
      'ownerDocument.addEventListener("copy", onCopy, true)',
    );
    expect(threadSource).toContain(
      'ownerDocument.removeEventListener("copy", onCopy, true)',
    );
    expect(threadSource).toContain(
      '<div ref={chatRootRef} className="lobe-chat"',
    );
    expect(rowRule).toMatch(/-webkit-user-select:\s*none\s*;/);
    expect(rowRule).toMatch(/(?:^|\n)\s*user-select:\s*none\s*;/);
    expect(bodyRule).toMatch(/-webkit-user-select:\s*text\s*;/);
    expect(bodyRule).toMatch(/(?:^|\n)\s*user-select:\s*text\s*;/);
  });

  it("隐藏对话中的 TodoWrite 工具调用但保留后续正文", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-plan",
            role: "assistant",
            content: "继续执行",
            segments: [
              {
                kind: "tool",
                toolCallId: "todo-1",
                title: "TodoWrite",
                toolKind: "TodoWrite",
                status: "completed",
                input: '{"todos":[{"content":"检查文件"}]}',
              },
              { kind: "content", text: "继续执行" },
            ],
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain("继续执行");
    expect(html).not.toContain("TodoWrite");
    expect(html).not.toContain("检查文件");
  });
});
