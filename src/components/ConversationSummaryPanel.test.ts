import { describe, expect, it } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import {
  ConversationSummaryPanel,
  compactToolDetail,
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

  it("工具详情超过上限时截断，避免超大 DOM", () => {
    const compacted = compactToolDetail("x".repeat(5_000));
    expect(compacted.length).toBeLessThan(5_000);
    expect(compacted.endsWith("\n…")).toBe(true);
  });

  it("没有子智能体时隐藏子智能体栏目", () => {
    const html = renderToStaticMarkup(
      createElement(ConversationSummaryPanel, {
        open: true,
        projectPath: "/repo",
        sessionId: "session-1",
        sessionState: "ready",
        subagents: [],
        locale: "zh",
        onClose: () => {},
        onOpenChanges: () => {},
      }),
    );

    expect(html).toContain("变更");
    expect(html).toContain("当前分支");
    expect(html).toContain("提交或推送");
    expect(html).not.toContain("子智能体");
  });

  it("存在子智能体时展示子智能体栏目", () => {
    const html = renderToStaticMarkup(
      createElement(ConversationSummaryPanel, {
        open: true,
        projectPath: "/repo",
        sessionId: "session-1",
        sessionState: "ready",
        subagents: [agent()],
        locale: "zh",
        onClose: () => {},
        onOpenChanges: () => {},
      }),
    );

    expect(html).toContain("子智能体");
    expect(html).toContain("1 个子智能体运行中");
  });
});
