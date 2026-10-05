import { describe, expect, it } from "vitest";

import { buildDraftCreateConfigPayload } from "../src/v4/composer/useDraftConfigControl.js";

describe("createSession 草稿 reasoning 配置", () => {
  it("省略空 thought，避免空值阻塞 createSession", () => {
    expect(
      buildDraftCreateConfigPayload({
        mode: "build",
        provider: "provider",
        model: "model",
        thought: " \t\n",
      }),
    ).toEqual({
      config: {
        mode: "build",
        provider: "provider",
        model: "model",
      },
    });
  });

  it("保留显式 none，而不是把有效档位当作空值", () => {
    expect(
      buildDraftCreateConfigPayload({
        mode: "build",
        provider: "provider",
        model: "model",
        thought: "none",
        modelSelection: {
          providerId: "provider",
          modelId: "model",
          options: { reasoningLevel: "none" },
        },
      }),
    ).toEqual({
      config: {
        mode: "build",
        provider: "provider",
        model: "model",
        thought: "none",
        modelSelection: {
          providerId: "provider",
          modelId: "model",
          options: { reasoningLevel: "none" },
        },
      },
    });
  });

  it("保留未知非空值，交由 Rust 的固定枚举校验拒绝", () => {
    expect(buildDraftCreateConfigPayload({ thought: "unsupported" })).toEqual({
      config: { thought: "unsupported" },
    });
  });
});
