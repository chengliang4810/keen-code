import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { TurnLatencyState } from "@/lib/turnLatency";
import type { SessionTurnState } from "./types";
import { useSessionTurnState } from "./useSessionTurnState";

/** 渲染一次没有外部 stateRefs 的 Hook，并返回其本地状态端口。 */
function renderLocalSessionTurnState(): SessionTurnState {
  let captured!: SessionTurnState;

  /** 在合法 React 渲染上下文中捕获 Hook 返回值。 */
  function Harness() {
    captured = useSessionTurnState();
    return null;
  }

  renderToString(createElement(Harness));
  return captured;
}

/** 创建 Active Turn 恢复测试所需的最小完整延迟观测。 */
function turnLatency(turnId: string): TurnLatencyState {
  return {
    turnId,
    startedAtMs: 1,
    sendAcknowledgedAtMs: null,
    firstSseAtMs: null,
    firstVisibleTokenAtMs: null,
    completedAtMs: null,
    usageObservations: [],
  };
}

describe("useSessionTurnState", () => {
  it("没有 stateRefs 时仍使用 canonical 规则拒绝迟到 Host 旧快照", () => {
    const state = renderLocalSessionTurnState();
    state.turnLatencyBySessionRef.current.set(
      "session-a",
      turnLatency("turn-new"),
    );
    state.activeTurnIdBySessionRef.current.set("session-a", "turn-old");
    state.recoverableCompletedTurnIdBySessionRef.current.set(
      "session-a",
      "turn-old",
    );

    state.observeHostActiveTurn({
      sessionId: "session-a",
      activeTurnId: "turn-old",
    });

    expect(state.activeTurnIdBySessionRef.current.get("session-a")).toBe(
      "turn-new",
    );
    expect(
      state.recoverableCompletedTurnIdBySessionRef.current.has("session-a"),
    ).toBe(false);
  });

  it("没有本地新回合时用 Host 空快照清理旧关联", () => {
    const state = renderLocalSessionTurnState();
    state.activeTurnIdBySessionRef.current.set("session-a", "turn-old");

    state.observeHostActiveTurn({
      sessionId: "session-a",
      activeTurnId: null,
    });

    expect(state.activeTurnIdBySessionRef.current.has("session-a")).toBe(false);
  });
});
