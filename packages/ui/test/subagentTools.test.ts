import { describe, expect, it } from "vitest";
import {
  allowsAllSubagentTools,
  mergeSubagentTools,
  resolveSubagentToolsFormState,
} from "../src/settings/subagentTools.js";

const knownTools = new Set(["Read", "Grep"]);

describe("Subagent 工具三态", () => {
  it("只把缺失字段识别为继承，空数组保持为禁用全部", () => {
    expect(allowsAllSubagentTools(undefined)).toBe(true);
    expect(allowsAllSubagentTools([])).toBe(false);
    expect(resolveSubagentToolsFormState(undefined, knownTools)).toEqual({
      inheritAllTools: true,
      selectedTools: [],
      preservedTools: [],
    });
    expect(resolveSubagentToolsFormState([], knownTools)).toEqual({
      inheritAllTools: false,
      selectedTools: [],
      preservedTools: [],
    });
  });

  it("全部模式标记在自定义列表中移除", () => {
    expect(allowsAllSubagentTools(["*"])).toBe(true);
    expect(resolveSubagentToolsFormState(["*"], knownTools)).toEqual({
      inheritAllTools: true,
      selectedTools: [],
      preservedTools: [],
    });
    expect(mergeSubagentTools(["Read"], ["*"])).toEqual(["Read"]);

    const allWithExplicitTool = resolveSubagentToolsFormState(["*", "Read"], knownTools);
    expect(allWithExplicitTool.inheritAllTools).toBe(true);
    expect(mergeSubagentTools([], allWithExplicitTool.preservedTools)).toEqual([]);
  });

  it("保留未枚举工具，同时允许显式保存空列表", () => {
    expect(resolveSubagentToolsFormState(["Read", "McpTool"], knownTools)).toEqual({
      inheritAllTools: false,
      selectedTools: ["Read"],
      preservedTools: ["McpTool"],
    });
    expect(mergeSubagentTools([], [])).toEqual([]);
    expect(mergeSubagentTools([], ["McpTool"])).toEqual(["McpTool"]);

    const persistedNone = mergeSubagentTools([], []);
    const reopenedNone = resolveSubagentToolsFormState(persistedNone, knownTools);
    expect(reopenedNone.inheritAllTools).toBe(false);
    expect(mergeSubagentTools(reopenedNone.selectedTools, reopenedNone.preservedTools)).toEqual(
      [],
    );
  });
});
