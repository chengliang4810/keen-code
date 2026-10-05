/**
 * 工作区入口只在 journal 清单已成功读到节点时出现。
 * 空清单仍表示真实的“尚未触碰工作区”，不能把它伪装成可打开的 transcript。
 */
export function isWorkflowRunWorkspaceOpenable(input: {
  loaded: boolean;
  unavailable: boolean;
  nodeCount: number;
}): boolean {
  return input.loaded && !input.unavailable && input.nodeCount > 0;
}
