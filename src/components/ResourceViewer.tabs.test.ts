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
    expect(source).toContain('className="rp-tab-picker"');
    expect(source).toContain("{hasModeTabs ? (");
    expect(source).toContain("setModeTabMenu({ x: event.clientX, y: event.clientY");
    expect(source).toContain('id: "close-others"');
    expect(source).toContain('id: "close-right"');
    expect(source).toContain('id: "close-left"');
    expect(source).toContain('id: "close-all"');
    expect(source).toContain("closeRequests={terminalCloseRequests}");
    expect(source).toContain("fontFamily={terminalFontFamily}");
    expect(source).toContain("onTabsEmpty?.()");
    expect(source).toContain("subagents.some((agent) => openSubagentIds.includes(agent.agent_id))");
    expect(source).not.toContain("onClose?.()");
  });
});
