import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createT, type Locale } from "@/i18n";
import type {
  ComposerApiPort,
  ComposerGoalTransitionResult,
  ComposerGoalTransitionStatus,
  ComposerFeedbackPort,
  ComposerSessionPort,
  ComposerWorkspacePort,
  StateSetter,
} from "../useComposerController";
import {
  reduceGoalSnapshot,
  type AcpGoalProjection,
  type AcpSessionView,
} from "@/lib/acp/store";
import {
  createGoalRequestNonce,
  getGoalMutationEpoch,
  invalidateGoalListRequests,
} from "@/lib/acp/goalSync";
import { isGoalToolName } from "@/lib/toolDisplay";

/** Goal 状态转换的唯一键，保证不同 Session 的相同 Goal id 互不影响。 */
export function makeGoalTransitionKey(
  sessionId: string,
  goalId: string,
): string {
  return `${sessionId}\u0000${goalId}`;
}

/** 根据当前 Session/Goal 投影判断对应状态转换是否仍在提交。 */
export function isGoalTransitionPending(
  pendingKeys: ReadonlySet<string>,
  sessionId: string | null,
  goalId: string | null,
): boolean {
  return Boolean(
    sessionId &&
      goalId &&
      pendingKeys.has(makeGoalTransitionKey(sessionId, goalId)),
  );
}

/** goals.list 发起时记录的 Session、请求序号和 Goal 投影身份。 */
export interface GoalListRequestSnapshot {
  /** 请求所属的 Session 标识。 */
  sessionId: string;
  /** 该 Session 发起的单调递增列表请求序号。 */
  requestSequence: number;
  /** 发起请求时的 Goal mutation epoch。 */
  mutationEpoch: number;
  /** 发起请求时的工作区 Session 视图；当时尚未创建时为空。 */
  view: AcpSessionView | null;
  /** 发起请求时的 Goal 投影对象身份；没有视图时为空。 */
  projection: AcpGoalProjection | null;
}

/** 判断迟到的 goals.list 响应是否仍可写入当前投影。 */
export function canApplyGoalListResult(
  request: GoalListRequestSnapshot,
  currentRequestSequence: number | undefined,
  currentMutationEpoch: number,
  currentView: AcpSessionView | undefined,
  responseRevision: number,
): boolean {
  // 请求发起时没有 Session 视图，后续新建的视图不是同一个身份，不能回写。
  if (request.view === null) return false;
  if (
    !currentView ||
    currentView !== request.view ||
    currentView.session_id !== request.sessionId
  ) {
    return false;
  }
  const currentProjection = currentView.goal;
  // 请求发起时没有 Goal 投影，后续创建的投影不是同一个身份，不能回写。
  if (request.projection === null || currentProjection !== request.projection) {
    return false;
  }
  return (
    currentRequestSequence === request.requestSequence &&
    currentMutationEpoch === request.mutationEpoch &&
    currentProjection.revision === request.projection.revision &&
    (currentProjection.goal?.id ?? null) ===
      (request.projection.goal?.id ?? null) &&
    responseRevision >= currentProjection.revision
  );
}

/** 推进一个 Session 维度的同步计数器，并返回新值。 */
function advanceGoalSyncCounter(
  counters: Map<string, number>,
  sessionId: string,
): number {
  const next = (counters.get(sessionId) ?? 0) + 1;
  counters.set(sessionId, next);
  return next;
}

export interface ComposerModesController {
  goalModeSessionKey: string | null;
  setGoalModeSessionKey: StateSetter<string | null>;
  planModeSessionKey: string | null;
  setPlanModeSessionKey: StateSetter<string | null>;
  ultraModeSessionKey: string | null;
  setUltraModeSessionKey: StateSetter<string | null>;
  showStatusModal: boolean;
  setShowStatusModal: StateSetter<boolean>;
  goalToolCompletionSignature: string;
  /** 当前是否正在提交 Goal 状态转换。 */
  goalTransitionPending: boolean;
  activateGoalMode: (sessionKey: string) => void;
  togglePlanMode: (sessionKey: string) => void;
  confirmClearCurrentGoal: () => void;
  editCurrentGoal: () => void;
  /** 打开确认弹窗并将当前 active Goal 标记为完成。 */
  completeCurrentGoal: () => void;
  /** 打开阻塞原因输入，并将当前 active Goal 标记为阻塞。 */
  blockCurrentGoal: () => void;
}

export interface UseComposerModesOptions {
  locale: Locale;
  session: ComposerSessionPort;
  api: ComposerApiPort;
  workspace: ComposerWorkspacePort;
  feedback: ComposerFeedbackPort;
}

/** Owns session mode keys and goal synchronization/dialog actions. */
export function useComposerModes({
  locale,
  session,
  api,
  workspace,
  feedback,
}: UseComposerModesOptions): ComposerModesController {
  const tr = useMemo(() => createT(locale), [locale]);
  const portsRef = useRef({ api, session, workspace, feedback });
  portsRef.current = { api, session, workspace, feedback };
  const [goalModeSessionKey, setGoalModeSessionKey] = useState<string | null>(
    null,
  );
  const [planModeSessionKey, setPlanModeSessionKey] = useState<string | null>(
    null,
  );
  const [ultraModeSessionKey, setUltraModeSessionKey] = useState<string | null>(
    null,
  );
  const [showStatusModal, setShowStatusModal] = useState(false);
  /** 按 Session/Goal 保存正在提交的状态转换，避免跨会话互相阻塞。 */
  const [goalTransitionPendingKeys, setGoalTransitionPendingKeys] = useState<
    Set<string>
  >(() => new Set());
  /** 同步读取 pending 集合，保证异步回调和重复点击检查使用最新值。 */
  const goalTransitionPendingKeysRef = useRef(new Set<string>());
  /** 按 Session 递增 goals.list 请求序号，拒绝较早的列表响应。 */
  const goalListRequestSequenceRef = useRef(new Map<string, number>());
  /** 使指定 Session 的所有在途 Goal 列表响应失效。 */
  const invalidateGoalListResponses = useCallback(
    (sessionId: string): void => {
      invalidateGoalListRequests(sessionId);
      // 同时推进序号，让状态转换开始/结束前已发出的列表请求全部过期。
      advanceGoalSyncCounter(goalListRequestSequenceRef.current, sessionId);
    },
    [],
  );

  const activateGoalMode = useCallback((sessionKey: string) => {
    if (!portsRef.current.session.acpSessionView?.goal.goal) {
      setGoalModeSessionKey(sessionKey);
    }
  }, []);
  const togglePlanMode = useCallback((sessionKey: string) => {
    setPlanModeSessionKey((previous) =>
      previous === sessionKey ? null : sessionKey,
    );
  }, []);

  const goalToolCompletionSignature = useMemo(() => {
    const view = session.acpSessionView;
    if (!view || view.session_id !== session.sessionId) return "";
    return view.live_segments
      .filter(
        (segment) =>
          segment.kind === "tool" &&
          !segment.streaming &&
          segment.status === "completed" &&
          isGoalToolName(segment.toolKind, segment.title),
      )
      .map((segment) => (segment.kind === "tool" ? segment.toolCallId : ""))
      .join("|");
  }, [session.acpSessionView, session.sessionId]);

  /** 查询并安全写回指定 Session 的 Goal 快照，供初始化与 mutation 失败对账复用。 */
  const refreshGoalSnapshot = useCallback(
    async (sessionId: string): Promise<void> => {
      const currentPorts = portsRef.current;
      if (!currentPorts.api.isTauri()) return;
      const view =
        currentPorts.workspace.acpWorkspaceRef.current.sessions[sessionId];
      const request: GoalListRequestSnapshot = {
        sessionId,
        requestSequence: advanceGoalSyncCounter(
          goalListRequestSequenceRef.current,
          sessionId,
        ),
        mutationEpoch: getGoalMutationEpoch(sessionId),
        view: view ?? null,
        projection: view?.goal ?? null,
      };
      try {
        const result = await currentPorts.api.goals.list(sessionId);
        const currentView =
          portsRef.current.workspace.acpWorkspaceRef.current.sessions[sessionId];
        if (!currentView) return;
        if (
          !canApplyGoalListResult(
            request,
            goalListRequestSequenceRef.current.get(sessionId),
            getGoalMutationEpoch(sessionId),
            currentView,
            result.revision,
          )
        ) {
          return;
        }
        reduceGoalSnapshot(
          currentView,
          result.revision,
          result.goals,
          request.projection ?? undefined,
        );
        portsRef.current.workspace.commitWorkspace();
        portsRef.current.workspace.applyViewProjectionRef.current(sessionId);
      } catch {
        // 对账失败不覆盖当前可见投影；原 mutation 错误由调用方负责提示。
      }
    },
    [],
  );

  useEffect(() => {
    const currentPorts = portsRef.current;
    const sessionId = currentPorts.session.sessionId;
    if (!sessionId) return;
    void refreshGoalSnapshot(sessionId);
  }, [goalToolCompletionSignature, refreshGoalSnapshot, session.sessionId]);

  /** 将服务端返回的 Goal 写回工作区唯一投影，并刷新当前视图。 */
  const applyGoalTransitionResult = useCallback(
    (
      sessionId: string,
      expectedGoalId: string,
      expectedRevision: number,
      expectedProjection: AcpGoalProjection,
      result: ComposerGoalTransitionResult,
    ) => {
      const view =
        portsRef.current.workspace.acpWorkspaceRef.current.sessions[sessionId];
      if (
        !view ||
        view.goal !== expectedProjection ||
        view.goal.revision !== expectedRevision ||
        view.goal.goal?.id !== expectedGoalId ||
        result.goal.id !== expectedGoalId ||
        result.revision <= expectedRevision
      ) {
        return;
      }
      view.goal = { revision: result.revision, goal: result.goal };
      portsRef.current.workspace.commitWorkspace();
      portsRef.current.workspace.applyViewProjectionRef.current(sessionId);
    },
    [],
  );

  /** 提交一个当前 active Goal 的终态转换，失败时保持原投影并提示用户。 */
  const transitionCurrentGoal = useCallback(
    async ({
      sessionId,
      goalId,
      status,
      reason,
      expectedRevision,
      expectedProjection,
    }: {
      sessionId: string;
      goalId: string;
      status: ComposerGoalTransitionStatus;
      reason?: string;
      expectedRevision: number;
      expectedProjection: AcpGoalProjection;
    }) => {
      const transitionKey = makeGoalTransitionKey(sessionId, goalId);
      if (goalTransitionPendingKeysRef.current.has(transitionKey)) return;
      goalTransitionPendingKeysRef.current.add(transitionKey);
      setGoalTransitionPendingKeys((previous) => {
        const next = new Set(previous);
        next.add(transitionKey);
        return next;
      });
      invalidateGoalListResponses(sessionId);
      const currentPorts = portsRef.current;
      try {
        const result = await currentPorts.api.goals.transition({
          sessionId,
          goalId,
          status,
          ...(reason ? { reason } : {}),
          expectedRevision,
          requestNonce: createGoalRequestNonce(),
        });
        applyGoalTransitionResult(
          sessionId,
          goalId,
          expectedRevision,
          expectedProjection,
          result,
        );
      } catch (cause) {
        await refreshGoalSnapshot(sessionId);
        currentPorts.feedback.showToast(
          tr(
            status === "completed"
              ? "goal.completeFailed"
              : "goal.blockFailed",
            { error: String(cause) },
          ),
          4000,
        );
      } finally {
        // 完成时再次推进 epoch，覆盖转换开始后才发出的迟到列表请求。
        invalidateGoalListResponses(sessionId);
        goalTransitionPendingKeysRef.current.delete(transitionKey);
        setGoalTransitionPendingKeys((previous) => {
          if (!previous.has(transitionKey)) return previous;
          const next = new Set(previous);
          next.delete(transitionKey);
          return next;
        });
      }
    },
    [
      applyGoalTransitionResult,
      invalidateGoalListResponses,
      refreshGoalSnapshot,
      tr,
    ],
  );

  const confirmClearCurrentGoal = useCallback(() => {
    const currentPorts = portsRef.current;
    const sessionId = currentPorts.session.sessionId;
    const projection = currentPorts.session.acpSessionView?.goal;
    const goal = projection?.goal;
    if (!sessionId || !projection || !goal) return;
    const clearRequest = {
      sessionId,
      goalId: goal.id,
      expectedRevision: projection.revision,
    };
    currentPorts.feedback.setAppDialog({
      kind: "confirm",
      title: tr("goal.clearTitle"),
      message: tr("goal.clearConfirm", { title: goal.title }),
      confirmLabel: tr("goal.clear"),
      danger: true,
      onConfirm: async () => {
        const transitionKey = makeGoalTransitionKey(sessionId, goal.id);
        if (goalTransitionPendingKeysRef.current.has(transitionKey)) return;
        invalidateGoalListResponses(sessionId);
        try {
          const result = await currentPorts.api.goals.clear({
            ...clearRequest,
            requestNonce: createGoalRequestNonce(),
          });
          const view =
            currentPorts.workspace.acpWorkspaceRef.current.sessions[sessionId];
          if (
            view &&
            view.goal === projection &&
            view.goal.revision === projection.revision &&
            view.goal.goal?.id === goal.id
          ) {
            view.goal = { revision: result.revision, goal: null };
            currentPorts.workspace.commitWorkspace();
          }
        } catch (cause) {
          await refreshGoalSnapshot(sessionId);
          currentPorts.feedback.showToast(
            tr("goal.clearFailed", { error: String(cause) }),
            4000,
          );
        }
      },
    });
  }, [
    invalidateGoalListResponses,
    refreshGoalSnapshot,
    tr,
  ]);

  const editCurrentGoal = useCallback(() => {
    const currentPorts = portsRef.current;
    const sessionId = currentPorts.session.sessionId;
    const projection = currentPorts.session.acpSessionView?.goal;
    const goal = projection?.goal;
    if (!sessionId || !projection || !goal) return;
    currentPorts.feedback.setAppDialog({
      kind: "prompt",
      title: tr("goal.editTitle"),
      initial: goal.objective || goal.title,
      placeholder: tr("goal.editPlaceholder"),
      onSubmit: async (value) => {
        const title = value.trim();
        if (!title || title === goal.objective) return;
        const transitionKey = makeGoalTransitionKey(sessionId, goal.id);
        if (goalTransitionPendingKeysRef.current.has(transitionKey)) return;
        invalidateGoalListResponses(sessionId);
        try {
          const result = await currentPorts.api.goals.upsert({
            sessionId,
            goal: { title, description: title },
            expectedRevision: projection.revision,
            requestNonce: createGoalRequestNonce(),
          });
          const view =
            currentPorts.workspace.acpWorkspaceRef.current.sessions[sessionId];
          if (
            view &&
            view.goal === projection &&
            view.goal.revision === projection.revision &&
            view.goal.goal?.id === goal.id
          ) {
            view.goal = { revision: result.revision, goal: result.goal };
            currentPorts.workspace.commitWorkspace();
          }
        } catch (cause) {
          await refreshGoalSnapshot(sessionId);
          currentPorts.feedback.showToast(
            tr("goal.editFailed", { error: String(cause) }),
            4000,
          );
        }
      },
    });
  }, [
    invalidateGoalListResponses,
    refreshGoalSnapshot,
    tr,
  ]);

  /** 打开完成确认弹窗；completed/blocked 终态不会进入该命令。 */
  const completeCurrentGoal = useCallback(() => {
    const currentPorts = portsRef.current;
    const sessionId = currentPorts.session.sessionId;
    const projection = currentPorts.session.acpSessionView?.goal;
    const goal = projection?.goal;
    if (!sessionId || !projection || !goal || goal.status !== "active") return;
    const transition = {
      sessionId,
      goalId: goal.id,
      expectedRevision: projection.revision,
      expectedProjection: projection,
    };
    currentPorts.feedback.setAppDialog({
      kind: "confirm",
      title: tr("goal.completeTitle"),
      message: tr("goal.completeConfirm", { title: goal.title }),
      confirmLabel: tr("goal.complete"),
      onConfirm: () =>
        transitionCurrentGoal({
          ...transition,
          status: "completed",
        }),
    });
  }, [tr, transitionCurrentGoal]);

  /** 打开阻塞原因输入；空白原因会保持提示弹窗而不会调用后端。 */
  const blockCurrentGoal = useCallback(() => {
    const currentPorts = portsRef.current;
    const sessionId = currentPorts.session.sessionId;
    const projection = currentPorts.session.acpSessionView?.goal;
    const goal = projection?.goal;
    if (!sessionId || !projection || !goal || goal.status !== "active") return;

    const transition = {
      sessionId,
      goalId: goal.id,
      expectedRevision: projection.revision,
      expectedProjection: projection,
    };
    const openReasonPrompt = (
      message = tr("goal.blockMessage", { title: goal.title }),
    ) => {
      currentPorts.feedback.setAppDialog({
        kind: "prompt",
        title: tr("goal.blockTitle"),
        message,
        initial: "",
        placeholder: tr("goal.blockPlaceholder"),
        submitLabel: tr("goal.block"),
        onSubmit: async (value) => {
          const reason = value.trim();
          if (!reason) {
            openReasonPrompt(tr("goal.blockReasonRequired"));
            return;
          }
          await transitionCurrentGoal({
            ...transition,
            status: "blocked",
            reason,
          });
        },
      });
    };
    openReasonPrompt();
  }, [tr, transitionCurrentGoal]);

  /** 当前 Session 当前 Goal 的 pending 状态，仅由对应复合键决定。 */
  const currentGoalId =
    session.acpSessionView?.session_id === session.sessionId
      ? session.acpSessionView.goal.goal?.id ?? null
      : null;
  const goalTransitionPending = isGoalTransitionPending(
    goalTransitionPendingKeys,
    session.sessionId,
    currentGoalId,
  );

  return {
    goalModeSessionKey,
    setGoalModeSessionKey,
    planModeSessionKey,
    setPlanModeSessionKey,
    ultraModeSessionKey,
    setUltraModeSessionKey,
    showStatusModal,
    setShowStatusModal,
    goalToolCompletionSignature,
    goalTransitionPending,
    activateGoalMode,
    togglePlanMode,
    confirmClearCurrentGoal,
    editCurrentGoal,
    completeCurrentGoal,
    blockCurrentGoal,
  };
}
