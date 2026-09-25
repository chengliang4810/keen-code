import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { Project } from "@/features/app/models";
import { IDLE_SNAPSHOT } from "@/lib/session";
import { ProjectTree } from "./ProjectTree";

const project: Project = {
  id: "project-1",
  name: "只读项目",
  path: "D:/projects/read-only",
  pathOk: false,
};

function renderTree(canWriteProjects: boolean) {
  return renderToStaticMarkup(
    <ProjectTree
      tr={(key) => key}
      projects={[project]}
      canWriteProjects={canWriteProjects}
      projectsOpen
      setProjectsOpen={vi.fn()}
      toggleProject={vi.fn()}
      expandedProjects={{ [project.id]: true }}
      setExpandedProjects={vi.fn()}
      projectDropHint={null}
      startSidebarDrag={vi.fn()}
      endSidebarDrag={vi.fn()}
      dragOverProject={vi.fn()}
      dropProject={vi.fn()}
      setProjectDropHint={vi.fn()}
      sessionsForProject={() => []}
      visibleSessionsByProject={{}}
      setVisibleSessionsByProject={vi.fn()}
      newChat={vi.fn()}
      dropSession={vi.fn()}
      session={IDLE_SNAPSHOT}
      busyIds={new Set()}
      unreadTerminalResults={new Map()}
      pendingAskUserSessionIds={new Set()}
      openProjectMenu={vi.fn()}
      relocateProject={vi.fn()}
      openSession={vi.fn()}
      openSessionMenu={vi.fn()}
      archiveSession={vi.fn()}
      pinSession={vi.fn()}
      applyProjectOrder={vi.fn()}
      addProject={vi.fn()}
      showToast={vi.fn()}
      sessionSortMode="updatedAt"
      onSessionSortModeChange={vi.fn()}
    />,
  );
}

describe("ProjectTree project write capability", () => {
  it("与 ZCode 一致地每批展示二十条项目会话", () => {
    const source = readFileSync(new URL("./ProjectTree.tsx", import.meta.url), "utf8");

    expect(source).toContain("visibleSessionsByProject[project.id] ?? 20");
    expect(source).toContain("[project.id]: visibleSessionCount + 20");
  });

  it("Web 只读投影保留项目选择但隐藏所有项目写入口", () => {
    const html = renderTree(false);

    expect(html).toContain("只读项目");
    expect(html).toContain('draggable="false"');
    expect(html).not.toContain("sidebar.addProject");
    expect(html).not.toContain("sidebar.menu");
    expect(html).not.toContain("sidebar.relocateProject");
    expect(html).not.toContain(project.path);
  });

  it("Desktop 仍显示项目写入口", () => {
    const html = renderTree(true);

    expect(html).toContain("sidebar.addProject");
    expect(html).toContain("tabler-icon-dots");
    expect(html).toContain("sidebar.relocateProject");
    expect(html).toContain('draggable="true"');
  });
});
