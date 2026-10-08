import { describe, expect, it } from "vitest";
import type { SessionMeta } from "@/modules/ai/lib/sessions";
import type { SpaceMeta } from "@/modules/spaces";
import {
  changeArchivedConversations,
  groupArchivedConversations,
} from "@/modules/ai/lib/archivedConversations";

const project: SpaceMeta = {
  id: "p",
  name: "Project",
  root: "D:/p",
  env: { kind: "local" },
  createdAt: 1,
  updatedAt: 1,
};
const archived: SessionMeta = {
  id: "a",
  title: "Check code",
  projectId: "p",
  archived: true,
  createdAt: 1,
  updatedAt: 2,
};
const independent: SessionMeta = { ...archived, id: "i", projectless: true };
const sessions = [
  archived,
  independent,
  { ...archived, id: "b", updatedAt: 3 },
  { ...archived, id: "live", archived: false },
];

describe("archived conversation grouping", () => {
  it("excludes active records, respects explicit independent ownership and sorts by update time", () => {
    const groups = groupArchivedConversations(sessions, [project]);
    expect(groups.map((g) => g.id)).toEqual(["project:p", "independent"]);
    expect(groups[0].sessions.map((s) => s.id)).toEqual(["b", "a"]);
    expect(groups[1].sessions).toEqual([independent]);
    expect(sessions.map((s) => s.id)).toEqual(["a", "i", "b", "live"]);
  });
  it("combines case-insensitive search with type and project filters", () => {
    expect(
      groupArchivedConversations(
        sessions,
        [project],
        " CODE ",
        "project",
        "project:p",
      )[0].sessions,
    ).toHaveLength(2);
    expect(groupArchivedConversations(sessions, [project], "unknown")).toEqual(
      [],
    );
    expect(
      groupArchivedConversations(sessions, [project], "", "independent")[0]
        .sessions,
    ).toEqual([independent]);
    expect(
      groupArchivedConversations(
        sessions,
        [project],
        "",
        "project",
        "project:other",
      ),
    ).toEqual([]);
  });
  it("retains removed projects and orphan project identities", () => {
    const removed = { ...project, removed: true as const };
    const groups = groupArchivedConversations(
      [archived, { ...archived, id: "orphan", projectId: "missing" }],
      [removed],
    );
    expect(groups.find((g) => g.id === "project:p")?.project).toBe(removed);
    expect(groups.find((g) => g.id === "project:missing")?.sessions[0].id).toBe(
      "orphan",
    );
  });
  it("uses stable IDs when timestamps tie", () => {
    expect(
      groupArchivedConversations(
        [{ ...archived, id: "z" }, archived],
        [project],
      )[0].sessions.map((s) => s.id),
    ).toEqual(["a", "z"]);
  });
});

describe("archive changes", () => {
  it("restores only captured IDs and preserves their project and timestamp", () => {
    const result = changeArchivedConversations(sessions, ["a"], "restore");
    expect(result[0]).toEqual({ ...archived, archived: false });
    expect(result[1]).toBe(independent);
    expect(archived.archived).toBe(true);
  });
  it("deletes only captured IDs and tolerates missing or duplicate IDs", () => {
    expect(
      changeArchivedConversations(
        sessions,
        ["a", "a", "missing"],
        "delete",
      ).map((s) => s.id),
    ).toEqual(["i", "b", "live"]);
    expect(
      changeArchivedConversations(sessions, ["missing"], "delete"),
    ).toEqual(sessions);
  });
  it.each(["delete", "restore"] as const)(
    "rejects %s atomically when a captured record is no longer archived",
    (operation) => {
      expect(() =>
        changeArchivedConversations(sessions, ["a", "live"], operation),
      ).toThrow("Archived conversations changed");
      expect(archived.archived).toBe(true);
    },
  );
});
