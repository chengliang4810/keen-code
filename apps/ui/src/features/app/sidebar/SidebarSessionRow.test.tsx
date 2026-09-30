import { describe, expect, it } from "vitest";
import { readSource } from "../../../test-utils/readCssSource";
import { readFileSync } from "node:fs";

const foundationCss = readSource(
  new URL("../../../styles/app-foundation.css", import.meta.url),
);
const source = readFileSync(new URL("./SidebarSessionRow.tsx", import.meta.url), "utf8");

describe("SidebarSessionRow metadata and actions", () => {
  it("归档项目名不通过原生 title 暴露完整路径", () => {
    expect(source).toContain('className="tree-l3__archive-workspace"');
    expect(source).not.toContain("title={project?.path ?? undefined}");
  });

  it("状态固定在左侧 leading 槽，待回答 badge 保留在标题行", () => {
    expect(source).toContain('className="tree-l3__leading tree-l3__kind"');
    expect(source).toContain('className="tree-l3__trailing"');
    expect(source).toContain('className="tree-l3__meta"');

    const leading = source.slice(
      source.indexOf('className="tree-l3__leading tree-l3__kind"'),
      source.indexOf('className="tree-l3__title"'),
    );
    // 运行中只看 working：恢复历史的 connecting 属于对话区的加载语义，
    // 不能借侧栏“工作中”图标短暂闪现。
    expect(leading).toContain("{working ? (");
    expect(leading).not.toContain("loading");
    expect(leading).toContain("unreadResult ?");
    expect(leading).toContain("session.pinned ?");
    expect(leading.indexOf("{working ? (")).toBeLessThan(
      leading.indexOf("unreadResult ?"),
    );
    expect(leading.indexOf("unreadResult ?")).toBeLessThan(
      leading.indexOf("session.pinned ?"),
    );

    const title = source.slice(
      source.indexOf('className="tree-l3__title"'),
      source.indexOf('className="tree-l3__trailing"'),
    );
    expect(title).toContain("needsInput ?");
    expect(title).not.toContain("session.pinned ?");

    const metadata = source.slice(source.indexOf('className="tree-l3__meta"'));
    expect(metadata).toContain("relativeTime ?");
    expect(metadata).not.toContain("{working ? (");
    expect(metadata).not.toContain("unreadResult ?");
    expect(metadata).not.toContain("needsInput ?");
  });

  it("运行中使用三点跳动动效，并提供无障碍标签与 reduced-motion 降级", () => {
    const working = source.slice(
      source.indexOf('className="tree-l3__status tree-l3__status--working"'),
      source.indexOf(") : unreadResult ? ("),
    );
    // role=status 挂在带 aria-label 的槽上，装饰性圆点自身对读屏隐藏。
    expect(working).toContain('role="status"');
    expect(working).toContain('aria-label={tr("sidebar.sessionWorking")}');
    expect(working).toContain('className="tree-l3__working-dots" aria-hidden');
    expect(working.match(/tree-l3__working-dot"/g)).toHaveLength(3);
    expect(source).not.toContain("tree-l3__spinner");

    // 着色来自 --working 修饰类，避免动效换掉后丢失 accent 色。
    expect(foundationCss).toMatch(
      /\.tree-l3__status--working \{[\s\S]*?color:\s*var\(--accent/,
    );
    expect(foundationCss).not.toMatch(/tree-l3__status--loading/);
    expect(foundationCss).toMatch(
      /\.tree-l3__working-dot \{[\s\S]*?animation:\s*tree-l3-working-bounce/,
    );
    expect(foundationCss).toMatch(
      /@media \(prefers-reduced-motion: reduce\)[\s\S]*?\.tree-l3__working-dot \{[\s\S]*?animation:\s*none;/,
    );
  });

  it("四个会话列表都只按 busyIds 判定运行中，不再传入恢复态 loading", () => {
    for (const file of [
      "./ProjectTree.tsx",
      "./HistorySessionList.tsx",
      "./PinnedSessionList.tsx",
      "./ArchivedSessionList.tsx",
    ]) {
      const listSource = readFileSync(
        new URL(file, import.meta.url),
        "utf8",
      );
      expect(listSource).toContain("working={busyIds.has(item.id)}");
      expect(listSource).not.toContain("loading={");
      expect(listSource).not.toContain('session.state === "connecting"');
    }
  });

  it("长标题使用渐隐和延迟走马灯，右侧 metadata 不固定占用标题宽度", () => {
    expect(source).toContain("<TaskTitleOverflowText");
    expect(foundationCss).toMatch(
      /\.tree-l3__trailing \{[\s\S]*?min-width:\s*0;/,
    );
    expect(foundationCss).toContain(".task-title-overflow-text--overflowing");
    expect(foundationCss).toContain(".task-title-marquee-track");
  });

  it("hover、focus、菜单打开时让 metadata 让位给右侧动作簇", () => {
    expect(foundationCss).toMatch(
      /\.tree-l3:hover \.tree-l3__meta,[\s\S]*?\.tree-l3:focus-within \.tree-l3__meta,[\s\S]*?\.tree-l3--metadata-suppressed \.tree-l3__meta,[\s\S]*?\.tree-l3--menu-open \.tree-l3__meta \{[\s\S]*?display:\s*none;/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3:hover \.tree-l3__actions,[\s\S]*?\.tree-l3:focus-within \.tree-l3__actions,[\s\S]*?\.tree-l3--actions-visible \.tree-l3__actions,[\s\S]*?\.tree-l3--menu-open \.tree-l3__actions \{[\s\S]*?position:\s*static;[\s\S]*?opacity:\s*1;[\s\S]*?pointer-events:\s*auto;/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3__actions \{[\s\S]*?position:\s*absolute;[\s\S]*?right:\s*4px;[\s\S]*?top:\s*4px;/,
    );
    expect(source).toContain("tree-l3--menu-open");
    expect(source).toContain("setMenuOpen(true)");
  });

  it("桌面端不把隐藏动作放进 Tab 顺序，触屏端仍挂载动作簇", () => {
    expect(source).toContain("const actionsVisible =");
    expect(source).toContain("{actionsVisible ? (");
    expect(source).toContain("tabIndex={actionsVisible ? 0 : -1}");
    expect(source).toContain("data-task-item-key={session.id}");
    expect(source).toContain('window.matchMedia("(hover: none), (pointer: coarse)")');
    expect(source).toContain("!noHoverDevice ?");
    expect(foundationCss).toMatch(
      /@media \(max-width: 760px\), \(hover: none\), \(pointer: coarse\) \{[\s\S]*?\.tree-l3__actions \{[\s\S]*?position:\s*static;[\s\S]*?opacity:\s*1;[\s\S]*?pointer-events:\s*auto;/,
    );
    const touchBlock = foundationCss.slice(
      foundationCss.lastIndexOf(
        "@media (max-width: 760px), (hover: none), (pointer: coarse)",
      ),
    );
    expect(touchBlock).not.toContain("tree-l3__meta");
    expect(foundationCss).toMatch(
      /\.tree-l3__actions:not\(\.tree-l3__actions--with-pin\) \{[\s\S]*?width:\s*var\(--ui-touch-target-touch\);[\s\S]*?min-width:\s*var\(--ui-touch-target-touch\);/,
    );
    expect(foundationCss).toMatch(
      /\.tree-l3__trailing \{[\s\S]*?min-width:\s*var\(--ui-touch-target-touch\);/,
    );
  });

  it("保留 KeenCode 现有的 Pin、归档和菜单动作，不引入外部业务入口", () => {
    expect(source).toContain('"tree-l3__actions" +');
    expect(source).toContain('" tree-l3__actions--with-pin"');
    expect(source).toContain("pinSession(session, !session.pinned)");
    expect(source).toContain("archiveSession(session, !session.archived)");
    expect(source).toContain("openSessionMenu(event, session)");
    expect(source).not.toContain("Automations");
    expect(source).not.toContain("tree-l3__actions--triple");
  });
});
