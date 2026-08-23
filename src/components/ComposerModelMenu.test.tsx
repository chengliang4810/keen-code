import React from "react";
import { readFileSync } from "node:fs";
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
          providerId: "provider-a",
          providerLabel: "Provider A",
          id: "model-a",
          label: "Model A",
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
        id: "provider-a",
        label: "Provider A",
        models: [
          {
            providerId: "provider-a",
            providerLabel: "Provider A",
            id: "model-a",
            label: "Model A",
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

  it("模型菜单在 SSR 下可渲染", () => {
    const html = renderToString(
      <ComposerModelMenu
        modelId="gpt-5"
        effort="medium"
        models={[
          {
            providerId: "openai",
            providerLabel: "OpenAI",
            id: "gpt-5",
            label: "GPT-5",
            reasoningSupported: true,
            reasoningEfforts: [{ id: "medium" }],
          },
        ]}
        labels={{
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
        }}
        onModel={() => {}}
        onEffort={() => {}}
        onAddModel={() => {}}
      />,
    );

    expect(html).toContain('aria-label="模型"');
  });

  it("触发器不显示模型图标，模型子菜单限高滚动且推理强度靠右", () => {
    const source = readFileSync(
      new URL("./ComposerModelMenu.tsx", import.meta.url),
      "utf8",
    );
    const cssSource = readFileSync(
      new URL("../styles/app.css", import.meta.url),
      "utf8",
    );

    expect(source).not.toContain("IconBolt");
    expect(source).toContain("cmm__model-list");
    expect(source).toContain("cmm__effort-value");
    expect(cssSource).toMatch(
      /\.cmm__model-list\s*\{[^}]*max-height:[^}]*overflow-y:\s*auto;/s,
    );
    expect(cssSource).toMatch(
      /\.cmm__effort-value\s*\{[^}]*margin-left:\s*auto;/s,
    );
  });
});
