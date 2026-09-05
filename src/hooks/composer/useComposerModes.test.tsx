import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { AppDialog } from "@/features/app/models";
import type { GoalRecordDto } from "@/lib/acp/events";
import { ensureAcpSession } from "@/lib/acp/projection";
import {
  createAcpWorkspaceState,
  reduceGoalSnapshot,
} from "@/lib/acp/store";
import type {
  ComposerApiPort,
  ComposerGoalTransitionResult,
  ComposerFeedbackPort,
  ComposerSessionPort,
  ComposerWorkspacePort,
} from "../useComposerController";
import {
  canApplyGoalListResult,
  type ComposerModesController,
  isGoalTransitionPending,
  makeGoalTransitionKey,
  useComposerModes,
} from "./useComposerModes";

/** 创建状态转换控制器测试使用的最小 Goal 记录。 */
function makeGoal(status: GoalRecordDto["status"] = "active"): GoalRecordDto {
  return {
    id: "goal-1",
    title: "测试目标",
    objective: "测试目标",
    scope: "project",
    status,
    created_at: "",
    updated_at: "",
    tokens_used: 0,
    time_used_seconds: 0,
  };
}

/** 创建可在 SSR 合法渲染上下文中捕获的 Goal controller。 */
function renderGoalController(options?: {
  list?: ComposerApiPort["goals"]["list"];
  clear?: ComposerApiPort["goals"]["clear"];
  upsert?: ComposerApiPort["goals"]["upsert"];
  transition?: ComposerApiPort["goals"]["transition"];
  status?: GoalRecordDto["status"];
}) {
  const workspace = createAcpWorkspaceState();
  const view = ensureAcpSession(workspace, "session-1");
  view.goal = { revision: 7, goal: makeGoal(options?.status) };
  /** 统一包装状态转换 mock，保留 Vitest 的调用断言类型。 */
  const transition = vi.fn<ComposerApiPort["goals"]["transition"]>(
    options?.transition ??
      (async (args: { status: GoalRecordDto["status"] }) => ({
        revision: 8,
        goal: makeGoal(args.status),
      })),
  );
  let dialog: AppDialog = null;
  const setAppDialog = vi.fn((next: AppDialog) => {
    dialog = next;
  });
  const showToast = vi.fn();
  const applyViewProjection = vi.fn();
  const feedback: ComposerFeedbackPort = {
    showToast,
    setLocalError: vi.fn(),
    setAppDialog,
  };
  const list = vi.fn<ComposerApiPort["goals"]["list"]>(
    options?.list ??
      (async () => ({ revision: 7, goals: [makeGoal()] })),
  );
  const clear = vi.fn<ComposerApiPort["goals"]["clear"]>(
    options?.clear ??
      (async () => ({
        sessionId: "session-1",
        revision: 8,
        cleared: true,
      })),
  );
  const upsert = vi.fn<ComposerApiPort["goals"]["upsert"]>(
    options?.upsert ??
      (async () => ({ revision: 8, goal: makeGoal() })),
  );
  const api: ComposerApiPort = {
    isTauri: () => true,
    attachments: {
      pickFiles: vi.fn().mockResolvedValue([]),
      savePastedFile: vi.fn().mockResolvedValue(""),
      classifyPaths: vi.fn().mockResolvedValue([]),
    },
    skillsList: vi.fn().mockResolvedValue({ skills: [] }),
    goals: {
      list,
      clear,
      upsert,
      transition,
    },
  };
  const session: ComposerSessionPort = {
    sessionId: "session-1",
    state: "ready",
    activeProject: null,
    messages: [],
    acpSessionView: view,
    contextUsage: null,
    modelId: "model-1",
  };
  const workspacePort: ComposerWorkspacePort = {
    acpWorkspaceRef: { current: workspace },
    commitWorkspace: vi.fn(),
    applyViewProjectionRef: { current: applyViewProjection },
  };
  let controller!: ComposerModesController;

  /** 在合法 React 渲染上下文中捕获 Goal controller。 */
  function Harness() {
    controller = useComposerModes({
      locale: "zh",
      session,
      api,
      workspace: workspacePort,
      feedback,
    });
    return null;
  }

  renderToString(createElement(Harness));
  return {
    controller,
    view,
    list,
    clear,
    upsert,
    transition,
    dialog: () => dialog,
    showToast,
    workspacePort,
  };
}

describe("useComposerModes Goal 状态转换", () => {
  it("完成 active Goal 时传递目标标识、修订号和 requestNonce，并更新唯一投影", async () => {
    const fixture = renderGoalController();

    fixture.controller.completeCurrentGoal();
    const dialog = fixture.dialog();
    expect(dialog?.kind).toBe("confirm");
    if (!dialog || dialog.kind !== "confirm") return;
    expect(dialog.message).toContain("完成后不能恢复");
    await dialog.onConfirm();

    expect(fixture.transition).toHaveBeenCalledWith(
      expect.objectContaining({
        sessionId: "session-1",
        goalId: "goal-1",
        status: "completed",
        expectedRevision: 7,
        requestNonce: expect.stringMatching(/^keencode-goal-\d+-/),
      }),
    );
    expect(fixture.view.goal.goal?.status).toBe("completed");
    expect(fixture.view.goal.revision).toBe(8);
    expect(fixture.workspacePort.commitWorkspace).toHaveBeenCalledTimes(1);
    expect(fixture.workspacePort.applyViewProjectionRef.current).toHaveBeenCalledWith(
      "session-1",
    );
  });

  it("阻塞必须填写非空原因，成功时传递 trimmed reason", async () => {
    const fixture = renderGoalController();

    fixture.controller.blockCurrentGoal();
    const firstDialog = fixture.dialog();
    expect(firstDialog?.kind).toBe("prompt");
    if (!firstDialog || firstDialog.kind !== "prompt") return;

    await firstDialog.onSubmit("   ");
    expect(fixture.transition).not.toHaveBeenCalled();
    const requiredDialog = fixture.dialog();
    expect(requiredDialog?.kind).toBe("prompt");
    expect(requiredDialog?.kind === "prompt" ? requiredDialog.message : "").toContain(
      "非空原因",
    );

    if (!requiredDialog || requiredDialog.kind !== "prompt") return;
    await requiredDialog.onSubmit("  缺少依赖  ");
    expect(fixture.transition).toHaveBeenCalledWith(
      expect.objectContaining({
        sessionId: "session-1",
        goalId: "goal-1",
        status: "blocked",
        reason: "缺少依赖",
        expectedRevision: 7,
        requestNonce: expect.stringMatching(/^keencode-goal-\d+-/),
      }),
    );
    expect(fixture.view.goal.goal?.status).toBe("blocked");
  });

  it("状态转换失败后重新查询并对账，同时显示错误 toast", async () => {
    const transition = vi
      .fn()
      .mockRejectedValue(new Error("revision conflict"));
    const list = vi
      .fn<ComposerApiPort["goals"]["list"]>()
      .mockResolvedValue({ revision: 8, goals: [makeGoal("completed")] });
    const fixture = renderGoalController({ transition, list });

    fixture.controller.completeCurrentGoal();
    const dialog = fixture.dialog();
    expect(dialog?.kind).toBe("confirm");
    if (!dialog || dialog.kind !== "confirm") return;
    await dialog.onConfirm();

    expect(list).toHaveBeenCalledWith("session-1");
    expect(fixture.view.goal.goal?.status).toBe("completed");
    expect(fixture.view.goal.revision).toBe(8);
    expect(fixture.showToast).toHaveBeenCalledWith(
      expect.stringContaining("标记目标完成失败"),
      4000,
    );
    expect(fixture.workspacePort.commitWorkspace).toHaveBeenCalledTimes(1);
  });

  it("清除 Goal 时传递目标身份、修订号和 requestNonce", async () => {
    const fixture = renderGoalController();

    fixture.controller.confirmClearCurrentGoal();
    const dialog = fixture.dialog();
    expect(dialog?.kind).toBe("confirm");
    if (!dialog || dialog.kind !== "confirm") return;
    await dialog.onConfirm();

    expect(fixture.clear).toHaveBeenCalledWith({
      sessionId: "session-1",
      goalId: "goal-1",
      expectedRevision: 7,
      requestNonce: expect.stringMatching(/^keencode-goal-\d+-/),
    });
    expect(fixture.view.goal.goal).toBeNull();
    expect(fixture.view.goal.revision).toBe(8);
  });

  it("completed 或 blocked 终态不会再次发起状态转换", async () => {
    const fixture = renderGoalController({ status: "completed" });

    await fixture.controller.completeCurrentGoal();
    fixture.controller.blockCurrentGoal();

    expect(fixture.transition).not.toHaveBeenCalled();
    expect(fixture.dialog()).toBeNull();
  });

  it("状态转换 pending 时会阻止重复提交", async () => {
    let resolveTransition!: (result: ComposerGoalTransitionResult) => void;
    const transition = vi.fn<ComposerApiPort["goals"]["transition"]>(
      () =>
        new Promise((resolve) => {
          resolveTransition = resolve;
        }),
    );
    const fixture = renderGoalController({ transition });

    fixture.controller.completeCurrentGoal();
    const dialog = fixture.dialog();
    expect(dialog?.kind).toBe("confirm");
    if (!dialog || dialog.kind !== "confirm") return;

    const first = dialog.onConfirm();
    const second = dialog.onConfirm();
    expect(fixture.transition).toHaveBeenCalledTimes(1);

    resolveTransition({ revision: 8, goal: makeGoal("completed") });
    await Promise.all([first, second]);
  });

  it("迟到的状态转换结果不会为已清除的 Goal 创建幽灵投影", async () => {
    let resolveTransition!: (result: ComposerGoalTransitionResult) => void;
    const transition = vi.fn<ComposerApiPort["goals"]["transition"]>(
      () =>
        new Promise((resolve) => {
          resolveTransition = resolve;
        }),
    );
    const fixture = renderGoalController({ transition });

    fixture.controller.completeCurrentGoal();
    const dialog = fixture.dialog();
    expect(dialog?.kind).toBe("confirm");
    if (!dialog || dialog.kind !== "confirm") return;
    const pending = dialog.onConfirm();

    fixture.view.goal = { revision: 0, goal: null };
    resolveTransition({ revision: 8, goal: makeGoal("completed") });
    await pending;

    expect(fixture.view.goal.goal).toBeNull();
    expect(fixture.workspacePort.commitWorkspace).not.toHaveBeenCalled();
  });

  it("当前 Goal 已被其他 mutation 更新时拒绝迟到的同 id transition 回包", async () => {
    let resolveTransition!: (result: ComposerGoalTransitionResult) => void;
    const transition = vi.fn<ComposerApiPort["goals"]["transition"]>(
      () =>
        new Promise((resolve) => {
          resolveTransition = resolve;
        }),
    );
    const fixture = renderGoalController({ transition });

    fixture.controller.completeCurrentGoal();
    const dialog = fixture.dialog();
    expect(dialog?.kind).toBe("confirm");
    if (!dialog || dialog.kind !== "confirm") return;
    const pending = dialog.onConfirm();

    fixture.view.goal = { revision: 8, goal: makeGoal("active") };
    resolveTransition({ revision: 9, goal: makeGoal("completed") });
    await pending;

    expect(fixture.view.goal.goal?.status).toBe("active");
    expect(fixture.view.goal.revision).toBe(8);
    expect(fixture.workspacePort.commitWorkspace).not.toHaveBeenCalled();
  });

  it("transition 与迟到 goals.list 响应交错时保留新投影", () => {
    const fixture = renderGoalController();
    const request = {
      sessionId: "session-1",
      requestSequence: 1,
      mutationEpoch: 0,
      view: fixture.view,
      projection: fixture.view.goal,
    };
    expect(canApplyGoalListResult(request, 1, 0, fixture.view, 7)).toBe(true);
    fixture.view.goal = { revision: 8, goal: makeGoal("completed") };

    expect(canApplyGoalListResult(request, 2, 1, fixture.view, 7)).toBe(
      false,
    );
    expect(canApplyGoalListResult(request, 1, 0, fixture.view, 7)).toBe(
      false,
    );
    expect(canApplyGoalListResult(request, 1, 1, fixture.view, 7)).toBe(
      false,
    );
    reduceGoalSnapshot(
      fixture.view,
      7,
      [makeGoal("active")],
      request.projection,
    );
    expect(fixture.view.goal.goal?.status).toBe("completed");
  });

  it("请求时缺失的 Session 或 Goal 投影不会接受创建后的迟到列表响应", () => {
    const fixture = renderGoalController();
    const request = {
      sessionId: "session-1",
      requestSequence: 1,
      mutationEpoch: 0,
      view: null,
      projection: null,
    };

    expect(canApplyGoalListResult(request, 1, 0, fixture.view, 7)).toBe(
      false,
    );
    expect(canApplyGoalListResult(request, 1, 0, undefined, 7)).toBe(false);
    expect(
      canApplyGoalListResult(
        {
          ...request,
          view: fixture.view,
        },
        1,
        0,
        fixture.view,
        7,
      ),
    ).toBe(false);
  });

  it("Goal transition pending 按 Session 与 Goal 隔离", () => {
    const pendingKeys = new Set([
      makeGoalTransitionKey("session-a", "goal-1"),
    ]);

    expect(isGoalTransitionPending(pendingKeys, "session-a", "goal-1")).toBe(
      true,
    );
    expect(isGoalTransitionPending(pendingKeys, "session-b", "goal-1")).toBe(
      false,
    );
    expect(isGoalTransitionPending(pendingKeys, "session-a", "goal-2")).toBe(
      false,
    );
  });
});
