import { afterEach, describe, expect, it, vi } from "vitest";
import * as api from "@/lib/api";
import { agentToolsPayload } from "./AgentsPanel";

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
    });

    expect(invoke).toHaveBeenCalledWith(
      "agent_create",
      {
        name: "code-reviewer",
        description: "Review code",
        prompt: "Review the diff",
        tools: null,
        maxTurns: null,
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
    });

    expect(invoke).toHaveBeenCalledWith(
      "agent_create",
      {
        name: "code-reviewer",
        description: "Review code",
        prompt: "Review the diff",
        tools: ["Read", "Glob"],
        maxTurns: 20,
      },
      undefined,
    );
  });
});
