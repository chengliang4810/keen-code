import { describe, expect, it } from "vitest";
import {
  createActiveTurnBootstrapBuffer,
  resolveActiveTurnFromHostSnapshot,
} from "./activeTurn";

describe("resolveActiveTurnFromHostSnapshot", () => {
  it("Host null 会清理漏收 done 后的旧关联", () => {
    expect(
      resolveActiveTurnFromHostSnapshot({
        snapshotTurnId: null,
        localTurnId: null,
        completedTurnId: null,
      }),
    ).toBeNull();
  });

  it("Host 的真实活跃回合会覆盖先前事件留下的旧 ID", () => {
    expect(
      resolveActiveTurnFromHostSnapshot({
        snapshotTurnId: "turn-2",
        localTurnId: null,
        completedTurnId: "turn-0",
      }),
    ).toBe("turn-2");
  });

  it("跨通道迟到快照不能覆盖请求之后刚开始的本地回合", () => {
    expect(
      resolveActiveTurnFromHostSnapshot({
        snapshotTurnId: null,
        localTurnId: "turn-new",
        completedTurnId: "turn-old",
      }),
    ).toBe("turn-new");
  });

  it("已经消费的 done 不会被迟到快照重新激活", () => {
    expect(
      resolveActiveTurnFromHostSnapshot({
        snapshotTurnId: "turn-done",
        localTurnId: null,
        completedTurnId: "turn-done",
      }),
    ).toBeNull();
  });

  it("等待 accepted 或 DOM commit 的已完成观测不会被重新标为 active", () => {
    expect(
      resolveActiveTurnFromHostSnapshot({
        snapshotTurnId: null,
        localTurnId: "turn-done",
        completedTurnId: "turn-done",
      }),
    ).toBeNull();
  });
});

describe("createActiveTurnBootstrapBuffer", () => {
  it("快照返回后只按到达顺序重放当前 Host turn", () => {
    const activeTurns = new Map<string, string>();
    const applied: string[] = [];
    const buffer = createActiveTurnBootstrapBuffer((sessionId) =>
      activeTurns.get(sessionId),
    );

    expect(
      buffer.deferUnknown("session-a", "turn-old", () =>
        applied.push("old"),
      ),
    ).toBe(true);
    expect(
      buffer.deferUnknown("session-a", "turn-live", () =>
        applied.push("first"),
      ),
    ).toBe(true);
    expect(
      buffer.deferUnknown("session-a", "turn-live", () =>
        applied.push("second"),
      ),
    ).toBe(true);

    activeTurns.set("session-a", "turn-live");
    buffer.replayMatching();
    expect(applied).toEqual(["first", "second"]);
  });

  it("done 清掉 active 后不会重放同 turn 的迟到事件", () => {
    const activeTurns = new Map<string, string>();
    const applied: string[] = [];
    const buffer = createActiveTurnBootstrapBuffer((sessionId) =>
      activeTurns.get(sessionId),
    );
    buffer.deferUnknown("session-a", "turn-1", () => {
      applied.push("delta");
    });
    buffer.deferUnknown("session-a", "turn-1", () => {
      applied.push("done");
      activeTurns.delete("session-a");
    });
    buffer.deferUnknown("session-a", "turn-1", () => {
      applied.push("late");
    });

    activeTurns.set("session-a", "turn-1");
    buffer.replayMatching();
    expect(applied).toEqual(["delta", "done"]);
  });

  it("快照失败后丢弃恢复窗口事件并继续 fail closed", () => {
    const buffer = createActiveTurnBootstrapBuffer(() => null);
    let applied = false;
    buffer.deferUnknown("session-a", "turn-1", () => {
      applied = true;
    });
    buffer.discard();

    expect(
      buffer.deferUnknown("session-a", "turn-1", () => {
        applied = true;
      }),
    ).toBe(true);
    expect(applied).toBe(false);
  });

  it("completed 快照先返回时允许尾随 update/done，并在 done 后封口", () => {
    const activeTurns = new Map<string, string>();
    const recoverableCompletedTurns = new Map<string, string>();
    const applied: string[] = [];
    const buffer = createActiveTurnBootstrapBuffer(
      (sessionId) =>
        activeTurns.get(sessionId) ??
        recoverableCompletedTurns.get(sessionId),
    );
    buffer.replayMatching();
    recoverableCompletedTurns.set("session-a", "turn-done");

    const route = (label: string, terminal = false) => {
      const apply = () => {
        applied.push(label);
        if (terminal) recoverableCompletedTurns.delete("session-a");
      };
      if (!buffer.deferUnknown("session-a", "turn-done", apply)) apply();
    };
    route("update");
    route("done", true);
    route("late-update");

    expect(applied).toEqual(["update", "done"]);
  });

  it("有本地 active 关联时不缓冲，并对异常积压设置溢出标记", () => {
    const activeTurns = new Map([["session-local", "turn-local"]]);
    const buffer = createActiveTurnBootstrapBuffer(
      (sessionId) => activeTurns.get(sessionId),
      1,
    );
    expect(
      buffer.deferUnknown("session-local", "turn-local", () => {}),
    ).toBe(false);
    expect(buffer.deferUnknown("session-a", "turn-1", () => {})).toBe(true);
    expect(buffer.deferUnknown("session-b", "turn-2", () => {})).toBe(true);
    expect(buffer.overflowed).toBe(true);
  });
});
