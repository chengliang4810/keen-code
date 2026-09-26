import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { readSource } from "../../test-utils/readCssSource";
import type { ChatMessage } from "@/lib/session";
import {
  buildComposerMentionMarkdown,
  encodeComposerMention,
} from "@/lib/composerMentions";
import { ConversationThread, formatRetryCountdown, goalIterationDividerIndices } from "./ConversationThread";

/** 测试用附件操作文案，满足 ConversationThread 的完整输入契约。 */
const attachLabels = {
  open: "打开",
  reveal: "显示",
  copyPath: "复制路径",
  copyImage: "复制图片",
  addToComposer: "添加到输入框",
  remove: "移除",
};

describe("Goal 多轮时间线", () => {
  const goalId = "goal-1";
  const messages: ChatMessage[] = [
    { id: "prior", role: "assistant", content: "旧任务", turnId: "turn-prior" },
    { id: "user", role: "user", content: "开始目标", turnId: "turn-start" },
    { id: "first", role: "assistant", content: "第一轮结束", turnId: "turn-start",
      segments: [
        { kind: "tool", toolCallId: "goal-call", title: "Goal", toolKind: "Goal", status: "completed" },
        { kind: "content", text: "第一轮结束" },
      ] },
    { id: "second", role: "assistant", content: "第二轮结束",
      turnId: "turn-goal-goal-1-1-1000-1" },
    { id: "third", role: "assistant", content: "第三轮结束",
      turnId: "turn-goal-goal-1-2-1001-2" },
  ];

  it("首轮与每个自动续跑轮次各有一条分割线，不标记无关消息", () => {
    expect([...goalIterationDividerIndices(messages, goalId)]).toEqual([
      [3, 2], [4, 3], [1, 1],
    ]);
    const html = renderToString(<ConversationThread locale="zh" messages={messages}
      sessionState="ready" attachLabels={attachLabels} />);
    expect(html.match(/class="lobe-chat-goal-divider"/g)).toHaveLength(3);
    expect(html.indexOf("第 1 次迭代")).toBeLessThan(html.indexOf("开始目标"));
    expect(html.indexOf("第 2 次迭代")).toBeLessThan(html.indexOf("第二轮结束"));
    expect(html.indexOf("第 3 次迭代")).toBeLessThan(html.indexOf("第三轮结束"));
  });

  it("未建立 Goal 或无可信创建轮次时不伪造首轮标记", () => {
    expect(goalIterationDividerIndices(messages.slice(0, 1), goalId).size).toBe(0);
    expect([...goalIterationDividerIndices(messages.slice(3), goalId)]).toEqual([[0, 2], [1, 3]]);
  });

  it("清除 Goal 状态后仍从历史 Turn 保留迭代分割线", () => {
    const html = renderToString(<ConversationThread locale="zh" messages={messages}
      sessionState="ready" attachLabels={attachLabels} />);
    expect(html.match(/class="lobe-chat-goal-divider"/g)).toHaveLength(3);
    expect(html.indexOf("第 1 次迭代")).toBeLessThan(html.indexOf("开始目标"));
  });

  it("仅有首轮时从成功的 Goal create 标记第一轮，update 不产生新标记", () => {
    const singleTurn: ChatMessage[] = [
      { id: "user", role: "user", content: "开始目标", turnId: "turn-start" },
      { id: "assistant", role: "assistant", content: "已完成", turnId: "turn-start",
        segments: [
          { kind: "tool", toolCallId: "create", title: "Goal", toolKind: "Goal",
            status: "completed", input: '{"action":"create"}' },
          { kind: "tool", toolCallId: "update", title: "Goal", toolKind: "Goal",
            status: "completed", input: '{"action":"update"}' },
        ] },
    ];
    expect([...goalIterationDividerIndices(singleTurn)]).toEqual([[0, 1]]);
    expect(goalIterationDividerIndices(singleTurn.slice(1)).get(0)).toBe(1);
    expect(goalIterationDividerIndices([{ ...singleTurn[1]!, segments: [
      { kind: "tool", toolCallId: "update", title: "Goal", toolKind: "Goal",
        status: "completed", input: '{"action":"update"}' },
    ] }]).size).toBe(0);
  });
});

describe("ConversationThread 思考耗时", () => {
  it("流式空 reasoning segment 不再额外渲染思考状态行", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "assistant-empty-thought",
            role: "assistant",
            content: "",
            streaming: true,
            segments: [{ kind: "thought", text: "   " }],
          },
        ]}
        sessionState="streaming"
        attachLabels={attachLabels}
      />,
    );

    expect(html.match(/data-variant="processing"/g) ?? []).toHaveLength(1);
    expect(html).not.toContain('data-variant="think"');
  });

  it("关闭显示思考过程后，已结束的思考块不再渲染，思考中仍实时显示", () => {
    const settled: ChatMessage[] = [{ id: "a", role: "assistant", content: "完成", segments: [
      { kind: "thought", text: "内部检查推理" },
      { kind: "content", text: "完成" },
    ] }];
    const off = renderToString(
      <ConversationThread
        locale="zh"
        messages={settled.slice()}
        sessionState="ready"
        attachLabels={attachLabels}
        showThinkingProcess={false}
      />,
    );
    expect(off).not.toContain("内部检查推理");
    expect(off).toContain("完成");

    const on = renderToString(
      <ConversationThread
        locale="zh"
        messages={settled.slice()}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );
    expect(on).toContain('data-variant="think"');
    expect(on).not.toContain("内部检查推理");

    const live = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{ id: "b", role: "assistant", content: "", streaming: true,
          segments: [{ kind: "thought", text: "正在推理" }] }]}
        sessionState="streaming"
        attachLabels={attachLabels}
        showThinkingProcess={false}
      />,
    );
    expect(live).toContain("正在推理");
  });
  it("关闭思考过程后，被隐藏思考分隔的相邻工具仍聚合成一个栏目", () => {
    const messages: ChatMessage[] = [{
      id: "assistant-grouped", role: "assistant", content: "完成",
      segments: [
        { kind: "thought", text: "第一段推理" },
        { kind: "tool", toolCallId: "t1", title: "Read a", toolKind: "Read", status: "completed" },
        { kind: "thought", text: "第二段推理" },
        { kind: "tool", toolCallId: "t2", title: "Read b", toolKind: "Read", status: "completed" },
        { kind: "content", text: "完成" },
      ],
    }];
    const off = renderToString(
      <ConversationThread
        locale="zh"
        messages={messages.slice()}
        sessionState="ready"
        attachLabels={attachLabels}
        showThinkingProcess={false}
      />,
    );
    expect(off.match(/data-testid="timeline-phase"/g)).toHaveLength(1);
    expect(off).not.toContain("第一段推理");

    const on = renderToString(
      <ConversationThread
        locale="zh"
        messages={messages.slice()}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );
    expect(on).not.toContain('data-testid="timeline-phase"');
  });

  it("压缩状态以低强调工具行位于正文之间，不出现在第一句之前", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{
          id: "assistant-compaction", role: "assistant", content: "压缩前输出压缩后输出",
          thinkingDurationMs: 1_000,
          segments: [
            { kind: "content", text: "压缩前输出" },
            { kind: "compaction", meta: { trigger: "auto", tokensAfter: 100 } },
            { kind: "content", text: "压缩后输出" },
          ],
        }, {
          id: "assistant-live", role: "assistant", content: "", streaming: true, segments: [],
        }]}
        sessionState="streaming"
        attachLabels={attachLabels}
      />,
    );
    expect(html.indexOf("压缩前输出")).toBeLessThan(html.indexOf("上下文已自动压缩"));
    expect(html.indexOf("上下文已自动压缩")).toBeLessThan(html.indexOf("压缩后输出"));
    expect(html.match(/上下文已自动压缩/g)).toHaveLength(1);
    expect(html.match(/已工作/g)).toHaveLength(1);
    expect(html).toContain('class="lobe-timeline-tool__row"');
    expect(html).not.toContain('class="lobe-chat-compact"');
  });

  it("回合结束后，末尾答案之前的工作折进以回合耗时为标题的折叠组", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{
          id: "work-answer", role: "assistant", content: "过程说明。最终结论。",
          thinkingDurationMs: 911_000,
          segments: [
            { kind: "thought", text: "内部推理过程" },
            { kind: "tool", toolCallId: "t1", title: "Read a", toolKind: "Read", status: "completed" },
            { kind: "tool", toolCallId: "t2", title: "Read b", toolKind: "Read", status: "completed" },
            { kind: "content", text: "过程说明。" },
            { kind: "tool", toolCallId: "t3", title: "Read c", toolKind: "Read", status: "completed" },
            { kind: "content", text: "最终结论。" },
          ],
        }]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );
    expect(html.match(/data-testid="turn-work-group"/g)).toHaveLength(1);
    expect(html).toContain("已工作 15分钟 11秒");
    expect(html).toContain("最终结论。");
    expect(html).not.toContain("内部推理过程");
    expect(html).not.toContain("Read a");
    expect(html).not.toContain("过程说明。");
    expect(html.match(/已工作/g)).toHaveLength(1);
  });

  it("回合进行中不折叠，工作单元保持平铺展开", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        sessionState="streaming"
        attachLabels={attachLabels}
        messages={[
          {
            id: "work-answer", role: "assistant", content: "过程说明。最终结论。",
            thinkingDurationMs: 911_000,
            segments: [
              { kind: "thought", text: "内部推理过程" },
              { kind: "tool", toolCallId: "t1", title: "Read a", toolKind: "Read", status: "completed" },
              { kind: "content", text: "过程说明。" },
            ],
          },
          { id: "assistant-live", role: "assistant", content: "", streaming: true, segments: [] },
        ]}
      />,
    );
    expect(html).not.toContain('data-testid="turn-work-group"');
    expect(html).toContain('data-variant="think"');
    expect(html).not.toContain("内部推理过程");
    expect(html).toContain('data-testid="timeline-tool"');
    expect(html).toContain("过程说明。");
  });

  it("纯文本回答不产生整体折叠组", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{ id: "plain", role: "assistant", content: "直接回答", segments: [{ kind: "content", text: "直接回答" }] }]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );
    expect(html).not.toContain('data-testid="turn-work-group"');
    expect(html).not.toContain("已工作");
    expect(html).toContain("直接回答");
  });

  it("压缩开始与失败使用实时状态文案", () => {
    const render = (status: "running" | "failed") => renderToString(
      <ConversationThread locale="zh" sessionState="streaming" attachLabels={attachLabels}
        messages={[{ id: "compacting", role: "assistant", content: "", streaming: true,
          segments: [{ kind: "compaction", meta: { trigger: "auto", status } }] }]} />,
    );
    expect(render("running")).toContain("上下文压缩中");
    expect(render("running")).toContain('data-status="running"');
    expect(render("failed")).toContain("上下文压缩失败");
  });

  it("相邻的已查看图片共用工具行并默认折叠预览", () => {
    const html = renderToString(<ConversationThread locale="zh" sessionState="ready" attachLabels={attachLabels}
      messages={[{ id: "images", role: "assistant", content: "", segments: [1, 2].map((n) => ({
        kind: "tool", toolCallId: `image-${n}`, title: "Read", status: "completed",
        imageSources: [`https://example.test/${n}.png`],
      })) }]} />);
    expect(html.match(/data-testid="timeline-images"/g)).toHaveLength(1);
    expect(html).toContain("已查看 2 张图像");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("md-body__img-frame--thumbnail");
    expect(html).not.toContain('aria-label="查看大图: 图像 1"');
  });

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
    expect(html).not.toContain("模型已切换");
  });

  it("仅在模型切换时展示分隔线，并显示旧模型与新模型", () => {
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

    expect(html.match(/lobe-chat-model-divider/g)).toHaveLength(1);
    expect(html).toContain("⇄ 模型已切换 GLM-5.3 → gpt-5.6-luna");
    expect(html.indexOf("⇄ 模型已切换 GLM-5.3 → gpt-5.6-luna")).toBeLessThan(
      html.indexOf("复查"),
    );
  });

  it("将供应商限额和恢复时间显示为正文而非仅放在提示属性中", () => {
    const reason = "已达到 5 小时的使用上限。您的限额将在 2026-09-17 19:50:12 重置。";
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{ id: "quota-user", role: "user", content: "继续" }]}
        sessionState="streaming"
        retryStatus={{ attempt: 1, maxAttempts: 10, delayMs: 3000, reason }}
        attachLabels={attachLabels}
      />,
    );
    const visibleText = html.replace(/<[^>]*>/g, "");
    expect(visibleText).toContain(reason);
    const css = readSource(new URL("./lobe-chat.css", import.meta.url));
    const labelRule = css.match(/\.lobe-chat-retry-status__label\s*\{([^}]*)\}/)?.[1];
    expect(labelRule).toContain("white-space: normal");
    expect(labelRule).toContain("overflow-wrap: anywhere");
    expect(labelRule).not.toContain("overflow: hidden");
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

    // 倒计时单独标记 aria-hidden，避免实时区域每 100ms 变更被反复播报。
    expect(retrying).toContain("正在进行第 6/10 次请求尝试");
    expect(retrying).toContain("· 0.8s");
    expect(retrying).toContain('<span aria-hidden="true"> · 0.8s</span>');
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

  it("把剩余重试等待格式化为一位小数秒，到期或非法值不显示", () => {
    expect(formatRetryCountdown(3_000)).toBe("3.0s");
    expect(formatRetryCountdown(2_950)).toBe("3.0s");
    expect(formatRetryCountdown(1_240)).toBe("1.2s");
    expect(formatRetryCountdown(100)).toBe("0.1s");
    expect(formatRetryCountdown(0)).toBe("");
    expect(formatRetryCountdown(-500)).toBe("");
    expect(formatRetryCountdown(Number.NaN)).toBe("");
  });

  it("Markdown 列表对齐 ZCode 的原生 marker、缩进和换行语义", () => {
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));

    expect(chatCss).toMatch(
      /\.chat-md ul\s*\{[^}]*margin:\s*12px 0;[^}]*padding-left:\s*20px;[^}]*list-style:\s*disc outside;/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md ol\s*\{[^}]*margin:\s*12px 0;[^}]*padding-left:\s*0;[^}]*list-style:\s*decimal inside;/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md ul::marker,\s*\.chat-md ol::marker\s*\{[^}]*color:\s*var\(--foreground-subtlest,\s*var\(--text-tertiary\)\);/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md li\s*\{[^}]*padding-left:\s*4px;/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md li > p\s*\{[^}]*display:\s*inline;[^}]*margin:\s*0;/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md ul > li \+ li,\s*\.chat-md ol > li \+ li\s*\{[^}]*margin-top:\s*6px;/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md ul ul,\s*\.chat-md ul ol,\s*\.chat-md ol ul,\s*\.chat-md ol ol\s*\{[^}]*margin-block:\s*6px;/s,
    );
    expect(chatCss).not.toContain("grid-template-columns: max-content minmax(0, 1fr)");
    expect(chatCss).not.toContain("counter(chat-ordered-item)");
    expect(chatCss).not.toMatch(/\.chat-md li::before/);
    expect(chatCss).toMatch(/--chat-prose-fs:\s*var\(--text-md\);/);
    expect(chatCss).toMatch(/\.chat-md\s*\{[^}]*font-family:\s*var\(--chat-font\);/s);
    // 对话区字体完全对齐 ZCode：正文常规 400 字重，字族为 ZCode 生效的默认 sans 栈与 CJK 等宽栈。
    expect(chatCss).toMatch(/\.chat-md\s*\{[^}]*font-weight:\s*400;/s);
    expect(chatCss).toMatch(/--chat-font:\s*ui-sans-serif,\s*system-ui,\s*sans-serif,/);
    expect(chatCss).toMatch(/--chat-mono:[^;]*'Noto Sans CJK SC',\s*monospace;/s);
    expect(chatCss).not.toMatch(/\.chat-md\s*\{[^}]*font:\s*var\(--dsw-font-markdown-base\);/s);
    expect(chatCss).toMatch(
      /\[data-theme="light"\] \.lobe-chat\s*\{[\s\S]*?--chat-prose-text:\s*var\(--text-primary\);/,
    );
  });

  it("Markdown 标题字号对齐 ZCode 的 18/16/14px token，行距与字距继承正文", () => {
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));

    expect(chatCss).toMatch(
      /--chat-prose-heading-xl:\s*calc\(18px \+ var\(--ui-font-delta\)\);/,
    );
    expect(chatCss).toMatch(
      /--chat-prose-heading-lg:\s*var\(--text-lg\);/,
    );
    expect(chatCss).toMatch(
      /--chat-prose-heading-base:\s*var\(--text-md\);/,
    );
    expect(chatCss).toMatch(
      /\.chat-md h1\s*\{[^}]*font-size:\s*var\(--chat-prose-heading-xl\);/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md h2\s*\{[^}]*font-size:\s*var\(--chat-prose-heading-lg\);/s,
    );
    expect(chatCss).toMatch(
      /\.chat-md h3\s*\{[^}]*font-size:\s*var\(--chat-prose-heading-base\);/s,
    );
    // ZCode 标题不自带行高与字距：两者都继承容器的 leading-1.75 / tracking-wide。
    expect(chatCss).not.toMatch(/\.chat-md h[1-6]\s*\{[^}]*line-height:/s);
    expect(chatCss).not.toMatch(/\.chat-md h1,\s*\.chat-md h2,[^}]*letter-spacing:\s*0;/s);
  });

  it("Markdown 正文使用 ZCode 的 unitless 行高和 tracking-wide，表格继承正文行高", () => {
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));

    expect(chatCss).toMatch(
      /\.chat-md\s*\{[^}]*font-size:\s*var\(--chat-prose-fs\);[^}]*line-height:\s*1\.75;[^}]*letter-spacing:\s*0\.025em;/s,
    );
    expect(chatCss).toMatch(
      /\.chat-code__pre\s*\{[^}]*line-height:\s*calc\(var\(--spacing\) \* 5\);/s,
    );
    // 代码字号对齐 ZCode：行内代码与代码块默认都是 12px（ui-sm / codePreviewSettings）。
    expect(chatCss).toMatch(
      /\.chat-md__inline-code,\s*\.chat-md :not\(pre\) > code\s*\{[^}]*font-size:\s*var\(--text-xs\);/s,
    );
    expect(chatCss).toMatch(
      /\.chat-code__pre\s*\{[^}]*font-size:\s*var\(--text-xs\);/s,
    );
    const tableRule = chatCss.match(/\.chat-md table\s*\{([^}]*)\}/s)?.[1] ?? "";
    expect(tableRule).toContain("border-collapse: separate;");
    expect(tableRule).toContain("border-spacing: 0;");
    expect(tableRule).toContain("width: max-content;");
    expect(tableRule).toContain("min-width: 100%;");
    expect(tableRule).not.toContain("line-height:");
  });

  it("历史媒体附件使用 80px 尺寸，文件附件使用紧凑 pill 且不污染 Composer", () => {
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));
    const composerCss = readSource(
      new URL("../../styles/app-conversation.css", import.meta.url),
    );

    expect(chatCss).toMatch(
      /\.lobe-chat \.att-card--image\s*\{[^}]*width:\s*80px;[^}]*height:\s*80px;[^}]*min-height:\s*80px;[^}]*flex:\s*0 0 80px;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat \.att-card:not\(\.att-card--image\)\s*\{[^}]*height:\s*auto;[^}]*min-height:\s*0;[^}]*border:\s*0;[^}]*border-radius:\s*var\(--radius-full\);/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat \.att-card:not\(\.att-card--image\) \.att-card__btn\s*\{[^}]*padding:\s*6px 12px;[^}]*min-height:\s*0;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat \.att-card__icon\s*\{[^}]*width:\s*36px;[^}]*height:\s*36px;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat \.att-card__btn--image,\s*\.lobe-chat \.att-card__thumb\s*\{[^}]*width:\s*80px;[^}]*height:\s*80px;[^}]*min-height:\s*80px;/s,
    );

    // lobe-chat.css must only override history cards; Composer chips keep their own 48px contract.
    expect(chatCss).not.toMatch(/(?:^|\n)\s*\.att-card(?:[.:#\[]|\s|\{)/);
    expect(chatCss).not.toContain("attach-chip");
    expect(composerCss).toMatch(
      /\.attach-chip\s*\{[^}]*--attach-h:\s*48px;/s,
    );
    expect(composerCss).toMatch(
      /\.attach-chip--image\s*\{[^}]*width:\s*var\(--attach-h\);[^}]*max-width:\s*var\(--attach-h\);/s,
    );
  });

  it("将助手消息操作区放在左下角", () => {
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));

    expect(chatCss).toMatch(
      /\.lobe-chat-item--assistant \.lobe-chat-item__actions\s*\{[^}]*justify-content:\s*flex-start\s*;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat-item:hover \.lobe-chat-item__actions,\s*\.lobe-chat-item:focus-within \.lobe-chat-item__actions\s*\{/s,
    );
    expect(chatCss).toMatch(
      /@media \(hover: none\)\s*\{[\s\S]*?\.lobe-chat-item__actions\s*\{[^}]*opacity:\s*1;/s,
    );
  });

  it("用户到助手只复用一个 20px 组间距，且不改变独立行与内容内间距", () => {
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));

    expect(chatCss).toMatch(
      /\.lobe-chat-item\s*\{[^}]*padding:\s*56px 16px 20px;[^}]*gap:\s*0;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat-item--assistant\s*\{[^}]*padding-top:\s*20px;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat-item--user\s*\+\s*\.lobe-chat-item--assistant,[\s\S]*?padding-top:\s*0;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat-item__body\s*\{[^}]*gap:\s*20px;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat-item__actions\s*\{[^}]*margin-top:\s*4px;/s,
    );
    expect(chatCss).toMatch(
      /\.lobe-chat-assistant-timeline\s*\{[^}]*gap:\s*16px;/s,
    );
  });

  it("消息列和 Composer 按 conversation-stage 容器宽度响应，移动 sticky 不增加顶部空隙", () => {
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));
    const conversationCss = readSource(
      new URL("../../styles/app-conversation.css", import.meta.url),
    );
    const governanceCss = readSource(
      new URL("../../styles/ui-governance.css", import.meta.url),
    );

    expect(conversationCss).toMatch(
      /\.main__stage\s*\{[\s\S]*?container-type:\s*inline-size;[\s\S]*?container-name:\s*conversation-stage;/,
    );
    expect(chatCss).toContain(
      "@container conversation-stage (min-width: 864px)",
    );
    expect(chatCss).toContain(
      "@container conversation-stage (min-width: 1280px)",
    );
    expect(conversationCss).toContain(
      "@container conversation-stage (min-width: 864px)",
    );
    expect(conversationCss).toContain(
      "@container conversation-stage (min-width: 1280px)",
    );
    expect(conversationCss).toMatch(
      /@container conversation-stage \(max-width: 660px\)[\s\S]*?\.composer-goal\s*\{[^}]*grid-template-columns:/s,
    );
    expect(conversationCss).not.toMatch(
      /@media \(max-width: 660px\)[\s\S]*?\.composer-goal\s*\{[^}]*grid-template-columns:/s,
    );
    expect(conversationCss).toMatch(
      /@container conversation-stage \(max-width: 760px\)[\s\S]*?\.composer-wrap--welcome\s*\{/s,
    );
    expect(governanceCss).toMatch(
      /@media \(max-width: 760px\)[\s\S]*?\.composer-wrap--sticky\s*\{[\s\S]*?padding-top:\s*0;/s,
    );
  });

  it("离底阅读时只对消息层应用与 Composer 留白对齐的动态遮罩", () => {
    const source = readSource(new URL("./ConversationThread.tsx", import.meta.url));

    expect(source).toContain("COMPOSER_MESSAGE_MASK_TRANSPARENT_HEIGHT_PX = 96");
    expect(source).toContain("COMPOSER_MESSAGE_MASK_FADE_PX = 24");
    expect(source).toContain('ref={messageLayerRef} className="lobe-chat__inner"');
    expect(source).toContain("viewport.addEventListener(\"scroll\", scheduleSync");
    expect(source).toContain("messageLayer.style.maskImage = maskImage");
    expect(source).toContain("messageLayer.style.webkitMaskImage = maskImage");
    expect(source).toMatch(
      /if \(distanceToBottom <= 2\) \{[\s\S]*?messageLayer\.style\.maskImage = "none";/,
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

  it("失败 Turn 重放后展示无图标耗时并由 Appica 管理触发器视觉", () => {
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
    const css = readSource(new URL("./lobe-chat.css", import.meta.url));

    expect(html).toContain("已工作 1分钟 41秒");
    expect(html).not.toContain("lobe-chat-thinking__status-icon");
    expect(html).not.toContain("lobe-chat-thinking__status-chevron");
    const statusRule = css.match(
      /\.lobe-chat-thinking__trigger--status\s*\{([^}]*)\}/,
    )?.[1];
    expect(statusRule).toMatch(/min-height:\s*34px/);
    expect(statusRule).not.toMatch(/(?:border|background|color|outline):/);
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

    expect(html.match(/data-variant="think"/g) ?? []).toHaveLength(1);
    expect(html).not.toContain("先分析请求");
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

    expect(html.match(/data-variant="think"/g) ?? []).toHaveLength(2);
    expect(html).not.toContain("第一段分析");
    expect(html).not.toContain("第二段分析");
  });

  it("完成后最新轮次也遵循悬浮显示用量和用时入口", () => {
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
              timeToFirstTokenMs: 610,
              timeToFirstVisibleTokenMs: 610,
              totalMs: 8_300,
              inputTokens: 4_000,
              outputTokens: null,
              totalTokens: null,
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

    expect(html).not.toContain("lobe-chat-item__actions--visible");
    expect(html).toContain('data-testid="turn-metrics"');
    expect(html).not.toContain("发送确认 16ms");
    expect(html).not.toContain("首 SSE 540ms");
    expect(html).toContain("用量 —");
    expect(html).toContain("用时 8秒");
    expect(html).not.toContain("缓存命中");
    expect(html.indexOf('aria-label="复制"')).toBeLessThan(
      html.indexOf('data-testid="turn-metrics"'),
    );


  });

  it("完成轮次的 footer 始终位于多段 Assistant 内容的末尾", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{
          id: "assistant-tail-actions",
          role: "assistant",
          content: "阶段结果最终答案",
          createdAt: "2026-09-22T12:34:56.000Z",
          segments: [
            { kind: "thought", text: "先检查实现" },
            { kind: "tool", toolCallId: "tail-tool", title: "Read", status: "completed" },
            { kind: "content", text: "阶段结果" },
            { kind: "thought", text: "再验证结果" },
            { kind: "content", text: "最终答案" },
          ],
        }]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    const footer = html.indexOf('class="lobe-chat-item__actions"');
    expect(footer).toBeGreaterThan(html.indexOf("最终答案"));
    expect(footer).toBeGreaterThan(html.indexOf('data-testid="turn-work-group"'));
    expect(html).toContain('data-testid="turn-metrics"');
    expect(html).toContain('class="lobe-chat-action-time"');
  });

  it("仅附件的完成 Assistant 也保留位于附件之后的 footer", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{
          id: "assistant-only-attachment",
          role: "assistant",
          content: "",
          attachments: [{ path: "C:\\work\\report.md", name: "report.md", isDir: false }],
          createdAt: "2026-09-22T12:34:56.000Z",
        }]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    const attachment = html.indexOf("report.md");
    const footer = html.indexOf('class="lobe-chat-item__actions"');
    expect(attachment).toBeGreaterThanOrEqual(0);
    expect(footer).toBeGreaterThan(attachment);
    expect(html).toContain('data-testid="turn-metrics"');
  });

  it("独立 tool-before-assistant 行不吞掉后续 Assistant footer", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          {
            id: "tool-before-assistant",
            role: "tool",
            content: "",
            marker: "tool_step",
            toolCallId: "before-tool",
            toolKind: "Read",
            toolStatus: "completed",
            toolDetail: "README.md",
          },
          {
            id: "assistant-after-tool",
            role: "assistant",
            content: "工具之后的回答",
            createdAt: "2026-09-22T12:34:56.000Z",
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    const tool = html.indexOf("README.md");
    const answer = html.indexOf("工具之后的回答");
    const footer = html.indexOf('class="lobe-chat-item__actions"');
    expect(tool).toBeGreaterThanOrEqual(0);
    expect(tool).toBeLessThan(answer);
    expect(footer).toBeGreaterThan(answer);
  });

  it("只在最新已完成 Assistant 的 footer 提供 Fork，并保持复制、Fork、指标、时间顺序", () => {
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[
          { id: "user-latest", role: "user", content: "继续检查" },
          {
            id: "assistant-older",
            role: "assistant",
            content: "中间结果",
            createdAt: "2026-09-22T12:33:56.000Z",
          },
          {
            id: "assistant-latest",
            role: "assistant",
            content: "最终回答",
            createdAt: "2026-09-22T12:34:56.000Z",
            turnMetrics: {
              turnId: "turn-latest",
              sendAcknowledgementMs: 16,
              timeToFirstSseMs: 540,
              timeToFirstTokenMs: 610,
              timeToFirstVisibleTokenMs: 610,
              totalMs: 8_300,
              inputTokens: 4_000,
              outputTokens: null,
              totalTokens: null,
              reasoningTokens: 300,
              cacheReadTokens: 3_000,
              cacheCreationTokens: 0,
            },
          },
        ]}
        sessionState="ready"
        attachLabels={attachLabels}
        onForkCurrentSession={() => {}}
      />,
    );

    expect(html.match(/aria-label="分叉会话"/g)).toHaveLength(1);
    const latestFooter = html.lastIndexOf('class="lobe-chat-item__actions"');
    const latestAnswer = html.lastIndexOf("最终回答");
    const latestCopy = html.indexOf('aria-label="复制"', latestFooter);
    const latestFork = html.indexOf('aria-label="分叉会话"', latestCopy);
    const latestMetrics = html.indexOf('data-testid="turn-metrics"', latestFork);
    const latestTime = html.indexOf('class="lobe-chat-action-time"', latestMetrics);

    expect(latestFooter).toBeGreaterThan(latestAnswer);
    expect(latestCopy).toBeGreaterThan(latestFooter);
    expect(latestFork).toBeGreaterThan(latestCopy);
    expect(latestMetrics).toBeGreaterThan(latestFork);
    expect(latestTime).toBeGreaterThan(latestMetrics);
  });

  it("流式、失败或取消的 Assistant，以及缺少真实回调时不显示 Fork", () => {
    const render = (
      message: ChatMessage,
      onForkCurrentSession?: () => void,
      sessionState: "ready" | "streaming" = "ready",
    ) =>
      renderToString(
        <ConversationThread
          locale="zh"
          messages={[message]}
          sessionState={sessionState}
          attachLabels={attachLabels}
          onForkCurrentSession={onForkCurrentSession}
        />,
      );

    expect(
      render(
        {
          id: "assistant-streaming-fork",
          role: "assistant",
          content: "正在输出",
          streaming: true,
        },
        () => {},
        "streaming",
      ),
    ).not.toContain('aria-label="分叉会话"');
    expect(
      render(
        {
          id: "assistant-failed-fork",
          role: "assistant",
          content: "失败",
          turnStatus: "failed",
        },
        () => {},
      ),
    ).not.toContain('aria-label="分叉会话"');
    expect(
      render({
        id: "assistant-cancelled-fork",
        role: "assistant",
        content: "取消",
        turnStatus: "cancelled",
      }, () => {}),
    ).not.toContain('aria-label="分叉会话"');
    expect(
      render({
        id: "assistant-without-fork",
        role: "assistant",
        content: "没有回调",
      }),
    ).not.toContain('aria-label="分叉会话"');
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
              timeToFirstTokenMs: 450,
              timeToFirstVisibleTokenMs: 450,
              totalMs: null,
              inputTokens: null,
              outputTokens: null,
              totalTokens: null,
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

  it("把 Runtime 系统通知渲染为安静的时间线状态行", () => {
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
      html.indexOf("修复已经完成。"),
    );
    // 回合落定后，末尾答案之前的工作单元折进整体折叠组，不再平铺渲染。
    expect(html).toContain('data-testid="turn-work-group"');
    expect(html).not.toContain("检查处理时间的渲染来源");
    expect(html).not.toContain("验证多段思考的展示结果");
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

    expect(html).toContain('<div class="lobe-chat-bubble">');
    expect(html).toContain('class="lobe-chat-user-body__content"');
    expect(html).toContain(
      '<span class="user-msg-body">以及本地/远程模型连接能力</span>',
    );
  });

  it("历史消息 Mention 使用独立的 inline token，并保留 ComposerMentionKind 语义", () => {
    const mention = {
      id: "file:README.md",
      kind: "file" as const,
      label: "README.md",
      value: "/work/README.md",
      markdown: buildComposerMentionMarkdown("file", "README.md", "/work/README.md"),
      data: { path: "/work/README.md" },
    };
    const html = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{
          id: "user-mention",
          role: "user",
          content: `请查看 ${encodeComposerMention(mention)}`,
        }]}
        sessionState="ready"
        attachLabels={attachLabels}
      />,
    );

    expect(html).toContain('class="message-mention message-mention--file"');
    expect(html).toContain('data-mention-kind="file"');
    expect(html).toContain('class="message-mention__icon"');
    expect(html).toContain("@README.md");
    expect(html).not.toContain("composer-mention--message");
  });

  it("消息 Mention 和用户附件容器遵循消息层的 typography 与 max-w-xl", () => {
    const source = readSource(new URL("./ConversationThread.tsx", import.meta.url));
    const css = readSource(new URL("./lobe-chat.css", import.meta.url));
    expect(source).toContain("messageMentionIcon");
    expect(source).toContain("type ComposerMentionKind");
    expect(css).toMatch(/\.message-mention\s*\{[^}]*font-weight:\s*500;[^}]*line-height:\s*calc\(24px/s);
    expect(css).toMatch(/\.lobe-chat-atts--user\s*\{[^}]*max-width:\s*min\(100%,\s*36rem\)/s);
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
    const source = readSource(
      new URL("./ConversationThread.tsx", import.meta.url),
    );
    const css = readSource(new URL("./lobe-chat.css", import.meta.url));

    expect(source).toContain("<Textarea");
    expect(source).toContain("onSend(value.trim())");
    expect(source).toContain('event.key === "Escape"');
    expect(source).toContain("event.metaKey || event.ctrlKey");
    expect(source).toContain('variant="primary"');
    expect(css).toMatch(/\.lobe-chat-user-editor\s*\{[^}]*border-radius:\s*var\(--radius-lg\);/s);
  });

  it("用户消息复制逻辑同时接入文档事件和正文选择边界", () => {
    const threadSource = readSource(
      new URL("./ConversationThread.tsx", import.meta.url),
    );
    const chatCss = readSource(new URL("./lobe-chat.css", import.meta.url));
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
    const source = readSource(new URL("../AttachmentCard.tsx", import.meta.url));
    const cardImage =
      source.match(/<Thumbnail[\s\S]*?onLoadingStatusChange[\s\S]*?\/>/)?.[0] ?? "";

    expect(cardImage).toContain('onLoadingStatusChange={(status) => {');
    expect(cardImage).toContain('if (status === "error") void recoverThumbnail();');
    expect(source).toContain("await resolveImageSrc(attachment.path)");
  });
});

describe("ConversationThread 会话恢复指示", () => {
  it("恢复窗口内空消息显示 Spinner 占位，正常空态与已有消息不显示", () => {
    const restoring = renderToString(
      <ConversationThread
        locale="zh"
        messages={[]}
        sessionState="connecting"
        suppressEmptyCopy
        attachLabels={attachLabels}
      />,
    );
    expect(restoring).toContain('data-slot="lobe-chat-restoring"');

    const freshDraft = renderToString(
      <ConversationThread
        locale="zh"
        messages={[]}
        sessionState="idle"
        suppressEmptyCopy={false}
        attachLabels={attachLabels}
      />,
    );
    expect(freshDraft).not.toContain('data-slot="lobe-chat-restoring"');

    const withMessages = renderToString(
      <ConversationThread
        locale="zh"
        messages={[{ id: "u-1", role: "user", content: "hello" }]}
        sessionState="connecting"
        suppressEmptyCopy
        attachLabels={attachLabels}
      />,
    );
    expect(withMessages).not.toContain('data-slot="lobe-chat-restoring"');
  });
});
