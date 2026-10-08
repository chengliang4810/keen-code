import { describe, expect, it } from "vitest";
import type { SessionMeta } from "./sessions";
import type { SpaceMeta } from "@/modules/spaces/lib/store";
import { projectTaskGroups, resolveSessionProject } from "./projectTasks";

const projects: SpaceMeta[] = [
  {
    id: "a",
    name: "A",
    root: "/project",
    env: { kind: "local" },
    createdAt: 1,
    updatedAt: 1,
  },
  {
    id: "b",
    name: "B",
    root: "/project",
    env: { kind: "wsl", distro: "Ubuntu" },
    createdAt: 1,
    updatedAt: 1,
  },
];
const task = (
  id: string,
  projectId: string,
  archived = false,
  updatedAt = 1,
): SessionMeta => ({
  id,
  projectId,
  archived,
  title: id,
  updatedAt,
  createdAt: 1,
});

describe("project task hierarchy", () => {
  it("preserves a removed task binding but never assigns new legacy tasks to removed projects", () => {
    const removed = projects.map((project) => ({
      ...project,
      removed: true as const,
    }));
    expect(resolveSessionProject(task("existing", "a"), removed, "a")).toBe(
      "a",
    );
    expect(
      resolveSessionProject(
        { ...task("legacy", "missing"), workspaceRoot: "/project" },
        removed,
        "a",
      ),
    ).toBeNull();
    expect(projectTaskGroups(removed, [task("existing", "a")], false)).toEqual(
      [],
    );
  });
  it("does not migrate explicitly projectless conversations into the last project", () => {
    const independent = { ...task("independent", ""), projectless: true };
    expect(resolveSessionProject(independent, projects, "a")).toBeNull();
    expect(
      projectTaskGroups(projects, [independent], false).every(
        (group) => group.tasks.length === 0,
      ),
    ).toBe(true);
  });
  it("keeps projects as the top-level nodes, including empty projects", () => {
    const groups = projectTaskGroups(
      projects,
      [task("one", "a"), task("two", "a", false, 2)],
      false,
    );
    expect(groups.map((g) => g.project.id)).toEqual(["a", "b"]);
    expect(groups[0].tasks.map((t) => t.id)).toEqual(["two", "one"]);
    expect(groups[1].tasks).toEqual([]);
  });
  it("isolates archived tasks while preserving project membership", () => {
    const tasks = [
      task("one", "a"),
      task("two", "a", true),
      task("three", "b"),
    ];
    expect(
      projectTaskGroups(projects, tasks, true)[0].tasks.map((t) => t.id),
    ).toEqual(["two"]);
    expect(
      projectTaskGroups(projects, tasks, false)[1].tasks.map((t) => t.id),
    ).toEqual(["three"]);
  });
  it("matches legacy paths together with their environment", () => {
    expect(
      resolveSessionProject(
        {
          ...task("legacy", ""),
          workspaceRoot: "/project",
          workspaceScope: "wsl:Ubuntu",
        },
        projects,
        "a",
      ),
    ).toBe("b");
  });
  it("preserves explicit membership and recovers missing project references", () => {
    expect(resolveSessionProject(task("one", "b"), projects, "a")).toBe("b");
    expect(resolveSessionProject(task("old", "deleted"), projects, "a")).toBe(
      "a",
    );
  });
});
