import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

describe("ResourceViewer top tabs", () => {
  it("keeps singleton tools unique and maps terminals and subagents to tabs", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./ResourceViewer.tsx", import.meta.url)),
      "utf8",
    );

    expect(source).toContain("current.includes(mode) ? current : [...current, mode]");
    expect(source).toContain("terminalTabs.map((tab)");
    expect(source).toContain("subagents.filter((agent)");
    expect(source).not.toContain("{subagents.map((agent) => <DropdownMenuItem");
    expect(source).toContain("setTerminalCreateRequest((request) => request + 1)");
    expect(source).toContain("onTabsChange={handleTerminalTabsChange}");
    expect(source).toContain("if (sideMode === mode) focusRemainingMode(mode)");
    expect(source).toContain('useState<SideMode | null>(null)');
    expect(source).toContain('useState<SingletonSideMode[]>([])');
    expect(source).toContain('className="rp-tab-picker"');
    expect(source).not.toContain("onClose?.()");
  });
});
