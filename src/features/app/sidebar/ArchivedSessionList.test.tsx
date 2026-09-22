import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { Project, SessionRow } from "@/features/app/models";
import { IDLE_SNAPSHOT } from "@/lib/session";
import { ArchivedSessionList } from "./ArchivedSessionList";

const project: Project = {
  id: "project-1",
  name: "KeenCode",
  path: "D:/projects/keen-code",
  pathOk: true,
};

function session(
  id: string,
  archived: boolean,
  updatedAt: string,
): SessionRow {
  return {
    id,
    title: id,
    projectId: project.id,
    updatedAt,
    lastUserMessageAt: updatedAt,
    archived,
    pinned: false,
  };
}

function renderList(sessions: SessionRow[]) {
  return renderToStaticMarkup(
    <ArchivedSessionList
      tr={(key) => key}
      sessions={sessions}
      projects={[project]}
      sessionOrder={[]}
      sessionSortMode="updatedAt"
      session={IDLE_SNAPSHOT}
      busyIds={new Set()}
      unreadTerminalResults={new Map()}
      pendingAskUserSessionIds={new Set()}
      startSidebarDrag={vi.fn()}
      endSidebarDrag={vi.fn()}
      dropSession={vi.fn()}
      openSession={vi.fn()}
      openSessionMenu={vi.fn()}
      archiveSession={vi.fn()}
      pinSession={vi.fn()}
      deleteArchivedSession={vi.fn()}
    />,
  );
}

describe("ArchivedSessionList", () => {
  it("只显示归档会话，并按现有侧栏排序规则排序", () => {
    const html = renderList([
      session("active", false, "2026-09-20T00:00:00Z"),
      session("older-archived", true, "2026-09-01T00:00:00Z"),
      session("newer-archived", true, "2026-09-10T00:00:00Z"),
    ]);

    expect(html).toContain("sidebar.archived");
    expect(html).toContain("tree-l3--archived");
    expect(html).not.toContain("active");
    expect(html.indexOf("newer-archived")).toBeLessThan(
      html.indexOf("older-archived"),
    );
  });

  it("没有归档会话时显示明确空态", () => {
    const html = renderList([session("active", false, "2026-09-20T00:00:00Z")]);

    expect(html).toContain("sidebar.noArchivedSessions");
  });
});
