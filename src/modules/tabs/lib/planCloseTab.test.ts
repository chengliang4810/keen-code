import { describe, expect, it } from "vitest";
import { planCloseTab, type Tab } from "./useTabs";

function terminal(id: number, spaceId = "a"): Tab {
  return {
    id,
    kind: "terminal",
    spaceId,
    title: "shell",
    paneTree: { kind: "leaf", id: id * 10 },
    activeLeafId: id * 10,
  };
}

describe("planCloseTab", () => {
  it("allows closing the final terminal without activating another project's tab", () => {
    const result = planCloseTab([terminal(1), terminal(2, "b")], 1, 1, true);
    expect(result?.tabs.map((tab) => tab.id)).toEqual([2]);
    expect(result?.nextActiveId).toBe(-1);
    expect(result?.disposeLeafIds).toEqual([10]);
  });

  it("closing one terminal retains the others and selects a same-project neighbor", () => {
    const result = planCloseTab(
      [terminal(1), terminal(3, "b"), terminal(2)],
      2,
      2,
      true,
    );
    expect(result?.tabs.map((tab) => tab.id)).toEqual([1, 3]);
    expect(result?.nextActiveId).toBe(1);
    expect(result?.disposeLeafIds).toEqual([20]);
  });

  it("closing an inactive terminal preserves the active selection", () => {
    expect(
      planCloseTab([terminal(1), terminal(2, "b")], 1, 2, true)?.nextActiveId,
    ).toBe(2);
  });

  it("releases all split leaves exactly once and accepts an empty workspace", () => {
    const tab = terminal(1);
    if (tab.kind !== "terminal") throw new Error("expected terminal");
    tab.paneTree = {
      kind: "split",
      id: 9,
      dir: "row",
      children: [
        { kind: "leaf", id: 10 },
        { kind: "leaf", id: 11 },
      ],
    };
    const result = planCloseTab([tab], 1, 1, true);
    expect(result?.tabs).toEqual([]);
    expect(result?.disposeLeafIds).toEqual([10, 11]);
    expect(result?.nextActiveId).toBe(-1);
    expect(planCloseTab([], 1, -1, true)).toBeNull();
  });

  it("preserves the last-tab guard for callers that do not opt in", () => {
    expect(planCloseTab([terminal(1)], 1, 1)).toBeNull();
  });
});
