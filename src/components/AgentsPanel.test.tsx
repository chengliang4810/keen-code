import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import * as api from "@/lib/api";
import {
  AgentDetailView,
  AgentModelPicker,
  AgentModelSelect,
  agentToolsPayload,
} from "./AgentsPanel";

describe("agentToolsPayload", () => {
  it("全部模式提交 null，表示继承主智能体全部工具", () => {
    expect(agentToolsPayload("all", new Set(["Read", "Glob"]))).toBeNull();
  });

  it("指定模式提交勾选集合", () => {
    expect(agentToolsPayload("specific", new Set(["Read", "Glob"]))).toEqual([
      "Read",
      "Glob",
    ]);
  });

  it("指定模式未勾选任何工具时提交空数组", () => {
    expect(agentToolsPayload("specific", new Set())).toEqual([]);
  });
});

describe("子智能体工具模式 API 契约", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("工具目录命令返回可勾选清单", async () => {
    const invoke = vi.fn().mockResolvedValue({
      tools: ["Bash", "Read", "Write"],
    });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    const result = await api.agentsToolCatalog();
    expect(result.tools).toContain("Bash");
    expect(invoke).toHaveBeenCalledWith("agents_tool_catalog", {}, undefined);
  });

  it("默认全部工具时向 agent_create 提交 null", async () => {
    const invoke = vi.fn().mockResolvedValue({});
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await api.agentCreate({
      name: "code-reviewer",
      description: "Review code",
      prompt: "Review the diff",
      tools: null,
      maxTurns: null,
      model: null,
    });

    expect(invoke).toHaveBeenCalledWith(
      "agent_create",
      {
        name: "code-reviewer",
        description: "Review code",
        prompt: "Review the diff",
        tools: null,
        maxTurns: null,
        model: null,
      },
      undefined,
    );
  });

  it("指定工具时向 agent_create 提交勾选列表", async () => {
    const invoke = vi.fn().mockResolvedValue({});
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await api.agentCreate({
      name: "code-reviewer",
      description: "Review code",
      prompt: "Review the diff",
      tools: ["Read", "Glob"],
      maxTurns: 20,
      model: "provider-a::cheap-model",
    });

    expect(invoke).toHaveBeenCalledWith(
      "agent_create",
      {
        name: "code-reviewer",
        description: "Review code",
        prompt: "Review the diff",
        tools: ["Read", "Glob"],
        maxTurns: 20,
        model: "provider-a::cheap-model",
      },
      undefined,
    );
  });

  it("agent_detail 按名称查询子智能体详情", async () => {
    const invoke = vi.fn().mockResolvedValue({
      name: "plan",
      systemPrompt: "You are a software architect.",
    });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    const result = await api.agentDetail("plan");
    expect(invoke).toHaveBeenCalledWith("agent_detail", { name: "plan" }, undefined);
    expect(result.systemPrompt).toContain("software architect");
  });
});

describe("AgentModelPicker", () => {
  it("默认跟随会话 Provider，并按供应商分组列出模型", () => {
    const html = renderToStaticMarkup(
      <AgentModelPicker
        locale="zh"
        value=""
        providerGroups={[
          { providerId: "p1", providerLabel: "Provider One", models: ["m-a", "m-b"] },
          { providerId: "p2", providerLabel: "Provider Two", models: ["m-c"] },
        ]}
        onChange={() => {}}
      />,
    );

    expect(html).toContain('id="agent-model"');
    expect(html).toContain("指定模型");
    expect(html).toContain("继承默认");
    expect(html).toContain('label="Provider One"');
    expect(html).toContain('value="p1::m-a"');
    expect(html).toContain('value="p2::m-c"');
  });

  it("选中模型时以 providerId::model 作为下拉值", () => {
    const html = renderToStaticMarkup(
      <AgentModelPicker
        locale="en"
        value="p1::m-a"
        providerGroups={[{ providerId: "p1", providerLabel: "Provider One", models: ["m-a"] }]}
        onChange={() => {}}
      />,
    );

    expect(html).toContain('value="p1::m-a"');
    expect(html).toContain("Model");
  });
});

describe("AgentModelSelect", () => {
  it("未指定模型时行内触发按钮显示继承默认", () => {
    const html = renderToStaticMarkup(
      <AgentModelSelect
        locale="zh"
        value={null}
        providerGroups={[{ providerId: "p1", providerLabel: "Provider One", models: ["m-a"] }]}
        onSelect={() => {}}
      />,
    );

    expect(html).toContain("ext-agent-model__trigger");
    expect(html).toContain("继承默认");
  });

  it("指定模型时行内触发按钮显示模型名", () => {
    const html = renderToStaticMarkup(
      <AgentModelSelect
        locale="zh"
        value="p1::cheap-model"
        providerGroups={[{ providerId: "p1", providerLabel: "Provider One", models: ["cheap-model"] }]}
        onSelect={() => {}}
      />,
    );

    expect(html).toContain("cheap-model");
    expect(html).not.toContain("继承默认");
  });
});

describe("AgentDetailView", () => {
  it("展示内置子智能体的提示词、工具边界与沙箱目录", () => {
    const html = renderToStaticMarkup(
      <AgentDetailView
        locale="zh"
        detail={{
          name: "plan",
          description: "Software architect agent for designing implementation plans.",
          source: "builtin",
          path: null,
          model: "inherit",
          tools: null,
          disallowedTools: ["Agent", "Write", "Edit", "Bash", "folder_operations"],
          maxTurns: null,
          allowedWriteDirs: [".peri/plans/"],
          systemPrompt: "You are a software architect and planning specialist.",
        }}
      />,
    );

    expect(html).toContain("内置");
    expect(html).toContain("继承主智能体的全部工具");
    expect(html).toContain("排除的工具");
    expect(html).toContain("folder_operations");
    expect(html).toContain("沙箱可写目录");
    expect(html).toContain("software architect and planning specialist");
    expect(html).toContain('data-testid="agent-detail-prompt"');
  });

  it("tools 为显式列表时逐项展示而非继承说明", () => {
    const html = renderToStaticMarkup(
      <AgentDetailView
        locale="en"
        detail={{
          name: "code-reviewer",
          description: "Reviews code for quality.",
          source: "global",
          path: "/home/u/.keencode/agents/code-reviewer.md",
          model: null,
          tools: ["Read", "Glob", "Grep"],
          disallowedTools: [],
          maxTurns: 20,
          allowedWriteDirs: [],
          systemPrompt: "Review the diff.",
        }}
      />,
    );

    expect(html).toContain("Read, Glob, Grep");
    expect(html).not.toContain("Inherits every tool");
    expect(html).not.toContain("Excluded tools");
  });
});
