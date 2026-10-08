import { describe, expect, it } from "vitest";
import { labelFor } from "./tabLabel";
import type { TerminalTab } from "./useTabs";

function terminalTab(over: Partial<TerminalTab> = {}): TerminalTab {
  return {
    id: 1,
    kind: "terminal",
    spaceId: "default",
    title: "shell",
    paneTree: { kind: "leaf", id: 2 },
    activeLeafId: 2,
    ...over,
  };
}

describe("labelFor (terminal tabs)", () => {
  it("derives the label from the last cwd segment", () => {
    expect(labelFor(terminalTab({ cwd: "/Users/me/projects/rcode-ai" }))).toBe(
      "rcode-ai",
    );
  });

  it("falls back to the title when there is no cwd", () => {
    expect(labelFor(terminalTab({ title: "private" }))).toBe("private");
  });

  it("prefers a custom title over the cwd-derived name", () => {
    expect(
      labelFor(
        terminalTab({
          cwd: "/Users/me/projects/rcode-ai",
          customTitle: "Server",
        }),
      ),
    ).toBe("Server");
  });

  it("keeps the custom title after the cwd changes (survives cd)", () => {
    const renamed = terminalTab({ cwd: "/Users/me/a", customTitle: "Server" });
    const afterCd = { ...renamed, cwd: "/Users/me/b/c" };
    expect(labelFor(afterCd)).toBe("Server");
  });

  it("handles Windows-style cwd separators", () => {
    expect(labelFor(terminalTab({ cwd: "C:\\Users\\me\\proj" }))).toBe("proj");
  });

  it("localizes only default labels and preserves user names, paths, and stored data", () => {
    const labels: Record<string, string> = {
      blocks: "块式终端",
      private: "隐私",
    };
    const localize = (message: string) =>
      labels[message] ?? `translated:${message}`;
    const defaultTab = terminalTab({ title: "blocks" });
    expect(labelFor(defaultTab, localize)).toBe("块式终端");
    expect(defaultTab.title).toBe("blocks");
    expect(
      labelFor(
        terminalTab({ title: "private", customTitle: "Copy" }),
        localize,
      ),
    ).toBe("Copy");
    expect(labelFor(terminalTab({ cwd: "D:/projects/blocks" }), localize)).toBe(
      "blocks",
    );
  });
});
