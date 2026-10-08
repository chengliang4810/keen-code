import { describe, expect, it } from "vitest";
import {
  applyCloseTabsPlan,
  nextActiveInSpace,
  planCloseOtherTabs,
  planCloseTabsToRight,
  planFileTabOpen,
  planMarkdownTabOpen,
  planGitDiffOpen,
  planCommitHistoryOpen,
  reorderTabsByGap,
  type Tab,
} from "@/modules/tabs/lib/useTabs";

function terminal(id: number, taskId: string): Tab {
  return {
    id,
    spaceId: "project",
    taskId,
    kind: "terminal",
    title: "shell",
    paneTree: { kind: "leaf", id: id * 10 },
    activeLeafId: id * 10,
  };
}
const ids = (tabs: Tab[]) => tabs.map((tab) => tab.id);

describe("task tab ownership", () => {
  it("does not activate another conversation's terminal after the final tab closes", () => {
    expect(
      nextActiveInSpace([terminal(1, "a"), terminal(2, "b")], 1),
    ).toBeNull();
  });
  it("close ranges stay within the same conversation in the same project", () => {
    const tabs = [
      terminal(1, "a"),
      terminal(2, "b"),
      terminal(3, "a"),
      terminal(4, "b"),
    ];
    expect(planCloseTabsToRight(tabs, 1, 3).closeIds).toEqual([3]);
    expect(planCloseOtherTabs(tabs, 1, 3).closeIds).toEqual([3]);
    expect(
      applyCloseTabsPlan(tabs, 1, { closeIds: [2, 3, 4], nextActiveId: 1 })
        ?.closeIds,
    ).toEqual([3]);
  });
  it("reordering does not use another conversation's insertion positions", () => {
    const tabs = [
      terminal(1, "a"),
      terminal(2, "b"),
      terminal(3, "a"),
      terminal(4, "b"),
    ];
    expect(ids(reorderTabsByGap(tabs, 1, 2))).toEqual([2, 3, 1, 4]);
  });
  it("the same file has separate preview and dirty-buffer identities in two conversations", () => {
    let nextId = 1;
    const alloc = () => nextId++;
    const a = planFileTabOpen(
      [],
      "/project/a.ts",
      false,
      "project",
      alloc,
      "a",
    );
    const b = planFileTabOpen(
      a.tabs,
      "/project/a.ts",
      false,
      "project",
      alloc,
      "b",
    );
    const c = planFileTabOpen(
      b.tabs,
      "/project/b.ts",
      false,
      "project",
      alloc,
      "b",
    );
    expect(c.tabs.find((tab) => tab.taskId === "a")).toEqual(a.tabs[0]);
    expect(b.tabId).not.toBe(a.tabId);
    expect(c.tabs.filter((tab) => tab.taskId === "b")).toHaveLength(1);
  });
  it("markdown and Git tool deduplication are scoped to a conversation", () => {
    let nextId = 1;
    const alloc = () => nextId++;
    const mdA = planMarkdownTabOpen([], "/readme.md", "project", alloc, "a");
    const mdB = planMarkdownTabOpen(
      mdA.tabs,
      "/readme.md",
      "project",
      alloc,
      "b",
    );
    expect(mdB.tabId).not.toBe(mdA.tabId);
    const input = { repoRoot: "/project", path: "a.ts", mode: "+" as const };
    const gitA = planGitDiffOpen([], input, "project", false, alloc, "a");
    const gitB = planGitDiffOpen(
      gitA.tabs,
      input,
      "project",
      false,
      alloc,
      "b",
    );
    expect(gitB.tabs).toHaveLength(2);
    const historyA = planCommitHistoryOpen([], input, "project", alloc, "a");
    expect(
      planCommitHistoryOpen(historyA.tabs, input, "project", alloc, "b").tabs,
    ).toHaveLength(2);
  });
});
