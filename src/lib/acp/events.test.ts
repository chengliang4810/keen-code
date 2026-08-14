import { describe, expect, it } from "vitest";
import {
  parseAgentEvent,
  shouldAcceptAgentDone,
  shouldDriveMainSessionStreaming,
} from "./events";

describe("Peri 3.6.5 ACP 事件契约", () => {
  it("识别挂起事件和 MCP OAuth host 级事件", () => {
    expect(
      parseAgentEvent(
        JSON.stringify({
          type: "turn_suspended",
          value: { turn_id: "turn-1", agent_id: "main" },
        }),
      ),
    ).toMatchObject({ type: "turn_suspended" });

    for (const event of [
      { type: "oauth_needed", value: { server_name: "docs", auth_url: "https://auth.test" } },
      { type: "oauth_completed", value: { server_name: "docs" } },
      { type: "oauth_failed", value: { server_name: "docs", error: "cancelled" } },
      { type: "oauth_restored", value: { server_name: "docs" } },
    ]) {
      expect(parseAgentEvent(JSON.stringify(event))).toEqual(event);
    }
  });

  it("压缩 trigger 缺省按 auto 处理并保留 manual", () => {
    const base = {
      type: "compact_completed",
      value: {
        summary: "保留摘要",
        files: [],
        skills: [],
        micro_cleared: 0,
        messages_json: "[]",
        strategy: "full",
        outcome: "completed",
      },
    };

    expect(parseAgentEvent(JSON.stringify(base))).toMatchObject({
      value: { trigger: "auto" },
    });
    expect(
      parseAgentEvent(
        JSON.stringify({ ...base, value: { ...base.value, trigger: "manual" } }),
      ),
    ).toMatchObject({ value: { trigger: "manual" } });
  });

  it("把 system_notification 的 warn 和 warning 统一为 warning", () => {
    for (const level of ["warn", "warning"]) {
      expect(
        parseAgentEvent(
          JSON.stringify({
            type: "system_notification",
            value: { text: "MCP 已断开", level },
          }),
        ),
      ).toEqual({
        type: "system_notification",
        value: { text: "MCP 已断开", level: "warning" },
      });
    }
  });

  it("只有主 Agent 的实时内容驱动主会话 streaming", () => {
    const update = {
      sessionUpdate: "agent_message_chunk" as const,
      content: { type: "text" as const, text: "主回复" },
    };

    expect(shouldDriveMainSessionStreaming(update)).toBe(true);
    expect(shouldDriveMainSessionStreaming(update, "child-1")).toBe(false);
  });

  it("只有当前 requestId 的终态通知可以结束活跃回合", () => {
    expect(shouldAcceptAgentDone("request-current", "request-current")).toBe(
      true,
    );
    expect(shouldAcceptAgentDone("request-current", "request-stale")).toBe(
      false,
    );
    expect(shouldAcceptAgentDone("request-current", undefined)).toBe(false);
    expect(shouldAcceptAgentDone(undefined, "request-current")).toBe(false);
  });
});
