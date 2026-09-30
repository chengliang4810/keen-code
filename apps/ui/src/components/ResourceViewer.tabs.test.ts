import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

describe("ResourceViewer top tabs", () => {
  it("keeps singleton tools unique and only maps opened subagents to tabs", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./ResourceViewer.tsx", import.meta.url)),
      "utf8",
    );

    expect(source).toContain("current.includes(mode) ? current : [...current, mode]");
    expect(source).toContain("terminalTabs.map((tab)");
    expect(source).toContain("subagents.filter((agent)");
    expect(source).toContain("openSubagentIds.includes(agent.agent_id)");
    expect(source).toContain("setOpenSubagentIds((current)");
    expect(source).not.toContain("dismissedSubagents");
    expect(source).not.toContain("{subagents.map((agent) => <DropdownMenuItem");
    expect(source).toContain("setTerminalCreateRequest((request) => request + 1)");
    expect(source).toContain("onTabsChange={handleTerminalTabsChange}");
    expect(source).toContain("if (sideMode === mode) focusRemainingMode(mode)");
    expect(source).toContain('useSessionState<SingletonSideMode[]>(sessionKey, [])');
    expect(source).toContain('useSessionState<SideMode | null>(sessionKey, null)');
    expect(source).toContain('useSessionState<FileTab[]>(sessionKey, [])');
    expect(source).toContain('sessionKey={sessionKey}');
    // 无标签时使用 ZCode 式启动器，不再是旧的标签网格。
    expect(source).toContain("<SidePaneLauncher");
    expect(source).not.toContain("rp-tab-picker");
    // 单行标签条：模式标签、终端、子 Agent 与文件标签合并为同一组键。
    expect(source).toContain("const tabItemKeys = [");
    expect(source).toContain("...openSingletons.map((mode) => `singleton:${mode}`)");
    expect(source).toContain("...openSubagents.map((agent) => `subagent:${agent.agent_id}`)");
    expect(source).toContain("...visibleResourceTabs.map((t) => `file:${t.id}`)");
    expect(source).toContain('className="rp-tabs-bar"');
    expect(source).toContain("<SidePaneTabOverview");
    expect(source).toContain('className="rp-tabs__add"');
    expect(source).not.toContain("rp-file-tabs");
    expect(source).not.toContain("rp-mode-tabs");
    expect(source).toContain("setModeTabMenu({ x: event.clientX, y: event.clientY");
    expect(source).toContain('id: "close-others"');
    expect(source).toContain('id: "close-right"');
    expect(source).toContain('id: "close-left"');
    expect(source).toContain('id: "close-all"');
    expect(source).toContain("closeRequests={terminalCloseRequests}");
    expect(source).toContain("fontFamily={terminalFontFamily}");
    expect(source).toContain("onTabsEmpty?.()");
    expect(source).toContain('| "web"');
    expect(source).toContain('mode === "web" ? <IconWorld size={14} />');
    expect(source).toContain('mode === "web" ? tr("resources.web")');
    expect(source).toContain('current.includes("web") ? current : [...current, "web"]');
    expect(source).toContain('setSideMode("web")');
    expect(source).toContain('visibleResourceTabs.map((t)');
    // 标签中键关闭不会把非激活标签激活。
    expect(source).toContain("if (event.button !== 1) return;");
    expect(source).toContain("closeTabItemByKey(item.key)");
    expect(source).not.toContain("onClose?.()");
  });

  /**
   * Appica Tabs 的触发器基础类含 `data-active:pointer-events-none`，而 Base UI 的
   * `getStateAttributesProps` 会把激活状态写成 `data-active`；据此渲染时激活标签
   * 整体不可点击，其关闭按钮随之失效。标签条因此改用普通元素并自行承担交互语义。
   */
  it("does not build the tab strip from Appica Tabs triggers", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./ResourceViewer.tsx", import.meta.url)),
      "utf8",
    );

    expect(source).not.toContain("@/components/ui/tabs");
    expect(source).not.toContain("<TabsTrigger");
    expect(source).not.toContain("<TabsList");
    expect(source).toContain('role="tablist"');
    expect(source).toContain('role="tab"');
    expect(source).toContain("aria-selected={item.key === activeTabKey}");
    expect(source).toContain("tabIndex={item.key === activeTabKey ? 0 : -1}");
    expect(source).toContain("onClick={() => focusTabItemByKey(item.key)}");
  });

  it("keeps the active tab clickable so its close button works", () => {
    const css = readFileSync(
      fileURLToPath(new URL("../styles/app-resource.css", import.meta.url)),
      "utf8",
    );
    const tabBlock = css.slice(
      css.indexOf(".rp-tab {"),
      css.indexOf(".rp-tab__icon {"),
    );

    expect(tabBlock.length).toBeGreaterThan(0);
    // 激活标签必须保留指针事件，否则选中标签的关闭按钮点不到。
    expect(tabBlock).not.toContain("pointer-events: none");
    expect(css).toContain(".rp-tab:hover .rp-tab__x");
  });
});
