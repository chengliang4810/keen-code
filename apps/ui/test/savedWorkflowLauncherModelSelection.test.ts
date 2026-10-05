import { describe, expect, it, vi } from "vitest";
import type { ModelSelectionView } from "@zcode/services";
import { commandPayloadSchemas } from "@zcode/shared/zcode-protocol-v4";
import {
  buildSavedWorkflowCreateSessionPayload,
  readSavedWorkflowModelSelection,
} from "../../../packages/ui/src/settings/saved-workflows/useSavedWorkflowLauncher.js";

describe("saved workflow launch model binding", () => {
  const selection = {
    providerId: "native-live-deepseek",
    modelId: "deepseek-v4.1-flash",
    options: { reasoningLevel: "high" },
  } as const;

  it("reads and preserves the Host preferred selection for createSession", async () => {
    const getView = vi.fn(async (): Promise<ModelSelectionView> => ({
      revision: 7,
      providers: [],
      preferredSelection: selection,
    }));

    const modelSelection = await readSavedWorkflowModelSelection({ getView });

    expect(modelSelection).toEqual(selection);
    const payload = buildSavedWorkflowCreateSessionPayload(
      "D:/projects/native",
      modelSelection,
    );
    expect(payload).toEqual({
      workspaceId: "D:/projects/native",
      config: { modelSelection: selection },
    });
    // 通过实际 V4 payload schema，防止 selection 回到顶层后被 zod 静默剥离。
    expect(commandPayloadSchemas.createSession.parse(payload)).toEqual(payload);
    expect(getView).toHaveBeenCalledTimes(1);
  });

  it("rejects an Environment without a preferred selection instead of inventing a model", async () => {
    const getView = vi.fn(async (): Promise<ModelSelectionView> => ({
      revision: 8,
      providers: [],
    }));

    await expect(readSavedWorkflowModelSelection({ getView })).rejects.toThrow(
      "当前 Environment 没有可冻结的 Provider 模型选择",
    );
  });
});
