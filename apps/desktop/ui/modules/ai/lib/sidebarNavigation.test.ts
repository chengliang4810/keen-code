import { describe, expect, it } from "vitest";
import {
  normalizeSidebarNavigation,
  sidebarNavigationGroups,
  toggleSidebarPin,
  toggleSidebarSection,
  moveSidebarProject,
  orderedSidebarProjectIds,
  setSidebarConversationSort,
  moveSidebarConversation,
  type SidebarPin,
} from "@/modules/ai/lib/sidebarNavigation";
import type { SpaceMeta } from "@/modules/spaces/lib/store";
import type { SessionMeta } from "@/modules/ai/lib/sessions";

const projects: SpaceMeta[] = ["a", "b"].map((id) => ({
  id,
  name: id,
  root: `/${id}`,
  env: { kind: "local" },
  createdAt: 1,
  updatedAt: 1,
}));
const sessions: SessionMeta[] = [
  { id: "one", title: "One", projectId: "a", createdAt: 1, updatedAt: 1 },
  { id: "two", title: "Two", projectId: "a", createdAt: 1, updatedAt: 2 },
  {
    id: "independent",
    title: "Independent",
    projectless: true,
    createdAt: 1,
    updatedAt: 1,
  },
  {
    id: "archived",
    title: "Archived",
    projectId: "a",
    archived: true,
    createdAt: 1,
    updatedAt: 1,
  },
];

describe("sidebar navigation", () => {
  it("hides removed projects and their pinned conversations until restored", () => {
    const removed = projects.map((project) =>
      project.id === "a" ? { ...project, removed: true as const } : project,
    );
    const pins: SidebarPin[] = [
      { kind: "project", id: "a" },
      { kind: "task", id: "one" },
    ];
    const hidden = sidebarNavigationGroups(removed, sessions, pins, false);
    expect(hidden.projects.map((group) => group.project.id)).toEqual(["b"]);
    expect(hidden.pinned).toEqual([]);
    expect(hidden.conversations.map((task) => task.id)).toEqual([
      "independent",
    ]);
    expect(
      sidebarNavigationGroups(projects, sessions, pins, false).pinned,
    ).toHaveLength(2);
    expect(sessions[0].projectId).toBe("a");
  });
  it("keeps new unsaved manual positions stable when conversation update times change", () => {
    const tasks = [
      { ...sessions[0], createdAt: 10, updatedAt: 100 },
      { ...sessions[1], createdAt: 20, updatedAt: 1 },
    ];
    const manual = setSidebarConversationSort(
      normalizeSidebarNavigation(null),
      "manual",
      [],
    );
    expect(
      sidebarNavigationGroups(
        projects,
        tasks,
        [],
        false,
        [],
        "manual",
        manual.conversationOrder,
      ).projects[0].tasks.map((task) => task.id),
    ).toEqual(["two", "one"]);
    expect(
      sidebarNavigationGroups(
        projects,
        tasks.map((task) => ({ ...task, updatedAt: 1000 - task.updatedAt })),
        [],
        false,
        [],
        "manual",
        manual.conversationOrder,
      ).projects[0].tasks.map((task) => task.id),
    ).toEqual(["two", "one"]);
  });
  it("defaults to recently updated conversations while preserving project order", () => {
    const state = normalizeSidebarNavigation({
      projectOrder: ["b", "a"],
      conversationSort: "invalid",
    });
    expect(state.conversationSort).toBeUndefined();
    const groups = sidebarNavigationGroups(
      projects,
      sessions,
      state.pins,
      false,
      state.projectOrder,
    );
    expect(groups.projects.map((group) => group.project.id)).toEqual([
      "b",
      "a",
    ]);
    expect(groups.projects[1].tasks.map((task) => task.id)).toEqual([
      "two",
      "one",
    ]);
  });
  it("captures current order on first manual selection and preserves it across mode switches and updates", () => {
    const manual = setSidebarConversationSort(
      normalizeSidebarNavigation(null),
      "manual",
      sessions,
    );
    const moved = moveSidebarConversation(
      manual,
      sessions,
      "one",
      "two",
      "before",
    );
    const updated = sessions.map((task) =>
      task.id === "two" ? { ...task, updatedAt: 100 } : task,
    );
    const manualGroups = sidebarNavigationGroups(
      projects,
      updated,
      moved.pins,
      false,
      [],
      moved.conversationSort,
      moved.conversationOrder,
    );
    expect(manualGroups.projects[0].tasks.map((task) => task.id)).toEqual([
      "one",
      "two",
    ]);
    const recent = setSidebarConversationSort(moved, "recent", updated);
    expect(
      sidebarNavigationGroups(
        projects,
        updated,
        recent.pins,
        false,
        [],
        recent.conversationSort,
        recent.conversationOrder,
      ).projects[0].tasks.map((task) => task.id),
    ).toEqual(["two", "one"]);
    const restored = setSidebarConversationSort(recent, "manual", updated);
    expect(restored.conversationOrder).toEqual(moved.conversationOrder);
    expect(setSidebarConversationSort(restored, "manual", updated)).toBe(
      restored,
    );
  });
  it("rejects recent-mode, cross-project, archived and pinned conversation moves", () => {
    const state = normalizeSidebarNavigation(null);
    expect(
      moveSidebarConversation(state, sessions, "one", "two", "before"),
    ).toBe(state);
    const manual = setSidebarConversationSort(state, "manual", sessions);
    expect(
      moveSidebarConversation(manual, sessions, "one", "independent", "before"),
    ).toBe(manual);
    expect(
      moveSidebarConversation(manual, sessions, "one", "archived", "before"),
    ).toBe(manual);
    expect(
      moveSidebarConversation(manual, sessions, "one", "missing", "before"),
    ).toBe(manual);
    const pinned = toggleSidebarPin(manual, { kind: "task", id: "one" });
    expect(
      moveSidebarConversation(pinned, sessions, "one", "two", "before"),
    ).toBe(pinned);
  });
  it("orders independent conversations in isolation and overrides stale project ownership", () => {
    const tasks = [
      ...sessions,
      { ...sessions[2], id: "independent-two", projectId: "a", updatedAt: 3 },
    ];
    const manual = setSidebarConversationSort(
      normalizeSidebarNavigation(null),
      "manual",
      tasks,
    );
    const moved = moveSidebarConversation(
      manual,
      tasks,
      "independent",
      "independent-two",
      "before",
    );
    const groups = sidebarNavigationGroups(
      projects,
      tasks,
      [],
      false,
      [],
      "manual",
      moved.conversationOrder,
    );
    expect(groups.conversations.map((task) => task.id)).toEqual([
      "independent",
      "independent-two",
    ]);
    expect(groups.projects[0].tasks.map((task) => task.id)).toEqual([
      "two",
      "one",
    ]);
    expect(
      moveSidebarConversation(moved, tasks, "one", "independent-two", "before"),
    ).toBe(moved);
  });
  it("keeps hidden pinned and archived positions and prepends new conversations to manual order", () => {
    const state = normalizeSidebarNavigation({
      conversationSort: "manual",
      conversationOrder: ["two", "archived", "one", "independent"],
      pins: [{ kind: "task", id: "two" }],
    });
    const tasks = [...sessions, { ...sessions[0], id: "new", updatedAt: 100 }];
    const moved = moveSidebarConversation(state, tasks, "one", "new", "before");
    expect(moved.conversationOrder).toEqual([
      "one",
      "two",
      "archived",
      "new",
      "independent",
    ]);
    expect(
      sidebarNavigationGroups(
        projects,
        tasks,
        state.pins,
        false,
        [],
        "manual",
        state.conversationOrder,
      ).projects[0].tasks.map((task) => task.id),
    ).toEqual(["new", "one"]);
    const unpinned = toggleSidebarPin(moved, { kind: "task", id: "two" });
    expect(
      sidebarNavigationGroups(
        projects,
        tasks,
        unpinned.pins,
        false,
        [],
        "manual",
        unpinned.conversationOrder,
      ).projects[0].tasks.map((task) => task.id),
    ).toEqual(["one", "two", "new"]);
  });
  it("validates persisted conversation sorting preferences", () => {
    expect(
      normalizeSidebarNavigation({
        conversationSort: "manual",
        conversationOrder: [null, 3, "two", "two", "", "one"],
      }),
    ).toEqual({
      pins: [],
      collapsed: [],
      conversationSort: "manual",
      conversationOrder: ["two", "one"],
    });
  });
  it("moves ordinary projects in both directions and leaves pinned slots untouched", () => {
    const state = normalizeSidebarNavigation({
      pins: [{ kind: "project", id: "pinned" }],
    });
    const source = { kind: "project", id: "a", section: "projects" } as const;
    const target = { kind: "project", id: "b", section: "projects" } as const;
    const next = moveSidebarProject(
      state,
      ["a", "pinned", "b"],
      source,
      target,
      "after",
    );
    expect(next.projectOrder).toEqual(["b", "pinned", "a"]);
    expect(next.pins).toEqual(state.pins);
    expect(
      moveSidebarProject(next, ["a", "pinned", "b"], source, target, "before")
        .projectOrder,
    ).toEqual(["a", "pinned", "b"]);
    expect(
      sidebarNavigationGroups(
        projects,
        sessions,
        next.pins,
        false,
        next.projectOrder,
      ).projects.map((group) => group.project.id),
    ).toEqual(["b", "a"]);
  });
  it("reorders a pinned project around pinned conversations without copying or unpinning", () => {
    const state = normalizeSidebarNavigation({
      pins: [
        { kind: "task", id: "a" },
        { kind: "project", id: "a" },
        { kind: "task", id: "two" },
      ],
    });
    const source = { kind: "project", id: "a", section: "pinned" } as const;
    const next = moveSidebarProject(
      state,
      ["a", "b"],
      source,
      { kind: "task", id: "a", section: "pinned" },
      "before",
    );
    expect(next.pins).toEqual([state.pins[1], state.pins[0], state.pins[2]]);
    expect(next.projectOrder).toBeUndefined();
    expect(state.pins[0].kind).toBe("task");
  });
  it("ignores same-position, missing, task-source and cross-section drops", () => {
    const state = normalizeSidebarNavigation(null);
    const source = { kind: "project", id: "a", section: "projects" } as const;
    const target = { kind: "project", id: "b", section: "projects" } as const;
    expect(moveSidebarProject(state, ["a", "b"], source, source, "after")).toBe(
      state,
    );
    expect(
      moveSidebarProject(state, ["a", "b"], source, target, "before"),
    ).toBe(state);
    expect(
      moveSidebarProject(
        state,
        ["a", "b"],
        source,
        { ...target, section: "pinned" },
        "after",
      ),
    ).toBe(state);
    expect(moveSidebarProject(state, ["b"], source, target, "after")).toBe(
      state,
    );
    expect(
      moveSidebarProject(
        state,
        ["a", "b"],
        { ...source, kind: "task" },
        target,
        "after",
      ),
    ).toBe(state);
  });
  it("restores valid saved order and appends newly created projects", () => {
    const state = normalizeSidebarNavigation({
      projectOrder: [null, 3, "b", "b", "deleted", "a", ""],
    });
    expect(state.projectOrder).toEqual(["b", "deleted", "a"]);
    expect(
      orderedSidebarProjectIds(["a", "b", "new"], state.projectOrder),
    ).toEqual(["b", "a", "new"]);
    expect(
      sidebarNavigationGroups(
        projects,
        sessions,
        [],
        false,
        state.projectOrder,
      ).projects.map((group) => group.project.id),
    ).toEqual(["b", "a"]);
  });
  it("keeps mixed pin order and hides individually pinned tasks inside a pinned parent", () => {
    const pins: SidebarPin[] = [
      { kind: "task", id: "one" },
      { kind: "project", id: "a" },
      { kind: "task", id: "independent" },
    ];
    const result = sidebarNavigationGroups(projects, sessions, pins, false);
    expect(result.pinned.map((item) => item.kind)).toEqual([
      "task",
      "project",
      "task",
    ]);
    expect(result.pinned[1]).toEqual({
      kind: "project",
      group: { project: projects[0], tasks: [sessions[1]] },
    });
    expect(result.projects.map((group) => group.project.id)).toEqual(["b"]);
    expect(result.conversations).toEqual([]);
    expect(sessions[0].projectId).toBe("a");
  });
  it("unpinning restores original membership and recency order", () => {
    let state = normalizeSidebarNavigation({
      pins: [{ kind: "task", id: "two" }],
    });
    expect(
      sidebarNavigationGroups(projects, sessions, state.pins, false).projects[0]
        .tasks,
    ).toEqual([sessions[0]]);
    state = toggleSidebarPin(state, { kind: "task", id: "two" });
    expect(
      sidebarNavigationGroups(projects, sessions, state.pins, false).projects[0]
        .tasks,
    ).toEqual([sessions[1], sessions[0]]);
  });
  it("ignores missing and duplicate IDs and separates archived conversations", () => {
    const pins: SidebarPin[] = [
      { kind: "task", id: "archived" },
      { kind: "task", id: "archived" },
      { kind: "task", id: "deleted" },
      { kind: "project", id: "deleted" },
    ];
    expect(
      sidebarNavigationGroups(projects, sessions, pins, false).pinned,
    ).toEqual([]);
    expect(
      sidebarNavigationGroups(projects, sessions, pins, true).pinned,
    ).toEqual([{ kind: "task", task: sessions[3] }]);
  });
  it("explicit projectless ownership overrides a stale project ID", () => {
    const tasks = [{ ...sessions[2], projectId: "a" }];
    const result = sidebarNavigationGroups(projects, tasks, [], false);
    expect(result.projects.every((group) => !group.tasks.length)).toBe(true);
    expect(result.conversations).toEqual(tasks);
    expect(
      sidebarNavigationGroups(
        projects,
        tasks,
        [{ kind: "task", id: "independent" }],
        false,
      ).conversations,
    ).toEqual([]);
  });
  it("validates persisted data and namespaces project and task IDs", () => {
    expect(normalizeSidebarNavigation(null)).toEqual({
      pins: [],
      collapsed: [],
    });
    expect(
      normalizeSidebarNavigation({
        pins: [
          null,
          "bad",
          { kind: "task", id: 5 },
          { kind: "project", id: "a" },
          { kind: "project", id: "a" },
          { kind: "task", id: "a" },
        ],
        collapsed: ["projects", "projects", "invalid"],
      }),
    ).toEqual({
      pins: [
        { kind: "project", id: "a" },
        { kind: "task", id: "a" },
      ],
      collapsed: ["projects"],
    });
  });
  it("each section collapses independently without touching pins", () => {
    const state = normalizeSidebarNavigation({
      pins: [{ kind: "project", id: "a" }],
    });
    const next = toggleSidebarSection(
      toggleSidebarSection(state, "projects"),
      "pinned",
    );
    expect(next.collapsed).toEqual(["projects", "pinned"]);
    expect(toggleSidebarSection(next, "projects").collapsed).toEqual([
      "pinned",
    ]);
    expect(next.pins).toEqual(state.pins);
  });
});
