export type UtilityTool = "explorer" | "source-control";

/** 文件树与 Git 面板各只有一个实例，重复打开只激活已有标签。 */
export function openUtilityTab(
  tabs: UtilityTool[],
  tool: UtilityTool,
): UtilityTool[] {
  return tabs.includes(tool) ? tabs : [...tabs, tool];
}

export function toolViewAfterClose(
  tabs: UtilityTool[],
  current: UtilityTool | "workspace" | "empty",
  closing: UtilityTool,
  hasWorkspaceTab: boolean,
): UtilityTool | "workspace" | "empty" {
  if (current !== closing) return current;
  return (
    tabs.find((tab) => tab !== closing) ??
    (hasWorkspaceTab ? "workspace" : "empty")
  );
}
