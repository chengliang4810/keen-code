import { describe, expect, it } from "vitest";
import {
  loadUnreadTerminalResults,
  saveUnreadTerminalResults,
  isNormalSessionCompletion,
} from "./sessionCompletion";

describe("isNormalSessionCompletion", () => {
  it("仅把无错误的 end_turn 识别为正常完成", () => {
    expect(isNormalSessionCompletion("end_turn", false)).toBe(true);
    expect(isNormalSessionCompletion("end_turn", true)).toBe(false);
    expect(isNormalSessionCompletion("cancelled", false)).toBe(false);
    expect(isNormalSessionCompletion("max_turn_requests", false)).toBe(false);
  });

  it("持久化并恢复未查看的终态结果", () => {
    const values = new Map<string, string>();
    const storage = {
      get length() {
        return values.size;
      },
      clear: () => values.clear(),
      getItem: (key: string) => values.get(key) ?? null,
      key: (index: number) => [...values.keys()][index] ?? null,
      removeItem: (key: string) => {
        values.delete(key);
      },
      setItem: (key: string, value: string) => {
        values.set(key, value);
      },
    } satisfies Storage;
    saveUnreadTerminalResults(
      new Map([
        ["session-a", "completed" as const],
        ["session-b", "failed" as const],
      ]),
      storage,
    );
    expect([...loadUnreadTerminalResults(storage)]).toEqual([
      ["session-a", "completed"],
      ["session-b", "failed"],
    ]);
  });

  it("拒绝非终态结果的持久化内容", () => {
    const storage = {
      length: 0,
      clear: () => {},
      key: () => null,
      removeItem: () => {},
      setItem: () => {},
      getItem: () => JSON.stringify([["session-a", "cancelled"]]),
    } satisfies Storage;
    expect(() => loadUnreadTerminalResults(storage)).toThrow();
  });
});
