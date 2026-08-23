import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import {
  ConversationSummaryPanel,
  shouldCloseConversationSummaryPanel,
  subagentExcerpt,
} from "./ConversationSummaryPanel";

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
        onOpenSubagent: () => {},
      }),
    );

    expect(html).toContain("子智能体");
    expect(html).toContain("本任务已创建：1");
    expect(html).toContain("explorer");
  });

});
