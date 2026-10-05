const ALL_TOOLS_MARKER = "*";

export interface SubagentToolsFormState {
  /** 当前表单是否对应省略 tools 字段的继承模式。 */
  inheritAllTools: boolean;
  /** 已枚举工具的自定义选择。 */
  selectedTools: string[];
  /** 当前界面未枚举但必须原样保留的工具名。 */
  preservedTools: string[];
}

/**
 * 前端必须保留 Rust 工具字段的三态：缺失表示继承，空数组表示禁用全部，非空数组表示限定列表。
 * UI 的 `*` 全部模式标记在保存时归一为省略字段，切换自定义时不能把它作为未知工具回写。
 */
export function allowsAllSubagentTools(tools: readonly string[] | undefined): boolean {
  return tools === undefined || tools.some((tool) => tool.trim() === ALL_TOOLS_MARKER);
}

export function resolveSubagentToolsFormState(
  tools: readonly string[] | undefined,
  knownTools: ReadonlySet<string>,
): SubagentToolsFormState {
  return {
    inheritAllTools: allowsAllSubagentTools(tools),
    selectedTools: tools?.filter((tool) => knownTools.has(tool)) ?? [],
    preservedTools:
      tools?.filter(
        (tool) => tool.trim() !== ALL_TOOLS_MARKER && !knownTools.has(tool),
      ) ?? [],
  };
}

/**
 * 自定义模式的空选择必须序列化为 `[]`，否则 Rust 会把省略字段解释为继承全部工具。
 * `*` 只能代表全部模式，不能和自定义工具列表混写。
 */
export function mergeSubagentTools(
  selectedTools: readonly string[],
  preservedTools: readonly string[],
): string[] {
  return [...selectedTools, ...preservedTools].filter(
    (tool, index, tools) =>
      tool.length > 0 &&
      tool.trim() !== ALL_TOOLS_MARKER &&
      tools.indexOf(tool) === index,
  );
}
