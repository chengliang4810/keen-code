import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import {
  ConversationSummaryPanel,
  groupSummarySubagents,
  shouldCloseConversationSummaryPanel,
} from "./ConversationSummaryPanel";
import { subagentExcerpt } from "./SubagentRow";

function agent(overrides: Partial<AcpSubagentInfo> = {}): AcpSubagentInfo {
  return {
    agent_id: "child-1",
    agent_name: "explorer",
    nickname: null,
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
  it("运行中列表摘要展示最新活动并压平空白", () => {
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

  it("运行中的最新工具作为实时活动", () => {
    expect(subagentExcerpt(agent({
      segments: [
        { kind: "content", text: "已定位入口" },
        { kind: "tool", toolCallId: "read-1", title: "读取文件", status: "pending", streaming: true },
      ],
    }))).toBe("读取文件");
  });

  it("完成或错误后展示原始委派任务", () => {
    expect(subagentExcerpt(agent({
      status: "failed",
      prompt: "只读检查侧边栏 Agent 信息",
      result: "LLM HTTP 500",
    }))).toBe("只读检查侧边栏 Agent 信息");
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
    expect(source).toContain("if (dismissOnOutsidePress)");
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

  it("按运行中、失败和已完成稳定分组", () => {
    const grouped = groupSummarySubagents([
      agent({ agent_id: "done", status: "done", started_at: 4 }),
      agent({ agent_id: "running-2", started_at: 3 }),
      agent({ agent_id: "failed", status: "failed", started_at: 2 }),
      agent({ agent_id: "running-1", started_at: 1 }),
    ]);

    expect(grouped.running.map((item) => item.agent_id)).toEqual([
      "running-1",
      "running-2",
    ]);
    expect(grouped.failed.map((item) => item.agent_id)).toEqual(["failed"]);
    expect(grouped.done.map((item) => item.agent_id)).toEqual(["done"]);
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
        onOpenSubagent: () => {},
        onOpenSubagentList: () => {},
      }),
    );

    expect(html).toContain("摘要");
    expect(html).toContain("暂无摘要内容");
    expect(html).not.toContain("变更");
    expect(html).not.toContain("当前分支");
    expect(html).not.toContain("提交或推送");
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
        subagents: [agent({
          nickname: { index: 0, generation: 1 },
          agent_description: "代码库检索与只读分析",
          task_title: "检查 Agent 信息",
        })],
        locale: "zh",
        onClose: () => {},
        onOpenChanges: () => {},
        onOpenSubagent: () => {},
        onOpenSubagentList: () => {},
      }),
    );

    expect(html).toContain("子智能体");
    expect(html).not.toContain("本任务已创建");
    expect(html).toContain("孔子");
    expect(html).toContain("孔子 · 检查 Agent 信息");
    expect(html).toContain("mo-always");
  });

  it("摘要只展开运行中和失败项，已完成项收进固定入口", () => {
    const html = renderToStaticMarkup(
      createElement(ConversationSummaryPanel, {
        open: true,
        triggerRef: { current: null },
        projectPath: "/repo",
        sessionId: "session-1",
        sessionState: "ready",
        subagents: [
          agent({ agent_id: "running", agent_name: "runner" }),
          agent({ agent_id: "failed", agent_name: "reviewer", status: "failed" }),
          agent({ agent_id: "done", agent_name: "explorer", status: "done" }),
        ],
        locale: "zh",
        onClose: () => {},
        onOpenChanges: () => {},
        onOpenSubagent: () => {},
        onOpenSubagentList: () => {},
      }),
    );

    expect(html).toContain("runner");
    expect(html).toContain("reviewer");
    expect(html).not.toContain(">explorer<");
    expect(html).toContain("已完成（1）");
  });

  it("已完成入口把完整列表交给右侧资源面板", () => {
    const summarySource = readFileSync(
      fileURLToPath(new URL("./ConversationSummaryPanel.tsx", import.meta.url)),
      "utf8",
    );
    const viewerSource = readFileSync(
      fileURLToPath(new URL("./ResourceViewer.tsx", import.meta.url)),
      "utf8",
    );

    expect(summarySource).toContain("onOpenSubagentList();");
    expect(viewerSource).toContain('openRequest.type === "subagents"');
    expect(viewerSource).toContain('sideMode === "agents" ? agentList : null');
  });

});
