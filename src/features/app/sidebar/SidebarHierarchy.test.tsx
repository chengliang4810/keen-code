import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { IDLE_SNAPSHOT } from "@/lib/session";
import { readSource } from "../../../test-utils/readCssSource";
import { SidebarNav } from "./SidebarNav";
import { HistorySessionList } from "./HistorySessionList";

const foundationCss = readSource(
  new URL("../../../styles/app-foundation.css", import.meta.url),
);
const governanceCss = readSource(
  new URL("../../../styles/ui-governance.css", import.meta.url),
);
const sidebarSource = readSource(new URL("../Sidebar.tsx", import.meta.url));

describe("侧栏层级与密度", () => {
  it("项目工具栏只在交互时显示，并为 ZCode 的右侧内边距预留空间", () => {
    expect(foundationCss).toContain("padding-inline-end: 6px;");
    expect(foundationCss).toMatch(
      /\.tree-l1:hover \.tree-l1__actions,[\s\S]*?\.tree-l1:focus-within \.tree-l1__actions \{/,
    );
    expect(foundationCss).not.toContain(
      '.tree-l1:has([aria-expanded="true"]) .tree-l1__actions',
    );
  });

  it("项目和会话行使用统一的 hover surface、active surface 与圆角", () => {
    expect(foundationCss).toMatch(
      /\.tree-l2 \{[\s\S]*?border-radius: var\(--radius-md\);/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3 \{[\s\S]*?border-radius: var\(--radius-md\);/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l2:hover \{[\s\S]*?background: var\(--bg-hover\);/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3:hover \{[\s\S]*?background: var\(--bg-hover\);/,
    );
    expect(foundationCss).toContain("background: var(--bg-active);");
  });

  it("侧栏外壳裁剪溢出，由内部 OverlayScroll 视口独占滚动", () => {
    expect(foundationCss).toMatch(
      /\.sidebar \{[\s\S]*?overflow:\s*hidden;[\s\S]*?background:\s*var\(--bg-sidebar\);/,
    );
    expect(sidebarSource).toContain('<OverlayScroll');
    expect(sidebarSource).toContain('className="sidebar__scroll"');
    expect(sidebarSource).toContain(
      'viewportClassName="sidebar__scroll-inner"',
    );
  });

  it("桌面项目和会话行保持 32px，触屏恢复 44px 行高", () => {
    expect(foundationCss).toMatch(
      /\.tree-l2 \{[\s\S]*?height: 32px;[\s\S]*?min-height: 32px;[\s\S]*?max-height: 32px;/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3 \{[\s\S]*?height: 32px;[\s\S]*?min-height: 32px;[\s\S]*?max-height: 32px;/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3__actions \{[\s\S]*?right: 4px;[\s\S]*?top: 4px;/,
    );
    expect(foundationCss).toMatch(
      /@media \(max-width: 760px\), \(hover: none\), \(pointer: coarse\) \{[\s\S]*?\.tree-l2,[\s\S]*?\.tree-l3 \{[\s\S]*?height: var\(--ui-touch-target-touch\);/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3__actions \{[\s\S]*?width: calc\(var\(--ui-touch-target-touch\) \* 2\);/,
    );
  });

  it("短会话列表与虚拟列表保持 2px 分组间距，触屏动作常驻", () => {
    expect(foundationCss).toContain("padding: 4px 0 0;");
    expect(foundationCss).toMatch(
      /\.tree-l3-list \{[\s\S]*?gap: 2px;/,
    );
    expect(foundationCss).toMatch(
      /\.tree-orphan-list \{[\s\S]*?gap: 2px;/,
    );
    expect(foundationCss).toMatch(
      /\/\* ZCode keeps row actions reachable on touch devices[\s\S]*?\.tree-l2__actions,[\s\S]*?\.tree-l3__actions \{[\s\S]*?position: static;[\s\S]*?opacity: 1;[\s\S]*?pointer-events: auto;/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3__pin-action \{[\s\S]*?display: none;/,
    );
    expect(foundationCss).toMatch(
      /\/\* ZCode keeps row actions reachable on touch devices[\s\S]*?\.tree-l3__pin-action \{[\s\S]*?display: inline-flex;/,
    );
    expect(foundationCss).toMatch(
      /\.sidebar-empty--compact \{[\s\S]*?padding: 8px 12px;/,
    );
    expect(foundationCss).toMatch(
      /\.sidebar \.sidebar-empty \{[\s\S]*?font-size: var\(--text-md\);[\s\S]*?line-height: 20px;/,
    );
  });

  it("归档行使用双行元数据布局，并在触屏保持独立行高", () => {
    expect(foundationCss).toMatch(
      /\.tree-l3--archived \{[\s\S]*?height: 48px;[\s\S]*?min-height: 48px;[\s\S]*?max-height: 48px;/,
    );
    expect(foundationCss).toContain(".tree-l3__archive-meta");
    expect(foundationCss).toMatch(
      /\.tree-l3--archived \.tree-l3__title \{[\s\S]*?flex-direction: column;/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3--archived \{[\s\S]*?height: var\(--ui-touch-target-touch-plus-12\);[\s\S]*?min-height: var\(--ui-touch-target-touch-plus-12\);/,
    );
    expect(governanceCss).toContain(
      "--ui-touch-target-touch-plus-12: 56px;",
    );
  });

  it("归档模式保留置顶区，移动侧栏宽度不超过视口一半", () => {
    const archiveStart = sidebarSource.indexOf("{showArchivedSessions ?");
    const normalStart = sidebarSource.indexOf("          ) : (", archiveStart);

    expect(archiveStart).toBeGreaterThanOrEqual(0);
    expect(normalStart).toBeGreaterThan(archiveStart);
    const archiveBranch = sidebarSource.slice(archiveStart, normalStart);
    expect(archiveBranch.indexOf("<PinnedSessionList")).toBeLessThan(
      archiveBranch.indexOf("<ArchivedSessionList"),
    );
    expect(governanceCss).toMatch(
      /\.sidebar \{[\s\S]*?width: min\(var\(--sidebar-width, 264px\), 50%\) !important;[\s\S]*?max-width: 50% !important;/,
    );
  });

  it("主导航保留 KeenCode 现有入口，不引入 Automations", () => {
    const html = renderToStaticMarkup(
      <SidebarNav
        tr={(key) => key}
        newChat={vi.fn()}
        openSearch={vi.fn()}
        openPluginMarketplace={vi.fn()}
        searchTriggerRef={{ current: null }}
        showArchivedSessions={false}
        onToggleArchivedSessions={vi.fn()}
      />,
    );

    expect(html).toContain("sidebar.newSession");
    expect(html).toContain("sidebar.search");
    expect(html).toContain("sidebar.plugins");
    expect(html).toContain("sidebar.archived");
    expect(html).not.toContain("Automations");
  });

  it("没有孤立会话时不渲染 Other sessions 分组", () => {
    const html = renderToStaticMarkup(
      <HistorySessionList
        tr={(key) => key}
        orphanSessions={[]}
        historyOpen
        setHistoryOpen={vi.fn()}
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
      />,
    );

    expect(html).toBe("");
  });
});
