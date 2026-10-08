import { describe, expect, it } from "vitest";
import {
  captureTaskTools,
  defaultTaskSidebar,
  normalizeTaskSidebar,
  restoreTaskTools,
  initializeTaskTools,
  validTaskSidebarView,
} from "@/app/lib/taskSidebar";
import type { Tab } from "@/modules/tabs/lib/useTabs";

const terminal: Tab = {
  id: 1,
  taskId: "a",
  spaceId: "project",
  kind: "terminal",
  title: "shell",
  cwd: "/project",
  paneTree: { kind: "leaf", id: 2, cwd: "/project" },
  activeLeafId: 2,
};
const editor: Tab = {
  id: 3,
  taskId: "a",
  spaceId: "project",
  kind: "editor",
  path: "/project/a.ts",
  title: "a.ts",
  dirty: true,
  preview: false,
};

describe("task sidebar snapshots", () => {
  it("leaves startup tools untouched when a draft or independent task opens", () => {
    const legacy = { ...terminal, taskId: undefined };
    const initial = initializeTaskTools(
      [legacy, editor],
      "project",
      "draft",
      undefined,
      () => 100,
      false,
    );
    expect(initial.owned).toEqual([]);
    expect(initial.tabs).toEqual([legacy, editor]);
    const live = { ...terminal, taskId: "draft" };
    const resumed = initializeTaskTools(
      [...initial.tabs, live],
      "project",
      "draft",
      defaultTaskSidebar(),
      () => 100,
      false,
    );
    expect(resumed.owned).toEqual([live]);
    expect(resumed.tabs).toEqual([legacy, editor, live]);
  });
  it("keeps live background tools instead of hydrating duplicate instances on first navigation", () => {
    const saved = {
      ...defaultTaskSidebar(),
      ...captureTaskTools([terminal], 1),
    };
    const allocate = () => {
      throw new Error("must reuse live terminal");
    };
    const initial = initializeTaskTools(
      [terminal],
      "project",
      "a",
      saved,
      allocate,
    );
    expect(initial.tabs).toEqual([terminal]);
    expect(initial.owned[0]).toBe(terminal);
  });
  it("migrates legacy terminals once and respects explicitly empty task records", () => {
    const legacy = { ...terminal, taskId: undefined };
    const migrated = initializeTaskTools(
      [legacy],
      "project",
      "a",
      undefined,
      () => 100,
    );
    expect(migrated.tabs).toHaveLength(1);
    expect(migrated.owned[0]).toMatchObject({ id: 1, taskId: "a" });
    const other = initializeTaskTools(
      migrated.tabs,
      "project",
      "b",
      undefined,
      () => 100,
    );
    expect(other.owned).toEqual([]);
    expect(other.tabs).toEqual(migrated.tabs);
    const empty = initializeTaskTools(
      [legacy],
      "project",
      "a",
      defaultTaskSidebar(),
      () => 100,
    );
    expect(empty.tabs).toEqual([]);
  });
  it("restores native tabs with fresh runtime IDs, the same project directory and task owner", () => {
    let next = 100;
    const snapshot = captureTaskTools([terminal, editor], 3);
    const restored = restoreTaskTools(
      snapshot.tabs,
      "project",
      "a",
      () => next++,
    );
    expect(snapshot.activeTabIndex).toBe(1);
    expect(restored.map((tab) => tab.taskId)).toEqual(["a", "a"]);
    expect(restored.every((tab) => tab.spaceId === "project" && tab.cold)).toBe(
      true,
    );
    expect(restored[0]).toMatchObject({ kind: "terminal", cwd: "/project" });
    expect(restored[1]).toMatchObject({
      kind: "editor",
      path: "/project/a.ts",
      dirty: false,
    });
    expect(restored.map((tab) => tab.id)).not.toContain(1);
  });
  it("does not persist private terminals or pending AI approval contents", () => {
    const privateTab = { ...terminal, private: true };
    const approval: Tab = {
      id: 4,
      spaceId: "project",
      taskId: "a",
      kind: "ai-diff",
      title: "proposal",
      path: "/project/a.ts",
      originalContent: "before",
      proposedContent: "after",
      approvalId: "approval",
      status: "pending",
      isNewFile: false,
    };
    const saved = captureTaskTools(
      [privateTab, editor, approval],
      privateTab.id,
    );
    expect(saved.tabs).toEqual([{ kind: "editor", path: "/project/a.ts" }]);
    expect(saved.activeTabIndex).toBe(-1);
  });
  it("keeps Git tabs and their repository identities across restart", () => {
    const git: Tab = {
      id: 5,
      taskId: "a",
      spaceId: "project",
      kind: "git-diff",
      title: "a.ts (+)",
      path: "a.ts",
      repoRoot: "/project",
      mode: "+",
      originalPath: null,
      preview: false,
    };
    const saved = captureTaskTools([git], 5);
    expect(
      restoreTaskTools(saved.tabs, "project", "a", () => 100)[0],
    ).toMatchObject({ kind: "git-diff", repoRoot: "/project", taskId: "a" });
  });
  it("empty and collapsed layouts remain empty and collapsed after restart", () => {
    const state = normalizeTaskSidebar({
      ...defaultTaskSidebar(),
      open: false,
      utilityTabs: [],
      view: "empty",
      tabs: [],
    });
    expect(state.open).toBe(false);
    expect(state.view).toBe("empty");
    expect(state.utilityTabs).toEqual([]);
    expect(restoreTaskTools(state.tabs, "project", "a", () => 1)).toEqual([]);
  });
  it("deduplicates singleton tools and repairs invalid active tool or width", () => {
    const state = normalizeTaskSidebar({
      utilityTabs: ["explorer", "explorer", "source-control", "unknown"],
      view: "workspace",
      width: Number.NaN,
      tabs: null,
    });
    expect(state.utilityTabs).toEqual(["explorer", "source-control"]);
    expect(state.view).toBe("explorer");
    expect(state.width).toBe(32);
    expect(validTaskSidebarView("explorer", [], true)).toBe("workspace");
  });
});
