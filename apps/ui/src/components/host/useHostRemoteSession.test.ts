import { describe, expect, it } from "vitest";
import type { SessionUpdateDeliveryEnvelope } from "@/lib/acp/events";
import {
  buildRemotePrompt,
  redactRemoteText,
  reduceRemoteActivities,
  reduceRemoteMessages,
  remoteGapRecoverySessionId,
  remoteDeliverySequenceDecision,
} from "./useHostRemoteSession";

function textEnvelope(
  deliverySequence: number,
  text: string,
  role: "user" | "assistant" = "assistant",
): SessionUpdateDeliveryEnvelope {
  return {
    schemaVersion: 1,
    sessionId: "session-a",
    turnId: "turn-a",
    sourceAgentId: "root",
    deliverySequence,
    occurredAtMs: 1_700_000_000_000 + deliverySequence,
    update: {
      sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk",
      content: { type: "text", text },
    },
  };
}

describe("remote session projection", () => {
  it("附件发送只使用 Host 签发的标准 resource_link", () => {
    expect(buildRemotePrompt("请分析", [{
      resourceId: "resource_1",
      fileName: "photo.png",
      contentType: "image/png",
      size: 4,
      previewUrl: "/api/resources/resource_1",
    }])).toEqual([
      { type: "text", text: "请分析" },
      {
        type: "resource_link",
        name: "photo.png",
        uri: "/api/resources/resource_1",
        mimeType: "image/png",
        size: 4,
      },
    ]);
  });

  it("合并同一轮文本分片并过滤本机绝对路径", () => {
    let messages = reduceRemoteMessages([], textEnvelope(1, "已检查 C:\\Users\\demo\\secret.txt"));
    messages = reduceRemoteMessages(messages, textEnvelope(2, " 与 /home/demo/private.log"));

    expect(messages).toHaveLength(1);
    expect(messages[0]?.text).toBe("已检查 [本机路径] 与 [本机路径]");
    expect(messages[0]?.text).not.toContain("demo");
  });

  it("不把子 Agent 文本混入根会话消息流", () => {
    const envelope = { ...textEnvelope(1, "内部结果"), sourceAgentId: "child-a" };
    expect(reduceRemoteMessages([], envelope)).toEqual([]);
  });

  it("投影标准工具状态与 Diff，忽略终端内容", () => {
    const envelope: SessionUpdateDeliveryEnvelope = {
      ...textEnvelope(1, ""),
      update: {
        sessionUpdate: "tool_call_update",
        toolCallId: "write-1",
        title: "写入文件",
        status: "completed",
        content: [
          { type: "terminal", terminalId: "terminal-secret" },
          { type: "diff", path: "src/App.tsx", oldText: "old", newText: "new" },
        ],
      },
    };
    const activities = reduceRemoteActivities([], envelope);
    expect(activities).toEqual([
      expect.objectContaining({ id: "tool:write-1", kind: "tool", status: "completed" }),
      expect.objectContaining({ id: "diff:write-1:1", kind: "diff", detail: expect.stringContaining("+++ 修改后\nnew") }),
    ]);
    expect(JSON.stringify(activities)).not.toContain("terminal-secret");
  });

  it("检测缺口并在恢复期拒绝非新世代起点", () => {
    expect(remoteDeliverySequenceDecision(undefined, 1, false)).toBe("accept");
    expect(remoteDeliverySequenceDecision(2, 2, false)).toBe("duplicate");
    expect(remoteDeliverySequenceDecision(2, 4, false)).toBe("recover");
    expect(remoteDeliverySequenceDecision(undefined, 4, true)).toBe("stale");
    expect(remoteDeliverySequenceDecision(undefined, 1, true)).toBe("accept");
  });

  it("gap 只恢复消息绑定的当前 Agent Session", () => {
    const gap = {
      type: "gap",
      sessionId: "session-a",
      snapshotRequired: true,
      recoveryMethod: "session/load",
    };
    expect(remoteGapRecoverySessionId(gap, "session-a")).toBe("session-a");
    expect(remoteGapRecoverySessionId(gap, "session-b")).toBeNull();
    expect(remoteGapRecoverySessionId({ ...gap, sessionId: "web-auth-cookie" }, "session-a"))
      .toBeNull();
    expect(remoteGapRecoverySessionId({ ...gap, recoveryMethod: "session/replay" }, "session-a"))
      .toBeNull();
  });

  it("保留普通 URL 和相对文本，不做过度脱敏", () => {
    expect(redactRemoteText("查看 https://example.com/docs 和 src/App.tsx"))
      .toBe("查看 https://example.com/docs 和 src/App.tsx");
  });
});
