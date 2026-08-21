import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import {
  canResumeSubagent,
  ConversationSummaryPanel,
  compactToolDetail,
  shouldCloseConversationSummaryPanel,
  subagentExcerpt,
} from "./ConversationSummaryPanel";
import { createT } from "@/i18n";

function agent(overrides: Partial<AcpSubagentInfo> = {}): AcpSubagentInfo {
  return {
    agent_id: "child-1",
    agent_name: "explorer",
    status: "running",
    is_background: false,
    started_at: 1_000,
    stopped_at: null,
    result: null,
    segments: [],
    ...overrides,
  };
}

describe("ConversationSummaryPanel helpers", () => {
  it("列表摘要优先展示子 Agent 正文并压平空白", () => {
    expect(
      subagentExcerpt(
        agent({
          result: "fallback",
          segments: [
            { kind: "thought", text: "先思考" },
            { kind: "content", text: "已定位\n  数据入口" },
          ],
        }),
      ),
    ).toBe("已定位 数据入口");
  });

  it("没有正文时回退到完成结果", () => {
    expect(subagentExcerpt(agent({ result: "检查完成" }))).toBe("检查完成");
  });

  it("工具详情超过上限时截断，避免超大 DOM", () => {
    const compacted = compactToolDetail("x".repeat(5_000));
    expect(compacted.length).toBeLessThan(5_000);
    expect(compacted.endsWith("\n…")).toBe(true);
  });

  it("只允许继续已经结束且具有稳定子线程标识的子智能体", () => {
    expect(canResumeSubagent(agent())).toBe(false);
    expect(canResumeSubagent(agent({ status: "failed" }))).toBe(true);
    expect(canResumeSubagent(agent({ status: "done" }))).toBe(true);
    expect(canResumeSubagent(agent({ status: "failed", agent_id: " " }))).toBe(
      false,
    );
  });

  it("只在点击任务摘要面板以外时关闭", () => {
    const inside = {} as EventTarget;
    const triggerTarget = {} as EventTarget;
    const outside = {} as EventTarget;
    const panel = {
      contains: (target: Node | null) => target === (inside as Node),
    } as Pick<HTMLElement, "contains">;
    const trigger = {
      contains: (target: Node | null) => target === (triggerTarget as Node),
    } as Pick<HTMLElement, "contains">;

    expect(
      shouldCloseConversationSummaryPanel(panel, trigger, inside),
    ).toBe(false);
    expect(
      shouldCloseConversationSummaryPanel(panel, trigger, triggerTarget),
    ).toBe(false);
    expect(
      shouldCloseConversationSummaryPanel(panel, trigger, outside),
    ).toBe(true);
    expect(
      shouldCloseConversationSummaryPanel(null, trigger, outside),
    ).toBe(false);
  });

  it("在捕获阶段使用 pointerdown 关闭，避免事件被外部控件截断", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./ConversationSummaryPanel.tsx", import.meta.url)),
      "utf8",
    );

    expect(source).toContain(
      'document.addEventListener("pointerdown", onDocumentPointerDown, true)',
    );
    expect(source).toContain("triggerRef.current");
  });

  it("流式任务进入终态时强制绕过 Git 短时缓存", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./ConversationSummaryPanel.tsx", import.meta.url)),
      "utf8",
    );

    expect(source).toContain("api.gitStatus(projectPath, { force })");
    expect(source).toContain('previous === "streaming"');
    expect(source).toContain("void refreshGit(true)");
  });

  it("继续提示精确携带 child_thread_id 并禁止新建子智能体", () => {
    const prompt = createT("zh")("summary.subagents.resumePrompt", {
      id: "child-thread-1",
      name: "explorer",
    });
    expect(prompt).toContain(
      'Agent(resume_thread_id: "child-thread-1")',
    );
    expect(prompt).toContain("只能调用一次");
    expect(prompt).toContain("不要创建新的子智能体");
  });

  it("没有子智能体时隐藏子智能体栏目", () => {
    const html = renderToStaticMarkup(
      createElement(ConversationSummaryPanel, {
        open: true,
        triggerRef: { current: null },
        projectPath: "/repo",
        sessionId: "session-1",
        sessionState: "ready",
        subagents: [],
        locale: "zh",
        onClose: () => {},
        onOpenChanges: () => {},
        onResumeSubagent: async () => true,
      }),
    );

    expect(html).toContain("摘要");
    expect(html).toContain("变更");
    expect(html).toContain("当前分支");
    expect(html).toContain("提交或推送");
    expect(html).not.toContain("任务摘要");
    expect(html).not.toContain("正在加载 git 状态");
    expect(html).not.toContain("请先选择项目文件夹");
    expect(html).not.toContain('aria-label="刷新摘要"');
    expect(html).not.toContain("子智能体");
    expect(html).not.toContain("后台进程");
  });

  it("存在子智能体时展示子智能体栏目", () => {
    const html = renderToStaticMarkup(
      createElement(ConversationSummaryPanel, {
        open: true,
        triggerRef: { current: null },
        projectPath: "/repo",
        sessionId: "session-1",
        sessionState: "ready",
        subagents: [agent()],
        locale: "zh",
        onClose: () => {},
        onOpenChanges: () => {},
        onResumeSubagent: async () => true,
      }),
    );

    expect(html).toContain("子智能体");
    expect(html).toContain("1 个子智能体运行中");
  });

});
