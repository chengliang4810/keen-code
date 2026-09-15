import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createT, type Locale } from "@/i18n";
import type {
  ComposerApiPort,
  ComposerFeedbackPort,
  ComposerSessionPort,
  ComposerWorkspacePort,
  StateSetter,
} from "../useComposerController";
import { reduceGoalSnapshot } from "@/lib/acp/store";
import { isGoalToolName } from "@/lib/toolDisplay";

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
  activateGoalMode: (sessionKey: string) => void;
  togglePlanMode: (sessionKey: string) => void;
  confirmClearCurrentGoal: () => void;
  editCurrentGoal: () => void;
  pauseCurrentGoal: () => Promise<boolean>;
  resumeCurrentGoal: () => Promise<boolean>;
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

  useEffect(() => {
    const currentPorts = portsRef.current;
    const sessionId = currentPorts.session.sessionId;
    if (!currentPorts.api.isTauri() || !sessionId) return;
    void currentPorts.api.goals
      .get(sessionId)
      .then((result) => {
        const view =
          portsRef.current.workspace.acpWorkspaceRef.current.sessions[sessionId];
        if (!view || !reduceGoalSnapshot(view, result.revision, result.goal ?? null)) return;
        portsRef.current.workspace.commitWorkspace();
        portsRef.current.workspace.applyViewProjectionRef.current(sessionId);
      })
      .catch(() => {});
  }, [goalToolCompletionSignature, session.sessionId]);

  const confirmClearCurrentGoal = useCallback(() => {
    const currentPorts = portsRef.current;
    const sessionId = currentPorts.session.sessionId;
    const goal = currentPorts.session.acpSessionView?.goal.goal;
    if (!sessionId || !goal) return;
    currentPorts.feedback.setAppDialog({
      kind: "confirm",
      title: tr("goal.clearTitle"),
      message: tr("goal.clearConfirm", { title: goal.title }),
      confirmLabel: tr("goal.clear"),
      danger: true,
      onConfirm: async () => {
        try {
          const snapshot = await currentPorts.api.goals.get(sessionId);
          const currentView =
            currentPorts.workspace.acpWorkspaceRef.current.sessions[sessionId];
          if (!snapshot.goal) {
            if (currentView && reduceGoalSnapshot(currentView, snapshot.revision, null)) {
              currentPorts.workspace.commitWorkspace();
            }
            return;
          }
          const result = await currentPorts.api.goals.clear({
            sessionId,
            expectedRevision: snapshot.revision,
            requestNonce: `keencode-${Date.now()}-${Math.random()
              .toString(36)
              .slice(2)}`,
          });
          const view =
            currentPorts.workspace.acpWorkspaceRef.current.sessions[sessionId];
          if (view && reduceGoalSnapshot(view, result.revision, null)) {
            currentPorts.workspace.commitWorkspace();
          }
        } catch (cause) {
          currentPorts.feedback.showToast(
            tr("goal.clearFailed", { error: String(cause) }),
            4000,
          );
        }
      },
    });
  }, [tr]);

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
        try {
          const result = await currentPorts.api.goals.upsert({
            sessionId,
            goal: {
              title,
              objective: title,
              ...(goal.description ? { description: goal.description } : {}),
            },
            expectedRevision: projection.revision,
            requestNonce: `keencode-${Date.now()}-${Math.random()
              .toString(36)
              .slice(2)}`,
          });
          const view =
            currentPorts.workspace.acpWorkspaceRef.current.sessions[sessionId];
          if (view && reduceGoalSnapshot(view, result.revision, result.goal)) {
            currentPorts.workspace.commitWorkspace();
          }
        } catch (cause) {
          currentPorts.feedback.showToast(
            tr("goal.editFailed", { error: String(cause) }),
            4000,
          );
        }
      },
    });
  }, [tr]);

  const pauseCurrentGoal = useCallback(async () => {
    const ports = portsRef.current;
    const sessionId = ports.session.sessionId;
    const projection = ports.session.acpSessionView?.goal;
    const goal = projection?.goal;
    if (!sessionId || !projection || !goal || goal.status !== "active") return false;
    try {
      const result = await ports.api.goals.transition({
        sessionId,
        goalId: goal.id,
        status: "paused",
        expectedRevision: projection.revision,
        requestNonce: `keencode-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      });
      const view = ports.workspace.acpWorkspaceRef.current.sessions[sessionId];
      if (view && reduceGoalSnapshot(view, result.revision, result.goal)) {
        ports.workspace.commitWorkspace();
      }
      return true;
    } catch (cause) {
      ports.feedback.showToast(String(cause), 4000);
      return false;
    }
  }, []);

  const resumeCurrentGoal = useCallback(async () => {
    const ports = portsRef.current;
    const sessionId = ports.session.sessionId;
    const projection = ports.session.acpSessionView?.goal;
    const goal = projection?.goal;
    if (!sessionId || !projection || !goal) return false;
    if (goal.status === "active") return true;
    if (goal.status !== "paused") return false;
    try {
      const result = await ports.api.goals.upsert({
        sessionId,
        goal: {
          title: goal.title,
          objective: goal.objective,
          ...(goal.description ? { description: goal.description } : {}),
          ...(goal.progressPercent != null ? { progressPercent: goal.progressPercent } : {}),
          ...(goal.tokenBudget != null ? { tokenBudget: goal.tokenBudget } : {}),
        },
        expectedRevision: projection.revision,
        requestNonce: `keencode-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      });
      const view = ports.workspace.acpWorkspaceRef.current.sessions[sessionId];
      if (view && reduceGoalSnapshot(view, result.revision, result.goal)) {
        ports.workspace.commitWorkspace();
      }
      return true;
    } catch (cause) {
      ports.feedback.showToast(String(cause), 4000);
      return false;
    }
  }, []);

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
    activateGoalMode,
    togglePlanMode,
    confirmClearCurrentGoal,
    editCurrentGoal,
    pauseCurrentGoal,
    resumeCurrentGoal,
  };
}
