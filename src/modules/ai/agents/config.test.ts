import { describe, expect, it } from "vitest";
import {
  configuredSubagents,
  EMPTY_SUBAGENT_CONFIG,
  parseSubagentConfig,
  resolveSubagentModel,
  subagentSystem,
} from "@/modules/ai/agents/config";
import {
  endpointModelSelectionId,
  type CustomEndpoint,
} from "@/modules/ai/config";

const profile = {
  id: "custom-review",
  label: "review",
  description: "Review",
  systemPrompt: "RULES",
  tools: ["grep"],
  enabled: false,
  color: "blue",
  injectAgentsMd: false,
};
describe("subagent configuration boundary", () => {
  it("keeps built-ins independent from main roles and preserves disabled user profiles", () => {
    const config = parseSubagentConfig({
      ...EMPTY_SUBAGENT_CONFIG,
      custom: [profile],
    });
    const agents = configuredSubagents(config);
    expect(agents).toHaveLength(5);
    expect(agents[4]).toMatchObject({ enabled: false, builtIn: false });
    expect(
      agents.slice(0, 4).every((agent) => agent.builtIn && agent.enabled),
    ).toBe(true);
  });
  it("rejects mutation, recursive delegation, unknown fields, duplicate IDs and duplicate names", () => {
    for (const tools of [
      ["write_file"],
      ["bash_run"],
      ["run_subagent"],
      ["grep", "grep"],
    ])
      expect(() =>
        parseSubagentConfig({
          ...EMPTY_SUBAGENT_CONFIG,
          custom: [{ ...profile, tools }],
        }),
      ).toThrow();
    for (const custom of [
      [profile, profile],
      [profile, { ...profile, id: "custom-other" }],
      [{ ...profile, permissions: "full-access" }],
      [{ ...profile, id: "explore" }],
    ])
      expect(() =>
        parseSubagentConfig({ ...EMPTY_SUBAGENT_CONFIG, custom }),
      ).toThrow();
    expect(() =>
      parseSubagentConfig({
        ...EMPTY_SUBAGENT_CONFIG,
        overrides: [{ id: "explore", systemPrompt: "REPLACED" }],
      }),
    ).toThrow();
  });
  it("supports zero tools and injects only global and project rules on explicit opt-in", () => {
    const config = parseSubagentConfig({
      ...EMPTY_SUBAGENT_CONFIG,
      custom: [{ ...profile, tools: [] }],
    });
    const agent = configuredSubagents(config)[4];
    expect(agent.tools).toEqual([]);
    expect(subagentSystem(agent, "GLOBAL", "PROJECT")).toBe("RULES");
    expect(
      subagentSystem({ ...agent, injectAgentsMd: true }, "GLOBAL", "PROJECT"),
    ).toBe("RULES\n\nGLOBAL\n\nPROJECT");
  });
  it("inherits the frozen parent selection and validates an explicit model and effort without fallback", () => {
    const endpoint: CustomEndpoint = {
      id: "test",
      name: "Test",
      baseURL: "https://example.com/v1",
      modelId: "model",
      contextLimit: 128000,
      models: [{ id: "model", reasoningLevels: ["low", "high"] }],
    };
    const modelId = endpointModelSelectionId(endpoint, { id: "model" });
    expect(
      resolveSubagentModel(
        {},
        { modelId: "parent", reasoningLevel: "low" },
        [],
      ),
    ).toEqual({ modelId: "parent", reasoningLevel: "low" });
    expect(resolveSubagentModel({ modelId }, {}, [endpoint])).toEqual({
      modelId,
      reasoningLevel: "high",
    });
    expect(
      resolveSubagentModel({ modelId, reasoningLevel: "low" }, {}, [endpoint])
        .reasoningLevel,
    ).toBe("low");
    expect(() =>
      resolveSubagentModel({ modelId, reasoningLevel: "ultra" }, {}, [
        endpoint,
      ]),
    ).toThrow();
    expect(() =>
      resolveSubagentModel({ modelId }, {}, [
        { ...endpoint, models: [{ id: "model", enabled: false }] },
      ]),
    ).toThrow();
  });
});
