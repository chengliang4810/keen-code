import { expect, test } from "vitest";
import type { SessionPhase, SessionSummary } from "../../../packages/shared/src/zcode-protocol-v4/index.js";
import { collectTerminalTaskNotificationPayloads } from "../../../packages/ui/src/lib/taskNotificationOrchestrator.js";

const formatMessage: Parameters<typeof collectTerminalTaskNotificationPayloads>[0]["formatMessage"] =
  (descriptor) => String(descriptor.id ?? "");

function summary(phase: SessionPhase): SessionSummary {
  return {
    sessionId: "notification-contract-session",
    workspaceId: "local:notification-contract-workspace",
    title: "通知验收",
    titleSource: "custom",
    createdAt: 1,
    lastActivityAt: 2,
    phase,
    sessionEnded: false,
    hasBackgroundWork: false,
  };
}

test("用户停止真实回合时不发送成功或失败通知", () => {
  const running = summary("running");
  expect(collectTerminalTaskNotificationPayloads({
    previousBySessionId: new Map([[running.sessionId, running]]),
    sessions: [summary("completedInterrupted")],
    formatMessage,
  })).toEqual([]);
});

test("停止之后新回合成功时只发送一次完成通知", () => {
  const stopped = summary("completedInterrupted");
  const completed = summary("completedSuccess");
  const notifications = collectTerminalTaskNotificationPayloads({
    previousBySessionId: new Map([[stopped.sessionId, stopped]]),
    sessions: [completed],
    formatMessage,
  });
  expect(notifications.map(({ taskId, status }) => ({ taskId, status }))).toEqual([
    { taskId: completed.sessionId, status: "completed" },
  ]);
  expect(collectTerminalTaskNotificationPayloads({
    previousBySessionId: new Map([[completed.sessionId, completed]]),
    sessions: [completed],
    formatMessage,
  })).toEqual([]);
});
