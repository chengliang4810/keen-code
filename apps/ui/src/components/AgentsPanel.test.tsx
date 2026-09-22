import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
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

describe("子智能体工具选择控件", () => {
  it("创建及重置表单均默认不限制轮数，留空向 API 提交 null", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");
    expect(source).toContain('const [maxTurns, setMaxTurns] = useState("")');
    expect(source.match(/setMaxTurns\(""\)/g)).toHaveLength(2);
    expect(source).not.toContain('setMaxTurns("20")');
    expect(source).toContain('maxTurns: maxTurns.trim() ? Number(maxTurns) : null');
  });

  it("使用 Appica Select 与 Checkbox，并保留受控状态和可访问名称", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");

    expect(source).toContain('from "@appica/ui-react/checkbox"');
    expect(source).toContain('className="ext-agent-tools-select"');
    expect(source).toContain('<SelectItem value="all">');
    expect(source).toContain('<SelectItem value="specific">');
    expect(source).toContain("<Checkbox");
    expect(source).toContain("onValueChange");
    expect(source).toContain("onCheckedChange");
    expect(source).toContain('aria-label={tr("agents.tools")}');
    expect(source).not.toMatch(/type=["']radio["']/);
    expect(source).not.toMatch(/type=["']checkbox["']/);
  });

  it("注入 AGENTS.md 开关默认启用且仅维护创建表单 UI 状态", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");

    expect(source).toContain("const [injectAgentsMd, setInjectAgentsMd] = useState(true)");
    expect(source).toContain('tr("agents.injectAgentsMd")');
    expect(source).toContain("checked={injectAgentsMd}");
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

    const result = await api.agentDetail("plan", "D:/projects/active");
    expect(invoke).toHaveBeenCalledWith(
      "agent_detail",
      { name: "plan", projectPath: "D:/projects/active" },
      undefined,
    );
    expect(result.systemPrompt).toContain("software architect");
  });

  it("面板查询时显式携带当前项目路径", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");

    expect(source).toContain("api.agentsList(projectPath?.trim() || null)");
    expect(source).toContain(
      "api.agentDetail(agent.name, projectPath?.trim() || null)",
    );
  });

  it("模型更新只传空值或 providerId::model", async () => {
    const invoke = vi.fn().mockResolvedValue({});
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await api.agentUpdate("code-reviewer", null);
    await api.agentUpdate("code-reviewer", "provider-a::cheap-model");

    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "agent_update",
      { name: "code-reviewer", model: null },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "agent_update",
      { name: "code-reviewer", model: "provider-a::cheap-model" },
      undefined,
    );
  });
});

describe("AgentModelPicker", () => {
  const providerGroups = [
    { providerId: "p1", providerLabel: "Provider One", models: ["m-a", "m-b"] },
    { providerId: "p2", providerLabel: "Provider Two", models: ["m-c"] },
  ];

  it("复用供应商二级模型菜单，并保留可访问名称", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");

    expect(source).toContain('from "@/components/ProviderModelMenu"');
    expect(source).not.toMatch(/<select(?:\s|>)/);
    expect(source).toContain("<AgentModelSelect");
    expect(source).toContain('label={tr("agents.model.assign")}');
  });

  it("默认跟随会话 Provider，并按供应商分组列出模型", () => {
    const html = renderToStaticMarkup(
      <AgentModelPicker
        locale="zh"
        value=""
        providerGroups={providerGroups}
        onChange={() => {}}
      />,
    );

    expect(html).toContain('id="agent-model"');
    expect(html).toContain("指定模型");
    expect(html).toMatch(/<button[\s\S]*?跟随当前会话[\s\S]*?<\/button>/);
  });

  it("模型子菜单限制高度并在内部滚动", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");
    const styles = readFileSync(new URL("../styles/app-features.css", import.meta.url), "utf8");

    expect(source).toContain('modelContentClassName="ext-agent-model__menu w-56"');
    expect(styles).toMatch(
      /\.ext-agent-model__menu\s*\{[^}]*max-height:[^}]*overflow-y:\s*auto;/s,
    );
  });

  it("选中模型时以 providerId::model 作为下拉值", () => {
    const html = renderToStaticMarkup(
      <AgentModelPicker
        locale="en"
        value="p1::m-a"
        providerGroups={[providerGroups[0]]}
        onChange={() => {}}
      />,
    );

    expect(html).toContain("Model");
  });
});

describe("AgentModelSelect", () => {
  it("未指定模型时行内触发按钮显示跟随当前会话", () => {
    const html = renderToStaticMarkup(
      <AgentModelSelect
        locale="zh"
        value={null}
        providerGroups={[{ providerId: "p1", providerLabel: "Provider One", models: ["m-a"] }]}
        onSelect={() => {}}
      />,
    );

    expect(html).toContain("ext-agent-model__trigger");
    expect(html).toContain("跟随当前会话");
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
    expect(html).not.toContain("跟随当前会话");
  });

  it("目录中不存在的 provider/model 不会作为设置页选项或文案显示", () => {
    const html = renderToStaticMarkup(
      <AgentModelSelect
        locale="en"
        value="provider-a::missing-model"
        providerGroups={[{ providerId: "p1", providerLabel: "Provider One", models: ["m-a"] }]}
        onSelect={() => {}}
      />,
    );

    expect(html).toContain("Follow current session");
    expect(html).not.toContain("provider-a::missing-model");
  });
});

describe("AgentsPanel 列表布局", () => {
  it("使用图标、信息区和右侧操作区组成统一列表行", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");
    const styles = readFileSync(new URL("../styles/app-features.css", import.meta.url), "utf8");

    expect(source).toContain('className="ext-list ext-agent-list"');
    expect(source).toContain('className="ext-item ext-agent-row"');
    expect(source).toContain('className="ext-agent-row__icon"');
    expect(source).toContain('className="ext-agent-row__content"');
    expect(source).toContain('className="ext-agent-row__controls"');
    expect(source).toContain("<IconSubagent");
    expect(styles).toMatch(
      /\.ext-list > \.ext-agent-row\s*\{[\s\S]*?display:\s*grid;[\s\S]*?grid-template-columns:\s*48px minmax\(0, 1fr\) auto;/,
    );
  });

  it("创建表单使用宽面板、三列概要和全宽工具区", () => {
    const source = readFileSync(new URL("./AgentsPanel.tsx", import.meta.url), "utf8");
    const styles = readFileSync(new URL("../styles/app-features.css", import.meta.url), "utf8");

    expect(source).toContain('className="ext-agent-create-modal"');
    expect(source).toContain('className="ext-agent-create"');
    expect(source).toContain('className="ext-agent-create__name"');
    expect(source).toContain('className="ext-agent-create__model"');
    expect(source).toContain('className="ext-agent-create__turns"');
    expect(source).toContain('className="ext-agent-create__tools"');
    expect(source).toContain('className="ext-agent-create__prompt"');
    expect(styles).toMatch(
      /\.ext-agent-create\s*\{[\s\S]*?grid-template-columns:\s*minmax\(0, 1\.5fr\)[\s\S]*?minmax\(220px, 1fr\)[\s\S]*?minmax\(160px, 0\.65fr\);/,
    );
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
          model: null,
          tools: null,
          disallowedTools: ["Agent", "Write", "Edit", "Bash", "folder_operations"],
          maxTurns: null,
          allowedWriteDirs: [".keencode/plans/"],
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

  it("仅展示 providerId::model 覆盖值", () => {
    const html = renderToStaticMarkup(
      <AgentDetailView
        locale="en"
        detail={{
          name: "code-reviewer",
          description: "Reviews code for quality.",
          source: "global",
          path: null,
          model: "provider-a::cheap-model",
          tools: null,
          disallowedTools: [],
          maxTurns: null,
          allowedWriteDirs: [],
          systemPrompt: "Review the diff.",
        }}
      />,
    );

    expect(html).toContain("Model override: provider-a::cheap-model");
  });
});
