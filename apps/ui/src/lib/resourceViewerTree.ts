import { normalizePath } from "@/lib/sessionChanges";
import type { WorkspaceGitFile } from "@/lib/workspaceGit";

/** 资源面板文件树节点。 */
export interface ResourceTreeNode {
  /** 节点显示名称。 */
  name: string;
  /** 项目内相对路径。 */
  relativePath: string;
  /** 节点是否为目录。 */
  isDir: boolean;
  /** 文件字节数，目录固定为零。 */
  size: number;
  /** 小写文件扩展名，目录为空。 */
  ext: string;
  /** 已读取的直接子节点。 */
  children?: ResourceTreeNode[];
  /** 目录内容是否仍为当前快照。 */
  loaded?: boolean;
}

/** 将保留的目录子树标记为待重载，同时继续显示旧节点以避免闪烁。 */
function markTreeDirectoriesStale(
  nodes: ResourceTreeNode[],
): ResourceTreeNode[] {
  return nodes.map((node) =>
    node.isDir
      ? {
          ...node,
          children: node.children
            ? markTreeDirectoriesStale(node.children)
            : node.children,
          loaded: false,
        }
      : node,
  );
}

/** 静默刷新根目录时保留可见子树，并让目录在下次展开时重新读取。 */
export function mergeLoadedTree(
  previous: ResourceTreeNode[],
  next: ResourceTreeNode[],
): ResourceTreeNode[] {
  if (previous.length === 0) return next;
  const previousByPath = new Map(
    previous.map((node) => [node.relativePath, node]),
  );
  return next.map((node) => {
    const old = previousByPath.get(node.relativePath);
    if (!old?.isDir || !node.isDir || !old.loaded) return node;
    return {
      ...node,
      children: old.children
        ? markTreeDirectoriesStale(old.children)
        : old.children,
      loaded: false,
    };
  });
}

/** 用 Git 过滤后的实际项替换一个 normal 模式目录占位项。 */
export function replaceWorkspaceDirectory(
  entries: WorkspaceGitFile[],
  directoryPath: string,
  files: WorkspaceGitFile[],
): WorkspaceGitFile[] {
  const key = normalizePath(directoryPath);
  return entries.flatMap((entry) =>
    normalizePath(entry.path) === key ? files : [entry],
  );
}

/** 统计变更项；未展开目录以一项作为非零下界，展开后自然替换为真实文件数。 */
export function countWorkspaceChangeFiles(entries: WorkspaceGitFile[]): number {
  return entries.length;
}
