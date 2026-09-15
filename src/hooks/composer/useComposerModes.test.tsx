import { createElement, type EffectCallback } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppDialog } from "@/features/app/models";
import type { GoalRecordDto } from "@/lib/acp/events";
import { emptySession, reduceGoalSnapshot } from "@/lib/acp/store";
import type {
  ComposerApiPort,
  ComposerFeedbackPort,
  ComposerGoalGetResult,
  ComposerGoalUpsertResult,
  ComposerSessionPort,
  ComposerWorkspacePort,
} from "../useComposerController";
import { useComposerModes } from "./useComposerModes";

const effects = vi.hoisted(() => [] as EffectCallback[]);

vi.mock("react", async (original) => ({
  ...await original<typeof import("react")>(),
  useEffect: (effect: EffectCallback) => {
    effects.push(effect);
  },
}));

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

async function flushMicrotasks(rounds = 4): Promise<void> {
  for (let index = 0; index < rounds; index += 1) await Promise.resolve();
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

describe("useComposerModes 的 Goal 查询竞态", () => {
  beforeEach(() => {
    effects.length = 0;
  });

  it("较新的响应先到达时，迟到的较低 revision 不得覆盖投影", async () => {
    const sessionId = "composer-goal-race";
    const view = emptySession(sessionId);
    const older = deferred<ComposerGoalGetResult>();
    const newer = deferred<ComposerGoalGetResult>();
    const goalGet = vi.fn()
      .mockReturnValueOnce(older.promise)
      .mockReturnValueOnce(newer.promise);
    const workspace = {
      acpWorkspaceRef: { current: { sessions: { [sessionId]: view } } },
      commitWorkspace: vi.fn(),
      applyViewProjectionRef: { current: vi.fn() },
    } as unknown as ComposerWorkspacePort;
    const api = {
      isTauri: () => true,
      goals: { get: goalGet },
    } as unknown as ComposerApiPort;
    const session = { sessionId, acpSessionView: view } as ComposerSessionPort;

    function Harness() {
      useComposerModes({
        locale: "zh",
        session,
        api,
        workspace,
        feedback: {} as ComposerFeedbackPort,
      });
      return null;
    }

    renderToString(createElement(Harness));
    expect(effects).toHaveLength(1);
    effects[0]!();
    effects[0]!();
    expect(goalGet).toHaveBeenCalledTimes(2);

    newer.resolve({ revision: 7 });
    await flushMicrotasks();
    expect(view.goal.revision).toBe(7);

    older.resolve({ revision: 6 });
    await flushMicrotasks();
    expect(view.goal.revision).toBe(7);
  });

  it("较新的 Goal 已先写入时，迟到的旧 clear 响应不得清空投影", async () => {
    const sessionId = "composer-goal-clear-race";
    const view = emptySession(sessionId);
    const currentGoal = goalRecord("goal-1", "当前目标");
    const newerGoal = goalRecord("goal-2", "并发更新后的目标");
    view.goal = { revision: 1, goal: currentGoal };
    const clear = deferred<{
      sessionId: string;
      revision: number;
      clearedGoalId: string;
      deduplicated: boolean;
    }>();
    let dialog: AppDialog = null;
    const setAppDialog = vi.fn((next: AppDialog) => {
      dialog = next;
    });
    const commitWorkspace = vi.fn();
    const workspace = {
      acpWorkspaceRef: { current: { sessions: { [sessionId]: view } } },
      commitWorkspace,
      applyViewProjectionRef: { current: vi.fn() },
    } as unknown as ComposerWorkspacePort;
    const api = {
      isTauri: () => false,
      goals: {
        get: vi.fn().mockResolvedValue({ revision: 1, goal: currentGoal }),
        clear: vi.fn(() => clear.promise),
      },
    } as unknown as ComposerApiPort;
    const session = { sessionId, acpSessionView: view } as ComposerSessionPort;

    let controller!: ReturnType<typeof useComposerModes>;
    function ControllerHarness() {
      controller = useComposerModes({
        locale: "zh",
        session,
        api,
        workspace,
        feedback: { setAppDialog } as unknown as ComposerFeedbackPort,
      });
      return null;
    }
    renderToString(createElement(ControllerHarness));
    controller.confirmClearCurrentGoal();
    const clearDialog = dialog as unknown as Extract<AppDialog, { kind: "confirm" }>;
    expect(clearDialog?.kind).toBe("confirm");

    const pending = clearDialog.onConfirm();
    reduceGoalSnapshot(view, 3, newerGoal);
    clear.resolve({
      sessionId,
      revision: 2,
      clearedGoalId: currentGoal.id,
      deduplicated: false,
    });
    await pending;

    expect(view.goal).toEqual({ revision: 3, goal: newerGoal });
    expect(commitWorkspace).not.toHaveBeenCalled();
  });

  it("清除 Goal 前使用权威 revision，不使用过期前端投影", async () => {
    const sessionId = "composer-goal-stale-revision";
    const view = emptySession(sessionId);
    const currentGoal = goalRecord("goal-1", "当前目标");
    view.goal = { revision: 1, goal: currentGoal };
    let dialog: AppDialog = null;
    const setAppDialog = vi.fn((next: AppDialog) => {
      dialog = next;
    });
    const clear = vi.fn().mockResolvedValue({
      sessionId,
      revision: 8,
      clearedGoalId: currentGoal.id,
      deduplicated: false,
    });
    const workspace = {
      acpWorkspaceRef: { current: { sessions: { [sessionId]: view } } },
      commitWorkspace: vi.fn(),
      applyViewProjectionRef: { current: vi.fn() },
    } as unknown as ComposerWorkspacePort;
    const api = {
      isTauri: () => false,
      goals: {
        get: vi.fn().mockResolvedValue({ revision: 7, goal: currentGoal }),
        clear,
      },
    } as unknown as ComposerApiPort;
    const session = { sessionId, acpSessionView: view } as ComposerSessionPort;
    let controller!: ReturnType<typeof useComposerModes>;
    function Harness() {
      controller = useComposerModes({
        locale: "zh",
        session,
        api,
        workspace,
        feedback: { setAppDialog } as unknown as ComposerFeedbackPort,
      });
      return null;
    }

    renderToString(createElement(Harness));
    controller.confirmClearCurrentGoal();
    const clearDialog = dialog as unknown as Extract<AppDialog, { kind: "confirm" }>;
    await clearDialog.onConfirm();

    expect(clear).toHaveBeenCalledWith(expect.objectContaining({ expectedRevision: 7 }));
    expect(view.goal).toEqual({ revision: 8, goal: null });
  });

  it("较新的 Goal 已先写入时，迟到的旧 edit 响应不得覆盖投影", async () => {
    const sessionId = "composer-goal-edit-race";
    const view = emptySession(sessionId);
    const currentGoal = goalRecord("goal-1", "当前目标");
    const newerGoal = goalRecord("goal-3", "并发更新后的目标");
    const staleEditedGoal = goalRecord("goal-2", "迟到的编辑目标");
    view.goal = { revision: 1, goal: currentGoal };
    const upsert = deferred<ComposerGoalUpsertResult>();
    let dialog: AppDialog = null;
    const setAppDialog = vi.fn((next: AppDialog) => {
      dialog = next;
    });
    const commitWorkspace = vi.fn();
    const workspace = {
      acpWorkspaceRef: { current: { sessions: { [sessionId]: view } } },
      commitWorkspace,
      applyViewProjectionRef: { current: vi.fn() },
    } as unknown as ComposerWorkspacePort;
    const api = {
      isTauri: () => false,
      goals: {
        upsert: vi.fn(() => upsert.promise),
      },
    } as unknown as ComposerApiPort;
    const session = { sessionId, acpSessionView: view } as ComposerSessionPort;
    let controller!: ReturnType<typeof useComposerModes>;
    function Harness() {
      controller = useComposerModes({
        locale: "zh",
        session,
        api,
        workspace,
        feedback: { setAppDialog } as unknown as ComposerFeedbackPort,
      });
      return null;
    }

    renderToString(createElement(Harness));
    controller.editCurrentGoal();
    const editDialog = dialog as unknown as Extract<AppDialog, { kind: "prompt" }>;
    expect(editDialog?.kind).toBe("prompt");

    const pending = editDialog.onSubmit("迟到的编辑目标");
    reduceGoalSnapshot(view, 3, newerGoal);
    upsert.resolve({ revision: 2, goal: staleEditedGoal });
    await pending;

    expect(view.goal).toEqual({ revision: 3, goal: newerGoal });
    expect(commitWorkspace).not.toHaveBeenCalled();
  });
});
