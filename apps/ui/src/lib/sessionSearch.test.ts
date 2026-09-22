import { describe, expect, it } from "vitest";
import { filterSessionSearch } from "./sessionSearch";

const projects = [
  { id: "p1", name: "keencode-desktop", path: "/Users/me/Code/keencode-desktop" },
  { id: "p2", name: "notes", path: "/Users/me/notes" },
];

const sessions = [
  { id: "s1", title: "Fix runtime reset", projectId: "p1" },
  { id: "s2", title: "Weekly plan", projectId: "p2" },
  { id: "s3", title: "Untitled", projectId: null },
  { id: "s4", title: "Old archived", projectId: "p1", archived: true },
];

describe("filterSessionSearch", () => {
  it("returns recent items when query is empty", () => {
    const hits = filterSessionSearch("", sessions, projects);
    expect(hits.matchedSessions.map((s) => s.id)).toEqual(["s1", "s2", "s3"]);
    expect(hits.matchedProjects.map((p) => p.id)).toEqual(["p1", "p2"]);
  });

  it("matches session title case-insensitively", () => {
    const hits = filterSessionSearch("runtime", sessions, projects);
    expect(hits.matchedSessions.map((s) => s.id)).toEqual(["s1"]);
  });

  it("matches project name and pulls related sessions", () => {
    const hits = filterSessionSearch("keencode-desktop", sessions, projects);
    expect(hits.matchedProjects.map((p) => p.id)).toEqual(["p1"]);
    expect(hits.matchedSessions.map((s) => s.id)).toContain("s1");
  });

  it("matches project path segments", () => {
    const hits = filterSessionSearch("Code/keencode", sessions, projects);
    expect(hits.matchedProjects[0]?.id).toBe("p1");
  });

  it("skips archived sessions by default", () => {
    const hits = filterSessionSearch("archived", sessions, projects);
    expect(hits.matchedSessions).toHaveLength(0);
  });

  it("can include archived when asked", () => {
    const hits = filterSessionSearch("archived", sessions, projects, {
      includeArchived: true,
    });
    expect(hits.matchedSessions.map((s) => s.id)).toEqual(["s4"]);
  });
});
