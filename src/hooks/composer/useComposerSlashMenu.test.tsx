import { renderToString } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { useComposerSlashMenu, type ComposerSlashMenuController } from "./useComposerSlashMenu";

vi.mock("@/lib/floatingMenu", () => ({ useFloatingMenu: () => ({ pos: null, style: undefined }) }));

it("编辑器事件直接更新查询，关闭后同一查询不重开，改变查询后恢复", () => {
  let menu!: ComposerSlashMenuController;
  function Harness() {
    menu = useComposerSlashMenu({
      locale: "zh", api: { isTauri: () => false } as never, projectPath: null,
      setDraft: vi.fn(), onAction: vi.fn(), composerInputRef: { current: null },
      composerShellRef: { current: null }, composerPlusTriggerRef: { current: null },
      composerPlusPanelRef: { current: null },
    });
    return null;
  }
  renderToString(<Harness />);
  menu.onSlashQueryChange({ start: 0, end: 3, query: "目标" });
  expect(menu.liveSlashRef.current).toEqual({ present: true, start: 0, end: 3, query: "目标" });
  menu.closeComposerMenu();
  menu.onSlashQueryChange({ start: 0, end: 3, query: "目标" });
  expect(menu.liveSlashRef.current.present).toBe(false);
  menu.onSlashQueryChange({ start: 0, end: 4, query: "目标新" });
  expect(menu.liveSlashRef.current.query).toBe("目标新");
  menu.onSlashQueryChange(null);
  expect(menu.liveSlashRef.current.present).toBe(false);
  menu.onSlashQueryChange({ start: 0, end: 1, query: "" });
  expect(menu.liveSlashRef.current.present).toBe(true);
});
