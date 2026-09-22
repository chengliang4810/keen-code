import React from "react";
import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ComposerModelMenu,
  groupComposerModelsByProvider,
} from "./ComposerModelMenu";
import { readCssSource } from "../test-utils/readCssSource";

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
        open: false,
        onOpenChange: () => {},
        modelId: "",
        models: [],
        labels: {
          model: "模型",
          vision: "视觉",
          addModel: "添加模型",
          manageModels: "管理模型",
        },
        onModel: () => {},
        onAddModel: () => {},
      }),
    );

    expect(html).toContain("添加模型");
    expect(html).not.toContain("推理强度");
  });

  it("模型菜单在 SSR 下可渲染", () => {
    const html = renderToString(
      <ComposerModelMenu
        open={false}
        onOpenChange={() => {}}
        modelId="gpt-5"
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
          vision: "视觉",
          addModel: "添加模型",
          manageModels: "管理模型",
        }}
        onModel={() => {}}
        onAddModel={() => {}}
      />,
    );

    expect(html).toContain('aria-label="模型"');
    // 触发器显示「供应商/模型」。
    expect(html).toContain("OpenAI/GPT-5");
  });

  it("未传当前供应商时回退到模型自身供应商，模型不在目录中时只显示模型名", () => {
    const withProvider = renderToString(
      <ComposerModelMenu
        open={false}
        onOpenChange={() => {}}
        modelId="glm-5.3-flash"
        models={[
          {
            providerId: "workbuddy",
            providerLabel: "Workbuddy",
            id: "glm-5.3-flash",
            label: "cn:glm-5.3-flash",
          },
        ]}
        labels={{
          model: "模型",
          vision: "视觉",
          addModel: "添加模型",
          manageModels: "管理模型",
        }}
        onModel={() => {}}
        onAddModel={() => {}}
      />,
    );
    expect(withProvider).toContain("Workbuddy/cn:glm-5.3-flash");

    const missing = renderToString(
      <ComposerModelMenu
        open={false}
        onOpenChange={() => {}}
        modelId="unknown-model"
        models={[
          {
            providerId: "openai",
            providerLabel: "OpenAI",
            id: "gpt-5",
            label: "GPT-5",
          },
        ]}
        labels={{
          model: "模型",
          vision: "视觉",
          addModel: "添加模型",
          manageModels: "管理模型",
        }}
        onModel={() => {}}
        onAddModel={() => {}}
      />,
    );
    expect(missing).toContain(">unknown-model<");
    expect(missing).not.toContain("OpenAI/unknown-model");
  });

  it("同名模型属于多个供应商时显示会话自身供应商", () => {
    // 多家网关都提供同一模型 ID：只按模型 ID 查找会显示成列表里第一个供应商。
    const catalog = [
      {
        providerId: "workbuddy",
        providerLabel: "Workbuddy",
        id: "deepseek-v4.1-flash",
        label: "deepseek-v4.1-flash",
      },
      {
        providerId: "workbuddy-ai",
        providerLabel: "WorkBuddyAI",
        id: "deepseek-v4.1-flash",
        label: "deepseek-v4.1-flash",
      },
    ];
    const html = renderToString(
      <ComposerModelMenu
        open={false}
        onOpenChange={() => {}}
        providerId="workbuddy-ai"
        modelId="deepseek-v4.1-flash"
        models={catalog}
        labels={{
          model: "模型",
          vision: "视觉",
          addModel: "添加模型",
          manageModels: "管理模型",
        }}
        onModel={() => {}}
        onAddModel={() => {}}
      />,
    );

    expect(html).toContain("WorkBuddyAI/deepseek-v4.1-flash");
    expect(html).not.toContain(">Workbuddy/deepseek-v4.1-flash<");
  });

  it("视觉能力标签跟随模型 supportsVision，并标记当前供应商与模型", () => {
    const source = readFileSync(
      new URL("./ComposerModelMenu.tsx", import.meta.url),
      "utf8",
    );

    // 仅对声明支持图片输入的模型渲染标签。
    expect(source).toMatch(/suffix: model\.supportsVision \? \([\s\S]*?labels\.vision/);
    // 当前供应商与当前模型都在列表中可见。
    expect(source).toContain("selectedProviderId={activeProviderId}");
    expect(source).toContain("selectedModelId={modelId}");
  });

  it("触发器不显示模型图标，模型子菜单限高滚动且不再承载推理强度", () => {
    const source = readFileSync(
      new URL("./ComposerModelMenu.tsx", import.meta.url),
      "utf8",
    );
    const cssSource = readCssSource(
      new URL("../styles/app.css", import.meta.url),
    );

    expect(source).not.toContain("IconBolt");
    expect(source).toContain("cmm__model-list");
    expect(source).not.toContain("effort");
    expect(cssSource).toMatch(
      /\.cmm__model-list\s*\{[^}]*max-height:[^}]*overflow-y:\s*auto;/s,
    );
  });
});
