import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { GitWorktreeEntry } from "@/lib/api";
import type { Project } from "@/features/app/models";
import { DEFAULT_LAYOUT } from "@/lib/layout";
import { IDLE_SNAPSHOT } from "@/lib/session";
import { readSource } from "@/test-utils/readCssSource";
import {
  MainHeader,
  resolveHeaderWorktreeBranch,
} from "./MainHeader";

const project: Project = {
  id: "project-current",
  name: "KeenCode",
  path: "D:/projects/keen-code",
  pathOk: true,
};

const sibling: Project = {
  id: "project-sibling",
  name: "ZCode",
  path: "D:/projects/ZCode",
  pathOk: true,
};

const worktree = (overrides: Partial<GitWorktreeEntry> = {}): GitWorktreeEntry => ({
  path: project.path,
  head: "abc1234",
  branch: "feat/header-context",
  detached: false,
  isMain: false,
  locked: false,
  prunable: false,
  ...overrides,
});

function renderHeader() {
  return renderToStaticMarkup(
    <MainHeader
      layout={DEFAULT_LAYOUT}
      setLayout={vi.fn()}
      useCustomWindowChrome={false}
      toggleMaximizeFromTitlebar={vi.fn()}
      tr={(key) => key}
      sessions={[]}
      activeProject={project}
      projects={[project, sibling]}
      gitWorktrees={[worktree()]}
      bindSessionProject={vi.fn().mockResolvedValue(undefined)}
      session={IDLE_SNAPSHOT}
      summaryOpen={false}
      summaryTriggerRef={{ current: null }}
      setSummaryOpen={vi.fn()}
      openSessionMenu={vi.fn()}
      newChat={vi.fn()}
      canGoBack={false}
      canGoForward={false}
      goBack={vi.fn()}
      goForward={vi.fn()}
    />,
  );
}

describe("MainHeader workspace context", () => {
  it("uses the matching worktree branch and supports detached worktrees", () => {
    expect(
      resolveHeaderWorktreeBranch(project.path, [worktree()], "detached"),
    ).toBe("feat/header-context");
    expect(
      resolveHeaderWorktreeBranch(
        "d:/PROJECTS/KEEN-CODE/",
        [worktree({ branch: undefined, detached: true })],
        "detached",
      ),
    ).toBe("detached");
    expect(resolveHeaderWorktreeBranch(project.path, [], "detached")).toBeNull();
  });

  it("renders a real clickable context trigger with path, branch, and project choices", () => {
    const html = renderHeader();
    const source = readSource(new URL("./MainHeader.tsx", import.meta.url));

    expect(html).toContain('data-testid="main-project-context"');
    // 上下文触发器统一渲染官方 Appica md 几何（h-10）。
    expect(html).toContain("h-10");
    expect(html).toContain('aria-label="KeenCode · feat/header-context"');
    // Appica renders DropdownMenuContent in a client portal, so SSR omits the
    // menu body. Keep a source contract for the actual browser-rendered body.
    expect(source).toContain("main__project-context-path");
    expect(source).toContain("{activeProject.path}");
    expect(source).toContain("currentBranch");
    expect(source).toContain("DropdownMenuRadioGroup");
    expect(source).toContain("{project.name}");
  });

  it("keeps title/status and actions in separate no-drag chrome slots", () => {
    const source = readSource(new URL("./MainHeader.tsx", import.meta.url));
    const css = readSource(
      new URL("../../../styles/app-conversation.css", import.meta.url),
    );

    expect(source).toContain('<div className="main__title-row">');
    expect(source).toContain('<div className="main__top-actions">');
    expect(source).not.toContain(
      '<div className="main__title-row" data-tauri-drag-region>',
    );
    expect(source).not.toContain(
      '<h1 className="main__title" data-tauri-drag-region>',
    );
    expect(css).toMatch(
      /\.main__title-row\s*\{[\s\S]*?flex: 0 1 auto;[\s\S]*?min-width: 0;[\s\S]*?-webkit-app-region: no-drag;[\s\S]*?app-region: no-drag;/,
    );
    expect(css).toMatch(
      /\.main__top-actions\s*\{[\s\S]*?flex-shrink: 0;[\s\S]*?-webkit-app-region: no-drag;[\s\S]*?app-region: no-drag;/,
    );
  });

  it("uses the header container for title shrink breakpoints", () => {
    const css = readSource(
      new URL("../../../styles/app-conversation.css", import.meta.url),
    );
    const titleStart = css.indexOf(".main__title {");
    const titleEnd = css.indexOf(".main__project-context {", titleStart);
    const titleCss = css.slice(titleStart, titleEnd);

    expect(css).toMatch(
      /\.main__top\s*\{[\s\S]*?container-type:\s*inline-size;[\s\S]*?container-name:\s*main-header;/,
    );
    expect(titleCss).toContain("max-width: min(25rem, 42cqw);");
    expect(css).toContain("@container main-header (max-width: 760px)");
    expect(css).toContain("max-width: min(50cqw, 25rem);");
    expect(css).toContain("@container main-header (max-width: 560px)");
    expect(css).toContain("max-width: 30cqw;");
    expect(css).toContain("@container main-header (max-width: 420px)");
    expect(css).toContain("max-width: 22cqw;");
    expect(css).toMatch(
      /@container main-header \(max-width: 760px\)[\s\S]*?\.main__task-nav\s*\{[^}]*display:\s*none;/s,
    );
    expect(css).not.toMatch(
      /@media \(max-width: 760px\)[\s\S]*?\.main__task-nav\s*\{[^}]*display:\s*none;/s,
    );
    expect(css).not.toContain("@media (max-width: 560px)");
    expect(css).not.toContain("@media (max-width: 420px)");
  });
});
