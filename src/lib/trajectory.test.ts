import { describe, expect, it } from "vitest";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import type { ChatMessage } from "@/lib/session";
import {
  buildTrajectoryRecords,
  compactTrajectoryDetail,
  filterTrajectoryRecords,
  summarizeTrajectory,
  toolRecordStatus,
  trajectorySingleLine,
} from "@/lib/trajectory";

function userMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "u1",
    role: "user",
    content: "帮我定位\n  数据入口",
    createdAt: "2026-08-20T10:00:00.000Z",
    ...overrides,
  };
}

function assistantMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "a1",
    role: "assistant",
    content: "已找到入口",
    createdAt: "2026-08-20T10:00:05.000Z",
    ...overrides,
  };
}

function subagent(
  overrides: Partial<AcpSubagentInfo> = {},
): AcpSubagentInfo {
  return {
    agent_id: "child-1",
    agent_name: "explorer",
    status: "done",
    is_background: true,
    started_at: 1_000,
    stopped_at: 61_000,
    result: "调研完成",
    segments: [
      { kind: "thought", text: "先想" },
      {
        kind: "tool",
        toolCallId: "t-child",
        title: "read file",
        status: "completed",
      },
    ],
    ...overrides,
  };
}

describe("buildTrajectoryRecords", () => {
  it("把用户、思考、工具与回复映射为按轮分组的记录", () => {
    const messages: ChatMessage[] = [
      userMessage(),
      assistantMessage({
        segments: [
          { kind: "thought", text: "先搜索" },
          {
            kind: "tool",
            toolCallId: "t1",
            title: "grep 关键字",
            status: "completed",
            input: "{ \"q\": \"入口\" }",
            output: "src/api.ts:12",
            durationMs: 1_500,
          },
          { kind: "content", text: "已找到入口" },
        ],
        thinkingDurationMs: 900,
        turnMetrics: {
          turnId: "turn-1",
          sendAcknowledgementMs: 30,
          timeToFirstSseMs: 400,
          timeToFirstVisibleTokenMs: 500,
          totalMs: 4_000,
          inputTokens: 1_200,
          cacheReadTokens: 800,
          cacheCreationTokens: 100,
        },
      }),
    ];

    const records = buildTrajectoryRecords(messages);
    expect(records.map((r) => r.kind)).toEqual([
      "user",
      "thinking",
      "tool",
      "assistant",
    ]);
    expect(records.map((r) => r.index)).toEqual([1, 2, 3, 4]);
    expect(records.every((r) => r.turn === 1)).toBe(true);
    expect(records[0]!.opensTurn).toBe(true);
    expect(records.slice(1).every((r) => !r.opensTurn)).toBe(true);

    const thinking = records[1]!;
    expect(thinking.thinking).toBe("先搜索");
    expect(thinking.durationMs).toBe(900);

    const tool = records[2]!;
    expect(tool.title).toBe("grep 关键字");
    expect(tool.status).toBe("completed");
    expect(tool.durationMs).toBe(1_500);
    expect(tool.input).toContain("入口");
    expect(tool.output).toBe("src/api.ts:12");

    const assistant = records[3]!;
    expect(assistant.output).toBe("已找到入口");
    expect(assistant.metrics?.inputTokens).toBe(1_200);
    // 指标只挂在首条正文记录上，避免多段正文重复计数。
    expect(records[1]!.metrics).toBeUndefined();
  });

  it("多轮会话按用户消息递增轮次并保留轮前前缀", () => {
    const messages: ChatMessage[] = [
      assistantMessage({ id: "a0", content: "轮前回复" }),
      userMessage({ id: "u1", content: "第一问" }),
      assistantMessage({ id: "a1", content: "第一答" }),
      userMessage({ id: "u2", content: "第二问" }),
      assistantMessage({ id: "a2", content: "第二答" }),
    ];
    const records = buildTrajectoryRecords(messages);
    expect(records[0]!.turn).toBe(0);
    expect(records[0]!.opensTurn).toBe(false);
    expect(records[1]!.turn).toBe(1);
    expect(records[1]!.opensTurn).toBe(true);
    expect(records[3]!.turn).toBe(2);
    expect(records[3]!.opensTurn).toBe(true);
  });

  it("已内联的工具行去重，未内联的独立工具行保留", () => {
    const messages: ChatMessage[] = [
      userMessage(),
      assistantMessage({
        segments: [
          {
            kind: "tool",
            toolCallId: "t1",
            title: "内联工具",
            status: "completed",
          },
          { kind: "content", text: "完成" },
        ],
      }),
      {
        id: "tool-t1",
        role: "tool",
        content: "内联工具",
        marker: "tool_step",
        toolCallId: "t1",
        toolStatus: "completed",
      },
      {
        id: "tool-t2",
        role: "tool",
        content: "独立工具",
        marker: "tool_step",
        toolCallId: "t2",
        toolStatus: "failed",
        toolDetail: "命令退出码 1",
        isError: true,
      },
    ];
    const records = buildTrajectoryRecords(messages);
    const tools = records.filter((r) => r.kind === "tool");
    expect(tools.map((t) => t.key)).toEqual(["a1:tool:0", "tool:t2"]);
    expect(tools[1]!.status).toBe("failed");
    expect(tools[1]!.output).toBe("命令退出码 1");
  });

  it("重放有损映射后的裸工具行也映射为工具记录", () => {
    // peri 把无 tool_use 块的工具调用存为独立 tool 行；restoreStoredHistory
    // 只保留 {role, content, thought, segments}，marker/toolCallId 被剥掉。
    const messages: ChatMessage[] = [
      userMessage(),
      assistantMessage({
        segments: [
          { kind: "thought", text: "先看目录" },
          { kind: "content", text: "已列出文件" },
        ],
      }),
      { id: "s1:history:2", role: "tool", content: "total 72\ndrwxr-xr-x" },
      { id: "s1:history:3", role: "tool", content: "写入完成" },
    ];
    const records = buildTrajectoryRecords(messages);
    const tools = records.filter((r) => r.kind === "tool");
    expect(tools).toHaveLength(2);
    expect(tools[0]!.status).toBe("completed");
    expect(tools[0]!.title).toBe("total 72 drwxr-xr-x");
    expect(tools[0]!.output).toBe("total 72\ndrwxr-xr-x");
  });

  it("压缩、取消与错误标记映射为独立记录", () => {
    const messages: ChatMessage[] = [
      userMessage(),
      {
        id: "c1",
        role: "tool",
        content: "context_compact|auto|tokens:9000->2000",
        marker: "context_compact",
        compactMeta: {
          trigger: "auto",
          tokensBefore: 9_000,
          tokensAfter: 2_000,
          summaryPreview: "摘要预览",
        },
      },
      {
        id: "x1",
        role: "tool",
        content: "用户停止了本轮",
        marker: "turn_cancelled",
      },
      assistantMessage({
        id: "e1",
        content: "请求失败，请重试。",
        isError: true,
        errorBodyFormatted: true,
      }),
    ];
    const records = buildTrajectoryRecords(messages);
    expect(records.map((r) => r.kind)).toEqual([
      "user",
      "compacted",
      "cancelled",
      "error",
    ]);
    expect(records[1]!.compactMeta?.tokensBefore).toBe(9_000);
    expect(records[1]!.title).toBe("auto 9000→2000");
    expect(records[3]!.status).toBe("failed");
  });

  it("直接识别 ACP Assistant 的 cancelled turnStatus", () => {
    const records = buildTrajectoryRecords([
      userMessage({ content: "停止当前任务" }),
      assistantMessage({
        content: "已输出部分结果",
        turnStatus: "cancelled",
        turnIncomplete: true,
        thinkingDurationMs: 1_200,
      }),
    ]);

    expect(records.map((record) => record.kind)).toEqual([
      "user",
      "cancelled",
    ]);
    expect(records[1]).toMatchObject({
      title: "已输出部分结果",
      output: "已输出部分结果",
      durationMs: 1_200,
    });
  });

  it("子代理追加为记录并映射状态与耗时", () => {
    const records = buildTrajectoryRecords(
      [userMessage()],
      [subagent(), subagent({ agent_id: "child-2", status: "running", stopped_at: null })],
    );
    const agents = records.filter((r) => r.kind === "subagent");
    expect(agents).toHaveLength(2);
    expect(agents[0]!.status).toBe("completed");
    expect(agents[0]!.durationMs).toBe(60_000);
    expect(agents[0]!.subagent?.segments).toHaveLength(2);
    expect(agents[1]!.status).toBe("running");
    expect(agents[1]!.durationMs).toBeNull();
  });
});

describe("filterTrajectoryRecords", () => {
  const records = buildTrajectoryRecords([
    userMessage({ content: "帮我 修复 登录" }),
    assistantMessage({
      segments: [
        {
          kind: "tool",
          toolCallId: "t1",
          title: "edit file",
          status: "completed",
          input: "path: src/login.ts",
          output: "done",
        },
        { kind: "content", text: "登录已修复" },
      ],
    }),
  ]);

  it("多词 AND 且大小写不敏感，命中输入与输出", () => {
    expect(filterTrajectoryRecords(records, "登录")).toHaveLength(2);
    expect(filterTrajectoryRecords(records, "login.ts")).toHaveLength(1);
    expect(filterTrajectoryRecords(records, "edit src")).toHaveLength(1);
    expect(filterTrajectoryRecords(records, "edit 不存在")).toHaveLength(0);
  });

  it("空白查询返回全部记录", () => {
    expect(filterTrajectoryRecords(records, "  ")).toHaveLength(records.length);
  });
});

describe("summarizeTrajectory", () => {
  it("聚合工具数、失败数、轮数与 Token", () => {
    const records = buildTrajectoryRecords(
      [
        userMessage({ content: "问一" }),
        assistantMessage({
          id: "a1",
          segments: [
            {
              kind: "tool",
              toolCallId: "t1",
              title: "ok",
              status: "completed",
              durationMs: 100,
            },
            {
              kind: "tool",
              toolCallId: "t2",
              title: "bad",
              status: "failed",
              durationMs: 50,
            },
            { kind: "content", text: "答一" },
          ],
          turnMetrics: {
            turnId: "turn-1",
            sendAcknowledgementMs: null,
            timeToFirstSseMs: null,
            timeToFirstVisibleTokenMs: null,
            totalMs: 300,
            inputTokens: 100,
            cacheReadTokens: 60,
            cacheCreationTokens: null,
          },
        }),
        userMessage({ id: "u2", content: "问二" }),
        assistantMessage({ id: "a2", content: "答二" }),
      ],
      [subagent({ status: "failed", stopped_at: 2_000 })],
    );
    const stats = summarizeTrajectory(records);
    expect(stats.total).toBe(7);
    expect(stats.tools).toBe(2);
    expect(stats.failed).toBe(2);
    expect(stats.turns).toBe(2);
    expect(stats.totalDurationMs).toBe(100 + 50 + 1_000);
    expect(stats.inputTokens).toBe(100);
    expect(stats.cacheReadTokens).toBe(60);
    expect(stats.cacheCreationTokens).toBeNull();
  });
});

describe("helpers", () => {
  it("singleLine 折叠空白并按需截断", () => {
    expect(trajectorySingleLine("a\n  b\tc")).toBe("a b c");
    const long = "x".repeat(200);
    expect(trajectorySingleLine(long, 120)).toHaveLength(121);
    expect(trajectorySingleLine(long, 120).endsWith("…")).toBe(true);
  });

  it("compactTrajectoryDetail 超限截断", () => {
    expect(compactTrajectoryDetail("短文本")).toBe("短文本");
    const big = "y".repeat(4_100);
    expect(compactTrajectoryDetail(big)).toHaveLength(4_002);
    expect(compactTrajectoryDetail(null)).toBe("");
  });

  it("工具状态映射覆盖运行、失败与完成", () => {
    expect(toolRecordStatus({ status: "in_progress", streaming: true })).toBe(
      "running",
    );
    expect(toolRecordStatus({ status: "completed" })).toBe("completed");
    expect(toolRecordStatus({ status: "done", isError: true })).toBe("failed");
    expect(toolRecordStatus({ status: "", streaming: false })).toBe("running");
  });
});
