import { createElement, type EffectCallback } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { emptySession } from "@/lib/acp/store";
import type {
  ComposerApiPort,
  ComposerFeedbackPort,
  ComposerGoalGetResult,
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
});
