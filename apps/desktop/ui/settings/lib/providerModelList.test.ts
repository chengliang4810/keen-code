import { describe, expect, it } from "vitest";
import {
  formatModelContextWindow,
  reorderProviderModels,
} from "@/settings/lib/providerModelList";

describe("provider model list", () => {
  it("uses technical K/M units in every application language", () => {
    expect(formatModelContextWindow(128_000)).toBe("128K");
    expect(formatModelContextWindow(1_000_000)).toBe("1M");
    expect(formatModelContextWindow(1_048_576)).toBe("1M");
    expect(formatModelContextWindow(Number.NaN)).toBe("");
  });

  it("moves a model in both directions without changing its configuration", () => {
    const models = [
      { id: "first", enabled: false, reasoningLevels: [] },
      { id: "second", enabled: true, reasoningLevels: ["low"] },
      { id: "third", enabled: true, reasoningLevels: ["high"] },
    ];
    const next = reorderProviderModels(models, "first", "third");
    expect(next.map((model) => model.id)).toEqual(["second", "third", "first"]);
    expect(next[2]).toBe(models[0]);
    expect(reorderProviderModels(next, "first", "second")).toEqual(models);
    expect(models.map((model) => model.id)).toEqual([
      "first",
      "second",
      "third",
    ]);
  });

  it("ignores obsolete drag targets and unchanged order without saving", () => {
    const models = [{ id: "first" }, { id: "second" }];
    expect(reorderProviderModels(models, "deleted", "second")).toBe(models);
    expect(reorderProviderModels(models, "first", "deleted")).toBe(models);
    expect(reorderProviderModels(models, "first", "first")).toBe(models);
  });
});
