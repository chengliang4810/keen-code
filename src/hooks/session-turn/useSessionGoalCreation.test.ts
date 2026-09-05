import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { createT } from "@/i18n";
import { getGoalMutationEpoch } from "@/lib/acp/goalSync";
import type { GoalRecordDto } from "@/lib/acp/events";
import { ensureAcpSession } from "@/lib/acp/projection";
import { createAcpWorkspaceState } from "@/lib/acp/store";
import { IDLE_SNAPSHOT } from "@/lib/session";
import type { QueuedSend } from "@/lib/sendQueue";
import type { SessionTurnApiPort, SessionTurnRuntimePort } from "./types";
import { useSessionQueueSteering } from "./useSessionQueueSteering";
import { useSessionSend, type UseSessionSendOptions } from "./useSessionSend";

/** 创建调用链测试使用的最小 Goal DTO。 */
function makeGoal(id: string, title: string): GoalRecordDto {
  return {
    id,
    title,
    objective: title,
    scope: "project",
    status: "active",
    created_at: "",
    updated_at: "",
    tokens_used: 0,
    time_used_seconds: 0,
  };
}

/** 构造首条消息创建 Goal 所需的最小运行时端口。 */
function makeSendFixture(sessionId: string) {
  const workspace = createAcpWorkspaceState();
  const upsert = vi.fn<SessionTurnApiPort["goalUpsert"]>(async (args) => ({
    revision: (args.expectedRevision ?? 0) + 1,
    goal: makeGoal("created-by-send", args.goal.title),
    deduplicated: false,
  }));
  const send = vi.fn<SessionTurnApiPort["send"]>(async (args) => ({
    state: "streaming",
    activeTurnId: args.requestId,
    backend: "peri_acp",
    acceptedAtMs: Date.now(),
  }));
  const runtime = {
    acpWorkspaceRef: { current: workspace },
    liveHostRef: { current: { ...IDLE_SNAPSHOT } },
    messagesBySessionRef: { current: new Map() },
    viewingSessionIdRef: { current: null },
    applyViewProjectionRef: { current: vi.fn() },
    commitWorkspace: vi.fn(),
    patchSessionMessages: vi.fn(),
    currentViewFocus: () => ({ sessionId: null, epoch: 0 }),
    replayHistory: vi.fn(),
    refreshSessions: vi.fn(),
    applyMessagePrefixTitle: vi.fn(),
    applyAutomaticSessionTitle: vi.fn(),
    updateSessionPreference: vi.fn(),
  } as unknown as SessionTurnRuntimePort;
  const ui = {
    setSession: vi.fn(),
    setMessages: vi.fn(),
    setLiveHost: vi.fn(),
    setLiveMap: vi.fn(),
    setRetryStatus: vi.fn(),
    setTurnStartedAt: vi.fn(),
    setLocalError: vi.fn(),
    setPlanModeSessionKey: vi.fn(),
    setUltraModeSessionKey: vi.fn(),
  } as unknown as UseSessionSendOptions["ui"];
  const state = {
    sendInFlightRef: { current: false },
    turnLatencyBySessionRef: { current: new Map() },
    activeTurnIdBySessionRef: { current: new Map() },
    recoverableCompletedTurnIdBySessionRef: { current: new Map() },
    pendingVisibleTurnBySessionRef: { current: new Map() },
  } as unknown as UseSessionSendOptions["state"];
  const sendQueue = {
    releaseFlushHold: vi.fn(),
    bindDraft: vi.fn(),
  } as unknown as UseSessionSendOptions["sendQueue"];
  const api = { goalUpsert: upsert, send } as unknown as SessionTurnApiPort;
  const options: UseSessionSendOptions = {
    locale: "zh",
    tr: createT("zh"),
    sessionId: null,
    modelLabel: "model-1",
    hasConfiguredModel: true,
    api,
    runtime,
    ui,
    state,
    ensureConnected: vi.fn(async () => sessionId),
    sendQueue,
  };
  return { workspace, upsert, options };
}

/** 创建可传给队列 steering 的固定队列条目。 */
function makeQueuedGoal(storedDisplay: string): QueuedSend {
  return {
    id: "queue-goal-1",
    storedDisplay,
    attachments: [],
    createGoal: true,
    planMode: false,
    ultraMode: false,
    createdAt: 1,
  };
}

describe("Goal 创建调用链", () => {
  it("首条消息创建 Goal 时按 ensure 后的 revision 与 nonce 提交", async () => {
    const sessionId = "goal-create-send-session";
    const fixture = makeSendFixture(sessionId);
    const beforeEpoch = getGoalMutationEpoch(sessionId);
    fixture.upsert.mockImplementationOnce(async (args) => {
      expect(getGoalMutationEpoch(sessionId)).toBe(beforeEpoch + 1);
      return {
        revision: (args.expectedRevision ?? 0) + 1,
        goal: makeGoal("created-by-send", args.goal.title),
        deduplicated: false,
      };
    });
    let executeSend!: ReturnType<typeof useSessionSend>;
    function Harness() {
      executeSend = useSessionSend(fixture.options);
      return null;
    }
    renderToString(createElement(Harness));

    await expect(
      executeSend({
        storedDisplay: "首条目标",
        att: [],
        createGoal: true,
        targetSessionId: null,
      }),
    ).resolves.toBe(true);

    expect(fixture.upsert).toHaveBeenCalledWith({
      sessionId,
      goal: { title: "首条目标", description: "首条目标" },
      expectedRevision: 0,
      requestNonce: expect.stringMatching(/^keencode-goal-\d+-/),
    });
    expect(fixture.workspace.sessions[sessionId]?.goal.revision).toBe(1);
  });

  it("队列 steering 创建 Goal 时保留当前投影 revision 并推进共享纪元", async () => {
    const sessionId = "goal-create-queue-session";
    const workspace = createAcpWorkspaceState();
    const view = ensureAcpSession(workspace, sessionId);
    view.goal = { revision: 5, goal: makeGoal("existing-goal", "旧目标") };
    const beforeEpoch = getGoalMutationEpoch(sessionId);
    const upsert = vi.fn<SessionTurnApiPort["goalUpsert"]>(async (args) => {
      expect(getGoalMutationEpoch(sessionId)).toBe(beforeEpoch + 1);
      return {
        revision: 6,
        goal: makeGoal("created-by-queue", args.goal.title),
        deduplicated: false,
      };
    });
    const steer = vi.fn<SessionTurnApiPort["steer"]>().mockResolvedValue();
    const api = { goalUpsert: upsert, steer } as unknown as SessionTurnApiPort;
    const runtime = {
      acpWorkspaceRef: { current: workspace },
      commitWorkspace: vi.fn(),
    };
    let steerQueued!: ReturnType<typeof useSessionQueueSteering>;
    function Harness() {
      steerQueued = useSessionQueueSteering({
        tr: createT("zh"),
        sessionId,
        sessionState: "streaming",
        api,
        runtime,
        showToast: vi.fn(),
      });
      return null;
    }
    renderToString(createElement(Harness));

    await steerQueued(makeQueuedGoal("队列目标"));

    expect(upsert).toHaveBeenCalledWith({
      sessionId,
      goal: { title: "队列目标", description: "队列目标" },
      expectedRevision: 5,
      requestNonce: expect.stringMatching(/^keencode-goal-\d+-/),
    });
    expect(steer).toHaveBeenCalledWith({
      sessionId,
      text: "队列目标",
    });
    expect(view.goal?.goal?.title).toBe("队列目标");
    expect(view.goal?.goal?.id).toBe("created-by-queue");
  });
});
