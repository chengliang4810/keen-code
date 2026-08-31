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
  it("开始界面不展示当前模型分隔线", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).not.toContain("lobe-chat-model-divider");
    expect(html).not.toContain("正在使用 GLM-5.3");
  });

  it("仅在首轮和模型切换时展示模型分隔线", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          { id: "user-1", role: "user", content: "检查项目", model: "GLM-5.3" },
          { id: "assistant-1", role: "assistant", content: "完成" },
          { id: "user-2", role: "user", content: "继续", model: "GLM-5.3" },
          { id: "assistant-2", role: "assistant", content: "继续完成" },
          { id: "user-3", role: "user", content: "复查", model: "gpt-5.6-luna" },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain("lobe-chat-model-divider");
    expect(html).toContain("正在使用 GLM-5.3");
    expect(html.indexOf("正在使用 GLM-5.3")).toBeLessThan(
      html.indexOf("检查项目"),
    );
    expect(html.match(/lobe-chat-model-divider/g)).toHaveLength(2);
    expect(html).toContain("正在使用 gpt-5.6-luna");
    expect(html.indexOf("正在使用 gpt-5.6-luna")).toBeLessThan(
      html.indexOf("复查"),
    );
  });

  it("在当前回合展示下一次请求尝试并在恢复后隐藏", () => {
    const retrying = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          { id: "user-retry", role: "user", content: "继续执行" },
          {
            id: "assistant-retry",
            role: "assistant",
            content: "",
            streaming: true,
          },
        ]}
        sessionState="streaming"
        retryStatus={{
          attempt: 5,
          maxAttempts: 10,
          delayMs: 800,
          reason: "服务商暂时不可用",
        }}
        attachLabels={attachLabels}
      />,
    );

    expect(retrying).toContain("正在进行第 6/10 次请求尝试 · 0.8s");
    expect(retrying).toContain('data-testid="chat-retry-status"');
    expect(retrying).toContain("服务商暂时不可用");

    const recovered = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          { id: "user-retry", role: "user", content: "继续执行" },
          {
            id: "assistant-retry",
            role: "assistant",
            content: "已恢复输出",
            streaming: false,
          },
        ]}
        sessionState="ready"
        retryStatus={{
          attempt: 5,
          maxAttempts: 10,
          delayMs: 800,
          reason: "服务商暂时不可用",
        }}
        attachLabels={attachLabels}
      />,
    );

    expect(recovered).toContain('data-testid="chat-retry-status"');
    expect(recovered).not.toContain("正在进行第 6/10 次请求尝试");
  });

  it("恢复 Markdown 无序列表和有序列表的可见标记", () => {
    const chatCss = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );

    expect(chatCss).toMatch(/\.chat-md ul\s*\{[^}]*list-style:\s*disc\s*;/s);
    expect(chatCss).toMatch(
      /\.chat-md ol\s*\{[^}]*list-style:\s*decimal\s*;/s,
    );
    expect(chatCss).toMatch(/--chat-prose-fs:\s*15px;/);
    expect(chatCss).toMatch(
      /\[data-theme="light"\] \.lobe-chat\s*\{[\s\S]*?--chat-prose-text:\s*color-mix\(/,
    );
    expect(chatCss).toMatch(
      /\.chat-md li::marker\s*\{[^}]*color:\s*var\(--chat-prose-text\);/s,
    );
  });

  it("将助手消息操作区放在左下角", () => {
    const chatCss = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );

    expect(chatCss).toMatch(
      /\.lobe-chat-item--assistant \.lobe-chat-item__actions\s*\{[^}]*justify-content:\s*flex-start\s*;/s,
    );
  });

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

    expect(html).toContain("工作中 1秒");
  });

  it("工具结束后回合仍忙碌时把工作时间留在 Assistant 回复顶部", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          { id: "user-active", role: "user", content: "继续执行" },
          {
            id: "assistant-active",
            role: "assistant",
            content: "正在验证接口。",
            streaming: false,
          },
        ]}
        sessionState="streaming"
        turnStartedAt={Date.now()}
        attachLabels={attachLabels}
      />,
    );

    expect(html.match(/工作中 1秒/g)).toHaveLength(1);
    expect(html.indexOf("工作中 1秒")).toBeLessThan(
      html.indexOf("正在验证接口。"),
    );
    expect(html).not.toContain("lobe-chat-live-tool is-running");
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

    expect(html).toContain("已工作 1秒");
    expect(html).not.toContain("本轮模型未返回思考内容");
    expect(html).toContain("你好，我是 KeenCode。");
  });

  it("失败 Turn 重放后展示无图标耗时和分割线", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-failed-turn",
            role: "assistant",
            content: "",
            thinkingDurationMs: 101_000,
            turnStatus: "failed",
            turnIncomplete: true,
            streaming: false,
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );
    const css = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );

    expect(html).toContain("已工作 1分钟 41秒");
    expect(html).not.toContain("lobe-chat-thinking__status-icon");
    expect(html).not.toContain("lobe-chat-thinking__status-chevron");
    expect(css).toMatch(
      /\.lobe-chat-thinking__trigger--status\s*\{[^}]*border-bottom:\s*1px solid var\(--chat-divider\);/s,
    );
  });

  it("已完成回复忽略正文后仅含标点的尾随 reasoning", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-completed-thought",
            role: "assistant",
            content: "已完成。",
            thought: "先分析请求\n.",
            segments: [
              { kind: "thought", text: "先分析请求" },
              { kind: "content", text: "已完成。" },
              { kind: "thought", text: "." },
            ],
            streaming: false,
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain("先分析请求");
    expect(html).not.toContain("思考中…");
    expect(html).toContain("思考过程");
  });

  it("始终展示一轮中的全部思考片段", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-all-thoughts",
            role: "assistant",
            content: "阶段结果最终结果",
            thought: "第一段分析第二段分析",
            segments: [
              { kind: "thought", text: "第一段分析" },
              { kind: "content", text: "阶段结果" },
              { kind: "thought", text: "第二段分析" },
              { kind: "content", text: "最终结果" },
            ],
            streaming: false,
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain("第一段分析");
    expect(html).toContain("第二段分析");
  });

  it("完成后只把本轮延迟放入现有 hover footer", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-metrics",
            role: "assistant",
            content: "修复已经完成。",
            turnMetrics: {
              turnId: "turn-1",
              sendAcknowledgementMs: 16,
              timeToFirstSseMs: 540,
              timeToFirstVisibleTokenMs: 610,
              totalMs: 8_300,
              inputTokens: 4_000,
              reasoningTokens: 300,
              cacheReadTokens: 3_000,
              cacheCreationTokens: 0,
            },
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain('class="lobe-chat-item__actions"');
    expect(html).toContain('data-testid="turn-metrics"');
    expect(html).not.toContain("发送确认 16ms");
    expect(html).not.toContain("首 SSE 540ms");
    expect(html).toContain("首 Token 610ms");
    expect(html).toContain("完成 8.3s");
    expect(html).not.toContain("缓存命中");
    expect(html.indexOf('aria-label="复制"')).toBeLessThan(
      html.indexOf('data-testid="turn-metrics"'),
    );

    const chatCss = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );
    expect(chatCss).toMatch(
      /\.lobe-turn-metrics\s*\{[^}]*line-height:\s*28px;[^}]*text-overflow:\s*ellipsis;/s,
    );
  });

  it("流式期间不展示尚未固化的 footer 指标", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-streaming-metrics",
            role: "assistant",
            content: "正在处理",
            streaming: true,
            turnMetrics: {
              turnId: "turn-1",
              sendAcknowledgementMs: 12,
              timeToFirstSseMs: 400,
              timeToFirstVisibleTokenMs: 450,
              totalMs: null,
              inputTokens: null,
              reasoningTokens: null,
              cacheReadTokens: null,
              cacheCreationTokens: null,
            },
          },
        ]}
        sessionState="streaming"
        attachLabels={attachLabels}
      />,
    );

    expect(html).not.toContain('data-testid="turn-metrics"');
    expect(html).not.toContain("发送确认 12ms");
  });

  it("已过滤的供应商错误不被渲染层再次替换为通用文案", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "provider-error",
            role: "assistant",
            content:
              'Model "grok-4.6" is not supported by any configured account in this group',
            isError: true,
            errorBodyFormatted: true,
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain(
      "Model &quot;grok-4.6&quot; is not supported by any configured account in this group",
    );
    expect(html).not.toContain("模型服务当前不可用");
  });

  it("把 Peri 系统通知渲染为安静的时间线状态行", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "system-notification-1",
            role: "tool",
            content: "MCP: docs connected (12 tools)",
            marker: "system_notification",
            systemNotificationLevel: "warning",
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain('data-message-marker="system_notification"');
    expect(html).toContain('data-level="warning"');
    expect(html).toContain("MCP 服务器 docs 已连接，可用工具 12 个。");
    expect(html).not.toContain("connected (12 tools)");
  });

  it("二次打开时在回复顶部只展示一次总工作时间", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-with-multiple-thoughts",
            role: "assistant",
            content: "先检查实现。修复已经完成。",
            thinkingDurationMs: 485_000,
            streaming: false,
            segments: [
              { kind: "thought", text: "检查处理时间的渲染来源" },
              { kind: "content", text: "先检查实现。" },
              { kind: "thought", text: "验证多段思考的展示结果" },
              { kind: "content", text: "修复已经完成。" },
            ],
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html.match(/已工作 8分钟 5秒/g)).toHaveLength(1);
    expect(html.indexOf("已工作 8分钟 5秒")).toBeLessThan(
      html.indexOf("检查处理时间的渲染来源"),
    );
    expect(html.indexOf("已工作 8分钟 5秒")).toBeLessThan(
      html.indexOf("先检查实现。"),
    );
    expect(html).toContain("检查处理时间的渲染来源");
    expect(html).toContain("验证多段思考的展示结果");
  });

  it("二次打开 Agent 回合时把工作时间锚定到首条 Assistant 记录顶部", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          { id: "user-1", role: "user", content: "制定计划" },
          {
            id: "assistant-before-agent",
            role: "assistant",
            content: "我会先调用 plan 智能体。",
          },
          {
            id: "assistant-final",
            role: "assistant",
            content: "计划已经完成。",
            thinkingDurationMs: 227_000,
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html.match(/已工作 3分钟 47秒/g)).toHaveLength(1);
    expect(html.indexOf("已工作 3分钟 47秒")).toBeLessThan(
      html.indexOf("我会先调用 plan 智能体。"),
    );
    expect(html.indexOf("已工作 3分钟 47秒")).toBeLessThan(
      html.indexOf("计划已经完成。"),
    );
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

  it("仅在空闲状态为最后一条用户消息提供编辑重发入口", () => {
    const render = (sessionState: "ready" | "streaming") =>
      renderToString(
        <ConversationThread
          locale="zh"
          messages={[
            { id: "user-1", role: "user", content: "第一条" },
            { id: "assistant-1", role: "assistant", content: "回复" },
            { id: "user-2", role: "user", content: "最后一条" },
          ]}
          sessionState={sessionState}
          attachLabels={attachLabels}
          onEditLastUserMessage={async () => true}
        />,
      );

    expect(render("ready").match(/aria-label="编辑并重新发送"/g)).toHaveLength(1);
    expect(render("streaming")).not.toContain('aria-label="编辑并重新发送"');
  });

  it("内联编辑器复用 Textarea 和 Button，并保留键盘提交与取消", () => {
    const source = readFileSync(
      new URL("./ConversationThread.tsx", import.meta.url),
      "utf8",
    );
    const css = readFileSync(new URL("./lobe-chat.css", import.meta.url), "utf8");

    expect(source).toContain("<Textarea");
    expect(source).toContain('event.key === "Escape"');
    expect(source).toContain("event.metaKey || event.ctrlKey");
    expect(source).toContain('className="btn btn--solid"');
    expect(css).toMatch(/\.lobe-chat-user-editor\s*\{[^}]*border-radius:\s*24px;/s);
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

  it("不让独立 TodoWrite 空行触发虚拟列表并累积白色间隙", () => {
    const hiddenTodoSteps = Array.from({ length: 60 }, (_, index) => ({
      id: `tool-todo-${index}`,
      role: "tool" as const,
      content: "tool_step|completed|TodoWrite",
      marker: "tool_step" as const,
      toolCallId: `todo-${index}`,
      toolKind: "TodoWrite",
      toolStatus: "completed",
      toolDetail: '{"todos":[]}',
    }));
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          ...hiddenTodoSteps,
          { id: "assistant-1", role: "assistant", content: "任务仍在运行" },
        ]}
        sessionState="streaming"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain("任务仍在运行");
    expect(html).not.toContain("data-virtual-message-index");
    expect(html).not.toContain("TodoWrite");
  });

  it("用户消息图片卡片在资源协议失败时回退二进制预览", () => {
    const source = readFileSync(
      new URL("../AttachmentCard.tsx", import.meta.url),
      "utf8",
    );
    const cardImage =
      source.match(/<img\s+className="att-card__thumb"[\s\S]*?\/>/)?.[0] ?? "";

    expect(cardImage).toContain("onError={() => void recoverThumbnail()}");
    expect(source).toContain("await resolveImageSrc(attachment.path)");
  });
});
