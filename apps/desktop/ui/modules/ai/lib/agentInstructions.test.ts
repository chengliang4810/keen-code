import { describe, expect, it } from "vitest";
import {
  buildStableSystem,
  DEFAULT_AGENT_INSTRUCTIONS,
} from "@/modules/ai/lib/agentInstructions";
import { BUILTIN_AGENTS } from "@/modules/ai/lib/agents";
import { tryRunSlashCommand } from "@/modules/ai/lib/slashCommands";

describe("agent instruction composition", () => {
  it("gives every built-in role the complete operating instructions once", () => {
    for (const agent of BUILTIN_AGENTS) {
      expect(agent.instructions.startsWith(DEFAULT_AGENT_INSTRUCTIONS)).toBe(
        true,
      );
      const system = buildStableSystem(agent, "GLOBAL_RULE", "PROJECT_RULE");
      expect(system.split(DEFAULT_AGENT_INSTRUCTIONS)).toHaveLength(2);
      expect(system.indexOf(agent.instructions)).toBeLessThan(
        system.indexOf("GLOBAL_RULE"),
      );
      expect(system.indexOf("GLOBAL_RULE")).toBeLessThan(
        system.indexOf("PROJECT_RULE"),
      );
      expect(system).not.toContain("RCODE.md");
    }
  });

  it("uses a custom role's full instructions without inserting another base prompt", () => {
    const system = buildStableSystem(
      { name: "My role", instructions: "CUSTOM_ROLE" },
      "GLOBAL_RULE",
      "PROJECT_RULE",
    );
    expect(system).not.toContain(DEFAULT_AGENT_INSTRUCTIONS);
    expect(system.indexOf("CUSTOM_ROLE")).toBeLessThan(
      system.indexOf("GLOBAL_RULE"),
    );
    expect(system.indexOf("GLOBAL_RULE")).toBeLessThan(
      system.indexOf("PROJECT_RULE"),
    );
  });

  it("keeps native tool instructions inside the current role, before both AGENTS.md files", () => {
    const system = buildStableSystem(
      BUILTIN_AGENTS[0],
      "GLOBAL_RULE",
      "PROJECT_RULE",
      "NATIVE_TOOLS",
    );
    expect(system.indexOf("NATIVE_TOOLS")).toBeLessThan(
      system.indexOf("GLOBAL_RULE"),
    );
    expect(system.endsWith("PROJECT_RULE")).toBe(true);
  });

  it("omits absent or blank instruction files", () => {
    const system = buildStableSystem(BUILTIN_AGENTS[0], " \n", null);
    expect(system).toContain(BUILTIN_AGENTS[0].instructions);
    expect(system).not.toContain("# Global instructions");
    expect(system).not.toContain("# Project instructions");
  });

  it("initializes AGENTS.md and preserves existing project instructions", () => {
    const command = tryRunSlashCommand("/init");
    if (command.kind !== "send-prompt")
      throw new Error("Expected initialization prompt");
    expect(command.prompt).toContain("produce AGENTS.md");
    expect(command.prompt).toContain("Preserve existing project instructions");
    expect(command.prompt).not.toContain("RCODE.md");
  });
});
