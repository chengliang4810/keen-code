import { describe, expect, it } from "vitest";
import {
  applyToolPermissions,
  normalizePermissionMode,
} from "@/modules/ai/lib/permissions";
import { buildTools } from "@/modules/ai/tools/tools";
import type { ToolContext } from "@/modules/ai/tools/context";

const context: ToolContext = {
  getCwd: () => "D:/workspace",
  getWorkspaceRoot: () => "D:/workspace",
  getSessionId: () => "session",
  getTerminalContext: () => null,
  isActiveTerminalPrivate: () => false,
  injectIntoActivePty: () => false,
  openPreview: () => false,
  spawnAgent: () => null,
  readAgentOutput: () => null,
  readCache: new Map(),
};

describe("SDK permission policy", () => {
  it("defaults conversation permissions to auto approval without granting full access", () => {
    for (const value of [undefined, null, "ask", "edit", "yolo", "auto", 1]) {
      const mode = normalizePermissionMode(value);
      expect(mode).toBe("edit");
      const tools = applyToolPermissions(buildTools(context), mode);
      expect(tools.write_file.needsApproval).toBe(false);
      expect(tools.bash_run.needsApproval).toBe(true);
    }
    expect(normalizePermissionMode("full-access")).toBe("full-access");
  });

  it("retains individual confirmation for tools without an explicit run policy", () => {
    const tools = applyToolPermissions(buildTools(context));
    for (const name of [
      "write_file",
      "edit",
      "multi_edit",
      "create_directory",
      "bash_run",
      "spawn_coding_agent",
      "send_to_agent",
    ] as const)
      expect(tools[name].needsApproval, name).toBe(true);
    expect(tools.read_file.needsApproval).toBeUndefined();
  });

  it("automates workspace edits while retaining command and agent confirmation", () => {
    const originals = buildTools(context);
    const tools = applyToolPermissions(originals, "edit");
    for (const name of [
      "write_file",
      "edit",
      "multi_edit",
      "create_directory",
    ] as const) {
      expect(tools[name].needsApproval, name).toBe(false);
      expect(tools[name].execute).toBe(originals[name].execute);
    }
    for (const name of [
      "bash_run",
      "spawn_coding_agent",
      "send_to_agent",
    ] as const)
      expect(tools[name].needsApproval, name).toBe(true);
    expect(originals.write_file.needsApproval).toBe(true);
  });

  it("full access retains the original safety checks and plan mode exposes only reads", async () => {
    const originals = buildTools(context);
    const tools = applyToolPermissions(originals, "full-access");
    expect(tools.bash_run.needsApproval).toBe(false);
    expect(tools.bash_run.execute).toBe(originals.bash_run.execute);
    expect(tools.write_file.needsApproval).toBe(false);
    const result = await tools.write_file.execute?.(
      { path: "D:/workspace/.env.local", content: "denied" },
      { toolCallId: "secret", messages: [] },
    );
    expect(result).toEqual(
      expect.objectContaining({ error: expect.any(String) }),
    );
    const plan = applyToolPermissions(originals, "full-access", true);
    for (const name of [
      "write_file",
      "edit",
      "multi_edit",
      "create_directory",
      "bash_run",
      "spawn_coding_agent",
      "send_to_agent",
    ])
      expect(plan).not.toHaveProperty(name);
    expect(plan.read_file.execute).toBe(originals.read_file.execute);
    expect(plan).not.toHaveProperty("run_subagent");
  });
});
