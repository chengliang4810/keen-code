import { describe, expect, it } from "vitest";
import {
  isForegroundRequestDone,
  parseAgentEvent,
  shouldAcceptAgentDone,
  shouldApplyAgentEvent,
  shouldApplySessionUpdate,
  shouldDriveMainSessionStreaming,
  type AcpEvent,
  type AgentDoneEnvelope,
  type AgentEventEnvelope,
  type SessionUpdateEnvelope,
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
      {
        type: "oauth_needed",
        value: { server_name: "docs", auth_url: "https://auth.test" },
      },
      { type: "oauth_completed", value: { server_name: "docs" } },
      {
        type: "oauth_failed",
        value: { server_name: "docs", error: "cancelled" },
      },
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
        JSON.stringify({
          ...base,
          value: { ...base.value, trigger: "manual" },
        }),
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
});

describe("ACP requestId 完成契约", () => {
  it("只有带 requestId 与完成时间的前台请求可以收口", () => {
    const foreground: AgentDoneEnvelope["params"] = {
      sessionId: "session-1",
      requestId: "request-1",
      stopReason: "end_turn",
      _keencode: { completedAtMs: 1_000 },
    };
    const background: AgentDoneEnvelope["params"] = {
      sessionId: "session-1",
      stopReason: "end_turn",
    };

    expect(isForegroundRequestDone(foreground)).toBe(true);
    expect(isForegroundRequestDone(background)).toBe(false);
  });

  it("只有当前 requestId 的终态通知可以结束活跃请求", () => {
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

describe("ACP requestId 事件关联", () => {
  const sessionParams = (
    requestId: string | undefined,
    update: SessionUpdateEnvelope["params"]["update"],
  ): SessionUpdateEnvelope["params"] => ({
    sessionId: "session-1",
    requestId,
    update,
  });

  it("实时正文和 usage 只写入精确匹配的 active request", () => {
    const text = sessionParams("request-old", {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "旧正文" },
    });
    const usage = sessionParams("request-new", {
      sessionUpdate: "usage_update",
      used: 10,
      size: 100,
      _meta: { llmStep: 0, inputTokens: 10 },
    });

    expect(shouldApplySessionUpdate(text, "request-new")).toBe(false);
    expect(shouldApplySessionUpdate(text, null)).toBe(false);
    expect(shouldApplySessionUpdate(usage, "request-new")).toBe(true);
  });

  it("历史重放、已登记子 Agent 和会话级更新不依赖前台请求", () => {
    const replay = sessionParams(undefined, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "历史思考" },
      _meta: { periReplay: true },
    });
    const child = {
      ...sessionParams("request-old", {
        sessionUpdate: "agent_message_chunk" as const,
        content: { type: "text" as const, text: "后台结果" },
      }),
      _peri: { sourceAgentId: "child-1" },
    };
    const config = sessionParams(undefined, {
      sessionUpdate: "current_mode_update",
      currentModeId: "default",
    });

    expect(shouldApplySessionUpdate(replay, null)).toBe(true);
    expect(
      shouldApplySessionUpdate(child, "request-new", "child-1"),
    ).toBe(true);
    expect(shouldApplySessionUpdate(config, null)).toBe(true);
  });

  it("未登记的 sourceAgentId 仍按主 Agent 请求关联", () => {
    const main = {
      ...sessionParams("request-old", {
        sessionUpdate: "agent_message_chunk" as const,
        content: { type: "text" as const, text: "主回复" },
      }),
      _peri: { sourceAgentId: "main-agent" },
    };

    expect(shouldApplySessionUpdate(main, "request-new")).toBe(false);
  });

  const agentParams = (
    requestId: string | undefined,
  ): AgentEventEnvelope["params"] => ({
    sessionId: "session-1",
    requestId,
    event_json: "{}",
  });

  it("迟到的旧失败不能覆盖新请求，项目 Goal 和后台生命周期可跨请求", () => {
    const failure: AcpEvent = {
      type: "agent_execution_failed",
      value: { message: "旧错误" },
    };
    const child: AcpEvent = {
      type: "subagent_stopped",
      value: {
        agent_name: "worker",
        result: "done",
        is_error: false,
        instance_id: "child-1",
      },
    };

    expect(
      shouldApplyAgentEvent(
        agentParams("request-old"),
        failure,
        "request-new",
      ),
    ).toBe(false);
    expect(shouldApplyAgentEvent(agentParams(undefined), failure, null)).toBe(
      false,
    );
    expect(
      shouldApplyAgentEvent(
        agentParams("request-old"),
        child,
        "request-new",
      ),
    ).toBe(true);
  });
});
