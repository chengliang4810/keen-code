import { describe, expect, it } from "vitest";
import {
  type CustomEndpoint,
  compatModelIdForEndpoint,
} from "@/modules/ai/config";
import {
  configuredCustomModelIds,
  isConfiguredCustomModel,
  NO_MODEL_ID,
  selectConfiguredCustomModel,
} from "@/modules/ai/lib/modelSelection";

const endpoint: CustomEndpoint = {
  id: "gateway",
  name: "Gateway",
  baseURL: "https://example.com/v1",
  modelId: "first",
  contextLimit: 128000,
  models: [
    { id: "first" },
    { id: "disabled", enabled: false },
    { id: "second" },
  ],
};

describe("custom provider selection", () => {
  it("exposes only usable custom models and never a built-in default", () => {
    expect(configuredCustomModelIds([endpoint])).toEqual([
      "compat-gateway/first",
      "compat-gateway/second",
    ]);
    expect(isConfiguredCustomModel("gpt-5.4-mini", [endpoint])).toBe(false);
    expect(selectConfiguredCustomModel("gpt-5.4-mini", [endpoint])).toBe(
      "compat-gateway/first",
    );
  });
  it("requires a configured endpoint and an enabled, nonempty model", () => {
    expect(configuredCustomModelIds([{ ...endpoint, baseURL: " " }])).toEqual(
      [],
    );
    expect(isConfiguredCustomModel("compat-gateway/disabled", [endpoint])).toBe(
      false,
    );
    expect(isConfiguredCustomModel("compat-gateway/removed", [endpoint])).toBe(
      false,
    );
    expect(selectConfiguredCustomModel("compat-gateway/first", [])).toBe(
      NO_MODEL_ID,
    );
  });
  it("preserves the selected model and canonicalizes a legacy model identity", () => {
    const preferred = compatModelIdForEndpoint(endpoint.id, "second");
    expect(selectConfiguredCustomModel(preferred, [endpoint])).toBe(preferred);
    expect(selectConfiguredCustomModel("compat-gateway", [endpoint])).toBe(
      "compat-gateway/first",
    );
    const legacy = { ...endpoint, models: undefined };
    expect(selectConfiguredCustomModel("compat-gateway", [legacy])).toBe(
      "compat-gateway",
    );
  });
  it("returns an empty selection when all models are removed or disabled", () => {
    expect(
      selectConfiguredCustomModel("compat-gateway/first", [
        { ...endpoint, models: [] },
      ]),
    ).toBe(NO_MODEL_ID);
    expect(
      selectConfiguredCustomModel("compat-gateway/first", [
        { ...endpoint, models: [{ id: "first", enabled: false }] },
      ]),
    ).toBe(NO_MODEL_ID);
  });
});
