import { afterEach, describe, expect, it, vi } from "vitest";
import { traceEager } from "../../../../../../scripts/eager-graph.mjs";
import { uiState } from "@/lib/uiState";
import { terminalDiagnosticsEnabled } from "@/modules/terminal/lib/terminalDiagnosticsEnabled";

describe("terminal diagnostic startup", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  it("loads independently of xterm and its renderer pool", () => {
    const graph = traceEager(
      "apps/desktop/ui/modules/terminal/lib/terminalDiagnostics.ts",
      ["@xterm"],
    );
    expect([...graph.hits.keys()]).toEqual([]);
  });

  it("has no release startup work until diagnostics are enabled", () => {
    vi.stubEnv("DEV", false);
    const getItem = vi.spyOn(uiState, "getItem").mockReturnValue(null);
    vi.stubGlobal("window", {});
    expect(terminalDiagnosticsEnabled()).toBe(false);
    getItem.mockReturnValue("1");
    expect(terminalDiagnosticsEnabled()).toBe(true);
  });

  it("tolerates unavailable UI state", () => {
    vi.stubEnv("DEV", false);
    vi.stubGlobal("window", {});
    vi.spyOn(uiState, "getItem").mockImplementation(() => { throw new Error("denied"); });
    expect(terminalDiagnosticsEnabled()).toBe(false);
  });
});
