import React from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ComposerModelMenu,
  groupComposerModelsByProvider,
} from "./ComposerModelMenu";

describe("ComposerModelMenu", () => {
  it("按供应商生成 Invite users 式级联菜单数据", () => {
    expect(
      groupComposerModelsByProvider([
        {
          providerId: "openai",
          providerLabel: "OpenAI",
          id: "gpt-5",
          label: "GPT-5",
        },
        {
          providerId: "anthropic",
          providerLabel: "Anthropic",
          id: "claude-sonnet",
          label: "Claude Sonnet",
        },
        {
          providerId: "openai",
          providerLabel: "OpenAI",
          id: "gpt-5-mini",
          label: "GPT-5 mini",
        },
      ]),
    ).toEqual([
      {
        id: "openai",
        label: "OpenAI",
        models: [
          {
            providerId: "openai",
            providerLabel: "OpenAI",
            id: "gpt-5",
            label: "GPT-5",
          },
          {
            providerId: "openai",
            providerLabel: "OpenAI",
            id: "gpt-5-mini",
            label: "GPT-5 mini",
          },
        ],
      },
      {
        id: "anthropic",
        label: "Anthropic",
        models: [
          {
            providerId: "anthropic",
            providerLabel: "Anthropic",
            id: "claude-sonnet",
            label: "Claude Sonnet",
          },
        ],
      },
    ]);
  });

  it("无供应商模型时只显示添加模型入口", () => {
    const html = renderToString(
      React.createElement(ComposerModelMenu, {
        modelId: "",
        effort: "medium",
        models: [],
        labels: {
          model: "模型",
          addModel: "添加模型",
          effort: "推理强度",
          reasoningSupported: "支持",
          reasoningUnsupported: "不支持",
          effortNone: "关闭",
          effortMinimal: "最小",
          effortHigh: "高",
          effortMedium: "中",
          effortLow: "低",
          effortXHigh: "极高",
          effortMax: "最大",
        },
        onModel: () => {},
        onEffort: () => {},
        onAddModel: () => {},
      }),
    );

    expect(html).toContain("添加模型");
    expect(html).not.toContain("推理强度");
  });
});
