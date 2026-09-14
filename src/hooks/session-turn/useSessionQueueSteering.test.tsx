import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { GoalRecordDto } from "@/lib/acp/events";
import {
  createAcpWorkspaceState,
  emptySession,
  reduceGoalSnapshot,
} from "@/lib/acp/store";
import type { QueuedSend } from "@/lib/sendQueue";
import type { SessionTurnApiPort } from "./types";
import { useSessionQueueSteering } from "./useSessionQueueSteering";

interface Deferred<Value> {
  promise: Promise<Value>;
  resolve: (value: Value | PromiseLike<Value>) => void;
}

function deferred<Value>(): Deferred<Value> {
  let resolve!: (value: Value | PromiseLike<Value>) => void;
  const promise = new Promise<Value>((accept) => {
    resolve = accept;
  });
  return { promise, resolve };
}

function goalRecord(id: string, objective: string): GoalRecordDto {
  return {
    id,
    title: objective,
    scope: "session",
    status: "active",
    objective,
    tokensUsed: 0,
    timeUsedSeconds: 0,
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

function renderSteering(options: Parameters<typeof useSessionQueueSteering>[0]) {
  let captured!: ReturnType<typeof useSessionQueueSteering>;
  function Harness() {
    captured = useSessionQueueSteering(options);
    return null;
  }
  renderToString(createElement(Harness));
  return captured;
}

describe("useSessionQueueSteering 的 Goal revision 竞态", () => {
  it("较新的 Goal 响应先归约时，迟到的旧 upsert 不得覆盖投影", async () => {
    const sessionId = "queue-goal-race";
    const workspace = createAcpWorkspaceState();
    const view = emptySession(sessionId);
    workspace.sessions[sessionId] = view;
    const staleGoal = goalRecord("goal-stale", "队列中的旧目标");
    const newerGoal = goalRecord("goal-new", "并发更新后的目标");
    const oldUpsert = deferred<Awaited<ReturnType<SessionTurnApiPort["goalUpsert"]>>>();
    const goalUpsert = vi.fn(() => oldUpsert.promise);
    const steer = vi.fn().mockResolvedValue(undefined);
    const commitWorkspace = vi.fn();
    const showToast = vi.fn();
    const options = {
      tr: (key: string) => key,
      sessionId,
      sessionState: "streaming" as const,
      api: { goalUpsert, steer } as unknown as SessionTurnApiPort,
      runtime: {
        acpWorkspaceRef: { current: workspace },
        commitWorkspace,
      },
      showToast,
    };
    const steerQueuedItem = renderSteering(options);
    const item: QueuedSend = {
      id: "queued-goal-race",
      storedDisplay: "队列消息",
      attachments: [],
      createGoal: true,
      planMode: false,
      ultraMode: false,
      createdAt: 1,
    };

    const pending = steerQueuedItem(item);
    reduceGoalSnapshot(view, 3, newerGoal);
    oldUpsert.resolve({
      revision: 2,
      goal: staleGoal,
      deduplicated: false,
    });
    await pending;

    expect(view.goal).toEqual({ revision: 3, goal: newerGoal });
    expect(commitWorkspace).not.toHaveBeenCalled();
    expect(goalUpsert).toHaveBeenCalledWith({
      sessionId,
      goal: { title: "队列消息", objective: "队列消息" },
      expectedRevision: 0,
      requestNonce: "queued-goal-race-goal",
    });
    expect(steer).toHaveBeenCalledOnce();
    expect(showToast).toHaveBeenCalledOnce();
  });
});
